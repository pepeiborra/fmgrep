//! rg's `-g`, `--iglob` and PATH arguments.

use std::borrow::Cow;

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

use crate::fetch::Paths;

/// A PATH argument.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchPath {
    /// Relative to the indexed root, `""` the root.
    pub path: String,
    /// As typed, `""` for the cwd of a search without PATH: its files print as
    /// `name` joined with the rest.
    pub name: String,
}

pub struct PathFilter {
    set: GlobSet,
    rules: Vec<Rule>,
    includes: usize,
    paths: Vec<SearchPath>,
    cwd: String,
}

struct Rule {
    exclude: bool,
    dir_only: bool,
}

impl PathFilter {
    /// None when nothing filters. `globs` then `iglobs` are gitignore lines
    /// whose `!` excludes. Under `paths` they see a file as rg run in `cwd`
    /// prints it; without, its root-relative path.
    pub fn new(
        globs: &[String],
        iglobs: &[String],
        case_insensitive: bool,
        paths: &[SearchPath],
        cwd: &str,
    ) -> Result<Option<Self>, String> {
        let lines = globs
            .iter()
            .map(|g| (g, case_insensitive))
            .chain(iglobs.iter().map(|g| (g, true)));
        let mut set = GlobSetBuilder::new();
        let mut rules = Vec::new();
        for (glob, fold) in lines {
            let Some((actual, rule)) = parse(glob) else {
                continue;
            };
            let built = GlobBuilder::new(&actual)
                .literal_separator(true)
                .backslash_escape(true)
                .case_insensitive(fold)
                .build()
                .map_err(|e| format!("error: invalid glob '{glob}': {}", e.kind()))?;
            set.add(built);
            rules.push(rule);
        }
        if rules.is_empty() && paths.is_empty() {
            return Ok(None);
        }
        let set = set.build().map_err(|e| format!("error: {e}"))?;
        Ok(Some(Self {
            set,
            includes: rules.iter().filter(|r| !r.exclude).count(),
            rules,
            paths: paths.to_vec(),
            cwd: cwd.to_owned(),
        }))
    }

    /// Whether a glob filters, not only PATHs.
    pub fn globs(&self) -> bool {
        !self.rules.is_empty()
    }

    pub fn retain<T>(&self, paths: &Paths<'_>, items: &mut Vec<T>, id: impl Fn(&T) -> usize) {
        let mut keep = self.keeper();
        items.retain(|t| paths.path(id(t)).is_some_and(&mut keep));
    }

    /// Whether a search with no PATH has no file of `paths` but the `gone`
    /// ones to search.
    pub fn excludes_all(&self, paths: &Paths<'_>, gone: impl Fn(usize) -> bool) -> bool {
        let mut keep = self.keeper();
        self.paths.iter().all(|p| p.name.is_empty())
            && (0..)
                .map_while(|id| paths.path(id).map(|p| (id, p)))
                .all(|(id, p)| gone(id) || !keep(p))
    }

    /// Whether a search covers the file at a path, remembering the last
    /// parent directory's verdict: paths in order share it.
    fn keeper(&self) -> impl FnMut(&str) -> bool + '_ {
        static ROOT: [SearchPath; 1] = [SearchPath {
            path: String::new(),
            name: String::new(),
        }];
        let roots = if self.paths.is_empty() {
            &ROOT[..]
        } else {
            &self.paths
        };
        let mut last: Option<(usize, String, bool)> = None;
        let mut hits = Vec::new();
        let mut buf = String::new();
        move |path: &str| {
            let parent = path.rsplit_once('/').map_or("", |(dir, _)| dir);
            roots.iter().enumerate().any(|(i, root)| {
                let Some(rest) = below(&root.path, path) else {
                    return path == root.path.trim_end_matches('/');
                };
                let open = match &last {
                    Some((j, dir, open)) if *j == i && dir == parent => *open,
                    _ => {
                        let open = self.open(root, parent, &mut hits, &mut buf);
                        last = Some((i, parent.to_owned(), open));
                        open
                    }
                };
                open && self.admits(root, rest, &mut hits, &mut buf)
            })
        }
    }

    /// `path` as rg prints it: under the first of the PATHs that keeps it.
    pub fn printed<'p>(&self, path: &'p str) -> Cow<'p, str> {
        let (mut hits, mut buf) = (Vec::new(), String::new());
        let mut holding = self.paths.iter().filter_map(|p| {
            let rest = below(&p.path, path);
            (rest.is_some() || path == p.path.trim_end_matches('/')).then_some((p, rest))
        });
        let mut next = holding.next();
        while let Some((root, rest)) = next {
            next = holding.next();
            let kept = |rest| {
                let parent = path.rsplit_once('/').map_or("", |(dir, _)| dir);
                self.open(root, parent, &mut hits, &mut buf)
                    && self.admits(root, rest, &mut hits, &mut buf)
            };
            if next.is_none() || rest.is_none_or(kept) {
                let mut out = String::new();
                match rest {
                    Some(rest) => join(&root.name, rest, &mut out),
                    None => out.push_str(&root.name),
                }
                return Cow::Owned(out);
            }
        }
        Cow::Borrowed(path)
    }

    /// Whether no directory from below `root` down to `dir` is excluded.
    fn open(&self, root: &SearchPath, dir: &str, hits: &mut Vec<usize>, buf: &mut String) -> bool {
        let Some(rest) = below(&root.path, dir) else {
            return true;
        };
        let ends = rest.match_indices('/').map(|(i, _)| i);
        !ends
            .chain([rest.len()])
            .any(|end| self.decide(root, &rest[..end], true, hits, buf) == Some(false))
    }

    /// What the globs see of `rest` below `root`: rg's printed path less one
    /// `./`, then less the cwd.
    fn seen<'a>(&self, root: &SearchPath, rest: &'a str, buf: &'a mut String) -> &'a str {
        if root.name.is_empty() {
            return rest;
        }
        buf.clear();
        join(&root.name, rest, buf);
        let s = buf.strip_prefix("./").unwrap_or(buf);
        match s.strip_prefix(self.cwd.as_str()) {
            Some(r) if !self.cwd.is_empty() && s.contains('/') => r.strip_prefix('/').unwrap_or(r),
            _ => s,
        }
    }

    fn admits(
        &self,
        root: &SearchPath,
        rest: &str,
        hits: &mut Vec<usize>,
        buf: &mut String,
    ) -> bool {
        self.decide(root, rest, false, hits, buf)
            .unwrap_or(self.includes == 0)
    }

    /// The last glob matching `rest` below `root`: true to include.
    fn decide(
        &self,
        root: &SearchPath,
        rest: &str,
        dir: bool,
        hits: &mut Vec<usize>,
        buf: &mut String,
    ) -> Option<bool> {
        if self.rules.is_empty() {
            return None;
        }
        self.set.matches_into(self.seen(root, rest, buf), hits);
        hits.iter()
            .rev()
            .map(|&i| &self.rules[i])
            .find(|r| dir || !r.dir_only)
            .map(|r| !r.exclude)
    }
}

/// `path` below the directory `root`, `""` the root.
fn below<'a>(root: &str, path: &'a str) -> Option<&'a str> {
    let root = root.trim_end_matches('/');
    if root.is_empty() {
        return Some(path).filter(|p| !p.is_empty());
    }
    path.strip_prefix(root)?
        .strip_prefix('/')
        .filter(|r| !r.is_empty())
}

fn join(name: &str, rest: &str, out: &mut String) {
    out.push_str(name);
    if !name.is_empty() && !name.ends_with('/') {
        out.push('/');
    }
    out.push_str(rest);
}

/// A gitignore line as a glob, as rg reads `-g`; None for a blank or `#` line.
fn parse(line: &str) -> Option<(String, Rule)> {
    if line.starts_with('#') {
        return None;
    }
    let mut line = if line.ends_with("\\ ") {
        line
    } else {
        line.trim_end()
    };
    if line.is_empty() {
        return None;
    }
    let mut rule = Rule {
        exclude: false,
        dir_only: false,
    };
    let mut anchored = false;
    if line.starts_with("\\!") || line.starts_with("\\#") {
        line = &line[1..];
    } else {
        if let Some(rest) = line.strip_prefix('!') {
            rule.exclude = true;
            line = rest;
        }
        if let Some(rest) = line.strip_prefix('/') {
            anchored = true;
            line = rest;
        }
    }
    if let Some(rest) = line.strip_suffix('/') {
        rule.dir_only = true;
        line = rest.strip_suffix('\\').unwrap_or(rest);
    }
    let mut actual = line.to_owned();
    if !anchored && !line.contains('/') && !line.starts_with("**/") && line != "**" {
        actual.insert_str(0, "**/");
    }
    if actual.ends_with("/**") {
        actual.push_str("/*");
    }
    Some((actual, rule))
}

#[cfg(test)]
mod tests;
