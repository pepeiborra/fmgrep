//! Files under the root that the manifest does not list: registered by
//! discovery and by what watchman reports, and scanned in full like stale
//! docs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, MutexGuard, PoisonError};

use super::{IGNORED, Known, Recorded, parallel};
use crate::binary::sniff_text;
use crate::diagnostic::Diagnostic;
use crate::manifest::{Checked, partition_ids};

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
    pub fn discover(&self, files: Checked<'_>, lens: &[u64], left_out: &mut Vec<Diagnostic>) {
        let known = &*self.known;
        let span = tracing::info_span!("discovery").entered();
        match &known.recorded {
            Some(recorded) => known.discover_recorded(recorded, files, lens, left_out),
            None => known.discover(&known.root, &known.root, files, lens, left_out),
        }
        drop(span);
        known.complete.store(true, Ordering::Release);
    }

    /// Under verbose, note how many added files every query scans in full.
    /// After the caller prints what the check left out.
    pub fn note_added(&self) {
        if !crate::verbose::verbose() {
            return;
        }
        let n = self.known.added().live().count();
        if n > 0 {
            eprintln!("note: {n} file(s) added since indexing are scanned in full by every query");
        }
    }
}

/// The first index, from `from` on, of an ascending list of `len` paths
/// (`path(i)` reads item `i`) not below `rel`.
fn seek<'a>(len: usize, path: impl Fn(usize) -> &'a str, from: usize, rel: &str) -> usize {
    let (mut lo, mut step) = (from, 1);
    while lo + step <= len && path(lo + step - 1) < rel {
        lo += step;
        step *= 2;
    }
    partition_ids(lo..(lo + step).min(len), |i| path(i) < rel)
}

/// Ids of ascending `files` at or under `rel`, `""` the root.
pub(super) fn docs_under<'a>(files: Checked<'a>, rel: &'a str) -> impl Iterator<Item = usize> + 'a {
    let exact = files.binary_search(rel).ok();
    let dir = format!("{rel}/");
    let lo = match rel {
        "" => 0,
        _ => files.partition_point(0..files.len(), |f| f < dir.as_str()),
    };
    let tree =
        (lo..files.len()).take_while(move |&id| rel.is_empty() || files[id].starts_with(&dir));
    exact.into_iter().chain(tree)
}

/// Ascending `walked` minus ascending `files`.
pub(super) fn unlisted(walked: Vec<String>, files: Checked<'_>) -> Vec<String> {
    let mut at = 0;
    walked
        .into_iter()
        .filter(|rel| {
            at = seek(files.len(), |i| &files[i], at, rel);
            files.get(at) != Some(rel.as_str())
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

    /// Make each of `found` a live added file.
    pub(super) fn add(&self, found: &[String]) {
        let mut added = self.added();
        let changed = found.iter().fold(false, |c, rel| added.set(rel, true) | c);
        if changed {
            self.epoch.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// The paths of `rels` whose first bytes allow text, in order.
    ///
    /// A file that cannot be read, or is too large, is left out with one
    /// entry in `left_out`. A binary one is not named, as at the index.
    pub(super) fn text_only(
        &self,
        root: &Path,
        rels: Vec<String>,
        left_out: &mut Vec<Diagnostic>,
    ) -> Vec<String> {
        let sniffed = parallel(&rels, |rel| sniff_text(&root.join(rel)));
        // The entries come in path order, whichever thread ended first.
        rels.into_iter()
            .zip(sniffed)
            .filter_map(|(rel, text)| match text {
                Ok(text) => text.then_some(rel),
                Err(why) => {
                    self.skipped(root.join(&rel), why, left_out);
                    None
                }
            })
            .collect()
    }

    /// Report that `path`, a file new since the index, is left out for `why`.
    fn skipped(&self, path: PathBuf, why: String, left_out: &mut Vec<Diagnostic>) {
        self.left_out_once(Diagnostic::Skipped { path, why }, left_out);
    }

    /// Walk `under` (at or below `root`) and reconcile what it holds: an
    /// indexed doc the rule skips, or no longer a regular file, is ignored;
    /// one the walk missed or lists again is statted.
    pub fn discover(
        &self,
        root: &Path,
        under: &Path,
        files: Checked<'_>,
        lens: &[u64],
        left_out: &mut Vec<Diagnostic>,
    ) {
        let walked = crate::walk::walk_paths(root, under, self.exclude.as_deref());
        self.left_out_unlisted(walked.unlisted, left_out);
        let walked = walked.files;
        let Some(rel) = under.strip_prefix(root).ok().and_then(Path::to_str) else {
            return;
        };
        let (mut skipped, mut back, mut at) = (Vec::new(), Vec::new(), 0);
        for id in docs_under(files, rel) {
            at = seek(walked.len(), |i| walked[i].as_str(), at, &files[id]);
            let listed = walked.get(at).map(String::as_str) == Some(&files[id]);
            match (listed, self.kind(id) == IGNORED) {
                (false, false) => skipped.push(id),
                (true, true) => back.push(id),
                _ => {}
            }
        }
        let whys = parallel(&skipped, |&id| {
            match std::fs::symlink_metadata(root.join(&files[id])) {
                Ok(m) if !m.is_file() || self.ignored(&files[id], false) => {
                    self.set(id, IGNORED);
                    None
                }
                Ok(_) => self.stat(id, files, lens, true),
                Err(_) => None,
            }
        });
        // In doc order, whichever thread ended first.
        for (&id, why) in skipped.iter().zip(whys) {
            if let Some(why) = why {
                self.exclude(id, &root.join(&files[id]), &why, left_out);
            }
        }
        parallel(&back, |&id| self.stat(id, files, lens, true));
        let found = self.text_only(root, unlisted(walked, files), left_out);
        self.reconcile(rel, &found);
    }

    /// [`Self::discover`] of the root, listing only the recorded dirs whose
    /// stamp moved and the new ones below them, then walking the dirs a
    /// changed ignore input affects.
    fn discover_recorded(
        &self,
        recorded: &Arc<Recorded>,
        files: Checked<'_>,
        lens: &[u64],
        left_out: &mut Vec<Diagnostic>,
    ) {
        let root = &self.root;
        let changed = recorded.changes(root, &|rel| self.excluded(root, rel));
        let scopes = self.apply_ignores(recorded.ignore_scopes(root, &changed));
        let prune = Arc::clone(recorded);
        let walked =
            crate::walk::walk_changed(root, changed, self.exclude.as_deref(), move |rel| {
                prune.is_dir(rel)
            });
        self.left_out_unlisted(walked.unlisted, left_out);
        let found = self.text_only(root, unlisted(walked.files, files), left_out);
        self.reconcile("", &found);
        self.discover_scopes(scopes, files, lens, left_out);
    }

    /// Whether `rel` is at or under the index dir.
    pub(super) fn excluded(&self, root: &Path, rel: &str) -> bool {
        self.exclude
            .as_deref()
            .and_then(|x| x.strip_prefix(root).ok())
            .is_some_and(|x| Path::new(rel).starts_with(x))
    }

    pub fn read_added(&self, doc: usize, left_out: &mut Vec<Diagnostic>) -> Option<Vec<u8>> {
        let slot = doc.checked_sub(self.state.len())?;
        let rel = self.added().paths.get(slot)?.clone();
        let path = self.root.join(&rel);
        match crate::binary::read_text(&path) {
            Ok(text) => Some(text.bytes),
            Err(why) => {
                if let Some(why) = why {
                    self.skipped(path, why, left_out);
                }
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
