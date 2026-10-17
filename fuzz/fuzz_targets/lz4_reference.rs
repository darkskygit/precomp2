#![no_main]
//! Byte-exact comparison of every pure-Rust LZ4 compressor with the C build of
//! its release. The first byte picks the compressor and output capacity;
//! the rest is a small program expanded into the compressor input, so short
//! fuzz inputs reach the 64 KiB window and long runs. Run from the precomp2
//! root: `cargo +nightly fuzz run lz4_reference`.

use libfuzzer_sys::fuzz_target;
use lz4_reference::{CVersion, compress_hc};
use precomp2::codec::lz4::{Compressor, compress_bound};

const MAX_INPUT: usize = 1 << 18;

fn byte(ops: &mut std::slice::Iter<u8>) -> u8 {
  ops.next().copied().unwrap_or(0)
}

fn word(ops: &mut std::slice::Iter<u8>) -> usize {
  usize::from(u16::from_be_bytes([byte(ops), byte(ops)]))
}

/// Ops, by `op % 5`: 0 literals, 1 byte run, 2 back-reference, 3
/// back-reference at the window edge, 4 pseudo-random noise.
fn expand(program: &[u8]) -> Vec<u8> {
  let mut ops = program.iter();
  let mut out = Vec::new();
  while out.len() < MAX_INPUT {
    let Some(&op) = ops.next() else { break };
    let arg = usize::from(op / 5);
    match op % 5 {
      0 => {
        for _ in 0..=arg {
          out.push(byte(&mut ops));
        }
      }
      1 => {
        let byte = byte(&mut ops);
        let len = (word(&mut ops) + 1) * (1 + arg % 8);
        out.extend(std::iter::repeat_n(byte, len));
      }
      2 | 3 => {
        let dist = if op % 5 == 2 { word(&mut ops) + 1 } else { 65_504 + arg };
        let len = 4 + usize::from(byte(&mut ops)) * (1 + arg % 4);
        if dist <= out.len() {
          for _ in 0..len {
            out.push(out[out.len() - dist]);
          }
        }
      }
      _ => {
        let mut state = 0x9e37_79b9_7f4a_7c15u64 ^ u64::from(byte(&mut ops));
        for _ in 0..=word(&mut ops) {
          state ^= state << 13;
          state ^= state >> 7;
          state ^= state << 17;
          out.push(state as u8);
        }
      }
    }
  }
  out.truncate(MAX_INPUT);
  out
}

fuzz_target!(|data: &[u8]| {
  let Some((&control, program)) = data.split_first() else {
    return;
  };
  let input = expand(program);
  let rust = Compressor::ALL[usize::from(control) % Compressor::ALL.len()];
  let c_version = CVersion::from_name(rust.release()).expect("C build of every release");
  let level = rust.level();
  let bound = compress_bound(input.len()).max(1);
  let full = rust.compress(&input, bound);
  assert_eq!(full, compress_hc(c_version, &input, bound, level), "full capacity");
  let full_len = full.expect("full capacity always fits").len();
  let capacity = match (usize::from(control) / Compressor::ALL.len()) % 3 {
    0 => return,
    1 => full_len,
    _ => full_len - 1,
  };
  assert_eq!(
    rust.compress(&input, capacity),
    compress_hc(c_version, &input, capacity, level),
    "capacity {capacity}"
  );
});
