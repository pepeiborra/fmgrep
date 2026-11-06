//! The byte pipe under every match-output path.

use std::io::Write;

use super::WRITE_FAILED;

/// Byte sink that ends silently once the reader hangs up.
pub struct Pipe<W: Write> {
    out: W,
    closed: bool,
}

impl<W: Write> Pipe<W> {
    pub fn new(out: W) -> Self {
        Self { out, closed: false }
    }

    pub fn raw(&mut self, bytes: &[u8]) -> Result<(), String> {
        if self.closed {
            return Ok(());
        }
        match self.out.write_all(bytes) {
            Ok(()) => Ok(()),
            Err(e) => self.fail(e),
        }
    }

    pub fn finish(&mut self) -> Result<(), String> {
        if self.closed {
            return Ok(());
        }
        match self.out.flush() {
            Ok(()) => Ok(()),
            Err(e) => self.fail(e),
        }
    }

    /// A closed pipe ends output silently, as rg's does, so `fmgrep x | head`
    /// is usable; any other IO failure is loud.
    fn fail(&mut self, e: std::io::Error) -> Result<(), String> {
        if e.kind() == std::io::ErrorKind::BrokenPipe {
            self.closed = true;
            return Ok(());
        }
        Err(format!("{WRITE_FAILED} matches: {e}"))
    }
}
