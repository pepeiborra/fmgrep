//! A search through a server from a git checkout: whether the checkout
//! can be compared with the served commit (the VCS commit that the
//! server's index was built from), and the one fallback when it cannot:
//! the server's answer alone, with one warning that says why.

use std::path::{Path, PathBuf};

use super::Server;
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
    /// The checkout holds the served commit.
    Checkout,
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
        Ok(true) => Plan::Checkout,
        Ok(false) => Plan::Degraded(format!(
            "this checkout does not have the served commit {} (run git fetch origin {id}, or \
             this is another repository)",
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
