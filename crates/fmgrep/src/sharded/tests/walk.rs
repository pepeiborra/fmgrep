//! Per-shard routing of a pattern whose anchor is too long to plan eagerly.

use super::*;

#[test]
fn a_rare_long_anchor_still_walks() {
    let docs = docs_of(&[b"CATALOG (\"%s\")", b"ALOGE(\"%s\")"]);
    assert_eq!(pruned(&docs, "ALOG[EW]\\(\"[^\"]*%s"), vec![1]);
}

#[test]
fn a_walk_that_gave_up_prunes_by_the_rarest_literal() {
    let mut wide = String::new();
    for a in 'a'..='z' {
        for b in 'a'..='z' {
            wide.push_str(&format!("zz{a}{b}_tail\n"));
        }
    }
    let docs = docs_of(&[wide.as_bytes(), b"zzab_tail rare"]);
    assert_eq!(pruned(&docs, "zz[a-z][a-z]_tail.*rare"), vec![1]);
}
