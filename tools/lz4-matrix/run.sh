#!/usr/bin/env bash
# Finds the liblz4 release and level that produced the UnityFS LZ4 blocks
# precomp2 leaves as stored.
# usage: tools/lz4-matrix/run.sh <file-list> [release-tag...]
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work="${LZ4_MATRIX_WORK:-$here/work}"
list="${1:?usage: run.sh <file-list> [release-tag...]}"
shift
releases=("$@")
if [ ${#releases[@]} -eq 0 ]; then
  releases=(r131 v1.7.3 v1.7.4 v1.7.5 v1.8.0 v1.8.1 v1.8.1.2 v1.8.2 v1.8.3 v1.9.0 v1.9.1 v1.9.2 v1.9.3 v1.9.4 v1.10.0)
fi

rm -rf "$work/blocks"
mkdir -p "$work/blocks" "$work/src" "$work/bin"
cargo run --quiet --release --manifest-path "$here/Cargo.toml" -- "$work/blocks" <"$list"

for tag in "${releases[@]}"; do
  src="$work/src/$tag"
  if [ ! -f "$src/lib/lz4hc.c" ]; then
    mkdir -p "$src"
    curl -fsSL "https://github.com/lz4/lz4/archive/refs/tags/$tag.tar.gz" | tar xz -C "$src" --strip-components=1
  fi
  cc -O2 -w -I"$src/lib" "$here/matrix.c" "$src/lib/lz4hc.c" "$src/lib/lz4.c" -o "$work/bin/$tag"
  "$work/bin/$tag" "$tag" "$work/blocks"
done
