//! RFC 4648 standard base64, padded: rg's `bytes` encoding.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

const DECODE: [u8; 256] = {
    let mut t = [u8::MAX; 256];
    let mut i = 0;
    while i < 64 {
        t[ALPHABET[i] as usize] = i as u8;
        i += 1;
    }
    t
};

/// Strict inverse of [`encode`]: padded, no whitespace, zero trailing bits.
pub fn decode(text: &str) -> Result<Vec<u8>, String> {
    let s = text.as_bytes();
    if !s.len().is_multiple_of(4) {
        return Err(format!("base64 length {} is not a multiple of 4", s.len()));
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    for (i, quad) in s.chunks(4).enumerate() {
        let last = i + 1 == s.len() / 4;
        let pad = quad.iter().rev().take_while(|&&c| c == b'=').count();
        if pad > 2 || (pad > 0 && !last) {
            return Err("misplaced base64 padding".to_owned());
        }
        let mut n = 0u32;
        for &c in &quad[..4 - pad] {
            let v = DECODE[c as usize];
            if v == u8::MAX {
                return Err(format!("invalid base64 byte {c:#04x}"));
            }
            n = (n << 6) | u32::from(v);
        }
        n <<= 6 * pad as u32;
        if n & ((1 << (8 * pad)) - 1) != 0 {
            return Err("nonzero base64 trailing bits".to_owned());
        }
        out.extend_from_slice(&n.to_be_bytes()[1..4 - pad]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
