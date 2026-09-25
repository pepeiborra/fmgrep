use libsais::BwtConstruction;
use libsais::bwt::AuxIndicesSamplingRate;

use crate::layout::{HEADER_BYTES, Header};
use crate::occ::write_occ;
use crate::{DOC_END, Error, MAX_TEXT_BYTES, View, marks, par};

/// Texts below this build on one thread.
const PAR_MIN_BYTES: usize = 1 << 16;

/// Bounds the LF steps from an aux row to a sampled position.
const MAX_AUX_RATE: usize = 64;

pub fn check_len(len: usize) -> Result<(), Error> {
    if len as u64 > MAX_TEXT_BYTES {
        return Err(Error::TextTooLong(len));
    }
    Ok(())
}

/// The blob of `text`, independent of `threads`: libsais runs on one thread,
/// the other phases on up to `threads`. Peak live heap: the text (replaced
/// by the BWT), libsais's `i32` temporary array and the aux rows. Each
/// document start costs fewer than the aux rate in LF steps.
pub fn build(mut text: Vec<u8>, sample_rate: u32, threads: usize) -> Result<Vec<u8>, Error> {
    check_len(text.len())?;
    if sample_rate == 0 {
        return Err(Error::ZeroSampleRate);
    }
    if text.last().is_some_and(|&b| b != DOC_END) {
        return Err(Error::Unterminated);
    }
    let n = text.len();
    let threads = if n < PAR_MIN_BYTES { 1 } else { threads.max(1) };
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
    let per = par::chunk_len(n, threads);
    // Order-preserving, so the suffix order is the original text's.
    par::map(text.chunks_mut(per).collect(), |chunk| {
        for b in chunk {
            *b = code_of[usize::from(*b)];
        }
    });
    let rate = sample_rate as usize;
    let r = 1 << (rate.min(MAX_AUX_RATE).ilog2().max(1));
    // aux[k] is the row of text position k * r; aux[0] is the sentinel row.
    let aux = if n == 0 {
        Vec::new()
    } else {
        bwt_in_place(&mut text, r)?
    };
    let sentinel_row = aux.first().map_or(0, |&row| row as usize);

    let off_rate_starts = starts
        .iter()
        .filter(|&&s| !(s as usize).is_multiple_of(rate))
        .count();
    let num_samples = n.div_ceil(rate) + off_rate_starts;
    let header = Header::new(
        n,
        sentinel_row,
        sample_rate,
        alphabet,
        c,
        starts.len(),
        num_samples,
    );
    let mut out = vec![0u8; header.blob_len];
    write_occ(&text, &header, &mut out, threads);
    drop(text);

    let view = View {
        bytes: &out,
        header: &header,
    };
    // Walks from the aux row at or after `p`, or from row 0 (position `n`).
    let row_of = |p: usize| {
        let q = p.next_multiple_of(r);
        let mut row = aux.get(q / r).map_or(0, |&row| row as usize);
        for _ in p..q.min(n) {
            row = view.lf(row);
        }
        row as u64
    };
    let spans: Vec<_> = (0..n)
        .step_by(per)
        .map(|lo| lo..(lo + per).min(n))
        .collect();
    // `row << 32 | doc` for each sampled position.
    let found = par::map(spans, |span| {
        let first = starts.partition_point(|&s| s as usize <= span.start) - 1;
        let mut found = Vec::with_capacity(span.len() / rate + 1);
        for (doc, &s) in starts.iter().enumerate().skip(first) {
            let s = s as usize;
            if s >= span.end {
                break;
            }
            let end = starts.get(doc + 1).map_or(n, |&e| e as usize).min(span.end);
            let lo = s.max(span.start);
            if lo == s && !s.is_multiple_of(rate) {
                found.push(row_of(s) << 32 | doc as u64);
            }
            found.extend(
                (lo.next_multiple_of(rate)..end)
                    .step_by(rate)
                    .map(|p| row_of(p) << 32 | doc as u64),
            );
        }
        found
    });
    drop(aux);
    let mut found: Vec<u64> = found.into_iter().flatten().collect();
    found.sort_unstable();
    debug_assert_eq!(found.len(), num_samples);

    let mut bits = vec![0u64; (n + 1).div_ceil(64)];
    for &x in &found {
        let row = (x >> 32) as usize;
        bits[row / 64] |= 1 << (row % 64);
    }
    let marked = marks::write(&mut out[header.marks_at..header.samples_at], n + 1, &bits);
    debug_assert_eq!(marked, num_samples);
    for (dst, x) in out[header.samples_at..]
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(&found)
    {
        dst.copy_from_slice(&(*x as u32).to_le_bytes());
    }
    header.write(&mut out[..HEADER_BYTES]);
    Ok(out)
}

/// Replaces the non-empty `text` by its sentinel-free BWT; returns the rows
/// of every `r`-th text position.
fn bwt_in_place(text: &mut [u8], r: usize) -> Result<Vec<i32>, Error> {
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
