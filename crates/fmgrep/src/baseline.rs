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

use crate::checkout::{Local, changes, resolve};
use crate::diagnostic::Diagnostic;
use crate::filter::PathFilter;
use crate::source::{Of, Remembered, Unsaid, remember, remembered, unsaid};
use crate::vcs;
use crate::wire::MAX_SKIP;

/// The skip list of the checkout at `top` against the served commit
/// `commit`, and the files of it this client searches; or why the
/// checkout cannot be compared. A search that asked git says so only
/// once the client can search the files it lists. Where watchman cannot
/// say what changed, git is asked on every search, and the warning says so
/// once for each reason and served commit, until watchman answers (always
/// under `FMGREP_VERBOSE`): the note that keeps it quiet is returned, to
/// keep once the warning is printed. What the search leaves out, and its
/// warnings and notes, go to `left_out`, in the order found, also when it
/// fails. `filter` is the search's, for the note.
pub fn skip_list(
    top: &Path,
    commit: &str,
    filter: Option<&PathFilter>,
    left_out: &mut Vec<Diagnostic>,
) -> Result<(Vec<String>, Local, Option<Unsaid>), String> {
    let of = Of::commit(commit);
    let (token, why) = match remembered(top, &of) {
        Remembered::Since {
            token,
            baseline,
            extra,
            changed,
        } => match since(top, baseline, &extra, changed) {
            Ok(skip) => {
                if let Err(check) = remember(top, &of, token, skip.clone(), extra) {
                    left_out.push(Diagnostic::Check(check));
                }
                let local = resolve(top, &skip, commit, left_out)?;
                note(
                    top,
                    Some(&of),
                    commit,
                    "watchman since the baseline",
                    &skip,
                    &local,
                    filter,
                    left_out,
                );
                return Ok((skip, local, None));
            }
            Err(why) => (token, why),
        },
        Remembered::Fresh { token, why } => (token, why),
        Remembered::Cannot(why) => {
            let asked = Instant::now();
            let skip = changes(top, commit, left_out)?;
            let took = asked.elapsed();
            let local = resolve(top, &skip, commit, left_out)?;
            // Said once while the reason holds; git is asked every time.
            let said = unsaid(top, &of, &why);
            if said.is_some() || crate::verbose::verbose() {
                left_out.push(Diagnostic::NoWatchman {
                    why,
                    commit: commit.to_owned(),
                    took,
                });
            }
            note(
                top,
                None,
                commit,
                "git, as watchman cannot say",
                &skip,
                &local,
                filter,
                left_out,
            );
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
    let git = git_inputs(top, &listed)?;
    let stamps = stamps(&inputs(top, &git));
    let skip = changes(top, commit, left_out)?;
    let same = settings(top) == listed;
    let took = asked.elapsed();
    if same {
        let extra = json!({"git": git, "stamps": stamps, "env": env()});
        if let Err(check) = remember(top, &of, token, skip.clone(), extra) {
            left_out.push(Diagnostic::Check(check));
        }
    }
    let local = resolve(top, &skip, commit, left_out)?;
    left_out.push(Diagnostic::AskedGit {
        commit: commit.to_owned(),
        why,
        took,
        saved: same,
    });
    let source = "git, a new baseline";
    note(
        top,
        same.then_some(&of),
        commit,
        source,
        &skip,
        &local,
        filter,
        left_out,
    );
    Ok((skip, local, None))
}

/// Under `FMGREP_VERBOSE`, one note: where the skip list `skip` of the
/// checkout at `top` came from (`source`), its size, the files of
/// `local` that the client searches for `filter` (those that its PATHs, globs
/// and cwd keep, as `--stats` counts them), the served commit `commit`,
/// and the record of `of`, when one is saved.
#[allow(clippy::too_many_arguments)]
fn note(
    top: &Path,
    of: Option<&Of>,
    commit: &str,
    source: &'static str,
    skip: &[String],
    local: &Local,
    filter: Option<&PathFilter>,
    left_out: &mut Vec<Diagnostic>,
) {
    if !crate::verbose::verbose() {
        return;
    }
    let record = of.and_then(|of| crate::source::record_path(top, of));
    let (n, bytes) = match filter {
        Some(f) => (f.kept_of(&local.files)).fold((0, 0), |(n, b), i| (n + 1, b + local.lens[i])),
        None => (local.files.len(), local.bytes),
    };
    left_out.push(Diagnostic::LocalChanges {
        source,
        skip: skip.len(),
        files: n,
        bytes,
        commit: commit.to_owned(),
        record,
    });
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
/// reads, from its settings `listed`; as paths.
fn git_inputs(top: &Path, listed: &[u8]) -> Result<Vec<String>, String> {
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
    lines.extend(config_files(top, listed));
    Ok(lines)
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
    let found = [
        vcs::git(top, &["var", "GIT_CONFIG_SYSTEM"]),
        vcs::git(top, &["var", "GIT_ATTR_SYSTEM"]),
        vcs::git(top, &["config", "--path", "core.excludesFile"]),
        vcs::git(top, &["config", "--path", "core.attributesFile"]),
    ];
    for found in found {
        paths.extend(
            found
                .and_then(vcs::line)
                .map(|p| top.join(p).display().to_string()),
        );
    }
    paths.sort_unstable();
    paths.dedup();
    paths
}

/// Every setting that git reads in the checkout at `top`, and the file
/// it comes from, as `git config --list --show-origin -z` prints them.
fn settings(top: &Path) -> Vec<u8> {
    vcs::git(top, &["config", "--list", "--show-origin", "-z"]).unwrap_or_default()
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
