use std::io::{self, Write};

use crate::error::{Error, Result};

#[derive(Default)]
pub(super) struct EncodeBudget {
  pub(super) total_output: u64,
}

impl EncodeBudget {
  pub(super) fn consume(&mut self, len: usize, config: &super::EncodeConfig) -> Result<()> {
    self.total_output = self.total_output.saturating_add(len as u64);
    if self.total_output > config.max_total_output {
      return Err(Error::Other("encode output exceeds budget".to_string()));
    }
    Ok(())
  }

  /// Output bytes still available under `config.max_total_output`.
  pub(super) fn remaining(&self, config: &super::EncodeConfig) -> u64 {
    config.max_total_output.saturating_sub(self.total_output)
  }
}

#[derive(Default)]
pub(super) struct DecodeBudget {
  pub(super) total_output: u64,
}

impl DecodeBudget {
  pub(super) fn consume(&mut self, len: usize, config: &super::DecodeConfig) -> Result<()> {
    self.total_output = self.total_output.saturating_add(len as u64);
    self.check(0, config)
  }

  /// Output bytes still available under `config.max_total_output`.
  pub(super) fn remaining(&self, config: &super::DecodeConfig) -> u64 {
    config.max_total_output.saturating_sub(self.total_output)
  }

  /// Fails before allocating when `len` more output bytes would exceed the
  /// budget.
  pub(super) fn check(&self, len: u64, config: &super::DecodeConfig) -> Result<()> {
    if self.total_output.saturating_add(len) > config.max_total_output {
      return Err(Error::Other("decode output exceeds budget".to_string()));
    }
    Ok(())
  }
}

/// Sink `Vec<u8>` writer that refuses to grow past `limit` bytes.
pub(super) struct BoundedWriter {
  buf: Vec<u8>,
  limit: u64,
  exceeded: bool,
}

impl BoundedWriter {
  pub(super) fn new(limit: u64) -> Self {
    Self {
      buf: Vec::new(),
      limit,
      exceeded: false,
    }
  }

  pub(super) fn is_exceeded(&self) -> bool {
    self.exceeded
  }

  pub(super) fn into_vec(self) -> Vec<u8> {
    self.buf
  }
}

impl Write for BoundedWriter {
  fn write(&mut self, data: &[u8]) -> io::Result<usize> {
    if (self.buf.len() as u64).saturating_add(data.len() as u64) > self.limit {
      self.exceeded = true;
      return Err(io::Error::new(io::ErrorKind::FileTooLarge, "decode output exceeds budget"));
    }
    self.buf.extend_from_slice(data);
    Ok(data.len())
  }

  fn flush(&mut self) -> io::Result<()> {
    Ok(())
  }
}
