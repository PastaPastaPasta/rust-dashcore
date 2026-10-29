#!/usr/bin/env bash
# Runs Dash Core's own functional tests that exercise the v24 wire formats against a dashd release,
# keeping every node's datadir for extract.py.
#
# Usage: run-functional-tests.sh <dash-core-checkout> <tag> <bin-dir> <work-dir> [test...]
#   e.g. run-functional-tests.sh ~/workspace/dash v24.0.0-rc.3 /path/to/dashcore-24.0.0-rc.3/bin /tmp/v24-vectors
#
# <bin-dir> must hold dashd and dash-cli of the same release as <tag>. Needs python3 with venv.
# Without test names it runs the tests extract.py reads. Exits non-zero if any test fails.
set -uo pipefail

src="${1:?dash checkout}"; tag="${2:?tag}"; bin="${3:?bin dir}"; work="${4:?work dir}"
shift 4
tests=("$@")
[ ${#tests[@]} -gt 0 ] || tests=(feature_asset_locks feature_masternode_shares feature_masternode_payout_shares
                                 feature_protx_version rpc_netinfo feature_llmq_evo feature_evo_shares_activation)

# The binaries must be exactly the release the tests come from: a substring match would let the
# v24.0.0 tag accept a v24.0.0-rc.3 binary.
version=$("$bin/dashd" -version | head -1)
if [ "$version" = "Dash Core version $tag" ]; then
    echo "$version"
else
    echo "$bin/dashd is \"$version\", not \"Dash Core version $tag\"" >&2; exit 1
fi

mkdir -p "$work"
rm -rf "$work/core" && mkdir -p "$work/core"
git -C "$src" archive "$tag" test/ share/rpcauth | tar -x -C "$work/core"
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

seed=410
pids=()
for t in "${tests[@]}"; do
    rm -rf "$work/ft/$t"; mkdir -p "$work/ft"
    (cd "$work/core/test/functional" && "$work/venv/bin/python" "$t.py" \
        --configfile="$work/core/test/config.ini" --nocleanup --tmpdir="$work/ft/$t" \
        --timeout-factor=4 --portseed=$seed > "$work/ft/$t.log" 2>&1) &
    pids+=($!)
    seed=$((seed + 7))
done
failed=0
for i in "${!tests[@]}"; do
    if wait "${pids[$i]}"; then status=ok; else status=FAILED; failed=1; fi
    echo "${tests[$i]}: $status, $(tail -1 "$work/ft/${tests[$i]}.log")"
done
exit $failed
