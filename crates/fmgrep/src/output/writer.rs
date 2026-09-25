//! The byte pipe under every match-output path.

use std::io::Write;

use super::WRITE_FAILED;

/// Byte sink that ends silently once the reader hangs up and counts exactly
/// what reached `out`. `written` is the only source rg's `bytes_printed`
/// may use, so no output path may bypass this type.
pub struct Pipe<W: Write> {
    out: W,
    /// The reader hung up: output stops, silently.
    closed: bool,
    written: u64,
}

impl<W: Write> Pipe<W> {
    pub fn new(out: W) -> Self {
        Self {
            out,
            closed: false,
            written: 0,
        }
    }

    pub fn written(&self) -> u64 {
        self.written
    }

    pub fn raw(&mut self, bytes: &[u8]) -> Result<(), String> {
        if self.closed {
            return Ok(());
        }
        match self.out.write_all(bytes) {
            Ok(()) => {
                self.written += bytes.len() as u64;
                Ok(())
            }
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
