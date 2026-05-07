use std::{
  io::Cursor,
  pin::Pin,
  task::{Context, Poll},
};

use tokio::io::ReadBuf;

use super::*;

const PNG_FIXTURE: &[u8] = include_bytes!("../../fixtures/minimal.png");
const JPG_FIXTURE: &[u8] = include_bytes!("../../fixtures/minimal.jpg");

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

  let preflate = preflate_analyze(&deflated).expect("preflate analyze");
  let (deflate_len, corrections, plain) = parse_preflate_output(&preflate).expect("preflate parse");
  assert_eq!(deflate_len, deflated.len() as u64);
  let rebuilt = preflate_reencode(&corrections, &plain).expect("preflate reencode");
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
  let mut config = EncodeConfig::default();
  config.enable_png_webp = true;
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
  let mut config = EncodeConfig::default();
  config.enable_png_webp = true;
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
fn roundtrip_base64_segment() {
  let payload = b"base64 payload data";
  let encoded_payload = BASE64_ENGINE.encode(payload);
  let mut base64_bytes = Vec::new();
  base64_bytes.extend_from_slice(b"Content-Transfer-Encoding: base64\r\n\r\n");
  for chunk in encoded_payload.as_bytes().chunks(8) {
    base64_bytes.extend_from_slice(chunk);
    base64_bytes.extend_from_slice(b"\r\n");
  }

  let encoded = encode_bytes(&base64_bytes, &EncodeConfig::default()).expect("encode");
  let segments = decode_segments(&encoded);
  assert_has_kind(&segments, SegmentKind::Base64);
  let decoded = decode_bytes(&encoded, &DecodeConfig::default()).expect("decode");
  assert_eq!(decoded, base64_bytes);
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
  assert_has_kind(&segments, SegmentKind::PdfFlate);
  let decoded = decode_bytes(&encoded, &DecodeConfig::default()).expect("decode");
  assert_eq!(decoded, pdf);
}

#[test]
fn roundtrip_pdf_predictor_segment() {
  let pdf = build_pdf_predictor_sample();
  let mut config = EncodeConfig::default();
  config.enable_pdf_predictor = true;
  let encoded = encode_bytes(&pdf, &config).expect("encode");
  let segments = decode_segments(&encoded);
  let segment = segments
    .iter()
    .find(|segment| segment.kind == SegmentKind::PdfFlate as u8)
    .expect("pdf flate");
  assert_eq!(segment.meta[0], 0);
  assert_eq!(segment.meta[3], 1);
  let decoded = decode_bytes(&encoded, &DecodeConfig::default()).expect("decode");
  assert_eq!(decoded, pdf);
}

#[test]
fn roundtrip_pdf_bmp_segment() {
  let pdf = build_pdf_bmp_sample();
  let mut config = EncodeConfig::default();
  config.enable_pdf_bmp_fallback = true;
  let encoded = encode_bytes(&pdf, &config).expect("encode");
  let segments = decode_segments(&encoded);
  let segment = segments
    .iter()
    .find(|segment| segment.kind == SegmentKind::PdfFlate as u8)
    .expect("pdf flate");
  assert_eq!(segment.meta[0], 0);
  assert_eq!(segment.meta[3], 2);
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
