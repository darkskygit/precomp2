use crate::error::{Error, Result};

const PREFLATE_PLAIN_TEXT_LIMIT: usize = i32::MAX as usize;

pub(super) fn parse_preflate_output(buf: &[u8]) -> Result<(u64, Vec<u8>, Vec<u8>)> {
  let mut offset = 0usize;
  let deflate_len = super::varint::read_varint_slice(buf, &mut offset)?;
  let corrections_len = super::varint::read_varint_slice(buf, &mut offset)? as usize;
  if offset + corrections_len > buf.len() {
    return Err(Error::InvalidSegment("preflate corrections bounds"));
  }
  let corrections = buf[offset..offset + corrections_len].to_vec();
  offset += corrections_len;
  let plain_len = super::varint::read_varint_slice(buf, &mut offset)? as usize;
  if offset + plain_len > buf.len() {
    return Err(Error::InvalidSegment("preflate plain bounds"));
  }
  let plain = buf[offset..offset + plain_len].to_vec();
  Ok((deflate_len, corrections, plain))
}

pub(super) fn build_preflate_data(corrections: &[u8], payload: &[u8]) -> Vec<u8> {
  let mut out = Vec::with_capacity(corrections.len() + payload.len() + 16);
  super::varint::write_varint_vec(corrections.len() as u64, &mut out);
  out.extend_from_slice(corrections);
  super::varint::write_varint_vec(payload.len() as u64, &mut out);
  out.extend_from_slice(payload);
  out
}

pub(super) fn parse_preflate_data(data: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
  let mut offset = 0usize;
  let corrections_len = super::varint::read_varint_slice(data, &mut offset)? as usize;
  if offset + corrections_len > data.len() {
    return Err(Error::InvalidSegment("corrections bounds"));
  }
  let corrections = data[offset..offset + corrections_len].to_vec();
  offset += corrections_len;
  let payload_len = super::varint::read_varint_slice(data, &mut offset)? as usize;
  if offset + payload_len > data.len() {
    return Err(Error::InvalidSegment("payload bounds"));
  }
  let payload = data[offset..offset + payload_len].to_vec();
  Ok((corrections, payload))
}

pub(super) fn preflate_analyze(deflate: &[u8]) -> Result<Vec<u8>> {
  preflate_analyze_limited(deflate, PREFLATE_PLAIN_TEXT_LIMIT)
}

pub(super) fn preflate_analyze_limited(deflate: &[u8], limit: usize) -> Result<Vec<u8>> {
  let config = preflate_rs::PreflateConfig {
    max_chain_length: 4096,
    plain_text_limit: limit.min(PREFLATE_PLAIN_TEXT_LIMIT),
    verify_compression: false,
  };
  let (analysis, plain_text) =
    preflate_rs::preflate_whole_deflate_stream(deflate, &config).map_err(|err| Error::Other(format!("preflate_analyze failed: {err}")))?;

  let mut out = Vec::with_capacity(deflate.len() + analysis.corrections.len() + 16);
  super::varint::write_varint_vec(analysis.compressed_size as u64, &mut out);
  super::varint::write_varint_vec(analysis.corrections.len() as u64, &mut out);
  out.extend_from_slice(&analysis.corrections);
  super::varint::write_varint_vec(plain_text.text().len() as u64, &mut out);
  out.extend_from_slice(plain_text.text());
  Ok(out)
}

pub(super) fn preflate_reencode(corrections: &[u8], plain: &[u8]) -> Result<Vec<u8>> {
  preflate_rs::recreate_whole_deflate_stream(plain, corrections).map_err(|err| Error::Other(format!("preflate_reencode failed: {err}")))
}
