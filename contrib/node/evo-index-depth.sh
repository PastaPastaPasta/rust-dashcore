#!/usr/bin/env bash
# CH7 §7.1's `evo-index-depth` growth guard: user-space instructions per round of
# dashd-chainstate/benches/evo_index_depth.rs's three reads, (instructions of R rounds - of 0) / R,
# each the median of 3 fresh processes, at 1,000 and 100,000 mined entries. Passes when the larger
# window costs at most 2 x the smaller.
#
# Usage: contrib/node/evo-index-depth.sh [rounds]
# Needs perf with access to the PMU (perf_event_paranoid <= 2).
set -euo pipefail

rounds=${1:-20000}
cd "$(dirname "$0")/../.."
bin=$(cargo bench -q -p dashd-chainstate --bench evo_index_depth --no-run --message-format=json |
    python3 -c 'import json, sys
for line in sys.stdin:
    m = json.loads(line)
    if m.get("reason") == "compiler-artifact" and m["target"]["name"] == "evo_index_depth" and m["executable"]:
        print(m["executable"])')

# Hybrid CPUs report one instructions line per core type (cpu_core/, cpu_atom/): sum them.
ins() { perf stat -x, -e instructions:u "$bin" "$@" 2>&1 >/dev/null | awk -F, '/instructions/ {n += $1} END {print n}'; }
median3() { printf '%s\n' "$@" | sort -n | sed -n 2p; }
per_round() {
    local base total
    base=$(median3 "$(ins "$1" 0)" "$(ins "$1" 0)" "$(ins "$1" 0)")
    total=$(median3 "$(ins "$1" "$rounds")" "$(ins "$1" "$rounds")" "$(ins "$1" "$rounds")")
    echo "($total - $base) / $rounds" | bc -l
}

small=$(per_round 1000)
large=$(per_round 100000)
ratio=$(echo "$large / $small" | bc -l)
printf 'entries %7d: %8.0f instructions:u per round\n' 1000 "$small" 100000 "$large"
printf 'ratio %.3f (pass at <= 2)\n' "$ratio"
[ "$(echo "$ratio <= 2" | bc -l)" = 1 ] || { echo FAIL; exit 1; }
echo PASS
