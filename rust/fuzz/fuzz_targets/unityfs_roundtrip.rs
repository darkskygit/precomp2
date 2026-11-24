#![no_main]
//! UnityFS container codec on fuzzer-built archives: encoding never fails and
//! decoding restores the exact bytes. The input describes an archive (header
//! fields, blocks and their compressor); the remaining bytes are flips applied
//! behind the magic, so both well-formed and broken layouts are reached.

use libfuzzer_sys::fuzz_target;
use precomp2::{
  DecodeConfig, EncodeConfig,
  codec::{
    analyze_unityfs,
    lz4::{Compressor, compress_bound},
  },
  decode_bytes, encode_bytes,
};

const MAGIC: &[u8] = b"UnityFS\0";
const REVISIONS: [&str; 4] = ["2017.4.39f1", "2021.3.35f1", "6000.3.8f1", ""];

struct Input<'a>(std::slice::Iter<'a, u8>);

impl Input<'_> {
  fn u8(&mut self) -> u8 {
    self.0.next().copied().unwrap_or(0)
  }

  fn u16(&mut self) -> u16 {
    u16::from_be_bytes([self.u8(), self.u8()])
  }
}

/// Our compressors, or a foreign one (`lz4_flex`) no candidate reproduces.
fn compress(selector: u8, raw: &[u8]) -> Vec<u8> {
  match Compressor::ALL.get(usize::from(selector) % (Compressor::ALL.len() + 1)) {
    Some(compressor) => compressor
      .compress(raw, compress_bound(raw.len()))
      .expect("full capacity always fits"),
    None => lz4_flex::block::compress(raw),
  }
}

fn build(input: &mut Input) -> Vec<u8> {
  let archive_version = 6 + u32::from(input.u8() % 3);
  let revision = REVISIONS[usize::from(input.u8()) % REVISIONS.len()];
  // Compression bits, 0x40 combined, 0x80 info at end, 0x200 padding.
  let flags = u32::from(input.u16()) & 0x2ff;
  let selector = input.u8();

  let mut info = vec![0u8; 16];
  let block_count = input.u8() % 4;
  info.extend_from_slice(&u32::from(block_count).to_be_bytes());
  let mut storage = Vec::new();
  for _ in 0..block_count {
    let flag = [0u16, 2, 3][usize::from(input.u8()) % 3];
    let seed = input.u8();
    let raw: Vec<u8> = (0..usize::from(input.u16() % 4096))
      .map(|i| if i % 7 < 4 { seed } else { (i as u8).wrapping_mul(seed) })
      .collect();
    let stored = if flag == 0 { raw.clone() } else { compress(selector, &raw) };
    info.extend_from_slice(&(raw.len() as u32).to_be_bytes());
    info.extend_from_slice(&(stored.len() as u32).to_be_bytes());
    info.extend_from_slice(&flag.to_be_bytes());
    storage.extend_from_slice(&stored);
  }
  info.extend_from_slice(&0u32.to_be_bytes());
  let info_bytes = match flags & 0x3f {
    2 | 3 => compress(selector, &info),
    _ => info.clone(),
  };

  let mut file = MAGIC.to_vec();
  file.extend_from_slice(&archive_version.to_be_bytes());
  file.extend_from_slice(b"5.x.x\0");
  file.extend_from_slice(revision.as_bytes());
  file.push(0);
  let size_offset = file.len();
  file.extend_from_slice(&0u64.to_be_bytes());
  file.extend_from_slice(&(info_bytes.len() as u32).to_be_bytes());
  file.extend_from_slice(&(info.len() as u32).to_be_bytes());
  file.extend_from_slice(&flags.to_be_bytes());
  if archive_version >= 7 {
    file.resize(file.len().next_multiple_of(16), 0);
  }
  if flags & 0x80 == 0 {
    file.extend_from_slice(&info_bytes);
  }
  if flags & 0x200 != 0 {
    file.resize(file.len().next_multiple_of(16), 0);
  }
  file.extend_from_slice(&storage);
  if flags & 0x80 != 0 {
    file.extend_from_slice(&info_bytes);
  }
  let size = file.len() as u64;
  file[size_offset..size_offset + 8].copy_from_slice(&size.to_be_bytes());
  file
}

fuzz_target!(|data: &[u8]| {
  let mut input = Input(data.iter());
  let mut file = build(&mut input);
  while input.0.len() >= 3 {
    let pos = MAGIC.len() + usize::from(input.u16()) % (file.len() - MAGIC.len());
    file[pos] ^= input.u8();
  }
  analyze_unityfs(&file);
  let encoded = encode_bytes(&file, &EncodeConfig::default()).expect("encode");
  assert!(decode_bytes(&encoded, &DecodeConfig::default()).expect("decode") == file);
});
