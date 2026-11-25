use std::{
  borrow::Cow,
  io::{Cursor, Read, Write},
};

use memchr::memmem;
use tokio::io::{AsyncRead, AsyncReadExt};

use super::{DecodeConfig, budget::DecodeBudget};
use crate::{
  error::{Error, Result},
  pcf2::{PCF2_MAGIC, Pcf2Decoder, Pcf2Segment, SegmentKind},
};

/// Small framing slack allowed on segment metadata/data sizes above
/// `DecodeConfig::max_total_output` so tiny valid files (e.g. a 68-byte PNG
/// whose PCF2 wrapper is ~143 bytes) still decode under exact output budgets.
const SEGMENT_INPUT_SLACK: u64 = 64 * 1024;
const STREAM_COPY_BUF_LEN: usize = 64 * 1024;

pub(super) fn segment_input_limit(config: &DecodeConfig) -> u64 {
  config.max_total_output.saturating_add(SEGMENT_INPUT_SLACK)
}

pub(super) async fn read_varint_async<R: AsyncRead + Unpin>(reader: &mut R) -> Result<u64> {
  let mut value = 0u64;
  let mut shift = 0u32;
  for _ in 0..10 {
    let mut buf = [0u8; 1];
    reader.read_exact(&mut buf).await?;
    let byte = buf[0];
    let part = (byte & 0x7f) as u64;
    if shift > 63 || (shift == 63 && part > 1) {
      return Err(Error::VarintOverflow);
    }
    value |= part << shift;
    if (byte & 0x80) == 0 {
      return Ok(value);
    }
    shift += 7;
  }
  Err(Error::VarintTooLong)
}

pub(super) fn decode_bytes_recover(input: &[u8], config: &DecodeConfig) -> Result<Vec<u8>> {
  let mut out = Vec::new();
  let mut offset = 0;
  let mut budget = DecodeBudget::default();
  let config = DecodeConfig {
    recover: false,
    ..config.clone()
  };
  while let Some(pos) = memmem::find(&input[offset..], PCF2_MAGIC) {
    let start = offset + pos;
    match decode_stream_with_len(&input[start..], &config, 0, &mut budget) {
      Ok((decoded, consumed)) => {
        out.extend_from_slice(&decoded);
        offset = start + consumed.max(1);
      }
      Err(_) => {
        offset = start + 1;
      }
    }
  }
  if out.is_empty() {
    return Err(Error::InvalidHeader("no recoverable PCF2 container"));
  }
  Ok(out)
}

pub(super) fn decode_stream<R: Read>(reader: R, config: &DecodeConfig, depth: u32, budget: &mut DecodeBudget) -> Result<Vec<u8>> {
  let mut out = Vec::new();
  decode_stream_to_writer(reader, &mut out, config, depth, budget)?;
  Ok(out)
}

pub(super) fn decode_stream_to_writer<R: Read, W: Write>(
  reader: R,
  writer: W,
  config: &DecodeConfig,
  depth: u32,
  budget: &mut DecodeBudget,
) -> Result<u64> {
  decode_segments_to_writer(&mut Pcf2Decoder::new(reader)?, writer, config, depth, budget)
}

pub(super) fn decode_stream_with_len(
  input: &[u8],
  config: &DecodeConfig,
  depth: u32,
  budget: &mut DecodeBudget,
) -> Result<(Vec<u8>, usize)> {
  let mut cursor = Cursor::new(input);
  let mut out = Vec::new();
  decode_segments_to_writer(&mut Pcf2Decoder::new(&mut cursor)?, &mut out, config, depth, budget)?;
  Ok((out, cursor.position() as usize))
}

/// Decodes segments directly into `writer` until the header's `original_size`
/// is reached, streaming `Lit` segments in fixed chunks without buffering the
/// full container or decoded output in memory.
fn decode_segments_to_writer<R: Read, W: Write>(
  decoder: &mut Pcf2Decoder<R>,
  mut writer: W,
  config: &DecodeConfig,
  depth: u32,
  budget: &mut DecodeBudget,
) -> Result<u64> {
  let original_size = decoder.header().original_size;
  budget.check(original_size, config)?;
  let seg_cap = segment_input_limit(config);
  let mut total = 0u64;
  let mut copy_buf = [0u8; STREAM_COPY_BUF_LEN];

  while total < original_size {
    let Some(mut seg_reader) = decoder.next_segment_bounded(seg_cap)? else {
      return Err(Error::SizeMismatch {
        expected: original_size,
        actual: total,
      });
    };
    let orig_len = seg_reader.orig_len;
    let next_total = total.saturating_add(orig_len);
    if next_total > original_size {
      seg_reader.abandon();
      return Err(Error::SizeMismatch {
        expected: original_size,
        actual: next_total,
      });
    }
    if let Err(err) = budget.check(orig_len, config) {
      seg_reader.abandon();
      return Err(err);
    }

    if seg_reader.kind == SegmentKind::Lit as u8 {
      let mut copied = 0u64;
      loop {
        let read = match seg_reader.data_reader().read(&mut copy_buf) {
          Ok(0) => break,
          Ok(n) => n,
          Err(err) => {
            seg_reader.abandon();
            return Err(err.into());
          }
        };
        if let Err(err) = budget.consume(read, config) {
          seg_reader.abandon();
          return Err(err);
        }
        if let Err(err) = writer.write_all(&copy_buf[..read]) {
          seg_reader.abandon();
          return Err(err.into());
        }
        copied += read as u64;
      }
      if copied != orig_len {
        return Err(Error::SizeMismatch {
          expected: orig_len,
          actual: copied,
        });
      }
    } else {
      let segment = seg_reader.into_segment()?;
      let decoded = decode_segment_checked(&segment, config, depth, budget)?;
      writer.write_all(&decoded)?;
    }

    total = next_total;
  }

  Ok(total)
}

/// Decodes one segment and verifies that it restores exactly `orig_len`
/// bytes, so a forged segment cannot shift the bytes that follow it.
pub(super) fn decode_segment_checked<'a>(
  segment: &'a Pcf2Segment,
  config: &DecodeConfig,
  depth: u32,
  budget: &mut DecodeBudget,
) -> Result<Cow<'a, [u8]>> {
  budget.check(segment.orig_len, config)?;
  let data = super::decode_segment(segment, config, depth, budget)?;
  if data.len() as u64 != segment.orig_len {
    return Err(Error::SizeMismatch {
      expected: segment.orig_len,
      actual: data.len() as u64,
    });
  }
  Ok(data)
}
