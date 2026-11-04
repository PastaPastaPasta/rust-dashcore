#!/usr/bin/env bash
# Mixed mode A's nightly (O12a; A1 §4.8): builds rdashd and rdash-cli at the checkout's HEAD, runs
# every DashTestFramework test with node 0 and the masternodes on dashd and the other nodes on
# rdashd, and diffs the result against the committed contrib/node/func-compat/mixed-a.json; then
# the same with an rdashd observer connected to node 0 (--observer), against mixed-a-obs.json.
#
# usage: contrib/node/func-compat/mixed-a-nightly.sh OUT_DIR
#   OUT_DIR gets head.txt, build.log, and per variant (mixed-a, mixed-a-obs) a log (the diff is at
#   its end), the runner logs, the regenerated <variant>-v24.0.json and <variant>-v24.0.run.json
#   (Core commit, reruns, confirmed and unconfirmed changes, dashd_only_failures, and
#   possible_crashes: tests whose output or node stderr suggests an rdashd crash, their datadirs
#   kept in <variant>/possible-crashes; triage those privately).
#
# Exit status: 0 when both runs match their committed passlists; 1 when one differs or an
# adaptation is stale (func-compat.py adaptations); 2 on a usage, build or harness error in
# either. func-compat.py writes <variant>-v24.0.json last, atomically, so a variant without it
# did not finish, whatever its exit status. To accept a difference, run
# `func-compat.py mixed-a --pin v24.0 [--observer] --update` and commit the passlist.
#
# env:
#   MIXED_A_JOBS       test_runner.py jobs; default 2. Each DTF test runs up to 11 nodes.
#   MIXED_A_CORE       the Dash Core repository; default ~/workspace/dash
#   MIXED_A_CORE_BRANCH  its branch to export; default func-compat.py's checked-in
#                      MIXED_A_CORE_BRANCH (node/o12a-mixed-a). A tree without mode A's hooks
#                      fails the variant (exit 2).
#   MIXED_A_WORK       where the test tree is exported; default func-compat.py's ($TMPDIR/func-compat)
#   MIXED_A_TMP        root of the test datadirs; default /dev/shm/mixed-a-$USER (mode 700). Each
#                      run uses a fresh directory below it and removes it.
#   MIXED_A_BIN_DIR    skip the build and run the rdashd and rdash-cli in this directory
set -u

if [ $# -ne 1 ]; then
  sed -n '2,/^[^#]/{/^#/p}' "$0" >&2
  exit 2
fi
HERE=$(cd "$(dirname "$0")" && pwd)
RDC=$(cd "$HERE/../../.." && pwd)
OUT=$(realpath -m "$1")
mkdir -p "$OUT" || exit 2
git -C "$RDC" rev-parse HEAD > "$OUT/head.txt" || exit 2

if [ -n "${MIXED_A_BIN_DIR:-}" ]; then
  BIN=$(realpath -m "$MIXED_A_BIN_DIR")
  echo "mixed-a-nightly: not built; rdashd and rdash-cli from $BIN (MIXED_A_BIN_DIR)" > "$OUT/build.log"
else
  if ! (cd "$RDC" && cargo build -p rdashd -p rdash-cli) > "$OUT/build.log" 2>&1; then
    echo "mixed-a-nightly: build failed; see $OUT/build.log" >&2
    exit 2
  fi
  # cargo's own answer: CARGO_TARGET_DIR, build.target-dir, or the workspace's target/.
  BIN=$(cd "$RDC" && cargo metadata --format-version 1 --no-deps |
    python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')/debug || exit 2
fi

worst=0
for variant in mixed-a mixed-a-obs; do
  observer=()
  [ "$variant" = mixed-a-obs ] && observer=(--observer)
  dir=$OUT/$variant
  mkdir -p "$dir" || exit 2
  result=$dir/$variant-v24.0.json
  rm -f "$result" "$dir/$variant-v24.0.run.json"  # a previous night's
  python3 -u "$HERE/func-compat.py" mixed-a --pin v24.0 ${observer[@]+"${observer[@]}"} \
    --core "${MIXED_A_CORE:-$HOME/workspace/dash}" ${MIXED_A_CORE_BRANCH:+--branch "$MIXED_A_CORE_BRANCH"} \
    ${MIXED_A_WORK:+--work "$MIXED_A_WORK"} --out "$dir" \
    --tmpdirprefix "${MIXED_A_TMP:-/dev/shm/mixed-a-$USER}" \
    --rdashd "$BIN/rdashd" --rdash-cli "$BIN/rdash-cli" \
    --jobs "${MIXED_A_JOBS:-2}" > "$OUT/$variant.log" 2>&1
  status=$?
  tail -n 20 "$OUT/$variant.log"
  # func-compat.py also exits 1 on a harness error; only a run that wrote its result finished.
  if [ $status -gt 1 ] || [ ! -s "$result" ]; then
    echo "mixed-a-nightly: $variant did not finish (exit $status); see $OUT/$variant.log" >&2
    status=2
  fi
  if [ $status -gt $worst ]; then worst=$status; fi
done
exit $worst
