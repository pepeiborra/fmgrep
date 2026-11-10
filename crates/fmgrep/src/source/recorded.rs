//! `dirs.stamps`: every directory the index walk listed, with its stamp, so
//! discovery at load lists only what changed; `ignores.stamps`: every ignore
//! input, so a load re-walks what an ignore file's change affects.
//!
//! An 8-byte magic (`fmdirs\0\x01`, `fmigns\0\x01`), then records to EOF,
//! strictly ascending by path: `u64 LE len | i64 LE mtime_ns | u32 LE n | n
//! bytes of UTF-8 path`. `dirs.stamps` starts with the root `""`. A stamp no
//! stat can vouch for is written [`UNKNOWN`], an absent file [`ABSENT`].

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::Stamp;
use crate::walk::WalkNotes;

use super::parallel;

const DIRS_STAMPS: &str = "dirs.stamps";
const IGNORES_STAMPS: &str = "ignores.stamps";

const DIRS_MAGIC: &[u8; 8] = b"fmdirs\0\x01";
const IGNORES_MAGIC: &[u8; 8] = b"fmigns\0\x01";
/// Bytes of a record's stamp.
const STAMP: usize = 16;

/// Equals no stat, so its directory is listed.
const UNKNOWN: Stamp = Stamp {
    len: u64::MAX,
    mtime_ns: i64::MIN,
};
/// Equals a failed stat.
const ABSENT: Stamp = Stamp {
    len: u64::MAX,
    mtime_ns: i64::MAX,
};
/// A stamp this close to the index start may hide a change in the same
/// timestamp tick.
const RACY: Duration = Duration::from_secs(2);

/// Write `dirs.stamps` and `ignores.stamps` of `walk`, which began at `started`.
pub fn write_walk(output: &Path, walk: &WalkNotes, started: SystemTime) -> Result<(), String> {
    let since = started
        .checked_sub(RACY)
        .unwrap_or(UNIX_EPOCH)
        .duration_since(UNIX_EPOCH);
    let racy = since.map_or(i64::MIN, |d| {
        i64::try_from(d.as_nanos()).unwrap_or(i64::MAX)
    });
    let vouched = |s: Option<Stamp>| s.filter(|s| s.mtime_ns < racy).unwrap_or(UNKNOWN);
    let dirs = walk.dirs.iter().map(|(rel, s)| (rel.as_str(), vouched(*s)));
    write_table(&output.join(DIRS_STAMPS), DIRS_MAGIC, dirs)?;
    let ignores = walk
        .ignores
        .iter()
        .map(|(rel, s)| (rel.as_str(), s.map_or(ABSENT, |s| vouched(Some(s)))));
    write_table(&output.join(IGNORES_STAMPS), IGNORES_MAGIC, ignores)
}

/// Ascending `rows`; paths that collide (lossy names) are written once, [`UNKNOWN`].
fn write_table<'a>(
    path: &Path,
    magic: &[u8; 8],
    rows: impl Iterator<Item = (&'a str, Stamp)>,
) -> Result<(), String> {
    let mut out = magic.to_vec();
    let mut rows = rows.peekable();
    while let Some((rel, mut stamp)) = rows.next() {
        while rows.next_if(|&(next, _)| next == rel).is_some() {
            stamp = UNKNOWN;
        }
        let n = u32::try_from(rel.len())
            .map_err(|_| format!("error: path too long for {}: {rel}", path.display()))?;
        out.extend_from_slice(&stamp.len.to_le_bytes());
        out.extend_from_slice(&stamp.mtime_ns.to_le_bytes());
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(rel.as_bytes());
    }
    std::fs::write(path, out).map_err(|e| format!("error: cannot write {}: {e}", path.display()))
}

/// Paths ascending, each with its stamp.
struct Table {
    text: String,
    /// Per path: where it ends in `text`, and its stamp.
    rows: Vec<(usize, Stamp)>,
}

impl Table {
    fn path(&self, i: usize) -> &str {
        let start = if i == 0 { 0 } else { self.rows[i - 1].0 };
        &self.text[start..self.rows[i].0]
    }

    fn find(&self, rel: &str) -> Option<usize> {
        let (mut lo, mut hi) = (0, self.rows.len());
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match self.path(mid).cmp(rel) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Some(mid),
            }
        }
        None
    }
}

/// `None` when absent or of another layout.
fn read_table(path: &Path, magic: &[u8; 8]) -> Result<Option<Table>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("error: cannot read {}: {e}", path.display())),
    };
    let tampered =
        |why: &str| format!("error: {} {why}; refusing a tampered index", path.display());
    let Some(body) = bytes.strip_prefix(&magic[..]) else {
        return if bytes.len() < magic.len() {
            Err(tampered("is truncated"))
        } else {
            Ok(None)
        };
    };
    let mut table = Table {
        text: String::new(),
        rows: Vec::new(),
    };
    let (mut at, len) = (0, STAMP + 4);
    while at < body.len() {
        let head = body
            .get(at..at + len)
            .ok_or_else(|| tampered("is truncated"))?;
        let word = |from: usize| -> [u8; 8] { head[from..from + 8].try_into().expect("8 bytes") };
        let stamp = Stamp {
            len: u64::from_le_bytes(word(0)),
            mtime_ns: i64::from_le_bytes(word(8)),
        };
        let n = u32::from_le_bytes(head[STAMP..len].try_into().expect("4 bytes")) as usize;
        let raw = body
            .get(at + len..at + len + n)
            .ok_or_else(|| tampered("is truncated"))?;
        let rel = std::str::from_utf8(raw).map_err(|_| tampered("holds a non-UTF-8 path"))?;
        let start = table.text.len();
        if !table.rows.is_empty() && table.path(table.rows.len() - 1) >= rel {
            return Err(tampered("is not ascending"));
        }
        table.text.push_str(rel);
        table.rows.push((start + n, stamp));
        at += len + n;
    }
    Ok(Some(table))
}

/// `dirs.stamps` and `ignores.stamps`, as `index` wrote them.
pub struct Recorded {
    dirs: Table,
    ignores: Table,
}

impl Recorded {
    /// `None` when either is absent or of another layout; a malformed one is
    /// refused.
    pub fn open(index_dir: &Path) -> Result<Option<Self>, String> {
        let path = index_dir.join(DIRS_STAMPS);
        let Some(dirs) = read_table(&path, DIRS_MAGIC)? else {
            return Ok(None);
        };
        if dirs.rows.is_empty() || !dirs.path(0).is_empty() {
            return Err(format!(
                "error: {} does not start with the root; refusing a tampered index",
                path.display()
            ));
        }
        let ignores = read_table(&index_dir.join(IGNORES_STAMPS), IGNORES_MAGIC)?;
        Ok(ignores.map(|ignores| Self { dirs, ignores }))
    }

    pub fn is_dir(&self, rel: &str) -> bool {
        self.dirs.find(rel).is_some()
    }

    /// The recorded dirs the walk from `root` still reaches (`excluded`
    /// aside) whose stamp moved.
    pub fn changes(&self, root: &Path, excluded: &(dyn Fn(&str) -> bool + Sync)) -> Vec<PathBuf> {
        let rows: Vec<usize> = (0..self.dirs.rows.len()).collect();
        let now = parallel(&rows, |&i| {
            let rel = self.dirs.path(i);
            let meta = if rel.is_empty() {
                std::fs::metadata(root)
            } else {
                std::fs::symlink_metadata(root.join(rel))
            };
            meta.ok().filter(|m| m.is_dir()).map(|m| Stamp::of(&m))
        });
        // A parent sorts before its children.
        let mut live = vec![false; rows.len()];
        let mut changed = Vec::new();
        for i in rows {
            let rel = self.dirs.path(i);
            let parent = rel.rfind('/').map_or("", |at| &rel[..at]);
            let reached = i == 0 || self.dirs.find(parent).is_some_and(|p| p < i && live[p]);
            live[i] = reached && now[i].is_some() && !excluded(rel);
            if live[i] && now[i] != Some(self.dirs.rows[i].1) {
                changed.push(if rel.is_empty() {
                    root.to_path_buf()
                } else {
                    root.join(rel)
                });
            }
        }
        changed
    }

    /// The root-relative dirs to walk again since `index` because an ignore
    /// input moved, or one appeared in one of the `changed` dirs; outermost
    /// only, ascending.
    pub fn ignore_scopes(&self, root: &Path, changed: &[PathBuf]) -> Vec<String> {
        let rows: Vec<usize> = (0..self.ignores.rows.len()).collect();
        let moved = parallel(&rows, |&i| {
            let rel = self.ignores.path(i);
            let now = crate::walk::stamp(&resolve(root, rel)).unwrap_or(ABSENT);
            now != self.ignores.rows[i].1
        });
        let mut scopes: Vec<String> = rows
            .into_iter()
            .filter(|&i| moved[i])
            .map(|i| {
                let rel = self.ignores.path(i);
                match crate::walk::ignore_file_dir(rel) {
                    Some(dir) if !rel.starts_with("../") && !Path::new(rel).is_absolute() => {
                        dir.to_owned()
                    }
                    _ => String::new(),
                }
            })
            .collect();
        for dir in changed {
            let Some(rel) = dir.strip_prefix(root).ok().and_then(Path::to_str) else {
                continue;
            };
            let new = crate::walk::ignore_files(dir).any(|(name, s)| {
                s.is_some()
                    && self
                        .ignores
                        .find(&crate::walk::join_rel(rel, name))
                        .is_none()
            });
            if new {
                scopes.push(rel.to_owned());
            }
        }
        outermost(scopes)
    }
}

/// `rel` of an ignores row against `root`: `../` climbs, an absolute path is itself.
fn resolve(root: &Path, rel: &str) -> PathBuf {
    let mut base = root;
    let mut rest = rel;
    while let Some(up) = rest.strip_prefix("../") {
        base = base.parent().unwrap_or(base);
        rest = up;
    }
    base.join(rest)
}

/// Ascending, deduped, without any dir under another.
pub fn outermost(mut dirs: Vec<String>) -> Vec<String> {
    dirs.sort_unstable();
    dirs.dedup();
    let mut kept: Vec<String> = Vec::new();
    for dir in dirs {
        let nested = kept.iter().any(|top| {
            top.is_empty()
                || dir
                    .strip_prefix(top.as_str())
                    .is_some_and(|r| r.starts_with('/'))
        });
        if !nested {
            kept.push(dir);
        }
    }
    kept
}

#[cfg(test)]
mod tests;
