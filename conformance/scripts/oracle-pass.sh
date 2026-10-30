#!/usr/bin/env bash
# One incremental conformance pass over the shared oracles (docs/node-program/ORACLE.md):
# RPC mode and blk-file mode for testnet and mainnet, resuming where the last pass stopped.
# Runs at idle CPU and I/O priority so it does not disturb benchmarks on the same disk.
#
# Usage: conformance/scripts/oracle-pass.sh [harness-binary] [out-root]
#   defaults: /work/conformance/bin/dash-conformance, /work/conformance
# Appends one line per network and mode to <out-root>/passes.log, with the run's exit status
# (0 = the whole chain is covered and clean, 1 = failing mismatches, 2 = not fully covered or
# an error) and its gate line. Checkpoints made by another build of the harness or libraries,
# or invalidated by a reorg, are re-checked automatically.
set -uo pipefail

bin="${1:-/work/conformance/bin/dash-conformance}"
root="${2:-/work/conformance}"
cli="$(dirname "$0")/../../contrib/oracle-cli"
[ -x "$cli" ] || cli=/work/conformance/bin/oracle-cli
run() { nice -n 19 ionice -c 3 "$bin" "$@" --resume --fail-on-mismatch --threads 8 2>&1 | grep -vE '^\['; }

for net in testnet mainnet; do
    blocks=/work/chains/$net/data/blocks
    [ "$net" = testnet ] && blocks=/work/chains/testnet/data/testnet3/blocks
    tip=$("$cli" "$net" getblockcount 2> /dev/null || echo "?")
    for mode in rpc blk; do
        out="$root/$net-$mode"
        if [ "$mode" = rpc ]; then
            log=$(run rpc --net "$net" --out "$out")
        else
            log=$(run blk --net "$net" --blocks-dir "$blocks" --include-stale --out "$out")
        fi
        status=$?
        summary=$(echo "$log" | grep -E '^all runs' | sed 's/^all runs in [^:]*: //')
        gate=$(echo "$log" | grep -E '^gate:')
        echo "$(date -u +%FT%TZ) $net $mode oracle-tip=$tip exit=$status $summary; $gate" >> "$root/passes.log"
    done
done
