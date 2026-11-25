use std::{
  io::Cursor,
  pin::Pin,
  task::{Context, Poll},
};

use tokio::io::ReadBuf;

use super::*;

const PNG_FIXTURE: &[u8] = include_bytes!("../../../fixtures/minimal.png");
const JPG_FIXTURE: &[u8] = include_bytes!("../../../fixtures/minimal.jpg");

fn decode_segments(encoded: &[u8]) -> Vec<Pcf2Segment> {
  Pcf2File::decode(encoded).expect("decode pcf2").segments
}

fn assert_has_kind(segments: &[Pcf2Segment], kind: SegmentKind) {
  assert!(
    segments.iter().any(|segment| segment.kind == kind as u8),
    "missing segment {:?}",
    kind
  );
}

fn build_zip_sample() -> Vec<u8> {
  let name = b"file.txt";
  let payload = b"zip payload";
  let mut encoder = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
  encoder.write_all(payload).expect("zip deflate");
  let compressed = encoder.finish().expect("zip finish");
  let crc = crc32fast::hash(payload);

  let mut out = Vec::new();
  out.extend_from_slice(b"PK\x03\x04");
  out.extend_from_slice(&20u16.to_le_bytes());
  out.extend_from_slice(&0u16.to_le_bytes());
  out.extend_from_slice(&8u16.to_le_bytes());
  out.extend_from_slice(&0u16.to_le_bytes());
  out.extend_from_slice(&0u16.to_le_bytes());
  out.extend_from_slice(&crc.to_le_bytes());
  out.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
  out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
  out.extend_from_slice(&(name.len() as u16).to_le_bytes());
  out.extend_from_slice(&0u16.to_le_bytes());
  out.extend_from_slice(name);
  out.extend_from_slice(&compressed);
  out
}

fn write_png_chunk(out: &mut Vec<u8>, chunk_type: &[u8; 4], data: &[u8]) {
  let len = data.len() as u32;
  out.extend_from_slice(&len.to_be_bytes());
  out.extend_from_slice(chunk_type);
  out.extend_from_slice(data);
  let mut hasher = Hasher::new();
  hasher.update(chunk_type);
  hasher.update(data);
  let crc = hasher.finalize();
  out.extend_from_slice(&crc.to_be_bytes());
}

fn build_png_rgba(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
  let row_bytes = width as usize * 4;
  assert_eq!(pixels.len(), row_bytes * height as usize);
  let mut raw = Vec::with_capacity((row_bytes + 1) * height as usize);
  for row in 0..height as usize {
    raw.push(0);
    let start = row * row_bytes;
    raw.extend_from_slice(&pixels[start..start + row_bytes]);
  }
  let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
  encoder.write_all(&raw).expect("png zlib");
  let compressed = encoder.finish().expect("png finish");

  let mut out = Vec::new();
  out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
  let mut ihdr = Vec::with_capacity(13);
  ihdr.extend_from_slice(&width.to_be_bytes());
  ihdr.extend_from_slice(&height.to_be_bytes());
  ihdr.push(8);
  ihdr.push(6);
  ihdr.push(0);
  ihdr.push(0);
  ihdr.push(0);
  write_png_chunk(&mut out, b"IHDR", &ihdr);
  write_png_chunk(&mut out, b"IDAT", &compressed);
  write_png_chunk(&mut out, b"IEND", &[]);
  out
}

fn split_png_idat(png: &[u8]) -> Vec<u8> {
  if png.len() < 8 || &png[..8] != b"\x89PNG\r\n\x1a\n" {
    return png.to_vec();
  }
  let mut out = Vec::new();
  out.extend_from_slice(&png[..8]);
  let mut offset = 8usize;
  let mut split_done = false;

  while offset + 12 <= png.len() {
    let len = u32::from_be_bytes([png[offset], png[offset + 1], png[offset + 2], png[offset + 3]]) as usize;
    let chunk_type = &png[offset + 4..offset + 8];
    let data_start = offset + 8;
    let data_end = data_start + len;
    let crc_end = data_end + 4;
    if crc_end > png.len() {
      break;
    }
    if chunk_type == b"IDAT" && !split_done {
      let split = len / 2;
      if split == 0 || split == len {
        out.extend_from_slice(&png[offset..crc_end]);
      } else {
        write_png_chunk(&mut out, b"IDAT", &png[data_start..data_start + split]);
        write_png_chunk(&mut out, b"IDAT", &png[data_start + split..data_end]);
        split_done = true;
      }
    } else {
      out.extend_from_slice(&png[offset..crc_end]);
    }
    offset = crc_end;
  }
  if offset < png.len() {
    out.extend_from_slice(&png[offset..]);
  }
  out
}

fn build_pdf_sample() -> Vec<u8> {
  let payload = b"pdf stream payload";
  let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
  encoder.write_all(payload).expect("pdf zlib");
  let compressed = encoder.finish().expect("pdf finish");

  let mut out = Vec::new();
  out.extend_from_slice(b"%PDF-1.4\n1 0 obj\n<< /Length ");
  out.extend_from_slice(compressed.len().to_string().as_bytes());
  out.extend_from_slice(b" /Filter /FlateDecode >>\nstream\n");
  out.extend_from_slice(&compressed);
  out.extend_from_slice(b"endstream\nendobj\n%%EOF\n");
  out
}

fn build_pdf_predictor_sample() -> Vec<u8> {
  let width = 4u32;
  let height = 2u32;
  let colors = 3u8;
  let bpc = 8u8;
  let row_bytes = width as usize * colors as usize;
  let mut raw = Vec::with_capacity(row_bytes * height as usize);
  for row in 0..height {
    for col in 0..width {
      raw.push((row * 10 + col) as u8);
      raw.push((row * 20 + col * 2) as u8);
      raw.push((row * 30 + col * 3) as u8);
    }
  }
  let params = PdfDecodeParms {
    predictor: 12,
    colors,
    bpc,
    columns: width,
  };
  let encoded = encode_pdf_predictor(&raw, &params).expect("predictor encode");
  let encoded = encoded
    .chunks_exact(row_bytes)
    .flat_map(|row| std::iter::once(2).chain(row.iter().copied()))
    .collect::<Vec<_>>();
  let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
  encoder.write_all(&encoded).expect("pdf predictor zlib");
  let compressed = encoder.finish().expect("pdf predictor finish");

  let mut out = Vec::new();
  out.extend_from_slice(b"%PDF-1.4\n1 0 obj\n<< /Length ");
  out.extend_from_slice(compressed.len().to_string().as_bytes());
  out.extend_from_slice(
    b" /Filter /FlateDecode /DecodeParms << /Predictor 12 /Colors 3 /BitsPerComponent 8 /Columns 4 >> /Width 4 /Height 2 /BitsPerComponent 8 >>\nstream\n",
  );
  out.extend_from_slice(&compressed);
  out.extend_from_slice(b"endstream\nendobj\n%%EOF\n");
  out
}

fn build_pdf_bmp_sample() -> Vec<u8> {
  let width = 3u32;
  let height = 2u32;
  let mut raw = Vec::with_capacity((width * height) as usize);
  for i in 0..(width * height) {
    raw.push((i * 7) as u8);
  }
  let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
  encoder.write_all(&raw).expect("pdf bmp zlib");
  let compressed = encoder.finish().expect("pdf bmp finish");

  let mut out = Vec::new();
  out.extend_from_slice(b"%PDF-1.4\n1 0 obj\n<< /Length ");
  out.extend_from_slice(compressed.len().to_string().as_bytes());
  out.extend_from_slice(b" /Filter /FlateDecode /Width 3 /Height 2 /BitsPerComponent 8 >>\nstream\n");
  out.extend_from_slice(&compressed);
  out.extend_from_slice(b"endstream\nendobj\n%%EOF\n");
  out
}

fn build_swf_sample() -> Vec<u8> {
  let payload = b"swf payload";
  let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
  encoder.write_all(payload).expect("swf zlib");
  let compressed = encoder.finish().expect("swf finish");

  let mut out = Vec::new();
  out.extend_from_slice(b"CWS");
  out.push(9);
  let uncompressed_len = 8u32 + payload.len() as u32;
  out.extend_from_slice(&uncompressed_len.to_le_bytes());
  out.extend_from_slice(&compressed);
  out
}

fn strip_jpeg_dht(jpeg: &[u8]) -> Vec<u8> {
  if jpeg.len() < 4 || jpeg[0] != 0xFF || jpeg[1] != 0xD8 {
    return jpeg.to_vec();
  }
  let mut out = Vec::new();
  out.extend_from_slice(&jpeg[..2]);
  let mut pos = 2usize;
  while pos + 1 < jpeg.len() {
    if jpeg[pos] != 0xFF {
      out.extend_from_slice(&jpeg[pos..]);
      break;
    }
    let mut marker_pos = pos;
    while marker_pos < jpeg.len() && jpeg[marker_pos] == 0xFF {
      marker_pos += 1;
    }
    if marker_pos >= jpeg.len() {
      break;
    }
    let marker = jpeg[marker_pos];
    let marker_end = marker_pos + 1;
    if marker == 0xDA {
      out.extend_from_slice(&jpeg[pos..]);
      break;
    }
    if marker == 0xD9 {
      out.extend_from_slice(&jpeg[pos..marker_end]);
      break;
    }
    if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
      out.extend_from_slice(&jpeg[pos..marker_end]);
      pos = marker_end;
      continue;
    }
    if marker_end + 1 >= jpeg.len() {
      break;
    }
    let seg_len = u16::from_be_bytes([jpeg[marker_end], jpeg[marker_end + 1]]) as usize;
    if seg_len < 2 || marker_end + seg_len > jpeg.len() {
      break;
    }
    let seg_end = marker_end + seg_len;
    if marker != 0xC4 {
      out.extend_from_slice(&jpeg[pos..seg_end]);
    }
    pos = seg_end;
  }
  out
}

fn build_standard_jpeg() -> Vec<u8> {
  let data = [255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0];
  let mut out = Vec::new();
  let encoder = jpeg_encoder::Encoder::new(&mut out, 100);
  encoder.encode(&data, 2, 2, jpeg_encoder::ColorType::Rgb).expect("jpeg encode");
  out
}

struct AsyncCursor {
  inner: Cursor<Vec<u8>>,
}

impl AsyncCursor {
  fn new(data: Vec<u8>) -> Self {
    Self { inner: Cursor::new(data) }
  }
}

impl tokio::io::AsyncRead for AsyncCursor {
  fn poll_read(mut self: Pin<&mut Self>, _cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
    let unfilled = buf.initialize_unfilled();
    let read = std::io::Read::read(&mut self.inner, unfilled)?;
    buf.advance(read);
    Poll::Ready(Ok(()))
  }
}

#[test]
fn roundtrip_deflate_segment() {
  let data = b"hello precomp2";
  let mut encoder = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
  encoder.write_all(data).expect("deflate");
  let deflated = encoder.finish().expect("finish");

  let encoded = encode_bytes(&deflated, &EncodeConfig::default()).expect("encode");
  let segments = decode_segments(&encoded);
  assert_has_kind(&segments, SegmentKind::Dflt);
  let decoded = decode_bytes(&encoded, &DecodeConfig::default()).expect("decode");
  assert_eq!(decoded, deflated);
}

#[test]
fn preflate_layout_roundtrip() {
  let data = b"preflate layout";
  let mut encoder = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
  encoder.write_all(data).expect("deflate");
  let deflated = encoder.finish().expect("finish");

  let analysis = preflate_analyze(&deflated).expect("preflate analyze");
  assert_eq!(analysis.deflate_len, deflated.len() as u64);
  assert_eq!(analysis.plain, data);
  let rebuilt = preflate_reencode(&analysis.corrections, &analysis.plain).expect("preflate reencode");
  assert_eq!(rebuilt, deflated);
}

#[test]
fn roundtrip_zlib_segment() {
  let data = b"hello zlib";
  let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
  encoder.write_all(data).expect("zlib");
  let zlib = encoder.finish().expect("zlib finish");

  let encoded = encode_bytes(&zlib, &EncodeConfig::default()).expect("encode");
  let segments = decode_segments(&encoded);
  assert_has_kind(&segments, SegmentKind::Zlib);
  let decoded = decode_bytes(&encoded, &DecodeConfig::default()).expect("decode");
  assert_eq!(decoded, zlib);
}

#[test]
fn roundtrip_gzip_segment() {
  let data = b"hello gzip";
  let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
  encoder.write_all(data).expect("gzip");
  let gzip = encoder.finish().expect("gzip finish");

  let encoded = encode_bytes(&gzip, &EncodeConfig::default()).expect("encode");
  let segments = decode_segments(&encoded);
  assert_has_kind(&segments, SegmentKind::Gzip);
  let decoded = decode_bytes(&encoded, &DecodeConfig::default()).expect("decode");
  assert_eq!(decoded, gzip);
}

#[test]
fn roundtrip_png_segment() {
  let png = PNG_FIXTURE.to_vec();
  let encoded = encode_bytes(&png, &EncodeConfig::default()).expect("encode");
  let segments = decode_segments(&encoded);
  assert_has_kind(&segments, SegmentKind::PngIdat);
  let segment = segments
    .iter()
    .find(|segment| segment.kind == SegmentKind::PngIdat as u8)
    .expect("png segment");
  assert_eq!(segment.meta[0], 1);
  assert_eq!(segment.meta[2], 0);
  let decoded = decode_bytes(&encoded, &DecodeConfig::default()).expect("decode");
  assert_eq!(decoded, png);
}

#[test]
fn roundtrip_png_webp_segment() {
  let pixels = [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255];
  let png = build_png_rgba(2, 2, &pixels);
  let config = EncodeConfig {
    enable_png_webp: true,
    ..Default::default()
  };
  let encoded = encode_bytes(&png, &config).expect("encode");
  let segments = decode_segments(&encoded);
  let segment = segments
    .iter()
    .find(|segment| segment.kind == SegmentKind::PngIdat as u8)
    .expect("png segment");
  assert_eq!(segment.meta[0], 1);
  assert_eq!(segment.meta[2], 1);
  let decoded = decode_bytes(&encoded, &DecodeConfig::default()).expect("decode");
  assert_eq!(decoded, png);
}

#[test]
fn roundtrip_png_multi_idat_webp_segment() {
  let pixels = [16, 32, 48, 255, 64, 80, 96, 255, 128, 144, 160, 255, 192, 208, 224, 255];
  let png = build_png_rgba(2, 2, &pixels);
  let png_multi = split_png_idat(&png);
  let config = EncodeConfig {
    enable_png_webp: true,
    ..Default::default()
  };
  let encoded = encode_bytes(&png_multi, &config).expect("encode");
  let segments = decode_segments(&encoded);
  let segment = segments
    .iter()
    .find(|segment| segment.kind == SegmentKind::PngIdat as u8)
    .expect("png segment");
  assert_eq!(segment.meta[0], 1);
  assert_eq!(segment.meta[2], 1);
  let decoded = decode_bytes(&encoded, &DecodeConfig::default()).expect("decode");
  assert_eq!(decoded, png_multi);
}

#[test]
fn roundtrip_jpg_segment() {
  let jpeg = JPG_FIXTURE.to_vec();
  let encoded = encode_bytes(&jpeg, &EncodeConfig::default()).expect("encode");
  let segments = decode_segments(&encoded);
  assert_has_kind(&segments, SegmentKind::Jpg);
  let segment = segments
    .iter()
    .find(|segment| segment.kind == SegmentKind::Jpg as u8)
    .expect("jpg segment");
  assert_eq!(segment.meta[0], 1);
  assert_eq!(segment.meta[1], 0);
  let decoded = decode_bytes(&encoded, &DecodeConfig::default()).expect("decode");
  assert_eq!(decoded, jpeg);
}

#[test]
fn jpg_segment_falls_back_when_lepton_output_differs() {
  // Drop the trailing EOI marker: lepton still encodes this, but its decode
  // regenerates a canonical EOI, so the transform would not be reversible.
  let jpeg = JPG_FIXTURE[..JPG_FIXTURE.len() - 2].to_vec();
  let encoded = encode_bytes(&jpeg, &EncodeConfig::default()).expect("encode");
  assert!(
    !decode_segments(&encoded)
      .iter()
      .any(|segment| segment.kind == SegmentKind::Jpg as u8)
  );
  let decoded = decode_bytes(&encoded, &DecodeConfig::default()).expect("decode");
  assert_eq!(decoded, jpeg);
}

#[test]
fn roundtrip_mjpg_segment() {
  let jpeg = build_standard_jpeg();
  let mjpeg = strip_jpeg_dht(&jpeg);
  assert!(mjpeg.len() < jpeg.len(), "strip mjpg dht");
  assert!(!jpeg_has_dht(&mjpeg), "mjpeg still has dht");
  let encoded = encode_bytes(&mjpeg, &EncodeConfig::default()).expect("encode");
  let segments = decode_segments(&encoded);
  let segment = segments
    .iter()
    .find(|segment| segment.kind == SegmentKind::Jpg as u8)
    .expect("jpg segment");
  assert_eq!(segment.meta[0], 1);
  assert_eq!(segment.meta[1], 1);
  let decoded = decode_bytes(&encoded, &DecodeConfig::default()).expect("decode");
  assert_eq!(decoded, mjpeg);
}

#[test]
fn roundtrip_base64_segment_with_uniform_last_short_and_irregular_lines() {
  let payload = b"base64 payload data";
  let encoded_payload = BASE64_ENGINE.encode(payload);
  assert_eq!(encoded_payload.len(), 28);
  for lines in [&[4usize; 7][..], &[8, 8, 8, 4], &[3, 9, 1, 15]] {
    let mut base64_bytes = Vec::new();
    base64_bytes.extend_from_slice(b"Content-Transfer-Encoding: base64\r\n\r\n");
    let mut rest = encoded_payload.as_bytes();
    for &len in lines {
      let (line, tail) = rest.split_at(len);
      base64_bytes.extend_from_slice(line);
      base64_bytes.extend_from_slice(b"\r\n");
      rest = tail;
    }

    let encoded = encode_bytes(&base64_bytes, &EncodeConfig::default()).expect("encode");
    let segments = decode_segments(&encoded);
    assert_has_kind(&segments, SegmentKind::Base64);
    let decoded = decode_bytes(&encoded, &DecodeConfig::default()).expect("decode");
    assert_eq!(decoded, base64_bytes, "{lines:?}");
  }
}

#[test]
fn invalid_base64_falls_back_to_lit() {
  let input = b"SGVsbG8=";
  let encoded = encode_bytes(input, &EncodeConfig::default()).expect("encode");
  let segments = decode_segments(&encoded);
  assert!(
    segments.iter().all(|segment| segment.kind == SegmentKind::Lit as u8),
    "expected lit fallback"
  );
  let decoded = decode_bytes(&encoded, &DecodeConfig::default()).expect("decode");
  assert_eq!(decoded, input);
}

#[test]
fn roundtrip_bzip2_segment() {
  let payload = b"bzip2 payload data";
  let mut encoder = BzEncoder::new(Vec::new(), Compression::new(6));
  encoder.write_all(payload).expect("bzip2");
  let bzip = encoder.finish().expect("bzip2 finish");

  let encoded = encode_bytes(&bzip, &EncodeConfig::default()).expect("encode");
  let segments = decode_segments(&encoded);
  assert_has_kind(&segments, SegmentKind::Bzip2);
  let decoded = decode_bytes(&encoded, &DecodeConfig::default()).expect("decode");
  assert_eq!(decoded, bzip);
}

#[test]
fn invalid_zlib_falls_back_to_lit() {
  let payload = b"bad zlib";
  let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
  encoder.write_all(payload).expect("zlib");
  let mut zlib = encoder.finish().expect("zlib finish");
  if let Some(last) = zlib.last_mut() {
    *last = last.wrapping_add(1);
  }
  let encoded = encode_bytes(&zlib, &EncodeConfig::default()).expect("encode");
  let segments = decode_segments(&encoded);
  assert!(
    segments.iter().all(|segment| segment.kind == SegmentKind::Lit as u8),
    "expected lit fallback"
  );
  let decoded = decode_bytes(&encoded, &DecodeConfig::default()).expect("decode");
  assert_eq!(decoded, zlib);
}

#[test]
fn roundtrip_zip_segment() {
  let zip = build_zip_sample();
  let encoded = encode_bytes(&zip, &EncodeConfig::default()).expect("encode");
  let segments = decode_segments(&encoded);
  assert_has_kind(&segments, SegmentKind::Dflt);
  let decoded = decode_bytes(&encoded, &DecodeConfig::default()).expect("decode");
  assert_eq!(decoded, zip);
}

#[test]
fn roundtrip_pdf_segment() {
  let pdf = build_pdf_sample();
  let encoded = encode_bytes(&pdf, &EncodeConfig::default()).expect("encode");
  let segments = decode_segments(&encoded);
  assert_has_kind(&segments, SegmentKind::PdfStream);
  let decoded = decode_bytes(&encoded, &DecodeConfig::default()).expect("decode");
  assert_eq!(decoded, pdf);
}

#[test]
fn roundtrip_pdf_predictor_segment() {
  let pdf = build_pdf_predictor_sample();
  let config = EncodeConfig {
    enable_pdf_predictor: true,
    ..Default::default()
  };
  let encoded = encode_bytes(&pdf, &config).expect("encode");
  let segments = decode_segments(&encoded);
  let segment = segments
    .iter()
    .find(|segment| segment.kind == SegmentKind::PdfStream as u8)
    .expect("pdf stream");
  assert_eq!(segment.meta[0], 0);
  assert_eq!(segment.meta[1], 2);
  let decoded = decode_bytes(&encoded, &DecodeConfig::default()).expect("decode");
  assert_eq!(decoded, pdf);
}

#[test]
fn roundtrip_pdf_small_image_flate_segment() {
  let pdf = build_pdf_bmp_sample();
  let config = EncodeConfig {
    enable_pdf_images: true,
    ..Default::default()
  };
  let encoded = encode_bytes(&pdf, &config).expect("encode");
  let segments = decode_segments(&encoded);
  let segment = segments
    .iter()
    .find(|segment| segment.kind == SegmentKind::PdfStream as u8)
    .expect("pdf stream");
  assert_eq!(segment.meta[0], 0);
  assert_eq!(segment.meta[1], 1);
  let decoded = decode_bytes(&encoded, &DecodeConfig::default()).expect("decode");
  assert_eq!(decoded, pdf);
}

#[test]
fn roundtrip_swf_segment() {
  let swf = build_swf_sample();
  let encoded = encode_bytes(&swf, &EncodeConfig::default()).expect("encode");
  let segments = decode_segments(&encoded);
  assert_has_kind(&segments, SegmentKind::Zlib);
  let decoded = decode_bytes(&encoded, &DecodeConfig::default()).expect("decode");
  assert_eq!(decoded, swf);
}

#[test]
fn recover_scan_restores_multiple_containers() {
  let payload = b"recover";
  let encoded = encode_bytes(payload, &EncodeConfig::default()).expect("encode");
  let mut buffer = Vec::new();
  buffer.extend_from_slice(b"XXXX");
  buffer.extend_from_slice(&encoded);
  buffer.extend_from_slice(b"YYYY");
  buffer.extend_from_slice(&encoded);

  let decoded = decode_bytes(
    &buffer,
    &DecodeConfig {
      recover: true,
      ..DecodeConfig::default()
    },
  )
  .expect("recover decode");
  assert_eq!(decoded, [payload.as_slice(), payload.as_slice()].concat());
}

#[test]
fn reject_unknown_segment_kind() {
  let segment = Pcf2Segment {
    kind: 0xFF,
    flags: 0,
    orig_len: 0,
    meta: Vec::new(),
    data: Vec::new(),
  };
  let mut budget = DecodeBudget::default();
  let err = decode_segment(&segment, &DecodeConfig::default(), 0, &mut budget).unwrap_err();
  assert!(matches!(err, Error::InvalidSegment("unknown segment kind")));
}

#[test]
fn reject_old_png_meta_version() {
  let segment = Pcf2Segment {
    kind: SegmentKind::PngIdat as u8,
    flags: 0,
    orig_len: 0,
    meta: vec![0, 0, 0],
    data: Vec::new(),
  };
  let mut budget = DecodeBudget::default();
  let err = decode_segment(&segment, &DecodeConfig::default(), 0, &mut budget).unwrap_err();
  assert!(matches!(err, Error::InvalidSegment("png meta_version")));
}

#[test]
fn reject_old_jpg_meta_version() {
  let segment = Pcf2Segment {
    kind: SegmentKind::Jpg as u8,
    flags: 0,
    orig_len: 0,
    meta: vec![0, 0],
    data: Vec::new(),
  };
  let mut budget = DecodeBudget::default();
  let err = decode_segment(&segment, &DecodeConfig::default(), 0, &mut budget).unwrap_err();
  assert!(matches!(err, Error::InvalidSegment("jpg meta_version")));
}

#[tokio::test]
async fn async_decode_roundtrip() {
  let payload = b"async";
  let encoded = encode_bytes(payload, &EncodeConfig::default()).expect("encode");
  let reader = AsyncCursor::new(encoded);
  let decoded = decode_async(reader, &DecodeConfig::default()).await.expect("decode");
  assert_eq!(decoded, payload);
}

fn png_with_chunk(png: &[u8], kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
  let mut out = png[..33].to_vec();
  write_png_chunk(&mut out, kind, data);
  out.extend_from_slice(&png[33..]);
  out
}

fn display_png(png: &[u8]) -> (Vec<u8>, Option<Vec<u8>>) {
  let pcf = encode_bytes(
    png,
    &EncodeConfig {
      enable_png_webp: true,
      ..Default::default()
    },
  )
  .unwrap();
  assert_eq!(decode_bytes(&pcf, &DecodeConfig::default()).unwrap(), png);
  let image = decode_image_for_display(&pcf, &DecodeConfig::default()).unwrap();
  (pcf, image)
}

#[test]
fn display_png_preserves_hidden_rgb_and_original_bytes() {
  let pixels = [17, 39, 91, 0, 13, 41, 97, 120, 0, 255, 0, 255];
  let png = build_png_rgba(3, 1, &pixels);
  let (_, display) = display_png(&png);
  let webp = display.unwrap();
  assert_eq!(&webp[8..12], b"WEBP");
  assert_eq!(decode_webp_bitmap(&webp, 3, 1, 6).unwrap(), pixels);
  let old = encode_bytes(&png, &EncodeConfig::default()).unwrap();
  assert!(decode_image_for_display(&old, &DecodeConfig::default()).unwrap().is_none());
}

#[test]
fn display_png_rejects_idat_orig_len_that_differs_from_its_chunks() {
  let png = build_png_rgba(3, 1, &[17, 39, 91, 0, 13, 41, 97, 120, 0, 255, 0, 255]);
  let (pcf, display) = display_png(&png);
  assert!(display.is_some());
  let file = Pcf2File::decode(pcf.as_slice()).unwrap();
  for delta in [-1i64, 1] {
    let mut segments = file.segments.clone();
    let idat = segments.iter_mut().find(|s| s.kind == SegmentKind::PngIdat as u8).unwrap();
    idat.orig_len = idat.orig_len.checked_add_signed(delta).unwrap();
    let mut forged = Vec::new();
    Pcf2File::from_segments(segments).encode(&mut forged).unwrap();
    let err = decode_image_for_display(&forged, &DecodeConfig::default()).unwrap_err();
    assert!(matches!(err, Error::InvalidSegment("png idat original length")), "{err:?}");
  }
}

#[test]
fn display_png_color_precedence_and_icc_mux() {
  let png = build_png_rgba(1, 1, &[23, 45, 67, 0]);
  let gamma = png_with_chunk(&png, b"gAMA", &45455u32.to_be_bytes());
  assert_eq!(&display_png(&gamma).1.unwrap()[8..12], b"WEBP");
  let srgb = png_with_chunk(&gamma, b"sRGB", &[0]);
  assert_eq!(&display_png(&srgb).1.unwrap()[8..12], b"WEBP");
  for data in [b"invalid profile".to_vec(), b"profile\0\0invalid zlib".to_vec()] {
    let input = png_with_chunk(&png, b"iCCP", &data);
    let webp = display_png(&input).1.unwrap();
    assert_eq!(&webp[8..12], b"WEBP");
    assert!(!webp.windows(4).any(|v| v == b"ICCP"));
  }
  for profile_len in [132, 133, 4096] {
    let mut profile = vec![0; profile_len];
    profile[..4].copy_from_slice(&(profile_len as u32).to_be_bytes());
    profile[16..20].copy_from_slice(b"RGB ");
    profile[36..40].copy_from_slice(b"acsp");
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&profile).unwrap();
    let mut iccp = b"profile\0\0".to_vec();
    iccp.extend_from_slice(&encoder.finish().unwrap());
    let icc_png = png_with_chunk(&srgb, b"iCCP", &iccp);
    let (pcf, image) = display_png(&icc_png);
    let webp = image.unwrap();
    assert_eq!(&webp[12..16], b"VP8X");
    assert_eq!(webp[20] & 0x20, 0x20);
    assert_eq!(&webp[30..34], b"ICCP");
    assert_eq!(&webp[38..38 + profile_len], profile.as_slice());
    assert_eq!(decode_webp_bitmap(&webp, 1, 1, 6).unwrap(), [23, 45, 67, 0]);
    for limit in [profile.len() - 1, webp.len() - 1, webp.len()] {
      let config = DecodeConfig {
        max_total_output: limit as u64,
        ..Default::default()
      };
      let result = decode_image_for_display(&pcf, &config);
      if limit == webp.len() {
        assert_eq!(result.unwrap().unwrap(), webp);
      } else {
        assert!(result.is_err(), "display output exceeded {limit} bytes");
      }
    }
    let limited_encode = EncodeConfig {
      enable_png_webp: true,
      max_total_output: (profile_len - 1) as u64,
      ..Default::default()
    };
    let encoded = encode_bytes(&icc_png, &limited_encode).unwrap();
    assert_has_kind(&decode_segments(&encoded), SegmentKind::PngIdat);
    assert_eq!(decode_bytes(&encoded, &DecodeConfig::default()).unwrap(), icc_png);
    let srgb_cicp = png_with_chunk(&icc_png, b"cICP", &[1, 13, 0, 1]);
    let webp = display_png(&srgb_cicp).1.unwrap();
    assert!(!webp.windows(4).any(|v| v == b"ICCP"));
    let unsupported_cicp = png_with_chunk(&icc_png, b"cICP", &[1, 1, 0, 1]);
    assert_eq!(&display_png(&unsupported_cicp).1.unwrap()[8..12], b"WEBP");
  }
}

#[test]
fn display_png_metadata_allows_fast_preview() {
  let png = build_png_rgba(1, 1, &[12, 34, 56, 255]);
  let mut trailing = png.clone();
  trailing.extend_from_slice(b"trailing");
  let mut bad_crc = png.clone();
  bad_crc[29] ^= 1;
  for input in [trailing, bad_crc, png[..40].to_vec(), png[..8].to_vec()] {
    let (pcf, image) = display_png(&input);
    assert_eq!(image.unwrap(), input);
    assert_eq!(decode_segments(&pcf), vec![Pcf2Segment::lit(input)]);
  }
  for (kind, bytes) in [
    (*b"acTL", vec![0; 8]),
    (*b"tRNS", vec![0; 6]),
    (*b"mDCV", vec![0; 24]),
    (*b"cLLI", vec![0, 0, 0, 1, 0, 0, 0, 0]),
    (*b"cHRM", vec![0; 32]),
    (*b"pHYs", vec![0, 0, 0, 2, 0, 0, 0, 1, 0]),
    (*b"bKGD", vec![0, 255, 0, 255, 0, 255]),
    (*b"sRGB", vec![3]),
  ] {
    let input = png_with_chunk(&png, &kind, &bytes);
    let (pcf, image) = display_png(&input);
    let webp = image.unwrap();
    assert_eq!(decode_webp_bitmap(&webp, 1, 1, 6).unwrap(), [12, 34, 56, 255]);
    assert_has_kind(&decode_segments(&pcf), SegmentKind::PngIdat);
  }
  let zero_clli = png_with_chunk(&png, b"cLLI", &[0; 8]);
  assert_eq!(&display_png(&zero_clli).1.unwrap()[8..12], b"WEBP");
  let mut animated = png[..33].to_vec();
  write_png_chunk(&mut animated, b"acTL", &[0, 0, 0, 2, 0, 0, 0, 0]);
  let mut frame = vec![0; 26];
  frame[7] = 1;
  frame[11] = 1;
  frame[21] = 1;
  frame[23] = 10;
  write_png_chunk(&mut animated, b"fcTL", &frame);
  animated.extend_from_slice(&png[33..png.len() - 12]);
  frame[3] = 1;
  write_png_chunk(&mut animated, b"fcTL", &frame);
  let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
  encoder.write_all(&[0, 255, 0, 0, 255]).unwrap();
  let mut next_frame = vec![0, 0, 0, 2];
  next_frame.extend_from_slice(&encoder.finish().unwrap());
  write_png_chunk(&mut animated, b"fdAT", &next_frame);
  write_png_chunk(&mut animated, b"IEND", &[]);
  let (pcf, image) = display_png(&animated);
  assert_has_kind(&decode_segments(&pcf), SegmentKind::PngIdat);
  assert_eq!(decode_webp_bitmap(&image.unwrap(), 1, 1, 6).unwrap(), [12, 34, 56, 255]);
  for orientation in 1u16..=8 {
    let mut exif = b"II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0".to_vec();
    exif.extend_from_slice(&orientation.to_le_bytes());
    exif.extend_from_slice(&[0; 6]);
    let input = png_with_chunk(&png, b"eXIf", &exif);
    let image = display_png(&input).1.unwrap();
    assert_eq!(&image[8..12], b"WEBP");
  }
  let absent = png_with_chunk(&png, b"eXIf", b"II\x2a\0\x08\0\0\0\0\0\0\0\0\0");
  assert_eq!(&display_png(&absent).1.unwrap()[8..12], b"WEBP");
  for (depth, color, interlace, samples) in [
    (8, 0, 0, vec![127]),
    (8, 3, 0, vec![0]),
    (16, 6, 0, vec![0, 12, 0, 34, 0, 56, 255, 255]),
    (8, 6, 1, vec![12, 34, 56, 255]),
  ] {
    let mut ihdr = png[16..29].to_vec();
    ihdr[8] = depth;
    ihdr[9] = color;
    ihdr[12] = interlace;
    let mut input = png[..8].to_vec();
    write_png_chunk(&mut input, b"IHDR", &ihdr);
    if color == 3 {
      write_png_chunk(&mut input, b"PLTE", &[12, 34, 56]);
    }
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&[0]).unwrap();
    encoder.write_all(&samples).unwrap();
    write_png_chunk(&mut input, b"IDAT", &encoder.finish().unwrap());
    write_png_chunk(&mut input, b"IEND", &[]);
    assert!(display_png(&input).1.is_none());
  }
}

#[test]
fn display_complete_images_and_invalid_containers() {
  for input in [JPG_FIXTURE, b"GIF89a\x01\0\x01\0\0\0\0;".as_slice()] {
    let pcf = encode_bytes(input, &EncodeConfig::default()).unwrap();
    assert_eq!(decode_image_for_display(&pcf, &DecodeConfig::default()).unwrap().unwrap(), input);
  }
  for input in [build_zip_sample(), build_pdf_sample(), b"not an image".to_vec()] {
    let pcf = encode_bytes(&input, &EncodeConfig::default()).unwrap();
    assert!(decode_image_for_display(&pcf, &DecodeConfig::default()).unwrap().is_none());
  }
  let nested_bytes = encode_bytes(JPG_FIXTURE, &EncodeConfig::default()).unwrap();
  let nested = Pcf2Segment {
    kind: SegmentKind::Nested as u8,
    flags: 0,
    orig_len: JPG_FIXTURE.len() as u64,
    meta: vec![0],
    data: nested_bytes,
  };
  let mut nested_bytes = Vec::new();
  Pcf2File::from_segments(vec![nested]).encode(&mut nested_bytes).unwrap();
  assert!(decode_image_for_display(&nested_bytes, &DecodeConfig::default()).unwrap().is_none());
  let png = build_png_rgba(1, 1, &[17, 39, 91, 0]);
  let (pcf, _) = display_png(&png);
  assert!(decode_image_for_display(&pcf[..pcf.len() - 1], &DecodeConfig::default()).is_err());
  let mut segments = decode_segments(&pcf);
  let segment = segments.iter_mut().find(|v| v.kind == SegmentKind::PngIdat as u8).unwrap();
  segment.meta[2] = 99;
  let mut corrupt = Vec::new();
  Pcf2File::from_segments(segments).encode(&mut corrupt).unwrap();
  assert!(decode_image_for_display(&corrupt, &DecodeConfig::default()).is_err());
}

fn varint(value: u64) -> Vec<u8> {
  let mut out = Vec::new();
  write_varint_vec(value, &mut out);
  out
}

fn segment(kind: SegmentKind, meta: Vec<u8>, data: Vec<u8>) -> Pcf2Segment {
  Pcf2Segment {
    kind: kind as u8,
    flags: 0,
    orig_len: 0,
    meta,
    data,
  }
}

/// Base64 meta: version, literal payload, empty header, then `lines`
/// (line_case, count, lengths) and `decoded_len`.
fn base64_meta(lines: &[u8], decoded_len: u64) -> Vec<u8> {
  [&[0u8, 0, 0][..], lines, &varint(decoded_len)].concat()
}

#[test]
fn forged_lengths_fail_cleanly_without_allocating_or_panicking() {
  let huge = 1u64 << 40;
  let cases = [
    (
      "base64 header length",
      segment(SegmentKind::Base64, [&[0u8, 0][..], &varint(u64::MAX)].concat(), Vec::new()),
    ),
    (
      "bzip2 penalty count",
      segment(SegmentKind::Bzip2, [&[0u8, 0, b'9'][..], &varint(u64::MAX)].concat(), Vec::new()),
    ),
    (
      "bzip2 penalty entries",
      segment(SegmentKind::Bzip2, [&[0u8, 0, b'9'][..], &varint(huge)].concat(), Vec::new()),
    ),
    (
      "gzip header length",
      segment(SegmentKind::Gzip, [&[0u8, 0][..], &varint(u64::MAX)].concat(), Vec::new()),
    ),
    (
      "preflate corrections length",
      segment(SegmentKind::Dflt, vec![0, 0], varint(u64::MAX)),
    ),
    (
      "preflate payload length",
      segment(SegmentKind::Dflt, vec![0, 0], [&[0u8][..], &varint(u64::MAX)].concat()),
    ),
  ];
  for (name, segment) in cases {
    let result = decode_segment(&segment, &DecodeConfig::default(), 0, &mut DecodeBudget::default());
    assert!(
      matches!(result, Err(Error::InvalidSegment(_) | Error::LengthOverflow)),
      "{name}: {result:?}"
    );
  }
}

/// Decodes a literal-payload base64 segment with the given line meta.
fn decode_base64_lines(lines: &[u8], payload: &[u8], config: &DecodeConfig) -> Result<Vec<u8>> {
  let segment = segment(SegmentKind::Base64, base64_meta(lines, payload.len() as u64), payload.to_vec());
  decode_segment(&segment, config, 0, &mut DecodeBudget::default()).map(Cow::into_owned)
}

/// The line walk of the original decoder: write lines while text remains,
/// clip the last one, and require the text to be used up.
fn reference_base64_lines(lengths: &[usize], payload: &[u8]) -> Option<Vec<u8>> {
  let encoded = BASE64_ENGINE.encode(payload);
  let mut out = Vec::new();
  let mut cursor = 0;
  for &len in lengths {
    if cursor >= encoded.len() {
      break;
    }
    let end = (cursor + len).min(encoded.len());
    out.extend_from_slice(&encoded.as_bytes()[cursor..end]);
    out.extend_from_slice(b"\r\n");
    cursor = end;
  }
  (cursor == encoded.len()).then_some(out)
}

#[test]
fn base64_line_layouts_follow_the_reference_decoder() {
  let huge = varint(1 << 40);
  // Name, line meta, payload, restored text.
  type Case<'a> = (&'a str, Vec<u8>, &'a [u8], Option<&'a [u8]>);
  let cases: [Case; 10] = [
    // Golden case shared with the Pion decoder: an empty first line.
    ("empty first line", vec![1, 2, 0, 4], &b"ABC"[..], Some(&b"\r\nQUJD\r\n"[..])),
    ("empty listed line", vec![2, 2, 0, 4], &b"ABC"[..], Some(&b"\r\nQUJD\r\n"[..])),
    (
      "unused uniform lines",
      [&[0][..], &huge, &[76]].concat(),
      &b"abc"[..],
      Some(&b"YWJj\r\n"[..]),
    ),
    (
      "unused lines before last",
      [&[1][..], &huge, &[76, 4]].concat(),
      &b"abc"[..],
      Some(&b"YWJj\r\n"[..]),
    ),
    (
      "empty lines then last",
      vec![1, 3, 0, 4],
      &b"abc"[..],
      Some(&b"\r\n\r\nYWJj\r\n"[..]),
    ),
    (
      "clipped last line",
      vec![2, 2, 4, 8],
      &b"abcdef"[..],
      Some(&b"YWJj\r\nZGVm\r\n"[..]),
    ),
    ("empty text", vec![0, 5, 0], &b""[..], Some(&b""[..])),
    (
      "empty uniform lines never reach the end",
      [&[0][..], &huge, &[0]].concat(),
      &b"abc"[..],
      None,
    ),
    ("short lines", vec![2, 2, 1, 1], &b"abc"[..], None),
    ("empty lines then short last", vec![1, 3, 0, 3], &b"abc"[..], None),
  ];
  for (name, lines, payload, expected) in cases {
    let result = decode_base64_lines(&lines, payload, &DecodeConfig::default());
    assert_eq!(result.as_deref().ok(), expected, "{name}: {result:?}");
  }

  // Seeded differential check against the reference walk for every case.
  let mut state = 0x9e37_79b9_7f4a_7c15u64;
  let mut next = |n: u64| {
    state ^= state << 13;
    state ^= state >> 7;
    state ^= state << 17;
    (state % n) as usize
  };
  for _ in 0..3000 {
    let payload: Vec<u8> = (0..next(12)).map(|i| i as u8).collect();
    let count = 1 + next(6);
    let (lines, lengths) = match next(3) {
      0 => {
        let len = next(6);
        ([&[0][..], &varint(count as u64), &[len as u8]].concat(), vec![len; count])
      }
      1 => {
        let (len, last) = (next(6), next(6));
        let mut lengths = vec![len; count];
        lengths[count - 1] = last;
        ([&[1][..], &varint(count as u64), &[len as u8, last as u8]].concat(), lengths)
      }
      _ => {
        let lengths: Vec<usize> = (0..count).map(|_| next(6)).collect();
        let bytes: Vec<u8> = lengths.iter().map(|&l| l as u8).collect();
        ([&[2][..], &varint(count as u64), &bytes].concat(), lengths)
      }
    };
    let result = decode_base64_lines(&lines, &payload, &DecodeConfig::default()).ok();
    assert_eq!(result, reference_base64_lines(&lengths, &payload), "{lengths:?} {payload:?}");
  }
}

#[test]
fn base64_output_beyond_budget_fails_before_decoding_payload() {
  let tight = DecodeConfig {
    max_total_output: 100,
    ..DecodeConfig::default()
  };
  // 300 decoded bytes re-encode to 400; the meta claims them, but the data
  // is empty, so only an early budget check reports the budget.
  let meta = base64_meta(&[0, 10, 76], 300);
  let claimed = segment(SegmentKind::Base64, meta, Vec::new());
  let err = decode_segment(&claimed, &tight, 0, &mut DecodeBudget::default()).unwrap_err();
  assert!(
    matches!(&err, Error::Other(msg) if msg == "decode output exceeds budget"),
    "{err:?}"
  );
  // 2^40 empty lines before the last would need 2^41 output bytes, and twice
  // a count near 2^63 overflows: both fail without panicking.
  for count in [1u64 << 40, (u64::MAX >> 1) + 1] {
    let lines = [&[1][..], &varint(count), &[0, 4]].concat();
    let result = decode_base64_lines(&lines, b"abc", &DecodeConfig::default());
    assert!(
      matches!(&result, Err(Error::Other(_) | Error::LengthOverflow)),
      "{count}: {result:?}"
    );
  }
  // Under an unlimited budget, an output past `isize::MAX` bytes cannot be
  // allocated and fails before the payload is decoded.
  let unlimited = DecodeConfig {
    max_total_output: u64::MAX,
    ..DecodeConfig::default()
  };
  let meta = base64_meta(&[&[0][..], &varint(1 << 62), &[255]].concat(), 0x7000_0000_0000_0000);
  let oversized = segment(SegmentKind::Base64, meta, Vec::new());
  let result = decode_segment(&oversized, &unlimited, 0, &mut DecodeBudget::default());
  assert!(matches!(result, Err(Error::LengthOverflow)), "{result:?}");
}

#[test]
fn bzip2_beyond_encode_budget_stays_literal() {
  let plain = vec![0u8; 2 << 20];
  let mut encoder = BzEncoder::new(Vec::new(), Compression::new(9));
  encoder.write_all(&plain).expect("bzip2");
  let bomb = encoder.finish().expect("bzip2 finish");
  for (max_total_output, kind) in [(1 << 20, SegmentKind::Lit), (1 << 32, SegmentKind::Bzip2)] {
    let config = EncodeConfig {
      max_total_output,
      ..EncodeConfig::default()
    };
    let encoded = encode_bytes(&bomb, &config).expect("encode");
    let segments = decode_segments(&encoded);
    assert!(segments.iter().all(|s| s.kind == kind as u8), "budget {max_total_output}");
    assert_eq!(decode_bytes(&encoded, &DecodeConfig::default()).expect("decode"), bomb);
  }
}

#[tokio::test]
async fn segment_restoring_a_different_length_than_orig_len_is_rejected() {
  let payload = [BASE64_ENGINE.encode(b"forged base64 segment length").as_bytes(), b"\r\n"].concat();
  let encoded = encode_bytes(&payload, &EncodeConfig::default()).expect("encode");
  let mut file = Pcf2File::decode(encoded.as_slice()).expect("pcf2");
  assert_eq!(file.segments.len(), 1);
  assert_eq!(file.segments[0].kind, SegmentKind::Base64 as u8);
  let orig_len = file.segments[0].orig_len;
  for forged in [orig_len - 1, orig_len + 1] {
    file.segments[0].orig_len = forged;
    let mut forged_bytes = Vec::new();
    Pcf2File::from_segments(file.segments.clone())
      .encode(&mut forged_bytes)
      .expect("encode");
    let expected = |err: Error| matches!(err, Error::SizeMismatch { expected, actual } if expected == forged && actual == orig_len);
    assert!(expected(decode_bytes(&forged_bytes, &DecodeConfig::default()).unwrap_err()));
    let reader = AsyncCursor::new(forged_bytes);
    assert!(expected(decode_async(reader, &DecodeConfig::default()).await.unwrap_err()));
  }
}

#[test]
fn png_idat_lengths_that_overflow_are_rejected() {
  let encoded = encode_bytes(PNG_FIXTURE, &EncodeConfig::default()).expect("encode");
  let segments = decode_segments(&encoded);
  let png = segments.iter().find(|s| s.kind == SegmentKind::PngIdat as u8).expect("png segment");
  let idat_len = single_idat_len(png);
  // Two lengths, each a valid `usize`, whose sum wraps to the real IDAT
  // length.
  let mut forged = png.meta[..9].to_vec();
  forged.extend_from_slice(&varint(2));
  forged.extend_from_slice(&varint(usize::MAX as u64));
  forged.extend_from_slice(&varint(idat_len + 1));
  forged.push(1);
  forged.extend_from_slice(&[0; 8]);
  let forged = Pcf2Segment {
    meta: forged,
    ..png.clone()
  };
  let err = decode_segment(&forged, &DecodeConfig::default(), 0, &mut DecodeBudget::default()).unwrap_err();
  assert!(matches!(err, Error::InvalidSegment("png idat length mismatch")), "{err:?}");
}

/// Total IDAT length of a single-chunk PNG segment.
fn single_idat_len(segment: &Pcf2Segment) -> u64 {
  let mut offset = 9;
  assert_eq!(read_varint_slice(&segment.meta, &mut offset).expect("count"), 1);
  read_varint_slice(&segment.meta, &mut offset).expect("length")
}

#[tokio::test]
async fn malformed_short_and_truncated_inputs_never_panic() {
  let bad_six: &[u8] = b"\x00\x05\x00\x00\x00\x00";
  let bad_gzip_fextra: Vec<u8> = vec![0x1f, 0x8b, 8, 0x04, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 0, 0, 0, 0, 0, 0];
  let bad_gzip_fname: Vec<u8> = vec![
    0x1f, 0x8b, 8, 0x08, 0, 0, 0, 0, 0, 0, b'a', b'b', b'c', b'd', b'e', b'f', b'g', b'h',
  ];
  let mut bad_zip = vec![0u8; 40];
  bad_zip[..4].copy_from_slice(b"PK\x03\x04");
  bad_zip[8..10].copy_from_slice(&8u16.to_le_bytes());
  bad_zip[18..22].copy_from_slice(&u32::MAX.to_le_bytes());
  let mut bad_png = b"\x89PNG\r\n\x1a\n".to_vec();
  bad_png.extend_from_slice(&u32::MAX.to_be_bytes());
  bad_png.extend_from_slice(b"IDAT");
  bad_png.extend_from_slice(&[0u8; 8]);

  let decode_cfg = DecodeConfig {
    max_total_output: 1 << 20,
    ..DecodeConfig::default()
  };
  let recover_cfg = DecodeConfig {
    recover: true,
    ..decode_cfg.clone()
  };
  let encode_cfg = EncodeConfig {
    max_total_output: 1 << 20,
    enable_png_webp: true,
    enable_pdf_predictor: true,
    ..EncodeConfig::default()
  };

  for input in [
    bad_six,
    b"".as_slice(),
    b"PCF2".as_slice(),
    b"PCF2\x02\x00".as_slice(),
    &bad_gzip_fextra,
    &bad_gzip_fname,
    &bad_zip,
    &bad_png,
  ] {
    assert!(decode_bytes(input, &decode_cfg).is_err());
    assert!(decode_bytes(input, &recover_cfg).is_err());
    assert!(decode_reader(Cursor::new(input), std::io::sink(), &decode_cfg).is_err());
    assert!(decode_async(AsyncCursor::new(input.to_vec()), &decode_cfg).await.is_err());
    assert!(decode_image_for_display(input, &decode_cfg).is_err());

    let encoded = encode_bytes(input, &encode_cfg).expect("malformed input must encode cleanly as lit");
    assert_eq!(decode_bytes(&encoded, &decode_cfg).expect("roundtrip"), input);
  }

  // Every byte-prefix truncation of a valid PCF2 container returns Err cleanly.
  let valid = encode_bytes(PNG_FIXTURE, &encode_cfg).expect("encode png");
  for len in 0..valid.len() {
    let prefix = &valid[..len];
    assert!(decode_bytes(prefix, &decode_cfg).is_err(), "prefix {len}");
    assert!(
      decode_reader(Cursor::new(prefix), std::io::sink(), &decode_cfg).is_err(),
      "prefix {len}"
    );
  }
}

#[test]
fn deflate_zlib_gzip_zip_and_png_bombs_respect_encode_budget() {
  let zeros = vec![0u8; 2 << 20];
  let mut dflt_enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
  dflt_enc.write_all(&zeros).unwrap();
  let dflt_bomb = dflt_enc.finish().unwrap();

  let mut zlib_enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
  zlib_enc.write_all(&zeros).unwrap();
  let zlib_bomb = zlib_enc.finish().unwrap();

  let mut gzip_enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
  gzip_enc.write_all(&zeros).unwrap();
  let gzip_bomb = gzip_enc.finish().unwrap();

  // Multi-entry ZIP: two entries of 700 KiB zeros each (1.4 MiB total > 1 MiB
  // budget).
  let part = vec![0u8; 700 * 1024];
  let mut part_enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
  part_enc.write_all(&part).unwrap();
  let part_dflt = part_enc.finish().unwrap();
  let part_crc = crc32fast::hash(&part);
  let mut multi_zip = Vec::new();
  for name in [b"a.bin", b"b.bin"] {
    multi_zip.extend_from_slice(b"PK\x03\x04");
    multi_zip.extend_from_slice(&20u16.to_le_bytes());
    multi_zip.extend_from_slice(&0u16.to_le_bytes());
    multi_zip.extend_from_slice(&8u16.to_le_bytes());
    multi_zip.extend_from_slice(&0u16.to_le_bytes());
    multi_zip.extend_from_slice(&0u16.to_le_bytes());
    multi_zip.extend_from_slice(&part_crc.to_le_bytes());
    multi_zip.extend_from_slice(&(part_dflt.len() as u32).to_le_bytes());
    multi_zip.extend_from_slice(&(part.len() as u32).to_le_bytes());
    multi_zip.extend_from_slice(&(name.len() as u16).to_le_bytes());
    multi_zip.extend_from_slice(&0u16.to_le_bytes());
    multi_zip.extend_from_slice(name);
    multi_zip.extend_from_slice(&part_dflt);
  }

  let png_pixels = vec![0u8; 512 * 512 * 4]; // 1 MiB + 512 filter bytes > 1 MiB budget
  let png_bomb = build_png_rgba(512, 512, &png_pixels);

  let tight = EncodeConfig {
    max_total_output: 1 << 20,
    enable_png_webp: true,
    ..EncodeConfig::default()
  };
  for (name, bomb) in [
    ("deflate", dflt_bomb.as_slice()),
    ("zlib", zlib_bomb.as_slice()),
    ("gzip", gzip_bomb.as_slice()),
    ("multi_zip", multi_zip.as_slice()),
    ("png", png_bomb.as_slice()),
  ] {
    let encoded = encode_bytes(bomb, &tight).expect(name);
    let segments = decode_segments(&encoded);
    assert!(
      segments.iter().all(|s| s.kind == SegmentKind::Lit as u8),
      "{name} bomb must remain literal under tight budget"
    );
    assert_eq!(decode_bytes(&encoded, &DecodeConfig::default()).expect(name), bomb);
  }
}

#[tokio::test]
async fn decode_reader_streams_and_enforces_budgets_before_allocating() {
  // 1. Streaming roundtrip parity across transformed and literal payloads.
  let pdf = build_pdf_sample();
  let zip = build_zip_sample();
  for sample in [
    PNG_FIXTURE,
    JPG_FIXTURE,
    pdf.as_slice(),
    zip.as_slice(),
    b"literal bytes".as_slice(),
  ] {
    let encoded = encode_bytes(sample, &EncodeConfig::default()).expect("encode");
    let mut streamed = Vec::new();
    let written = decode_reader(Cursor::new(&encoded), &mut streamed, &DecodeConfig::default()).expect("decode_reader");
    assert_eq!(written as usize, sample.len());
    assert_eq!(streamed, sample);
  }

  // 2. Forged header claiming 1 TiB original_size fails immediately on all
  //    decoders.
  let mut forged_hdr = Vec::new();
  Pcf2Header::new(1 << 40).encode(&mut forged_hdr).unwrap();
  let tight = DecodeConfig {
    max_total_output: 1 << 20,
    ..DecodeConfig::default()
  };
  for err in [
    decode_bytes(&forged_hdr, &tight).unwrap_err(),
    decode_reader(Cursor::new(&forged_hdr), std::io::sink(), &tight).unwrap_err(),
    decode_async(AsyncCursor::new(forged_hdr.clone()), &tight).await.unwrap_err(),
  ] {
    assert!(matches!(err, Error::Other(ref m) if m == "decode output exceeds budget"), "{err:?}");
  }

  // 3. Forged segment claiming 1 TiB meta_len or data_len backed by infinite
  //    zeros
  // fails before reading the payload.
  for (meta_len, data_len) in [(1u64 << 40, 0u64), (0u64, 1u64 << 40)] {
    let mut prefix = Vec::new();
    Pcf2Header::new(64).encode(&mut prefix).unwrap();
    prefix.push(SegmentKind::Dflt as u8);
    prefix.push(0);
    write_varint_vec(64, &mut prefix);
    write_varint_vec(meta_len, &mut prefix);
    write_varint_vec(data_len, &mut prefix);
    let stream = std::io::Read::chain(Cursor::new(prefix), std::io::repeat(0));
    let err = decode_reader(stream, std::io::sink(), &tight).unwrap_err();
    assert!(matches!(err, Error::Other(ref m) if m == "decode output exceeds budget"), "{err:?}");
  }

  // 4. Streaming Lit segment aborts on bounded Write without draining the rest
  //    of the stream.
  struct CountingReader<R> {
    inner: R,
    read: std::sync::Arc<std::sync::atomic::AtomicU64>,
  }
  impl<R: std::io::Read> std::io::Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
      let n = self.inner.read(buf)?;
      self.read.fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
      Ok(n)
    }
  }
  struct CappedWriter {
    limit: usize,
    written: usize,
  }
  impl Write for CappedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
      if self.written + buf.len() > self.limit {
        return Err(std::io::Error::new(std::io::ErrorKind::FileTooLarge, "cap reached"));
      }
      self.written += buf.len();
      Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
      Ok(())
    }
  }

  let big_lit = encode_bytes(&vec![0x55u8; 512 * 1024], &EncodeConfig::default()).unwrap();
  let bytes_read = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
  let reader = CountingReader {
    inner: Cursor::new(&big_lit),
    read: bytes_read.clone(),
  };
  let err = decode_reader(reader, CappedWriter { limit: 64, written: 0 }, &DecodeConfig::default()).unwrap_err();
  assert!(matches!(err, Error::Io(_)), "{err:?}");
  assert!(
    bytes_read.load(std::sync::atomic::Ordering::Relaxed) <= 128 * 1024,
    "decode_reader drained too much after write failure"
  );

  // 5. Forged PngIdat WebP segment with 16383x16383 RGBA fails budget before
  //    WebP decode.
  let mut webp_meta = vec![1, 0, 1, 0x78, 0x9c, 0, 0, 0, 1];
  write_varint_vec(1, &mut webp_meta);
  write_varint_vec(6, &mut webp_meta);
  webp_meta.push(0); // no crc
  webp_meta.push(6); // RGBA
  write_varint_vec(16383, &mut webp_meta);
  write_varint_vec(16383, &mut webp_meta);
  webp_meta.resize(webp_meta.len() + 16383, 0);
  let webp_seg = Pcf2Segment {
    kind: SegmentKind::PngIdat as u8,
    flags: 0,
    orig_len: 18,
    meta: webp_meta,
    data: vec![0, 0],
  };
  let err = decode_segment(&webp_seg, &tight, 0, &mut DecodeBudget::default()).unwrap_err();
  assert!(matches!(err, Error::Other(ref m) if m == "decode output exceeds budget"), "{err:?}");

  // 6. Pcf2File::encode rejects overflowing u64 orig_len sums cleanly.
  let overflow_file = Pcf2File {
    header: Pcf2Header::new(0),
    segments: vec![
      Pcf2Segment {
        kind: SegmentKind::Dflt as u8,
        flags: 0,
        orig_len: u64::MAX,
        meta: Vec::new(),
        data: Vec::new(),
      },
      Pcf2Segment {
        kind: SegmentKind::Dflt as u8,
        flags: 0,
        orig_len: 1,
        meta: Vec::new(),
        data: Vec::new(),
      },
    ],
  };
  assert!(matches!(overflow_file.encode(&mut Vec::new()), Err(Error::LengthOverflow)));
}
