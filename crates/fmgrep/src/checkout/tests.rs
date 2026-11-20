use super::{Plan, plan, short};
use crate::remote::Server;
use crate::wire::ServeStatus;

const ID: &str = "0123456789abcdef0123456789abcdef01234567";

/// The `status` of a `--store-docs` server whose served commit is `vcs`
/// and `id`.
fn status(vcs: &str, id: &str) -> ServeStatus {
    serde_json::from_value(serde_json::json!({
        "format": 9,
        "store_docs": true,
        "files": 1,
        "num_bytes": 1,
        "index_size_bytes": 1,
        "load_us": 1,
        "root": "/nowhere/.fmgrep",
        "commit": {"vcs": vcs, "id": id},
    }))
    .unwrap()
}

#[test]
fn a_short_id_is_cut_only_at_a_character() {
    assert_eq!(short(ID), "0123456");
    assert_eq!(short("1a2b"), "1a2b");
    assert_eq!(short(""), "");
    let odd = "\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}";
    assert_eq!(short(odd), odd);
}

/// Only a full git commit id is handed to git. Another VCS, a ref, or
/// text that is not hex makes the search the server's alone, with the
/// warning, before git runs: a ref such as `HEAD` would name a commit of
/// this checkout, not the served one.
#[test]
fn a_served_commit_that_is_not_a_git_id_is_not_asked_of_git() {
    let tmp = tempfile::tempdir().unwrap();
    let top = std::fs::canonicalize(tmp.path()).unwrap();
    // A checkout to the client, which git cannot read: git must not run.
    std::fs::create_dir(top.join(".git")).unwrap();
    let server = Server {
        addr: "127.0.0.1:1".to_owned(),
        env: true,
    };
    let upper = ID.to_uppercase();
    let sha256 = format!("{ID}{}", &ID[..24]);
    for (vcs, id) in [
        ("git", "HEAD"),
        ("git", "main"),
        ("git", "HEAD~0"),
        ("git", "-oops"),
        ("git", " 1a2b"),
        ("git", "1a2b c"),
        ("git", "\n1"),
        (
            "git",
            "\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}",
        ),
        ("git", "1a2b"),
        ("git", &ID[1..]),
        ("git", &upper),
        ("git", &sha256[1..]),
        ("sl", ID),
        ("svn", ID),
    ] {
        match plan(&server, &status(vcs, id), &top) {
            Plan::Degraded(why) => assert_eq!(
                why,
                "the server at 127.0.0.1:1 (FMGREP_SERVER) reports a served commit that is not \
                 a git commit id",
                "{vcs} {id:?}"
            ),
            _ => panic!("{vcs} {id:?} was compared"),
        }
    }
}

/// A server that serves no commit, from inside the tree it names: a
/// files-layout server reads that tree, so no warning; stored docs ignore
/// its edits, so the warning. From another checkout, in either layout:
/// the warning. From no checkout: none. The warning says why the server
/// serves no commit, when it can: the reason a `--store-docs` index
/// recorded, or that only a `--store-docs` index records one.
#[test]
fn a_server_without_a_served_commit_warns_unless_it_reads_the_tree() {
    let canon = |t: &tempfile::TempDir| std::fs::canonicalize(t.path()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let top = canon(&tmp);
    for dir in [".git", ".fmgrep", "dir"] {
        std::fs::create_dir(top.join(dir)).unwrap();
    }
    let inside = top.join("dir");
    let other = tempfile::tempdir().unwrap();
    let other = canon(&other);
    std::fs::create_dir(other.join(".git")).unwrap();
    let away = tempfile::tempdir().unwrap();
    let away = canon(&away);
    let server = Server {
        addr: "127.0.0.1:1".to_owned(),
        env: true,
    };
    let none = "the server at 127.0.0.1:1 (FMGREP_SERVER) reports no served commit";
    let index = top.join(".fmgrep");
    let submodule = "the checkout holds a submodule (agents/internal)";
    // The layout, the root, the reason the status carries, and the
    // warning from a checkout that is not the served tree.
    let cases = [
        (
            false,
            &top,
            None,
            format!("{none}: only a --store-docs index records one"),
        ),
        (true, &index, None, none.to_owned()),
        (
            true,
            &index,
            Some(submodule),
            format!("{none}: {submodule}"),
        ),
    ];
    for (store_docs, root, why, text) in cases {
        let status: ServeStatus = serde_json::from_value(serde_json::json!({
            "format": 9,
            "store_docs": store_docs,
            "files": 1,
            "num_bytes": 1,
            "index_size_bytes": 1,
            "load_us": 1,
            "root": root.display().to_string(),
            "why_no_commit": why,
        }))
        .unwrap();
        let warns = |cwd: &std::path::Path, want: &str| match plan(&server, &status, cwd) {
            Plan::Degraded(got) => {
                assert_eq!(got, want);
                true
            }
            Plan::Plain => false,
            _ => panic!("store_docs {store_docs}: {}: a checkout", cwd.display()),
        };
        assert_eq!(warns(&inside, &text), store_docs, "store_docs {store_docs}");
        assert!(warns(&other, &text), "store_docs {store_docs}");
        assert!(!warns(&away, &text), "store_docs {store_docs}");
    }
}
