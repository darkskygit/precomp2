use std::io::Read;

use super::{
  DecodeBudget, DecodeConfig, Error, Pcf2Segment, PngIhdr, Result, SegmentKind, decode_payload, parse_png_meta, parse_preflate_data,
};

pub(super) struct PngDisplayMetadata {
  pub(super) ihdr: PngIhdr,
  icc: Option<Vec<u8>>,
}

// Also accepts the concatenated literal prefix/suffix of a PNG PCF: its IDAT
// chunks are deliberately absent, while every display-affecting chunk remains.
pub(super) fn parse_png_metadata(input: &[u8]) -> Result<Option<PngDisplayMetadata>> {
  if !input.starts_with(b"\x89PNG\r\n\x1a\n") {
    return Ok(None);
  }
  let mut offset = 8usize;
  let mut ihdr = None;
  let mut icc = None;
  let mut cicp = false;
  let mut ended = false;
  while offset < input.len() {
    let header = input.get(offset..offset + 8).ok_or(Error::InvalidSegment("png chunk header"))?;
    let len = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
    let kind = &header[4..];
    let end = offset
      .checked_add(12)
      .and_then(|v| v.checked_add(len))
      .ok_or(Error::LengthOverflow)?;
    let chunk = input.get(offset..end).ok_or(Error::InvalidSegment("png chunk bounds"))?;
    let data = &chunk[8..8 + len];
    if crc32fast::hash(&chunk[4..8 + len]) != u32::from_be_bytes(chunk[8 + len..].try_into().unwrap()) {
      return Err(Error::InvalidSegment("png chunk crc"));
    }
    if ihdr.is_none() && kind != b"IHDR" {
      return Err(Error::InvalidSegment("png missing ihdr"));
    }
    match kind {
      b"IHDR" => {
        if ihdr.is_some() {
          return Err(Error::InvalidSegment("png duplicate ihdr"));
        }
        let Some(parsed) = PngIhdr::parse(data) else {
          return Err(Error::InvalidSegment("png ihdr"));
        };
        if !parsed.is_webp_compatible() {
          return Ok(None);
        }
        ihdr = Some(parsed);
      }
      b"cICP" => cicp = true,
      b"iCCP" if icc.is_none() => icc = Some(data.to_vec()),
      b"IEND" => {
        if !data.is_empty() || end != input.len() {
          return Err(Error::InvalidSegment("png iend"));
        }
        ended = true;
      }
      _ => {}
    }
    offset = end;
  }
  if !ended {
    return Err(Error::InvalidSegment("png missing iend"));
  }
  let Some(ihdr) = ihdr else {
    return Err(Error::InvalidSegment("png missing ihdr"));
  };
  if cicp {
    icc = None;
  }
  Ok(Some(PngDisplayMetadata { ihdr, icc }))
}

fn decode_png_icc(compressed: &[u8], max_profile_size: u64) -> Result<Option<Vec<u8>>> {
  let Some(name_end) = compressed.iter().position(|v| *v == 0) else {
    return Ok(None);
  };
  if !(1..=79).contains(&name_end) || compressed.get(name_end + 1) != Some(&0) {
    return Ok(None);
  }
  let mut profile = Vec::new();
  let mut decoder = flate2::read::ZlibDecoder::new(&compressed[name_end + 2..]);
  if decoder
    .by_ref()
    .take(max_profile_size.saturating_add(1))
    .read_to_end(&mut profile)
    .is_err()
  {
    return Ok(None);
  }
  if profile.len() as u64 > max_profile_size {
    return Err(Error::Other("png icc exceeds budget".to_string()));
  }
  if decoder.total_in() != (compressed.len() - name_end - 2) as u64 {
    return Ok(None);
  }
  if profile.len() < 132
    || profile.get(16..20) != Some(b"RGB ")
    || profile.get(36..40) != Some(b"acsp")
    || u32::from_be_bytes(profile[..4].try_into().unwrap()) as usize != profile.len()
  {
    return Ok(None);
  }
  Ok(Some(profile))
}

pub(in crate::codec) fn extract_png_display(segments: &[Pcf2Segment], config: &DecodeConfig) -> Result<Option<Vec<u8>>> {
  let mut metadata = Vec::new();
  let mut carrier = None;
  let mut parsed = None;
  for segment in segments {
    if segment.kind == SegmentKind::Lit as u8 {
      metadata.extend_from_slice(&segment.data);
    } else if segment.kind == SegmentKind::PngIdat as u8 && carrier.is_none() {
      let meta = parse_png_meta(segment)?;
      let (.., payload) = parse_preflate_data(&segment.data)?;
      if meta.png_payload_kind == 0 {
        return Ok(None);
      }
      carrier = Some(decode_payload(meta.payload_kind, payload, config, 0, &mut DecodeBudget::default())?);
      parsed = meta.webp_meta;
    } else {
      return Err(Error::InvalidSegment("png display segment layout"));
    }
  }
  let Some(carrier) = carrier else {
    return Ok(None);
  };
  let Some(plan) = parse_png_metadata(&metadata)? else {
    return Ok(None);
  };
  let meta = parsed.ok_or(Error::InvalidSegment("png webp meta"))?;
  if meta.width != plan.ihdr.width
    || meta.height != plan.ihdr.height
    || meta.color_type != plan.ihdr.color_type
    || meta.filters.iter().any(|f| *f > 4)
  {
    return Err(Error::InvalidSegment("png display dimensions"));
  }
  let features = webp::BitstreamFeatures::new(&carrier).ok_or(Error::InvalidSegment("webp header"))?;
  if features.width() != meta.width || features.height() != meta.height || features.has_animation() {
    return Err(Error::InvalidSegment("png webp size"));
  }
  // Validate the complete standalone RIFF, even when no ICC needs muxing.
  if carrier.len() < 12
    || &carrier[..4] != b"RIFF"
    || &carrier[8..12] != b"WEBP"
    || u32::from_le_bytes(carrier[4..8].try_into().unwrap()) as usize + 8 != carrier.len()
  {
    return Err(Error::InvalidSegment("webp riff"));
  }
  let mut offset = 12usize;
  while offset < carrier.len() {
    let header = carrier.get(offset..offset + 8).ok_or(Error::InvalidSegment("webp chunk"))?;
    let len = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
    offset = offset
      .checked_add(8)
      .and_then(|v| v.checked_add(len))
      .and_then(|v| v.checked_add(len % 2))
      .ok_or(Error::LengthOverflow)?;
    if offset > carrier.len() {
      return Err(Error::InvalidSegment("webp chunk bounds"));
    }
  }
  let mut budget = DecodeBudget::default();
  budget.consume(carrier.len(), config)?;
  let icc = match plan.icc {
    Some(compressed) => decode_png_icc(&compressed, config.max_total_output)?,
    None => None,
  };
  let Some(icc) = icc else {
    return Ok(Some(carrier));
  };
  let extra_len = icc.len().checked_add(26 + icc.len() % 2).ok_or(Error::LengthOverflow)?;
  budget.consume(extra_len, config)?;
  // Add a WebP extended header and ICCP chunk without decoding/reencoding
  // pixels.
  let mut out = b"RIFF\0\0\0\0WEBP".to_vec();
  let mut vp8x = vec![0x20 | if features.has_alpha() { 0x10 } else { 0 }, 0, 0, 0];
  vp8x.extend_from_slice(&(meta.width - 1).to_le_bytes()[..3]);
  vp8x.extend_from_slice(&(meta.height - 1).to_le_bytes()[..3]);
  write_webp_chunk(&mut out, b"VP8X", &vp8x)?;
  write_webp_chunk(&mut out, b"ICCP", &icc)?;
  out.extend_from_slice(&carrier[12..]);
  let size = u32::try_from(out.len() - 8).map_err(|_| Error::LengthOverflow)?;
  out[4..8].copy_from_slice(&size.to_le_bytes());
  Ok(Some(out))
}

fn write_webp_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) -> Result<()> {
  out.extend_from_slice(kind);
  out.extend_from_slice(&u32::try_from(data.len()).map_err(|_| Error::LengthOverflow)?.to_le_bytes());
  out.extend_from_slice(data);
  if !data.len().is_multiple_of(2) {
    out.push(0);
  }
  Ok(())
}
