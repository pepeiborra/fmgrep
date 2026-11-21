use super::{Plan, plan, short, unopened};
use crate::diagnostic::Diagnostic;
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

/// Mode `mode` on `path`.
#[cfg(unix)]
fn chmod(path: &std::path::Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// A path of the skip list under a directory this process cannot search
/// is left out with a warning, as one that cannot be read is, not dropped
/// without a word.
#[cfg(unix)]
#[test]
fn a_path_of_the_skip_list_below_a_closed_directory_gets_a_warning() {
    let tmp = tempfile::tempdir().unwrap();
    let top = std::fs::canonicalize(tmp.path()).unwrap();
    std::fs::create_dir(top.join("shut")).unwrap();
    std::fs::write(top.join("shut/x.c"), b"printk").unwrap();
    std::fs::write(top.join("y.c"), b"printk").unwrap();
    chmod(&top.join("shut"), 0o000);
    let closed = std::fs::symlink_metadata(top.join("shut/x.c")).is_err();
    let skip = ["gone.c", "shut/x.c", "y.c"].map(str::to_owned);
    let mut said = Vec::new();
    let local = super::resolve(&top, &skip, ID, &mut said);
    chmod(&top.join("shut"), 0o755);
    if !closed {
        println!("skipped: the directory can be searched (running as root)");
        return;
    }
    assert_eq!(local.unwrap().files, ["y.c"], "gone.c is no news");
    assert_eq!(
        said,
        [Diagnostic::Skipped {
            path: top.join("shut/x.c"),
            why: "cannot stat (Permission denied (os error 13))".to_owned(),
        }]
    );
}

/// A directory of the skip list that this process cannot list is named, and
/// the files around it are still searched.
#[cfg(unix)]
#[test]
fn a_directory_of_the_skip_list_that_cannot_be_listed_is_named() {
    let tmp = tempfile::tempdir().unwrap();
    let top = std::fs::canonicalize(tmp.path()).unwrap();
    std::fs::create_dir_all(top.join("ut/sub")).unwrap();
    std::fs::write(top.join("ut/u.c"), b"printk").unwrap();
    std::fs::write(top.join("ut/sub/s.c"), b"printk").unwrap();
    chmod(&top.join("ut/sub"), 0o000);
    let listable = std::fs::read_dir(top.join("ut/sub")).is_ok();
    let mut said = Vec::new();
    let local = super::resolve(&top, &["ut/".to_owned()], ID, &mut said);
    chmod(&top.join("ut/sub"), 0o755);
    if listable {
        println!("skipped: the directory can be listed (running as root)");
        return;
    }
    assert_eq!(local.unwrap().files, ["ut/u.c"]);
    assert_eq!(
        said,
        [Diagnostic::Unlisted {
            dir: top.join("ut/sub"),
            why: "Permission denied (os error 13)".to_owned(),
        }]
    );
}

fn skips(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_directory_git_cannot_open_is_named_once() {
    let nd = "warning: could not open directory 'nd/': Permission denied\n";
    let ut = "warning: could not open directory 'ut/sub/': Permission denied\n";
    let once = vec![Diagnostic::Unopened {
        dir: "nd/".to_owned(),
        why: "Permission denied".to_owned(),
    }];
    let skip = skips(&["a.c", "ut/"]);
    // The shape of a filesystem that lists `u.c` first, and of one that
    // lists `sub` first.
    assert_eq!(unopened(nd, &skip), once);
    assert_eq!(unopened(&format!("{nd}{ut}"), &skip), once);
    // A name that only starts like a walked directory is not below it.
    let near = "warning: could not open directory 'utx/': Permission denied\n\
                warning: could not open directory 'ut-b/': Permission denied\n";
    assert_eq!(unopened(near, &skips(&["ut/"])).len(), 2);
    // A walked root names everything below it.
    assert!(unopened(&format!("{nd}{ut}"), &skips(&["/"])).is_empty());
    // Other lines of git's stderr are not warnings of this kind.
    assert!(unopened("d/f.c: Permission denied\n", &skip).is_empty());
}

/// Git writes a directory's name as it is, a newline included, so its
/// warning can take two lines. A warning that cannot be read goes on as
/// git wrote it: a search that left a directory out is never silent.
#[test]
fn a_directory_name_with_a_newline_is_read_whole() {
    let both = "warning: could not open directory 'two\nlines/': Permission denied\n\
                warning: could not open directory 'nd/': Permission denied\n";
    let denied = |dir: &str| Diagnostic::Unopened {
        dir: dir.to_owned(),
        why: "Permission denied".to_owned(),
    };
    assert_eq!(
        unopened(both, &skips(&["a.c"])),
        [denied("two\nlines/"), denied("nd/")]
    );
    // A record that stops before its reason is kept as git wrote it, and
    // the warning after it is read on its own.
    let cut = "warning: could not open directory 'two\n";
    assert_eq!(
        unopened(cut, &[]),
        [Diagnostic::Git("could not open directory 'two".to_owned())]
    );
    let cut_then_whole =
        format!("{cut}warning: could not open directory 'nd/': Permission denied\n");
    assert_eq!(
        unopened(&cut_then_whole, &[]),
        [
            Diagnostic::Git("could not open directory 'two".to_owned()),
            denied("nd/")
        ]
    );
}
