//! What the files layout knows of each file since indexing: fresh, stale
//! (scanned in full), gone (excluded) or added (not in the manifest; scanned
//! in full). A change journal, when it vouches, names what to re-check
//! instead of the stat pass and discovery.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::Duration;

use rayon::prelude::*;

use crate::Stamp;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};

mod added;
mod journal;
mod recorded;

pub use added::Discovery;
pub use journal::{Journal, Knob, marks};
pub use recorded::{Recorded, write_walk};

/// Per-doc mtimes of a files-layout index, at the index root.
pub const DOCS_STAMPS: &str = "docs.stamps";

/// Write `docs.stamps`: one little-endian i64 mtime (ns) per doc, no header.
pub fn write_stamps(output: &Path, mtimes: &[i64]) -> Result<(), String> {
    let path = output.join(DOCS_STAMPS);
    let failed = |e: std::io::Error| format!("error: cannot write {}: {e}", path.display());
    let file = std::fs::File::create(&path).map_err(failed)?;
    let mut out = std::io::BufWriter::new(file);
    for m in mtimes {
        out.write_all(&m.to_le_bytes()).map_err(failed)?;
    }
    out.flush().map_err(failed)
}

/// The mapped `docs.stamps`.
pub struct Stamps {
    map: memmap2::Mmap,
}

impl Stamps {
    /// Map `docs.stamps` under `index_dir`; a size other than 8 bytes per doc
    /// is refused.
    pub fn open(index_dir: &Path, num_docs: usize) -> Result<Self, String> {
        let path = index_dir.join(DOCS_STAMPS);
        let failed = |e: std::io::Error| format!("error: cannot read {}: {e}", path.display());
        let file = std::fs::File::open(&path).map_err(failed)?;
        // Safety: the index is read-only while loaded, as `blobs.shard` is.
        let map = unsafe { memmap2::Mmap::map(&file) }.map_err(failed)?;
        if map.len() as u64 != num_docs as u64 * 8 {
            return Err(format!(
                "error: {} holds {} byte(s), want {} for {num_docs} doc(s); refusing a tampered index",
                path.display(),
                map.len(),
                num_docs as u64 * 8
            ));
        }
        Ok(Self { map })
    }

    pub fn get(&self, doc: usize) -> i64 {
        let at = doc * 8;
        i64::from_le_bytes(self.map[at..at + 8].try_into().expect("8-byte slot"))
    }

    /// Whether `meta` still matches doc `doc` of recorded length `len`.
    fn holds(&self, doc: usize, meta: &std::fs::Metadata, len: u64) -> bool {
        Stamp::of(meta)
            == Stamp {
                len,
                mtime_ns: self.get(doc),
            }
    }
}

/// Per-doc states, plus [`WARNED`] once a doc's exclusion was reported.
const FRESH: u8 = 0;
const STALE: u8 = 1;
const GONE: u8 = 2;
const KIND: u8 = 3;
const WARNED: u8 = 4;

/// Docs above this are statted on several threads.
const PARALLEL_STAT_MIN: usize = 64;
const STAT_THREADS: usize = 16;
/// How long the watcher waits for a burst of events to end.
const WATCH_SETTLE: Duration = Duration::from_millis(20);
/// How often an idle watcher checks its stop flag.
const WATCH_TICK: Duration = Duration::from_millis(50);

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
    latch: added::Latch,
    /// Whether discovery has started; added files are reconciled only after
    /// it.
    discovering: AtomicBool,
    recorded: Option<Recorded>,
    /// Whether every doc was statted and every added file found.
    complete: AtomicBool,
}

impl Known {
    fn kind(&self, id: usize) -> u8 {
        self.state[id].load(Ordering::Acquire) & KIND
    }

    fn set(&self, id: usize, kind: u8) {
        let old = self.state[id]
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |s| {
                Some(s & WARNED | kind)
            })
            .expect("the update always applies");
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
    }

    /// Mark `id` gone; the first exclusion of a doc per process warns.
    fn exclude(&self, id: usize, path: &Path, why: &str) {
        self.set(id, GONE);
        if self.state[id].fetch_or(WARNED, Ordering::AcqRel) & WARNED == 0 {
            eprintln!("warning: excluding {}: {why}", path.display());
        }
    }

    /// Stat `id` against its stamp: why it is gone, if it is.
    fn stat(&self, id: usize, files: &[String], lens: &[u64]) -> Option<String> {
        match std::fs::metadata(self.root.join(&files[id])) {
            Ok(meta) => {
                let fresh = self.stamps.holds(id, &meta, lens[id]);
                self.set(id, if fresh { FRESH } else { STALE });
                None
            }
            Err(e) => {
                self.set(id, GONE);
                Some(match e.kind() {
                    std::io::ErrorKind::NotFound => "is missing".to_string(),
                    _ => format!("cannot stat ({e})"),
                })
            }
        }
    }

    /// Stat `ids` (ascending), then warn about the gone ones in order, or
    /// once when over half the docs are.
    fn stat_ids(&self, ids: &[usize], files: &[String], lens: &[u64]) {
        let gone = parallel(ids, |&id| self.stat(id, files, lens));
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

    fn stat_all(&self, files: &[String], lens: &[u64]) {
        let ids: Vec<usize> = (0..self.state.len()).collect();
        self.stat_ids(&ids, files, lens);
    }

    /// Take ascending `paths` as all that changed in ascending `files`:
    /// re-stat the docs at or under each and reconcile each; list each of
    /// `relist` one level.
    fn replay(&self, paths: &[String], relist: &[String], files: &[String], lens: &[u64]) {
        self.discovering.store(true, Ordering::Release);
        let root = &self.root;
        let by_path: Vec<usize> = (0..files.len()).collect();
        let mut ids = Vec::new();
        if paths
            .iter()
            .any(|rel| docs_at(root, &root.join(rel), files, &by_path, &mut ids))
        {
            self.stat_all(files, lens);
            self.discover(root, root, files);
        } else {
            ids.sort_unstable();
            ids.dedup();
            self.stat_ids(&ids, files, lens);
            parallel(paths, |rel| {
                self.reconcile_path(root, &root.join(rel), files)
            });
            for dir in relist {
                self.relist(dir, files);
            }
        }
        self.complete.store(true, Ordering::Release);
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
    journal: Option<Journal>,
    /// Taken by [`Self::replay`] before asking, for [`Self::checkpoint`].
    marks: OnceLock<Vec<journal::Token>>,
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
                latch: added::Latch::default(),
                discovering: AtomicBool::new(false),
                recorded: None,
                complete: AtomicBool::new(false),
            }),
            journal: None,
            marks: OnceLock::new(),
        })
    }

    /// Replay `journal` rather than stat every doc, when it vouches.
    pub fn journal(&mut self, journal: Journal) {
        self.journal = Some(journal);
    }

    /// Take what the journal names since its marks instead of
    /// [`Self::stat_all`] and discovery: false when it cannot vouch.
    pub fn replay(&self, files: &[String], lens: &[u64]) -> bool {
        let Some(journal) = &self.journal else {
            return false;
        };
        let Ok(root) = self.known.root.canonicalize() else {
            return false;
        };
        if !files.is_sorted() {
            return false;
        }
        let asked = std::time::Instant::now();
        let (marks, replay) = journal.ask(&root, files.len());
        let _ = self.marks.set(marks);
        let Some(replay) = replay else {
            if crate::verbose::verbose() {
                eprintln!("note: no change source vouches; statting every file");
            }
            return false;
        };
        let applied = std::time::Instant::now();
        self.known
            .replay(&replay.paths, &replay.relist, files, lens);
        if crate::verbose::verbose() {
            let (n, ask, apply) = (replay.paths.len(), applied - asked, applied.elapsed());
            let kind = replay.kind;
            eprintln!(
                "note: {kind} named {n} path(s) to re-check (ask {ask:.1?}, apply {apply:.1?})"
            );
            let n = self.known.added().live().count();
            if n > 0 {
                eprintln!(
                    "note: {n} file(s) added since indexing are scanned in full by every query"
                );
            }
        }
        true
    }

    /// Once every doc is known, rewrite the journal under `index_dir` from
    /// [`Self::replay`]'s marks and what differs now.
    pub fn checkpoint(&mut self, index_dir: &Path, files: &[String]) {
        let (Some(journal), Some(marks)) = (self.journal.take(), self.marks.take()) else {
            return;
        };
        if !self.known.complete.load(Ordering::Acquire) {
            return;
        }
        let snap = self.snapshot();
        let mut named: Vec<String> = snap
            .stale
            .iter()
            .chain(&snap.gone)
            .map(|&id| files[id].clone())
            .collect();
        named.extend(
            snap.added
                .iter()
                .map(|&doc| snap.names[doc - files.len()].clone()),
        );
        named.sort_unstable();
        journal.checkpoint(index_dir, marks, named);
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
        known.recorded = Some(recorded);
    }

    pub fn wait_discovered(&self) {
        self.known.latch.wait(None);
    }

    /// Stat every doc; each newly gone one warns, in doc order.
    pub fn stat_all(&self, files: &[String], lens: &[u64]) {
        self.known.stat_all(files, lens);
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
                    GONE => snap.gone.push(id),
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

    /// Doc `doc`'s current bytes: `buf` (its recorded length) filled while
    /// the stamp holds, else the whole file, and the doc turns stale. A file
    /// that cannot be read or now looks binary is excluded: `None`.
    pub fn read(&self, doc: usize, rel: &str, mut buf: Vec<u8>) -> Option<Vec<u8>> {
        use std::io::Read as _;
        let known = &*self.known;
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
        if known.stamps.holds(doc, &meta, buf.len() as u64) {
            return match file.read_exact(&mut buf) {
                Ok(()) => Some(buf),
                Err(e) => failed(e),
            };
        }
        known.set(doc, STALE);
        let mut whole = Vec::with_capacity(meta.len().try_into().unwrap_or(0));
        if let Err(e) = file.read_to_end(&mut whole) {
            return failed(e);
        }
        if crate::is_binary_doc(&whole) {
            known.exclude(doc, &path, "looks binary");
            return None;
        }
        Some(whole)
    }

    /// Watch the root, recursively; events queue until [`Watch::run`].
    pub fn watch(&self) -> Result<Watch, String> {
        let failed =
            |e: &dyn std::fmt::Display| format!("cannot watch {}: {e}", self.known.root.display());
        let root = self.known.root.canonicalize().map_err(|e| failed(&e))?;
        let (tx, events) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = tx.send(event);
        })
        .map_err(|e| failed(&e))?;
        watcher
            .watch(&root, RecursiveMode::Recursive)
            .map_err(|e| failed(&e))?;
        Ok(Watch {
            known: Arc::clone(&self.known),
            root,
            events,
            _watcher: watcher,
        })
    }
}

/// Filesystem events on the corpus root, re-checking the paths they name.
pub struct Watch {
    known: Arc<Known>,
    /// Canonical, as events name paths.
    root: PathBuf,
    events: mpsc::Receiver<notify::Result<Event>>,
    _watcher: RecommendedWatcher,
}

impl Watch {
    /// Once discovery is done, re-stat and re-check for added files what each
    /// burst names, until `stop`; an error, overflow or root event redoes all.
    pub fn run(self, files: &[String], lens: &[u64], stop: &AtomicBool) {
        if !self.known.latch.wait(Some(stop)) {
            return;
        }
        let mut by_path: Vec<usize> = (0..files.len()).collect();
        by_path.sort_by(|&a, &b| files[a].cmp(&files[b]));
        while !stop.load(Ordering::Relaxed) {
            let first = match self.events.recv_timeout(WATCH_TICK) {
                Ok(event) => event,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            };
            std::thread::sleep(WATCH_SETTLE);
            let mut all = false;
            let mut ids = Vec::new();
            let mut named = Vec::new();
            for event in std::iter::once(first).chain(self.events.try_iter()) {
                match event {
                    Ok(e) if e.need_rescan() => all = true,
                    Ok(e) if matches!(e.kind, EventKind::Access(_)) => {}
                    Ok(e) => {
                        for path in e.paths {
                            all |= docs_at(&self.root, &path, files, &by_path, &mut ids);
                            named.push(path);
                        }
                    }
                    Err(_) => all = true,
                }
            }
            if all {
                self.known.stat_all(files, lens);
                if self.known.discovering.load(Ordering::Acquire) {
                    self.known.discover(&self.root, &self.root, files);
                }
            } else {
                ids.sort_unstable();
                ids.dedup();
                self.known.stat_ids(&ids, files, lens);
                named.sort_unstable();
                named.dedup();
                for path in &named {
                    self.known.reconcile_path(&self.root, path, files);
                }
            }
        }
    }
}

/// Push the docs at or under `path`, via `by_path` (`files` ids by path);
/// true when `path` holds `root`.
fn docs_at(
    root: &Path,
    path: &Path,
    files: &[String],
    by_path: &[usize],
    ids: &mut Vec<usize>,
) -> bool {
    if root.starts_with(path) {
        return true;
    }
    let Some(rel) = path.strip_prefix(root).ok().and_then(Path::to_str) else {
        return false;
    };
    let at = by_path.partition_point(|&id| files[id].as_str() < rel);
    if by_path.get(at).is_some_and(|&id| files[id] == rel) {
        ids.push(by_path[at]);
    }
    let dir = format!("{rel}/");
    let lo = by_path.partition_point(|&id| files[id].as_str() < dir.as_str());
    ids.extend(
        by_path[lo..]
            .iter()
            .take_while(|&&id| files[id].starts_with(&dir)),
    );
    false
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
        write_stamps(tmp.path(), &[1, -2, i64::MAX]).unwrap();
        let stamps = Stamps::open(tmp.path(), 3).unwrap();
        assert_eq!(
            [stamps.get(0), stamps.get(1), stamps.get(2)],
            [1, -2, i64::MAX]
        );
        let Err(err) = Stamps::open(tmp.path(), 2) else {
            panic!("a skewed docs.stamps must be refused");
        };
        assert!(
            err.contains(DOCS_STAMPS) && err.contains("tampered"),
            "got: {err}"
        );
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
        write_stamps(dir, &mtimes).unwrap();
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
    fn replay_restats_named_docs_and_walks_named_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let fix = stage_files(tmp.path(), &[b"printk", b"zz", b"aa"]);
        std::fs::write(fix.path.join(&fix.files[0]), b"printk changed").unwrap();
        std::fs::write(fix.path.join(&fix.files[1]), b"zz changed").unwrap();
        std::fs::remove_file(fix.path.join(&fix.files[2])).unwrap();
        std::fs::create_dir_all(fix.path.join("sub/deep")).unwrap();
        std::fs::write(fix.path.join("sub/deep/n.c"), b"new").unwrap();
        std::fs::write(fix.path.join("sub/bin.dat"), b"a\0b").unwrap();
        std::fs::create_dir(fix.path.join("rl")).unwrap();
        std::fs::write(fix.path.join("rl/r.c"), b"relisted").unwrap();
        std::fs::write(fix.path.join("unnamed.c"), b"unseen").unwrap();
        let files = open_files(&fix);
        let named = ["d0000.c", "d0002.c", "sub"].map(String::from);
        files
            .known
            .replay(&named, &["rl".into()], &fix.files, &fix.lens);
        assert_eq!(files.kinds(), [STALE, FRESH, GONE], "only named docs");
        let snap = files.snapshot();
        let added: Vec<&str> = snap
            .added
            .iter()
            .map(|&d| snap.names[d - 3].as_str())
            .collect();
        assert_eq!(added, ["sub/deep/n.c", "rl/r.c"]);
        files
            .known
            .replay(&[String::new()], &[], &fix.files, &fix.lens);
        assert_eq!(
            files.kinds(),
            [STALE, STALE, GONE],
            "the root is everything"
        );
        assert_eq!(files.snapshot().added.len(), 3);
    }

    /// Wait up to 60 s for `done`: a loaded box can hold events that long.
    pub(super) fn eventually(done: impl Fn() -> bool) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while std::time::Instant::now() < deadline {
            if done() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        done()
    }

    /// Stops the watcher when dropped, so a failed assertion inside the
    /// scope fails instead of joining the watcher forever.
    pub(super) struct StopOnDrop<'a>(pub(super) &'a AtomicBool);

    impl Drop for StopOnDrop<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }

    #[test]
    fn watch_restats_what_events_name_and_returns_on_stop() {
        let tmp = tempfile::tempdir().unwrap();
        let fix = stage_files(tmp.path(), &[b"printk", b"zz", b"aa"]);
        std::fs::create_dir(fix.path.join("sub")).unwrap();
        let files = open_files(&fix);
        let watch = files.watch().unwrap();
        files.stat_all(&fix.files, &fix.lens);
        let stop = AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| watch.run(&fix.files, &fix.lens, &stop));
            let _stop = StopOnDrop(&stop);
            let saw = || format!("kinds {:?}, snapshot {:?}", files.kinds(), files.snapshot());
            // FSEvents can miss changes made just after the watch starts, so
            // the write repeats until one is seen.
            let changed = || {
                std::fs::write(fix.path.join(&fix.files[1]), b"zz changed").unwrap();
                files.snapshot().stale == [1]
            };
            assert!(eventually(changed), "{}", saw());
            std::fs::remove_file(fix.path.join(&fix.files[2])).unwrap();
            assert!(eventually(|| files.snapshot().gone == [2]), "{}", saw());
        });
    }

    #[test]
    fn watch_maps_a_directory_event_to_the_docs_under_it() {
        let tmp = tempfile::tempdir().unwrap();
        let fix = stage_files(tmp.path(), &[b"a"]);
        let files = open_files(&fix);
        let watch = files.watch().unwrap();
        let names: Vec<String> = ["a/b", "a/b/c.c", "a/b/d.c", "a/bc.c", "z.c"]
            .map(String::from)
            .to_vec();
        let by_path: Vec<usize> = (0..names.len()).collect();
        let mut ids = Vec::new();
        let root = &watch.root;
        let mut at = |path: &Path| docs_at(root, path, &names, &by_path, &mut ids);
        assert!(!at(&root.join("a/b")));
        assert!(at(root));
        assert!(!at(Path::new("/elsewhere/a/b")));
        assert_eq!(ids, [0, 1, 2]);
    }
}
