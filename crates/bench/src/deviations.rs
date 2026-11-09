//! Third-party rows' differences from the reference row: notes under the
//! table, the verify line and the trace.

use serde_json::{Value, json};

use crate::report::Found;
use crate::{Row, RowKind, RowName, THIRD_PARTY};

/// A third-party row's lines from one file, where they differ from the reference row's.
#[derive(Debug, PartialEq, Eq)]
pub struct Deviation {
    pub term: String,
    pub row: RowName,
    /// The first row that searched the file.
    pub against: RowName,
    pub file: u32,
    pub want: u32,
    pub got: u32,
    /// Same count, other line numbers.
    pub other: bool,
}

/// Paths shown per part and queries per note; the rest are elided.
const PATHS: usize = 3;
const QUERIES: usize = 5;

fn plural(k: usize, one: &str, many: &str) -> String {
    format!("{k} {}", if k == 1 { one } else { many })
}

fn code(s: &str) -> String {
    let ticks = if s.contains('`') { "``" } else { "`" };
    format!("{ticks}{s}{ticks}")
}

/// The number of distinct (row, query) pairs.
pub fn count(deviations: &[Deviation]) -> usize {
    let pairs = deviations.iter().map(|d| (d.row, &d.term));
    let mut pairs: Vec<_> = pairs.collect();
    pairs.sort_unstable();
    pairs.dedup();
    pairs.len()
}

pub fn differences(k: usize) -> String {
    plural(k, "third-party difference", "third-party differences")
}

/// `verify: OK, ...` for a run with no fatal difference.
pub fn ok_line(rows: &[Row], n: usize, deviations: &[Deviation]) -> String {
    let agree = |k| plural(k, "row agrees", "rows agree");
    let k = count(deviations);
    if k == 0 {
        return format!("verify: OK, {} on {n} queries\n", agree(rows.len()));
    }
    let ours = rows.iter().filter(|r| !THIRD_PARTY.contains(&r.name.kind));
    let ours: Vec<RowKind> = ours.map(|r| r.name.kind).collect();
    let subject = match ours[..] {
        [RowKind::Rg, _, ..] => "fmgrep rows agree with rg".to_string(),
        _ => agree(ours.len()),
    };
    let k = differences(k);
    format!("verify: OK, {subject} on {n} queries; {k} (see notes)\n")
}

/// `missed`, `extra` or `other` lines of one query's deviations.
fn part(found: &Found, devs: &[&Deviation]) -> Option<String> {
    let (first, k) = (devs.first()?, devs.len());
    let lines: u32 = devs.iter().map(|d| d.want.abs_diff(d.got)).sum();
    let files = plural(k, "file", "files");
    let what = match (first.other, first.got < first.want) {
        (true, _) => format!("other lines in {files}"),
        (_, true) => format!(
            "{} missed in {files}",
            plural(lines as usize, "line", "lines")
        ),
        _ => format!(
            "{} in {files}",
            plural(lines as usize, "extra line", "extra lines")
        ),
    };
    let shown = devs.iter().take(PATHS);
    let shown = shown.map(|d| code(&found.paths.name(d.file)));
    let more = if k > PATHS { ", …" } else { "" };
    let shown = shown.collect::<Vec<_>>().join(", ");
    Some(format!("{what} ({shown}{more})"))
}

/// One `(row, note)` per third-party row that deviates, in query order.
pub fn notes(n: usize, found: &Found) -> Vec<(RowName, String)> {
    let mut out = Vec::new();
    for row in THIRD_PARTY.map(RowName::from) {
        let own: Vec<&Deviation> = found.deviations.iter().filter(|d| d.row == row).collect();
        if own.is_empty() {
            continue;
        }
        let mut against: Vec<RowName> = Vec::new();
        for d in &own {
            if !against.contains(&d.against) {
                against.push(d.against);
            }
        }
        let queries: Vec<_> = own.chunk_by(|a, b| a.term == b.term).collect();
        let shown = queries.iter().take(QUERIES).map(|devs| {
            let kinds: [fn(&Deviation) -> bool; 3] =
                [|d| d.got < d.want, |d| d.got > d.want, |d| d.other];
            let parts = kinds.iter().filter_map(|is| {
                let of: Vec<&Deviation> = devs.iter().copied().filter(|d| is(d)).collect();
                part(found, &of)
            });
            let parts: Vec<String> = parts.collect();
            format!("{}, {}", code(&devs[0].term), parts.join(", "))
        });
        let mut shown: Vec<String> = shown.collect();
        if queries.len() > QUERIES {
            shown.push(format!("and {} more", queries.len() - QUERIES));
        }
        let k = queries.len();
        let verb = if k == 1 { "differs" } else { "differ" };
        let against: Vec<String> = against.iter().map(RowName::to_string).collect();
        let against = against.join(" or ");
        let shown = shown.join("; ");
        out.push((
            row,
            format!("{k} of {n} queries {verb} from {against}: {shown}"),
        ));
    }
    out
}

/// The trace's `differs` for one row's query.
pub fn trace(found: &Found, row: RowName, term: &str) -> Value {
    let own = found
        .deviations
        .iter()
        .filter(|d| d.row == row && d.term == term);
    let own = own.map(|d| {
        let file = found.paths.name(d.file);
        json!({"against": d.against.to_string(), "file": file, "lines": d.got, "want": d.want})
    });
    Value::Array(own.collect())
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use super::*;
    use crate::Query;
    use crate::report::{Run, Stamps, markdown};
    use crate::verify::{FileHits, Hits, Paths};

    fn paths() -> Paths {
        let walk = b"/r/a\n/r/b\n/r/c\n/r/d\n/r/e\n";
        Paths::from_lists(Path::new("/r"), walk, b"", &[])
    }

    fn dev(term: &str, row: RowKind, file: u32, want: u32, got: u32) -> Deviation {
        Deviation {
            term: term.to_string(),
            row: row.into(),
            against: RowKind::Rg.into(),
            file,
            want,
            got,
            other: want == got,
        }
    }

    fn found<'a>(paths: &'a Paths, deviations: &'a [Deviation]) -> Found<'a> {
        Found {
            diffs: &[],
            unsearched: &[],
            deviations,
            paths,
        }
    }

    #[test]
    fn notes_count_queries_lines_and_files() {
        let paths = paths();
        let one = [dev("MCK_ AND C_Check", RowKind::Elgrep, 0, 40, 0)];
        assert_eq!(
            notes(46, &found(&paths, &one)),
            [(
                RowKind::Elgrep.into(),
                "1 of 46 queries differs from rg: `MCK_ AND C_Check`, 40 lines missed in 1 file (`a`)"
                    .to_string()
            )]
        );
        let mut many: Vec<_> = (0..5).map(|f| dev("x", RowKind::Tgrep, f, 2, 1)).collect();
        many.extend([
            dev("x", RowKind::Tgrep, 0, 1, 3),
            dev("x", RowKind::Tgrep, 1, 2, 2),
        ]);
        many.push(dev("a`b", RowKind::Elgrep, 2, 0, 1));
        for t in ["t1", "t2", "t3", "t4", "t5"] {
            many.push(dev(t, RowKind::Tgrep, 4, 1, 0));
        }
        let mut vs_fmgrep = dev("y", RowKind::Tgrep, 3, 1, 2);
        vs_fmgrep.against = RowKind::Fmgrep.into();
        many.push(vs_fmgrep);
        let got = notes(46, &found(&paths, &many));
        let tgrep = "7 of 46 queries differ from rg or fmgrep: \
                     `x`, 5 lines missed in 5 files (`a`, `b`, `c`, …), \
                     2 extra lines in 1 file (`a`), other lines in 1 file (`b`); \
                     `t1`, 1 line missed in 1 file (`e`); `t2`, 1 line missed in 1 file (`e`); \
                     `t3`, 1 line missed in 1 file (`e`); `t4`, 1 line missed in 1 file (`e`); \
                     and 2 more";
        let elgrep = "1 of 46 queries differs from rg: ``a`b``, 1 extra line in 1 file (`c`)";
        assert_eq!(
            got,
            [
                (RowKind::Tgrep.into(), tgrep.to_string()),
                (RowKind::Elgrep.into(), elgrep.to_string())
            ]
        );
        assert_eq!((count(&many), count(&one), count(&[])), (8, 1, 0));
        assert_eq!(differences(1), "1 third-party difference");
        assert_eq!(differences(2), "2 third-party differences");
    }

    fn hsc_env(name: RowKind, lines: u32) -> Row {
        let mut row = Row::new(name);
        row.searched = vec![0];
        let file = FileHits {
            file: 0,
            lines,
            digest: 0,
        };
        let hits = Hits {
            per_file: vec![file],
            binary: Vec::new(),
        };
        let latency = Duration::from_millis(1);
        row.queries.push(Query::new("HscEnv", latency, hits));
        row
    }

    #[test]
    fn markdown_and_trace_carry_deviations() {
        let mut set = crate::corpus::query_set("ghc");
        set.queries = vec!["HscEnv".to_string()];
        let (machine, versions, date) = ("m".into(), "v".into(), "d".into());
        let run = Run {
            corpus: "ghc",
            fixture: true,
            set,
            revision: None,
            files: 1,
            bytes: 1,
            stamps: Stamps {
                machine,
                versions,
                notes: Vec::new(),
                date,
            },
        };
        let paths = paths();
        let mut rows = vec![
            hsc_env(RowKind::Rg, 1),
            hsc_env(RowKind::Fmgrep, 1),
            hsc_env(RowKind::Elgrep, 1),
        ];
        let md = markdown(&run, &rows, &found(&paths, &[]));
        assert!(
            md.contains("\nverify: OK, 3 rows agree on 1 queries\n"),
            "{md}"
        );

        rows[2].queries[0].hits.per_file.clear();
        let devs = [dev("HscEnv", RowKind::Elgrep, 0, 1, 0)];
        let found = found(&paths, &devs);
        for (name, note) in notes(1, &found) {
            rows.iter_mut()
                .find(|r| r.name == name)
                .unwrap()
                .notes
                .push(note);
        }
        let md = markdown(&run, &rows, &found);
        let wants = [
            "\n\n- elgrep: 1 of 1 queries differs from rg: `HscEnv`, 1 line missed in 1 file (`a`)\n\nLatency",
            "\nverify: OK, fmgrep rows agree with rg on 1 queries; 1 third-party difference (see notes)\n",
        ];
        for want in wants {
            assert!(md.contains(want), "{want:?} not in\n{md}");
        }
        let trace = crate::report::trace(&run, &rows, &found);
        let want = r#""differs":[{"against":"rg","file":"a","lines":0,"want":1}]"#;
        assert_eq!(trace.matches(want).count(), 1, "{trace}");
        assert_eq!(trace.matches(r#""differs":[]"#).count(), 2, "{trace}");
    }
}
