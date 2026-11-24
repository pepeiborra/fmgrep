//! Git, run as a child process: which commit a tree holds, and how a
//! checkout differs from a commit. Every call runs in a given directory
//! with the repository variables of the caller's environment removed, so
//! that git finds the repository from that directory alone; it takes no
//! optional lock, follows no replace ref, never fetches a missing object,
//! never prompts, and reads nothing on stdin.

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
    // A replace ref is the repository's own: a commit read through one
    // holds files that another clone's commit of that id does not.
    // A line of git's stderr that a caller matches is in English, whatever
    // the user's locale: `LC_ALL` outranks `LANG` and `LC_MESSAGES`, and a
    // translated git also reads `LANGUAGE`. The `-z` output is not
    // translated.
    cmd.env("LC_ALL", "C")
        .env_remove("LANGUAGE")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// Why git failed: the first line of its stderr, else its exit status.
fn failed(stderr: &[u8], status: std::process::ExitStatus) -> String {
    said(stderr).unwrap_or_else(|| format!("git failed: {status}"))
}

/// The first line of git's stderr, as why git failed.
fn said(stderr: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(stderr);
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(format!("git failed: {line}"))
}

fn spawn_failed(e: std::io::Error) -> String {
    format!("cannot run git ({e})")
}

/// The stdout of `git -C dir <args>`, or why there is none.
pub fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    Ok(noted(dir, args)?.0)
}

/// [`git`], and what git wrote on stderr although it succeeded: the
/// warnings a caller may need to pass on.
pub fn noted(dir: &Path, args: &[&str]) -> Result<(Vec<u8>, String), String> {
    let out = command(dir, args).output().map_err(spawn_failed)?;
    match out.status.success() {
        true => Ok((
            out.stdout,
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )),
        false => Err(failed(&out.stderr, out.status)),
    }
}

/// Whether the repository at `dir` has the commit `id`, as `git cat-file
/// --batch-check` says; or why git cannot tell, in git's words. Git
/// prints `missing` for an object it lacks, and fails for a repository it
/// cannot read.
pub fn has_commit(dir: &Path, id: &str) -> Result<bool, String> {
    let mut child = command(dir, &["cat-file", "--batch-check=%(objecttype)"])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(spawn_failed)?;
    let mut stdin = child.stdin.take().expect("piped");
    // A git that fails reads nothing; its stderr says why.
    let _ = writeln!(stdin, "{id}^{{commit}}");
    drop(stdin);
    let out = child.wait_with_output().map_err(spawn_failed)?;
    if !out.status.success() {
        return Err(failed(&out.stderr, out.status));
    }
    match String::from_utf8_lossy(&out.stdout).trim_end() {
        "commit" => Ok(true),
        line if line.ends_with(" missing") => Ok(false),
        line => Err(format!("git cat-file printed {line:?}")),
    }
}

/// `out` as one line of UTF-8, its newline dropped.
pub fn line(out: Vec<u8>) -> Result<String, String> {
    let text = String::from_utf8(out).map_err(|_| "git printed a path that is not UTF-8")?;
    Ok(text.trim_end_matches('\n').to_owned())
}

/// Every setting that git reads in `dir`, and the file it comes from, as
/// `git config --list --show-origin -z` prints them: `file:<path>`, then
/// `<key>\n<value>` (a key with no `=` has no newline), for each setting.
pub fn listed(dir: &Path) -> Result<Vec<u8>, String> {
    git(dir, &["config", "--list", "--show-origin", "-z"])
}

/// The settings of `listed`, in order: the key as git prints it (section
/// and name in lowercase), and its value, none for a key with no `=`.
fn settings(listed: &[u8]) -> impl Iterator<Item = (&[u8], Option<&[u8]>)> {
    let mut records = records(listed);
    std::iter::from_fn(move || {
        let (_origin, setting) = (records.next()?, records.next()?);
        Some(match setting.iter().position(|&b| b == b'\n') {
            Some(i) => (&setting[..i], Some(&setting[i + 1..])),
            None => (setting, None),
        })
    })
}

/// The values of `key` (in lowercase) in `listed`, in order.
pub fn values<'a>(listed: &'a [u8], key: &str) -> Vec<Option<&'a [u8]>> {
    let found = settings(listed).filter(|(k, _)| *k == key.as_bytes());
    found.map(|(_, value)| value).collect()
}

/// What `git config --type=bool-or-str --default=false --get <key>` prints
/// for the `values` of the key, when it can be told with certainty; else
/// none, and git has to say. The value that wins is the last.
pub fn bool_or_str(values: &[Option<&[u8]>]) -> Option<String> {
    let mut wins = "false".to_owned();
    for value in values {
        wins = match value {
            None => "true".to_owned(),
            Some(v) => {
                let v = std::str::from_utf8(v).ok().filter(|v| !v.contains('\n'))?;
                match v.to_ascii_lowercase().as_str() {
                    "true" | "yes" | "on" => "true".to_owned(),
                    "" | "false" | "no" | "off" => "false".to_owned(),
                    // A number is a boolean: git tries one, with its
                    // leading white space, sign and units.
                    _ if v
                        .starts_with(|c: char| c.is_ascii_whitespace() || "+-\x0b".contains(c)) =>
                    {
                        return None;
                    }
                    _ if v.starts_with(|c: char| c.is_ascii_digit()) => return None,
                    _ => v.to_owned(),
                }
            }
        };
    }
    Some(wins)
}

/// What `git config --path --get <key>` prints for the `values` of the key,
/// when it can be told with certainty: none if git prints no path, the
/// outer none if git has to say.
pub fn path_of(values: &[Option<&[u8]>]) -> Option<Option<String>> {
    let mut wins = None;
    for value in values {
        let v = std::str::from_utf8((*value)?).ok();
        let v = v.filter(|v| !v.is_empty() && !v.contains('\n'))?;
        wins = Some(match v.strip_prefix("~/") {
            Some(rest) => {
                let home = std::env::var("HOME").ok();
                let home = home.filter(|h| !h.is_empty() && !h.ends_with('/'))?;
                format!("{home}/{rest}")
            }
            // `~user`, `%(prefix)`, `:(optional)`.
            None if v.starts_with(['~', '%', ':']) => return None,
            None => v.to_owned(),
        });
    }
    Some(wins)
}

/// The keys of `listed` that `git config --get-regexp
/// '^filter\..*\.(clean|smudge|process)$'` prints, in order.
pub fn filter_keys(listed: &[u8]) -> Vec<String> {
    let ends = |k: &[u8]| {
        [&b".clean"[..], b".smudge", b".process"]
            .iter()
            .any(|s| k.ends_with(s))
    };
    (settings(listed))
        .map(|(key, _)| key)
        .filter(|k| k.starts_with(b"filter.") && ends(&k[b"filter.".len()..]))
        .map(|k| String::from_utf8_lossy(k).into_owned())
        .collect()
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

/// One file of a commit's tree, as `git ls-tree -r -z` lists it.
pub struct Entry<'a> {
    pub mode: &'a str,
    pub oid: &'a str,
    pub path: &'a [u8],
}

/// The entries of `git ls-tree -r -z` output, in its (path) order.
pub fn entries(out: &[u8]) -> impl Iterator<Item = Result<Entry<'_>, String>> {
    records(out).map(|r| {
        let ([mode, _type, oid], path) = fields(r, "ls-tree")?;
        Ok(Entry { mode, oid, path })
    })
}

/// One record of `git ls-files -s -z`: `<mode> <oid> <stage>\t<path>`.
pub fn entry(r: &[u8]) -> Result<Entry<'_>, String> {
    let ([mode, oid, _stage], path) = fields(r, "ls-files")?;
    Ok(Entry { mode, oid, path })
}

/// The three fields of record `r` before its tab, and its path; `what`
/// is the git command that printed it.
fn fields<'a>(r: &'a [u8], what: &str) -> Result<([&'a str; 3], &'a [u8]), String> {
    let bad = || format!("git {what} printed an entry it should not");
    let tab = r.iter().position(|&b| b == b'\t').ok_or_else(bad)?;
    let head = std::str::from_utf8(&r[..tab]).map_err(|_| bad())?;
    let mut fields = head.split(' ');
    let (Some(a), Some(b), Some(c)) = (fields.next(), fields.next(), fields.next()) else {
        return Err(bad());
    };
    Ok(([a, b, c], &r[tab + 1..]))
}

/// Why reading blobs stopped.
enum Stop {
    /// Git's answer ended or was not a blob: this, unless git said why.
    Git(String),
    /// The caller's own error.
    Each(String),
}

/// Each of `oids`' blob bytes, in order, to `each` with its position;
/// one `git cat-file --batch`. A missing object is an error, and so is an
/// object git cannot read, with the first line git printed. No oid, no
/// git.
pub fn blobs(
    dir: &Path,
    oids: &[&str],
    mut each: impl FnMut(usize, Vec<u8>) -> Result<(), String>,
) -> Result<(), String> {
    if oids.is_empty() {
        return Ok(());
    }
    let mut child = command(dir, &["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(spawn_failed)?;
    let mut stdin = child.stdin.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");
    let out = child.stdout.take().expect("piped");
    let ask: Vec<u8> = oids
        .iter()
        .flat_map(|o| [o.as_bytes(), b"\n"])
        .flatten()
        .copied()
        .collect();
    let (read, stderr) = std::thread::scope(|scope| {
        // The writer and the stderr reader run apart: git answers as it
        // reads, and a full pipe would stop it.
        scope.spawn(move || {
            let _ = stdin.write_all(&ask);
        });
        let errors = scope.spawn(move || {
            let mut text = Vec::new();
            let _ = stderr.read_to_end(&mut text);
            text
        });
        let read = read_blobs(out, oids, &mut each);
        // Its stdout closed, git stops; then its stderr ends.
        let _ = child.kill();
        (read, errors.join().unwrap_or_default())
    });
    let _ = child.wait();
    read.map_err(|stop| match stop {
        Stop::Git(why) => said(&stderr).unwrap_or(why),
        Stop::Each(e) => e,
    })
}

/// [`blobs`]' reading of git's answer `out`.
fn read_blobs(
    out: std::process::ChildStdout,
    oids: &[&str],
    each: &mut impl FnMut(usize, Vec<u8>) -> Result<(), String>,
) -> Result<(), Stop> {
    let mut out = BufReader::with_capacity(1 << 20, out);
    let cannot = |e: std::io::Error| Stop::Git(format!("cannot read git cat-file ({e})"));
    let mut head = String::new();
    for (i, oid) in oids.iter().enumerate() {
        head.clear();
        out.read_line(&mut head).map_err(cannot)?;
        let size = match head.trim_end().split(' ').collect::<Vec<_>>()[..] {
            [got, "blob", size] if got == *oid => size.parse::<usize>().ok(),
            _ => None,
        };
        let Some(size) = size else {
            return Err(Stop::Git(format!("git has no blob {oid}")));
        };
        let mut bytes = vec![0; size + 1];
        out.read_exact(&mut bytes).map_err(cannot)?;
        bytes.pop();
        each(i, bytes).map_err(Stop::Each)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::Path;

    use super::{bool_or_str, command, filter_keys, listed, path_of, values};
    use crate::vcs::{git, line, records};

    /// A git built with gettext says its warnings in the user's language, and
    /// a caller that matches a line of them would then see nothing: the
    /// command fixes the locale, and drops `LANGUAGE`, which a translated
    /// git reads before the locale.
    #[test]
    fn git_runs_in_the_c_locale_whatever_the_users() {
        let cmd = command(Path::new("."), &["status"]);
        let set = |key: &str| cmd.get_envs().find(|(k, _)| *k == OsStr::new(key));
        assert_eq!(
            set("LC_ALL"),
            Some((OsStr::new("LC_ALL"), Some(OsStr::new("C"))))
        );
        assert_eq!(
            set("LANGUAGE"),
            Some((OsStr::new("LANGUAGE"), None)),
            "removed"
        );
    }

    /// Each of the `values` (`;` apart, `@` for the key) as the setting
    /// `key` in a repository, with its list of settings; and an include.
    fn each(key: &str, values: &str, mut check: impl FnMut(&Path, &[u8], &str)) {
        for value in values.split(';') {
            let tmp = tempfile::tempdir().unwrap();
            let top = tmp.path();
            git(top, &["init", "-q"]).unwrap();
            let (section, name) = key.split_once('.').unwrap();
            let set = if value.is_empty() {
                "@".into()
            } else {
                format!("@ = {value}")
            };
            let config = format!(
                "[{section}]\n\t{}\n[include]\n\tpath = inc\n",
                set.replace('@', name)
            );
            let file = top.join(".git/config");
            let text = std::fs::read_to_string(&file).unwrap();
            std::fs::write(&file, text + &config).unwrap();
            std::fs::write(top.join(".git/inc"), "[core]\n\tautocrlf = input\n").unwrap();
            check(top, &listed(top).unwrap(), value);
        }
    }

    /// What the list of settings gives for a key is what git says for it,
    /// or nothing: git is asked when the answer is not certain.
    #[test]
    fn settings_derived_from_the_list_are_what_git_says() {
        let flags = ";yes;TRUE;off;\"\";input;INPUT;08;0x10;\" on\";1;-1;1.5;yes\n\t@ = input;\
            input\n\t@ = false;\"a\\nb\";false\n\t@ = 2;no\n\t@";
        let paths = ";~;~/x;~/;~nobody/x;%(prefix)/x;\"\";:(optional)x;rel/x;/abs/x;a b;\
            ~/x\n\t@ = y/z;y/z\n\t@ = ~/x;~/x\n\t@ = ~nobody/x";
        let mut derived = 0;
        for key in ["core.sparseCheckout", "core.autocrlf"] {
            each(key, flags, |top, list, value| {
                let args = [
                    "config",
                    "--type=bool-or-str",
                    "--default=false",
                    "--get",
                    key,
                ];
                let old = line(git(top, &args).unwrap());
                if let Some(new) = bool_or_str(&values(list, &key.to_ascii_lowercase())) {
                    assert_eq!((key, value, Ok(new)), (key, value, old));
                    derived += 1;
                }
            });
        }
        for key in ["core.excludesFile", "core.attributesFile"] {
            each(key, paths, |top, list, value| {
                let old = git(top, &["config", "--path", key]).and_then(line).ok();
                if let Some(new) = path_of(&values(list, &key.to_ascii_lowercase())) {
                    assert_eq!((key, value, new), (key, value, old));
                    derived += 1;
                }
            });
        }
        assert!(derived >= 30, "{derived}");
    }

    /// The filters of the list are those that `git config --get-regexp`
    /// finds, with their keys, in order.
    #[test]
    fn filters_derived_from_the_list_are_what_git_says() {
        let tmp = tempfile::tempdir().unwrap();
        let top = tmp.path();
        git(top, &["init", "-q"]).unwrap();
        let config = "[Filter \"Up.x\"]\n\tClean = a\n\tsmudge\n[filter \"b\"]\n\tprocess = p\n\
            \tclean = c\n\trequired = true\n[filter]\n\tclean = x\n[filter \"\"]\n\tsmudge = s\n";
        let file = top.join(".git/config");
        std::fs::write(&file, std::fs::read_to_string(&file).unwrap() + config).unwrap();
        let regex = r"^filter\..*\.(clean|smudge|process)$";
        let old = git(top, &["config", "-z", "--get-regexp", regex]).unwrap();
        let old = records(&old).map(|r| {
            String::from_utf8_lossy(r)
                .split('\n')
                .next()
                .unwrap()
                .to_owned()
        });
        assert_eq!(filter_keys(&listed(top).unwrap()), old.collect::<Vec<_>>());
    }
}
