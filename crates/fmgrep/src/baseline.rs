//! A checkout's skip list, from git once, then from watchman. The first
//! search takes a token (the clock value that watchman returns for the
//! present moment; a later query can ask what changed since it), asks git
//! what differs from the served commit (the baseline), and saves both in
//! the record: a small file in the user's cache directory that stores the
//! token, the watchman socket and watch, and the paths found to differ. A
//! later search takes a new token, asks watchman what changed since the
//! old one, and adds that to the baseline, with no git run; unless one of
//! the files that decide what git says (a config, ignore or attributes
//! file outside the tree) moved, or a variable that chooses those files
//! changed, and then it asks git again. A commit, `git status` or `git
//! add` moves HEAD, a ref or the index without any of that: see [`since`].

use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::checkout::{Held, Local, changes, converts, resolve, short, tracked};
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
    // Searches started together find no record and would all ask git: one
    // at a time asks (see [`Lock`]), and the others read what it saved.
    let record = crate::source::record_path(top, &of);
    let seen = record.as_deref().map(stamp);
    let mut lock: Option<Lock> = None;
    let mut found = remembered(top, &of);
    let (token, why) = loop {
        let (token, why) = match found {
            Remembered::Since {
                token,
                baseline,
                extra,
                changed,
            } => match since(top, baseline, &extra, changed) {
                Ok((skip, extra)) => {
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
                let (skip, _) = changes(top, commit, Held::default(), left_out)?;
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
        // Git is asked next. After the wait for the lock, or when another
        // search saved a record meanwhile, the record is read again.
        if lock.is_some() {
            break (token, why);
        }
        let held = Lock::take(record.as_deref(), left_out);
        let again = held.waited || record.as_deref().map(stamp) != seen;
        lock = Some(held);
        if !again {
            break (token, why);
        }
        found = remembered(top, &of);
    };
    // The inputs before git reads them: one that moves while git runs
    // makes the next search ask git again. Git lists its settings again
    // once it is done: when they differ, a file may have come to be listed
    // after the first list, and its stamp is in no list; no record is
    // saved.
    let asked = Instant::now();
    let listed = settings(top);
    let (git, exclude, index, moves) = git_inputs(top, &listed)?;
    let stamps = stamps(&inputs(top, &git));
    // What git said already: its listing, unless that failed or is empty.
    let held = Held {
        listed: Some(&listed[..]).filter(|l| !l.is_empty()),
        exclude: exclude.as_deref(),
    };
    let (skip, entries) = changes(top, commit, held, left_out)?;
    let same = settings(top) == listed;
    let took = asked.elapsed();
    if same {
        let extra = json!({"git": git, "stamps": stamps, "env": env(), "index": index,
            "moves": moves, "entries": entries, "settings": digest(&listed)});
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

/// How long a search waits for another to finish asking git.
const LOCK_WAIT: Duration = Duration::from_secs(2);

/// The lock of a checkout's record: held while a search asks git, so that
/// searches started together ask once. It is an advisory lock (`flock`) on
/// the file of the record with `.lock` added, which stays: the kernel lets
/// go of the lock when the search ends, crashed or not. A search that
/// cannot take it, because the file cannot be opened or locked, or because
/// another holds it past [`LOCK_WAIT`], asks git as it did without it.
struct Lock {
    /// Whether the search had to wait for another to let go.
    waited: bool,
    /// The lock file, held until it drops.
    _file: Option<File>,
}

impl Lock {
    /// The lock of the record at `record`, waiting at most [`LOCK_WAIT`].
    /// Under `FMGREP_VERBOSE`, a note to `left_out` says why it is not held.
    fn take(record: Option<&Path>, left_out: &mut Vec<Diagnostic>) -> Self {
        let mut held = Self {
            waited: false,
            _file: None,
        };
        let Some(record) = record else {
            return held;
        };
        let mut path = record.as_os_str().to_owned();
        path.push(".lock");
        let path = PathBuf::from(path);
        let mut say = |line: String| {
            if crate::verbose::verbose() {
                left_out.push(Diagnostic::Note(line));
            }
        };
        let file = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| File::create(&path));
        let file = match file {
            Ok(file) => file,
            Err(e) => {
                say(format!(
                    "cannot lock {} ({e}); asked git without it",
                    path.display()
                ));
                return held;
            }
        };
        let started = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => {
                    held._file = Some(file);
                    return held;
                }
                Err(TryLockError::WouldBlock) if started.elapsed() < LOCK_WAIT => {
                    held.waited = true;
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(TryLockError::WouldBlock) => {
                    let secs = LOCK_WAIT.as_secs();
                    say(format!(
                        "another search held {} for {secs} s; asked git as well",
                        path.display()
                    ));
                    return held;
                }
                Err(TryLockError::Error(e)) => {
                    say(format!(
                        "cannot lock {} ({e}); asked git without it",
                        path.display()
                    ));
                    return held;
                }
            }
        }
    }
}

/// The size and mtime of `path`, when it exists.
fn stamp(path: &Path) -> Option<(u64, std::time::SystemTime)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
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
        Some(f) => (f.kept_of(&local.files)).fold((0, 0), |(n, b), (i, k)| {
            (n + k, b + local.lens[i] * k as u64)
        }),
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

/// The baseline plus what watchman says `changed`, and the record's `extra`
/// as it is now, when the variables and the inputs that `extra` keeps are
/// as they were, but for HEAD, the ref it is on, the packed refs and the
/// index; else why git must be asked again.
///
/// Why those four may move with no ask. The skip list must hold every path
/// where the work tree W differs from the served commit C, and each path
/// that the client's excludes hide of C. C never changes. W changes only
/// by writes to files, and watchman reports each. So if the baseline B0
/// held the paths where W0 differed from C when git was asked, then
/// `W1 != C` is inside `B0 + (W1 != W0)`, which is inside `B0 + what watchman
/// says`. The paths of C that the excludes hide are in B0 too: through
/// `excluded` when tracked, through status or diff-tree when not; they
/// depend on C and the exclude files, which stay stamped. HEAD, a ref or
/// the packed refs moving (a commit, `reset --soft`, a checkout, a
/// rebase) changes none of this: a checkout or rebase writes the files it
/// changes, and watchman says so. Staging (`add`, `add -f`, `rm --cached`)
/// writes no file. So B0 stays a superset, and only grows.
///
/// What the index still decides is whether a new ask would refuse: a
/// hidden entry (skip-worktree, assume-unchanged), a submodule, and a file
/// that a conversion applies to and that is tracked now. [`tracked`] reads
/// the entries again (one git run) when the index moved: the same digest
/// as in the record means the same answer; another is checked by
/// [`converts`]; a hidden entry or a submodule asks git, which refuses. With
/// `GIT_ATTR_SOURCE` set attributes follow HEAD, so a move of it asks too.
///
/// What HEAD still decides is git's settings: `includeIf "onbranch:..."`
/// reads a config file on one branch and not on another, with no file
/// changed, and that file may turn a conversion on or name another
/// excludes or attributes file, which no stamp covers. So when HEAD itself
/// moved (a checkout, a rebase; a commit moves the ref only), git lists
/// its settings again (one git run) and they must be those of the record,
/// else git is asked. A reftable repository never rewrites HEAD, so there
/// a move of a list of ref tables counts as one of HEAD, a commit too.
fn since(
    top: &Path,
    mut skip: Vec<String>,
    extra: &Value,
    changed: Vec<String>,
) -> Result<(Vec<String>, Value), String> {
    let malformed = |_| "the record is malformed".to_owned();
    let git: Vec<String> = serde_json::from_value(extra["git"].clone()).map_err(malformed)?;
    let was: Vec<(String, Option<(u64, i64)>)> =
        serde_json::from_value(extra["stamps"].clone()).map_err(malformed)?;
    let vars: Vec<(String, Option<String>)> =
        serde_json::from_value(extra["env"].clone()).map_err(malformed)?;
    let index: String = serde_json::from_value(extra["index"].clone()).map_err(malformed)?;
    let moves: Vec<String> = serde_json::from_value(extra["moves"].clone()).map_err(malformed)?;
    let entries: String = serde_json::from_value(extra["entries"].clone()).map_err(malformed)?;
    if let Some((name, _)) = env().iter().find(|v| !vars.contains(v)) {
        return Err(format!("{name} changed"));
    }
    let now = stamps(&inputs(top, &git));
    let moved: Vec<&String> = (now.iter().filter(|s| !was.contains(s)))
        .chain(was.iter().filter(|s| !now.contains(s)))
        .map(|(path, _)| path)
        .collect();
    if let Some(path) = moved.iter().find(|p| **p != &index && !moves.contains(p)) {
        return Err(format!("{path} changed"));
    }
    let mut extra = extra.clone();
    let mut listed = None;
    if let Some(path) = moved.iter().find(|p| moves.contains(p))
        && std::env::var_os("GIT_ATTR_SOURCE").is_some()
    {
        return Err(format!("{path} changed"));
    }
    if moved.contains(&&index) {
        let asked = || format!("{index} changed");
        let digest = tracked(top).map_err(|_| asked())?;
        if digest != entries {
            let held = listed.insert(settings(top));
            converts(top, Some(&held[..]).filter(|l| !l.is_empty())).map_err(|_| asked())?;
            extra["entries"] = Value::String(digest);
        }
    }
    if (moved.iter()).any(|p| moves.first() == Some(*p) || p.ends_with(TABLES)) {
        let listed = listed.unwrap_or_else(|| settings(top));
        if listed.is_empty() || extra["settings"].as_str() != Some(&digest(&listed)) {
            return Err("git's settings changed with the branch".to_owned());
        }
    }
    if !moved.is_empty() {
        extra["stamps"] = json!(now);
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
    Ok((skip, extra))
}

/// The list of ref tables of a reftable repository, in a git directory.
const TABLES: &str = "reftable/tables.list";

/// What [`git_inputs`] finds: the paths; the path that git says
/// `info/exclude` has, as it printed it, when it printed one for each; the
/// path of the index; and those of HEAD, the packed refs, the ref HEAD is
/// on and the ref tables, which may move without a new ask (see [`since`]).
type Inputs = (Vec<String>, Option<String>, String, Vec<String>);

/// The files in the git directory of the checkout at `top` that decide
/// what git status says: HEAD, the ref it is on, the packed refs, the
/// index, the repository's config and the worktree's, and its
/// `info/exclude` and `info/attributes`; and the config files that git
/// reads, from its settings `listed`; as paths. And the rest of [`Inputs`].
fn git_inputs(top: &Path, listed: &[u8]) -> Result<Inputs, String> {
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
    let index = lines[1].clone();
    let mut moves = vec![lines[0].clone(), lines[4].clone()];
    moves.extend(lines.get(names.len()).cloned());
    // A reftable repository never rewrites HEAD: a checkout writes the
    // list of tables beside it (a linked worktree has its own), and a
    // commit the list of the common git directory.
    let beside = Path::new(&lines[0]).with_file_name(TABLES);
    let shared = whole.then(|| top.join(said[names.len()]).join(TABLES));
    for list in [Some(beside), shared].into_iter().flatten() {
        let list = list.display().to_string();
        if !moves.contains(&list) && Path::new(&list).exists() {
            lines.push(list.clone());
            moves.push(list);
        }
    }
    lines.extend(config_files(top, listed));
    Ok((lines, exclude, index, moves))
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

/// The digest of git's settings `listed`, as [`settings`] prints them.
fn digest(listed: &[u8]) -> String {
    let hash = crate::source::fnv_with(crate::source::FNV_BASIS, listed);
    format!("{hash:016x}")
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
            let (lines, exclude, ..) = git_inputs(top, &listed(top).unwrap()).unwrap();
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
