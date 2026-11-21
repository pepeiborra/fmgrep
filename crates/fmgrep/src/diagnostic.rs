//! What a load or a search leaves out of its answer, as values. The code
//! that finds such a case adds an entry to a list that its caller passes
//! down; only the command line prints them, in the order found.

use std::fmt;
use std::path::PathBuf;

/// A case that the answer lacks something for, and the reason.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Diagnostic {
    /// A file left out of the answer: it cannot be read, is over 2 GiB, or
    /// cannot be looked at.
    Skipped { path: PathBuf, why: String },
    /// A directory that could not be listed: what is new under it is
    /// missing. `dir` is empty when the error names none.
    Unlisted { dir: PathBuf, why: String },
    /// A warning of git status about a directory it could not open, as git
    /// wrote it, when the client cannot read it: without `warning: `.
    Git(String),
    /// An untracked directory that git status could not open, so that it
    /// lists none of the files in it. `dir` is as git writes it: `nd/`.
    Unopened { dir: String, why: String },
    /// The planner's account of a search under `FMGREP_VERBOSE=2`: whole
    /// lines, each ending in a newline.
    Plan(String),
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
            Self::Git(text) => write!(f, "warning: git status: {text}"),
            Self::Unopened { dir, why } => write!(
                f,
                "warning: git status: could not open directory '{dir}': {why}; untracked files \
                 under it are not searched"
            ),
            Self::Plan(text) => f.write_str(text.strip_suffix('\n').unwrap_or(text)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Diagnostic;

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
