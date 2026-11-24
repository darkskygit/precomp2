use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
  #[error("io error: {0}")]
  Io(#[from] std::io::Error),
  #[error("invalid header: {0}")]
  InvalidHeader(&'static str),
  #[error("invalid segment: {0}")]
  InvalidSegment(&'static str),
  #[error("varint too long")]
  VarintTooLong,
  #[error("varint overflow")]
  VarintOverflow,
  #[error("length overflow")]
  LengthOverflow,
  #[error("original size mismatch: expected {expected}, got {actual}")]
  SizeMismatch { expected: u64, actual: u64 },
  #[error("{0}")]
  Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;
