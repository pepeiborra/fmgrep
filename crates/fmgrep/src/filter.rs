//! rg's `-g`, `--iglob` and PATH arguments over root-relative paths.

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

use crate::fetch::Paths;

pub struct PathFilter {
    set: GlobSet,
    rules: Vec<Rule>,
    includes: usize,
    paths: Vec<String>,
}

struct Rule {
    exclude: bool,
    dir_only: bool,
}

impl PathFilter {
    /// None when nothing filters. `globs` then `iglobs` are gitignore lines
    /// whose `!` excludes; `paths` are files or directories, `""` the root.
    pub fn new(
        globs: &[String],
        iglobs: &[String],
        case_insensitive: bool,
        paths: &[String],
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
            paths: paths
                .iter()
                .map(|p| p.trim_end_matches('/').to_owned())
                .collect(),
        }))
    }

    pub fn retain<T>(&self, paths: &Paths<'_>, items: &mut Vec<T>, id: impl Fn(&T) -> usize) {
        let mut keep = self.keeper();
        items.retain(|t| paths.path(id(t)).is_some_and(&mut keep));
    }

    /// Whether the globs leave no file of `paths` but the `gone` ones to
    /// search, with no PATH.
    pub fn excludes_all(&self, paths: &Paths<'_>, gone: impl Fn(usize) -> bool) -> bool {
        let mut keep = self.keeper();
        self.paths.is_empty()
            && (0..)
                .map_while(|id| paths.path(id).map(|p| (id, p)))
                .all(|(id, p)| gone(id) || !keep(p))
    }

    /// Whether a search covers the file at a path, remembering the last
    /// parent directory's verdict: paths in order share it.
    fn keeper(&self) -> impl FnMut(&str) -> bool + '_ {
        static ROOT: [String; 1] = [String::new()];
        let roots = if self.paths.is_empty() {
            &ROOT[..]
        } else {
            &self.paths
        };
        let mut last: Option<(usize, String, bool)> = None;
        let mut hits = Vec::new();
        move |path: &str| {
            let parent = path.rsplit_once('/').map_or("", |(dir, _)| dir);
            roots.iter().enumerate().any(|(i, root)| {
                let below = root.is_empty()
                    || path
                        .strip_prefix(root.as_str())
                        .is_some_and(|r| r.starts_with('/'));
                if path == root || !below {
                    return path == root;
                }
                let open = match &last {
                    Some((j, dir, open)) if *j == i && dir == parent => *open,
                    _ => {
                        let open = self.open(root, parent, &mut hits);
                        last = Some((i, parent.to_owned(), open));
                        open
                    }
                };
                open && self
                    .decide(path, false, &mut hits)
                    .unwrap_or(self.includes == 0)
            })
        }
    }

    /// Whether no directory from below `root` down to `dir` is excluded.
    fn open(&self, root: &str, dir: &str, hits: &mut Vec<usize>) -> bool {
        if dir.len() <= root.len() {
            return true;
        }
        let from = if root.is_empty() { 0 } else { root.len() + 1 };
        let ends = dir[from..].match_indices('/').map(|(i, _)| from + i);
        !ends
            .chain([dir.len()])
            .any(|end| self.decide(&dir[..end], true, hits) == Some(false))
    }

    /// `path`'s last matching glob: true to include.
    fn decide(&self, path: &str, dir: bool, hits: &mut Vec<usize>) -> Option<bool> {
        if self.rules.is_empty() {
            return None;
        }
        self.set.matches_into(path, hits);
        hits.iter()
            .rev()
            .map(|&i| &self.rules[i])
            .find(|r| dir || !r.dir_only)
            .map(|r| !r.exclude)
    }
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
