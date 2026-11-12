//! What changed since the last search, so a load re-checks only that. A
//! search keeps a record in the user's cache directory, one per tree and
//! index: the baseline (the paths that may differ from the index) and a
//! watchman token, with the daemon's socket and watch. A later search asks
//! watchman what changed since the token (the token question), adds the
//! answer to the baseline, re-checks those paths, and saves the new
//! baseline with a newer token. A search that cannot use a record finds
//! the daemon (its client starts one when none runs), asks it to watch the
//! tree when no watch covers it,
//! takes a token and checks every file (the stat pass). The index only
//! asks watchman to watch the tree once its walk has listed the files,
//! without waiting for the crawl; it records no watchman state, and a
//! search writes nothing into it. When watchman cannot answer, the search
//! checks every file and says why, once per process.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[cfg(unix)]
mod watchman;

const VERSION: u32 = 1;
/// Below this many docs the stat pass beats asking.
const MIN_DOCS: usize = 50_000;

/// `watchman get-sockname`, a process start: 2-10 ms with a daemon or
/// without one (`--no-spawn`), more when the client starts the daemon, and
/// up to this when a daemon hangs.
const FIND_WAIT: Duration = Duration::from_millis(500);
/// A load's token and question, small next to the stat pass it would
/// spare; also what `index` gives watchman to take its request to watch
/// the tree.
const ASK_WAIT: Duration = Duration::from_millis(100);

/// Whether an index asks watchman to watch the tree and a load asks it
/// what changed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Knob {
    /// Where it pays off.
    Auto,
    Off,
    /// Even where the stat pass is cheaper.
    On,
}

impl Knob {
    /// `FMGREP_JOURNAL`: unset or empty, `off` or `on`. Tests read it as
    /// unset; their hooks say where a load asks.
    pub fn read() -> Result<Self, String> {
        #[cfg(test)]
        let raw: Result<Option<&str>, String> = Ok(None);
        #[cfg(not(test))]
        let raw = crate::env::choice("FMGREP_JOURNAL", &["off", "on"]);
        raw.map(|raw| match raw {
            None => Self::Auto,
            Some("off") => Self::Off,
            Some(_) => Self::On,
        })
    }
}

/// Whether a load of `docs` docs asks watchman: from [`MIN_DOCS`], at any
/// size under `FMGREP_JOURNAL=on`, never under `off`.
pub fn due(docs: usize) -> bool {
    #[cfg(test)]
    return crate::tests::ASK_FROM
        .get()
        .is_some_and(|from| docs >= from);
    #[cfg(not(test))]
    return match Knob::read() {
        Ok(Knob::Auto) => docs >= MIN_DOCS,
        Ok(Knob::On) => true,
        Ok(Knob::Off) | Err(_) => false,
    };
}

/// Once the index walk of `root` has listed `files` files, before it reads
/// them, where a search of as many docs would ask watchman: have watchman
/// watch the tree, waiting at most [`ASK_WAIT`] for its reply, which comes
/// only once its crawl ends; the watch stays, and watchman crawls the tree
/// while the index reads it. Takes no token. Why not, or that the crawl
/// goes on, only under `--verbose`.
pub fn request_watch(root: &Path, files: usize) {
    if !due(files) {
        return;
    }
    let Ok(root) = root.canonicalize() else {
        return;
    };
    #[cfg(unix)]
    let asked = watchman::Watchman::find(ASK_WAIT).and_then(|w| w.watch(&root).map(drop));
    #[cfg(not(unix))]
    let asked: Result<(), String> = Err("no change source on this platform".to_owned());
    if let Err(why) = asked
        && crate::verbose::verbose()
    {
        eprintln!("note: {why}");
    }
}

/// Re-checking a named path costs about 7 µs, the stat pass about 1 µs a
/// doc: past this many named paths the stat pass is cheaper. Never below
/// 64, where either costs microseconds.
pub fn named_cap(docs: usize) -> usize {
    (docs / 8).max(64)
}

/// A watchman clock of the watch on a tree, and the daemon that gave it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Token {
    pub sock: String,
    pub watch: String,
    /// The tree, relative to `watch`.
    pub relative: String,
    pub clock: String,
}

/// The index a record is of: its canonical directory, and when it began.
#[derive(Clone, Debug)]
pub struct Index {
    pub dir: PathBuf,
    pub started_ns: i64,
}

/// One search's state for the next, at [`path`].
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Record {
    v: u32,
    root: String,
    index: String,
    index_started_ns: i64,
    token: Token,
    /// Ascending root-relative paths that may differ from the index; `None`
    /// when more than [`named_cap`] do.
    baseline: Option<Vec<String>>,
}

/// What a load learned before it re-checks anything.
#[derive(Debug, Default)]
pub struct Asked {
    /// Taken now, for the record written once the search answered.
    pub token: Option<Token>,
    /// What may differ, unsorted; `None`: check every file.
    pub paths: Option<Vec<String>>,
}

/// `$XDG_CACHE_HOME/fmgrep`, else `$HOME/.cache/fmgrep`; tests use only
/// the directory they are given.
fn cache_dir() -> Option<PathBuf> {
    #[cfg(test)]
    return crate::tests::CACHE.with_borrow(Clone::clone);
    #[cfg(not(test))]
    {
        let var = |name| std::env::var_os(name).map(PathBuf::from);
        let xdg = var("XDG_CACHE_HOME").filter(|p| p.is_absolute());
        let home = var("HOME").filter(|p| p.is_absolute());
        Some(
            xdg.or_else(|| home.map(|h| h.join(".cache")))?
                .join("fmgrep"),
        )
    }
}

/// FNV-1a, 64 bits: stable across builds, so a record keeps its name.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The record of tree `root` and index directory `index`, both canonical:
/// `<cache>/<basename of root>-<16 hex>.json`.
fn path(root: &str, index: &str) -> Option<PathBuf> {
    let base = Path::new(root).file_name().and_then(|n| n.to_str());
    let hash = fnv(format!("{root}\0{index}").as_bytes());
    Some(cache_dir()?.join(format!("{}-{hash:016x}.json", base.unwrap_or("root"))))
}

impl Record {
    /// The record of `root` and `index`, or why there is none to use.
    fn read(root: &str, index: &Index) -> Result<Self, String> {
        let dir = index.dir.to_str().ok_or("the index path is not UTF-8")?;
        let path = path(root, dir).ok_or("no cache directory")?;
        let bytes = std::fs::read(&path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => "no record of an earlier search".to_owned(),
            _ => format!("cannot read {} ({e})", path.display()),
        })?;
        let record: Self = serde_json::from_slice(&bytes)
            .map_err(|e| format!("{} is malformed ({e})", path.display()))?;
        let ours = record.v == VERSION
            && record.root == root
            && record.index == dir
            && record.index_started_ns == index.started_ns;
        match ours {
            true => Ok(record),
            false => Err(format!(
                "{} was written for another index build",
                path.display()
            )),
        }
    }

    /// Atomic: a crash leaves the old file or the new one, and two writers
    /// leave one of theirs. Says why it could not, once per process.
    fn write(&self) {
        let Some(path) = path(&self.root, &self.index) else {
            return cannot_save(format_args!(
                "the watchman token (no cache directory: set XDG_CACHE_HOME or HOME)"
            ));
        };
        static WRITES: AtomicUsize = AtomicUsize::new(0);
        let n = WRITES.fetch_add(1, Ordering::Relaxed);
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("record");
        let tmp = path.with_file_name(format!(".{name}.{}.{n}", std::process::id()));
        let bytes = serde_json::to_vec(self).expect("a record serializes");
        let wrote = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&tmp, bytes))
            .and_then(|()| std::fs::rename(&tmp, &path));
        if let Err(e) = wrote {
            let _ = std::fs::remove_file(&tmp);
            cannot_save(format_args!("{} ({e})", path.display()));
        }
    }
}

/// What may differ under canonical `root` from `index`, of `docs` docs, and
/// a token for the record; why watchman cannot say goes to stderr.
pub fn ask(root: &Path, index: &Index, docs: usize) -> Asked {
    #[cfg(unix)]
    {
        let Some(root_str) = root.to_str() else {
            cannot_answer("the tree's path is not UTF-8");
            return Asked::default();
        };
        let (why, taken) = match Record::read(root_str, index).and_then(same_socket) {
            Ok(record) => {
                let w = watchman::Watchman::at(record.token.sock.clone().into(), ASK_WAIT);
                match later(&w, &record, root, docs) {
                    Ok(asked) => return asked,
                    Err((why, token)) if w.late() => {
                        cannot_answer(&why);
                        return Asked { token, paths: None };
                    }
                    Err(not_used) => not_used,
                }
            }
            Err(why) => (why, None),
        };
        first(root, taken, &why)
    }
    #[cfg(not(unix))]
    {
        let _ = (root, index, docs);
        cannot_answer("watchman is not supported on this platform");
        Asked::default()
    }
}

/// `record`, unless `WATCHMAN_SOCK` gives another socket than the one it
/// keeps: the variable chooses the daemon.
#[cfg(unix)]
fn same_socket(record: Record) -> Result<Record, String> {
    match watchman::Watchman::given() {
        Some(sock) if sock != Path::new(&record.token.sock) => Err(format!(
            "WATCHMAN_SOCK is {}, and the record keeps {}",
            sock.display(),
            record.token.sock
        )),
        _ => Ok(record),
    }
}

/// A search with `record`: a newer token, then the token question, plus
/// the baseline; or why not, with the token if it was taken.
#[cfg(unix)]
fn later(
    w: &watchman::Watchman,
    record: &Record,
    root: &Path,
    docs: usize,
) -> Result<Asked, (String, Option<Token>)> {
    let token = w.mark(root).map_err(|why| (why, None))?;
    let Some(baseline) = &record.baseline else {
        over(named_cap(docs));
        return Ok(Asked {
            token: Some(token),
            paths: None,
        });
    };
    let mut paths = match w.since(&record.token, root) {
        Ok(paths) => paths,
        Err(why) => return Err((why, Some(token))),
    };
    paths.extend(baseline.iter().cloned());
    Ok(Asked {
        token: Some(token),
        paths: Some(paths),
    })
}

/// A search that cannot use a record, for reason `why`: find the daemon
/// and take a token, unless one was `taken` just now; every file is
/// checked.
#[cfg(unix)]
fn first(root: &Path, taken: Option<Token>, why: &str) -> Asked {
    let taken = match taken {
        Some(token) => Ok(token),
        None => watchman::Watchman::find(ASK_WAIT).and_then(|w| w.mark(root)),
    };
    match taken {
        Ok(token) => {
            checked_once(why);
            Asked {
                token: Some(token),
                paths: None,
            }
        }
        Err(why) => {
            cannot_answer(&why);
            Asked::default()
        }
    }
}

/// Save `token` and `baseline` (ascending, deduped) as what differs from
/// `index` under canonical `root` at it, after the search answered.
pub fn save(root: &Path, index: &Index, token: Token, baseline: Vec<String>, cap: usize) {
    let (Some(root), Some(dir)) = (root.to_str(), index.dir.to_str()) else {
        return cannot_save(format_args!("the record (the index path is not UTF-8)"));
    };
    let record = Record {
        v: VERSION,
        root: root.to_owned(),
        index: dir.to_owned(),
        index_started_ns: index.started_ns,
        token,
        baseline: (baseline.len() <= cap).then_some(baseline),
    };
    record.write();
}

/// `line` on stderr, once per process for each `seen`.
pub(super) fn warn(seen: &AtomicBool, line: std::fmt::Arguments<'_>) {
    #[cfg(test)]
    crate::tests::NOTES.with_borrow(|n| n.lock().unwrap().push(line.to_string()));
    if !seen.swap(true, Ordering::Relaxed) {
        eprintln!("warning: {line}");
    }
}

/// No usable answer from watchman on a tree worth asking about.
pub fn cannot_answer(why: &str) {
    static SEEN: AtomicBool = AtomicBool::new(false);
    warn(
        &SEEN,
        format_args!("watchman cannot say what changed ({why}); checked every file"),
    );
}

/// This search took a token, but checked every file for its baseline.
fn checked_once(why: &str) {
    static SEEN: AtomicBool = AtomicBool::new(false);
    warn(
        &SEEN,
        format_args!("checked every file once ({why}); later searches ask watchman"),
    );
}

/// The record could not be written.
fn cannot_save(what: std::fmt::Arguments<'_>) {
    static SEEN: AtomicBool = AtomicBool::new(false);
    warn(
        &SEEN,
        format_args!("cannot save {what}; the next search checks again"),
    );
}

/// More paths may differ than re-checking them beats the stat pass.
pub fn over(cap: usize) {
    static SEEN: AtomicBool = AtomicBool::new(false);
    warn(
        &SEEN,
        format_args!(
            "more than {cap} paths may differ from the index; checked every file (re-run index)"
        ),
    );
}

#[cfg(test)]
mod tests;
