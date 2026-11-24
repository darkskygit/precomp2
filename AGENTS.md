# Working on precomp2 (for agents)

These rules apply to the whole repository. Keep them true whenever a change alters paths, commands, or invariants.

## Repository layout

- `rust/`: reference Rust implementation of `precomp2` (Cargo workspace member of root `Cargo.toml`), including `tests/lz4-reference`, `examples/`, `fuzz/`, and `tools/lz4-matrix/`.
- `pion/`: pure-Pion workspace implementing `packages/precomp2`. Reuse official `AFK-surf/pion-packages` libraries (`base64`, `bzip2`, `flate`, `lepton`, `preflate`, `webp`) instead of duplicating generic codecs in-repo.
- `fixtures/`: binary test fixtures shared by both Rust and Pion test suites.
- `benchmarks/`: head-to-head Rust (`--release`) vs Pion (`--release`) benchmark suite with interleaved rounds and mandatory output SHA-256 verification.

## Commits and style

- Write commit messages, code comments, documentation, error messages, and PR titles/descriptions in clear, concise, direct English (active voice, no filler).
- Use `{type}({scope}): {description}` for commit subjects (for example `feat(pion): port lz4hc compressors and unityfs codec`).
- Keep code, comments, workflows, `AGENTS.md`, and commit messages free of internal process-journal jargon. Do not add AI `Co-Authored-By` trailers.

## Engineering and testing principles

- **Global optimum over band-aid patches:** prefer clean structure and bold deletion over minimal-diff workarounds, defensive clutter, or one-caller indirection. Split source files when a single non-test file grows past ~500 lines and carries distinct responsibilities.
- **Zero C/FFI and zero `World` in Pion library code:** all packages under `pion/packages/` must be 100% pure Pion, `World`-free, and Wasm32-compatible.
- **Deterministic, scope-accurate tests:** keep tests deterministic with fixed seeds and no sleeps or retries. Group related cases into table-driven loops. Name every test function and file after everything it asserts.
- **Cross-language verification:** every PCF2 segment kind implemented in Pion must pass lossless roundtrip tests, shared `fixtures/` tests, and byte-exact `lz4` reference checks where applicable.
- **Zero worktree pollution:** never commit build caches, `target/`, temporary binaries, or benchmark output artifacts.

## Keeping rules true

- Update `AGENTS.md` in the same commit whenever a change makes a documented path, command, or rule untrue.
- Pair any new directory `AGENTS.md` with a one-line `CLAUDE.md` containing `@AGENTS.md`.
