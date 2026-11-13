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
        // An ordinary change's third field says whether it is a submodule.
        let path = vcs::status_path(r).ok_or_else(|| format!("git status printed {r:?}"))?;
        if r.starts_with('1') && r.split(' ').nth(2).is_some_and(|s| s.starts_with('S')) {
            return Err(format!("the checkout holds a submodule ({path})"));
        }
        skip.push(path.to_owned());
    }
    match head.as_deref() {
        Some("(initial)") | None => return Err("the checkout has no commit yet".to_owned()),
        Some(h) if h != commit => skip.extend(committed(top, commit, h)?),
        Some(_) => {}
    }
    hidden(top)?;
    converts(top, &skip)?;
    // An ignore file that differs can change which files of its directory
    // an index holds: the whole directory is searched here.
    let governed: Vec<String> = (skip.iter())
        .filter_map(|p| crate::walk::ignore_file_dir(p))
        .map(|dir| format!("{dir}/"))
        .collect();
    skip.extend(governed);
    outer_ignores(top)?;
    // Tracked files that the client's own excludes ignore are left out of
    // its part when it resolves the list, as its index would leave them.
    skip.extend(excluded(top)?);
    skip.sort_unstable();
    skip.dedup();
    if skip.len() > MAX_SKIP {
        return Err(over(skip.len(), commit));
    }
    Ok(skip)
}

/// The paths where commit `head` differs from the served commit `commit`,
/// in either direction: `git diff-tree -r --raw --no-renames`. A
/// submodule on either side refuses.
fn committed(top: &Path, commit: &str, head: &str) -> Result<Vec<String>, String> {
    let out = vcs::git(
        top,
        &[
            "diff-tree",
            "-r",
            "-z",
            "--raw",
            "--no-renames",
            commit,
            head,
        ],
    )?;
    let mut paths = Vec::new();
    let mut records = vcs::records(&out);
    // `:<mode> <mode> <oid> <oid> <status>`, then the path.
    while let (Some(meta), Some(path)) = (records.next(), records.next()) {
        let path = std::str::from_utf8(path).map_err(|_| "a changed path is not UTF-8")?;
        let modes = String::from_utf8_lossy(meta);
        if modes
            .split(' ')
            .take(2)
            .any(|m| m.trim_start_matches(':') == "160000")
        {
            return Err(format!("the checkout holds a submodule ({path})"));
        }
        paths.push(path.to_owned());
    }
    Ok(paths)
}

/// The reason of a checkout that differs in more than [`MAX_SKIP`] files.
pub fn over(n: usize, commit: &str) -> String {
    format!(
        "{n} files differ from the served commit {}; the limit is {MAX_SKIP}",
        short(commit)
    )
}

/// Refuse a checkout whose index hides changes from git status
/// (assume-unchanged or skip-worktree entries), or that holds a submodule.
fn hidden(top: &Path) -> Result<(), String> {
    let listed = vcs::git(top, &["ls-files", "-z", "-s", "-v"])?;
    let mut hidden = 0;
    for r in vcs::records(&listed) {
        // `<tag> <mode> <oid> <stage>\t<path>`: `S` is skip-worktree, a
        // lowercase tag assume-unchanged.
        let (tag, rest) = r.split_at(r.iter().position(|&b| b == b' ').unwrap_or(0));
        if tag == b"S" || tag.first().is_some_and(u8::is_ascii_lowercase) {
            hidden += 1;
        }
        let entry = vcs::entry(rest.get(1..).unwrap_or_default())?;
        if entry.mode == "160000" {
            let path = String::from_utf8_lossy(entry.path);
            return Err(format!("the checkout holds a submodule ({path})"));
        }
    }
    match hidden {
        0 => Ok(()),
        n => Err(format!(
            "git hides changes to {n} file{} (assume-unchanged or skip-worktree)",
            crate::verbose::plural(n)
        )),
    }
}

/// Refuse a checkout whose files git converts between the work tree and
/// the index (line endings, a filter that cleans or smudges), so that they
/// can differ from their blobs when git status says they do not; or a
/// sparse one. `changed` are the paths that differ.
fn converts(top: &Path, changed: &[String]) -> Result<(), String> {
    let config = vcs::git(
        top,
        &[
            "config",
            "-z",
            "--get-regexp",
            r"^(core\.(sparsecheckout|autocrlf|eol|attributesfile)|filter\..*\.(clean|smudge|process))$",
        ],
    );
    // `git config --get-regexp` fails when nothing matches.
    let config = config.unwrap_or_default();
    let converts = |what: String| Err(format!("git converts files ({what})"));
    for r in vcs::records(&config) {
        let r = String::from_utf8_lossy(r);
        let (key, value) = r.split_once('\n').unwrap_or((&r, ""));
        let on = matches!(
            value.to_ascii_lowercase().as_str(),
            "true" | "yes" | "on" | "1"
        );
        match key {
            "core.sparsecheckout" if on => {
                return Err("the checkout is sparse (core.sparseCheckout)".to_owned());
            }
            "core.autocrlf" if on => return converts("core.autocrlf=true".to_owned()),
            "core.eol" if value.eq_ignore_ascii_case("crlf") => {
                return converts("core.eol=crlf".to_owned());
            }
            "core.attributesfile" => return converts("core.attributesFile".to_owned()),
            _ => {}
        }
        if let Some(name) = key
            .strip_prefix("filter.")
            .and_then(|k| k.rsplit_once('.'))
            .map(|(name, _)| name)
        {
            let spec = format!(":(attr:filter={name})");
            let files = vcs::git(top, &["ls-files", "-z", "--", &spec])?;
            let n = vcs::records(&files).count();
            if n > 0 {
                return converts(format!(
                    "the filter {name} applies to {n} file{}",
                    crate::verbose::plural(n)
                ));
            }
        }
    }
    if changed
        .iter()
        .any(|p| p.rsplit('/').next() == Some(".gitattributes"))
    {
        return converts(".gitattributes differs from the served commit".to_owned());
    }
    let info = vcs::line(vcs::git(
        top,
        &["rev-parse", "--git-path", "info/attributes"],
    )?)?;
    let global = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|c| c.join("git/attributes"));
    for path in std::iter::once(top.join(info)).chain(global) {
        if vcs::served::has_pattern(&path) {
            return converts(path.display().to_string());
        }
    }
    Ok(())
}

/// Refuse a checkout that the ignore files of its ancestors apply to,
/// when they hold a pattern.
fn outer_ignores(top: &Path) -> Result<(), String> {
    for dir in top.ancestors().skip(1) {
        for name in [".gitignore", ".ignore", ".rgignore"] {
            let path = dir.join(name);
            if vcs::served::has_pattern(&path) {
                return Err(format!(
                    "an ignore file above the checkout applies ({})",
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

/// The tracked files that rules the served commit does not carry ignore:
/// the patterns of the checkout's `info/exclude` and of the global
/// excludes file that the index's walk reads.
fn excluded(top: &Path) -> Result<Vec<String>, String> {
    let exclude = vcs::line(vcs::git(top, &["rev-parse", "--git-path", "info/exclude"])?)?;
    let global = ignore::gitignore::gitconfig_excludes_path();
    let from: Vec<String> = std::iter::once(top.join(exclude))
        .chain(global)
        .filter(|p| vcs::served::has_pattern(p))
        .map(|p| format!("--exclude-from={}", p.display()))
        .collect();
    if from.is_empty() {
        return Ok(Vec::new());
    }
    let mut args = vec!["ls-files", "-z", "-c", "-i"];
    args.extend(from.iter().map(String::as_str));
    let out = vcs::git(top, &args)?;
    vcs::records(&out)
        .map(|r| {
            std::str::from_utf8(r)
                .map(str::to_owned)
                .map_err(|_| "a changed path is not UTF-8".to_owned())
        })
        .collect()
}

/// The files of the checkout at `top` that the skip list `skip` holds and
/// that a local index of the checkout would hold: what the walk lists
/// (ignore rules, hidden names and symlinks apply), as text. Ascending,
/// with their count and their bytes less leading UTF-8 BOMs. Else, as
/// soon as it lists more than [`MAX_SKIP`] files and before it reads any,
/// why: the reason of the warning.
pub fn resolve(top: &Path, skip: &[String], commit: &str) -> Result<Local, String> {
    let mut rules = crate::walk::rules(top);
    let mut listed = Vec::new();
    // The directories walked; the entries below the last one, which sort
    // right after it, are in its walk.
    let mut walked: Vec<&str> = Vec::new();
    for entry in skip {
        let rel = entry.trim_end_matches('/');
        let below = |d: &&str| {
            d.is_empty()
                || rel
                    .strip_prefix(*d)
                    .is_some_and(|r| r.is_empty() || r.starts_with('/'))
        };
        if walked.last().is_some_and(below) {
            continue;
        }
        let room = MAX_SKIP - listed.len();
        match lstat(top, rel) {
            Some(m) if m.is_dir() => {
                walked.push(rel);
                let found = crate::walk::walk_paths_within(top, &top.join(rel), room);
                listed.extend(found.ok_or_else(|| too_many(skip, &walked, commit))?);
            }
            Some(m) if m.is_file() && !rules.matched(rel, false).is_ignore() => {
                if room == 0 {
                    return Err(too_many(skip, &walked, commit));
                }
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

/// Why the files to search, listed from the skip list `skip`, passed
/// [`MAX_SKIP`] once the client had walked the directories `walked`: an
/// ignore file that differs and governs one of them, else the count.
fn too_many(skip: &[String], walked: &[&str], commit: &str) -> String {
    let governs =
        |p: &&String| crate::walk::ignore_file_dir(p).is_some_and(|d| walked.contains(&d));
    match skip.iter().find(governs) {
        Some(file) => format!(
            "the ignore file {file} differs from the served commit and governs more than \
             {MAX_SKIP} files"
        ),
        None => format!(
            "more than {MAX_SKIP} files differ from the served commit {}",
            short(commit)
        ),
    }
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
