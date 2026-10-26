//! Files under the root that the manifest does not list: registered by
//! discovery and the watcher, and scanned in full like stale docs.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use super::{Known, Recorded, WATCH_TICK, parallel};
use crate::binary::sniff_binary;

/// Append-only: slot `i` is doc `num_docs + i` for the life of the process.
#[derive(Default)]
pub struct Added {
    paths: Vec<String>,
    live: Vec<bool>,
    slots: HashMap<String, usize>,
    /// `paths`, shared; dropped on append.
    names: Option<Arc<[String]>>,
}

impl Added {
    /// Whether this changed anything.
    fn set(&mut self, rel: &str, live: bool) -> bool {
        match self.slots.get(rel) {
            Some(&slot) => std::mem::replace(&mut self.live[slot], live) != live,
            None if live => {
                self.slots.insert(rel.to_owned(), self.paths.len());
                self.paths.push(rel.to_owned());
                self.live.push(true);
                self.names = None;
                true
            }
            None => false,
        }
    }

    /// Live paths at or under `rel`; `""` is the root.
    fn live_under(&self, rel: &str) -> Vec<String> {
        let under = |p: &str| {
            rel.is_empty()
                || p.strip_prefix(rel)
                    .is_some_and(|r| r.is_empty() || r.starts_with('/'))
        };
        self.paths
            .iter()
            .zip(&self.live)
            .filter(|&(p, &l)| l && under(p))
            .map(|(p, _)| p.clone())
            .collect()
    }

    /// Live slots, ascending.
    pub fn live(&self) -> impl Iterator<Item = usize> + '_ {
        self.live
            .iter()
            .enumerate()
            .filter(|&(_, &l)| l)
            .map(|(slot, _)| slot)
    }

    /// Every slot's path.
    pub fn names(&mut self) -> Arc<[String]> {
        Arc::clone(
            self.names
                .get_or_insert_with(|| self.paths.as_slice().into()),
        )
    }
}

/// Closed while a [`Discovery`] exists; open when none was made.
#[derive(Default)]
pub struct Latch {
    closed: AtomicBool,
    lock: Mutex<()>,
    open: Condvar,
}

impl Latch {
    fn release(&self) {
        let _held = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        self.closed.store(false, Ordering::Release);
        self.open.notify_all();
    }

    /// Block until open, or until `stop`: whether it opened.
    pub fn wait(&self, stop: Option<&AtomicBool>) -> bool {
        if !self.closed.load(Ordering::Acquire) {
            return true;
        }
        let mut held = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        while self.closed.load(Ordering::Acquire) {
            if stop.is_some_and(|s| s.load(Ordering::Relaxed)) {
                return false;
            }
            held = self
                .open
                .wait_timeout(held, WATCH_TICK)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }
}

/// One walk of the root for added files; waiters are released when it is
/// dropped, run or not.
pub struct Discovery {
    known: Arc<Known>,
}

impl super::Files {
    /// The walk for added files; [`Self::wait_discovered`] blocks until it
    /// is dropped.
    pub fn discovery(&self) -> Discovery {
        self.known.latch.closed.store(true, Ordering::Release);
        Discovery {
            known: Arc::clone(&self.known),
        }
    }
}

impl Discovery {
    /// Register every text file under the root that ascending `files` does
    /// not list; with a record, only changed and new directories are listed.
    pub fn run(self, files: &[String], stop: &AtomicBool) {
        let known = &*self.known;
        if stop.load(Ordering::Relaxed) {
            return;
        }
        known.discovering.store(true, Ordering::Release);
        match &known.recorded {
            Some(recorded) => known.discover_recorded(recorded, files),
            None => known.discover(&known.root, &known.root, files),
        }
        let n = known.added().live().count();
        if n > 0 && crate::verbose::verbose() {
            eprintln!("note: {n} file(s) added since indexing are scanned in full by every query");
        }
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        self.known.latch.release();
    }
}

/// The first index of ascending `files`, from `from` on, not below `rel`.
fn seek(files: &[String], from: usize, rel: &str) -> usize {
    let (mut lo, mut step) = (from, 1);
    while lo + step <= files.len() && files[lo + step - 1].as_str() < rel {
        lo += step;
        step *= 2;
    }
    let hi = (lo + step).min(files.len());
    lo + files[lo..hi].partition_point(|f| f.as_str() < rel)
}

/// Ascending `walked` minus ascending `files`.
fn unlisted(walked: Vec<String>, files: &[String]) -> Vec<String> {
    let mut at = 0;
    walked
        .into_iter()
        .filter(|rel| {
            at = seek(files, at, rel);
            files.get(at) != Some(rel)
        })
        .collect()
}

impl Known {
    pub fn added(&self) -> MutexGuard<'_, Added> {
        self.added.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Make ascending `found` the live added files at or under `rel`.
    fn reconcile(&self, rel: &str, found: &[String]) {
        let mut added = self.added();
        let mut changed = false;
        for old in added.live_under(rel) {
            if found.binary_search(&old).is_err() {
                changed |= added.set(&old, false);
            }
        }
        for rel in found {
            changed |= added.set(rel, true);
        }
        if changed {
            self.epoch.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// The paths of `rels` whose first bytes do not look binary, in order.
    fn text_only(&self, root: &Path, rels: Vec<String>) -> Vec<String> {
        let keep = parallel(&rels, |rel| sniff_binary(&root.join(rel)).is_ok_and(|b| !b));
        rels.into_iter()
            .zip(keep)
            .filter_map(|(rel, k)| k.then_some(rel))
            .collect()
    }

    /// Walk `under` (at or below `root`) and reconcile what it holds.
    pub fn discover(&self, root: &Path, under: &Path, files: &[String]) {
        let walked = crate::walk::walk_paths(root, under, self.exclude.as_deref());
        let found = self.text_only(root, unlisted(walked, files));
        let Some(rel) = under.strip_prefix(root).ok().and_then(Path::to_str) else {
            return;
        };
        self.reconcile(rel, &found);
    }

    /// [`Self::discover`] of the root, listing only the recorded dirs whose
    /// stamp moved and the new ones below them.
    fn discover_recorded(&self, recorded: &Recorded, files: &[String]) {
        let root = &self.root;
        let changed = recorded.changes(root, &|rel| self.excluded(root, rel));
        let walked = crate::walk::walk_changed(root, changed, self.exclude.as_deref(), &|rel| {
            recorded.is_dir(rel)
        });
        let found = self.text_only(root, unlisted(walked, files));
        self.reconcile("", &found);
    }

    /// Whether `rel` is at or under the index dir.
    fn excluded(&self, root: &Path, rel: &str) -> bool {
        self.exclude
            .as_deref()
            .and_then(|x| x.strip_prefix(root).ok())
            .is_some_and(|x| Path::new(rel).starts_with(x))
    }

    /// Reconcile what is at `path` now: a text file not in ascending `files`
    /// is added, a directory is walked, anything else is dropped.
    pub fn reconcile_path(&self, root: &Path, path: &Path, files: &[String]) {
        if !self.discovering.load(Ordering::Acquire) {
            return;
        }
        let Some(rel) = path.strip_prefix(root).ok().and_then(Path::to_str) else {
            return;
        };
        if rel.is_empty() || self.excluded(root, rel) || rel.split('/').any(|c| c.starts_with('.'))
        {
            return;
        }
        let text = match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.is_dir() => return self.discover(root, path, files),
            _ if files.binary_search_by(|f| f.as_str().cmp(rel)).is_ok() => return,
            Ok(meta) => {
                let file = meta.is_file()
                    || meta.is_symlink() && std::fs::metadata(path).is_ok_and(|m| m.is_file());
                file && sniff_binary(path).is_ok_and(|b| !b)
            }
            Err(_) => false,
        };
        let found = if text {
            vec![rel.to_owned()]
        } else {
            Vec::new()
        };
        self.reconcile(rel, &found);
    }

    pub fn read_added(&self, doc: usize) -> Option<Vec<u8>> {
        let slot = doc.checked_sub(self.state.len())?;
        let rel = self.added().paths.get(slot)?.clone();
        match std::fs::read(self.root.join(&rel)) {
            Ok(bytes) if !crate::is_binary_doc(&bytes) => Some(bytes),
            _ => {
                if self.added().set(&rel, false) {
                    self.epoch.fetch_add(1, Ordering::AcqRel);
                }
                None
            }
        }
    }
}

#[cfg(test)]
mod tests;
