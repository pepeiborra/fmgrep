//! Change sources: which corpus paths changed since a mark taken before the
//! index walk or at a search's checkpoint, so a load re-checks only those.
//! `docs.journal` holds the marks, the symlinked docs (`recheck`), the dirs
//! holding a dangling symlink (`relist`) and what differed at the marks
//! (`named`).

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::walk::WalkNotes;

#[cfg(unix)]
mod watchman;

pub const DOCS_JOURNAL: &str = "docs.journal";
const VERSION: u32 = 1;
const VAR: &str = "FMGREP_JOURNAL";
/// Below this many docs the stat pass beats a source's fixed cost.
const MIN_DOCS: usize = 50_000;

/// Whether an index marks and a load asks.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Knob {
    /// Where it pays off.
    Auto,
    #[default]
    Off,
    /// Even where the stat pass is cheaper.
    On,
}

impl Knob {
    /// `FMGREP_JOURNAL`: unset or empty, `off` or `on`.
    pub fn read() -> Result<Self, String> {
        #[cfg(test)]
        if let Some(raw) = crate::tests::JOURNAL.get() {
            return Ok(Self::of(Some(raw)));
        }
        crate::env::choice(VAR, &["off", "on"]).map(Self::of)
    }

    fn of(raw: Option<&'static str>) -> Self {
        match raw {
            None => Self::Auto,
            Some("off") => Self::Off,
            Some(_) => Self::On,
        }
    }
}

/// Named paths past this cost more than the stat pass of `docs` docs.
fn named_cap(docs: usize) -> usize {
    (docs / 2).max(4096)
}

/// A source's whole answer for `docs` docs: 2 us a doc within 1-5 s.
fn budget(docs: usize) -> Duration {
    let per_doc = u32::try_from(docs).map_or(Duration::MAX, |n| Duration::from_micros(2) * n);
    per_doc.clamp(Duration::from_secs(1), Duration::from_secs(5))
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

/// A filesystem change record, marked before the walk and asked at load.
pub trait ChangeSource {
    fn kind(&self) -> &'static str;
    /// A token for now; `None` when `root` is not covered.
    fn mark(&self, root: &Path) -> Option<Value>;
    fn since(&self, ask: &Ask<'_>) -> Since;
}

/// Every source available now, unless `knob` is off.
fn available(knob: Knob) -> Vec<Box<dyn ChangeSource>> {
    let mut all: Vec<Box<dyn ChangeSource>> = Vec::new();
    #[cfg(unix)]
    if knob != Knob::Off
        && let Some(w) = watchman::Watchman::find(budget(usize::MAX))
    {
        all.push(Box::new(w));
    }
    all
}

/// The source that recorded `token`, within the budget for `docs` docs.
fn source(token: &Token, docs: usize) -> Option<Box<dyn ChangeSource>> {
    match token.kind.as_str() {
        #[cfg(unix)]
        "watchman" => watchman::Watchman::of(&token.token, budget(docs))
            .map(|w| Box::new(w) as Box<dyn ChangeSource>),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Token {
    kind: String,
    token: Value,
}

fn token(source: &dyn ChangeSource, root: &Path) -> Option<Token> {
    source.mark(root).map(|token| Token {
        kind: source.kind().to_owned(),
        token,
    })
}

/// A mark per source, unless `knob` is off, before the index walk of `root`.
pub fn marks(root: &Path, knob: Knob) -> Vec<Token> {
    match root.canonicalize() {
        Ok(root) => available(knob)
            .iter()
            .filter_map(|s| token(s.as_ref(), &root))
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Journal {
    v: u32,
    root: String,
    tokens: Vec<Token>,
    recheck: Vec<String>,
    relist: Vec<String>,
    named: Vec<String>,
    #[serde(skip)]
    knob: Knob,
}

/// What a source named, for the files layout to re-check.
pub struct Replay {
    pub kind: &'static str,
    /// Ascending, deduped.
    pub paths: Vec<String>,
    pub relist: Vec<String>,
}

impl Journal {
    /// The journal of `walk` from canonical `root`; nothing without marks.
    pub fn write_new(
        output: &Path,
        root: &str,
        tokens: Vec<Token>,
        walk: &WalkNotes,
    ) -> Result<(), String> {
        if tokens.is_empty() {
            return Ok(());
        }
        let mut recheck = walk.links.clone();
        recheck.sort();
        let relist = walk
            .dirs
            .iter()
            .filter(|(_, s)| s.is_none())
            .map(|(rel, _)| rel.clone())
            .collect();
        let journal = Self {
            v: VERSION,
            root: root.to_owned(),
            tokens,
            recheck,
            relist,
            named: Vec::new(),
            knob: Knob::Off,
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
        let journal: Self = serde_json::from_value(v).map_err(tampered)?;
        Ok(Some(Self { knob, ..journal }))
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

    /// Fresh marks for the next checkpoint, and the first vouching source's
    /// replay with `recheck` and `named`; no marks when no source pays off
    /// for `docs` docs.
    pub fn ask(&self, root: &Path, docs: usize) -> (Vec<Token>, Option<Replay>) {
        if Path::new(&self.root) != root {
            return (Vec::new(), None);
        }
        let forced = self.knob == Knob::On;
        let mut asked = Vec::new();
        for t in &self.tokens {
            match source(t, docs) {
                Some(s) if forced || docs >= MIN_DOCS => asked.push((t, s)),
                Some(s) => note(format_args!(
                    "{} does not pay off for {docs} files",
                    s.kind()
                )),
                None => {}
            }
        }
        let marks = asked
            .iter()
            .filter_map(|(_, s)| token(s.as_ref(), root))
            .collect();
        for (t, s) in asked {
            let ask = Ask {
                root,
                token: &t.token,
            };
            match s.since(&ask) {
                Since::Paths(paths) if paths.len() > named_cap(docs) => {
                    note(format_args!(
                        "{} named {} paths, too many",
                        s.kind(),
                        paths.len()
                    ));
                }
                Since::Paths(mut paths) => {
                    paths.extend(self.recheck.iter().chain(&self.named).cloned());
                    paths.sort_unstable();
                    paths.dedup();
                    let relist = self.relist.clone();
                    let replay = Replay {
                        kind: s.kind(),
                        paths,
                        relist,
                    };
                    return (marks, Some(replay));
                }
                Since::Unknown(why) => note(format_args!("{} cannot vouch ({why})", s.kind())),
            }
        }
        (marks, None)
    }

    /// Rewrite under `index_dir` with `marks`, which saw `named` differ.
    pub fn checkpoint(mut self, index_dir: &Path, marks: Vec<Token>, named: Vec<String>) {
        if marks.is_empty() {
            return;
        }
        (self.tokens, self.named) = (marks, named);
        if let Err(e) = self.write(index_dir)
            && crate::verbose::verbose()
        {
            eprintln!("note: no checkpoint: {e}");
        }
    }
}

/// The warning for a source that missed its deadline.
fn late(kind: &str) -> String {
    format!("warning: {kind} did not answer in time; set {VAR}=off to skip it")
}

#[cfg(test)]
thread_local! {
    /// Late warnings on this thread.
    pub static WARNED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Warns, verbose or not, that `kind` missed its deadline.
#[cfg_attr(not(unix), allow(dead_code))]
fn warn_late(kind: &str) {
    let warning = late(kind);
    eprintln!("{warning}");
    #[cfg(test)]
    WARNED.with_borrow_mut(|w| w.push(warning));
}

/// Why a source is not used, under `--verbose`.
fn note(why: std::fmt::Arguments<'_>) {
    if crate::verbose::verbose() {
        eprintln!("note: {why}");
    }
}

#[cfg(test)]
mod tests;
