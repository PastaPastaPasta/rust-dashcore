#!/usr/bin/env bash
# Builds the dashd-written blocks directories the ST2 block-file parity test compares against
# (dash-coreimport/tests/blockfiles_parity.rs), from a base chain made by
# `gen-blockfile-chains.py generate`. Prints the ST2_PARITY_DATADIRS value to use.
#
# usage: st2-parity-cases.sh <dashd v24.0.0-rc.3> <base datadir> <out dir> [first port]
#
# Every case is its own dashd run on its own datadir and ports. The node targets v24 alone
# (DEC-82), so the cases of earlier releases are gone.
set -euo pipefail

D24=$1 BASE=$2 OUT=$3 PORT=${4:-34700}
GEN="python3 $(dirname "$0")/gen-blockfile-chains.py"
mkdir -p "$OUT"
BLOCKS=$OUT/blocks.bin
[ -f "$BLOCKS" ] || $GEN export --dashd "$D24" --datadir "$BASE" --port "$PORT" --out "$BLOCKS"
BIG=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['big_block_at'])" "$BASE/st2-manifest.json")

pids=()
start() { # name, then gen-blockfile-chains.py arguments; runs in the background on its own ports
    local name=$1; shift
    PORT=$((PORT + 2))
    if [ ! -f "$OUT/$name/st2-manifest.json" ]; then
        rm -rf "${OUT:?}/$name"
        $GEN "$@" --datadir "$OUT/$name" --port "$PORT" > "$OUT/$name.log" 2>&1 &
        pids+=($!)
    fi
}
wait_all() {
    for p in "${pids[@]}"; do wait "$p" || true; done
    pids=()
}

start r24-default replay --dashd "$D24" --blocks "$BLOCKS" --restart-before 10000 --restart-before 25000
start r24-tinyblk replay --dashd "$D24" --blocks "$BLOCKS" --flag=-tinyblk --restart-before 20000
start r24-fastprune replay --dashd "$D24" --blocks "$BLOCKS" --flag=-fastprune --restart-before-rollover 25
# A reorg away and back right before a -fastprune rollover (the fork's blocks roll the file over),
# and a manual prune.
start g24-reorg replay --dashd "$D24" --blocks "$BLOCKS" --flag=-fastprune --reorg-before-rollover 20
PRUNE_AT=$((BIG / 2))
start p24-prune replay --dashd "$D24" --blocks "$BLOCKS" --flag=-fastprune --flag=-prune=1 \
    --prune-at "$PRUNE_AT"
# Regtest's default -checkblockindex walks the whole index for every block an import accepts
# (quadratic); it does not change what is written to blk/rev files.
NOCHK=--flag=-checkblockindex=0
start l24-fastprune loadblock --dashd "$D24" --blocks "$BLOCKS" --flag=-fastprune $NOCHK
wait_all
# -reindex reuses the blk files of finished replays.
start x24-fastprune reindex --dashd "$D24" --source "$OUT/r24-fastprune" --flag=-fastprune $NOCHK
wait_all

dirs=$BASE
for c in r24-default r24-tinyblk r24-fastprune g24-reorg p24-prune l24-fastprune x24-fastprune; do
    [ -f "$OUT/$c/st2-manifest.json" ] || { echo "$c failed, see $OUT/$c.log" >&2; exit 1; }
    dirs=$dirs:$OUT/$c
done
echo "ST2_PARITY_DATADIRS=$dirs"
