# precomp2

`precomp2` is a Rust library for reversible preprocessing of compressed and container data. It turns recognizable streams into PCF2 segments that are easier for an outer compressor to handle, then restores the original bytes exactly.

The project is inspired by [precomp-cpp](https://github.com/schnaader/precomp-cpp) and [preflate-rs](https://github.com/microsoft/preflate-rs).

## Features

- Lossless encode/decode APIs for byte slices and async decode input.
- PCF2 segment container with literal, nested, DEFLATE, zlib, gzip, PNG IDAT, JPEG, Base64, bzip2, PDF stream, and UnityFS segment kinds.
- Recursive preprocessing with depth and output budget limits.
- Configurable PNG and PDF preprocessing through `EncodeConfig`.

## Usage

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

## Testing

`cargo test` needs a C compiler: `tests/lz4-reference` builds the vendored liblz4 releases as the reference for the LZ4HC ports. Real-archive acceptance and coverage-guided fuzzing are opt-in:

```sh
PRECOMP2_UNITYFS_FILES=list.txt cargo test --release --test unityfs_roundtrip -- --ignored --nocapture
cargo +nightly fuzz run lz4_reference   # see fuzz/README.md
```

## Status

This crate is early-stage. The on-disk PCF2 format should be treated as unstable until a 1.0 release.

## License

Licensed under the Apache License, Version 2.0.
