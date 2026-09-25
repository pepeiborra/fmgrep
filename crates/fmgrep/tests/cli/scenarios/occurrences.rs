//! `--occurrences` end to end through the binary: the default
//! mode's totals, no `docs_matched`.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Output, Stdio};

use super::roundtrip::{index_dir_of, stage};
use crate::{Fmgrep, index_argv};

fn run(e: &Fmgrep, envs: &[(&str, &str)], args: &[&str]) -> Output {
    e.command()
        .args(args)
        .envs(envs.iter().copied())
        .output()
        .unwrap()
}

/// Index the staged docs at `shard_bytes`; the docs dir holder and index path.
fn indexed(e: &Fmgrep, shard_bytes: &str) -> (tempfile::TempDir, String) {
    let (docs_dir, _) = stage();
    let index = index_dir_of(docs_dir.path());
    let argv = index_argv(docs_dir.path(), &index, e.store_docs(), &[]);
    let out = run(
        e,
        &[("FMGREP_SHARD_BYTES", shard_bytes)],
        &argv.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    (docs_dir, index.display().to_string())
}

/// `[flags] -- terms` under `envs`: stdout, or the stderr of an exit-2
/// refusal.
fn query(
    e: &Fmgrep,
    envs: &[(&str, &str)],
    index: &str,
    flags: &[&str],
    terms: &[&str],
) -> Result<String, String> {
    let mut args = vec!["--index", index];
    args.extend(flags);
    args.push("--");
    args.extend(terms);
    let out = run(e, envs, &args);
    match out.status.code() {
        Some(0) => Ok(String::from_utf8(out.stdout).unwrap()),
        Some(2) => Err(String::from_utf8_lossy(&out.stderr).into_owned()),
        other => panic!("exit {other:?}: {}", String::from_utf8_lossy(&out.stderr)),
    }
}

fn report(json: &str) -> serde_json::Value {
    serde_json::from_str(json.lines().next().unwrap()).unwrap()
}

/// `(term, total_matches, counts_exact)` per query; `docs_matched` must be
/// present exactly when `docs`.
fn totals(report: &serde_json::Value, docs: bool) -> Vec<(String, u64, bool)> {
    report["queries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|q| {
            assert_eq!(q.get("docs_matched").is_some(), docs, "{q}");
            (
                q["term"].as_str().unwrap().to_string(),
                q["total_matches"].as_u64().unwrap(),
                q["counts_exact"].as_bool().unwrap_or(true),
            )
        })
        .collect()
}

/// A report with every timing zeroed.
fn timeless(mut report: serde_json::Value) -> serde_json::Value {
    report["index_time_us"] = 0.into();
    for q in report["queries"].as_array_mut().unwrap() {
        q["latency_us"] = 0.into();
    }
    report
}

const TERMS: [&str; 7] = [
    "printk",
    "hello",
    "aaaa",
    "/print[k]/",
    "/pr[i1]ntk/",
    "/a+/",
    "missing",
];

pub fn occurrences_equal_default_totals(e: &Fmgrep) {
    for at in ["16", "100MB"] {
        let (_tmp, index) = indexed(e, at);
        let want = report(&query(e, &[], &index, &["--json"], &TERMS).unwrap());
        let got = report(&query(e, &[], &index, &["--json", "--occurrences"], &TERMS).unwrap());
        assert_eq!(totals(&got, false), totals(&want, true), "{at}");
        let human = query(e, &[], &index, &["--occurrences"], &["printk", "hello"]).unwrap();
        assert_eq!(
            human, "printk: 3 occurrences\nhello: 1 occurrence\n",
            "{at}"
        );
        let lines = |flags: &[&str]| query(e, &[], &index, flags, &["printk"]).unwrap();
        assert_eq!(lines(&["--occurrences", "-c"]), lines(&["-c"]), "{at}");
    }
}

/// Killed on drop, so a failing test leaves no server behind.
struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn serve_and_server_flag_answer_occurrences(e: &Fmgrep) {
    let (_tmp, index) = indexed(e, "100MB");
    let mut stdio = e
        .command()
        .args(["serve", "--stdio", &index])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = stdio.stdin.take().unwrap();
    writeln!(stdin, r#"{{"jsonrpc":"2.0","id":1,"method":"query","params":{{"terms":["printk","/a+/"],"occurrences":true}}}}"#).unwrap();
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","id":2,"method":"query","params":{{"terms":["printk","/a+/"]}}}}"#
    )
    .unwrap();
    drop(stdin);
    let mut out = String::new();
    stdio
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut out)
        .unwrap();
    assert!(stdio.wait().unwrap().success());
    let responses: Vec<serde_json::Value> = out
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        totals(&responses[0]["result"], false),
        totals(&responses[1]["result"], true)
    );

    let mut server = Server(
        e.command()
            .args(["serve", &index])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut addr = String::new();
    BufReader::new(server.0.stdout.take().unwrap())
        .read_line(&mut addr)
        .unwrap();
    let client = run(
        e,
        &[],
        &[
            &["--server", addr.trim_end(), "--json", "--occurrences", "--"][..],
            &TERMS,
        ]
        .concat(),
    );
    assert!(
        client.status.success(),
        "{}",
        String::from_utf8_lossy(&client.stderr)
    );
    let want = query(e, &[], &index, &["--json", "--occurrences"], &TERMS).unwrap();
    assert_eq!(
        timeless(report(std::str::from_utf8(&client.stdout).unwrap())),
        timeless(report(&want))
    );
    assert!(
        run(e, &[], &["--server", addr.trim_end(), "--shutdown"])
            .status
            .success()
    );
}

/// Files source: a stale file counts its current bytes and a deleted one
/// is excluded, one warning each, exactly as the default mode has it.
pub fn stale_file_is_not_ignored_by_occurrences(e: &Fmgrep) {
    for shard_bytes in ["16", "100MB"] {
        let (docs_dir, index) = indexed(e, shard_bytes);
        std::fs::write(docs_dir.path().join("a.bin"), b"printk changed printk\n").unwrap();
        std::fs::remove_file(docs_dir.path().join("c.bin")).unwrap();
        let mut args = vec!["--index", &index, "--json", "--occurrences", "--"];
        args.extend(TERMS);
        let out = run(e, &[], &args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{shard_bytes}: {stderr}");
        assert_eq!(
            stderr.matches("warning:").count(),
            1,
            "{shard_bytes}: {stderr}"
        );
        assert!(stderr.contains("c.bin"), "{shard_bytes}: {stderr}");
        let got = report(std::str::from_utf8(&out.stdout).unwrap());
        let want = report(&query(e, &[], &index, &["--json", "--approx-counts"], &TERMS).unwrap());
        assert_eq!(totals(&got, false), totals(&want, true), "{shard_bytes}");
        assert_eq!(
            got["queries"][0]["total_matches"], 3,
            "{shard_bytes}: a.bin's current printk"
        );
        assert_eq!(
            got["queries"][2]["total_matches"], 0,
            "{shard_bytes}: c.bin's aaaa excluded"
        );
    }
}
