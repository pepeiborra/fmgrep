//! Whether a `--store-docs` index holds one commit's files, byte for byte:
//! the served commit, which a client checkout can be compared with.

use std::io::Read;
use std::path::Path;

use super::{entries, git, records};
use crate::manifest::{Commit, DOCS_SHARD};

/// What `git status` says of a checkout before the index walks it.
pub type Before = Result<String, String>;

/// The commit at HEAD of the checkout whose top is `folder`, when no
/// tracked file differs from it; else why not.
pub fn before(folder: &Path) -> Before {
    let folder = std::fs::canonicalize(folder)
        .map_err(|e| format!("cannot resolve {} ({e})", folder.display()))?;
    let top = super::top(&folder).map_err(|why| match why.contains("not a git repository") {
        true => format!("{} is in no git checkout", folder.display()),
        false => why,
    })?;
    if top != folder {
        return Err(format!(
            "{} is not the top of its git checkout, {}",
            folder.display(),
            top.display()
        ));
    }
    clean_head(&folder)
}

/// HEAD, when `git status` shows no tracked change.
fn clean_head(folder: &Path) -> Result<String, String> {
    let status = git(
        folder,
        &[
            "status",
            "--porcelain=v2",
            "-z",
            "--branch",
            "--untracked-files=no",
            "--ignore-submodules=none",
        ],
    )?;
    let mut head = None;
    for r in records(&status) {
        let r = String::from_utf8_lossy(r);
        match r.strip_prefix("# branch.oid ") {
            Some("(initial)") => return Err("the checkout has no commit yet".to_owned()),
            Some(oid) => head = Some(oid.to_owned()),
            None if r.starts_with("# ") => {}
            // A rename's second path is its own record; the first stops here.
            None => {
                let path = super::status_path(&r).unwrap_or(&r);
                return Err(format!("{path} differs from HEAD"));
            }
        }
    }
    head.ok_or_else(|| "git status gave no HEAD".to_owned())
}

/// The served commit of the index just built from `folder` into `output`:
/// HEAD, unchanged since [`before`], when every indexed file in `files`
/// (ascending) is a regular file of HEAD whose text is the stored one, no
/// entry is a submodule, and no ignore rule from outside the checkout
/// applies. Else why not.
pub fn after(
    before: Before,
    folder: &Path,
    output: &Path,
    files: &[String],
    lens: &[u64],
) -> Result<Commit, String> {
    let head = before?;
    let folder = std::fs::canonicalize(folder)
        .map_err(|e| format!("cannot resolve {} ({e})", folder.display()))?;
    if clean_head(&folder)? != head {
        return Err("HEAD moved during the build".to_owned());
    }
    let listed = git(&folder, &["ls-files", "-s", "-z"])?;
    let mut oids = Vec::with_capacity(files.len());
    let mut want = files.iter().peekable();
    for entry in entries(&listed) {
        let entry = entry?;
        if entry.mode == "160000" {
            let path = String::from_utf8_lossy(entry.path);
            return Err(format!("the checkout holds a submodule ({path})"));
        }
        let Some(file) = want.peek() else {
            continue;
        };
        if file.as_bytes() != entry.path {
            continue;
        }
        if entry.stage != "0" || !matches!(entry.mode, "100644" | "100755") {
            return Err(format!("{file} is not a regular file of HEAD"));
        }
        oids.push(entry.oid);
        want.next();
    }
    if let Some(file) = want.next() {
        return Err(format!("{file} is not tracked by git"));
    }
    same_text(&folder, output, &oids, files, lens)?;
    Ok(Commit {
        vcs: "git".to_owned(),
        id: head,
    })
}

/// Whether each stored document is the text of its blob, as the index
/// reads a file: one `git cat-file --batch`, `docs.shard` read in order.
fn same_text(
    folder: &Path,
    output: &Path,
    oids: &[&str],
    files: &[String],
    lens: &[u64],
) -> Result<(), String> {
    let path = output.join(DOCS_SHARD);
    let failed = |e: std::io::Error| format!("cannot read {} ({e})", path.display());
    let mut shard =
        std::io::BufReader::with_capacity(1 << 20, std::fs::File::open(&path).map_err(failed)?);
    let mut stored = Vec::new();
    super::blobs(folder, oids, |i, blob| {
        stored.resize(lens[i] as usize, 0);
        shard.read_exact(&mut stored).map_err(failed)?;
        match crate::binary::text(blob) {
            Ok(text) if text.bytes == stored => Ok(()),
            _ => Err(format!("{} differs from its blob in HEAD", files[i])),
        }
    })
}

/// [`after`], said on stderr in one note.
pub fn note(commit: &Result<Commit, String>) {
    match commit {
        Ok(c) => eprintln!("note: the index holds commit {}", c.id),
        Err(why) => eprintln!("note: the index records no served commit ({why})"),
    }
}
