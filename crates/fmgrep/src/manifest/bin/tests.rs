use super::*;
use crate::manifest::{MANIFEST_FORMAT, validate};

const AT: &str = "idx/manifest.bin";

/// `paths` in `shards` shards; files layout under `root` if given.
fn sample(paths: &[&str], shards: usize, root: Option<&str>) -> Manifest {
    let n = paths.len();
    let doc_lens: Vec<u64> = (0..n as u64).map(|i| i * 7 + 1).collect();
    let per = n.div_ceil(shards.max(1));
    Manifest {
        format: MANIFEST_FORMAT,
        store_docs: root.is_none(),
        num_docs: n,
        num_bytes: doc_lens.iter().sum(),
        bom_bytes: 1,
        index_size_bytes: 12_345,
        build_time_us: 678,
        index_started_ns: 1_763_061_773_123_456_789,
        files: paths.iter().map(|p| p.to_string()).collect(),
        blobs: (0..shards)
            .map(|s| {
                format!(
                    "shard-{:08}-{:08}.bin",
                    (s * per).min(n),
                    ((s + 1) * per).min(n)
                )
            })
            .collect(),
        doc_lens: doc_lens.into(),
        blob_lens: (0..shards as u64).map(|s| 100 + s).collect(),
        corpus_root: root.map(str::to_owned),
        commit: None,
        why_no_commit: None,
    }
}

/// [`sample`] in the documents layout, recording no commit for `why`.
fn unserved(paths: &[&str], why: &str) -> Manifest {
    Manifest {
        why_no_commit: Some(why.into()),
        ..sample(paths, 1, None)
    }
}

/// [`sample`] in the documents layout, serving commit `id`.
fn served(paths: &[&str], id: &str) -> Manifest {
    Manifest {
        commit: Some(Commit {
            vcs: "git".into(),
            id: id.into(),
        }),
        ..sample(paths, 1, None)
    }
}

fn small() -> Manifest {
    sample(&["a.c", "b/c.rs", "d"], 2, Some("/tree"))
}

/// 300 paths of 1 to 4 UTF-8 bytes per char, in 5 shards.
fn mid() -> Manifest {
    let mut paths: Vec<String> = (0..300)
        .map(|i| match i % 4 {
            0 => format!("src/m{i}.rs"),
            1 => format!("é/ü{i}.txt"),
            2 => format!("中文/{i}.c"),
            _ => format!("😀/{i}"),
        })
        .collect();
    paths.sort();
    let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
    sample(&paths, 5, Some("/home/é/tree"))
}

/// Bytes of the path text.
fn text_len(m: &Manifest) -> usize {
    m.files.checked().unwrap().iter().map(str::len).sum()
}

fn bytes(m: &Manifest) -> Vec<u8> {
    let mut out = Vec::new();
    encode(m, &mut out).unwrap();
    out
}

fn read(b: &[u8]) -> Result<Manifest, String> {
    decode(b.to_vec(), Path::new(AT))
}

fn put(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

fn get(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// Where each section of `m`'s file starts.
struct Offsets {
    path_ends: usize,
    label_ends: usize,
    root: usize,
    commit: usize,
    why: usize,
    labels: usize,
    paths: usize,
}

fn offsets(m: &Manifest) -> Offsets {
    let (n, s) = (m.files.len(), m.blobs.len());
    let path_ends = 96 + 8 * n;
    let label_ends = path_ends + 8 * n + 8 * s;
    let root = label_ends + 8 * s;
    let commit = root + m.corpus_root.as_deref().map_or(0, str::len);
    let commit_len = m
        .commit
        .as_ref()
        .map_or(0, |c| c.vcs.len() + 1 + c.id.len());
    let why = commit + commit_len;
    let labels = why + m.why_no_commit.as_deref().map_or(0, str::len);
    let paths = labels + m.blobs.iter().map(String::len).sum::<usize>();
    Offsets {
        path_ends,
        label_ends,
        root,
        commit,
        why,
        labels,
        paths,
    }
}

fn refused(b: &[u8], want: &str) {
    match read(b) {
        Ok(m) => panic!("read back {m:?}; want {want}"),
        Err(e) => {
            assert!(e.contains(want), "want {want:?}, got {e}");
            assert!(
                e.starts_with(&format!("error: {AT} ")) || e.contains("unsupported"),
                "{e}"
            );
            assert!(
                e.ends_with("re-run `index`") || e.contains("re-run `index` with"),
                "{e}"
            );
        }
    }
}

#[test]
fn a_manifest_reads_back_equal() {
    let mut max = sample(&["x"], 1, None);
    max.num_bytes = u64::MAX;
    max.bom_bytes = u64::MAX;
    max.index_size_bytes = u64::MAX;
    max.build_time_us = u64::MAX;
    max.doc_lens = vec![u64::MAX].into();
    max.blob_lens = vec![u64::MAX];
    let mut before_epoch = small();
    before_epoch.index_started_ns = -5;
    let many: Vec<String> = (0..80).map(|i| format!("f{i:02}")).collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    for m in [
        sample(&["one"], 1, None),
        sample(&["one"], 1, Some("/r")),
        sample(&["é.c", "中.c", "😀.c"], 1, Some("/home/中")),
        sample(&["a\"b", "c\\d", "line\nbreak", ""], 2, None),
        sample(&many, 40, Some("/many")),
        small(),
        mid(),
        max,
        before_epoch,
        served(&["a.c"], "1a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d"),
        served(&[], "0"),
        unserved(&["a.c"], "the checkout holds a submodule (agents/internal)"),
        unserved(&[], "cannot read é\n中"),
    ] {
        let b = bytes(&m);
        assert_eq!(read(&b).as_ref(), Ok(&m));
        assert_eq!(b.len(), offsets(&m).paths + text_len(&m));
    }
}

/// Every field of a fixed 2-doc manifest, in file order: a change of the
/// layout shows here.
#[test]
fn the_layout_is_fixed() {
    let m = sample(&["a.c", "é"], 1, Some("/r"));
    let want = [
        "666d6d616e696600", // magic
        "09000000",         // format
        "00000000",         // flags: the files layout
        "0200000000000000", // num_docs
        "0900000000000000", // num_bytes
        "0100000000000000", // bom_bytes
        "3930000000000000", // index_size_bytes
        "a602000000000000", // build_time_us
        "150f187057a77718", // index_started_ns
        "0100000000000000", // shards
        "0200000000000000", // bytes of corpus_root
        "0000000000000000", // bytes of the served commit
        "0000000000000000", // bytes of why_no_commit
        "0100000000000000", // doc_lens
        "0800000000000000",
        "0300000000000000", // path ends
        "0500000000000000",
        "6400000000000000",                                       // blob_lens
        "1b00000000000000",                                       // label ends
        "2f72",                                                   // corpus_root
        "73686172642d30303030303030302d30303030303030322e62696e", // labels
        "612e63c3a9",                                             // paths
    ];
    let hex: String = bytes(&m).iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(hex, want.concat());
    // A served commit fills its length and its section, and moves nothing
    // else but the sections after it.
    let m = served(&["a"], "1a2b");
    let want = [
        "666d6d616e696600",                                       // magic
        "09000000",                                               // format
        "01000000",                                               // flags: store_docs
        "0100000000000000",                                       // num_docs
        "0100000000000000",                                       // num_bytes
        "0100000000000000",                                       // bom_bytes
        "3930000000000000",                                       // index_size_bytes
        "a602000000000000",                                       // build_time_us
        "150f187057a77718",                                       // index_started_ns
        "0100000000000000",                                       // shards
        "0000000000000000",                                       // bytes of corpus_root
        "0800000000000000",                                       // bytes of the served commit
        "0000000000000000",                                       // bytes of why_no_commit
        "0100000000000000",                                       // doc_lens
        "0100000000000000",                                       // path ends
        "6400000000000000",                                       // blob_lens
        "1b00000000000000",                                       // label ends
        "6769742031613262",                                       // the served commit: "git 1a2b"
        "73686172642d30303030303030302d30303030303030312e62696e", // labels
        "61",                                                     // paths
    ];
    let hex: String = bytes(&m).iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(hex, want.concat());
    // A reason fills its length and its section, after where a commit
    // would go, and moves nothing else but the sections after it.
    let m = unserved(&["a"], "a.c differs from HEAD");
    let want = [
        "666d6d616e696600",                                       // magic
        "09000000",                                               // format
        "01000000",                                               // flags: store_docs
        "0100000000000000",                                       // num_docs
        "0100000000000000",                                       // num_bytes
        "0100000000000000",                                       // bom_bytes
        "3930000000000000",                                       // index_size_bytes
        "a602000000000000",                                       // build_time_us
        "150f187057a77718",                                       // index_started_ns
        "0100000000000000",                                       // shards
        "0000000000000000",                                       // bytes of corpus_root
        "0000000000000000",                                       // bytes of the served commit
        "1500000000000000",                                       // bytes of why_no_commit
        "0100000000000000",                                       // doc_lens
        "0100000000000000",                                       // path ends
        "6400000000000000",                                       // blob_lens
        "1b00000000000000",                                       // label ends
        "612e6320646966666572732066726f6d2048454144",             // why_no_commit
        "73686172642d30303030303030302d30303030303030312e62696e", // labels
        "61",                                                     // paths
    ];
    let hex: String = bytes(&m).iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(hex, want.concat());
}

#[test]
fn every_truncation_is_refused() {
    for m in [
        small(),
        mid(),
        served(&["a.c", "b"], "1a2b"),
        unserved(&["a.c", "b"], "a.c differs from HEAD"),
    ] {
        let b = bytes(&m);
        for cut in 0..b.len() {
            refused(&b[..cut], "is truncated");
        }
        let mut long = b.clone();
        long.push(0);
        refused(&long, "holds 1 byte(s) past its last section");
    }
}

/// What becomes of a changed manifest file.
#[derive(Debug, PartialEq)]
enum Fate {
    /// Refused at open, with the advice to re-index.
    Open,
    /// Opened; refused with the manifest's words by the first read of a
    /// damaged path, and by the check of all of them.
    Read,
    /// Reads back as a manifest that writes the same bytes.
    Whole,
}

/// What becomes of `c`, a changed manifest file. Never a panic, and never a
/// path read back other than its bytes.
fn fate(c: &[u8], what: &str) -> Fate {
    let got = match read(c) {
        Err(e) => {
            assert!(
                e.ends_with("re-run `index`") || e.contains("re-run `index` with"),
                "{what}: {e}"
            );
            return Fate::Open;
        }
        Ok(got) => got,
    };
    let files = &got.files;
    let each: Vec<_> = (0..files.len()).map(|id| files.path(id)).collect();
    for (id, path) in each.iter().enumerate() {
        match path {
            Ok(path) => assert_eq!(path.map(str::as_bytes), files.raw(id), "{what}"),
            Err(e) => assert_eq!(e, &refusal("holds a path that is not UTF-8"), "{what}"),
        }
    }
    match files.checked() {
        Ok(all) => {
            assert!(each.iter().enumerate().all(|(id, p)| *p == Ok(all.get(id))));
            assert_eq!(bytes(&got), c, "{what}");
            Fate::Whole
        }
        Err(e) => {
            // The paths tile the text, so the text fails exactly when a
            // path does.
            assert!(each.iter().any(Result::is_err), "{what}: {e}");
            assert!(e.starts_with(&format!("error: {AT} ")), "{what}: {e}");
            Fate::Read
        }
    }
}

/// The refusal of the manifest at [`AT`] for `why`.
fn refusal(why: &str) -> String {
    format!("error: {AT} {why}; re-run `index`")
}

/// A changed byte is refused at open, refused where a path it damages is
/// read, or reads back whole. A changed length or scalar reads back
/// changed, as a changed JSON number did. Only a change of the path ends
/// or of the path text opens and is refused when read.
#[test]
fn every_byte_change_is_refused_or_reads_back_whole() {
    for m in [
        small(),
        mid(),
        served(&["a.c", "b"], "1a2b"),
        unserved(&["a.c", "b"], "a.c differs from HEAD"),
    ] {
        let b = bytes(&m);
        let off = offsets(&m);
        let ends = off.path_ends..off.path_ends + 8 * m.files.len();
        let late = |i: usize| ends.contains(&i) || i >= off.paths;
        let mut seen = std::collections::BTreeMap::new();
        for at in 0..b.len() {
            for new in [b[at] ^ 0x01, b[at] ^ 0x80, 0x00, 0xFF] {
                if new == b[at] {
                    continue;
                }
                let mut c = b.clone();
                c[at] = new;
                let fate = fate(&c, &format!("byte {at} = {new:#x}"));
                assert!(fate != Fate::Read || late(at), "byte {at}");
                *seen.entry(format!("{fate:?}")).or_insert(0) += 1;
            }
        }
        assert_eq!(seen.len(), 3, "{seen:?}");
    }
}

/// The values a hostile file would put in a count, a length or an end,
/// in each word of the header and of the tables: refused, or read back
/// whole. A count of 2^60 sizes no buffer.
#[test]
fn edge_values_in_every_word_are_refused_or_read_back() {
    for m in [
        small(),
        mid(),
        served(&["a.c", "b"], "1a2b"),
        unserved(&["a.c", "b"], "a.c differs from HEAD"),
    ] {
        let b = bytes(&m);
        let len = b.len() as u64;
        for at in (16..offsets(&m).root).step_by(8) {
            for v in [
                0,
                1,
                len - 1,
                len,
                len + 1,
                1 << 32,
                1 << 60,
                1 << 63,
                u64::MAX,
            ] {
                let mut c = b.clone();
                put(&mut c, at, v);
                fate(&c, &format!("word {at} = {v:#x}"));
            }
        }
    }
    // The blocks of a file that a crash cut short can read as zeros.
    refused(&vec![0; bytes(&small()).len()], "is not an fmgrep manifest");
}

#[test]
fn each_bad_header_is_refused_by_name() {
    let m = small();
    let b = bytes(&m);
    let mut c = b.clone();
    c[0] = b'X';
    refused(&c, "is not an fmgrep manifest");
    for format in [MANIFEST_FORMAT - 1, MANIFEST_FORMAT + 1] {
        let mut c = b.clone();
        c[8..12].copy_from_slice(&format.to_le_bytes());
        // Refused by number, whatever else the file holds.
        c.truncate(20);
        refused(
            &c,
            &format!(
                "error: unsupported manifest format {format} in {AT} (want {MANIFEST_FORMAT}); re-run `index` with this binary"
            ),
        );
    }
    let mut c = b.clone();
    c[12] |= 2;
    refused(&c, "sets unknown flag bits 0x2");
    let mut c = b.clone();
    put(&mut c, 16, 1 << 62);
    // Refused before any buffer is sized from the count.
    refused(
        &c,
        &format!(
            "is truncated: its {} byte(s) end inside its doc_lens",
            b.len()
        ),
    );
    let mut c = b.clone();
    put(&mut c, 64, b.len() as u64);
    refused(&c, "is truncated");
    let mut c = b.clone();
    put(&mut c, 80, b.len() as u64);
    refused(&c, "is truncated");
    let mut c = b.clone();
    put(&mut c, 88, b.len() as u64);
    refused(&c, "is truncated");
}

#[test]
fn a_short_file_is_refused_by_where_it_ends() {
    let b = bytes(&mid());
    refused(&[], "is truncated: its 0 byte(s) end inside its header");
    refused(&b[..5], "is truncated: its 5 byte(s) end inside its header");
    refused(
        &b[..50],
        "is truncated: its 50 byte(s) end inside its header",
    );
}

#[test]
fn each_bad_section_is_refused_by_name() {
    let m = small();
    let (b, at) = (bytes(&m), offsets(&m));
    let mut c = b.clone();
    put(&mut c, at.path_ends + 8, get(&b, at.path_ends) - 1);
    refused(&c, "records the end of path 1 before the end of path 0");
    // An end past the next one: every path read alone could be UTF-8, so
    // only the ascending ends, checked at open, refuse it.
    let m = sample(&["a.c", "b.c", "c.c", "d.c"], 1, None);
    let (mut c, at) = (bytes(&m), offsets(&m));
    put(&mut c, at.path_ends + 8, 12);
    refused(&c, "records the end of path 2 before the end of path 1");
    let (b, at) = (bytes(&small()), offsets(&small()));
    let mut c = b.clone();
    put(&mut c, at.label_ends + 8, 0);
    refused(
        &c,
        "records the end of shard label 1 before the end of shard label 0",
    );
    // The last end gives the text's length: past it, the file is short.
    let mut c = b.clone();
    put(&mut c, at.path_ends + 16, get(&b, at.path_ends + 16) + 1);
    refused(&c, "is truncated");
    let m = unserved(&["a.c"], "a.c differs from HEAD");
    let (b, at) = (bytes(&m), offsets(&m));
    let mut c = b.clone();
    c[at.why] = 0xFF;
    refused(&c, "holds a why_no_commit that is not UTF-8");
    let (b, at) = (bytes(&small()), offsets(&small()));
    for (offset, what) in [(at.root, "a corpus_root"), (at.labels, "a shard label")] {
        let mut c = b.clone();
        c[offset] = 0xFF;
        refused(&c, &format!("holds {what} that is not UTF-8"));
    }
}

/// A damaged path text opens. The path it damages is refused when it is
/// read, with the manifest's words; the others read back; the check of
/// every path refuses it with the words it had at open before.
#[test]
fn a_damaged_path_is_refused_when_read() {
    let not_utf8 = refusal("holds a path that is not UTF-8");
    // small(): "a.c", "b/c.rs", "d"; the first byte of "b/c.rs".
    let (b, at) = (bytes(&small()), offsets(&small()));
    let mut c = b.clone();
    c[at.paths + 3] = 0xFF;
    let got = read(&c).unwrap();
    let files = &got.files;
    assert_eq!(files.path(0), Ok(Some("a.c")));
    assert_eq!(files.path(1), Err(not_utf8.clone()));
    assert_eq!(files.path(2), Ok(Some("d")));
    assert_eq!(files.checked().err(), Some(not_utf8.clone()));
    assert_eq!(fate(&c, "a byte of path 1"), Fate::Read);
    // An end inside a character.
    let m = sample(&["é", "b"], 1, None);
    let (mut c, at) = (bytes(&m), offsets(&m));
    put(&mut c, at.path_ends, 1);
    let got = read(&c).unwrap();
    let files = &got.files;
    assert_eq!(files.path(0), Err(not_utf8.clone()));
    assert_eq!(files.path(1), Err(not_utf8.clone()));
    assert_eq!(
        files.checked().err(),
        Some(refusal("cuts path 0 inside a character"))
    );
    assert_eq!(fate(&c, "an end inside é"), Fate::Read);
}

/// A file without a served commit reads as one without a commit.
#[test]
fn a_served_commit_is_its_vcs_a_space_and_its_id() {
    let m = sample(&["a.c", "b"], 1, None);
    let (b, at) = (bytes(&m), offsets(&m));
    assert_eq!(at.commit, at.why);
    assert_eq!(get(&b, 80), 0);
    let with = |commit: &[u8]| {
        let mut c = b[..at.commit].to_vec();
        put(&mut c, 80, commit.len() as u64);
        c.extend_from_slice(commit);
        c.extend_from_slice(&b[at.commit..]);
        c
    };
    let id = "0123456789abcdef0123456789abcdef01234567";
    let pinned = read(&with(format!("git {id}").as_bytes())).unwrap();
    assert_eq!(pinned, served(&["a.c", "b"], id));
    assert_eq!(bytes(&pinned), with(format!("git {id}").as_bytes()));
    // A VCS that a later fmgrep names reads back as it is.
    let sl = read(&with(b"sl 1a2b")).unwrap().commit.unwrap();
    assert_eq!((sl.vcs.as_str(), sl.id.as_str()), ("sl", "1a2b"));
    refused(
        &with(b"git \xFF"),
        "holds a served commit that is not UTF-8",
    );
    // Only a VCS name and a hex id: a ref, a revision, a second word or a
    // letter outside ASCII is not a commit to hand to the VCS.
    for bad in [
        &b"git"[..],
        b"git ",
        b" 1a2b",
        b" ",
        b"git zzzz",
        b"git HEAD",
        b"git main",
        b"git HEAD~0",
        b"git -oops",
        b"git  1a2b",
        b"git 1a2b c",
        b"git \n1",
        b"git 1A2B",
        "git \u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}".as_bytes(),
        b"Git 1a2b",
        b"g-t 1a2b",
    ] {
        refused(
            &with(bad),
            "holds a served commit that is not a VCS, a space and a hex id",
        );
    }
}

#[test]
fn a_manifest_the_layout_cannot_hold_is_not_written() {
    let mut m = small();
    m.num_docs += 1;
    assert!(encode(&m, &mut Vec::new()).is_err());
    let mut m = small();
    m.doc_lens = m.doc_lens[1..].to_vec().into();
    assert!(encode(&m, &mut Vec::new()).is_err());
    let mut m = small();
    m.blob_lens.pop();
    assert!(encode(&m, &mut Vec::new()).is_err());
    let mut m = small();
    m.corpus_root = Some(String::new());
    assert!(encode(&m, &mut Vec::new()).is_err());
    for (vcs, id) in [
        ("", "1a2b"),
        ("g it", "1a2b"),
        ("git", ""),
        ("git", "HEAD"),
        ("git", " 1a2b"),
        ("git", "1a2b\n"),
        ("Git", "1a2b"),
    ] {
        let mut m = served(&["a"], id);
        m.commit.as_mut().unwrap().vcs = vcs.into();
        assert!(encode(&m, &mut Vec::new()).is_err(), "{vcs:?} {id:?}");
    }
}

/// The checks of the values run as on the JSON, with the same words.
#[test]
fn values_are_checked_as_before() {
    let at = Path::new(AT);
    assert_eq!(validate(&small(), at), Ok(()));
    assert_eq!(validate(&sample(&["a"], 1, None), at), Ok(()));
    let check = |edit: &dyn Fn(&mut Manifest), want: &str| {
        let mut m = small();
        edit(&mut m);
        let err = validate(&m, at).unwrap_err();
        assert!(
            err.contains(want) && err.contains("refusing a tampered index"),
            "{err}"
        );
    };
    check(&|m| m.corpus_root = None, "records no absolute corpus_root");
    check(
        &|m| m.corpus_root = Some("tree".into()),
        "records no absolute corpus_root",
    );
    check(&|m| m.store_docs = true, "records a corpus_root");
    check(&|m| m.bom_bytes = m.num_bytes + 1, "BOM byte(s)");
    check(
        &|m| m.doc_lens = m.doc_lens[..2].to_vec().into(),
        "per-doc tables skew (files=3 doc_lens=2)",
    );
    check(
        &|m| m.blob_lens.truncate(1),
        "blob tables skew (blobs=2 blob_lens=1)",
    );
    assert_eq!(validate(&served(&["a"], "1a2b"), at), Ok(()));
    assert_eq!(validate(&unserved(&["a"], "a.c differs"), at), Ok(()));
    check(
        &|m| m.why_no_commit = Some("a.c differs".into()),
        "records why it has no commit but reads documents from files",
    );
    let mut both = served(&["a"], "1a2b");
    both.why_no_commit = Some("a.c differs".into());
    let err = validate(&both, at).unwrap_err();
    assert!(
        err.contains("records why it has no commit but records a commit")
            && err.contains("refusing a tampered index"),
        "{err}"
    );
    check(
        &|m| m.commit = served(&[], "1a2b").commit,
        "reads documents from files but records a commit",
    );
}

/// The read-ahead covers the header and the tables, and never more than
/// the file.
#[test]
fn the_tables_end_where_the_root_starts() {
    for m in [small(), mid()] {
        let b = bytes(&m);
        assert_eq!(tables_len(&b), offsets(&m).root);
        // Counts the file cannot hold ask for no read-ahead: decode
        // refuses them.
        for count in [u64::MAX, 1 << 26, b.len() as u64 / 16] {
            for at in [16, 64] {
                let mut c = b.clone();
                put(&mut c, at, count);
                assert_eq!(tables_len(&c), 0, "count {count} at {at}");
                assert!(read(&c).is_err());
            }
        }
    }
    assert_eq!(tables_len(&bytes(&small())[..50]), 0);
}

/// A file of another kind or format is refused from its header alone: its
/// counts size no read-ahead, however large the file.
#[test]
fn a_foreign_header_asks_for_no_read_ahead() {
    // A count of 2^20 documents, and the bytes to hold their tables.
    let n = 1 << 20;
    let mut b = bytes(&small());
    put(&mut b, 16, n);
    b.resize(96 + 16 * (n as usize + 2), 0);
    assert_eq!(tables_len(&b), b.len());
    let format = 7u32.to_le_bytes();
    for (at, with, why) in [
        (0..8, &b"NOTMANIF"[..], "is not an fmgrep manifest"),
        (8..12, &format[..], "unsupported manifest format 7"),
    ] {
        let mut c = b.clone();
        c[at].copy_from_slice(with);
        assert_eq!(tables_len(&c), 0, "{why}");
        let err = read(&c).unwrap_err();
        assert!(err.contains(why), "{err}");
    }
}
