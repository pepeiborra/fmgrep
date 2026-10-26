//! The markdown docs stay in step with the code.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Files under `dir` whose name ends in `ext`, recursively, sorted.
fn files(dir: &Path, ext: &str, skip_dir: Option<&str>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap_or_else(|e| panic!("{}: {e}", d.display())) {
            let path = entry.unwrap().path();
            if path.is_dir() {
                if skip_dir != path.file_name().and_then(|n| n.to_str()) {
                    stack.push(path);
                }
            } else if path.to_str().is_some_and(|p| p.ends_with(ext)) {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Every `FMGREP_[A-Z0-9_]+` token in `text`.
fn knobs(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = text;
    while let Some(at) = rest.find("FMGREP_") {
        let tail = &rest[at..];
        let len = tail
            .find(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
            .unwrap_or(tail.len());
        let name = tail[..len].trim_end_matches('_');
        if name.len() > "FMGREP_".len() {
            out.insert(name.to_owned());
        }
        rest = &tail[len..];
    }
    out
}

#[test]
fn tuning_names_every_env_knob_and_no_other() {
    let mut code = BTreeSet::new();
    for path in files(&repo().join("crates/fmgrep/src"), ".rs", Some("tests")) {
        if path.file_name().is_some_and(|n| n == "tests.rs") {
            continue;
        }
        let text = read(&path);
        let live = text.split("#[cfg(test)]").next().unwrap_or_default();
        code.extend(knobs(live));
    }
    assert!(code.len() >= 3, "the scan found too few knobs: {code:?}");
    let doc = knobs(&read(&repo().join("docs/tuning.md")));
    let undocumented: Vec<_> = code.difference(&doc).collect();
    let stale: Vec<_> = doc.difference(&code).collect();
    assert!(
        undocumented.is_empty() && stale.is_empty(),
        "docs/tuning.md misses {undocumented:?} and names unknown {stale:?}"
    );
}

#[test]
fn every_doc_opens_with_a_purpose_line() {
    let mut docs = files(&repo().join("docs"), ".md", None);
    docs.push(repo().join("AGENTS.md"));
    for path in docs {
        let text = read(&path);
        let mut lines = text.lines();
        let (title, blank) = (lines.next().unwrap_or_default(), lines.next());
        let purpose: Vec<&str> = lines.take_while(|l| !l.is_empty()).collect();
        let purpose = purpose.join(" ");
        let one_sentence = purpose.ends_with('.')
            && !purpose[..purpose.len() - 1].contains(". ")
            && !purpose.starts_with(['#', '|', '>', '`', '-']);
        assert!(
            title.starts_with("# ") && blank == Some("") && one_sentence,
            "{}: want a `# title`, a blank line, then one purpose sentence",
            path.display()
        );
    }
}

#[test]
fn manifest_fields_are_documented() {
    let src = read(&repo().join("crates/fmgrep/src/manifest.rs"));
    let body = src
        .split("pub struct Manifest {")
        .nth(1)
        .and_then(|s| s.split("\n}").next())
        .expect("manifest.rs defines pub struct Manifest");
    let fields: Vec<&str> = body
        .lines()
        .filter_map(|l| l.trim().strip_prefix("pub "))
        .filter_map(|l| l.split(':').next())
        .collect();
    assert!(fields.len() >= 10, "too few fields parsed: {fields:?}");
    let doc = read(&repo().join("docs/index-format.md"));
    let missing: Vec<_> = fields
        .iter()
        .filter(|f| !doc.contains(&format!("`{f}`")))
        .collect();
    assert!(
        missing.is_empty(),
        "docs/index-format.md misses {missing:?}"
    );
}

/// README's environment table lists exactly the knobs `fmgrep index --help` does.
#[test]
fn readme_names_exactly_the_public_knobs() {
    let src = read(&repo().join("crates/fmgrep/src/cli/args.rs"));
    let help = src
        .split("Environment:\n")
        .nth(1)
        .and_then(|s| s.split("\";").next())
        .expect("the index help has an Environment: block");
    let public = knobs(help);
    assert!(public.len() >= 2, "too few knobs parsed: {public:?}");
    let readme = read(&repo().join("README.md"));
    let table = readme
        .split("\n## Environment\n")
        .nth(1)
        .and_then(|s| s.split("\n## ").next())
        .expect("README has an Environment section");
    assert_eq!(knobs(table), public);
}

/// Every relative `](target)` link in the markdown docs names an existing path.
#[test]
fn relative_links_resolve() {
    let mut docs = files(&repo().join("docs"), ".md", None);
    for name in [
        "README.md",
        "BENCHMARKS.md",
        "CONTRIBUTING.md",
        "SECURITY.md",
        "AGENTS.md",
    ] {
        docs.push(repo().join(name));
    }
    let mut broken = Vec::new();
    for path in docs {
        let text = read(&path);
        for tail in text.split("](").skip(1) {
            let target = tail.split(')').next().unwrap_or_default();
            let target = target.split('#').next().unwrap_or_default();
            if target.is_empty() || target.contains(':') {
                continue;
            }
            if !path.parent().unwrap().join(target).exists() {
                broken.push(format!("{}: {target}", path.display()));
            }
        }
    }
    assert!(broken.is_empty(), "broken links: {broken:?}");
}
