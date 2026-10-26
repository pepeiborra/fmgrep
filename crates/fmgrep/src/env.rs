//! `FMGREP_*` variables: unset or empty is the default; any other value must
//! parse, else the error names the variable.

/// A non-negative integer.
pub fn count(var: &str, default: u64) -> Result<u64, String> {
    read(var, default, "a non-negative integer", |s| {
        s.trim().parse().ok()
    })
}

/// A positive byte size: `<n>` with an optional `KB`/`MB`/`GB` suffix,
/// 1024-based.
pub fn bytes(var: &str, default: u64) -> Result<u64, String> {
    let what = "a positive byte size (<n>, <n>KB, <n>MB, <n>GB)";
    read(var, default, what, parse_bytes)
}

fn read(
    var: &str,
    default: u64,
    what: &str,
    parse: fn(&str) -> Option<u64>,
) -> Result<u64, String> {
    match std::env::var_os(var) {
        Some(raw) if !raw.is_empty() => raw
            .to_str()
            .and_then(parse)
            .ok_or_else(|| format!("{var}={raw:?} is not {what}")),
        _ => Ok(default),
    }
}

/// Physical memory, where the platform reports it.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn physical_memory() -> Option<u64> {
    use std::ffi::{c_int, c_long};
    unsafe extern "C" {
        fn sysconf(name: c_int) -> c_long;
    }
    // _SC_PHYS_PAGES and _SC_PAGESIZE.
    #[cfg(target_os = "linux")]
    let (pages, size) = (85, 30);
    #[cfg(target_os = "macos")]
    let (pages, size) = (200, 29);
    // SAFETY: sysconf takes no pointers.
    let (pages, size) = unsafe { (sysconf(pages), sysconf(size)) };
    u64::try_from(pages)
        .ok()?
        .checked_mul(u64::try_from(size).ok()?)
        .filter(|&m| m > 0)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn physical_memory() -> Option<u64> {
    None
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
    fn unset_or_empty_is_the_default_and_junk_is_an_error() {
        const VAR: &str = "FMGREP_ENV_TEST_VAR";
        // SAFETY: no other test reads this name.
        let set = |raw: &str| unsafe { std::env::set_var(VAR, raw) };
        assert_eq!(count(VAR, 7), Ok(7));
        assert_eq!(bytes(VAR, 7), Ok(7));
        set("");
        assert_eq!(count(VAR, 7), Ok(7));
        assert_eq!(bytes(VAR, 7), Ok(7));
        for (raw, want) in [("0", 0), (" 9 ", 9)] {
            set(raw);
            assert_eq!(count(VAR, 7), Ok(want), "{raw:?}");
        }
        for junk in ["nine", "-1", "1.5", "8x"] {
            set(junk);
            assert!(count(VAR, 7).unwrap_err().contains(VAR), "{junk:?}");
        }
        set("0");
        assert!(bytes(VAR, 7).unwrap_err().contains(VAR));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn physical_memory_is_read() {
        assert!(physical_memory().is_some_and(|m| m >= 256 << 20));
    }
}
