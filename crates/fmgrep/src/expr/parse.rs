//! The text form `Display` prints: `AND` binds tighter than `OR`, `( )`
//! group, words quote as in sh, `/../` is a regex leaf.

use std::str::FromStr;

use super::Expr;

/// `AND` / `OR` nodes, and groups, nest at most this deep; the node bound keeps
/// the tree within serve's JSON recursion limit.
const MAX_DEPTH: usize = 60;

/// A syntax error at byte `at` of the source.
#[derive(Debug, PartialEq, Eq)]
pub struct SyntaxError {
    pub at: usize,
    pub msg: String,
}

impl SyntaxError {
    /// `msg`, the source on one line and a caret under `at`.
    pub fn render(&self, src: &str) -> String {
        let line: String = src
            .chars()
            .map(|c| if c.is_whitespace() { ' ' } else { c })
            .collect();
        let pad = " ".repeat(src[..self.at].chars().count());
        format!("{}\n  {line}\n  {pad}^", self.msg)
    }
}

fn node(
    at: usize,
    mut v: Vec<(Expr, usize)>,
    mk: fn(Vec<Expr>) -> Expr,
) -> Result<(Expr, usize), SyntaxError> {
    if v.len() == 1 {
        return Ok(v.remove(0));
    }
    let depth = 1 + v.iter().map(|&(_, d)| d).max().unwrap_or(0);
    if depth > MAX_DEPTH {
        return fail(at, &format!("nesting deeper than {MAX_DEPTH}"));
    }
    Ok((mk(v.into_iter().map(|(e, _)| e).collect()), depth))
}

fn fail<T>(at: usize, msg: &str) -> Result<T, SyntaxError> {
    Err(SyntaxError {
        at,
        msg: msg.to_owned(),
    })
}

enum Kind {
    Open,
    Close,
    And,
    Or,
    Leaf(Expr),
    End,
}

struct Token {
    at: usize,
    end: usize,
    kind: Kind,
}

struct Parser<'a> {
    src: &'a str,
    pos: usize,
    peeked: Option<Token>,
}

impl Parser<'_> {
    fn rest(&self) -> std::str::CharIndices<'_> {
        self.src[self.pos..].char_indices()
    }

    fn lex(&mut self) -> Result<Token, SyntaxError> {
        let skip = self
            .rest()
            .find(|&(_, c)| !c.is_whitespace())
            .map_or(self.src.len() - self.pos, |(i, _)| i);
        self.pos += skip;
        let at = self.pos;
        let kind = match self.rest().next() {
            None => Kind::End,
            Some((_, '(')) => Kind::Open,
            Some((_, ')')) => Kind::Close,
            Some((_, '/')) => self.regex()?,
            Some(_) => self.word()?,
        };
        if matches!(kind, Kind::Open | Kind::Close) {
            self.pos += 1;
        }
        Ok(Token {
            at,
            end: self.pos,
            kind,
        })
    }

    fn regex(&mut self) -> Result<Kind, SyntaxError> {
        let at = self.pos;
        let mut out = String::new();
        let mut chars = self.src[at + 1..].char_indices();
        let end = loop {
            match chars.next() {
                None => return fail(at, "unterminated /regex/"),
                Some((i, '/')) => break at + 1 + i + 1,
                Some((_, '\\')) => match chars.next() {
                    None => return fail(at, "unterminated /regex/"),
                    Some((_, '/')) => out.push('/'),
                    Some((_, c)) => {
                        out.push('\\');
                        out.push(c);
                    }
                },
                Some((_, c)) => out.push(c),
            }
        };
        if out.is_empty() {
            return fail(at, "empty /regex/");
        }
        if self.src[end..].starts_with(|c: char| !c.is_whitespace() && c != '(' && c != ')') {
            return fail(end, "expected a space or parenthesis after /regex/");
        }
        self.pos = end;
        Ok(Kind::Leaf(Expr::Regex(out)))
    }

    fn word(&mut self) -> Result<Kind, SyntaxError> {
        let mut out = String::new();
        let mut quoted = false;
        let base = self.pos;
        let mut chars = self.src[base..]
            .char_indices()
            .map(|(i, c)| (base + i, c))
            .peekable();
        while let Some(&(i, c)) = chars.peek() {
            if c.is_whitespace() || c == '(' || c == ')' {
                break;
            }
            chars.next();
            match c {
                '\'' => {
                    quoted = true;
                    loop {
                        match chars.next() {
                            None => return fail(i, "unterminated '"),
                            Some((_, '\'')) => break,
                            Some((_, c)) => out.push(c),
                        }
                    }
                }
                '"' => {
                    quoted = true;
                    loop {
                        match chars.next() {
                            None => return fail(i, "unterminated \""),
                            Some((_, '"')) => break,
                            Some((_, '\\')) => match chars.peek() {
                                Some(&(_, e @ ('"' | '\\'))) => {
                                    chars.next();
                                    out.push(e);
                                }
                                _ => out.push('\\'),
                            },
                            Some((_, c)) => out.push(c),
                        }
                    }
                }
                '\\' => {
                    quoted = true;
                    match chars.next() {
                        None => return fail(i, "trailing backslash"),
                        Some((_, c)) => out.push(c),
                    }
                }
                c => out.push(c),
            }
        }
        self.pos = chars.peek().map_or(self.src.len(), |&(i, _)| i);
        Ok(match out.as_str() {
            "AND" if !quoted => Kind::And,
            "OR" if !quoted => Kind::Or,
            _ => Kind::Leaf(Expr::Term(out)),
        })
    }

    fn peek(&mut self) -> Result<&Token, SyntaxError> {
        if self.peeked.is_none() {
            self.peeked = Some(self.lex()?);
        }
        Ok(self.peeked.as_ref().unwrap())
    }

    fn next(&mut self) -> Result<Token, SyntaxError> {
        self.peek()?;
        Ok(self.peeked.take().unwrap())
    }

    /// Parsed trees come with their node depth; `groups` counts enclosing parentheses.
    fn or(&mut self, groups: usize) -> Result<(Expr, usize), SyntaxError> {
        let at = self.peek()?.at;
        let mut v = vec![self.and(groups)?];
        while matches!(self.peek()?.kind, Kind::Or) {
            self.next()?;
            v.push(self.and(groups)?);
        }
        node(at, v, Expr::Or)
    }

    fn and(&mut self, groups: usize) -> Result<(Expr, usize), SyntaxError> {
        let at = self.peek()?.at;
        let mut v = vec![self.primary(groups)?];
        while matches!(self.peek()?.kind, Kind::And) {
            self.next()?;
            v.push(self.primary(groups)?);
        }
        node(at, v, Expr::And)
    }

    fn primary(&mut self, groups: usize) -> Result<(Expr, usize), SyntaxError> {
        let open = self.next()?;
        match open.kind {
            Kind::Leaf(e) => return Ok((e, 0)),
            Kind::Open => {}
            _ => return fail(open.at, "expected a term, /regex/ or '('"),
        }
        if groups == MAX_DEPTH {
            return fail(open.at, &format!("nesting deeper than {MAX_DEPTH}"));
        }
        let first = self.peek()?;
        if matches!(first.kind, Kind::Close) {
            return fail(first.at, "empty ()");
        }
        let e = self.or(groups + 1)?;
        let close = self.next()?;
        match close.kind {
            Kind::Close => Ok(e),
            Kind::End => fail(open.at, "unclosed '('"),
            _ => self.after_operand(close),
        }
    }

    /// The error for `t` where only `AND`, `OR` or `)` may follow.
    fn after_operand<T>(&self, t: Token) -> Result<T, SyntaxError> {
        let raw = &self.src[t.at..t.end];
        let hint = match t.kind {
            _ if raw.eq_ignore_ascii_case("and") || raw.eq_ignore_ascii_case("or") => {
                " (operators are AND / OR in capitals)"
            }
            Kind::Open | Kind::Leaf(Expr::Term(_)) => {
                " (quote a term that holds spaces or parentheses)"
            }
            _ => "",
        };
        fail(t.at, &format!("expected AND, OR or ')'{hint}"))
    }
}

impl FromStr for Expr {
    type Err = SyntaxError;

    fn from_str(src: &str) -> Result<Expr, SyntaxError> {
        let mut p = Parser {
            src,
            pos: 0,
            peeked: None,
        };
        if matches!(p.peek()?.kind, Kind::End) {
            return fail(0, "empty expression");
        }
        let (e, _) = p.or(0)?;
        let t = p.next()?;
        match t.kind {
            Kind::End => Ok(e),
            Kind::Close => fail(t.at, "unmatched ')'"),
            _ => p.after_operand(t),
        }
    }
}

#[cfg(test)]
mod tests;
