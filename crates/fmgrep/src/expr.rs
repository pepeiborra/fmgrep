//! Document-level boolean expressions over query leaves.

use std::fmt;

use serde::{Deserialize, Serialize};

mod parse;

/// `{"term": ..}` | `{"regex": ..}` | `{"and": [..]}` | `{"or": [..]}`,
/// nested freely. A document satisfies a leaf when the leaf matches anywhere
/// in it; `term` is a literal verbatim, `regex` a pattern verbatim.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Expr {
    Term(String),
    Regex(String),
    And(Vec<Expr>),
    Or(Vec<Expr>),
}

pub const TERMS_REFUSAL: &str = "an expression takes no terms";
pub const REGEX_REFUSAL: &str =
    "an expression takes no regex flag; spell regex leaves as /../ or {\"regex\": ..}";

impl Expr {
    /// Refuses an `and` / `or` with no operand.
    pub fn check(&self) -> Result<(), String> {
        match self {
            Expr::Term(_) | Expr::Regex(_) => Ok(()),
            Expr::And(v) | Expr::Or(v) if v.is_empty() => {
                Err("an and/or expression needs at least one operand".to_owned())
            }
            Expr::And(v) | Expr::Or(v) => v.iter().try_for_each(Expr::check),
        }
    }

    /// The leaves in depth-first order: a leaf's position is its index.
    pub fn leaves(&self) -> Vec<&Expr> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect<'a>(&'a self, out: &mut Vec<&'a Expr>) {
        match self {
            Expr::Term(_) | Expr::Regex(_) => out.push(self),
            Expr::And(v) | Expr::Or(v) => v.iter().for_each(|e| e.collect(out)),
        }
    }

    /// The tree's truth given each leaf's, by leaf index. Every leaf is
    /// asked, in order.
    pub fn eval(&self, leaf: &mut impl FnMut(usize) -> bool) -> bool {
        let mut next = 0;
        self.eval_from(&mut next, leaf)
    }

    fn eval_from(&self, next: &mut usize, leaf: &mut impl FnMut(usize) -> bool) -> bool {
        match self {
            Expr::Term(_) | Expr::Regex(_) => {
                *next += 1;
                leaf(*next - 1)
            }
            Expr::And(v) => v.iter().fold(true, |acc, e| e.eval_from(next, leaf) & acc),
            Expr::Or(v) => v.iter().fold(false, |acc, e| e.eval_from(next, leaf) | acc),
        }
    }
}

/// A term printed bare would not read back as itself.
fn needs_quotes(term: &str) -> bool {
    term.is_empty()
        || term == "AND"
        || term == "OR"
        || term.starts_with('/')
        || term
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '\\' | '(' | ')'))
}

/// The node a one-operand `and` / `or` stands for.
fn unwrapped(mut e: &Expr) -> &Expr {
    while let Expr::And(v) | Expr::Or(v) = e {
        match v.as_slice() {
            [only] => e = only,
            _ => break,
        }
    }
    e
}

/// The text `FromStr` reads back; a one-operand node prints as its operand
/// and an unescaped `/` in a regex as `\/`.
impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match unwrapped(self) {
            Expr::Term(t) if needs_quotes(t) => {
                f.write_str("\"")?;
                for c in t.chars() {
                    if matches!(c, '"' | '\\') {
                        f.write_str("\\")?;
                    }
                    write!(f, "{c}")?;
                }
                f.write_str("\"")
            }
            Expr::Term(t) => f.write_str(t),
            Expr::Regex(r) => {
                f.write_str("/")?;
                let mut escaped = false;
                for c in r.chars() {
                    if c == '/' && !escaped {
                        f.write_str("\\")?;
                    }
                    escaped = c == '\\' && !escaped;
                    write!(f, "{c}")?;
                }
                f.write_str("/")
            }
            node @ (Expr::And(v) | Expr::Or(v)) => {
                let and = matches!(node, Expr::And(_));
                for (i, e) in v.iter().enumerate() {
                    if i > 0 {
                        f.write_str(if and { " AND " } else { " OR " })?;
                    }
                    // Nested same-operator nodes keep their parentheses, so
                    // the tree reads back unchanged.
                    match unwrapped(e) {
                        Expr::Or(_) => write!(f, "({e})")?,
                        Expr::And(_) if and => write!(f, "({e})")?,
                        _ => write!(f, "{e}")?,
                    }
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests;
