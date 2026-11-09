//! The markdown results, the JSONL trace and the machine and version stamps.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde_json::json;

use crate::corpus::QuerySet;
use crate::verify::{Paths, Unsearched};
use crate::{Kind, Row, RowKind};

pub struct Run {
    pub corpus: &'static str,
    pub fixture: bool,
    pub set: QuerySet,
    pub revision: Option<String>,
    pub files: usize,
    pub bytes: u64,
    pub stamps: Stamps,
}

/// What verify found.
pub struct Found<'a> {
    pub diffs: &'a [String],
    pub unsearched: &'a [Unsearched],
    pub paths: &'a Paths,
}

pub struct Stamps {
    pub machine: String,
    pub versions: String,
    /// Why an optional tool has no row.
    pub notes: Vec<String>,
    pub date: String,
}

const SUMMARY: &str = "\
| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | intersection p50 | intersection mean | all p50 | all mean | query footprint | query RSS | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|";

pub fn run(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    let text = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !text.is_empty()).then_some(text)
}

fn machine() -> Option<String> {
    let cores = std::thread::available_parallelism().ok()?;
    let uname = run("uname", &["-srm"])?;
    let (cpu, mem, os) = if cfg!(target_os = "macos") {
        let mem: u64 = run("sysctl", &["-n", "hw.memsize"])?.parse().ok()?;
        let os = format!("macOS {}", run("sw_vers", &["-productVersion"])?);
        (run("sysctl", &["-n", "machdep.cpu.brand_string"])?, mem, os)
    } else {
        let field = |file: &str, key: &str| {
            let text = std::fs::read_to_string(file).ok()?;
            let line = text.lines().find(|l| l.starts_with(key))?;
            Some(
                line[key.len()..]
                    .trim_matches([' ', '\t', ':', '=', '"'])
                    .to_string(),
            )
        };
        let kib: u64 = field("/proc/meminfo", "MemTotal")?
            .trim_end_matches(" kB")
            .parse()
            .ok()?;
        let os = field("/etc/os-release", "PRETTY_NAME").unwrap_or_default();
        (
            field("/proc/cpuinfo", "model name").unwrap_or_else(|| "unknown".into()),
            kib << 10,
            os,
        )
    };
    let gb = mem as f64 / (1u64 << 30) as f64;
    Some(format!("{cpu}, {cores} cores, {gb:.0} GB; {os} ({uname})"))
}

pub fn stamps(
    fmgrep: Option<&Path>,
    tgrep: &Result<crate::tgrep::Tgrep, String>,
    elgrep: &Result<crate::elgrep::Elgrep, String>,
) -> Stamps {
    let unknown = || "unknown".to_string();
    let ws = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let rev = run("git", &["-C", ws, "rev-parse", "--short", "HEAD"]).map(|rev| {
        let dirty = run("git", &["-C", ws, "status", "--porcelain", "-uno"]).is_some();
        format!(" ({rev}{})", if dirty { "-dirty" } else { "" })
    });
    let fmgrep = fmgrep.and_then(|bin| run(bin.to_str()?, &["--version"]));
    let rg = run("rg", &["--version"]).and_then(|v| Some(v.lines().next()?.to_string()));
    let mut versions = format!(
        "{}{}; {}",
        fmgrep.unwrap_or_else(unknown),
        rev.unwrap_or_default(),
        rg.unwrap_or_else(unknown)
    );
    let found = [
        tgrep.as_ref().map(|t| &t.version),
        elgrep.as_ref().map(|e| &e.version),
    ];
    let mut notes = Vec::new();
    for tool in found {
        match tool {
            Ok(version) => versions += &format!("; {version}"),
            Err(note) => notes.push(note.clone()),
        }
    }
    Stamps {
        machine: machine().unwrap_or_else(unknown),
        versions,
        notes,
        date: run("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"]).unwrap_or_else(unknown),
    }
}

/// The lower median.
pub fn p50(latencies: &[Duration]) -> Option<Duration> {
    let mut sorted = latencies.to_vec();
    sorted.sort();
    sorted.get(sorted.len().checked_sub(1)? / 2).copied()
}

fn mean(latencies: &[Duration]) -> Option<Duration> {
    let n = u32::try_from(latencies.len()).ok().filter(|&n| n > 0)?;
    Some(latencies.iter().sum::<Duration>() / n)
}

fn ms(d: Duration) -> String {
    match d.as_secs_f64() * 1000.0 {
        ms if ms >= 1000.0 => format!("{:.2} s", ms / 1000.0),
        ms if ms >= 100.0 => format!("{ms:.0} ms"),
        ms if ms >= 10.0 => format!("{ms:.1} ms"),
        ms => format!("{ms:.2} ms"),
    }
}

pub fn bytes(n: u64) -> String {
    let (gb, mb) = ((1u64 << 30) as f64, (1u64 << 20) as f64);
    match n as f64 {
        n if n >= gb => format!("{:.2} GB", n / gb),
        n if n >= mb => format!("{:.2} MB", n / mb),
        n if n >= 1024.0 => format!("{:.2} KB", n / 1024.0),
        n => format!("{n:.0} B"),
    }
}

fn opt<T>(v: Option<T>, f: impl Fn(T) -> String) -> String {
    v.map_or_else(|| "-".to_string(), f)
}

fn label(name: RowKind) -> &'static str {
    match name {
        RowKind::FmgrepStoreDocs => "fmgrep `--store-docs`",
        RowKind::FmgrepDirect => "fmgrep, no server",
        RowKind::Rg | RowKind::Fmgrep | RowKind::Tgrep | RowKind::Elgrep => name.into(),
    }
}

pub fn markdown(run: &Run, rows: &[Row], found: &Found) -> String {
    let diffs = found.diffs;
    let (url, pin) = (&run.set.repo_url, &run.set.commit);
    let revision = match &run.revision {
        Some(rev) if rev == pin => format!("{url} @ {rev} (pinned)"),
        Some(rev) => format!("{url} @ {rev} (differs from pinned {pin}; not comparable)"),
        None => format!("unknown (not a git checkout; pinned {url} @ {pin})"),
    };
    let n = run.set.queries.len();
    let counts = Kind::ALL.map(|k| {
        let count = run.set.queries.iter().filter(|q| Kind::of(q) == k).count();
        format!("{count} {}", k.name())
    });
    let stamps = &run.stamps;
    let note: String = stamps.notes.iter().map(|n| format!("- {n}\n")).collect();
    let searched = rows
        .iter()
        .map(|r| format!("{} {}", label(r.name), r.searched.len()));
    let searched = searched.collect::<Vec<_>>().join(", ");
    let mut md = format!(
        "## fmgrep benchmark: {}{}\n\n- corpus: {} files, {}\n\
         - files searched: {searched} (non-empty)\n- revision: {revision}\n\
         - queries: {n} ({})\n- machine: {}\n- versions: {}\n{note}\
         - date: {}\n\n{SUMMARY}\n",
        run.corpus,
        if run.fixture { " (FIXTURE)" } else { "" },
        run.files,
        bytes(run.bytes),
        counts.join(", "),
        stamps.machine,
        stamps.versions,
        stamps.date,
    );
    for r in rows {
        let mut cells = vec![
            label(r.name).to_string(),
            opt(r.index_time, ms),
            opt(r.index_bytes, bytes),
            opt(r.ready, ms),
        ];
        for kind in Kind::ALL {
            let of_kind = r.queries.iter().filter(|q| Kind::of(&q.term) == kind);
            let latencies: Vec<Duration> = of_kind.map(|q| q.latency).collect();
            cells.extend([opt(p50(&latencies), ms), opt(mean(&latencies), ms)]);
        }
        let all: Vec<Duration> = r.queries.iter().map(|q| q.latency).collect();
        cells.extend([opt(p50(&all), ms), opt(mean(&all), ms)]);
        cells.extend([
            opt(r.footprint, bytes),
            opt(r.query_rss, bytes),
            opt(r.index_peak_rss, bytes),
            opt(r.load1, |(a, b)| {
                format!("{a:.2}->{b:.2}{}", if a.max(b) >= 4.0 { "!" } else { "" })
            }),
        ]);
        md += &format!("| {} |\n", cells.join(" | "));
    }
    let notes = rows
        .iter()
        .flat_map(|r| r.notes.iter().map(|n| (label(r.name), n)));
    let notes: String = notes.map(|(row, n)| format!("- {row}: {n}\n")).collect();
    if !notes.is_empty() {
        md += &format!("\n{notes}");
    }
    let tgrep = if rows.iter().any(|r| r.name == RowKind::Tgrep) {
        ", or a `tgrep` client against one `tgrep serve`"
    } else {
        ""
    };
    let tgrep_and = if tgrep.is_empty() {
        ""
    } else {
        ", or several `tgrep` clients"
    };
    let direct = if rows.iter().any(|r| r.name == RowKind::FmgrepDirect) {
        ", or one `fmgrep --index` process with no server"
    } else {
        ""
    };
    let has_eg = rows.iter().any(|r| r.name == RowKind::Elgrep);
    let eg = [", or `eg` beside one `eg-indexd`", ", or an `eg` pipe"];
    let [elgrep, elgrep_and] = eg.map(|s| if has_eg { s } else { "" });
    md += &format!(
        "\nLatency is per query, spawn to exit, of the processes printing the matching lines: \
         `rg` over the checkout, or `fmgrep --server` against one \
         `fmgrep serve`{direct}{tgrep}{elgrep}. \
         An intersection is an `rg` pipe{tgrep_and}{elgrep_and}. \
         Footprint is the peak anonymous memory while querying; RSS also counts \
         file-backed pages such as the mapped index. load1 is the 1-minute load average \
         at the row's start and end; `!` marks 4 or more, where timings are likely inflated.\n\n"
    );
    md += &match diffs.len() {
        0 => format!(
            "verify: OK, {} on {n} queries\n",
            match rows.len() {
                1 => "1 row agrees".to_string(),
                k => format!("{k} rows agree"),
            }
        ),
        k => format!(
            "verify: FAILED, {k} difference(s)\n\n```\n{}\n```\n",
            diffs.join("\n")
        ),
    };
    md += &unsearched(n, found);

    let Some((base, others)) = rows.split_first() else {
        return md;
    };
    let mut header = vec!["query".to_string(), "lines".into(), "files".into()];
    header.extend(rows.iter().map(|r| label(r.name).to_string()));
    header.extend(others.iter().map(|r| match others.len() {
        1 => "speedup".to_string(),
        _ => format!("speedup {}", label(r.name)),
    }));
    let align = "|--:".repeat(header.len() - 1);
    for kind in Kind::ALL {
        let queries: Vec<_> = base
            .queries
            .iter()
            .filter(|q| Kind::of(&q.term) == kind)
            .collect();
        if queries.is_empty() {
            continue;
        }
        md += &format!(
            "\n<details><summary>{} queries ({})</summary>\n\n| {} |\n|---{align}|\n",
            kind.name(),
            queries.len(),
            header.join(" | ")
        );
        for q in queries {
            let ticks = if q.term.contains('`') { "``" } else { "`" };
            let term = format!("{ticks}{}{ticks}", q.term.replace('|', "\\|"));
            let mut cells = vec![term, q.hits.lines().to_string(), q.hits.files().to_string()];
            let latency = |r: &Row| Some(r.queries.iter().find(|o| o.term == q.term)?.latency);
            cells.extend(rows.iter().map(|r| opt(latency(r), ms)));
            cells.extend(others.iter().map(|r| {
                opt(latency(r).filter(|d| !d.is_zero()), |d| {
                    let x = q.latency.as_secs_f64() / d.as_secs_f64();
                    if x >= 10.0 {
                        format!("{x:.0}x")
                    } else {
                        format!("{x:.1}x")
                    }
                })
            }));
            md += &format!("| {} |\n", cells.join(" | "));
        }
        md += "\n</details>\n";
    }
    md
}

/// One bullet per query printing lines from files some rows did not search.
fn unsearched(n: usize, found: &Found) -> String {
    let class = |u: &Unsearched| {
        let (k, shown) = (u.files.len(), u.files.iter().take(3));
        let shown = shown.map(|&f| format!("`{}`", found.paths.name(f)));
        let more = if k > 3 { ", …" } else { "" };
        let shown = shown.collect::<Vec<_>>().join(", ");
        format!("{} {k} ({shown}{more})", u.class.name())
    };
    let row = |us: &[Unsearched]| {
        let classes = us.iter().map(class).collect::<Vec<_>>().join(", ");
        format!("{} did not search {classes}", label(us[0].row))
    };
    let terms: Vec<_> = found.unsearched.chunk_by(|a, b| a.term == b.term).collect();
    if terms.is_empty() {
        return String::new();
    }
    let mut md = format!(
        "\n{} of {n} queries print lines from files some rows did not search \
         (not failures):\n\n",
        terms.len()
    );
    for us in terms {
        let (term, rows) = (&us[0].term, us.chunk_by(|a, b| a.row == b.row));
        let ticks = if term.contains('`') { "``" } else { "`" };
        let rows = rows.map(row).collect::<Vec<_>>().join("; ");
        md += &format!("- {ticks}{term}{ticks}: {rows}\n");
    }
    md
}

pub fn trace(run: &Run, rows: &[Row], found: &Found) -> String {
    let us = |d: Option<Duration>| d.map(|d| d.as_micros() as u64);
    let not_searched = |row: RowKind, term: &str| {
        let own = found
            .unsearched
            .iter()
            .filter(|u| u.row == row && u.term == term);
        let own = own.map(|u| {
            let files: Vec<_> = u.files.iter().map(|&f| found.paths.name(f)).collect();
            json!({"class": u.class.name(), "files": files})
        });
        own.collect::<Vec<_>>()
    };
    let mut lines = Vec::new();
    for r in rows {
        for q in &r.queries {
            lines.push(json!({
                "type": "query",
                "index_name": r.name.to_string(),
                "corpus": run.corpus,
                "corpus_revision": run.revision,
                "fixture": run.fixture,
                "term": q.term,
                "term_kind": Kind::of(&q.term).name(),
                "latency_us": q.latency.as_micros() as u64,
                "lines": q.hits.lines(),
                "files": q.hits.files(),
                "not_searched": not_searched(r.name, &q.term),
            }));
        }
        lines.push(json!({
            "type": "index",
            "index_name": r.name.to_string(),
            "corpus": run.corpus,
            "corpus_revision": run.revision,
            "fixture": run.fixture,
            "num_files_searched": r.searched.len(),
            "index_time_us": us(r.index_time),
            "index_size_bytes": r.index_bytes,
            "ready_us": us(r.ready),
            "index_peak_rss_bytes": r.index_peak_rss,
            "query_peak_footprint_bytes": r.footprint,
            "query_peak_rss_bytes": r.query_rss,
            "loadavg_start": r.load1.map(|l| l.0),
            "loadavg_end": r.load1.map(|l| l.1),
            "notes": r.notes,
        }));
    }
    lines.iter().map(|l| format!("{l}\n")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Query;
    use crate::verify::{Class, FileHits, Hits};

    const PIN: &str = "4cf7628aa66f56000c29aef93924b6b8515db8f7";

    const TERMS: [&str; 3] = ["a", "/b/", "a AND b"];

    fn run_of(fixture: bool, revision: &str) -> (Run, Vec<Row>) {
        let notes = vec!["tgrep: not found on PATH".to_string()];
        let (machine, versions, date) = ("m".into(), "v".into(), "d".into());
        let stamps = Stamps {
            machine,
            versions,
            notes,
            date,
        };
        let (mut set, revision) = (crate::corpus::query_set("ghc"), Some(revision.to_string()));
        set.queries = TERMS.map(String::from).to_vec();
        let run = Run {
            corpus: "ghc",
            fixture,
            set,
            revision,
            files: 9,
            bytes: 9,
            stamps,
        };
        let (mut rg, mut docs) = (Row::new(RowKind::Rg), Row::new(RowKind::FmgrepStoreDocs));
        (rg.searched, docs.searched) = (vec![0, 1], vec![0]);
        for term in TERMS {
            let rows = if term.contains(" AND ") {
                vec![&mut rg]
            } else {
                vec![&mut rg, &mut docs]
            };
            for row in rows {
                let file = FileHits {
                    file: 0,
                    lines: 1,
                    digest: 0,
                };
                let hits = Hits {
                    per_file: vec![file],
                    binary: Vec::new(),
                };
                row.queries
                    .push(Query::new(term, Duration::from_millis(2), hits));
            }
        }
        (rg.footprint, rg.query_rss) = (Some(1 << 20), Some(2 << 20));
        rg.notes = vec!["n".to_string()];
        (docs.index_time, docs.load1) = (Some(Duration::from_secs(2)), Some((1.0, 4.5)));
        (run, vec![rg, docs])
    }

    fn paths() -> Paths {
        let walk = b"/r/a\n/r/.w\n/r/b\n/r/c\n/r/d\n/r/e\n";
        Paths::from_lists(Path::new("/r"), walk, b"", &[])
    }

    fn markdown_of(fixture: bool, revision: &str, diffs: &[String]) -> String {
        let (run, rows) = run_of(fixture, revision);
        let paths = paths();
        let unsearched = [];
        let found = Found {
            diffs,
            unsearched: &unsearched,
            paths: &paths,
        };
        markdown(&run, &rows, &found)
    }

    fn assert_has(md: &str, wants: &[&str]) {
        for want in wants {
            assert!(md.contains(want), "{want:?} not in\n{md}");
        }
    }

    #[test]
    fn p50_is_the_lower_median() {
        let d = Duration::from_millis;
        assert_eq!(p50(&[]), None);
        assert_eq!(p50(&[d(3), d(1), d(2)]), Some(d(2)));
        assert_eq!(p50(&[d(4), d(1), d(3), d(2)]), Some(d(2)));
    }

    #[test]
    fn benchmarks_md_has_the_summary_columns_and_row_commands() {
        let doc = include_str!("../../../BENCHMARKS.md");
        assert!(doc.contains(SUMMARY));
        let lines = crate::rows::LINES[..2].join(" ");
        for row in ["rg", "fmgrep --server ADDR", "tgrep", "eg"] {
            let command = format!("`{row} {lines}");
            assert!(doc.contains(&command), "{command} not in BENCHMARKS.md");
        }
        for tool in ["rg", "eg"] {
            let pipe = format!("xargs -0 -r {tool} {lines}");
            assert!(doc.contains(&pipe), "{pipe} not in BENCHMARKS.md");
        }
        assert!(
            doc.contains("-t and`"),
            "tgrep's intersection not in BENCHMARKS.md"
        );
        let md = markdown_of(false, PIN, &[]);
        assert_has(
            &md,
            &[
                &format!(
                    "\n| rg | - | - | - |{} 1.00 MB | 2.00 MB | - | - |\n",
                    " 2.00 ms |".repeat(8)
                ),
                "\n| fmgrep `--store-docs` | 2.00 s | - |",
                "| 2.00 ms | 2.00 ms | - | - | 2.00 ms | 2.00 ms | - | - | - | 1.00->4.50! |\n",
                "\n- queries: 3 (1 literal, 1 regex, 1 intersection)\n",
                "\n- tgrep: not found on PATH\n",
                " 1.00->4.50! |\n\n- rg: n\n\nLatency is",
                "verify: OK, 2 rows agree on 3 queries",
            ],
        );
    }

    #[test]
    fn markdown_flags_fixture_and_off_pin_revision() {
        let md = markdown_of(false, PIN, &[]);
        assert!(md.starts_with("## fmgrep benchmark: ghc\n"), "{md}");
        assert_has(&md, &[&format!("@ {PIN} (pinned)")]);
        let md = markdown_of(true, "abc", &["x".into()]);
        let off_pin = format!("@ abc (differs from pinned {PIN}; not comparable)");
        assert_has(
            &md,
            &[
                "## fmgrep benchmark: ghc (FIXTURE)\n",
                &off_pin,
                "verify: FAILED, 1 difference(s)",
            ],
        );
    }

    #[test]
    fn per_query_tables_follow_the_kinds() {
        let md = markdown_of(false, PIN, &[]);
        let at = |want: &str| {
            md.find(want)
                .unwrap_or_else(|| panic!("{want:?} not in\n{md}"))
        };
        let blocks = ["literal", "regex", "intersection"].map(|k| {
            at(&format!(
                "\n<details><summary>{k} queries (1)</summary>\n\n|"
            ))
        });
        assert!(blocks.is_sorted(), "{md}");
        assert_eq!(md.matches("<details>").count(), 3, "{md}");
        assert_eq!(md.matches("\n\n</details>\n").count(), 3, "{md}");
        let and = at("\n| `a AND b` | 1 | 1 | 2.00 ms | - | - |\n");
        assert!(and > blocks[2], "{md}");

        let (run, rows) = run_of(false, PIN);
        let paths = paths();
        let found = Found {
            diffs: &[],
            unsearched: &[],
            paths: &paths,
        };
        let md = markdown(&run, &rows[1..], &found);
        assert_has(&md, &["regex queries (1)"]);
        assert!(!md.contains("intersection queries"), "{md}");

        let trace = trace(&run, &rows, &found);
        for (term, kind) in [
            ("a AND b", "intersection"),
            ("/b/", "regex"),
            ("a", "literal"),
        ] {
            let want = format!(r#""term":"{term}","term_kind":"{kind}""#);
            assert!(trace.contains(&want), "{want} not in\n{trace}");
        }
        for rss in ["2097152", "null"] {
            let want = format!(r#""query_peak_rss_bytes":{rss}"#);
            assert!(trace.contains(&want), "{want} not in\n{trace}");
        }
    }

    #[test]
    fn markdown_lists_searched_counts_and_differences() {
        let (run, rows) = run_of(false, PIN);
        let paths = paths();
        let missing = |term: &str, row, class, files: &[u32]| Unsearched {
            term: term.to_string(),
            row,
            class,
            files: files.to_vec(),
        };
        let unsearched = [
            missing("a", RowKind::Rg, Class::Symlink, &[0]),
            missing("a", RowKind::FmgrepStoreDocs, Class::Hidden, &[1]),
            missing("a", RowKind::FmgrepStoreDocs, Class::Binary, &[2, 3, 4, 5]),
            missing("/b/", RowKind::Rg, Class::Ignored, &[2]),
        ];
        let found = Found {
            diffs: &[],
            unsearched: &unsearched,
            paths: &paths,
        };
        let md = markdown(&run, &rows, &found);
        assert_has(
            &md,
            &[
                "\n- files searched: rg 2, fmgrep `--store-docs` 1 (non-empty)\n",
                "verify: OK, 2 rows agree on 3 queries\n\n\
                 2 of 3 queries print lines from files some rows did not search (not failures):\n\n\
                 - `a`: rg did not search symlink 1 (`a`); fmgrep `--store-docs` did not search \
                 hidden 1 (`.w`), binary 4 (`b`, `c`, `d`, …)\n\
                 - `/b/`: rg did not search ignored 1 (`b`)\n",
            ],
        );
        let trace = trace(&run, &rows, &found);
        let want = [
            r#""index_name":"rg","latency_us":2000,"lines":1,"not_searched":[{"class":"symlink","files":["a"]}],"term":"a","#,
            r#""not_searched":[{"class":"hidden","files":[".w"]},{"class":"binary","files":["b","c","d","e"]}],"term":"a","#,
            r#""not_searched":[],"term":"a AND b","#,
            r#""num_files_searched":2"#,
        ];
        for want in want {
            assert!(trace.contains(want), "{want} not in\n{trace}");
        }
    }
}
