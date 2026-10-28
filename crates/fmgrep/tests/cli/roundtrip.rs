use std::path::Path;

use crate::*;

#[test]
fn human_output_is_readable() {
    let (tmp, docs) = stage();
    let index_dir = tmp.path().join("idx");
    let out = run(&index_argv(&docs, &index_dir, false, &[]));
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("indexed 3 files"));
    let index = index_dir.display().to_string();
    let out = run(&["--index", &index, "printk", "missing"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.starts_with("== printk ==\na.bin\n1:printk hello printk\n"),
        "{stdout}"
    );
    assert!(stdout.ends_with("\n== missing ==\n"), "{stdout}");
    let out = run(&["--index", &index, "missing"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    let out = run(&["--index", &index, "-c", "printk"]);
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a.bin:1\nb.bin:1\n");
}

#[test]
fn index_refuses_to_overwrite_and_reports_argv_errors() {
    let (_tmp, docs) = stage();
    let index_dir = do_index(&docs, false);
    let out = run(&index_argv(&docs, &index_dir, false, &[]));
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("already exists"), "{}", stderr(&out));
    let out = run(&["index", &docs.display().to_string(), "--bogus"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).contains("Usage: fmgrep index"),
        "{}",
        stderr(&out)
    );
}

/// The build window and thread counts are a schedule, not a format: the
/// persisted bytes and the counts must not move with them.
#[test]
fn shards_in_flight_do_not_change_the_index() {
    for store_docs in [false, true] {
        let (tmp, docs) = stage();
        let mut cells = Vec::new();
        // 20-byte shards take 160 bytes each to build: windows 1 and 2.
        for (budget, pool) in [("160", "3"), ("640", "2")] {
            let index_dir = tmp.path().join(format!("idx-{budget}"));
            let out = fmgrep()
                .args(index_argv(&docs, &index_dir, store_docs, &[]))
                .env("FMGREP_SHARD_BYTES", "20")
                .env("FMGREP_BUILD_BYTES", budget)
                .env("RAYON_NUM_THREADS", pool)
                .output()
                .expect("spawn");
            assert!(out.status.success(), "index: {}", stderr(&out));
            let manifest: serde_json::Value =
                serde_json::from_slice(&std::fs::read(index_dir.join("manifest.json")).unwrap())
                    .unwrap();
            assert!(
                manifest["blobs"].as_array().unwrap().len() > 2,
                "{store_docs} budget={budget}"
            );
            let query = run(&query_argv(
                &index_dir,
                false,
                &["printk", "aaaa", "missing"],
            ));
            let mut files: Vec<_> = std::fs::read_dir(&index_dir)
                .unwrap()
                .map(|e| e.unwrap().path())
                .filter(|p| !p.ends_with("manifest.json"))
                .map(|p| {
                    (
                        p.file_name().unwrap().to_owned(),
                        std::fs::read(&p).unwrap(),
                    )
                })
                .collect();
            files.sort();
            cells.push((
                files,
                manifest["blobs"].clone(),
                manifest["blob_lens"].clone(),
                manifest["index_size_bytes"].clone(),
                counts(&query.stdout),
            ));
        }
        assert_eq!(cells[0], cells[1], "{store_docs}");
    }
}

/// Files source: a deleted file is excluded from every term, with one
/// warning per process.
#[test]
fn missing_file_is_warned_once_and_excluded() {
    let (_tmp, docs) = stage();
    let index_dir = do_index(&docs, false);
    std::fs::remove_file(docs.join("a.bin")).unwrap();
    for (regex, terms) in [(false, ["printk", "hello"]), (true, ["pr[i1]ntk", "h.llo"])] {
        let out = run(&query_argv(&index_dir, regex, &terms));
        let stderr = stderr(&out);
        assert!(out.status.success(), "{stderr}");
        assert_eq!(stderr.matches("warning:").count(), 1, "{stderr}");
        assert!(
            stderr.contains("warning: excluding") && stderr.contains("a.bin"),
            "{stderr}"
        );
        assert_eq!(counts(&out.stdout), [(1, 1), (0, 0)], "{terms:?}");
    }
}

/// `index` with no `--output` writes `.fmgrep`, which a search finds from
/// any directory below, searching that directory's subtree as rg does.
#[test]
fn index_defaults_to_a_dotdir_found_from_subdirectories() {
    for store_docs in [false, true] {
        let (_tmp, root) = stage();
        let sub = root.join("sub/deeper");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("d.bin"), "printk deeper\n").unwrap();
        let index: &[&str] = if store_docs {
            &["index", "--store-docs", "--json"]
        } else {
            &["index", "--json"]
        };
        let out = run_in(&root, index);
        assert!(out.status.success(), "{}", stderr(&out));
        let summary: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert!(
            summary["path"].as_str().unwrap().ends_with(".fmgrep"),
            "{summary}"
        );
        assert_eq!(summary["num_files"], 4, "{store_docs}");
        let dotdir = root.join(".fmgrep").display().to_string();
        let want = run(&["--index", &dotdir, "-l", "printk"]);
        assert!(
            want.status.success() && !want.stdout.is_empty(),
            "{store_docs}"
        );
        let root_arg = root.display().to_string();
        for (dir, args, want) in [
            (sub.as_path(), &["-l", "printk"][..], &b"d.bin\n"[..]),
            (&root, &["-l", "printk"], &want.stdout),
            (
                Path::new("/"),
                &["--index", &root_arg, "-l", "printk"],
                &want.stdout,
            ),
        ] {
            let got = run_in(dir, args);
            let err = stderr(&got);
            assert_eq!(
                (got.status.code(), got.stdout),
                (Some(0), want.to_vec()),
                "{store_docs} from {}: {err}",
                dir.display(),
            );
        }
        let out = run(&query_argv(&sub, false, &["printk"]));
        assert_eq!(counts(&out.stdout), [(4, 3)], "{store_docs}");
    }
}

/// With no index at or above the cwd a search is refused, even for the word
/// `index` after `--`.
#[test]
fn no_index_above_the_cwd_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let canon = std::fs::canonicalize(dir.path()).unwrap();
    if canon.ancestors().any(|a| a.join(".fmgrep").is_dir()) {
        return;
    }
    for args in [&["printk"][..], &["--", "index"]] {
        let out = run_in(dir.path(), args);
        let stderr = stderr(&out);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(stderr.contains("no .fmgrep index in"), "{args:?}: {stderr}");
        assert!(!dir.path().join(".fmgrep").exists());
    }
}

/// A `.fmgrep` anywhere but directly under the indexed folder is refused:
/// a search would read the wrong tree through it.
#[test]
fn dotdir_output_outside_the_folder_is_refused() {
    let (tmp, docs) = stage();
    std::fs::create_dir(tmp.path().join("other")).unwrap();
    let foreign = tmp.path().join("other/.fmgrep");
    let out = run(&index_argv(&docs, &foreign, false, &[]));
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("outside"), "{}", stderr(&out));
    assert!(!foreign.exists());
    let out = run(&index_argv(&docs, &docs.join(".fmgrep"), false, &[]));
    assert!(out.status.success(), "{}", stderr(&out));
}

/// Files source: where a pattern's anchor is common, a document holding the
/// anchor but no match is never read.
#[test]
fn a_common_anchor_reads_only_matching_docs() {
    use std::os::unix::fs::PermissionsExt;
    const PATTERN: &str = "union[ \\t]+\\{";
    // fmgrep's `MIN_ANCHOR_COUNT`: from this many anchors a shard walks.
    const WALKS: usize = 256;
    let stage = |copies: usize| {
        let common = b"union x {\n".repeat(copies);
        let (tmp, docs) =
            stage_files(&[("a.c", &common), ("b.c", b"union {\n"), ("c.c", b"none\n")]);
        let index_dir = do_index(&docs, false);
        let unread = std::fs::Permissions::from_mode(0o000);
        std::fs::set_permissions(docs.join("a.c"), unread).unwrap();
        (tmp, index_dir)
    };
    let (_tmp, index_dir) = stage(WALKS);
    let out = run(&query_argv(&index_dir, true, &[PATTERN]));
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(counts(&out.stdout), [(1, 1)]);
    let (index, regex) = (index_dir.display().to_string(), format!("/{PATTERN}/"));
    let out = run(&["--index", &index, "-c", &regex]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "b.c:1\n");
    assert!(!stderr(&out).contains("excluding"));
    // Control arm: the literal prune reads `a.c`, which is excluded.
    let (_tmp, index_dir) = stage(8);
    let out = run(&query_argv(&index_dir, true, &[PATTERN]));
    let stderr = stderr(&out);
    assert!(
        stderr.contains("warning: excluding") && stderr.contains("a.c"),
        "{stderr}"
    );
}

#[test]
fn tampered_blob_is_refused_loudly() {
    let (_tmp, docs) = stage();
    let index_dir = do_index(&docs, false);
    let blobs = index_dir.join("blobs.shard");
    let mut bytes = std::fs::read(&blobs).unwrap();
    bytes[..5].copy_from_slice(b"XXXXX");
    std::fs::write(&blobs, &bytes).unwrap();
    let out = run(&query_argv(&index_dir, false, &["printk"]));
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty(), "no report on failure");
    assert!(
        stderr(&out).contains("corrupt docfm blob"),
        "{}",
        stderr(&out)
    );
}

/// A build that fails after the output dir exists removes it: nothing
/// half-written survives, and a retry does not hit "already exists".
#[test]
fn failed_build_removes_the_output() {
    let (tmp, docs) = stage();
    let index_dir = tmp.path().join("idx");
    for (var, raw) in [
        ("FMGREP_SHARD_BYTES", "nope"),
        ("FMGREP_SAMPLE_RATE", "nope"),
        ("FMGREP_SAMPLE_RATE", "0"),
        ("FMGREP_SAMPLE_RATE", "4294967296"),
        ("FMGREP_JOURNAL", "nope"),
        ("FMGREP_JOURNAL", "!off"),
    ] {
        let out = fmgrep()
            .args(index_argv(&docs, &index_dir, false, &[]))
            .env(var, raw)
            .output()
            .expect("spawn");
        assert_eq!(out.status.code(), Some(2), "{var}={raw}");
        assert!(stderr(&out).contains(var), "{}", stderr(&out));
        if var == "FMGREP_JOURNAL" {
            assert!(stderr(&out).contains("off, on"), "{}", stderr(&out));
        }
        assert!(!index_dir.exists(), "a failed index must persist nothing");
    }
}

/// The sample rate trades blob bytes for locate steps: nothing else on disk,
/// and no answer, moves with it.
#[test]
fn the_sample_rate_changes_the_blobs_not_the_answers() {
    for store_docs in [false, true] {
        let (tmp, docs) = stage();
        let mut cells = Vec::new();
        for rate in ["", "16", "1", "5", "32"] {
            let index_dir = tmp.path().join(format!("idx-{rate}"));
            let out = fmgrep()
                .args(index_argv(&docs, &index_dir, store_docs, &[]))
                .env("FMGREP_SAMPLE_RATE", rate)
                .output()
                .expect("spawn");
            assert!(out.status.success(), "index: {}", stderr(&out));
            let mut manifest: serde_json::Value =
                serde_json::from_slice(&std::fs::read(index_dir.join("manifest.json")).unwrap())
                    .unwrap();
            let blobs = std::fs::read(index_dir.join("blobs.shard")).unwrap();
            let blob_lens = manifest["blob_lens"].take();
            manifest["index_size_bytes"].take();
            manifest["build_time_us"].take();
            let mut files: Vec<_> = std::fs::read_dir(&index_dir)
                .unwrap()
                .map(|e| e.unwrap().path())
                .filter(|p| !p.ends_with("manifest.json") && !p.ends_with("blobs.shard"))
                .map(|p| (p.file_name().unwrap().to_owned(), std::fs::read(p).unwrap()))
                .collect();
            files.sort();
            let index = index_dir.display().to_string();
            let answers: Vec<_> = [
                &["printk", "aaaa", "missing"][..],
                &["/pr.n[a-z]k/", "/n.n-/"],
                &["-c", "printk"],
                &["--and", "printk", "hello"],
            ]
            .iter()
            .map(|args| run(&[&["--index", &index][..], args].concat()))
            .map(|out| (out.status.code(), out.stdout))
            .collect();
            cells.push(((files, manifest, answers), (blobs, blob_lens)));
        }
        for cell in &cells {
            assert_eq!(cell.0, cells[0].0, "{store_docs}");
        }
        assert_eq!(cells[0].1, cells[1].1, "{store_docs}");
        assert_ne!(cells[0].1.0, cells[2].1.0, "{store_docs}");
        assert_ne!(cells[0].1.0, cells[3].1.0, "{store_docs}");
        assert_ne!(cells[0].1.0, cells[4].1.0, "{store_docs}");
    }
}

#[test]
fn paths_resolve_from_the_cwd() {
    let (tmp, docs) = stage_files(&[("a.c", b"printk a\n"), ("b.h", b"printk b\n")]);
    std::fs::create_dir(docs.join("sub")).unwrap();
    std::fs::write(docs.join("sub/c.c"), "printk c\n").unwrap();
    std::fs::write(docs.join("sub/d.h"), "printk d\n").unwrap();
    assert!(run_in(&docs, &["index"]).status.success());
    let sub = docs.join("sub");
    let lists = |args: &[&str]| {
        let out = run_in(&sub, &[&["-l"][..], args].concat());
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    };
    assert_eq!(lists(&["printk", "."]), (Some(0), "./c.c\n./d.h\n".into()));
    assert_eq!(
        lists(&["printk", "../a.c", "."]),
        (Some(0), "../a.c\n./c.c\n./d.h\n".into())
    );
    assert_eq!(lists(&["-g", "*.c", "printk"]), (Some(0), "c.c\n".into()));
    assert_eq!(
        lists(&["-g", "*.c", "printk", ".."]),
        (Some(0), "../a.c\n../sub/c.c\n".into())
    );
    let out = run_in(&sub, &["-g", "*.zz", "printk"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).starts_with("error: no files were searched"),
        "{}",
        stderr(&out)
    );
    let out = run_in(&sub, &["--expr", "printk", tmp.path().to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).contains("is outside the indexed tree"),
        "{}",
        stderr(&out)
    );
}

/// Each expectation is rg 15.2.0's from the same directory.
#[test]
fn no_path_searches_the_cwd_subtree_as_rg() {
    let (_tmp, docs) = stage_files(&[("a.c", b"printk a\n"), ("b.h", b"printk b\n")]);
    std::fs::create_dir(docs.join("sub")).unwrap();
    std::fs::write(docs.join("sub/c.c"), "printk c\n").unwrap();
    std::fs::write(docs.join("sub/d.h"), "printk d\n").unwrap();
    assert!(run_in(&docs, &["index"]).status.success());
    let sub = docs.join("sub");
    let found = |dir: &Path, args: &[&str]| {
        let out = run_in(dir, args);
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    };
    let ok = |s: &str| (Some(0), s.to_owned());
    assert_eq!(found(&sub, &["-l", "printk"]), ok("c.c\nd.h\n"));
    assert_eq!(
        found(&sub, &["printk"]),
        ok("c.c\n1:printk c\n\nd.h\n1:printk d\n")
    );
    assert_eq!(found(&sub, &["-c", "printk"]), ok("c.c:1\nd.h:1\n"));
    assert_eq!(found(&sub, &["-l", "-g", "/c.c", "printk"]), ok("c.c\n"));
    assert_eq!(found(&sub, &["-l", "-g", "!*.c", "printk"]), ok("d.h\n"));
    let json = found(&sub, &["--json", "--matches", "printk"]).1;
    let begin: serde_json::Value = serde_json::from_str(json.lines().next().unwrap()).unwrap();
    assert_eq!(begin["data"]["path"]["text"], "c.c", "{json}");
    let empty = docs.join("empty");
    std::fs::create_dir(&empty).unwrap();
    for (dir, args, why) in [
        (
            &sub,
            &["-l", "-g", "sub/*", "printk"][..],
            "the globs exclude every file",
        ),
        (&empty, &["printk"], "no indexed file is below the cwd"),
    ] {
        let out = run_in(dir, args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert_eq!(
            stderr(&out),
            format!("error: no files were searched: {why}\n"),
            "{args:?}"
        );
    }
}

/// Each expectation is rg 15.2.0's from the same directory.
#[test]
fn paths_print_and_glob_as_rg_from_a_subdirectory() {
    let (_tmp, docs) = stage_files(&[("a.c", b"printk a\n"), ("b.h", b"printk b\n")]);
    std::fs::create_dir(docs.join("sub")).unwrap();
    std::fs::write(docs.join("sub/c.c"), "printk c\n").unwrap();
    std::fs::write(docs.join("sub/d.h"), "printk d\n").unwrap();
    assert!(run_in(&docs, &["index"]).status.success());
    let sub = std::fs::canonicalize(docs.join("sub")).unwrap();
    let found = |args: &[&str]| {
        let out = run_in(&sub, args);
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    };
    let ok = |s: &str| (Some(0), s.to_owned());
    assert_eq!(found(&["-l", "-g", "/c.c", "printk", "."]), ok("./c.c\n"));
    assert_eq!(
        found(&["-l", "-g", "sub/*", "printk", ".."]),
        (Some(1), String::new())
    );
    assert_eq!(
        found(&["-l", "-g", "!sub", "printk", ".."]),
        ok("../a.c\n../b.h\n")
    );
    assert_eq!(
        found(&["-l", "-g", "!sub", "printk", "..", "."]),
        ok("../a.c\n../b.h\n./c.c\n./d.h\n")
    );
    assert_eq!(
        found(&["-l", "-g", "!*.c", "printk c", "..", "c.c"]),
        ok("c.c\n")
    );
    assert_eq!(found(&["-N", "printk", "c.c"]), ok("printk c\n"));
    assert_eq!(found(&["-c", "printk", "../a.c"]), ok("1\n"));
    let abs = sub.to_str().unwrap();
    assert_eq!(
        found(&["-l", "-g", "/c.c", "printk", abs]),
        ok(&format!("{abs}/c.c\n"))
    );
}

/// `FMGREP_VERBOSE=2` prints the plan on stderr and changes nothing on stdout.
#[test]
fn verbose_prints_the_plan_on_stderr_and_leaves_stdout_alone() {
    let common = "common ".repeat(20) + "zonkA\n";
    let (_tmp, docs) = stage_files(&[
        ("a.c", common.as_bytes()),
        ("b.c", b"common\n"),
        ("c.c", b"zonkB\n"),
    ]);
    std::fs::create_dir(docs.join("sub")).unwrap();
    std::fs::write(docs.join("sub/d.c"), "common\n").unwrap();
    assert!(run_in(&docs, &["index"]).status.success());
    let search = |dir: &Path, args: &[&str], verbose: bool| {
        let mut cmd = fmgrep();
        cmd.current_dir(dir).args(args).env_remove("FMGREP_VERBOSE");
        if verbose {
            cmd.env("FMGREP_VERBOSE", "2");
        }
        let out = cmd.output().expect("spawn fmgrep");
        assert_eq!(out.status.code(), Some(0), "{args:?}: {}", stderr(&out));
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr(&out),
        )
    };
    let expr = ["-l", "--expr", r"common AND /zonk\w+/"];
    for (dir, args, stdout, said) in [
        (
            docs.clone(),
            &expr[..],
            "a.c\n",
            &[
                "plan: common AND /zonk\\w+/\nleaf 0 common ",
                "demoted by the 8x fallback ratio: 22 > 8 x 2 of leaf 1 (a regex reference)",
                "\nroot: yes 0, maybe 2 (stale 0); -l reads 2\n",
            ][..],
        ),
        (
            docs.join("sub"),
            &["-l", "common"],
            "d.c\n",
            &["plan:\nterm 0 common  read 1 file (7 bytes) instead of locating"],
        ),
    ] {
        let plain = search(&dir, args, false);
        assert_eq!(plain, (stdout.to_owned(), String::new()), "{args:?}");
        let (out, err) = search(&dir, args, true);
        assert_eq!(out, stdout, "{args:?}");
        for s in said {
            assert!(err.contains(s), "{args:?}: {err}");
        }
    }
}
