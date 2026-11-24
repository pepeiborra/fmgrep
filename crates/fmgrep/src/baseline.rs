//! A checkout's skip list, from git once, then from watchman. The first
//! search takes a token (the clock value that watchman returns for the
//! present moment; a later query can ask what changed since it), asks git
//! what differs from the served commit (the baseline), and saves both in
//! the record: a small file in the user's cache directory that stores the
//! token, the watchman socket and watch, and the paths found to differ. A
//! later search takes a new token, asks watchman what changed since the
//! old one, and adds that to the baseline, with no git run; unless one of
//! the files that decide what git says (HEAD, the index, a config, ignore
//! or attributes file outside the tree) moved, or a variable that chooses
//! those files changed, and then it asks git again.

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{Value, json};

use crate::checkout::{Held, Local, changes, resolve, short};
use crate::diagnostic::Diagnostic;
use crate::filter::PathFilter;
use crate::source::{Of, Remembered, remember, remembered, unsaid};
use crate::vcs;
use crate::wire::MAX_SKIP;

/// What runs once a warning that watchman cannot say prints: it keeps the
/// search after it quiet.
pub type Said = Option<Box<dyn FnOnce()>>;

/// The skip list of the checkout at `top` against the served commit
/// `commit`, and the files of it this client searches; or why the
/// checkout cannot be compared. A search that asked git says so only
/// once the client can search the files it lists. Where watchman cannot
/// say what changed, git is asked on every search, and the warning says so
/// once for each reason and served commit, until watchman answers (always
/// under `FMGREP_VERBOSE`): what keeps it quiet is returned, to run once
/// the warning prints. Its lines go to `left_out`, in the order found.
/// `filter` is the search's, for the note.
pub fn skip_list(
    top: &Path,
    commit: &str,
    filter: Option<&PathFilter>,
    left_out: &mut Vec<Diagnostic>,
) -> Result<(Vec<String>, Local, Said), String> {
    let of = Of::commit(commit);
    let (token, why) = match remembered(top, &of) {
        Remembered::Since {
            token,
            baseline,
            extra,
            changed,
        } => match since(top, baseline, &extra, changed) {
            Ok(skip) => {
                if let Err(line) = remember(top, &of, token, skip.clone(), extra) {
                    left_out.push(Diagnostic::Unsaved(line));
                }
                let local = resolve(top, &skip, commit, left_out)?;
                let source = "watchman since the baseline";
                left_out.extend(note(top, Some(&of), commit, source, &skip, &local, filter));
                return Ok((skip, local, None));
            }
            Err(why) => (token, why),
        },
        Remembered::Fresh { token, why } => (token, why),
        Remembered::Cannot(why) => {
            let asked = Instant::now();
            let skip = changes(top, commit, Held::default(), left_out)?;
            let took = asked.elapsed();
            let local = resolve(top, &skip, commit, left_out)?;
            // Said once while the reason holds; git is asked every time.
            let said = unsaid(top, &of, &why);
            if said.is_some() || crate::verbose::verbose() {
                left_out.push(Diagnostic::Warning(format!(
                    "watchman cannot say what changed ({why}); asked git what differs from the \
                     served commit {} (took {})",
                    short(commit),
                    crate::verbose::Us(took.as_secs_f64() * 1e6)
                )));
            }
            let source = "git, as watchman cannot say";
            left_out.extend(note(top, None, commit, source, &skip, &local, filter));
            return Ok((skip, local, said));
        }
    };
    // The inputs before git reads them: one that moves while git runs
    // makes the next search ask git again. Git lists its settings again
    // once it is done: when they differ, a file may have come to be listed
    // after the first list, and its stamp is in no list; no record is
    // saved.
    let asked = Instant::now();
    let listed = settings(top);
    let (git, exclude) = git_inputs(top, &listed)?;
    let stamps = stamps(&inputs(top, &git));
    // What git said already: its listing, unless that failed or is empty.
    let held = Held {
        listed: Some(&listed[..]).filter(|l| !l.is_empty()),
        exclude: exclude.as_deref(),
    };
    let skip = changes(top, commit, held, left_out)?;
    let same = settings(top) == listed;
    let took = asked.elapsed();
    if same {
        let extra = json!({"git": git, "stamps": stamps, "env": env()});
        if let Err(line) = remember(top, &of, token, skip.clone(), extra) {
            left_out.push(Diagnostic::Unsaved(line));
        }
    }
    let local = resolve(top, &skip, commit, left_out)?;
    let next = match same {
        true => "later searches ask watchman",
        false => "git's config files changed while git ran; the next search asks git again",
    };
    left_out.push(Diagnostic::Warning(format!(
        "asked git what differs from the served commit {} ({why}; took {}); {next}",
        short(commit),
        crate::verbose::Us(took.as_secs_f64() * 1e6)
    )));
    let source = "git, a new baseline";
    let saved = same.then_some(&of);
    left_out.extend(note(top, saved, commit, source, &skip, &local, filter));
    Ok((skip, local, None))
}

/// Under `FMGREP_VERBOSE`, the note: where the skip list `skip` of the
/// checkout at `top` came from (`source`), its size, the files of
/// `local` that the client searches for `filter` (those that its PATHs, globs
/// and cwd keep, as `--stats` counts them), the served commit `commit`,
/// and the record of `of`, when one is saved.
fn note(
    top: &Path,
    of: Option<&Of>,
    commit: &str,
    source: &str,
    skip: &[String],
    local: &Local,
    filter: Option<&PathFilter>,
) -> Option<Diagnostic> {
    if !crate::verbose::verbose() {
        return None;
    }
    let record = of
        .and_then(|of| crate::source::record_path(top, of))
        .map_or_else(|| "none".to_owned(), |p| p.display().to_string());
    let (n, bytes) = match filter {
        Some(f) => (f.kept_of(&local.files)).fold((0, 0), |(n, b), i| (n + 1, b + local.lens[i])),
        None => (local.files.len(), local.bytes),
    };
    let s = skip.len();
    Some(Diagnostic::Note(format!(
        "local changes from {source}: {s} path{} on the skip list, {n} file{} ({} bytes) \
         searched here; served commit {}; record {record}",
        crate::verbose::plural(s),
        crate::verbose::plural(n),
        bytes,
        short(commit)
    )))
}

/// The baseline plus what watchman says `changed`, when the variables
/// and the inputs that `extra` keeps are as they were; else why git must
/// be asked again.
fn since(
    top: &Path,
    mut skip: Vec<String>,
    extra: &Value,
    changed: Vec<String>,
) -> Result<Vec<String>, String> {
    let git: Vec<String> = serde_json::from_value(extra["git"].clone())
        .map_err(|_| "the record is malformed".to_owned())?;
    let was: Vec<(String, Option<(u64, i64)>)> = serde_json::from_value(extra["stamps"].clone())
        .map_err(|_| "the record is malformed".to_owned())?;
    let vars: Vec<(String, Option<String>)> = serde_json::from_value(extra["env"].clone())
        .map_err(|_| "the record is malformed".to_owned())?;
    if let Some((name, _)) = env().iter().find(|v| !vars.contains(v)) {
        return Err(format!("{name} changed"));
    }
    let now = stamps(&inputs(top, &git));
    if let Some((path, _)) = now.iter().find(|s| !was.contains(s)) {
        return Err(format!("{path} changed"));
    }
    if let Some((path, _)) = was.iter().find(|s| !now.contains(s)) {
        return Err(format!("{path} changed"));
    }
    for p in changed {
        if p == ".git" || p.starts_with(".git/") {
            continue;
        }
        if p.rsplit('/').next() == Some(".gitattributes") {
            return Err(format!("{p} changed"));
        }
        if let Some(dir) = crate::walk::ignore_file_dir(&p) {
            skip.push(format!("{dir}/"));
        }
        // Gone, or below a symlink now: it may have been a directory of
        // tracked files.
        if crate::checkout::lstat(top, &p).is_none() {
            skip.push(format!("{p}/"));
        }
        skip.push(p);
    }
    skip.sort_unstable();
    skip.dedup();
    if skip.len() > MAX_SKIP {
        return Err(format!("more than {MAX_SKIP} paths changed"));
    }
    Ok(skip)
}

/// The files in the git directory of the checkout at `top` that decide
/// what git status says: HEAD, the ref it is on, the packed refs, the
/// index, the repository's config and the worktree's, and its
/// `info/exclude` and `info/attributes`; and the config files that git
/// reads, from its settings `listed`; as paths. And the path that git says
/// `info/exclude` has, as it printed it, when it printed one for each.
fn git_inputs(top: &Path, listed: &[u8]) -> Result<(Vec<String>, Option<String>), String> {
    let names = [
        "HEAD",
        "index",
        "config",
        "config.worktree",
        "packed-refs",
        "info/exclude",
        "info/attributes",
    ];
    let mut args = vec!["rev-parse".to_owned()];
    for name in names {
        args.extend(["--git-path".to_owned(), name.to_owned()]);
    }
    args.extend(["--git-common-dir", "--symbolic-full-name", "HEAD"].map(str::to_owned));
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = vcs::git(top, &args)?;
    let out = String::from_utf8(out).map_err(|_| "a git path is not UTF-8".to_owned())?;
    let said: Vec<&str> = out.lines().collect();
    let mut lines: Vec<String> = (said.iter().take(names.len()))
        .map(|l| top.join(l).display().to_string())
        .collect();
    // A path that holds a newline makes more lines: git has to say.
    let whole = said.len() == names.len() + 2;
    // The ref HEAD is on, when it is on one.
    if let Some(name) = said.get(names.len() + 1).filter(|n| n.starts_with("refs/")) {
        // Git keeps a ref in the common git directory, but those of
        // `refs/bisect`, `refs/worktree` and `refs/rewritten`.
        let own = ["bisect", "worktree", "rewritten"].iter().any(|d| {
            name.strip_prefix("refs/")
                .and_then(|n| n.strip_prefix(d))
                .is_some_and(|n| n.is_empty() || n.starts_with('/'))
        });
        let common = said[names.len()];
        let path = match whole && !own && common != "." && !common.ends_with('/') {
            true => format!("{common}/{name}"),
            false => vcs::line(vcs::git(top, &["rev-parse", "--git-path", name])?)?,
        };
        lines.push(top.join(path).display().to_string());
    }
    let exclude = (whole || said.len() == names.len() + 1).then(|| said[5].to_owned());
    lines.extend(config_files(top, listed));
    Ok((lines, exclude))
}

/// The config files that git reads in the checkout at `top`: the system,
/// global and repository files, those that `include.path` and `includeIf`
/// name, and the system file that `git var` names (both also when missing
/// or empty, so that they hold no setting yet), the system attributes
/// file that it names, and the excludes and attributes files that one of
/// them sets; as paths. `listed` is what [`settings`] printed.
fn config_files(top: &Path, listed: &[u8]) -> Vec<String> {
    // `file:<path>`, then `<key>\n<value>`, for each setting.
    let mut records = vcs::records(listed);
    let mut paths = Vec::new();
    while let (Some(origin), Some(setting)) = (records.next(), records.next()) {
        let Some(origin) = std::str::from_utf8(origin)
            .ok()
            .and_then(|o| o.strip_prefix("file:"))
        else {
            continue;
        };
        let origin = top.join(origin);
        paths.extend(included(&origin, setting).map(|p| p.display().to_string()));
        paths.push(origin.display().to_string());
    }
    // `git var` fails before git 2.42, and with `GIT_CONFIG_NOSYSTEM` or
    // `GIT_ATTR_NOSYSTEM`.
    let mut found = vec![
        vcs::git(top, &["var", "GIT_CONFIG_SYSTEM"]).and_then(vcs::line),
        vcs::git(top, &["var", "GIT_ATTR_SYSTEM"]).and_then(vcs::line),
    ];
    // The setting that wins, as `git config --path` prints it; git has to
    // say when the list is empty (it failed) or a value is not plain.
    for key in ["core.excludesFile", "core.attributesFile"] {
        let derived = Some(listed)
            .filter(|l| !l.is_empty())
            .and_then(|l| vcs::path_of(&vcs::values(l, &key.to_ascii_lowercase())));
        found.push(match derived {
            Some(path) => path.ok_or_else(String::new),
            None => vcs::git(top, &["config", "--path", key]).and_then(vcs::line),
        });
    }
    for found in found {
        paths.extend(found.map(|p| top.join(p).display().to_string()));
    }
    paths.sort_unstable();
    paths.dedup();
    paths
}

/// Every setting that git reads in the checkout at `top`, and the file
/// it comes from, as `git config --list --show-origin -z` prints them.
fn settings(top: &Path) -> Vec<u8> {
    vcs::listed(top).unwrap_or_default()
}

/// The file that `setting` (`<key>\n<value>`) of the config file `origin`
/// includes, when it is `include.path` or `includeIf.<condition>.path`,
/// whether the condition holds or not: a `~/` path is in `HOME`, a
/// relative one beside `origin`.
fn included(origin: &Path, setting: &[u8]) -> Option<PathBuf> {
    let (key, value) = std::str::from_utf8(setting).ok()?.split_once('\n')?;
    let include = key == "include.path" || key.starts_with("includeif.") && key.ends_with(".path");
    if !include {
        return None;
    }
    match value.strip_prefix("~/") {
        Some(rest) => Some(PathBuf::from(std::env::var_os("HOME")?).join(rest)),
        None => Some(origin.parent()?.join(value)),
    }
}

/// The variables that choose which config and attributes files git reads,
/// with their values. Git never sees those that point it at another
/// repository or add settings (`GIT_DIR`, `GIT_CONFIG_COUNT`,
/// `GIT_CONFIG_PARAMETERS`): [`vcs::git`] removes them.
const VARS: [&str; 7] = [
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_NOSYSTEM",
    "GIT_CONFIG_GLOBAL",
    "HOME",
    "XDG_CONFIG_HOME",
    "GIT_ATTR_NOSYSTEM",
    "GIT_ATTR_SOURCE",
];

/// Each of [`VARS`] and its value now, when it is set.
fn env() -> Vec<(String, Option<String>)> {
    VARS.iter()
        .map(|&name| {
            let value = std::env::var_os(name).map(|v| v.to_string_lossy().into_owned());
            (name.to_owned(), value)
        })
        .collect()
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
    // Not their `.gitignore`: git and rg stop at the top of a repository.
    for dir in top.ancestors().skip(1) {
        paths.extend([".ignore", ".rgignore"].map(|f| dir.join(f)));
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::git_inputs;
    use crate::vcs::{git, line, listed};

    /// A commit in the repository at `top`.
    fn commit(top: &Path) {
        let ident = ["-c", "user.name=a", "-c", "user.email=a@b"];
        let args = [&ident[..], &["commit", "-q", "--allow-empty", "-m", "x"]].concat();
        git(top, &args).unwrap();
    }

    /// The path of the ref HEAD is on and the one of `info/exclude` are
    /// those that `git rev-parse --git-path` gives, in the main work tree,
    /// a linked one, one with its git directory elsewhere, and for refs
    /// that each work tree has of its own.
    #[test]
    fn the_ref_and_exclude_paths_are_what_git_says() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let (main, linked, apart) = (root.join("main"), root.join("linked"), root.join("apart"));
        std::fs::create_dir(&main).unwrap();
        git(&main, &["init", "-q"]).unwrap();
        commit(&main);
        git(
            &main,
            &["worktree", "add", "-q", "-b", "other", "../linked"],
        )
        .unwrap();
        std::fs::create_dir(&apart).unwrap();
        git(
            &apart,
            &["init", "-q", "--separate-git-dir", "../apart.git"],
        )
        .unwrap();
        commit(&apart);
        for (top, head) in [
            (&main, None),
            (&linked, None),
            (&apart, None),
            (&main, Some("refs/worktree/w")),
            (&main, Some("refs/bisect/b")),
        ] {
            if let Some(head) = head {
                git(top, &["update-ref", head, "HEAD"]).unwrap();
                git(top, &["symbolic-ref", "HEAD", head]).unwrap();
            }
            let (lines, exclude) = git_inputs(top, &listed(top).unwrap()).unwrap();
            let said = |args: &[&str]| line(git(top, args).unwrap()).unwrap();
            let name = said(&["rev-parse", "--symbolic-full-name", "HEAD"]);
            let path = said(&["rev-parse", "--git-path", &name]);
            assert_eq!(
                lines[7],
                top.join(path).display().to_string(),
                "{top:?} {name}"
            );
            let path = said(&["rev-parse", "--git-path", "info/exclude"]);
            assert_eq!(exclude, Some(path), "{top:?}");
        }
    }
}
