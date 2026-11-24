# Rust vs Pion Benchmark & Interoperability Results

Generated directly from `interop.json` and `bench.json` by `benchmarks/render_results.py`.

## Environment

- **OS / Platform**: `Linux-6.18.64-l4h-x86_64-with-glibc2.41`
- **CPU**: 8× `x86_64` (`l4h paravirtual vCPU`)
- **Host Load Average (1m / 5m / 15m at start)**: `21.69, 17.51, 11.48`
- **CPU Pinning**: `taskset -c 0`
- **Rust**: `rustc 1.96.0 (ac68faa20 2026-05-25)` / `cargo 1.96.0 (30a34c682 2026-05-25)` (`--release --locked`, `opt-level = 3`, `lto = "thin"`)
- **Pion**: `pionc 0.1.0 (amd64_sysv, self-hosted)` (`--release`)
- **Python**: `Python 3.13.5`
- **Command**: `bash benchmarks/run.sh --warmup 2 --rounds 15 --slow-rounds 11 --render-results`
- **Methodology**: Strictly interleaved alternating rounds (`R/P` on even rounds, `P/R` on odd rounds), 2 warmup rounds + 15 measured rounds (11 rounds for >500 ms workloads). Timing measures in-process codec execution (`Instant::now()` in Rust, `clock_monotonic_us()` in Pion), excluding process startup and file I/O. Every workload enforces SHA-256 verification (including decoded RGBA pixel SHA-256 for `display`).
- **Throughput & Peak RSS Notes**: Throughput (`MiB/s`) is normalized to the source/original input byte size (`input_bytes / (1024^2) / median_s`) for encode, decode, and display alike. Rust Peak RSS is read from `/proc/self/status` (`VmHWM`). Pion Peak RSS is reported as `N/A` because Pion's `fs_read_bytes` reads 0 bytes from 0-size `/proc` pseudo-files, and Linux `wait4` `ru_maxrss` is floored by the parent Python harness RSS.

## 1. Cross-Language PCF2 Interoperability

### 1.1 Bidirectional PCF2 Wire Interop (`Rust -> Pion` and `Pion -> Rust`)

Every case below verified exact `SHA-256(decode(encode(x))) == SHA-256(x)` in both directions, exact expected `SegmentKind` tags, and byte-identical PCF2 wire output between Rust and Pion. All `bzip2` cases additionally verified **0 penalty bytes** in both encoders.

| Case | Original (B) | Rust PCF2 (B) | Pion PCF2 (B) | Segment Kinds | Wire Exact | Rust -> Pion | Pion -> Rust |
| --- | ---: | ---: | ---: | --- | :---: | :---: | :---: |
| `lit` | 992 | 1,017 | 1,017 | `[0]` | YES | PASS | PASS |
| `base64_case0_uniform` | 127 | 150 | 150 | `[7]` | YES | PASS | PASS |
| `base64_case1_last_short` | 125 | 151 | 151 | `[7]` | YES | PASS | PASS |
| `base64_case2_irregular` | 127 | 154 | 154 | `[7]` | YES | PASS | PASS |
| `base64_nested_bzip2` | 157 | 1,892 | 1,892 | `[7]` | YES | PASS | PASS |
| `bzip2_L1_single` | 98 | 3,614 | 3,614 | `[8]` | YES | PASS | PASS |
| `bzip2_L6_single` | 98 | 3,614 | 3,614 | `[8]` | YES | PASS | PASS |
| `bzip2_L9_single` | 98 | 3,614 | 3,614 | `[8]` | YES | PASS | PASS |
| `bzip2_L1_multiblock` | 1,786 | 130,033 | 130,033 | `[8]` | YES | PASS | PASS |
| `unityfs_v1_10_0_l12` | 4,115 | 49,334 | 49,334 | `[0, 11, 0, 11, 11, 11]` | YES | PASS | PASS |
| `unityfs_v1_9_1_l12` | 4,115 | 49,334 | 49,334 | `[0, 11, 0, 11, 11, 11]` | YES | PASS | PASS |
| `unityfs_r131_l16` | 4,103 | 49,312 | 49,312 | `[0, 11, 11, 11, 11]` | YES | PASS | PASS |
| `nested_bzip2_segment` | 98 | 3,639 | 3,639 | `[6]` | YES | PASS | PASS |

### 1.2 Byte-Exact LZ4HC Output Match (`Rust == Pion`)

| Compressor | Input (B) | Compressed Output (B) | SHA-256 (`Rust == Pion`) |
| --- | ---: | ---: | --- |
| `v1_10_0_l12` | 262,144 | 18,098 | `5370ac40d93f8e08c9253f7ed77ef3310ffba87f6f8de8f64f9b145212de7838` |
| `v1_9_1_l12` | 262,144 | 18,098 | `5370ac40d93f8e08c9253f7ed77ef3310ffba87f6f8de8f64f9b145212de7838` |
| `r131_l16` | 262,144 | 18,136 | `6c6a7c1c67ef6cc995fa72faad5bb80911b52f7de2b0e1e180d490b24952a88e` |

### 1.3 PNG + WebP Lossless (`enable_png_webp=true`)

Both encoders emit `[Lit, PngIdat(webp), Lit]` (`[0, 4, 0]`) and pass same-language byte-exact `decode_bytes` roundtrip (including RGB preservation under `alpha == 0`). Full `PngIdat` `decode_bytes` embeds language-native `preflate` corrections, while `decode_image_for_display` interoperates bidirectionally (`Rust->Rust == Rust->Pion` and `Pion->Pion == Pion->Rust` byte-for-byte, with decoded RGBA/RGB pixel SHA-256 verified against source pixels).

| Case | Original PNG (B) | Rust PCF2 (B) | Pion PCF2 (B) | R->P Display WebP (B) | P->R Display WebP (B) | Pixels SHA-256 |
| --- | ---: | ---: | ---: | ---: | ---: | :---: |
| `png_rgb_64x64` | 526 | 298 | 536 | 50 | 332 | PASS |
| `png_rgba_alpha0_64x64` | 2,061 | 448 | 1,840 | 226 | 1,634 | PASS |
| `png_rgba_iccp_64x64` | 2,108 | 495 | 1,887 | 384 | 1,792 | PASS |

### 1.4 Shared `fixtures/` Roundtrip & Legacy v1 PCF2 Containers

All shared fixtures passed exact SHA-256 roundtrip in both Rust and Pion with required non-literal segment kinds verified. Note that `fixtures/pdf-v1-{0,1,2}.pcf` are legacy v1 containers with `[0, 0, 0]` (`Lit`) segments and decode identically to `fixtures/pdf-v1-{0,1,2}.pdf` in both languages.

| Fixture | Original (B) | Rust PCF2 (B) | Pion PCF2 (B) | Transformed Segs (R / P) | Rust | Pion |
| --- | ---: | ---: | ---: | :---: | :---: | :---: |
| `minimal.jpg` | 222 | 236 | 178 | 1 / 1 | PASS | PASS |
| `minimal.png` | 68 | 143 | 129 | 1 / 1 | PASS | PASS |
| `pdf-v1-0.pdf` | 140 | 200 | 186 | 1 / 1 | PASS | PASS |
| `pdf-v1-1.pdf` | 213 | 275 | 261 | 1 / 1 | PASS | PASS |
| `pdf-v1-2.pdf` | 140 | 200 | 186 | 1 / 1 | PASS | PASS |
| `qpdf/filter-abbreviation.pdf` | 743 | 1,055 | 994 | 3 / 3 | PASS | PASS |
| `qpdf/jpeg-qstream.pdf` | 1,156 | 1,230 | 1,196 | 1 / 2 | PASS | PASS |
| `qpdf/image-streams-small.pdf` | 21,748 | 22,048 | 21,101 | 10 / 12 | PASS | PASS |
| `qpdf/image-streams.pdf` | 2,970,696 | 1,430,380 | 1,420,188 | 5 / 6 | PASS | PASS |

- Legacy v1 containers (`pdf-v1-0.pcf`, `pdf-v1-1.pcf`, `pdf-v1-2.pcf`, segment kinds `[0, 0, 0]`): **PASS** in both Rust and Pion.

## 2. Head-to-Head Rust (`--release`) vs Pion (`--release`) Benchmarks

> **Host-Contention Caveat:** These timings were collected on a shared, loaded VM (1m load average `21.69` on `8` vCPU). Absolute latencies and Pion/Rust ratios are indicative and can vary noticeably across runs under host contention; run `bash benchmarks/run.sh --render-results` (optionally with `--pin-cpu none`) on an idle host for authoritative measurements.

Ratio is `Pion median / Rust median` (`< 1.00x` means Pion is faster; `> 1.00x` means Rust is faster). Output columns report whole PCF2 container size for `encode`, restored original size for `decode`, raw compressed block size for `lz4hc`, and extracted WebP size for `display`.

| Workload | Op | Input (B) | Rust Med / Min / P95 (ms) | Rust (MiB/s) | Rust RSS (MiB) | Pion Med / Min / P95 (ms) | Pion (MiB/s) | Pion RSS (MiB) | Ratio (P/R) | Output R / P (B) | Notes |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| `lz4hc/v1_10_0_l12` | compress | 4,194,304 | 711.36 / 575.91 / 897.14 | 5.62 | 7.69 | 1647.36 / 1540.36 / 1979.25 | 2.43 | N/A | 2.32x | 216,437 / 216,437 | byte-exact match |
| `lz4hc/v1_9_1_l12` | compress | 4,194,304 | 550.68 / 426.33 / 717.38 | 7.26 | 7.63 | 1229.27 / 1057.31 / 1510.16 | 3.25 | N/A | 2.23x | 216,437 / 216,437 | byte-exact match |
| `lz4hc/r131_l16` | compress | 4,194,304 | 159.03 / 111.95 / 239.00 | 25.15 | 7.44 | 358.94 / 307.81 / 657.76 | 11.14 | N/A | 2.26x | 217,214 / 217,214 | byte-exact match |
| `unityfs/v6_4m` | encode | 225,523 | 625.25 / 525.52 / 846.56 | 0.34 | 11.64 | 1657.50 / 1360.60 / 1938.28 | 0.13 | N/A | 2.65x | 4,195,017 / 4,195,017 |  |
| `unityfs/v6_4m` | decode | 225,523 | 546.01 / 497.21 / 916.49 | 0.39 | 8.00 | 1503.36 / 1279.85 / 1838.90 | 0.14 | N/A | 2.75x | 225,523 / 225,523 |  |
| `zlib/L1_2m` | encode | 75,795 | 77.33 / 51.72 / 109.60 | 0.93 | 9.98 | 158.34 / 116.59 / 352.80 | 0.46 | N/A | 2.05x | 2,097,270 / 2,097,230 |  |
| `zlib/L1_2m` | decode | 75,795 | 32.60 / 29.34 / 41.00 | 2.22 | 14.42 | 34.66 / 30.76 / 38.77 | 2.09 | N/A | 1.06x | 75,795 / 75,795 |  |
| `zlib/L6_2m` | encode | 65,762 | 102.57 / 82.53 / 145.08 | 0.61 | 10.17 | 241.22 / 198.03 / 345.59 | 0.26 | N/A | 2.35x | 2,097,395 / 2,097,220 |  |
| `zlib/L6_2m` | decode | 65,762 | 39.83 / 34.79 / 102.82 | 1.57 | 14.18 | 76.53 / 66.40 / 133.94 | 0.82 | N/A | 1.92x | 65,762 / 65,762 |  |
| `zlib/L9_2m` | encode | 64,502 | 109.73 / 88.43 / 211.91 | 0.56 | 9.91 | 277.83 / 220.96 / 447.26 | 0.22 | N/A | 2.53x | 2,098,205 / 2,097,220 |  |
| `zlib/L9_2m` | decode | 64,502 | 45.02 / 34.00 / 96.07 | 1.37 | 14.44 | 114.44 / 97.54 / 143.42 | 0.54 | N/A | 2.54x | 64,502 / 64,502 |  |
| `gzip/L6_2m` | encode | 65,774 | 102.66 / 83.23 / 223.23 | 0.61 | 10.26 | 229.28 / 191.38 / 402.18 | 0.27 | N/A | 2.23x | 2,097,408 / 2,097,233 |  |
| `gzip/L6_2m` | decode | 65,774 | 42.12 / 37.25 / 91.05 | 1.49 | 14.42 | 79.89 / 66.97 / 132.58 | 0.79 | N/A | 1.90x | 65,774 / 65,774 |  |
| `bzip2/L6_512k` | encode | 8,066 | 91.57 / 69.89 / 140.18 | 0.08 | 8.77 | 236.52 / 196.17 / 351.46 | 0.03 | N/A | 2.58x | 524,321 / 524,321 |  |
| `bzip2/L6_512k` | decode | 8,066 | 122.31 / 72.66 / 213.65 | 0.06 | 9.20 | 267.34 / 193.97 / 457.71 | 0.03 | N/A | 2.19x | 8,066 / 8,066 |  |
| `bzip2/L9_512k` | encode | 8,066 | 87.47 / 67.23 / 148.10 | 0.09 | 8.82 | 272.19 / 203.46 / 411.10 | 0.03 | N/A | 3.11x | 524,321 / 524,321 |  |
| `bzip2/L9_512k` | decode | 8,066 | 63.93 / 52.99 / 104.06 | 0.12 | 9.13 | 217.03 / 165.31 / 304.37 | 0.04 | N/A | 3.39x | 8,066 / 8,066 |  |
| `png_webp/rgba_512x512` | encode | 344,513 | 343.23 / 258.80 / 527.50 | 0.96 | 23.60 | 353.95 / 314.10 / 577.47 | 0.93 | N/A | 1.03x | 5,948 / 69,204 |  |
| `png_webp/rgba_512x512` | decode | 344,513 | 51.29 / 41.44 / 173.73 | 6.41 | 8.58 | 138.63 / 129.69 / 210.34 | 2.37 | N/A | 2.70x | 344,513 / 344,513 |  |
| `png_webp/rgba_512x512` | display | 344,513 | 0.028 / 0.024 / 0.169 | 11548.44 | 3.52 | 0.561 / 0.404 / 0.894 | 585.24 | N/A | 20.04x | 4,684 / 68,476 | RGBA pixels + cross-lang display verified |
| `jpeg/minimal_222b` | encode | 222 | 0.276 / 0.184 / 0.477 | 0.77 | 5.35 | 0.287 / 0.213 / 0.514 | 0.74 | N/A | 1.04x | 236 / 178 | segs R=1 P=1 |
| `jpeg/minimal_222b` | decode | 222 | 0.136 / 0.110 / 0.174 | 1.55 | 5.02 | 0.042 / 0.036 / 0.060 | 5.05 | N/A | 0.31x | 222 / 222 | segs R=1 P=1 |
| `jpeg/pdf_extracted` | encode | 3,650 | 6.51 / 5.70 / 13.62 | 0.53 | 6.27 | 6.27 / 4.89 / 10.22 | 0.56 | N/A | 0.96x | 951 / 268 | segs R=1 P=1 |
| `jpeg/pdf_extracted` | decode | 3,650 | 4.54 / 3.98 / 6.27 | 0.77 | 6.03 | 2.94 / 2.12 / 4.07 | 1.18 | N/A | 0.65x | 3,650 / 3,650 | segs R=1 P=1 |
| `pdf/image-streams-small` | encode | 21,748 | 79.31 / 56.78 / 105.67 | 0.26 | 7.03 | 8.32 / 4.63 / 11.37 | 2.49 | N/A | 0.10x | 22,048 / 21,101 | segs R=10 P=12 |
| `pdf/image-streams-small` | decode | 21,748 | 2.69 / 1.69 / 5.54 | 7.70 | 5.54 | 1.32 / 1.13 / 3.02 | 15.73 | N/A | 0.49x | 21,748 / 21,748 | segs R=10 P=12 |
| `pdf/image-streams_2.97m` | encode | 2,970,696 | 75.59 / 62.76 / 384.81 | 37.48 | 13.99 | 157.10 / 134.89 / 225.70 | 18.03 | N/A | 2.08x | 1,430,380 / 1,420,188 | segs R=5 P=6 |
| `pdf/image-streams_2.97m` | decode | 2,970,696 | 47.45 / 37.37 / 70.66 | 59.70 | 11.34 | 79.34 / 68.37 / 103.85 | 35.71 | N/A | 1.67x | 2,970,696 / 2,970,696 | segs R=5 P=6 |
| `base64/mime_zlib` | encode | 90,029 | 96.77 / 84.59 / 185.09 | 0.89 | 10.34 | 217.11 / 178.06 / 363.31 | 0.40 | N/A | 2.24x | 2,097,471 / 2,097,296 |  |
| `base64/mime_zlib` | decode | 90,029 | 60.68 / 49.59 / 103.07 | 1.41 | 20.38 | 95.42 / 73.62 / 141.42 | 0.90 | N/A | 1.57x | 90,029 / 90,029 |  |
