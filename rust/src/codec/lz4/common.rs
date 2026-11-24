//! Low-level helpers shared by both parser families.

pub(super) const MIN_MATCH: usize = 4;
/// The last match must start at least this far before the end.
pub(super) const MF_LIMIT: usize = 12;
/// The last literals run is at least this long.
pub(super) const LAST_LITERALS: usize = 5;
pub(super) const LZ4_MAX_INPUT_SIZE: usize = 0x7E00_0000;

#[inline(always)]
pub(super) fn read16(s: &[u8], p: usize) -> u16 {
  u16::from_le_bytes([s[p], s[p + 1]])
}

#[inline(always)]
pub(super) fn read32(s: &[u8], p: usize) -> u32 {
  u32::from_le_bytes(s[p..p + 4].try_into().unwrap())
}

#[inline(always)]
pub(super) fn read64(s: &[u8], p: usize) -> u64 {
  u64::from_le_bytes(s[p..p + 8].try_into().unwrap())
}

/// Knuth multiplicative hash, 15 bits.
#[inline(always)]
pub(super) fn hash(v: u32) -> usize {
  (v.wrapping_mul(2_654_435_761) >> (32 - 15)) as usize
}

#[inline(always)]
pub(super) fn hash4(src: &[u8], pos: usize) -> usize {
  hash(read32(src, pos))
}

/// Length of the common prefix of `src[a..limit]` and `src[b..]` (`b < a`,
/// ranges may overlap).
#[inline(always)]
pub(super) fn common_prefix(src: &[u8], a: usize, b: usize, limit: usize) -> usize {
  let mut n = 0;
  while a + n + 8 <= limit {
    let x = read64(src, a + n);
    let y = read64(src, b + n);
    if x != y {
      return n + ((x ^ y).trailing_zeros() / 8) as usize;
    }
    n += 8;
  }
  while a + n < limit && src[a + n] == src[b + n] {
    n += 1;
  }
  n
}

/// Run length of `byte` in `src[from..to]`.
#[inline(always)]
pub(super) fn run_forward(s: &[u8], from: usize, to: usize, byte: u8) -> usize {
  let word = u64::from_ne_bytes([byte; 8]);
  let mut p = from;
  while p + 8 <= to {
    let diff = read64(s, p) ^ word;
    if diff != 0 {
      return p - from + (diff.trailing_zeros() / 8) as usize;
    }
    p += 8;
  }
  p - from + s[p..to].iter().take_while(|&&c| c == byte).count()
}

/// Run length of `byte` ending at `s[..to]`, not before `low`.
#[inline(always)]
pub(super) fn run_backward(s: &[u8], to: usize, low: usize, byte: u8) -> usize {
  if to <= low {
    return 0;
  }
  s[low..to].iter().rev().take_while(|&&c| c == byte).count()
}

/// `len` bytes at `start` repeat the bytes at `source` (`source < start`).
#[derive(Clone, Copy, Debug)]
pub(super) struct Match {
  pub start: usize,
  pub source: usize,
  pub len: usize,
}

impl Match {
  pub fn end(&self) -> usize {
    self.start + self.len
  }

  pub fn offset(&self) -> usize {
    self.start - self.source
  }

  /// Drop the first `n` bytes of the match.
  pub fn skip(&mut self, n: usize) {
    self.start += n;
    self.source += n;
    self.len -= n;
  }
}
