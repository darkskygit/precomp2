//! Byte-exact pure-Rust ports of the liblz4 compressors UnityFS bundles use.
//!
//! Only decompression is standardized by the LZ4 block format, so each
//! [`Compressor`] must reproduce one liblz4 release class at one level byte
//! for byte; decompression uses [`lz4_flex`]. Releases with identical output
//! form a class, named after the release the tests compare against.
//!
//! Adding a compressor: a new variant with a new id, its level, and a
//! `compress` arm combining a parser family (`hashchain` or `optimal`)
//! with that family's version quirks and level parameters.

mod common;
mod finder;
mod hashchain;
mod optimal;
mod writer;

use common::{LAST_LITERALS, LZ4_MAX_INPUT_SIZE, MF_LIMIT, MIN_MATCH};
use finder::Quirks;

use crate::error::Error;

/// `LZ4_compressBound`.
pub fn compress_bound(len: usize) -> usize {
  if len > LZ4_MAX_INPUT_SIZE { 0 } else { len + len / 255 + 16 }
}

/// A liblz4 release class and compression level whose `LZ4_compress_HC`
/// output this module reproduces.
///
/// The discriminant is written into PCF2 segment meta and must never change
/// meaning; retired ids are never reused (id 1 was liblz4 1.7.5 level 10).
#[allow(non_camel_case_types)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Compressor {
  /// liblz4 1.9.2–1.10.0, level 12 (Unity 2022.3, 6000): optimal parser.
  V1_10_0_L12 = 3,
  /// liblz4 1.8.2–1.9.1, level 12 (Unity 2021): optimal parser.
  V1_9_1_L12 = 2,
  /// liblz4 r131 (1.7.1), level 16 (Unity 2017): lazy hash-chain parser.
  R131_L16 = 4,
}

impl TryFrom<u8> for Compressor {
  type Error = Error;

  fn try_from(id: u8) -> Result<Self, Error> {
    Self::ALL
      .into_iter()
      .find(|c| *c as u8 == id)
      .ok_or(Error::InvalidSegment("unknown lz4 compressor id"))
  }
}

impl Compressor {
  /// Newest release first.
  pub const ALL: [Compressor; 3] = [Self::V1_10_0_L12, Self::V1_9_1_L12, Self::R131_L16];

  /// The liblz4 release the class is named and tested after.
  pub fn release(self) -> &'static str {
    match self {
      Self::V1_10_0_L12 => "1.10.0",
      Self::V1_9_1_L12 => "1.9.1",
      Self::R131_L16 => "r131",
    }
  }

  /// The `compressionLevel` passed to `LZ4_compress_HC`.
  pub fn level(self) -> i32 {
    match self {
      Self::V1_10_0_L12 | Self::V1_9_1_L12 => 12,
      Self::R131_L16 => 16,
    }
  }

  /// Compress `src` exactly like the corresponding `LZ4_compress_HC` call
  /// with an output buffer of `dst_capacity` bytes. Returns `None` where the
  /// C function returns 0 (does not fit / input too large).
  pub fn compress(self, src: &[u8], dst_capacity: usize) -> Option<Vec<u8>> {
    if src.len() > LZ4_MAX_INPUT_SIZE {
      return None;
    }
    // `LZ4_compress_HC` only checks the output size below the worst-case bound.
    let limit = (dst_capacity < compress_bound(src.len())).then_some(dst_capacity);
    match self {
      Self::V1_10_0_L12 => optimal::compress(src, limit, Quirks::V1_10_0, optimal::LEVEL_12),
      Self::V1_9_1_L12 => optimal::compress(src, limit, Quirks::V1_9_1, optimal::LEVEL_12),
      // r131 levels: `1 << (level - 1)` match attempts.
      Self::R131_L16 => hashchain::compress(src, limit, 1 << 15),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn compressor_ids_are_stable() {
    for (id, c) in [(2, Compressor::V1_9_1_L12), (3, Compressor::V1_10_0_L12), (4, Compressor::R131_L16)] {
      assert_eq!(Compressor::try_from(id).ok(), Some(c));
      assert_eq!(c as u8, id);
    }
    for retired_or_unknown in [0, 1, 5] {
      assert!(Compressor::try_from(retired_or_unknown).is_err());
    }
  }
}
