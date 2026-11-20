//! What a load or a search leaves out of its answer, as values. The code
//! that finds such a case adds an entry to a list that its caller passes
//! down; only the command line prints them, in the order found.

use std::fmt;
use std::path::PathBuf;

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
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
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
