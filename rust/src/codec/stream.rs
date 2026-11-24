use std::io::{Cursor, Read};

use memchr::memmem;
use tokio::io::{AsyncRead, AsyncReadExt};

use super::{DecodeConfig, budget::DecodeBudget};
use crate::{
  error::{Error, Result},
  pcf2::{PCF2_MAGIC, Pcf2Decoder},
};

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
  let mut decoder = Pcf2Decoder::new(reader)?;
  let header = decoder.header().clone();
  let mut out = Vec::new();
  let mut total = 0u64;

  while total < header.original_size {
    let Some(segment) = decoder.next_segment()? else {
      return Err(Error::SizeMismatch {
        expected: header.original_size,
        actual: total,
      });
    };
    let segment = segment.into_segment()?;
    total = total.saturating_add(segment.orig_len);
    if total > header.original_size {
      return Err(Error::SizeMismatch {
        expected: header.original_size,
        actual: total,
      });
    }
    let data = super::decode_segment(&segment, config, depth, budget)?;
    out.extend_from_slice(&data);
  }

  Ok(out)
}

pub(super) fn decode_stream_with_len(
  input: &[u8],
  config: &DecodeConfig,
  depth: u32,
  budget: &mut DecodeBudget,
) -> Result<(Vec<u8>, usize)> {
  let mut cursor = Cursor::new(input);
  let mut decoder = Pcf2Decoder::new(&mut cursor)?;
  let header = decoder.header().clone();
  let mut out = Vec::new();
  let mut total = 0u64;

  while total < header.original_size {
    let Some(segment) = decoder.next_segment()? else {
      return Err(Error::SizeMismatch {
        expected: header.original_size,
        actual: total,
      });
    };
    let segment = segment.into_segment()?;
    total = total.saturating_add(segment.orig_len);
    if total > header.original_size {
      return Err(Error::SizeMismatch {
        expected: header.original_size,
        actual: total,
      });
    }
    let data = super::decode_segment(&segment, config, depth, budget)?;
    out.extend_from_slice(&data);
  }

  Ok((out, cursor.position() as usize))
}
