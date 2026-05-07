use std::io::{Read, Write};

use crate::{
  error::{Error, Result},
  varint::{read_varint, write_varint},
};

pub const PCF2_MAGIC: &[u8; 4] = b"PCF2";
pub const PCF2_VERSION: u16 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SegmentKind {
  Lit = 0x00,
  Dflt = 0x01,
  Zlib = 0x02,
  Gzip = 0x03,
  PngIdat = 0x04,
  Jpg = 0x05,
  Nested = 0x06,
  Base64 = 0x07,
  Bzip2 = 0x08,
  PdfFlate = 0x09,
}

impl SegmentKind {
  pub fn from_u8(value: u8) -> Option<Self> {
    match value {
      0x00 => Some(Self::Lit),
      0x01 => Some(Self::Dflt),
      0x02 => Some(Self::Zlib),
      0x03 => Some(Self::Gzip),
      0x04 => Some(Self::PngIdat),
      0x05 => Some(Self::Jpg),
      0x06 => Some(Self::Nested),
      0x07 => Some(Self::Base64),
      0x08 => Some(Self::Bzip2),
      0x09 => Some(Self::PdfFlate),
      _ => None,
    }
  }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pcf2Header {
  pub version: u16,
  pub flags: u16,
  pub original_size: u64,
  pub header_ext: Vec<u8>,
}

impl Pcf2Header {
  pub fn new(original_size: u64) -> Self {
    Self {
      version: PCF2_VERSION,
      flags: 0,
      original_size,
      header_ext: Vec::new(),
    }
  }

  pub fn encode<W: Write>(&self, mut writer: W) -> Result<()> {
    writer.write_all(PCF2_MAGIC)?;
    writer.write_all(&self.version.to_le_bytes())?;
    writer.write_all(&self.flags.to_le_bytes())?;

    let ext_len: u16 = self.header_ext.len().try_into().map_err(|_| Error::LengthOverflow)?;
    writer.write_all(&ext_len.to_le_bytes())?;
    writer.write_all(&self.original_size.to_le_bytes())?;
    writer.write_all(&self.header_ext)?;
    Ok(())
  }

  pub fn decode<R: Read>(mut reader: R) -> Result<Self> {
    let mut magic = [0u8; 4];
    reader.read_exact(&mut magic)?;
    if &magic != PCF2_MAGIC {
      return Err(Error::InvalidHeader("invalid magic"));
    }

    let mut version = [0u8; 2];
    let mut flags = [0u8; 2];
    let mut ext_len = [0u8; 2];
    let mut original_size = [0u8; 8];

    reader.read_exact(&mut version)?;
    reader.read_exact(&mut flags)?;
    reader.read_exact(&mut ext_len)?;
    reader.read_exact(&mut original_size)?;

    let version = u16::from_le_bytes(version);
    let flags = u16::from_le_bytes(flags);
    let ext_len = u16::from_le_bytes(ext_len) as usize;
    let original_size = u64::from_le_bytes(original_size);

    if version != PCF2_VERSION {
      return Err(Error::InvalidHeader("unsupported version"));
    }
    if flags != 0 {
      return Err(Error::InvalidHeader("flags must be 0 for v1"));
    }
    if ext_len != 0 {
      return Err(Error::InvalidHeader("header_ext_len must be 0 for v1"));
    }

    let mut header_ext = vec![0u8; ext_len];
    reader.read_exact(&mut header_ext)?;

    Ok(Self {
      version,
      flags,
      original_size,
      header_ext,
    })
  }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pcf2Segment {
  pub kind: u8,
  pub flags: u8,
  pub orig_len: u64,
  pub meta: Vec<u8>,
  pub data: Vec<u8>,
}

impl Pcf2Segment {
  pub fn lit(data: Vec<u8>) -> Self {
    Self {
      kind: SegmentKind::Lit as u8,
      flags: 0,
      orig_len: data.len() as u64,
      meta: Vec::new(),
      data,
    }
  }

  pub fn validate(&self) -> Result<()> {
    if self.flags != 0 {
      return Err(Error::InvalidSegment("flags must be 0 for v1"));
    }

    if self.kind == SegmentKind::Lit as u8 {
      if !self.meta.is_empty() {
        return Err(Error::InvalidSegment("lit meta must be empty"));
      }
      if self.orig_len != self.data.len() as u64 {
        return Err(Error::InvalidSegment("lit orig_len must match data_len"));
      }
    }

    if self.kind == SegmentKind::Nested as u8 {
      if self.meta.len() != 1 || self.meta[0] != 0 {
        return Err(Error::InvalidSegment("nested meta_version must be 0"));
      }
      if self.data.len() < PCF2_MAGIC.len() {
        return Err(Error::InvalidSegment("nested data too small"));
      }
      if &self.data[..PCF2_MAGIC.len()] != PCF2_MAGIC {
        return Err(Error::InvalidSegment("nested data missing magic"));
      }
    }

    Ok(())
  }

  pub fn encode<W: Write>(&self, mut writer: W) -> Result<()> {
    self.validate()?;

    writer.write_all(&[self.kind])?;
    writer.write_all(&[self.flags])?;
    write_varint(self.orig_len, &mut writer)?;
    write_varint(self.meta.len() as u64, &mut writer)?;
    write_varint(self.data.len() as u64, &mut writer)?;
    writer.write_all(&self.meta)?;
    writer.write_all(&self.data)?;
    Ok(())
  }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pcf2File {
  pub header: Pcf2Header,
  pub segments: Vec<Pcf2Segment>,
}

impl Pcf2File {
  pub fn from_segments(segments: Vec<Pcf2Segment>) -> Self {
    let original_size = segments.iter().map(|segment| segment.orig_len).sum();
    Self {
      header: Pcf2Header::new(original_size),
      segments,
    }
  }

  pub fn encode<W: Write>(&self, mut writer: W) -> Result<()> {
    let total: u64 = self.segments.iter().map(|segment| segment.orig_len).sum();
    if total != self.header.original_size {
      return Err(Error::InvalidHeader("original_size mismatch"));
    }

    self.header.encode(&mut writer)?;
    for segment in &self.segments {
      segment.encode(&mut writer)?;
    }
    Ok(())
  }

  pub fn decode<R: Read>(reader: R) -> Result<Self> {
    let mut decoder = Pcf2Decoder::new(reader)?;
    let header = decoder.header.clone();
    let mut segments = Vec::new();

    while let Some(segment) = decoder.next_segment()? {
      segments.push(segment.into_segment()?);
    }

    Ok(Self { header, segments })
  }
}

pub struct Pcf2Decoder<R: Read> {
  reader: R,
  header: Pcf2Header,
  remaining_orig: u64,
  finished: bool,
}

impl<R: Read> Pcf2Decoder<R> {
  pub fn new(mut reader: R) -> Result<Self> {
    let header = Pcf2Header::decode(&mut reader)?;
    let remaining_orig = header.original_size;
    Ok(Self {
      reader,
      header,
      remaining_orig,
      finished: false,
    })
  }

  pub fn header(&self) -> &Pcf2Header {
    &self.header
  }

  pub fn next_segment(&mut self) -> Result<Option<Pcf2SegmentReader<'_, R>>> {
    if self.finished {
      return Ok(None);
    }

    let mut kind_buf = [0u8; 1];
    match self.reader.read(&mut kind_buf)? {
      0 => {
        self.finished = true;
        if self.remaining_orig != 0 {
          let actual = self.header.original_size - self.remaining_orig;
          return Err(Error::SizeMismatch {
            expected: self.header.original_size,
            actual,
          });
        }
        return Ok(None);
      }
      1 => {}
      _ => unreachable!(),
    }

    let kind = kind_buf[0];
    let mut flags = [0u8; 1];
    self.reader.read_exact(&mut flags)?;
    let flags = flags[0];

    let orig_len = read_varint(&mut self.reader)?;
    let meta_len = read_varint(&mut self.reader)?;
    let data_len = read_varint(&mut self.reader)?;

    if flags != 0 {
      return Err(Error::InvalidSegment("flags must be 0 for v1"));
    }

    let meta_len_usize: usize = meta_len.try_into().map_err(|_| Error::LengthOverflow)?;
    let mut meta = vec![0u8; meta_len_usize];
    self.reader.read_exact(&mut meta)?;

    if orig_len > self.remaining_orig {
      return Err(Error::InvalidSegment("orig_len exceeds original_size"));
    }
    self.remaining_orig -= orig_len;

    if kind == SegmentKind::Lit as u8 {
      if meta_len != 0 {
        return Err(Error::InvalidSegment("lit meta must be empty"));
      }
      if orig_len != data_len {
        return Err(Error::InvalidSegment("lit orig_len must match data_len"));
      }
    }

    let mut prefix = Vec::new();
    if kind == SegmentKind::Nested as u8 {
      if meta_len != 1 || meta.first() != Some(&0) {
        return Err(Error::InvalidSegment("nested meta_version must be 0"));
      }
      if data_len < PCF2_MAGIC.len() as u64 {
        return Err(Error::InvalidSegment("nested data too small"));
      }
      let mut magic = [0u8; 4];
      self.reader.read_exact(&mut magic)?;
      if &magic != PCF2_MAGIC {
        return Err(Error::InvalidSegment("nested data missing magic"));
      }
      prefix.extend_from_slice(&magic);
    }

    let reader = LimitedReader::new(&mut self.reader, data_len, prefix);
    Ok(Some(Pcf2SegmentReader {
      kind,
      flags,
      orig_len,
      meta,
      data_len,
      data: reader,
    }))
  }
}

pub struct Pcf2SegmentReader<'a, R: Read> {
  pub kind: u8,
  pub flags: u8,
  pub orig_len: u64,
  pub meta: Vec<u8>,
  pub data_len: u64,
  data: LimitedReader<'a, R>,
}

impl<'a, R: Read> Pcf2SegmentReader<'a, R> {
  pub fn data_reader(&mut self) -> &mut LimitedReader<'a, R> {
    &mut self.data
  }

  pub fn read_data_to_vec(&mut self) -> Result<Vec<u8>> {
    let data_len: usize = self.data_len.try_into().map_err(|_| Error::LengthOverflow)?;
    let mut out = vec![0u8; data_len];
    self.data.read_exact(&mut out)?;
    Ok(out)
  }

  pub fn into_segment(mut self) -> Result<Pcf2Segment> {
    let data = self.read_data_to_vec()?;
    let segment = Pcf2Segment {
      kind: self.kind,
      flags: self.flags,
      orig_len: self.orig_len,
      meta: self.meta,
      data,
    };
    segment.validate()?;
    Ok(segment)
  }
}

pub struct LimitedReader<'a, R: Read> {
  reader: &'a mut R,
  remaining: u64,
  prefix: Vec<u8>,
  prefix_pos: usize,
}

impl<'a, R: Read> LimitedReader<'a, R> {
  fn new(reader: &'a mut R, remaining: u64, prefix: Vec<u8>) -> Self {
    Self {
      reader,
      remaining,
      prefix,
      prefix_pos: 0,
    }
  }
}

impl<R: Read> Read for LimitedReader<'_, R> {
  fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
    if self.remaining == 0 || buf.is_empty() {
      return Ok(0);
    }

    let mut written = 0;
    if self.prefix_pos < self.prefix.len() {
      let available = self.prefix.len() - self.prefix_pos;
      let to_copy = available.min(buf.len()).min(self.remaining as usize);
      buf[..to_copy].copy_from_slice(&self.prefix[self.prefix_pos..self.prefix_pos + to_copy]);
      self.prefix_pos += to_copy;
      self.remaining -= to_copy as u64;
      written += to_copy;
    }

    if written < buf.len() && self.remaining > 0 {
      let max_len = (buf.len() - written).min(self.remaining as usize);
      let read = self.reader.read(&mut buf[written..written + max_len])?;
      self.remaining -= read as u64;
      written += read;
    }

    Ok(written)
  }
}

impl<R: Read> Drop for LimitedReader<'_, R> {
  fn drop(&mut self) {
    let mut sink = [0u8; 4096];
    while self.remaining > 0 {
      let max_len = (self.remaining as usize).min(sink.len());
      match self.read(&mut sink[..max_len]) {
        Ok(0) => break,
        Ok(_) => {}
        Err(_) => break,
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use std::io::Cursor;

  use super::*;

  fn build_nested_segment(payload: &[u8]) -> Pcf2Segment {
    let nested = Pcf2File::from_segments(vec![Pcf2Segment::lit(payload.to_vec())]);
    let mut buf = Vec::new();
    nested.encode(&mut buf).expect("encode nested");
    Pcf2Segment {
      kind: SegmentKind::Nested as u8,
      flags: 0,
      orig_len: nested.header.original_size,
      meta: vec![0],
      data: buf,
    }
  }

  #[test]
  fn header_roundtrip() {
    let header = Pcf2Header::new(123);
    let mut buf = Vec::new();
    header.encode(&mut buf).expect("encode header");
    let decoded = Pcf2Header::decode(buf.as_slice()).expect("decode header");
    assert_eq!(decoded, header);
  }

  #[test]
  fn file_roundtrip_with_nested() {
    let segments = vec![Pcf2Segment::lit(b"hello".to_vec()), build_nested_segment(b"nested")];
    let file = Pcf2File::from_segments(segments);
    let mut buf = Vec::new();
    file.encode(&mut buf).expect("encode file");

    let decoded = Pcf2File::decode(buf.as_slice()).expect("decode file");
    assert_eq!(decoded.header.original_size, file.header.original_size);
    assert_eq!(decoded.segments.len(), 2);
    assert_eq!(decoded.segments[0].data, b"hello");
    assert_eq!(decoded.segments[1].kind, SegmentKind::Nested as u8);
  }

  #[test]
  fn streaming_reader_drains_on_drop() {
    let segments = vec![Pcf2Segment::lit(b"first-segment".to_vec()), Pcf2Segment::lit(b"second".to_vec())];
    let file = Pcf2File::from_segments(segments);
    let mut buf = Vec::new();
    file.encode(&mut buf).expect("encode file");

    let mut decoder = Pcf2Decoder::new(Cursor::new(buf)).expect("decode header");
    let mut first = decoder.next_segment().expect("first segment").expect("first segment");

    let mut tmp = [0u8; 4];
    first.data_reader().read_exact(&mut tmp).expect("read partial");
    drop(first);

    let mut second = decoder.next_segment().expect("second segment").expect("second segment");
    let data = second.read_data_to_vec().expect("read data");
    assert_eq!(data, b"second");
  }

  #[test]
  fn invalid_lit_meta_is_rejected() {
    let header = Pcf2Header::new(1);
    let mut buf = Vec::new();
    header.encode(&mut buf).expect("encode header");

    buf.push(SegmentKind::Lit as u8);
    buf.push(0);
    write_varint(1, &mut buf).expect("orig_len");
    write_varint(1, &mut buf).expect("meta_len");
    write_varint(1, &mut buf).expect("data_len");
    buf.push(1);
    buf.push(2);

    let mut decoder = Pcf2Decoder::new(Cursor::new(buf)).expect("header ok");
    match decoder.next_segment() {
      Ok(_) => panic!("expected error"),
      Err(err) => assert!(matches!(err, Error::InvalidSegment(_))),
    }
  }

  #[test]
  fn original_size_mismatch_is_rejected() {
    let header = Pcf2Header::new(10);
    let segment = Pcf2Segment::lit(b"abc".to_vec());
    let mut buf = Vec::new();
    header.encode(&mut buf).expect("encode header");
    segment.encode(&mut buf).expect("encode segment");

    let err = Pcf2File::decode(buf.as_slice()).expect_err("expected error");
    assert!(matches!(err, Error::SizeMismatch { .. }));
  }
}
