use super::*;

use std::sync::Barrier;

fn live() -> (Cuts, Writer) {
    feed(CutSource::Shard(PathBuf::from("docs.shard")))
}

fn stamp(len: u64) -> Stamp {
    Stamp {
        len,
        mtime_ns: len as i64 * 10,
    }
}

/// A cut is served while the writer is still appending: the barrier below
/// deadlocks if `get` waited for the stream to end.
#[test]
fn cuts_close_before_the_stream_ends() {
    let (cuts, mut writer) = live();
    let served = Barrier::new(2);
    let wall = &served;
    std::thread::scope(|scope| {
        scope.spawn(move || {
            writer.pushed("a.c".to_string(), stamp(2), None, Some(0..1));
            wall.wait();
            writer.pushed("b.c".to_string(), stamp(3), None, Some(1..2));
            writer.pushed("c.c".to_string(), stamp(4), None, None);
            writer.end(Some(2..3), None);
        });
        let first = cuts.get(0).expect("cut 0 closes with the first document");
        assert_eq!((first.docs, first.at, first.lens), (0..1, 0, vec![2]));
        served.wait();
        let second = cuts.get(1).expect("cut 1 closes before the stream ends");
        assert_eq!((second.docs, second.at, second.lens), (1..2, 2, vec![3]));
        let last = cuts.get(2).expect("the trailing cut closes at the end");
        assert_eq!(
            (last.docs, last.at, last.lens, last.paths, last.mtimes),
            (2..3, 5, vec![4], vec!["c.c".to_string()], vec![40])
        );
        assert!(cuts.get(3).is_none());
        assert_eq!(cuts.num_docs(), 3);
        assert!(cuts.error().is_none());
    });
}

/// A stream that fails wakes every waiter with `None` and leaves its error
/// for the indexer to report.
#[test]
fn a_stream_failure_wakes_every_waiter() {
    let (cuts, writer) = live();
    let feed = &cuts;
    std::thread::scope(|scope| {
        let waiters: Vec<_> = (0..3)
            .map(|slot| scope.spawn(move || feed.get(slot).is_none()))
            .collect();
        writer.end(None, Some("error: cannot read a.c".to_string()));
        for waiter in waiters {
            assert!(waiter.join().unwrap());
        }
    });
    assert_eq!(cuts.error().as_deref(), Some("error: cannot read a.c"));
    assert!(cuts.shape().is_none());
}

/// The path a panicking stream takes: it never calls `end`, so only the
/// writer's drop guard can stop the waiters hanging forever — and it must
/// leave an error, or the empty plan reads as an empty corpus.
#[test]
fn a_dropped_writer_wakes_every_waiter() {
    let (cuts, writer) = live();
    let feed = &cuts;
    std::thread::scope(|scope| {
        let waiter = scope.spawn(move || feed.get(0).is_none());
        drop(writer);
        assert!(waiter.join().unwrap());
    });
    assert_eq!(cuts.error().as_deref(), Some(BROKEN));
}

/// A reported error survives the drop that follows `end`.
#[test]
fn end_keeps_its_error_over_the_drop_guard() {
    let (cuts, writer) = live();
    writer.end(None, Some("error: cannot read a.c".to_string()));
    assert_eq!(cuts.error().as_deref(), Some("error: cannot read a.c"));

    let (clean, mut writer) = live();
    writer.pushed("a.c".to_string(), stamp(2), None, Some(0..1));
    writer.end(None, None);
    assert!(clean.error().is_none());
}

/// A build that gives up stops the stream: the writer sees the abort on its
/// next document, and no worker can block on the feed again.
#[test]
fn an_aborted_build_stops_the_stream_early() {
    let (cuts, mut writer) = live();
    writer.pushed("a.c".to_string(), stamp(2), None, Some(0..1));
    assert!(!writer.aborted());
    cuts.abort();
    assert!(writer.aborted());
    assert!(cuts.get(0).is_some());
    assert!(cuts.get(1).is_none());
}
