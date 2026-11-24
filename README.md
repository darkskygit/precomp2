# precomp2

[![CI](https://github.com/darkskygit/precomp2/actions/workflows/ci.yml/badge.svg)](https://github.com/darkskygit/precomp2/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/precomp2.svg)](https://crates.io/crates/precomp2)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`precomp2` is a reversible preprocessing library for compressed and container data, implemented in both **Rust** (`rust/`) and **pure Pion** (`pion/`). It transforms recognizable streams into PCF2 segments that outer compressors can compress more effectively, then restores the original bytes identically.

The project is inspired by [precomp-cpp](https://github.com/schnaader/precomp-cpp) and [preflate-rs](https://github.com/microsoft/preflate-rs).

## Implementations

| Aspect | Rust (`rust/`) | Pure Pion (`pion/packages/precomp2`) |
| --- | --- | --- |
| **Targets** | Native (`#![forbid(unsafe_code)]`) | Native & **Wasm32** (`--target wasm32 --no-main`) |
| **Runtime / FFI** | Rust std + Tokio async decode (`libwebp-sys` C bindings for WebP) | **100% pure Pion**, zero C/FFI, zero `World` dependency |
| **Codec stack** | `base64`, `bzip2`, `flate2`, `preflate-rs`, `lepton_jpeg`, `lz4_flex`, `webp` | [`AFK-surf/pion-packages`](https://github.com/AFK-surf/pion-packages) (`base64`, `bzip2`, `flate`, `lepton`, `preflate`, `webp`) + built-in LZ4HC |
| **API parity** | `encode_bytes`, `encode_bytes_with_stats`, `decode_bytes` (incl. `recover`), `decode_async`, `decode_image_for_display`, `analyze_unityfs` | `encode_bytes`, `encode_bytes_with_stats`, `decode_bytes` (incl. `recover`), `decode_image_for_display`, `analyze_unityfs` |

## Supported Formats & Cross-Language Interoperability

Both implementations share the same **PCF2** container framing (`SegmentKind` tags, varints, recursion depth/output budgets, and legacy v1 containers):

| Format / Container | PCF2 Segment Kind(s) | Rust | Pion | Rust ↔ Pion Wire Interop |
| --- | --- | :---: | :---: | --- |
| Raw / nested data | `Lit` (0), `Nested` (6) | ✅ | ✅ | **Byte-exact bidirectional** |
| Base64 (all 3 line layouts + MIME) | `Base64` (7) | ✅ | ✅ | **Byte-exact bidirectional** |
| bzip2 (single & multi-block) | `Bzip2` (8) | ✅ | ✅ | **Byte-exact bidirectional** (0-penalty parity with `libbz2-rs-sys`) |
| UnityFS (LZ4HC `V1_10_0_L12`, `V1_9_1_L12`, `R131_L16`) | `UnityFs` (11) | ✅ | ✅ | **Byte-exact bidirectional** (SHA-256 identical LZ4HC blocks) |
| Legacy v1 PCF2 (`Lit` containers) | `Lit` (0) | ✅ | ✅ | **Byte-exact bidirectional** (`fixtures/pdf-v1-{0,1,2}.pcf`) |
| PNG (`IDAT` + lossless WebP pixel carrier) | `PngIdat` (4) | ✅ | ✅ | **Fast preview** (`decode_image_for_display`) bidirectional; full `decode_bytes` uses language-native `preflate` corrections |
| DEFLATE / zlib / gzip / ZIP / SWF | `Dflt` (1), `Zlib` (2), `Gzip` (3) | ✅ | ✅ | Language-native inner corrections (`preflate-rs` vs Pion `preflate`) |
| JPEG (incl. MJPG without DHT) | `Jpg` (5) | ✅ | ✅ | Language-native inner payloads (`lepton_jpeg` vs Pion `lepton`) |
| PDF streams (Flate, DCT, ASCIIHex/85, predictors, legacy BMP) | `PdfFlate` (9), `PdfStream` (10) | ✅ | ✅ | Language-native inner payloads (lossless roundtrip within each language) |

## Repository Layout

- `rust/`: Reference Rust crate (`precomp2`, `#![forbid(unsafe_code)]`), including `tests/lz4-reference`, `examples/`, `fuzz/`, and `tools/lz4-matrix/`.
- `pion/`: Pure-Pion workspace (`packages/precomp2`), pulling `base64`, `bzip2`, `flate`, `lepton`, `preflate`, and `webp` directly from [`AFK-surf/pion-packages`](https://github.com/AFK-surf/pion-packages).
- `fixtures/`: Shared binary test fixtures (JPEG, PNG, legacy v1 PCF2, and `qpdf` PDFs).
- `benchmarks/`: Head-to-head Rust (`--release`) vs Pion (`--release`) benchmark suite and cross-language PCF2 interoperability harness (`benchmarks/run.sh`, `benchmarks/RESULTS.md`).

## Usage

### Rust

Add `precomp2` to your `Cargo.toml`:

```toml
[dependencies]
precomp2 = "0.3.2"
```

```rust
use precomp2::{DecodeConfig, EncodeConfig};

fn main() -> precomp2::Result<()> {
    let input = b"payload";
    let encoded = precomp2::encode_bytes(input, &EncodeConfig::default())?;
    let decoded = precomp2::decode_bytes(&encoded, &DecodeConfig::default())?;

    assert_eq!(decoded, input);
    Ok(())
}
```

### Pure Pion (Native & Wasm32)

The Pion package under `pion/packages/precomp2` is 100% pure Pion with zero `World` or C/FFI dependencies and compiles cleanly to both native binaries and standalone `wasm32` modules.

Add it to your `pion.toml`:

```toml
[dependencies]
precomp2 = { git = "https://github.com/darkskygit/precomp2.git", subdir = "pion/packages/precomp2" }
```

```pion
import "precomp2"

pub fn roundtrip(input : Bytes) -> Result[Bytes, precomp2::Error] {
  let encoded = match precomp2::encode_bytes(input, precomp2::EncodeConfig::default()) {
    Ok(bytes) => bytes
    Err(err) => return Err(err)
  }
  precomp2::decode_bytes(encoded, precomp2::DecodeConfig::default())
}

pub fn fast_preview(pcf2 : Bytes) -> Result[Option[Bytes], precomp2::Error] {
  precomp2::decode_image_for_display(pcf2, precomp2::DecodeConfig::default())
}
```

Compile directly to Wasm32 without host bindings:

```sh
pionc build --target wasm32 --no-main -o /tmp/precomp2.wasm pion/packages/precomp2
```

## Build & Test

### Rust

`cargo test` requires a C compiler to build the vendored `liblz4` releases in `rust/tests/lz4-reference`:

```sh
cargo test --workspace
PRECOMP2_UNITYFS_FILES=list.txt cargo test --release --test unityfs_roundtrip -- --ignored --nocapture
cargo +nightly fuzz run lz4_reference   # see rust/fuzz/README.md
```

### Pion

Requires [`pionc`](https://github.com/AFK-surf/pion):

```sh
pionc fmt --check pion
pionc check --json pion
pionc test --json pion
pionc build --target wasm32 --no-main -o /tmp/precomp2.wasm pion/packages/precomp2
```

## Benchmarks (Rust vs Pion)

Measured on Linux x86_64 (`--release`, `taskset -c 0`, 2 warmup + 15 interleaved rounds / 11 rounds for >500 ms workloads, mandatory SHA-256 verification). See [`benchmarks/RESULTS.md`](benchmarks/RESULTS.md) for the full report and cross-language interoperability matrix.

> **Note:** The committed timings below were collected on a shared, loaded VM (1m load average `21.69` on `8` vCPU), so absolute latencies and Pion/Rust ratios are indicative and can vary noticeably under contention. Run `bash benchmarks/run.sh --render-results` on an idle host for authoritative measurements.

| Workload | Op | Rust Median (ms) | Pion Median (ms) | Ratio (Pion / Rust) |
| --- | --- | ---: | ---: | ---: |
| `lz4hc/v1_10_0_l12` (4 MiB) | compress | 711.36 | 1647.36 | 2.32x |
| `unityfs/v6_4m` (4 MiB) | encode / decode | 625.25 / 546.01 | 1657.50 / 1503.36 | 2.65x / 2.75x |
| `zlib/L6_2m` (2 MiB) | encode / decode | 102.57 / 39.83 | 241.22 / 76.53 | 2.35x / 1.92x |
| `bzip2/L6_512k` (512 KiB) | encode / decode | 91.57 / 122.31 | 236.52 / 267.34 | 2.58x / 2.19x |
| `png_webp/rgba_512x512` | encode / decode | 343.23 / 51.29 | 353.95 / 138.63 | 1.03x / 2.70x |
| `jpeg/pdf_extracted` | encode / decode | 6.51 / 4.54 | 6.27 / 2.94 | 0.96x / 0.65x |
| `pdf/image-streams.pdf` (2.97 MB, segs R=5/P=6) | encode / decode | 75.59 / 47.45 | 157.10 / 79.34 | 2.08x / 1.67x |

Run interop checks and benchmarks locally:

```sh
bash benchmarks/run.sh --interop
bash benchmarks/run.sh --render-results
```

## Status

This project is early-stage. The on-disk PCF2 format should be treated as unstable until a 1.0 release.

## License

Licensed under the Apache License, Version 2.0.
