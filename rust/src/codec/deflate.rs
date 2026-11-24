use super::{
  DecodeConfig, EncodeConfig,
  budget::{DecodeBudget, EncodeBudget},
  payload::{choose_payload, decode_payload},
  preflate::{build_preflate_data, parse_preflate_data, parse_preflate_output, preflate_analyze, preflate_reencode},
  varint::{parse_meta_header, read_varint_slice, write_varint_vec},
};
use crate::{
  error::{Error, Result},
  pcf2::{Pcf2Segment, SegmentKind},
};

pub(super) fn encode_deflate(input: &[u8], config: &EncodeConfig, depth: u32, budget: &mut EncodeBudget) -> Result<Option<Pcf2Segment>> {
  if input.is_empty() {
    return Ok(None);
  }
  let preflate = match preflate_analyze(input) {
    Ok(v) => v,
    Err(_) => return Ok(None),
  };
  let (deflate_len, corrections, plain) = match parse_preflate_output(&preflate) {
    Ok(v) => v,
    Err(_) => return Ok(None),
  };
  if deflate_len != input.len() as u64 {
    return Ok(None);
  }
  let (payload_kind, payload) = choose_payload(plain, input.len(), config, depth, budget)?;
  let data = build_preflate_data(&corrections, &payload);
  let meta = vec![0u8, payload_kind];
  Ok(Some(Pcf2Segment {
    kind: SegmentKind::Dflt as u8,
    flags: 0,
    orig_len: input.len() as u64,
    meta,
    data,
  }))
}

pub(super) fn encode_zlib(input: &[u8], config: &EncodeConfig, depth: u32, budget: &mut EncodeBudget) -> Result<Option<Pcf2Segment>> {
  if input.len() < 6 {
    return Ok(None);
  }
  let cmf = input[0];
  let flg = input[1];
  if (cmf & 0x0f) != 8 {
    return Ok(None);
  }
  if (cmf >> 4) > 7 {
    return Ok(None);
  }
  let check = ((cmf as u16) << 8) | (flg as u16);
  if !check.is_multiple_of(31) {
    return Ok(None);
  }
  if (flg & 0x20) != 0 {
    return Ok(None);
  }
  let header = [cmf, flg];
  let footer = &input[input.len() - 4..];
  let deflate = &input[2..input.len() - 4];
  let preflate = match preflate_analyze(deflate) {
    Ok(v) => v,
    Err(_) => return Ok(None),
  };
  let (deflate_len, corrections, plain) = match parse_preflate_output(&preflate) {
    Ok(v) => v,
    Err(_) => return Ok(None),
  };
  if deflate_len != deflate.len() as u64 {
    return Ok(None);
  }
  let adler = adler32(&plain);
  if adler != u32::from_be_bytes([footer[0], footer[1], footer[2], footer[3]]) {
    return Ok(None);
  }
  let (payload_kind, payload) = choose_payload(plain, input.len(), config, depth, budget)?;
  let data = build_preflate_data(&corrections, &payload);
  let mut meta = Vec::with_capacity(2 + 2 + 4);
  meta.push(0);
  meta.push(payload_kind);
  meta.extend_from_slice(&header);
  meta.extend_from_slice(footer);
  Ok(Some(Pcf2Segment {
    kind: SegmentKind::Zlib as u8,
    flags: 0,
    orig_len: input.len() as u64,
    meta,
    data,
  }))
}

pub(super) fn encode_gzip(input: &[u8], config: &EncodeConfig, depth: u32, budget: &mut EncodeBudget) -> Result<Option<Pcf2Segment>> {
  if input.len() < 18 || input[0] != 0x1f || input[1] != 0x8b {
    return Ok(None);
  }
  let (header_len, header_bytes) = parse_gzip_header(input)?;
  if input.len() < header_len + 8 {
    return Ok(None);
  }
  let footer = &input[input.len() - 8..];
  let deflate = &input[header_len..input.len() - 8];
  let preflate = match preflate_analyze(deflate) {
    Ok(v) => v,
    Err(_) => return Ok(None),
  };
  let (deflate_len, corrections, plain) = match parse_preflate_output(&preflate) {
    Ok(v) => v,
    Err(_) => return Ok(None),
  };
  if deflate_len != deflate.len() as u64 {
    return Ok(None);
  }
  if plain.len() > u32::MAX as usize {
    return Ok(None);
  }
  let crc_expected = u32::from_le_bytes([footer[0], footer[1], footer[2], footer[3]]);
  let isize_expected = u32::from_le_bytes([footer[4], footer[5], footer[6], footer[7]]);
  let crc_actual = crc32fast::hash(&plain);
  if crc_actual != crc_expected || isize_expected != plain.len() as u32 {
    return Ok(None);
  }
  let (payload_kind, payload) = choose_payload(plain, input.len(), config, depth, budget)?;
  let data = build_preflate_data(&corrections, &payload);
  let mut meta = Vec::new();
  meta.push(0);
  meta.push(payload_kind);
  write_varint_vec(header_bytes.len() as u64, &mut meta);
  meta.extend_from_slice(&header_bytes);
  meta.extend_from_slice(footer);
  Ok(Some(Pcf2Segment {
    kind: SegmentKind::Gzip as u8,
    flags: 0,
    orig_len: input.len() as u64,
    meta,
    data,
  }))
}

pub(super) fn decode_deflate_segment(
  segment: &Pcf2Segment,
  config: &DecodeConfig,
  depth: u32,
  budget: &mut DecodeBudget,
) -> Result<Vec<u8>> {
  let (meta_version, payload_kind, _rest) = parse_meta_header(&segment.meta)?;
  if meta_version != 0 {
    return Err(Error::InvalidSegment("deflate meta_version"));
  }
  let (corrections, payload) = parse_preflate_data(&segment.data)?;
  let plain = decode_payload(payload_kind, payload, config, depth, budget)?;
  let deflate = preflate_reencode(&corrections, &plain)?;
  budget.consume(deflate.len(), config)?;
  Ok(deflate)
}

pub(super) fn decode_zlib_segment(segment: &Pcf2Segment, config: &DecodeConfig, depth: u32, budget: &mut DecodeBudget) -> Result<Vec<u8>> {
  let (meta_version, payload_kind, mut rest) = parse_meta_header(&segment.meta)?;
  if meta_version != 0 || rest.len() < 6 {
    return Err(Error::InvalidSegment("zlib meta"));
  }
  let header = &rest[..2];
  let footer = &rest[2..6];
  rest = &rest[6..];
  if !rest.is_empty() {
    return Err(Error::InvalidSegment("zlib meta trailing"));
  }
  let (corrections, payload) = parse_preflate_data(&segment.data)?;
  let plain = decode_payload(payload_kind, payload, config, depth, budget)?;
  let deflate = preflate_reencode(&corrections, &plain)?;
  let mut out = Vec::with_capacity(deflate.len() + 6);
  out.extend_from_slice(header);
  out.extend_from_slice(&deflate);
  out.extend_from_slice(footer);
  budget.consume(out.len(), config)?;
  Ok(out)
}

pub(super) fn decode_gzip_segment(segment: &Pcf2Segment, config: &DecodeConfig, depth: u32, budget: &mut DecodeBudget) -> Result<Vec<u8>> {
  let (meta_version, payload_kind, rest) = parse_meta_header(&segment.meta)?;
  if meta_version != 0 {
    return Err(Error::InvalidSegment("gzip meta_version"));
  }
  let mut offset = 0usize;
  let header_len = read_varint_slice(rest, &mut offset)? as usize;
  if offset + header_len + 8 > rest.len() {
    return Err(Error::InvalidSegment("gzip meta bounds"));
  }
  let header = &rest[offset..offset + header_len];
  offset += header_len;
  let footer = &rest[offset..offset + 8];

  let (corrections, payload) = parse_preflate_data(&segment.data)?;
  let plain = decode_payload(payload_kind, payload, config, depth, budget)?;
  let deflate = preflate_reencode(&corrections, &plain)?;
  let mut out = Vec::with_capacity(header.len() + deflate.len() + 8);
  out.extend_from_slice(header);
  out.extend_from_slice(&deflate);
  out.extend_from_slice(footer);
  budget.consume(out.len(), config)?;
  Ok(out)
}

pub(super) fn parse_gzip_header(input: &[u8]) -> Result<(usize, Vec<u8>)> {
  if input.len() < 10 {
    return Err(Error::InvalidSegment("gzip header too short"));
  }
  let flags = input[3];
  let mut offset = 10usize;
  if flags & 0x04 != 0 {
    if offset + 2 > input.len() {
      return Err(Error::InvalidSegment("gzip extra"));
    }
    let xlen = u16::from_le_bytes([input[offset], input[offset + 1]]) as usize;
    offset += 2 + xlen;
  }
  if flags & 0x08 != 0 {
    while offset < input.len() && input[offset] != 0 {
      offset += 1;
    }
    offset += 1;
  }
  if flags & 0x10 != 0 {
    while offset < input.len() && input[offset] != 0 {
      offset += 1;
    }
    offset += 1;
  }
  if flags & 0x02 != 0 {
    offset += 2;
  }
  if offset > input.len() {
    return Err(Error::InvalidSegment("gzip header bounds"));
  }
  Ok((offset, input[..offset].to_vec()))
}

pub(super) fn adler32(data: &[u8]) -> u32 {
  const MOD: u32 = 65521;
  let mut s1 = 1u32;
  let mut s2 = 0u32;
  for &b in data {
    s1 = (s1 + b as u32) % MOD;
    s2 = (s2 + s1) % MOD;
  }
  (s2 << 16) | s1
}
