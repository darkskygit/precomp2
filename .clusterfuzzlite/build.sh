#!/bin/bash -eu

# The base image's toolchain may lag behind this repository; use the nightly
# pinned in .github/workflows/ci.yml. rustup downloads it on first use.
export RUSTUP_TOOLCHAIN=nightly-2026-07-07

# The cc crate would compile the C dependencies (the liblz4 reference builds,
# libwebp) with the image's sanitizer CFLAGS from another LLVM than rustc's.
# They are trusted code; build them uninstrumented.
unset CFLAGS CXXFLAGS

cd "$SRC/precomp2/fuzz"

case "${SANITIZER:-address}" in
address)
  cargo fuzz build -O --debug-assertions --sanitizer address
  ;;
coverage)
  # cargo-fuzz has no coverage sanitizer; instrument via rustflags instead.
  export RUSTFLAGS="${RUSTFLAGS:-} -Cinstrument-coverage"
  cargo fuzz build -O --sanitizer none
  ;;
*)
  echo "unsupported sanitizer: ${SANITIZER}" >&2
  exit 1
  ;;
esac

for target_src in fuzz_targets/*.rs; do
  target_name="$(basename "$target_src" .rs)"
  cp "target/x86_64-unknown-linux-gnu/release/$target_name" "$OUT/"

  # Every fuzzing run replays the committed seeds and crash reproducers first.
  if [ -d "seed_corpus/$target_name" ] && [ -n "$(ls -A "seed_corpus/$target_name")" ]; then
    (cd "seed_corpus/$target_name" && zip -j -q -r "$OUT/${target_name}_seed_corpus.zip" .)
  fi
done
