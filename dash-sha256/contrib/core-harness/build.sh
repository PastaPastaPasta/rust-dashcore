#!/usr/bin/env bash
# Builds the Dash Core SHA-256 harness (harness.cpp) from Core's unmodified sources at a commit.
#
# Usage: build.sh <dash-core-git-dir> <commit> <output-binary> [x86_64|aarch64]
#
# Each Core file gets the flags Core's build gives it (src/Makefile.am and configure.ac at
# v24.0.0-rc.3): -msse4.1 for sha256_sse41.cpp, -mavx -mavx2 for sha256_avx2.cpp, -msse4 -msha
# for sha256_x86_shani.cpp, -march=armv8-a+crypto for sha256_arm_shani.cpp, with the matching
# ENABLE_* defines. aarch64 builds use aarch64-linux-gnu-g++; run them under qemu-aarch64.
set -euo pipefail

core=$1 commit=$2 out=$3 arch=${4:-x86_64}
here=$(cd "$(dirname "$0")" && pwd)
# Core's sources go next to the output, not into a shared /tmp.
src=$(mktemp -d "$(dirname "$out")/sha256-harness.XXXXXX")
trap 'rm -rf "$src"' EXIT

git -C "$core" archive "$commit" src/crypto/sha256.cpp src/crypto/sha256.h src/crypto/common.h \
    src/crypto/sha256_sse4.cpp src/crypto/sha256_sse41.cpp src/crypto/sha256_avx2.cpp \
    src/crypto/sha256_x86_shani.cpp src/crypto/sha256_arm_shani.cpp src/compat src/attributes.h |
    tar -x -C "$src"

flags=(-O2 -std=c++20 -I"$src/src")
case $arch in
x86_64)
    cxx=${CXX:-g++}
    defs=(-DENABLE_SSE41 -DENABLE_AVX2 -DENABLE_X86_SHANI)
    objs=(
        "crypto/sha256_sse4.cpp"
        "crypto/sha256_sse41.cpp -msse4.1"
        "crypto/sha256_avx2.cpp -mavx -mavx2"
        "crypto/sha256_x86_shani.cpp -msse4 -msha"
    )
    ;;
aarch64)
    cxx=${CXX:-aarch64-linux-gnu-g++}
    defs=(-DENABLE_ARM_SHANI)
    objs=("crypto/sha256_arm_shani.cpp -march=armv8-a+crypto")
    flags+=(-static)
    ;;
*)
    echo "unknown arch $arch" >&2
    exit 1
    ;;
esac

files=("$here/harness.cpp" "$src/src/crypto/sha256.cpp")
for o in "${objs[@]}"; do
    read -r file extra <<< "$o"
    # shellcheck disable=SC2086 # extra holds the per-file flags
    "$cxx" "${flags[@]}" "${defs[@]}" $extra -c "$src/src/$file" -o "$src/$(basename "$file").o"
    files+=("$src/$(basename "$file").o")
done
"$cxx" "${flags[@]}" "${defs[@]}" "${files[@]}" -o "$out"
echo "built $out from $commit ($arch)"
