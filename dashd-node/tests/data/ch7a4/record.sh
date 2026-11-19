#!/bin/bash
# Records regtest blocks from dashd v24.0.0-rc.4 with DIP0003 at 48 and DIP0024 moved out of the
# window, one block's hex per line after `#` header lines naming the recording (CH7-notes D70).
#   record.sh DATADIR OUT          blocks 1-57 (ch7a4/blocks.txt)
#   record.sh DATADIR OUT N FORKS  blocks 1-N, then two forks into FORKS-a.txt and FORKS-b.txt
#                                  (ch7a4b: N 200, FORKS ch7a4b/fork)
#   record.sh mn DATADIR OUT       300 blocks with masternodes (ch7a5/blocks.txt; see below)
#   record.sh deep DATADIR OUT N   blocks 1-N with DIP0024, V20 and MN_RR moved past N
#                                  (CH7a-5's evo-deep-window guard; not committed)
# A fork's header names the block it branches off; its blocks follow at the heights after it.
# Fork a: `invalidateblock 62` (inside llmq_test's first mining window, 58-66), 150 blocks from
# 61 to `raw(51)`, `reconsiderblock 62`; it stays the active chain, as it has more work. Fork b:
# `invalidateblock 41` (below DIP0003), 7 blocks from 40 to `raw(51)`, `reconsiderblock 41`.
# mn: a wallet mines every block; an EvoNode and a regular masternode register at 116 and 117
# (`protx register_fund_evo`, `register_fund`), the EvoNode's ProUpServTx is in 141, its
# ProUpRegTx (a new operator key) in 161, the regular node's ProUpRevTx (reason 2) in 181, and
# the spend of its collateral in 201. V20 is at 432, so all 300 blocks are before it. A `#` line
# after the first header names each special transaction: `# HEIGHT KIND TXID`, and the keys.
# DATADIR must be new and empty.
set -e
bin=~/.rust-dashcore-test/dashcore-24.0.0-rc.4/bin
mode=
case $1 in mn | deep) mode=$1; shift ;; esac
dir=$1
out=$2
n=${3:-57}
forks=$4
rpc=$((20000 + RANDOM % 20000))
cli="$bin/dash-cli -regtest -datadir=$dir -rpcport=$rpc -rpcuser=r -rpcpassword=r"
args=(-regtest -mocktime=1700000000 -dip3params=48:48 -testactivationheight=dip0024@1000)
if [ "$mode" = deep ]; then
    args=(-regtest -mocktime=1700000000 -dip3params=48:48 -testactivationheight=dip0024@1000000
        -testactivationheight=v20@1000000 -testactivationheight=mn_rr@1000000)
fi
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
if [ "$mode" = mn ]; then
    $cli createwallet w >/dev/null
    a=$($cli getnewaddress)
    # Mines to HEIGHT.
    to() { $cli generatetoaddress $(($1 - $($cli getblockcount))) "$a" >/dev/null; }
    events=()
    # Mines one block holding TXID and notes it as KIND.
    mined() {
        to $(($($cli getblockcount) + 1))
        local h
        h=$($cli getblockcount)
        $cli getblock "$($cli getblockhash "$h")" | grep -q "\"$2\"" || { echo "$2 not in $h" >&2; exit 1; }
        events+=("# $h $1 $2")
    }
    field() { sed -n "s/.*\"$1\": \"\(.*\)\".*/\1/p"; }
    bls1=$($cli bls generate)
    bls2=$($cli bls generate)
    bls3=$($cli bls generate)
    to 115
    evo=$($cli protx register_fund_evo "$($cli getnewaddress)" '["127.0.0.1:20001"]' \
        "$($cli getnewaddress)" "$(echo "$bls1" | field public)" "$($cli getnewaddress)" 0 \
        "$($cli getnewaddress)" 0000000000000000000000000000000000000001 20002 20003 "$a")
    mined register_fund_evo "$evo"
    mn=$($cli protx register_fund "$($cli getnewaddress)" '["127.0.0.1:20021"]' \
        "$($cli getnewaddress)" "$(echo "$bls2" | field public)" "$($cli getnewaddress)" 0 \
        "$($cli getnewaddress)" "$a")
    mined register_fund "$mn"
    to 140
    mined update_service_evo "$($cli protx update_service_evo "$evo" '["127.0.0.1:20011"]' \
        "$(echo "$bls1" | field secret)" 0000000000000000000000000000000000000002 20012 20013 "" "$a")"
    to 160
    mined update_registrar "$($cli protx update_registrar "$evo" \
        "$(echo "$bls3" | field public)" "$($cli getnewaddress)" "" "$a")"
    to 180
    mined revoke "$($cli protx revoke "$mn" "$(echo "$bls2" | field secret)" 2 "$a")"
    to 200
    # The regular node's collateral: output 1 of its ProRegTx (`register_fund`'s layout).
    collateral=$($cli protx info "$mn" | field collateralHash)
    index=$($cli protx info "$mn" | sed -n 's/.*"collateralIndex": \([0-9]*\).*/\1/p')
    $cli lockunspent true "[{\"txid\":\"$collateral\",\"vout\":$index}]" >/dev/null
    raw=$($cli createrawtransaction "[{\"txid\":\"$collateral\",\"vout\":$index}]" \
        "[{\"$($cli getnewaddress)\":999.999}]")
    signed=$($cli signrawtransactionwithwallet "$raw" | field hex)
    mined collateral_spend "$($cli sendrawtransaction "$signed")"
    to 300
    mkdir -p "$out"
    {
        header "a wallet mines every block to $a; the special transactions, as HEIGHT KIND TXID:"
        printf '%s\n' "${events[@]}"
        echo "# EvoNode $evo: operator keys $(echo "$bls1" | field public), then $(echo "$bls3" | field public)"
        echo "# masternode $mn: operator key $(echo "$bls2" | field public)"
        dump 1 300
    } > "$out/blocks.txt"
    $cli stop >/dev/null
    exit 0
fi
# In calls of at most 1,000 blocks, within dash-cli's timeout. Each block's time is a second past
# its median time, so a deep chain outruns the mock clock by more than the two hours a block may
# lead it: there the clock moves to the tip's time after each call.
while [ "$($cli getblockcount)" -lt "$n" ]; do
    left=$((n - $($cli getblockcount)))
    $cli generatetoaddress $((left < 1000 ? left : 1000)) ycwedq2f3sz2Yf9JqZsBCQPxp18WU3Hp4J >/dev/null
    if [ "$mode" = deep ]; then
        $cli setmocktime "$($cli getblockheader "$($cli getbestblockhash)" | sed -n 's/.*"time": \([0-9]*\).*/\1/p')"
    fi
done
{
    header "generatetoaddress $n ycwedq2f3sz2Yf9JqZsBCQPxp18WU3Hp4J: blocks 1-$n${mode:+, setmocktime to the tip after each 1,000}"
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
