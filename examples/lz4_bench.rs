//! Single-core encode/restore throughput of the byte-exact LZ4HC compressors,
//! measured on decompressed bytes. Run: `cargo run --release --example
//! lz4_bench`.

use std::time::Instant;

use precomp2::codec::lz4::{Compressor, compress_bound};

fn sample_data() -> Vec<u8> {
  // Game-asset-like mix: repeated records plus binary noise.
  let mut state = 0x243f_6a88_85a3_08d3u64;
  let mut v = Vec::with_capacity(64 << 20);
  while v.len() < (64 << 20) {
    state ^= state << 13;
    state ^= state >> 7;
    state ^= state << 17;
    match state % 4 {
      0 => v.extend_from_slice(b"SerializeFileHeader asset bundle block record; "),
      1 => v.extend_from_slice(&state.to_le_bytes()),
      2 => v.extend(std::iter::repeat_n(state as u8, 32)),
      _ => v.push((state >> 29) as u8),
    }
  }
  v
}

fn main() {
  let data = sample_data();
  println!("input: {} MB (decompressed bytes)", data.len() / 1_000_000);
  for compressor in Compressor::ALL {
    let bound = compress_bound(data.len());
    let t0 = Instant::now();
    let compressed = compressor.compress(&data, bound).expect("compress");
    let enc = t0.elapsed().as_secs_f64();
    // Restore = recompress with the original (tight) capacity, as decode does.
    let t1 = Instant::now();
    let restored = compressor.compress(&data, compressed.len()).expect("restore");
    let dec = t1.elapsed().as_secs_f64();
    assert_eq!(restored, compressed);
    println!(
      "{compressor:?}: encode {:.1} MB/s, restore {:.1} MB/s (ratio {:.3})",
      data.len() as f64 / enc / 1e6,
      data.len() as f64 / dec / 1e6,
      compressed.len() as f64 / data.len() as f64
    );
  }
}
