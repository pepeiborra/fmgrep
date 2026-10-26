//! The markdown results, the JSONL trace and the machine and version stamps.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde_json::json;

use crate::{Kind, QuerySet, Row};

pub struct Run {
    pub corpus: &'static str,
    pub fixture: bool,
    pub set: QuerySet,
    pub revision: Option<String>,
    pub files: usize,
    pub bytes: u64,
    pub stamps: Stamps,
}

pub struct Stamps {
    pub machine: String,
    pub versions: String,
    /// Why there is no tgrep row.
    pub tgrep_note: Option<String>,
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

pub fn stamps(fmgrep: Option<&Path>, tgrep: &Result<crate::tgrep::Tgrep, String>) -> Stamps {
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
    if let Ok(t) = tgrep {
        versions += &format!("; {}", t.version);
    }
    Stamps {
        machine: machine().unwrap_or_else(unknown),
        versions,
        tgrep_note: tgrep.as_ref().err().cloned(),
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

fn bytes(n: u64) -> String {
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

fn label(name: &str) -> &str {
    match name {
        "fmgrep-store-docs" => "fmgrep `--store-docs`",
        name => name,
    }
}

pub fn markdown(run: &Run, rows: &[Row], diffs: &[String]) -> String {
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
    let note = stamps
        .tgrep_note
        .as_ref()
        .map_or(String::new(), |n| format!("- {n}\n"));
    let mut md = format!(
        "## fmgrep benchmark: {}{}\n\n- corpus: {} files, {}\n- revision: {revision}\n\
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
    let tgrep = if rows.iter().any(|r| r.name == "tgrep") {
        ", or a `tgrep` client against one `tgrep serve`"
    } else {
        ""
    };
    let tgrep_and = if tgrep.is_empty() {
        ""
    } else {
        ", or several `tgrep` clients"
    };
    md += &format!(
        "\nLatency is per query, spawn to exit, of the processes printing the matching lines: \
         `rg` over a staged copy of the corpus, or `fmgrep --server` against one \
         `fmgrep serve`{tgrep}. An intersection is an `rg` pipe{tgrep_and}. \
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
            let mut cells = vec![term, q.hits.lines.to_string(), q.hits.files.to_string()];
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

pub fn trace(run: &Run, rows: &[Row]) -> String {
    let us = |d: Option<Duration>| d.map(|d| d.as_micros() as u64);
    let mut lines = Vec::new();
    for r in rows {
        for q in &r.queries {
            lines.push(json!({
                "type": "query",
                "index_name": r.name,
                "corpus": run.corpus,
                "corpus_revision": run.revision,
                "fixture": run.fixture,
                "term": q.term,
                "term_kind": Kind::of(&q.term).name(),
                "latency_us": q.latency.as_micros() as u64,
                "lines": q.hits.lines,
                "files": q.hits.files,
            }));
        }
        lines.push(json!({
            "type": "index",
            "index_name": r.name,
            "corpus": run.corpus,
            "corpus_revision": run.revision,
            "fixture": run.fixture,
            "num_files_indexed": r.files,
            "index_time_us": us(r.index_time),
            "index_size_bytes": r.index_bytes,
            "ready_us": us(r.ready),
            "index_peak_rss_bytes": r.index_peak_rss,
            "query_peak_footprint_bytes": r.footprint,
            "query_peak_rss_bytes": r.query_rss,
            "loadavg_start": r.load1.map(|l| l.0),
            "loadavg_end": r.load1.map(|l| l.1),
        }));
    }
    lines.iter().map(|l| format!("{l}\n")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Query;

    const PIN: &str = "4cf7628aa66f56000c29aef93924b6b8515db8f7";

    const TERMS: [&str; 3] = ["a", "/b/", "a AND b"];

    fn run_of(fixture: bool, revision: &str) -> (Run, Vec<Row>) {
        let note = Some("tgrep: not found on PATH".to_string());
        let (machine, versions, date) = ("m".into(), "v".into(), "d".into());
        let stamps = Stamps {
            machine,
            versions,
            tgrep_note: note,
            date,
        };
        let (mut set, revision) = (crate::query_set("ghc"), Some(revision.to_string()));
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
        let (mut rg, mut docs) = (Row::new("rg", 1), Row::new("fmgrep-store-docs", 1));
        for term in TERMS {
            let rows = if term.contains(" AND ") {
                vec![&mut rg]
            } else {
                vec![&mut rg, &mut docs]
            };
            for row in rows {
                let hits = crate::rows::Hits {
                    lines: 1,
                    files: 1,
                    digest: 0,
                };
                row.queries
                    .push(Query::new(term, Duration::from_millis(2), hits));
            }
        }
        (rg.footprint, rg.query_rss) = (Some(1 << 20), Some(2 << 20));
        (docs.index_time, docs.load1) = (Some(Duration::from_secs(2)), Some((1.0, 4.5)));
        (run, vec![rg, docs])
    }

    fn markdown_of(fixture: bool, revision: &str, diffs: &[String]) -> String {
        let (run, rows) = run_of(fixture, revision);
        markdown(&run, &rows, diffs)
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
        for row in ["rg", "fmgrep --server ADDR", "tgrep"] {
            let command = format!("`{row} {lines}");
            assert!(doc.contains(&command), "{command} not in BENCHMARKS.md");
        }
        let pipe = format!("xargs -0 -r rg {lines}");
        assert!(doc.contains(&pipe), "{pipe} not in BENCHMARKS.md");
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
        let md = markdown(&run, &rows[1..], &[]);
        assert_has(&md, &["regex queries (1)"]);
        assert!(!md.contains("intersection queries"), "{md}");

        let trace = trace(&run, &rows);
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
}
