//! A walk that gives up after it walked its anchor: what each shard locates
//! and keeps, against the anchor it fell back to before.

use super::*;

/// What one shard's walk of `p` locates and keeps, with the frontier
/// (`frontier`) or without it: rows located, then docs.
fn walked(
    meta: &ShardMeta,
    s: &Structure,
    p: &PreparedPattern,
    frontier: bool,
) -> (u64, Vec<usize>) {
    let (plan, anchor) = (p.automaton().expect("a walk"), crate::pattern::anchor(p));
    let with = anchor.filter(|_| frontier);
    let hits = crate::automaton::automaton_locate(s.cursor(), plan, with);
    let rows = match &hits {
        Some(hits) => hits.len() as u64,
        None => anchor.map_or(0, |a| meta.count(s, a)),
    };
    (rows, meta.docs_at(s, hits, anchor))
}

/// [`walked`] summed over every shard of `index`: with the frontier, then
/// without it.
fn both(index: &DocfmIndex, p: &PreparedPattern) -> [(u64, Vec<usize>); 2] {
    [true, false].map(|frontier| {
        let mut all = (0, Vec::new());
        for (meta, s) in index.metas.iter().zip(&index.shards) {
            let (rows, docs) = walked(meta, s, p, frontier);
            all.0 += rows;
            all.1.extend(docs);
        }
        all
    })
}

/// `n` distinct `[0-9A-F]{3}ULL` strings, one a line, none after `0x`.
fn hex_ull(n: u32) -> Vec<u8> {
    (0..n)
        .flat_map(|i| format!(" {i:03X}ULL\n").into_bytes())
        .collect()
}

#[test]
fn a_walk_that_gave_up_after_its_anchor_keeps_its_frontier_docs() {
    // Doc 1 holds 300 distinct `hhhULL`: over the 256 live intervals, so
    // its shard's walk gives up on the `hhULL` strings, which docs 2, 4
    // and 5 do not hold.
    let noise = hex_ull(300);
    let docs = docs_of(&[
        b"a = 0x12ABULL;\n",
        &noise,
        b"return NULL;\n",
        b"b = 0x00FFULL; c = NULL;\n",
        b" ULL ULL\n",
        b"9ULL",
    ]);
    let pattern = "0x[0-9A-F]{4}ULL";
    let p = PreparedPattern::prepare(pattern).unwrap();
    let one = DocfmIndex::build_with_cap(&docs, 1 << 20);
    assert_eq!(one.blob_slots(), 1);
    let [frontier, anchor] = both(&one, &p);
    assert_eq!(frontier, (302, vec![0, 1, 3]));
    assert_eq!(anchor, (307, vec![0, 1, 2, 3, 4, 5]));
    for cap in CAPS {
        let index = DocfmIndex::build_with_cap(&docs, cap);
        let got = candidates(&index, pattern, docs.len());
        assert_eq!(got, vec![0, 1, 3], "cap={cap}");
    }
    assert_eq!(pruned(&docs, pattern), vec![0, 1, 3]);
}

/// `lines` lines of hex constants and their neighbours, as code holds them;
/// with `plain`, only words that end like them.
fn hex_doc(rng: &mut crate::tests::support::Rng, lines: usize, plain: bool) -> Vec<u8> {
    const HEX: &[u8] = b"0123456789ABCDEF";
    let mut doc = String::new();
    for _ in 0..lines {
        let kind = if plain {
            8 + rng.below(2)
        } else {
            rng.below(8)
        };
        let mut hex = |n: usize| -> String { (0..n).map(|_| HEX[rng.below(16)] as char).collect() };
        let line = match kind {
            0 => format!("x = 0x{}ULL;", hex(8)),
            1 => format!(" {}ULL", hex(3)),
            2 => "return NULL;".to_string(),
            3 => format!("0x{}L,", hex(4)),
            4 => format!("{}UL ULL", hex(2)),
            5 => format!("0x{}uLL", hex(4)),
            6 => format!("0x{}ull", hex(4).to_lowercase()),
            7 => format!("{};", hex(4)),
            8 => "return NULL;".to_string(),
            _ => "p = NULL; q = ULL; /* UL L, uLL ull 0x */".to_string(),
        };
        doc.push_str(&line);
        doc.push('\n');
    }
    doc.into_bytes()
}

#[test]
fn the_frontier_never_locates_more_rows_or_keeps_more_docs() {
    let patterns = [
        ("0x[0-9A-F]{8}ULL", false),
        ("0x[0-9A-F]{4}ULL", false),
        ("[0-9A-F]{3}ULL", false),
        ("[0-9A-F]{2}UL", false),
        ("0x[0-9A-F]{4}L", false),
        ("0x[0-9A-F]{2,4}(UL|ULL)", false),
        ("(0x|x)[0-9A-F]{4}uLL", false),
        ("[0-9A-F]{4}ULL[;,]", false),
        ("[0-9A-F]{3}(ULL|L,)", false),
        ("0x[0-9a-f]{4}ull", true),
        ("[0-9a-f]{3}ull", true),
    ];
    let mut rng = crate::tests::support::Rng(0x2545_F491_4F6C_DD1D);
    let (mut fewer, mut same) = (0, 0);
    for round in 0..3 {
        let docs: Vec<Vec<u8>> = [2000, 3, 40, 300, 5, 900, 1, 8]
            .iter()
            .enumerate()
            .map(|(i, &n)| hex_doc(&mut rng, n, i % 4 == 1))
            .collect();
        let docs = docs_of(&docs.iter().map(Vec::as_slice).collect::<Vec<_>>());
        for cap in [1 << 20, 30_000, 4_000] {
            let index = DocfmIndex::build_with_cap(&docs, cap);
            for (pattern, folded) in patterns {
                let mode = if folded { ci() } else { MatchMode::default() };
                let p = PreparedPattern::prepare_with(pattern, mode).unwrap();
                let what = format!("round {round} cap {cap} {pattern:?}");
                let [(rows, got), (anchor_rows, anchor)] = both(&index, &p);
                let found = candidate_docs(index.regex_candidates(&p, None), docs.len());
                assert_eq!(found, got, "{what}");
                assert!(rows <= anchor_rows, "{what}: {rows} > {anchor_rows} rows");
                assert!(got.iter().all(|d| anchor.contains(d)), "{what}");
                let mut matching = (0..docs.len()).filter(|&d| p.count(&docs[d].bytes) > 0);
                assert!(matching.all(|d| got.contains(&d)), "{what}");
                match got.len() < anchor.len() {
                    true => fewer += 1,
                    false => same += 1,
                }
            }
        }
    }
    // Some walks gave up and kept fewer docs; the others kept as many.
    assert!(fewer > 5 && same > 5, "fewer {fewer} same {same}");
}
