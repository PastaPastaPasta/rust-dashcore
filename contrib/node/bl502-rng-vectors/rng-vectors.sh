#!/usr/bin/env bash
# Builds rng_vectors.cpp against Dash Core v24.0.0-rc.5's own RNGState, SHA-256, SHA-512 and
# memory_cleanse, runs it, and writes the JSON that dashd-random's
# `mix_extract_and_events_match_cores_vectors` test replays (BL-502 test 3).
#
# The sources come from the tag in a Dash Core git repository (not a working tree), so the
# vectors depend only on the tag. The RNGState class is cut verbatim out of src/random.cpp; the
# one edit is `class RNGState {` → `struct RNGState {`, which makes its members readable.
# SHA-256 is built with DISABLE_OPTIMIZED_SHA256 (Core's portable transform).
#
# Usage: contrib/node/bl502-rng-vectors/rng-vectors.sh <dash git repo> <out.json>
set -euo pipefail
repo="$1"; out="$2"
here="$(cd "$(dirname "$0")" && pwd)"
tag=v24.0.0-rc.5
work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
for f in crypto/sha256.cpp crypto/sha256.h crypto/sha512.cpp crypto/sha512.h crypto/common.h \
        compat/endian.h compat/byteswap.h support/cleanse.cpp support/cleanse.h random.cpp; do
    mkdir -p "$work/src/$(dirname "$f")"
    git -C "$repo" show "$tag:src/$f" > "$work/src/$f"
done
awk '/^class RNGState \{$/ { on = 1; print "struct RNGState {"; next }
     on { print } on && /^\};$/ { exit }' "$work/src/random.cpp" > "$work/rngstate.inc"
grep -q 'bool MixExtract(' "$work/rngstate.inc" && grep -q 'void SeedEvents(' "$work/rngstate.inc" &&
    tail -n1 "$work/rngstate.inc" | grep -qx '};' ||
    { echo "RNGState not found in $tag:src/random.cpp" >&2; exit 1; }
${CXX:-g++} -std=c++20 -O1 -DDISABLE_OPTIMIZED_SHA256 -I"$work/src" -I"$work" \
    "$here/rng_vectors.cpp" "$work/src/crypto/sha256.cpp" "$work/src/crypto/sha512.cpp" \
    "$work/src/support/cleanse.cpp" -o "$work/rng_vectors"
"$work/rng_vectors" > "$out"
echo "$(git -C "$repo" rev-parse "$tag^{commit}") ($tag)"
