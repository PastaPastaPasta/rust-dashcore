#!/usr/bin/env bash
# Runs capture_sb11_testnet.py against a private testnet dashd and writes the SB11 testnet capture.
#
# Usage: run-testnet.sh <dash-core-checkout> <tag> <bin-dir> <work-dir> <rpc-url> <p2p-port> <out-json.gz> [args...]
#   e.g. run-testnet.sh ~/workspace/dash v24.0.0-rc.3 ~/.rust-dashcore-test/dashcore-24.0.0-rc.3/bin \
#            /work/scratch/sb11/capture-testnet http://sb11:sb11@127.0.0.1:19512 19511 \
#            dash-mnlist/tests/data/sb11-testnet-v24.0.captures.json.gz
#
# The dashd serving <rpc-url> must be <bin-dir>/dashd of exactly <tag>, on a datadir copy with no outbound
# networking (-connect=0 -dnsseed=0 -listen=1 -bind=127.0.0.1); see PROVENANCE.md.
# SB11_CAPTURE names another harness here (capture_sb11_testnet_fetch.py); [args...] go to it.
set -euo pipefail
src="${1:?dash checkout}"; tag="${2:?tag}"; bin="${3:?bin dir}"; work="${4:?work dir}"
rpc="${5:?rpc url}"; port="${6:?p2p port}"; out="${7:?output json}"; shift 7
capture="${SB11_CAPTURE:-capture_sb11_testnet.py}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
probe="$(mktemp -d)"
version=$("$bin/dashd" -datadir="$probe" -version | head -1)
rm -rf "$probe"
[ "$version" = "Dash Core version $tag" ] || { echo "$bin/dashd is \"$version\", not $tag" >&2; exit 1; }
case "$out" in /*) ;; *) out="$PWD/$out" ;; esac
mkdir -p "$work" "$(dirname "$out")"
rm -rf "$work/core" && mkdir -p "$work/core"
git -C "$src" archive "$tag" test/functional | tar -x -C "$work/core"
cp "$here/capture_sb11_testnet.py" "$here/$capture" "$here/../lq3a-captures/capture_lq3a.py" "$work/core/test/functional/"
[ -x "$work/venv/bin/python" ] || { python3 -m venv "$work/venv" && "$work/venv/bin/pip" -q install dash_hash; }
cd "$work/core/test/functional" && "$work/venv/bin/python" "$capture" \
    --rpc="$rpc" --p2p-port="$port" --dashd="$bin/dashd" --core-tag="$tag" --out="$out" "$@"
