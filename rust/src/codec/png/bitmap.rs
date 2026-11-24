use super::{Error, Result};
use crate::codec::scanline::{filter_row, unfilter_row};

#[derive(Clone, Copy, Debug)]
pub(super) struct PngIhdr {
  pub(super) width: u32,
  pub(super) height: u32,
  bit_depth: u8,
  pub(super) color_type: u8,
  compression: u8,
  filter: u8,
  interlace: u8,
}

impl PngIhdr {
  pub(super) fn parse(data: &[u8]) -> Option<Self> {
    if data.len() != 13 {
      return None;
    }
    let width = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
    let height = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    Some(Self {
      width,
      height,
      bit_depth: data[8],
      color_type: data[9],
      compression: data[10],
      filter: data[11],
      interlace: data[12],
    })
  }

  pub(super) fn is_webp_compatible(&self) -> bool {
    if self.width == 0 || self.height == 0 {
      return false;
    }
    if self.bit_depth != 8 {
      return false;
    }
    if self.compression != 0 || self.filter != 0 || self.interlace != 0 {
      return false;
    }
    matches!(self.color_type, 2 | 6)
  }

  pub(super) fn bytes_per_pixel(&self) -> Result<usize> {
    png_bytes_per_pixel(self.color_type)
  }
}

#[derive(Clone, Debug)]
pub(super) struct PngWebpMeta {
  pub(super) color_type: u8,
  pub(super) width: u32,
  pub(super) height: u32,
  pub(super) filters: Vec<u8>,
}

pub(super) fn png_bytes_per_pixel(color_type: u8) -> Result<usize> {
  match color_type {
    2 => Ok(3),
    6 => Ok(4),
    _ => Err(Error::InvalidSegment("png color_type")),
  }
}

fn png_row_bytes(width: u32, bpp: usize) -> Result<usize> {
  let width = usize::try_from(width).map_err(|_| Error::InvalidSegment("png width"))?;
  width.checked_mul(bpp).ok_or(Error::InvalidSegment("png row overflow"))
}

pub(super) fn undo_png_filters(filtered: &[u8], width: u32, height: u32, bpp: usize) -> Result<(Vec<u8>, Vec<u8>)> {
  let row_bytes = png_row_bytes(width, bpp)?;
  let height_usize = usize::try_from(height).map_err(|_| Error::InvalidSegment("png height"))?;
  let expected = row_bytes
    .checked_add(1)
    .and_then(|v| v.checked_mul(height_usize))
    .ok_or(Error::InvalidSegment("png size overflow"))?;
  if filtered.len() != expected {
    return Err(Error::InvalidSegment("png scanline size"));
  }

  let mut out = vec![0u8; row_bytes * height_usize];
  let mut filters = Vec::with_capacity(height_usize);
  let zeros = vec![0u8; row_bytes.min(filtered.len())];
  for (row, scanline) in filtered.chunks_exact(row_bytes + 1).enumerate() {
    filters.push(scanline[0]);
    let (done, rest) = out.split_at_mut(row * row_bytes);
    let prev = if row == 0 { &zeros } else { &done[(row - 1) * row_bytes..] };
    let current = &mut rest[..row_bytes];
    current.copy_from_slice(&scanline[1..]);
    unfilter_row(scanline[0], current, prev, bpp)?;
  }
  Ok((out, filters))
}

pub(super) fn apply_png_filters_with_types(bitmap: &[u8], width: u32, height: u32, bpp: usize, filters: &[u8]) -> Result<Vec<u8>> {
  let row_bytes = png_row_bytes(width, bpp)?;
  let height_usize = usize::try_from(height).map_err(|_| Error::InvalidSegment("png height"))?;
  if filters.len() != height_usize {
    return Err(Error::InvalidSegment("png filter count"));
  }
  if row_bytes.checked_mul(height_usize) != Some(bitmap.len()) || row_bytes == 0 {
    return Err(Error::InvalidSegment("png bitmap size"));
  }

  let mut out = vec![0u8; (row_bytes + 1) * height_usize];
  let zeros = vec![0u8; row_bytes.min(bitmap.len())];
  let mut prev = zeros.as_slice();
  for ((row, scanline), &filter) in bitmap.chunks_exact(row_bytes).zip(out.chunks_exact_mut(row_bytes + 1)).zip(filters) {
    scanline[0] = filter;
    filter_row(filter, row, prev, bpp, &mut scanline[1..])?;
    prev = row;
  }
  Ok(out)
}

pub(in crate::codec) fn encode_webp_lossless(bitmap: &[u8], width: u32, height: u32, color_type: u8) -> Result<Vec<u8>> {
  let encoder = match color_type {
    2 => webp::Encoder::from_rgb(bitmap, width, height),
    6 => webp::Encoder::from_rgba(bitmap, width, height),
    _ => return Err(Error::InvalidSegment("png color_type")),
  };
  let mut config = webp::WebPConfig::new().map_err(|_| Error::Other("webp config".to_string()))?;
  config.lossless = 1;
  config.exact = 1;
  config.alpha_compression = 0;
  config.alpha_filtering = 0;
  config.quality = 100.0;
  let webp = encoder
    .encode_advanced(&config)
    .map_err(|_| Error::Other("webp encode failed".to_string()))?;
  Ok(webp.to_vec())
}

pub(in crate::codec) fn decode_webp_bitmap(webp_bytes: &[u8], width: u32, height: u32, color_type: u8) -> Result<Vec<u8>> {
  let features = webp::BitstreamFeatures::new(webp_bytes).ok_or(Error::InvalidSegment("webp header"))?;
  if features.width() != width || features.height() != height || features.has_animation() {
    return Err(Error::InvalidSegment("png webp size"));
  }
  let decoded = webp::Decoder::new(webp_bytes)
    .decode()
    .ok_or_else(|| Error::Other("webp decode failed".to_string()))?;
  if decoded.width() != width || decoded.height() != height {
    return Err(Error::InvalidSegment("png webp size"));
  }
  let expected_layout = match color_type {
    2 => webp::PixelLayout::Rgb,
    6 => webp::PixelLayout::Rgba,
    _ => return Err(Error::InvalidSegment("png color_type")),
  };
  let data = &*decoded;
  match (expected_layout, decoded.layout()) {
    (webp::PixelLayout::Rgb, webp::PixelLayout::Rgb) => Ok(data.to_vec()),
    (webp::PixelLayout::Rgba, webp::PixelLayout::Rgba) => Ok(data.to_vec()),
    (webp::PixelLayout::Rgba, webp::PixelLayout::Rgb) => {
      let mut out = Vec::with_capacity((data.len() / 3) * 4);
      for chunk in data.as_chunks::<3>().0 {
        out.extend_from_slice(chunk);
        out.push(255);
      }
      Ok(out)
    }
    (webp::PixelLayout::Rgb, webp::PixelLayout::Rgba) => {
      let (pixels, _) = data.as_chunks::<4>();
      if pixels.iter().all(|px| px[3] == 255) {
        let mut out = Vec::with_capacity(pixels.len() * 3);
        for px in pixels {
          out.extend_from_slice(&px[..3]);
        }
        Ok(out)
      } else {
        Err(Error::InvalidSegment("png webp layout"))
      }
    }
  }
}
