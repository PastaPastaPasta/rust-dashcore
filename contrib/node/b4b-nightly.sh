#!/usr/bin/env bash
# CH3b b4b's nightly: the -dbcrashratio reorg loop (rdashd/tests/dbcrash.rs, ignored tests). It
# builds the dbcrash test binary at the checkout's HEAD and runs each component alone at nice 10,
# recording its CPU time: reorg_loop_split and reorg_loop_pressure (rdashd crashing) and
# reorg_loop_on_dashd (dashd crashing, both profiles), each against an uncrashed dashd
# v24.0.0-rc.5 (DASHD_V24_PATH or contrib/setup-dashd.py's cache).
#
# usage: contrib/node/b4b-nightly.sh OUT_DIR [COMPONENT...]
#   With no components, runs every one. OUT_DIR gets head.txt, build.log, a log and a time file
#   per component, and summary.txt: the seed, then one row per component (user, system and wall
#   seconds, the name, the exit status, then each profile's B4B-SUMMARY counters), then DONE.
#   head.txt holds the checkout's revision, or "unknown". A failed component's home (every
#   node's debug.log, the seed and ops.log) is kept under OUT_DIR/retained; its log names it.
#   Each component runs in its own session: one that outlives the limit is killed with every
#   node it started, and its status is TIMEOUT.
#
# The exit status is non-zero if the build fails, any component fails, including one that runs
# no test (a stale or misspelled name under --exact), or a required artifact (head.txt, a log, a
# time file, summary.txt) cannot be written. Every component runs regardless.
#
# env:
#   B4B_SEED             the script's seed; default: the UTC date (YYYYMMDD), so the script
#                        rotates nightly. summary.txt records it; set it to replay a night's run.
#   B4B_NIGHTLY_BIN      skip the build and run this dbcrash test binary.
set -u

COMPONENTS=(reorg_loop_split reorg_loop_pressure reorg_loop_on_dashd)
# A component that runs longer is killed and fails.
LIMIT=3600

if [ $# -lt 1 ]; then
  sed -n '2,24p' "$0" >&2
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
note_unwritten() {
  echo "b4b-nightly: cannot write $*" >&2
  unwritten=$((unwritten + 1))
}
report() {
  printf '%s\n' "$*" >> "$SUMMARY" || note_unwritten "$SUMMARY: $*"
}
: > "$SUMMARY" || note_unwritten "$SUMMARY"

export B4B_SEED=${B4B_SEED:-$(date -u +%Y%m%d)}
# A failed run keeps its home here (rdashd/tests/common's RetainedHome).
export DASHD_TEST_RETAIN_DIR=$OUT/retained
report "seed B4B_SEED=$B4B_SEED"

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
{ git -C "$ROOT" rev-parse HEAD 2> /dev/null || echo unknown; } > "$OUT/head.txt" \
  || note_unwritten "$OUT/head.txt"
# A new session per component (util-linux setsid, or Python's where it is missing): its process
# group holds the test binary and every node it starts.
if command -v setsid > /dev/null; then
  SESSION=(setsid)
else
  SESSION=(python3 -c 'import os, sys; os.setsid(); os.execvp(sys.argv[1], sys.argv[1:])')
fi
if [ -n "${B4B_NIGHTLY_BIN:-}" ]; then
  BIN=$(realpath -m "$B4B_NIGHTLY_BIN")
else
  # The executable this build produced (not the newest file in target/).
  if ! (cd "$ROOT" && cargo test --locked -p rdashd --test dbcrash --no-run \
      --message-format=json 2> "$OUT/build.log" > "$OUT/build.json"); then
    report "BUILD FAILED (build.log)"
    exit 1
  fi
  BIN=$(jq -r 'select(.reason == "compiler-artifact" and .profile.test and .executable
                      and .target.name == "dbcrash") | .executable' "$OUT/build.json")
fi

usable=0
[ -n "$BIN" ] && [ -x "$BIN" ] && usable=1
failed=0
for c in "$@"; do
  log=$OUT/$c.log
  if [ $usable -eq 0 ]; then
    report "- - - $c status=NO-BINARY"
    failed=$((failed + 1))
    continue
  fi
  # A reused OUT_DIR's old times must not stand for this run's.
  rm -f "$OUT/$c.time"
  # In the background so its pid, which leads the new session, is known. A non-interactive
  # shell's background job is no group leader, so setsid takes the pid itself and does not fork.
  # --foreground keeps timeout, and so the binary and its nodes, in that group: without it,
  # timeout moves into a group of its own, which the kill below would miss.
  (cd "$ROOT/rdashd" && exec "${SESSION[@]}" /usr/bin/time -f "%U %S %e" -o "$OUT/$c.time" \
    timeout --foreground -k 60 $LIMIT nice -n 10 "$BIN" --exact "$c" --ignored --nocapture \
    > "$log" 2>&1) &
  pid=$!
  wait $pid
  rc=$?
  status=ok
  times=$(tail -1 "$OUT/$c.time" 2> /dev/null)
  if [ $rc -eq 124 ] || [ $rc -eq 137 ]; then
    # The limit: the binary was signalled and did not unwind, so its nodes outlive it; the
    # session's group holds them.
    kill -KILL -- "-$pid" 2> /dev/null
    status=TIMEOUT
  elif [ $rc -ne 0 ]; then
    status=FAILED
  elif ! grep -Eq '^test result: ok\. [1-9][0-9]* passed' "$log" 2> /dev/null; then
    status=NO-TEST
  elif [ -z "$times" ]; then
    status=NO-TIME
  fi
  [ $status = ok ] || failed=$((failed + 1))
  report "${times:-- - -} $c exit=$rc status=$status"
  while read -r line; do
    report "  ${line#B4B-SUMMARY }"
  done < <(grep '^B4B-SUMMARY ' "$log" 2> /dev/null)
done
report "DONE failed=$failed unwritten=$unwritten"
[ $failed -eq 0 ] && [ $unwritten -eq 0 ]
