#!/usr/bin/env bash
# Builds the Dash Core X11 harness (harness.cpp) against Core's X11 code at a commit.
#
# Usage: build.sh <dash-core-git-dir> <commit> <output-binary> [<built-core-src-dir>]
#
# Without the fourth argument, Core's unmodified sources are compiled here with the flags
# Core's build gives them (src/Makefile.am and configure.ac, identical in v23.1.8 and
# v24.0.0-rc.3): the sph C files with -O3 then CFLAGS' -O2 (so -O2) and
# -DSPH_SMALL_FOOTPRINT_CUBEHASH=1 -DSPH_SMALL_FOOTPRINT_JH=1; the C++ files with Core's
# hardening flags; x86_aesni/*.cpp with -msse4.1 -maes and -DENABLE_SSE41 -DENABLE_X86_AESNI;
# ssse3/echo.cpp with -mssse3 -DENABLE_SSSE3. dispatch.cpp gets every ENABLE_* define, as in
# Core. x86_64 only.
#
# With <built-core-src-dir>, a tree Core's own build has compiled (node-bench's
# scripts/build-dashd-bench.sh leaves one in /work/bench/src/dash-<tag>), the harness links that
# build's libbitcoin_crypto_{sph,ssse3,x86_aesni}.a, the objects bench_dash itself runs. The
# headers still come from <commit>; the script checks the tree's COMMIT file matches.
set -euo pipefail

core=$1 commit=$2 out=$3 built=${4:-}
here=$(cd "$(dirname "$0")" && pwd)
cxx=${CXX:-g++} cc=${CC:-gcc}
src=$(mktemp -d "$(dirname "$out")/x11-harness.XXXXXX")
trap 'rm -rf "$src"' EXIT

git -C "$core" archive "$commit" src/crypto/x11 src/hash_x11.h src/uint256.h src/crypto/common.h \
    src/span.h src/compat src/attributes.h |
    tar -x -C "$src"

defs=(-DENABLE_SSE41 -DENABLE_X86_AESNI -DENABLE_SSSE3 -DHAVE_GETCPUID)
hard=(-fstack-protector-all -fcf-protection=full -fstack-clash-protection -fno-extended-identifiers
    -fstack-reuse=none -fPIE -U_FORTIFY_SOURCE -D_FORTIFY_SOURCE=3)
cxxflags=(-O2 -std=c++20 -I"$src/src")
objs=()

if [ -n "$built" ]; then
    full=$(git -C "$core" rev-parse "$commit^{commit}")
    if [ "$(cat "$built/COMMIT")" != "$full" ]; then
        echo "$built holds $(cat "$built/COMMIT"), not $commit ($full)" >&2
        exit 1
    fi
    for lib in sph ssse3 x86_aesni; do
        objs+=("$built/src/crypto/.libs/libbitcoin_crypto_$lib.a")
    done
else
    x11=$src/src/crypto/x11
    for f in blake bmw cubehash groestl jh keccak luffa simd skein; do
        "$cc" -I"$src/src" -DSPH_SMALL_FOOTPRINT_CUBEHASH=1 -DSPH_SMALL_FOOTPRINT_JH=1 -O3 -O2 \
            -c "$x11/$f.c" -o "$src/$f.o"
        objs+=("$src/$f.o")
    done
    for f in aes dispatch echo shavite; do
        "$cxx" "${cxxflags[@]}" "${hard[@]}" -O3 -O2 "${defs[@]}" -c "$x11/$f.cpp" -o "$src/$f.o"
        objs+=("$src/$f.o")
    done
    for f in echo shavite; do
        "$cxx" "${cxxflags[@]}" "${hard[@]}" -msse4.1 -maes -DENABLE_SSE41 -DENABLE_X86_AESNI \
            -c "$x11/x86_aesni/$f.cpp" -o "$src/aesni_$f.o"
        objs+=("$src/aesni_$f.o")
    done
    "$cxx" "${cxxflags[@]}" "${hard[@]}" -mssse3 -DENABLE_SSSE3 -c "$x11/ssse3/echo.cpp" \
        -o "$src/ssse3_echo.o"
    objs+=("$src/ssse3_echo.o")
fi

"$cxx" "${cxxflags[@]}" "${defs[@]}" "$here/harness.cpp" "${objs[@]}" -o "$out"
echo "built $out from $commit${built:+ (objects from $built)}"
