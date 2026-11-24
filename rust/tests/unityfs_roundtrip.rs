//! Roundtrip tests for the UnityFS container codec. Synthetic bundles reuse
//! the header variants of real archives (archive version, Unity revision,
//! flags) plus the rare ones; every LZ4 block is produced by the C reference
//! build the Unity version maps to, with synthetic payloads.

use lz4_reference::{CVersion, compress_hc as c_compress_hc};
use precomp2::{
  DecodeConfig, EncodeConfig, Pcf2File, SegmentKind,
  codec::{
    UnityFsReport, analyze_unityfs,
    lz4::{Compressor, compress_bound},
  },
  decode_bytes, encode_bytes,
};

struct Block {
  /// Compression flag written into the blocks info table (0/2/3).
  flag: u16,
  payload: Vec<u8>,
  /// Stored bytes produced by a compressor no candidate reproduces.
  foreign: Option<Vec<u8>>,
}

impl Block {
  fn new(flag: u16, payload: Vec<u8>) -> Self {
    Block {
      flag,
      payload,
      foreign: None,
    }
  }

  fn foreign(payload: Vec<u8>) -> Self {
    Block {
      flag: 3,
      foreign: Some(lz4_flex::block::compress(&payload)),
      payload,
    }
  }
}

struct BundleSpec {
  archive_version: u32,
  unity_version: &'static str,
  /// Blocks-info compression in the low 6 bits, 0x40 = blocks and directory
  /// combined, 0x80 = info at end, 0x200 = blocks-info padding.
  flags: u32,
  /// Produces every LZ4 region through its C reference build.
  compressor: Compressor,
  blocks: Vec<Block>,
}

/// The compressor a Unity version uses (RFC 12 §5.9).
fn engine_compressor(unity_version: &str) -> Compressor {
  if unity_version.starts_with("2017") {
    Compressor::R131_L16
  } else if unity_version.starts_with("2021") {
    Compressor::V1_9_1_L12
  } else {
    Compressor::V1_10_0_L12
  }
}

fn hits(report: &UnityFsReport) -> usize {
  report.lz4_regions.iter().filter(|r| r.compressor.is_some()).count()
}

impl BundleSpec {
  fn compress(&self, raw: &[u8]) -> Vec<u8> {
    let c_version = CVersion::from_name(self.compressor.release()).expect("C build of every release");
    c_compress_hc(c_version, raw, compress_bound(raw.len()), self.compressor.level()).expect("c reference")
  }

  fn stored(&self, block: &Block) -> Vec<u8> {
    match (&block.foreign, block.flag) {
      (Some(bytes), _) => bytes.clone(),
      (None, 0) => block.payload.clone(),
      (None, _) => self.compress(&block.payload),
    }
  }

  /// Offset of the blocks info when it is stored right after the header.
  fn header_len(&self) -> usize {
    let len = 8 + 4 + 6 + self.unity_version.len() + 1 + 8 + 4 + 4 + 4;
    if self.archive_version >= 7 { len.next_multiple_of(16) } else { len }
  }

  fn build(&self) -> Vec<u8> {
    let stored: Vec<Vec<u8>> = self.blocks.iter().map(|b| self.stored(b)).collect();
    let mut info = vec![0u8; 16]; // uncompressed data hash
    info.extend_from_slice(&(self.blocks.len() as u32).to_be_bytes());
    for (block, bytes) in self.blocks.iter().zip(&stored) {
      info.extend_from_slice(&(block.payload.len() as u32).to_be_bytes());
      info.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
      info.extend_from_slice(&block.flag.to_be_bytes());
    }
    info.extend_from_slice(&0u32.to_be_bytes()); // nodes count
    let info_bytes = match self.flags & 0x3f {
      0 => info.clone(),
      _ => self.compress(&info),
    };

    let mut file = b"UnityFS\0".to_vec();
    file.extend_from_slice(&self.archive_version.to_be_bytes());
    // Real bundles carry the "5.x.x" format placeholder here and the engine
    // version in the revision string.
    file.extend_from_slice(b"5.x.x\0");
    file.extend_from_slice(self.unity_version.as_bytes());
    file.push(0);
    let size_offset = file.len();
    file.extend_from_slice(&0u64.to_be_bytes());
    file.extend_from_slice(&(info_bytes.len() as u32).to_be_bytes());
    file.extend_from_slice(&(info.len() as u32).to_be_bytes());
    file.extend_from_slice(&self.flags.to_be_bytes());
    file.resize(self.header_len(), 0);
    if self.flags & 0x80 == 0 {
      file.extend_from_slice(&info_bytes);
    }
    if self.flags & 0x200 != 0 {
      file.resize(file.len().next_multiple_of(16), 0);
    }
    stored.iter().for_each(|bytes| file.extend_from_slice(bytes));
    if self.flags & 0x80 != 0 {
      file.extend_from_slice(&info_bytes);
    }
    let size = file.len() as u64;
    file[size_offset..size_offset + 8].copy_from_slice(&size.to_be_bytes());
    file
  }
}

fn structured_payload(seed: u64, len: usize) -> Vec<u8> {
  let mut state = seed;
  let mut v = Vec::with_capacity(len);
  while v.len() < len {
    state ^= state << 13;
    state ^= state >> 7;
    state ^= state << 17;
    if state.is_multiple_of(3) {
      v.extend_from_slice(b"UnityFS serialized asset data with repeated structure; ");
    } else {
      v.push((state >> 33) as u8);
    }
  }
  v.truncate(len);
  v
}

fn roundtrip(original: &[u8]) -> Pcf2File {
  let encoded = encode_bytes(original, &EncodeConfig::default()).expect("encode");
  let decoded = decode_bytes(&encoded, &DecodeConfig::default()).expect("decode");
  assert!(decoded == original, "roundtrip mismatch");
  Pcf2File::decode(encoded.as_slice()).expect("pcf2")
}

fn unityfs_segments(pcf2: &Pcf2File) -> impl Iterator<Item = &precomp2::Pcf2Segment> {
  pcf2.segments.iter().filter(|s| s.kind == SegmentKind::UnityFs as u8)
}

#[test]
fn converts_every_reproducible_region_across_layouts() {
  let variants = [
    // Header variants seen in real archives.
    (6, "2017.4.39f1", 0x43),
    (7, "2021.1.16f1", 0x43),
    (8, "2021.3.35f1", 0x243),
    (8, "2022.3.62f3", 0x243),
    (8, "6000.3.8f1", 0x243),
    // Rare variants: uncompressed or LZ4 info, info at the end, padding.
    (6, "2017.4.39f1", 0),
    (7, "2021.3.45f1", 2),
    (7, "6000.0.23f1", 0x80 | 0x200 | 3),
    (6, "2017.4.39f1", 0x80),
  ];
  for (archive_version, unity_version, flags) in variants {
    let spec = BundleSpec {
      archive_version,
      unity_version,
      flags,
      compressor: engine_compressor(unity_version),
      blocks: vec![
        Block::new(0, structured_payload(1, 512)),
        Block::new(3, structured_payload(2, 60_000)),
        Block::new(2, structured_payload(3, 30_000)),
      ],
    };
    let original = spec.build();
    let report = analyze_unityfs(&original).expect("analyze");
    let lz4_regions = 2 + usize::from(flags & 0x3f != 0);
    assert_eq!(report.engine_version, unity_version);
    assert_eq!(report.lz4_regions.len(), lz4_regions, "{unity_version} {flags:#x}");
    assert!(
      report.lz4_regions.iter().all(|r| r.compressor == Some(spec.compressor)),
      "{unity_version} {flags:#x}"
    );

    let pcf2 = roundtrip(&original);
    assert_eq!(unityfs_segments(&pcf2).count(), lz4_regions);
  }
}

#[test]
fn keeps_foreign_and_implausible_blocks_verbatim() {
  let spec = BundleSpec {
    archive_version: 7,
    unity_version: "2022.3.20f1",
    flags: 3,
    compressor: Compressor::V1_10_0_L12,
    blocks: vec![
      Block::new(3, structured_payload(0xabcd, 80_000)),
      Block::foreign(structured_payload(0xef01, 30_000)),
    ],
  };
  let foreign = spec.stored(&spec.blocks[1]);
  let pcf2 = roundtrip(&spec.build());
  assert_eq!(unityfs_segments(&pcf2).count(), 2, "blocks info and the reproducible block convert");
  let literals: Vec<u8> = pcf2
    .segments
    .iter()
    .filter(|s| s.kind == SegmentKind::Lit as u8)
    .flat_map(|s| s.data.iter().copied())
    .collect();
  assert!(literals.windows(foreign.len()).any(|w| w == foreign.as_slice()));

  // A block claiming an impossible decompressed size is never decompressed.
  let spec = BundleSpec {
    archive_version: 6,
    unity_version: "6000.3.8f1",
    flags: 0x40,
    compressor: Compressor::V1_10_0_L12,
    blocks: vec![Block::new(3, structured_payload(5, 10_000))],
  };
  let mut original = spec.build();
  let size_field = spec.header_len() + 16 + 4;
  original[size_field..size_field + 4].copy_from_slice(&u32::MAX.to_be_bytes());
  let report = analyze_unityfs(&original).expect("layout still parses");
  assert_eq!((report.lz4_regions.len(), hits(&report)), (1, 0));
  assert_eq!(unityfs_segments(&roundtrip(&original)).count(), 0);
}

#[test]
fn candidates_follow_the_archive_and_give_up_on_unknown_compressors() {
  // The header claims 2017, but the blocks come from 1.10.0: the fallback
  // finds it. (The tiny blocks info compresses the same under r131.)
  let spec = BundleSpec {
    archive_version: 6,
    unity_version: "2017.4.39f1",
    flags: 0x43,
    compressor: Compressor::V1_10_0_L12,
    blocks: (1..4).map(|i| Block::new(3, structured_payload(i, 20_000))).collect(),
  };
  let report = analyze_unityfs(&spec.build()).expect("analyze");
  let blocks = &report.lz4_regions[1..];
  assert!(blocks.iter().all(|r| r.compressor == Some(Compressor::V1_10_0_L12)), "{report:?}");

  // No compressor reproduces the first regions: the rest is not tried, even
  // a block that would match.
  let mut blocks: Vec<Block> = (0..4).map(|i| Block::foreign(structured_payload(i, 20_000))).collect();
  blocks.push(Block::new(3, structured_payload(9, 20_000)));
  let spec = BundleSpec {
    archive_version: 7,
    unity_version: "2019.4.40f1",
    flags: 0x40,
    compressor: Compressor::V1_10_0_L12,
    blocks,
  };
  let original = spec.build();
  let report = analyze_unityfs(&original).expect("analyze");
  assert_eq!((report.lz4_regions.len(), hits(&report)), (5, 0));
  assert_eq!(unityfs_segments(&roundtrip(&original)).count(), 0);
}

#[test]
fn non_unityfs_and_truncated_inputs_stay_literal() {
  let spec = BundleSpec {
    archive_version: 8,
    unity_version: "6000.3.8f1",
    flags: 0x243,
    compressor: Compressor::V1_10_0_L12,
    blocks: vec![Block::new(3, structured_payload(9, 20_000))],
  };
  let bundle = spec.build();
  let truncated = &bundle[..bundle.len() - 1];
  for input in [&b"random bytes that are definitely not UnityFS"[..], truncated, &bundle[..40]] {
    assert!(analyze_unityfs(input).is_none());
    assert_eq!(unityfs_segments(&roundtrip(input)).count(), 0);
  }
}

/// Real-archive acceptance: `PRECOMP2_UNITYFS_FILES` names a file listing one
/// UnityFS path per line. Run with
/// `cargo test --release --test unityfs_roundtrip -- --ignored --nocapture`.
#[test]
#[ignore = "needs PRECOMP2_UNITYFS_FILES"]
fn real_archives_roundtrip() {
  let list = std::env::var("PRECOMP2_UNITYFS_FILES").expect("PRECOMP2_UNITYFS_FILES");
  // engine version -> files, regions, hit regions, bytes, hit bytes, hits per
  // compressor
  let mut per_version = std::collections::BTreeMap::<String, ([u64; 5], std::collections::BTreeMap<String, u64>)>::new();
  let started = std::time::Instant::now();
  let mut input_bytes = 0;
  for path in std::fs::read_to_string(list).expect("file list").lines().filter(|l| !l.is_empty()) {
    let original = std::fs::read(path).expect(path);
    let report = analyze_unityfs(&original).unwrap_or_else(|| panic!("not a reproducible UnityFS layout: {path}"));
    roundtrip(&original);
    input_bytes += original.len();
    let (totals, by_compressor) = per_version.entry(report.engine_version).or_default();
    totals[0] += 1;
    for region in &report.lz4_regions {
      let bytes = region.range.len() as u64;
      totals[1] += 1;
      totals[3] += bytes;
      if let Some(c) = region.compressor {
        totals[2] += 1;
        totals[4] += bytes;
        *by_compressor.entry(format!("{c:?}")).or_default() += 1;
      }
    }
  }
  for (version, ([files, regions, hit, bytes, bytes_hit], by_compressor)) in per_version {
    eprintln!(
      "{version}: {files} files, regions {hit}/{regions}, bytes {:.4} ({bytes_hit}/{bytes}), by compressor {by_compressor:?}",
      bytes_hit as f64 / bytes.max(1) as f64
    );
  }
  eprintln!(
    "{:.1} MB in {:.1?} (analyze + encode + decode)",
    input_bytes as f64 / 1e6,
    started.elapsed()
  );
}
