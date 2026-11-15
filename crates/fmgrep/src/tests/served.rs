//! A server's re-check before each query, on one set of marks that lives
//! across queries, against a private daemon; skipped without watchman.

use super::watched::{
    AB, TAKES, TRIES, came_late, cannot, clock, give_back, indexed, late, notes, put, records,
    tried,
};
use super::watchman::Daemon;
use super::*;
use crate::fetch::DocStore;
use crate::source::Learned;

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
    /// Why the server last checked every file, as it said it.
    said: Option<String>,
    /// Whether the last [`Self::asked`] made a query again that came late.
    came_late: bool,
}

/// The warnings given since there were `from`, left for [`notes`].
fn since(from: usize) -> Vec<String> {
    NOTES.with_borrow(|n| n.lock().unwrap()[from..].to_vec())
}

fn noted() -> usize {
    NOTES.with_borrow(|n| n.lock().unwrap().len())
}

fn serving(index: &Path) -> Served {
    let from = noted();
    let manifest = read_manifest(index).unwrap();
    let docs = DocStore::open(index, &manifest, None, crate::fetch::Ask::EachQuery).unwrap();
    let files = manifest.files.checked().unwrap();
    if !matches!(docs.replay(files), Learned::Answered { .. }) {
        docs.discover(files);
        docs.stat_all(files);
    }
    // A start that took no token says why; a query says it again only
    // when the reason changes.
    let cannot = "watchman cannot say what changed (";
    let said = since(from).into_iter().find(|n| n.starts_with(cannot));
    Served {
        docs,
        files: manifest.files,
        said,
        came_late: false,
    }
}

/// What a server said last, after it said `got` before a query: a
/// reason to check every file, until it says [`AGAIN`]. A server says a
/// reason when it changes, and [`AGAIN`] once watchman answers after one.
fn heard(said: &mut Option<String>, got: &[String]) {
    let reason = got
        .iter()
        .find(|n| n.as_str() != AGAIN && !n.contains(" ignore file"));
    if let Some(reason) = reason {
        *said = Some(reason.clone());
    } else if got.iter().any(|n| n.as_str() == AGAIN) {
        *said = None;
    }
}

/// `run`, a query of a server that asks `daemon` and last said `said`,
/// made again while it came late, at most [`TRIES`] times: on a loaded
/// machine the product's 100 ms for its calls to watchman can run out. A
/// late query checks every file and says so, or says nothing while the
/// reason holds. [`Daemon::sent`] skips the late tries' commands. Each late
/// try must answer as the last one. Of the last try's warnings, those that
/// follow from the late tries are dropped: [`AGAIN`], where the server had
/// said nothing, or a reason it had said already. Returns the answer and
/// whether a try came late; the last try's warnings are left for
/// [`notes`]. A late try may take a new token and check every file; the
/// next try then has no reason of its own to say. Fails, with the late
/// warning, when every try came late.
pub(super) fn served_on_time<T: PartialEq + std::fmt::Debug>(
    daemon: &Daemon,
    said: &mut Option<String>,
    mut run: impl FnMut() -> T,
) -> (T, bool) {
    let kept = notes();
    let before = said.clone();
    let mut lates = Vec::new();
    loop {
        let from = daemon.fence();
        let answer = run();
        let mut got = notes();
        heard(said, &got);
        if *said != Some(late()) {
            if !lates.is_empty() {
                got.retain(|n| Some(n) != before.as_ref() && (before.is_some() || n != AGAIN));
            }
            for late in &lates {
                assert_eq!(*late, answer, "a late query answers the same");
            }
            give_back(kept, got);
            return (answer, !lates.is_empty());
        }
        assert!(
            lates.len() + 1 < TRIES,
            "{TRIES} tries came late: {}",
            late()
        );
        daemon.skip_since(from);
        lates.push(answer);
        came_late(lates.len());
    }
}

impl Served {
    /// One query's re-check: the paths it reads in full (stale, then
    /// added) and those it drops; then the record, as a query saves it.
    fn query(&mut self) -> (Vec<String>, Vec<String>) {
        let from = noted();
        let answer = self.recheck();
        heard(&mut self.said, &since(from));
        answer
    }

    fn recheck(&mut self) -> (Vec<String>, Vec<String>) {
        self.docs.recheck(self.files.checked().unwrap());
        self.docs.begin();
        let paths = self.docs.paths(&self.files);
        let name = |&id: &usize| paths.path(id).unwrap().unwrap().to_owned();
        let read = self.docs.stale().iter().map(name).collect();
        let gone = self
            .docs
            .changed()
            .iter()
            .filter(|&&id| self.docs.is_gone(id));
        let gone = gone.map(name).collect();
        self.docs.checkpoint(&self.files).unwrap();
        (read, gone)
    }

    fn read(&mut self) -> Vec<String> {
        self.query().0
    }

    /// [`Self::query`], by [`served_on_time`].
    fn asked(&mut self, daemon: &Daemon) -> (Vec<String>, Vec<String>) {
        let mut said = self.said.take();
        let (answer, late) = served_on_time(daemon, &mut said, || self.recheck());
        (self.said, self.came_late) = (said, late);
        answer
    }

    /// [`Self::read`], by [`served_on_time`].
    fn read_asked(&mut self, daemon: &Daemon) -> Vec<String> {
        self.asked(daemon).0
    }
}

/// A server started on a tree [`indexed`] while a private daemon watches
/// it, after its first query. A start that came late is made again.
fn started(tmp: &Path) -> Option<(PathBuf, Served, Daemon)> {
    let (folder, output, daemon) = indexed(tmp, AB)?;
    let (mut served, _) = tried(&daemon, || serving(&output));
    assert_eq!(notes(), [ONCE], "no record yet");
    assert_eq!(served.asked(&daemon), (vec![], vec![]));
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
    assert_eq!(served.asked(&daemon), (read, vec!["a.c".to_owned()]));
    assert_eq!(daemon.sent(), ASKS, "the token question, since the last");
    put(&folder, "n/.ignore", b"deep/\n");
    let read = served.read_asked(&daemon);
    assert_eq!(read, ["b.c"], "the new dir's ignore file");
    std::fs::remove_file(folder.join("n/.ignore")).unwrap();
    assert_eq!(served.read_asked(&daemon), ["b.c", "n/deep/n.c"]);
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
    let Some((folder, output, daemon)) = indexed(tmp.path(), docs) else {
        return;
    };
    let (mut served, _) = tried(&daemon, || serving(&output));
    assert_eq!(notes(), [ONCE]);
    assert_eq!(served.asked(&daemon), (vec![], vec![]));
    assert!(notes().is_empty(), "no ignore file changed");
    put(&folder, ".ignore", b"*.o\nb.c\n");
    served.asked(&daemon);
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
    assert_eq!(served.read_asked(&daemon), ["b.c"]);
    let once = "checked every file once (watchman restarted, added the watch again or pruned \
                deleted files since the server last asked it); the next query asks watchman";
    // A try late past the new token checked every file, and said so: the
    // try after it asks since that token, and has nothing more to say.
    let said = notes();
    let taken = said.is_empty() && served.came_late;
    assert!(said == [once] || taken, "{said:?}");
    put(&folder, "c.c", b"printk\n");
    assert_eq!(served.read_asked(&daemon), ["b.c", "c.c"]);
    let again: &[&str] = if taken { &[] } else { &[AGAIN] };
    assert_eq!(notes(), again);
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
    assert_eq!(served.read_asked(&daemon), ["b.c", "c.c"]);
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
    assert_eq!(served.read_asked(&daemon), ["b.c", "c.c"]);
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
    assert_eq!(served.read_asked(&daemon), ["b.c", "c.c"]);
    let once = "checked every file once (no token from an earlier query); the next query asks \
                watchman";
    assert_eq!(notes(), [once]);
    daemon.sent();
    assert_eq!(served.read_asked(&daemon), ["b.c", "c.c"]);
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
    assert_eq!(served.read_asked(&daemon).len(), 65);
    let over = "more than 64 paths changed since the last query; checked every file";
    // A try late past the new token checked every file, and said so: the
    // try after it asks since that token, and has nothing more to say.
    let said = notes();
    let taken = said.is_empty() && served.came_late;
    assert!(said == [over] || taken, "{said:?}");
    put(&folder, "b.c", b"printk now\n");
    assert_eq!(served.read_asked(&daemon).len(), 66);
    let again: &[&str] = if taken { &[] } else { &[AGAIN] };
    assert_eq!(notes(), again);
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
    let (mut served, _) = tried(&daemon, || serving(&output));
    assert_eq!(notes(), [ONCE]);
    put(&folder, "b.c", b"printk now\n");
    assert_eq!(served.read_asked(&daemon), ["b.c"]);
    assert_eq!(daemon.sent(), [&TAKES[..], &ASKS].concat());
}
