#!/usr/bin/env bash
# Builds muhash_vectors.cpp against a built Dash Core tree whose crypto sources are
# v24.0.0-rc.3's (the release's own, or a shim tree on top of it), runs it, and writes the JSON
# that dash-consensus's `muhash_arithmetic_reproduces_cores_on_boundary_values` test replays.
#
# Usage: contrib/node/muhash-vectors/muhash-vectors.sh <core tree> <out.json>
set -euo pipefail
core="$1"; out="$2"
here="$(cd "$(dirname "$0")" && pwd)"
tag=v24.0.0-rc.3
# The sources as built: the working tree, untracked files included, not only HEAD.
sources=(src/crypto src/hash.h src/serialize.h src/span.h src/streams.h src/uint256.h
    src/support/cleanse.cpp src/support/cleanse.h)
git -C "$core" diff --quiet "$tag" -- "${sources[@]}" &&
    [ -z "$(git -C "$core" status --porcelain -- "${sources[@]}")" ] ||
    { echo "$core: crypto or cleanse sources differ from $tag" >&2; exit 1; }
# Core's crypto library with its SHA-256 variants (dispatched at run time), and memory_cleanse.
libs=()
for lib in crypto/.libs/libbitcoin_crypto_{base,sse41,avx2,x86_shani}.a support/libbitcoin_util_a-cleanse.o; do
    [ -f "$core/src/$lib" ] || { echo "$core/src/$lib: not built" >&2; exit 1; }
    libs+=("$core/src/$lib")
done
work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
${CXX:-g++} -std=c++20 -O1 -DHAVE_CONFIG_H -I"$core/src/config" -I"$core/src" \
    "$here/muhash_vectors.cpp" "${libs[@]}" -o "$work/muhash_vectors"
"$work/muhash_vectors" > "$out"
echo "$(git -C "$core" rev-parse HEAD) ($(git -C "$core" describe --tags HEAD))"
