//! The markdown results, the JSONL trace and the machine and version stamps.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use fmgrep::regex_pattern;
use serde_json::json;

use crate::{QuerySet, Row};

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
| row | index build | index size | ready | literal p50 | literal mean | regex p50 | regex mean | query footprint | index peak RSS | load1 |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|";

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
    let regexes = run
        .set
        .queries
        .iter()
        .filter(|q| regex_pattern(q).is_some());
    let regexes = regexes.count();
    let stamps = &run.stamps;
    let note = stamps
        .tgrep_note
        .as_ref()
        .map_or(String::new(), |n| format!("- {n}\n"));
    let mut md = format!(
        "## fmgrep benchmark: {}{}\n\n- corpus: {} files, {}\n- revision: {revision}\n\
         - queries: {n} ({} literal, {regexes} regex)\n- machine: {}\n- versions: {}\n{note}\
         - date: {}\n\n{SUMMARY}\n",
        run.corpus,
        if run.fixture { " (FIXTURE)" } else { "" },
        run.files,
        bytes(run.bytes),
        n - regexes,
        stamps.machine,
        stamps.versions,
        stamps.date,
    );
    for r in rows {
        let kind = |regex: bool| -> Vec<Duration> {
            let queries = r.queries.iter();
            let of_kind = queries.filter(|q| regex_pattern(&q.term).is_some() == regex);
            of_kind.map(|q| q.latency).collect()
        };
        let (literal, regex) = (kind(false), kind(true));
        let cells = [
            label(r.name).to_string(),
            opt(r.index_time, ms),
            opt(r.index_bytes, bytes),
            opt(r.ready, ms),
            opt(p50(&literal), ms),
            opt(mean(&literal), ms),
            opt(p50(&regex), ms),
            opt(mean(&regex), ms),
            opt(r.footprint, bytes),
            opt(r.index_peak_rss, bytes),
            opt(r.load1, |(a, b)| {
                format!("{a:.2}->{b:.2}{}", if a.max(b) >= 4.0 { "!" } else { "" })
            }),
        ];
        md += &format!("| {} |\n", cells.join(" | "));
    }
    let tgrep = if rows.iter().any(|r| r.name == "tgrep") {
        ", or one `tgrep` client against one `tgrep serve`"
    } else {
        ""
    };
    md += &format!(
        "\nLatency is per query, spawn to exit: one `rg` process over a staged copy of the corpus, \
         or one `fmgrep --server` process against one `fmgrep serve`{tgrep}. Footprint is the peak \
         anonymous memory while querying. load1 is the 1-minute load average at the row's start \
         and end; `!` marks 4 or more, where timings are likely inflated.\n\n"
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
    let mut header = vec!["query".to_string(), "matches".into(), "files".into()];
    header.extend(rows.iter().map(|r| label(r.name).to_string()));
    header.extend(others.iter().map(|r| match others.len() {
        1 => "speedup".to_string(),
        _ => format!("speedup {}", label(r.name)),
    }));
    let align = "|--:".repeat(header.len() - 1);
    md += &format!(
        "\n<details><summary>Per query</summary>\n\n| {} |\n|---{align}|\n",
        header.join(" | ")
    );
    for (i, q) in base.queries.iter().enumerate() {
        let ticks = if q.term.contains('`') { "``" } else { "`" };
        let term = format!("{ticks}{}{ticks}", q.term.replace('|', "\\|"));
        let mut cells = vec![term, q.matches.to_string(), q.files.to_string()];
        let latency = |r: &Row| r.queries.get(i).map(|q| q.latency);
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
    md + "\n</details>\n"
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
                "latency_us": q.latency.as_micros() as u64,
                "total_matches": q.matches,
                "docs_matched": q.files,
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

    fn markdown_of(fixture: bool, revision: &str, diffs: &[String]) -> String {
        let note = Some("tgrep: not found on PATH".to_string());
        let (machine, versions, date) = ("m".into(), "v".into(), "d".into());
        let stamps = Stamps {
            machine,
            versions,
            tgrep_note: note,
            date,
        };
        let (set, revision) = (crate::query_set("ghc"), Some(revision.to_string()));
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
        for term in ["a", "/b/"] {
            rg.queries
                .push(Query::new(term, Duration::from_millis(2), (1, 1)));
            docs.queries
                .push(Query::new(term, Duration::from_millis(2), (1, 1)));
        }
        rg.footprint = Some(1 << 20);
        (docs.index_time, docs.load1) = (Some(Duration::from_secs(2)), Some((1.0, 4.5)));
        markdown(&run, &[rg, docs], diffs)
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
    fn summary_table_has_the_benchmarks_md_columns() {
        assert!(include_str!("../../../BENCHMARKS.md").contains(SUMMARY));
        let md = markdown_of(false, PIN, &[]);
        assert_has(
            &md,
            &[
                "\n| rg | - | - | - | 2.00 ms | 2.00 ms | 2.00 ms | 2.00 ms | 1.00 MB | - | - |\n",
                "\n| fmgrep `--store-docs` | 2.00 s | - |",
                "| 1.00->4.50! |\n",
                "\n- tgrep: not found on PATH\n",
                "verify: OK, 2 rows agree on 35 queries",
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
}
