#!/usr/bin/env bash
# ST3b's device nightly (PLAN-ST3b §10): builds release rdashd and st3b-crash at the checkout's
# HEAD, sweeps stale runs, then runs each component alone and records it.
#
# usage: contrib/node/crash/nightly-device.sh OUT_DIR [COMPONENT...]
#   With no components, runs every one. A component is <scenario>:<fs>, the scenarios being
#   selftest (the device stack on a known file workload) and d-ibd-s (W-small's IBD over P2P and
#   submitblock at -dbcache=4 -dbbatchsize=20000: the night's shard of its flush, FUA, random and
#   torn points, C0-C6), the filesystems ext4 and xfs. OUT_DIR gets head.txt, build.log, a log and a time
#   file per component, and summary.txt: the seed, one row per component, then DONE. Each row is
#   also appended to $ST3B_LEDGER: UTC, SHA, component, fs, points, points by class, failures,
#   CPU-s (user + system), seed, status.
#
# The exit status is non-zero if the build fails, any component fails (a component that checks no
# point fails), a run leaves a device or mount behind, or a required artifact cannot be written.
# Every component runs regardless. A failed run keeps its directory under /work/scratch/st3b/run
# for the repro; the newest $ST3B_KEEP_FAILED are kept, older ones removed through $ST3B_RM.
#
# env:
#   ST3B_SEED        the points' seed; default: the UTC date (YYYYMMDD), so it rotates nightly.
#   ST3B_SHARD       the d-ibd-s shard I/N (DEC-718); default: (UTC day mod 24)/24, so every point
#                    index is checked once in 24 nights at <= 2 CPU-h a night. 0/1: every point.
#   ST3B_DASHD       the fixtures' dashd (default: the pinned v24.0.0-rc.4 under ~/.rust-dashcore-test)
#   ST3B_FIXTURES    the fixture directory (default: /work/scratch/st3b/fixtures)
#   ST3B_BIN_DIR     skip the build and run rdashd and st3b-crash from this directory
#   ST3B_DRY_RUN=1   no devices, no sudo: campaigns run with --dry-run, selftests are skipped.
#                    For the runner's own test; a dry night is never a passing night's evidence.
#   ST3B_LEDGER      default: /work/scratch/st3b/nightly/ledger.tsv
#   ST3B_KEEP_FAILED default 3
#   ST3B_RM          default: /work/scratch/mgr/rm-if-idle.sh (one directory per call)
set -u

COMPONENTS=(selftest:ext4 selftest:xfs d-ibd-s:ext4 d-ibd-s:xfs)
RUNS=/work/scratch/st3b/run

if [ $# -lt 1 ]; then
  sed -n '2,30p' "$0" >&2
  exit 2
fi
OUT=$(realpath -m "$1")
shift
[ $# -eq 0 ] && set -- "${COMPONENTS[@]}"
mkdir -p "$OUT" || exit 2
SUMMARY=$OUT/summary.txt
LEDGER=${ST3B_LEDGER:-/work/scratch/st3b/nightly/ledger.tsv}
SEED=${ST3B_SEED:-$(date -u +%Y%m%d)}
SHARD=${ST3B_SHARD:-$(($(date -u +%s) / 86400 % 24))/24}
DASHD=${ST3B_DASHD:-$HOME/.rust-dashcore-test/dashcore-24.0.0-rc.4/bin/dashd}
FIXTURES=${ST3B_FIXTURES:-/work/scratch/st3b/fixtures}
DRY=${ST3B_DRY_RUN:-0}
export TMPDIR=${TMPDIR:-/work/scratch/st3b/tmp}

# Every write of a required artifact goes through here: a report the runner could not write
# fails the run, though the components still run.
unwritten=0
report() {
  if ! printf '%s\n' "$*" >> "$SUMMARY"; then
    echo "nightly-device: cannot write $SUMMARY: $*" >&2
    unwritten=$((unwritten + 1))
  fi
}
if ! : > "$SUMMARY"; then
  echo "nightly-device: cannot create $SUMMARY" >&2
  unwritten=$((unwritten + 1))
fi
report "seed ST3B_SEED=$SEED shard ST3B_SHARD=$SHARD dry=$DRY"

ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
SHA=$(git -C "$ROOT" rev-parse HEAD 2> /dev/null || echo unknown)
if ! echo "$SHA" > "$OUT/head.txt"; then
  unwritten=$((unwritten + 1))
fi
if [ -n "${ST3B_BIN_DIR:-}" ]; then
  BIN=$(realpath -m "$ST3B_BIN_DIR")
else
  if ! (cd "$ROOT" && cargo build --release -p rdashd -p dash-testkit --features dash-testkit/crash \
      --bin rdashd --bin st3b-crash > "$OUT/build.log" 2>&1); then
    report "BUILD FAILED (build.log)"
    exit 1
  fi
  BIN=$ROOT/target/release
fi
CRASH=$BIN/st3b-crash
for b in "$CRASH" "$BIN/rdashd"; do
  if [ ! -x "$b" ]; then
    report "NO BINARY $b"
    exit 1
  fi
done
export ST3B_PRIV=$ROOT/contrib/node/crash/st3b-priv

failed=0
# A run killed on an earlier night leaves its devices: tear them down before anything starts.
if [ "$DRY" != 1 ] && ! "$CRASH" sweep --stale > "$OUT/sweep.log" 2>&1; then
  report "SWEEP FAILED (sweep.log)"
  failed=$((failed + 1))
fi

for c in "$@"; do
  scenario=${c%%:*}
  fs=${c#*:}
  log=$OUT/$c.log
  args=()
  case $scenario in
    selftest) args=(selftest --fs "$fs" --seed "$SEED") ;;
    d-ibd-s)
      args=(campaign "$FIXTURES/w-small" --rdashd "$BIN/rdashd" --dashd "$DASHD" --fs "$fs"
        --seed "$SEED" --shard "$SHARD")
      [ "$DRY" = 1 ] && args+=(--dry-run)
      args+=(-- -dbcache=4 -dbbatchsize=20000)
      ;;
    *)
      report "- - - $c status=UNKNOWN"
      failed=$((failed + 1))
      continue
      ;;
  esac
  if [ "$DRY" = 1 ] && [ "$scenario" = selftest ]; then
    report "- - - $c status=SKIPPED-DRY"
    continue
  fi
  # A reused OUT_DIR's old times must not stand for this run's.
  rm -f "$OUT/$c.time"
  /usr/bin/time -f "%U %S %e" -o "$OUT/$c.time" "$CRASH" "${args[@]}" > "$log" 2>&1
  rc=$?
  times=$(tail -1 "$OUT/$c.time" 2> /dev/null)
  points=$(grep -oE 'points [0-9]+' "$log" | tail -1 | cut -d' ' -f2)
  kinds=$(grep -oE 'points [0-9]+ \{[^}]*\}' "$log" | tail -1 | sed 's/^points [0-9]* //; s/[" ]//g')
  fails=$(grep -c '^FAIL ' "$log")
  status=ok
  if [ $rc -ne 0 ]; then
    status=FAILED
  elif [ "${points:-0}" -eq 0 ]; then
    status=NO-POINTS
  elif [ -z "$times" ]; then
    status=NO-TIME
  fi
  [ $status = ok ] || failed=$((failed + 1))
  report "${times:-- - -} $c exit=$rc points=${points:-0} failures=$fails status=$status"
  cpu=$(echo "${times:-0 0 0}" | awk '{printf "%.0f", $1 + $2}')
  if ! printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$(date -u +%FT%TZ)" "$SHA" \
      "$scenario" "$fs" "${points:-0}" "${kinds:--}" "$fails" "$cpu" "$SEED" \
      "$status$([ "$DRY" = 1 ] && echo -dry)" >> "$LEDGER"; then
    echo "nightly-device: cannot append to $LEDGER" >&2
    unwritten=$((unwritten + 1))
  fi
  kept=$(grep -oE "^kept $RUNS/[0-9]+-[0-9]+" "$log" | cut -d' ' -f2)
  [ -n "$kept" ] && echo "$kept" >> "$(dirname "$LEDGER")/kept.txt"
done

# Nothing of the night's runs may be left, whatever a component reported. Only the runs its
# components printed (`run <id>`): another thread's live campaign is not this night's leak.
if [ "$DRY" != 1 ]; then
  : > "$OUT/leaks.log"
  leaked=0
  for c in "$@"; do
    while read -r _ id; do
      "$CRASH" leaks "$id" >> "$OUT/leaks.log" 2>&1 || leaked=1
    done < <(grep -oE '^run [0-9]+-[0-9]+' "$OUT/$c.log" 2> /dev/null)
  done
  if [ $leaked = 1 ]; then
    report "LEAKS (leaks.log)"
    failed=$((failed + 1))
  fi
fi

# The newest failed runs stay for their repros; older ones go, each a directory of ours.
KEPT=$(dirname "$LEDGER")/kept.txt
if [ -f "$KEPT" ]; then
  keep=${ST3B_KEEP_FAILED:-3}
  total=$(wc -l < "$KEPT")
  if [ "$total" -gt "$keep" ]; then
    # A run that still has devices or mounts stays, and stays listed: removing its directory
    # would leave an orphan the sweep refuses (rm-if-idle sees processes, not loop devices).
    : > "$KEPT.held"
    head -n $((total - keep)) "$KEPT" | while read -r d; do
      if [[ $d =~ ^$RUNS/[0-9]+-[0-9]+$ ]] && [ -d "$d" ]; then
        if [ "$DRY" != 1 ] && ! "$CRASH" leaks "${d##*/}" >> "$OUT/prune.log" 2>&1; then
          echo "kept $d: it still has devices or mounts" >> "$OUT/prune.log"
          echo "$d" >> "$KEPT.held"
          continue
        fi
        "${ST3B_RM:-/work/scratch/mgr/rm-if-idle.sh}" "$d" >> "$OUT/prune.log" 2>&1
      fi
    done
    { cat "$KEPT.held"; tail -n "$keep" "$KEPT"; } > "$KEPT.new" && mv "$KEPT.new" "$KEPT"
    rm -f -- "$KEPT.held"
  fi
fi

report "DONE failed=$failed unwritten=$unwritten"
[ $failed -eq 0 ] && [ $unwritten -eq 0 ]
