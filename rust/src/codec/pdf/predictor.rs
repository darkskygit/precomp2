//! Metadata: u8 predictor, varint colors/bits/columns, then a length-prefixed
//! PNG row-tag blob (empty for TIFF). PNG predictors 10–15 retain every row's
//! filter byte. TIFF differencing changes only samples, preserving unused bits
//! in the last byte of each packed row.

use super::{Error, Result, Value, read_blob, read_num, write_blob, write_num};

#[derive(Clone, Copy)]
pub(super) struct Predictor {
  pub kind: u8,
  pub colors: usize,
  pub bits: usize,
  pub columns: usize,
}

impl Predictor {
  pub fn from_params(params: &Value) -> Result<Self> {
    let get = |name, default| params.get(name).and_then(Value::int).unwrap_or(default);
    let p = Self {
      kind: u8::try_from(get(b"Predictor", 1)).map_err(|_| Error::InvalidSegment("predictor"))?,
      colors: usize::try_from(get(b"Colors", 1)).map_err(|_| Error::LengthOverflow)?,
      bits: usize::try_from(get(b"BitsPerComponent", 8)).map_err(|_| Error::LengthOverflow)?,
      columns: usize::try_from(get(b"Columns", 1)).map_err(|_| Error::LengthOverflow)?,
    };
    p.row_bytes()?;
    if !matches!(p.kind, 1 | 2 | 10..=15) {
      return Err(Error::InvalidSegment("predictor kind"));
    }
    Ok(p)
  }

  pub fn row_bytes(&self) -> Result<usize> {
    if self.colors == 0 || self.colors > 32 || self.columns == 0 || !matches!(self.bits, 1 | 2 | 4 | 8 | 16) {
      return Err(Error::InvalidSegment("predictor dimensions"));
    }
    self
      .columns
      .checked_mul(self.colors)
      .and_then(|n| n.checked_mul(self.bits))
      .and_then(|n| n.checked_add(7))
      .map(|n| n / 8)
      .ok_or(Error::LengthOverflow)
  }

  pub fn undo(&self, data: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let row = self.row_bytes()?;
    let stride = row.checked_add(usize::from(self.kind >= 10)).ok_or(Error::LengthOverflow)?;
    if !data.len().is_multiple_of(stride) {
      return Err(Error::InvalidSegment("predictor row length"));
    }
    let mut pixels = Vec::with_capacity(data.len());
    let mut tags = Vec::new();
    for encoded in data.chunks_exact(stride) {
      let start = pixels.len();
      if self.kind >= 10 {
        let tag = encoded[0];
        if tag > 4 {
          return Err(Error::InvalidSegment("png row filter"));
        }
        tags.push(tag);
        pixels.extend_from_slice(&encoded[1..]);
        let bpp = (self.colors * self.bits).div_ceil(8);
        for x in 0..row {
          let a = if x >= bpp { pixels[start + x - bpp] } else { 0 };
          let b = if start >= row { pixels[start + x - row] } else { 0 };
          let c = if start >= row && x >= bpp {
            pixels[start + x - row - bpp]
          } else {
            0
          };
          pixels[start + x] = pixels[start + x].wrapping_add(prediction(tag, a, b, c));
        }
      } else {
        pixels.extend_from_slice(encoded);
        if self.kind == 2 {
          let row = &mut pixels[start..];
          for i in self.colors..self.columns * self.colors {
            let sample = sample(row, i, self.bits).wrapping_add(sample(row, i - self.colors, self.bits));
            set_sample(row, i, self.bits, sample);
          }
        }
      }
    }
    let mut meta = vec![self.kind];
    for n in [self.colors, self.bits, self.columns] {
      write_num(n, &mut meta);
    }
    write_blob(&tags, &mut meta);
    Ok((pixels, meta))
  }
}

pub(super) fn redo(pixels: &[u8], meta: &[u8], limit: usize) -> Result<Vec<u8>> {
  let mut pos = 1;
  let kind = *meta.first().ok_or(Error::InvalidSegment("predictor metadata"))?;
  if !matches!(kind, 1 | 2 | 10..=15) {
    return Err(Error::InvalidSegment("predictor kind"));
  }
  let p = Predictor {
    kind,
    colors: read_num(meta, &mut pos)?,
    bits: read_num(meta, &mut pos)?,
    columns: read_num(meta, &mut pos)?,
  };
  let tags = read_blob(meta, &mut pos)?;
  let row = p.row_bytes()?;
  if pos != meta.len()
    || !pixels.len().is_multiple_of(row)
    || (kind >= 10 && tags.len() != pixels.len() / row)
    || (kind < 10 && !tags.is_empty())
    || pixels.len().saturating_add(tags.len()) > limit
  {
    return Err(Error::InvalidSegment("predictor data length"));
  }
  let mut out = Vec::with_capacity(pixels.len() + tags.len());
  for (y, data) in pixels.chunks_exact(row).enumerate() {
    if kind >= 10 {
      let tag = tags[y];
      if tag > 4 {
        return Err(Error::InvalidSegment("png row filter"));
      }
      out.push(tag);
      let bpp = (p.colors * p.bits).div_ceil(8);
      for x in 0..row {
        let a = if x >= bpp { data[x - bpp] } else { 0 };
        let b = if y > 0 { pixels[(y - 1) * row + x] } else { 0 };
        let c = if y > 0 && x >= bpp { pixels[(y - 1) * row + x - bpp] } else { 0 };
        out.push(data[x].wrapping_sub(prediction(tag, a, b, c)));
      }
    } else {
      let start = out.len();
      out.extend_from_slice(data);
      if kind == 2 {
        for i in p.colors..p.columns * p.colors {
          set_sample(
            &mut out[start..],
            i,
            p.bits,
            sample(data, i, p.bits).wrapping_sub(sample(data, i - p.colors, p.bits)),
          );
        }
      }
    }
  }
  Ok(out)
}

fn prediction(tag: u8, a: u8, b: u8, c: u8) -> u8 {
  match tag {
    0 => 0,
    1 => a,
    2 => b,
    3 => ((u16::from(a) + u16::from(b)) / 2) as u8,
    _ => {
      let p = i32::from(a) + i32::from(b) - i32::from(c);
      let (pa, pb, pc) = ((p - i32::from(a)).abs(), (p - i32::from(b)).abs(), (p - i32::from(c)).abs());
      if pa <= pb && pa <= pc {
        a
      } else if pb <= pc {
        b
      } else {
        c
      }
    }
  }
}

fn sample(data: &[u8], index: usize, bits: usize) -> u16 {
  let byte = index * bits / 8;
  if bits == 16 {
    u16::from_be_bytes([data[byte], data[byte + 1]])
  } else {
    u16::from(data[byte] >> (8 - bits - index * bits % 8)) & ((1 << bits) - 1)
  }
}

fn set_sample(data: &mut [u8], index: usize, bits: usize, value: u16) {
  let byte = index * bits / 8;
  if bits == 16 {
    data[byte..byte + 2].copy_from_slice(&value.to_be_bytes());
  } else {
    let shift = 8 - bits - index * bits % 8;
    let mask = (((1u16 << bits) - 1) as u8) << shift;
    data[byte] = (data[byte] & !mask) | (((value as u8) << shift) & mask);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn normative_sample_values() {
    let p = Predictor {
      kind: 15,
      colors: 1,
      bits: 8,
      columns: 3,
    };
    let encoded = [1, 10, 10, 10, 2, 5, 5, 5];
    let (pixels, meta) = p.undo(&encoded).unwrap();
    assert_eq!(pixels, [10, 20, 30, 15, 25, 35]);
    assert_eq!(redo(&pixels, &meta, encoded.len()).unwrap(), encoded);
    let p = Predictor {
      kind: 2,
      colors: 1,
      bits: 4,
      columns: 3,
    };
    let (pixels, meta) = p.undo(&[0x12, 0x3f]).unwrap();
    assert_eq!(pixels, [0x13, 0x6f]);
    assert_eq!(redo(&pixels, &meta, 2).unwrap(), [0x12, 0x3f]);
    let p = Predictor {
      kind: 2,
      colors: 1,
      bits: 16,
      columns: 2,
    };
    assert_eq!(p.undo(&[0xff, 0xff, 0, 2]).unwrap().0, [0xff, 0xff, 0, 1]);
  }

  #[test]
  fn all_predictors_preserve_samples_and_padding() {
    for bits in [1, 2, 4, 8, 16] {
      for colors in [1, 3, 4] {
        for kind in [1, 2, 10, 11, 12, 13, 14, 15] {
          let p = Predictor {
            kind,
            bits,
            colors,
            columns: 7,
          };
          let row = p.row_bytes().unwrap();
          let data: Vec<_> = (0..row * 5).map(|n| (n * 71 + 123) as u8).collect();
          let mut meta = vec![kind];
          for n in [colors, bits, 7] {
            write_num(n, &mut meta);
          }
          write_blob(if kind >= 10 { &[0, 1, 2, 3, 4] } else { &[] }, &mut meta);
          let encoded = redo(&data, &meta, usize::MAX).unwrap();
          let (decoded, saved) = p.undo(&encoded).unwrap();
          assert_eq!(decoded, data, "kind={kind},bits={bits},colors={colors}");
          assert_eq!(saved, meta);
        }
      }
    }
  }
}
