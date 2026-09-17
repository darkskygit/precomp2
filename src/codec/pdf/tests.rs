use std::io::Write;

use super::*;
use crate::{decode_async, decode_bytes, encode_bytes_with_stats};

fn pdf(data: &[u8], dict: &str) -> Vec<u8> {
  let mut out = format!("%PDF-1.7\n1 0 obj\n<< /Length {} {dict} >>\nstream\r\n", data.len()).into_bytes();
  out.extend_from_slice(data);
  out.extend_from_slice(b"\r\nendstream\nendobj\n%%EOF\n");
  out
}

fn zlib(data: &[u8]) -> Vec<u8> {
  let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::none());
  z.write_all(data).unwrap();
  z.finish().unwrap()
}

fn hex(data: &[u8]) -> Vec<u8> {
  let mut out = data.iter().map(|b| format!("{b:02X} ")).collect::<String>().into_bytes();
  out.extend_from_slice(b">\ntrailing");
  out
}

fn ascii85(data: &[u8]) -> Vec<u8> {
  let mut out = Vec::new();
  for chunk in data.chunks(4) {
    let mut word = [0; 4];
    word[..chunk.len()].copy_from_slice(chunk);
    let mut word = u32::from_be_bytes(word);
    let mut digits = [0; 5];
    for digit in digits.iter_mut().rev() {
      *digit = (word % 85) as u8 + b'!';
      word /= 85;
    }
    out.extend_from_slice(&digits[..chunk.len() + 1]);
    out.push(b'\n');
  }
  out.extend_from_slice(b"~>");
  out
}

fn roundtrip(input: &[u8]) -> PdfStats {
  let config = EncodeConfig {
    enable_pdf_predictor: true,
    max_expand_ratio: 1024.0,
    ..Default::default()
  };
  let (encoded, stats) = encode_bytes_with_stats(input, &config).unwrap();
  assert_eq!(decode_bytes(&encoded, &DecodeConfig::default()).unwrap(), input);
  stats
}

#[test]
fn filter_chains_and_opaque_inner_filters() {
  let text = b"BT /F1 12 Tf (exact endstream bytes) Tj ET\n".repeat(32);
  for (bytes, filters, flates, ascii) in [
    (zlib(&text), "/FlateDecode", 1, 0),
    (hex(&zlib(&text)), "[/ASCIIHexDecode /FlateDecode]", 1, 1),
    (ascii85(&zlib(&text)), "[/ASCII85Decode /FlateDecode]", 1, 1),
    (hex(&zlib(&zlib(&text))), "[/AHx /Fl /Fl]", 2, 1),
    (hex(&text), "[/AHx /Unknown]", 0, 1),
    (b"z !!!!! 87cURD_*#TDfTZ)+T~>tail".to_vec(), "/A85", 0, 1),
  ] {
    let stats = roundtrip(&pdf(&bytes, &format!("/Filter {filters}")));
    assert_eq!((stats.transformed, stats.flate, stats.ascii), (1, flates, ascii));
  }
  let bytes = hex(&zlib(&[0, 1, 2, 3, 2, 1, 1, 1]));
  let stats = roundtrip(&pdf(
    &bytes,
    "/Filter [/AHx /Fl] /DecodeParms [null << /Predictor 15 /Columns 3 >>]",
  ));
  assert_eq!((stats.ascii, stats.flate), (1, 1));
}

#[test]
fn image_samples_masks_and_padding() {
  for bits in [1usize, 2, 4, 8, 16] {
    for (colors, space) in [
      (1, "/DeviceGray"),
      (3, "/DeviceRGB"),
      (4, "/DeviceCMYK"),
      (1, "[/Indexed /DeviceRGB 1 <000000ffffff>]"),
    ] {
      let row = (127 * colors * bits).div_ceil(8);
      let samples = vec![0x35; row * 64];
      let dict = format!("/Subtype /Image /Width 127 /Height 64 /BitsPerComponent {bits} /ColorSpace {space} /Decode [1 0] /Filter /Fl");
      let stats = roundtrip(&pdf(&zlib(&samples), &dict));
      assert_eq!((stats.flate, stats.images), (1, 1), "{dict}");
    }
  }
  for extra in [
    "/ImageMask true",
    "/ColorSpace /DeviceGray /BitsPerComponent 1 /SMask 4 0 R",
    "/ColorSpace /DeviceGray /BitsPerComponent 1 /Mask 4 0 R",
  ] {
    let stats = roundtrip(&pdf(
      &zlib(&[0xa5; 1024]),
      &format!("/Subtype /Image /Width 127 /Height 64 /Filter /Fl {extra}"),
    ));
    assert_eq!(stats.images, 1);
  }
}

#[test]
fn jpeg_and_qpdf_fixtures() {
  let jpeg = include_bytes!("../../../fixtures/minimal.jpg");
  let stats = roundtrip(&pdf(jpeg, "/Filter /DCTDecode"));
  assert_eq!(stats.dct, 1);
  let stats = roundtrip(&pdf(&hex(jpeg), "/Filter [/ASCIIHexDecode /DCTDecode]"));
  assert_eq!((stats.ascii, stats.dct), (1, 1));
  let stats = roundtrip(&pdf(&ascii85(jpeg), "/Filter [/ASCII85Decode /DCTDecode]"));
  assert_eq!((stats.ascii, stats.dct), (1, 1));
  let stats = roundtrip(&pdf(&zlib(jpeg), "/Filter [/FlateDecode /DCTDecode]"));
  assert_eq!((stats.flate, stats.dct), (1, 1));
  for bytes in [
    include_bytes!("../../../fixtures/qpdf/filter-abbreviation.pdf").as_slice(),
    include_bytes!("../../../fixtures/qpdf/image-streams-small.pdf").as_slice(),
    include_bytes!("../../../fixtures/qpdf/jpeg-qstream.pdf").as_slice(),
  ] {
    assert!(roundtrip(bytes).transformed > 0);
  }
  assert!(roundtrip(include_bytes!("../../../fixtures/qpdf/image-streams.pdf")).images > 0);
}

#[tokio::test]
async fn legacy_and_current_versions() {
  for (encoded, original) in [
    (
      include_bytes!("../../../fixtures/pdf-v1-0.pcf").as_slice(),
      include_bytes!("../../../fixtures/pdf-v1-0.pdf").as_slice(),
    ),
    (
      include_bytes!("../../../fixtures/pdf-v1-1.pcf").as_slice(),
      include_bytes!("../../../fixtures/pdf-v1-1.pdf").as_slice(),
    ),
    (
      include_bytes!("../../../fixtures/pdf-v1-2.pcf").as_slice(),
      include_bytes!("../../../fixtures/pdf-v1-2.pdf").as_slice(),
    ),
  ] {
    assert_eq!(decode_bytes(encoded, &DecodeConfig::default()).unwrap(), original);
    assert_eq!(decode_async(encoded, &DecodeConfig::default()).await.unwrap(), original);
  }
  let original = pdf(&hex(b"hello"), "/Filter /AHx");
  let (mut encoded, _) = encode_bytes_with_stats(&original, &EncodeConfig::default()).unwrap();
  assert_eq!(encoded[4], 2);
  assert_eq!(decode_async(encoded.as_slice(), &DecodeConfig::default()).await.unwrap(), original);
  encoded[4] = 1;
  assert!(decode_bytes(&encoded, &DecodeConfig::default()).is_err());
  assert!(decode_async(encoded.as_slice(), &DecodeConfig::default()).await.is_err());
}

#[test]
fn damaged_streams_and_budget_fall_back() {
  for (data, dict) in [
    (b"bad zlib".as_slice(), "/Filter /Fl"),
    (b"0g>", "/Filter /AHx"),
    (b"!z~>", "/Filter /A85"),
    (b"abcd", "/Filter [/Fl /Fl] /DecodeParms [null]"),
  ] {
    assert_eq!(roundtrip(&pdf(data, dict)).transformed, 0);
  }
  let input = pdf(&zlib(&[0; 1024]), "/Filter /Fl");
  let config = EncodeConfig {
    max_total_output: 10,
    ..Default::default()
  };
  let (encoded, stats) = encode_bytes_with_stats(&input, &config).unwrap();
  assert_eq!(stats.transformed, 0);
  assert_eq!(decode_bytes(&encoded, &DecodeConfig::default()).unwrap(), input);
}
