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
}

#[derive(Default)]
pub(super) struct DecodeBudget {
  pub(super) total_output: u64,
}

impl DecodeBudget {
  pub(super) fn consume(&mut self, len: usize, config: &super::DecodeConfig) -> Result<()> {
    self.total_output = self.total_output.saturating_add(len as u64);
    if self.total_output > config.max_total_output {
      return Err(Error::Other("decode output exceeds budget".to_string()));
    }
    Ok(())
  }
}
