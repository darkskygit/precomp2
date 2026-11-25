use std::io::{Read, Write};

use bzip2::{Compression, read::BzDecoder, write::BzEncoder};

use super::{
  DecodeConfig, EncodeConfig,
  budget::{BoundedWriter, DecodeBudget, EncodeBudget},
  payload::{choose_payload, decode_payload},
  varint::{read_len, take_bytes, write_varint_vec},
};
use crate::{
  error::{Error, Result},
  pcf2::{Pcf2Segment, SegmentKind},
};

/// Big-endian u32 position plus the original byte.
const PENALTY_ENTRY_LEN: usize = 5;

/// An original byte that differs from the recompressed stream.
struct PenaltyEntry {
  pos: u32,
  value: u8,
}

pub(super) fn encode_bzip2(input: &[u8], config: &EncodeConfig, depth: u32, budget: &mut EncodeBudget) -> Result<Option<Pcf2Segment>> {
  if input.len() < 4 || &input[..3] != b"BZh" {
    return Ok(None);
  }
  let level = input[3];
  if !(b'1'..=b'9').contains(&level) {
    return Ok(None);
  }
  let level_num = (level - b'0') as u32;
  // Budget guard: a stream that decompresses past the remaining encode
  // budget stays literal, so a small bomb is never fully decompressed.
  let limit = budget.remaining(config);
  let plain = match decode_bzip2_raw(input, limit) {
    Ok(Some(v)) => v,
    Ok(None) | Err(_) => return Ok(None),
  };
  let recompressed = encode_bzip2_raw(&plain, level_num)?;
  if recompressed.len() != input.len() {
    return Ok(None);
  }
  let penalty = compute_penalty(input, &recompressed);
  let plain_len = plain.len();
  let (payload_kind, payload) = choose_payload(plain, input.len(), config, depth, budget)?;

  let mut meta = Vec::new();
  meta.push(0);
  meta.push(payload_kind);
  meta.push(level);
  write_varint_vec(penalty.len() as u64, &mut meta);
  for entry in &penalty {
    meta.extend_from_slice(&entry.pos.to_be_bytes());
    meta.push(entry.value);
  }
  write_varint_vec(plain_len as u64, &mut meta);

  Ok(Some(Pcf2Segment {
    kind: SegmentKind::Bzip2 as u8,
    flags: 0,
    orig_len: input.len() as u64,
    meta,
    data: payload,
  }))
}

pub(super) fn decode_bzip2_segment(segment: &Pcf2Segment, config: &DecodeConfig, depth: u32, budget: &mut DecodeBudget) -> Result<Vec<u8>> {
  budget.check(segment.orig_len, config)?;
  if segment.meta.len() < 3 {
    return Err(Error::InvalidSegment("bzip2 meta"));
  }
  let meta_version = segment.meta[0];
  let payload_kind = segment.meta[1];
  let level = segment.meta[2];
  if meta_version != 0 {
    return Err(Error::InvalidSegment("bzip2 meta_version"));
  }
  if !(b'1'..=b'9').contains(&level) {
    return Err(Error::InvalidSegment("bzip2 level"));
  }
  let mut offset = 3usize;
  let penalty_count = read_len(&segment.meta, &mut offset)?;
  let entries = penalty_count
    .checked_mul(PENALTY_ENTRY_LEN)
    .ok_or(Error::InvalidSegment("bzip2 penalty bounds"))?;
  let penalty = take_bytes(&segment.meta, &mut offset, entries, "bzip2 penalty bounds")?;
  let decoded_len = if offset < segment.meta.len() {
    Some(read_len(&segment.meta, &mut offset)?)
  } else {
    None
  };

  let payload = decode_payload(payload_kind, &segment.data, config, depth, budget)?;
  if let Some(expected) = decoded_len
    && expected != payload.len()
  {
    return Err(Error::InvalidSegment("bzip2 decoded_len mismatch"));
  }
  let level_num = (level - b'0') as u32;
  let mut recompressed = encode_bzip2_raw_limited(&payload, level_num, budget.remaining(config))?;
  for entry in penalty.as_chunks::<PENALTY_ENTRY_LEN>().0 {
    let pos = u32::from_be_bytes([entry[0], entry[1], entry[2], entry[3]]) as usize;
    *recompressed
      .get_mut(pos)
      .ok_or(Error::InvalidSegment("bzip2 penalty out of range"))? = entry[4];
  }
  budget.consume(recompressed.len(), config)?;
  Ok(recompressed)
}

/// Decompresses `input`; `None` when the plain text exceeds `limit` bytes.
fn decode_bzip2_raw(input: &[u8], limit: u64) -> Result<Option<Vec<u8>>> {
  let mut out = Vec::new();
  BzDecoder::new(input).take(limit.saturating_add(1)).read_to_end(&mut out)?;
  Ok((out.len() as u64 <= limit).then_some(out))
}

fn encode_bzip2_raw(input: &[u8], level: u32) -> Result<Vec<u8>> {
  let mut encoder = BzEncoder::new(Vec::new(), Compression::new(level));
  encoder.write_all(input)?;
  Ok(encoder.finish()?)
}

fn encode_bzip2_raw_limited(input: &[u8], level: u32, limit: u64) -> Result<Vec<u8>> {
  let mut encoder = BzEncoder::new(BoundedWriter::new(limit), Compression::new(level));
  if let Err(err) = encoder.write_all(input) {
    if encoder.get_ref().is_exceeded() {
      return Err(Error::Other("decode output exceeds budget".to_string()));
    }
    return Err(err.into());
  }
  match encoder.finish() {
    Ok(writer) => Ok(writer.into_vec()),
    Err(err) => {
      if err.kind() == std::io::ErrorKind::FileTooLarge {
        Err(Error::Other("decode output exceeds budget".to_string()))
      } else {
        Err(err.into())
      }
    }
  }
}

fn compute_penalty(original: &[u8], recompressed: &[u8]) -> Vec<PenaltyEntry> {
  let mut penalty = Vec::new();
  for (pos, (&a, &b)) in original.iter().zip(recompressed).enumerate() {
    if a != b {
      penalty.push(PenaltyEntry { pos: pos as u32, value: a });
    }
  }
  penalty
}
