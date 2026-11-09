//! Executable scripts standing in for a tool in tests.

use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

fn create(path: &Path, script: &str) {
    fs::write(path, script).unwrap();
    fs::set_permissions(path, Permissions::from_mode(0o755)).unwrap();
}

/// Creates `path` and waits for its last `CLOSE_WRITE` event. That event is
/// not an execution-readiness barrier, so callers retry transient `ETXTBSY`.
#[cfg(target_os = "linux")]
pub fn write(path: &Path, script: &str) {
    use inotify::{Inotify, WatchMask};
    use std::io::ErrorKind;
    use std::thread::sleep;
    use std::time::{Duration, Instant};

    let mut inotify = Inotify::init().unwrap();
    let dir = path.parent().unwrap();
    inotify.watches().add(dir, WatchMask::CLOSE_WRITE).unwrap();
    create(path, script);

    // Sent on the last close of the file, whichever process makes it.
    let name = path.file_name();
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut buffer = [0; 4096];
    loop {
        match inotify.read_events(&mut buffer) {
            Ok(mut events) => {
                if events.any(|e| e.name == name) {
                    return;
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "{} is still open for writing",
                    path.display()
                );
                sleep(Duration::from_millis(1));
            }
            Err(e) => panic!("inotify: {e}"),
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub fn write(path: &Path, script: &str) {
    create(path, script);
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::process::Command;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn output_retries_until_the_executable_write_handle_closes() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake");
        write(&script, "#!/bin/sh\nexit 0\n");
        let handle = OpenOptions::new().write(true).open(&script).unwrap();
        let release = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            drop(handle);
        });

        let output = crate::rows::output_retry_etxtbsy(&mut Command::new(&script)).unwrap();
        release.join().unwrap();
        assert!(output.status.success());
    }
}
