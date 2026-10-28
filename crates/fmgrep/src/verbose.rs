//! The one verbosity switch: level 1 prints per-file walk detail and serve's
//! measured costs, level 2 also every query's plan.

use std::fmt;
use std::sync::OnceLock;

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
