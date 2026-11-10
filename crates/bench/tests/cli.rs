//! The bench binary end to end: corpus refusals, a fixture run and a run in
//! place.

use std::path::Path;
use std::process::{Command, Output};

fn bench(cwd: &Path, env: &[(&str, &Path)], args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_bench"));
    cmd.current_dir(cwd).args(args).env("FMGREP_JOURNAL", "off");
    for var in [
        "FMGREP_CORPUS_GHC",
        "FMGREP_CORPUS_LINUX",
        "FMGREP_CORPUS_CHROMIUM",
        "FMGREP_CORPUS_AOSP",
        "FMGREP_BENCH_TGREP_BIN",
        "FMGREP_BENCH_ELGREP_BIN",
    ] {
        cmd.env_remove(var);
    }
    // Without tgrep or eg, so no machine-dependent note reaches stderr.
    let path = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::split_paths(&path)
        .filter(|d| !d.join("tgrep").exists() && !d.join("eg").exists());
    cmd.env("PATH", std::env::join_paths(path).unwrap());
    for (var, val) in env {
        cmd.env(var, val);
    }
    cmd.output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn assert_refused(out: &Output, fragments: &[impl AsRef<str>]) {
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(2), "{stdout}\n{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    for f in fragments.iter().map(AsRef::as_ref) {
        assert!(stderr.contains(f), "{f:?} not in {stderr}");
    }
}

fn clone_hint(id: &str) -> [String; 2] {
    let var = format!("FMGREP_CORPUS_{}=DIR", id.to_uppercase());
    [
        format!("scripts/bench.sh --corpus {id} [--repo-path DIR]"),
        var,
    ]
}

#[test]
fn missing_corpus_exits_2_naming_both_ways() {
    let dir = tempfile::tempdir().unwrap();
    let absent = dir.path().join("absent");
    let out = bench(dir.path(), &[("FMGREP_CORPUS_GHC", &absent)], &[]);
    assert_refused(&out, &clone_hint("ghc"));
    let default_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/bench");
    if !default_root.join("chromium").exists() {
        let out = bench(dir.path(), &[], &["--corpus", "chromium"]);
        assert_refused(&out, &clone_hint("chromium"));
    }
    if !default_root.join("aosp").exists() {
        let out = bench(dir.path(), &[], &["--corpus", "aosp"]);
        assert_refused(&out, &["--repo-path DIR", "FMGREP_CORPUS_AOSP=DIR"]);
        assert!(!text(&out.stderr).contains("to clone"));
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
        md.contains(&format!("verify: OK, {agree} on 40 queries")),
        "{md}"
    );
    let searched = rows.split(',').map(|r| format!("{r} 1"));
    let searched = searched.collect::<Vec<_>>().join(", ");
    let searched = format!("\n- files searched: {searched} (non-empty)\n");
    assert!(md.contains(&searched), "{md}");
    let trace = std::fs::read_to_string(dir.path().join("t.jsonl")).unwrap();
    let lines: Vec<serde_json::Value> = trace
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), n * 41, "{trace}");
    assert!(lines.iter().all(|v| v["fixture"] == true), "{trace}");
    for v in lines
        .iter()
        .filter(|v| v["type"] == "query" && !v["term"].as_str().unwrap().starts_with('/'))
    {
        let (lines, files) = (v["lines"].as_u64(), v["files"].as_u64());
        assert!(lines >= files && files > Some(0), "{v}");
    }
    let queries = lines.iter().filter(|v| v["type"] == "query");
    let mut kinds = std::collections::BTreeMap::new();
    for v in queries {
        let (term, kind) = (v["term"].as_str().unwrap(), v["term_kind"].as_str());
        let regex = term.starts_with('/') && !term.contains(" AND ");
        assert_eq!(kind == Some("regex"), regex, "{v}");
        *kinds.entry(kind.unwrap()).or_insert(0) += 1;
    }
    let per_kind = [
        ("intersection", 5 * n),
        ("literal", 30 * n),
        ("regex", 5 * n),
    ];
    assert_eq!(kinds, per_kind.into(), "{trace}");
}

/// Every file under `root` but `.git`, with its modification time.
fn snapshot(root: &Path) -> Vec<(std::path::PathBuf, std::time::SystemTime)> {
    let mut all = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() && !path.ends_with(".git") {
                stack.push(path.clone());
            }
            all.push((path, meta.modified().unwrap()));
        }
    }
    all.sort();
    all
}

#[test]
fn in_place_run_lists_files_only_some_rows_searched() {
    let fmgrep = Path::new(env!("CARGO_BIN_EXE_bench")).with_file_name("fmgrep");
    let tools = ["rg", "git"].map(|t| Command::new(t).arg("--version").output().is_ok());
    if !fmgrep.is_file() || tools.contains(&false) {
        eprintln!("skipped: needs fmgrep, rg and git");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("tree");
    std::fs::create_dir(&root).unwrap();
    let git = Command::new("git")
        .arg("init")
        .arg("-q")
        .arg(&root)
        .status();
    assert!(git.unwrap().success());
    let queries: serde_json::Value =
        serde_json::from_str(include_str!("../queries/ghc.json")).unwrap();
    let literals = queries["queries"].as_array().unwrap().iter();
    let literals = literals
        .filter_map(|q| q.as_str())
        .filter(|q| !q.starts_with('/'));
    let lines: String = literals
        .flat_map(|q| q.split(" AND "))
        .map(|l| format!("{l}\n"))
        .collect();
    for name in ["a.txt", "ign.txt", ".w.txt"] {
        std::fs::write(root.join(name), &lines).unwrap();
    }
    std::fs::write(root.join("p.txt"), format!("%PDF-\n{lines}")).unwrap();
    std::fs::write(root.join(".gitignore"), "ign.txt\n!.w.txt\n").unwrap();
    std::os::unix::fs::symlink("a.txt", root.join("link.txt")).unwrap();
    let before = snapshot(&root);

    let out = bench(
        dir.path(),
        &[("FMGREP_CORPUS_GHC", &root)],
        &["--rows", "rg,fmgrep", "--trace", "t.jsonl"],
    );
    let md = text(&out.stdout);
    assert!(out.status.success(), "{md}\n{}", text(&out.stderr));
    assert_eq!(snapshot(&root), before);
    let wants = [
        "\n- files searched: rg 3, fmgrep 2 (non-empty)\n",
        "verify: OK, 2 rows agree on 40 queries\n",
        "queries print lines from files some rows did not search (not failures):",
        ": fmgrep did not search binary 1 (`p.txt`)\n",
    ];
    for want in wants {
        assert!(md.contains(want), "{want:?} not in\n{md}");
    }
}

/// An `eg` wrapping rg that prints nothing for `HscEnv`, beside an
/// `eg-indexd` that only signals readiness.
fn stand_in_eg(dir: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let eg = r#"#!/bin/bash
args=()
for a in "$@"; do
  case $a in
    --version) echo 'elgrep (eg) 0.0 (stand-in)'; exit 0 ;;
    --bench) echo '{"ok":true,"cold_build":true,"timings_ms":{"cold_build_total":1}}'; exit 0 ;;
    --index-dir=*|--debug) ;;
    *) args+=("$a") ;;
  esac
done
[[ " ${args[*]} " == *" -e HscEnv "* ]] && exit 1
exec rg "${args[@]}"
"#;
    let daemon = "#!/bin/sh\nmkdir -p \"$2\"\necho $$ >\"$2/daemon.lock\"\n\
                  echo $$ >\"$2/startup-ready\"\nexec sleep 600\n";
    for (name, script) in [("eg", eg), ("eg-indexd", daemon)] {
        let bin = dir.join(name);
        std::fs::write(&bin, script).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    dir.join("eg")
}

#[test]
fn third_party_row_dropping_a_file_is_a_footnote_not_a_failure() {
    let fmgrep = Path::new(env!("CARGO_BIN_EXE_bench")).with_file_name("fmgrep");
    if !fmgrep.is_file() || Command::new("rg").arg("--version").output().is_err() {
        eprintln!("skipped: needs fmgrep and rg");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let eg = stand_in_eg(dir.path());
    let out = bench(
        dir.path(),
        &[("FMGREP_BENCH_ELGREP_BIN", &eg)],
        &[
            "--fixture",
            "--rows",
            "rg,fmgrep,elgrep",
            "--trace",
            "t.jsonl",
        ],
    );
    let (md, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(out.status.success(), "{md}\n{stderr}");
    let wants = [
        "\n- elgrep: 1 of 40 queries differs from rg: `HscEnv`, 1 line missed in 1 file (`ghc.txt`)\n",
        "\nverify: OK, fmgrep rows agree with rg on 40 queries; 1 third-party difference (see notes)\n",
    ];
    for want in wants {
        assert!(md.contains(want), "{want:?} not in\n{md}");
    }
    assert!(
        stderr.ends_with("verify: OK (1 third-party difference)\n"),
        "{stderr}"
    );
    let trace = std::fs::read_to_string(dir.path().join("t.jsonl")).unwrap();
    let differs: Vec<(String, String)> = trace
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .filter(|v| v["differs"].as_array().is_some_and(|d| !d.is_empty()))
        .map(|v| (v["index_name"].to_string(), v["differs"].to_string()))
        .collect();
    let want = r#"[{"against":"rg","file":"ghc.txt","lines":0,"want":1}]"#;
    assert_eq!(differs, [(r#""elgrep""#.to_string(), want.to_string())]);
}
