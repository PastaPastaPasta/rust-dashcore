#!/usr/bin/env bash
# Records the evo-deep-window guard's regtest chain (BL-646): runs capture_deep_window.py through
# ../lq3a-captures/run.sh, checks the bytes with verify_chain.py, compresses the chain files and
# writes PROVENANCE.
#
# Usage: record.sh <dash-core-checkout> <tag> <bin-dir> <work-dir> [capture args...]
#   full:  record.sh ~/workspace/dash v24.0.0 ~/.rust-dashcore-test/dashcore-24.0.0/bin <work>/full
#   pilot: record.sh ... <work>/pilot --cycles 20 --fork-depth 120 --side-blocks 48
# Capture args: --blocks N (20100), --cycles C, --fork-depth D (1150), --side-blocks L (100).
#
# <work-dir>/out ends with chain.txt.zst (M), fork.txt.zst (S), summary.json, verify.txt,
# test_framework.log and PROVENANCE. The functional-test datadirs (<work-dir>/ft) are deleted after
# a passing run unless BL646_KEEP_DATADIRS=1, and kept after a failing one.
# LQ3A_PORTSEED (default here 9173) and LQ3A_TIMEOUT_FACTOR (8) pass through to run.sh.
# A work dir whose out/ already holds a PROVENANCE is refused unless BL646_OVERWRITE=1: a recording
# cannot be regenerated (fresh BLS keys every run).
set -euo pipefail

src="${1:?dash checkout}"; tag="${2:?tag}"; bin="${3:?bin dir}"; work="${4:?work dir}"
shift 4
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
mkdir -p "$work"
work="$(cd "$work" && pwd)"
out="$work/out"
if [ -e "$out/PROVENANCE" ] && [ "${BL646_OVERWRITE:-0}" != 1 ]; then
    echo "$out holds a recording (PROVENANCE); move it away or set BL646_OVERWRITE=1" >&2
    exit 1
fi
rm -rf "$out"
mkdir -p "$out"
# The recorder as it starts: run.sh copies the capture script now, and verify_chain.py runs at the end.
recorder="$(git -C "$here" rev-parse HEAD)"
dirty="$(git -C "$here" status --porcelain -- . ../lq3a-captures)"
cp "$here/verify_chain.py" "$work/verify_chain.py"
recorder_sums="$(cd "$here" && sha256sum capture_deep_window.py verify_chain.py record.sh ../lq3a-captures/run.sh)"

export LQ3A_SCRIPT=capture_deep_window.py LQ3A_EXTRA_DIR="$here" LQ3A_PORTSEED="${LQ3A_PORTSEED:-9173}"
started="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
t0=$(date +%s)
rc=0
bash "$here/../lq3a-captures/run.sh" "$src" "$tag" "$bin" "$work" "$out" "$@" || rc=$?
wall=$(( $(date +%s) - t0 ))
# The children's user and system time: run.sh's python waits for every dashd it started. `times`
# runs in this shell (a pipe or $(...) would fork and report the fork's zero).
times > "$work/times.txt"
cpu="$(tail -1 "$work/times.txt")"
cp "$work/ft/run/test_framework.log" "$out/" 2>/dev/null || true
if [ "$rc" -ne 0 ]; then
    echo "capture failed (rc $rc) after ${wall}s; datadirs kept in $work/ft" >&2
    exit "$rc"
fi

"$work/venv/bin/python" -I "$work/verify_chain.py" "$out" | tee "$out/verify.txt"

sums_txt="$(cd "$out" && sha256sum chain.txt fork.txt)"
zstd -q -19 --rm "$out/chain.txt" "$out/fork.txt"
sums_zst="$(cd "$out" && sha256sum chain.txt.zst fork.txt.zst summary.json)"
version="$("$work/venv/bin/python" -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["dashd_version"])' "$out/summary.json")"
{
    echo "BL-646 evo-deep-window chain, recorded $started to $(date -u +%Y-%m-%dT%H:%M:%SZ) on $(hostname)"
    echo "recorder: contrib/node/evo-deep-window-chain at $recorder${dirty:+ (modified or untracked at start: $(printf '%s\n' "$dirty" | tr '\n' ';'))}"
    echo "recorder files at start (sha256):"
    echo "$recorder_sums"
    echo "verifier as run: $(sha256sum "$work/verify_chain.py" | cut -d' ' -f1)"
    echo "capture script as run: $(sha256sum "$work/core/test/functional/capture_deep_window.py" | cut -d' ' -f1)"
    echo "command: record.sh $src $tag $bin $work $*"
    echo "core: $tag ($(git -C "$src" rev-parse "$tag^{commit}")), test/ exported from it"
    echo "dashd: $version, $bin/dashd sha256 $(sha256sum "$bin/dashd" | cut -d' ' -f1)"
    echo "portseed $LQ3A_PORTSEED, timeout factor ${LQ3A_TIMEOUT_FACTOR:-8}"
    echo "wall ${wall}s; children user/sys: $cpu (includes run.sh's export, and the dash_hash build on a new venv)"
    echo
    echo "sha256 (uncompressed):"
    echo "$sums_txt"
    echo "sha256:"
    echo "$sums_zst"
    echo "sizes (bytes):"
    (cd "$out" && wc -c chain.txt.zst fork.txt.zst summary.json | grep -v total)
    echo
    cat "$out/verify.txt"
} > "$out/PROVENANCE"

if [ "${BL646_KEEP_DATADIRS:-0}" != 1 ]; then
    rm -rf "$work/ft"
fi
echo "done: $out"
