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
        doc_lens,
        blob_lens: (0..shards as u64).map(|s| 100 + s).collect(),
        corpus_root: root.map(str::to_owned),
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
    m.files.iter().map(|p| p.len()).sum()
}

fn bytes(m: &Manifest) -> Vec<u8> {
    let mut out = Vec::new();
    encode(m, &mut out).unwrap();
    out
}

fn read(b: &[u8]) -> Result<Manifest, String> {
    decode(&mut &b[..], b.len() as u64, Path::new(AT))
}

fn put(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

fn get(b: &[u8], at: usize) -> u64 {
    u64_at(b, at)
}

/// Where each section of `m`'s file starts.
struct Offsets {
    path_ends: usize,
    label_ends: usize,
    root: usize,
    labels: usize,
    paths: usize,
}

fn offsets(m: &Manifest) -> Offsets {
    let (n, s) = (m.files.len(), m.blobs.len());
    let path_ends = 88 + 8 * n;
    let label_ends = path_ends + 8 * n + 8 * s;
    let root = label_ends + 8 * s;
    let labels = root + m.corpus_root.as_deref().map_or(0, str::len);
    let paths = labels + m.blobs.iter().map(String::len).sum::<usize>();
    Offsets {
        path_ends,
        label_ends,
        root,
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
    max.doc_lens = vec![u64::MAX];
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
        "07000000",         // format
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
}

#[test]
fn every_truncation_is_refused() {
    for m in [small(), mid()] {
        let b = bytes(&m);
        for cut in 0..b.len() {
            refused(&b[..cut], "is truncated");
        }
        let mut long = b.clone();
        long.push(0);
        refused(&long, "holds 1 byte(s) past its last section");
        // The file shrank after its size was read.
        let err = decode(&mut &b[..b.len() - 1], b.len() as u64, Path::new(AT)).unwrap_err();
        assert!(err.contains("is truncated"), "{err}");
    }
}

/// Whether `c`, a changed manifest file, is refused with the advice to
/// re-index (true), or reads back as a manifest that writes the same bytes
/// (false). Never a panic.
fn refused_or_whole(c: &[u8], what: &str) -> bool {
    match read(c) {
        Err(e) => {
            assert!(
                e.ends_with("re-run `index`") || e.contains("re-run `index` with"),
                "{what}: {e}"
            );
            true
        }
        Ok(got) => {
            assert_eq!(bytes(&got), c, "{what}");
            false
        }
    }
}

/// A changed byte is refused, or reads back whole. A changed length or
/// scalar reads back changed, as a changed JSON number did.
#[test]
fn every_byte_change_is_refused_or_reads_back_whole() {
    for m in [small(), mid()] {
        let b = bytes(&m);
        let (mut refusals, mut reads) = (0, 0);
        for at in 0..b.len() {
            for new in [b[at] ^ 0x01, b[at] ^ 0x80, 0x00, 0xFF] {
                if new == b[at] {
                    continue;
                }
                let mut c = b.clone();
                c[at] = new;
                match refused_or_whole(&c, &format!("byte {at} = {new:#x}")) {
                    true => refusals += 1,
                    false => reads += 1,
                }
            }
        }
        assert!(refusals > 0 && reads > 0, "{refusals} {reads}");
    }
}

/// The values a hostile file would put in a count, a length or an end,
/// in each word of the header and of the tables: refused, or read back
/// whole. A count of 2^60 sizes no buffer.
#[test]
fn edge_values_in_every_word_are_refused_or_read_back() {
    for m in [small(), mid()] {
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
                refused_or_whole(&c, &format!("word {at} = {v:#x}"));
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
    refused(&c, &format!("holds {} byte(s), its sections need", b.len()));
    let mut c = b.clone();
    put(&mut c, 64, b.len() as u64);
    refused(&c, "is truncated");
    let mut c = b.clone();
    put(&mut c, 80, b.len() as u64);
    refused(&c, "is truncated");
}

#[test]
fn each_bad_section_is_refused_by_name() {
    let m = small();
    let (b, at) = (bytes(&m), offsets(&m));
    let mut c = b.clone();
    put(&mut c, at.path_ends + 8, get(&b, at.path_ends) - 1);
    refused(&c, "records the end of path 1 before the end of path 0");
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
    for (offset, what) in [
        (at.root, "a corpus_root"),
        (at.labels, "a shard label"),
        (at.paths, "a path"),
    ] {
        let mut c = b.clone();
        c[offset] = 0xFF;
        refused(&c, &format!("holds {what} that is not UTF-8"));
    }
    let m = sample(&["é", "b"], 1, None);
    let (mut c, at) = (bytes(&m), offsets(&m));
    put(&mut c, at.path_ends, 1);
    refused(&c, "cuts path 0 inside a character");
}

/// A later fmgrep fills the served commit: this one checks it and reads
/// past it.
#[test]
fn a_served_commit_is_read_past() {
    let m = small();
    let (b, at) = (bytes(&m), offsets(&m));
    let with = |commit: &[u8]| {
        let mut c = b[..at.labels].to_vec();
        put(&mut c, 80, commit.len() as u64);
        c.extend_from_slice(commit);
        c.extend_from_slice(&b[at.labels..]);
        c
    };
    assert_eq!(read(&with(b"git 0123456789abcdef")), Ok(m));
    refused(
        &with(b"git \xFF"),
        "holds a served commit that is not UTF-8",
    );
}

#[test]
fn a_manifest_the_layout_cannot_hold_is_not_written() {
    let mut m = small();
    m.num_docs += 1;
    assert!(encode(&m, &mut Vec::new()).is_err());
    let mut m = small();
    m.doc_lens.pop();
    assert!(encode(&m, &mut Vec::new()).is_err());
    let mut m = small();
    m.blob_lens.pop();
    assert!(encode(&m, &mut Vec::new()).is_err());
    let mut m = small();
    m.corpus_root = Some(String::new());
    assert!(encode(&m, &mut Vec::new()).is_err());
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
        &|m| m.doc_lens.truncate(2),
        "per-doc tables skew (files=3 doc_lens=2)",
    );
    check(
        &|m| m.blob_lens.truncate(1),
        "blob tables skew (blobs=2 blob_lens=1)",
    );
}
