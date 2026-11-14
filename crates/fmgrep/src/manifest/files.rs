//! The manifest's paths: one text, one end offset per path.

use std::fmt;
use std::ops::{Index, Range};

use serde::{Serialize, Serializer};

/// Corpus-relative paths by doc id, held as one text: path `i` is
/// `text[ends[i]..ends[i + 1]]`. One heap block for every path, not one
/// per path.
#[derive(Clone, PartialEq, Eq)]
pub struct FileTable {
    text: String,
    /// `ends[0] == 0`; `ends.len() == len() + 1`; ascending, each on a char
    /// boundary of `text`, the last one `text.len()`.
    ends: Vec<usize>,
}

impl Default for FileTable {
    fn default() -> Self {
        Self {
            text: String::new(),
            ends: vec![0],
        }
    }
}

impl FileTable {
    pub fn len(&self) -> usize {
        self.ends.len() - 1
    }

    pub fn get(&self, id: usize) -> Option<&str> {
        let (&lo, &hi) = (self.ends.get(id)?, self.ends.get(id + 1)?);
        Some(&self.text[lo..hi])
    }

    /// The paths in id order.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &str> + ExactSizeIterator + Clone {
        self.ends.windows(2).map(|w| &self.text[w[0]..w[1]])
    }

    /// The first id in `range` whose path fails `pred`, as
    /// [`slice::partition_point`] over that part of the table.
    pub fn partition_point(
        &self,
        range: Range<usize>,
        mut pred: impl FnMut(&str) -> bool,
    ) -> usize {
        partition_ids(range, |id| pred(&self[id]))
    }

    /// As [`slice::binary_search`] over an ascending table.
    pub fn binary_search(&self, path: &str) -> Result<usize, usize> {
        let at = self.partition_point(0..self.len(), |p| p < path);
        match self.get(at) {
            Some(p) if p == path => Ok(at),
            _ => Err(at),
        }
    }

    pub fn is_sorted(&self) -> bool {
        self.iter().is_sorted()
    }

    fn push(&mut self, path: &str) {
        self.text.push_str(path);
        self.ends.push(self.text.len());
    }

    /// The table of `text` cut at `ends`. The caller has checked that
    /// `ends` starts at 0, ascends, ends at `text.len()` and cuts only at
    /// char boundaries.
    pub(super) fn from_parts(text: String, ends: Vec<usize>) -> Self {
        debug_assert!(ends.first() == Some(&0) && ends.last() == Some(&text.len()));
        debug_assert!(ends.is_sorted() && ends.iter().all(|&e| text.is_char_boundary(e)));
        Self { text, ends }
    }
}

/// The first index in `range` where `pred` fails, as
/// [`slice::partition_point`] over the indices: `pred` holds on a prefix
/// of `range` and fails on the rest.
pub fn partition_ids(range: Range<usize>, mut pred: impl FnMut(usize) -> bool) -> usize {
    let (mut lo, mut hi) = (range.start, range.end);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if pred(mid) {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

impl Index<usize> for FileTable {
    type Output = str;

    fn index(&self, id: usize) -> &str {
        &self.text[self.ends[id]..self.ends[id + 1]]
    }
}

impl<S: AsRef<str>> FromIterator<S> for FileTable {
    fn from_iter<I: IntoIterator<Item = S>>(paths: I) -> Self {
        let mut table = Self::default();
        for path in paths {
            table.push(path.as_ref());
        }
        table
    }
}

impl fmt::Debug for FileTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

/// A JSON list of strings, as a `Vec<String>` would be.
impl Serialize for FileTable {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(self.iter())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(paths: &[&str]) -> FileTable {
        paths.iter().collect()
    }

    #[test]
    fn paths_read_back_by_id() {
        let t = table(&["a/one.rs", "b", "é/ü.txt", "中/文", "😀"]);
        assert_eq!(t.len(), 5);
        assert_eq!(t.get(0), Some("a/one.rs"));
        assert_eq!(t.get(2), Some("é/ü.txt"));
        assert_eq!(&t[3], "中/文");
        assert_eq!(&t[4], "😀");
        assert_eq!(t.get(5), None);
        assert_eq!(
            t.iter().collect::<Vec<_>>(),
            ["a/one.rs", "b", "é/ü.txt", "中/文", "😀"]
        );
        assert_eq!(t.iter().len(), 5);
        assert_eq!(t.iter().next_back(), Some("😀"));
    }

    #[test]
    fn an_empty_table_holds_no_path() {
        let t = FileTable::default();
        assert_eq!(t.len(), 0);
        assert_eq!(t.get(0), None);
        assert_eq!(t.iter().count(), 0);
        assert_eq!(t.binary_search("a"), Err(0));
        assert!(t.is_sorted());
        assert_eq!(t, table(&[]));
        // An empty path is a path.
        let t = table(&[""]);
        assert_eq!((t.len(), t.get(0)), (1, Some("")));
    }

    #[test]
    fn searches_match_the_slice_ones() {
        let paths = ["a", "a/b", "a/c", "b", "b/d", "c"];
        let t = table(&paths);
        for key in ["", "a", "a/", "a/b", "a/bb", "b", "bz", "c", "d"] {
            assert_eq!(t.binary_search(key), paths.binary_search(&key), "{key}");
        }
        for range in [0..6, 1..4, 3..6, 2..2] {
            for key in ["a", "a/c", "b", "z"] {
                let want = range.start + paths[range.clone()].partition_point(|p| *p < key);
                assert_eq!(t.partition_point(range.clone(), |p| p < key), want);
            }
        }
        assert!(t.is_sorted());
        assert!(!table(&["b", "a"]).is_sorted());
    }

    #[test]
    fn json_is_a_list_of_strings() {
        let paths = ["a\"b", "c\\d", "é", "line\nbreak", "tab\t", "\u{1F600}"];
        let t = table(&paths);
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(json, serde_json::to_string(&paths).unwrap());
        assert_eq!(format!("{t:?}"), format!("{paths:?}"));
    }
}
