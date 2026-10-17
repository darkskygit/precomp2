use std::io::Cursor;

use ::base64::engine::general_purpose::STANDARD as BASE64_ENGINE;
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::{
  error::{Error, Result},
  pcf2::{PCF2_MAGIC, PCF2_VERSION, Pcf2File, Pcf2Header, Pcf2Segment, SegmentKind},
};

mod base64;
mod budget;
mod bzip2;
mod containers;
mod deflate;
pub mod lz4;
mod media;
mod payload;
mod pdf;
mod png;
mod preflate;
mod stream;
mod unityfs;
mod varint;

#[cfg(test)]
mod tests;

#[cfg(test)]
use std::io::Write;

#[cfg(test)]
use ::base64::Engine;
#[cfg(test)]
use ::bzip2::Compression;
#[cfg(test)]
use ::bzip2::write::BzEncoder;
use base64::{decode_base64_segment, encode_base64};
use budget::{DecodeBudget, EncodeBudget};
use bzip2::{decode_bzip2_segment, encode_bzip2};
use containers::{encode_swf, encode_zip};
#[cfg(test)]
use crc32fast::Hasher;
use deflate::{adler32, decode_deflate_segment, decode_gzip_segment, decode_zlib_segment, encode_deflate, encode_gzip, encode_zlib};
#[cfg(test)]
use media::jpeg_has_dht;
use media::{decode_jpg_segment, encode_jpg};
use payload::decode_payload;
pub use pdf::PdfStats;
#[cfg(test)]
use pdf::{PdfDecodeParms, encode_pdf_predictor};
use pdf::{decode_pdf_flate_segment, decode_pdf_stream, encode_pdf};
use png::{decode_png_segment, decode_webp_bitmap, encode_png, encode_webp_lossless};
#[cfg(test)]
use preflate::preflate_analyze;
use preflate::{parse_preflate_data, parse_preflate_output, preflate_analyze_limited, preflate_reencode};
use stream::read_varint_async;
pub use unityfs::{Lz4Region, UnityFsReport, analyze as analyze_unityfs};
use unityfs::{decode_unityfs_segment, encode_unityfs};
use varint::{read_varint_slice, write_varint_vec};

#[derive(Clone, Debug)]
pub struct EncodeConfig {
  pub max_depth: u32,
  pub max_expand_ratio: f64,
  pub max_total_output: u64,
  pub enable_png_webp: bool,
  /// Undo TIFF 2 and PNG 10–15 predictors. Off by default; the assetpack
  /// adapter enables this. Reconstruction retains PNG row tags and packed
  /// sample padding.
  pub enable_pdf_predictor: bool,
  pub enable_pdf_dct: bool,
  pub enable_pdf_ascii: bool,
  /// Reencode image samples and separate masks losslessly with WebP. Replaces
  /// the old BMP fallback encoder; legacy BMP payloads remain decodable.
  pub enable_pdf_images: bool,
  /// Maximum PDF filter-chain length, capped at 32 regardless of this value.
  pub max_pdf_filters: u32,
}

impl Default for EncodeConfig {
  fn default() -> Self {
    Self {
      max_depth: 4,
      max_expand_ratio: 16.0,
      max_total_output: 1 << 32,
      enable_png_webp: false,
      enable_pdf_predictor: false,
      enable_pdf_dct: true,
      enable_pdf_ascii: true,
      enable_pdf_images: true,
      max_pdf_filters: 16,
    }
  }
}

#[derive(Clone, Debug)]
pub struct DecodeConfig {
  pub max_depth: u32,
  pub max_total_output: u64,
  pub recover: bool,
}

impl Default for DecodeConfig {
  fn default() -> Self {
    Self {
      max_depth: 4,
      max_total_output: 1 << 32,
      recover: false,
    }
  }
}

pub fn encode_bytes(input: &[u8], config: &EncodeConfig) -> Result<Vec<u8>> {
  encode_bytes_with_stats(input, config).map(|(bytes, _)| bytes)
}

/// Returns PDF stream counters for top-level PDFs; other formats report zero
/// counters.
///
/// The intermediate stream may grow, especially after Flate expansion. Compare
/// candidates after outer compression, not by this output length alone.
pub fn encode_bytes_with_stats(input: &[u8], config: &EncodeConfig) -> Result<(Vec<u8>, PdfStats)> {
  let mut budget = EncodeBudget::default();
  let mut stats = PdfStats::default();
  let segments = if input.starts_with(b"%PDF") {
    pdf::encode_pdf_with_stats(input, config, 0, &mut budget, &mut stats)?.unwrap_or_default()
  } else {
    encode_segments(input, config, 0, &mut budget)?
  };
  let file = Pcf2File::from_segments(segments);
  let mut out = Vec::new();
  file.encode(&mut out)?;
  Ok((out, stats))
}

pub fn decode_bytes(input: &[u8], config: &DecodeConfig) -> Result<Vec<u8>> {
  if config.recover {
    return stream::decode_bytes_recover(input, config);
  }
  let mut budget = DecodeBudget::default();
  stream::decode_stream(Cursor::new(input), config, 0, &mut budget)
}

pub async fn decode_async<R: AsyncRead + Unpin>(mut reader: R, config: &DecodeConfig) -> Result<Vec<u8>> {
  let mut header_bytes = [0u8; 4 + 2 + 2 + 2 + 8];
  reader.read_exact(&mut header_bytes).await?;
  if &header_bytes[..4] != PCF2_MAGIC {
    return Err(Error::InvalidHeader("invalid magic"));
  }
  let version = u16::from_le_bytes([header_bytes[4], header_bytes[5]]);
  let flags = u16::from_le_bytes([header_bytes[6], header_bytes[7]]);
  let header_ext_len = u16::from_le_bytes([header_bytes[8], header_bytes[9]]);
  let original_size = u64::from_le_bytes([
    header_bytes[10],
    header_bytes[11],
    header_bytes[12],
    header_bytes[13],
    header_bytes[14],
    header_bytes[15],
    header_bytes[16],
    header_bytes[17],
  ]);
  if !(1..=PCF2_VERSION).contains(&version) {
    return Err(Error::InvalidHeader("unsupported version"));
  }
  if flags != 0 {
    return Err(Error::InvalidHeader("flags must be 0 for v1"));
  }
  if header_ext_len != 0 {
    return Err(Error::InvalidHeader("header_ext_len must be 0 for v1"));
  }
  if header_ext_len > 0 {
    let mut skip = vec![0u8; header_ext_len as usize];
    reader.read_exact(&mut skip).await?;
  }

  let header = Pcf2Header {
    version,
    flags,
    original_size,
    header_ext: Vec::new(),
  };

  let mut out = Vec::new();
  let mut remaining = header.original_size;
  let mut budget = DecodeBudget::default();

  loop {
    let mut kind_buf = [0u8; 1];
    let read = reader.read(&mut kind_buf).await?;
    if read == 0 {
      if remaining != 0 {
        return Err(Error::SizeMismatch {
          expected: header.original_size,
          actual: header.original_size - remaining,
        });
      }
      break;
    }
    let kind = kind_buf[0];
    if SegmentKind::from_u8(kind).is_some_and(|k| version < k.min_version()) {
      return Err(Error::InvalidSegment("segment kind requires v2"));
    }
    let mut flags_buf = [0u8; 1];
    reader.read_exact(&mut flags_buf).await?;
    let flags = flags_buf[0];
    if flags != 0 {
      return Err(Error::InvalidSegment("flags must be 0 for v1"));
    }

    let orig_len = read_varint_async(&mut reader).await?;
    let meta_len = read_varint_async(&mut reader).await?;
    let data_len = read_varint_async(&mut reader).await?;
    if orig_len > remaining {
      return Err(Error::InvalidSegment("orig_len exceeds original_size"));
    }
    remaining -= orig_len;

    let mut meta = Vec::new();
    (&mut reader).take(meta_len).read_to_end(&mut meta).await?;
    if meta.len() as u64 != meta_len {
      return Err(Error::SizeMismatch {
        expected: meta_len,
        actual: meta.len() as u64,
      });
    }
    let mut data = Vec::new();
    (&mut reader).take(data_len).read_to_end(&mut data).await?;
    if data.len() as u64 != data_len {
      return Err(Error::SizeMismatch {
        expected: data_len,
        actual: data.len() as u64,
      });
    }

    let segment = Pcf2Segment {
      kind,
      flags,
      orig_len,
      meta,
      data,
    };
    let decoded = decode_segment(&segment, config, 0, &mut budget)?;
    out.extend_from_slice(&decoded);
  }

  Ok(out)
}

#[derive(Clone, Debug)]
struct PenaltyEntry {
  pos: u32,
  value: u8,
}

fn encode_segments(input: &[u8], config: &EncodeConfig, depth: u32, budget: &mut EncodeBudget) -> Result<Vec<Pcf2Segment>> {
  if let Some(segments) = encode_unityfs(input, config, budget)? {
    return Ok(segments);
  }
  if let Some(segments) = encode_png(input, config, depth, budget)? {
    return Ok(segments);
  }
  if let Some(segment) = encode_gzip(input, config, depth, budget)? {
    return Ok(vec![segment]);
  }
  if let Some(segment) = encode_zlib(input, config, depth, budget)? {
    return Ok(vec![segment]);
  }
  if let Some(segment) = encode_deflate(input, config, depth, budget)? {
    return Ok(vec![segment]);
  }
  if let Some(segment) = encode_jpg(input)? {
    return Ok(vec![segment]);
  }
  if let Some(segment) = encode_base64(input, config, depth, budget)? {
    return Ok(vec![segment]);
  }
  if let Some(segment) = encode_bzip2(input, config, depth, budget)? {
    return Ok(vec![segment]);
  }
  if let Some(segments) = encode_zip(input, config, depth, budget)? {
    return Ok(segments);
  }
  if let Some(segments) = encode_pdf(input, config, depth, budget)? {
    return Ok(segments);
  }
  if let Some(segments) = encode_swf(input, config, depth, budget)? {
    return Ok(segments);
  }

  Ok(vec![Pcf2Segment::lit(input.to_vec())])
}

fn decode_segment(segment: &Pcf2Segment, config: &DecodeConfig, depth: u32, budget: &mut DecodeBudget) -> Result<Vec<u8>> {
  match segment.kind {
    k if k == SegmentKind::Lit as u8 => {
      budget.consume(segment.data.len(), config)?;
      Ok(segment.data.clone())
    }
    k if k == SegmentKind::Nested as u8 => {
      if depth >= config.max_depth {
        return Err(Error::Other("nested depth exceeded".to_string()));
      }
      let nested = stream::decode_stream(Cursor::new(&segment.data), config, depth + 1, budget)?;
      budget.consume(nested.len(), config)?;
      Ok(nested)
    }
    k if k == SegmentKind::Dflt as u8 => decode_deflate_segment(segment, config, depth, budget),
    k if k == SegmentKind::Zlib as u8 => decode_zlib_segment(segment, config, depth, budget),
    k if k == SegmentKind::Gzip as u8 => decode_gzip_segment(segment, config, depth, budget),
    k if k == SegmentKind::PngIdat as u8 => decode_png_segment(segment, config, depth, budget),
    k if k == SegmentKind::Jpg as u8 => decode_jpg_segment(segment, budget, config),
    k if k == SegmentKind::Base64 as u8 => decode_base64_segment(segment, config, depth, budget),
    k if k == SegmentKind::Bzip2 as u8 => decode_bzip2_segment(segment, config, depth, budget),
    k if k == SegmentKind::PdfFlate as u8 => decode_pdf_flate_segment(segment, config, depth, budget),
    k if k == SegmentKind::PdfStream as u8 => decode_pdf_stream(segment, config, depth, budget),
    k if k == SegmentKind::UnityFs as u8 => decode_unityfs_segment(segment, config, budget),
    _ => Err(Error::InvalidSegment("unknown segment kind")),
  }
}
