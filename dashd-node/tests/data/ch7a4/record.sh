#!/bin/bash
# Records regtest blocks from dashd v24.0.0-rc.4 with DIP0003 at 48 and DIP0024 moved out of the
# window, one block's hex per line after `#` header lines naming the recording (CH7-notes D70).
#   record.sh DATADIR OUT          blocks 1-57 (ch7a4/blocks.txt)
#   record.sh DATADIR OUT N FORKS  blocks 1-N, then two forks into FORKS-a.txt and FORKS-b.txt
#                                  (ch7a4b: N 200, FORKS ch7a4b/fork)
# A fork's header names the block it branches off; its blocks follow at the heights after it.
# Fork a: `invalidateblock 62` (inside llmq_test's first mining window, 58-66), 150 blocks from
# 61 to `raw(51)`, `reconsiderblock 62`; it stays the active chain, as it has more work. Fork b:
# `invalidateblock 41` (below DIP0003), 7 blocks from 40 to `raw(51)`, `reconsiderblock 41`.
# DATADIR must be new and empty.
set -e
bin=~/.rust-dashcore-test/dashcore-24.0.0-rc.4/bin
dir=$1
out=$2
n=${3:-57}
forks=$4
rpc=$((20000 + RANDOM % 20000))
cli="$bin/dash-cli -regtest -datadir=$dir -rpcport=$rpc -rpcuser=r -rpcpassword=r"
args=(-regtest -mocktime=1700000000 -dip3params=48:48 -testactivationheight=dip0024@1000)
mkdir -p "$dir"
"$bin/dashd" "${args[@]}" -datadir="$dir" -port=$((rpc + 1)) -rpcport=$rpc -listen=0 -server=1 \
    -rpcuser=r -rpcpassword=r -printtoconsole=0 -daemon
for _ in $(seq 1 120); do $cli getblockcount >/dev/null 2>&1 && break; sleep 0.5; done
header() {
    echo "# $($cli -version | head -1)"
    echo "# dashd ${args[*]}"
    echo "# $1"
}
# Blocks FROM to TO of the active chain, one hex per line.
dump() {
    for h in $(seq "$1" "$2"); do $cli getblock "$($cli getblockhash "$h")" 0; done
}
$cli generatetoaddress "$n" ycwedq2f3sz2Yf9JqZsBCQPxp18WU3Hp4J >/dev/null
{
    header "generatetoaddress $n ycwedq2f3sz2Yf9JqZsBCQPxp18WU3Hp4J: blocks 1-$n"
    dump 1 "$n"
} > "$out"
if [ -n "$forks" ]; then
    raw=$($cli getdescriptorinfo 'raw(51)' | sed -n 's/.*"descriptor": "\(.*\)".*/\1/p')
    # Off PARENT, LEN blocks to `raw(51)`, into FILE.
    fork() {
        local parent=$1 len=$2 file=$3 cut
        cut=$($cli getblockhash $((parent + 1)))
        $cli invalidateblock "$cut"
        $cli generatetodescriptor "$len" "$raw" >/dev/null
        {
            header "fork off $parent $($cli getblockhash "$parent"): invalidateblock $cut, \
generatetodescriptor $len $raw, reconsiderblock"
            dump $((parent + 1)) $((parent + len))
        } > "$file"
        $cli reconsiderblock "$cut"
    }
    fork 61 150 "$forks-a.txt"
    fork 40 7 "$forks-b.txt"
fi
$cli stop >/dev/null
