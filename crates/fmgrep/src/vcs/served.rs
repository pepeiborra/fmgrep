//! Whether a `--store-docs` index holds one commit's files, byte for byte:
//! the served commit, which a client checkout can be compared with.

use std::io::Read;
use std::path::Path;

use super::{entries, git, records};
use crate::manifest::{Commit, DOCS_SHARD};
use crate::walk::WalkNotes;

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
/// HEAD, unchanged since [`before`], when git status sees every tracked
/// file, no folder the walk listed changed since, every indexed file in
/// `files` (ascending) is a regular file of HEAD whose text is the stored
/// one, every file of HEAD that the walk left out as binary or too large
/// is one the index would leave out as HEAD has it, no entry is a
/// submodule, every ignore file the walk read is one of HEAD, as HEAD has
/// it, and no ignore rule from outside the checkout applies. Else why
/// not.
pub fn after(
    before: Before,
    folder: &Path,
    output: &Path,
    files: &[String],
    lens: &[u64],
    walk: &WalkNotes,
) -> Result<Commit, String> {
    let head = before?;
    let folder = std::fs::canonicalize(folder)
        .map_err(|e| format!("cannot resolve {} ({e})", folder.display()))?;
    if clean_head(&folder)? != head {
        return Err("HEAD moved during the build".to_owned());
    }
    // After git status: a file moved away from the walk and put back
    // before git status ran is missing from the index, and git status
    // shows no change, but its folder's time changed.
    for (rel, stamp) in &walk.dirs {
        if crate::walk::stamp(&folder.join(rel)) != *stamp {
            let rel = if rel.is_empty() { "." } else { rel };
            return Err(format!("the folder {rel} changed during the build"));
        }
    }
    hides_none(&folder)?;
    // HEAD's tree, not git's index, which can change after git status.
    let listed = git(&folder, &["ls-tree", "-r", "-z", &head])?;
    let mut oids = Vec::with_capacity(files.len());
    let (mut left_out, mut left_oids) = (Vec::new(), Vec::new());
    let mut want = files.iter().peekable();
    let mut skips = walk.skipped.iter().peekable();
    for entry in entries(&listed) {
        let entry = entry?;
        if entry.mode == "160000" {
            let path = String::from_utf8_lossy(entry.path);
            return Err(format!("the checkout holds a submodule ({path})"));
        }
        // A skipped file that git does not track is in no checkout.
        while skips.next_if(|f| f.as_bytes() < entry.path).is_some() {}
        if let Some(file) = skips.next_if(|f| f.as_bytes() == entry.path) {
            left_out.push(file);
            left_oids.push(entry.oid);
            continue;
        }
        let Some(file) = want.peek() else {
            continue;
        };
        if file.as_bytes() != entry.path {
            continue;
        }
        if !matches!(entry.mode, "100644" | "100755") {
            return Err(format!("{file} is not a regular file of HEAD"));
        }
        oids.push(entry.oid);
        want.next();
    }
    if let Some(file) = want.next() {
        return Err(format!("{file} is not tracked by git"));
    }
    ignores_are_tracked(&folder, &listed, walk)?;
    same_text(&folder, output, &oids, files, lens)?;
    left_out_too(&folder, &left_oids, &left_out)?;
    Ok(Commit {
        vcs: "git".to_owned(),
        id: head,
    })
}

/// Whether git status sees every tracked file: no index entry is
/// skip-worktree (as a sparse checkout marks the files it leaves out) or
/// assume-unchanged, as `git ls-files -v` tags them (`S`, lowercase).
/// Such a file can be missing from the tree, and so from the index, while
/// git status shows no change.
fn hides_none(folder: &Path) -> Result<(), String> {
    let listed = git(folder, &["ls-files", "-v", "-z"])?;
    let hidden = records(&listed)
        .filter(|r| r.starts_with(b"S ") || r.first().is_some_and(u8::is_ascii_lowercase))
        .count();
    match hidden {
        0 => Ok(()),
        n => Err(format!(
            "git hides changes to {n} file{} (assume-unchanged or skip-worktree)",
            crate::verbose::plural(n)
        )),
    }
}

/// Whether every ignore input of the walk is one that a checkout of HEAD
/// has too: each ignore file below the top is a file of HEAD (`listed` is
/// `git ls-tree -r -z HEAD`) with its blob's bytes, unchanged since the
/// walk read it, and those outside the tree (the ancestors', the global
/// excludes file and the checkout's `info/exclude`, wherever git keeps it)
/// hold no pattern.
fn ignores_are_tracked(folder: &Path, listed: &[u8], walk: &WalkNotes) -> Result<(), String> {
    let mut tracked = std::collections::HashMap::new();
    for entry in entries(listed) {
        let entry = entry?;
        tracked.insert(entry.path, entry.oid);
    }
    let exclude = super::line(git(folder, &["rev-parse", "--git-path", "info/exclude"])?)?;
    let mut outside = vec![folder.join(exclude)];
    let (mut read, mut oids) = (Vec::new(), Vec::new());
    for (rel, stamp) in &walk.ignores {
        let inside =
            !(rel.starts_with('/') || rel.starts_with("../") || rel.ends_with(GIT_EXCLUDE));
        match (inside, stamp) {
            (true, Some(stamp)) => {
                let Some(&oid) = tracked.get(rel.as_bytes()) else {
                    return Err(format!("the ignore file {rel} is not tracked by git"));
                };
                read.push((rel, *stamp));
                oids.push(oid);
            }
            (true, None) => {}
            (false, _) => outside.push(folder.join(rel)),
        }
    }
    if let Some(path) = outside.into_iter().find(|p| has_pattern(p)) {
        return Err(format!(
            "the ignore file {} outside the checkout has a pattern",
            path.display()
        ));
    }
    // A filter can change an ignore file on checkout while git status
    // shows no change.
    super::blobs(folder, &oids, |i, blob| {
        let (rel, stamp) = read[i];
        let path = folder.join(rel);
        let now = std::fs::read(&path).ok();
        if crate::walk::stamp(&path) != Some(stamp) {
            return Err(format!("the ignore file {rel} changed during the build"));
        }
        match now == Some(blob) {
            true => Ok(()),
            false => Err(format!(
                "the ignore file {rel} differs from its blob in HEAD"
            )),
        }
    })
}

/// `.git/info/exclude`, as the walk lists it.
const GIT_EXCLUDE: &str = ".git/info/exclude";

/// Whether the ignore file at `path` exists and holds a line that is
/// neither blank nor a comment. Leading blanks belong to a pattern: ` #a`
/// is one.
pub fn has_pattern(path: &Path) -> bool {
    std::fs::read(path).is_ok_and(|text| {
        text.split(|&b| b == b'\n').any(|l| {
            let l = l.trim_ascii_end();
            !l.is_empty() && !l.starts_with(b"#")
        })
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

/// Whether the index would leave out each blob of `oids` too, as it left
/// out the file of `files`: a filter can make a text blob binary on
/// checkout, while git status shows no change.
fn left_out_too(folder: &Path, oids: &[&str], files: &[&String]) -> Result<(), String> {
    super::blobs(folder, oids, |i, blob| match crate::binary::text(blob) {
        Err(_) => Ok(()),
        Ok(_) => Err(format!("{} differs from its blob in HEAD", files[i])),
    })
}

/// [`after`], said on stderr in one note.
pub fn note(commit: &Result<Commit, String>) {
    match commit {
        Ok(c) => eprintln!("note: the index holds commit {}", c.id),
        Err(why) => eprintln!("note: the index records no served commit ({why})"),
    }
}
