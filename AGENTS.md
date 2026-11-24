# Working on precomp2 (for agents)

These rules apply to the whole repository. Keep them true whenever a change alters paths, commands, or invariants.

## Repository layout

- `rust/`: reference Rust implementation of `precomp2` (Cargo workspace member of root `Cargo.toml`), including `tests/lz4-reference`, `examples/`, `fuzz/`, and `tools/lz4-matrix/`.
- `pion/`: pure-Pion workspace implementing `precomp2` and its minimal in-repo codec packages (`packages/bzip2`, `packages/preflate`, `packages/lepton`, `packages/precomp2`). Reuse official `AFK-surf/pion-packages` libraries (`flate`, `base64`, `webp`) instead of duplicating generic codecs.
- `fixtures/`: binary test fixtures shared by both Rust and Pion test suites.
- `benchmarks/`: head-to-head Rust (`--release`) vs Pion (`--release`) benchmark suite with interleaved rounds and mandatory output SHA-256 verification.

## Branching, pull requests, and reviews

- Develop the Pion port on `feat/reimpl-pion`. Parallel topic branches (`feat/reimpl-pion-*`) run CI on push and squash-land onto `feat/reimpl-pion` without opening PRs against `main`. Never push to or modify `main`.
- When opening PRs in external upstream repositories (such as `AFK-surf/pion-packages`), open one PR per request from the target base branch, squash each task into one commit before the first PR, and end the PR description with the models used and test frameworks run.
- Each topic branch requires independent multi-seat review approval (at least 3 of 4 seats, with all reasonable objections resolved) before squash-landing onto `feat/reimpl-pion`.
- Set both Git author and committer to `DarkSky <darksky2048@gmail.com>` for all commits, including sub-chats and squashes. Do not add AI `Co-Authored-By` trailers. Preserve original authors on external commits.

## Commit, comment, and PR messages

- Write commit messages, code comments, documentation, error messages, and PR titles/descriptions in clear, concise, direct English (active voice, no filler).
- Use `{type}({scope}): {description}` for commit subjects (for example `feat(pion): port lz4hc compressors and unityfs codec`).
- Keep code, comments, workflows, `AGENTS.md`, and commit messages free of internal process-journal jargon.

## Engineering and testing principles

- **Global optimum over band-aid patches:** prefer clean structure and bold deletion over minimal-diff workarounds, defensive clutter, or one-caller indirection. Split source files when a single non-test file grows past ~500 lines and carries distinct responsibilities.
- **Minimal necessary internal codecs:** `pion/packages/{bzip2,preflate,lepton}` implement only what `precomp2` requires for lossless reversible preprocessing — do not bloat them with unused features.
- **Zero C/FFI and zero `World` in Pion library code:** all packages under `pion/packages/` must be 100% pure Pion, `World`-free, and Wasm32-compatible.
- **Deterministic, scope-accurate tests:** keep tests deterministic with fixed seeds and no sleeps or retries. Group related cases into table-driven loops. Name every test function and file after everything it asserts.
- **Cross-language verification:** every PCF2 segment kind implemented in Pion must pass lossless roundtrip tests, shared `fixtures/` tests, and byte-exact `lz4` reference checks where applicable.
- **Zero worktree pollution:** never commit build caches, `target/`, temporary binaries, or benchmark output artifacts.

## Keeping rules true

- Update `AGENTS.md` in the same commit whenever a change makes a documented path, command, or rule untrue.
- Pair any new directory `AGENTS.md` with a one-line `CLAUDE.md` containing `@AGENTS.md`.
