#!/bin/bash
# Captures every chain's fixture: start rc.3 dashd on a fresh copy, capture, stop.
set -euo pipefail
cd /work/scratch/ev4s
T=tools
i=2
for name in feature_protx_version feature_asset_locks feature_masternode_shares feature_masternode_payout_shares rpc_netinfo; do
  rm -rf chains/$name && cp -a pristine/$name-node0 chains/$name
  $T/node.sh start $name $i
  for _ in $(seq 1 120); do $T/node.sh cli $name $i getblockcount >/dev/null 2>&1 && break; sleep 2; done
  python3 -I $T/capture.py $name $((47300 + i * 10)) $T/node.sh cli $name $i > fixtures/$name.json.tmp
  mv fixtures/$name.json.tmp fixtures/$name.json
  $T/node.sh stop $name $i
  echo "done $name"
  i=$((i + 1))
done
