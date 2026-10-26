//! The bench binary end to end: corpus refusals and a fixture run.

use std::path::Path;
use std::process::{Command, Output};

fn bench(cwd: &Path, env: &[(&str, &Path)], args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_bench"));
    cmd.current_dir(cwd).args(args);
    for var in [
        "FMGREP_CORPUS_GHC",
        "FMGREP_CORPUS_LINUX",
        "FMGREP_CORPUS_CHROMIUM",
    ] {
        cmd.env_remove(var);
    }
    for (var, val) in env {
        cmd.env(var, val);
    }
    cmd.output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn assert_refused(out: &Output, id: &str) {
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(2), "{stdout}\n{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(
        stderr.contains(&format!("scripts/bench.sh --corpus {id} [--repo-path DIR]")),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!("FMGREP_CORPUS_{}=DIR", id.to_uppercase())),
        "{stderr}"
    );
}

#[test]
fn missing_corpus_exits_2_naming_both_ways() {
    let dir = tempfile::tempdir().unwrap();
    let absent = dir.path().join("absent");
    assert_refused(
        &bench(dir.path(), &[("FMGREP_CORPUS_GHC", &absent)], &[]),
        "ghc",
    );
    let default_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/bench/chromium");
    if !default_root.exists() {
        assert_refused(
            &bench(dir.path(), &[], &["--corpus", "chromium"]),
            "chromium",
        );
    }
}

#[test]
fn empty_corpus_exits_2() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("blob.bin"), b"a\x00b").unwrap();
    let out = bench(dir.path(), &[("FMGREP_CORPUS_GHC", dir.path())], &[]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("no indexable files"));
    assert!(out.stdout.is_empty());
}

#[test]
fn fixture_run_agrees_and_writes_markdown_and_trace() {
    if Command::new("rg").arg("--version").output().is_err() {
        eprintln!("skipped: rg is not on PATH");
        return;
    }
    let fmgrep = Path::new(env!("CARGO_BIN_EXE_bench")).with_file_name("fmgrep");
    let rows = if fmgrep.is_file() { "rg,fmgrep" } else { "rg" };
    let dir = tempfile::tempdir().unwrap();
    let out = bench(
        dir.path(),
        &[],
        &["--fixture", "--rows", rows, "--trace", "t.jsonl"],
    );
    let md = text(&out.stdout);
    assert!(out.status.success(), "{md}\n{}", text(&out.stderr));
    assert!(
        md.starts_with("## fmgrep benchmark: ghc (FIXTURE)\n"),
        "{md}"
    );
    for row in rows.split(',') {
        assert!(md.contains(&format!("\n| {row} | ")), "{md}");
    }
    let n = rows.split(',').count();
    let agree = if n == 1 {
        "1 row agrees".into()
    } else {
        format!("{n} rows agree")
    };
    assert!(
        md.contains(&format!("verify: OK, {agree} on 35 queries")),
        "{md}"
    );
    let trace = std::fs::read_to_string(dir.path().join("t.jsonl")).unwrap();
    let lines: Vec<serde_json::Value> = trace
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), n * 36, "{trace}");
    assert!(lines.iter().all(|v| v["fixture"] == true), "{trace}");
    for v in lines
        .iter()
        .filter(|v| v["type"] == "query" && !v["term"].as_str().unwrap().starts_with('/'))
    {
        assert!(v["total_matches"].as_u64() > Some(0), "{v}");
    }
}
