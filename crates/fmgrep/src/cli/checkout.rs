//! A search through a server from a git checkout: whether the checkout
//! can be compared with the served commit (the VCS commit that the
//! server's index was built from), and the one fallback when it cannot:
//! the server's answer alone, with one warning that says why.

use std::path::{Path, PathBuf};

use super::Server;
use super::params::MAX_SKIP;
use super::serve::ServeStatus;
use crate::vcs;

/// How a search through a server treats the client's checkout.
pub enum Plan {
    /// As the server answers: the cwd is in the tree the server reads, or
    /// in no checkout while the server serves no commit.
    Plain,
    /// The server's answer alone, and a warning: local changes are ignored,
    /// for this reason.
    Degraded(String),
    /// The checkout at `top` holds the served commit `commit`.
    Checkout { top: PathBuf, commit: String },
}

/// The top of the work tree that holds `dir`: the nearest directory at or
/// above it with a `.git` (a directory, or the file of a linked worktree).
pub fn top(dir: &Path) -> Option<PathBuf> {
    dir.ancestors()
        .find(|d| d.join(".git").exists())
        .map(Path::to_path_buf)
}

/// A commit id as people read it: its first 7 hex digits.
pub fn short(id: &str) -> &str {
    &id[..id.len().min(7)]
}

/// How a search from canonical `cwd` through `server`, of `status`, treats
/// the checkout.
pub fn plan(server: Server<'_>, status: &ServeStatus, cwd: &Path) -> Plan {
    let top = top(cwd);
    let Some(commit) = &status.commit else {
        let root = Path::new(&status.root);
        let served = super::served_tree(root, status.store_docs)
            .and_then(|t| std::fs::canonicalize(t).ok())
            .is_some_and(|t| cwd.starts_with(t));
        return match (served, top) {
            (false, Some(_)) => Plan::Degraded(format!("{server} reports no served commit")),
            _ => Plan::Plain,
        };
    };
    let Some(top) = top else {
        return Plan::Degraded(format!("{} is in no git checkout", cwd.display()));
    };
    let id = &commit.id;
    match vcs::git_ok(&top, &["cat-file", "-e", &format!("{id}^{{commit}}")]) {
        Ok(true) => Plan::Checkout {
            top,
            commit: id.clone(),
        },
        Ok(false) => Plan::Degraded(format!(
            "this checkout does not have the served commit {} (run git fetch origin {id}, or \
             this is another repository)",
            short(id)
        )),
        Err(why) => Plan::Degraded(why),
    }
}

/// The skip list of the checkout at `top` against the served commit
/// `commit`: every path where the checkout differs from it, as git says
/// (`d/` for an untracked directory), ascending. Else why the checkout
/// cannot be compared: the reason of the warning.
pub fn changes(top: &Path, commit: &str) -> Result<Vec<String>, String> {
    let status = vcs::git(
        top,
        &[
            "status",
            "--porcelain=v2",
            "-z",
            "--branch",
            "--no-renames",
            "--untracked-files=normal",
            "--ignore-submodules=dirty",
        ],
    )?;
    let (mut head, mut skip) = (None, Vec::new());
    for r in vcs::records(&status) {
        let r = std::str::from_utf8(r).map_err(|_| "a changed path is not UTF-8".to_owned())?;
        if let Some(oid) = r.strip_prefix("# branch.oid ") {
            head = Some(oid.to_owned());
            continue;
        }
        if r.starts_with("# ") {
            continue;
        }
        let path = vcs::status_path(r).ok_or_else(|| format!("git status printed {r:?}"))?;
        skip.push(path.to_owned());
    }
    match head.as_deref() {
        Some("(initial)") | None => return Err("the checkout has no commit yet".to_owned()),
        Some(h) if h != commit => {
            return Err(format!("HEAD is not the served commit {}", short(commit)));
        }
        Some(_) => {}
    }
    if let Some(p) = skip
        .iter()
        .find(|p| crate::walk::ignore_file_dir(p).is_some())
    {
        return Err(format!(
            "an ignore file differs from the served commit ({p})"
        ));
    }
    if skip.len() > MAX_SKIP {
        return Err(over(skip.len(), commit));
    }
    skip.sort_unstable();
    skip.dedup();
    Ok(skip)
}

/// The reason of a checkout that differs in more than [`MAX_SKIP`] files.
pub fn over(n: usize, commit: &str) -> String {
    format!(
        "{n} files differ from the served commit {}; the limit is {MAX_SKIP}",
        short(commit)
    )
}

/// The files of the checkout at `top` that the skip list `skip` holds and
/// that a local index of the checkout would hold: what the walk lists
/// (ignore rules, hidden names and symlinks apply), as text. Ascending,
/// with their count and their bytes less leading UTF-8 BOMs.
pub fn resolve(top: &Path, skip: &[String]) -> Result<Local, String> {
    let mut rules = crate::walk::rules(top);
    let mut listed = Vec::new();
    for entry in skip {
        let rel = entry.trim_end_matches('/');
        let path = top.join(rel);
        match lstat(top, rel) {
            Some(m) if m.is_dir() => listed.extend(crate::walk::walk_paths(top, &path, None)),
            Some(m) if m.is_file() && !rules.matched(rel, false).is_ignore() => {
                listed.push(rel.to_owned());
            }
            _ => {}
        }
    }
    listed.sort_unstable();
    listed.dedup();
    let mut local = Local::default();
    for rel in listed {
        let Ok(raw) = std::fs::read(top.join(&rel)) else {
            continue;
        };
        let Ok(text) = crate::binary::text(raw) else {
            continue;
        };
        let bom = text.bytes.starts_with(&[0xEF, 0xBB, 0xBF]);
        local.bytes += text.bytes.len() as u64 - if bom { 3 } else { 0 };
        local.files.push(rel);
    }
    Ok(local)
}

/// The metadata of `rel` below `top`, of the path itself when it is a
/// symlink; none when it is absent or a directory above it is a symlink
/// or not a directory. A file seen through a symlinked directory is
/// outside the tree that the index's walk lists.
pub fn lstat(top: &Path, rel: &str) -> Option<std::fs::Metadata> {
    let mut at = top.to_path_buf();
    let mut parts = rel.split('/').filter(|p| !p.is_empty()).peekable();
    while let Some(part) = parts.next() {
        at.push(part);
        let m = std::fs::symlink_metadata(&at).ok()?;
        if parts.peek().is_none() {
            return Some(m);
        }
        if !m.is_dir() {
            return None;
        }
    }
    std::fs::symlink_metadata(top).ok()
}

/// The files a client searches itself, and the shape of a full scan of
/// them.
#[derive(Default)]
pub struct Local {
    pub files: Vec<String>,
    pub bytes: u64,
}

/// The one warning of a search whose local changes are ignored.
pub fn degraded(why: &str) {
    super::warn(&format!(
        "local changes are ignored ({why}); these results are the server's alone"
    ));
}
