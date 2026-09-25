//! Term spelling, shared with the bench harness: a `/.../`-wrapped term is a
//! regex, anything else a literal.

/// The pattern inside a `/.../`-spelled term, or `None` for a literal term.
///
/// `//` yields the empty pattern, which the runners reject loudly rather than
/// treating as match-everywhere.
pub fn regex_pattern(term: &str) -> Option<&str> {
    let b = term.as_bytes();
    if b.len() >= 2 && b[0] == b'/' && b[b.len() - 1] == b'/' {
        Some(&term[1..term.len() - 1])
    } else {
        None
    }
}

/// Invalid-pattern error for native regex search: a 0 count always means
/// "valid pattern, no matches", never "bad pattern".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegexError {
    pub pattern: String,
    pub message: String,
}

impl RegexError {
    pub fn new(pattern: &str, message: impl Into<String>) -> Self {
        Self {
            pattern: pattern.to_owned(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for RegexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "invalid regex pattern {:?}: {}",
            self.pattern, self.message
        )
    }
}

impl std::error::Error for RegexError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slashes_spell_regex_terms() {
        assert_eq!(regex_pattern("/printk/"), Some("printk"));
        assert_eq!(regex_pattern("/a|b/"), Some("a|b"));
        // Inner slashes belong to the pattern.
        assert_eq!(regex_pattern("/base/\\w+/"), Some("base/\\w+"));
        assert_eq!(regex_pattern("//"), Some(""));
        for literal in ["printk", "/", "", "/printk", "printk/", "a/b"] {
            assert_eq!(regex_pattern(literal), None, "term={literal:?}");
        }
    }
}
