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

const VERSION: u32 = 2;
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
/// A search through a server's token and question: they spare a git
/// status of the checkout, which takes far longer on a large one.
const CHECKOUT_WAIT: Duration = Duration::from_millis(500);

/// Whether a load of `docs` docs asks watchman.
pub fn due(docs: usize) -> bool {
    #[cfg(test)]
    return crate::tests::ASK_FROM
        .get()
        .is_some_and(|from| docs >= from);
    #[cfg(not(test))]
    return docs >= MIN_DOCS;
}

/// Once the index walk of `root` has listed `files` files, before it reads
/// them, where a search of as many docs would ask watchman: have watchman
/// watch the tree, waiting at most [`ASK_WAIT`] for its reply, which comes
/// only once its crawl ends; the watch stays, and watchman crawls the tree
/// while the index reads it. Takes no token, and asks nothing where no
/// search asks: on a network filesystem, or when the mount table cannot
/// be read. Why not, or that the crawl goes on, only under `--verbose`.
pub fn request_watch(root: &Path, files: usize) {
    if !due(files) {
        return;
    }
    let Ok(root) = root.canonicalize() else {
        return;
    };
    let asked = local(&root).and_then(|()| {
        #[cfg(unix)]
        return watchman::Watchman::find(ASK_WAIT).and_then(|w| w.watch(&root).map(drop));
        #[cfg(not(unix))]
        return Err("no change source on this platform".to_owned());
    });
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

/// The type of the filesystem holding canonical `root`, from
/// `/proc/self/mountinfo` on Linux; `None` elsewhere. Tests give their own
/// mountinfo file, or none.
fn filesystem_of(root: &Path) -> Result<Option<String>, String> {
    #[cfg(test)]
    let file = crate::tests::MOUNTS.with_borrow(Clone::clone);
    #[cfg(not(test))]
    let file = cfg!(target_os = "linux").then(|| PathBuf::from("/proc/self/mountinfo"));
    let Some(file) = file else {
        return Ok(None);
    };
    let mounts = std::fs::read_to_string(&file).map_err(|e| {
        format!(
            "cannot read {} to tell the tree's filesystem ({e})",
            file.display()
        )
    })?;
    Ok(filesystem(&mounts, root))
}

/// The type of the filesystem holding `root`, from mountinfo text: the
/// longest mount point above it, the last one mounted there.
fn filesystem(mountinfo: &str, root: &Path) -> Option<String> {
    let mounts = mountinfo.lines().filter_map(|line| {
        let (head, tail) = line.split_once(" - ")?;
        let point = unescape(head.split(' ').nth(4)?);
        let fs = tail.split(' ').next()?;
        root.starts_with(&point).then_some((point.len(), fs))
    });
    mounts
        .max_by_key(|&(len, _)| len)
        .map(|(_, fs)| fs.to_owned())
}

/// A mount point as mountinfo writes it, `\040` for a space.
fn unescape(field: &str) -> String {
    let mut out = Vec::new();
    let bytes = field.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let octal = bytes
            .get(i + 1..i + 4)
            .filter(|o| o.iter().all(u8::is_ascii_digit));
        match (bytes[i], octal) {
            (b'\\', Some(o)) => {
                out.push(o.iter().fold(0u8, |n, d| n.wrapping_mul(8) + (d - b'0')));
                i += 4;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Whether watchman sees every change under canonical `root`; if not, why:
/// it is on a network filesystem, or the mount table cannot be read, so
/// that is not known.
fn local(root: &Path) -> Result<(), String> {
    match filesystem_of(root)? {
        Some(fs) if network(&fs) => Err(format!(
            "{} is on {fs}, a network or shared filesystem: watchman does not see \
             changes made there from other machines",
            root.display()
        )),
        _ => Ok(()),
    }
}

/// Network filesystems: a file changed from another machine sends this
/// machine no event, so watchman never lists it.
fn network(fs: &str) -> bool {
    let network = [
        "nfs", "nfs4", "cifs", "smb3", "smbfs", "9p", "ceph", "afs", "virtiofs", "lustre", "gpfs",
    ];
    network.contains(&fs)
}

/// One search's state for the next, at [`path`].
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Record {
    v: u32,
    root: String,
    /// What the baseline differs from: `index <dir>@<started_ns>`, or
    /// `git <commit>`.
    of: String,
    token: Token,
    /// Ascending root-relative paths that may differ from the index; `None`
    /// when more than [`named_cap`] do.
    baseline: Option<Vec<String>>,
    /// What else a search through a server checks before it builds on the
    /// baseline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    extra: Option<serde_json::Value>,
}

/// What a load learned before it re-checks anything.
#[derive(Debug, Default)]
pub struct Asked {
    /// Taken now, for the record written once the search answered. For a
    /// server whose daemon is late, the record's: every file is checked
    /// after it was taken, so the next query can ask since it.
    pub token: Option<Token>,
    /// What may differ, unsorted; `None`: check every file.
    pub paths: Option<Vec<String>>,
    /// Why watchman could not answer, as stderr says it.
    pub why: Option<String>,
    /// Why watchman cannot be used at all, when a later ask would fail as
    /// well (not installed, no daemon on `WATCHMAN_SOCK`, a network
    /// filesystem); set instead of `why` when the caller refuses on it.
    pub lasting: Option<String>,
}

/// What a server learned before one query.
#[derive(Debug, Default)]
pub struct Again {
    /// Taken now: the next query asks since it.
    pub token: Option<Token>,
    /// Without a new token: whether the last one may answer later.
    pub keep: bool,
    /// What changed since the last token, unsorted; `None`: check every file.
    pub paths: Option<Vec<String>>,
    /// Why every file is checked, as stderr says it.
    pub why: Option<String>,
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

/// The record of canonical tree `root` and `key`:
/// `<cache>/<basename of root>-<16 hex>.json`.
fn path(root: &str, key: &str) -> Option<PathBuf> {
    let base = Path::new(root).file_name().and_then(|n| n.to_str());
    let hash = fnv(format!("{root}\0{key}").as_bytes());
    Some(cache_dir()?.join(format!("{}-{hash:016x}.json", base.unwrap_or("root"))))
}

/// What a record's baseline differs from: `key` names its file, and `of`,
/// which the record keeps, must match for the record to be used.
pub struct Of {
    key: String,
    of: String,
    /// Says what a record of another `of` was written for.
    other: &'static str,
}

impl Of {
    /// A checkout's record against served commit `id`: one per checkout and
    /// commit, so that searches through servers of two commits keep two.
    pub fn commit(id: &str) -> Self {
        Self {
            key: format!("git {id}"),
            of: format!("git {id}"),
            other: "another commit",
        }
    }
}

impl Index {
    /// The index directory names the file, its build the baseline.
    fn of(&self) -> Result<Of, String> {
        let dir = self.dir.to_str().ok_or("the index path is not UTF-8")?;
        Ok(Of {
            key: dir.to_owned(),
            of: format!("index {dir}@{}", self.started_ns),
            other: "another index build",
        })
    }
}

impl Record {
    /// The record of `root` and `of`, or why there is none to use.
    fn read(root: &str, of: &Of) -> Result<Self, String> {
        let path = path(root, &of.key).ok_or("no cache directory")?;
        let bytes = std::fs::read(&path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => "no record of an earlier search".to_owned(),
            _ => format!("cannot read {} ({e})", path.display()),
        })?;
        let malformed = |e: serde_json::Error| format!("{} is malformed ({e})", path.display());
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(malformed)?;
        if value.get("v").and_then(serde_json::Value::as_u64) != Some(u64::from(VERSION)) {
            return Err(format!(
                "{} was written by another fmgrep version",
                path.display()
            ));
        }
        let record: Self = serde_json::from_value(value).map_err(malformed)?;
        match record.root == root && record.of == of.of {
            true => Ok(record),
            false => Err(format!("{} was written for {}", path.display(), of.other)),
        }
    }

    /// Atomic: a crash leaves the old file or the new one, and two writers
    /// leave one of theirs. Says why it could not, once per process.
    fn write(&self, key: &str) {
        let Some(path) = path(&self.root, key) else {
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
/// a token for the record; why watchman cannot say goes to stderr, worded
/// for a server's queries if `served`, unless `served` and the cause
/// lasts: then it is returned, for the server to refuse. On a network
/// filesystem, or when the mount table cannot be read, there is no token,
/// so no record either.
pub fn ask(root: &Path, index: &Index, docs: usize, served: bool) -> Asked {
    let fail = |why: String, lasting: bool| match lasting && served {
        true => Asked {
            lasting: Some(why),
            ..Asked::default()
        },
        false => Asked {
            why: Some(cannot_answer(&why)),
            ..Asked::default()
        },
    };
    let root_str = match unaskable(root) {
        Err(why) => return fail(why, true),
        Ok(root_str) => root_str,
    };
    #[cfg(unix)]
    {
        let record = index.of().and_then(|of| Record::read(root_str, &of));
        let (why, taken) = match record.and_then(same_socket) {
            Ok(record) => {
                let sock = record.token.sock.clone().into();
                let w = watchman::Watchman::at(sock, ASK_WAIT).served(served);
                match later(&w, &record, root, docs) {
                    Ok(asked) => return asked,
                    Err((why, token)) if w.late() => {
                        let kept = served.then(|| record.token.clone());
                        return Asked {
                            token: token.or(kept),
                            why: Some(cannot_answer(&why)),
                            ..Asked::default()
                        };
                    }
                    Err(not_used) => not_used,
                }
            }
            Err(why) => (why, None),
        };
        match first(root, taken, served) {
            Ok(token) => {
                checked_once(&why, served);
                Asked {
                    token: Some(token),
                    ..Asked::default()
                }
            }
            Err((why, late)) => fail(why, !late),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (root_str, index, docs);
        unreachable!("unaskable refuses every root here")
    }
}

/// Canonical `root` as UTF-8, or why watchman cannot answer for it at all.
fn unaskable(root: &Path) -> Result<&str, String> {
    if !cfg!(unix) {
        return Err("watchman is not supported on this platform".to_owned());
    }
    let root_str = root.to_str().ok_or("the tree's path is not UTF-8")?;
    local(root)?;
    Ok(root_str)
}

/// Before a served query on canonical `root`: a new token, then what
/// changed since `last`, the token taken for the query before. Without
/// `last`, or when its daemon is gone, the daemon is found again and a
/// first token taken; every file is checked then. A late daemon keeps
/// `last`. Nothing is printed.
pub fn again(root: &Path, last: Option<&Token>) -> Again {
    if let Err(why) = unaskable(root) {
        return Again {
            why: Some(cannot(&why)),
            ..Again::default()
        };
    }
    #[cfg(unix)]
    {
        let Some(last) = last else {
            return renew(root, "no token from an earlier query");
        };
        let w = watchman::Watchman::at(last.sock.clone().into(), ASK_WAIT).served(true);
        let token = match w.mark(root) {
            Ok(token) => token,
            Err(why) if w.late() => {
                return Again {
                    keep: true,
                    why: Some(cannot(&why)),
                    ..Again::default()
                };
            }
            Err(why) => return renew(root, &why),
        };
        let (paths, why) = match w.since(last, root) {
            Ok(paths) => (Some(paths), None),
            Err(why) if w.late() => (None, Some(cannot(&why))),
            Err(why) => (None, Some(once(&why, true))),
        };
        Again {
            token: Some(token),
            keep: false,
            paths,
            why,
        }
    }
    #[cfg(not(unix))]
    unreachable!("unaskable refuses every root here: {last:?}")
}

/// A server's first token, or one from the daemon found again because
/// the last token cannot answer, for reason `why`.
#[cfg(unix)]
fn renew(root: &Path, why: &str) -> Again {
    match watchman::Watchman::find(ASK_WAIT).and_then(|w| w.served(true).mark(root)) {
        Ok(token) => Again {
            token: Some(token),
            why: Some(once(why, true)),
            ..Again::default()
        },
        Err(why) => Again {
            why: Some(cannot(&why)),
            ..Again::default()
        },
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
            ..Asked::default()
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
        ..Asked::default()
    })
}

/// For a search that cannot use a record: find the daemon and take a
/// token, unless one was `taken` just now; every file is checked. Else
/// why not, worded for a server's queries if `served`, and whether
/// watchman may answer later.
#[cfg(unix)]
fn first(root: &Path, taken: Option<Token>, served: bool) -> Result<Token, (String, bool)> {
    match taken {
        Some(token) => Ok(token),
        None => match watchman::Watchman::find(ASK_WAIT) {
            Ok(w) => {
                let w = w.served(served);
                w.mark(root).map_err(|why| (why, w.late()))
            }
            Err(why) => {
                let late = watchman::late_find(&why);
                Err((why, late))
            }
        },
    }
}

/// Save `token` and `baseline` (ascending, deduped) as what differs from
/// `index` under canonical `root` at it, after the search answered.
pub fn save(root: &Path, index: &Index, token: Token, baseline: Vec<String>, cap: usize) {
    let (Some(root), Ok(of)) = (root.to_str(), index.of()) else {
        return cannot_save(format_args!("the record (the index path is not UTF-8)"));
    };
    let record = Record {
        v: VERSION,
        root: root.to_owned(),
        of: of.of,
        token,
        baseline: (baseline.len() <= cap).then_some(baseline),
        extra: None,
    };
    record.write(&of.key);
}

/// What a search through a server learned of its checkout from its record
/// and watchman, before it asks git anything.
pub enum Remembered {
    /// A token taken now; the record's baseline and extra; and the paths
    /// under the root that changed since the record's token, unsorted.
    Since {
        token: Token,
        baseline: Vec<String>,
        extra: serde_json::Value,
        changed: Vec<String>,
    },
    /// A token taken now, and why there is no baseline to build on.
    Fresh { token: Token, why: String },
    /// Why watchman cannot say what changed.
    Cannot(String),
}

/// The record of the checkout at canonical `root` against `of`, and what
/// watchman says changed since it; at any checkout size.
pub fn remembered(root: &Path, of: &Of) -> Remembered {
    let root_str = match unaskable(root) {
        Err(why) => return Remembered::Cannot(why),
        Ok(root_str) => root_str,
    };
    #[cfg(unix)]
    {
        let why = match Record::read(root_str, of).and_then(same_socket) {
            Ok(record) => {
                let w = watchman::Watchman::at(record.token.sock.clone().into(), CHECKOUT_WAIT);
                match w.mark(root) {
                    Ok(token) => {
                        let (baseline, extra) = (record.baseline, record.extra);
                        return match (w.since(&record.token, root), baseline, extra) {
                            (Ok(changed), Some(baseline), Some(extra)) => Remembered::Since {
                                token,
                                baseline,
                                extra,
                                changed,
                            },
                            (Err(why), _, _) => Remembered::Fresh { token, why },
                            _ => Remembered::Fresh {
                                token,
                                why: "the record holds no baseline".to_owned(),
                            },
                        };
                    }
                    Err(why) => why,
                }
            }
            Err(why) => why,
        };
        match watchman::Watchman::find(CHECKOUT_WAIT).and_then(|w| w.mark(root)) {
            Ok(token) => Remembered::Fresh { token, why },
            Err(why) => Remembered::Cannot(why),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (root_str, of);
        unreachable!("unaskable refuses every root here")
    }
}

/// Whether the checkout at canonical `root` keeps a record against `of`:
/// one was saved after git compared them.
pub fn recorded(root: &Path, of: &Of) -> bool {
    root.to_str()
        .is_some_and(|root| Record::read(root, of).is_ok())
}

/// Save `token`, `baseline` (ascending) and `extra` as what differs from
/// `of` in the checkout at canonical `root`.
pub fn remember(
    root: &Path,
    of: &Of,
    token: Token,
    baseline: Vec<String>,
    extra: serde_json::Value,
) {
    let Some(root) = root.to_str() else {
        return cannot_save(format_args!(
            "the record (the checkout's path is not UTF-8)"
        ));
    };
    let record = Record {
        v: VERSION,
        root: root.to_owned(),
        of: of.of.clone(),
        token,
        baseline: Some(baseline),
        extra: Some(extra),
    };
    record.write(&of.key);
}

/// `line` on stderr, once per process for each `seen`.
pub(super) fn warn(seen: &AtomicBool, line: &str) {
    #[cfg(test)]
    crate::tests::NOTES.with_borrow(|n| n.lock().unwrap().push(line.to_owned()));
    if !seen.swap(true, Ordering::Relaxed) {
        eprintln!("warning: {line}");
    }
}

/// `line` on stderr, as a warning, every time: a server's.
pub fn say(line: &str) {
    #[cfg(test)]
    crate::tests::NOTES.with_borrow(|n| n.lock().unwrap().push(line.to_owned()));
    eprintln!("warning: {line}");
}

/// What a server prints once watchman answers after a warning.
pub const ANSWERS_AGAIN: &str = "each query re-checks only what watchman reports again";

/// [`ANSWERS_AGAIN`] on stderr, as a note.
pub fn answers_again() {
    #[cfg(test)]
    crate::tests::NOTES.with_borrow(|n| n.lock().unwrap().push(ANSWERS_AGAIN.to_owned()));
    eprintln!("note: {ANSWERS_AGAIN}");
}

pub fn cannot(why: &str) -> String {
    format!("watchman cannot say what changed ({why}); checked every file")
}

/// Every file was checked once, for reason `why`, and a token taken: later
/// searches ask watchman, or a server's next query if `served`.
fn once(why: &str, served: bool) -> String {
    let next = match served {
        true => "the next query asks watchman",
        false => "later searches ask watchman",
    };
    format!("checked every file once ({why}); {next}")
}

/// No usable answer from watchman on a tree worth asking about: the line.
pub fn cannot_answer(why: &str) -> String {
    static SEEN: AtomicBool = AtomicBool::new(false);
    let line = cannot(why);
    warn(&SEEN, &line);
    line
}

/// This search, or server start if `served`, took a token, but checked
/// every file for its baseline.
fn checked_once(why: &str, served: bool) {
    static SEEN: AtomicBool = AtomicBool::new(false);
    warn(&SEEN, &once(why, served));
}

/// The record could not be written.
fn cannot_save(what: std::fmt::Arguments<'_>) {
    static SEEN: AtomicBool = AtomicBool::new(false);
    warn(
        &SEEN,
        &format!("cannot save {what}; the next search checks again"),
    );
}

/// More paths may differ than re-checking them beats the stat pass.
pub fn over(cap: usize) {
    static SEEN: AtomicBool = AtomicBool::new(false);
    warn(
        &SEEN,
        &format!(
            "more than {cap} paths may differ from the index; checked every file (re-run index)"
        ),
    );
}

/// A server's query after more than `cap` paths changed: the line.
pub fn over_since(cap: usize) -> String {
    format!("more than {cap} paths changed since the last query; checked every file")
}

#[cfg(test)]
mod tests;
