//! UnityFS container codec: recompresses LZ4/LZ4HC-compressed storage blocks
//! (and the blocks info) with the byte-exact LZ4HC compressors in
//! [`super::lz4`], storing the decompressed bytes plus the compressor id.
//!
//! Reversibility does not rely on the compressors being byte-exact: a block is
//! only accepted after its recompression matches the original bytes exactly,
//! so a mismatch only costs compression ratio, never correctness.

use std::{borrow::Cow, ops::Range};

use super::{
  DecodeConfig, EncodeConfig,
  budget::{DecodeBudget, EncodeBudget},
  lz4::Compressor,
};
use crate::{
  error::{Error, Result},
  pcf2::{Pcf2Segment, SegmentKind},
};

const MAGIC: &[u8; 8] = b"UnityFS\0";
const COMPRESSION_MASK: u32 = 0x3f;
const BLOCKS_INFO_AT_END: u32 = 0x80;
const BLOCK_INFO_NEED_PADDING: u32 = 0x200;

const COMPRESSION_NONE: u32 = 0;
const COMPRESSION_LZ4: u32 = 2;
const COMPRESSION_LZ4HC: u32 = 3;

/// LZ4 expands at most about 255:1; larger claimed sizes are rejected before
/// allocating.
const MAX_LZ4_RATIO: usize = 255;

fn is_lz4(compression: u32) -> bool {
  matches!(compression, COMPRESSION_LZ4 | COMPRESSION_LZ4HC)
}

struct Reader<'a> {
  buf: &'a [u8],
  pos: usize,
}

impl<'a> Reader<'a> {
  fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
    let chunk = *self.buf.get(self.pos..)?.first_chunk::<N>()?;
    self.pos += N;
    Some(chunk)
  }

  fn u16(&mut self) -> Option<u16> {
    self.take().map(u16::from_be_bytes)
  }

  fn u32(&mut self) -> Option<u32> {
    self.take().map(u32::from_be_bytes)
  }

  fn u64(&mut self) -> Option<u64> {
    self.take().map(u64::from_be_bytes)
  }

  fn cstr(&mut self) -> Option<&'a [u8]> {
    let rest = self.buf.get(self.pos..)?;
    let len = memchr::memchr(0, rest)?;
    self.pos += len + 1;
    Some(&rest[..len])
  }
}

/// A byte range of the archive; `lz4_size` is the decompressed size when the
/// range is an LZ4 block.
struct Region {
  range: Range<usize>,
  lz4_size: Option<usize>,
}

struct Layout {
  /// The unity_revision string (unity_version is the "5.x.x" format
  /// placeholder), or unity_version when the revision is empty.
  engine_version: String,
  /// Every byte of the archive, in file order.
  regions: Vec<Region>,
}

fn decompress(compressed: &[u8], size: usize) -> Option<Vec<u8>> {
  if size / MAX_LZ4_RATIO > compressed.len() {
    return None;
  }
  lz4_flex::block::decompress(compressed, size).ok().filter(|raw| raw.len() == size)
}

/// Parses the archive structure. Returns `None` when the layout is not one we
/// can reproduce byte for byte (unsupported compression, truncated fields,
/// unexpected sizes); the caller then keeps the file as plain segments.
fn parse_layout(input: &[u8]) -> Option<Layout> {
  let mut header = Reader { buf: input, pos: 0 };
  if header.take::<8>()? != *MAGIC {
    return None;
  }
  let archive_version = header.u32()?;
  let unity_version = header.cstr()?;
  let unity_revision = header.cstr()?;
  let _size = header.u64()?;
  let info_compressed_size = header.u32()? as usize;
  let info_size = header.u32()? as usize;
  let flags = header.u32()?;
  let mut header_end = header.pos;
  if archive_version >= 7 {
    header_end = header_end.next_multiple_of(16);
  }

  let info_at_end = flags & BLOCKS_INFO_AT_END != 0;
  let info_range = if info_at_end {
    input.len().checked_sub(info_compressed_size)?..input.len()
  } else {
    header_end..header_end.checked_add(info_compressed_size)?
  };
  if info_range.is_empty() || info_range.start < header_end || info_range.end > input.len() {
    return None;
  }
  let mut storage_start = if info_at_end { header_end } else { info_range.end };
  if flags & BLOCK_INFO_NEED_PADDING != 0 {
    storage_start = storage_start.next_multiple_of(16);
  }
  let storage_end = if info_at_end { info_range.start } else { input.len() };
  if storage_start > storage_end {
    return None;
  }

  let info_compression = flags & COMPRESSION_MASK;
  let info_compressed = &input[info_range.clone()];
  let info = match info_compression {
    COMPRESSION_NONE if info_compressed.len() == info_size => Cow::Borrowed(info_compressed),
    c if is_lz4(c) => Cow::Owned(decompress(info_compressed, info_size)?),
    _ => return None,
  };
  let info_lz4_size = is_lz4(info_compression).then_some(info_size);

  let mut regions = Vec::new();
  if info_at_end {
    regions.push(Region {
      range: 0..storage_start,
      lz4_size: None,
    });
  } else {
    regions.push(Region {
      range: 0..info_range.start,
      lz4_size: None,
    });
    regions.push(Region {
      range: info_range.clone(),
      lz4_size: info_lz4_size,
    });
    regions.push(Region {
      range: info_range.end..storage_start,
      lz4_size: None,
    });
  }

  // hash[16], block count, then the storage block table; the node directory
  // that follows is not needed to locate the block bytes.
  let mut table = Reader { buf: &info, pos: 16 };
  let block_count = table.u32()? as usize;
  if block_count > info.len() / 10 {
    return None;
  }
  let mut offset = storage_start;
  for _ in 0..block_count {
    let size = table.u32()? as usize;
    let compressed_size = table.u32()? as usize;
    let block_flags = table.u16()?;
    let end = offset.checked_add(compressed_size).filter(|&end| end <= storage_end)?;
    regions.push(Region {
      range: offset..end,
      lz4_size: is_lz4(u32::from(block_flags) & COMPRESSION_MASK).then_some(size),
    });
    offset = end;
  }
  if offset != storage_end {
    return None;
  }
  if info_at_end {
    regions.push(Region {
      range: info_range,
      lz4_size: info_lz4_size,
    });
  }

  let engine_version = if unity_revision.is_empty() { unity_version } else { unity_revision };
  Some(Layout {
    engine_version: String::from_utf8_lossy(engine_version).into_owned(),
    regions,
  })
}

/// The compressor each engine version uses (RFC 12 §5.9), by version
/// prefix. Other versions try [`Compressor::ALL`], newest first.
const ENGINE_COMPRESSORS: [(&str, Compressor); 4] = [
  ("2017.", Compressor::R131_L16),
  ("2021.", Compressor::V1_9_1_L12),
  ("2022.", Compressor::V1_10_0_L12),
  ("6000.", Compressor::V1_10_0_L12),
];

/// LZ4 regions tried without any match before the rest of an archive stays
/// as stored: an engine none of the compressors reproduces would otherwise
/// cost every compressor on every block.
const GIVE_UP_AFTER: usize = 4;

/// Candidate order for one archive: the engine's compressor first, then the
/// rest; whichever reproduces a region moves to the front.
struct Candidates {
  order: [Compressor; Compressor::ALL.len()],
  hit: bool,
  misses: usize,
}

impl Candidates {
  fn for_engine(engine_version: &str) -> Self {
    let mut order = Compressor::ALL;
    if let Some(&(_, preferred)) = ENGINE_COMPRESSORS.iter().find(|(prefix, _)| engine_version.starts_with(prefix))
      && let Some(i) = order.iter().position(|&c| c == preferred)
    {
      order[..=i].rotate_right(1);
    }
    Candidates {
      order,
      hit: false,
      misses: 0,
    }
  }

  fn given_up(&self) -> bool {
    !self.hit && self.misses >= GIVE_UP_AFTER
  }

  fn reproduce(&mut self, raw: &[u8], compressed: &[u8]) -> Option<Compressor> {
    let Some(i) = self
      .order
      .iter()
      .position(|c| c.compress(raw, compressed.len()).is_some_and(|out| out == compressed))
    else {
      self.misses += 1;
      return None;
    };
    self.order[..=i].rotate_right(1);
    self.hit = true;
    Some(self.order[0])
  }
}

fn push_lit(segments: &mut Vec<Pcf2Segment>, bytes: &[u8]) {
  if bytes.is_empty() {
    return;
  }
  if let Some(last) = segments.last_mut()
    && last.kind == SegmentKind::Lit as u8
  {
    last.data.extend_from_slice(bytes);
    last.orig_len += bytes.len() as u64;
    return;
  }
  segments.push(Pcf2Segment::lit(bytes.to_vec()));
}

/// One LZ4 region of an archive: a storage block or an LZ4-compressed
/// blocks info.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lz4Region {
  /// Compressed bytes in the archive.
  pub range: Range<usize>,
  pub decompressed_len: usize,
  /// The compressor that reproduced the region; `None` keeps it as stored.
  pub compressor: Option<Compressor>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnityFsReport {
  pub engine_version: String,
  /// In file order.
  pub lz4_regions: Vec<Lz4Region>,
}

fn convert(input: &[u8], config: &EncodeConfig, budget: &mut EncodeBudget) -> Result<Option<(Vec<Pcf2Segment>, UnityFsReport)>> {
  let Some(layout) = parse_layout(input) else {
    return Ok(None);
  };
  let mut candidates = Candidates::for_engine(&layout.engine_version);
  let mut lz4_regions = Vec::new();
  let mut segments = Vec::new();
  for Region { range, lz4_size } in layout.regions {
    let bytes = &input[range.clone()];
    let Some(size) = lz4_size else {
      push_lit(&mut segments, bytes);
      continue;
    };
    let converted = if candidates.given_up() {
      None
    } else {
      decompress(bytes, size).and_then(|raw| Some((candidates.reproduce(&raw, bytes)?, raw)))
    };
    let compressor = match converted {
      Some((compressor, raw)) if budget.total_output.saturating_add(raw.len() as u64) <= config.max_total_output => {
        budget.consume(raw.len(), config)?;
        segments.push(Pcf2Segment {
          kind: SegmentKind::UnityFs as u8,
          flags: 0,
          orig_len: bytes.len() as u64,
          meta: vec![compressor as u8],
          data: raw,
        });
        Some(compressor)
      }
      _ => {
        push_lit(&mut segments, bytes);
        None
      }
    };
    lz4_regions.push(Lz4Region {
      range,
      decompressed_len: size,
      compressor,
    });
  }
  debug_assert_eq!(segments.iter().map(|s| s.orig_len).sum::<u64>(), input.len() as u64);
  let report = UnityFsReport {
    engine_version: layout.engine_version,
    lz4_regions,
  };
  Ok(Some((segments, report)))
}

/// Converts a UnityFS archive with the default config and reports each LZ4
/// region; `None` when the input is not a reproducible UnityFS layout.
pub fn analyze(input: &[u8]) -> Option<UnityFsReport> {
  let (_, report) = convert(input, &EncodeConfig::default(), &mut EncodeBudget::default()).ok()??;
  Some(report)
}

pub(super) fn encode_unityfs(input: &[u8], config: &EncodeConfig, budget: &mut EncodeBudget) -> Result<Option<Vec<Pcf2Segment>>> {
  Ok(convert(input, config, budget)?.map(|(segments, _)| segments))
}

pub(super) fn decode_unityfs_segment(segment: &Pcf2Segment, config: &DecodeConfig, budget: &mut DecodeBudget) -> Result<Vec<u8>> {
  let &[id] = segment.meta.as_slice() else {
    return Err(Error::InvalidSegment("unityfs meta must be exactly the compressor id"));
  };
  let compressor = Compressor::try_from(id)?;
  let orig_len = usize::try_from(segment.orig_len).map_err(|_| Error::LengthOverflow)?;
  budget.consume(orig_len, config)?;
  compressor
    .compress(&segment.data, orig_len)
    .filter(|out| out.len() == orig_len)
    .ok_or(Error::InvalidSegment("unityfs payload does not recompress to orig_len"))
}
