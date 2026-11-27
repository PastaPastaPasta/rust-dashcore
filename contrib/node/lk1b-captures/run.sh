#!/usr/bin/env bash
# Runs one LK1b-s S1e capture against the O8-patched v24.0.0 dashd and writes its fixture.
#
# Usage: run.sh <dash-core-checkout> <bin-dir> <work-dir> <out-json> [main|rot] [capture args...]
#   e.g. run.sh ~/workspace/dash /work/scratch/statedigest/out /work/scratch/lk1b-capture \
#            dashd-node/tests/data/lk1b-is/main.json main
#
# <bin-dir> is contrib/node/statedigest/build.sh's --out: dashd, dash-cli, dash-wallet and
# build-info.txt, whose sha256 lines must match the binaries and whose describe must be a v24.0.0
# build. The functional tests are exported from build-info's commit of <dash-core-checkout>, so they
# are the patched tree's own. isdump is built here (clang, its own target dir) unless ISDUMP names a
# binary. The datadirs and logs stay in <work-dir>/ft (--nocleanup); the per-checkpoint isdb copies
# and dumps go to <work-dir>/checkpoints. LK1B_PORTSEED (default 2411) and LK1B_TIMEOUT_FACTOR
# (default 8) tune the run. Needs python3 with venv.
set -uo pipefail

src="${1:?dash checkout}"; bin="${2:?bin dir}"; work="${3:?work dir}"; out="${4:?output json}"
run="${5:-main}"
shift 4; [ $# -gt 0 ] && shift
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
seed="${LK1B_PORTSEED:-2411}"
factor="${LK1B_TIMEOUT_FACTOR:-8}"
case "$run" in
    main) script=capture_lk1b.py ;;
    rot) script=capture_lk1b_rot.py ;;
    *) echo "run must be main or rot, not $run" >&2; exit 2 ;;
esac

info="$bin/build-info.txt"
[ -f "$info" ] || { echo "$info is missing: <bin-dir> must be statedigest/build.sh's --out" >&2; exit 1; }
while read -r key digest name; do
    [ "$key" = sha256 ] || continue
    have=$(sha256sum "$bin/$name" | cut -d' ' -f1)
    [ "$have" = "$digest" ] || { echo "$bin/$name does not match build-info.txt" >&2; exit 1; }
done < "$info"
commit=$(sed -n 's/^commit //p' "$info")
describe=$(sed -n 's/^describe //p' "$info")
case "$describe" in
    v24.0.0|v24.0.0-*) ;;
    *) echo "build-info describe is \"$describe\", not a v24.0.0 build" >&2; exit 1 ;;
esac
git -C "$src" cat-file -e "$commit^{commit}" || { echo "$src has no commit $commit" >&2; exit 1; }

# The probe gets its own datadir: `-version` writes the datadir's settings.json.
probe="$(mktemp -d)"
version=$("$bin/dashd" -datadir="$probe" -version | head -1)
rm -rf "$probe"
case "$version" in
    "Dash Core version v24.0.0"*) echo "$version ($describe)" ;;
    *) echo "$bin/dashd is \"$version\", not a v24.0.0 build" >&2; exit 1 ;;
esac

if [ -z "${ISDUMP:-}" ]; then
    (cd "$here/isdump" && CC=clang CXX=clang++ cargo build --locked --release -q) || exit 1
    ISDUMP="$here/isdump/target/release/isdump"
fi
[ -x "$ISDUMP" ] || { echo "no isdump binary at $ISDUMP" >&2; exit 1; }

mkdir -p "$work"
case "$out" in /*) ;; *) out="$PWD/$out" ;; esac
mkdir -p "$(dirname "$out")"
rm -rf "$work/core" && mkdir -p "$work/core"
git -C "$src" archive "$commit" test/ share/rpcauth | tar -x -C "$work/core" || exit 1
cp "$here"/capture_lk1b*.py "$here"/../lq3a-captures/capture_lq3a.py \
    "$here"/../lq3a-captures/capture_lq3_rotation.py "$work/core/test/functional/" || exit 1
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
export DASHD="$bin/dashd" DASHCLI="$bin/dash-cli" DASHWALLET="$bin/dash-wallet"
export LK1B_DASHD_VERSION="$version" LK1B_BUILD_DESCRIBE="$describe" LK1B_BUILD_COMMIT="$commit"

rm -rf "$work/ft" "$work/checkpoints"; mkdir -p "$work/ft" "$work/checkpoints"
cd "$work/core/test/functional" && "$work/venv/bin/python" "$script" \
    --configfile="$work/core/test/config.ini" --nocleanup --tmpdir="$work/ft/run" \
    --timeout-factor="$factor" --portseed="$seed" --out="$out" \
    --isdump="$ISDUMP" --checkpoints="$work/checkpoints" "$@"
