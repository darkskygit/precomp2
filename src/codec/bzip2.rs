use std::io::{Read, Write};

use bzip2::{Compression, read::BzDecoder, write::BzEncoder};

use super::{
  DecodeConfig, EncodeConfig, PenaltyEntry,
  budget::{DecodeBudget, EncodeBudget},
  payload::{choose_payload, decode_payload},
  varint::{read_varint_slice, write_varint_vec},
};
use crate::{
  error::{Error, Result},
  pcf2::{Pcf2Segment, SegmentKind},
};

pub(super) fn encode_bzip2(input: &[u8], config: &EncodeConfig, depth: u32, budget: &mut EncodeBudget) -> Result<Option<Pcf2Segment>> {
  if input.len() < 4 || &input[..3] != b"BZh" {
    return Ok(None);
  }
  let level = input[3];
  if !(b'1'..=b'9').contains(&level) {
    return Ok(None);
  }
  let level_num = (level - b'0') as u32;
  let plain = match decode_bzip2_raw(input) {
    Ok(v) => v,
    Err(_) => return Ok(None),
  };
  let (payload_kind, payload) = choose_payload(plain.clone(), input.len(), config, depth, budget)?;
  let recompressed = encode_bzip2_raw(&plain, level_num)?;
  if recompressed.len() != input.len() {
    return Ok(None);
  }
  let penalty = compute_penalty(input, &recompressed);

  let mut meta = Vec::new();
  meta.push(0);
  meta.push(payload_kind);
  meta.push(level);
  write_varint_vec(penalty.len() as u64, &mut meta);
  for entry in &penalty {
    meta.extend_from_slice(&entry.pos.to_be_bytes());
    meta.push(entry.value);
  }
  write_varint_vec(plain.len() as u64, &mut meta);

  Ok(Some(Pcf2Segment {
    kind: SegmentKind::Bzip2 as u8,
    flags: 0,
    orig_len: input.len() as u64,
    meta,
    data: payload,
  }))
}

pub(super) fn decode_bzip2_segment(segment: &Pcf2Segment, config: &DecodeConfig, depth: u32, budget: &mut DecodeBudget) -> Result<Vec<u8>> {
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
  let penalty_count = read_varint_slice(&segment.meta, &mut offset)? as usize;
  let mut penalty = Vec::with_capacity(penalty_count);
  for _ in 0..penalty_count {
    if offset + 5 > segment.meta.len() {
      return Err(Error::InvalidSegment("bzip2 penalty bounds"));
    }
    let pos = u32::from_be_bytes([
      segment.meta[offset],
      segment.meta[offset + 1],
      segment.meta[offset + 2],
      segment.meta[offset + 3],
    ]);
    let value = segment.meta[offset + 4];
    offset += 5;
    penalty.push(PenaltyEntry { pos, value });
  }
  let decoded_len = if offset < segment.meta.len() {
    Some(read_varint_slice(&segment.meta, &mut offset)? as usize)
  } else {
    None
  };

  let payload = decode_payload(payload_kind, segment.data.clone(), config, depth, budget)?;
  if let Some(expected) = decoded_len {
    if expected != payload.len() {
      return Err(Error::InvalidSegment("bzip2 decoded_len mismatch"));
    }
  }
  let level_num = (level - b'0') as u32;
  let mut recompressed = encode_bzip2_raw(&payload, level_num)?;
  for entry in penalty {
    let pos = entry.pos as usize;
    if pos >= recompressed.len() {
      return Err(Error::InvalidSegment("bzip2 penalty out of range"));
    }
    recompressed[pos] = entry.value;
  }
  budget.consume(recompressed.len(), config)?;
  Ok(recompressed)
}

fn decode_bzip2_raw(input: &[u8]) -> Result<Vec<u8>> {
  let mut decoder = BzDecoder::new(input);
  let mut out = Vec::new();
  decoder.read_to_end(&mut out)?;
  Ok(out)
}

fn encode_bzip2_raw(input: &[u8], level: u32) -> Result<Vec<u8>> {
  let mut encoder = BzEncoder::new(Vec::new(), Compression::new(level));
  encoder.write_all(input)?;
  Ok(encoder.finish()?)
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
