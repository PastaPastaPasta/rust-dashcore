#!/usr/bin/env bash
# Standalone run of the BL-875 chain driver (driver_standalone.py) on Core v24.0.0's functional-test tree.
#
# Usage: contrib/node/evo-images/driver-standalone.sh <dash-checkout> <bin-dir> <work-dir> [driver_standalone args...]
#   e.g. contrib/node/evo-images/driver-standalone.sh ~/workspace/dash ~/.rust-dashcore-test/dashcore-24.0.0/bin \
#          /work/scratch/bl875/q1 --until 700 --testkit-keys <target>/debug/testkit-keys --key-seed-file <0600 file>
# Long: run it under systemd-run --user. Writes <work-dir>/{core,venv,ft,out,run.log}.
set -euo pipefail

src="${1:?dash checkout}"; bin="${2:?bin dir}"; work="${3:?work dir}"; shift 3
here="$(cd "$(dirname "$0")" && pwd)"
tag=v24.0.0
tag_commit=938fba502824b9d850b7c710601456dc3d71603e
dashd_sha256=2ad05ccb0951bc127d1048bf0bfea0956a968482644ed130f8f59fbfcd732223

[ "$(git -C "$src" rev-parse "$tag^{commit}")" = "$tag_commit" ] || { echo "tag $tag is not $tag_commit" >&2; exit 1; }
[ "$(sha256sum "$bin/dashd" | cut -d' ' -f1)" = "$dashd_sha256" ] || { echo "$bin/dashd is not v24.0.0" >&2; exit 1; }

umask 077
mkdir -p "$work"
rm -rf "$work/core" && mkdir -p "$work/core"
git -C "$src" archive "$tag" test/ | tar -x -C "$work/core"
cat > "$work/core/test/config.ini" <<CONF
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
CONF
[ -x "$work/venv/bin/python" ] || { python3 -m venv "$work/venv" && "$work/venv/bin/pip" -q install dash_hash==1.5.1; }
export DASHD="$bin/dashd" DASHCLI="$bin/dash-cli" DASHUTIL="$bin/dash-util" DASHWALLET="$bin/dash-wallet"
export PYTHONPATH="$work/core/test/functional"

rm -rf "$work/ft"
"$work/venv/bin/python" "$here/driver_standalone.py" --configfile="$work/core/test/config.ini" --nocleanup \
    --tmpdir="$work/ft" --timeout-factor=4 --out="$work/out" "$@" 2>&1 | tee "$work/run.log"
