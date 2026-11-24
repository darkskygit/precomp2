use std::borrow::Cow;

use super::{
  DecodeBudget, DecodeConfig, Error, Pcf2Segment, Result, decode_payload, parse_preflate_data, preflate_reencode, read_varint_slice,
};
use crate::codec::scanline::filter_row;

#[derive(Clone, Copy)]
pub(crate) struct PdfDecodeParms {
  pub predictor: u8,
  pub colors: u8,
  pub bpc: u8,
  pub columns: u32,
}
struct PdfBmpMeta {
  mode: u8,
  width: u32,
  height: u32,
}

pub(crate) fn decode_pdf_flate_segment(
  segment: &Pcf2Segment,
  config: &DecodeConfig,
  depth: u32,
  budget: &mut DecodeBudget,
) -> Result<Vec<u8>> {
  if segment.meta.len() < 4 {
    return Err(Error::InvalidSegment("pdf meta"));
  }
  let meta_version = segment.meta[0];
  if meta_version != 0 {
    return Err(Error::InvalidSegment("pdf meta_version"));
  }
  let compression = segment.meta[1];
  let payload_kind = segment.meta[2];
  let transform = segment.meta[3];
  let mut offset = 4usize;

  let (header, footer) = if compression == 1 {
    if offset + 6 > segment.meta.len() {
      return Err(Error::InvalidSegment("pdf zlib meta"));
    }
    let header = [segment.meta[offset], segment.meta[offset + 1]];
    let footer = [
      segment.meta[offset + 2],
      segment.meta[offset + 3],
      segment.meta[offset + 4],
      segment.meta[offset + 5],
    ];
    offset += 6;
    (Some(header), Some(footer))
  } else if compression == 0 {
    (None, None)
  } else {
    return Err(Error::InvalidSegment("pdf compression"));
  };

  let predictor_meta = if transform == 1 {
    if offset + 3 > segment.meta.len() {
      return Err(Error::InvalidSegment("pdf predictor meta"));
    }
    let predictor = segment.meta[offset];
    let colors = segment.meta[offset + 1];
    let bpc = segment.meta[offset + 2];
    offset += 3;
    let columns = read_varint_slice(&segment.meta, &mut offset)? as u32;
    Some(PdfDecodeParms {
      predictor,
      colors,
      bpc,
      columns,
    })
  } else {
    None
  };

  let bmp_meta = if transform == 2 {
    if offset >= segment.meta.len() {
      return Err(Error::InvalidSegment("pdf bmp meta"));
    }
    let mode = segment.meta[offset];
    offset += 1;
    let width = read_varint_slice(&segment.meta, &mut offset)? as u32;
    let height = read_varint_slice(&segment.meta, &mut offset)? as u32;
    Some(PdfBmpMeta { mode, width, height })
  } else {
    None
  };

  if offset != segment.meta.len() {
    return Err(Error::InvalidSegment("pdf meta trailing"));
  }

  let (corrections, payload) = parse_preflate_data(&segment.data)?;
  let payload = decode_payload(payload_kind, payload, config, depth, budget)?;
  let plain = match transform {
    0 => payload,
    1 => {
      let params = predictor_meta.ok_or(Error::InvalidSegment("pdf predictor meta"))?;
      Cow::Owned(encode_pdf_predictor(&payload, &params)?)
    }
    2 => {
      let meta = bmp_meta.ok_or(Error::InvalidSegment("pdf bmp meta"))?;
      Cow::Owned(decode_pdf_bmp_payload(&payload, meta)?)
    }
    _ => return Err(Error::InvalidSegment("pdf transform")),
  };

  let deflate = preflate_reencode(corrections, &plain)?;
  let mut out = Vec::new();
  if let (Some(header), Some(footer)) = (header, footer) {
    out.extend_from_slice(&header);
    out.extend_from_slice(&deflate);
    out.extend_from_slice(&footer);
  } else {
    out.extend_from_slice(&deflate);
  }
  budget.consume(out.len(), config)?;
  Ok(out)
}

fn pdf_predictor_bpp(params: &PdfDecodeParms) -> Result<usize> {
  if params.colors == 0 || params.bpc == 0 || params.columns == 0 {
    return Err(Error::InvalidSegment("pdf predictor params"));
  }
  if !params.bpc.is_multiple_of(8) {
    return Err(Error::InvalidSegment("pdf predictor bpc"));
  }
  let bytes_per_sample = (params.bpc / 8) as usize;
  let colors = params.colors as usize;
  colors
    .checked_mul(bytes_per_sample)
    .ok_or(Error::InvalidSegment("pdf predictor bpp"))
}

fn pdf_predictor_row_bytes(params: &PdfDecodeParms) -> Result<usize> {
  let bpp = pdf_predictor_bpp(params)?;
  let columns = usize::try_from(params.columns).map_err(|_| Error::InvalidSegment("pdf predictor columns"))?;
  columns.checked_mul(bpp).ok_or(Error::InvalidSegment("pdf predictor row"))
}

pub(crate) fn encode_pdf_predictor(data: &[u8], params: &PdfDecodeParms) -> Result<Vec<u8>> {
  if params.predictor == 1 {
    return Ok(data.to_vec());
  }
  if !(10..=14).contains(&params.predictor) {
    return Err(Error::InvalidSegment("pdf predictor"));
  }
  let filter = params.predictor - 10;
  let bpp = pdf_predictor_bpp(params)?;
  let row_bytes = pdf_predictor_row_bytes(params)?;
  if row_bytes == 0 || !data.len().is_multiple_of(row_bytes) {
    return Err(Error::InvalidSegment("pdf predictor size"));
  }
  let mut out = vec![0u8; data.len()];
  let zeros = vec![0; row_bytes.min(data.len())];
  let mut prev = zeros.as_slice();
  for (row, filtered) in data.chunks_exact(row_bytes).zip(out.chunks_exact_mut(row_bytes)) {
    filter_row(filter, row, prev, bpp, filtered)?;
    prev = row;
  }
  Ok(out)
}

fn decode_pdf_bmp_payload(payload: &[u8], meta: PdfBmpMeta) -> Result<Vec<u8>> {
  let bytes_per_pixel = match meta.mode {
    1 => 1,
    2 => 3,
    _ => return Err(Error::InvalidSegment("pdf bmp mode")),
  };
  let header_size = 14 + 40 + if meta.mode == 1 { 1024 } else { 0 };
  if payload.len() < header_size {
    return Err(Error::InvalidSegment("pdf bmp payload"));
  }
  let width = usize::try_from(meta.width).map_err(|_| Error::InvalidSegment("pdf bmp width"))?;
  let height = usize::try_from(meta.height).map_err(|_| Error::InvalidSegment("pdf bmp height"))?;
  let row_bytes = width.checked_mul(bytes_per_pixel).ok_or(Error::InvalidSegment("pdf bmp size"))?;
  // A zero-width or zero-height bitmap has no samples; return before walking
  // up to `u32::MAX` empty rows.
  if row_bytes == 0 || height == 0 {
    return Ok(Vec::new());
  }
  let padded_row_bytes = row_bytes.checked_next_multiple_of(4).ok_or(Error::InvalidSegment("pdf bmp size"))?;
  let data_size = padded_row_bytes.checked_mul(height).ok_or(Error::InvalidSegment("pdf bmp size"))?;
  let pixels = &payload[header_size..];
  if pixels.len() < data_size {
    return Err(Error::InvalidSegment("pdf bmp payload"));
  }
  let mut out = Vec::with_capacity(row_bytes * height);
  for row in pixels.chunks_exact(padded_row_bytes).take(height) {
    out.extend_from_slice(&row[..row_bytes]);
  }
  Ok(out)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn bmp_payload_strips_row_padding_and_rejects_impossible_sizes() {
    let header = 14 + 40;
    let mut two_rows = vec![0u8; header];
    two_rows.extend_from_slice(&[1, 2, 3, 4, 5, 6, 0, 0, 7, 8, 9, 10, 11, 12, 0, 0]);
    let cases: [(u32, u32, Option<&[u8]>); 5] = [
      (2, 2, Some(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12])),
      // Empty bitmaps return at once instead of walking u32::MAX empty rows.
      (0, u32::MAX, Some(&[])),
      (1, 0, Some(&[])),
      (u32::MAX, u32::MAX, None),
      (2, 3, None),
    ];
    for (width, height, expected) in cases {
      let result = decode_pdf_bmp_payload(&two_rows, PdfBmpMeta { mode: 2, width, height });
      assert_eq!(result.ok().as_deref(), expected, "{width}x{height}");
    }
  }
}
