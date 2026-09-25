//! Both ends of `blobs.shard`: append slots while indexing, map them back
//! while loading.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::manifest::{BLOBS_SHARD, SHARD_WRITE_BUFFER, shard_offsets};

/// Counts the bytes a slot writes without buffering them.
struct Counted<'a> {
    out: &'a mut dyn Write,
    written: u64,
}

impl Write for Counted<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.out.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}

/// Streaming writer for [`BLOBS_SHARD`]: slots are appended in ascending
/// order and only each slot's inventory label and byte count are retained.
pub struct BlobSink {
    out: BufWriter<File>,
    path: PathBuf,
    labels: Vec<String>,
    lens: Vec<u64>,
}

/// Open `<dir>/blobs.shard` and take slots.
pub fn create_sink(dir: &Path) -> Result<BlobSink, String> {
    let path = dir.join(BLOBS_SHARD);
    let file =
        File::create(&path).map_err(|e| format!("error: cannot write {}: {e}", path.display()))?;
    Ok(BlobSink {
        out: BufWriter::with_capacity(SHARD_WRITE_BUFFER, file),
        path,
        labels: Vec::new(),
        lens: Vec::new(),
    })
}

/// Flush and yield the manifest `blobs` / `blob_lens` tables.
pub fn finish_sink(mut sink: BlobSink) -> Result<(Vec<String>, Vec<u64>), String> {
    sink.out
        .flush()
        .map_err(|e| format!("error: cannot write {}: {e}", sink.path.display()))?;
    Ok((sink.labels, sink.lens))
}

impl BlobSink {
    /// Append one slot; `encode` returns its inventory label.
    pub fn write_slot<F>(&mut self, encode: F) -> Result<(), String>
    where
        F: FnOnce(&mut dyn Write) -> Result<String, String>,
    {
        let mut counted = Counted {
            out: &mut self.out,
            written: 0,
        };
        let label = encode(&mut counted).map_err(|e| format!("error: {e}"))?;
        let written = counted.written;
        self.labels.push(label);
        self.lens.push(written);
        Ok(())
    }
}

/// The persisted blob slots, one [`BLOBS_SHARD`] range each.
pub struct BlobSlots<'a> {
    file: File,
    path: PathBuf,
    labels: &'a [String],
    offsets: Vec<u64>,
}

impl<'a> BlobSlots<'a> {
    /// Opens the shard and refuses a `labels`/`lens` arity skew or a byte
    /// count differing from the `lens` sum.
    pub fn open(dir: &Path, labels: &'a [String], lens: &[u64]) -> Result<Self, String> {
        if labels.len() != lens.len() {
            return Err(format!(
                "error: manifest blob tables skew (blobs={} blob_lens={}); refusing a tampered index",
                labels.len(),
                lens.len(),
            ));
        }
        let path = dir.join(BLOBS_SHARD);
        let file =
            File::open(&path).map_err(|e| format!("error: cannot read {}: {e}", path.display()))?;
        let total = file
            .metadata()
            .map_err(|e| format!("error: cannot read {}: {e}", path.display()))?
            .len();
        let offsets = shard_offsets(lens)?;
        let want = offsets[lens.len()];
        if total != want {
            return Err(format!(
                "error: {} holds {total} byte(s) but the manifest's {} blob(s) sum to {want}; refusing a tampered index",
                path.display(),
                lens.len(),
            ));
        }
        Ok(Self {
            file,
            path,
            labels,
            offsets,
        })
    }

    fn io_err(&self, e: std::io::Error) -> String {
        format!("error: cannot read {}: {e}", self.path.display())
    }
}

/// The manifest inventory `blobs` was opened with.
pub fn labels_of<'a>(blobs: &BlobSlots<'a>) -> &'a [String] {
    blobs.labels
}

/// One slot's bytes: built in memory, or a range of the mapped `blobs.shard`.
pub enum Blob {
    Owned(Vec<u8>),
    Mapped {
        map: Arc<memmap2::Mmap>,
        range: Range<usize>,
    },
}

impl std::ops::Deref for Blob {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match self {
            Self::Owned(bytes) => bytes,
            Self::Mapped { map, range } => &map[range.clone()],
        }
    }
}

impl AsRef<[u8]> for Blob {
    fn as_ref(&self) -> &[u8] {
        self
    }
}

/// Maps [`BLOBS_SHARD`] once, with default read-ahead: one [`Blob`] per
/// slot. An empty file maps nothing.
pub fn map_slots(blobs: &BlobSlots<'_>) -> Result<Vec<Blob>, String> {
    let total = blobs.offsets[blobs.labels.len()];
    let map = if total == 0 {
        None
    } else {
        // SAFETY: an index directory is never rewritten in place (`index`
        // refuses an existing output), and callers keep it unchanged while
        // loaded; a truncated file would fault, not read stale bytes.
        let map = unsafe { memmap2::Mmap::map(&blobs.file) }.map_err(|e| blobs.io_err(e))?;
        map.advise(memmap2::Advice::Normal)
            .map_err(|e| blobs.io_err(e))?;
        Some(Arc::new(map))
    };
    Ok((0..blobs.labels.len())
        .map(|slot| {
            let range = blobs.offsets[slot] as usize..blobs.offsets[slot + 1] as usize;
            match &map {
                Some(map) => Blob::Mapped {
                    map: Arc::clone(map),
                    range,
                },
                None => Blob::Owned(Vec::new()),
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write `slots` through a sink and return the manifest tables.
    fn sink(dir: &Path, slots: &[&[u8]]) -> (Vec<String>, Vec<u64>) {
        std::fs::create_dir_all(dir).unwrap();
        let mut sink = create_sink(dir).unwrap();
        for (i, slot) in slots.iter().enumerate() {
            sink.write_slot(|out| {
                out.write_all(slot).unwrap();
                Ok(format!("{i:08}.bin"))
            })
            .unwrap();
        }
        finish_sink(sink).unwrap()
    }

    fn err_of(opened: Result<BlobSlots<'_>, String>) -> String {
        match opened {
            Err(e) => e,
            Ok(_) => panic!("open must refuse"),
        }
    }

    #[test]
    fn sink_round_trips_labels_and_lens_in_slot_order() {
        let tmp = tempfile::tempdir().unwrap();
        let (labels, lens) = sink(tmp.path(), &[b"aa", b"", b"cccc"]);
        assert_eq!(lens, vec![2, 0, 4]);
        assert_eq!(labels, ["00000000.bin", "00000001.bin", "00000002.bin"]);
        assert_eq!(
            std::fs::read(tmp.path().join(BLOBS_SHARD)).unwrap(),
            b"aacccc"
        );
        let blobs = BlobSlots::open(tmp.path(), &labels, &lens).unwrap();
        assert_eq!(labels_of(&blobs), labels);
    }

    #[test]
    fn mapped_slots_equal_the_written_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let big: Vec<u8> = (0..(1 << 20) + 5).map(|i| (i as u8) ^ 3).collect();
        let slots: [&[u8]; 4] = [b"aa", b"", &big, b"c"];
        let (labels, lens) = sink(tmp.path(), &slots);
        let blobs = BlobSlots::open(tmp.path(), &labels, &lens).unwrap();
        let mapped: Vec<Vec<u8>> = map_slots(&blobs)
            .unwrap()
            .into_iter()
            .map(|b| b.to_vec())
            .collect();
        assert_eq!(mapped, slots.map(<[u8]>::to_vec));

        let empty = tempfile::tempdir().unwrap();
        let (labels, lens) = sink(empty.path(), &[b""]);
        let blobs = BlobSlots::open(empty.path(), &labels, &lens).unwrap();
        let mapped = map_slots(&blobs).unwrap();
        assert!(matches!(&mapped[..], [b] if b.is_empty()));
    }

    #[test]
    fn open_refuses_a_skewed_shard() {
        let tmp = tempfile::tempdir().unwrap();
        let (labels, _) = sink(tmp.path(), &[b"aa"]);
        let err = err_of(BlobSlots::open(tmp.path(), &labels, &[]));
        assert!(err.contains("blobs=1 blob_lens=0"), "got: {err}");
        let err = err_of(BlobSlots::open(tmp.path(), &labels, &[3]));
        assert!(err.contains("1 blob(s) sum to 3"), "got: {err}");
    }
}
