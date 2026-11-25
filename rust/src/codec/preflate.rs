use super::varint::{read_len, take_bytes, write_varint_vec};
use crate::error::{Error, Result};

const PREFLATE_PLAIN_TEXT_LIMIT: usize = i32::MAX as usize;

/// A deflate stream split into its plain text and the corrections that
/// recreate the exact compressed bytes.
pub(super) struct PreflateAnalysis {
  /// Compressed bytes the analysis consumed.
  pub(super) deflate_len: u64,
  pub(super) corrections: Vec<u8>,
  pub(super) plain: Vec<u8>,
}

pub(super) fn build_preflate_data(corrections: &[u8], payload: &[u8]) -> Vec<u8> {
  let mut out = Vec::with_capacity(corrections.len() + payload.len() + 16);
  write_varint_vec(corrections.len() as u64, &mut out);
  out.extend_from_slice(corrections);
  write_varint_vec(payload.len() as u64, &mut out);
  out.extend_from_slice(payload);
  out
}

/// Splits segment data written by [`build_preflate_data`] into corrections
/// and payload.
pub(super) fn parse_preflate_data(data: &[u8]) -> Result<(&[u8], &[u8])> {
  let mut offset = 0usize;
  let corrections_len = read_len(data, &mut offset)?;
  let corrections = take_bytes(data, &mut offset, corrections_len, "corrections bounds")?;
  let payload_len = read_len(data, &mut offset)?;
  let payload = take_bytes(data, &mut offset, payload_len, "payload bounds")?;
  Ok((corrections, payload))
}

#[cfg(test)]
pub(super) fn preflate_analyze(deflate: &[u8]) -> Result<PreflateAnalysis> {
  preflate_analyze_limited(deflate, PREFLATE_PLAIN_TEXT_LIMIT)
}

pub(super) fn preflate_analyze_limited(deflate: &[u8], limit: usize) -> Result<PreflateAnalysis> {
  let config = preflate_rs::PreflateConfig {
    max_chain_length: 4096,
    plain_text_limit: limit.min(PREFLATE_PLAIN_TEXT_LIMIT),
    verify_compression: false,
  };
  let (analysis, plain_text) =
    preflate_rs::preflate_whole_deflate_stream(deflate, &config).map_err(|err| Error::Other(format!("preflate_analyze failed: {err}")))?;
  Ok(PreflateAnalysis {
    deflate_len: analysis.compressed_size as u64,
    corrections: analysis.corrections,
    plain: plain_text.text().to_vec(),
  })
}

pub(super) fn preflate_reencode(corrections: &[u8], plain: &[u8]) -> Result<Vec<u8>> {
  preflate_rs::recreate_whole_deflate_stream(plain, corrections).map_err(|err| Error::Other(format!("preflate_reencode failed: {err}")))
}
