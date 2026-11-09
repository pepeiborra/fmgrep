//! The corpus root's change events, in bursts; on Linux inotify without `IN_OPEN`, whose own reads would overflow the queue.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long a burst of events is let run before it is taken.
const SETTLE: Duration = Duration::from_millis(20);

/// What one burst names.
#[derive(Default)]
pub struct Burst {
    /// Events were lost: re-check everything.
    pub rescan: bool,
    /// Paths that changed; a directory stands for its subtree.
    pub paths: Vec<PathBuf>,
}

pub enum Next {
    /// Nothing within the tick.
    Idle,
    Burst(Burst),
    /// The event source is gone.
    Closed,
}

#[cfg(target_os = "linux")]
pub use inotify_feed::Feed;
#[cfg(not(target_os = "linux"))]
pub use notify_feed::Feed;

#[cfg(target_os = "linux")]
mod inotify_feed {
    use std::collections::HashMap;
    use std::ffi::OsStr;
    use std::io::ErrorKind;

    use inotify::{EventMask, Inotify, WatchDescriptor, WatchMask};

    use super::*;

    /// Changes only; `IN_OPEN` and `IN_CLOSE_*` are left out.
    const MASK: WatchMask = WatchMask::ATTRIB
        .union(WatchMask::CREATE)
        .union(WatchMask::DELETE)
        .union(WatchMask::MODIFY)
        .union(WatchMask::MOVED_FROM)
        .union(WatchMask::MOVED_TO)
        .union(WatchMask::DELETE_SELF)
        .union(WatchMask::MOVE_SELF)
        .union(WatchMask::ONLYDIR)
        .union(WatchMask::DONT_FOLLOW)
        .union(WatchMask::EXCL_UNLINK);

    /// Events taken per drain, so a sustained producer cannot hold a burst open.
    const MAX_EVENTS: usize = 1 << 16;

    pub struct Feed {
        inotify: Inotify,
        root: PathBuf,
        pub(super) dirs: HashMap<WatchDescriptor, PathBuf>,
        buf: Vec<u8>,
        /// Whether running out of watches was reported.
        warned: bool,
    }

    /// `e`, naming the limit when it is running out of watches.
    fn explain(path: &Path, e: &std::io::Error) -> String {
        match e.kind() {
            ErrorKind::StorageFull => format!(
                "{}: out of inotify watches (raise fs.inotify.max_user_watches)",
                path.display()
            ),
            _ => format!("{}: {e}", path.display()),
        }
    }

    impl Feed {
        /// Watch every directory under `root` the index walk descends.
        pub fn new(root: &Path) -> Result<Self, String> {
            let mut feed = Self {
                inotify: Inotify::init().map_err(|e| e.to_string())?,
                root: root.to_path_buf(),
                dirs: HashMap::new(),
                buf: vec![0; 64 << 10],
                warned: false,
            };
            feed.add_tree(root).map_err(|(p, e)| explain(&p, &e))?;
            Ok(feed)
        }

        /// Watch `top` and the dirs under it; only a failure on `top` or running out of watches is an error.
        fn add_tree(&mut self, top: &Path) -> Result<(), (PathBuf, std::io::Error)> {
            let mut stack = vec![top.to_path_buf()];
            while let Some(dir) = stack.pop() {
                match self.inotify.watches().add(&dir, MASK) {
                    Ok(wd) => {
                        self.dirs.insert(wd, dir.clone());
                    }
                    Err(e) if dir == top || e.kind() == ErrorKind::StorageFull => {
                        return Err((dir, e));
                    }
                    Err(_) => continue,
                }
                let Ok(entries) = std::fs::read_dir(&dir) else {
                    continue;
                };
                for entry in entries.flatten() {
                    if crate::walk::descends(&entry) {
                        stack.push(entry.path());
                    }
                }
            }
            Ok(())
        }

        /// Watch `top` again; out of watches, warn once and ask for a rescan.
        fn rewatch(&mut self, top: &Path, burst: &mut Burst) {
            match self.add_tree(top) {
                Err((path, e)) if e.kind() == ErrorKind::StorageFull => {
                    if !std::mem::replace(&mut self.warned, true) {
                        eprintln!("warning: {}; some changes go unseen", explain(&path, &e));
                    }
                    burst.rescan = true;
                }
                // Gone or replaced already: its path is in the burst, which re-checks it.
                _ => {}
            }
        }

        /// The watches at or under `path`.
        fn under(&self, path: &Path) -> Vec<WatchDescriptor> {
            let at = self.dirs.iter().filter(|(_, d)| d.starts_with(path));
            at.map(|(wd, _)| wd.clone()).collect()
        }

        /// Drop each of `wds` still at or under `path`: moved away, not renamed in the tree.
        fn forget(&mut self, path: &Path, wds: Vec<WatchDescriptor>) {
            for wd in wds {
                if self.dirs.get(&wd).is_some_and(|d| d.starts_with(path)) {
                    self.dirs.remove(&wd);
                    let _ = self.inotify.watches().remove(wd);
                }
            }
        }

        /// Add up to [`MAX_EVENTS`] queued events to `burst`: false when none was, `Err` when unreadable.
        fn drain(&mut self, burst: &mut Burst) -> Result<bool, ()> {
            let mut taken = 0;
            // Dirs moved from, by cookie, with their watches then.
            let mut away: Vec<(u32, PathBuf, Vec<WatchDescriptor>)> = Vec::new();
            while taken < MAX_EVENTS {
                let events: Vec<_> = match self.inotify.read_events(&mut self.buf) {
                    Ok(events) => events
                        .map(|e| (e.wd, e.mask, e.cookie, e.name.map(OsStr::to_os_string)))
                        .collect(),
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(e) => {
                        eprintln!(
                            "warning: cannot read inotify events ({e}); not watching for changes"
                        );
                        return Err(());
                    }
                };
                let mut lost = false;
                // In order: a rename refreshes the paths of the events queued after it.
                for (wd, mask, cookie, name) in events {
                    taken += 1;
                    if mask.contains(EventMask::Q_OVERFLOW) {
                        lost = true;
                        continue;
                    }
                    if mask.contains(EventMask::IGNORED) {
                        self.dirs.remove(&wd);
                        continue;
                    }
                    let Some(dir) = self.dirs.get(&wd) else {
                        continue;
                    };
                    let path = name.as_ref().map_or_else(|| dir.clone(), |n| dir.join(n));
                    let dir_name = name.as_deref().filter(|_| mask.contains(EventMask::ISDIR));
                    if let Some(n) = dir_name {
                        if mask.contains(EventMask::MOVED_FROM) {
                            away.push((cookie, path.clone(), self.under(&path)));
                        } else if mask.intersects(EventMask::CREATE | EventMask::MOVED_TO)
                            && !crate::walk::pruned(n)
                        {
                            if mask.contains(EventMask::MOVED_TO) {
                                away.retain(|(c, _, _)| *c != cookie);
                            }
                            // Re-adding a moved dir keeps its watches and refreshes their paths.
                            self.rewatch(&path, burst);
                        }
                    }
                    burst.paths.push(path);
                }
                // Dirs made or renamed while events were lost: re-adding refreshes each path.
                if lost {
                    burst.rescan = true;
                    let root = self.root.clone();
                    self.rewatch(&root, burst);
                }
            }
            for (_, path, wds) in away {
                self.forget(&path, wds);
            }
            Ok(taken > 0)
        }

        pub fn next(&mut self, tick: Duration) -> Next {
            let mut burst = Burst::default();
            match self.drain(&mut burst) {
                Err(()) => return Next::Closed,
                Ok(false) => {
                    std::thread::sleep(tick);
                    return Next::Idle;
                }
                Ok(true) => std::thread::sleep(SETTLE),
            }
            match self.drain(&mut burst) {
                Err(()) => Next::Closed,
                Ok(_) => Next::Burst(burst),
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod notify_feed {
    use std::sync::mpsc;

    use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};

    use super::*;

    pub struct Feed {
        events: mpsc::Receiver<notify::Result<Event>>,
        _watcher: RecommendedWatcher,
    }

    impl Feed {
        pub fn new(root: &Path) -> Result<Self, String> {
            let (tx, events) = mpsc::channel();
            let mut watcher = notify::recommended_watcher(move |event| {
                let _ = tx.send(event);
            })
            .map_err(|e| e.to_string())?;
            watcher
                .watch(root, RecursiveMode::Recursive)
                .map_err(|e| e.to_string())?;
            Ok(Self {
                events,
                _watcher: watcher,
            })
        }

        pub fn next(&mut self, tick: Duration) -> Next {
            let first = match self.events.recv_timeout(tick) {
                Ok(event) => event,
                Err(mpsc::RecvTimeoutError::Timeout) => return Next::Idle,
                Err(mpsc::RecvTimeoutError::Disconnected) => return Next::Closed,
            };
            std::thread::sleep(SETTLE);
            let mut burst = Burst::default();
            for event in std::iter::once(first).chain(self.events.try_iter()) {
                match event {
                    Ok(e) if e.need_rescan() => burst.rescan = true,
                    Ok(e) if matches!(e.kind, EventKind::Access(_)) => {}
                    Ok(e) => burst.paths.extend(e.paths),
                    Err(_) => burst.rescan = true,
                }
            }
            Next::Burst(burst)
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// The next burst within a few seconds, if any.
    fn burst(feed: &mut Feed) -> Option<Burst> {
        for _ in 0..40 {
            match feed.next(Duration::from_millis(50)) {
                Next::Idle => {}
                Next::Burst(b) => return Some(b),
                Next::Closed => panic!("the feed closed"),
            }
        }
        None
    }

    #[test]
    fn reads_queue_nothing_and_new_dirs_are_watched() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir(root.join(".git")).unwrap();
        std::fs::write(root.join("a.c"), b"a").unwrap();
        let mut feed = Feed::new(&root).unwrap();
        assert_eq!(feed.dirs.len(), 1, "dot dirs are not watched");
        for _ in 0..20_000 {
            std::fs::read(root.join("a.c")).unwrap();
        }
        assert!(burst(&mut feed).is_none(), "a read is no change");
        std::fs::create_dir_all(root.join("new/deep")).unwrap();
        let seen = burst(&mut feed).unwrap();
        assert!(!seen.rescan);
        assert!(seen.paths.contains(&root.join("new")));
        std::fs::write(root.join("new/deep/b.c"), b"b").unwrap();
        assert!(
            seen_path(&mut feed, &root.join("new/deep/b.c")),
            "a file under a new dir"
        );
        std::fs::rename(root.join("new"), root.join("other")).unwrap();
        std::fs::write(root.join("other/deep/c.c"), b"c").unwrap();
        assert!(
            seen_path(&mut feed, &root.join("other/deep/c.c")),
            "a file under a renamed dir"
        );
        std::fs::create_dir_all(root.join(".x/y")).unwrap();
        while burst(&mut feed).is_some() {}
        assert!(
            feed.dirs.values().all(|d| !d.starts_with(root.join(".x"))),
            "a new dot dir stays unwatched: {:?}",
            feed.dirs.values().collect::<Vec<_>>()
        );
    }

    /// Whether a burst within a few seconds names `want`.
    fn seen_path(feed: &mut Feed, want: &Path) -> bool {
        while let Some(b) = burst(feed) {
            if b.paths.iter().any(|p| p == want) {
                return true;
            }
        }
        false
    }
}
