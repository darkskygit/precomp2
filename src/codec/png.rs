use crc32fast::Hasher;

use super::{
  DecodeConfig, EncodeConfig,
  budget::{DecodeBudget, EncodeBudget},
  deflate::adler32,
  payload::{choose_payload, decode_payload},
  preflate::{build_preflate_data, parse_preflate_data, parse_preflate_output, preflate_analyze, preflate_reencode},
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
  let mut offset = 8;
  let mut idat_chunks = Vec::new();
  let mut idat_lengths = Vec::new();
  let mut idat_crcs = Vec::new();
  let mut first_idat_start = None;
  let mut last_idat_end = None;
  let mut ihdr = None;

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
    if chunk_type == b"IHDR"
      && len == 13
      && let Some(parsed) = PngIhdr::parse(&input[data_start..data_end])
    {
      ihdr = Some(parsed);
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
  if adler != u32::from_be_bytes([zlib_footer[0], zlib_footer[1], zlib_footer[2], zlib_footer[3]]) {
    return Ok(None);
  }

  let mut png_payload_kind = 0u8;
  let mut payload_bytes = plain;
  let mut webp_meta = None;

  if config.enable_png_webp
    && let Some(ihdr) = ihdr
    && ihdr.is_webp_compatible()
  {
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

pub(super) fn decode_png_segment(segment: &Pcf2Segment, config: &DecodeConfig, depth: u32, budget: &mut DecodeBudget) -> Result<Vec<u8>> {
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
  let zlib_header = &rest[..2];
  let zlib_footer = &rest[2..6];
  let mut offset = 6usize;
  let idat_count = read_varint_slice(rest, &mut offset)? as usize;
  let mut idat_lengths = Vec::with_capacity(idat_count);
  for _ in 0..idat_count {
    idat_lengths.push(read_varint_slice(rest, &mut offset)? as usize);
  }
  if offset >= rest.len() {
    return Err(Error::InvalidSegment("png meta truncated"));
  }
  let crc_present = rest[offset];
  offset += 1;
  let mut idat_crcs = Vec::new();
  if crc_present == 1 {
    let crc_bytes = idat_count * 4;
    if offset + crc_bytes > rest.len() {
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
    let width = read_varint_slice(rest, &mut offset)? as u32;
    let height = read_varint_slice(rest, &mut offset)? as u32;
    let height_usize = usize::try_from(height).map_err(|_| Error::InvalidSegment("png webp height"))?;
    if offset + height_usize > rest.len() {
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

  let (corrections, payload) = parse_preflate_data(&segment.data)?;
  let payload = decode_payload(payload_kind, payload, config, depth, budget)?;
  let plain = if png_payload_kind == 0 {
    payload
  } else {
    let meta = webp_meta.ok_or(Error::InvalidSegment("png webp meta"))?;
    let bitmap = decode_webp_bitmap(&payload, meta.width, meta.height, meta.color_type)?;
    let bpp = png_bytes_per_pixel(meta.color_type)?;
    apply_png_filters_with_types(&bitmap, meta.width, meta.height, bpp, &meta.filters)?
  };
  let idat_data = preflate_reencode(&corrections, &plain)?;
  let mut full_idat = Vec::with_capacity(idat_data.len() + 6);
  full_idat.extend_from_slice(zlib_header);
  full_idat.extend_from_slice(&idat_data);
  full_idat.extend_from_slice(zlib_footer);
  let total_len: usize = idat_lengths.iter().sum();
  if total_len != full_idat.len() {
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

#[derive(Clone, Copy, Debug)]
struct PngIhdr {
  width: u32,
  height: u32,
  bit_depth: u8,
  color_type: u8,
  compression: u8,
  filter: u8,
  interlace: u8,
}

impl PngIhdr {
  fn parse(data: &[u8]) -> Option<Self> {
    if data.len() != 13 {
      return None;
    }
    let width = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
    let height = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    Some(Self {
      width,
      height,
      bit_depth: data[8],
      color_type: data[9],
      compression: data[10],
      filter: data[11],
      interlace: data[12],
    })
  }

  fn is_webp_compatible(&self) -> bool {
    if self.width == 0 || self.height == 0 {
      return false;
    }
    if self.bit_depth != 8 {
      return false;
    }
    if self.compression != 0 || self.filter != 0 || self.interlace != 0 {
      return false;
    }
    matches!(self.color_type, 2 | 6)
  }

  fn bytes_per_pixel(&self) -> Result<usize> {
    png_bytes_per_pixel(self.color_type)
  }
}

#[derive(Clone, Debug)]
struct PngWebpMeta {
  color_type: u8,
  width: u32,
  height: u32,
  filters: Vec<u8>,
}

fn png_bytes_per_pixel(color_type: u8) -> Result<usize> {
  match color_type {
    2 => Ok(3),
    6 => Ok(4),
    _ => Err(Error::InvalidSegment("png color_type")),
  }
}

fn png_row_bytes(width: u32, bpp: usize) -> Result<usize> {
  let width = usize::try_from(width).map_err(|_| Error::InvalidSegment("png width"))?;
  width.checked_mul(bpp).ok_or(Error::InvalidSegment("png row overflow"))
}

fn undo_png_filters(filtered: &[u8], width: u32, height: u32, bpp: usize) -> Result<(Vec<u8>, Vec<u8>)> {
  let row_bytes = png_row_bytes(width, bpp)?;
  let height_usize = usize::try_from(height).map_err(|_| Error::InvalidSegment("png height"))?;
  let expected = row_bytes
    .checked_add(1)
    .and_then(|v| v.checked_mul(height_usize))
    .ok_or(Error::InvalidSegment("png size overflow"))?;
  if filtered.len() != expected {
    return Err(Error::InvalidSegment("png scanline size"));
  }

  let mut out = vec![0u8; row_bytes * height_usize];
  let mut filters = Vec::with_capacity(height_usize);

  for row in 0..height_usize {
    let row_start = row * (row_bytes + 1);
    let filter = filtered[row_start];
    filters.push(filter);
    let row_in = &filtered[row_start + 1..row_start + 1 + row_bytes];
    let (prev_rows, current_rows) = out.split_at_mut(row * row_bytes);
    let row_out = &mut current_rows[..row_bytes];
    let prev_row = if row > 0 {
      &prev_rows[(row - 1) * row_bytes..row * row_bytes]
    } else {
      &[]
    };

    for col in 0..row_bytes {
      let left = if col >= bpp { row_out[col - bpp] } else { 0 };
      let up = if row > 0 { prev_row[col] } else { 0 };
      let up_left = if row > 0 && col >= bpp { prev_row[col - bpp] } else { 0 };
      row_out[col] = match filter {
        0 => row_in[col],
        1 => row_in[col].wrapping_add(left),
        2 => row_in[col].wrapping_add(up),
        3 => row_in[col].wrapping_add(((left as u16 + up as u16) / 2) as u8),
        4 => row_in[col].wrapping_add(paeth_predictor(left, up, up_left)),
        _ => return Err(Error::InvalidSegment("png filter type")),
      };
    }
  }

  Ok((out, filters))
}

fn apply_png_filters_with_types(bitmap: &[u8], width: u32, height: u32, bpp: usize, filters: &[u8]) -> Result<Vec<u8>> {
  let row_bytes = png_row_bytes(width, bpp)?;
  let height_usize = usize::try_from(height).map_err(|_| Error::InvalidSegment("png height"))?;
  if filters.len() != height_usize {
    return Err(Error::InvalidSegment("png filter count"));
  }
  if bitmap.len() != row_bytes * height_usize {
    return Err(Error::InvalidSegment("png bitmap size"));
  }

  let mut out = vec![0u8; (row_bytes + 1) * height_usize];
  for row in 0..height_usize {
    let filter = filters[row];
    let row_start = row * (row_bytes + 1);
    out[row_start] = filter;
    let row_out = &mut out[row_start + 1..row_start + 1 + row_bytes];
    let row_in = &bitmap[row * row_bytes..(row + 1) * row_bytes];

    for col in 0..row_bytes {
      let left = if col >= bpp { row_in[col - bpp] } else { 0 };
      let up = if row > 0 { bitmap[(row - 1) * row_bytes + col] } else { 0 };
      let up_left = if row > 0 && col >= bpp {
        bitmap[(row - 1) * row_bytes + col - bpp]
      } else {
        0
      };
      let predicted = match filter {
        0 => 0,
        1 => left,
        2 => up,
        3 => ((left as u16 + up as u16) / 2) as u8,
        4 => paeth_predictor(left, up, up_left),
        _ => return Err(Error::InvalidSegment("png filter type")),
      };
      row_out[col] = row_in[col].wrapping_sub(predicted);
    }
  }

  Ok(out)
}

fn paeth_predictor(a: u8, b: u8, c: u8) -> u8 {
  let a = a as i32;
  let b = b as i32;
  let c = c as i32;
  let p = a + b - c;
  let pa = (p - a).abs();
  let pb = (p - b).abs();
  let pc = (p - c).abs();
  if pa <= pb && pa <= pc {
    a as u8
  } else if pb <= pc {
    b as u8
  } else {
    c as u8
  }
}

fn encode_webp_lossless(bitmap: &[u8], width: u32, height: u32, color_type: u8) -> Result<Vec<u8>> {
  let encoder = match color_type {
    2 => webp::Encoder::from_rgb(bitmap, width, height),
    6 => webp::Encoder::from_rgba(bitmap, width, height),
    _ => return Err(Error::InvalidSegment("png color_type")),
  };
  let mut config = webp::WebPConfig::new().map_err(|_| Error::Other("webp config".to_string()))?;
  config.lossless = 1;
  config.exact = 1;
  config.alpha_compression = 0;
  config.alpha_filtering = 0;
  config.quality = 100.0;
  let webp = encoder
    .encode_advanced(&config)
    .map_err(|_| Error::Other("webp encode failed".to_string()))?;
  Ok(webp.to_vec())
}

fn decode_webp_bitmap(webp_bytes: &[u8], width: u32, height: u32, color_type: u8) -> Result<Vec<u8>> {
  let decoded = webp::Decoder::new(webp_bytes)
    .decode()
    .ok_or_else(|| Error::Other("webp decode failed".to_string()))?;
  if decoded.width() != width || decoded.height() != height {
    return Err(Error::InvalidSegment("png webp size"));
  }
  let expected_layout = match color_type {
    2 => webp::PixelLayout::Rgb,
    6 => webp::PixelLayout::Rgba,
    _ => return Err(Error::InvalidSegment("png color_type")),
  };
  let data = &*decoded;
  match (expected_layout, decoded.layout()) {
    (webp::PixelLayout::Rgb, webp::PixelLayout::Rgb) => Ok(data.to_vec()),
    (webp::PixelLayout::Rgba, webp::PixelLayout::Rgba) => Ok(data.to_vec()),
    (webp::PixelLayout::Rgba, webp::PixelLayout::Rgb) => {
      let mut out = Vec::with_capacity((data.len() / 3) * 4);
      for chunk in data.chunks_exact(3) {
        out.extend_from_slice(chunk);
        out.push(255);
      }
      Ok(out)
    }
    (webp::PixelLayout::Rgb, webp::PixelLayout::Rgba) => {
      if data.chunks_exact(4).all(|px| px[3] == 255) {
        let mut out = Vec::with_capacity((data.len() / 4) * 3);
        for chunk in data.chunks_exact(4) {
          out.extend_from_slice(&chunk[..3]);
        }
        Ok(out)
      } else {
        Err(Error::InvalidSegment("png webp layout"))
      }
    }
  }
}
