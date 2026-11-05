#!/usr/bin/env bash
# CH5-5's nightly (docs/node-program/designs/CH5-notes.md §6.7, supplementary): a copy of a live
# test-hooks rdashd under a dashd peer's relay, through the reopen oracle and the §2.2 counter
# check (nightly_copy_under_relay, asserted), and the report-only variant where blocks arrive
# during the copy (nightly_report_only_blocks_arrive, deltas and the oracle's outcome logged).
#
# usage: contrib/node/livecopy/nightly.sh OUT_DIR
#   OUT_DIR gets head.txt, a log per component and summary.txt (one row per component, then
#   DONE). Needs dashd v24 (contrib/setup-dashd.py, run here). The exit status is non-zero if
#   the build or the asserted component fails, or a component runs no test.
set -u

COMPONENTS=(nightly_copy_under_relay nightly_report_only_blocks_arrive)

out=${1:?usage: nightly.sh OUT_DIR}
mkdir -p "$out" || exit 1
cd "$(dirname "$0")/../../.." || exit 1
git rev-parse HEAD > "$out/head.txt" || exit 1
eval "$(python3 contrib/setup-dashd.py)" || exit 1
ulimit -n 65536 2>/dev/null || true

status=0
: > "$out/summary.txt"
for c in "${COMPONENTS[@]}"; do
  cargo test -p rdashd --features test-hooks --test live_copy -- --ignored --exact "$c" \
    --nocapture > "$out/$c.log" 2>&1
  rc=$?
  # A component that runs no test (a stale or misspelled name under --exact) fails too.
  if [ "$rc" -eq 0 ] && ! grep -q "^test result: ok. 1 passed" "$out/$c.log"; then
    rc=1
  fi
  [ "$rc" -ne 0 ] && status=1
  echo "$c rc=$rc $(grep -h 'CH5-5 nightly' "$out/$c.log" | head -1)" >> "$out/summary.txt"
done
echo DONE >> "$out/summary.txt"
exit $status
