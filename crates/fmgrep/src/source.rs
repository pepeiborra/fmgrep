//! What the files layout knows of each file since indexing: fresh, stale
//! (scanned in full), gone (excluded), ignored (excluded silently: an ignore
//! rule now skips it) or added (not in the manifest; scanned in full).
//! Watchman and the saved baseline, when they can say, name what to
//! re-check instead of the stat pass and discovery.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use rayon::prelude::*;

use crate::Stamp;

mod added;
mod journal;
mod recorded;

pub use journal::{
    Index, Of, Remembered, Token, record_path, recorded, remember, remembered, request_watch,
};
pub use recorded::{Recorded, write_walk};
use recorded::{direct, outermost};

/// Per-doc mtimes and UTF-16 file lengths of a files-layout index, at the
/// index root.
pub const DOCS_STAMPS: &str = "docs.stamps";

/// Write `docs.stamps`: one little-endian i64 mtime (ns) per doc, then a
/// little-endian `(u64 id, u64 file length)` per doc decoded from UTF-16.
pub fn write_stamps(output: &Path, mtimes: &[i64], file_lens: &[(u64, u64)]) -> Result<(), String> {
    let path = output.join(DOCS_STAMPS);
    let failed = |e: std::io::Error| format!("error: cannot write {}: {e}", path.display());
    let file = std::fs::File::create(&path).map_err(failed)?;
    let mut out = std::io::BufWriter::new(file);
    for m in mtimes {
        out.write_all(&m.to_le_bytes()).map_err(failed)?;
    }
    for (id, len) in file_lens {
        out.write_all(&id.to_le_bytes()).map_err(failed)?;
        out.write_all(&len.to_le_bytes()).map_err(failed)?;
    }
    out.flush().map_err(failed)
}

/// The mapped `docs.stamps`.
pub struct Stamps {
    /// None: no indexed doc.
    map: Option<memmap2::Mmap>,
    /// `(id, file length)` per doc decoded from UTF-16, ascending.
    utf16: Vec<(u64, u64)>,
}

impl Stamps {
    /// Map `docs.stamps` under `index_dir`; a size or a UTF-16 table other
    /// than [`write_stamps`] writes for `num_docs` is refused.
    pub fn open(index_dir: &Path, num_docs: usize) -> Result<Self, String> {
        let path = index_dir.join(DOCS_STAMPS);
        let failed = |e: std::io::Error| format!("error: cannot read {}: {e}", path.display());
        let file = std::fs::File::open(&path).map_err(failed)?;
        // Safety: the index is read-only while loaded, as `blobs.shard` is.
        let map = unsafe { memmap2::Mmap::map(&file) }.map_err(failed)?;
        let tampered = |why: String| {
            format!(
                "error: {} {why}, for {num_docs} doc(s); refusing a tampered index",
                path.display()
            )
        };
        let want = num_docs as u64 * 8;
        let len = map.len() as u64;
        if len < want || !(len - want).is_multiple_of(16) {
            return Err(tampered(format!(
                "holds {len} byte(s), want {want} plus 16 per UTF-16 doc"
            )));
        }
        let word = |at: &[u8]| u64::from_le_bytes(at.try_into().expect("8 bytes"));
        let (pairs, _) = map[want as usize..].as_chunks::<16>();
        let utf16: Vec<(u64, u64)> = pairs
            .iter()
            .map(|p| (word(&p[..8]), word(&p[8..])))
            .collect();
        let ordered = utf16.windows(2).all(|w| w[0].0 < w[1].0);
        if !ordered || utf16.last().is_some_and(|&(id, _)| id >= num_docs as u64) {
            return Err(tampered(
                "lists UTF-16 docs out of order or range".to_string(),
            ));
        }
        Ok(Self {
            map: Some(map),
            utf16,
        })
    }

    /// The stamps of no doc: the files of a search that no index describes.
    pub fn none() -> Self {
        Self {
            map: None,
            utf16: Vec::new(),
        }
    }

    pub fn get(&self, doc: usize) -> i64 {
        let at = doc * 8;
        let map = self.map.as_deref().unwrap_or_default();
        i64::from_le_bytes(map[at..at + 8].try_into().expect("8-byte slot"))
    }

    /// Doc `doc`'s file length when it was decoded from UTF-16.
    fn utf16(&self, doc: usize) -> Option<u64> {
        let at = self
            .utf16
            .binary_search_by_key(&(doc as u64), |&(id, _)| id);
        at.ok().map(|at| self.utf16[at].1)
    }

    /// Whether `meta` still matches doc `doc` of recorded text length `len`.
    fn holds(&self, doc: usize, meta: &std::fs::Metadata, len: u64) -> bool {
        Stamp::of(meta)
            == Stamp {
                len: self.utf16(doc).unwrap_or(len),
                mtime_ns: self.get(doc),
            }
    }
}

/// Per-doc states, plus [`WARNED`] once a doc's exclusion was reported.
const FRESH: u8 = 0;
const STALE: u8 = 1;
const GONE: u8 = 2;
const IGNORED: u8 = 3;
const KIND: u8 = 3;
const WARNED: u8 = 4;

/// Docs above this are statted on several threads.
const PARALLEL_STAT_MIN: usize = 64;
const STAT_THREADS: usize = 16;

/// What stats and reads learned about each doc.
struct Known {
    root: PathBuf,
    stamps: Stamps,
    state: Vec<AtomicU8>,
    /// Docs not [`FRESH`].
    changed: AtomicUsize,
    /// Bumped on every change of kind, and of the added set.
    epoch: AtomicUsize,
    /// The index dir, canonical: never an added file.
    exclude: Option<PathBuf>,
    added: Mutex<added::Added>,
    recorded: Option<Arc<Recorded>>,
    /// The walk's rule, loaded on first use.
    rules: Mutex<Option<ignore::IncrementalIgnore>>,
    /// Ignore files the index did not record, seen by this load: the
    /// baseline keeps them, so later loads walk their dirs again. A set: a
    /// server finds the same ones again at each query that walks there.
    new_ignores: Mutex<std::collections::BTreeSet<String>>,
    /// Whether every doc was statted and every added file found.
    complete: AtomicBool,
    /// Whether a server asks watchman again before each query: its
    /// warnings then speak of queries, not searches.
    each_query: bool,
}

impl Known {
    fn kind(&self, id: usize) -> u8 {
        self.state[id].load(Ordering::Acquire) & KIND
    }

    fn set(&self, id: usize, kind: u8) {
        self.update(id, kind, true);
    }

    /// [`Self::set`] unless `id` is [`IGNORED`]: whether it applied.
    fn settle(&self, id: usize, kind: u8) -> bool {
        self.update(id, kind, false)
    }

    fn update(&self, id: usize, kind: u8, force: bool) -> bool {
        let Ok(old) = self.state[id].try_update(Ordering::AcqRel, Ordering::Acquire, |s| {
            (force || s & KIND != IGNORED).then_some(s & WARNED | kind)
        }) else {
            return false;
        };
        // `changed` moves before `epoch`: a reader that sees the new epoch
        // sees the new count.
        match (old & KIND == FRESH, kind == FRESH) {
            (true, false) => {
                self.changed.fetch_add(1, Ordering::AcqRel);
            }
            (false, true) => {
                self.changed.fetch_sub(1, Ordering::AcqRel);
            }
            _ => {}
        }
        if old & KIND != kind {
            self.epoch.fetch_add(1, Ordering::AcqRel);
        }
        true
    }

    /// Mark `id` gone unless ignored; the first exclusion of a doc per
    /// process warns.
    fn exclude(&self, id: usize, path: &Path, why: &str) {
        let first = self.settle(id, GONE)
            && self.state[id].fetch_or(WARNED, Ordering::AcqRel) & WARNED == 0;
        if first {
            eprintln!("warning: excluding {}: {why}", path.display());
        }
    }

    /// Stat `id` against its stamp, an ignored doc only if `force`: why it
    /// is gone, if it is.
    fn stat(&self, id: usize, files: &[String], lens: &[u64], force: bool) -> Option<String> {
        let (kind, why) = match std::fs::metadata(self.root.join(&files[id])) {
            Ok(meta) if self.stamps.holds(id, &meta, lens[id]) => (FRESH, None),
            Ok(_) => (STALE, None),
            Err(e) => (
                GONE,
                Some(match e.kind() {
                    std::io::ErrorKind::NotFound => "is missing".to_string(),
                    _ => format!("cannot stat ({e})"),
                }),
            ),
        };
        if self.update(id, kind, force) {
            why
        } else {
            None
        }
    }

    /// Whether the walk's rule skips root-relative `rel`.
    fn ignored(&self, rel: &str, is_dir: bool) -> bool {
        let mut rules = self.rules.lock().unwrap_or_else(PoisonError::into_inner);
        rules
            .get_or_insert_with(|| crate::walk::rules(&self.root))
            .matched(rel, is_dir)
            .is_ignore()
    }

    /// Reload the walk's rule at its next use.
    fn reload_rules(&self) {
        *self.rules.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// Stat `ids` (ascending), ignored ones only if `force`, then warn about
    /// the gone ones in order, or once when over half the docs are.
    fn stat_ids(&self, ids: &[usize], files: &[String], lens: &[u64], force: bool) {
        let gone = parallel(ids, |&id| self.stat(id, files, lens, force));
        let n = gone.iter().flatten().count();
        if n * 2 > self.state.len() {
            eprintln!(
                "warning: excluding {n} of {} indexed files, missing under {}",
                self.state.len(),
                self.root.display()
            );
        }
        for (&id, why) in ids.iter().zip(gone) {
            match why {
                Some(_) if n * 2 > self.state.len() => {
                    self.state[id].fetch_or(WARNED, Ordering::AcqRel);
                }
                Some(why) => self.exclude(id, &self.root.join(&files[id]), &why),
                None => {}
            }
        }
    }

    fn stat_all(&self, files: &[String], lens: &[u64], force: bool) {
        let ids: Vec<usize> = (0..self.state.len()).collect();
        self.stat_ids(&ids, files, lens, force);
    }

    /// Take root-relative `paths` (any order) as all that may differ in
    /// ascending `files`, instead of the stat pass and discovery: the number
    /// kept, or Err when they cost more than the stat pass (nothing is
    /// touched then: past [`journal::named_cap`]).
    ///
    /// A dir the index did not record, or a path under one, counts once, as
    /// the topmost such dir, which discovery walks: its added files become
    /// those the walk finds now. A hidden one is skipped. A named doc is
    /// statted. A named recorded dir, and the parent of any other named path
    /// that is not hidden, are listed again, one level: the docs in each are
    /// statted, a recorded subdir that moved is listed in turn, one that
    /// went has its docs statted, a new one is walked. Then the ignore
    /// inputs, as discovery does.
    fn replay(
        &self,
        recorded: &Arc<Recorded>,
        paths: &[String],
        files: &[String],
        lens: &[u64],
    ) -> Result<usize, ()> {
        let root = &self.root;
        let (mut ids, mut dirs, mut scopes, mut kept) = (Vec::new(), Vec::new(), Vec::new(), 0);
        let (mut named_ignores, mut new_dirs) = (Vec::new(), std::collections::HashSet::new());
        for rel in paths {
            let (parent, name) = rel.rsplit_once('/').unwrap_or(("", rel));
            if rel.is_empty() || self.excluded(root, rel) {
                continue;
            }
            if !recorded.is_dir(parent) {
                let top = unrecorded(Some(recorded), rel);
                if !top.rsplit('/').next().is_some_and(|n| n.starts_with('.')) {
                    new_dirs.insert(top);
                }
                continue;
            }
            if let Ok(id) = files.binary_search_by(|f| f.as_str().cmp(rel)) {
                ids.push(id);
            } else if recorded.is_dir(rel) {
                dirs.push(rel.clone());
            } else if !name.starts_with('.') {
                if std::fs::symlink_metadata(root.join(rel)).is_ok_and(|m| m.is_dir()) {
                    new_dirs.insert(rel.as_str());
                    continue;
                }
                dirs.push(parent.to_owned());
            }
            kept += 1;
            if let Some(dir) = crate::walk::ignore_file_dir(rel) {
                scopes.push(dir.to_owned());
                if !recorded.is_ignore(rel) && root.join(rel).exists() {
                    named_ignores.push(rel.clone());
                }
            }
        }
        self.keep_ignores(named_ignores);
        let kept = kept + new_dirs.len();
        if kept > journal::named_cap(files.len()) {
            return Err(());
        }
        scopes.extend(new_dirs.into_iter().map(str::to_owned));
        let seek = |key: &str| files.partition_point(|f| f.as_str() < key);
        let (mut listed, mut seen) = (Vec::new(), std::collections::HashSet::new());
        while let Some(dir) = dirs.pop() {
            if !seen.insert(dir.clone()) {
                continue;
            }
            ids.extend(direct(&dir, files.len(), |i| files[i].as_str(), seek));
            for (child, stamp) in recorded.child_dirs(&dir) {
                match std::fs::symlink_metadata(root.join(child)) {
                    Ok(m) if m.is_dir() && Stamp::of(&m) == stamp => {}
                    Ok(m) if m.is_dir() => dirs.push(child.to_owned()),
                    _ => ids.extend(added::docs_under(files, child)),
                }
            }
            listed.push(if dir.is_empty() {
                root.clone()
            } else {
                root.join(dir)
            });
        }
        ids.sort_unstable();
        ids.dedup();
        self.stat_ids(&ids, files, lens, false);
        let prune = Arc::clone(recorded);
        let walked = crate::walk::walk_changed(root, listed.clone(), self.exclude.as_deref(), {
            move |rel| prune.is_dir(rel)
        });
        self.add(&self.text_only(root, added::unlisted(walked, files)));
        scopes.extend(self.apply_ignores(recorded.ignore_scopes(root, &listed)));
        self.discover_scopes(scopes, files, lens);
        self.complete.store(true, Ordering::Release);
        Ok(kept)
    }

    /// Keep `found` (ignore files) for the baseline.
    fn keep_ignores(&self, found: Vec<String>) {
        let mut ignores = self
            .new_ignores
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        ignores.extend(found);
    }

    /// The dirs of `changes` to walk again. Keeps its new ignore files for
    /// the baseline, and warns when any ignore file moved or is new
    /// since the index: every load walks the dirs they govern again.
    fn apply_ignores(&self, changes: recorded::IgnoreScopes) -> Vec<String> {
        self.keep_ignores(changes.found);
        let new = self
            .new_ignores
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len();
        let n = changes.moved + new;
        if n > 0 {
            ignores_changed(n, self.each_query);
        }
        changes.scopes
    }

    /// [`Self::discover`] of the outermost of root-relative `scopes`.
    fn discover_scopes(&self, scopes: Vec<String>, files: &[String], lens: &[u64]) {
        let root = &self.root;
        for scope in outermost(scopes) {
            self.discover(root, &root.join(scope), files, lens);
        }
    }
}

/// `n` ignore files moved or are new since the index, which every load
/// applies by walking the dirs they govern again; worded for a server's
/// queries if `served`.
fn ignores_changed(n: usize, served: bool) {
    static SEEN: AtomicBool = AtomicBool::new(false);
    let (files, dirs) = match n {
        1 => ("file", "its directory"),
        _ => ("files", "their directories"),
    };
    let each = if served { "query" } else { "search" };
    journal::warn(
        &SEEN,
        &format!(
            "{n} ignore {files} changed since the index; each {each} reads {dirs} again \
             (re-run index to make searches fast again)"
        ),
    );
}

/// Added file `rel`, or its topmost ancestor the walk did not record: a
/// replay walks that dir for any path under it.
fn unrecorded<'a>(recorded: Option<&Recorded>, rel: &'a str) -> &'a str {
    let new = |i: &usize| recorded.is_some_and(|r| !r.is_dir(&rel[..*i]));
    match rel.match_indices('/').map(|(i, _)| i).find(new) {
        Some(i) => &rel[..i],
        None => rel,
    }
}

/// Under `--verbose`: what a re-check took from watchman, and its cost,
/// the ask from `asked` to `applied` and the re-check since.
fn timed(what: std::fmt::Arguments<'_>, asked: std::time::Instant, applied: std::time::Instant) {
    if crate::verbose::verbose() {
        let (ask, apply) = (applied - asked, applied.elapsed());
        eprintln!("note: {what} (ask {ask:.1?}, apply {apply:.1?})");
    }
}

/// `f` of each item, in order; on a pool of [`STAT_THREADS`] above [`PARALLEL_STAT_MIN`] items.
fn parallel<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    if items.len() > PARALLEL_STAT_MIN
        && let Ok(pool) = rayon::ThreadPoolBuilder::new()
            .num_threads(STAT_THREADS)
            .build()
    {
        return pool.install(|| items.par_iter().map(&f).collect());
    }
    items.iter().map(f).collect()
}

/// Docs a query treats apart from the index's answer, each ascending.
#[derive(Debug, Default, PartialEq)]
pub struct Snapshot {
    pub stale: Vec<usize>,
    pub gone: Vec<usize>,
    /// Live added docs: `num_docs + slot`.
    pub added: Vec<usize>,
    /// Every added slot's path.
    pub names: Arc<[String]>,
}

/// The files source: root plus the recorded stamps, and what is known of
/// each doc since.
pub struct Files {
    known: Arc<Known>,
    /// The index a load asks watchman about, and keeps a record of.
    index: Option<journal::Index>,
    /// The last token taken, before its re-check: the next ask is since
    /// it, and [`Self::checkpoint`] saves it.
    last: Mutex<Option<journal::Token>>,
    /// Whether the record was saved: once per process.
    saved: AtomicBool,
    /// Whether `files` ascend, checked once.
    sorted: OnceLock<bool>,
    /// The warning a server printed last, until watchman answers again.
    said: Mutex<Option<String>>,
    /// Why a server cannot use watchman at all, found at its start.
    refusal: Mutex<Option<String>>,
}

impl Files {
    /// Checks the root only; no doc is statted here.
    pub fn open(root: PathBuf, stamps: Stamps, num_docs: usize) -> Result<Self, String> {
        if !root.is_dir() {
            return Err(format!(
                "error: corpus root {} is not a directory; move the tree back or re-run `fmgrep index`",
                root.display()
            ));
        }
        Ok(Self {
            known: Arc::new(Known {
                root,
                stamps,
                state: (0..num_docs).map(|_| AtomicU8::new(FRESH)).collect(),
                changed: AtomicUsize::new(0),
                epoch: AtomicUsize::new(0),
                exclude: None,
                added: Mutex::default(),
                recorded: None,
                rules: Mutex::default(),
                new_ignores: Mutex::default(),
                complete: AtomicBool::new(false),
                each_query: false,
            }),
            index: None,
            last: Mutex::default(),
            saved: AtomicBool::new(false),
            sorted: OnceLock::new(),
            said: Mutex::default(),
            refusal: Mutex::default(),
        })
    }

    /// The files source of `rels` under `root` (each a text file found
    /// just now), and of no indexed doc: all of them are added files.
    pub fn listed(root: PathBuf, rels: &[String]) -> Result<Self, String> {
        let mut files = Self::open(root, Stamps::none(), 0)?;
        let known = Arc::get_mut(&mut files.known).expect("listed precedes sharing");
        known.add(rels);
        known.complete.store(true, Ordering::Release);
        Ok(files)
    }

    /// Let [`Self::replay`] ask watchman, for `index`; and
    /// [`Self::recheck`] too, before each query, if `each_query`. Before
    /// any watch or discovery.
    pub fn ask(&mut self, index: journal::Index, each_query: bool) {
        let known = Arc::get_mut(&mut self.known).expect("ask precedes sharing");
        known.each_query = each_query;
        self.index = Some(index);
    }

    /// The canonical root and the walk's record to ask with, or why not.
    fn askable(&self, files: &[String]) -> Result<(PathBuf, &Arc<Recorded>), String> {
        if !*self.sorted.get_or_init(|| files.is_sorted()) {
            return Err("the manifest lists files out of order; re-run index".to_owned());
        }
        let root = &self.known.root;
        let canonical = root
            .canonicalize()
            .map_err(|e| format!("cannot resolve {} ({e})", root.display()))?;
        match &self.known.recorded {
            Some(recorded) => Ok((canonical, recorded)),
            None => Err("the index has no dirs.stamps; re-run index".to_owned()),
        }
    }

    /// Re-check what may differ (what watchman names, plus the saved
    /// baseline) instead of [`Self::stat_all`] and discovery: false when no
    /// ask is due (a server's always is), or, saying why on stderr, when
    /// watchman cannot say.
    pub fn replay(&self, files: &[String], lens: &[u64]) -> bool {
        let Some(index) = &self.index else {
            return false;
        };
        if !self.known.each_query && !journal::due(files.len()) {
            return false;
        }
        let (root, recorded) = match self.askable(files) {
            Ok(askable) => askable,
            Err(why) if self.known.each_query => {
                *self.refusal.lock().unwrap_or_else(PoisonError::into_inner) = Some(why);
                return false;
            }
            Err(why) => {
                journal::cannot_answer(&why);
                return false;
            }
        };
        let asked = std::time::Instant::now();
        let answer = journal::ask(&root, index, files.len(), self.known.each_query);
        if let Some(why) = answer.why {
            self.degraded(why);
        }
        if let Some(why) = answer.lasting {
            *self.refusal.lock().unwrap_or_else(PoisonError::into_inner) = Some(why);
        }
        *self.last.lock().unwrap_or_else(PoisonError::into_inner) = answer.token;
        let Some(paths) = answer.paths else {
            return false;
        };
        let applied = std::time::Instant::now();
        let kept = match self.known.replay(recorded, &paths, files, lens) {
            Ok(kept) => kept,
            Err(()) => {
                journal::over(journal::named_cap(files.len()));
                return false;
            }
        };
        let n = paths.len();
        timed(
            format_args!("watchman and the baseline named {n} path(s), {kept} kept"),
            asked,
            applied,
        );
        if crate::verbose::verbose() {
            let n = self.known.added().live().count();
            if n > 0 {
                eprintln!(
                    "note: {n} file(s) added since indexing are scanned in full by every query"
                );
            }
        }
        true
    }

    /// Why a server cannot use watchman at all, as its start found: not
    /// installed, no daemon on `WATCHMAN_SOCK`, a network filesystem, an
    /// index without `dirs.stamps`, and the like. Watchman reading the
    /// tree, or late, is no such cause.
    pub fn refusal(&self) -> Option<String> {
        self.refusal
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// A server's start that took no token says why: its first query
    /// prints a warning only when the reason differs.
    fn degraded(&self, why: String) {
        if self.known.each_query {
            *self.said.lock().unwrap_or_else(PoisonError::into_inner) = Some(why);
        }
    }

    /// Before a served query: ask watchman what changed since the last
    /// token, then re-check those paths; when watchman cannot say, check
    /// every file as a new load would. On stderr, why every file was
    /// checked, each time the reason changes, and a note once watchman
    /// answers again.
    pub fn recheck(&self, files: &[String], lens: &[u64]) {
        if self.index.is_none() || !self.known.each_query {
            return;
        }
        self.known.reload_rules();
        let why = match self.askable(files) {
            Ok((root, recorded)) => self.ask_again(&root, recorded, files, lens),
            Err(why) => Some(journal::cannot(&why)),
        };
        if why.is_some() {
            self.check_all(files, lens);
        }
        let mut said = self.said.lock().unwrap_or_else(PoisonError::into_inner);
        match (&why, said.is_some()) {
            (Some(line), _) if said.as_ref() != Some(line) => journal::say(line),
            (None, true) => journal::answers_again(),
            _ => {}
        }
        *said = why;
    }

    /// The ask and re-check of [`Self::recheck`]: why every file must be
    /// checked, if it must. The last token stays when watchman is only
    /// late; it goes when its daemon is gone.
    fn ask_again(
        &self,
        root: &Path,
        recorded: &Arc<Recorded>,
        files: &[String],
        lens: &[u64],
    ) -> Option<String> {
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        let asked = std::time::Instant::now();
        let again = journal::again(root, last.as_ref());
        if again.token.is_some() || !again.keep {
            *last = again.token;
        }
        drop(last);
        let Some(paths) = again.paths else {
            return again.why;
        };
        let applied = std::time::Instant::now();
        let Ok(kept) = self.known.replay(recorded, &paths, files, lens) else {
            return Some(journal::over_since(journal::named_cap(files.len())));
        };
        let n = paths.len();
        timed(
            format_args!("watchman reported {n} path(s) changed, {kept} kept"),
            asked,
            applied,
        );
        again.why
    }

    /// What a new load finds: every doc statted, ignored ones too, then
    /// discovery, which marks again what the rules skip.
    fn check_all(&self, files: &[String], lens: &[u64]) {
        self.known.stat_all(files, lens, true);
        self.discover(files, lens);
    }

    /// Once every doc is known, save the last token and what differs now
    /// (the baseline) in the user's cache: once per process, so a server
    /// saves after its first query that has a token.
    pub fn checkpoint(&self, files: &[String]) {
        let Some(index) = &self.index else {
            return;
        };
        if self.saved.load(Ordering::Acquire) || !self.known.complete.load(Ordering::Acquire) {
            return;
        }
        let token = self
            .last
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let (Some(token), Ok(root)) = (token, self.known.root.canonicalize()) else {
            return;
        };
        self.saved.store(true, Ordering::Release);
        let snap = self.snapshot();
        let mut baseline: Vec<String> = snap
            .stale
            .iter()
            .chain(&snap.gone)
            .map(|&id| files[id].clone())
            .collect();
        let recorded = self.known.recorded.as_deref();
        baseline.extend(
            snap.added
                .iter()
                .map(|&doc| unrecorded(recorded, &snap.names[doc - files.len()]).to_owned()),
        );
        let ignores = self.known.new_ignores.lock();
        baseline.extend(
            ignores
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .cloned(),
        );
        baseline.sort_unstable();
        baseline.dedup();
        let cap = journal::named_cap(files.len());
        journal::save(&root, index, token, baseline, cap);
    }

    /// Never take `index_dir` for an added file. Before any watch or
    /// discovery.
    pub fn exclude(&mut self, index_dir: &Path) {
        let known = Arc::get_mut(&mut self.known).expect("exclude precedes sharing");
        known.exclude = index_dir.canonicalize().ok();
    }

    /// Discover added files from what `index` recorded, not a full walk.
    /// Before any watch or discovery.
    pub fn record(&mut self, recorded: Recorded) {
        let known = Arc::get_mut(&mut self.known).expect("record precedes sharing");
        known.recorded = Some(Arc::new(recorded));
    }

    /// Stat every doc; each newly gone one warns, in doc order.
    pub fn stat_all(&self, files: &[String], lens: &[u64]) {
        self.known.stat_all(files, lens, false);
    }

    /// Whether any doc is stale or gone.
    pub fn changed(&self) -> bool {
        self.known.changed.load(Ordering::Acquire) > 0
    }

    /// A count that moves whenever a doc changes kind.
    pub fn epoch(&self) -> usize {
        self.known.epoch.load(Ordering::Acquire)
    }

    pub fn snapshot(&self) -> Snapshot {
        let mut snap = Snapshot::default();
        let num_docs = self.known.state.len();
        if self.changed() {
            for id in 0..num_docs {
                match self.known.kind(id) {
                    STALE => snap.stale.push(id),
                    GONE | IGNORED => snap.gone.push(id),
                    _ => {}
                }
            }
        }
        let mut added = self.known.added();
        snap.added = added.live().map(|slot| num_docs + slot).collect();
        snap.names = added.names();
        snap
    }

    /// Added doc `doc`'s whole bytes; one now missing, unreadable or binary
    /// is dropped silently: `None`.
    pub fn read_added(&self, doc: usize) -> Option<Vec<u8>> {
        self.known.read_added(doc)
    }

    /// Doc `doc`'s current text: `buf` (its recorded length) filled while
    /// the stamp holds and the doc is not UTF-16, else
    /// [`crate::binary::text`] of the whole file, and the doc turns stale
    /// unless it is the indexed UTF-16 text. A file that is ignored, cannot
    /// be read or is no longer text is excluded: `None`.
    pub fn read(&self, doc: usize, rel: &str, mut buf: Vec<u8>) -> Option<Vec<u8>> {
        use std::io::Read as _;
        let known = &*self.known;
        if known.kind(doc) == IGNORED {
            return None;
        }
        let path = known.root.join(rel);
        let failed = |e: std::io::Error| {
            known.exclude(doc, &path, &format!("cannot read ({e})"));
            None
        };
        let mut file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(e) => return failed(e),
        };
        let meta = match file.metadata() {
            Ok(m) => m,
            Err(e) => return failed(e),
        };
        let holds = known.stamps.holds(doc, &meta, buf.len() as u64);
        if holds && known.stamps.utf16(doc).is_none() {
            return match file.read_exact(&mut buf) {
                Ok(()) => Some(buf),
                Err(e) => failed(e),
            };
        }
        if !holds && !known.settle(doc, STALE) {
            return None;
        }
        if meta.len() > crate::binary::MAX_DOC_BYTES {
            known.exclude(doc, &path, &crate::binary::Skip::TooLarge.to_string());
            return None;
        }
        let mut whole = Vec::with_capacity(meta.len().try_into().unwrap_or(0));
        if let Err(e) = file.read_to_end(&mut whole) {
            return failed(e);
        }
        match crate::binary::text(whole) {
            Ok(text) => {
                if text.bytes.len() != buf.len() {
                    known.settle(doc, STALE);
                }
                Some(text.bytes)
            }
            Err(why) => {
                known.exclude(doc, &path, &why.to_string());
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parallel_keeps_item_order_and_runs_each_item_once() {
        for len in [0, PARALLEL_STAT_MIN, PARALLEL_STAT_MIN + 1, 1000] {
            let items: Vec<usize> = (0..len).collect();
            let runs = AtomicUsize::new(0);
            let got = parallel(&items, |&i| {
                runs.fetch_add(1, Ordering::Relaxed);
                i * 3
            });
            assert_eq!(
                got,
                items.iter().map(|i| i * 3).collect::<Vec<_>>(),
                "len={len}"
            );
            assert_eq!(runs.into_inner(), len, "len={len}");
        }
    }

    #[test]
    #[should_panic(expected = "boom")]
    fn a_parallel_panic_resumes_on_the_caller() {
        let items: Vec<usize> = (0..100).collect();
        parallel(&items, |&i| assert!(i != 42, "boom"));
    }

    #[test]
    fn stamps_round_trip_and_size_skew_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        write_stamps(tmp.path(), &[1, -2, i64::MAX], &[]).unwrap();
        let stamps = Stamps::open(tmp.path(), 3).unwrap();
        assert_eq!(
            [stamps.get(0), stamps.get(1), stamps.get(2)],
            [1, -2, i64::MAX]
        );
        assert_eq!(stamps.utf16(1), None);
        let Err(err) = Stamps::open(tmp.path(), 2) else {
            panic!("a skewed docs.stamps must be refused");
        };
        assert!(
            err.contains(DOCS_STAMPS) && err.contains("tampered"),
            "got: {err}"
        );
        write_stamps(tmp.path(), &[1, -2, i64::MAX], &[(0, 7), (2, 9)]).unwrap();
        let stamps = Stamps::open(tmp.path(), 3).unwrap();
        assert_eq!(stamps.get(2), i64::MAX);
        let utf16: Vec<_> = (0..3).map(|doc| stamps.utf16(doc)).collect();
        assert_eq!(utf16, [Some(7), None, Some(9)]);
        assert!(Stamps::open(tmp.path(), 4).is_err(), "8 bytes short");
    }

    #[test]
    fn stamps_refuse_unordered_or_out_of_range_utf16_pairs() {
        let tmp = tempfile::tempdir().unwrap();
        for pairs in [&[(1, 5), (1, 5)][..], &[(2, 5), (1, 5)], &[(3, 5)]] {
            write_stamps(tmp.path(), &[0, 0, 0], pairs).unwrap();
            let Err(err) = Stamps::open(tmp.path(), 3) else {
                panic!("{pairs:?} must be refused");
            };
            assert!(err.contains("tampered"), "got: {err}");
        }
    }

    /// One file per doc under `<dir>/corpus`, stamped into `<dir>`.
    pub(super) struct Fix {
        pub(super) path: PathBuf,
        index: PathBuf,
        pub(super) files: Vec<String>,
        pub(super) lens: Vec<u64>,
    }

    pub(super) fn stage_files(dir: &Path, docs: &[&[u8]]) -> Fix {
        let path = dir.join("corpus");
        std::fs::create_dir_all(&path).unwrap();
        let files: Vec<String> = (0..docs.len()).map(|i| format!("d{i:04}.c")).collect();
        let mut mtimes = Vec::new();
        for (name, d) in files.iter().zip(docs) {
            std::fs::write(path.join(name), d).unwrap();
            mtimes.push(Stamp::of(&std::fs::metadata(path.join(name)).unwrap()).mtime_ns);
        }
        write_stamps(dir, &mtimes, &[]).unwrap();
        Fix {
            path,
            index: dir.to_path_buf(),
            files,
            lens: docs.iter().map(|d| d.len() as u64).collect(),
        }
    }

    pub(super) fn open_files(fix: &Fix) -> Files {
        let stamps = Stamps::open(&fix.index, fix.lens.len()).unwrap();
        Files::open(fix.path.clone(), stamps, fix.lens.len()).unwrap()
    }

    impl Files {
        fn kinds(&self) -> Vec<u8> {
            (0..self.known.state.len())
                .map(|id| self.known.kind(id))
                .collect()
        }

        fn warned(&self, id: usize) -> bool {
            self.known.state[id].load(Ordering::Acquire) & WARNED != 0
        }
    }

    fn stale_gone(files: &Files) -> (Vec<usize>, Vec<usize>) {
        let snap = files.snapshot();
        (snap.stale, snap.gone)
    }

    #[test]
    fn stat_all_finds_stale_and_gone_and_warns_once() {
        let tmp = tempfile::tempdir().unwrap();
        let fix = stage_files(tmp.path(), &[b"printk", b"zz", b"printk zz"]);
        std::fs::write(fix.path.join(&fix.files[1]), b"zzzz").unwrap();
        std::fs::remove_file(fix.path.join(&fix.files[2])).unwrap();
        let files = open_files(&fix);
        assert!(!files.changed(), "nothing is statted at open");
        files.stat_all(&fix.files, &fix.lens);
        assert_eq!(files.kinds(), [FRESH, STALE, GONE]);
        assert_eq!(stale_gone(&files), (vec![1], vec![2]));
        assert!(files.changed() && files.warned(2) && !files.warned(1));
    }

    #[test]
    fn stat_pass_tracks_fresh_stale_gone_transitions() {
        let tmp = tempfile::tempdir().unwrap();
        let many: Vec<Vec<u8>> = (0..200).map(|i| format!("doc {i}").into_bytes()).collect();
        let docs: Vec<&[u8]> = many.iter().map(Vec::as_slice).collect();
        let fix = stage_files(tmp.path(), &docs);
        let files = open_files(&fix);
        let path = |id: usize| fix.path.join(&fix.files[id]);
        files.stat_all(&fix.files, &fix.lens);
        assert!(!files.changed());
        std::fs::write(path(150), b"changed!!").unwrap();
        std::fs::remove_file(path(7)).unwrap();
        files.stat_all(&fix.files, &fix.lens);
        assert_eq!(stale_gone(&files), (vec![150], vec![7]));
        // A reappearing file is stale; one matching its stamp again is fresh.
        std::fs::write(path(7), b"back").unwrap();
        let old = std::fs::File::options()
            .write(true)
            .open(path(150))
            .unwrap();
        old.set_len(0).unwrap();
        std::io::Write::write_all(&mut &old, &many[150]).unwrap();
        old.set_modified(
            std::time::UNIX_EPOCH
                + std::time::Duration::from_nanos(files.known.stamps.get(150) as u64),
        )
        .unwrap();
        files.stat_all(&fix.files, &fix.lens);
        assert_eq!(stale_gone(&files), (vec![7], vec![]));
        assert_eq!(files.known.changed.load(Ordering::Acquire), 1);
    }

    #[test]
    fn read_turns_a_changed_doc_stale_and_an_unreadable_one_gone() {
        let tmp = tempfile::tempdir().unwrap();
        let fix = stage_files(tmp.path(), &[b"printk", b"zz", b"aa"]);
        let files = open_files(&fix);
        files.stat_all(&fix.files, &fix.lens);
        std::fs::write(fix.path.join(&fix.files[1]), b"zz grew").unwrap();
        std::fs::write(fix.path.join(&fix.files[2]), b"a\0binary").unwrap();
        let read = |id: usize| files.read(id, &fix.files[id], vec![0; fix.lens[id] as usize]);
        assert_eq!(read(0).as_deref(), Some(&b"printk"[..]));
        assert_eq!(read(1).as_deref(), Some(&b"zz grew"[..]));
        assert_eq!(read(2), None);
        std::fs::remove_file(fix.path.join(&fix.files[0])).unwrap();
        assert_eq!(read(0), None);
        assert_eq!(files.kinds(), [GONE, STALE, GONE]);
        assert!(files.warned(0) && files.warned(2));
    }

    #[test]
    fn an_ignored_doc_survives_the_stat_pass_and_is_never_warned() {
        let tmp = tempfile::tempdir().unwrap();
        let fix = stage_files(tmp.path(), &[b"printk", b"zz", b"aa"]);
        let files = open_files(&fix);
        files.known.set(1, IGNORED);
        files.known.set(2, IGNORED);
        std::fs::write(fix.path.join(&fix.files[1]), b"zz changed").unwrap();
        std::fs::remove_file(fix.path.join(&fix.files[2])).unwrap();
        files.stat_all(&fix.files, &fix.lens);
        assert_eq!(files.kinds(), [FRESH, IGNORED, IGNORED]);
        assert_eq!(stale_gone(&files), (vec![], vec![1, 2]));
        assert_eq!(files.read(1, &fix.files[1], vec![0; 2]), None);
        assert!(!files.warned(1) && !files.warned(2));
    }

    /// `docs` and a binary `kb/k.bin` under `<dir>/corpus`, backdated and
    /// recorded as `index` records them.
    #[cfg(unix)]
    fn stage_named(dir: &Path, docs: &[&str]) -> Fix {
        let path = dir.join("corpus");
        for rel in docs.iter().chain(&["kb/k.bin"]) {
            std::fs::create_dir_all(path.join(rel).parent().unwrap()).unwrap();
            let bytes = if rel.ends_with(".bin") {
                &b"\0\n"[..]
            } else {
                b"x\n"
            };
            std::fs::write(path.join(rel), bytes).unwrap();
        }
        crate::tests::support::backdate(&path);
        write_stamps(dir, &vec![0; docs.len()], &[]).unwrap();
        let walk = crate::for_each_index_doc(&path, None, |_, _, _, _| Ok(())).unwrap();
        write_walk(dir, &walk, std::time::SystemTime::now()).unwrap();
        Fix {
            path,
            index: dir.to_path_buf(),
            files: docs.iter().map(|d| d.to_string()).collect(),
            lens: vec![2; docs.len()],
        }
    }

    /// `fix`'s files with its record, after a replay of `named`.
    #[cfg(unix)]
    fn replayed(fix: &Fix, named: &[&str]) -> (Files, Result<usize, ()>) {
        let mut files = open_files(fix);
        let recorded = Arc::new(Recorded::open(&fix.index).unwrap().unwrap());
        files.record(Recorded::open(&fix.index).unwrap().unwrap());
        let named: Vec<String> = named.iter().map(|n| n.to_string()).collect();
        let kept = files.known.replay(&recorded, &named, &fix.files, &fix.lens);
        (files, kept)
    }

    #[cfg(unix)]
    #[test]
    fn replay_rechecks_what_is_named_and_lists_named_dirs_one_level() {
        let tmp = tempfile::tempdir().unwrap();
        let docs = ["a.c", "d/in/i.c", "d/m.c", "d/x.c", "keep/k.c", "z.c"];
        let fix = stage_named(tmp.path(), &docs);
        let at = |rel: &str| fix.path.join(rel);
        let added = |f: &Files| {
            let s = f.snapshot();
            let n = fix.files.len();
            s.added
                .iter()
                .map(|&d| s.names[d - n].clone())
                .collect::<Vec<_>>()
        };
        std::fs::write(at("a.c"), b"xy\n").unwrap();
        // Same size, mtime restored: the stamp holds, so it stays fresh.
        std::fs::write(at("z.c"), b"y\n").unwrap();
        crate::tests::support::set_mtime(&at("z.c"), crate::tests::support::T0);
        std::fs::remove_file(at("d/x.c")).unwrap();
        std::fs::rename(at("d/in"), at("d/out")).unwrap();
        std::fs::write(at("kb/k.bin"), b"now text\n").unwrap();
        std::fs::remove_file(at("keep/k.c")).unwrap();
        let named = ["a.c", "z.c", "d", "kb/k.bin", "gone/dir/x.c", ".hidden"];
        let (files, kept) = replayed(&fix, &named);
        assert_eq!(
            kept,
            Ok(6),
            "a path under an unrecorded dir counts as that dir"
        );
        let want = [STALE, GONE, FRESH, GONE, FRESH, FRESH];
        assert_eq!(files.kinds(), want, "keep/k.c is not named, nor its dir");
        assert_eq!(added(&files), ["d/out/i.c", "kb/k.bin"]);
        // A new file in the root: the root is listed, then each recorded
        // dir in it whose stamp moved.
        std::fs::write(at("top.c"), b"x\n").unwrap();
        let (files, kept) = replayed(&fix, &["top.c"]);
        assert_eq!(kept, Ok(1));
        assert_eq!(files.kinds(), [STALE, GONE, FRESH, GONE, GONE, FRESH]);
        assert_eq!(added(&files), ["d/out/i.c", "top.c"], "kb did not move");
        let many: Vec<String> = (0..65).map(|i| format!("n{i}.c")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        let (files, kept) = replayed(&fix, &many);
        assert_eq!(kept, Err(()), "past 64, the cap of a small tree");
        assert!(!files.changed(), "past the cap nothing is touched");
    }

    /// One set of marks across replays, as a server keeps: a path deep in
    /// a new dir walks that dir, and an ignore file made in it later drops
    /// the files it hides.
    #[cfg(unix)]
    #[test]
    fn replay_walks_a_new_dir_and_drops_what_its_ignore_file_hides() {
        let tmp = tempfile::tempdir().unwrap();
        let fix = stage_named(tmp.path(), &["a.c"]);
        let (files, _) = replayed(&fix, &[]);
        let recorded = Arc::new(Recorded::open(&fix.index).unwrap().unwrap());
        let replay = |named: &[&str]| {
            let named: Vec<String> = named.iter().map(|n| n.to_string()).collect();
            files.known.replay(&recorded, &named, &fix.files, &fix.lens)
        };
        let put = |rel: &str| {
            std::fs::create_dir_all(fix.path.join(rel).parent().unwrap()).unwrap();
            std::fs::write(fix.path.join(rel), b"x\n").unwrap();
        };
        put("e/x.c");
        put("e/sub/y.c");
        put(".h/z.c");
        assert_eq!(replay(&["e/sub/y.c", "e/sub", ".h/z.c"]), Ok(1), "e, once");
        assert_eq!(added_paths(&files, &fix), ["e/sub/y.c", "e/x.c"]);
        std::fs::write(fix.path.join("e/.ignore"), b"x.c\n").unwrap();
        assert_eq!(replay(&["e/.ignore"]), Ok(1));
        assert_eq!(
            added_paths(&files, &fix),
            ["e/sub/y.c"],
            "e/x.c is ignored now"
        );
        std::fs::remove_file(fix.path.join("e/.ignore")).unwrap();
        assert_eq!(replay(&["e"]), Ok(1), "a new dir itself");
        assert_eq!(added_paths(&files, &fix), ["e/sub/y.c", "e/x.c"]);
    }

    /// A server's stat pass on marks it keeps finds what a new load finds:
    /// a doc that a new ignore file hid comes back once the file goes.
    #[cfg(unix)]
    #[test]
    fn checking_every_file_again_brings_back_a_doc_no_longer_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let fix = stage_named(tmp.path(), &["a.c", "sub/b.c"]);
        let (files, _) = replayed(&fix, &[]);
        files.check_all(&fix.files, &fix.lens);
        assert_eq!(files.kinds(), [FRESH, FRESH]);
        std::fs::write(fix.path.join("sub/.ignore"), b"b.c\n").unwrap();
        files.check_all(&fix.files, &fix.lens);
        assert_eq!(files.kinds(), [FRESH, IGNORED]);
        std::fs::remove_file(fix.path.join("sub/.ignore")).unwrap();
        files.check_all(&fix.files, &fix.lens);
        assert_eq!(files.kinds(), [FRESH, FRESH], "as a new load finds it");
        assert!(!files.warned(1));
    }

    /// The live added paths, by slot.
    fn added_paths(files: &Files, fix: &Fix) -> Vec<String> {
        let s = files.snapshot();
        let n = fix.files.len();
        s.added.iter().map(|&d| s.names[d - n].clone()).collect()
    }

    #[test]
    fn an_added_file_is_entered_as_its_topmost_unrecorded_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let fix = stage_files(tmp.path(), &[b"a"]);
        std::fs::create_dir_all(fix.path.join("old")).unwrap();
        let walk = crate::for_each_index_doc(&fix.path, None, |_, _, _, _| Ok(())).unwrap();
        write_walk(&fix.index, &walk, std::time::SystemTime::now()).unwrap();
        let recorded = Recorded::open(&fix.index).unwrap().unwrap();
        let r = Some(&recorded);
        assert_eq!(unrecorded(r, "n.c"), "n.c");
        assert_eq!(unrecorded(r, "old/n.c"), "old/n.c");
        assert_eq!(unrecorded(r, "new/sub/n.c"), "new");
        assert_eq!(unrecorded(r, "old/new/n.c"), "old/new");
        assert_eq!(unrecorded(None, "new/sub/n.c"), "new/sub/n.c");
    }
}
