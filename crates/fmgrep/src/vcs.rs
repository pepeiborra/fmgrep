//! Git, run as a child process: which commit a tree holds, and how a
//! checkout differs from a commit. Every call runs in a given directory
//! with the repository variables of the caller's environment removed, so
//! that git finds the repository from that directory alone; it takes no
//! optional lock, never fetches a missing object, never prompts, and reads
//! nothing on stdin.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

pub mod served;

/// What `git rev-parse --local-env-vars` lists: variables that point git
/// at another repository, index or object store than the directory's.
const REPO_VARS: [&str; 16] = [
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_OBJECT_DIRECTORY",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_INDEX_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_PREFIX",
    "GIT_INTERNAL_SUPER_PREFIX",
    "GIT_SHALLOW_FILE",
    "GIT_COMMON_DIR",
];

/// `git -C dir <args>`, as the module says.
fn command(dir: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    for var in REPO_VARS {
        cmd.env_remove(var);
    }
    cmd.env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// Why git failed: the first line of its stderr, else its exit status.
fn failed(stderr: &[u8], status: std::process::ExitStatus) -> String {
    let text = String::from_utf8_lossy(stderr);
    match text.lines().map(str::trim).find(|l| !l.is_empty()) {
        Some(line) => format!("git failed: {line}"),
        None => format!("git failed: {status}"),
    }
}

fn spawn_failed(e: std::io::Error) -> String {
    format!("cannot run git ({e})")
}

/// The stdout of `git -C dir <args>`, or why there is none.
pub fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = command(dir, args).output().map_err(spawn_failed)?;
    match out.status.success() {
        true => Ok(out.stdout),
        false => Err(failed(&out.stderr, out.status)),
    }
}

/// Whether `git -C dir <args>` succeeds, its output dropped; or why git
/// cannot run.
pub fn git_ok(dir: &Path, args: &[&str]) -> Result<bool, String> {
    let status = command(dir, args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(spawn_failed)?;
    Ok(status.success())
}

/// `out` as one line of UTF-8, its newline dropped.
pub fn line(out: Vec<u8>) -> Result<String, String> {
    let text = String::from_utf8(out).map_err(|_| "git printed a path that is not UTF-8")?;
    Ok(text.trim_end_matches('\n').to_owned())
}

/// The NUL-terminated records of `out`.
pub fn records(out: &[u8]) -> impl Iterator<Item = &[u8]> {
    out.split(|&b| b == 0).filter(|r| !r.is_empty())
}

/// The path of one `git status --porcelain=v2 -z` change record: after 8
/// fields for an ordinary change (`1`), 9 for a rename or copy (`2`), 10
/// for an unmerged path (`u`), 1 for an untracked or ignored one.
pub fn status_path(record: &str) -> Option<&str> {
    let fields = match record.as_bytes().first()? {
        b'1' => 8,
        b'2' => 9,
        b'u' => 10,
        b'?' | b'!' => 1,
        _ => return None,
    };
    record.splitn(fields + 1, ' ').nth(fields)
}

/// The top of the work tree that holds `dir`, canonical.
pub fn top(dir: &Path) -> Result<std::path::PathBuf, String> {
    let top = line(git(dir, &["rev-parse", "--show-toplevel"])?)?;
    std::fs::canonicalize(&top).map_err(|e| format!("cannot resolve {top} ({e})"))
}

/// One stage-0-or-not index entry of `git ls-files -s -z`.
pub struct Entry<'a> {
    pub mode: &'a str,
    pub oid: &'a str,
    pub stage: &'a str,
    pub path: &'a [u8],
}

/// The entries of `git ls-files -s -z` output, in its (path) order.
pub fn entries(out: &[u8]) -> impl Iterator<Item = Result<Entry<'_>, String>> {
    records(out).map(entry)
}

/// One record of `git ls-files -s -z`: `<mode> <oid> <stage>\t<path>`.
pub fn entry(r: &[u8]) -> Result<Entry<'_>, String> {
    let bad = || "git ls-files printed an entry it should not".to_owned();
    let tab = r.iter().position(|&b| b == b'\t').ok_or_else(bad)?;
    let head = std::str::from_utf8(&r[..tab]).map_err(|_| bad())?;
    let mut fields = head.split(' ');
    let (Some(mode), Some(oid), Some(stage)) = (fields.next(), fields.next(), fields.next()) else {
        return Err(bad());
    };
    Ok(Entry {
        mode,
        oid,
        stage,
        path: &r[tab + 1..],
    })
}

/// Each of `oids`' blob bytes, in order, to `each` with its position;
/// one `git cat-file --batch`. A missing object is an error.
pub fn blobs(
    dir: &Path,
    oids: &[&str],
    mut each: impl FnMut(usize, Vec<u8>) -> Result<(), String>,
) -> Result<(), String> {
    let mut child = command(dir, &["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(spawn_failed)?;
    let mut stdin = child.stdin.take().expect("piped");
    let ask: Vec<u8> = oids
        .iter()
        .flat_map(|o| [o.as_bytes(), b"\n"])
        .flatten()
        .copied()
        .collect();
    let read = std::thread::scope(|scope| {
        // The writer runs apart: git answers as it reads, and a full pipe
        // both ways would stop both.
        scope.spawn(move || {
            let _ = stdin.write_all(&ask);
        });
        let mut out = BufReader::with_capacity(1 << 20, child.stdout.take().expect("piped"));
        let mut head = String::new();
        for (i, oid) in oids.iter().enumerate() {
            head.clear();
            out.read_line(&mut head)
                .map_err(|e| format!("cannot read git cat-file ({e})"))?;
            let size = match head.trim_end().split(' ').collect::<Vec<_>>()[..] {
                [got, "blob", size] if got == *oid => size.parse::<usize>().ok(),
                _ => None,
            };
            let Some(size) = size else {
                return Err(format!("git has no blob {oid}"));
            };
            let mut bytes = vec![0; size + 1];
            out.read_exact(&mut bytes)
                .map_err(|e| format!("cannot read git cat-file ({e})"))?;
            bytes.pop();
            each(i, bytes)?;
        }
        Ok(())
    });
    let _ = child.kill();
    let _ = child.wait();
    read
}
