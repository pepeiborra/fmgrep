use super::*;

const DOCS: [&[u8]; 17] = [
    b"",
    b"\n",
    b"\n\n",
    b"x\n\n",
    b"alpha one\nbeta\n",
    b"alpha last",
    b"one\r\ntwo\r\n",
    b"  \t\n",
    b"xxx\nyyy",
    b"a\nb\nc",
    b"word\nword2 x",
    b"caf\xc3\xa9 KELVIN \xe2\x84\xaa\n",
    b"\xff\xfe raw bytes\n",
    b"only\n",
    b"ALPHA\nalpha\n",
    b"end with space \n",
    b"tab\there\n",
];

const PATTERNS: [&str; 24] = [
    r"\s",
    r"[^x]",
    r"\Aalpha",
    r"one\z",
    r"^$",
    r"$",
    r"(?s).",
    r"a*",
    r"(?-m)^b",
    r"\W",
    r"\bword\b",
    r"x$",
    r"^y",
    r"\s+\z",
    r"[^a-z]+",
    r"(?-m)$",
    r"\Ay",
    r"(a|[\n\r])",
    r"[\s\S]",
    r"\x20$",
    r"k",
    r"\r$",
    r"(\Ab|c\z)",
    r"^\s*$",
];

fn scan(src: &str, mode: MatchMode, doc: &[u8]) -> bool {
    LineMatcher::regex(src, None, mode)
        .unwrap()
        .scan(doc, QueryOutput::FilesWithMatches)
        .matched_lines
        > 0
}

fn set_of<'a>(srcs: &[&'a str], matchers: &'a [LineMatcher], mode: MatchMode) -> LeafSet<'a> {
    let leaves: Vec<Leaf<'a>> = srcs
        .iter()
        .zip(matchers)
        .map(|(src, matcher)| Leaf::Pattern { src, matcher })
        .collect();
    LeafSet::new(&leaves, mode)
}

fn filled(set: &LeafSet<'_>, doc: &[u8]) -> Vec<bool> {
    let mut known = vec![None; set.tests.len()];
    set.fill(doc, &mut known);
    known.into_iter().map(Option::unwrap).collect()
}

#[test]
fn set_membership_equals_line_scan() {
    for ignore_case in [false, true] {
        let mode = MatchMode {
            ignore_case,
            ..MatchMode::default()
        };
        let matchers: Vec<LineMatcher> = PATTERNS
            .iter()
            .map(|p| LineMatcher::regex(p, None, mode).unwrap())
            .collect();
        let set = set_of(&PATTERNS, &matchers, mode);
        assert!(set.set.is_some());
        for doc in DOCS {
            let got = filled(&set, doc);
            for (p, got) in PATTERNS.iter().zip(got) {
                assert_eq!(
                    got,
                    scan(p, mode, doc),
                    "-i={ignore_case} {p:?} on {:?}",
                    String::from_utf8_lossy(doc)
                );
            }
        }
    }
    // Under -U a pattern that cannot match a terminator keeps the line path.
    let mode = MatchMode {
        multiline: true,
        ..MatchMode::default()
    };
    let srcs = [r"\bword\b", r"alpha \w+", r"[a-z]{3}"];
    let matchers: Vec<LineMatcher> = srcs
        .iter()
        .map(|p| LineMatcher::regex(p, None, mode).unwrap())
        .collect();
    let set = set_of(&srcs, &matchers, mode);
    assert!(set.tests.iter().all(|t| matches!(t, Test::Set(..))));
    for doc in DOCS {
        for (p, got) in srcs.iter().zip(filled(&set, doc)) {
            assert_eq!(got, scan(p, mode, doc), "-U {p:?}");
        }
    }
}

#[test]
fn crlf_and_multiline_leaves_use_their_own_scan() {
    let cases: [(&str, MatchMode); 4] = [
        (r"(?R)two$", MatchMode::default()),
        (
            r"a\nb",
            MatchMode {
                multiline: true,
                ..MatchMode::default()
            },
        ),
        (
            r"\Aa",
            MatchMode {
                multiline: true,
                ..MatchMode::default()
            },
        ),
        (
            r"(?s)b.*\z",
            MatchMode {
                multiline: true,
                dotall: true,
                ..MatchMode::default()
            },
        ),
    ];
    for (src, mode) in cases {
        let matchers = [LineMatcher::regex(src, None, mode).unwrap()];
        let set = set_of(&[src], &matchers, mode);
        assert!(matches!(set.tests[0], Test::Own(_)), "{src}");
        for doc in DOCS {
            assert_eq!(
                filled(&set, doc),
                [scan(src, mode, doc)],
                "{src:?} on {doc:?}"
            );
        }
    }
}

#[test]
fn literal_leaves_match_by_containment() {
    let set = LeafSet::new(
        &[
            Leaf::Literal(b"a\nb"),
            Leaf::Literal(b""),
            Leaf::Never,
            Leaf::Literal(b"beta"),
        ],
        MatchMode::default(),
    );
    assert_eq!(filled(&set, b"a\nb\nc"), [true, false, false, false]);
    assert_eq!(
        filled(&set, b"alpha one\nbeta\n"),
        [false, false, false, true]
    );
    // A known answer is kept, never recomputed.
    let mut known = [Some(false), None, None, Some(false)];
    set.fill(b"a\nb beta", &mut known);
    assert_eq!(known, [Some(false), Some(false), Some(false), Some(false)]);
}
