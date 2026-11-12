//! Change sources: which corpus paths changed since a token taken before
//! the index walk or at a search's checkpoint, so a load re-checks only
//! those. `docs.journal` holds the tokens and what differed at them
//! (`named`). When no source can say, the search checks every file and
//! says why, once per process.

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

/// Whether an index takes a token and a load asks.
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

/// Index time: detection, connects and calls.
const MARK_WAIT: Duration = Duration::from_secs(5);
/// A load's token and question, small next to the stat pass it would
/// spare.
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

/// Named paths past this cost more than the stat pass of `docs` docs.
fn named_cap(docs: usize) -> usize {
    (docs / 2).max(4096)
}

/// What a source vouches for since its token.
pub enum Since {
    /// Only these corpus-relative paths may differ; a directory stands for
    /// its subtree, `""` for the root.
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

/// A token per source, unless `knob` is off, before the index walk of
/// `root`; why there is none only under `--verbose`.
pub fn marks(root: &Path, knob: Knob) -> Vec<Token> {
    let Ok(root) = root.canonicalize() else {
        return Vec::new();
    };
    if knob == Knob::Off {
        return Vec::new();
    }
    #[cfg(unix)]
    let found = watchman::Watchman::find(MARK_WAIT, &Policy::here()).and_then(|w| token(&w, &root));
    #[cfg(not(unix))]
    let found = Err("no change source on this platform".to_owned());
    match found {
        Ok(token) => vec![token],
        Err(why) => {
            note(format_args!("no watchman token ({why})"));
            Vec::new()
        }
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
    /// The journal of an index of canonical `root`; nothing without tokens.
    pub fn write_new(output: &Path, root: &str, tokens: Vec<Token>) -> Result<(), String> {
        if tokens.is_empty() {
            return Ok(());
        }
        let journal = Self {
            v: VERSION,
            root: root.to_owned(),
            tokens,
            named: Vec::new(),
        };
        journal.write(output)
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
    /// source's replay with `named`, or why none vouches, for a load of
    /// `docs` docs.
    pub fn ask(&self, root: &Path, docs: usize) -> (Vec<Token>, Result<Replay, String>) {
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
                Since::Paths(paths) if paths.len() > named_cap(docs) => {
                    why = format!(
                        "{} reported {} changed paths, too many",
                        s.kind(),
                        paths.len()
                    );
                }
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

#[cfg(test)]
thread_local! {
    /// The warnings given on this thread, every one.
    pub static NOTES: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// `line` on stderr, once per process for each `seen`.
fn warn(seen: &AtomicBool, line: std::fmt::Arguments<'_>) {
    #[cfg(test)]
    NOTES.with_borrow_mut(|n| n.push(line.to_string()));
    if !seen.swap(true, Ordering::Relaxed) {
        eprintln!("warning: {line}");
    }
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
