use ::base64::Engine as _;
use memchr::memmem;

use super::{
  BASE64_ENGINE, DecodeConfig, EncodeConfig,
  budget::{DecodeBudget, EncodeBudget},
  payload::{choose_payload, decode_payload},
  varint::{read_varint_slice, write_varint_vec},
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
    let line_end = match memmem::find(&input[cursor..], b"\r\n") {
      Some(v) => cursor + v,
      None => return None,
    };
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

pub(super) fn decode_base64_segment(
  segment: &Pcf2Segment,
  config: &DecodeConfig,
  depth: u32,
  budget: &mut DecodeBudget,
) -> Result<Vec<u8>> {
  if segment.meta.len() < 2 {
    return Err(Error::InvalidSegment("base64 meta"));
  }
  let meta_version = segment.meta[0];
  let payload_kind = segment.meta[1];
  if meta_version != 0 {
    return Err(Error::InvalidSegment("base64 meta_version"));
  }
  let mut offset = 2usize;
  let header_len = read_varint_slice(&segment.meta, &mut offset)? as usize;
  if offset + header_len > segment.meta.len() {
    return Err(Error::InvalidSegment("base64 header bounds"));
  }
  let header = segment.meta[offset..offset + header_len].to_vec();
  offset += header_len;
  if offset >= segment.meta.len() {
    return Err(Error::InvalidSegment("base64 line_case"));
  }
  let line_case = segment.meta[offset];
  offset += 1;
  let line_count = read_varint_slice(&segment.meta, &mut offset)? as usize;
  if line_count == 0 {
    return Err(Error::InvalidSegment("base64 line_count"));
  }
  let mut line_lengths = Vec::new();
  match line_case {
    0 => {
      if offset >= segment.meta.len() {
        return Err(Error::InvalidSegment("base64 line_len_0"));
      }
      let len = segment.meta[offset] as usize;
      offset += 1;
      line_lengths = vec![len; line_count];
    }
    1 => {
      if offset + 1 >= segment.meta.len() {
        return Err(Error::InvalidSegment("base64 line_len_1"));
      }
      let len = segment.meta[offset] as usize;
      let last = segment.meta[offset + 1] as usize;
      offset += 2;
      line_lengths = vec![len; line_count];
      if let Some(last_len) = line_lengths.last_mut() {
        *last_len = last;
      }
    }
    2 => {
      if offset + line_count > segment.meta.len() {
        return Err(Error::InvalidSegment("base64 line_len_list"));
      }
      for _ in 0..line_count {
        line_lengths.push(segment.meta[offset] as usize);
        offset += 1;
      }
    }
    _ => return Err(Error::InvalidSegment("base64 line_case")),
  }
  let decoded_len = if offset < segment.meta.len() {
    read_varint_slice(&segment.meta, &mut offset)? as usize
  } else {
    segment.data.len()
  };

  let payload = decode_payload(payload_kind, segment.data.clone(), config, depth, budget)?;
  if decoded_len != payload.len() {
    return Err(Error::InvalidSegment("base64 decoded_len mismatch"));
  }

  let encoded = BASE64_ENGINE.encode(payload);
  let mut out = Vec::new();
  out.extend_from_slice(&header);
  let mut cursor = 0usize;
  for len in line_lengths {
    if cursor >= encoded.len() {
      break;
    }
    let end = (cursor + len).min(encoded.len());
    out.extend_from_slice(encoded[cursor..end].as_bytes());
    out.extend_from_slice(b"\r\n");
    cursor = end;
  }
  if cursor != encoded.len() {
    return Err(Error::InvalidSegment("base64 line lengths mismatch"));
  }
  budget.consume(out.len(), config)?;
  Ok(out)
}
