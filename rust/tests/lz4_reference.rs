//! Byte-exact comparison of every pure-Rust LZ4 compressor with the C liblz4
//! build of its release: a deterministic boundary corpus and a seeded random
//! sweep. Coverage-guided fuzzing is in `fuzz/fuzz_targets/lz4_reference.rs`.

use lz4_reference::{CVersion, compress_bound as c_compress_bound, compress_hc as c_compress_hc};
use precomp2::codec::lz4::{Compressor, compress_bound};

/// The optimal-parser compressors; the hash chain walks long runs slowly.
const OPTIMAL: [Compressor; 2] = [Compressor::V1_10_0_L12, Compressor::V1_9_1_L12];

struct Rng(u64);

impl Rng {
  fn next(&mut self) -> u64 {
    self.0 ^= self.0 << 13;
    self.0 ^= self.0 >> 7;
    self.0 ^= self.0 << 17;
    self.0
  }

  fn below(&mut self, n: usize) -> usize {
    (self.next() % n.max(1) as u64) as usize
  }

  fn bytes(&mut self, len: usize) -> Vec<u8> {
    (0..len).map(|_| self.next() as u8).collect()
  }
}

fn env_or<T: std::str::FromStr>(name: &str, default: T) -> T {
  std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// Compares Rust and C at the full bound and, when `limited`, at the exact
/// output size and one byte below it.
fn check(input: &[u8], limited: bool, compressors: &[Compressor]) {
  assert_eq!(compress_bound(input.len()), c_compress_bound(CVersion::R131, input.len()));
  let bound = compress_bound(input.len()).max(1);
  for &rust in compressors {
    let c_version = CVersion::from_name(rust.release()).expect("C build of every release");
    let level = rust.level();
    let full = rust.compress(input, bound).expect("rust full capacity");
    assert_eq!(
      Some(&full),
      c_compress_hc(c_version, input, bound, level).as_ref(),
      "full-capacity mismatch ({c_version:?}, len {})",
      input.len()
    );
    assert_eq!(lz4_flex::block::decompress(&full, input.len()).expect("decompress"), input);
    if limited {
      for capacity in [full.len(), full.len() - 1] {
        let rust_out = rust.compress(input, capacity);
        assert_eq!(rust_out.is_some(), capacity == full.len(), "{c_version:?} capacity {capacity}");
        assert_eq!(
          rust_out,
          c_compress_hc(c_version, input, capacity, level),
          "limited-capacity mismatch ({c_version:?}, len {}, capacity {capacity})",
          input.len()
        );
      }
    }
  }
}

fn boundary_corpus() -> Vec<Vec<u8>> {
  let mut rng = Rng(0x853c_49e6_748f_ea9b);
  let mut inputs = Vec::new();
  // Exhaustive small lengths catch parser boundary bugs.
  for len in 0..=96usize {
    inputs.push(
      (0..len)
        .map(|_| match rng.below(4) {
          0 => b'a' + rng.below(8) as u8,
          1 => rng.next() as u8,
          _ => (rng.next() % 3) as u8,
        })
        .collect(),
    );
  }
  // Window and price-table boundaries.
  for len in [255, 256, 257, 4095, 4096, 4097, 65534, 65535, 65536, 65537, 131071, 131072] {
    for kind in 0..3 {
      let mut v = Vec::with_capacity(len);
      while v.len() < len {
        match kind {
          0 => v.extend_from_slice(b"the quick brown fox jumps over the lazy dog. "),
          1 => {
            v.extend(std::iter::repeat_n(rng.next() as u8, 1 + rng.below(700)));
            v.push(rng.next() as u8);
          }
          _ => v.extend((0..64).map(|_| b"aeiourstln "[rng.below(11)])),
        }
      }
      v.truncate(len);
      inputs.push(v);
    }
  }
  // Hash-chain lazy parser: a 30-byte match m1 at the target, a longer m2 from
  // 5 bytes in, and an m3 found from m2's end that starts either right at
  // m1.end() + 3 (three ascending matches, m2 still overlapping m1) or inside
  // m1 (m3 replaces m2).
  for m3_start in [33, 25] {
    let target = rng.bytes(200);
    let distinct = |b: u8| b.wrapping_add(1);
    let mut v = rng.bytes(64);
    v.extend_from_slice(&target[..30]);
    v.push(distinct(target[30]));
    v.extend(rng.bytes(64));
    v.push(distinct(target[4]));
    v.extend_from_slice(&target[5..45]);
    v.push(distinct(target[45]));
    v.extend(rng.bytes(64));
    v.push(distinct(target[m3_start - 1]));
    v.extend_from_slice(&target[m3_start..m3_start + 46]);
    v.push(distinct(target[m3_start + 46]));
    v.extend(rng.bytes(64));
    v.extend_from_slice(&target);
    inputs.push(v);
  }
  inputs
}

/// Inputs for the optimal parser's run shortcut; checked against [`OPTIMAL`]
/// only since the hash chain walks these runs slowly.
fn long_run_corpus() -> Vec<Vec<u8>> {
  let mut rng = Rng(0x2545_f491_4f6c_dd1d);
  let mut inputs = Vec::new();
  // Single-byte runs longer than the 64 KiB window, starting at different
  // window offsets and optionally resumed after a gap.
  for run in [65_536, 140_000] {
    for prefix in [13, 40_000] {
      for resume in [false, true] {
        let mut v = rng.bytes(prefix);
        v.extend(std::iter::repeat_n(0xaa, run));
        if resume {
          let gap = 1 + rng.below(32);
          v.extend(rng.bytes(gap));
          v.extend(std::iter::repeat_n(0xaa, 1 + rng.below(run)));
        }
        v.extend(rng.bytes(64));
        inputs.push(v);
      }
    }
  }
  // A short run whose last 4-byte position is exactly the window floor seen
  // from the start of a later run of the same byte: the only way the chain
  // walk reaches the repeat-candidate floor (differs between 1.9.1 and
  // 1.10.0) and the distance guard.
  for (first_run, shift) in [(5, 0), (8, 0), (8, 1), (8, usize::MAX), (40, 0)] {
    let mut v = rng.bytes(17);
    v.extend(std::iter::repeat_n(0xbb, first_run));
    let floor = v.len() - 4;
    let gap = (floor + 65_535).wrapping_add(shift) - v.len();
    v.extend((0..gap).map(|_| (rng.next() % 0xbb) as u8));
    v.extend(std::iter::repeat_n(0xbb, 100));
    v.extend(rng.bytes(64));
    inputs.push(v);
  }
  inputs
}

/// Random input with mixed structure, including occasional runs longer than
/// the window.
fn random_input(rng: &mut Rng) -> Vec<u8> {
  let len = match rng.below(10) {
    0 => rng.below(64),
    1..=4 => 64 + rng.below(4096),
    5..=7 => 4096 + rng.below(65536),
    8 => 65536 + rng.below(65536),
    _ => 131072 + rng.below(262144),
  };
  let mut v = Vec::with_capacity(len);
  while v.len() < len {
    match rng.below(64) {
      0..=9 => v.push(rng.next() as u8),
      10..=19 => v.extend_from_slice(&rng.next().to_le_bytes()),
      20..=29 => v.extend(std::iter::repeat_n(rng.next() as u8, 1 + rng.below(1000))),
      30..=39 => v.extend_from_slice(b"archive block with structure and repeated words; "),
      40..=52 => v.push((rng.next() % 5) as u8),
      53..=62 => {
        let word = rng.next();
        v.extend((0..8).map(|i| (word >> (i * 8)) as u8));
      }
      _ => v.extend(std::iter::repeat_n(rng.next() as u8, 65_536 + rng.below(100_000))),
    }
  }
  v.truncate(len);
  v
}

#[test]
fn vendored_versions_match_releases() {
  for &v in CVersion::ALL {
    assert_eq!(v.version_number(), v.expected_version_number(), "{}", v.name());
  }
}

#[test]
fn boundary_corpus_matches_c() {
  for input in boundary_corpus() {
    check(&input, true, &Compressor::ALL);
  }
  for input in long_run_corpus() {
    check(&input, true, &OPTIMAL);
  }
}

#[test]
fn random_sweep_matches_c() {
  let iters = env_or("LZ4_SWEEP_ITERS", if cfg!(debug_assertions) { 300 } else { 20_000 });
  let seed = env_or("LZ4_SWEEP_SEED", 0x9e37_79b9_7f4a_7c15u64);
  let mut rng = Rng(seed);
  for _ in 0..iters {
    let input = random_input(&mut rng);
    let limited = rng.below(4) == 0;
    check(&input, limited, &Compressor::ALL);
  }
}
