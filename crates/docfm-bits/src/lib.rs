//! Succinct structures over plain bytes, each laid out in sections a blob
//! can hold and read in place: [`wavelet`] levels of compressed digits,
//! Elias-Fano [`marks`] and [`packed`] integers. They know nothing of
//! documents, samples or a blob's header.
//!
//! Cross-crate callers reach the hot paths through `#[inline]` functions:
//! the workspace does not use LTO.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod marks;
pub mod packed;
mod par;
#[cfg(test)]
mod test_util;
pub mod wavelet;
mod word;
