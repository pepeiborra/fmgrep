//! A search through a server from a git checkout: whether the checkout
//! can be compared with the served commit (the VCS commit that the
//! server's index was built from), and the one fallback when it cannot:
//! the server's answer alone, with one warning that says why.

use std::path::{Path, PathBuf};

use super::Server;
use super::serve::ServeStatus;
use crate::vcs;

#[cfg(test)]
mod tests;

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

/// A commit id as people read it: its first 7 hex digits; all of it when
/// those are not 7 characters.
pub fn short(id: &str) -> &str {
    id.get(..id.len().min(7)).unwrap_or(id)
}

/// Whether a served commit of `vcs` and `id` is a git commit by its full
/// id: 40 lowercase hex digits (SHA-1) or 64 (SHA-256). Only such an id
/// goes to git: a ref such as `HEAD` names whatever it points to here.
fn git_commit(vcs: &str, id: &str) -> bool {
    let hex = |c: u8| c.is_ascii_digit() || (b'a'..=b'f').contains(&c);
    vcs == "git" && matches!(id.len(), 40 | 64) && id.bytes().all(hex)
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
    if !git_commit(&commit.vcs, &commit.id) {
        return Plan::Degraded(format!(
            "{server} reports a served commit that is not a git commit id"
        ));
    }
    let id = &commit.id;
    match vcs::git_ok(&top, &["cat-file", "-e", &format!("{id}^{{commit}}")]) {
        Ok(true) => Plan::Checkout {
            top,
            commit: id.clone(),
        },
        Ok(false) => Plan::Degraded(format!(
            "this checkout does not have the served commit {}; run git fetch origin {id}, unless \
             this is another repository",
            short(id)
        )),
        Err(why) => Plan::Degraded(why),
    }
}

/// The one warning of a search whose local changes are ignored.
pub fn degraded(why: &str) {
    super::warn(&format!(
        "local changes are ignored ({why}); these results are the server's alone"
    ));
}
