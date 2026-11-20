//! What a load or a search leaves out of its answer, as values. The code
//! that finds such a case adds an entry to a list that its caller passes
//! down; only the command line prints them, in the order found.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use crate::checkout::short;
use crate::remote::Server;
use crate::verbose::{Us, plural};

/// `d` as the verbose output writes a time.
fn us(d: &Duration) -> Us {
    Us(d.as_secs_f64() * 1e6)
}

/// A warning about how a load checked what changed since the index: why it
/// checked every file, or why it could not keep what it learned. A server
/// compares the last one it said with the next, and `status` prints them
/// as its own lines, so each has a type of its own.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Check {
    /// Watchman could not say what changed.
    Cannot { why: String },
    /// Every file was checked once, and a token taken.
    Once { why: String, served: bool },
    /// The record or pin could not be written; `what` says which.
    CannotSave { what: String },
    /// More paths may differ than re-checking them beats the stat pass.
    Over { cap: usize },
    /// A server's query after more than `cap` paths changed.
    OverSince { cap: usize },
    /// Ignore files moved or are new since the index.
    IgnoresChanged { n: usize, served: bool },
}

/// The line for the check, without its `warning: ` prefix.
impl fmt::Display for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cannot { why } => {
                write!(
                    f,
                    "watchman cannot say what changed ({why}); checked every file"
                )
            }
            Self::Once { why, served } => {
                let next = match served {
                    true => "the next query asks watchman",
                    false => "later searches ask watchman",
                };
                write!(f, "checked every file once ({why}); {next}")
            }
            Self::CannotSave { what } => {
                write!(f, "cannot save {what}; the next search checks again")
            }
            Self::Over { cap } => write!(
                f,
                "more than {cap} paths may differ from the index; checked every file (re-run index)"
            ),
            Self::OverSince { cap } => write!(
                f,
                "more than {cap} paths changed since the last query; checked every file"
            ),
            Self::IgnoresChanged { n, served } => {
                let (files, dirs) = match n {
                    1 => ("file", "its directory"),
                    _ => ("files", "their directories"),
                };
                let each = if *served { "query" } else { "search" };
                write!(
                    f,
                    "{n} ignore {files} changed since the index; each {each} reads {dirs} again \
                     (re-run index to make searches fast again)"
                )
            }
        }
    }
}

/// A case that the answer lacks something for, and the reason.
#[derive(Clone, Debug, PartialEq)]
pub enum Diagnostic {
    /// A file left out of the answer: it cannot be read, is over 2 GiB, or
    /// cannot be looked at.
    Skipped { path: PathBuf, why: String },
    /// A directory that could not be listed: what is new under it is
    /// missing. `dir` is empty when the error names none.
    Unlisted { dir: PathBuf, why: String },
    /// An untracked directory that git status could not open, so that it
    /// lists none of the files in it. `dir` is as git writes it: `nd/`.
    Unopened { dir: String, why: String },
    /// The planner's account of a search under `FMGREP_VERBOSE=2`: whole
    /// lines, each ending in a newline. It is text because the planner
    /// renders it from values that the query owns.
    Plan { text: String },
    /// An indexed file left out of the answer: it is missing, cannot be
    /// read or statted, or is no longer text.
    Excluded { path: PathBuf, why: String },
    /// Over half of the `files` indexed files are missing under `root`:
    /// one entry for all of them.
    MostlyGone {
        gone: usize,
        files: usize,
        root: PathBuf,
    },
    /// Under `FMGREP_VERBOSE`: `n` indexed files changed since indexing.
    Stale { n: usize },
    /// Under `FMGREP_VERBOSE`: `n` files were added since indexing.
    Added { n: usize },
    /// Under `FMGREP_VERBOSE`: what a read and a locate cost on this
    /// machine, and how long the sample took.
    Costs {
        read_us: f64,
        locate_us: f64,
        sampled_us: u128,
    },
    /// Under `FMGREP_VERBOSE`: what a re-check took from watchman, and its
    /// cost. `served` words it for a server's query.
    Rechecked {
        reported: usize,
        kept: usize,
        served: bool,
        ask: Duration,
        apply: Duration,
    },
    /// A warning of the check of what changed since the index.
    Check(Check),
    /// A server's note once watchman answers again after a warning.
    AnswersAgain,
    /// A search that found a local index, and `note` of why it passed over
    /// a server; `note` is the suffix that `status` prints too.
    LocalIndex { dir: PathBuf, note: String },
    /// A search through a server that cannot compare the checkout with the
    /// served commit: the server's answer alone.
    Degraded { why: String },
    /// A pin that cannot be used; the search asked `server` for its status.
    PinDamaged { why: String, server: Server },
    /// Git was asked what differs from the served commit. `saved` is
    /// whether the record could be saved for watchman to answer later.
    AskedGit {
        commit: String,
        why: String,
        took: Duration,
        saved: bool,
    },
    /// Watchman cannot say what changed, and git was asked.
    NoWatchman {
        why: String,
        commit: String,
        took: Duration,
    },
    /// Under `FMGREP_VERBOSE`: the query went pinned to the served
    /// `commit`, which `pin` keeps, with no status request.
    SentPinned {
        commit: String,
        pin: Option<PathBuf>,
    },
    /// Under `FMGREP_VERBOSE`: the search asked `server` for its status.
    Asked { server: Server, why: String },
    /// Under `FMGREP_VERBOSE`: where the skip list came from, and how much
    /// the client searches by itself.
    LocalChanges {
        source: &'static str,
        skip: usize,
        files: usize,
        bytes: u64,
        commit: String,
        record: Option<PathBuf>,
    },
    /// A pattern `/x/` that found nothing: it matches its slashes too.
    Slashes { pattern: String },
    /// `index` skipped `n` binary files, said in one line without
    /// `FMGREP_VERBOSE`.
    Binaries { n: usize },
    /// The walk of `index` met an error that it could go on past. It is
    /// text because the walk's error holds the path itself and cannot be
    /// kept as a value.
    Walk { why: String },
    /// `index` records the served commit `id`.
    Commit { id: String },
    /// `index` records no served commit.
    NoCommit { why: String },
    /// Under `FMGREP_VERBOSE`: `index` did not ask watchman to watch the
    /// tree, or the crawl goes on.
    NoWatch { why: String },
}

/// The line printed for an entry, with its `warning: ` prefix.
impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Skipped { path, why } => {
                write!(f, "warning: skipping {}: {why}", path.display())
            }
            Self::Unlisted { dir, why } => {
                let at = match dir.as_os_str().is_empty() {
                    true => String::new(),
                    false => format!("{}: ", dir.display()),
                };
                write!(
                    f,
                    "warning: cannot list {at}{why}; files under it may be missing from the answer"
                )
            }
            Self::Unopened { dir, why } => write!(
                f,
                "warning: git status: could not open directory '{dir}': {why}; untracked files \
                 under it are not searched"
            ),
            Self::Plan { text } => f.write_str(text.strip_suffix('\n').unwrap_or(text)),
            Self::Check(check) => write!(f, "warning: {check}"),
            Self::LocalIndex { dir, note } => {
                write!(f, "warning: Using local index {}{note}", dir.display())
            }
            Self::Degraded { why } => write!(
                f,
                "warning: local changes are ignored ({why}); these results are the server's \
                 alone; use --ignore-local-changes to avoid this warning"
            ),
            Self::PinDamaged { why, server } => {
                write!(f, "warning: {why}; asked {server} for its status")
            }
            Self::AskedGit {
                commit,
                why,
                took,
                saved,
            } => {
                let next = match saved {
                    true => "later searches ask watchman",
                    false => {
                        "git's config files changed while git ran; the next search asks \
                              git again"
                    }
                };
                write!(
                    f,
                    "warning: asked git what differs from the served commit {} ({why}; took {}); \
                     {next}",
                    short(commit),
                    us(took)
                )
            }
            Self::NoWatchman { why, commit, took } => write!(
                f,
                "warning: watchman cannot say what changed ({why}); asked git what differs from \
                 the served commit {} (took {})",
                short(commit),
                us(took)
            ),
            Self::SentPinned { commit, pin } => {
                let pin = pin
                    .as_ref()
                    .map_or_else(|| "none".to_owned(), |p| p.display().to_string());
                write!(
                    f,
                    "note: sent the query pinned to the served commit {}, which the pin {pin} \
                     keeps, with no status request",
                    short(commit)
                )
            }
            Self::Asked { server, why } => {
                write!(f, "note: asked {server} for its status ({why})")
            }
            Self::LocalChanges {
                source,
                skip,
                files,
                bytes,
                commit,
                record,
            } => {
                let record = record
                    .as_ref()
                    .map_or_else(|| "none".to_owned(), |p| p.display().to_string());
                write!(
                    f,
                    "note: local changes from {source}: {skip} path{} on the skip list, {files} \
                     file{} ({bytes} bytes) searched here; served commit {}; record {record}",
                    plural(*skip),
                    plural(*files),
                    short(commit)
                )
            }
            Self::Binaries { n } => write!(
                f,
                "skipped {n} non-text file(s) (binary or NUL); set FMGREP_VERBOSE=1 or pass \
                 --verbose to list them"
            ),
            Self::Walk { why } => write!(f, "warning: {why}"),
            Self::Commit { id } => write!(f, "note: the index holds commit {id}"),
            Self::NoCommit { why } => {
                write!(f, "note: the index records no served commit ({why})")
            }
            Self::NoWatch { why } => write!(f, "note: {why}"),
            Self::Slashes { pattern } => write!(
                f,
                "note: the pattern {pattern} matches its slashes too; write {} for what they hold",
                &pattern[1..pattern.len() - 1]
            ),
            Self::AnswersAgain => {
                write!(
                    f,
                    "note: each query re-checks only what watchman reports again"
                )
            }
            Self::Excluded { path, why } => {
                write!(f, "warning: excluding {}: {why}", path.display())
            }
            Self::MostlyGone { gone, files, root } => write!(
                f,
                "warning: excluding {gone} of {files} indexed files, missing under {}",
                root.display()
            ),
            Self::Stale { n } => write!(
                f,
                "note: {n} file(s) changed since indexing are scanned in full by every query; \
                 re-run index"
            ),
            Self::Added { n } => write!(
                f,
                "note: {n} file(s) added since indexing are scanned in full by every query"
            ),
            Self::Costs {
                read_us,
                locate_us,
                sampled_us,
            } => write!(
                f,
                "note: a read costs {read_us:.1} µs per file, a locate {locate_us:.3} µs per \
                 occurrence (sampled in {sampled_us} µs)"
            ),
            Self::Rechecked {
                reported,
                kept,
                served,
                ask,
                apply,
            } => {
                let what = match served {
                    true => format!("watchman reported {reported} path(s) changed, {kept} kept"),
                    false => crate::source::named(*reported, *kept),
                };
                write!(f, "note: {what} (ask {ask:.1?}, apply {apply:.1?})")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Check, Diagnostic};

    /// Each warning of the check reads as the line a search has always
    /// printed for it.
    #[test]
    fn each_warning_of_the_check_reads_as_before() {
        let why = || "no daemon".to_owned();
        let cases = [
            (
                Check::Cannot { why: why() },
                "watchman cannot say what changed (no daemon); checked every file",
            ),
            (
                Check::Once {
                    why: why(),
                    served: false,
                },
                "checked every file once (no daemon); later searches ask watchman",
            ),
            (
                Check::Once {
                    why: why(),
                    served: true,
                },
                "checked every file once (no daemon); the next query asks watchman",
            ),
            (
                Check::CannotSave {
                    what: "the record (it is full)".into(),
                },
                "cannot save the record (it is full); the next search checks again",
            ),
            (
                Check::Over { cap: 64 },
                "more than 64 paths may differ from the index; checked every file (re-run index)",
            ),
            (
                Check::OverSince { cap: 64 },
                "more than 64 paths changed since the last query; checked every file",
            ),
            (
                Check::IgnoresChanged {
                    n: 1,
                    served: false,
                },
                "1 ignore file changed since the index; each search reads its directory again \
                 (re-run index to make searches fast again)",
            ),
            (
                Check::IgnoresChanged { n: 2, served: true },
                "2 ignore files changed since the index; each query reads their directories again \
                 (re-run index to make searches fast again)",
            ),
        ];
        for (check, line) in cases {
            assert_eq!(check.to_string(), line);
        }
    }

    /// The notes and warnings of a load read as the lines a search has
    /// always printed.
    #[test]
    fn the_lines_of_a_load_read_as_before() {
        use std::time::Duration;
        let ms = Duration::from_micros(1500);
        let cases = [
            (
                Diagnostic::Excluded {
                    path: "/src/a.c".into(),
                    why: "is missing".into(),
                },
                "warning: excluding /src/a.c: is missing",
            ),
            (
                Diagnostic::MostlyGone {
                    gone: 3,
                    files: 4,
                    root: "/src".into(),
                },
                "warning: excluding 3 of 4 indexed files, missing under /src",
            ),
            (
                Diagnostic::Stale { n: 2 },
                "note: 2 file(s) changed since indexing are scanned in full by every query; \
                 re-run index",
            ),
            (
                Diagnostic::Added { n: 5 },
                "note: 5 file(s) added since indexing are scanned in full by every query",
            ),
            (
                Diagnostic::Costs {
                    read_us: 12.34,
                    locate_us: 0.7,
                    sampled_us: 90,
                },
                "note: a read costs 12.3 µs per file, a locate 0.700 µs per occurrence \
                 (sampled in 90 µs)",
            ),
            (
                Diagnostic::Rechecked {
                    reported: 3,
                    kept: 2,
                    served: false,
                    ask: ms,
                    apply: ms,
                },
                "note: watchman and the baseline named 3 path(s), 2 kept (ask 1.5ms, apply 1.5ms)",
            ),
            (
                Diagnostic::Rechecked {
                    reported: 3,
                    kept: 2,
                    served: true,
                    ask: ms,
                    apply: ms,
                },
                "note: watchman reported 3 path(s) changed, 2 kept (ask 1.5ms, apply 1.5ms)",
            ),
        ];
        for (d, line) in cases {
            assert_eq!(d.to_string(), line);
        }
    }

    /// The lines of a search through a server read as they always did.
    #[test]
    fn the_lines_of_the_client_read_as_before() {
        use std::time::Duration;
        let server = || crate::remote::Server {
            addr: "127.0.0.1:47700".into(),
            env: true,
        };
        let id = "0123456789abcdef0123456789abcdef01234567";
        let took = Duration::from_millis(150);
        let cases = [
            (
                Diagnostic::LocalIndex {
                    dir: "/src/.fmgrep".into(),
                    note: ", not the server at x (FMGREP_SERVER)".into(),
                },
                "warning: Using local index /src/.fmgrep, not the server at x (FMGREP_SERVER)",
            ),
            (
                Diagnostic::Degraded {
                    why: "git failed".into(),
                },
                "warning: local changes are ignored (git failed); these results are the \
                 server's alone; use --ignore-local-changes to avoid this warning",
            ),
            (
                Diagnostic::PinDamaged {
                    why: "the pin p is malformed".into(),
                    server: server(),
                },
                "warning: the pin p is malformed; asked the server at 127.0.0.1:47700 \
                 (FMGREP_SERVER) for its status",
            ),
            (
                Diagnostic::AskedGit {
                    commit: id.into(),
                    why: "no record".into(),
                    took,
                    saved: true,
                },
                "warning: asked git what differs from the served commit 0123456 (no record; \
                 took 150.0 ms); later searches ask watchman",
            ),
            (
                Diagnostic::AskedGit {
                    commit: id.into(),
                    why: "no record".into(),
                    took,
                    saved: false,
                },
                "warning: asked git what differs from the served commit 0123456 (no record; \
                 took 150.0 ms); git's config files changed while git ran; the next search \
                 asks git again",
            ),
            (
                Diagnostic::NoWatchman {
                    why: "no daemon".into(),
                    commit: id.into(),
                    took,
                },
                "warning: watchman cannot say what changed (no daemon); asked git what differs \
                 from the served commit 0123456 (took 150.0 ms)",
            ),
            (
                Diagnostic::SentPinned {
                    commit: id.into(),
                    pin: Some("/cache/p.pin".into()),
                },
                "note: sent the query pinned to the served commit 0123456, which the pin \
                 /cache/p.pin keeps, with no status request",
            ),
            (
                Diagnostic::SentPinned {
                    commit: id.into(),
                    pin: None,
                },
                "note: sent the query pinned to the served commit 0123456, which the pin none \
                 keeps, with no status request",
            ),
            (
                Diagnostic::Asked {
                    server: server(),
                    why: "no pin of an earlier search".into(),
                },
                "note: asked the server at 127.0.0.1:47700 (FMGREP_SERVER) for its status \
                 (no pin of an earlier search)",
            ),
            (
                Diagnostic::LocalChanges {
                    source: "git, a new baseline",
                    skip: 1,
                    files: 2,
                    bytes: 30,
                    commit: id.into(),
                    record: None,
                },
                "note: local changes from git, a new baseline: 1 path on the skip list, 2 files \
                 (30 bytes) searched here; served commit 0123456; record none",
            ),
            (
                Diagnostic::Slashes {
                    pattern: "/x+/".into(),
                },
                "note: the pattern /x+/ matches its slashes too; write x+ for what they hold",
            ),
        ];
        for (d, line) in cases {
            assert_eq!(d.to_string(), line);
        }
    }

    /// What `index` says reads as it always did; a binary file is skipped
    /// in one line without a prefix.
    #[test]
    fn the_lines_of_an_index_read_as_before() {
        let cases = [
            (
                Diagnostic::Binaries { n: 70 },
                "skipped 70 non-text file(s) (binary or NUL); set FMGREP_VERBOSE=1 or pass \
                 --verbose to list them",
            ),
            (
                Diagnostic::Walk {
                    why: "/src/x: Permission denied".into(),
                },
                "warning: /src/x: Permission denied",
            ),
            (
                Diagnostic::Commit { id: "abc".into() },
                "note: the index holds commit abc",
            ),
            (
                Diagnostic::NoCommit {
                    why: "git is not on PATH".into(),
                },
                "note: the index records no served commit (git is not on PATH)",
            ),
            (
                Diagnostic::NoWatch {
                    why: "no daemon".into(),
                },
                "note: no daemon",
            ),
        ];
        for (d, line) in cases {
            assert_eq!(d.to_string(), line);
        }
    }

    /// A check's warning is a warning line; a server's note is a note.
    #[test]
    fn a_check_warns_and_a_server_notes() {
        let over = Diagnostic::Check(Check::Over { cap: 64 });
        assert_eq!(
            over.to_string(),
            "warning: more than 64 paths may differ from the index; checked every file \
             (re-run index)"
        );
        assert_eq!(
            Diagnostic::AnswersAgain.to_string(),
            "note: each query re-checks only what watchman reports again"
        );
    }

    #[test]
    fn a_skipped_file_reads_as_rg_names_it() {
        let d = Diagnostic::Skipped {
            path: "/src/new.c".into(),
            why: "cannot read (Permission denied (os error 13))".into(),
        };
        assert_eq!(
            d.to_string(),
            "warning: skipping /src/new.c: cannot read (Permission denied (os error 13))"
        );
    }

    #[test]
    fn a_directory_not_listed_says_what_may_be_missing() {
        let d = Diagnostic::Unlisted {
            dir: "/src/newdir".into(),
            why: "Permission denied (os error 13)".into(),
        };
        assert_eq!(
            d.to_string(),
            "warning: cannot list /src/newdir: Permission denied (os error 13); files under \
             it may be missing from the answer"
        );
        let none = Diagnostic::Unlisted {
            dir: "".into(),
            why: "loop detected".into(),
        };
        assert_eq!(
            none.to_string(),
            "warning: cannot list loop detected; files under it may be missing from the answer"
        );
    }

    #[test]
    fn a_directory_git_cannot_open_says_what_is_not_searched() {
        let d = Diagnostic::Unopened {
            dir: "nd/".into(),
            why: "Permission denied".into(),
        };
        assert_eq!(
            d.to_string(),
            "warning: git status: could not open directory 'nd/': Permission denied; \
             untracked files under it are not searched"
        );
    }
}
