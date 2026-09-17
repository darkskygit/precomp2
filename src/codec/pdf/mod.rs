//! Byte-preserving PDF stream preprocessing: dictionaries, xref offsets, old
//! revisions and bytes outside accepted stream ranges remain literal. No PDF
//! object reserialization is performed; each transformed stream is restored and
//! compared before acceptance.
//!
//! PdfStream (PCF2 v2, tag 0x0a) stores the innermost representation in `data`.
//! Metadata uses PCF2 unsigned varints (`v`) and length-prefixed blobs
//! (`blob`): `u8 version=0 | v step_count(1..65) | [u8 op | v original_len |
//! blob meta]*`. Steps follow PDF filter decode order and are restored in
//! reverse, checking each recorded length and the final segment `orig_len`.
//! Reconstruction budgets include intermediate output. Each Flate predictor
//! belongs to that layer, not to the filter chain as a whole.
//!
//! Unknown inner filters stop preprocessing; accepted outer steps can remain.
//! Unreliable or unsupported ranges stay literal. JPEG and image choices
//! include local metadata cost, but the caller must compare final compressed
//! file sizes.

use super::{
  DecodeBudget, DecodeConfig, EncodeBudget, EncodeConfig, Error, Pcf2Segment, Result, SegmentKind, adler32, decode_jpg_segment,
  decode_payload, decode_webp_bitmap, encode_jpg, encode_webp_lossless, parse_preflate_data, parse_preflate_output,
  preflate_analyze_limited, preflate_reencode, read_varint_slice, write_varint_vec,
};

mod ascii;
mod image;
mod legacy;
mod predictor;
mod syntax;
#[cfg(test)]
mod tests;

pub(super) use legacy::decode_pdf_flate_segment;
#[cfg(test)]
pub(super) use legacy::{PdfDecodeParms, encode_pdf_predictor};
use predictor::Predictor;
use syntax::{Value, whitespace};

#[derive(Clone, Debug, Default)]
pub struct PdfStats {
  pub streams: usize,
  pub transformed: usize,
  pub dct: usize,
  pub flate: usize,
  pub ascii: usize,
  pub images: usize,
  /// Streams left unchanged, including unfiltered streams; not an error count.
  pub fallback: usize,
}

const HEX: u8 = 0;
const A85: u8 = 1;
const FLATE: u8 = 2;
const PREDICTOR: u8 = 3;
// Reuses Jpg segment metadata and reversible JPEG data, not decoded RGB pixels.
const JPEG: u8 = 4;
const WEBP: u8 = 5;

struct Step {
  kind: u8,
  original_len: usize,
  meta: Vec<u8>,
}

pub(super) fn write_num(n: usize, out: &mut Vec<u8>) {
  write_varint_vec(n as u64, out);
}
pub(super) fn read_num(data: &[u8], pos: &mut usize) -> Result<usize> {
  usize::try_from(read_varint_slice(data, pos)?).map_err(|_| Error::LengthOverflow)
}
pub(super) fn write_blob(bytes: &[u8], out: &mut Vec<u8>) {
  write_num(bytes.len(), out);
  out.extend_from_slice(bytes);
}
pub(super) fn read_blob<'a>(data: &'a [u8], pos: &mut usize) -> Result<&'a [u8]> {
  let len = read_num(data, pos)?;
  let end = pos.checked_add(len).ok_or(Error::LengthOverflow)?;
  let bytes = data.get(*pos..end).ok_or(Error::InvalidSegment("pdf metadata bounds"))?;
  *pos = end;
  Ok(bytes)
}

pub(super) fn encode_pdf(input: &[u8], config: &EncodeConfig, depth: u32, budget: &mut EncodeBudget) -> Result<Option<Vec<Pcf2Segment>>> {
  encode_pdf_with_stats(input, config, depth, budget, &mut PdfStats::default())
}

pub(super) fn encode_pdf_with_stats(
  input: &[u8],
  config: &EncodeConfig,
  depth: u32,
  budget: &mut EncodeBudget,
  stats: &mut PdfStats,
) -> Result<Option<Vec<Pcf2Segment>>> {
  if !input.starts_with(b"%PDF") {
    return Ok(None);
  }
  let mut result = Vec::new();
  let mut cursor = 0;
  for stream in syntax::streams(input) {
    stats.streams += 1;
    let bytes = &input[stream.range.clone()];
    let before = budget.total_output;
    let encoded = encode_stream(bytes, &stream.dict, config, depth, budget);
    let segment = match encoded {
      Ok(Some(segment)) => {
        let decode_config = DecodeConfig {
          max_depth: config.max_depth,
          max_total_output: config.max_total_output,
          recover: false,
        };
        match decode_pdf_stream(&segment, &decode_config, depth, &mut DecodeBudget::default()) {
          Ok(restored) if restored == bytes => segment,
          _ => {
            budget.total_output = before;
            stats.fallback += 1;
            continue;
          }
        }
      }
      _ => {
        budget.total_output = before;
        stats.fallback += 1;
        continue;
      }
    };
    if cursor < stream.range.start {
      result.push(Pcf2Segment::lit(input[cursor..stream.range.start].to_vec()));
    }
    count_steps(&segment, stats)?;
    result.push(segment);
    stats.transformed += 1;
    cursor = stream.range.end;
  }
  if cursor < input.len() {
    result.push(Pcf2Segment::lit(input[cursor..].to_vec()));
  }
  Ok(Some(result))
}

fn filters(dict: &Value) -> Result<Vec<(Vec<u8>, Value)>> {
  let names = match dict.get(b"Filter") {
    None | Some(Value::Null) => Vec::new(),
    Some(Value::Name(name)) => vec![name.clone()],
    Some(Value::Array(items)) => items
      .iter()
      .map(|v| v.name().map(ToOwned::to_owned).ok_or(Error::InvalidSegment("pdf filter name")))
      .collect::<Result<Vec<_>>>()?,
    _ => return Err(Error::InvalidSegment("pdf filters")),
  };
  let params = match dict.get(b"DecodeParms") {
    None | Some(Value::Null) => vec![Value::Null; names.len()],
    Some(value @ Value::Dict(_)) if names.len() == 1 => vec![value.clone()],
    Some(Value::Array(items)) if items.len() == names.len() => items.clone(),
    _ => return Err(Error::InvalidSegment("pdf filter parameters")),
  };
  if params.iter().any(|p| !matches!(p, Value::Null | Value::Dict(_))) {
    return Err(Error::InvalidSegment("pdf filter parameter entry"));
  }
  Ok(names.into_iter().zip(params).collect())
}

fn encode_stream(input: &[u8], dict: &Value, config: &EncodeConfig, depth: u32, budget: &mut EncodeBudget) -> Result<Option<Pcf2Segment>> {
  let filters = filters(dict)?;
  if filters.len() > config.max_pdf_filters.min(32) as usize || depth > config.max_depth {
    return Ok(None);
  }
  let mut bytes = input.to_vec();
  let mut steps = Vec::new();
  let mut samples = true;
  for (name, params) in &filters {
    let original_len = bytes.len();
    let (next, kind, meta) = match name.as_slice() {
      b"ASCIIHexDecode" | b"AHx" if config.enable_pdf_ascii => {
        let (next, meta) = ascii::unpack(&bytes, true)?;
        (next, HEX, meta)
      }
      b"ASCII85Decode" | b"A85" if config.enable_pdf_ascii => {
        let (next, meta) = ascii::unpack(&bytes, false)?;
        (next, A85, meta)
      }
      b"FlateDecode" | b"Fl" => {
        let remaining = config.max_total_output.saturating_sub(budget.total_output);
        let limit = remaining
          .min((bytes.len() as f64 * config.max_expand_ratio) as u64)
          .min(usize::MAX as u64) as usize;
        let (next, meta) = unpack_flate(&bytes, limit)?;
        (next, FLATE, meta)
      }
      b"DCTDecode" | b"DCT" if config.enable_pdf_dct => {
        let Some(jpeg) = encode_jpg(&bytes)? else {
          break;
        };
        if jpeg.data.len() + jpeg.meta.len() >= bytes.len() {
          break;
        }
        samples = false;
        (jpeg.data, JPEG, jpeg.meta)
      }
      _ => {
        samples = false;
        break;
      }
    };
    budget.consume(next.len(), config)?;
    steps.push(Step { kind, original_len, meta });
    bytes = next;
    if kind == JPEG {
      samples = false;
      break;
    }
    if kind == FLATE {
      let predictor = Predictor::from_params(params)?;
      if predictor.kind != 1 {
        if !config.enable_pdf_predictor {
          samples = false;
          break;
        }
        let original_len = bytes.len();
        let (next, meta) = predictor.undo(&bytes)?;
        steps.push(Step {
          kind: PREDICTOR,
          original_len,
          meta,
        });
        bytes = next;
      }
    }
  }
  if samples
    && config.enable_pdf_images
    && let Some((encoded, meta)) = image::encode(&bytes, dict)?
    && encoded.len() + meta.len() < bytes.len().min(input.len())
  {
    steps.push(Step {
      kind: WEBP,
      original_len: bytes.len(),
      meta,
    });
    bytes = encoded;
  }
  if steps.is_empty() {
    return Ok(None);
  }
  let mut meta = vec![0];
  write_num(steps.len(), &mut meta);
  for step in steps {
    meta.push(step.kind);
    write_num(step.original_len, &mut meta);
    write_blob(&step.meta, &mut meta);
  }
  Ok(Some(Pcf2Segment {
    kind: SegmentKind::PdfStream as u8,
    flags: 0,
    orig_len: input.len() as u64,
    meta,
    data: bytes,
  }))
}

// Metadata: blob(original zlib header), blob(checksum + trailing bytes),
// blob(preflate corrections). Corrections apply before predictor removal.
fn unpack_flate(bytes: &[u8], limit: usize) -> Result<(Vec<u8>, Vec<u8>)> {
  if bytes.len() < 6
    || bytes[0] & 15 != 8
    || bytes[0] >> 4 > 7
    || bytes[1] & 32 != 0
    || !u16::from_be_bytes([bytes[0], bytes[1]]).is_multiple_of(31)
  {
    return Err(Error::InvalidSegment("pdf zlib header"));
  }
  let analysis = preflate_analyze_limited(&bytes[2..], limit)?;
  let (consumed, corrections, plain) = parse_preflate_output(&analysis)?;
  let end = usize::try_from(consumed)
    .ok()
    .and_then(|n| n.checked_add(2))
    .ok_or(Error::LengthOverflow)?;
  let checksum = bytes.get(end..end + 4).ok_or(Error::InvalidSegment("pdf zlib checksum"))?;
  if checksum != adler32(&plain).to_be_bytes() {
    return Err(Error::InvalidSegment("pdf zlib checksum"));
  }
  let mut meta = Vec::new();
  write_blob(&bytes[..2], &mut meta);
  write_blob(&bytes[end..], &mut meta);
  write_blob(&corrections, &mut meta);
  Ok((plain, meta))
}

pub(super) fn decode_pdf_stream(segment: &Pcf2Segment, config: &DecodeConfig, depth: u32, budget: &mut DecodeBudget) -> Result<Vec<u8>> {
  if depth > config.max_depth || segment.meta.first() != Some(&0) {
    return Err(Error::InvalidSegment("pdf stream version"));
  }
  let mut pos = 1;
  let count = read_num(&segment.meta, &mut pos)?;
  if count == 0 || count > 65 {
    return Err(Error::InvalidSegment("pdf step count"));
  }
  let mut steps = Vec::new();
  for _ in 0..count {
    let kind = *segment.meta.get(pos).ok_or(Error::InvalidSegment("pdf step"))?;
    pos += 1;
    let len = read_num(&segment.meta, &mut pos)?;
    let meta = read_blob(&segment.meta, &mut pos)?;
    steps.push((kind, len, meta));
  }
  if pos != segment.meta.len() {
    return Err(Error::InvalidSegment("pdf step trailing bytes"));
  }
  let mut bytes = segment.data.clone();
  for (kind, len, meta) in steps.into_iter().rev() {
    if (len as u64) > config.max_total_output.saturating_sub(budget.total_output) {
      return Err(Error::InvalidSegment("pdf output budget"));
    }
    let output = match kind {
      HEX | A85 => ascii::repack(&bytes, meta, kind == HEX, len)?,
      FLATE => {
        let mut pos = 0;
        let header = read_blob(meta, &mut pos)?;
        let tail = read_blob(meta, &mut pos)?;
        let corrections = read_blob(meta, &mut pos)?;
        if pos != meta.len() || header.len() != 2 || tail.len() < 4 {
          return Err(Error::InvalidSegment("pdf flate metadata"));
        }
        let data = preflate_reencode(corrections, &bytes)?;
        if header.len().saturating_add(data.len()).saturating_add(tail.len()) != len {
          return Err(Error::InvalidSegment("pdf flate length"));
        }
        let mut out = Vec::with_capacity(len);
        out.extend_from_slice(header);
        out.extend_from_slice(&data);
        out.extend_from_slice(tail);
        out
      }
      PREDICTOR => predictor::redo(&bytes, meta, len)?,
      JPEG => {
        let jpeg = Pcf2Segment {
          kind: SegmentKind::Jpg as u8,
          flags: 0,
          orig_len: len as u64,
          meta: meta.to_vec(),
          data: bytes,
        };
        decode_jpg_segment(&jpeg, &mut DecodeBudget::default(), config)?
      }
      WEBP => image::decode(&bytes, meta, len)?,
      _ => return Err(Error::InvalidSegment("pdf operation")),
    };
    if output.len() != len {
      return Err(Error::InvalidSegment("pdf reconstructed length"));
    }
    budget.consume(output.len(), config)?;
    bytes = output;
  }
  if bytes.len() as u64 != segment.orig_len {
    return Err(Error::InvalidSegment("pdf original length"));
  }
  Ok(bytes)
}

fn count_steps(segment: &Pcf2Segment, stats: &mut PdfStats) -> Result<()> {
  let mut pos = 1;
  let count = read_num(&segment.meta, &mut pos)?;
  for _ in 0..count {
    let kind = segment.meta[pos];
    pos += 1;
    read_num(&segment.meta, &mut pos)?;
    read_blob(&segment.meta, &mut pos)?;
    match kind {
      HEX | A85 => stats.ascii += 1,
      FLATE => stats.flate += 1,
      JPEG => stats.dct += 1,
      WEBP => stats.images += 1,
      _ => (),
    }
  }
  Ok(())
}
