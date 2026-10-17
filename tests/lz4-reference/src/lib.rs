//! Test/bench-only bindings to reference liblz4 builds (one per version).
#![allow(non_camel_case_types)]

use std::os::raw::{c_char, c_int};

macro_rules! versions {
  ($($variant:ident => $sym:ident, $bound:ident, $version:ident, $name:literal, $number:literal;)*) => {
    unsafe extern "C" {
      $(fn $sym(s: *const c_char, d: *mut c_char, n: c_int, cap: c_int, lvl: c_int) -> c_int;)*
      $(fn $bound(n: c_int) -> c_int;)*
      $(fn $version() -> c_int;)*
    }

    /// Reference liblz4 release.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum CVersion {
      $($variant,)*
    }

    impl CVersion {
      pub const ALL: &'static [CVersion] = &[$(CVersion::$variant,)*];

      pub fn name(self) -> &'static str {
        match self {
          $(CVersion::$variant => $name,)*
        }
      }

      pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|v| v.name() == name)
      }

      unsafe fn compress_hc_raw(
        self,
        s: *const c_char,
        d: *mut c_char,
        n: c_int,
        cap: c_int,
        lvl: c_int,
      ) -> c_int {
        // SAFETY: the caller passes valid buffers of the given sizes.
        unsafe {
          match self {
            $(CVersion::$variant => $sym(s, d, n, cap, lvl),)*
          }
        }
      }

      /// `LZ4_VERSION_NUMBER` the release declares (r131 is 1.7.1).
      pub fn expected_version_number(self) -> i32 {
        match self {
          $(CVersion::$variant => $number,)*
        }
      }

      /// `LZ4_VERSION_NUMBER` of the vendored headers.
      pub fn version_number(self) -> i32 {
        // SAFETY: returns a compile-time constant.
        unsafe {
          match self {
            $(CVersion::$variant => $version(),)*
          }
        }
      }

      fn compress_bound_raw(self, n: c_int) -> c_int {
        // SAFETY: pure function of `n`.
        unsafe {
          match self {
            $(CVersion::$variant => $bound(n),)*
          }
        }
      }
    }
  };
}

versions! {
  R131 => r131_compress_hc, r131_compress_bound, r131_version_number, "r131", 10701;
  V1_9_1 => v1_9_1_compress_hc, v1_9_1_compress_bound, v1_9_1_version_number, "1.9.1", 10901;
  V1_10_0 => v1_10_0_compress_hc, v1_10_0_compress_bound, v1_10_0_version_number, "1.10.0", 11000;
}

/// `LZ4_compressBound(n)` of the reference build.
pub fn compress_bound(v: CVersion, n: usize) -> usize {
  v.compress_bound_raw(n.min(i32::MAX as usize) as c_int) as usize
}

/// `LZ4_compress_HC(src, dst, srcSize, dstCapacity, level)`; `None` when it
/// returns 0.
pub fn compress_hc(v: CVersion, src: &[u8], dst_capacity: usize, level: i32) -> Option<Vec<u8>> {
  let mut dst = vec![0u8; dst_capacity.max(1)];
  // SAFETY: buffers are valid for the given sizes; the C function writes at
  // most dst_capacity bytes.
  let n = unsafe {
    v.compress_hc_raw(
      src.as_ptr().cast(),
      dst.as_mut_ptr().cast(),
      src.len() as c_int,
      dst_capacity as c_int,
      level,
    )
  };
  if n <= 0 {
    return None;
  }
  dst.truncate(n as usize);
  Some(dst)
}
