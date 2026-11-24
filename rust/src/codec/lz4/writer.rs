//! LZ4 block sequence encoding with liblz4's (conservative) output-limit
//! checks. Shared by both parser families.

use super::{LAST_LITERALS, MIN_MATCH, compress_bound};

const RUN_MASK: usize = 15;
const ML_MASK: usize = 15;

/// How the output-limit check estimates the extra length bytes of a run.
#[derive(Clone, Copy)]
pub(super) enum LengthEstimate {
  /// `len >> 8` (r131 hash chain).
  Shift8,
  /// `len / 255` (optimal parser classes).
  Div255,
}

/// The output would exceed `dstCapacity` (C returns 0).
#[derive(Debug)]
pub(super) struct Overflow;

pub(super) struct SequenceWriter<'a> {
  src: &'a [u8],
  out: Vec<u8>,
  /// Start of the pending literals.
  anchor: usize,
  /// `Some(dstCapacity)` in limited-output mode (only below the worst-case
  /// bound).
  limit: Option<usize>,
  estimate: LengthEstimate,
}

impl<'a> SequenceWriter<'a> {
  pub fn new(src: &'a [u8], limit: Option<usize>, estimate: LengthEstimate) -> Self {
    SequenceWriter {
      src,
      out: Vec::with_capacity(compress_bound(src.len())),
      anchor: 0,
      limit,
      estimate,
    }
  }

  pub fn anchor(&self) -> usize {
    self.anchor
  }

  fn estimate(&self, len: usize) -> usize {
    match self.estimate {
      LengthEstimate::Shift8 => len >> 8,
      LengthEstimate::Div255 => len / 255,
    }
  }

  fn exceeds(&self, needed: usize) -> bool {
    self.limit.is_some_and(|cap| needed > cap)
  }

  fn push_length(&mut self, mut rest: usize) {
    while rest >= 255 {
      self.out.push(255);
      rest -= 255;
    }
    self.out.push(rest as u8);
  }

  /// Literals `[anchor, start)` followed by a match of `len` bytes copied from
  /// `start - offset`.
  pub fn sequence(&mut self, start: usize, offset: usize, len: usize) -> Result<(), Overflow> {
    let literals = start - self.anchor;
    let token = self.out.len();
    self.out.push(0);
    if self.exceeds(self.out.len() + self.estimate(literals) + literals + 2 + 1 + LAST_LITERALS) {
      return Err(Overflow);
    }
    if literals >= RUN_MASK {
      self.out[token] = (RUN_MASK << 4) as u8;
      self.push_length(literals - RUN_MASK);
    } else {
      self.out[token] = (literals << 4) as u8;
    }
    self.out.extend_from_slice(&self.src[self.anchor..start]);
    self.out.extend_from_slice(&(offset as u16).to_le_bytes());
    let ml_code = len - MIN_MATCH;
    if self.exceeds(self.out.len() + self.estimate(ml_code) + 1 + LAST_LITERALS) {
      return Err(Overflow);
    }
    if ml_code >= ML_MASK {
      self.out[token] |= ML_MASK as u8;
      self.push_length(ml_code - ML_MASK);
    } else {
      self.out[token] |= ml_code as u8;
    }
    self.anchor = start + len;
    Ok(())
  }

  /// Emit the trailing literals and return the block.
  pub fn finish(mut self) -> Result<Vec<u8>, Overflow> {
    let literals = self.src.len() - self.anchor;
    let extra = (literals + 255 - RUN_MASK) / 255;
    if self.exceeds(self.out.len() + 1 + extra + literals) {
      return Err(Overflow);
    }
    if literals >= RUN_MASK {
      self.out.push((RUN_MASK << 4) as u8);
      self.push_length(literals - RUN_MASK);
    } else {
      self.out.push((literals << 4) as u8);
    }
    self.out.extend_from_slice(&self.src[self.anchor..]);
    Ok(self.out)
  }
}
