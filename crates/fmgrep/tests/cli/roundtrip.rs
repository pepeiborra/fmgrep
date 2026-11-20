use std::path::Path;
use std::time::{Duration, Instant};

use crate::*;

#[test]
fn human_output_is_readable() {
    let (tmp, docs) = stage();
    let index_dir = tmp.path().join("idx");
    let out = run(&index_argv(&docs, &index_dir, false, &[]));
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("indexed 3 files"));
    let index = index_dir.display().to_string();
    let out = run(&["--index", &index, "printk"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.starts_with("a.bin:printk hello printk\n"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("=="),
        "one pattern prints no banner: {stdout}"
    );
    let out = run(&["--index", &index, "missing"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    let out = run(&["--index", &index, "-c", "printk"]);
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a.bin:1\nb.bin:1\n");
}

/// A PATTERN is a regex, as in rg; `-F` makes it the text it is.
#[test]
fn a_term_is_a_regex_and_f_makes_it_a_literal() {
    let (_tmp, docs) = stage_files(&[
        ("a.txt", b"call foo(bar)\nx a.b y\naxb\n/a.b/\n"),
        ("b.txt", b"nothing\n"),
    ]);
    let index_dir = do_index(&docs, false);
    let index = index_dir.display().to_string();
    let search =
        |args: &[&str]| run(&[&["--index", &index, "--no-heading", "-N"][..], args].concat());
    let text = |args: &[&str]| String::from_utf8_lossy(&search(args).stdout).into_owned();
    // The dot matches any character, as in rg; -F matches a dot.
    assert_eq!(text(&["a.b"]), "a.txt:x a.b y\na.txt:axb\na.txt:/a.b/\n");
    assert_eq!(text(&["-F", "a.b"]), "a.txt:x a.b y\na.txt:/a.b/\n");
    // The slashes of /a.b/ are text, not a delimiter.
    assert_eq!(text(&["/a.b/"]), "a.txt:/a.b/\n");
    let none = search(&["/a.c/"]);
    assert_eq!(none.status.code(), Some(1));
    assert!(
        stderr(&none).contains("the pattern /a.c/ matches its slashes too"),
        "{}",
        stderr(&none)
    );
    // Several patterns are one search: it says so when it found nothing,
    // and a search that counts says it of each pattern that counted none.
    let some = search(&["-e", "foo", "-e", "/a.c/", "-e", "/a.b/"]);
    assert!(some.status.success());
    assert_eq!(stderr(&some), "");
    let counted = search(&["-c", "-e", "foo", "-e", "/a.c/", "-e", "/a.b/"]);
    assert!(counted.status.success());
    assert_eq!(
        stderr(&counted).matches("matches its slashes too").count(),
        1,
        "{}",
        stderr(&counted)
    );
    assert!(stderr(&counted).contains("/a.c/"), "{}", stderr(&counted));
    let leaf = search(&["-c", "--or", "-e", "foo", "-e", "/a.c/"]);
    assert!(leaf.status.success());
    assert!(stderr(&leaf).contains("/a.c/"), "{}", stderr(&leaf));
    // A pattern that is not a regex is an error, with the way out.
    let bad = search(&["foo("]);
    assert_eq!(bad.status.code(), Some(2));
    let said = stderr(&bad);
    assert!(said.contains("unclosed group"), "{said}");
    assert!(
        said.ends_with("hint: to search for the text as it is, pass -F\n"),
        "{said}"
    );
    assert_eq!(text(&["-F", "foo("]), "a.txt:call foo(bar)\n");
    // A NUL is refused as rg refuses it, and -F is no way out of that.
    let nul = search(&["a\\x00b"]);
    assert_eq!(nul.status.code(), Some(2));
    let said = stderr(&nul);
    assert!(said.contains("impossible to match"), "{said}");
    assert!(!said.contains("hint:"), "{said}");
    // v0.1.0's flag still works, and means what the bare PATTERN means now.
    assert_eq!(text(&["--regex", "a.b"]), text(&["a.b"]));
    assert_eq!(search(&["-F", "--regex", "a"]).status.code(), Some(2));
    assert_eq!(search(&["-F", "--expr", "a"]).status.code(), Some(2));
    // -F reads --and and --or leaves as literals.
    assert_eq!(text(&["-l", "--and", "-e", "a.b", "-e", "foo("]), "");
    assert_eq!(
        text(&["-l", "--and", "-F", "-e", "a.b", "-e", "foo\\("]),
        ""
    );
    assert_eq!(
        text(&["-l", "--and", "-e", "a.b", "-e", "foo\\("]),
        "a.txt\n"
    );
}

/// As in rg, `-e` gives a pattern, and several make one search: the lines
/// that match any, with no section per pattern.
#[test]
fn several_e_patterns_are_one_output() {
    let (_tmp, docs) = stage_files(&[
        ("a.txt", b"call foo(bar)\nx a.b y\naxb\n/a.b/\n-x dash\n"),
        ("b.txt", b"nothing\n"),
    ]);
    let index_dir = do_index(&docs, false);
    let index = index_dir.display().to_string();
    let search =
        |args: &[&str]| run(&[&["--index", &index, "--no-heading", "-N"][..], args].concat());
    let text = |args: &[&str]| String::from_utf8_lossy(&search(args).stdout).into_owned();
    assert_eq!(
        text(&["-e", "foo\\(", "-e", "a\\.b"]),
        "a.txt:call foo(bar)\na.txt:x a.b y\na.txt:/a.b/\n"
    );
    assert_eq!(
        text(&["-F", "-e", "foo(", "-e", "a.b"]),
        "a.txt:call foo(bar)\na.txt:x a.b y\na.txt:/a.b/\n"
    );
    // The first of the patterns that matches wins, as in rg.
    assert_eq!(text(&["-o", "-e", "fo", "-e", "foo"]), "a.txt:fo\n");
    assert_eq!(text(&["-c", "-e", "foo", "-e", "axb"]), "a.txt:2\n");
    assert_eq!(
        text(&["-l", "-e", "axb", "-e", "nothing"]),
        "a.txt\nb.txt\n"
    );
    // A pattern that starts with a hyphen is no flag after -e.
    assert_eq!(text(&["-e", "-x"]), "a.txt:-x dash\n");
    // --and and --or take their patterns from -e.
    assert_eq!(text(&["-l", "--and", "-e", "foo", "-e", "axb"]), "a.txt\n");
    assert_eq!(text(&["-l", "--and", "-e", "foo", "-e", "nothing"]), "");
    // One that is no regex fails the search, with the way out.
    let bad = search(&["-e", "foo", "-e", "bar("]);
    assert_eq!(bad.status.code(), Some(2));
    assert!(stderr(&bad).contains("unclosed group"), "{}", stderr(&bad));
    assert!(stderr(&bad).contains("pass -F"), "{}", stderr(&bad));
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

/// The manifest of `index`, as `status --json` prints it.
fn status_json(index: &Path) -> serde_json::Value {
    let out = run(&["status".as_ref(), "--json".as_ref(), index.as_os_str()]);
    assert!(out.status.success(), "status: {}", stderr(&out));
    serde_json::from_slice(&out.stdout).unwrap()
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
            let manifest = status_json(&index_dir);
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
                .filter(|p| !p.ends_with("manifest.bin"))
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
                leaf_totals(&query.stdout),
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
    for terms in [["printk", "hello"], ["pr[i1]ntk", "h.llo"]] {
        let out = run(&query_argv(&index_dir, false, &terms));
        let stderr = stderr(&out);
        assert!(out.status.success(), "{stderr}");
        assert_eq!(stderr.matches("warning:").count(), 1, "{stderr}");
        assert!(
            stderr.contains("warning: excluding") && stderr.contains("a.bin"),
            "{stderr}"
        );
        assert_eq!(leaf_totals(&out.stdout), [1, 0], "{terms:?}");
    }
}

/// Files source, no watchman: a search applies the ignore files new since
/// the index, with one warning per process on its cost, until re-index.
#[test]
fn a_new_ignore_file_is_applied_and_warned_once() {
    let (tmp, docs) = stage();
    std::fs::create_dir(docs.join("sub")).unwrap();
    std::fs::write(docs.join("sub/d.bin"), b"printk in sub\n").unwrap();
    let index_dir = do_index(&docs, false);
    let search = |index: &Path| run(&query_argv(index, false, &["printk", "hello"]));
    let out = search(&index_dir);
    assert_eq!(stderr(&out), "");
    assert_eq!(leaf_totals(&out.stdout), [4, 1]);
    // Older than 2 s, so that the fresh index below records their stamps:
    // it records a newer one as unknown, which every load takes as changed.
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(10);
    for (rel, text) in [(".ignore", b"b.bin\n"), ("sub/.ignore", b"d.bin\n")] {
        let file = std::fs::File::create(docs.join(rel)).unwrap();
        std::io::Write::write_all(&mut &file, text).unwrap();
        file.set_modified(old).unwrap();
    }
    let warning = "warning: 2 ignore files changed since the index; each search reads their \
                   directories again (re-run index to make searches fast again)\n";
    for _ in 0..2 {
        let out = search(&index_dir);
        assert!(out.status.success(), "{}", stderr(&out));
        assert_eq!(stderr(&out), warning);
        assert_eq!(leaf_totals(&out.stdout), [2, 1]);
    }
    let fresh = tmp.path().join("fresh");
    let out = run(&index_argv(&docs, &fresh, false, &[]));
    assert!(out.status.success(), "index: {}", stderr(&out));
    let out = search(&fresh);
    assert_eq!(stderr(&out), "");
    assert_eq!(leaf_totals(&out.stdout), [2, 1]);
}

/// `status` runs a search's check of what changed and prints what it
/// found: the counts, and the lines that check warns, on stdout only,
/// where a search warns them on stderr.
#[test]
fn status_says_what_the_check_of_what_changed_did() {
    let (_tmp, docs) = stage();
    let out = run_in(&docs, &["index"]);
    assert!(out.status.success(), "index: {}", stderr(&out));
    std::fs::write(docs.join("a.bin"), b"printk edited\n").unwrap();
    std::fs::write(docs.join("e.bin"), b"printk added\n").unwrap();
    std::fs::remove_file(docs.join("c.bin")).unwrap();
    // Older than 2 s, so that the index's stamp of the dir tells it apart.
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(10);
    let file = std::fs::File::create(docs.join(".ignore")).unwrap();
    std::io::Write::write_all(&mut &file, b"nothing.bin\n").unwrap();
    file.set_modified(old).unwrap();
    let ignores = "1 ignore file changed since the index; each search reads its directory \
                   again (re-run index to make searches fast again)";
    let out = run_in(&docs, &["status"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = String::from_utf8_lossy(&out.stdout);
    let facts: Vec<&str> = text
        .lines()
        .skip_while(|l| !l.starts_with("changes"))
        .collect();
    let changes = [
        "changes: fewer than 50000 files: checked every file".to_owned(),
        format!("changes: {ignores}"),
        "since the index: 1 stale, 1 added, 1 gone".to_owned(),
        "ignore files changed since the index: .ignore".to_owned(),
        "record: none saved".to_owned(),
    ];
    assert_eq!(facts, changes);
    let c = std::fs::canonicalize(&docs).unwrap().join("c.bin");
    let gone = format!("warning: excluding {}: is missing", c.display());
    let note = "note: 1 file(s) changed since indexing are scanned in full by every query; \
                re-run index";
    // The search's notes, and not the ignore line a second time.
    let lines = |out: &std::process::Output| {
        let mut lines: Vec<String> = stderr(out).lines().map(str::to_owned).collect();
        lines.sort();
        lines
    };
    assert_eq!(lines(&out), [note, gone.as_str()]);
    let search = run_in(&docs, &["-l", "printk"]);
    assert_eq!(
        String::from_utf8_lossy(&search.stdout),
        "a.bin\nb.bin\ne.bin\n"
    );
    // The stat pass and discovery warn from two threads, in either order.
    let ignored = format!("warning: {ignores}");
    assert_eq!(lines(&search), [note, ignored.as_str(), gone.as_str()]);
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

/// `status --json` prints the manifest a search reads, with the fields and
/// the order an older fmgrep wrote to manifest.json; plain `status` prints
/// its scalars; `-- status` searches for the word.
#[test]
fn status_prints_the_manifest_of_the_index() {
    for store_docs in [false, true] {
        let (_tmp, root) = stage();
        std::fs::write(root.join("s.txt"), "status\n").unwrap();
        let index: &[&str] = match store_docs {
            true => &["index", "--store-docs", "--json"],
            false => &["index", "--json"],
        };
        let out = run_in(&root, index);
        assert!(out.status.success(), "{}", stderr(&out));
        let summary: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        let dotdir = std::fs::canonicalize(root.join(".fmgrep")).unwrap();
        let json = run_in(&root, &["status", "--json"]);
        assert_eq!(json.status.code(), Some(0), "{}", stderr(&json));
        let given = run(&["status", &dotdir.display().to_string(), "--json"]);
        assert_eq!(
            given.stdout, json.stdout,
            "{store_docs}: PATH names the index"
        );
        let text = String::from_utf8(json.stdout).unwrap();
        assert_eq!(text.lines().count(), 1, "{text}");
        let m: serde_json::Value = serde_json::from_str(&text).unwrap();
        let mut want_keys = vec![
            "format",
            "store_docs",
            "num_docs",
            "num_bytes",
            "bom_bytes",
            "index_size_bytes",
            "build_time_us",
            "index_started_ns",
            "files",
            "blobs",
            "doc_lens",
            "blob_lens",
        ];
        if !store_docs {
            want_keys.push("corpus_root");
        }
        // The corpus is in no git checkout: the reason there is no commit.
        if store_docs {
            want_keys.push("why_no_commit");
        }
        // Then what a search uses, and what its check did.
        let facts = [
            "uses", "note", "learned", "warned", "stale", "added", "gone", "ignores", "record",
        ];
        want_keys.extend(facts);
        assert_eq!(m.as_object().unwrap().len(), want_keys.len(), "{text}");
        assert_eq!(m["uses"], dotdir.to_str().unwrap());
        let why = m.get("why_no_commit").and_then(|w| w.as_str());
        assert_eq!(
            why.is_some_and(|w| w.ends_with(" is in no git checkout")),
            store_docs,
            "{text}"
        );
        let learned = if store_docs { "stored" } else { "not_due" };
        assert_eq!((&m["learned"], &m["note"]), (&learned.into(), &().into()));
        let ignores = if store_docs {
            ().into()
        } else {
            serde_json::json!([])
        };
        assert_eq!((&m["ignores"], &m["record"]), (&ignores, &().into()));
        let at: Vec<usize> = want_keys
            .iter()
            .map(|k| text.find(&format!("\"{k}\":")).unwrap())
            .collect();
        assert!(
            at.is_sorted(),
            "{store_docs}: the keys keep their order: {text}"
        );
        assert_eq!(m["format"], 9);
        for (key, of) in [
            ("store_docs", "store_docs"),
            ("num_docs", "num_files"),
            ("num_bytes", "num_bytes"),
            ("index_size_bytes", "index_size_bytes"),
            ("build_time_us", "build_time_us"),
        ] {
            assert_eq!(m[key], summary[of], "{store_docs} {key}");
        }
        assert_eq!(
            m["files"],
            serde_json::json!(["a.bin", "b.bin", "c.bin", "s.txt"])
        );
        let out = run_in(&root, &["status"]);
        assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
        let text = String::from_utf8(out.stdout).unwrap();
        let mut want = vec![format!(
            "a search here uses the local index {}",
            dotdir.display()
        )];
        for key in [
            "format",
            "store_docs",
            "num_docs",
            "num_bytes",
            "bom_bytes",
            "index_size_bytes",
            "build_time_us",
            "index_started_ns",
        ] {
            want.push(format!("{key}: {}", m[key]));
        }
        want.push(format!("shards: {}", m["blobs"].as_array().unwrap().len()));
        if !store_docs {
            want.push(format!(
                "corpus_root: {}",
                m["corpus_root"].as_str().unwrap()
            ));
            want.extend([
                "changes: fewer than 50000 files: checked every file".into(),
                "since the index: 0 stale, 0 added, 0 gone".into(),
                "ignore files changed since the index: none".into(),
                "record: none saved".into(),
            ]);
        } else {
            want.push(format!(
                "why_no_commit: {}",
                m["why_no_commit"].as_str().unwrap()
            ));
            want.push("changes: none: the index holds the files".into());
        }
        assert_eq!(text.lines().collect::<Vec<_>>(), want, "{store_docs}");

        for args in [&["--", "status"][..], &["-l", "status"]] {
            let out = run_in(&root, args);
            assert_eq!(out.status.code(), Some(0), "{args:?}: {}", stderr(&out));
            assert!(
                String::from_utf8_lossy(&out.stdout).contains("s.txt"),
                "{args:?} searches"
            );
        }
    }
}

/// `status` refuses what a search refuses before the index loads, with
/// the same words: a manifest of another format; in either layout, a
/// `blobs.shard` longer than the manifest's blob lengths.
#[test]
fn status_of_a_refused_index_fails_as_a_search() {
    let (_tmp, docs) = stage();
    let index_dir = do_index(&docs, true);
    let path = index_dir.join("manifest.bin");
    let mut raw = std::fs::read(&path).unwrap();
    raw[8..12].copy_from_slice(&12u32.to_le_bytes());
    std::fs::write(&path, raw).unwrap();
    let refused = |index_dir: &Path, words: &str| {
        let index = index_dir.display().to_string();
        let search = run(&["--index", &index, "printk"]);
        let status = run(&["status", &index]);
        assert_eq!(status.status.code(), Some(2));
        assert!(status.stdout.is_empty());
        assert!(stderr(&status).contains(words), "{}", stderr(&status));
        assert_eq!(stderr(&status), stderr(&search));
    };
    refused(&index_dir, "unsupported manifest format 12");
    for store_docs in [true, false] {
        let (_tmp, docs) = stage();
        let index_dir = do_index(&docs, store_docs);
        let shard = index_dir.join("blobs.shard");
        let mut raw = std::fs::read(&shard).unwrap();
        raw.push(0);
        std::fs::write(&shard, raw).unwrap();
        refused(&index_dir, "refusing a tampered index");
    }
}

/// A damaged path in the manifest is refused by what reads it, in the
/// manifest's words: a search that prints it, before any line of that
/// term; `status --json`; a files-layout search and status, which read
/// every path; a server, at start. What reads no damaged path answers as
/// on the whole index.
#[test]
fn a_damaged_path_is_refused_by_what_reads_it() {
    let files: [(&str, &[u8]); 3] = [("a.c", b"printk"), ("b.c", b"zz"), ("c.c", b"printk zz")];
    for store_docs in [true, false] {
        let (tmp, docs) = stage_files(&files);
        let whole = tmp.path().join("whole");
        let out = run(&index_argv(&docs, &whole, store_docs, &[]));
        assert!(out.status.success(), "index: {}", stderr(&out));
        let index_dir = do_index(&docs, store_docs);
        let path = index_dir.join("manifest.bin");
        let mut raw = std::fs::read(&path).unwrap();
        // The path text "a.cb.cc.c" ends the file: damage the "b" of "b.c".
        let at = raw.len() - 6;
        assert_eq!(raw[at], b'b');
        raw[at] = 0xFF;
        std::fs::write(&path, raw).unwrap();
        let words = format!(
            "error: {} holds a path that is not UTF-8; re-run `index`\n",
            path.display()
        );
        let (index, whole) = (index_dir.display().to_string(), whole.display().to_string());
        let refused = |args: &[&str]| {
            let out = run(args);
            assert_eq!(out.status.code(), Some(2), "{args:?}: {}", stderr(&out));
            assert!(out.stdout.is_empty(), "{args:?}");
            assert_eq!(stderr(&out), words, "{args:?}");
        };
        refused(&["status", "--json", &index]);
        if store_docs {
            let status = run(&["status", &index]);
            assert!(status.status.success(), "{}", stderr(&status));
            assert!(String::from_utf8_lossy(&status.stdout).contains("num_docs: 3"));
        } else {
            refused(&["status", &index]);
        }
        let served = fmgrep()
            .args(["serve", &index, "--stdio", "--no-watch"])
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert_eq!(served.status.code(), Some(2), "{}", stderr(&served));
        assert_eq!(stderr(&served), words);
        // "zz" prints b.c then c.c: nothing of it is printed.
        refused(&["--index", &index, "zz"]);
        refused(&["--index", &index, "-l", "--or", "-e", "zz", "-e", "printk"]);
        if !store_docs {
            // The files layout reads every path at load.
            refused(&["--index", &index, "printk"]);
            continue;
        }
        for args in [
            &["printk"][..],
            &["-l", "printk"],
            &["-c", "printk"],
            &["missing"],
        ] {
            let got = run(&[&["--index", &index][..], args].concat());
            let want = run(&[&["--index", &whole][..], args].concat());
            assert_eq!(
                (got.status.code(), &got.stdout, stderr(&got)),
                (want.status.code(), &want.stdout, stderr(&want)),
                "{args:?}"
            );
        }
    }
}

/// `status` is not a search: under `FMGREP_VERBOSE=1` it prints the same
/// stdout, exits the same and prints no spans, on a good index and on a
/// refused one.
#[test]
fn status_prints_no_spans() {
    let (_tmp, docs) = stage();
    let index_dir = do_index(&docs, false);
    let index = index_dir.display().to_string();
    let status = |args: &[&str], verbose: bool| {
        let mut cmd = fmgrep();
        cmd.arg("status").arg(&index).args(args);
        cmd.env_remove("FMGREP_VERBOSE");
        if verbose {
            cmd.env("FMGREP_VERBOSE", "1");
        }
        cmd.output().expect("spawn fmgrep")
    };
    let path = index_dir.join("manifest.bin");
    for (refused, code) in [(false, 0), (true, 2)] {
        if refused {
            let mut raw = std::fs::read(&path).unwrap();
            raw[8..12].copy_from_slice(&12u32.to_le_bytes());
            std::fs::write(&path, raw).unwrap();
        }
        for args in [&[][..], &["--json"]] {
            let (plain, verbose) = (status(args, false), status(args, true));
            let what = format!("{refused} {args:?}");
            assert_eq!(plain.status.code(), Some(code), "{what}");
            assert_eq!(verbose.status.code(), Some(code), "{what}");
            assert_eq!(plain.stdout, verbose.stdout, "{what}");
            assert_eq!(stderr(&plain), stderr(&verbose), "{what}");
            assert!(!stderr(&verbose).contains("note: "), "{what}");
        }
    }
}

/// An index that an older fmgrep wrote, kept as it wrote it: search, serve
/// and status refuse it by its format.
#[test]
fn an_index_of_format_7_is_refused_by_its_number() {
    let tmp = tempfile::tempdir().unwrap();
    let index = tmp.path().join("idx");
    std::fs::create_dir(&index).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cli/format-7-manifest.json"),
        index.join("manifest.json"),
    )
    .unwrap();
    std::fs::write(index.join("blobs.shard"), b"").unwrap();
    let at = index.display().to_string();
    let want = format!(
        "error: unsupported manifest format 7 in {at}/manifest.json (want 9); re-run `index` with this binary\n"
    );
    for args in [
        &["--index", &at, "printk"][..],
        &["status", &at],
        &["serve", "--stdio", "--no-watch", &at],
    ] {
        let out = run(args);
        assert_eq!(
            (out.status.code(), stderr(&out)),
            (Some(2), want.clone()),
            "{args:?}"
        );
        assert!(out.stdout.is_empty(), "{args:?}");
    }
    std::fs::write(index.join("manifest.json"), b"{\"format\":").unwrap();
    let out = run(&["status", &at]);
    assert_eq!(stderr(&out), want.replace("format 7 in", "format in"));
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
    for args in [
        &["printk"][..],
        &["--", "index"],
        &["status"],
        &["--", "status"],
    ] {
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

/// Files source: a document holding a long anchor but no match is never
/// read.
#[test]
fn a_long_anchor_reads_only_matching_docs() {
    use std::os::unix::fs::PermissionsExt;
    let (_tmp, docs) = stage_files(&[
        ("a.c", b"union x {\n"),
        ("b.c", b"union {\n"),
        ("c.c", b"none\n"),
    ]);
    let index_dir = do_index(&docs, false);
    std::fs::set_permissions(docs.join("a.c"), std::fs::Permissions::from_mode(0o000)).unwrap();
    let out = run(&query_argv(&index_dir, false, &["union[ \\t]+\\{"]));
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(counts(&out.stdout), [(1, 1)]);
    let index = index_dir.display().to_string();
    let out = run(&["--index", &index, "-c", "union[ \\t]+\\{"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "b.c:1\n");
    assert!(!stderr(&out).contains("excluding"));
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
    let (var, raw) = ("FMGREP_SHARD_BYTES", "nope");
    let out = fmgrep()
        .args(index_argv(&docs, &index_dir, false, &[]))
        .env(var, raw)
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(2), "{var}={raw}");
    assert!(stderr(&out).contains(var), "{}", stderr(&out));
    assert!(!index_dir.exists(), "a failed index must persist nothing");
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

/// As in rg, the first word is the PATTERN and the rest are PATHs. A PATH
/// that does not exist is an error, the others are searched, and the exit
/// is 2 whatever they found. Each expectation is rg 15.2.0's.
#[test]
fn a_path_that_does_not_exist_is_an_error_as_in_rg() {
    let (_tmp, docs) = stage_files(&[("a.c", b"printk a\n"), ("b.h", b"printk b\n")]);
    assert!(run_in(&docs, &["index"]).status.success());
    let found = |args: &[&str]| {
        let out = run_in(&docs, args);
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr(&out),
        )
    };
    let no = |path: &str| format!("error: {path}: No such file or directory (os error 2)\n");
    assert_eq!(
        found(&["printk", "nosuch"]),
        (Some(2), "".into(), no("nosuch"))
    );
    assert_eq!(
        found(&["printk", "nosuch", "a.c"]),
        (Some(2), "a.c:printk a\n".into(), no("nosuch"))
    );
    assert_eq!(
        found(&["-c", "printk", "a.c", "nosuch"]),
        (Some(2), "a.c:1\n".into(), no("nosuch"))
    );
    assert_eq!(
        found(&["printk", "nosuch", "nope"]),
        (Some(2), "".into(), no("nosuch") + &no("nope"))
    );
    // A word that names no file is no pattern: -e gives it.
    assert_eq!(
        found(&["-e", "printk", "nosuch"]),
        (Some(2), "".into(), no("nosuch"))
    );
    assert_eq!(
        found(&["-l", "-e", "printk", "a.c"]),
        (Some(0), "a.c\n".into(), "".into())
    );
    // The word after -- is the PATTERN.
    assert_eq!(
        found(&["-l", "--", "printk", "a.c"]),
        (Some(0), "a.c\n".into(), "".into())
    );
    // The old `--and A B`: B is a PATH, and one pattern is no conjunction.
    let (code, out, said) = found(&["--and", "printk", "b.h"]);
    assert_eq!((code, out.as_str()), (Some(2), ""));
    assert!(said.contains("--and needs at least two patterns"), "{said}");
    // A search that matches nothing in the PATHs it has is 2 as well.
    assert_eq!(
        found(&["absent", "a.c", "nosuch"]),
        (Some(2), "".into(), no("nosuch"))
    );
    // The counts report is printed, and the error follows.
    let (code, out, said) = found(&["--json", "printk", "a.c", "nosuch"]);
    assert_eq!((code, said), (Some(2), no("nosuch")));
    assert!(out.contains("\"queries\""), "{out}");
}

/// A search whose stdin is a pipe holding `piped`.
fn run_piped(dir: &Path, args: &[&str], piped: &[u8]) -> Output {
    let mut child = fmgrep()
        .current_dir(dir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn fmgrep");
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), piped).unwrap();
    child.wait_with_output().unwrap()
}

/// fmgrep searches no stdin, whatever it holds, and the PATH `-` is no
/// stdin as it is in rg: it is a PATH like any other. Without a file of that
/// name it is the error of a missing PATH; with one, that file is searched.
#[test]
fn stdin_is_never_read_and_a_dash_is_a_plain_path() {
    let said = |out: &Output| {
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        (out.status.code(), stdout, stderr(out))
    };
    let piped = b"printk piped\n";
    let (_tmp, docs) = stage_files(&[("a.c", b"printk a\n")]);
    assert!(run_in(&docs, &["index"]).status.success());
    // No PATH: the tree is searched, not the pipe.
    let out = run_piped(&docs, &["printk"], piped);
    assert_eq!(said(&out), (Some(0), "a.c:printk a\n".into(), "".into()));
    // The PATH `-` names no file: the error, and the pipe stays unread.
    let out = run_piped(&docs, &["printk", "-"], piped);
    let no = "error: -: No such file or directory (os error 2)\n";
    assert_eq!(said(&out), (Some(2), "".into(), no.into()));
    let out = run_piped(&docs, &["printk", "a.c", "-"], piped);
    assert_eq!(said(&out), (Some(2), "a.c:printk a\n".into(), no.into()));

    let (_tmp, docs) = stage_files(&[("a.c", b"printk a\n"), ("-", b"printk dash\n")]);
    assert!(run_in(&docs, &["index"]).status.success());
    let out = run_piped(&docs, &["printk", "-"], piped);
    assert_eq!(said(&out), (Some(0), "printk dash\n".into(), "".into()));
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
    assert_eq!(found(&sub, &["printk"]), ok("c.c:printk c\nd.h:printk d\n"));
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

/// Each expectation is rg 15.2.0's from the same directory, stdout piped.
#[test]
fn piped_output_prints_as_rg() {
    let (_tmp, docs) = stage_files(&[("a.c", b"printk a\n"), ("b.h", b"printk b\n")]);
    assert!(run_in(&docs, &["index"]).status.success());
    for (args, want) in [
        (&["printk"][..], "a.c:printk a\nb.h:printk b\n"),
        (&["-n", "printk"], "a.c:1:printk a\nb.h:1:printk b\n"),
        (&["--heading", "printk"], "a.c\nprintk a\n\nb.h\nprintk b\n"),
        (
            &["-n", "--heading", "printk"],
            "a.c\n1:printk a\n\nb.h\n1:printk b\n",
        ),
    ] {
        let out = run_in(&docs, args);
        assert_eq!(out.status.code(), Some(0), "{args:?}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{args:?}");
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

/// A row of the span block: its name, depth, thread, and start and took in
/// ms.
struct Row {
    name: String,
    depth: usize,
    thread: u64,
    start: f64,
    took: f64,
}

/// The rows and the total of a stderr that holds the span block and
/// nothing else.
fn span_rows(stderr: &str) -> (Vec<Row>, f64) {
    let lines: Vec<&str> = stderr.lines().collect();
    let [head, rows @ .., last] = &lines[..] else {
        panic!("{stderr}")
    };
    let want = "note: span                 thread  start ms  took ms";
    assert_eq!(*head, want, "{stderr}");
    let total = last.strip_prefix("note: total").expect(stderr);
    let rows = rows.iter().map(|line| {
        let rest = line.strip_prefix("note: ").expect(stderr);
        let words: Vec<&str> = rest.split_whitespace().collect();
        let [name, thread, start, took] = words[..] else {
            panic!("{stderr}")
        };
        Row {
            name: name.to_owned(),
            depth: (rest.len() - rest.trim_start().len()) / 2,
            thread: thread.parse().unwrap(),
            start: start.parse().unwrap(),
            took: took.parse().unwrap(),
        }
    });
    (rows.collect(), total.trim().parse().expect(stderr))
}

/// The span block of one search: each span under its parent and inside
/// it; spans of one thread under one parent follow each other; `check`,
/// `stat` and `discovery` run on other threads than the main one; `wait`
/// ends after `check`; the top adds up to the total, and the total is
/// within the process's wall time.
pub(crate) fn check_spans(err: &str, store_docs: bool, wall: Duration) {
    // Each figure is rounded to 0.01 ms.
    const ROUND: f64 = 0.02;
    let (rows, total) = span_rows(err);
    let parent = |i: usize| {
        let depth = rows[i].depth.checked_sub(1)?;
        (0..i).rev().find(|&j| rows[j].depth == depth)
    };
    let mut shape: Vec<(&str, Option<&str>)> = (0..rows.len())
        .map(|i| (&*rows[i].name, parent(i).map(|j| &*rows[j].name)))
        .collect();
    shape.sort();
    let mut want = vec![
        ("search", None),
        ("locate", Some("search")),
        ("manifest", Some("search")),
        ("load", Some("search")),
        ("open", Some("load")),
        ("index", Some("load")),
        ("check", Some("load")),
        ("wait", Some("load")),
        ("query", Some("search")),
        ("save", Some("search")),
        ("drop", Some("search")),
        ("flush", None),
    ];
    if !store_docs {
        want.extend([("stat", Some("check")), ("discovery", Some("check"))]);
    }
    want.sort();
    assert_eq!(shape, want, "{err}");
    let end = |r: &Row| r.start + r.took;
    for (i, r) in rows.iter().enumerate() {
        let beside = ["check", "stat", "discovery"].contains(&&*r.name);
        assert_eq!(r.thread != 0, beside, "{}: {err}", r.name);
        if let Some(p) = parent(i).map(|j| &rows[j]) {
            assert!(r.start >= p.start - ROUND, "{}: {err}", r.name);
            assert!(end(r) <= end(p) + ROUND, "{}: {err}", r.name);
        }
        for (j, q) in rows.iter().enumerate().take(i) {
            if parent(j) == parent(i) && q.thread == r.thread {
                assert!(r.start >= end(q) - ROUND, "{} {}: {err}", q.name, r.name);
            }
        }
    }
    let at = |name: &str| rows.iter().find(|r| r.name == name).unwrap();
    if !store_docs {
        assert_eq!(at("stat").thread, at("check").thread, "{err}");
        assert_ne!(at("discovery").thread, at("check").thread, "{err}");
    }
    // The main thread waits for check, and check ends at its last exit,
    // on its own thread or on discovery's.
    assert!(end(at("wait")) >= end(at("check")) - ROUND, "{err}");
    let (search, flush) = (at("search"), at("flush"));
    assert!(flush.start - end(search) < 5.0, "{err}");
    assert!(
        total >= end(flush) - ROUND && total - end(flush) < 5.0,
        "{err}"
    );
    assert!(total <= wall.as_secs_f64() * 1e3, "{wall:?}: {err}");
}

/// Every output mode prints the same stdout and exits the same with and
/// without `FMGREP_VERBOSE=1`, and prints nothing on stderr without it.
/// With it, a search ends with its span block on stderr, and `index`
/// prints none.
#[test]
fn verbose_leaves_stdout_and_the_exit_status_alone() {
    const MODES: [&[&str]; 9] = [
        &["printk"],
        &["-l", "printk"],
        &["-c", "printk"],
        &["--count-matches", "printk"],
        &["--json", "printk"],
        &["--approx-counts", "-e", "printk", "-e", "hello"],
        &["-C", "1", "printk"],
        &["--expr", "printk AND hello"],
        &["-c", "absent"],
    ];
    for store_docs in [false, true] {
        let (_tmp, docs) = stage_files(&[
            ("a.c", b"printk hello printk\n"),
            ("b.c", b"int x;\nprintk(x);\nreturn;\n"),
        ]);
        let index = do_index(&docs, store_docs).display().to_string();
        let again = index_argv(&docs, &docs.with_file_name("again"), store_docs, &[]);
        let out = fmgrep().args(again).env("FMGREP_VERBOSE", "1").output();
        let out = out.expect("spawn fmgrep");
        assert!(out.status.success(), "{}", stderr(&out));
        assert!(!stderr(&out).contains("note: span"), "{}", stderr(&out));
        for args in MODES {
            let search = |verbose: bool| {
                let mut cmd = fmgrep();
                cmd.arg("--index").arg(&index).args(args);
                cmd.env_remove("FMGREP_VERBOSE");
                if verbose {
                    cmd.env("FMGREP_VERBOSE", "1");
                }
                let start = Instant::now();
                let out = cmd.output().expect("spawn fmgrep");
                (out, start.elapsed())
            };
            let ((plain, _), (verbose, wall)) = (search(false), search(true));
            let what = format!("{store_docs} {args:?}");
            let code = if args.contains(&"absent") { 1 } else { 0 };
            assert_eq!(plain.status.code(), Some(code), "{what}");
            assert_eq!(verbose.status.code(), Some(code), "{what}");
            let stdout = |o: &Output| serve::timeless(&o.stdout);
            assert_eq!(stdout(&plain), stdout(&verbose), "{what}");
            assert_eq!(stderr(&plain), "", "{what}");
            check_spans(&stderr(&verbose), store_docs, wall);
        }
    }
}

fn utf16(bom: [u8; 2], s: &str) -> Vec<u8> {
    let be = bom == [0xfe, 0xff];
    let units = s.encode_utf16();
    let mut raw = bom.to_vec();
    raw.extend(units.flat_map(|u| if be { u.to_be_bytes() } else { u.to_le_bytes() }));
    raw
}

/// A UTF-16 file with a BOM is searched as UTF-8, as rg decodes it, and
/// stays fresh until rewritten.
#[test]
fn utf16_file_prints_as_utf8_lines() {
    let le = utf16([0xff, 0xfe], "alpha\r\nthe CLASS ü\r\n");
    let (tmp, docs) = stage_files(&[("u.txt", &le), ("z.txt", b"no match\n")]);
    for store_docs in [false, true] {
        let index_dir = tmp.path().join(format!("idx-{store_docs}"));
        let out = run(&index_argv(&docs, &index_dir, store_docs, &[]));
        assert!(out.status.success(), "{}", stderr(&out));
        let index = index_dir.display().to_string();
        let out = run(&["--index", &index, "-n", "CLASS"]);
        assert_eq!(stderr(&out), "");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "u.txt:2:the CLASS ü\r\n"
        );
    }
    std::fs::write(docs.join("u.txt"), utf16([0xfe, 0xff], "CLASS again\n")).unwrap();
    let index = tmp.path().join("idx-false").display().to_string();
    let out = run(&["--index", &index, "-n", "CLASS"]);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "u.txt:1:CLASS again\n"
    );
}

/// A file docfm cannot hold is skipped with a warning, never failing the tree.
#[test]
fn a_file_over_2_gib_is_skipped_with_a_warning() {
    let (tmp, docs) = stage_files(&[("a.c", b"printk\n")]);
    let huge = std::fs::File::create(docs.join("huge.log")).unwrap();
    huge.set_len(1 << 31).unwrap();
    let index_dir = tmp.path().join("idx");
    let out = run(&index_argv(&docs, &index_dir, false, &[]));
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stderr(&out),
        "warning: skipping huge.log: over 2147483646 bytes\n"
    );
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("indexed 1 file (7 bytes)"));
}

/// A file new since the index that cannot be read, or that docfm cannot
/// hold, is left out of the answer and named on stderr, as rg names a file
/// it cannot open.
#[cfg(unix)]
#[test]
fn a_new_file_that_cannot_be_read_is_skipped_with_a_warning() {
    use std::os::unix::fs::PermissionsExt;
    let (_tmp, docs) = stage_files(&[("a.c", b"printk\n")]);
    let index_dir = do_index(&docs, false);
    let shut = docs.join("new.c");
    std::fs::write(&shut, b"printk\n").unwrap();
    std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o000)).unwrap();
    let huge = std::fs::File::create(docs.join("huge.log")).unwrap();
    huge.set_len(1 << 31).unwrap();
    if std::fs::read(&shut).is_ok() {
        println!("skipped: the file can be read (running as root)");
        return;
    }
    let index = index_dir.display().to_string();
    let out = run(&["--index", &index, "printk"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a.c:printk\n");
    let docs = docs.canonicalize().unwrap().display().to_string();
    let mut lines: Vec<String> = stderr(&out).lines().map(str::to_owned).collect();
    lines.sort();
    assert_eq!(
        lines,
        [
            format!("warning: skipping {docs}/huge.log: over 2147483646 bytes"),
            format!(
                "warning: skipping {docs}/new.c: cannot read (Permission denied (os error 13))"
            ),
        ]
    );
}

/// A file new since the index that two walks of one load both find is
/// named once: a changed ignore file makes the load walk its directory
/// again, after the walk of the changed directories.
#[cfg(unix)]
#[test]
fn a_new_file_that_two_walks_find_is_named_once() {
    use std::os::unix::fs::PermissionsExt;
    let (_tmp, docs) = stage_files(&[("a.c", b"printk\n"), (".ignore", b"*.gen\n")]);
    let index_dir = do_index(&docs, false);
    std::fs::write(docs.join(".ignore"), b"*.gen\n*.tmp\n").unwrap();
    let shut = docs.join("new.c");
    std::fs::write(&shut, b"printk\n").unwrap();
    std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&shut).is_ok() {
        println!("skipped: the file can be read (running as root)");
        return;
    }
    let index = index_dir.display().to_string();
    let out = run(&["--index", &index, "printk"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a.c:printk\n");
    let named: Vec<String> = stderr(&out)
        .lines()
        .filter(|l| l.starts_with("warning: skipping"))
        .map(str::to_owned)
        .collect();
    let docs = docs.canonicalize().unwrap().display().to_string();
    assert_eq!(
        named,
        [format!(
            "warning: skipping {docs}/new.c: cannot read (Permission denied (os error 13))"
        )],
        "{}",
        stderr(&out)
    );
}

/// A directory new since the index that cannot be listed is named on
/// stderr, and the answer is the one without what is under it.
#[cfg(unix)]
#[test]
fn a_new_directory_that_cannot_be_listed_is_named_with_a_warning() {
    use std::os::unix::fs::PermissionsExt;
    let (_tmp, docs) = stage_files(&[("a.c", b"printk\n")]);
    let index_dir = do_index(&docs, false);
    let shut = docs.join("newdir");
    std::fs::create_dir(&shut).unwrap();
    std::fs::write(shut.join("e.c"), b"printk\n").unwrap();
    std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o000)).unwrap();
    let listable = std::fs::read_dir(&shut).is_ok();
    let index = index_dir.display().to_string();
    let out = run(&["--index", &index, "printk"]);
    std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o755)).unwrap();
    if listable {
        println!("skipped: the directory can be listed (running as root)");
        return;
    }
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a.c:printk\n");
    let shut = shut.canonicalize().unwrap();
    assert_eq!(
        stderr(&out),
        format!(
            "warning: cannot list {}: Permission denied (os error 13); files under it may be \
             missing from the answer\n",
            shut.display()
        )
    );
}
