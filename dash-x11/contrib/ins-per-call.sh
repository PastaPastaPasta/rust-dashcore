#!/usr/bin/env bash
# User-space instructions per call of each workload of examples/workload.rs:
# (instructions of N iterations - instructions of 0) / N, the method of SH1's ins-per-byte.sh.
#
# Usage: contrib/ins-per-call.sh [implementation] [iterations] [stage...]
# Needs perf with access to the PMU (perf_event_paranoid <= 2).
set -euo pipefail

imp=${1:-auto} n=${2:-20000}
shift $(( $# > 2 ? 2 : $# ))
stages=("$@")
[ ${#stages[@]} -gt 0 ] || stages=(x11 blake bmw groestl skein jh keccak luffa cubehash shavite simd echo)
cd "$(dirname "$0")/.."
cargo build -q --release --example workload
bin=${CARGO_TARGET_DIR:-../target}/release/examples/workload

# Hybrid CPUs report one instructions line per core type (cpu_core/, cpu_atom/): sum them.
ins() { perf stat -x, -e instructions:u "$bin" "$@" 2>&1 >/dev/null | awk -F, '/instructions/ {n += $1} END {print n}'; }

printf '%-10s %12s %10s\n' stage ins/call ins/byte
for stage in "${stages[@]}"; do
    base=$(ins "$imp" "$stage" 0)
    total=$(ins "$imp" "$stage" "$n")
    per=$(echo "($total - $base) / $n" | bc -l)
    bytes=64
    [ "$stage" = x11 ] || [ "$stage" = blake ] || [ "$stage" = self-test ] && bytes=80
    printf '%-10s %12.0f %10.2f\n' "$stage" "$per" "$(echo "$per / $bytes" | bc -l)"
done
