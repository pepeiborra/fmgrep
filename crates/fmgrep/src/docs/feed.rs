//! The producer/consumer seam between the document stream and the build. An
//! announced cut is always readable from its [`CutSource`]. Every end path
//! (clean, error or panic) marks the feed ended and wakes every waiter; a
//! build that gives up aborts the feed, which stops the stream.

use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use crate::{Document, Stamp};

use crate::docs::read::{read_cut, read_files};

/// One blob slot's documents as the stream closed them: the global doc range,
/// its start offset in the concatenated documents, and its per-doc table.
/// Self-describing, so a build never locks the live tables to read it.
#[derive(Clone)]
pub struct Cut {
    pub docs: Range<usize>,
    pub at: u64,
    pub paths: Vec<String>,
    pub lens: Vec<u64>,
    /// The walk's mtimes; empty for a closed (in-memory) cut.
    pub mtimes: Vec<i64>,
}

/// Where [`Cuts::read`] reads a cut back.
#[derive(Clone)]
pub enum CutSource {
    /// `docs.shard`, flushed through every announced cut.
    Shard(PathBuf),
    /// The walked files under this root, checked against the walk's stamps.
    Files(PathBuf),
}

struct Live {
    docs: usize,
    cuts: Vec<Cut>,
    /// The latest cut's documents, until read or replaced by the next cut.
    held: Option<(usize, Vec<Document>)>,
    ended: bool,
    error: Option<String>,
}

struct Feed {
    /// Where [`Cuts::read`] reads; `None` when the consumer holds the docs.
    source: Option<CutSource>,
    live: Mutex<Live>,
    ready: Condvar,
    abort: AtomicBool,
}

impl Feed {
    fn lock(&self) -> MutexGuard<'_, Live> {
        self.live.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn wait<'a>(&self, live: MutexGuard<'a, Live>) -> MutexGuard<'a, Live> {
        self.ready.wait(live).unwrap_or_else(|e| e.into_inner())
    }

    fn aborted(&self) -> bool {
        self.abort.load(Ordering::Relaxed)
    }
}

/// Shard cuts as the document stream closes them.
pub struct Cuts {
    feed: Arc<Feed>,
}

impl Cuts {
    /// An already-final source over a known plan. `shard` is where
    /// [`Cuts::read`] looks. A slot outside `lens` is a loud error, never a
    /// panic.
    #[cfg(test)]
    pub fn closed(
        shard: Option<PathBuf>,
        plan: crate::plan::ShardPlan,
        lens: &[u64],
        paths: &[String],
    ) -> Result<Self, String> {
        let offsets = crate::manifest::shard_offsets(lens)?;
        let mut cuts = Vec::with_capacity(plan.len());
        for docs in plan {
            if docs.start > docs.end || docs.end > lens.len() {
                return Err(format!(
                    "error: doc range {}..{} is out of range for {} doc(s)",
                    docs.start,
                    docs.end,
                    lens.len()
                ));
            }
            cuts.push(Cut {
                at: offsets[docs.start],
                paths: paths[docs.clone()].to_vec(),
                lens: lens[docs.clone()].to_vec(),
                mtimes: Vec::new(),
                docs,
            });
        }
        Ok(Self {
            feed: Arc::new(Feed {
                source: shard.map(CutSource::Shard),
                live: Mutex::new(Live {
                    docs: lens.len(),
                    cuts,
                    held: None,
                    ended: true,
                    error: None,
                }),
                ready: Condvar::new(),
                abort: AtomicBool::new(false),
            }),
        })
    }

    /// Blocks until slot `slot` is closed. `None` means the plan is final and
    /// holds fewer slots — including when the stream failed, whose error
    /// [`Cuts::error`] carries — or that the build aborted.
    pub fn get(&self, slot: usize) -> Option<Cut> {
        let mut live = self.feed.lock();
        while slot >= live.cuts.len() && !live.ended && !self.feed.aborted() {
            live = self.feed.wait(live);
        }
        live.cuts.get(slot).cloned()
    }

    /// Why the stream stopped short, once `get` returned `None`.
    pub fn error(&self) -> Option<String> {
        self.feed.lock().error.clone()
    }

    /// Documents streamed; final once `get` has returned `None`.
    pub fn num_docs(&self) -> usize {
        self.feed.lock().docs
    }

    /// The documents of `cut`, in id order. A files source hands over the
    /// latest cut's streamed bytes once; any other read re-reads the files.
    pub fn read(&self, cut: &Cut) -> Result<Vec<Document>, String> {
        match &self.feed.source {
            Some(CutSource::Shard(shard)) => read_cut(shard, cut),
            Some(CutSource::Files(root)) => {
                let mut live = self.feed.lock();
                match live.held.take() {
                    Some((start, docs)) if start == cut.docs.start => Ok(docs),
                    held => {
                        live.held = held;
                        drop(live);
                        read_files(root, cut)
                    }
                }
            }
            None => Err("these cuts hold no document shard to read".to_string()),
        }
    }

    /// Stop serving cuts and stop the stream: a build that gives up.
    pub fn abort(&self) {
        self.feed.abort.store(true, Ordering::Relaxed);
        drop(self.feed.lock());
        self.feed.ready.notify_all();
    }
}

/// The stream's end: it owns the per-doc tables and announces cuts. Dropping
/// it ends the feed, so a panicking stream wakes every waiter and the feed
/// carries [`BROKEN`] rather than looking like an empty corpus.
pub struct Writer {
    feed: Arc<Feed>,
    paths: Vec<String>,
    lens: Vec<u64>,
    mtimes: Vec<i64>,
    /// The open cut's documents, when the stream hands them over.
    open: Vec<Document>,
    /// Offset in the concatenated documents of every document appended so
    /// far; the `docs.shard` offset for a shard source.
    offsets: Vec<u64>,
    at: u64,
    ended: bool,
}

/// Per-doc tables of a whole stream, ascending by id.
pub struct Tables {
    pub paths: Vec<String>,
    pub lens: Vec<u64>,
    pub mtimes: Vec<i64>,
}

/// The feed's error when the stream left without calling [`Writer::end`].
const BROKEN: &str = "error: the document stream stopped without reporting an error";

/// A live feed plus the writer filling it.
pub fn feed(source: CutSource) -> (Cuts, Writer) {
    let feed = Arc::new(Feed {
        source: Some(source),
        live: Mutex::new(Live {
            docs: 0,
            cuts: Vec::new(),
            held: None,
            ended: false,
            error: None,
        }),
        ready: Condvar::new(),
        abort: AtomicBool::new(false),
    });
    let writer = Writer {
        feed: Arc::clone(&feed),
        paths: Vec::new(),
        lens: Vec::new(),
        mtimes: Vec::new(),
        open: Vec::new(),
        offsets: Vec::new(),
        at: 0,
        ended: false,
    };
    (Cuts { feed }, writer)
}

impl Writer {
    /// `true` once the build gave up: the stream may stop.
    pub fn aborted(&self) -> bool {
        self.feed.aborted()
    }

    /// Record a streamed document and announce `cut` if it closed one. Only
    /// call once the source is readable through `cut`. `bytes`, when given,
    /// are handed to the cut's first read instead of re-reading the file.
    pub fn pushed(
        &mut self,
        path: String,
        stamp: Stamp,
        bytes: Option<Vec<u8>>,
        cut: Option<Range<usize>>,
    ) {
        if let Some(bytes) = bytes {
            self.open.push(Document {
                id: self.paths.len(),
                path: path.clone(),
                bytes,
            });
        }
        self.offsets.push(self.at);
        self.at += stamp.len;
        self.paths.push(path);
        self.lens.push(stamp.len);
        self.mtimes.push(stamp.mtime_ns);
        if let Some(cut) = cut {
            self.announce(cut);
        }
    }

    /// Announce the trailing cut, end the feed, yield the per-doc tables.
    pub fn end(mut self, cut: Option<Range<usize>>, error: Option<String>) -> Tables {
        if let Some(cut) = cut {
            self.announce(cut);
        }
        self.ended = true;
        let mut live = self.feed.lock();
        live.docs = self.paths.len();
        live.error = error;
        live.ended = true;
        drop(live);
        self.feed.ready.notify_all();
        Tables {
            paths: std::mem::take(&mut self.paths),
            lens: std::mem::take(&mut self.lens),
            mtimes: std::mem::take(&mut self.mtimes),
        }
    }

    fn announce(&mut self, docs: Range<usize>) {
        // A byte cut closes before the document that overflowed it.
        let rest = self
            .open
            .split_off(self.open.partition_point(|d| d.id < docs.end));
        let held = std::mem::replace(&mut self.open, rest);
        let cut = Cut {
            at: self.offsets[docs.start],
            paths: self.paths[docs.clone()].to_vec(),
            lens: self.lens[docs.clone()].to_vec(),
            mtimes: self.mtimes[docs.clone()].to_vec(),
            docs,
        };
        let mut live = self.feed.lock();
        live.docs = self.paths.len();
        live.held =
            (held.len() == cut.docs.len() && !held.is_empty()).then_some((cut.docs.start, held));
        live.cuts.push(cut);
        drop(live);
        self.feed.ready.notify_all();
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        let mut live = self.feed.lock();
        live.ended = true;
        if !self.ended && live.error.is_none() {
            live.error = Some(BROKEN.to_string());
        }
        drop(live);
        self.feed.ready.notify_all();
    }
}

#[cfg(test)]
mod tests;
