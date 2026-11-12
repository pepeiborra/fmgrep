//! Change sources: which corpus paths changed since a token taken at a
//! search's checkpoint, so a load re-checks only those. `docs.journal`
//! holds the tokens and what differed at them (`named`). The first search
//! after an index takes the first token and checks every file; the index
//! only asks watchman to watch the tree once its walk has listed the
//! files, without waiting for the crawl.
//! When no source can say, the search checks every file and says why, once
//! per process.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[cfg(unix)]
mod watchman;

pub const DOCS_JOURNAL: &str = "docs.journal";
const VERSION: u32 = 1;
/// Below this many docs the stat pass beats a source's fixed cost.
const MIN_DOCS: usize = 50_000;

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

/// A load's token and question, small next to the stat pass it would
/// spare; also what `index` gives watchman to take its request to watch
/// the tree.
const ASK_WAIT: Duration = Duration::from_millis(100);

/// From how many docs a load asks watchman, and, in tests, the one socket
/// it may reach: read on the thread that opens the index.
#[derive(Clone, Debug)]
pub struct Policy {
    from: Option<usize>,
    #[cfg(test)]
    sock: Option<std::path::PathBuf>,
}

impl Policy {
    pub fn here() -> Self {
        #[cfg(test)]
        return Self {
            from: crate::tests::ASK_FROM.get(),
            sock: crate::tests::WATCHMAN.with_borrow(Clone::clone),
        };
        #[cfg(not(test))]
        return Self {
            from: match Knob::read() {
                Ok(Knob::Auto) => Some(MIN_DOCS),
                Ok(Knob::On) => Some(0),
                Ok(Knob::Off) | Err(_) => None,
            },
        };
    }

    /// Whether a load of `docs` docs asks.
    pub fn due(&self, docs: usize) -> bool {
        self.from.is_some_and(|from| docs >= from)
    }
}

/// Re-checking a named path costs about 7 µs, the stat pass about 1 µs a
/// doc: past this many named paths the stat pass is cheaper. Never below
/// 64, where either costs microseconds.
pub fn named_cap(docs: usize) -> usize {
    (docs / 8).max(64)
}

/// What a source vouches for since its token.
pub enum Since {
    /// Only these corpus-relative paths may differ; a named directory is
    /// listed again, one level.
    Paths(Vec<String>),
    /// Why it cannot vouch.
    Unknown(String),
}

/// One load-time question to a source.
pub struct Ask<'a> {
    /// Canonical.
    pub root: &'a Path,
    pub token: &'a Value,
}

/// A filesystem change record, asked for a token before the walk and
/// asked what changed since it at load.
pub trait ChangeSource {
    fn kind(&self) -> &'static str;
    /// A token for now, or why there is none.
    fn mark(&self, root: &Path) -> Result<Value, String>;
    fn since(&self, ask: &Ask<'_>) -> Since;
}

/// The source that took `token`, within [`ASK_WAIT`].
fn source(token: &Token) -> Option<Box<dyn ChangeSource>> {
    match token.kind.as_str() {
        #[cfg(unix)]
        "watchman" => watchman::Watchman::of(&token.token, ASK_WAIT)
            .map(|w| Box::new(w) as Box<dyn ChangeSource>),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Token {
    kind: String,
    token: Value,
}

fn token(source: &dyn ChangeSource, root: &Path) -> Result<Token, String> {
    source.mark(root).map(|token| Token {
        kind: source.kind().to_owned(),
        token,
    })
}

/// Once the index walk of `root` has listed `files` files, before it reads
/// them, where a search of as many docs would ask watchman: have watchman
/// watch the tree, waiting at most [`ASK_WAIT`] for its reply, which comes
/// only once its crawl ends; the watch stays, and watchman crawls the tree
/// while the index reads it. Takes no token. Why not, or that the crawl
/// goes on, only under `--verbose`.
pub fn request_watch(root: &Path, files: usize) {
    let policy = Policy::here();
    if !policy.due(files) {
        return;
    }
    let Ok(root) = root.canonicalize() else {
        return;
    };
    #[cfg(unix)]
    let asked = watchman::Watchman::find(ASK_WAIT, &policy).and_then(|w| w.watch(&root).map(drop));
    #[cfg(not(unix))]
    let asked: Result<(), String> = Err("no change source on this platform".to_owned());
    if let Err(why) = asked {
        note(format_args!("{why}"));
    }
}

/// A first token for canonical `root`, for a search with no journal, or
/// why there is none.
pub fn first(root: &Path, policy: &Policy) -> Result<Vec<Token>, String> {
    #[cfg(unix)]
    return watchman::Watchman::find(ASK_WAIT, policy)
        .and_then(|w| token(&w, root))
        .map(|token| vec![token]);
    #[cfg(not(unix))]
    {
        let _ = (root, policy);
        Err("watchman is not supported on this platform".to_owned())
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Journal {
    v: u32,
    root: String,
    tokens: Vec<Token>,
    named: Vec<String>,
}

/// What a source named, for the files layout to re-check.
pub struct Replay {
    pub kind: &'static str,
    /// Ascending, deduped.
    pub paths: Vec<String>,
}

impl Journal {
    /// A journal of canonical `root` with no token yet, for the first
    /// checkpoint.
    pub fn new(root: &str) -> Self {
        Self {
            v: VERSION,
            root: root.to_owned(),
            tokens: Vec::new(),
            named: Vec::new(),
        }
    }

    /// `None` when absent, of another version, or under [`Knob::Off`];
    /// malformed is refused.
    pub fn open(index_dir: &Path, knob: Knob) -> Result<Option<Self>, String> {
        if knob == Knob::Off {
            return Ok(None);
        }
        let path = index_dir.join(DOCS_JOURNAL);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("error: cannot read {}: {e}", path.display())),
        };
        let tampered = |e: serde_json::Error| {
            format!(
                "error: {} is malformed ({e}); refusing a tampered index",
                path.display()
            )
        };
        let v: Value = serde_json::from_slice(&bytes).map_err(tampered)?;
        if v.get("v").and_then(Value::as_u64) != Some(u64::from(VERSION)) {
            return Ok(None);
        }
        serde_json::from_value(v).map(Some).map_err(tampered)
    }

    /// Atomic: a crash leaves the old file or the new one.
    fn write(&self, dir: &Path) -> Result<(), String> {
        let path = dir.join(DOCS_JOURNAL);
        let tmp = dir.join(format!(".{DOCS_JOURNAL}.{}", std::process::id()));
        let failed =
            |e: &dyn std::fmt::Display| format!("error: cannot write {}: {e}", path.display());
        let bytes = serde_json::to_vec(self).map_err(|e| failed(&e))?;
        std::fs::write(&tmp, bytes)
            .and_then(|()| std::fs::rename(&tmp, &path))
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                failed(&e)
            })
    }

    /// Fresh tokens for the next checkpoint, and the first vouching
    /// source's replay with `named`, or why none vouches.
    pub fn ask(&self, root: &Path) -> (Vec<Token>, Result<Replay, String>) {
        if Path::new(&self.root) != root {
            return (Vec::new(), Err("the index is of another root".to_owned()));
        }
        let asked: Vec<_> = self
            .tokens
            .iter()
            .filter_map(|t| Some((t, source(t)?)))
            .collect();
        let (mut marks, mut unmarked) = (Vec::new(), None);
        for (_, s) in &asked {
            match token(s.as_ref(), root) {
                Ok(token) => marks.push(token),
                Err(why) => {
                    note(format_args!("no new {} token ({why})", s.kind()));
                    unmarked.get_or_insert(why);
                }
            }
        }
        let mut why = "the index holds no token of a known source".to_owned();
        for (t, s) in asked {
            let ask = Ask {
                root,
                token: &t.token,
            };
            match s.since(&ask) {
                Since::Paths(mut paths) => {
                    paths.extend(self.named.iter().cloned());
                    paths.sort_unstable();
                    paths.dedup();
                    let replay = Replay {
                        kind: s.kind(),
                        paths,
                    };
                    return (marks, Ok(replay));
                }
                Since::Unknown(unknown) => why = unknown,
            }
        }
        (marks, Err(unmarked.unwrap_or(why)))
    }

    /// Rewrite under `index_dir` with `marks`, which saw `named` differ.
    pub fn checkpoint(mut self, index_dir: &Path, marks: Vec<Token>, named: Vec<String>) {
        if marks.is_empty() {
            return;
        }
        (self.tokens, self.named) = (marks, named);
        if let Err(e) = self.write(index_dir) {
            note(format_args!("no checkpoint: {e}"));
        }
    }
}

/// `line` on stderr, once per process for each `seen`.
pub(super) fn warn(seen: &AtomicBool, line: std::fmt::Arguments<'_>) {
    #[cfg(test)]
    crate::tests::NOTES.with_borrow(|n| n.lock().unwrap().push(line.to_string()));
    if !seen.swap(true, Ordering::Relaxed) {
        eprintln!("warning: {line}");
    }
}

/// This search took a token, but checked every file to learn what differs.
pub fn checked_once(why: &str) {
    static SEEN: AtomicBool = AtomicBool::new(false);
    warn(
        &SEEN,
        format_args!("checked every file once ({why}); later searches ask watchman"),
    );
}

/// No source can say what changed on a tree worth asking about.
pub fn cannot_answer(why: &str) {
    static SEEN: AtomicBool = AtomicBool::new(false);
    warn(
        &SEEN,
        format_args!("watchman cannot say what changed ({why}); checked every file"),
    );
}

/// Why a source is not used, under `--verbose`.
fn note(why: std::fmt::Arguments<'_>) {
    if crate::verbose::verbose() {
        eprintln!("note: {why}");
    }
}

#[cfg(test)]
mod tests;
