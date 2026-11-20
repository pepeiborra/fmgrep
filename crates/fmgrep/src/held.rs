//! The lines on stderr of a search through a server that sent its query
//! pinned to the commit a pin keeps, held until the server answers: the
//! first frame or the result. If the server refuses the query, the lines
//! spoke of a commit it no longer serves, and are dropped.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};

/// What runs once a held line is printed.
pub type After = Box<dyn FnOnce()>;

/// Held text, ending in a newline, the flag of a line printed once per
/// process, and what runs once it is printed.
type Line = (String, Option<&'static AtomicBool>, Option<After>);

thread_local! {
    static HELD: RefCell<Option<Vec<Line>>> = const { RefCell::new(None) };
}

/// Hold this thread's lines from now on.
pub fn hold() {
    HELD.set(Some(Vec::new()));
}

/// Whether this thread holds its lines.
pub fn holding() -> bool {
    HELD.with_borrow(Option::is_some)
}

/// Print the held lines, and hold no more.
pub fn release() {
    if let Some(lines) = HELD.take() {
        for (text, _, after) in lines {
            eprint!("{text}");
            if let Some(after) = after {
                after();
            }
        }
    }
}

/// Drop the held lines, and hold no more. A line printed once per process
/// may print again.
pub fn discard() {
    for (_, seen, _) in HELD.take().unwrap_or_default() {
        if let Some(seen) = seen {
            seen.store(false, Ordering::Relaxed);
        }
    }
}

/// `line` on stderr, or held.
pub fn line(line: &str) {
    once(None, line);
}

/// `line` on stderr, or held, and then `after`: a held line that is
/// dropped runs nothing.
pub fn then(line: &str, after: Option<After>) {
    HELD.with_borrow_mut(|held| match held {
        Some(lines) => lines.push((format!("{line}\n"), None, after)),
        None => {
            eprintln!("{line}");
            if let Some(after) = after {
                after();
            }
        }
    });
}

/// `line` on stderr, or held with `seen`, the flag that kept it to once
/// per process.
pub fn once(seen: Option<&'static AtomicBool>, line: &str) {
    HELD.with_borrow_mut(|held| match held {
        Some(lines) => lines.push((format!("{line}\n"), seen, None)),
        None => eprintln!("{line}"),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A dropped line printed once per process may print again; a printed
    /// one may not.
    #[test]
    fn a_dropped_line_may_print_again() {
        static SEEN: AtomicBool = AtomicBool::new(true);
        hold();
        assert!(holding());
        line("warning: held");
        once(Some(&SEEN), "warning: once");
        assert_eq!(HELD.with_borrow(|h| h.as_ref().map(Vec::len)), Some(2));
        discard();
        assert!(!holding() && !SEEN.load(Ordering::Relaxed));
        SEEN.store(true, Ordering::Relaxed);
        hold();
        once(Some(&SEEN), "warning: once");
        release();
        assert!(!holding() && SEEN.load(Ordering::Relaxed));
    }

    /// What follows a held line runs once the line is printed, and never
    /// when it is dropped.
    #[test]
    fn what_follows_a_line_runs_when_it_prints() {
        use std::cell::Cell;
        use std::rc::Rc;
        let ran = Rc::new(Cell::new(0));
        let count = || -> Option<After> {
            let ran = Rc::clone(&ran);
            Some(Box::new(move || ran.set(ran.get() + 1)))
        };
        hold();
        then("warning: dropped", count());
        discard();
        assert_eq!(ran.get(), 0);
        hold();
        then("warning: printed", count());
        assert_eq!(ran.get(), 0);
        release();
        assert_eq!(ran.get(), 1);
        then("warning: at once", count());
        assert_eq!(ran.get(), 2);
    }
}
