//! Checks that rows print the same lines for every file they all searched.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsStr;
use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::{Command, Output};

use crate::rows::failure;
use crate::{Row, RowKind};

pub struct Paths {
    root: Vec<u8>,
    ids: HashMap<Box<[u8]>, u32>,
    names: Vec<Box<[u8]>>,
    walked: Vec<bool>,
    unignored: Vec<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    Hidden,
    Symlink,
    Ignored,
    Name,
    Binary,
}

impl Class {
    pub fn name(self) -> &'static str {
        match self {
            Class::Hidden => "hidden",
            Class::Symlink => "symlink",
            Class::Ignored => "ignored",
            Class::Name => "name",
            Class::Binary => "binary",
        }
    }
}

/// The lines one row printed for one query, by file.
#[derive(Default)]
pub struct Hits {
    /// Sorted by file.
    pub per_file: Vec<FileHits>,
    /// Files rg reported as binary, sorted.
    pub binary: Vec<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileHits {
    pub file: u32,
    pub lines: u32,
    /// Of its sorted line numbers.
    pub digest: u64,
}

impl Hits {
    pub fn lines(&self) -> u64 {
        self.per_file.iter().map(|f| u64::from(f.lines)).sum()
    }

    pub fn files(&self) -> usize {
        self.per_file.len()
    }

    fn get(&self, file: u32) -> Option<&FileHits> {
        let at = self.per_file.binary_search_by_key(&file, |f| f.file);
        at.ok().map(|i| &self.per_file[i])
    }
}

/// Files printed by some row for `term` that `row` did not search.
#[derive(Debug, PartialEq, Eq)]
pub struct Unsearched {
    pub term: String,
    pub row: RowKind,
    pub class: Class,
    pub files: Vec<u32>,
}

enum Line {
    Hit(u32, u32),
    Binary(u32),
}

const BINARY: [&[u8]; 2] = [
    b" binary file matches (found ",
    b" WARNING: stopped searching binary file after match (found ",
];

fn split_lines(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    bytes.split(|&b| b == b'\n').filter(|l| !l.is_empty())
}

impl Paths {
    /// `rg --files`, `rg --files --no-ignore` and `docs`, relative to `root`.
    pub fn new(root: &Path, docs: &[fmgrep::DocMeta]) -> Result<Self, String> {
        let list = |no_ignore: Option<&str>| {
            let mut cmd = rg();
            cmd.arg("--files").args(no_ignore).arg("--").arg(root);
            let out = cmd.output().map_err(|e| format!("cannot run rg: {e}"))?;
            match out.status.code() {
                Some(0 | 1) => Ok(out.stdout),
                _ => Err(failure("rg --files", &out)),
            }
        };
        let (walk, unignored) = (list(None)?, list(Some("--no-ignore"))?);
        Ok(Self::from_lists(root, &walk, &unignored, docs))
    }

    pub fn from_lists(
        root: &Path,
        walk: &[u8],
        unignored: &[u8],
        docs: &[fmgrep::DocMeta],
    ) -> Self {
        let mut root = root.as_os_str().as_encoded_bytes().to_vec();
        root.push(b'/');
        let mut paths = Paths {
            root,
            ids: HashMap::new(),
            names: Vec::new(),
            walked: Vec::new(),
            unignored: Vec::new(),
        };
        for line in split_lines(walk) {
            let id = paths.add(line);
            paths.walked[id as usize] = true;
        }
        for line in split_lines(unignored) {
            let id = paths.add(line);
            paths.unignored[id as usize] = true;
        }
        for doc in docs {
            paths.add(doc.path.as_bytes());
        }
        paths
    }

    fn add(&mut self, path: &[u8]) -> u32 {
        let rel = self.relative(path);
        if let Some(&id) = self.ids.get(rel) {
            return id;
        }
        let id = self.names.len() as u32;
        self.ids.insert(rel.into(), id);
        self.names.push(rel.into());
        self.walked.push(false);
        self.unignored.push(false);
        id
    }

    fn relative<'a>(&self, path: &'a [u8]) -> &'a [u8] {
        let path = path.strip_prefix(&self.root[..]).unwrap_or(path);
        path.strip_prefix(b"./").unwrap_or(path)
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn id(&self, path: &str) -> Option<u32> {
        self.ids.get(path.as_bytes()).copied()
    }

    pub fn name(&self, id: u32) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.names[id as usize])
    }

    /// A `path:N:text` line; the path is the first known `:`-split prefix.
    fn line(&self, line: &[u8]) -> Option<Line> {
        let rel = self.relative(line);
        let mut colons = rel.iter().enumerate().filter(|b| *b.1 == b':');
        colons.find_map(|(i, _)| {
            let &id = self.ids.get(&rel[..i])?;
            let rest = &rel[i + 1..];
            if BINARY.iter().any(|b| rest.starts_with(b)) {
                return Some(Line::Binary(id));
            }
            let digits = &rest[..rest.iter().position(|&b| b == b':')?];
            if !digits.iter().all(u8::is_ascii_digit) {
                return None;
            }
            let n = std::str::from_utf8(digits).ok()?.parse().ok()?;
            Some(Line::Hit(id, n))
        })
    }

    /// The file of a `path:N:text` line, if it is one.
    pub fn file_of(&self, line: &[u8]) -> Option<u32> {
        match self.line(line)? {
            Line::Hit(id, _) | Line::Binary(id) => Some(id),
        }
    }

    /// The `path:N:text` lines of a run that exited 0 or 1.
    pub fn hits(&self, what: &str, out: &Output) -> Result<Hits, String> {
        if !matches!(out.status.code(), Some(0 | 1)) {
            return Err(failure(what, out));
        }
        let (mut set, mut binary) = (Vec::new(), Vec::new());
        for l in split_lines(&out.stdout) {
            match self.line(l) {
                Some(Line::Hit(id, n)) => set.push((id, n)),
                Some(Line::Binary(id)) => binary.push(id),
                None => {
                    let l = String::from_utf8_lossy(l);
                    return Err(format!("{what}: not a known path:N:text: {l:?}"));
                }
            }
        }
        set.sort_unstable();
        binary.sort_unstable();
        binary.dedup();
        let hash = BuildHasherDefault::<DefaultHasher>::default();
        let per_file = set.chunk_by(|a, b| a.0 == b.0).map(|c| FileHits {
            file: c[0].0,
            lines: c.len() as u32,
            digest: hash.hash_one(c.iter().map(|l| l.1).collect::<Vec<_>>()),
        });
        let per_file = per_file.collect();
        Ok(Hits { per_file, binary })
    }

    /// The known files of a `-l` listing that exited 0 or 1, sorted.
    pub fn listed(&self, what: &str, out: &Output) -> Result<Vec<u32>, String> {
        if !matches!(out.status.code(), Some(0 | 1)) {
            return Err(failure(what, out));
        }
        let ids = split_lines(&out.stdout).filter_map(|l| self.ids.get(self.relative(l)));
        let mut ids: Vec<u32> = ids.copied().collect();
        ids.sort_unstable();
        ids.dedup();
        Ok(ids)
    }

    /// Why a row may not have searched the file: the first that applies.
    pub fn class(&self, root: &Path, id: u32) -> Class {
        let (i, name) = (id as usize, &self.names[id as usize]);
        let link = std::fs::symlink_metadata(root.join(OsStr::from_bytes(name)));
        if name.split(|&b| b == b'/').any(|c| c.starts_with(b".")) {
            Class::Hidden
        } else if link.is_ok_and(|m| m.is_symlink()) {
            Class::Symlink
        } else if self.unignored[i] && !self.walked[i] {
            Class::Ignored
        } else if name.contains(&b'\\') || String::from_utf8_lossy(name).contains('\u{FFFD}') {
            Class::Name
        } else {
            Class::Binary
        }
    }
}

/// rg without the user's configuration file.
pub fn rg() -> Command {
    let mut cmd = Command::new("rg");
    cmd.env_remove("RIPGREP_CONFIG_PATH");
    cmd
}

/// Per query: files searched by all rows but printed differently, and files unsearched by some.
pub fn verify(paths: &Paths, root: &Path, rows: &[Row]) -> (Vec<String>, Vec<Unsearched>) {
    let Some(base) = rows.first() else {
        return (Vec::new(), Vec::new());
    };
    let terms = |r: &Row| r.queries.iter().map(|q| q.term.clone()).collect::<Vec<_>>();
    let mut fatal = Vec::new();
    for row in &rows[1..] {
        if terms(row) != terms(base) {
            fatal.push(format!("queries: {} and {} differ", base.name, row.name));
        }
    }
    if !fatal.is_empty() {
        return (fatal, Vec::new());
    }
    let mut unsearched = Vec::new();
    for (q, base_query) in base.queries.iter().enumerate() {
        let term = &base_query.term;
        let hits: Vec<&Hits> = rows.iter().map(|r| &r.queries[q].hits).collect();
        let printed = hits.iter().flat_map(|h| h.per_file.iter().map(|f| f.file));
        let printed: BTreeSet<u32> = printed.collect();
        let mut missing: BTreeMap<(usize, Class), Vec<u32>> = BTreeMap::new();
        let mut differ = Vec::new();
        for &file in &printed {
            let mut reference: Option<(usize, Option<&FileHits>)> = None;
            for (r, row) in rows.iter().enumerate() {
                let listed = row.searched.binary_search(&file).is_ok();
                let got = hits[r].get(file);
                if got.is_some() && !listed {
                    let path = paths.name(file);
                    fatal.push(format!(
                        "{term:?}: {} prints {path}, not searched",
                        row.name
                    ));
                } else if !listed || hits[r].binary.binary_search(&file).is_ok() {
                    let class = paths.class(root, file);
                    missing.entry((r, class)).or_default().push(file);
                } else if let Some((b, want)) = reference {
                    let key = |h: Option<&FileHits>| h.map_or((0, 0), |h| (h.lines, h.digest));
                    let ((bc, bd), (rc, rd)) = (key(want), key(got));
                    let (bn, rn, path) = (rows[b].name, row.name, paths.name(file));
                    if bc != rc {
                        differ.push(format!("{term:?}: {path}: {bn} {bc} lines, {rn} {rc}"));
                    } else if bd != rd {
                        differ.push(format!(
                            "{term:?}: {path}: {bn} and {rn} print different lines"
                        ));
                    }
                } else {
                    reference = Some((r, got));
                }
            }
        }
        if differ.len() > 5 {
            let more = differ.len() - 5;
            differ.truncate(5);
            differ.push(format!("{term:?}: and {more} more files differ"));
        }
        fatal.extend(differ);
        for ((r, class), files) in missing {
            let (term, row) = (term.clone(), rows[r].name);
            unsearched.push(Unsearched {
                term,
                row,
                class,
                files,
            });
        }
    }
    (fatal, unsearched)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Query;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;
    use std::time::Duration;

    fn out(code: i32, stdout: &[u8]) -> Output {
        Output {
            status: ExitStatus::from_raw(code << 8),
            stdout: stdout.to_vec(),
            stderr: Vec::new(),
        }
    }

    fn doc(path: &str) -> fmgrep::DocMeta {
        fmgrep::DocMeta {
            path: path.to_string(),
            len: 1,
        }
    }

    fn table() -> Paths {
        let walk = b"/r/a/c:1:d.txt\n/r/b\\s.txt\n/r/x.c\n";
        let docs = [doc("x.c"), doc("y.c"), doc("a/b")];
        Paths::from_lists(Path::new("/r"), walk, b"/r/ign.c\n", &docs)
    }

    #[test]
    fn hits_resolve_real_paths_under_the_root() {
        let p = table();
        let names: Vec<_> = (0..p.len() as u32)
            .map(|i| p.name(i).into_owned())
            .collect();
        assert_eq!(
            names,
            ["a/c:1:d.txt", "b\\s.txt", "x.c", "ign.c", "y.c", "a/b"]
        );
        let run = |code, stdout: &[u8]| p.hits("t", &out(code, stdout));
        let h = run(
            0,
            b"/r/a/c:1:d.txt:1:x: y\n./x.c:7:a\n\nx.c:9:\xff\nb\\s.txt:3:\na/b:2:1:z\n",
        )
        .unwrap();
        let per: Vec<_> = h.per_file.iter().map(|f| (f.file, f.lines)).collect();
        assert_eq!(per, [(0, 1), (1, 1), (2, 2), (5, 1)]);
        assert_eq!((h.lines(), h.files()), (5, 4));
        let reordered = run(0, b"x.c:9:\n/r/x.c:7:\n").unwrap();
        assert_eq!(reordered.per_file[0].digest, h.per_file[2].digest);
        let other = run(0, b"x.c:8:\nx.c:9:\n").unwrap();
        assert_ne!(other.per_file[0].digest, h.per_file[2].digest);
        let bin = run(0, b"x.c:1:a\nx.c: WARNING: stopped searching binary file after match (found \"\\0\" byte around offset 9)\n");
        assert_eq!(bin.unwrap().binary, [2]);
        assert_eq!(run(1, b"").unwrap().files(), 0);
        assert!(run(2, b"").is_err());
        for bad in [
            &b"x.c:3\n"[..],
            b"x.c\n1:foo\n",
            b"z.c:1:foo\n",
            b"x.c::foo\n",
        ] {
            assert!(run(0, bad).is_err(), "{:?}", String::from_utf8_lossy(bad));
        }
        let listed = p.listed("t", &out(0, b"/r/y.c\nx.c\n/r/b/s.txt\n./y.c\n"));
        assert_eq!(listed.unwrap(), [2, 4]);
    }

    fn row(name: RowKind, searched: &[u32], hits: &[&[(u32, u32, u64)]]) -> Row {
        let mut row = Row::new(name);
        row.searched = searched.to_vec();
        for (i, files) in hits.iter().enumerate() {
            let per_file = files.iter().map(|&(file, lines, digest)| FileHits {
                file,
                lines,
                digest,
            });
            let hits = Hits {
                per_file: per_file.collect(),
                binary: Vec::new(),
            };
            row.queries
                .push(Query::new(&format!("t{i}"), Duration::ZERO, hits));
        }
        row
    }

    #[test]
    fn verify_fails_on_files_both_rows_searched_and_lists_the_rest() {
        let (p, root) = (table(), Path::new("/nonexistent"));
        let rg = || row(RowKind::Rg, &[0, 1, 2], &[&[(0, 2, 7), (2, 1, 1)], &[]]);
        assert_eq!(verify(&p, root, &[rg(), rg()]), (vec![], vec![]));

        let fm = row(
            RowKind::Fmgrep,
            &[2, 3, 4],
            &[&[(2, 1, 1), (3, 1, 5)], &[(4, 1, 1)]],
        );
        let (fatal, unsearched) = verify(&p, root, &[rg(), fm]);
        assert_eq!(fatal, Vec::<String>::new());
        let missing = |term: &str, row, class, files: &[u32]| Unsearched {
            term: term.to_string(),
            row,
            class,
            files: files.to_vec(),
        };
        let want = [
            missing("t0", RowKind::Rg, Class::Ignored, &[3]),
            missing("t0", RowKind::Fmgrep, Class::Binary, &[0]),
            missing("t1", RowKind::Rg, Class::Binary, &[4]),
        ];
        assert_eq!(unsearched, want);

        let fm = row(
            RowKind::Fmgrep,
            &[0, 2],
            &[&[(0, 1, 7), (2, 1, 2)], &[(1, 1, 1)]],
        );
        let (fatal, _) = verify(&p, root, &[rg(), fm]);
        let want = [
            "\"t0\": a/c:1:d.txt: rg 2 lines, fmgrep 1",
            "\"t0\": x.c: rg and fmgrep print different lines",
            "\"t1\": fmgrep prints b\\s.txt, not searched",
        ];
        assert_eq!(fatal, want);

        let mut partial = rg();
        partial.name = RowKind::Tgrep;
        partial.queries.remove(1);
        let (fatal, _) = verify(&p, root, &[rg(), partial]);
        assert_eq!(fatal, ["queries: rg and tgrep differ"]);
    }

    #[test]
    fn class_is_the_first_property_that_applies() {
        let git = Command::new("git").arg("--version").output();
        if rg().arg("--version").output().is_err() || git.is_err() {
            eprintln!("skipped: rg or git is not on PATH");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        assert!(
            Command::new("git")
                .arg("init")
                .arg("-q")
                .arg(&root)
                .status()
                .unwrap()
                .success()
        );
        let files = [".h/a", "ign.txt", "b\\s.txt", "bin.c", ".gitignore"];
        for (name, text) in files.iter().zip(["a", "a", "a", "a", "ign.txt\nlink\n"]) {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        std::os::unix::fs::symlink("bin.c", root.join("link")).unwrap();
        let docs = [doc("link")];
        let p = Paths::new(&root, &docs).unwrap();
        let class = |name: &str| p.id(name).map(|id| p.class(&root, id));
        assert_eq!(class(".h/a"), None);
        assert_eq!(class("link"), Some(Class::Symlink));
        assert_eq!(class("ign.txt"), Some(Class::Ignored));
        assert_eq!(class("b\\s.txt"), Some(Class::Name));
        assert_eq!(class("bin.c"), Some(Class::Binary));
        let hidden = Paths::from_lists(&root, b".h/a\n", b"", &[]);
        assert_eq!(hidden.class(&root, 0), Class::Hidden);
        let raw = b"n\xff.txt\n";
        let lossy = Paths::from_lists(&root, raw, raw, &[doc("n\u{FFFD}.txt")]);
        assert_eq!(lossy.len(), 2);
        assert_eq!(lossy.class(&root, 0), Class::Name);
        assert_eq!(lossy.class(&root, 1), Class::Name);
    }
}
