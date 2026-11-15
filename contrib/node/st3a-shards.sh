#!/usr/bin/env bash
# ST3a's sharded exhaustive enumeration of nested_depth1_extended (DEC-185 (2)): plans the shards
# at the checkout's HEAD, runs each uncapped as its own low-priority systemd user unit, and keeps
# a ledger of shard completion per head. The component is clean only when every planned shard
# finished with 0 findings at this head.
#
# usage: contrib/node/st3a-shards.sh OUT_DIR
#   OUT_DIR gets head.txt, build.log, plan.txt (the depth, then one `<component>:<shard>` per
#   line), a log and a time file per shard under shards/, ledger.tsv and summary.txt.
#   ledger.tsv is append-only, one row per finished shard run:
#     head  shard  status  user-s  sys-s  wall-s  result
#   A rerun with the same OUT_DIR at the same head skips the shards whose last row there is ok;
#   a row from another head counts for nothing: every head reruns every shard (no carry-over).
#   summary.txt ends with `DONE head=… shards=N ok=… failed=… unwritten=…`.
#
# The exit status is zero only if every planned shard has an ok row at this head and every
# required artifact was written.
#
# env:
#   ST3A_SHARD_JOBS     shards run at once; default 8. Each unit runs with Nice=19,
#                       IOSchedulingClass=idle, CPUWeight=10.
#   ST3A_SHARD_DEPTH    override the plan's decision-prefix depth (the test's SHARD_DEPTH).
#   ST3A_SHARDS_BIN     skip the build and use this power_loss_deep binary; then
#   ST3A_SHARDS_HEAD    names the head the ledger records (default: a one-off name, so nothing
#                       resumes).
#   ST3A_SHARDS_INLINE  if 1, run shards as child processes at nice 19 instead of systemd units.
#                       For the runner's own test.
set -u

if [ $# -ne 1 ]; then
  sed -n '2,27p' "$0" >&2
  exit 2
fi
OUT=$(realpath -m "$1")
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
JOBS=${ST3A_SHARD_JOBS:-8}
COMPONENT=nested_depth1_extended
mkdir -p "$OUT/shards" "$OUT/bin" || exit 2
SUMMARY=$OUT/summary.txt
LEDGER=$OUT/ledger.tsv
unwritten=0
report() {
  if ! printf '%s\n' "$*" >> "$SUMMARY"; then
    echo "st3a-shards: cannot write $SUMMARY: $*" >&2
    unwritten=$((unwritten + 1))
  fi
}
if ! : > "$SUMMARY"; then
  echo "st3a-shards: cannot create $SUMMARY" >&2
  unwritten=$((unwritten + 1))
fi
touch "$LEDGER" || exit 2

if [ -n "${ST3A_SHARDS_BIN:-}" ]; then
  src=$ST3A_SHARDS_BIN
  HEAD=${ST3A_SHARDS_HEAD:-unknown-$(date -u +%s)-$$}
else
  HEAD=$(git -C "$ROOT" rev-parse HEAD) || exit 2
  # A dirty tree is not the head: its shards resume nothing and nothing resumes them.
  git -C "$ROOT" diff --quiet HEAD -- || HEAD=$HEAD-dirty-$(date -u +%s)
  if ! (cd "$ROOT" && cargo test --release -p dashd-store --no-default-features \
      --features faulty --test power_loss_deep --no-run --message-format=json \
      2> "$OUT/build.log" > "$OUT/build.json"); then
    report "BUILD FAILED (build.log)"
    exit 1
  fi
  src=$(jq -r 'select(.reason == "compiler-artifact" and .profile.test and .executable
                and .target.name == "power_loss_deep") | .executable' "$OUT/build.json")
fi
# The units run a copy, so a later build in the checkout cannot swap the binary under them.
BIN=$OUT/bin/power_loss_deep-${HEAD:0:12}
if [ -z "$src" ] || ! cp "$src" "$BIN"; then
  report "NO BINARY ($src)"
  exit 1
fi
printf '%s\n' "$HEAD" > "$OUT/head.txt" || unwritten=$((unwritten + 1))
report "head $HEAD"

if ! (cd "$ROOT/dashd-store" && ST3A_SHARD_PLAN=$OUT/plan.txt \
    "$BIN" --exact "$COMPONENT" --ignored --nocapture > "$OUT/plan.log" 2>&1); then
  report "PLAN FAILED (plan.log)"
  exit 1
fi
mapfile -t PLAN < <(grep -v '^#' "$OUT/plan.txt")
report "$(head -1 "$OUT/plan.txt") shards ${#PLAN[@]}"
if [ ${#PLAN[@]} -eq 0 ]; then
  report "EMPTY PLAN"
  exit 1
fi

# Each shard's last status at this head: the resume skips the ok ones, and the verdict reads
# them again after the run, by the same rule.
declare -A LAST
read_last() {
  local h id st
  LAST=()
  while IFS=$'\t' read -r h id st _; do
    [ "$h" = "$HEAD" ] && LAST[$id]=$st
  done < "$LEDGER"
}
read_last

# Runs one shard and appends its ledger row.
run_shard() {
  local n=$1 id=$2
  local f=${id//[:]/_}
  local log=$OUT/shards/$f.log time=$OUT/shards/$f.time rc
  rm -f "$time"
  local cmd=(/usr/bin/time -f "%U %S %e" -o "$time" "$BIN" --exact "$COMPONENT" --ignored
             --nocapture)
  if [ "${ST3A_SHARDS_INLINE:-0}" = 1 ]; then
    (cd "$ROOT/dashd-store" && ST3A_SHARD=$id nice -n 19 "${cmd[@]}" > "$log" 2>&1)
    rc=$?
  else
    systemd-run --user --quiet --wait --collect --unit="st3a-shard-${HEAD:0:8}-$$-$n" \
      -p Nice=19 -p IOSchedulingClass=idle -p CPUWeight=10 \
      -p WorkingDirectory="$ROOT/dashd-store" -p StandardOutput=truncate:"$log" \
      -p StandardError=inherit --setenv=ST3A_SHARD="$id" "${cmd[@]}" > /dev/null 2>&1
    rc=$?
  fi
  local status=ok times result
  times=$(tail -1 "$time" 2> /dev/null)
  result=$(grep -m1 '^SHARD ' "$log" 2> /dev/null | cut -d' ' -f3-)
  if [ $rc -ne 0 ]; then
    status=FAILED
  elif ! grep -Eq '^test result: ok\. 1 passed' "$log"; then
    status=NO-TEST
  elif [ -z "$result" ]; then
    status=NO-RESULT
  elif [ -z "$times" ]; then
    status=NO-TIME
  fi
  [ -n "$times" ] || times="- - -"
  (
    flock 9
    printf '%s\t%s\t%s\t%s\t%s\n' "$HEAD" "$id" "$status" "${times// /$'\t'}" \
      "${result:-exit=$rc}" >> "$LEDGER"
  ) 9>> "$LEDGER.lock"
}

started=$(date +%s)
skipped=0
running=0
for i in "${!PLAN[@]}"; do
  id=${PLAN[$i]}
  if [ "${LAST[$id]:-}" = ok ]; then
    skipped=$((skipped + 1))
    continue
  fi
  if [ $running -ge "$JOBS" ]; then
    wait -n
    running=$((running - 1))
  fi
  run_shard "$i" "$id" &
  running=$((running + 1))
done
wait

# The verdict: every planned shard's last row at this head.
read_last
ok=0
failed=0
user=0
for id in "${PLAN[@]}"; do
  if [ "${LAST[$id]:-}" = ok ]; then
    ok=$((ok + 1))
  else
    failed=$((failed + 1))
    report "NOT OK $id ${LAST[$id]:-missing}"
  fi
done
user=$(awk -F'\t' -v h="$HEAD" '$1 == h && $3 == "ok" { u += $4; s += $5 }
       END { printf "%.0f", u + s }' "$LEDGER")
report "cpu-s $user (ok rows at this head) wall-s $(($(date +%s) - started)) skipped $skipped"
report "DONE head=$HEAD shards=${#PLAN[@]} ok=$ok failed=$failed unwritten=$unwritten"
[ $failed -eq 0 ] && [ $unwritten -eq 0 ]
