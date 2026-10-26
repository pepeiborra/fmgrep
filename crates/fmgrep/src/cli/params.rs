//! `query` params: a search's flags on the wire.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::events::JsonConfig;
use crate::expr::Expr;
use crate::filter::PathFilter;
use crate::manifest::Manifest;
use crate::matching::{MatchMode, QueryOutput};
use crate::output::{Format, OutputConfig};
use crate::query::QueryRequest;

/// Every field but `terms` is optional; `terms` may be omitted only when
/// `expr` is given.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryParams {
    #[serde(default)]
    pub terms: Vec<String>,
    /// Exclusive with non-empty `terms` and with `regex`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expr: Option<Expr>,
    #[serde(default)]
    pub regex: bool,
    #[serde(default)]
    pub ignore_case: bool,
    #[serde(default)]
    pub multiline: bool,
    #[serde(default)]
    pub dotall: bool,
    #[serde(default)]
    pub mode: Mode,
    #[serde(default, skip_serializing_if = "Context::is_zero")]
    pub context: Context,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub line_numbers: bool,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub heading: bool,
    #[serde(default, skip_serializing_if = "Color::is_never")]
    pub color: Color,
    /// `-g`, in argv order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub globs: Vec<String>,
    /// `--iglob`, after `globs`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub iglobs: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub glob_case_insensitive: bool,
    /// Files or directories relative to the indexed root, `""` the root.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
}

/// What a request produces: the report (`counts`) or the
/// local stdout of lines, `-l`, `-c`, `--count-matches`, `-o` and
/// `--json --matches`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Counts,
    Lines,
    Files,
    Count,
    CountMatches,
    Only,
    JsonMatches,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Context {
    pub before: usize,
    pub after: usize,
}

impl Context {
    fn is_zero(&self) -> bool {
        *self == Self::default()
    }
}

/// `auto` is the client's to resolve.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Color {
    #[default]
    Never,
    Always,
}

impl Color {
    fn is_never(&self) -> bool {
        *self == Self::Never
    }
}

fn yes() -> bool {
    true
}

fn is_true(v: &bool) -> bool {
    *v
}

fn is_false(v: &bool) -> bool {
    !*v
}

impl Default for QueryParams {
    fn default() -> Self {
        Self {
            terms: Vec::new(),
            expr: None,
            regex: false,
            ignore_case: false,
            multiline: false,
            dotall: false,
            mode: Mode::Counts,
            context: Context::default(),
            line_numbers: true,
            heading: true,
            color: Color::Never,
            globs: Vec::new(),
            iglobs: Vec::new(),
            glob_case_insensitive: false,
            paths: Vec::new(),
        }
    }
}

impl QueryParams {
    pub fn request<'a>(
        &'a self,
        index_dir: &'a Path,
        manifest: &'a Manifest,
        corpus_root: Option<&'a Path>,
    ) -> Result<QueryRequest<'a>, String> {
        Ok(QueryRequest {
            index_dir,
            manifest,
            corpus_root,
            regex: self.regex,
            mode: MatchMode {
                multiline: self.multiline,
                dotall: self.dotall,
                ignore_case: self.ignore_case,
            },
            terms: &self.terms,
            expr: self.expr.as_ref(),
            output: self.output(),
            filter: self.filter()?,
        })
    }

    pub fn filter(&self) -> Result<Option<PathFilter>, String> {
        PathFilter::new(
            &self.globs,
            &self.iglobs,
            self.glob_case_insensitive,
            &self.paths,
        )
    }

    fn output(&self) -> QueryOutput {
        match self.mode {
            Mode::Counts => QueryOutput::Counts,
            Mode::Lines | Mode::Only | Mode::JsonMatches => QueryOutput::Matches {
                before: self.context.before,
                after: self.context.after,
                line_numbers: self.line_numbers,
            },
            Mode::Files => QueryOutput::FilesWithMatches,
            Mode::Count => QueryOutput::CountLines,
            Mode::CountMatches => QueryOutput::CountMatches,
        }
    }

    /// The printer config of a human search, local or served.
    pub fn output_config(&self) -> OutputConfig {
        OutputConfig {
            format: match self.mode {
                Mode::Only => Format::OnlyMatching,
                Mode::Count => Format::CountLines,
                Mode::CountMatches => Format::CountMatches,
                Mode::Files => Format::FilesWithMatches,
                _ => Format::Lines,
            },
            heading: self.heading,
            line_numbers: self.line_numbers,
            context: !self.context.is_zero(),
            multiline: self.multiline,
            color: self.color == Color::Always,
            banner: self.terms.len() > 1,
        }
    }

    pub fn json_config(&self) -> JsonConfig {
        JsonConfig {
            line_numbers: self.line_numbers,
            multiline: self.multiline,
        }
    }
}

#[cfg(test)]
mod tests;
