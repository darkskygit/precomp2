//! PNG scanline filters (types 0–4), shared by PNG IDAT and PDF predictors.
//! `prev` is the previous unfiltered row, all zeros for the first row; each
//! filter type runs as its own loop so the per-byte work has no dispatch.

use crate::error::{Error, Result};

/// Reverses filter `filter` on `row` in place.
pub(super) fn unfilter_row(filter: u8, row: &mut [u8], prev: &[u8], bpp: usize) -> Result<()> {
  debug_assert_eq!(row.len(), prev.len());
  let lead = bpp.min(row.len());
  match filter {
    0 => {}
    1 => {
      for i in lead..row.len() {
        row[i] = row[i].wrapping_add(row[i - bpp]);
      }
    }
    2 => {
      for (x, &up) in row.iter_mut().zip(prev) {
        *x = x.wrapping_add(up);
      }
    }
    3 => {
      for i in 0..lead {
        row[i] = row[i].wrapping_add(prev[i] / 2);
      }
      for i in lead..row.len() {
        row[i] = row[i].wrapping_add(average(row[i - bpp], prev[i]));
      }
    }
    4 => {
      for i in 0..lead {
        row[i] = row[i].wrapping_add(prev[i]);
      }
      for i in lead..row.len() {
        row[i] = row[i].wrapping_add(paeth(row[i - bpp], prev[i], prev[i - bpp]));
      }
    }
    _ => return Err(Error::InvalidSegment("png filter type")),
  }
  Ok(())
}

/// Applies filter `filter` to `row`, writing the filtered bytes to `out`.
pub(super) fn filter_row(filter: u8, row: &[u8], prev: &[u8], bpp: usize, out: &mut [u8]) -> Result<()> {
  debug_assert!(row.len() == prev.len() && row.len() == out.len());
  let lead = bpp.min(row.len());
  match filter {
    0 => out.copy_from_slice(row),
    1 => {
      out[..lead].copy_from_slice(&row[..lead]);
      for i in lead..row.len() {
        out[i] = row[i].wrapping_sub(row[i - bpp]);
      }
    }
    2 => {
      for ((o, &x), &up) in out.iter_mut().zip(row).zip(prev) {
        *o = x.wrapping_sub(up);
      }
    }
    3 => {
      for i in 0..lead {
        out[i] = row[i].wrapping_sub(prev[i] / 2);
      }
      for i in lead..row.len() {
        out[i] = row[i].wrapping_sub(average(row[i - bpp], prev[i]));
      }
    }
    4 => {
      for i in 0..lead {
        out[i] = row[i].wrapping_sub(prev[i]);
      }
      for i in lead..row.len() {
        out[i] = row[i].wrapping_sub(paeth(row[i - bpp], prev[i], prev[i - bpp]));
      }
    }
    _ => return Err(Error::InvalidSegment("png filter type")),
  }
  Ok(())
}

#[inline(always)]
fn average(left: u8, up: u8) -> u8 {
  ((u16::from(left) + u16::from(up)) / 2) as u8
}

#[inline(always)]
fn paeth(a: u8, b: u8, c: u8) -> u8 {
  let (a16, b16, c16) = (i16::from(a), i16::from(b), i16::from(c));
  let pa = (b16 - c16).abs();
  let pb = (a16 - c16).abs();
  let pc = (a16 + b16 - 2 * c16).abs();
  if pa <= pb && pa <= pc {
    a
  } else if pb <= pc {
    b
  } else {
    c
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// The PNG specification's predictor, byte by byte.
  fn reference(filter: u8, row: &[u8], prev: &[u8], bpp: usize) -> Vec<u8> {
    (0..row.len())
      .map(|i| {
        let a = if i >= bpp { i32::from(row[i - bpp]) } else { 0 };
        let b = i32::from(prev[i]);
        let c = if i >= bpp { i32::from(prev[i - bpp]) } else { 0 };
        let predicted = match filter {
          0 => 0,
          1 => a,
          2 => b,
          3 => (a + b) / 2,
          _ => {
            let p = a + b - c;
            let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
            if pa <= pb && pa <= pc {
              a
            } else if pb <= pc {
              b
            } else {
              c
            }
          }
        };
        row[i].wrapping_sub(predicted as u8)
      })
      .collect()
  }

  #[test]
  fn filters_match_reference_and_unfilter_inverts_them() {
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut bytes = |len: usize| -> Vec<u8> {
      (0..len)
        .map(|_| {
          state ^= state << 13;
          state ^= state >> 7;
          state ^= state << 17;
          state as u8
        })
        .collect()
    };
    for bpp in [1, 2, 3, 4, 6, 8] {
      for len in [0, 1, bpp, bpp + 1, 37, 256] {
        let row = bytes(len);
        for prev in [vec![0; len], bytes(len)] {
          for filter in 0..=4 {
            let mut filtered = vec![0; len];
            filter_row(filter, &row, &prev, bpp, &mut filtered).unwrap();
            assert_eq!(filtered, reference(filter, &row, &prev, bpp), "filter {filter} bpp {bpp} len {len}");
            unfilter_row(filter, &mut filtered, &prev, bpp).unwrap();
            assert_eq!(filtered, row, "unfilter {filter} bpp {bpp} len {len}");
          }
        }
      }
    }
    assert!(unfilter_row(5, &mut [0], &[0], 1).is_err());
    assert!(filter_row(5, &[0], &[0], 1, &mut [0]).is_err());
  }
}
