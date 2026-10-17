# precomp2 fuzzing

Fuzz targets driven by [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) (libFuzzer). Continuous fuzzing runs through
[ClusterFuzzLite](https://google.github.io/clusterfuzzlite/) in GitHub Actions (`.github/workflows/cflite_*.yml`); corpora, crashes and
coverage reports are stored as workflow artifacts.

## Targets

| Target              | What it covers                                                                                              |
| ------------------- | ----------------------------------------------------------------------------------------------------------- |
| `lz4_reference`     | differential: each pure-Rust LZ4 compressor must match the C liblz4 build of its release byte for byte, at full, exact and one-byte-short output capacity |
| `unityfs_roundtrip` | UnityFS archives built from the input, then byte-flipped behind the magic, must encode and decode back to themselves |

`lz4_reference` reads its first byte as `compressor + 3 * capacity mode` (compressors in `Compressor::ALL` order: 1.10.0, 1.9.1, r131;
modes: full, exact, one byte short) and expands the rest into the compressor input, so short inputs reach the 64 KiB window and long
runs. Adding a compressor shifts this mapping; update the seed control bytes with it.

## Running locally

```bash
cargo +nightly fuzz run <target> corpus/<target> seed_corpus/<target>
cargo +nightly fuzz run <target> corpus/<target> seed_corpus/<target> -- -runs=1000   # bounded smoke run
```

## Seed corpus and crash regression loop

- `corpus/<target>/` is the fuzzer's working corpus. It is ignored by git; the CI corpus lives in ClusterFuzzLite artifacts and is
  pruned daily by `cflite_cron`.
- `seed_corpus/<target>/` is committed. Every ClusterFuzzLite build packages it as `<target>_seed_corpus.zip`, so every run replays it
  first. Seed names state the path they cover.

When a crash is found (locally or from a CI artifact):

1. Reproduce: `cargo +nightly fuzz run <target> <crash-file>`
2. Minimize: `cargo +nightly fuzz tmin <target> <crash-file>`
3. Fix the bug and commit the minimized input to `seed_corpus/<target>/` with the fix.

Commit only handcrafted seeds and minimized reproducers to `seed_corpus/`, never the working corpus.
