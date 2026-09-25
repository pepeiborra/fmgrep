//! Env-read walk budgets. Callers parse once into a `OnceLock`.

/// Whether `var` is set to exactly `on`.
pub fn flag(var: &str, on: &str) -> bool {
    std::env::var(var).is_ok_and(|v| v == on)
}

/// `var` as a positive count, or `default` when unset, unparsable or zero.
pub fn count(var: &str, default: usize) -> usize {
    std::env::var(var)
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test-only variable names, so no other test's `OnceLock` sees them.
    const FLAG: &str = "FMGREP_BUDGET_TEST_FLAG";
    const COUNT: &str = "FMGREP_BUDGET_TEST_COUNT";

    #[test]
    fn env_knobs_read_exactly_and_fall_back() {
        assert!(!flag(FLAG, "off"));
        assert_eq!(count(COUNT, 7), 7);
        // SAFETY: names unique to this test, and no thread reads them.
        unsafe {
            std::env::set_var(FLAG, "off");
            std::env::set_var(COUNT, "0");
        }
        assert!(flag(FLAG, "off"));
        assert!(!flag(FLAG, "on"));
        // Zero and unparsable values fall back rather than disabling a walk.
        assert_eq!(count(COUNT, 7), 7);
        unsafe {
            std::env::set_var(COUNT, "nine");
        }
        assert_eq!(count(COUNT, 7), 7);
        unsafe {
            std::env::set_var(COUNT, "9");
        }
        assert_eq!(count(COUNT, 7), 9);
    }
}
