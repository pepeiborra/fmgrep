//! The error of a pattern that does not compile.

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
