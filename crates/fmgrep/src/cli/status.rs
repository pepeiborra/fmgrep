//! `status`: what a search started in PATH would read, and the state of
//! it, for a person or, under `--json`, as one object.

use std::io::Write;
use std::path::Path;

use serde::Serialize;

use super::serve::{ServeStatus, status_on};
use super::{Choice, Env, Reached, Server, choose, has_blobs, print, reach, served_tree, tree_of};
use crate::fetch::{Ask, DocStore};
use crate::manifest::{Manifest, read_manifest};
use crate::source::{Ignores, Looked};

/// `status --json`. Keys may be added, never renamed; `null` is "not
/// known" or "does not apply".
#[derive(Serialize)]
struct Report<'a> {
    /// `"index"` or `"server"`.
    uses: &'static str,
    /// What a search warns after `Using local index DIR`.
    note: Option<String>,
    index: Option<IndexReport<'a>>,
    /// How a search learns what changed; `null` for stored documents or a
    /// server.
    changes: Option<Changes>,
    server: Option<ServerReport>,
}

#[derive(Serialize)]
struct Changes {
    /// Whether watchman can say what changed.
    watchman: bool,
    /// Why not, and what a search does instead.
    why: Option<String>,
    /// The record that a search keeps for the next one, when it exists.
    record: Option<String>,
    /// Indexed files whose size or mtime moved; `null` when not counted.
    stale: Option<usize>,
    /// Text files the index does not list; `null` when not counted.
    added: Option<usize>,
    /// Indexed files missing, unreadable or now ignored; `null` when not
    /// counted.
    gone: Option<usize>,
    /// Ignore inputs changed since the index; `null` when the index records
    /// none.
    ignores: Option<Vec<String>>,
    /// Whether `ignores` looked for new ignore files too, or only statted
    /// those the index recorded; `null` with `ignores`.
    ignores_all: Option<bool>,
}

impl Changes {
    fn of(l: Looked) -> Self {
        Self {
            watchman: l.watchman,
            why: (!l.watchman).then_some(l.line),
            record: l.record.map(|r| r.display().to_string()),
            stale: l.counts.map(|c| c.stale),
            added: l.counts.map(|c| c.added),
            gone: l.counts.map(|c| c.gone),
            ignores_all: l.ignores.as_ref().map(|i| i.all),
            ignores: l.ignores.map(|i| i.names),
        }
    }
}

/// The `changes:` line of an index that holds the files.
const STORED: &str = "none: the index holds the files; a search reads none of the tree";

#[derive(Serialize)]
struct IndexReport<'a> {
    /// Canonical.
    dir: String,
    manifest: &'a Manifest,
}

#[derive(Serialize)]
struct ServerReport {
    addr: String,
    /// Whether `FMGREP_SERVER` gave it.
    env: bool,
    /// What `fmgrep --status` prints.
    status: ServeStatus,
}

/// Report on what a search started in `path` (else the cwd) reads: the
/// index or server that [`choose`] and [`reach`] pick, with `server` as a
/// search's `--server`. Fails where that search fails before it searches,
/// with its words.
pub fn run(
    path: Option<&Path>,
    server: Option<&str>,
    json: bool,
    env: Env,
    out: &mut dyn Write,
) -> Result<(), String> {
    let start = path.unwrap_or(Path::new("."));
    let here = match path {
        Some(p) => format!("in {}", canonical(p)),
        None => "here".to_owned(),
    };
    let (found, note) = match choose(None, server, env, start)? {
        Choice::Index { found, note } => (found, note),
        Choice::Server(server) => match reach(&server, start)? {
            Reached::Conn(mut conn) => {
                let status = status_on(&mut conn)?;
                return served(out, json, &here, server, status);
            }
            Reached::Index { found, note } => (found, Some(note)),
        },
    };
    has_blobs(&found)?;
    let manifest = read_manifest(&found.index_dir)?;
    let dir = canonical(&found.index_dir);
    let root = found.root.as_deref().filter(|_| !manifest.store_docs);
    let docs = DocStore::open(&found.index_dir, &manifest, root, Ask::Look)?;
    let looked = docs.look(&manifest.files);
    if json {
        let report = Report {
            uses: "index",
            note,
            index: Some(IndexReport {
                dir,
                manifest: &manifest,
            }),
            changes: looked.map(Changes::of),
            server: None,
        };
        return print::line(out, &encoded(&report)?);
    }
    let note = note.unwrap_or_default();
    let mut lines = vec![
        format!("a search {here} uses the local index {dir}{note}"),
        format!(
            "index: format {}, built {} ago, {} files, {} of text, {} of index",
            manifest.format,
            ago(manifest.index_started_ns),
            manifest.num_docs,
            size(manifest.num_bytes),
            size(manifest.index_size_bytes),
        ),
    ];
    lines.extend(tree_of(&found, &manifest).map(|t| format!("tree: {}", canonical(&t))));
    let changes = looked.as_ref().map_or(STORED, |l: &Looked| &l.line);
    lines.push(format!("changes: {changes}"));
    if let Some(l) = &looked {
        lines.push(format!("since the index: {}", counted(l)));
        lines.push(format!("ignore files: {}", ignores(l.ignores.as_ref())));
    }
    lines.iter().try_for_each(|l| print::line(out, l))
}

/// The `since the index:` line.
fn counted(l: &Looked) -> String {
    match l.counts {
        Some(c) => format!(
            "{} stale, {} added, {} gone (a search reads {} file{} in full)",
            c.stale,
            c.added,
            c.gone,
            c.stale + c.added,
            crate::verbose::plural(c.stale + c.added),
        ),
        None => "not known without checking every file; a search checks".to_owned(),
    }
}

/// The `ignore files:` line.
fn ignores(found: Option<&Ignores>) -> String {
    let Some(found) = found else {
        return "not known: the index records none".to_owned();
    };
    let n = found.names.len();
    let dirs = if n == 1 {
        "its directory"
    } else {
        "their directories"
    };
    match (n, found.all) {
        (0, true) => "none changed since the index".to_owned(),
        (0, false) => "none changed since the index (of those the index recorded)".to_owned(),
        _ => format!(
            "{n} changed since the index ({}); every search reads {dirs} again (re-run index)",
            found.names.join(", ")
        ),
    }
}

/// The report on a server that answered.
fn served(
    out: &mut dyn Write,
    json: bool,
    here: &str,
    server: Server,
    status: ServeStatus,
) -> Result<(), String> {
    let first = format!("a search {here} uses {server}");
    if json {
        let report = Report {
            uses: "server",
            note: None,
            index: None,
            changes: None,
            server: Some(ServerReport {
                addr: server.addr,
                env: server.env,
                status,
            }),
        };
        return print::line(out, &encoded(&report)?);
    }
    let root = Path::new(&status.root);
    let place = match (status.store_docs, served_tree(root, status.store_docs)) {
        (false, _) => format!("tree {}", status.root),
        (true, Some(tree)) => format!("tree {} (stored documents)", tree.display()),
        (true, None) => format!("index {} (stored documents)", status.root),
    };
    let line = format!(
        "server: {place}, {} files, {} of text, {} of index, loaded in {}",
        status.files,
        size(status.num_bytes),
        size(status.index_size_bytes),
        took(status.load_us),
    );
    [first, line].iter().try_for_each(|l| print::line(out, l))
}

fn encoded(report: &Report<'_>) -> Result<String, String> {
    serde_json::to_string(report).map_err(|e| format!("error: failed to encode status: {e}"))
}

/// `p` resolved, else as given.
fn canonical(p: &Path) -> String {
    let resolved = std::fs::canonicalize(p);
    resolved.as_deref().unwrap_or(p).display().to_string()
}

/// How long ago `ns` (since the epoch) was, in its largest whole unit.
fn ago(ns: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let secs = now.saturating_sub(u128::try_from(ns).unwrap_or(0)) / 1_000_000_000;
    match secs {
        0..60 => format!("{secs} s"),
        60..3600 => format!("{} min", secs / 60),
        3600..86400 => format!("{} h", secs / 3600),
        _ => format!("{} d", secs / 86400),
    }
}

/// `bytes` in binary units, one decimal past bytes.
fn size(bytes: u64) -> String {
    let units = ["KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = "B";
    for u in units {
        if value < 1024.0 {
            break;
        }
        value /= 1024.0;
        unit = u;
    }
    match unit {
        "B" => format!("{bytes} B"),
        _ => format!("{value:.1} {unit}"),
    }
}

/// `us` microseconds, in ms below a second.
fn took(us: u64) -> String {
    match us {
        0..1_000_000 => format!("{} ms", us / 1000),
        _ => format!("{:.1} s", us as f64 / 1e6),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_times_read_in_their_largest_unit() {
        let sizes = [(0, "0 B"), (1023, "1023 B"), (1024, "1.0 KiB")];
        for (n, want) in sizes {
            assert_eq!(size(n), want);
        }
        assert_eq!(size(20_508_468_838), "19.1 GiB");
        assert_eq!(size(7 << 40), "7.0 TiB");
        assert_eq!(took(3_100_000), "3.1 s");
        assert_eq!(took(44_900), "44 ms");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64;
        let s = 1_000_000_000;
        assert_eq!(ago(now - 5 * s), "5 s");
        assert_eq!(ago(now - 240 * s), "4 min");
        assert_eq!(ago(now - 3 * 3600 * s), "3 h");
        assert_eq!(ago(now - 2 * 86400 * s), "2 d");
        assert_eq!(ago(now + 60 * s), "0 s", "a clock that moved back");
    }
}
