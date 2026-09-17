use super::{
  DecodeBudget, DecodeConfig, Error, Pcf2Segment, Result, decode_payload, parse_preflate_data, preflate_reencode, read_varint_slice,
};

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
      encode_pdf_predictor(&payload, &params)?
    }
    2 => {
      let meta = bmp_meta.ok_or(Error::InvalidSegment("pdf bmp meta"))?;
      decode_pdf_bmp_payload(&payload, meta)?
    }
    _ => return Err(Error::InvalidSegment("pdf transform")),
  };

  let deflate = preflate_reencode(&corrections, &plain)?;
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
  let rows = data.len() / row_bytes;
  let mut out = vec![0u8; rows * row_bytes];
  for row in 0..rows {
    let row_in = &data[row * row_bytes..(row + 1) * row_bytes];
    let row_out = &mut out[row * row_bytes..(row + 1) * row_bytes];
    for col in 0..row_bytes {
      let left = if col >= bpp { row_in[col - bpp] } else { 0 };
      let up = if row > 0 { data[(row - 1) * row_bytes + col] } else { 0 };
      let up_left = if row > 0 && col >= bpp {
        data[(row - 1) * row_bytes + col - bpp]
      } else {
        0
      };
      let predicted = match filter {
        0 => 0,
        1 => left,
        2 => up,
        3 => ((left as u16 + up as u16) / 2) as u8,
        4 => paeth_predictor(left, up, up_left),
        _ => return Err(Error::InvalidSegment("pdf predictor filter")),
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

fn decode_pdf_bmp_payload(payload: &[u8], meta: PdfBmpMeta) -> Result<Vec<u8>> {
  let bytes_per_pixel = if meta.mode == 1 {
    1
  } else if meta.mode == 2 {
    3
  } else {
    return Err(Error::InvalidSegment("pdf bmp mode"));
  };
  let header_size = 14 + 40 + if meta.mode == 1 { 1024 } else { 0 };
  if payload.len() < header_size {
    return Err(Error::InvalidSegment("pdf bmp payload"));
  }
  let row_bytes = meta.width as usize * bytes_per_pixel;
  let padded_row_bytes = (row_bytes + 3) & !3;
  let height_usize = usize::try_from(meta.height).map_err(|_| Error::InvalidSegment("pdf bmp height"))?;
  let data_size = padded_row_bytes
    .checked_mul(height_usize)
    .ok_or(Error::InvalidSegment("pdf bmp size"))?;
  if payload.len() < header_size + data_size {
    return Err(Error::InvalidSegment("pdf bmp payload"));
  }
  let mut out = Vec::with_capacity(row_bytes * height_usize);
  let mut cursor = header_size;
  for _ in 0..height_usize {
    out.extend_from_slice(&payload[cursor..cursor + row_bytes]);
    cursor += padded_row_bytes;
  }
  Ok(out)
}
