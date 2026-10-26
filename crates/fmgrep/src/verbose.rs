//! The one verbosity switch: per-file walk detail is off unless asked for.

use std::sync::OnceLock;

static VERBOSE: OnceLock<bool> = OnceLock::new();

/// Apply `--verbose` or a positive `FMGREP_VERBOSE`; the first call fixes the
/// answer.
pub fn set_verbose(flag: bool) -> Result<(), String> {
    let on = crate::env::count("FMGREP_VERBOSE", 0)? > 0 || flag;
    let _ = VERBOSE.set(on);
    Ok(())
}

/// Whether per-file detail is printed.
pub fn verbose() -> bool {
    VERBOSE.get().copied().unwrap_or(false)
}
