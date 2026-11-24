use crate::error::{Error, Result};

pub(super) fn parse_meta_header(meta: &[u8]) -> Result<(u8, u8, &[u8])> {
  if meta.len() < 2 {
    return Err(Error::InvalidSegment("meta too short"));
  }
  let meta_version = meta[0];
  let payload_kind = meta[1];
  Ok((meta_version, payload_kind, &meta[2..]))
}

pub(super) fn read_varint_slice(data: &[u8], offset: &mut usize) -> Result<u64> {
  let mut value = 0u64;
  let mut shift = 0u32;
  for _ in 0..10 {
    if *offset >= data.len() {
      return Err(Error::VarintTooLong);
    }
    let byte = data[*offset];
    *offset += 1;
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

pub(super) fn write_varint_vec(value: u64, out: &mut Vec<u8>) {
  let mut val = value;
  loop {
    let mut byte = (val & 0x7f) as u8;
    val >>= 7;
    if val != 0 {
      byte |= 0x80;
    }
    out.push(byte);
    if val == 0 {
      break;
    }
  }
}
