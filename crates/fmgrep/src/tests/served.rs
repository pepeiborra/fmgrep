//! A server's re-check before each query, on one set of marks that lives
//! across queries, against a private daemon; skipped without watchman.

use super::watched::{AB, TAKES, cannot, clock, indexed, notes, put, records};
use super::*;
use crate::fetch::DocStore;

/// The requests of an ask with a token: a new token, the token question.
const ASKS: [&str; 5] = ["watch-list", "get-config", "clock", "clock", "query"];

const AGAIN: &str = "each query re-checks only what watchman reports again";

/// What a server's start says when it had no record.
const ONCE: &str =
    "checked every file once (no record of an earlier search); the next query asks watchman";

/// The docs of a server of `index`, loaded as `serve` loads them.
struct Served {
    docs: DocStore,
    files: crate::manifest::FileTable,
}

fn serving(index: &Path) -> Served {
    let manifest = read_manifest(index).unwrap();
    let docs = DocStore::open(index, &manifest, None, crate::fetch::Ask::EachQuery).unwrap();
    if !docs.replay(&manifest.files) {
        docs.discover(&manifest.files);
        docs.stat_all(&manifest.files);
    }
    Served {
        docs,
        files: manifest.files,
    }
}

impl Served {
    /// One query's re-check: the paths it reads in full (stale, then
    /// added) and those it drops; then the record, as a query saves it.
    fn query(&mut self) -> (Vec<String>, Vec<String>) {
        self.docs.recheck(&self.files);
        self.docs.begin();
        let paths = self.docs.paths(&self.files);
        let name = |&id: &usize| paths.path(id).unwrap().to_owned();
        let read = self.docs.stale().iter().map(name).collect();
        let gone = self
            .docs
            .changed()
            .iter()
            .filter(|&&id| self.docs.is_gone(id));
        let gone = gone.map(name).collect();
        self.docs.checkpoint(&self.files);
        (read, gone)
    }

    fn read(&mut self) -> Vec<String> {
        self.query().0
    }
}

/// A server started on a tree [`indexed`] while a private daemon watches
/// it, after its first query.
fn started(tmp: &Path) -> Option<(PathBuf, Served, super::watchman::Daemon)> {
    let (folder, output, daemon) = indexed(tmp, AB)?;
    let mut served = serving(&output);
    assert_eq!(notes(), [ONCE], "no record yet");
    assert_eq!(served.query(), (vec![], vec![]));
    assert_eq!(daemon.sent(), [&TAKES[..], &ASKS].concat());
    assert!(notes().is_empty());
    Some((folder, served, daemon))
}

#[test]
fn each_query_sees_what_changed_before_it_and_the_record_is_saved_once() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, mut served, daemon)) = started(tmp.path()) else {
        return;
    };
    let first = clock(tmp.path());
    put(&folder, "b.c", b"printk now\n");
    put(&folder, "n/deep/n.c", b"printk\n");
    std::fs::remove_file(folder.join("a.c")).unwrap();
    let read = ["b.c", "n/deep/n.c"].map(String::from).to_vec();
    assert_eq!(served.query(), (read, vec!["a.c".to_owned()]));
    assert_eq!(daemon.sent(), ASKS, "the token question, since the last");
    put(&folder, "n/.ignore", b"deep/\n");
    assert_eq!(served.read(), ["b.c"], "the new dir's ignore file");
    std::fs::remove_file(folder.join("n/.ignore")).unwrap();
    assert_eq!(served.read(), ["b.c", "n/deep/n.c"]);
    assert_eq!(daemon.sent(), [&ASKS[..], &ASKS].concat());
    assert!(notes().is_empty());
    assert_eq!(records(tmp.path()).len(), 1);
    assert_eq!(clock(tmp.path()), first, "saved after the first query only");
}

/// A server's warning about an edited ignore file speaks of its queries.
#[test]
fn a_changed_ignore_file_warns_of_each_query() {
    let tmp = tempfile::tempdir().unwrap();
    let docs: &[(&str, &[u8])] = &[(".ignore", b"*.o\n"), ("a.c", b"printk\n"), ("b.c", b"x\n")];
    let Some((folder, output, _daemon)) = indexed(tmp.path(), docs) else {
        return;
    };
    let mut served = serving(&output);
    assert_eq!(notes(), [ONCE]);
    assert_eq!(served.query(), (vec![], vec![]));
    assert!(notes().is_empty(), "no ignore file changed");
    put(&folder, ".ignore", b"*.o\nb.c\n");
    served.query();
    let warning = "1 ignore file changed since the index; each query reads its directory again \
                   (re-run index to make searches fast again)";
    assert_eq!(notes(), [warning]);
}

#[test]
fn a_restart_makes_one_query_check_every_file() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, mut served, mut daemon)) = started(tmp.path()) else {
        return;
    };
    put(&folder, "b.c", b"printk now\n");
    daemon.restart();
    daemon.watch(&folder);
    assert_eq!(served.read(), ["b.c"]);
    let once = "checked every file once (watchman restarted, added the watch again or pruned \
                deleted files since the server last asked it); the next query asks watchman";
    assert_eq!(notes(), [once]);
    put(&folder, "c.c", b"printk\n");
    assert_eq!(served.read(), ["b.c", "c.c"]);
    assert_eq!(notes(), [AGAIN]);
    assert_eq!(daemon.sent(), [&ASKS[..], &ASKS].concat());
}

/// A late daemon leaves the token as it was: once it answers, the next
/// query asks since that token, and needs no new first token.
#[test]
fn a_hung_watchman_checks_every_file_and_keeps_the_last_token() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, mut served, daemon)) = started(tmp.path()) else {
        return;
    };
    daemon.hang();
    put(&folder, "b.c", b"printk now\n");
    let answers: Vec<_> = (0..3).map(|_| served.read()).collect();
    daemon.resume();
    assert_eq!(answers, vec![vec!["b.c".to_owned()]; 3]);
    let late = cannot("watchman did not answer within 100 ms");
    assert_eq!(notes(), [late], "once, while the reason holds");
    put(&folder, "c.c", b"printk\n");
    assert_eq!(served.read(), ["b.c", "c.c"]);
    assert_eq!(notes(), [AGAIN], "no first token: the kept one answered");
}

/// A server that starts while the record's daemon hangs keeps the
/// record's token: its start checks every file after that token was
/// taken. Once the daemon answers, the next query asks since that token,
/// with no new first token and no second check of every file.
#[test]
fn a_daemon_hung_at_the_start_leaves_the_record_token_to_ask_since() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, _first, daemon)) = started(tmp.path()) else {
        return;
    };
    let saved = clock(tmp.path());
    daemon.hang();
    let mut served = serving(&tmp.path().join("idx"));
    let late = cannot("watchman did not answer within 100 ms");
    assert_eq!(notes(), [late], "the start says why");
    put(&folder, "b.c", b"printk now\n");
    assert_eq!(served.read(), ["b.c"], "every file checked");
    assert!(notes().is_empty(), "the reason holds: said once");
    daemon.resume();
    put(&folder, "c.c", b"printk\n");
    assert_eq!(served.read(), ["b.c", "c.c"]);
    assert_eq!(notes(), [AGAIN], "asked since the record's token");
    assert_eq!(clock(tmp.path()), saved, "saved with the record's token");
}

/// A daemon gone from the token's socket drops the token; a daemon there
/// again gives a first one, and every file is checked once more.
#[test]
fn a_stopped_watchman_drops_the_token_until_a_daemon_gives_one() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, mut served, mut daemon)) = started(tmp.path()) else {
        return;
    };
    daemon.stop();
    put(&folder, "b.c", b"printk now\n");
    assert_eq!(served.read(), ["b.c"]);
    let got = notes();
    let gone = "watchman cannot say what changed (no watchman daemon answers on ";
    assert!(got.len() == 1 && got[0].starts_with(gone), "{got:?}");
    daemon.restart();
    daemon.watch(&folder);
    put(&folder, "c.c", b"printk\n");
    assert_eq!(served.read(), ["b.c", "c.c"]);
    let once = "checked every file once (no token from an earlier query); the next query asks \
                watchman";
    assert_eq!(notes(), [once]);
    daemon.sent();
    assert_eq!(served.read(), ["b.c", "c.c"]);
    assert_eq!(notes(), [AGAIN]);
    assert_eq!(daemon.sent(), ASKS);
}

#[test]
fn past_the_cap_one_query_checks_every_file() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, mut served, daemon)) = started(tmp.path()) else {
        return;
    };
    for i in 0..65 {
        put(&folder, &format!("n{i}.c"), b"printk\n");
    }
    assert_eq!(served.read().len(), 65);
    let over = "more than 64 paths changed since the last query; checked every file";
    assert_eq!(notes(), [over]);
    put(&folder, "b.c", b"printk now\n");
    assert_eq!(served.read().len(), 66);
    assert_eq!(notes(), [AGAIN]);
    assert_eq!(daemon.sent(), [&ASKS[..], &ASKS].concat());
}

/// A search of a tree this small asks watchman nothing; a server asks.
#[test]
fn a_server_asks_watchman_at_any_size() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((folder, output, daemon)) = indexed(tmp.path(), AB) else {
        return;
    };
    ASK_FROM.set(Some(3));
    assert_eq!(printed(&output, &["-l"], &["printk"]), "a.c\n");
    assert!(daemon.sent().is_empty() && notes().is_empty(), "a search");
    let mut served = serving(&output);
    assert_eq!(notes(), [ONCE]);
    put(&folder, "b.c", b"printk now\n");
    assert_eq!(served.read(), ["b.c"]);
    assert_eq!(daemon.sent(), [&TAKES[..], &ASKS].concat());
}
