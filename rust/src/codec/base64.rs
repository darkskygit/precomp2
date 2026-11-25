use ::base64::Engine as _;
use memchr::memmem;

use super::{
  BASE64_ENGINE, DecodeConfig, EncodeConfig,
  budget::{DecodeBudget, EncodeBudget},
  payload::{choose_payload, decode_payload},
  varint::{read_len, read_varint_slice, take_bytes, write_varint_vec},
};
use crate::{
  error::{Error, Result},
  pcf2::{Pcf2Segment, SegmentKind},
};

#[derive(Clone, Debug)]
pub(super) struct Base64Block {
  pub(super) header: Vec<u8>,
  pub(super) line_case: u8,
  pub(super) line_lengths: Vec<usize>,
  pub(super) decoded: Vec<u8>,
  pub(super) decoded_len: usize,
}

pub(super) fn parse_base64_block(input: &[u8]) -> Option<Base64Block> {
  let mut header = Vec::new();
  let mut cursor = 0usize;
  if let Some(pos) = memmem::find(input, b"\r\n\r\n") {
    header.extend_from_slice(&input[..pos + 4]);
    cursor = pos + 4;
  }

  let mut line_lengths = Vec::new();
  let mut encoded = Vec::new();
  while cursor < input.len() {
    if input[cursor] == b'-' {
      break;
    }
    let line_end = cursor + memmem::find(&input[cursor..], b"\r\n")?;
    let line = &input[cursor..line_end];
    if line.is_empty() {
      return None;
    }
    if line.len() > 255 {
      return None;
    }
    line_lengths.push(line.len());
    encoded.extend_from_slice(line);
    cursor = line_end + 2;
  }
  if line_lengths.is_empty() || cursor != input.len() {
    return None;
  }
  let decoded = BASE64_ENGINE.decode(encoded).ok()?;
  let decoded_len = decoded.len();
  let line_case = if line_lengths.iter().all(|&l| l == line_lengths[0]) {
    0
  } else if line_lengths.len() > 1 && line_lengths[..line_lengths.len() - 1].iter().all(|&l| l == line_lengths[0]) {
    1
  } else {
    2
  };

  Some(Base64Block {
    header,
    line_case,
    line_lengths,
    decoded,
    decoded_len,
  })
}

pub(super) fn encode_base64(input: &[u8], config: &EncodeConfig, depth: u32, budget: &mut EncodeBudget) -> Result<Option<Pcf2Segment>> {
  let Some(block) = parse_base64_block(input) else {
    return Ok(None);
  };
  if (block.decoded.len() as u64) > budget.remaining(config) {
    return Ok(None);
  }
  let (payload_kind, payload) = choose_payload(block.decoded, input.len(), config, depth, budget)?;

  let mut meta = Vec::new();
  meta.push(0);
  meta.push(payload_kind);
  write_varint_vec(block.header.len() as u64, &mut meta);
  meta.extend_from_slice(&block.header);
  meta.push(block.line_case);
  write_varint_vec(block.line_lengths.len() as u64, &mut meta);
  match block.line_case {
    0 => meta.push(block.line_lengths[0] as u8),
    1 => {
      meta.push(block.line_lengths[0] as u8);
      meta.push(*block.line_lengths.last().unwrap() as u8);
    }
    2 => {
      for len in &block.line_lengths {
        meta.push(*len as u8);
      }
    }
    _ => return Ok(None),
  }
  write_varint_vec(block.decoded_len as u64, &mut meta);

  Ok(Some(Pcf2Segment {
    kind: SegmentKind::Base64 as u8,
    flags: 0,
    orig_len: input.len() as u64,
    meta,
    data: payload,
  }))
}

/// Line lengths of a base64 segment as stored in meta: `repeat_count` lines
/// of `repeated` bytes followed by one line per byte of `tail`.
struct LineLengths<'a> {
  repeated: usize,
  repeat_count: u64,
  tail: &'a [u8],
}

impl LineLengths<'_> {
  fn iter(&self) -> impl Iterator<Item = usize> + '_ {
    // Callers take at most `written` lines, which fits in `usize`, so a
    // repeat count past `usize::MAX` never reaches the tail.
    let repeat_count = usize::try_from(self.repeat_count).unwrap_or(usize::MAX);
    std::iter::repeat_n(self.repeated, repeat_count).chain(self.tail.iter().map(|&len| usize::from(len)))
  }

  /// Number of lines written for `encoded_len` bytes of text, or `None` when
  /// the lines do not reach its end. Lines are written while text remains:
  /// the last one is clipped to the text, later ones are dropped, and empty
  /// lines before the end are written as a bare CRLF. Computed without
  /// walking the repeated lines.
  fn written(&self, encoded_len: u64) -> Option<u64> {
    if encoded_len == 0 {
      return Some(0);
    }
    let (repeated, repeat_count) = (self.repeated as u64, self.repeat_count);
    let mut cursor = 0u64;
    let mut written = repeat_count;
    if repeated > 0 {
      let needed = encoded_len.div_ceil(repeated);
      if needed <= repeat_count {
        return Some(needed);
      }
      cursor = repeated * repeat_count;
    }
    for &len in self.tail {
      if cursor >= encoded_len {
        break;
      }
      written += 1;
      cursor += u64::from(len).min(encoded_len - cursor);
    }
    (cursor == encoded_len).then_some(written)
  }
}

pub(super) fn decode_base64_segment(
  segment: &Pcf2Segment,
  config: &DecodeConfig,
  depth: u32,
  budget: &mut DecodeBudget,
) -> Result<Vec<u8>> {
  let meta = segment.meta.as_slice();
  let &[meta_version, payload_kind, ..] = meta else {
    return Err(Error::InvalidSegment("base64 meta"));
  };
  if meta_version != 0 {
    return Err(Error::InvalidSegment("base64 meta_version"));
  }
  let mut offset = 2usize;
  let header_len = read_len(meta, &mut offset)?;
  let header = take_bytes(meta, &mut offset, header_len, "base64 header bounds")?;
  let line_case = take_bytes(meta, &mut offset, 1, "base64 line_case")?[0];
  let line_count = read_varint_slice(meta, &mut offset)?;
  if line_count == 0 {
    return Err(Error::InvalidSegment("base64 line_count"));
  }
  let lines = match line_case {
    0 => LineLengths {
      repeated: usize::from(take_bytes(meta, &mut offset, 1, "base64 line_len_0")?[0]),
      repeat_count: line_count,
      tail: &[],
    },
    1 => {
      let pair = take_bytes(meta, &mut offset, 2, "base64 line_len_1")?;
      LineLengths {
        repeated: usize::from(pair[0]),
        repeat_count: line_count - 1,
        tail: &pair[1..],
      }
    }
    2 => LineLengths {
      repeated: 0,
      repeat_count: 0,
      tail: take_bytes(
        meta,
        &mut offset,
        usize::try_from(line_count).map_err(|_| Error::LengthOverflow)?,
        "base64 line_len_list",
      )?,
    },
    _ => return Err(Error::InvalidSegment("base64 line_case")),
  };
  let decoded_len = if offset < meta.len() {
    read_len(meta, &mut offset)?
  } else {
    segment.data.len()
  };

  // The output size follows from the meta alone, so it is checked against
  // the budget before the payload is decoded.
  let encoded_len = (decoded_len as u64).div_ceil(3).checked_mul(4).ok_or(Error::LengthOverflow)?;
  let written = lines
    .written(encoded_len)
    .ok_or(Error::InvalidSegment("base64 line lengths mismatch"))?;
  let out_len = written
    .checked_mul(2)
    .and_then(|n| n.checked_add(encoded_len))
    .and_then(|n| n.checked_add(header.len() as u64))
    .ok_or(Error::LengthOverflow)?;
  budget.check(out_len, config)?;
  // A `Vec` holds at most `isize::MAX` bytes.
  let out_len = isize::try_from(out_len).map_err(|_| Error::LengthOverflow)?.unsigned_abs();

  let payload = decode_payload(payload_kind, &segment.data, config, depth, budget)?;
  if decoded_len != payload.len() {
    return Err(Error::InvalidSegment("base64 decoded_len mismatch"));
  }
  // A nested payload consumed budget of its own.
  budget.check(out_len as u64, config)?;
  // Encode into the tail of the output, then move each line forward to make
  // room for its CRLF. The gap between source and destination is two bytes
  // per line still to write, so no line overwrites text not yet moved.
  let mut out = vec![0u8; out_len];
  out[..header.len()].copy_from_slice(header);
  let mut src = out_len - encoded_len as usize;
  BASE64_ENGINE
    .encode_slice(&*payload, &mut out[src..])
    .map_err(|_| Error::InvalidSegment("base64 encoded length"))?;
  let mut dst = header.len();
  for len in lines.iter().take(written as usize) {
    let len = len.min(out_len - src);
    out.copy_within(src..src + len, dst);
    out[dst + len..dst + len + 2].copy_from_slice(b"\r\n");
    src += len;
    dst += len + 2;
  }
  budget.consume(out.len(), config)?;
  Ok(out)
}
