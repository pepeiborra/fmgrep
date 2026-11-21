//! What a load or a search leaves out of its answer, as values. The code
//! that finds such a case adds an entry to a list that its caller passes
//! down; only the command line prints them, in the order found.

use std::fmt;

/// A case that the answer lacks something for, and the reason. No case
/// is reported yet.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Diagnostic {}

/// The line printed for an entry, with its `warning: ` prefix.
impl fmt::Display for Diagnostic {
    fn fmt(&self, _: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {}
    }
}
