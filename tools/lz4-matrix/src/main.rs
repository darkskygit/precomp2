//! Writes the LZ4 regions precomp2 leaves as stored as `<n>.raw` / `<n>.lz4`
//! pairs for `matrix.c`. Reads UnityFS paths from stdin; writes at most
//! `LZ4_MATRIX_BLOCKS` (default 200) pairs.

use std::{collections::BTreeMap, io::BufRead, path::PathBuf};

use precomp2::codec::analyze_unityfs;

fn main() {
  let out = PathBuf::from(std::env::args().nth(1).expect("usage: dump-misses <out-dir> < file-list"));
  let max: usize = std::env::var("LZ4_MATRIX_BLOCKS").ok().and_then(|v| v.parse().ok()).unwrap_or(200);
  // engine version -> (LZ4 regions, unreproduced)
  let mut per_version = BTreeMap::<String, (usize, usize)>::new();
  let mut written = 0;
  for path in std::io::stdin()
    .lock()
    .lines()
    .map(|l| l.expect("file list"))
    .filter(|l| !l.is_empty())
  {
    let data = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let Some(report) = analyze_unityfs(&data) else {
      eprintln!("skipped, not a reproducible UnityFS layout: {path}");
      continue;
    };
    let (regions, misses) = per_version.entry(report.engine_version).or_default();
    *regions += report.lz4_regions.len();
    for region in report.lz4_regions.iter().filter(|r| r.compressor.is_none()) {
      *misses += 1;
      let compressed = &data[region.range.clone()];
      let Ok(raw) = lz4_flex::block::decompress(compressed, region.decompressed_len) else {
        continue;
      };
      if written < max {
        std::fs::write(out.join(format!("{written:05}.lz4")), compressed).expect("write block");
        std::fs::write(out.join(format!("{written:05}.raw")), raw).expect("write block");
        written += 1;
      }
    }
  }
  for (version, (regions, misses)) in per_version {
    eprintln!("{version}: {misses}/{regions} LZ4 regions not reproduced");
  }
  eprintln!("{written} blocks written to {}", out.display());
}
