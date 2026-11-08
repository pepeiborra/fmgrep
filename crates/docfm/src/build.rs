use libsais::BwtConstruction;
use libsais::bwt::AuxIndicesSamplingRate;

use rayon::slice::ParallelSliceMut;

use crate::layout::{Config, Gaps, HEADER_BYTES, Header, Levels};
use crate::{DOC_END, Error, MAX_TEXT_BYTES, View, marks, wavelet};

/// Text positions per aux row: a sample costs fewer LF steps to place.
const AUX_RATE: usize = 4;

/// Bytes per parallel unit of work.
const SUPER: usize = 65536;

/// `f` of every item, one job per item on the current rayon pool; a lone
/// item runs inline. `f` runs no parallel work, so no thread ever waits
/// inside a job.
pub fn each<I: Send>(items: impl IntoIterator<Item = I>, f: impl Fn(I) + Sync) {
    let mut items = items.into_iter().peekable();
    let Some(first) = items.next() else { return };
    if items.peek().is_none() {
        return f(first);
    }
    let f = &f;
    rayon::in_place_scope(|scope| {
        for item in std::iter::once(first).chain(items) {
            scope.spawn(move |_| f(item));
        }
    });
}

pub fn check_len(len: usize) -> Result<(), Error> {
    if len as u64 > MAX_TEXT_BYTES {
        return Err(Error::TextTooLong(len));
    }
    Ok(())
}

/// What the encoder needs of a text besides its BWT.
#[derive(Clone, Copy, Debug)]
pub struct Text {
    /// The sentinel's row among the `n + 1` rows.
    pub sentinel_row: usize,
    /// The bytes the text uses; codes are their ranks.
    pub alphabet: [u64; 4],
    /// First row per code, `c[sigma] = n + 1`.
    pub c: [u32; 257],
    pub num_docs: usize,
}

/// The blob of `bwt` (the sentinel-free BWT over codes, all below the
/// alphabet's size) with its wavelet levels written, sized for
/// `num_samples` samples; [`encode_samples`] completes it. The levels are
/// encoded on the current rayon pool with one scratch copy of the BWT (two
/// past four levels).
pub fn encode_bwt(
    bwt: &[u8],
    text: &Text,
    num_samples: usize,
    config: Config,
) -> (Vec<u8>, Header) {
    let Config { gaps, d, lb } = config;
    let n = bwt.len();
    let sigma = text.alphabet.iter().map(|w| w.count_ones() as usize).sum();
    let (levels, digit_starts, bottom) = wavelet::encode(bwt, sigma, d, lb);
    let header = Header::new(
        n,
        text.sentinel_row,
        gaps,
        text.alphabet,
        text.c,
        (text.num_docs, num_samples),
        &Levels {
            lb,
            d,
            starts: digit_starts,
            payload: wavelet::payloads(&levels, n, d, lb),
            bottom,
        },
    );
    let mut out = vec![0u8; header.blob_len];
    wavelet::write(&levels, &header.wavelet, &mut out);
    (out, header)
}

/// Completes a blob [`encode_bwt`] began: its marks, its doc ids and its
/// header. `samples` holds `row << 32 | doc` per sampled row, ascending, as
/// many as `encode_bwt` was told.
pub fn encode_samples(out: &mut [u8], header: &Header, samples: &[u64]) {
    debug_assert_eq!(samples.len(), header.num_samples);
    let rows = header.n + 1;
    let mut bits = vec![0u64; rows.div_ceil(64)];
    for &x in samples {
        let row = (x >> 32) as usize;
        bits[row / 64] |= 1 << (row % 64);
    }
    let marked = marks::write(&mut out[header.marks_at..header.samples_at], rows, &bits);
    debug_assert_eq!(marked, header.num_samples);
    marks::pack(
        &mut out[header.samples_at..],
        samples.iter().map(|&x| u64::from(x as u32)),
        header.sample_bits,
    );
    header.write(&mut out[..HEADER_BYTES]);
}

/// The blob of `text`: libsais runs on one thread, the other phases in
/// `SUPER`-byte units on the current rayon pool. Peak live heap: the text
/// (replaced by the BWT), libsais's `i32` temporary array, the aux rows and
/// the sampled positions; the wavelet levels take a scratch copy of the BWT
/// once libsais is done. `gaps` passes [`Gaps::check`].
pub fn build(mut text: Vec<u8>, config: Config) -> Result<Vec<u8>, Error> {
    let gaps = config.gaps;
    check_len(text.len())?;
    if text.last().is_some_and(|&b| b != DOC_END) {
        return Err(Error::Unterminated);
    }
    let n = text.len();
    let mut counts = [0usize; 256];
    let mut starts = if n == 0 { vec![] } else { vec![0u32] };
    for (p, &b) in text.iter().enumerate() {
        counts[usize::from(b)] += 1;
        if b == DOC_END && p + 1 < n {
            starts.push(p as u32 + 1);
        }
    }
    let mut code_of = [0u8; 256];
    let mut alphabet = [0u64; 4];
    let mut c = [0u32; 257];
    let mut sigma = 0;
    let mut row = 1;
    for (byte, &count) in counts.iter().enumerate() {
        if count > 0 {
            code_of[byte] = sigma as u8;
            alphabet[byte / 64] |= 1 << (byte % 64);
            c[sigma] = row as u32;
            sigma += 1;
            row += count;
        }
    }
    c[sigma] = row as u32;
    let positions = text_samples(&text, &starts, gaps);
    // Order-preserving, so the suffix order is the original text's.
    each(text.chunks_mut(SUPER), |chunk| {
        for b in chunk {
            *b = code_of[usize::from(*b)];
        }
    });
    // aux[k] is the row of text position k * AUX_RATE; aux[0] is the sentinel row.
    let aux = if n == 0 {
        Vec::new()
    } else {
        bwt_in_place(&mut text, AUX_RATE)?
    };
    let sentinel_row = aux.first().map_or(0, |&row| row as usize);
    let meta = Text {
        sentinel_row,
        alphabet,
        c,
        num_docs: starts.len(),
    };
    let (mut out, header) = encode_bwt(&text, &meta, positions.len(), config);
    drop(text);

    let view = View {
        bytes: &out,
        header: &header,
    };
    // Walks from the aux row at or after `p`, or from row 0 (position `n`).
    let row_of = |p: usize| {
        let q = p.next_multiple_of(AUX_RATE);
        let mut row = aux.get(q / AUX_RATE).map_or(0, |&row| row as usize);
        for _ in p..q.min(n) {
            row = view.lf(row);
        }
        row as u64
    };
    // `row << 32 | doc` for each sampled position of unit `i`.
    let sampled = |i: usize| {
        let span = i * SUPER..(i * SUPER + SUPER).min(n);
        let first = starts.partition_point(|&s| s as usize <= span.start) - 1;
        let from = positions.partition_point(|&p| (p as usize) < span.start);
        let to = positions.partition_point(|&p| (p as usize) < span.end);
        let mut doc = first;
        let mut found = Vec::with_capacity(to - from);
        for &p in &positions[from..to] {
            while starts.get(doc + 1).is_some_and(|&s| s <= p) {
                doc += 1;
            }
            found.push(row_of(p as usize) << 32 | doc as u64);
        }
        found
    };
    let mut units = vec![Vec::new(); n.div_ceil(SUPER)];
    each(units.iter_mut().enumerate(), |(i, unit)| *unit = sampled(i));
    let mut found = units.concat();
    drop(aux);
    drop(positions);
    found.par_sort_unstable();
    debug_assert_eq!(found.len(), header.num_samples);
    encode_samples(&mut out, &header, &found);
    Ok(out)
}

/// The sorted sampled positions of `text`, documents grouped per `SUPER` bytes.
fn text_samples(text: &[u8], starts: &[u32], gaps: Gaps) -> Vec<u32> {
    let mut groups = Vec::new();
    let mut from = 0;
    for (k, &s) in starts.iter().enumerate().skip(1) {
        if s as usize - starts[from] as usize >= SUPER {
            groups.push(from..k);
            from = k;
        }
    }
    if !starts.is_empty() {
        groups.push(from..starts.len());
    }
    let mut out = vec![Vec::new(); groups.len()];
    each(out.iter_mut().zip(groups), |(found, docs)| {
        let end = starts.get(docs.end).map_or(text.len(), |&e| e as usize);
        let lo = starts[docs.start] as usize;
        found.reserve((end - lo) / (gaps.min as usize).max(4));
        for doc in docs {
            let s = starts[doc] as usize;
            let e = starts.get(doc + 1).map_or(text.len(), |&e| e as usize);
            doc_samples(&text[s..e], s as u32, gaps, found);
        }
    });
    out.concat()
}

/// Whether `b` after `prev` starts a word or subword: after a non-word
/// byte or `_`, or lower to upper.
pub fn word_start(prev: u8, b: u8) -> bool {
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    (word(b) && (!word(prev) || prev == b'_' && b != b'_'))
        || (prev.is_ascii_lowercase() && b.is_ascii_uppercase())
}

/// Greedy over one document: its start; a word, subword or line start `min`
/// past the previous sample; any byte `max` past it, `ws` on whitespace.
fn doc_samples(doc: &[u8], at: u32, gaps: Gaps, found: &mut Vec<u32>) {
    let [min, max, ws] = [gaps.min, gaps.max, gaps.ws].map(|g| g as usize);
    found.push(at);
    let (mut last, mut prev) = (0, doc[0]);
    // Only blanks or '\r' since the line's start.
    let mut line_start = matches!(prev, b' ' | b'\t' | b'\r' | b'\n');
    for (p, &b) in doc.iter().enumerate().skip(1) {
        let start = word_start(prev, b) || (line_start && !b.is_ascii_whitespace());
        let force = if b.is_ascii_whitespace() { ws } else { max };
        if p - last >= force || (start && p - last >= min) {
            found.push(at + p as u32);
            last = p;
        }
        line_start = match b {
            b'\n' => true,
            b' ' | b'\t' | b'\r' => line_start,
            _ => false,
        };
        prev = b;
    }
}

/// Replaces the non-empty `text` by its sentinel-free BWT; returns the rows
/// of every `r`-th text position.
pub fn bwt_in_place(text: &mut [u8], r: usize) -> Result<Vec<i32>, Error> {
    let n = text.len();
    let rate = AuxIndicesSamplingRate::from(r as i32);
    let (_, aux, _) = BwtConstruction::replace_text(text)
        .with_owned_temporary_array_buffer32()
        .single_threaded()
        .with_aux_indices(rate)
        .run()
        .map_err(|e| Error::Construction(format!("libsais failed on {n} bytes: {e}")))?
        .into_parts();
    Ok(aux)
}
