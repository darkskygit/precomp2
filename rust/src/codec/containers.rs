use super::{
  EncodeConfig,
  budget::EncodeBudget,
  deflate::{encode_deflate, encode_zlib},
};
use crate::{error::Result, pcf2::Pcf2Segment};

pub(super) fn encode_zip(input: &[u8], config: &EncodeConfig, depth: u32, budget: &mut EncodeBudget) -> Result<Option<Vec<Pcf2Segment>>> {
  if input.len() < 4 || &input[..4] != b"PK\x03\x04" {
    return Ok(None);
  }
  let mut segments = Vec::new();
  let mut cursor = 0usize;
  let mut offset = 0usize;

  while offset + 30 <= input.len() {
    if &input[offset..offset + 4] != b"PK\x03\x04" {
      break;
    }
    let flags = u16::from_le_bytes([input[offset + 6], input[offset + 7]]);
    if flags & 0x0008 != 0 {
      return Ok(None);
    }
    let compression = u16::from_le_bytes([input[offset + 8], input[offset + 9]]);
    let compressed_size = u32::from_le_bytes([input[offset + 18], input[offset + 19], input[offset + 20], input[offset + 21]]) as usize;
    let name_len = u16::from_le_bytes([input[offset + 26], input[offset + 27]]) as usize;
    let extra_len = u16::from_le_bytes([input[offset + 28], input[offset + 29]]) as usize;
    let data_start = offset + 30 + name_len + extra_len;
    let data_end = data_start + compressed_size;
    if data_end > input.len() {
      break;
    }

    if cursor < data_start {
      segments.push(Pcf2Segment::lit(input[cursor..data_start].to_vec()));
    }

    if compression == 8 {
      if let Some(segment) = encode_deflate(&input[data_start..data_end], config, depth, budget)? {
        segments.push(segment);
      } else {
        return Ok(None);
      }
    } else {
      segments.push(Pcf2Segment::lit(input[data_start..data_end].to_vec()));
    }

    cursor = data_end;
    offset = data_end;
  }

  if segments.is_empty() {
    return Ok(None);
  }

  if cursor < input.len() {
    segments.push(Pcf2Segment::lit(input[cursor..].to_vec()));
  }

  Ok(Some(segments))
}

pub(super) fn encode_swf(input: &[u8], config: &EncodeConfig, depth: u32, budget: &mut EncodeBudget) -> Result<Option<Vec<Pcf2Segment>>> {
  if input.len() < 8 {
    return Ok(None);
  }
  if &input[..3] != b"CWS" {
    return Ok(None);
  }
  let header = &input[..8];
  let body = &input[8..];
  let mut segments = Vec::new();
  segments.push(Pcf2Segment::lit(header.to_vec()));
  if let Some(segment) = encode_zlib(body, config, depth, budget)? {
    segments.push(segment);
  } else {
    return Ok(None);
  }
  Ok(Some(segments))
}
