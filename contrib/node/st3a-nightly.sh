#!/usr/bin/env bash
# ST3a's nightly (design §8.4): builds dashd-store's release test binaries at the checkout's HEAD
# and runs each nightly component (an ignored test) alone, recording its CPU time.
#
# usage: contrib/node/st3a-nightly.sh OUT_DIR [COMPONENT...]
#   With no components, runs every one. OUT_DIR gets head.txt, build.log, a log and a time file
#   per component, and summary.txt: the seed, then one row per component, then DONE.
#   nested_depth1_extended is enumerated in shards by contrib/node/st3a-shards.sh (DEC-185 (2))
#   into OUT_DIR/nested_depth1_extended.shards, whose summary.txt and ledger.tsv carry the
#   verdict; its time file has the runner's wall time only (the shards run in their own units).
#
# The exit status is non-zero if the build fails, any component fails, including one that runs
# no test (a stale or misspelled name under --exact), or a required artifact (head.txt, a log, a
# time file, summary.txt) cannot be written. Every component runs regardless.
#
# env:
#   ST3A_SWEEP_SEED      the sweep's seed; default: the UTC date (YYYYMMDD), so it rotates
#                        nightly. summary.txt records it; set it to replay a night's sweep.
#   ST3A_NIGHTLY_BIN_DIR skip the build and run the binaries in this directory, named by test
#                        target (power_loss_deep, power_loss_model). For the runner's own test.
#   ST3A_SHARD_*         passed to st3a-shards.sh (ST3A_SHARD_JOBS, ST3A_SHARD_DEPTH,
#                        ST3A_SHARDS_INLINE).
set -u

COMPONENTS=(
  clean_matrix production_scale_prune_and_locks nested_depth1 nested_depth1_extended
  nested_depth2_sample sweep spv_sweep
  regenerate_toy_repros witnesses_with_large_records_extended_byte
  s11d_nested_at_production_d_max m10_lazy_equals_eager_at_scale s5b_clean s5c_clean s11r_clean
  s13_clean s14b_clean s11c_clean
)

# The test target each component is in.
target() {
  case $1 in
    m10_lazy_equals_eager_at_scale) echo power_loss_model ;;
    *) echo power_loss_deep ;;
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
    echo "st3a-nightly: cannot write $SUMMARY: $*" >&2
    unwritten=$((unwritten + 1))
  fi
}
if ! : > "$SUMMARY"; then
  echo "st3a-nightly: cannot create $SUMMARY" >&2
  unwritten=$((unwritten + 1))
fi

export ST3A_SWEEP_SEED=${ST3A_SWEEP_SEED:-$(date -u +%Y%m%d)}
report "seed ST3A_SWEEP_SEED=$ST3A_SWEEP_SEED"

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
declare -A BIN
if [ -n "${ST3A_NIGHTLY_BIN_DIR:-}" ]; then
  for t in power_loss_deep power_loss_model; do
    BIN[$t]=$(realpath -m "$ST3A_NIGHTLY_BIN_DIR/$t")
  done
else
  if ! git -C "$ROOT" rev-parse HEAD > "$OUT/head.txt"; then
    echo "st3a-nightly: cannot write $OUT/head.txt" >&2
    unwritten=$((unwritten + 1))
  fi
  # The executables this build produced (not the newest file in target/).
  if ! (cd "$ROOT" && cargo test --release -p dashd-store --no-default-features \
      --features faulty --test power_loss_deep --test power_loss_model --no-run \
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
  if [ "$c" = nested_depth1_extended ]; then
    # The shard runner's own verdict: every planned shard ok at this head.
    /usr/bin/time -f "%U %S %e" -o "$OUT/$c.time" env ST3A_SHARDS_BIN="$bin" \
      ST3A_SHARDS_HEAD="$(cat "$OUT/head.txt" 2> /dev/null)" \
      "$ROOT/contrib/node/st3a-shards.sh" "$OUT/$c.shards" > "$log" 2>&1
    rc=$?
    pass='^DONE .* failed=0 unwritten=0$'
    passlog=$OUT/$c.shards/summary.txt
  else
    (cd "$ROOT/dashd-store" && /usr/bin/time -f "%U %S %e" -o "$OUT/$c.time" \
      "$bin" --exact "$c" --ignored --nocapture > "$log" 2>&1)
    rc=$?
    pass='^test result: ok\. [1-9][0-9]* passed'
    passlog=$log
  fi
  status=ok
  times=$(tail -1 "$OUT/$c.time" 2> /dev/null)
  if [ $rc -ne 0 ]; then
    status=FAILED
  elif ! grep -Eq "$pass" "$passlog" 2> /dev/null; then
    status=NO-TEST
  elif [ -z "$times" ]; then
    status=NO-TIME
  fi
  [ $status = ok ] || failed=$((failed + 1))
  report "${times:-- - -} $c exit=$rc status=$status"
done
report "DONE failed=$failed unwritten=$unwritten"
[ $failed -eq 0 ] && [ $unwritten -eq 0 ]
