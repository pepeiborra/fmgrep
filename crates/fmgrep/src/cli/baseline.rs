//! A checkout's skip list, from git once, then from watchman. The first
//! search takes a token (the clock value that watchman returns for the
//! present moment; a later query can ask what changed since it), asks git
//! what differs from the served commit (the baseline), and saves both in
//! the record: a small file in the user's cache directory that stores the
//! token, the watchman socket and watch, and the paths found to differ. A
//! later search takes a new token, asks watchman what changed since the
//! old one, and adds that to the baseline, with no git run; unless one of
//! the files that decide what git says (HEAD, the index, a config or ignore
//! file outside the tree) moved, and then it asks git again.

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{Value, json};

use super::checkout::{Local, changes, resolve, short};
use super::params::MAX_SKIP;
use crate::source::{Of, Remembered, Token, remember, remembered};
use crate::vcs;

/// The skip list of the checkout at `top` against the served commit
/// `commit`, and the files of it this client searches; or why the
/// checkout cannot be compared. A search that asked git says so only
/// once the client can search the files it lists.
pub fn skip_list(top: &Path, commit: &str) -> Result<(Vec<String>, Local), String> {
    let of = Of::commit(commit);
    let (token, why) = match remembered(top, &of) {
        Remembered::Since {
            token,
            baseline,
            extra,
            changed,
        } => match since(top, baseline, &extra, changed) {
            Ok(Again::Skip(skip)) => {
                return match resolve(top, &skip, commit) {
                    Ok(local) => {
                        remember(top, &of, token, skip.clone(), extra);
                        Ok((skip, local))
                    }
                    Err(why) => Err(unsound(top, &of, token, extra, why)),
                };
            }
            Ok(Again::Unsound(why)) => {
                remember(top, &of, token, Vec::new(), extra);
                return Err(why);
            }
            Err(why) => (token, why),
        },
        Remembered::Fresh { token, why } => (token, why),
        Remembered::Cannot(why) => {
            let asked = Instant::now();
            let skip = changes(top, commit)?;
            let took = asked.elapsed();
            let local = resolve(top, &skip, commit)?;
            super::warn(&format!(
                "watchman cannot say what changed ({why}); asked git what differs from the \
                 served commit {} (took {})",
                short(commit),
                crate::verbose::Us(took.as_secs_f64() * 1e6)
            ));
            return Ok((skip, local));
        }
    };
    // The inputs before git reads them: one that moves while git runs
    // makes the next search ask git again.
    let asked = Instant::now();
    let git = git_inputs(top)?;
    let extra = json!({"git": git, "stamps": stamps(&inputs(top, &git))});
    let skip = match changes(top, commit) {
        Ok(skip) => skip,
        Err(why) => return Err(unsound(top, &of, token, extra, why)),
    };
    let took = asked.elapsed();
    let local = match resolve(top, &skip, commit) {
        Ok(local) => local,
        Err(why) => return Err(unsound(top, &of, token, extra, why)),
    };
    remember(top, &of, token, skip.clone(), extra);
    super::warn(&format!(
        "asked git what differs from the served commit {} ({why}; took {}); later searches ask \
         watchman",
        short(commit),
        crate::verbose::Us(took.as_secs_f64() * 1e6)
    ));
    Ok((skip, local))
}

/// The reason `why` that the checkout at `top` cannot be compared, after
/// saving it in the record of `of` with an empty baseline, the token and
/// the stamps of `extra`: it stays so until an input moves or watchman
/// reports a change. A git that failed, or that cannot run, is not saved:
/// it may not fail next time.
fn unsound(top: &Path, of: &Of, token: Token, mut extra: Value, why: String) -> String {
    if !why.starts_with("git failed") && !why.starts_with("cannot run git") {
        extra["why"] = Value::String(why.clone());
        remember(top, of, token, Vec::new(), extra);
    }
    why
}

/// What a record tells a search whose inputs are as they were.
enum Again {
    /// The skip list: the baseline plus what watchman says changed.
    Skip(Vec<String>),
    /// Why git could not list the checkout's changes soundly, as it said
    /// at the baseline.
    Unsound(String),
}

/// The baseline plus what watchman says `changed`, or the reason the
/// record keeps for an unsound checkout, when the inputs that `extra`
/// stamped are as they were; else why git must be asked again.
fn since(
    top: &Path,
    mut skip: Vec<String>,
    extra: &Value,
    changed: Vec<String>,
) -> Result<Again, String> {
    let git: Vec<String> = serde_json::from_value(extra["git"].clone())
        .map_err(|_| "the record is malformed".to_owned())?;
    let was: Vec<(String, Option<(u64, i64)>)> = serde_json::from_value(extra["stamps"].clone())
        .map_err(|_| "the record is malformed".to_owned())?;
    let now = stamps(&inputs(top, &git));
    if let Some((path, _)) = now.iter().find(|s| !was.contains(s)) {
        return Err(format!("{path} changed"));
    }
    if let Some((path, _)) = was.iter().find(|s| !now.contains(s)) {
        return Err(format!("{path} changed"));
    }
    let git_dir = |p: &String| p == ".git" || p.starts_with(".git/");
    // A reason can stop holding when the work tree changes (files deleted
    // below the limit, a `.gitattributes` edited): any change asks git.
    if let Some(why) = extra["why"].as_str() {
        return match changed.iter().find(|p| !git_dir(p)) {
            Some(p) => Err(format!("{p} changed")),
            None => Ok(Again::Unsound(why.to_owned())),
        };
    }
    let attributes = |p: &String| p.rsplit('/').next() == Some(".gitattributes");
    for p in changed {
        if git_dir(&p) {
            continue;
        }
        if attributes(&p) {
            return Err(format!("{p} changed"));
        }
        if let Some(dir) = crate::walk::ignore_file_dir(&p) {
            skip.push(format!("{dir}/"));
        }
        // Gone, or below a symlink now: it may have been a directory of
        // tracked files.
        if super::checkout::lstat(top, &p).is_none() {
            skip.push(format!("{p}/"));
        }
        skip.push(p);
    }
    skip.sort_unstable();
    skip.dedup();
    if skip.len() > MAX_SKIP {
        return Err(format!("more than {MAX_SKIP} paths changed"));
    }
    Ok(Again::Skip(skip))
}

/// The files in the git directory of the checkout at `top` that decide
/// what git status says: HEAD, the ref it is on, the packed refs, the
/// index, the repository's config, and its `info/exclude` and
/// `info/attributes`; and the config files that git reads; as paths.
fn git_inputs(top: &Path) -> Result<Vec<String>, String> {
    let names = [
        "HEAD",
        "index",
        "config",
        "packed-refs",
        "info/exclude",
        "info/attributes",
    ];
    let mut args = vec!["rev-parse".to_owned()];
    for name in names {
        args.extend(["--git-path".to_owned(), name.to_owned()]);
    }
    args.extend(["--symbolic-full-name".to_owned(), "HEAD".to_owned()]);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = vcs::git(top, &args)?;
    let out = String::from_utf8(out).map_err(|_| "a git path is not UTF-8".to_owned())?;
    let mut lines: Vec<String> = out
        .lines()
        .map(|l| top.join(l).display().to_string())
        .collect();
    // The ref HEAD is on, when it is on one.
    if let Some(name) = out
        .lines()
        .nth(names.len())
        .filter(|n| n.starts_with("refs/"))
    {
        let path = vcs::line(vcs::git(top, &["rev-parse", "--git-path", name])?)?;
        lines.truncate(names.len());
        lines.push(top.join(path).display().to_string());
    } else {
        lines.truncate(names.len());
    }
    lines.extend(config_files(top));
    Ok(lines)
}

/// The config files that git reads in the checkout at `top`: the system,
/// global and repository files, those that `include.path` brings in, and
/// the excludes file that one of them sets; as paths.
fn config_files(top: &Path) -> Vec<String> {
    let listed = vcs::git(top, &["config", "--list", "--show-origin", "-z"]).unwrap_or_default();
    // `file:<path>`, then `<key>\n<value>`, for each setting.
    let origins = vcs::records(&listed).step_by(2);
    let mut paths: Vec<String> = origins
        .filter_map(|o| std::str::from_utf8(o).ok()?.strip_prefix("file:"))
        .map(|p| top.join(p).display().to_string())
        .collect();
    let excludes = vcs::git(top, &["config", "--path", "core.excludesFile"]);
    paths.extend(
        excludes
            .and_then(vcs::line)
            .map(|p| top.join(p).display().to_string()),
    );
    paths.sort_unstable();
    paths.dedup();
    paths
}

/// Every input: those of the git directory, the global git config, ignore
/// and attributes files, and the ignore files above the checkout.
fn inputs(top: &Path, git: &[String]) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = git.iter().map(PathBuf::from).collect();
    let var = |name| std::env::var_os(name).map(PathBuf::from);
    let home = var("HOME");
    paths.extend(var("GIT_CONFIG_GLOBAL").or_else(|| home.as_ref().map(|h| h.join(".gitconfig"))));
    let xdg = var("XDG_CONFIG_HOME")
        .filter(|p| p.is_absolute())
        .or_else(|| home.as_ref().map(|h| h.join(".config")));
    if let Some(xdg) = xdg {
        paths.extend(["git/config", "git/ignore", "git/attributes"].map(|f| xdg.join(f)));
    }
    paths.extend(ignore::gitignore::gitconfig_excludes_path());
    for dir in top.ancestors().skip(1) {
        paths.extend([".gitignore", ".ignore", ".rgignore"].map(|f| dir.join(f)));
    }
    paths
}

/// Each of `paths`, with its size and mtime when it exists.
fn stamps(paths: &[PathBuf]) -> Vec<(String, Option<(u64, i64)>)> {
    paths
        .iter()
        .map(|p| {
            let stamp = crate::walk::stamp(p).map(|s| (s.len, s.mtime_ns));
            (p.display().to_string(), stamp)
        })
        .collect()
}
