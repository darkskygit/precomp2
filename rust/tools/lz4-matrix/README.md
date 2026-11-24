# lz4-matrix

Finds the liblz4 release and compression level that produced the UnityFS LZ4 blocks precomp2 cannot reproduce, so a new engine version
gets exactly one new `codec::lz4::Compressor` instead of speculative ports.

```bash
tools/lz4-matrix/run.sh <file-list> [release-tag...]
```

`<file-list>` names one UnityFS file per line. `dump-misses` writes up to `LZ4_MATRIX_BLOCKS` (default 200) unreproduced blocks as
`<n>.raw` / `<n>.lz4` pairs and reports per engine version how many LZ4 regions were not reproduced. Every release tag (default: r131
to v1.10.0) is downloaded once into `work/src/` (`LZ4_MATRIX_WORK` moves `work/`), `matrix.c` is compiled against it, and each level is
tried on every pair:

```
r131      150/150 blocks reproduced; L16=150
v1.7.5    0/150 blocks reproduced;
```

Precomp2 stops trying an archive after a few regions without any match, so for an unsupported engine every block is dumped.

## Adding a compressor for a new release class

1. Pick the release and level the matrix reports; releases with identical output form one class, named after the release tested
   against.
2. Vendor that release under `tests/lz4-reference/vendor/` (unmodified `lib/` subset, `LICENSE`, sha256 in its README) and add it to
   `build.rs` and the `versions!` list.
3. Add a `Compressor` variant with a new id (never reuse one), its `release()` and `level()`, and a `compress` arm: a parser family
   with its level parameters, plus a new `Quirks` constant or parser change if the release behaves differently.
4. Map the engine version in `ENGINE_COMPRESSORS` (`src/codec/unityfs.rs`), update the seed control bytes in `fuzz/seed_corpus/`, and
   rerun `tests/lz4_reference.rs` and the real-archive test.
