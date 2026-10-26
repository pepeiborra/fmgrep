//! Reads a closed cut's documents back, from `docs.shard` or the files.

use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use crate::{Document, Stamp};

use crate::docs::Cut;

/// The documents of `cut`, in id order, one resident read at a time.
pub fn read_cut(shard: &Path, cut: &Cut) -> Result<Vec<Document>, String> {
    let mut out = Vec::with_capacity(cut.docs.len());
    if cut.docs.is_empty() {
        return Ok(out);
    }
    let file = std::fs::File::open(shard)
        .map_err(|e| format!("error: cannot read {}: {e}", shard.display()))?;
    let mut reader = BufReader::new(file);
    reader
        .seek(SeekFrom::Start(cut.at))
        .map_err(|e| format!("error: cannot read {}: {e}", shard.display()))?;
    for (at, id) in cut.docs.clone().enumerate() {
        let mut bytes = vec![0u8; cut.lens[at] as usize];
        reader
            .read_exact(&mut bytes)
            .map_err(|e| format!("error: cannot read doc {id} from {}: {e}", shard.display()))?;
        out.push(Document {
            id,
            path: cut.paths[at].clone(),
            bytes,
        });
    }
    Ok(out)
}

/// The documents of `cut` re-read from the files under `root`; a file whose
/// stamp moved since the walk read it is refused by path.
pub fn read_files(root: &Path, cut: &Cut) -> Result<Vec<Document>, String> {
    let pool = crate::walk::read_pool()?;
    let read = crate::walk::read_batch(&pool, root, &cut.paths, &String::as_str)?;
    let mut out = Vec::with_capacity(read.len());
    for (at, (id, (bytes, stamp))) in cut.docs.clone().zip(read).enumerate() {
        let want = Stamp {
            len: cut.lens[at],
            mtime_ns: cut.mtimes[at],
        };
        if stamp != want || bytes.len() as u64 != want.len {
            return Err(changed_while_indexing(&root.join(&cut.paths[at])));
        }
        out.push(Document {
            id,
            path: cut.paths[at].clone(),
            bytes,
        });
    }
    Ok(out)
}

pub fn changed_while_indexing(path: &Path) -> String {
    format!(
        "error: {} changed while indexing; re-run index",
        path.display()
    )
}
