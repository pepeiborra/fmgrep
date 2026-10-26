//! What only the binary shows: exit codes, stderr, the environment, the cwd.

use std::ffi::OsStr;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

mod roundtrip;
mod serve;

fn fmgrep() -> Command {
    Command::new(env!("CARGO_BIN_EXE_fmgrep"))
}

fn run<S: AsRef<OsStr>>(args: &[S]) -> Output {
    fmgrep().args(args).output().expect("spawn fmgrep")
}

fn run_in(dir: &Path, args: &[&str]) -> Output {
    fmgrep()
        .current_dir(dir)
        .args(args)
        .output()
        .expect("spawn fmgrep")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// `b.bin` is non-UTF8; its high bytes stay control-free so the binary
/// filter keeps it.
const DOCS: [(&str, &[u8]); 3] = [
    ("a.bin", b"printk hello printk\n"),
    ("b.bin", b"\x80\x81 non-utf8 \xe9 blob printk"),
    ("c.bin", b"aaaa nothing here"),
];

/// `files` in `<tmp>/docs`.
fn stage_files(files: &[(&str, &[u8])]) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let docs = tmp.path().join("docs");
    std::fs::create_dir(&docs).unwrap();
    for (name, bytes) in files {
        std::fs::write(docs.join(name), bytes).unwrap();
    }
    (tmp, docs)
}

fn stage() -> (tempfile::TempDir, PathBuf) {
    stage_files(&DOCS)
}

/// `index <folder> --output <index> [--store-docs] <extra...>`.
fn index_argv(folder: &Path, index: &Path, store_docs: bool, extra: &[&str]) -> Vec<String> {
    let mut argv = vec![
        "index".to_string(),
        folder.display().to_string(),
        "--output".into(),
        index.display().to_string(),
    ];
    if store_docs {
        argv.push("--store-docs".into());
    }
    argv.extend(extra.iter().map(|s| s.to_string()));
    argv
}

/// `docs` indexed into its sibling `idx`.
fn do_index(docs: &Path, store_docs: bool) -> PathBuf {
    let index = docs.with_file_name("idx");
    let out = run(&index_argv(docs, &index, store_docs, &[]));
    assert!(out.status.success(), "index: {}", stderr(&out));
    index
}

/// `--index <index> --json [--regex] -- <terms>`.
fn query_argv(index: &Path, regex: bool, terms: &[&str]) -> Vec<String> {
    let mut argv = vec![
        "--index".to_string(),
        index.display().to_string(),
        "--json".into(),
    ];
    if regex {
        argv.push("--regex".into());
    }
    argv.push("--".into());
    argv.extend(terms.iter().map(|t| t.to_string()));
    argv
}

/// `(total_matches, docs_matched)` per term of a `--json` report.
fn counts(stdout: &[u8]) -> Vec<(u64, u64)> {
    let report: serde_json::Value = serde_json::from_slice(stdout).expect("report JSON");
    let queries = report["queries"].as_array().expect("queries");
    queries
        .iter()
        .map(|q| {
            let n = |k: &str| q[k].as_u64().unwrap();
            (n("total_matches"), n("docs_matched"))
        })
        .collect()
}

/// Killed on drop, so a failing test leaves no server behind.
struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Server {
    fn spawn(args: &[&str]) -> Self {
        Server(
            fmgrep()
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn serve"),
        )
    }

    /// The readiness line: the bound address.
    fn addr(&mut self) -> String {
        let mut addr = String::new();
        BufReader::new(self.0.stdout.take().unwrap())
            .read_line(&mut addr)
            .unwrap();
        addr.trim_end().to_owned()
    }

    fn exits_cleanly(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                assert!(status.success(), "serve exited {status}");
                return;
            }
            assert!(
                Instant::now() < deadline,
                "serve did not exit after shutdown"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
