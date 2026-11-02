#!/usr/bin/env bash
# Regression test for the generator's provenance checks (BS5 review r1, GPT
# major 2, and r2's major). Each drift must be refused; the genuine dumps must
# pass. Checks 1 and 6-7 run on a copy of the pin's anchored build tree, so the
# real one is never changed; the last check confirms it still equals its anchor.
#   1. GPT's r1 experiment: a consensus header of the build tree changed
#      (llmq/params.h max_store_depth + 1). build.sh refuses to reuse it.
#   2. A dump binary that is not the MANIFEST's (one byte appended; also the
#      reviewer's drifted binary if it is still on disk). run.py refuses it.
#   3. A MANIFEST naming another commit than pins.txt. run.py refuses it.
#   4. A MANIFEST for another chainparams_dump.cpp. run.py refuses it.
#   5. A dump from a reused build tree (BS5_REUSE=1). run.py refuses it unless
#      given --allow-reused-build.
#   6. Reusing the intact tree: build.sh accepts it (the check is not vacuous).
#   7. GPT's r2 experiment: chainparams.o rebuilt with regtest
#      nCreditPoolPeriodBlocks = 101 and swapped into libbitcoin_common.a, the
#      source then restored. build.sh refuses to reuse the tree.
#
# usage: selftest.sh <dumps-dir>     (build.sh's output dir for both pins, from
#                                     a fresh build)
#   BS5_BUILD  build root, as for build.sh (default /work/scratch/bs5/core-build)
#   BS5_COPY   where the tree copy goes (default /work/scratch/bs5)
set -euo pipefail

dumps=$(cd "${1:?usage: selftest.sh <dumps-dir>}" && pwd)
here=$(cd "$(dirname "$0")" && pwd)
root=${BS5_BUILD:-/work/scratch/bs5/core-build}
pin=v24.0
commit=$(awk -v p="$pin" '!/^#/ && $1 == p { print $2 }' "$here/../../../dash-oracle-sys/pins.txt")
[ -f "$root/$commit/.bs5-anchor" ] || { echo "FAIL: no anchored $pin build in $root; run build.sh $pin"; exit 1; }
tmp=$(mktemp -d "${TMPDIR:-/dev/shm}/bs5-selftest.XXXXXX")
copy=$(mktemp -d "${BS5_COPY:-/work/scratch/bs5}/selftest-tree.XXXXXX")
trap 'rm -rf "$tmp" "$copy"' EXIT
cp -a "$root/$commit" "$copy/"
tree=$copy/$commit

verify() { python3 -I "$here/run.py" --verify-dump "$pin" --dumps "$1" "${@:2}" > "$tmp/out" 2>&1; }
reuse() { BS5_BUILD=$1 BS5_REUSE=1 "$here/build.sh" "$pin" "$2" > "$tmp/out" 2>&1; }
refused() {  # refused <what> <grep pattern>
  if grep -q "$2" "$tmp/out"; then echo "ok: $1 refused"; else
    echo "FAIL: $1 not refused as expected:"; cat "$tmp/out"; exit 1
  fi
}

verify "$dumps" || { echo "FAIL: the genuine $pin dump is refused:"; cat "$tmp/out"; exit 1; }
echo "ok: genuine $pin dump accepted"

# 1. Drift in a tracked file of the build tree.
header=$tree/src/llmq/params.h
cp -p "$header" "$tmp/params.h.orig"
sed -i 's/return max_cycles(keepOldKeys) \* dkgInterval;/return max_cycles(keepOldKeys) * dkgInterval + 1;/' "$header"
cmp -s "$header" "$tmp/params.h.orig" && { echo "FAIL: drift not applied"; exit 1; }
reuse "$copy" "$tmp/drifted" && { echo "FAIL: build.sh reused a drifted tree"; exit 1; }
refused "reuse of a tree with a drifted header" "src/llmq/params.h"
cp -p "$tmp/params.h.orig" "$header"

# 2. A binary that is not the MANIFEST's.
mkdir "$tmp/d2"
cp "$dumps/MANIFEST-$pin" "$dumps/chainparams_dump_$pin" "$tmp/d2/"
printf 'x' >> "$tmp/d2/chainparams_dump_$pin"
verify "$tmp/d2" && { echo "FAIL: altered binary accepted"; exit 1; }
refused "altered binary" "binary"
gpt=/work/scratch/bs5-gpt/dump-v24-drifted
if [ -x "$gpt" ]; then
  cp "$gpt" "$tmp/d2/chainparams_dump_$pin"
  verify "$tmp/d2" && { echo "FAIL: the reviewer's drifted binary accepted"; exit 1; }
  refused "the reviewer's drifted binary" "binary"
fi

# 3. A MANIFEST for another commit.
mkdir "$tmp/d3"
cp "$dumps/chainparams_dump_$pin" "$tmp/d3/"
sed "s/^commit .*/commit $(printf '%040d' 0)/" "$dumps/MANIFEST-$pin" > "$tmp/d3/MANIFEST-$pin"
verify "$tmp/d3" && { echo "FAIL: foreign commit accepted"; exit 1; }
refused "MANIFEST of another commit" "commit in pins.txt"

# 4. A MANIFEST for another dump source.
mkdir "$tmp/d4"
cp "$dumps/chainparams_dump_$pin" "$tmp/d4/"
sed "s/^source [0-9a-f]*/source $(printf '%064d' 0)/" "$dumps/MANIFEST-$pin" > "$tmp/d4/MANIFEST-$pin"
verify "$tmp/d4" && { echo "FAIL: foreign dump source accepted"; exit 1; }
refused "MANIFEST of another dump source" "dump source"

# 5 and 6. Reusing the intact tree works, but run.py takes its dump only when told to.
reuse "$copy" "$tmp/reused" || { echo "FAIL: build.sh refused the intact tree:"; cat "$tmp/out"; exit 1; }
echo "ok: reuse of the intact anchored tree accepted"
verify "$tmp/reused" && { echo "FAIL: a reused build's dump accepted by default"; exit 1; }
refused "a reused build's dump without --allow-reused-build" "fresh build"
verify "$tmp/reused" --allow-reused-build || { echo "FAIL:"; cat "$tmp/out"; exit 1; }
echo "ok: a reused build's dump accepted with --allow-reused-build"

# 7. An archive rebuilt from changed source, the source then restored.
src=$tree/src
cp -p "$src/chainparams.cpp" "$tmp/chainparams.cpp.orig"
cp -p "$src/libbitcoin_common.a" "$tmp/common.a.orig"
sed -i 's/nCreditPoolPeriodBlocks = 100;/nCreditPoolPeriodBlocks = 101;/' "$src/chainparams.cpp"
cmp -s "$src/chainparams.cpp" "$tmp/chainparams.cpp.orig" && { echo "FAIL: source change not applied"; exit 1; }
make -C "$src" -j"${JOBS:-12}" libbitcoin_common_a-chainparams.o > "$tmp/make.log" 2>&1 ||
  { echo "FAIL: could not rebuild chainparams.o:"; tail "$tmp/make.log"; exit 1; }
ar r "$src/libbitcoin_common.a" "$src/libbitcoin_common_a-chainparams.o"
cp -p "$tmp/chainparams.cpp.orig" "$src/chainparams.cpp"
cmp -s "$src/libbitcoin_common.a" "$tmp/common.a.orig" && { echo "FAIL: archive not changed"; exit 1; }
reuse "$copy" "$tmp/swapped" && { echo "FAIL: build.sh reused a tree with a swapped archive"; exit 1; }
grep -q "differs from the anchor" "$tmp/out" || { echo "FAIL: not the anchor check:"; cat "$tmp/out"; exit 1; }
refused "reuse of a tree whose archive was rebuilt from restored source" "src/libbitcoin_common.a"

# The real tree still equals its anchor.
reuse "$root" "$tmp/real" || { echo "FAIL: the real $pin tree no longer equals its anchor:"; cat "$tmp/out"; exit 1; }
echo "ok: the real $pin build tree still equals its anchor"

echo "bs5-chainparams selftest: ok"
