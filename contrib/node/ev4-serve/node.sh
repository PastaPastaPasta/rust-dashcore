#!/bin/bash
# Starts or stops a v24.0.0-rc.3 dashd on one of the scratch copies of Core's functional-test
# regtest datadirs (/work/conformance/regtest-v24, copied to /work/scratch/ev4s/chains).
# Usage: node.sh start|stop NAME INDEX
set -euo pipefail
op=$1 name=$2 idx=$3
dir=/work/scratch/ev4s/chains/$name
bin=$HOME/.rust-dashcore-test/dashcore-24.0.0-rc.3/bin
p2p=$((47300 + idx * 10)) rpc=$((47301 + idx * 10))
extra=()
# Each test's consensus and relay arguments, as its node0 debug.log records them.
case $name in
  feature_asset_locks) extra+=(-llmqtestinstantsenddip0024=llmq_test_instantsend -acceptnonstdtxn=1) ;;
  feature_masternode_payout_shares) extra+=(-vbparams=v24:0:999999999999:100:10:8:6:5:0) ;;
  feature_masternode_shares) extra+=(-vbparams=evo_shares:0:999999999999:2000:10:8:6:5:0 -vbparams=v24:0:999999999999:250:10:8:6:5:0) ;;
  feature_protx_version) extra+=(-deprecatedrpc=legacy_mn -testactivationheight=v19@200 -vbparams=v24:0:999999999999:350:10:8:6:5:0) ;;
  rpc_netinfo) extra+=(-dip3params=2:2 -vbparams=v24:0:999999999999:100:10:8:6:5:0 -deprecatedrpc=service) ;;
esac
case $op in
  start)
    systemd-run --user --unit="ev4s-dashd-$name" --collect --same-dir \
      "$bin/dashd" -datadir="$dir" -port=$p2p -rpcport=$rpc -listen=1 -connect=0 \
      -rpcuser=ev4s -rpcpassword=ev4s -v2transport=0 -debug=net "${extra[@]}"
    ;;
  stop) systemctl --user stop "ev4s-dashd-$name" ;;
  cli) shift 3; "$bin/dash-cli" -datadir="$dir" -rpcport=$rpc -rpcuser=ev4s -rpcpassword=ev4s "$@" ;;
esac
