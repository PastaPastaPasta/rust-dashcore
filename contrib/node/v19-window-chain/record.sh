#!/usr/bin/env bash
# Records a BL-648/BL-839 V19-window regtest chain: runs capture_v19_window.py through
# ../lq3a-captures/run.sh, checks the bytes with verify_chain.py, replays the chain into a fresh dashd
# (the probe), and writes PROVENANCE.
#
# Usage: record.sh <dash-core-checkout> <tag> <bin-dir> <work-dir> <reorg|v20-first|v1-above>
#   R1: record.sh ~/workspace/dash v24.0.0 ~/.rust-dashcore-test/dashcore-24.0.0/bin <work>/r1 reorg
#   R2: record.sh ~/workspace/dash v24.0.0 ~/.rust-dashcore-test/dashcore-24.0.0/bin <work>/r2 v20-first
#   R3: record.sh ~/workspace/dash v24.0.0 ~/.rust-dashcore-test/dashcore-24.0.0/bin <work>/r3 v1-above
#
# <work-dir>/out ends with chain.txt (R1 also fork.txt, R2 also clsigs.jsonl and switch-refused.txt, R3
# also serv-refused.txt and rev-refused.txt),
# summary.json, verify.txt, probe.json, probe.txt, test_framework.log, probe_framework.log and PROVENANCE. The
# datadirs (<work-dir>/ft, <work-dir>/probe/ft) are deleted after a passing run unless
# BL648_KEEP_DATADIRS=1, and kept after a failing one. LQ3A_PORTSEED (default here 9481) and
# LQ3A_TIMEOUT_FACTOR (8) pass through to run.sh. A work dir whose out/ already holds a PROVENANCE is
# refused unless BL648_OVERWRITE=1: a recording cannot be regenerated (fresh BLS keys every run).
# The probe check (verify_chain.py --probe) exits non-zero on a verdict other than the expected one,
# which fails this script before PROVENANCE is written.
set -euo pipefail

src="${1:?dash checkout}"; tag="${2:?tag}"; bin="${3:?bin dir}"; work="${4:?work dir}"; mode="${5:?reorg|v20-first|v1-above}"
case "$mode" in reorg|v20-first|v1-above) ;; *) echo "mode: reorg, v20-first or v1-above, not $mode" >&2; exit 2 ;; esac
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
mkdir -p "$work"
work="$(cd "$work" && pwd)"
out="$work/out"
if [ -e "$out/PROVENANCE" ] && [ "${BL648_OVERWRITE:-0}" != 1 ]; then
    echo "$out holds a recording (PROVENANCE); move it away or set BL648_OVERWRITE=1" >&2
    exit 1
fi
rm -rf "$out" "$work/probe"
mkdir -p "$out" "$work/probe"
# The recorder as it starts: run.sh copies the capture script now, and verify_chain.py runs later.
recorder="$(git -C "$here" rev-parse HEAD)"
dirty="$(git -C "$here" status --porcelain -- . ../lq3a-captures)"
cp "$here/verify_chain.py" "$work/verify_chain.py"
recorder_sums="$(cd "$here" && sha256sum capture_v19_window.py verify_chain.py record.sh ../lq3a-captures/run.sh ../lq3a-captures/capture_lq3a.py)"

# R3 signs ProTx payloads under the basic scheme with py_ecc; run.sh keeps a venv that exists.
if [ "$mode" = v1-above ]; then
    [ -x "$work/venv/bin/python" ] || python3 -m venv "$work/venv"
    "$work/venv/bin/pip" -q install dash_hash "py_ecc==8.0.0"
fi

export LQ3A_SCRIPT=capture_v19_window.py LQ3A_EXTRA_DIR="$here" LQ3A_PORTSEED="${LQ3A_PORTSEED:-9481}"
started="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
t0=$(date +%s)
rc=0
bash "$here/../lq3a-captures/run.sh" "$src" "$tag" "$bin" "$work" "$out" --mode "$mode" || rc=$?
wall=$(( $(date +%s) - t0 ))
cp "$work/ft/run/test_framework.log" "$out/" 2>/dev/null || true
if [ "$rc" -ne 0 ]; then
    echo "capture failed (rc $rc) after ${wall}s; datadirs kept in $work/ft" >&2
    exit "$rc"
fi
"$work/venv/bin/python" -I "$work/verify_chain.py" "$out" | tee "$out/verify.txt"

# The probe: a fresh dashd per replay, with the recording's chain args; its own work dir, the same venv.
ln -sfn "$work/venv" "$work/probe/venv"
t1=$(date +%s)
rc=0
bash "$here/../lq3a-captures/run.sh" "$src" "$tag" "$bin" "$work/probe" "$work/probe/out" \
    --mode "probe-$mode" --chain "$out" || rc=$?
probe_wall=$(( $(date +%s) - t1 ))
cp "$work/probe/ft/run/test_framework.log" "$out/probe_framework.log" 2>/dev/null || true
if [ "$rc" -ne 0 ]; then
    echo "probe failed (rc $rc) after ${probe_wall}s; datadirs kept in $work/probe/ft" >&2
    exit "$rc"
fi
cp "$work/probe/out/probe.json" "$out/probe.json"
"$work/venv/bin/python" -I "$work/verify_chain.py" --probe "$out" | tee "$out/probe.txt"
# The children's user and system time: run.sh's python waits for every dashd it started. `times`
# runs in this shell (a pipe or $(...) would fork and report the fork's zero).
times > "$work/times.txt"
cpu="$(tail -1 "$work/times.txt")"

files="chain.txt summary.json"
[ "$mode" = reorg ] && files="chain.txt fork.txt summary.json"
[ "$mode" = v20-first ] && files="chain.txt clsigs.jsonl switch-refused.txt summary.json"
[ "$mode" = v1-above ] && files="chain.txt serv-refused.txt rev-refused.txt summary.json"
sums="$(cd "$out" && sha256sum $files probe.json)"
version="$("$work/venv/bin/python" -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["dashd_version"])' "$out/summary.json")"
{
    echo "BL-648/BL-839 V19-window chain ($mode), recorded $started to $(date -u +%Y-%m-%dT%H:%M:%SZ) on $(hostname)"
    echo "recorder: contrib/node/v19-window-chain at $recorder${dirty:+ (modified or untracked at start: $(printf '%s\n' "$dirty" | tr '\n' ';'))}"
    echo "recorder files at start (sha256):"
    echo "$recorder_sums"
    echo "verifier as run: $(sha256sum "$work/verify_chain.py" | cut -d' ' -f1)"
    echo "capture script as run: $(sha256sum "$work/core/test/functional/capture_v19_window.py" | cut -d' ' -f1)"
    echo "command: record.sh $src $tag $bin $work $mode"
    echo "core: $tag ($(git -C "$src" rev-parse "$tag^{commit}")), test/ exported from it"
    echo "dashd: $version, $bin/dashd sha256 $(sha256sum "$bin/dashd" | cut -d' ' -f1)"
    echo "portseed $LQ3A_PORTSEED, timeout factor ${LQ3A_TIMEOUT_FACTOR:-8}"
    echo "venv: $("$work/venv/bin/pip" freeze 2>/dev/null | tr '\n' ' ')"
    echo "wall: capture ${wall}s, probe ${probe_wall}s; children user/sys: $cpu (includes run.sh's exports)"
    echo
    echo "sha256:"
    echo "$sums"
    echo "sizes (bytes):"
    (cd "$out" && wc -c $files | grep -v total)
    echo
    cat "$out/verify.txt"
    echo
    cat "$out/probe.txt"
} > "$out/PROVENANCE"

if [ "${BL648_KEEP_DATADIRS:-0}" != 1 ]; then
    rm -rf "$work/ft" "$work/probe/ft"
fi
echo "done: $out"
