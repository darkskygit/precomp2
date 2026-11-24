#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="${PRECOMP2_BENCH_OUT:-$ROOT/benchmarks/out}"
mkdir -p "$OUT_DIR"

RUN_INTEROP=0
RUN_BENCH=0
RENDER_RESULTS=0
ROUNDS=15
SLOW_ROUNDS=11
WARMUP=2
PIN_CPU=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --interop)        RUN_INTEROP=1; shift ;;
    --bench)          RUN_BENCH=1; shift ;;
    --render-results) RENDER_RESULTS=1; shift ;;
    --rounds)         ROUNDS="$2"; shift 2 ;;
    --slow-rounds)    SLOW_ROUNDS="$2"; shift 2 ;;
    --warmup)         WARMUP="$2"; shift 2 ;;
    --pin-cpu)        PIN_CPU="$2"; shift 2 ;;
    -h|--help)
      echo "usage: $0 [--interop] [--bench] [--render-results] [--rounds N] [--slow-rounds N] [--warmup N] [--pin-cpu N]"
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

if [[ $RUN_INTEROP -eq 0 && $RUN_BENCH -eq 0 ]]; then
  RUN_INTEROP=1
  RUN_BENCH=1
fi

RUST_TARGET_DIR="$OUT_DIR/rust-target"
RUST_BIN="$RUST_TARGET_DIR/release/precomp2-bench-rust"
PION_BIN="$OUT_DIR/precomp2-bench-pion"

echo "=== Building Rust CLI (--release --locked) ==="
CARGO_TARGET_DIR="$RUST_TARGET_DIR" cargo build --release --locked --manifest-path "$ROOT/benchmarks/rust/Cargo.toml"

echo "=== Building Pion CLI (--release) ==="
pionc build --release -o "$PION_BIN" "$ROOT/benchmarks/pion"

if [[ $RUN_INTEROP -eq 1 ]]; then
  echo "=== Running Cross-Language Interop & Shared Fixtures Checks ==="
  python3 "$ROOT/benchmarks/interop_check.py" \
    --rust-bin "$RUST_BIN" \
    --pion-bin "$PION_BIN" \
    --fixtures-dir "$ROOT/fixtures" \
    --json-out "$OUT_DIR/interop.json"
fi

if [[ $RUN_BENCH -eq 1 ]]; then
  echo "=== Running Head-to-Head Rust vs Pion Benchmarks ==="
  python3 "$ROOT/benchmarks/bench_compare.py" \
    --rust-bin "$RUST_BIN" \
    --pion-bin "$PION_BIN" \
    --fixtures-dir "$ROOT/fixtures" \
    --warmup "$WARMUP" \
    --rounds "$ROUNDS" \
    --slow-rounds "$SLOW_ROUNDS" \
    --pin-cpu "$PIN_CPU" \
    --json-out "$OUT_DIR/bench.json"
fi

if [[ $RENDER_RESULTS -eq 1 ]]; then
  echo "=== Rendering benchmarks/RESULTS.md ==="
  python3 "$ROOT/benchmarks/render_results.py" \
    --interop-json "$OUT_DIR/interop.json" \
    --bench-json "$OUT_DIR/bench.json" \
    --out "$ROOT/benchmarks/RESULTS.md"
fi
