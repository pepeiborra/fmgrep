//! What a load or a search leaves out of its answer, as values. The code
//! that finds such a case adds an entry to a list that its caller passes
//! down; only the command line prints them, in the order found.

use std::fmt;
use std::path::PathBuf;

/// A case that the answer lacks something for, and the reason.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Diagnostic {
    /// A file left out of the answer: it cannot be read, or is over 2 GiB.
    Skipped { path: PathBuf, why: String },
    /// A directory that could not be listed: what is new under it is
    /// missing. `dir` is empty when the error names none.
    Unlisted { dir: PathBuf, why: String },
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
}
