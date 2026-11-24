# precomp2

`precomp2` is a reversible preprocessing library for compressed and container data, implemented in both **Rust** (`rust/`) and **pure Pion** (`pion/`). It transforms recognizable streams into PCF2 segments that outer compressors can compress more effectively, then restores the original bytes identically.

The project is inspired by [precomp-cpp](https://github.com/schnaader/precomp-cpp) and [preflate-rs](https://github.com/microsoft/preflate-rs).

## Repository Layout

- `rust/`: Reference Rust crate (`#![forbid(unsafe_code)]`), including `tests/lz4-reference`, `examples/`, `fuzz/`, and `tools/lz4-matrix/`.
- `pion/`: Pure-Pion workspace (`World`-free, zero C/FFI, Wasm32-compatible) with `packages/bzip2`, `packages/preflate`, `packages/lepton`, and `packages/precomp2`.
- `fixtures/`: Shared binary test fixtures (JPEG, PNG, legacy v1 PCF2, and `qpdf` PDFs).
- `benchmarks/`: Head-to-head Rust (`--release`) vs Pion (`--release`) benchmark suite and cross-language PCF2 interoperability harness (`benchmarks/run.sh`, `benchmarks/RESULTS.md`).

## Features

- Lossless encode/decode APIs for byte slices (and async decode input in Rust) plus fast image preview extraction (`decode_image_for_display`).
- PCF2 segment container supporting `Lit`, `Nested`, `Dflt`, `Zlib`, `Gzip`, `PngIdat` (including lossless WebP pixel carrier), `Jpg` (including MJPG without DHT), `Base64`, `Bzip2`, legacy `PdfFlate`, `PdfStream`, and `UnityFs` (byte-exact LZ4HC `V1_10_0_L12`, `V1_9_1_L12`, `R131_L16`).
- Recursive preprocessing with depth and output budget limits.
- Configurable PNG and PDF preprocessing through `EncodeConfig`.

## Usage

### Rust

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

### Pion

```pion
import "precomp2"

fn roundtrip(input : Bytes) -> Result[Bytes, precomp2::Error] {
  let encoded = precomp2::encode_bytes(input, precomp2::EncodeConfig::default())?
  precomp2::decode_bytes(encoded, precomp2::DecodeConfig::default())
}
```

## Build & Test

### Rust

`cargo test` requires a C compiler to build the vendored `liblz4` releases in `rust/tests/lz4-reference`:

```sh
cargo test
PRECOMP2_UNITYFS_FILES=list.txt cargo test --release --test unityfs_roundtrip -- --ignored --nocapture
cargo +nightly fuzz run lz4_reference   # see rust/fuzz/README.md
```

### Pion

```sh
pionc fmt --check pion
pionc check --json pion
pionc test --json pion
pionc build --target wasm32 --no-main -o /tmp/precomp2.wasm pion/packages/precomp2
```

## Cross-Language PCF2 Interoperability

- **Directly interoperable (`Rust <-> Pion`)**: `Lit`, `Nested`, `Base64` (line cases 0/1/2 + MIME headers), `Bzip2` (zero-penalty parity with `libbz2-rs-sys`), `UnityFs` (byte-exact LZ4HC across all three compressor classes), `PngIdat` fast preview (`decode_image_for_display`, lossless WebP carrier), and legacy v1 `Lit`-segment containers (`fixtures/pdf-v1-{0,1,2}.pcf`).
- **Language-native inner payloads**: `Dflt`/`Zlib`/`Gzip`/`PdfStream` and `PngIdat` full `decode_bytes` (`preflate` `"PF"` corrections in Pion vs `preflate-rs` in Rust), `Jpg`/`PdfDct` (`lepton` `"LP\x01"` in Pion vs `lepton_jpeg` in Rust), and legacy `PdfFlate` (kind 9, verified by in-language tests) roundtrip losslessly within each language and are not cross-decoded between implementations.

## Benchmarks (Rust vs Pion)

Measured on Linux x86_64 (`--release`, `taskset -c 0`, 2 warmup + 15 interleaved rounds / 11 rounds for >500 ms workloads, mandatory SHA-256 verification). See [`benchmarks/RESULTS.md`](benchmarks/RESULTS.md) for the full report and interop matrix.

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

Run locally with:

```sh
bash benchmarks/run.sh --render-results
```

## Status

This project is early-stage. The on-disk PCF2 format should be treated as unstable until a 1.0 release.

## License

Licensed under the Apache License, Version 2.0.
