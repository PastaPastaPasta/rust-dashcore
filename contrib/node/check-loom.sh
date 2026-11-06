#!/usr/bin/env bash
# The node's loom models: every module each crate builds under `--cfg loom`, over production types
# with loom's atomics and locks swapped in:
# - dash-chain: SB4's seqlock model (`entry::loom_tests`, SB4 §13.2) and SB5's feed, hold, table,
#   status-feed and cold-cache models (SB5 §10.2, L1–L14);
# - dashd-chainstate: the signals queue (CH1-node §9.6, DEC-152), producers against a consumer
#   that loom preempts inside the queue's leaf section. Its `--cfg loom` build leaves out RocksDB
#   (dashd-chainstate's Cargo.toml), so it stays small; and the script check queue (CS6), helpers
#   draining the shared stack while the master adds and then joins.
# Each model fixes its own preemption bound; LOOM_MAX_PREEMPTIONS documents SB4's for loom's
# env-driven defaults. Release builds in their own target directory, so the `--cfg loom`
# RUSTFLAGS never invalidate the workspace build; each crate's run is bounded by LOOM_TIMEOUT
# seconds (default 1,200).
#
# A renamed or cfg'd-out model would pass with fewer tests run, so MODELS lists every loom module
# with its crate and test count: each must report exactly that many, and a module named `*loom*`
# in a crate's loom test list that MODELS lacks fails the check. A model counts when it passes, or
# when it is a negative control ignored here because a deadlock aborts the binary: its module's
# `*_negatives_fail` test runs it in a child process and checks that it ran and failed. Update
# MODELS with the models.
#
# Usage: contrib/node/check-loom.sh
set -euo pipefail

cd "$(dirname "$0")/../.."
target="${CARGO_TARGET_DIR:-target}/loom"
export RUSTFLAGS="--cfg loom" LOOM_MAX_PREEMPTIONS=2
MODELS=(
    "dash-chain cold::loom_tests 6"
    "dash-chain entry::loom_tests 1"
    "dash-chain feed::hold_loom 16"
    "dash-chain feed::loom_tests 8"
    "dash-chain feed::table_loom 11"
    "dash-chain status_feed::loom_tests 5"
    "dashd-chainstate chain::signals::fifo::loom_tests 1"
    "dashd-chainstate checkqueue::loom_tests 1"
)
log="$(mktemp)" list="$(mktemp)"
trap 'rm -f "$log" "$list"' EXIT

fail=0
for pkg in $(printf '%s\n' "${MODELS[@]}" | cut -d' ' -f1 | sort -u); do
    mine=$(printf '%s\n' "${MODELS[@]}" | sed -n "s/^$pkg //p")
    filters=()
    while read -r module _; do
        filters+=("$module::")
    done <<<"$mine"
    test=(cargo test -p "$pkg" --release --lib --target-dir "$target" --)
    timeout "${LOOM_TIMEOUT:-1200}" "${test[@]}" "${filters[@]}" --test-threads=1 2>&1 | tee "$log"

    while read -r module want; do
        got=$(grep -cE "^test ${module}::[A-Za-z0-9_]+ (- should panic )?\.\.\. (ok|ignored, .*)$" \
            "$log" || true)
        if [ "$got" != "$want" ]; then
            echo "check-loom.sh: $pkg $module reported $got of $want models" >&2
            fail=1
        fi
    done <<<"$mine"
    "${test[@]}" --list >"$list" 2>/dev/null
    listed=$(sed 's/ .*//' <<<"$mine")
    unlisted=$(sed -n 's/^\(.*\)::[A-Za-z0-9_]*: test$/\1/p' "$list" |
        grep -E '(^|::)[A-Za-z0-9_]*loom' | sort -u | grep -vxF "$listed" || true)
    if [ -n "$unlisted" ]; then
        echo "check-loom.sh: $pkg loom modules missing from MODELS: $unlisted" >&2
        fail=1
    fi
done
exit "$fail"
