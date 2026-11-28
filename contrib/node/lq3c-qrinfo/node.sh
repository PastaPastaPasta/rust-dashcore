#!/bin/bash
# Starts or stops a v24.0.0 dashd on a copy of feature_llmq_rotation's node0 datadir, or calls its RPC.
# Usage: node.sh start|stop|cli DATADIR [rpc args...]
# The copy keeps node0's dash.conf (its consensus arguments and -sporkkey); the command-line
# arguments below are the rest of node0's consensus arguments, as its debug.log records them
# (`Command-line arg:` lines), with the test's mocktime left out.
set -euo pipefail
op=$1 dir=$2
bin=${LQ3C_BIN:-/work/scratch/lq3c/bin/dashcore-24.0.0/bin}
p2p=${LQ3C_P2P:-47710} rpc=$((p2p + 1))
unit=lq3c-dashd-$(basename "$dir")
case $op in
  start)
    systemd-run --user --unit="$unit" --collect --same-dir \
      "$bin/dashd" -datadir="$dir" -port=$p2p -rpcport=$rpc -listen=1 -connect=0 \
      -rpcuser=lq3c -rpcpassword=lq3c -v2transport=0 -debug=net \
      -llmqtestinstantsendparams=4:4 -llmqtestparams=4:4 -llmqtestplatformparams=4:4 \
      -vbparams=testdummy:999999999999:999999999999
    ;;
  stop) systemctl --user stop "$unit" ;;
  cli) shift 2; "$bin/dash-cli" -datadir="$dir" -rpcport=$rpc -rpcuser=lq3c -rpcpassword=lq3c "$@" ;;
esac
