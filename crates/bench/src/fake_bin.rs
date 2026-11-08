//! Executable scripts standing in for a tool in tests.

use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

fn create(path: &Path, script: &str) {
    fs::write(path, script).unwrap();
    fs::set_permissions(path, Permissions::from_mode(0o755)).unwrap();
}

/// Creates `path`, returning once nothing has it open for writing: another
/// test's `Command::spawn` can inherit the handle between its fork and exec,
/// and until that child execs, running the script fails with `ETXTBSY`.
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
