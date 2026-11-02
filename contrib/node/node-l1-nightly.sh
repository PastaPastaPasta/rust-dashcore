#!/usr/bin/env bash
# node-l1's nightly (BL-548, DEC-905): builds node-l1's oracle test binaries at the checkout's HEAD
# and runs each nightly component (an ignored test) alone at nice 10, recording its CPU time.
#
# usage: contrib/node/node-l1-nightly.sh OUT_DIR [COMPONENT...]
#   With no components, runs every one. OUT_DIR gets head.txt, build.log, a log and a time file
#   per component, and summary.txt: the seed, then one row per component, then DONE.
#   pairs_sweep_every_argument_d19 runs D19's ordered pairs with every argument pair below 16
#   (CI's pairs_first_reject_d19 runs four) from a seeded start, until its wall-clock limit.
#   node-l1's other ignored tests need inputs or hands a nightly has not got; they are not here.
#
# The exit status is non-zero if the build fails, any component fails, including one that runs
# no test (a stale or misspelled name under --exact), or a required artifact (head.txt, a log, a
# time file, summary.txt) cannot be written. Every component runs regardless.
#
# env:
#   L1_SWEEP_SEED        the sweep's seed; default: the UTC date (YYYYMMDD), so its start rotates
#                        nightly. summary.txt records it; set it to replay a night's sweep.
#   L1_SWEEP_SECONDS     the sweep's wall-clock limit; default 3000. A component that overruns
#                        it by 600 s is killed and fails.
#   L1_NIGHTLY_BIN_DIR   skip the build and run the binaries in this directory, named by test
#                        target (l1_specialtx).
set -u

COMPONENTS=(pairs_sweep_every_argument_d19)

# The test target each component is in.
target() {
  case $1 in
    *) echo l1_specialtx ;;
  esac
}

if [ $# -lt 1 ]; then
  sed -n '2,22p' "$0" >&2
  exit 2
fi
OUT=$(realpath -m "$1")
shift
[ $# -eq 0 ] && set -- "${COMPONENTS[@]}"
mkdir -p "$OUT" || exit 2
SUMMARY=$OUT/summary.txt
# Every write of a required artifact goes through here: a report the runner could not write
# fails the run, though the components still run.
unwritten=0
report() {
  if ! printf '%s\n' "$*" >> "$SUMMARY"; then
    echo "node-l1-nightly: cannot write $SUMMARY: $*" >&2
    unwritten=$((unwritten + 1))
  fi
}
if ! : > "$SUMMARY"; then
  echo "node-l1-nightly: cannot create $SUMMARY" >&2
  unwritten=$((unwritten + 1))
fi

export L1_SWEEP_SEED=${L1_SWEEP_SEED:-$(date -u +%Y%m%d)}
export L1_SWEEP_SECONDS=${L1_SWEEP_SECONDS:-3000}
report "seed L1_SWEEP_SEED=$L1_SWEEP_SEED limit L1_SWEEP_SECONDS=$L1_SWEEP_SECONDS"

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
declare -A BIN
if [ -n "${L1_NIGHTLY_BIN_DIR:-}" ]; then
  BIN[l1_specialtx]=$(realpath -m "$L1_NIGHTLY_BIN_DIR/l1_specialtx")
else
  if ! git -C "$ROOT" rev-parse HEAD > "$OUT/head.txt"; then
    echo "node-l1-nightly: cannot write $OUT/head.txt" >&2
    unwritten=$((unwritten + 1))
  fi
  # The executables this build produced (not the newest file in target/).
  if ! (cd "$ROOT" && cargo test -p node-l1 --features oracle --test l1_specialtx --no-run \
      --message-format=json 2> "$OUT/build.log" > "$OUT/build.json"); then
    report "BUILD FAILED (build.log)"
    exit 1
  fi
  while read -r name exe; do
    BIN[$name]=$exe
  done < <(jq -r 'select(.reason == "compiler-artifact" and .profile.test and .executable)
                  | "\(.target.name) \(.executable)"' "$OUT/build.json")
fi

failed=0
for c in "$@"; do
  bin=${BIN[$(target "$c")]:-}
  log=$OUT/$c.log
  if [ -z "$bin" ] || [ ! -x "$bin" ]; then
    report "- - - $c status=NO-BINARY"
    failed=$((failed + 1))
    continue
  fi
  # A reused OUT_DIR's old times must not stand for this run's.
  rm -f "$OUT/$c.time"
  (cd "$ROOT/node-l1" && /usr/bin/time -f "%U %S %e" -o "$OUT/$c.time" \
    timeout -k 60 $((L1_SWEEP_SECONDS + 600)) nice -n 10 \
    "$bin" --exact "$c" --ignored --nocapture > "$log" 2>&1)
  rc=$?
  status=ok
  times=$(tail -1 "$OUT/$c.time" 2> /dev/null)
  if [ $rc -ne 0 ]; then
    status=FAILED
  elif ! grep -Eq '^test result: ok\. [1-9][0-9]* passed' "$log" 2> /dev/null; then
    status=NO-TEST
  elif [ -z "$times" ]; then
    status=NO-TIME
  fi
  [ $status = ok ] || failed=$((failed + 1))
  report "${times:-- - -} $c exit=$rc status=$status"
done
report "DONE failed=$failed unwritten=$unwritten"
[ $failed -eq 0 ] && [ $unwritten -eq 0 ]
