//! Byte-size budgets read from the environment.

/// Byte size from `var`, or `default` when it is unset. Accepts `<n>` or `<n>`
/// with a `KB`/`MB`/`GB` suffix, 1024-based; junk or zero is a loud error,
/// never a silent default.
pub fn env_bytes(var: &str, default: u64) -> Result<u64, String> {
    let Some(raw) = std::env::var_os(var) else {
        return Ok(default);
    };
    raw.to_str().and_then(parse_bytes).ok_or_else(|| {
        format!("{var}={raw:?} is not a positive byte size (<n>, <n>KB, <n>MB, <n>GB)")
    })
}

fn parse_bytes(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    let (digits, unit) = match raw
        .len()
        .checked_sub(2)
        .and_then(|at| raw.split_at_checked(at))
    {
        Some((head, tail)) => match tail.to_ascii_uppercase().as_str() {
            "KB" => (head, 1024),
            "MB" => (head, 1024 * 1024),
            "GB" => (head, 1024 * 1024 * 1024),
            _ => (raw, 1),
        },
        None => (raw, 1),
    };
    let n: u64 = digits.parse().ok()?;
    n.checked_mul(unit).filter(|&b| b > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_budgets_parse_suffixes_and_refuse_junk() {
        for (raw, want) in [
            ("1", Some(1)),
            (" 20MB ", Some(20 * 1024 * 1024)),
            ("20mb", Some(20 * 1024 * 1024)),
            ("4KB", Some(4096)),
            ("2GB", Some(2 * 1024 * 1024 * 1024)),
            ("0", None),
            ("0MB", None),
            ("", None),
            ("MB", None),
            ("20 MB", None),
            ("20MiB", None),
            ("-1", None),
            ("1e6", None),
            // A multibyte tail must refuse, not split mid-character.
            ("1\u{20ac}", None),
            ("\u{20ac}", None),
        ] {
            assert_eq!(parse_bytes(raw), want, "raw={raw:?}");
        }
    }

    #[test]
    fn an_unset_var_takes_the_default() {
        assert_eq!(env_bytes("FMGREP_NO_SUCH_VAR_XYZ", 7), Ok(7));
    }
}
