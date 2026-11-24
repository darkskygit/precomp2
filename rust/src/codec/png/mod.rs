mod bitmap;
mod display;

use std::borrow::Cow;

use bitmap::{PngIhdr, PngWebpMeta, apply_png_filters_with_types, png_bytes_per_pixel, undo_png_filters};
pub(super) use bitmap::{decode_webp_bitmap, encode_webp_lossless};
use crc32fast::Hasher;
pub(super) use display::extract_png_display;
use display::parse_png_metadata;

use super::{
  DecodeConfig, EncodeConfig,
  budget::{DecodeBudget, EncodeBudget},
  deflate::adler32,
  payload::{choose_payload, decode_payload},
  preflate::{PreflateAnalysis, build_preflate_data, parse_preflate_data, preflate_analyze, preflate_reencode},
  varint::{read_varint_slice, write_varint_vec},
};
use crate::{
  error::{Error, Result},
  pcf2::{Pcf2Segment, SegmentKind},
};

pub(super) fn encode_png(input: &[u8], config: &EncodeConfig, depth: u32, budget: &mut EncodeBudget) -> Result<Option<Vec<Pcf2Segment>>> {
  if input.len() < 8 || &input[..8] != b"\x89PNG\r\n\x1a\n" {
    return Ok(None);
  }
  let metadata = if config.enable_png_webp {
    match parse_png_metadata(input) {
      Ok(plan) => plan,
      Err(_) => return Ok(Some(vec![Pcf2Segment::lit(input.to_vec())])),
    }
  } else {
    None
  };
  let mut offset = 8;
  let mut idat_chunks = Vec::new();
  let mut idat_lengths = Vec::new();
  let mut idat_crcs = Vec::new();
  let mut first_idat_start = None;
  let mut last_idat_end = None;

  while offset + 12 <= input.len() {
    let len = u32::from_be_bytes([input[offset], input[offset + 1], input[offset + 2], input[offset + 3]]) as usize;
    let chunk_type = &input[offset + 4..offset + 8];
    let data_start = offset + 8;
    let data_end = data_start + len;
    let crc_start = data_end;
    let crc_end = crc_start + 4;
    if crc_end > input.len() {
      break;
    }
    if chunk_type == b"IDAT" {
      if first_idat_start.is_none() {
        first_idat_start = Some(offset);
      }
      last_idat_end = Some(crc_end);
      idat_lengths.push(len as u64);
      let crc = u32::from_be_bytes([input[crc_start], input[crc_start + 1], input[crc_start + 2], input[crc_start + 3]]);
      idat_crcs.push(crc);
      idat_chunks.extend_from_slice(&input[data_start..data_end]);
    } else if last_idat_end.is_some() {
      break;
    }
    offset = crc_end;
    if chunk_type == b"IEND" {
      break;
    }
  }

  let Some(idat_start) = first_idat_start else {
    return Ok(None);
  };
  let Some(idat_end) = last_idat_end else {
    return Ok(None);
  };

  if idat_chunks.is_empty() {
    return Ok(None);
  }

  if idat_chunks.len() < 6 {
    return Ok(None);
  }
  let zlib_header = [idat_chunks[0], idat_chunks[1]];
  let zlib_footer = &idat_chunks[idat_chunks.len() - 4..];
  let deflate = &idat_chunks[2..idat_chunks.len() - 4];
  let cmf = zlib_header[0];
  let flg = zlib_header[1];
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

  let Ok(PreflateAnalysis {
    deflate_len,
    corrections,
    plain,
  }) = preflate_analyze(deflate)
  else {
    return Ok(None);
  };
  if deflate_len != deflate.len() as u64 {
    return Ok(None);
  }
  let adler = adler32(&plain);
  if adler != u32::from_be_bytes([zlib_footer[0], zlib_footer[1], zlib_footer[2], zlib_footer[3]]) {
    return Ok(None);
  }

  let mut png_payload_kind = 0u8;
  let mut payload_bytes = plain;
  let mut webp_meta = None;

  if let Some(plan) = metadata {
    let ihdr = plan.ihdr;
    let bpp = ihdr.bytes_per_pixel()?;
    if let Ok((bitmap, filters)) = undo_png_filters(&payload_bytes, ihdr.width, ihdr.height, bpp)
      && let Ok(webp_bytes) = encode_webp_lossless(&bitmap, ihdr.width, ihdr.height, ihdr.color_type)
    {
      png_payload_kind = 1;
      payload_bytes = webp_bytes;
      webp_meta = Some(PngWebpMeta {
        color_type: ihdr.color_type,
        width: ihdr.width,
        height: ihdr.height,
        filters,
      });
    }
  }

  let (payload_kind, payload) = choose_payload(payload_bytes, idat_chunks.len(), config, depth, budget)?;

  let mut meta = Vec::new();
  meta.push(1);
  meta.push(payload_kind);
  meta.push(png_payload_kind);
  meta.extend_from_slice(&zlib_header);
  meta.extend_from_slice(zlib_footer);
  write_varint_vec(idat_lengths.len() as u64, &mut meta);
  for len in &idat_lengths {
    write_varint_vec(*len, &mut meta);
  }
  meta.push(1);
  for crc in &idat_crcs {
    meta.extend_from_slice(&crc.to_le_bytes());
  }
  if let Some(meta_info) = webp_meta {
    meta.push(meta_info.color_type);
    write_varint_vec(meta_info.width as u64, &mut meta);
    write_varint_vec(meta_info.height as u64, &mut meta);
    meta.extend_from_slice(&meta_info.filters);
  }

  let data = build_preflate_data(&corrections, &payload);

  let mut segments = Vec::new();
  if idat_start > 0 {
    segments.push(Pcf2Segment::lit(input[..idat_start].to_vec()));
  }

  let idat_orig_len = idat_end - idat_start;
  segments.push(Pcf2Segment {
    kind: SegmentKind::PngIdat as u8,
    flags: 0,
    orig_len: idat_orig_len as u64,
    meta,
    data,
  });
  if idat_end < input.len() {
    segments.push(Pcf2Segment::lit(input[idat_end..].to_vec()));
  }
  Ok(Some(segments))
}

struct ParsedPngMeta {
  payload_kind: u8,
  png_payload_kind: u8,
  zlib_header: [u8; 2],
  zlib_footer: [u8; 4],
  idat_lengths: Vec<usize>,
  crc_present: u8,
  idat_crcs: Vec<u32>,
  webp_meta: Option<PngWebpMeta>,
}

fn parse_png_meta(segment: &Pcf2Segment) -> Result<ParsedPngMeta> {
  if segment.meta.len() < 3 {
    return Err(Error::InvalidSegment("png meta"));
  }
  let meta_version = segment.meta[0];
  let payload_kind = segment.meta[1];
  let png_payload_kind = segment.meta[2];
  let rest = &segment.meta[3..];
  if meta_version != 1 {
    return Err(Error::InvalidSegment("png meta_version"));
  }
  if rest.len() < 6 {
    return Err(Error::InvalidSegment("png meta truncated"));
  }
  let zlib_header = rest[..2].try_into().unwrap();
  let zlib_footer = rest[2..6].try_into().unwrap();
  let mut offset = 6usize;
  let idat_count = usize::try_from(read_varint_slice(rest, &mut offset)?).map_err(|_| Error::LengthOverflow)?;
  if idat_count == 0 || idat_count > rest.len() - offset {
    return Err(Error::InvalidSegment("png idat count"));
  }
  let mut idat_lengths = Vec::with_capacity(idat_count);
  for _ in 0..idat_count {
    idat_lengths.push(usize::try_from(read_varint_slice(rest, &mut offset)?).map_err(|_| Error::LengthOverflow)?);
  }
  if offset >= rest.len() {
    return Err(Error::InvalidSegment("png meta truncated"));
  }
  let crc_present = rest[offset];
  offset += 1;
  let mut idat_crcs = Vec::new();
  if crc_present > 1 {
    return Err(Error::InvalidSegment("png crc flag"));
  }
  if crc_present == 1 {
    let crc_bytes = idat_count.checked_mul(4).ok_or(Error::LengthOverflow)?;
    if crc_bytes > rest.len() - offset {
      return Err(Error::InvalidSegment("png crc bounds"));
    }
    for _ in 0..idat_count {
      let crc = u32::from_le_bytes([rest[offset], rest[offset + 1], rest[offset + 2], rest[offset + 3]]);
      idat_crcs.push(crc);
      offset += 4;
    }
  }

  let mut webp_meta = None;
  if png_payload_kind == 1 {
    if offset >= rest.len() {
      return Err(Error::InvalidSegment("png webp meta"));
    }
    let color_type = rest[offset];
    offset += 1;
    let width = u32::try_from(read_varint_slice(rest, &mut offset)?).map_err(|_| Error::LengthOverflow)?;
    let height = u32::try_from(read_varint_slice(rest, &mut offset)?).map_err(|_| Error::LengthOverflow)?;
    let height_usize = usize::try_from(height).map_err(|_| Error::InvalidSegment("png webp height"))?;
    if height_usize > rest.len() - offset {
      return Err(Error::InvalidSegment("png webp filters"));
    }
    let filters = rest[offset..offset + height_usize].to_vec();
    offset += height_usize;
    webp_meta = Some(PngWebpMeta {
      color_type,
      width,
      height,
      filters,
    });
  } else if png_payload_kind != 0 {
    return Err(Error::InvalidSegment("png payload_kind"));
  }

  if offset != rest.len() {
    return Err(Error::InvalidSegment("png meta trailing"));
  }

  Ok(ParsedPngMeta {
    payload_kind,
    png_payload_kind,
    zlib_header,
    zlib_footer,
    idat_lengths,
    crc_present,
    idat_crcs,
    webp_meta,
  })
}

pub(super) fn decode_png_segment(segment: &Pcf2Segment, config: &DecodeConfig, depth: u32, budget: &mut DecodeBudget) -> Result<Vec<u8>> {
  let ParsedPngMeta {
    payload_kind,
    png_payload_kind,
    zlib_header,
    zlib_footer,
    idat_lengths,
    crc_present,
    idat_crcs,
    webp_meta,
  } = parse_png_meta(segment)?;

  let (corrections, payload) = parse_preflate_data(&segment.data)?;
  let payload = decode_payload(payload_kind, payload, config, depth, budget)?;
  let plain = if png_payload_kind == 0 {
    payload
  } else {
    let meta = webp_meta.ok_or(Error::InvalidSegment("png webp meta"))?;
    let bitmap = decode_webp_bitmap(&payload, meta.width, meta.height, meta.color_type)?;
    let bpp = png_bytes_per_pixel(meta.color_type)?;
    Cow::Owned(apply_png_filters_with_types(&bitmap, meta.width, meta.height, bpp, &meta.filters)?)
  };
  let idat_data = preflate_reencode(corrections, &plain)?;
  let mut full_idat = Vec::with_capacity(idat_data.len() + 6);
  full_idat.extend_from_slice(&zlib_header);
  full_idat.extend_from_slice(&idat_data);
  full_idat.extend_from_slice(&zlib_footer);
  let total_len = idat_lengths.iter().try_fold(0usize, |sum, &len| sum.checked_add(len));
  if total_len != Some(full_idat.len()) {
    return Err(Error::InvalidSegment("png idat length mismatch"));
  }

  let mut out = Vec::new();
  let mut cursor = 0usize;
  for (idx, len) in idat_lengths.iter().enumerate() {
    let len_u32 = *len as u32;
    out.extend_from_slice(&len_u32.to_be_bytes());
    out.extend_from_slice(b"IDAT");
    out.extend_from_slice(&full_idat[cursor..cursor + len]);
    cursor += len;
    let crc = if crc_present == 1 {
      idat_crcs.get(idx).copied().unwrap_or(0)
    } else {
      let mut hasher = Hasher::new();
      hasher.update(b"IDAT");
      hasher.update(&full_idat[cursor - len..cursor]);
      hasher.finalize()
    };
    out.extend_from_slice(&crc.to_be_bytes());
  }
  budget.consume(out.len(), config)?;
  Ok(out)
}
