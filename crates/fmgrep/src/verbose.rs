//! The one verbosity switch: per-file walk detail is off unless asked for.

use std::sync::OnceLock;

/// Any value but empty or `0` turns the per-file lines on.
const VERBOSE_VAR: &str = "FMGREP_VERBOSE";

static VERBOSE: OnceLock<bool> = OnceLock::new();

fn from_env() -> bool {
    std::env::var(VERBOSE_VAR).is_ok_and(|v| !v.is_empty() && v != "0")
}

/// Apply a CLI `--verbose` flag. The env var switches it on by itself, so a
/// false flag changes nothing; the first call fixes the answer.
pub fn set_verbose(flag: bool) {
    let _ = VERBOSE.set(flag || from_env());
}

/// Whether per-file detail is printed; read once per process.
pub fn verbose() -> bool {
    *VERBOSE.get_or_init(from_env)
}
