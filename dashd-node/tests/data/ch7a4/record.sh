#!/bin/bash
# Records blocks.txt: regtest blocks 1-57 from dashd v24.0.0-rc.4 with DIP0003 at 48 and DIP0024
# moved out of the window, one block's hex per line (CH7-notes D70).
# Usage: record.sh DATADIR OUT (DATADIR new and empty)
set -e
bin=~/.rust-dashcore-test/dashcore-24.0.0-rc.4/bin
dir=$1
out=$2
rpc=$((20000 + RANDOM % 20000))
cli="$bin/dash-cli -regtest -datadir=$dir -rpcport=$rpc -rpcuser=r -rpcpassword=r"
mkdir -p "$dir"
"$bin/dashd" -regtest -datadir="$dir" -port=$((rpc + 1)) -rpcport=$rpc -listen=0 -server=1 \
    -rpcuser=r -rpcpassword=r -mocktime=1700000000 -printtoconsole=0 -daemon \
    -dip3params=48:48 -testactivationheight=dip0024@1000
for _ in $(seq 1 120); do $cli getblockcount >/dev/null 2>&1 && break; sleep 0.5; done
$cli generatetoaddress 57 ycwedq2f3sz2Yf9JqZsBCQPxp18WU3Hp4J >/dev/null
: > "$out"
for h in $(seq 1 57); do $cli getblock "$($cli getblockhash "$h")" 0 >> "$out"; done
$cli stop >/dev/null
