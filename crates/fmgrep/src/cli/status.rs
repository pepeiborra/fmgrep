//! What `status` says of an index: what a search's check of what changed
//! just did. Status runs that check itself, as a search runs it, and
//! prints the values it returned; it makes no decision of its own.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::print;
use crate::fetch::Ask;
use crate::manifest::Manifest;
use crate::query::{self, check_changes, note_stale};
use crate::source::{Learned, keep, kept, named, not_due};

/// What the check did and found, as `--json` prints it too.
#[derive(Serialize)]
pub struct Facts {
    pub learned: Learned,
    /// The lines the check warned, in the search's words.
    pub warned: Vec<String>,
    pub stale: usize,
    pub added: usize,
    pub gone: usize,
    /// The ignore files that moved or are new since the index; `None`
    /// when the index recorded no walk.
    pub ignores: Option<Vec<String>>,
    /// The record the check saved.
    pub record: Option<PathBuf>,
}

/// The check of what changed that a search of the index at `index_dir`
/// runs, as it runs it, without the index load or a query: it may take a
/// token, ask watchman or run the stat pass, and it saves the record. The
/// lines it warns are kept, not printed; its notes print on stderr.
pub fn check(index_dir: &Path, manifest: &Manifest, root: Option<&Path>) -> Result<Facts, String> {
    let (mut docs, _blobs) = query::open(index_dir, manifest, root, Ask::Load)?;
    keep();
    let files = &manifest.files;
    let none = tracing::Span::none();
    let mut left_out = Vec::new();
    let learned =
        std::thread::scope(|s| check_changes(s, &none, &docs, files, None, &mut left_out));
    super::print_diagnostics(left_out);
    let learned = learned?;
    let mut notes = Vec::new();
    let stale = note_stale(&mut docs, files, &mut notes);
    super::print_diagnostics(notes);
    let record = docs.checkpoint(files)?;
    Ok(Facts {
        learned,
        warned: kept(),
        stale,
        added: docs.stale().len() - stale,
        gone: docs.gone(),
        ignores: docs.ignores(),
        record,
    })
}

/// One line per fact: how the check learned what changed, then what it
/// warned, in the search's words; the counts; the ignore files that
/// changed; the record.
pub fn lines(facts: &Facts) -> Vec<String> {
    let Facts {
        learned,
        warned,
        stale,
        added,
        gone,
        ignores,
        record,
    } = facts;
    let how = match learned {
        Learned::Stored => Some("none: the index holds the files".to_owned()),
        Learned::NotDue => Some(not_due()),
        Learned::Answered { named: n, kept } => Some(named(*n, *kept)),
        // What the check warned says why.
        Learned::Checked => warned.is_empty().then(|| "checked every file".to_owned()),
    };
    let mut lines: Vec<String> = how
        .iter()
        .chain(warned)
        .map(|l| format!("changes: {l}"))
        .collect();
    if *learned != Learned::Stored {
        lines.push(format!(
            "since the index: {stale} stale, {added} added, {gone} gone"
        ));
        let names = ignores.as_ref().map(|n| match n.is_empty() {
            true => "none".to_owned(),
            false => n.join(", "),
        });
        lines.extend(names.map(|n| format!("ignore files changed since the index: {n}")));
        lines.push(match record {
            Some(path) => format!("record: {}", path.display()),
            None => "record: none saved".to_owned(),
        });
    }
    lines
}

pub fn print(out: &mut dyn Write, facts: &Facts) -> Result<(), String> {
    lines(facts).iter().try_for_each(|l| print::line(out, l))
}

/// `status --json`: the manifest's fields, then what status found.
#[derive(Serialize)]
pub struct Json<'a> {
    #[serde(flatten)]
    pub manifest: &'a Manifest,
    /// The index directory a search uses.
    pub uses: &'a Path,
    /// What that search warns after `Using local index DIR`, if anything.
    pub note: Option<&'a str>,
    #[serde(flatten)]
    pub facts: &'a Facts,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every value the check returns is printed: each way it learned what
    /// changed, each count, each warned line, the record. The reason no
    /// ask was due follows the size at which a load asks.
    #[test]
    fn status_prints_every_value_the_check_returned() {
        crate::tests::ASK_FROM.set(Some(7));
        let ways = [
            Learned::Stored,
            Learned::NotDue,
            Learned::Answered {
                named: 17,
                kept: 19,
            },
            Learned::Checked,
        ];
        for learned in ways {
            let facts = Facts {
                learned,
                warned: vec!["the first warned line".into(), "the second".into()],
                stale: 11,
                added: 12,
                gone: 13,
                ignores: Some(vec![".ignore".into(), "sub/.gitignore".into()]),
                record: Some("/cache/r.json".into()),
            };
            let text = lines(&facts).join("\n");
            let mut want = vec!["changes: the first warned line", "changes: the second"];
            match learned {
                Learned::Stored => want.push("changes: none: the index holds the files"),
                Learned::NotDue => want.push("changes: fewer than 7 files: checked every file"),
                Learned::Answered { .. } => {
                    want.push("changes: watchman and the baseline named 17 path(s), 19 kept")
                }
                Learned::Checked => {}
            }
            if learned != Learned::Stored {
                want.push("since the index: 11 stale, 12 added, 13 gone");
                want.push("ignore files changed since the index: .ignore, sub/.gitignore");
                want.push("record: /cache/r.json");
            }
            for line in want {
                assert!(text.contains(line), "{learned:?}: {line:?} in {text}");
            }
            let quiet = Facts {
                warned: vec![],
                ignores: Some(vec![]),
                record: None,
                ..facts
            };
            let text = lines(&quiet).join("\n");
            let none = text.contains("ignore files changed since the index: none");
            assert_eq!(none, learned != Learned::Stored, "{text}");
            let unrecorded = Facts {
                ignores: None,
                ..quiet
            };
            let names = lines(&unrecorded).join("\n");
            assert!(!names.contains("ignore files"), "no walk record: {names}");
            let checked = text.contains("changes: checked every file");
            assert_eq!(checked, learned == Learned::Checked, "{text}");
            let none = text.contains("record: none saved");
            assert_eq!(none, learned != Learned::Stored, "{text}");
        }
        crate::tests::ASK_FROM.set(None);
    }
}
