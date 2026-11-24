use std::env;
use std::fs;
use std::process;
use std::time::Instant;

use precomp2::codec::lz4::{Compressor, compress_bound};
use precomp2::{DecodeConfig, EncodeConfig, decode_bytes, decode_image_for_display, encode_bytes};

fn usage() -> ! {
  eprintln!(
    "usage:\n  \
     precomp2-bench-rust encode <in> <out> [--png-webp] [--pdf-predictor] [--iters N]\n  \
     precomp2-bench-rust decode <in> <out> [--recover] [--iters N]\n  \
     precomp2-bench-rust display <in> <out> [--iters N]\n  \
     precomp2-bench-rust lz4hc <v1_10_0_l12|v1_9_1_l12|r131_l16> <in> <out> [--iters N]\n  \
     precomp2-bench-rust webp-decode <in.webp> <out.raw>"
  );
  process::exit(2);
}

fn parse_iters(args: &[String]) -> usize {
  let mut i = 0;
  while i < args.len() {
    if args[i] == "--iters" {
      if i + 1 >= args.len() {
        usage();
      }
      let n: usize = args[i + 1].parse().unwrap_or_else(|_| usage());
      if n == 0 {
        usage();
      }
      return n;
    }
    i += 1;
  }
  1
}

fn read_vmhwm_kib() -> u64 {
  if let Ok(status) = fs::read_to_string("/proc/self/status") {
    for line in status.lines() {
      if let Some(rest) = line.strip_prefix("VmHWM:") {
        if let Some(tok) = rest.split_whitespace().next() {
          if let Ok(v) = tok.parse::<u64>() {
            return v;
          }
        }
      }
    }
  }
  0
}

fn main() {
  let args: Vec<String> = env::args().collect();
  if args.len() < 4 {
    usage();
  }
  let cmd = args[1].as_str();
  match cmd {
    "encode" => {
      let in_path = &args[2];
      let out_path = &args[3];
      let flags = &args[4..];
      let iters = parse_iters(flags);
      let mut cfg = EncodeConfig::default();
      for f in flags {
        if f == "--png-webp" {
          cfg.enable_png_webp = true;
        } else if f == "--pdf-predictor" {
          cfg.enable_pdf_predictor = true;
        }
      }
      let input = fs::read(in_path).unwrap_or_else(|e| {
        eprintln!("read {in_path}: {e}");
        process::exit(1);
      });
      let start = Instant::now();
      let mut out = Vec::new();
      for _ in 0..iters {
        out = encode_bytes(&input, &cfg).unwrap_or_else(|e| {
          eprintln!("encode failed: {e}");
          process::exit(1);
        });
      }
      let elapsed_us = start.elapsed().as_micros();
      fs::write(out_path, &out).unwrap_or_else(|e| {
        eprintln!("write {out_path}: {e}");
        process::exit(1);
      });
      println!(
        "elapsed_us={elapsed_us} iters={iters} out_bytes={} vmhwm_kib={}",
        out.len(),
        read_vmhwm_kib()
      );
    }
    "decode" => {
      let in_path = &args[2];
      let out_path = &args[3];
      let flags = &args[4..];
      let iters = parse_iters(flags);
      let mut cfg = DecodeConfig::default();
      for f in flags {
        if f == "--recover" {
          cfg.recover = true;
        }
      }
      let input = fs::read(in_path).unwrap_or_else(|e| {
        eprintln!("read {in_path}: {e}");
        process::exit(1);
      });
      let start = Instant::now();
      let mut out = Vec::new();
      for _ in 0..iters {
        out = decode_bytes(&input, &cfg).unwrap_or_else(|e| {
          eprintln!("decode failed: {e}");
          process::exit(1);
        });
      }
      let elapsed_us = start.elapsed().as_micros();
      fs::write(out_path, &out).unwrap_or_else(|e| {
        eprintln!("write {out_path}: {e}");
        process::exit(1);
      });
      println!(
        "elapsed_us={elapsed_us} iters={iters} out_bytes={} vmhwm_kib={}",
        out.len(),
        read_vmhwm_kib()
      );
    }
    "display" => {
      let in_path = &args[2];
      let out_path = &args[3];
      let flags = &args[4..];
      let iters = parse_iters(flags);
      let cfg = DecodeConfig::default();
      let input = fs::read(in_path).unwrap_or_else(|e| {
        eprintln!("read {in_path}: {e}");
        process::exit(1);
      });
      let start = Instant::now();
      let mut out = Vec::new();
      for _ in 0..iters {
        match decode_image_for_display(&input, &cfg) {
          Ok(Some(img)) => out = img,
          Ok(None) => {
            eprintln!("decode_image_for_display returned None");
            process::exit(1);
          }
          Err(e) => {
            eprintln!("decode_image_for_display failed: {e}");
            process::exit(1);
          }
        }
      }
      let elapsed_us = start.elapsed().as_micros();
      fs::write(out_path, &out).unwrap_or_else(|e| {
        eprintln!("write {out_path}: {e}");
        process::exit(1);
      });
      println!(
        "elapsed_us={elapsed_us} iters={iters} out_bytes={} vmhwm_kib={}",
        out.len(),
        read_vmhwm_kib()
      );
    }
    "lz4hc" => {
      if args.len() < 5 {
        usage();
      }
      let comp = match args[2].as_str() {
        "v1_10_0_l12" => Compressor::V1_10_0_L12,
        "v1_9_1_l12" => Compressor::V1_9_1_L12,
        "r131_l16" => Compressor::R131_L16,
        _ => usage(),
      };
      let in_path = &args[3];
      let out_path = &args[4];
      let flags = &args[5..];
      let iters = parse_iters(flags);
      let input = fs::read(in_path).unwrap_or_else(|e| {
        eprintln!("read {in_path}: {e}");
        process::exit(1);
      });
      let bound = compress_bound(input.len()).max(1);
      let start = Instant::now();
      let mut out = Vec::new();
      for _ in 0..iters {
        out = comp.compress(&input, bound).unwrap_or_else(|| {
          eprintln!("lz4hc compress returned None");
          process::exit(1);
        });
      }
      let elapsed_us = start.elapsed().as_micros();
      fs::write(out_path, &out).unwrap_or_else(|e| {
        eprintln!("write {out_path}: {e}");
        process::exit(1);
      });
      println!(
        "elapsed_us={elapsed_us} iters={iters} out_bytes={} vmhwm_kib={}",
        out.len(),
        read_vmhwm_kib()
      );
    }
    "webp-decode" => {
      let in_path = &args[2];
      let out_path = &args[3];
      let input = fs::read(in_path).unwrap_or_else(|e| {
        eprintln!("read {in_path}: {e}");
        process::exit(1);
      });
      let decoded = webp::Decoder::new(&input)
        .decode()
        .unwrap_or_else(|| {
          eprintln!("webp decode failed for {in_path}");
          process::exit(1);
        });
      let raw = &*decoded;
      fs::write(out_path, raw).unwrap_or_else(|e| {
        eprintln!("write {out_path}: {e}");
        process::exit(1);
      });
      println!(
        "width={} height={} out_bytes={}",
        decoded.width(),
        decoded.height(),
        raw.len()
      );
    }
    _ => usage(),
  }
}
