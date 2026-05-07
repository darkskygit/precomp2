use memchr::memmem;
use nom::{
  IResult,
  bytes::complete::tag,
  error::{Error as NomError, ErrorKind},
};

use super::{
  DecodeConfig, EncodeConfig,
  budget::{DecodeBudget, EncodeBudget},
  deflate::{adler32, encode_deflate, encode_zlib},
  payload::{choose_payload, decode_payload},
  preflate::{build_preflate_data, parse_preflate_data, parse_preflate_output, preflate_analyze, preflate_reencode},
  varint::{read_varint_slice, write_varint_vec},
};
use crate::{
  error::{Error, Result},
  pcf2::{Pcf2Segment, SegmentKind},
};

pub(super) fn encode_pdf(input: &[u8], config: &EncodeConfig, depth: u32, budget: &mut EncodeBudget) -> Result<Option<Vec<Pcf2Segment>>> {
  if !input.starts_with(b"%PDF") {
    return Ok(None);
  }
  let mut segments = Vec::new();
  let mut cursor = 0usize;
  let mut pos = 0usize;

  while let Some(idx) = memmem::find(&input[pos..], b"stream") {
    let stream_pos = pos + idx;
    let mut data_start = stream_pos + 6;
    if data_start < input.len() && input[data_start] == b'\r' && input.get(data_start + 1) == Some(&b'\n') {
      data_start += 2;
    } else if data_start < input.len() && input[data_start] == b'\n' {
      data_start += 1;
    }
    let end_idx = match memmem::find(&input[data_start..], b"endstream") {
      Some(v) => data_start + v,
      None => break,
    };
    let data_end = end_idx;
    if data_end <= data_start || data_end > input.len() {
      break;
    }

    if cursor < data_start {
      segments.push(Pcf2Segment::lit(input[cursor..data_start].to_vec()));
    }

    let data = &input[data_start..data_end];
    let mut handled = false;
    if let Some(info) = parse_pdf_stream_info(input, stream_pos)
      && let Some(segment) = encode_pdf_flate_stream(data, &info, config, depth, budget)?
    {
      segments.push(segment);
      handled = true;
    }
    if !handled {
      if let Some(segment) = encode_zlib(data, config, depth, budget)? {
        segments.push(segment);
      } else if let Some(segment) = encode_deflate(data, config, depth, budget)? {
        segments.push(segment);
      } else {
        segments.push(Pcf2Segment::lit(data.to_vec()));
      }
    }
    cursor = data_end;
    pos = data_end;
  }

  if segments.is_empty() {
    return Ok(None);
  }

  if cursor < input.len() {
    segments.push(Pcf2Segment::lit(input[cursor..].to_vec()));
  }

  Ok(Some(segments))
}

#[derive(Clone, Debug)]
struct PdfStreamInfo {
  decode_parms: Option<PdfDecodeParms>,
  width: Option<u32>,
  height: Option<u32>,
  bpc: Option<u8>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct PdfDecodeParms {
  pub(super) predictor: u8,
  pub(super) colors: u8,
  pub(super) bpc: u8,
  pub(super) columns: u32,
}

#[derive(Clone, Copy, Debug)]
enum PdfCompression {
  Deflate,
  Zlib { header: [u8; 2], footer: [u8; 4] },
}

#[derive(Clone, Debug)]
struct PdfFlateAnalysis {
  compression: PdfCompression,
  corrections: Vec<u8>,
  plain: Vec<u8>,
}

#[derive(Clone, Copy, Debug)]
struct PdfBmpMeta {
  mode: u8,
  width: u32,
  height: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PdfToken {
  Name(String),
  Number(i64),
  Array(Vec<PdfToken>),
  Dict(Vec<(String, PdfToken)>),
  Bool,
  Null,
  Other,
}

type NomResult<'a, O> = IResult<&'a [u8], O, NomError<&'a [u8]>>;

fn nom_err(input: &[u8], kind: ErrorKind) -> nom::Err<NomError<&[u8]>> {
  nom::Err::Error(NomError::new(input, kind))
}

fn is_pdf_ws(b: u8) -> bool {
  matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c | 0x00)
}

fn is_pdf_delim(b: u8) -> bool {
  matches!(b, b'/' | b'<' | b'>' | b'[' | b']' | b'(' | b')')
}

fn nom_skip_ws(mut input: &[u8]) -> NomResult<'_, ()> {
  loop {
    let mut advanced = false;
    while let Some(&b) = input.first() {
      if is_pdf_ws(b) {
        input = &input[1..];
        advanced = true;
      } else {
        break;
      }
    }
    if input.first() == Some(&b'%') {
      input = &input[1..];
      while let Some(&b) = input.first() {
        if b == b'\n' || b == b'\r' {
          break;
        }
        input = &input[1..];
      }
      continue;
    }
    if !advanced {
      break;
    }
  }
  Ok((input, ()))
}

fn hex_value(b: u8) -> Option<u8> {
  match b {
    b'0'..=b'9' => Some(b - b'0'),
    b'a'..=b'f' => Some(10 + b - b'a'),
    b'A'..=b'F' => Some(10 + b - b'A'),
    _ => None,
  }
}

fn parse_name_nom(input: &[u8]) -> NomResult<'_, String> {
  let (input, _) = tag(b"/")(input)?;
  let mut idx = 0usize;
  let mut out = Vec::new();
  while idx < input.len() {
    let b = input[idx];
    if is_pdf_ws(b) || is_pdf_delim(b) {
      break;
    }
    if b == b'#'
      && idx + 2 < input.len()
      && let (Some(h1), Some(h2)) = (hex_value(input[idx + 1]), hex_value(input[idx + 2]))
    {
      out.push((h1 << 4) | h2);
      idx += 3;
      continue;
    }
    out.push(b);
    idx += 1;
  }
  Ok((&input[idx..], String::from_utf8_lossy(&out).into_owned()))
}

fn parse_number_nom(input: &[u8]) -> NomResult<'_, i64> {
  if input.is_empty() {
    return Err(nom_err(input, ErrorKind::Eof));
  }
  let mut idx = 0usize;
  let mut sign = 1i64;
  if input[idx] == b'-' {
    sign = -1;
    idx += 1;
  } else if input[idx] == b'+' {
    idx += 1;
  }
  let mut value = 0i64;
  let mut saw_digit = false;
  while idx < input.len() {
    let b = input[idx];
    if b.is_ascii_digit() {
      saw_digit = true;
      value = value.saturating_mul(10).saturating_add((b - b'0') as i64);
      idx += 1;
    } else {
      break;
    }
  }
  if idx < input.len() && input[idx] == b'.' {
    idx += 1;
    while idx < input.len() && input[idx].is_ascii_digit() {
      idx += 1;
    }
  }
  if !saw_digit {
    return Err(nom_err(input, ErrorKind::Digit));
  }
  Ok((&input[idx..], sign * value))
}

fn parse_literal_string_nom(input: &[u8]) -> NomResult<'_, PdfToken> {
  if input.first() != Some(&b'(') {
    return Err(nom_err(input, ErrorKind::Tag));
  }
  let mut idx = 1usize;
  let mut depth = 1u32;
  while idx < input.len() && depth > 0 {
    match input[idx] {
      b'\\' => {
        idx += 1;
        if idx < input.len() {
          idx += 1;
        }
      }
      b'(' => {
        depth += 1;
        idx += 1;
      }
      b')' => {
        depth -= 1;
        idx += 1;
      }
      _ => idx += 1,
    }
  }
  if depth != 0 {
    return Err(nom_err(input, ErrorKind::Tag));
  }
  Ok((&input[idx..], PdfToken::Other))
}

fn parse_hex_string_nom(input: &[u8]) -> NomResult<'_, PdfToken> {
  if input.first() != Some(&b'<') || input.get(1) == Some(&b'<') {
    return Err(nom_err(input, ErrorKind::Tag));
  }
  let mut idx = 1usize;
  while idx < input.len() {
    if input[idx] == b'>' {
      idx += 1;
      return Ok((&input[idx..], PdfToken::Other));
    }
    idx += 1;
  }
  Err(nom_err(input, ErrorKind::Tag))
}

fn parse_array_nom(input: &[u8]) -> NomResult<'_, Vec<PdfToken>> {
  let (mut input, _) = tag(b"[")(input)?;
  let mut items = Vec::new();
  loop {
    let (rest, _) = nom_skip_ws(input)?;
    input = rest;
    if input.is_empty() {
      return Err(nom_err(input, ErrorKind::Eof));
    }
    if input[0] == b']' {
      input = &input[1..];
      break;
    }
    let (rest, token) = parse_token_nom(input)?;
    items.push(token);
    input = rest;
  }
  Ok((input, items))
}

fn parse_dict_nom(input: &[u8]) -> NomResult<'_, Vec<(String, PdfToken)>> {
  let (mut input, _) = tag(b"<<")(input)?;
  let mut items = Vec::new();
  loop {
    let (rest, _) = nom_skip_ws(input)?;
    input = rest;
    if input.is_empty() {
      return Err(nom_err(input, ErrorKind::Eof));
    }
    if let Ok((rest, _)) = tag::<_, _, NomError<&[u8]>>(b">>")(input) {
      input = rest;
      break;
    }
    let (rest, key) = parse_name_nom(input)?;
    let (rest, _) = nom_skip_ws(rest)?;
    let (rest, value) = parse_token_nom(rest)?;
    items.push((key, value));
    input = rest;
  }
  Ok((input, items))
}

fn parse_token_nom(input: &[u8]) -> NomResult<'_, PdfToken> {
  let (input, _) = nom_skip_ws(input)?;
  if input.is_empty() {
    return Err(nom_err(input, ErrorKind::Eof));
  }
  if input.starts_with(b"true") {
    return Ok((&input[4..], PdfToken::Bool));
  }
  if input.starts_with(b"false") {
    return Ok((&input[5..], PdfToken::Bool));
  }
  if input.starts_with(b"null") {
    return Ok((&input[4..], PdfToken::Null));
  }
  match input[0] {
    b'/' => parse_name_nom(input).map(|(rest, name)| (rest, PdfToken::Name(name))),
    b'[' => parse_array_nom(input).map(|(rest, items)| (rest, PdfToken::Array(items))),
    b'<' => {
      if input.get(1) == Some(&b'<') {
        parse_dict_nom(input).map(|(rest, dict)| (rest, PdfToken::Dict(dict)))
      } else {
        parse_hex_string_nom(input)
      }
    }
    b'(' => parse_literal_string_nom(input),
    b'+' | b'-' | b'0'..=b'9' => parse_number_nom(input).map(|(rest, value)| (rest, PdfToken::Number(value))),
    _ => Ok((&input[1..], PdfToken::Other)),
  }
}

fn parse_pdf_stream_info(input: &[u8], stream_pos: usize) -> Option<PdfStreamInfo> {
  let dict = parse_pdf_dict_before_stream(input, stream_pos)?;
  let filter_token = pdf_dict_get(&dict, "Filter");
  let filters = pdf_parse_filter_names(filter_token);
  if filters.len() != 1 {
    return None;
  }
  let filter = &filters[0];
  if filter != "FlateDecode" && filter != "Fl" {
    return None;
  }
  let width = pdf_dict_number(&dict, "Width").and_then(|v| u32::try_from(v).ok());
  let height = pdf_dict_number(&dict, "Height").and_then(|v| u32::try_from(v).ok());
  let bpc = pdf_dict_number(&dict, "BitsPerComponent").and_then(|v| u8::try_from(v).ok());
  let decode_parms = parse_pdf_decode_parms(pdf_dict_get(&dict, "DecodeParms"), width, bpc);
  Some(PdfStreamInfo {
    decode_parms,
    width,
    height,
    bpc,
  })
}

fn parse_pdf_dict_slice_nom(input: &[u8]) -> Option<Vec<(String, PdfToken)>> {
  let (rest, dict) = parse_dict_nom(input).ok()?;
  let (rest, _) = nom_skip_ws(rest).ok()?;
  if rest.is_empty() { Some(dict) } else { None }
}

fn parse_pdf_dict_before_stream(input: &[u8], stream_pos: usize) -> Option<Vec<(String, PdfToken)>> {
  if stream_pos < 2 {
    return None;
  }
  for idx in (2..=stream_pos).rev() {
    if &input[idx - 2..idx] != b"<<" {
      continue;
    }
    let start = idx - 2;
    let slice = &input[start..stream_pos];
    if let Some(dict) = parse_pdf_dict_slice_nom(slice) {
      return Some(dict);
    }
  }
  None
}

fn pdf_dict_get<'a>(dict: &'a [(String, PdfToken)], key: &str) -> Option<&'a PdfToken> {
  dict.iter().find_map(|(k, v)| if k == key { Some(v) } else { None })
}

fn pdf_dict_number(dict: &[(String, PdfToken)], key: &str) -> Option<i64> {
  match pdf_dict_get(dict, key) {
    Some(PdfToken::Number(value)) => Some(*value),
    _ => None,
  }
}

fn pdf_parse_filter_names(token: Option<&PdfToken>) -> Vec<String> {
  match token {
    Some(PdfToken::Name(name)) => vec![name.clone()],
    Some(PdfToken::Array(items)) => items
      .iter()
      .filter_map(|item| match item {
        PdfToken::Name(name) => Some(name.clone()),
        _ => None,
      })
      .collect(),
    _ => Vec::new(),
  }
}

fn parse_pdf_decode_parms(token: Option<&PdfToken>, width: Option<u32>, bpc: Option<u8>) -> Option<PdfDecodeParms> {
  let dict = match token {
    Some(PdfToken::Dict(entries)) => entries.as_slice(),
    Some(PdfToken::Array(entries)) => {
      if entries.len() == 1 {
        if let PdfToken::Dict(entries) = &entries[0] {
          entries.as_slice()
        } else {
          return None;
        }
      } else {
        return None;
      }
    }
    _ => return None,
  };
  let predictor = pdf_dict_number(dict, "Predictor").unwrap_or(1) as u8;
  let colors = pdf_dict_number(dict, "Colors").unwrap_or(1) as u8;
  let bpc_val = pdf_dict_number(dict, "BitsPerComponent")
    .or_else(|| bpc.map(|v| v as i64))
    .unwrap_or(8) as u8;
  let columns = pdf_dict_number(dict, "Columns").or_else(|| width.map(|v| v as i64))?;
  if columns <= 0 {
    return None;
  }
  Some(PdfDecodeParms {
    predictor,
    colors,
    bpc: bpc_val,
    columns: columns as u32,
  })
}

fn encode_pdf_flate_stream(
  data: &[u8],
  info: &PdfStreamInfo,
  config: &EncodeConfig,
  depth: u32,
  budget: &mut EncodeBudget,
) -> Result<Option<Pcf2Segment>> {
  let Some(analysis) = analyze_pdf_flate(data)? else {
    return Ok(None);
  };
  let PdfFlateAnalysis {
    compression,
    corrections,
    plain,
  } = analysis;

  let mut transform = 0u8;
  let mut transformed = plain;
  let mut predictor_meta = None;
  let mut bmp_meta = None;

  if config.enable_pdf_predictor
    && let Some(params) = info.decode_parms
    && (params.predictor == 1 || (10..=14).contains(&params.predictor))
    && let Ok(decoded) = decode_pdf_predictor(&transformed, &params)
  {
    transform = 1;
    transformed = decoded;
    predictor_meta = Some(params);
  }

  if transform == 0
    && config.enable_pdf_bmp_fallback
    && let Some((meta, bmp_bytes)) = pdf_bmp_transform(&transformed, info.width, info.height, info.bpc)?
  {
    transform = 2;
    bmp_meta = Some(meta);
    transformed = bmp_bytes;
  }

  let orig_len = data.len() as u64;
  let (payload_kind, payload) = choose_payload(transformed, data.len(), config, depth, budget)?;

  let mut meta = Vec::new();
  meta.push(0);
  match compression {
    PdfCompression::Deflate => meta.push(0),
    PdfCompression::Zlib { .. } => meta.push(1),
  }
  meta.push(payload_kind);
  meta.push(transform);

  if let PdfCompression::Zlib { header, footer } = compression {
    meta.extend_from_slice(&header);
    meta.extend_from_slice(&footer);
  }

  if transform == 1 {
    let params = predictor_meta.ok_or(Error::InvalidSegment("pdf predictor meta"))?;
    meta.push(params.predictor);
    meta.push(params.colors);
    meta.push(params.bpc);
    write_varint_vec(params.columns as u64, &mut meta);
  } else if transform == 2 {
    let meta_info = bmp_meta.ok_or(Error::InvalidSegment("pdf bmp meta"))?;
    meta.push(meta_info.mode);
    write_varint_vec(meta_info.width as u64, &mut meta);
    write_varint_vec(meta_info.height as u64, &mut meta);
  }

  let segment_data = build_preflate_data(&corrections, &payload);

  Ok(Some(Pcf2Segment {
    kind: SegmentKind::PdfFlate as u8,
    flags: 0,
    orig_len,
    meta,
    data: segment_data,
  }))
}

fn analyze_pdf_flate(data: &[u8]) -> Result<Option<PdfFlateAnalysis>> {
  if data.len() >= 6 {
    let cmf = data[0];
    let flg = data[1];
    if (cmf & 0x0f) == 8 && (cmf >> 4) <= 7 {
      let check = ((cmf as u16) << 8) | (flg as u16);
      if check.is_multiple_of(31) && (flg & 0x20) == 0 {
        let header = [cmf, flg];
        let footer = &data[data.len() - 4..];
        let deflate = &data[2..data.len() - 4];
        if let Ok(preflate) = preflate_analyze(deflate)
          && let Ok((deflate_len, corrections, plain)) = parse_preflate_output(&preflate)
          && deflate_len == deflate.len() as u64
        {
          let adler = adler32(&plain);
          let expected = u32::from_be_bytes([footer[0], footer[1], footer[2], footer[3]]);
          if adler == expected {
            return Ok(Some(PdfFlateAnalysis {
              compression: PdfCompression::Zlib {
                header,
                footer: [footer[0], footer[1], footer[2], footer[3]],
              },
              corrections,
              plain,
            }));
          }
        }
      }
    }
  }

  let preflate = match preflate_analyze(data) {
    Ok(v) => v,
    Err(_) => return Ok(None),
  };
  let (deflate_len, corrections, plain) = match parse_preflate_output(&preflate) {
    Ok(v) => v,
    Err(_) => return Ok(None),
  };
  if deflate_len != data.len() as u64 {
    return Ok(None);
  }
  Ok(Some(PdfFlateAnalysis {
    compression: PdfCompression::Deflate,
    corrections,
    plain,
  }))
}

pub(super) fn decode_pdf_flate_segment(
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

fn decode_pdf_predictor(data: &[u8], params: &PdfDecodeParms) -> Result<Vec<u8>> {
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
        _ => return Err(Error::InvalidSegment("pdf predictor filter")),
      };
    }
  }
  Ok(out)
}

pub(super) fn encode_pdf_predictor(data: &[u8], params: &PdfDecodeParms) -> Result<Vec<u8>> {
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

fn pdf_bmp_transform(plain: &[u8], width: Option<u32>, height: Option<u32>, bpc: Option<u8>) -> Result<Option<(PdfBmpMeta, Vec<u8>)>> {
  let Some(width) = width else {
    return Ok(None);
  };
  let Some(height) = height else {
    return Ok(None);
  };
  let bpc = bpc.unwrap_or(8);
  if bpc != 8 || width == 0 || height == 0 {
    return Ok(None);
  }
  let width_usize = usize::try_from(width).map_err(|_| Error::InvalidSegment("pdf bmp width"))?;
  let height_usize = usize::try_from(height).map_err(|_| Error::InvalidSegment("pdf bmp height"))?;
  let expected_8 = width_usize.checked_mul(height_usize).ok_or(Error::InvalidSegment("pdf bmp size"))?;
  let expected_24 = expected_8.checked_mul(3).ok_or(Error::InvalidSegment("pdf bmp size"))?;
  let (mode, payload) = if plain.len() == expected_8 {
    (1u8, build_pdf_bmp_payload(plain, width, height, 1)?)
  } else if plain.len() == expected_24 {
    (2u8, build_pdf_bmp_payload(plain, width, height, 2)?)
  } else {
    return Ok(None);
  };
  Ok(Some((PdfBmpMeta { mode, width, height }, payload)))
}

fn build_pdf_bmp_payload(plain: &[u8], width: u32, height: u32, mode: u8) -> Result<Vec<u8>> {
  let bytes_per_pixel = if mode == 1 { 1 } else { 3 };
  let row_bytes = width as usize * bytes_per_pixel;
  let padded_row_bytes = (row_bytes + 3) & !3;
  let height_usize = usize::try_from(height).map_err(|_| Error::InvalidSegment("pdf bmp height"))?;
  let data_size = padded_row_bytes
    .checked_mul(height_usize)
    .ok_or(Error::InvalidSegment("pdf bmp size"))?;
  let header_size = 14 + 40 + if mode == 1 { 1024 } else { 0 };
  let file_size = header_size + data_size;

  let mut out = Vec::with_capacity(file_size);
  out.extend_from_slice(b"BM");
  out.extend_from_slice(&(file_size as u32).to_le_bytes());
  out.extend_from_slice(&0u16.to_le_bytes());
  out.extend_from_slice(&0u16.to_le_bytes());
  out.extend_from_slice(&(header_size as u32).to_le_bytes());

  out.extend_from_slice(&40u32.to_le_bytes());
  out.extend_from_slice(&(width as i32).to_le_bytes());
  out.extend_from_slice(&(height as i32).to_le_bytes());
  out.extend_from_slice(&1u16.to_le_bytes());
  out.extend_from_slice(&(if mode == 1 { 8u16 } else { 24u16 }).to_le_bytes());
  out.extend_from_slice(&0u32.to_le_bytes());
  out.extend_from_slice(&(data_size as u32).to_le_bytes());
  out.extend_from_slice(&0u32.to_le_bytes());
  out.extend_from_slice(&0u32.to_le_bytes());
  out.extend_from_slice(&0u32.to_le_bytes());
  out.extend_from_slice(&0u32.to_le_bytes());

  if mode == 1 {
    out.extend(std::iter::repeat_n(0u8, 1024));
  }

  let row_padding = padded_row_bytes - row_bytes;
  let mut cursor = 0usize;
  for _ in 0..height_usize {
    out.extend_from_slice(&plain[cursor..cursor + row_bytes]);
    cursor += row_bytes;
    if row_padding > 0 {
      out.extend(std::iter::repeat_n(0u8, row_padding));
    }
  }

  Ok(out)
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

#[cfg(test)]
mod tests {
  use std::io::Write;

  use super::*;

  #[derive(Clone, Copy, Debug, PartialEq, Eq)]
  enum PdfSampleOutcome {
    Hit,
    NoDict,
    FilterMismatch,
  }

  #[derive(Clone, Debug)]
  struct PdfSample {
    name: &'static str,
    data: Vec<u8>,
    expected: PdfSampleOutcome,
  }

  #[derive(Default, Debug, PartialEq, Eq)]
  struct PdfStats {
    total: usize,
    hits: usize,
    no_dict: usize,
    filter_mismatch: usize,
  }

  fn zlib_bytes(payload: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(payload).expect("pdf zlib");
    encoder.finish().expect("pdf zlib finish")
  }

  fn build_pdf_with_compressed(dict: Option<&str>, compressed: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n1 0 obj\n");
    if let Some(dict) = dict {
      out.extend_from_slice(dict.as_bytes());
      out.extend_from_slice(b"\n");
    }
    out.extend_from_slice(b"stream\n");
    out.extend_from_slice(compressed);
    out.extend_from_slice(b"\nendstream\nendobj\n%%EOF\n");
    out
  }

  fn build_pdf(dict_body: &str, payload: &[u8]) -> Vec<u8> {
    let compressed = zlib_bytes(payload);
    let dict = format!("<< /Length {} {} >>", compressed.len(), dict_body);
    build_pdf_with_compressed(Some(&dict), &compressed)
  }

  fn build_pdf_no_dict(payload: &[u8]) -> Vec<u8> {
    let compressed = zlib_bytes(payload);
    build_pdf_with_compressed(None, &compressed)
  }

  fn find_stream_pos(input: &[u8]) -> usize {
    memmem::find(input, b"stream").expect("stream")
  }

  fn classify(dict: Option<Vec<(String, PdfToken)>>) -> PdfSampleOutcome {
    let Some(dict) = dict else {
      return PdfSampleOutcome::NoDict;
    };
    let filter_token = pdf_dict_get(&dict, "Filter");
    let filters = pdf_parse_filter_names(filter_token);
    if filters.len() == 1 && (filters[0] == "FlateDecode" || filters[0] == "Fl") {
      PdfSampleOutcome::Hit
    } else {
      PdfSampleOutcome::FilterMismatch
    }
  }

  fn compute_stats(samples: &[PdfSample]) -> PdfStats {
    let mut stats = PdfStats::default();
    for sample in samples {
      let stream_pos = find_stream_pos(&sample.data);
      let outcome = classify(parse_pdf_dict_before_stream(&sample.data, stream_pos));
      stats.total += 1;
      match outcome {
        PdfSampleOutcome::Hit => stats.hits += 1,
        PdfSampleOutcome::NoDict => stats.no_dict += 1,
        PdfSampleOutcome::FilterMismatch => stats.filter_mismatch += 1,
      }
    }
    stats
  }

  fn pdf_samples() -> Vec<PdfSample> {
    let payload = b"pdf stream payload";
    let compressed = zlib_bytes(payload);
    let raw_dict = format!("<< /Length {} /Filter /FlateDecode", compressed.len());
    vec![
      PdfSample {
        name: "simple_flate",
        data: build_pdf("/Filter /FlateDecode", payload),
        expected: PdfSampleOutcome::Hit,
      },
      PdfSample {
        name: "filter_array_single",
        data: build_pdf("/Filter [/FlateDecode]", payload),
        expected: PdfSampleOutcome::Hit,
      },
      PdfSample {
        name: "filter_array_multi",
        data: build_pdf("/Filter [/FlateDecode /ASCII85Decode]", payload),
        expected: PdfSampleOutcome::FilterMismatch,
      },
      PdfSample {
        name: "filter_lzw",
        data: build_pdf("/Filter /LZWDecode", payload),
        expected: PdfSampleOutcome::FilterMismatch,
      },
      PdfSample {
        name: "decode_parms_dict",
        data: build_pdf(
          "/Filter /FlateDecode /DecodeParms << /Predictor 12 /Colors 3 /BitsPerComponent 8 /Columns 4 >> /Width 4 /Height 2 \
           /BitsPerComponent 8",
          payload,
        ),
        expected: PdfSampleOutcome::Hit,
      },
      PdfSample {
        name: "decode_parms_array",
        data: build_pdf("/Filter /FlateDecode /DecodeParms [ << /Predictor 12 /Columns 4 >> ]", payload),
        expected: PdfSampleOutcome::Hit,
      },
      PdfSample {
        name: "escaped_names",
        data: build_pdf("/F#69lter /Flate#44ecode", payload),
        expected: PdfSampleOutcome::Hit,
      },
      PdfSample {
        name: "string_hex_comment",
        data: build_pdf(
          "/Filter /FlateDecode /Title (hello (nested) \\) world) /Foo <4a4b4c> % comment\n /Bar /Baz",
          payload,
        ),
        expected: PdfSampleOutcome::Hit,
      },
      PdfSample {
        name: "missing_dict",
        data: build_pdf_no_dict(payload),
        expected: PdfSampleOutcome::NoDict,
      },
      PdfSample {
        name: "unterminated_dict",
        data: build_pdf_with_compressed(Some(&raw_dict), &compressed),
        expected: PdfSampleOutcome::NoDict,
      },
    ]
  }

  #[test]
  fn pdf_nom_samples() {
    let samples = pdf_samples();
    for sample in &samples {
      let stream_pos = find_stream_pos(&sample.data);
      let outcome = classify(parse_pdf_dict_before_stream(&sample.data, stream_pos));
      assert_eq!(outcome, sample.expected, "outcome mismatch: {}", sample.name);
    }
  }

  #[test]
  fn pdf_nom_stats() {
    let samples = pdf_samples();
    let stats = compute_stats(&samples);
    assert_eq!(
      stats,
      PdfStats {
        total: 10,
        hits: 6,
        no_dict: 2,
        filter_mismatch: 2,
      }
    );
  }
}
