//! The one verbosity switch: level 1 prints per-file walk detail, serve's
//! measured costs and a search's spans, level 2 also every query's plan.

use std::cell::RefCell;
use std::fmt::{self, Write as _};
use std::io::Write as _;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata};

static LEVEL: OnceLock<u64> = OnceLock::new();

/// Apply `--verbose` (level 1) or `FMGREP_VERBOSE`'s count; the first call
/// fixes the level.
pub fn set_verbose(flag: bool) -> Result<(), String> {
    let level = crate::env::count("FMGREP_VERBOSE", 0)?.max(u64::from(flag));
    let _ = LEVEL.set(level);
    Ok(())
}

fn level() -> u64 {
    LEVEL.get().copied().unwrap_or(0)
}

/// Whether per-file detail and serve's measured costs are printed.
pub fn verbose() -> bool {
    level() >= 1
}

/// Whether each query's plan is printed.
pub fn plans() -> bool {
    level() >= 2
}

static START: OnceLock<Instant> = OnceLock::new();

/// Start the clock that spans count from: first thing in a process.
pub fn start_clock() {
    START.get_or_init(Instant::now);
}

fn since_start() -> Duration {
    START.get_or_init(Instant::now).elapsed()
}

/// Under verbose, record each span from now on, for [`print_spans`].
/// Without it no subscriber exists, so a span is one atomic load.
pub fn record_spans() {
    if verbose() {
        let _ = tracing::dispatcher::set_global_default(tracing::Dispatch::new(Recorder));
    }
}

/// The spans recorded, if any, as a tree on stderr in one write.
pub fn print_spans() {
    let total = since_start();
    let all = recorded();
    if !all.spans.is_empty() {
        let _ = std::io::stderr().write_all(tree(&all.spans, total).as_bytes());
    }
}

/// One row a span: its thread, start and duration in ms from
/// [`start_clock`], each child under its parent, indented; then `total`.
fn tree(spans: &[Span], total: Duration) -> String {
    let mut text = format!("note: {:<20} thread  start ms  took ms\n", "span");
    rows(spans, None, 0, &mut text);
    let _ = writeln!(text, "note: {:<27} {:>9.2}", "total", ms(total));
    text
}

fn rows(spans: &[Span], parent: Option<usize>, depth: usize, text: &mut String) {
    for (i, s) in spans.iter().enumerate().filter(|(_, s)| s.parent == parent) {
        let name = format!("{:indent$}{}", "", s.name, indent = 2 * depth);
        let (thread, start) = (s.thread, ms(s.start));
        let took = ms(s.end.saturating_sub(s.start));
        let _ = writeln!(
            text,
            "note: {name:<20} {thread:>6} {start:>9.2} {took:>8.2}"
        );
        rows(spans, Some(i), depth + 1, text);
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

/// A span: its name, its parent's slot, its thread (0 is the first one
/// seen), when it was made and when it was last left.
struct Span {
    name: &'static str,
    parent: Option<usize>,
    thread: usize,
    start: Duration,
    end: Duration,
}

/// The spans in the order they were made, and the threads in the order
/// they made their first span.
struct Recorded {
    spans: Vec<Span>,
    threads: Vec<ThreadId>,
}

static RECORDED: Mutex<Recorded> = Mutex::new(Recorded {
    spans: Vec::new(),
    threads: Vec::new(),
});

fn recorded() -> MutexGuard<'static, Recorded> {
    RECORDED.lock().unwrap_or_else(PoisonError::into_inner)
}

thread_local! {
    /// The spans this thread is in, innermost last.
    static ENTERED: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

/// The subscriber: a span's id is 1 + its slot in [`RECORDED`].
struct Recorder;

fn slot(id: &Id) -> usize {
    id.into_u64() as usize - 1
}

impl tracing::Subscriber for Recorder {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.is_span()
    }

    fn new_span(&self, span: &Attributes<'_>) -> Id {
        let parent = match span.parent() {
            Some(id) => Some(slot(id)),
            None if span.is_contextual() => ENTERED.with_borrow(|e| e.last().copied()),
            None => None,
        };
        let me = std::thread::current().id();
        let mut all = recorded();
        let thread = match all.threads.iter().position(|&t| t == me) {
            Some(t) => t,
            None => {
                all.threads.push(me);
                all.threads.len() - 1
            }
        };
        let now = since_start();
        all.spans.push(Span {
            name: span.metadata().name(),
            parent,
            thread,
            start: now,
            end: now,
        });
        Id::from_u64(all.spans.len() as u64)
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, _: &Event<'_>) {}

    fn enter(&self, id: &Id) {
        ENTERED.with_borrow_mut(|e| e.push(slot(id)));
    }

    /// A span entered on more than one thread ends at its last exit.
    fn exit(&self, id: &Id) {
        ENTERED.with_borrow_mut(|e| {
            if let Some(i) = e.iter().rposition(|&s| s == slot(id)) {
                e.remove(i);
            }
        });
        let now = since_start();
        let span = &mut recorded().spans[slot(id)];
        span.end = span.end.max(now);
    }
}

/// Microseconds, shown in µs, ms or s.
pub struct Us(pub f64);

impl fmt::Display for Us {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            us if us < 10.0 && us.fract() != 0.0 => write!(f, "{us:.2} µs"),
            us if us < 1e3 => write!(f, "{us:.0} µs"),
            us if us < 1e6 => write!(f, "{:.1} ms", us / 1e3),
            us => write!(f, "{:.2} s", us / 1e6),
        }
    }
}

/// `"s"` unless `n` is 1.
pub fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_print_as_a_tree_with_each_child_under_its_parent() {
        let span = |name, parent, thread, start: u64, end: u64| Span {
            name,
            parent,
            thread,
            start: Duration::from_micros(start),
            end: Duration::from_micros(end),
        };
        let spans = [
            span("search", None, 0, 200, 9_000),
            span("load", Some(0), 0, 300, 8_000),
            span("check", Some(1), 1, 400, 7_250),
            span("flush", None, 0, 9_010, 9_020),
            span("query", Some(0), 0, 8_000, 8_990),
        ];
        assert_eq!(
            tree(&spans, Duration::from_micros(9_030)),
            "note: span                 thread  start ms  took ms
note: search                    0      0.20     8.80
note:   load                    0      0.30     7.70
note:     check                 1      0.40     6.85
note:   query                   0      8.00     0.99
note: flush                     0      9.01     0.01
note: total                            9.03
"
        );
    }
}
