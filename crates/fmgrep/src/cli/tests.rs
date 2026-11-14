//! Which index or server a search reads.

use std::path::PathBuf;

use super::*;
use crate::manifest::{BLOBS_SHARD, MANIFEST_BIN};

fn unset() -> Result<Option<String>, String> {
    Ok(None)
}

fn set() -> Result<Option<String>, String> {
    Ok(Some("build1:7777".to_owned()))
}

fn bad() -> Result<Option<String>, String> {
    Err("error: FMGREP_SERVER is bad".to_owned())
}

fn never() -> Result<Option<String>, String> {
    panic!("FMGREP_SERVER was read")
}

/// `<tmp>/tree/.fmgrep`, an index as `locate` sees one, and `<tmp>/away`.
fn trees() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dotdir = tmp.path().join("tree").join(INDEX_DIR);
    std::fs::create_dir_all(&dotdir).unwrap();
    std::fs::write(dotdir.join(MANIFEST_BIN), "").unwrap();
    std::fs::write(dotdir.join(BLOBS_SHARD), "").unwrap();
    let away = tmp.path().join("away");
    std::fs::create_dir(&away).unwrap();
    (tmp, dotdir.parent().unwrap().to_path_buf(), away)
}

/// The index dir and note of a choice, or the server.
fn named(choice: Result<Choice, String>) -> Result<(Option<PathBuf>, Option<String>), String> {
    match choice? {
        Choice::Index { found, note } => Ok((Some(found.index_dir), note)),
        Choice::Server(s) => Ok((None, Some(s.to_string()))),
    }
}

#[test]
fn a_flag_wins_and_reads_no_fmgrep_server() {
    let (_tmp, tree, away) = trees();
    let got = named(choose(None, Some("h:1"), never, &tree));
    assert_eq!(got, Ok((None, Some("the server at h:1".to_owned()))));
    let got = named(choose(Some(&tree), None, never, &away));
    let dotdir = std::fs::canonicalize(&tree).unwrap().join(INDEX_DIR);
    assert_eq!(got, Ok((Some(dotdir), None)));
    let missing = away.join("missing");
    let got = named(choose(Some(&missing), None, never, &tree));
    assert!(got.unwrap_err().starts_with("error: cannot resolve"));
}

#[test]
fn an_index_found_by_walking_up_wins_over_fmgrep_server() {
    let (_tmp, tree, away) = trees();
    let dotdir = std::fs::canonicalize(&tree).unwrap().join(INDEX_DIR);
    let note = ", not the server at build1:7777 (FMGREP_SERVER)".to_owned();
    let cases: [(Env, _); 3] = [(unset, None), (set, Some(note)), (bad, None)];
    for (env, want) in cases {
        let got = named(choose(None, None, env, &tree));
        assert_eq!(got, Ok((Some(dotdir.clone()), want)));
    }
    let server = "the server at build1:7777 (FMGREP_SERVER)".to_owned();
    assert_eq!(
        named(choose(None, None, set, &away)),
        Ok((None, Some(server)))
    );
    // Only where an ancestor of the temp dir holds no index.
    if locate(&away).is_err() {
        let err = named(choose(None, None, bad, &away)).unwrap_err();
        assert_eq!(err, "error: FMGREP_SERVER is bad");
        let err = named(choose(None, None, unset, &away)).unwrap_err();
        assert!(err.contains("no .fmgrep index"), "{err}");
    }
}

/// A port nothing listens on.
fn closed() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().to_string()
}

#[test]
fn a_server_that_cannot_be_reached_falls_back_only_when_a_flag_gave_it() {
    let (_tmp, tree, away) = trees();
    let addr = closed();
    let flag = Server {
        addr: addr.clone(),
        env: false,
    };
    let Ok(Reached::Index { found, note }) = reach(&flag, &tree) else {
        panic!("an index above answers");
    };
    assert_eq!(found.root, Some(std::fs::canonicalize(&tree).unwrap()));
    let want = format!(": cannot reach the server at {addr} (");
    assert!(note.starts_with(&want), "{note}");
    let env = Server { addr, env: true };
    let Err(e) = reach(&env, &tree) else {
        panic!("FMGREP_SERVER does not fall back");
    };
    assert!(e.starts_with("error: cannot reach the server at "), "{e}");
    assert!(e.contains("(FMGREP_SERVER): "), "{e}");
    if locate(&away).is_err() {
        assert!(matches!(reach(&flag, &away), Err(e) if e.starts_with("error: cannot reach")));
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let up = Server {
        addr: listener.local_addr().unwrap().to_string(),
        env: false,
    };
    assert!(matches!(reach(&up, &tree), Ok(Reached::Conn(_))));
}
