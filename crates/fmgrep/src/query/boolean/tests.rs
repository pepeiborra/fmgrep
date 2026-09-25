use super::*;

/// Deterministic xorshift, so failures replay.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> usize {
        (self.next() % n) as usize
    }
}

fn tree(rng: &mut Rng, depth: u32) -> Expr {
    if depth == 0 || rng.below(3) == 0 {
        return Expr::Term(String::new());
    }
    let kids = (0..1 + rng.below(3))
        .map(|_| tree(rng, depth - 1))
        .collect();
    if rng.below(2) == 0 {
        Expr::And(kids)
    } else {
        Expr::Or(kids)
    }
}

/// Kleene truth: 0 false, 1 unknown, 2 true.
fn kleene(expr: &Expr, leaf: &[u8], next: &mut usize) -> u8 {
    match expr {
        Expr::Term(_) | Expr::Regex(_) => {
            *next += 1;
            leaf[*next - 1]
        }
        Expr::And(v) => v.iter().map(|e| kleene(e, leaf, next)).fold(2, u8::min),
        Expr::Or(v) => v.iter().map(|e| kleene(e, leaf, next)).fold(0, u8::max),
    }
}

#[test]
fn tri_algebra_matches_per_doc_kleene_eval() {
    const DOCS: usize = 12;
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for _ in 0..500 {
        let expr = tree(&mut rng, 4);
        let n = expr.leaves().len();
        // Per leaf and doc: 0 absent, 1 maybe, 2 yes.
        let truth: Vec<Vec<u8>> = (0..n)
            .map(|_| (0..DOCS).map(|_| rng.below(3) as u8).collect())
            .collect();
        let tris: Vec<Tri> = truth
            .iter()
            .map(|t| Tri {
                yes: (0..DOCS).filter(|&d| t[d] == 2).collect(),
                maybe: (0..DOCS).filter(|&d| t[d] == 1).collect(),
            })
            .collect();
        let got = prune(&expr, &tris, &mut 0);
        let per_doc: Vec<u8> = (0..DOCS)
            .map(|d| {
                kleene(
                    &expr,
                    &truth.iter().map(|t| t[d]).collect::<Vec<_>>(),
                    &mut 0,
                )
            })
            .collect();
        let want = Tri {
            yes: (0..DOCS).filter(|&d| per_doc[d] == 2).collect(),
            maybe: (0..DOCS).filter(|&d| per_doc[d] == 1).collect(),
        };
        assert_eq!(got, want, "{expr:?}");
    }
}

#[test]
fn overlap_is_containment_or_a_suffix_prefix_border() {
    assert!(can_overlap(b"abc", b"b"));
    assert!(can_overlap(b"abc", b"cde"));
    assert!(can_overlap(b"cde", b"abc"));
    assert!(can_overlap(b"x", b"x"));
    assert!(!can_overlap(b"abc", b"def"));
    assert!(!can_overlap(b"HscEnv", b"DynFlags"));
}
