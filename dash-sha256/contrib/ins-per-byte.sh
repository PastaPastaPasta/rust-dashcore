#!/usr/bin/env bash
# User-space instructions per byte of each bench workload, as bench_dash's nanobench reports
# them: (instructions of N iterations - instructions of 0) / (N * bytes per iteration).
#
# Usage: contrib/ins-per-byte.sh [iterations-scale]
# Needs perf with access to the PMU (perf_event_paranoid <= 2).
set -euo pipefail

scale=${1:-1}
cd "$(dirname "$0")/.."
cargo build -q --release --example workload
bin=${CARGO_TARGET_DIR:-../target}/release/examples/workload

# Hybrid CPUs report one instructions line per core type (cpu_core/, cpu_atom/): sum them.
ins() { perf stat -x, -e instructions:u "$bin" "$@" 2>&1 >/dev/null | awk -F, '/instructions/ {n += $1} END {print n}'; }

printf '%-28s %12s\n' workload ins/byte
"$bin" --list | while read -r name bytes; do
    # About 200 MB of input per measurement, at least 1000 iterations.
    n=$(( (200000000 / bytes > 1000 ? 200000000 / bytes : 1000) * scale ))
    base=$(ins "$name" 0)
    total=$(ins "$name" "$n")
    printf '%-28s %12.2f\n' "$name" "$(echo "($total - $base) / ($n * $bytes)" | bc -l)"
done
