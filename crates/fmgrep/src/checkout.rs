//! A search through a server from a git checkout: whether the checkout
//! can be compared with the served commit (the VCS commit that the
//! server's index was built from), and the one fallback when it cannot:
//! the server's answer alone, with one warning that says why.

use std::path::{Path, PathBuf};

use crate::diagnostic::Diagnostic;
use crate::remote::Server;
use crate::vcs;
use crate::wire::{MAX_SKIP, ServeStatus};

#[cfg(test)]
mod tests;

/// How a search through a server treats the client's checkout.
pub enum Plan {
    /// As the server answers: the server serves no commit, and the cwd is
    /// in no checkout, or in the tree of a files-layout server.
    Plain,
    /// The server's answer alone, and a warning: local changes are ignored,
    /// for this reason.
    Degraded(String),
    /// The checkout at `top` holds the served commit `commit`.
    Checkout { top: PathBuf, commit: String },
    /// The server's answer alone, with no warning: `--ignore-local-changes`
    /// asks for it, and nothing about the checkout is looked at but its top.
    Ignored,
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
    vcs == "git" && full_id(id)
}

/// Whether `id` is a full git commit id: 40 or 64 lowercase hex digits.
pub fn full_id(id: &str) -> bool {
    let hex = |c: u8| c.is_ascii_digit() || (b'a'..=b'f').contains(&c);
    matches!(id.len(), 40 | 64) && id.bytes().all(hex)
}

/// The commit that a pin keeps for `status`: its served commit, when it is
/// a git commit by its full id; else none.
pub fn pin_of(status: &ServeStatus) -> Option<&str> {
    let commit = status.commit.as_ref()?;
    git_commit(&commit.vcs, &commit.id).then_some(commit.id.as_str())
}

/// What follows "reports no served commit": why the server serves none,
/// when it says. A `store_docs` index gives the reason that its build
/// had; a files layout never has a commit; an index of neither (made by
/// hand) says nothing.
fn why_none(status: &ServeStatus) -> String {
    match (status.store_docs, &status.why_no_commit) {
        (_, Some(why)) => format!(": {why}"),
        (false, None) => ": only a --store-docs index records one".to_owned(),
        (true, None) => String::new(),
    }
}

/// How a search from canonical `cwd` through `server`, of `status`, treats
/// the checkout.
pub fn plan(server: &Server, status: &ServeStatus, cwd: &Path) -> Plan {
    let top = top(cwd);
    let Some(commit) = &status.commit else {
        // A files-layout server reads the files of its tree, so a search
        // from inside that tree gets them as they are now. Stored docs
        // ignore the edits made since the index, in every checkout.
        let served = !status.store_docs
            && std::fs::canonicalize(&status.root).is_ok_and(|t| cwd.starts_with(t));
        return match (served, top) {
            (false, Some(_)) => Plan::Degraded(format!(
                "{server} reports no served commit{}",
                why_none(status)
            )),
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
    // A record of this checkout against the commit was saved after git
    // compared them: the checkout has it.
    let has = match crate::source::recorded(&top, &crate::source::Of::commit(id)) {
        true => Ok(true),
        false => vcs::has_commit(&top, id),
    };
    match has {
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
    converts(top)?;
    // An ignore file that differs can change which files of its directory
    // an index holds: the whole directory is searched here. So is a path
    // that is a directory now, as a tracked file replaced by one: git
    // lists none of the files in it.
    let governed: Vec<String> = (skip.iter())
        .filter_map(|p| crate::walk::ignore_file_dir(p))
        .map(|dir| format!("{dir}/"))
        .collect();
    let dirs: Vec<String> = (skip.iter())
        .filter(|p| !p.ends_with('/') && lstat(top, p).is_some_and(|m| m.is_dir()))
        .map(|p| format!("{p}/"))
        .collect();
    skip.extend(governed);
    skip.extend(dirs);
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
/// the index (line endings, `ident`, an encoding, a filter that cleans or
/// smudges), so that they can differ from their blobs when git status says
/// they do not; or a sparse one. Git says what applies: the value of a
/// setting that wins, and the attributes of the tracked files, from every
/// attributes file.
fn converts(top: &Path) -> Result<(), String> {
    // The value that wins: `true`, `false` (also when unset) or a string.
    let get = |key: &str| {
        let args = [
            "config",
            "--type=bool-or-str",
            "--default=false",
            "--get",
            key,
        ];
        vcs::line(vcs::git(top, &args)?)
    };
    if get("core.sparseCheckout")? == "true" {
        return Err("the checkout is sparse (core.sparseCheckout)".to_owned());
    }
    let converts = |what: String| Err(format!("git converts files ({what})"));
    // `input` converts a file only on its way to the index: a file changed
    // to CRLF line endings can still match its blob for git status.
    if let v @ ("true" | "input") = get("core.autocrlf")?.as_str() {
        return converts(format!("core.autocrlf={v}"));
    }
    let config = vcs::git(
        top,
        &[
            "config",
            "-z",
            "--get-regexp",
            r"^filter\..*\.(clean|smudge|process)$",
        ],
    );
    // `git config --get-regexp` fails when nothing matches.
    let config = config.unwrap_or_default();
    for r in vcs::records(&config) {
        let r = String::from_utf8_lossy(r);
        let key = r.split_once('\n').map_or(&*r, |(key, _)| key);
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
    // The first tracked file with one of these attributes. `core.eol`
    // converts only a file with `text`, and `eol` sets `text`.
    let first = |specs: &[&str]| {
        let mut args = vec!["ls-files", "-z", "--"];
        args.extend(specs);
        let out = vcs::git(top, &args)?;
        let path = vcs::records(&out).next();
        Ok::<_, String>(path.map(|p| String::from_utf8_lossy(p).into_owned()))
    };
    let text = [
        ":(attr:text)",
        ":(attr:text=auto)",
        ":(attr:eol=lf)",
        ":(attr:eol=crlf)",
        ":(attr:ident)",
    ];
    // Any value of `working-tree-encoding`: every file but those where it
    // is unspecified.
    let found = match first(&text)? {
        Some(path) => Some(path),
        None => first(&[":(exclude,attr:!working-tree-encoding)"])?,
    };
    match found {
        Some(path) => converts(format!(
            "the attribute {} applies to {path}",
            attribute(top, &path)?
        )),
        None => Ok(()),
    }
}

/// The first attribute by which git converts the tracked file `path`, as
/// an attributes file writes it: `ident`, `text=auto`.
fn attribute(top: &Path, path: &str) -> Result<String, String> {
    let names = ["text", "eol", "ident", "working-tree-encoding"];
    let mut args = vec!["check-attr", "-z"];
    args.extend(names);
    args.extend(["--", path]);
    let out = vcs::git(top, &args)?;
    let fields: Vec<_> = vcs::records(&out).map(String::from_utf8_lossy).collect();
    // `<path>`, `<attribute>`, `<value>`, for each attribute.
    for field in fields.chunks(3) {
        match field {
            [_, _, value] if value == "unspecified" || value == "unset" => {}
            [_, name, value] if value == "set" => return Ok(name.to_string()),
            [_, name, value] => return Ok(format!("{name}={value}")),
            _ => {}
        }
    }
    Ok(names.join(" or "))
}

/// Refuse a checkout that the ignore files of its ancestors apply to,
/// when one holds a pattern that can reach a file of it. rg and git stop reading `.gitignore` and
/// `info/exclude` at the top of a repository, which `top` is, so those of
/// a repository around it are no rule here; `.ignore` and `.rgignore`
/// are read all the way up.
fn outer_ignores(top: &Path) -> Result<(), String> {
    for dir in top.ancestors().skip(1) {
        for name in [".ignore", ".rgignore"] {
            let path = dir.join(name);
            if vcs::served::reaches(&path, top) {
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
    // The last file passed wins; `info/exclude` wins in the walk.
    let from: Vec<String> = (global.into_iter())
        .chain([top.join(exclude)])
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
pub fn resolve(
    top: &Path,
    skip: &[String],
    commit: &str,
    left_out: &mut Vec<Diagnostic>,
) -> Result<Local, String> {
    let mut rules = crate::walk::rules(top);
    let mut listed = Vec::new();
    // The walk of `/` lists every file that another entry would: it goes
    // alone, so that no file counts twice against the limit.
    let root = ["/".to_owned()];
    let entries = match skip.contains(&root[0]) {
        true => &root[..],
        false => skip,
    };
    // The directories walked; an entry below any of them is in its walk.
    // The entries below a directory need not follow it: `a-b/` sorts
    // between `a` and `a/x.c`.
    let mut walked: Vec<&str> = Vec::new();
    for entry in entries {
        // `d/` is a subtree, and the server leaves out all of it; a path
        // without the slash is that path alone, though it is a directory.
        let (rel, tree) = match entry.strip_suffix('/') {
            Some(rel) => (rel, true),
            None => (entry.as_str(), false),
        };
        let below = |d: &&str| {
            d.is_empty()
                || rel
                    .strip_prefix(*d)
                    .is_some_and(|r| r.is_empty() || r.starts_with('/'))
        };
        if walked.iter().any(below) {
            continue;
        }
        let room = MAX_SKIP - listed.len();
        let found = match try_lstat(top, rel) {
            Ok(found) => found,
            // A path of the skip list that cannot be looked at: in neither part.
            Err(e) => {
                left_out.push(Diagnostic::Skipped {
                    path: top.join(rel),
                    why: format!("cannot stat ({e})"),
                });
                continue;
            }
        };
        match found {
            Some(m) if m.is_dir() && tree => {
                walked.push(rel);
                let found = crate::walk::walk_paths_within(top, &top.join(rel), room);
                let found = found.ok_or_else(|| too_many(skip, &walked, commit))?;
                let unlisted = found.unlisted;
                left_out.extend(
                    unlisted
                        .into_iter()
                        .map(|(dir, why)| Diagnostic::Unlisted { dir, why }),
                );
                listed.extend(found.files);
            }
            Some(m) if m.is_file() && !tree && !rules.matched(rel, false).is_ignore() => {
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
        // A file that is gone or binary is no news, as at the index; one
        // that cannot be read, or is too large, is left out and named.
        let path = top.join(&rel);
        let text = match crate::binary::read_text(&path) {
            Ok(text) => text,
            Err(None) => continue,
            Err(Some(why)) => {
                left_out.push(Diagnostic::Skipped { path, why });
                continue;
            }
        };
        let bom = text.bytes.starts_with(&[0xEF, 0xBB, 0xBF]);
        let len = text.bytes.len() as u64 - if bom { 3 } else { 0 };
        local.bytes += len;
        local.lens.push(len);
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
    try_lstat(top, rel).ok().flatten()
}

/// [`lstat`], with why it could not look at a path that may be there: a
/// directory above it that this process cannot search, say. Absent is
/// `Ok(None)`.
pub fn try_lstat(top: &Path, rel: &str) -> std::io::Result<Option<std::fs::Metadata>> {
    let absent = |e: std::io::Error| match e.kind() {
        std::io::ErrorKind::NotFound => Ok(None),
        _ => Err(e),
    };
    let mut at = top.to_path_buf();
    let mut parts = rel.split('/').filter(|p| !p.is_empty()).peekable();
    while let Some(part) = parts.next() {
        at.push(part);
        let m = match std::fs::symlink_metadata(&at) {
            Ok(m) => m,
            Err(e) => return absent(e),
        };
        if parts.peek().is_none() {
            return Ok(Some(m));
        }
        if !m.is_dir() {
            return Ok(None);
        }
    }
    std::fs::symlink_metadata(top).map(Some).or_else(absent)
}

/// The files a client searches itself, and the shape of a full scan of
/// them.
#[derive(Default)]
pub struct Local {
    pub files: Vec<String>,
    pub bytes: u64,
    /// Each file's bytes, less a leading UTF-8 BOM.
    pub lens: Vec<u64>,
}
