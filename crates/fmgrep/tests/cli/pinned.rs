//! A later search of a checkout through a server sends its query pinned
//! to the commit that the pin keeps, and asks for no status; when the
//! server refuses it, the search prints what a first search prints.

use std::path::Path;
use std::process::{Output, Stdio};

use crate::proxy::{Proxy, scripted};
use crate::remote::{ASKED_AGAIN, ASKED_GIT, FILES, Remote, comparable, ignored, stderr, stdout};

/// `args` from `cwd` through `proxy`, and the methods that it sent, of the
/// last try.
fn counted(r: &Remote, proxy: &Proxy, cwd: &Path, args: &[&str]) -> (Output, Vec<String>) {
    let (out, requests) = requested(r, proxy, cwd, args);
    let methods = requests.iter().map(|q| q["method"].as_str().unwrap_or("?"));
    (out, methods.map(str::to_owned).collect())
}

/// [`counted`], with the requests themselves.
fn requested(
    r: &Remote,
    proxy: &Proxy,
    cwd: &Path,
    args: &[&str],
) -> (Output, Vec<serde_json::Value>) {
    let mut requests = Vec::new();
    let out = r.tried(|| {
        proxy.requests();
        let out = r.once(&proxy.addr, cwd, args);
        requests = proxy.requests();
        out
    });
    (out, requests)
}

/// The kind of a request of `counted`: `query`, `status`, or, for a query,
/// `query+rendered` when it asks for the server's own output, and `query`
/// with no `commit` unless it is pinned.
fn kinds(requests: &[serde_json::Value]) -> Vec<String> {
    let kind = |q: &serde_json::Value| match q["method"].as_str().unwrap_or("?") {
        "query" => format!(
            "query{}{}",
            if q["params"]["commit"].is_string() {
                " pinned"
            } else {
                ""
            },
            if q["params"]["rendered"] == true {
                " rendered"
            } else {
                ""
            },
        ),
        other => other.to_owned(),
    };
    requests.iter().map(kind).collect()
}

/// What a search printed, with its timings and the address of its server
/// left out.
fn seen(out: &Output, addr: &str) -> (String, String, Option<i32>) {
    let took = regex::Regex::new(r"took [^;)]*").unwrap();
    let text = stderr(out).replace(addr, "ADDR");
    let text = took.replace_all(&text, "took T").into_owned();
    (comparable(&stdout(out)), text, out.status.code())
}

/// The pin of the client's checkout and the server at `addr`.
fn pin_of(r: &Remote, addr: &str) -> std::path::PathBuf {
    let dir = r.s.path("cache/fmgrep");
    let pins: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "pin"))
        .filter(|p| {
            std::fs::read_to_string(p)
                .unwrap()
                .contains(&format!("\"{addr}\""))
        })
        .collect();
    assert_eq!(pins.len(), 1, "{pins:?}");
    pins[0].clone()
}

const ALL: &str = "a.c\ndir/b.c\ndir/sub/c.h\ne.c\n";

/// After the first search of a checkout through a server, each search
/// from it sends one request, and prints what a search that asked for the
/// status prints: from below the top, with PATHs, with
/// `--ignore-local-changes`, with `--server`. Outside a checkout, and for
/// `fmgrep status`, the status is asked every time.
#[test]
fn a_later_search_sends_its_pinned_query_alone() {
    let r = Remote::new(&FILES);
    r.s.write(
        &r.client,
        &[("dir/new.c", "printk new\n"), ("a.c", "none\n")],
    );
    let proxy = Proxy::new(&r.addr);
    let (_, asked) = counted(&r, &proxy, &r.client, &["-l", "printk"]);
    assert_eq!(asked, ["status", "query"]);
    let dir = r.client.join("dir");
    let cases: [(&Path, &[&str]); 6] = [
        (&r.client, &["-l", "printk"]),
        (&dir, &["-n", "printk", "../e.c", "sub"]),
        (&dir, &["--json", "printk"]),
        (&r.client, &["--stats", "printk"]),
        (&r.client, &["--ignore-local-changes", "-c", "printk"]),
        (&dir, &["--server", "ADDR", "-l", "printk"]),
    ];
    for (cwd, args) in cases {
        let with =
            |addr: &str| -> Vec<String> { args.iter().map(|a| a.replace("ADDR", addr)).collect() };
        // A server address of its own has no pin: the status first.
        let fresh = Proxy::new(&r.addr);
        let words = with(&fresh.addr);
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        let (want, asked) = counted(&r, &fresh, cwd, &words);
        assert_eq!(asked, ["status", "query"], "{args:?}");
        let words = with(&proxy.addr);
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        let (got, asked) = counted(&r, &proxy, cwd, &words);
        assert_eq!(asked, ["query"], "{args:?}");
        assert_eq!(
            seen(&got, &proxy.addr),
            seen(&want, &fresh.addr),
            "{args:?}"
        );
    }
    let away = std::fs::canonicalize(r.s.path("home")).unwrap();
    for _ in 0..2 {
        let (out, asked) = counted(&r, &proxy, &away, &["-l", "printk"]);
        assert_eq!(asked, ["status", "query"]);
        let why = format!("{} is in no git checkout", away.display());
        assert_eq!(
            (stdout(&out), stderr(&out)),
            (ALL.to_owned(), ignored(&why))
        );
        let status = r.s.run(&r.client, &["status", "--server", &proxy.addr]);
        assert!(status.status.success(), "{}", stderr(&status));
        assert_eq!(proxy.methods(), ["status"]);
    }
}

/// A server that moved to another commit between two searches refuses
/// the query pinned to the old one: the search asks for the status and
/// prints what a first search against the new commit prints, and nothing
/// of the old commit. The next search sends one request again.
#[test]
fn a_search_after_the_server_moved_answers_for_its_new_commit() {
    let r = Remote::new(&FILES);
    let proxy = Proxy::new(&r.addr);
    let (_, asked) = counted(&r, &proxy, &r.client, &["-l", "printk"]);
    assert_eq!(asked, ["status", "query"]);
    r.s.write(&r.origin, &[("a.c", "none\n"), ("dir/n.c", "printk n\n")]);
    let second = r.s.commit(&r.origin, "second");
    r.s.git(&r.client, &["fetch", "-q", "origin"]);
    r.s.write(&r.client, &[("e.c", "printk e2\n")]);
    let (index, _) = r.s.index(&r.origin, "idx2");
    let (_moved, addr) = r.s.serve(&index, &[]);
    proxy.point(&addr);
    let args = ["-n", "printk"];
    let (out, asked) = counted(&r, &proxy, &r.client, &args);
    assert_eq!(asked, ["query", "status", "query"]);
    let want = r.oracle("", &args);
    assert_eq!(comparable(&stdout(&out)), comparable(&stdout(&want)));
    assert_eq!(r.warnings(&out), "");
    let text = stderr(&out);
    assert!(!text.contains(&r.commit[..7]), "{text}");
    let asked_git = match r.daemon {
        Some(_) => format!(
            "{ASKED_AGAIN}{} (no record of an earlier search; ",
            &second[..7]
        ),
        None => ASKED_GIT.to_owned(),
    };
    assert!(
        text.starts_with(&asked_git) && text.lines().count() == 1,
        "{text}"
    );
    let (again, asked) = counted(&r, &proxy, &r.client, &args);
    assert_eq!(asked, ["query"]);
    assert_eq!(comparable(&stdout(&again)), comparable(&stdout(&want)));
}

/// A server of the same commit at another root answers the pinned query
/// as the first server did. A server of the same tree without stored
/// documents reports no served commit: it refuses the query, and the
/// search prints the fallback of a first search; with no commit to pin,
/// each later search asks for the status.
#[test]
fn a_server_of_another_index_of_the_commit_answers_right() {
    let r = Remote::new(&FILES);
    let proxy = Proxy::new(&r.addr);
    let args = ["-l", "printk"];
    let (_, asked) = counted(&r, &proxy, &r.client, &args);
    assert_eq!(asked, ["status", "query"]);
    let (want, asked) = counted(&r, &proxy, &r.client, &args);
    assert_eq!(asked, ["query"]);
    let url = r.origin.display().to_string();
    r.s.git(r.s.tmp.path(), &["clone", "-q", &url, "elsewhere"]);
    let elsewhere = std::fs::canonicalize(r.s.path("elsewhere")).unwrap();
    let (index, _) = r.s.index(&elsewhere, "idx-elsewhere");
    let (_elsewhere, addr) = r.s.serve(&index, &[]);
    assert_ne!(r.s.status(&addr)["root"], r.s.status(&r.addr)["root"]);
    proxy.point(&addr);
    let (got, asked) = counted(&r, &proxy, &r.client, &args);
    assert_eq!(asked, ["query"]);
    assert_eq!(seen(&got, &proxy.addr), seen(&want, &proxy.addr));

    let output = r.s.path("idx-files").display().to_string();
    let built = r.s.run(&elsewhere, &["index", ".", "--output", &output]);
    assert!(built.status.success(), "{}", stderr(&built));
    let (_files, addr) = r.s.serve(&r.s.path("idx-files"), &["--no-watch"]);
    proxy.point(&addr);
    let why = format!(
        "the server at {} (FMGREP_SERVER) reports no served commit: only a --store-docs index \
         records one",
        proxy.addr
    );
    for methods in [
        &["query", "status", "query"][..],
        &["status", "query"],
        &["status", "query"],
    ] {
        let (out, asked) = counted(&r, &proxy, &r.client, &args);
        assert_eq!(asked, methods);
        assert_eq!(
            (stdout(&out), stderr(&out)),
            (ALL.to_owned(), ignored(&why))
        );
    }
    let first = Proxy::new(&addr);
    for _ in 0..2 {
        let (_, asked) = counted(&r, &first, &r.client, &args);
        assert_eq!(asked, ["status", "query"]);
    }
}

/// A damaged pin is not used: the search says why, asks for the status,
/// answers as a first search does, and pins the commit again.
#[test]
fn a_damaged_pin_says_why_and_asks_for_the_status() {
    let r = Remote::new(&FILES);
    let proxy = Proxy::new(&r.addr);
    let args = ["-l", "printk"];
    counted(&r, &proxy, &r.client, &args);
    let (want, asked) = counted(&r, &proxy, &r.client, &args);
    assert_eq!(asked, ["query"]);
    let pin = pin_of(&r, &proxy.addr);
    for (bytes, why) in [
        ("{", "is malformed: "),
        ("{\"v\":9}", "was written by another fmgrep version"),
    ] {
        std::fs::write(&pin, bytes).unwrap();
        let (out, asked) = counted(&r, &proxy, &r.client, &args);
        assert_eq!(asked, ["status", "query"]);
        let text = stderr(&out);
        let first = format!("warning: the pin {} {why}", pin.display());
        let asked = format!(
            "; asked the server at {} (FMGREP_SERVER) for its status",
            proxy.addr
        );
        let line = text.lines().next().unwrap_or_default();
        assert!(line.starts_with(&first) && line.ends_with(&asked), "{text}");
        let (printed, warned, code) = seen(&out, &proxy.addr);
        let rest: String = warned.lines().skip(1).map(|l| format!("{l}\n")).collect();
        assert_eq!((printed, rest, code), seen(&want, &proxy.addr));
        let (_, asked) = counted(&r, &proxy, &r.client, &args);
        assert_eq!(asked, ["query"]);
    }
}

/// Searches at once, with no pin and with one, and after the server
/// moved, each answer right, and leave a pin that the next search uses.
#[test]
fn searches_at_once_answer_right() {
    let r = Remote::new(&FILES);
    let proxy = Proxy::new(&r.addr);
    let at_once = |want: &str| {
        let runs: Vec<_> = (0..4)
            .map(|_| {
                let mut cmd = r.command(&proxy.addr, &r.client, &["-l", "printk"]);
                cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
                cmd.spawn().unwrap()
            })
            .collect();
        let outs: Vec<Output> = runs
            .into_iter()
            .map(|c| c.wait_with_output().unwrap())
            .collect();
        for out in outs {
            assert_eq!(
                (stdout(&out), out.status.code()),
                (want.to_owned(), Some(0)),
                "{}",
                stderr(&out)
            );
            let text = stderr(&out);
            assert!(!text.lines().any(|l| l.starts_with("error")), "{text}");
        }
        proxy.methods();
        let (out, asked) = counted(&r, &proxy, &r.client, &["-l", "printk"]);
        assert_eq!(
            (stdout(&out), asked),
            (want.to_owned(), vec!["query".to_owned()])
        );
    };
    at_once(ALL);
    at_once(ALL);
    r.s.write(&r.origin, &[("dir/n.c", "printk n\n")]);
    r.s.commit(&r.origin, "second");
    r.s.git(&r.client, &["pull", "-q", "--ff-only"]);
    let (index, _) = r.s.index(&r.origin, "idx2");
    let (_moved, addr) = r.s.serve(&index, &[]);
    proxy.point(&addr);
    at_once("a.c\ndir/b.c\ndir/n.c\ndir/sub/c.h\ne.c\n");
}

/// A pinned query refused by a server of another format, or by a server
/// that knows no pinned query (v0.1.0 answers -32602 to the fields of a
/// streamed query and has no format), fails as a first search does, after
/// one status; one of the pinned commit that refuses it says that the
/// server changed its index during the search, and one that fails before
/// any frame fails the search with its own error, and no output.
#[test]
fn a_refused_pinned_query_fails_as_a_first_search_does() {
    let r = Remote::new(&FILES);
    let proxy = Proxy::new(&r.addr);
    let (_, asked) = counted(&r, &proxy, &r.client, &["-l", "printk"]);
    assert_eq!(asked, ["status", "query"]);
    let format = r.s.status(&r.addr)["format"].as_u64().unwrap();
    let status = |format: Option<u64>| {
        let format = format
            .map(|f| format!("\"format\":{f},"))
            .unwrap_or_default();
        format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{{format}\"store_docs\":true,\"files\":1,\
             \"num_bytes\":1,\"index_size_bytes\":1,\"load_us\":1,\"root\":\"/x\",\
             \"commit\":{{\"vcs\":\"git\",\"id\":\"{}\"}}}}}}",
            r.commit
        )
    };
    let refused = |code: i64| {
        format!("{{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{{\"code\":{code},\"message\":\"m\"}}}}")
    };
    let server = format!("the server at {} (FMGREP_SERVER)", proxy.addr);
    for (replies, error) in [
        (
            [refused(-32602), status(None)],
            format!(
                "error: {server} reports no index format; it is older than this fmgrep (use one \
                 fmgrep version on both sides)"
            ),
        ),
        (
            [refused(-32001), status(Some(format + 1))],
            format!(
                "error: {server} serves an index of format {}; this fmgrep reads format {format} \
                 (use one fmgrep version on both sides)",
                format + 1
            ),
        ),
    ] {
        proxy.point(&scripted(replies.to_vec()));
        let out = r.once(&proxy.addr, &r.client, &["-l", "printk"]);
        assert_eq!(proxy.methods(), ["query", "status"]);
        assert_eq!(
            (stdout(&out), stderr(&out), out.status.code()),
            (String::new(), format!("{error}\n"), Some(2))
        );
    }
    proxy.point(&scripted(vec![refused(-32001), status(Some(format))]));
    let out = r.once(&proxy.addr, &r.client, &["-l", "printk"]);
    assert_eq!(proxy.methods(), ["query", "status"]);
    let error =
        format!("error: {server} changed its index during this search; run the search again");
    assert_eq!(
        stderr(&out).lines().last(),
        Some(error.as_str()),
        "{}",
        stderr(&out)
    );
    assert_eq!((stdout(&out), out.status.code()), (String::new(), Some(2)));
    // A server of the pinned commit that fails before any frame: its own error.
    proxy.point(&scripted(vec![refused(-32000), status(Some(format))]));
    let out = r.once(&proxy.addr, &r.client, &["-l", "printk"]);
    assert_eq!(proxy.methods(), ["query", "status"]);
    assert_eq!(
        (stdout(&out), stderr(&out).lines().last().map(str::to_owned)),
        (String::new(), Some("error: server: m".to_owned())),
        "{}",
        stderr(&out)
    );
    assert_eq!(out.status.code(), Some(2));
}

/// Under `FMGREP_VERBOSE=2`, `args` from the client's top through `proxy`,
/// and the methods that it sent, of the last try.
fn planned(r: &Remote, proxy: &Proxy, args: &[&str]) -> (Output, Vec<String>) {
    let mut methods = Vec::new();
    let out = r.tried(|| {
        proxy.methods();
        let mut cmd = r.command(&proxy.addr, &r.client, args);
        let out = cmd.env("FMGREP_VERBOSE", "2").output().unwrap();
        methods = proxy.methods();
        out
    });
    (out, methods)
}

/// Under `FMGREP_VERBOSE=2` a later search prints the plan of the files
/// it searches itself where a search that asked for the status prints it:
/// after the lines that it held until the server answered.
#[test]
fn a_later_search_prints_its_plan_in_place() {
    let r = Remote::new(&FILES);
    r.s.write(&r.client, &[("a.c", "printk edited\n")]);
    let proxy = Proxy::new(&r.addr);
    let args = ["--and", "printk", "/ed+/"];
    planned(&r, &proxy, &args);
    let (got, asked) = planned(&r, &proxy, &args);
    assert_eq!(asked, ["query"]);
    let fresh = Proxy::new(&r.addr);
    let (want, asked) = planned(&r, &fresh, &args);
    assert_eq!(asked, ["status", "query"]);
    let costs = regex::Regex::new(r"\d+(\.\d+)? ?(µs|ms)").unwrap();
    // Less the note on the status, which differs.
    let plain = |out: &Output, addr: &str| {
        let (printed, text, code) = seen(out, addr);
        let text: String = (text.lines())
            .filter(|l| !l.starts_with("note: sent the query") && !l.starts_with("note: asked"))
            .map(|l| format!("{l}\n"))
            .collect();
        (printed, costs.replace_all(&text, "T").into_owned(), code)
    };
    assert!(stderr(&got).contains("\nplan: "), "{}", stderr(&got));
    assert_eq!(plain(&got, &proxy.addr), plain(&want, &fresh.addr));
}

/// A later search that has no file of its own to merge asks for the
/// server's own output, in frames of bytes; one that merges files of its
/// own asks for a frame per file. Both are pinned, and print what a
/// search that asked for the status prints.
#[test]
fn a_pinned_query_asks_for_what_the_search_needs() {
    let r = Remote::new(&FILES);
    let proxy = Proxy::new(&r.addr);
    let args = ["-n", "printk"];
    let (_, sent) = requested(&r, &proxy, &r.client, &args);
    assert_eq!(kinds(&sent), ["status", "query pinned rendered"]);
    for (edited, query) in [(false, "query pinned rendered"), (true, "query pinned")] {
        if edited {
            r.s.write(&r.client, &[("a.c", "printk edited\n")]);
        }
        let fresh = Proxy::new(&r.addr);
        let (want, sent) = requested(&r, &fresh, &r.client, &args);
        assert_eq!(kinds(&sent), ["status", query]);
        let (got, sent) = requested(&r, &proxy, &r.client, &args);
        assert_eq!(kinds(&sent), [query]);
        assert_eq!(sent[0]["params"]["commit"], r.commit.as_str());
        assert_eq!(seen(&got, &proxy.addr), seen(&want, &fresh.addr));
    }
}

/// A server that moved to another commit refuses the pinned query before
/// any frame, whether the query asks for frames of bytes (a checkout with
/// no file of its own to merge) or a frame per file (one with a file of
/// its own): the status is asked and the search starts again from it, so
/// that what prints is what a first search prints, and nothing of the old
/// commit.
#[test]
fn a_moved_server_refuses_either_kind_of_pinned_query() {
    for (edited, first) in [(false, "query pinned rendered"), (true, "query pinned")] {
        let r = Remote::new(&FILES);
        let proxy = Proxy::new(&r.addr);
        let args = ["-n", "printk"];
        counted(&r, &proxy, &r.client, &args);
        r.s.write(&r.origin, &[("a.c", "none\n"), ("dir/n.c", "printk n\n")]);
        r.s.commit(&r.origin, "second");
        r.s.git(&r.client, &["fetch", "-q", "origin"]);
        if edited {
            r.s.write(&r.client, &[("e.c", "printk e2\n")]);
        }
        let (index, _) = r.s.index(&r.origin, "idx2");
        let (_moved, addr) = r.s.serve(&index, &[]);
        proxy.point(&addr);
        let new = r.s.status(&addr)["commit"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        // Under FMGREP_VERBOSE=1 the first try holds a note that names the
        // old commit, and the second prints one that names the new.
        let mut sent = Vec::new();
        let out = r.tried(|| {
            proxy.requests();
            let mut cmd = r.command(&proxy.addr, &r.client, &args);
            let out = cmd.env("FMGREP_VERBOSE", "1").output().unwrap();
            sent = proxy.requests();
            out
        });
        let sent = kinds(&sent);
        assert_eq!(sent[..2], [first, "status"], "{sent:?}");
        assert_eq!(sent.len(), 3, "{sent:?}");
        let want = r.oracle("", &args);
        assert_eq!(comparable(&stdout(&out)), comparable(&stdout(&want)));
        let text = stderr(&out);
        let notes: Vec<&str> = (text.lines())
            .filter(|l| l.starts_with("note: local changes from "))
            .collect();
        assert_eq!(notes.len(), 1, "{text}");
        assert!(
            notes[0].contains(&format!("served commit {}", &new[..7])),
            "{text}"
        );
        // The note on why the status was asked names the old commit, once.
        let named: Vec<&str> = (text.lines())
            .filter(|l| l.contains(&r.commit[..7]))
            .collect();
        assert_eq!(named.len(), 1, "{text}");
        assert!(named[0].contains("it no longer serves"), "{text}");
        assert!(!text.lines().any(|l| l.starts_with("error")), "{text}");
    }
}

/// A server that fails after its first frame: the search has printed, and
/// is not asked again. The lines held until the server answered print, the
/// status is not asked, and the error is the server's, with frames of
/// bytes and with a frame per file.
#[test]
fn a_failure_after_the_first_frame_asks_for_no_status() {
    let r = Remote::new(&FILES);
    let proxy = Proxy::new(&r.addr);
    let (_, asked) = counted(&r, &proxy, &r.client, &["-l", "printk"]);
    assert_eq!(asked, ["status", "query"]);
    let failed = "{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":-32000,\"message\":\"m\"}}";
    for (edited, frame) in [(false, "4\na.c\n"), (true, "18\n{\"term\":\"printk\"}\n")] {
        if edited {
            r.s.write(&r.client, &[("a.c", "printk edited\n")]);
        }
        proxy.point(&scripted(vec![format!("{frame}{failed}")]));
        proxy.requests();
        let out = r.tried(|| {
            let mut cmd = r.command(&proxy.addr, &r.client, &["-l", "printk"]);
            cmd.env("FMGREP_VERBOSE", "1").output().unwrap()
        });
        let text = stderr(&out);
        assert_eq!(
            proxy.methods().last().map(String::as_str),
            Some("query"),
            "{text}"
        );
        assert!(text.contains("note: local changes"), "{text}");
        assert_eq!(text.lines().last(), Some("error: server: m"), "{text}");
        assert_eq!(out.status.code(), Some(2), "{text}");
    }
}

/// Under `FMGREP_VERBOSE=1`, a search from a checkout says whether it
/// asked for the status, and why; from no checkout it says nothing of it.
#[test]
fn fmgrep_verbose_says_whether_the_status_was_asked() {
    let r = Remote::new(&FILES);
    let proxy = Proxy::new(&r.addr);
    let note = |cwd: &Path| {
        let out = r.tried(|| {
            let mut cmd = r.command(&proxy.addr, cwd, &["-l", "printk"]);
            cmd.env("FMGREP_VERBOSE", "1").output().unwrap()
        });
        assert!(out.status.success(), "{}", stderr(&out));
        let text = stderr(&out);
        let notes: Vec<String> = (text.lines())
            .filter(|l| l.starts_with("note: ") && !l.starts_with("note: local changes from "))
            .map(str::to_owned)
            .collect();
        assert!(notes.len() <= 1, "{text}");
        notes.into_iter().next().unwrap_or_default()
    };
    let server = format!("the server at {} (FMGREP_SERVER)", proxy.addr);
    let asked = |why: &str| format!("note: asked {server} for its status ({why})");
    assert_eq!(note(&r.client), asked("no pin of an earlier search"));
    let pin = pin_of(&r, &proxy.addr);
    let short = &r.commit[..7];
    let pinned = format!(
        "note: sent the query pinned to the served commit {short}, which the pin {} keeps, with \
         no status request",
        pin.display()
    );
    assert_eq!(note(&r.client.join("dir")), pinned);
    let away = std::fs::canonicalize(r.s.path("home")).unwrap();
    assert_eq!(note(&away), "");
    // A search that fails before a result, through a server that still
    // serves the pinned commit.
    let mut cmd = r.command(&proxy.addr, &r.client, &["--regex", "(", "-l"]);
    let out = cmd.env("FMGREP_VERBOSE", "1").output().unwrap();
    let text = stderr(&out);
    let failed = asked("the search failed before a result came");
    assert!(text.lines().any(|l| l == failed), "{text}");
    assert_eq!(out.status.code(), Some(2), "{text}");

    let output = r.s.path("idx-files").display().to_string();
    let built = r.s.run(&r.origin, &["index", ".", "--output", &output]);
    assert!(built.status.success(), "{}", stderr(&built));
    let (_files, addr) = r.s.serve(&r.s.path("idx-files"), &["--no-watch"]);
    proxy.point(&addr);
    let moved = format!("it no longer serves {short}, the commit that the pin keeps");
    assert_eq!(note(&r.client), asked(&moved));
    assert_eq!(note(&r.client), asked("no pin of an earlier search"));
}
