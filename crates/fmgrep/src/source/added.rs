//! Files under the root that the manifest does not list: registered by
//! discovery and the watcher, and scanned in full like stale docs.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, MutexGuard, PoisonError};

use super::{IGNORED, Known, Recorded, parallel};
use crate::binary::sniff_text;

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

impl super::Files {
    /// Register every text file under the root that ascending `files` does
    /// not list, and mark the indexed ones an ignore rule now skips; with a
    /// record, only changed and new directories are listed, and the dirs a
    /// changed ignore input affects.
    pub fn discover(&self, files: &[String], lens: &[u64]) {
        let known = &*self.known;
        match &known.recorded {
            Some(recorded) => known.discover_recorded(recorded, files, lens),
            None => known.discover(&known.root, &known.root, files, lens),
        }
        known.complete.store(true, Ordering::Release);
        let n = known.added().live().count();
        if n > 0 && crate::verbose::verbose() {
            eprintln!("note: {n} file(s) added since indexing are scanned in full by every query");
        }
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

/// Ids of ascending `files` at or under `rel`, `""` the root.
fn docs_under<'a>(files: &'a [String], rel: &'a str) -> impl Iterator<Item = usize> + 'a {
    let exact = files.binary_search_by(|f| f.as_str().cmp(rel)).ok();
    let dir = format!("{rel}/");
    let lo = match rel {
        "" => 0,
        _ => files.partition_point(|f| f.as_str() < dir.as_str()),
    };
    let tree =
        (lo..files.len()).take_while(move |&id| rel.is_empty() || files[id].starts_with(&dir));
    exact.into_iter().chain(tree)
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

    /// The paths of `rels` whose first bytes allow text, in order.
    fn text_only(&self, root: &Path, rels: Vec<String>) -> Vec<String> {
        let keep = parallel(&rels, |rel| sniff_text(&root.join(rel)));
        rels.into_iter()
            .zip(keep)
            .filter_map(|(rel, k)| k.then_some(rel))
            .collect()
    }

    /// Walk `under` (at or below `root`) and reconcile what it holds: an
    /// indexed doc the rule skips, or no longer a regular file, is ignored;
    /// one the walk missed or lists again is statted.
    pub fn discover(&self, root: &Path, under: &Path, files: &[String], lens: &[u64]) {
        let walked = crate::walk::walk_paths(root, under, self.exclude.as_deref());
        let Some(rel) = under.strip_prefix(root).ok().and_then(Path::to_str) else {
            return;
        };
        let (mut skipped, mut back, mut at) = (Vec::new(), Vec::new(), 0);
        for id in docs_under(files, rel) {
            at = seek(&walked, at, &files[id]);
            match (walked.get(at) == Some(&files[id]), self.kind(id) == IGNORED) {
                (false, false) => skipped.push(id),
                (true, true) => back.push(id),
                _ => {}
            }
        }
        parallel(&skipped, |&id| {
            let path = root.join(&files[id]);
            match std::fs::symlink_metadata(&path) {
                Ok(m) if !m.is_file() || self.ignored(&files[id], false) => self.set(id, IGNORED),
                Ok(_) => {
                    if let Some(why) = self.stat(id, files, lens, true) {
                        self.exclude(id, &path, &why);
                    }
                }
                Err(_) => {}
            }
        });
        parallel(&back, |&id| self.stat(id, files, lens, true));
        let found = self.text_only(root, unlisted(walked, files));
        self.reconcile(rel, &found);
    }

    /// [`Self::discover`] of the root, listing only the recorded dirs whose
    /// stamp moved and the new ones below them, then walking the dirs a
    /// changed ignore input affects.
    fn discover_recorded(&self, recorded: &Arc<Recorded>, files: &[String], lens: &[u64]) {
        let root = &self.root;
        let changed = recorded.changes(root, &|rel| self.excluded(root, rel));
        let scopes = recorded.ignore_scopes(root, &changed);
        let prune = Arc::clone(recorded);
        let walked =
            crate::walk::walk_changed(root, changed, self.exclude.as_deref(), move |rel| {
                prune.is_dir(rel)
            });
        let found = self.text_only(root, unlisted(walked, files));
        self.reconcile("", &found);
        self.discover_scopes(scopes, files, lens);
    }

    /// Whether `rel` is at or under the index dir.
    fn excluded(&self, root: &Path, rel: &str) -> bool {
        self.exclude
            .as_deref()
            .and_then(|x| x.strip_prefix(root).ok())
            .is_some_and(|x| Path::new(rel).starts_with(x))
    }

    /// Reconcile what is at `path` now: a text file not in ascending `files`
    /// is added, a directory is walked, anything else is dropped; what the
    /// walk's rule skips is left alone.
    pub fn reconcile_path(&self, root: &Path, path: &Path, files: &[String], lens: &[u64]) {
        let Some(rel) = path.strip_prefix(root).ok().and_then(Path::to_str) else {
            return;
        };
        if rel.is_empty() || self.excluded(root, rel) {
            return;
        }
        let meta = std::fs::symlink_metadata(path).ok();
        let is_dir = meta.as_ref().is_some_and(|m| m.is_dir());
        if !is_dir && files.binary_search_by(|f| f.as_str().cmp(rel)).is_ok()
            || self.ignored(rel, is_dir)
        {
            return;
        }
        if is_dir {
            return self.discover(root, path, files, lens);
        }
        let text = meta.is_some_and(|m| m.is_file()) && sniff_text(path);
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
        let path = self.root.join(&rel);
        let text = std::fs::metadata(&path)
            .is_ok_and(|m| m.len() <= crate::binary::MAX_DOC_BYTES)
            .then(|| std::fs::read(&path).ok())
            .flatten()
            .and_then(|raw| crate::binary::text(raw).ok());
        match text {
            Some(text) => Some(text.bytes),
            None => {
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
