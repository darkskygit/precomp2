#![no_main]

use std::io::{self, Cursor};

use libfuzzer_sys::fuzz_target;
use precomp2::{DecodeConfig, EncodeConfig, decode_bytes, decode_image_for_display, decode_reader, encode_bytes};

const BUDGET: u64 = 1 << 20;

fuzz_target!(|data: &[u8]| {
  let decode_cfg = DecodeConfig {
    max_depth: 4,
    max_total_output: BUDGET,
    recover: false,
  };
  let _ = decode_bytes(data, &decode_cfg);
  let _ = decode_reader(Cursor::new(data), io::sink(), &decode_cfg);
  let _ = decode_image_for_display(data, &decode_cfg);

  let recover_cfg = DecodeConfig {
    recover: true,
    ..decode_cfg.clone()
  };
  let _ = decode_bytes(data, &recover_cfg);

  if data.len() <= 64 * 1024 {
    let encode_cfg = EncodeConfig {
      max_total_output: BUDGET,
      enable_png_webp: true,
      enable_pdf_predictor: true,
      ..EncodeConfig::default()
    };
    if let Ok(encoded) = encode_bytes(data, &encode_cfg) {
      let decoded = decode_bytes(&encoded, &decode_cfg).expect("encoded output must decode");
      assert_eq!(decoded, data);
      let mut streamed = Vec::new();
      let written = decode_reader(Cursor::new(&encoded), &mut streamed, &decode_cfg).expect("encoded output must stream-decode");
      assert_eq!(written as usize, data.len());
      assert_eq!(streamed, data);
    }
  }
});
