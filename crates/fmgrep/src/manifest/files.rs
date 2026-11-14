//! The manifest's paths: one text, one end offset per path, both read in
//! place in the bytes of `manifest.bin`.

use std::fmt;
use std::ops::{Deref, Index, Range};
use std::panic::RefUnwindSafe;
use std::path::Path;
use std::sync::Arc;

use serde::{Serialize, Serializer};
use zerocopy::little_endian::U64;
use zerocopy::{FromBytes, IntoBytes};

/// What holds the bytes of a manifest file.
pub(super) trait Bytes: AsRef<[u8]> + Send + Sync + RefUnwindSafe {}

impl<T: AsRef<[u8]> + Send + Sync + RefUnwindSafe> Bytes for T {}

/// The bytes of a manifest, shared by the tables that view them.
#[derive(Clone)]
pub(super) struct Store(Arc<dyn Bytes>);

impl Store {
    pub(super) fn new(bytes: impl Bytes + 'static) -> Self {
        Self(Arc::new(bytes))
    }
}

impl Deref for Store {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        (*self.0).as_ref()
    }
}

/// Corpus-relative paths by doc id, held as one text: path `i` runs from
/// the end of path `i - 1` (0 for the first) to its own end. The ends and
/// the text are read where they lie in the manifest's bytes.
///
/// The decoder checks that the ends ascend and that the last one is the
/// length of the text. Their UTF-8 is checked when they are read:
/// [`Self::path`] checks one path, [`Self::checked`] all of them.
#[derive(Clone)]
pub struct FileTable {
    store: Store,
    /// Where the `len` ends start in `store`: little-endian u64 words.
    ends_at: usize,
    /// Where the path text starts in `store`; it runs to the end.
    text_at: usize,
    len: usize,
    /// The manifest, named in a refusal.
    origin: Arc<Path>,
}

impl Default for FileTable {
    fn default() -> Self {
        std::iter::empty::<&str>().collect()
    }
}

/// Equal when they hold the same paths.
impl PartialEq for FileTable {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len && (0..self.len).all(|id| self.raw(id) == other.raw(id))
    }
}

impl Eq for FileTable {}

impl FileTable {
    pub fn len(&self) -> usize {
        self.len
    }

    /// The ends, as the decoder found them.
    fn ends(&self) -> &[U64] {
        let ends = self.store.get(self.ends_at..).unwrap_or_default();
        <[U64]>::ref_from_prefix_with_elems(ends, self.len).map_or(&[], |(ends, _)| ends)
    }

    fn text(&self) -> &[u8] {
        self.store.get(self.text_at..).unwrap_or_default()
    }

    /// The bytes of path `id`, unchecked; `None` past the last path.
    pub fn raw(&self, id: usize) -> Option<&[u8]> {
        let ends = self.ends();
        let hi = ends.get(id)?.get();
        let lo = id.checked_sub(1).map_or(0, |prev| ends[prev].get());
        let (lo, hi) = (usize::try_from(lo).ok()?, usize::try_from(hi).ok()?);
        self.text().get(lo..hi)
    }

    /// Path `id`, checked; `None` past the last path. A path that is not
    /// UTF-8, or that an end cuts inside a character, is refused with the
    /// manifest's name.
    pub fn path(&self, id: usize) -> Result<Option<&str>, String> {
        if id >= self.len {
            return Ok(None);
        }
        match self.raw(id).map(std::str::from_utf8) {
            Some(Ok(path)) => Ok(Some(path)),
            _ => Err(self.refusal("holds a path that is not UTF-8")),
        }
    }

    /// Every path, checked once over the whole text: for the users of
    /// every path. Refuses a text that is not UTF-8, then an end that cuts
    /// inside a character, with the manifest's name.
    pub fn checked(&self) -> Result<Checked<'_>, String> {
        let text = std::str::from_utf8(self.text())
            .map_err(|_| self.refusal("holds a path that is not UTF-8"))?;
        let ends = self.ends();
        if let Some(id) = ends
            .iter()
            .position(|e| usize::try_from(e.get()).map_or(true, |e| !text.is_char_boundary(e)))
        {
            return Err(self.refusal(&format!("cuts path {id} inside a character")));
        }
        Ok(Checked { text, ends })
    }

    /// The first id in `range` whose path fails `pred`, as
    /// [`slice::partition_point`]; a path read on the way is checked.
    pub fn partition_point(
        &self,
        range: Range<usize>,
        mut pred: impl FnMut(&str) -> bool,
    ) -> Result<usize, String> {
        let mut failed = None;
        let at = partition_ids(range, |id| match self.path(id) {
            Ok(path) => path.is_some_and(&mut pred),
            Err(e) => {
                failed.get_or_insert(e);
                false
            }
        });
        failed.map_or(Ok(at), Err)
    }

    /// Ascending by bytes, which is the order of the strings.
    pub fn is_sorted(&self) -> bool {
        (0..self.len).map(|id| self.raw(id)).is_sorted()
    }

    fn refusal(&self, why: &str) -> String {
        format!("error: {} {why}; re-run `index`", self.origin.display())
    }

    /// The table of the `len` ends at `ends_at` in `store`, cutting the
    /// text that runs from `text_at` to the end of `store`, read from the
    /// manifest at `origin`. The caller has checked that the ends ascend
    /// and that the last one is the length of the text.
    pub(super) fn from_parts(
        store: Store,
        ends_at: usize,
        text_at: usize,
        len: usize,
        origin: &Path,
    ) -> Self {
        let table = Self {
            store,
            ends_at,
            text_at,
            len,
            origin: origin.into(),
        };
        debug_assert_eq!(table.ends().len(), len);
        debug_assert!(table.ends().is_sorted_by_key(|e| e.get()));
        debug_assert_eq!(
            table.ends().last().map_or(0, |e| e.get()),
            table.text().len() as u64
        );
        table
    }
}

/// The paths of a [`FileTable`] whose text has been checked whole: every
/// read is infallible.
#[derive(Clone, Copy)]
pub struct Checked<'a> {
    text: &'a str,
    ends: &'a [U64],
}

impl<'a> Checked<'a> {
    pub fn len(&self) -> usize {
        self.ends.len()
    }

    /// Path `id`; `None` past the last path.
    pub fn get(&self, id: usize) -> Option<&'a str> {
        (id < self.len()).then(|| self.at(id))
    }

    /// The paths in id order.
    pub fn iter(self) -> impl DoubleEndedIterator<Item = &'a str> + ExactSizeIterator + Clone {
        (0..self.len()).map(move |id| self.at(id))
    }

    /// Path `id`, which is below [`Self::len`].
    fn at(self, id: usize) -> &'a str {
        let hi = self.ends[id].get() as usize;
        let lo = id
            .checked_sub(1)
            .map_or(0, |prev| self.ends[prev].get() as usize);
        // Both ends are char boundaries of `text`: [`FileTable::checked`].
        &self.text[lo..hi]
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
}

impl Index<usize> for Checked<'_> {
    type Output = str;

    fn index(&self, id: usize) -> &str {
        match self.get(id) {
            Some(path) => path,
            None => panic!("path {id} of {}", self.len()),
        }
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

/// A table of `paths` in one block of its own: the ends, then the text.
impl<S: AsRef<str>> FromIterator<S> for FileTable {
    fn from_iter<I: IntoIterator<Item = S>>(paths: I) -> Self {
        let (mut ends, mut text) = (Vec::new(), Vec::new());
        for path in paths {
            text.extend_from_slice(path.as_ref().as_bytes());
            ends.extend_from_slice(U64::new(text.len() as u64).as_bytes());
        }
        let (len, text_at) = (ends.len() / 8, ends.len());
        ends.extend_from_slice(&text);
        Self::from_parts(Store::new(ends), 0, text_at, len, Path::new(""))
    }
}

impl fmt::Debug for FileTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let paths =
            (0..self.len).map(|id| String::from_utf8_lossy(self.raw(id).unwrap_or_default()));
        f.debug_list().entries(paths).finish()
    }
}

/// A JSON list of strings, as a `Vec<String>` would be; a path that is not
/// UTF-8 fails the encoding.
impl Serialize for FileTable {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let checked = self.checked().map_err(serde::ser::Error::custom)?;
        s.collect_seq(checked.iter())
    }
}

/// The table of `text` cut at ascending `ends`, as a manifest at
/// `idx/manifest.bin` whose path text is damaged would give it.
#[cfg(test)]
pub(crate) fn damaged(ends: &[u64], text: &[u8]) -> FileTable {
    let mut bytes: Vec<u8> = ends.iter().flat_map(|e| e.to_le_bytes()).collect();
    let text_at = bytes.len();
    bytes.extend_from_slice(text);
    let at = Path::new("idx/manifest.bin");
    FileTable::from_parts(Store::new(bytes), 0, text_at, ends.len(), at)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(paths: &[&str]) -> FileTable {
        paths.iter().collect()
    }

    use super::damaged as raw_table;

    #[test]
    fn paths_read_back_by_id() {
        let t = table(&["a/one.rs", "b", "é/ü.txt", "中/文", "😀"]);
        let c = t.checked().unwrap();
        assert_eq!((t.len(), c.len()), (5, 5));
        assert_eq!(c.get(0), Some("a/one.rs"));
        assert_eq!(c.get(2), Some("é/ü.txt"));
        assert_eq!(&c[3], "中/文");
        assert_eq!(&c[4], "😀");
        assert_eq!(c.get(5), None);
        assert_eq!(
            c.iter().collect::<Vec<_>>(),
            ["a/one.rs", "b", "é/ü.txt", "中/文", "😀"]
        );
        assert_eq!(c.iter().len(), 5);
        assert_eq!(c.iter().next_back(), Some("😀"));
        // One path read alone gives what the whole check gives.
        for id in 0..t.len() {
            assert_eq!(t.path(id), Ok(c.get(id)));
        }
        assert_eq!(t.path(5), Ok(None));
    }

    #[test]
    fn an_empty_table_holds_no_path() {
        let t = FileTable::default();
        let c = t.checked().unwrap();
        assert_eq!((t.len(), c.len()), (0, 0));
        assert_eq!(t.path(0), Ok(None));
        assert_eq!(c.get(0), None);
        assert_eq!(c.iter().count(), 0);
        assert_eq!(c.binary_search("a"), Err(0));
        assert!(t.is_sorted() && c.is_sorted());
        assert_eq!(t, table(&[]));
        // An empty path is a path.
        let t = table(&[""]);
        assert_eq!((t.len(), t.path(0)), (1, Ok(Some(""))));
    }

    #[test]
    fn searches_match_the_slice_ones() {
        let paths = ["a", "a/b", "a/c", "b", "b/d", "c"];
        let t = table(&paths);
        let c = t.checked().unwrap();
        for key in ["", "a", "a/", "a/b", "a/bb", "b", "bz", "c", "d"] {
            assert_eq!(c.binary_search(key), paths.binary_search(&key), "{key}");
        }
        for range in [0..6, 1..4, 3..6, 2..2] {
            for key in ["a", "a/c", "b", "z"] {
                let want = range.start + paths[range.clone()].partition_point(|p| *p < key);
                assert_eq!(c.partition_point(range.clone(), |p| p < key), want);
                assert_eq!(t.partition_point(range.clone(), |p| p < key), Ok(want));
            }
        }
        assert!(t.is_sorted() && c.is_sorted());
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

    const NOT_UTF8: &str = "error: idx/manifest.bin holds a path that is not UTF-8; re-run `index`";

    /// A path that is not UTF-8 is refused when it is read, with the
    /// manifest's name; the others read back.
    #[test]
    fn a_damaged_path_is_refused_when_read() {
        // "a.c", "b\xFF", "d": one byte that is not UTF-8.
        let t = raw_table(&[3, 5, 6], b"a.cb\xFFd");
        assert_eq!(t.path(0), Ok(Some("a.c")));
        assert_eq!(t.path(1), Err(NOT_UTF8.to_owned()));
        assert_eq!(t.path(2), Ok(Some("d")));
        assert_eq!(t.checked().err(), Some(NOT_UTF8.to_owned()));
        assert!(
            serde_json::to_string(&t)
                .unwrap_err()
                .to_string()
                .contains(NOT_UTF8)
        );
        // A binary search that reads it stops there.
        assert_eq!(
            t.partition_point(0..3, |p| p < "c"),
            Err(NOT_UTF8.to_owned())
        );
        assert_eq!(t.partition_point(2..3, |p| p < "z"), Ok(3));
        // The order is the bytes', and needs no check.
        assert!(t.is_sorted());
    }

    /// An end inside a character: the pieces on both sides are not UTF-8,
    /// while the text is.
    #[test]
    fn an_end_inside_a_character_is_refused_when_read() {
        // "é" is c3 a9: an end of 1 cuts it.
        let t = raw_table(&[1, 2, 3], "éb".as_bytes());
        assert_eq!(t.path(0), Err(NOT_UTF8.to_owned()));
        assert_eq!(t.path(1), Err(NOT_UTF8.to_owned()));
        assert_eq!(t.path(2), Ok(Some("b")));
        assert_eq!(
            t.checked().err().as_deref(),
            Some("error: idx/manifest.bin cuts path 0 inside a character; re-run `index`")
        );
        // The last path cut inside a character: the text is not UTF-8.
        let t = raw_table(&[1, 2], b"a\xC3");
        assert_eq!(t.path(0), Ok(Some("a")));
        assert_eq!(t.path(1), Err(NOT_UTF8.to_owned()));
        assert_eq!(t.checked().err(), Some(NOT_UTF8.to_owned()));
    }

    /// The whole check and the check of each path agree on every id, on
    /// paths of 1 to 4 bytes per character.
    #[test]
    fn the_two_checks_agree() {
        let paths: Vec<String> = (0..200)
            .map(|i| match i % 4 {
                0 => format!("src/m{i}.rs"),
                1 => format!("é/ü{i}.txt"),
                2 => format!("中文/{i}.c"),
                _ => format!("😀/{i}"),
            })
            .collect();
        let t: FileTable = paths.iter().collect();
        let c = t.checked().unwrap();
        for (id, want) in paths.iter().enumerate() {
            assert_eq!(t.path(id), Ok(Some(want.as_str())));
            assert_eq!(c.get(id), Some(want.as_str()));
        }
        assert!(c.iter().eq(paths.iter().map(String::as_str)));
    }
}
