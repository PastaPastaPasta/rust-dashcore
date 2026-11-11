#!/usr/bin/env bash
# Builds regtest chains that exercise Dash Core v24 payloads by running Core's own functional
# tests, then runs the conformance harness over each chain in RPC mode (every block is
# cross-checked against getblock 1 and 2).
#
# Usage: conformance/scripts/regtest-v24.sh <dash-core-checkout> <tag> <bin-dir> <work-dir>
#   e.g. conformance/scripts/regtest-v24.sh ~/workspace/dash v24.0.0-rc.3 \
#          /work/chains/bin/current/bin /tmp/regtest-v24
#
# <bin-dir> must hold dashd/dash-cli of the same release as <tag>. Needs python3 with venv.
set -euo pipefail

src="${1:?dash checkout}"; tag="${2:?tag}"; bin="${3:?bin dir}"; work="${4:?work dir}"
harness="$(cd "$(dirname "$0")/../.." && pwd)/target/release/dash-conformance"
tests=(feature_asset_locks feature_masternode_shares feature_masternode_payout_shares
       feature_protx_version rpc_netinfo)

mkdir -p "$work"
if [ ! -x "$harness" ]; then
    (cd "$(dirname "$0")/../.." && cargo build --release -p dash-conformance)
fi

# Functional tests of exactly the release under test, with a hand-written config.ini.
rm -rf "$work/core" && mkdir -p "$work/core"
git -C "$src" archive "$tag" test/ | tar -x -C "$work/core"
cat > "$work/core/test/config.ini" <<EOF
[environment]
PACKAGE_NAME=Dash Core
PACKAGE_BUGREPORT=https://github.com/dashpay/dash/issues
SRCDIR=$work/core
BUILDDIR=$work/core
EXEEXT=
RPCAUTH=$work/core/share/rpcauth/rpcauth.py
[components]
ENABLE_WALLET=true
USE_SQLITE=true
USE_BDB=true
ENABLE_CLI=true
ENABLE_BITCOIND=true
ENABLE_ZMQ=true
EOF
[ -x "$work/venv/bin/python" ] || { python3 -m venv "$work/venv" && "$work/venv/bin/pip" -q install dash_hash; }
export DASHD="$bin/dashd" DASHCLI="$bin/dash-cli" DASHUTIL="$bin/dash-util" DASHWALLET="$bin/dash-wallet"

port=26100
for t in "${tests[@]}"; do
    rm -rf "$work/ft/$t"
    echo "== $t: functional test"
    if ! (cd "$work/core/test/functional" && "$work/venv/bin/python" "$t.py" \
            --configfile="$work/core/test/config.ini" --nocleanup --tmpdir="$work/ft/$t" \
            --timeout-factor=4 --portseed=$((port % 1000)) > "$work/ft-$t.log" 2>&1); then
        echo "   (test failed, see $work/ft-$t.log; checking the chain it left anyway)"
    fi

    # Restart node0 from a copy of its datadir and check it over RPC.
    dd="$work/node/$t"; rm -rf "$dd"; mkdir -p "$(dirname "$dd")"; cp -a "$work/ft/$t/node0" "$dd"
    "$bin/dashd" -datadir="$dd" -port=$((port + 1)) -rpcport=$port -listen=1 -connect=0 \
        -checkblocks=0 -checklevel=0 -daemon=1 -printtoconsole=0 > /dev/null
    for _ in $(seq 1 60); do "$bin/dash-cli" -datadir="$dd" -rpcport=$port getblockcount > /dev/null 2>&1 && break; sleep 1; done
    echo "== $t: harness"
    rm -rf "$work/reports/$t"
    "$harness" rpc --net regtest --rpc-url "http://127.0.0.1:$port" --cookie "$dd/regtest/.cookie" \
        --verbose-every 1 --chunk 100 --batch 20 --out "$work/reports/$t" 2>&1 | grep -vE '^\[' || true
    "$bin/dash-cli" -datadir="$dd" -rpcport=$port stop > /dev/null
    port=$((port + 10))
done
