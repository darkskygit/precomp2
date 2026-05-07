use std::io::{Read, Write};

use crate::error::{Error, Result};

pub fn write_varint<W: Write>(mut value: u64, mut writer: W) -> Result<()> {
  loop {
    let mut byte = (value & 0x7f) as u8;
    value >>= 7;
    if value != 0 {
      byte |= 0x80;
    }
    writer.write_all(&[byte])?;
    if value == 0 {
      return Ok(());
    }
  }
}

pub fn read_varint<R: Read>(mut reader: R) -> Result<u64> {
  let mut value = 0u64;
  let mut shift = 0u32;

  for _ in 0..10 {
    let mut buf = [0u8; 1];
    reader.read_exact(&mut buf)?;
    let byte = buf[0];
    let part = (byte & 0x7f) as u64;

    if shift > 63 {
      return Err(Error::VarintOverflow);
    }
    if shift == 63 && part > 1 {
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn varint_roundtrip() {
    let values = [0u64, 1, 2, 127, 128, 255, 16384, u32::MAX as u64, u64::MAX];

    for value in values {
      let mut buf = Vec::new();
      write_varint(value, &mut buf).expect("write varint");
      let decoded = read_varint(buf.as_slice()).expect("read varint");
      assert_eq!(value, decoded);
    }
  }

  #[test]
  fn varint_rejects_too_long() {
    let buf = [0x80u8; 11];
    let err = read_varint(buf.as_slice()).expect_err("expected error");
    assert!(matches!(err, Error::VarintTooLong | Error::VarintOverflow));
  }
}
