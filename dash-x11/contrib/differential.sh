#!/usr/bin/env bash
# The large differential against Dash Core: tests/core.rs `core_live_x11` and
# `core_live_stages` over many seeds, in parallel, for each harness given.
#
# Usage: contrib/differential.sh <seeds> <cases-per-seed> <harness>...
#   contrib/differential.sh 10 1000000 /path/core-v23.1.8 /path/core-v24.0.0-rc.3
# runs, per harness and per seed 1..=<seeds>: <cases-per-seed> X11 inputs of every length 0..=200
# ("random"), as many 80-byte inputs ("header"), <cases-per-seed>/10 inputs of 201..=1100 bytes
# ("long") and <cases-per-seed>/10 inputs per chain stage, each through every implementation this
# CPU runs. A harness may be a command with arguments (quote it), e.g. to make Core use its
# software ECHO and SHAvite-3: "env DASH_X11_CORE_SOFT=1 /path/harness"; give absolute paths
# (the script runs from the workspace root). JOBS (default 12) bounds the processes. DIFF_LOG
# sets the log directory (default target/dash-x11-differential; it is emptied first);
# DIFF_TEST_BIN runs a given test binary instead of building tests/core.rs
# (contrib/test-differential.sh uses it).
#
# Exits 0 only if every scheduled run left its log, and that log reports a pass and every
# stream the run was to compare (one per X11 run, eleven per stage run).
set -euo pipefail

usage() {
    echo "usage: differential.sh <seeds >= 1> <cases-per-seed >= 10> <harness>..." >&2
    exit 2
}
[ $# -ge 3 ] || usage
seeds=$1 cases=$2
shift 2
[ "$seeds" -ge 1 ] && [ "$cases" -ge 10 ] || usage
for harness in "$@"; do
    [ -n "${harness// /}" ] || usage
done
# abspath <path>: absolute, resolved against the caller's directory (portable: no realpath -m).
abspath() {
    mkdir -p "$(dirname "$1")"
    echo "$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
}
# Resolved before the cd below.
bin=${DIFF_TEST_BIN:+$(abspath "$DIFF_TEST_BIN")}
log=$(abspath "${DIFF_LOG:-$(dirname "$0")/../../target/dash-x11-differential}")
cd "$(dirname "$0")/../.."
[ -n "$bin" ] || bin=$(cargo test -q --release -p dash-x11 --test core --no-run --message-format=json |
    python3 -c 'import json, sys
for line in sys.stdin:
    m = json.loads(line)
    if m.get("reason") == "compiler-artifact" and m["target"]["name"] == "core" and m.get("executable"):
        print(m["executable"])')
rm -rf "$log"
mkdir -p "$log"
jobs=()
logs=()
streams=()
# job <log name> <streams> <environment> <test>: schedules one run of the test binary, which
# must report <streams> matching streams.
job() {
    logs+=("$log/$1.log")
    streams+=("$2")
    jobs+=("$3 '$bin' --exact $4 --nocapture > '$log/$1.log' 2>&1")
}
h=0
for harness in "$@"; do
    h=$((h + 1))
    for seed in $(seq 1 "$seeds"); do
        # The harness's position keeps logs apart when two share a file name.
        tag=h$h-$(basename "${harness##* }")-$seed
        env="DASH_X11_CORE_HARNESS='$harness' DASH_X11_CORE_SEED=$seed"
        for kind in random header; do
            job "x11-$kind-$tag" 1 "$env DASH_X11_CORE_CASES=$cases DASH_X11_KIND=$kind" core_live_x11
        done
        job "x11-long-$tag" 1 "$env DASH_X11_CORE_CASES=$((cases / 10)) DASH_X11_KIND=long" core_live_x11
        job "stages-$tag" 11 "$env DASH_X11_CORE_CASES=$((cases / 10))" core_live_stages
    done
done
# NUL-separated, so the quotes around multi-word harness commands survive, and without -I,
# whose substitutions BSD xargs truncates at 255 bytes. A failed run makes xargs exit non-zero;
# the logs are still summarised below.
status=0
printf '%s\0' "${jobs[@]}" | xargs -0 -n 1 -P "${JOBS:-12}" sh -c || status=$?
# Every scheduled run must have left a log that reports a pass and all its streams.
passed=0 failed=()
for i in "${!logs[@]}"; do
    l=${logs[i]}
    if [ -f "$l" ] && grep -q "test result: ok. 1 passed" "$l" &&
        [ "$(grep -c "cases (seed [0-9]*) match" "$l")" = "${streams[i]}" ]; then
        passed=$((passed + 1))
    else
        failed+=("$l")
    fi
done
# Total matching cases per stream and harness (a multi-word harness command prints as a list).
{ cat "${logs[@]}" 2>/dev/null || true; } | { grep -h "cases (seed" || true; } |
    sed -E 's/^(.*): ([0-9]+) cases \(seed [0-9]+\) match \[(.*)\]$/\1 \3 \2/; s/", "/ /g; s/"//g' |
    awk '{ c = $NF; NF--; n[$0] += c } END { for (k in n) printf "%12d  %s\n", n[k], k }' | sort -k2
if [ "$status" != 0 ] || [ "${#failed[@]}" != 0 ] || [ "$passed" != "${#jobs[@]}" ]; then
    echo "FAILED: $passed of ${#jobs[@]} runs passed (xargs status $status); missing or failing:" \
        "${failed[@]}" >&2
    exit 1
fi
echo "all $passed of ${#jobs[@]} scheduled runs passed; logs in $log"
