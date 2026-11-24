//! Metadata: u8 mode, varint encoded width, varint height. Mode 1 uses RGB8
//! samples directly; mode 0 maps each original byte to an equal RGB triplet and
//! verifies equality on decode. Thus packed row padding, 16-bit byte order,
//! palette indices and CMYK samples survive without color conversion.
//!
//! SMask/Mask images and ImageMask stencils stay independent streams, never
//! merged into RGBA. Decode, Matte, palette and color-space dictionaries remain
//! literal; color-key Mask arrays need no image transform.

use super::{Error, Result, Value, decode_webp_bitmap, encode_webp_lossless, read_num, write_num};

pub(super) fn encode(bytes: &[u8], dict: &Value) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
  if dict.get(b"Subtype").and_then(Value::name) != Some(b"Image".as_slice()) {
    return Ok(None);
  }
  let width = dict
    .get(b"Width")
    .and_then(Value::int)
    .and_then(|n| usize::try_from(n).ok())
    .unwrap_or(0);
  let height = dict
    .get(b"Height")
    .and_then(Value::int)
    .and_then(|n| usize::try_from(n).ok())
    .unwrap_or(0);
  let mask = matches!(dict.get(b"ImageMask"), Some(Value::Bool(true)));
  let bits = dict
    .get(b"BitsPerComponent")
    .and_then(Value::int)
    .unwrap_or(if mask { 1 } else { 8 });
  let colors = if mask {
    1
  } else {
    match dict.get(b"ColorSpace") {
      Some(Value::Name(name)) => match name.as_slice() {
        b"DeviceGray" | b"G" => 1,
        b"DeviceRGB" | b"RGB" => 3,
        b"DeviceCMYK" | b"CMYK" => 4,
        _ => return Ok(None),
      },
      Some(Value::Array(items)) if items.first().and_then(Value::name) == Some(b"Indexed".as_slice()) => 1,
      _ => return Ok(None),
    }
  };
  if width == 0 || height == 0 || !matches!(bits, 1 | 2 | 4 | 8 | 16) {
    return Ok(None);
  }
  let row = width
    .checked_mul(colors)
    .and_then(|n| n.checked_mul(bits as usize))
    .and_then(|n| n.checked_add(7))
    .map(|n| n / 8)
    .ok_or(Error::LengthOverflow)?;
  if row.checked_mul(height) != Some(bytes.len()) {
    return Ok(None);
  }
  let rgb = colors == 3 && bits == 8;
  let encoded_width = if rgb { width } else { row };
  if encoded_width > 16383 || height > 16383 {
    return Ok(None);
  }
  let expanded;
  let pixels = if rgb {
    bytes
  } else {
    expanded = bytes.iter().flat_map(|b| [*b; 3]).collect::<Vec<_>>();
    &expanded
  };
  let encoded = encode_webp_lossless(pixels, encoded_width as u32, height as u32, 2)?;
  let mut meta = vec![u8::from(rgb)];
  write_num(encoded_width, &mut meta);
  write_num(height, &mut meta);
  Ok(Some((encoded, meta)))
}

pub(super) fn decode(bytes: &[u8], meta: &[u8], len: usize) -> Result<Vec<u8>> {
  let rgb = match meta.first() {
    Some(0) => false,
    Some(1) => true,
    _ => return Err(Error::InvalidSegment("pdf image mode")),
  };
  let mut pos = 1;
  let width = read_num(meta, &mut pos)?;
  let height = read_num(meta, &mut pos)?;
  if pos != meta.len()
    || width == 0
    || height == 0
    || width > 16383
    || height > 16383
    || width.checked_mul(height).and_then(|n| n.checked_mul(if rgb { 3 } else { 1 })) != Some(len)
  {
    return Err(Error::InvalidSegment("pdf image dimensions"));
  }
  let pixels = decode_webp_bitmap(bytes, width as u32, height as u32, 2)?;
  if rgb {
    Ok(pixels)
  } else {
    let (pixels, _) = pixels.as_chunks::<3>();
    if !pixels.iter().all(|&[r, g, b]| r == g && r == b) {
      return Err(Error::InvalidSegment("pdf gray channels"));
    }
    Ok(pixels.iter().map(|&[gray, _, _]| gray).collect())
  }
}
