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

  /// Fails before allocating when `len` more output bytes would exceed the
  /// budget.
  pub(super) fn check(&self, len: u64, config: &super::DecodeConfig) -> Result<()> {
    if self.total_output.saturating_add(len) > config.max_total_output {
      return Err(Error::Other("decode output exceeds budget".to_string()));
    }
    Ok(())
  }
}
