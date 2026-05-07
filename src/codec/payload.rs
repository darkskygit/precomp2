use std::io::Cursor;

use super::{
  DecodeConfig, EncodeConfig,
  budget::{DecodeBudget, EncodeBudget},
};
use crate::{
  error::{Error, Result},
  pcf2::Pcf2File,
};

pub(super) fn choose_payload(
  plain: Vec<u8>,
  orig_len: usize,
  config: &EncodeConfig,
  depth: u32,
  budget: &mut EncodeBudget,
) -> Result<(u8, Vec<u8>)> {
  if depth < config.max_depth && orig_len > 0 {
    let ratio = plain.len() as f64 / orig_len as f64;
    if ratio <= config.max_expand_ratio && budget.total_output + plain.len() as u64 <= config.max_total_output {
      let nested_segments = super::encode_segments(&plain, config, depth + 1, budget)?;
      let nested_file = Pcf2File::from_segments(nested_segments);
      let mut nested_bytes = Vec::new();
      nested_file.encode(&mut nested_bytes)?;
      budget.consume(plain.len(), config)?;
      return Ok((1, nested_bytes));
    }
  }
  Ok((0, plain))
}

pub(super) fn decode_payload(
  payload_kind: u8,
  payload: Vec<u8>,
  config: &DecodeConfig,
  depth: u32,
  budget: &mut DecodeBudget,
) -> Result<Vec<u8>> {
  if payload_kind == 0 {
    return Ok(payload);
  }
  if payload_kind != 1 {
    return Err(Error::InvalidSegment("payload_kind"));
  }
  if depth >= config.max_depth {
    return Err(Error::Other("nested depth exceeded".to_string()));
  }
  super::stream::decode_stream(Cursor::new(&payload), config, depth + 1, budget)
}
