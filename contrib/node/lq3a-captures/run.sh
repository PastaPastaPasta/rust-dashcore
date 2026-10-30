#!/usr/bin/env bash
# Runs capture_lq3a.py against a Dash Core release and writes the LQ3a regtest capture fixture.
#
# Usage: run.sh <dash-core-checkout> <tag> <bin-dir> <work-dir> <out-json>
#   e.g. run.sh ~/workspace/dash v24.0.0-rc.3 ~/.rust-dashcore-test/dashcore-24.0.0-rc.3/bin \
#            /work/scratch/lq3a/capture dash-evo/tests/data/lq3a-regtest-captures.json
#
# <bin-dir> must hold dashd and dash-cli of exactly the release <tag>. Needs python3 with venv.
# LQ3A_SCRIPT picks the capture (default capture_lq3a.py; capture_lq3_rotation.py for BL-218's chain).
# LQ3A_EXTRA_DIR adds another directory's capture_*.py (sb11-captures builds on capture_lq3a.py).
# The functional-test datadirs and logs stay in <work-dir>/ft (--nocleanup).
set -uo pipefail

src="${1:?dash checkout}"; tag="${2:?tag}"; bin="${3:?bin dir}"; work="${4:?work dir}"; out="${5:?output json}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
seed="${LQ3A_PORTSEED:-2311}"
factor="${LQ3A_TIMEOUT_FACTOR:-8}"
script="${LQ3A_SCRIPT:-capture_lq3a.py}"

# The binaries must be exactly the release the tests come from. The probe gets its own datadir:
# `-version` writes the datadir's settings.json, so a probe on the shared ~/.dashcore can race another.
probe="$(mktemp -d)"
version=$("$bin/dashd" -datadir="$probe" -version | head -1)
rm -rf "$probe"
if [ "$version" = "Dash Core version $tag" ]; then
    echo "$version"
else
    echo "$bin/dashd is \"$version\", not \"Dash Core version $tag\"" >&2; exit 1
fi

mkdir -p "$work"
case "$out" in /*) ;; *) out="$PWD/$out" ;; esac
mkdir -p "$(dirname "$out")"
rm -rf "$work/core" && mkdir -p "$work/core"
git -C "$src" archive "$tag" test/ share/rpcauth | tar -x -C "$work/core"
cp "$here"/capture_*.py ${LQ3A_EXTRA_DIR:+"$LQ3A_EXTRA_DIR"/capture_*.py} "$work/core/test/functional/"
cat > "$work/core/test/config.ini" <<EOC
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
EOC
[ -x "$work/venv/bin/python" ] || { python3 -m venv "$work/venv" && "$work/venv/bin/pip" -q install dash_hash; } || exit 1
export DASHD="$bin/dashd" DASHCLI="$bin/dash-cli" DASHUTIL="$bin/dash-util" DASHWALLET="$bin/dash-wallet"
export LQ3A_DASHD_VERSION="$version" LQ3A_CORE_TAG="$tag"

rm -rf "$work/ft"; mkdir -p "$work/ft"
cd "$work/core/test/functional" && "$work/venv/bin/python" "$script" \
    --configfile="$work/core/test/config.ini" --nocleanup --tmpdir="$work/ft/run" \
    --timeout-factor="$factor" --portseed="$seed" --out="$out"
