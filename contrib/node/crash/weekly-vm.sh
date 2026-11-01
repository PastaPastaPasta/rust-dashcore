#!/usr/bin/env bash
# ST3b's VM weekly (PLAN-ST3b-S7 §2.4): builds release rdashd and st3b-crash at the checkout's
# HEAD, the guest (vm/mkguest.sh) and the st3b-qemu image, sweeps stale runs, then runs each VM
# scenario alone and records it.
#
# usage:
#   weekly-vm.sh OUT_DIR [COMPONENT...]   everything in this process (by hand)
#   weekly-vm.sh --units OUT_DIR          what st3b-vm-weekly.service runs: --prepare, then each
#                                         component as its own st3b-vm-weekly@<component>.service
#                                         in turn (each with its own timeout), then --finish
#   weekly-vm.sh --prepare OUT_DIR        the gate, the build, kernel, guest, image and sweep
#   weekly-vm.sh --component COMPONENT [OUT_DIR]
#                                         one component: what st3b-vm-weekly@.service runs
#   weekly-vm.sh --finish [OUT_DIR]       the week's leak check and the DONE line
#   weekly-vm.sh --sweep [OUT_DIR]        `st3b-crash sweep --stale-vm` with the week's binary
#                                         (the units' ExecStopPost)
#
# OUT_DIR must be under /work/scratch/ (the guest is built in it). --prepare records it in
# $ST3B_VM_STATE/current, which the other modes read when OUT_DIR is left out, and writes
# OUT_DIR/weekly.env: the binary directory, guest, seed and SHA every later step uses, so the
# build happens once (ST3B_BIN_DIR for the rest).
#
# A component is <scenario>-<fs> (`<scenario>:<fs>` is read the same): v-ibd, v-flush and
# v-recovery on W-40k, v-reorg on W-small (its branch B), each on ext4 and xfs, all with
# -dbcache=4 -dbbatchsize=20000; and v-reindex, which prints SKIPPED (CH5) and is not counted. Each
# runs `st3b-crash vm` with $ST3B_VM_KILLS_EXT4 (default 50) or $ST3B_VM_KILLS_XFS (default 20)
# kills, holding $ST3B_VM_STATE/guest.lock (one guest at a time; it waits up to
# $ST3B_VM_LOCK_WAIT seconds, default 600, then records BUSY and exits 75). OUT_DIR gets head.txt,
# build.log, guest.log, image.log, a log and a time file per component, and summary.txt: the seed,
# one row per component, then DONE. Each row is also appended to $ST3B_VM_LEDGER, tab-separated:
# UTC, SHA, scenario, fs, counted kills (the scenario's own, in their window), the scenario's own
# kills, all kills, reorg kills C3 found strictly inside a reorg (v-reorg; `-` for the others),
# failures, CPU-s (user + system), wall-s, seed, status. The summary's legend line says what "in
# window" means per scenario (st3b-crash vm prints the same); v-reorg's rows are followed by one
# line per reorg of W-small with its quota and counted kills (S7 r2 N6, r3).
#
# A killed component (its unit's timeout, or a stop) writes a `TIMEOUT <component>` row to the
# summary and the ledger before it exits non-zero, with the counts the harness's last `so far:`
# line had (S7 r2 N-c).
#
# A heavy unit (PLAN-ST3b-S7 §4): --prepare starts only with >= $ST3B_VM_MIN_FREE_G (default 260)
# GB free on /work and a 5-minute load under $ST3B_VM_MAX_LOAD (default 80); otherwise it records a
# DEFERRED row in the summary and the ledger and exits 75 (the unit tries again later).
#
# The exit status is non-zero if the build fails, any component fails (one that makes fewer counted
# kills than asked fails), a run leaves a container behind, or a required artifact cannot be
# written. Every component runs regardless. A failed run keeps its directory under
# /work/scratch/st3b/run for the repro (remove it with /work/scratch/mgr/rm-if-idle.sh).
#
# The kernel is /work/scratch/st3b/vm/vmlinuz-$(uname -r), checked against its line in
# /work/scratch/st3b/vm/vmlinuz.sha256 every week (S7 r1 m5); if it is missing, the one sudo call of
# the VM level makes it, `sudo -n contrib/node/crash/st3b-priv kernel <that path>`, and records its
# sha256 there.
#
# env:
#   ST3B_SEED          the kills' seed; default: the UTC date (YYYYMMDD)
#   ST3B_DASHD         the fixtures' dashd (default: the pinned v24.0.0-rc.4 under ~/.rust-dashcore-test)
#   ST3B_FIXTURES      the fixture directory (default: /work/scratch/st3b/fixtures)
#   ST3B_BIN_DIR       skip the build and run rdashd and st3b-crash from this directory
#   ST3B_VM_KILLS_EXT4 default 50
#   ST3B_VM_KILLS_XFS  default 20
#   ST3B_VM_STATE      default: /work/scratch/st3b/weekly-vm (current, guest.lock)
#   ST3B_VM_LEDGER     default: $ST3B_VM_STATE/ledger.tsv
set -u
umask 077

SCENARIOS=(v-ibd v-reorg v-flush v-recovery v-reindex)
COMPONENTS=()
for s in "${SCENARIOS[@]}"; do COMPONENTS+=("$s-ext4" "$s-xfs"); done
STATE=${ST3B_VM_STATE:-/work/scratch/st3b/weekly-vm}
LEDGER=${ST3B_VM_LEDGER:-$STATE/ledger.tsv}
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
VM_DIR=/work/scratch/st3b/vm
KERNEL=$VM_DIR/vmlinuz-$(uname -r)
export TMPDIR=${TMPDIR:-/work/scratch/st3b/tmp}

usage() {
  sed -n '2,/^set -u/p' "$0" | sed '$d' >&2
  exit 2
}

# The OUT_DIR of a mode: the argument, or the one --prepare recorded.
out_dir() {
  local o=${1:-}
  if [ -z "$o" ]; then
    o=$(cat "$STATE/current" 2> /dev/null) || {
      echo "weekly-vm: no OUT_DIR and no $STATE/current" >&2
      exit 2
    }
  fi
  o=$(realpath -m "$o")
  case $o in /work/scratch/?*) ;; *)
    echo "weekly-vm: OUT_DIR must be under /work/scratch/: $o" >&2
    exit 2
    ;;
  esac
  printf '%s\n' "$o"
}

unwritten=0
report() {
  if ! printf '%s\n' "$*" >> "$SUMMARY"; then
    echo "weekly-vm: cannot write $SUMMARY: $*" >&2
    unwritten=$((unwritten + 1))
  fi
}

# One ledger row from its fields.
ledger() {
  local IFS=$'\t'
  if ! printf '%s\t%s\t%s\n' "$(date -u +%FT%TZ)" "${SHA:-unknown}" "$*" >> "$LEDGER"; then
    echo "weekly-vm: cannot append to $LEDGER" >&2
    unwritten=$((unwritten + 1))
  fi
}

# Sets OUT, SUMMARY and the week's settings from weekly.env (after --prepare wrote it).
load_week() {
  OUT=$1
  SUMMARY=$OUT/summary.txt
  if [ ! -r "$OUT/weekly.env" ]; then
    echo "weekly-vm: no $OUT/weekly.env (run --prepare first)" >&2
    exit 2
  fi
  # shellcheck source=/dev/null
  . "$OUT/weekly.env"
  export ST3B_BIN_DIR=$BIN
  CRASH=$BIN/st3b-crash
}

prepare() {
  OUT=$1
  SUMMARY=$OUT/summary.txt
  mkdir -p "$OUT" "$TMPDIR" "$STATE" "$(dirname "$LEDGER")" || exit 2
  SEED=${ST3B_SEED:-$(date -u +%Y%m%d)}
  SHA=$(git -C "$ROOT" rev-parse HEAD 2> /dev/null || echo unknown)
  if ! : > "$SUMMARY"; then
    echo "weekly-vm: cannot create $SUMMARY" >&2
    exit 2
  fi
  report "seed ST3B_SEED=$SEED"
  report "legend: counted = the scenario's own kills in their window. flush, recovery: the next boot replays blocks. reorg: the kills are split evenly over W-small's reorgs as quotas (the remainder to the last); each boot aims at one reorg still ahead with quota left (seeded; none left: the lap kills no more), and the component passes only with every quota met; the submitblock that sets it off had not returned when the kill was armed, and the killed boot's console shows the reorg cut mid-way (a height drop, no UpdateTip at or above the old top yet); nothing is flushed inside a reorg at -dbcache=4, so the node recovers to the last flushed state with no half-applied reorg (C1-C4). inside = C3 found the node strictly inside a reorg, the stronger case, not required (durable mid-reorg states are the device level's, D-REORG-S), and failed unless the killed boot's console shows a coins flush inside it. random: always. Every kill passes C1-C4 at its floor."
  echo "$SHA" > "$OUT/head.txt" || unwritten=$((unwritten + 1))

  # The heavy-start gate. A deferral is a row too: a skipped week shows (S7 r1 m9).
  local free load
  free=$(df -BG --output=avail /work | tail -1 | tr -dc 0-9)
  load=$(cut -d' ' -f2 /proc/loadavg)
  if [ "${free:-0}" -lt "${ST3B_VM_MIN_FREE_G:-260}" ] ||
    ! awk -v l="$load" -v m="${ST3B_VM_MAX_LOAD:-80}" 'BEGIN { exit !(l < m) }'; then
    report "DEFERRED free=${free}G load5=$load"
    ledger - - 0 0 0 - 0 0 0 "$SEED" "DEFERRED free=${free}G load5=$load"
    exit 75
  fi

  if [ -n "${ST3B_BIN_DIR:-}" ]; then
    BIN=$(realpath -m "$ST3B_BIN_DIR")
  else
    # Bounded, so a hung build cannot hold the driver (S7 r2 N-b).
    if ! (cd "$ROOT" && timeout 3h cargo build --release -p rdashd -p dash-testkit \
        --features dash-testkit/crash --bin rdashd --bin st3b-crash > "$OUT/build.log" 2>&1); then
      report "BUILD FAILED (build.log)"
      ledger - - 0 0 0 - 0 0 0 "$SEED" BUILD-FAILED
      exit 1
    fi
    BIN=$ROOT/target/release
  fi
  CRASH=$BIN/st3b-crash
  local b
  for b in "$CRASH" "$BIN/rdashd" "${ST3B_DASHD:-$HOME/.rust-dashcore-test/dashcore-24.0.0-rc.4/bin/dashd}"; do
    if [ ! -x "$b" ]; then
      report "NO BINARY $b"
      exit 1
    fi
  done

  # The kernel: made once, checked every week against the sha256 recorded when it was made.
  local name
  name=$(basename "$KERNEL")
  if [ ! -e "$KERNEL" ]; then
    # The log is ours to write: only the copy runs as root.
    # shellcheck disable=SC2024
    if ! sudo -n "$ROOT/contrib/node/crash/st3b-priv" kernel "$KERNEL" > "$OUT/kernel.log" 2>&1; then
      report "NO KERNEL $KERNEL (kernel.log)"
      exit 1
    fi
    awk -v n="$name" 'NF == 2 { print $1 "  " n }' "$OUT/kernel.log" >> "$VM_DIR/vmlinuz.sha256" ||
      unwritten=$((unwritten + 1))
  fi
  if ! (cd "$VM_DIR" && grep -E "^[0-9a-f]{64}  $name\$" vmlinuz.sha256 | tail -1 | sha256sum -c --status); then
    report "KERNEL MISMATCH $KERNEL (no matching line in $VM_DIR/vmlinuz.sha256)"
    exit 1
  fi

  # One guest per rdashd: a rerun with a new build gets its own (S7 r1 #11). mkguest.sh builds it
  # beside its place and renames it in whole.
  GUEST=$OUT/guest-$(sha256sum "$BIN/rdashd" | cut -c1-12)
  if [ ! -e "$GUEST/manifest.txt" ] &&
    ! "$ROOT/contrib/node/crash/vm/mkguest.sh" "$BIN/rdashd" "$GUEST" "$KERNEL" > "$OUT/guest.log" 2>&1; then
    report "GUEST FAILED (guest.log)"
    exit 1
  fi
  local image
  image=st3b-qemu:$(sha256sum "$ROOT/contrib/node/crash/vm/Dockerfile" | cut -c1-12)
  if ! docker image inspect "$image" > /dev/null 2>&1 &&
    ! docker build -t "$image" "$ROOT/contrib/node/crash/vm" > "$OUT/image.log" 2>&1; then
    report "IMAGE FAILED $image (image.log)"
    exit 1
  fi
  # A run killed on an earlier week leaves its containers. Containers only: the VM level needs no
  # sudo beyond the kernel copy.
  if ! "$CRASH" sweep --stale-vm > "$OUT/sweep.log" 2>&1; then
    report "SWEEP FAILED (sweep.log)"
  fi
  if ! printf 'BIN=%q\nGUEST=%q\nSEED=%q\nSHA=%q\n' "$BIN" "$GUEST" "$SEED" "$SHA" > "$OUT/weekly.env" ||
    ! printf '%s\n' "$OUT" > "$STATE/current"; then
    report "CANNOT RECORD THE WEEK ($OUT/weekly.env, $STATE/current)"
    exit 1
  fi
}

# The TERM handler of a running component: its unit's timeout (or a stop) kills the whole cgroup,
# the harness first; bash runs this once its foreground child is gone. The row keeps the partial
# evidence: the counts of the harness's last `so far:` line, printed after each checked kill.
on_term() {
  local total=0 own=0 counted=0 inside=-
  read -r total own counted inside < <(sed -nE \
    's/^so far: kills ([0-9]+), own ([0-9]+), counted ([0-9]+), inside ([0-9]+)$/\1 \2 \3 \4/p' \
    "$OUT/$COMPONENT.log" 2> /dev/null | tail -1)
  [ "$scenario" = v-reorg ] || inside=-
  report "TIMEOUT $COMPONENT counted=${counted:-0} own=${own:-0} kills=${total:-0} inside=${inside:--}"
  ledger "$scenario" "$fs" "${counted:-0}" "${own:-0}" "${total:-0}" "${inside:--}" 0 0 0 "$SEED" "TIMEOUT $COMPONENT"
  exit 143
}

# One component; 0 when it passed or was skipped, 75 when the guest lock stayed busy.
component() {
  COMPONENT=${1/:/-}
  scenario=${COMPONENT%-*}
  fs=${COMPONENT##*-}
  local log=$OUT/$COMPONENT.log kills fixture
  local fixtures=${ST3B_FIXTURES:-/work/scratch/st3b/fixtures}
  case $fs in
    ext4) kills=${ST3B_VM_KILLS_EXT4:-50} ;;
    xfs) kills=${ST3B_VM_KILLS_XFS:-20} ;;
    *) kills= ;;
  esac
  case $scenario in
    v-ibd | v-flush | v-recovery) fixture=$fixtures/w-40k ;;
    v-reorg) fixture=$fixtures/w-small ;;
    v-reindex) fixture=- ;;
    *) fixture= ;;
  esac
  if [ -z "$kills" ] || [ -z "$fixture" ]; then
    report "- - - $COMPONENT status=UNKNOWN"
    return 1
  fi
  trap on_term TERM
  # One guest at a time on the box, whoever runs it.
  exec 9>> "$STATE/guest.lock" || return 1
  if ! flock -w "${ST3B_VM_LOCK_WAIT:-600}" 9; then
    report "- - - $COMPONENT status=BUSY ($STATE/guest.lock)"
    ledger "$scenario" "$fs" 0 0 0 - 0 0 0 "$SEED" BUSY
    return 75
  fi
  rm -f "$OUT/$COMPONENT.time"
  # In the background and waited for, so TERM reaches the trap at once.
  /usr/bin/time -f "%U %S %e" -o "$OUT/$COMPONENT.time" "$CRASH" vm "$fixture" \
    --scenario "${scenario^^}" --fs "$fs" --kills "$kills" --seed "$SEED" \
    --dashd "${ST3B_DASHD:-$HOME/.rust-dashcore-test/dashcore-24.0.0-rc.4/bin/dashd}" \
    --guest "$GUEST" -- -dbcache=4 -dbbatchsize=20000 > "$log" 2>&1 9>&- &
  wait $!
  local rc=$?
  trap - TERM
  flock -u 9
  local times total counted own inside fails status=ok
  times=$(tail -1 "$OUT/$COMPONENT.time" 2> /dev/null)
  read -r total counted < <(sed -nE 's/^kills ([0-9]+) \(([0-9]+) counted.*/\1 \2/p' "$log" | tail -1)
  own=$(sed -nE 's/^kills [0-9]+ \([0-9]+ counted of [0-9]+\), [a-z]+ kills ([0-9]+):.*/\1/p' "$log" | tail -1)
  inside=$(sed -nE 's/^strictly inside a reorg \(C3\): ([0-9]+) of .*/\1/p' "$log" | tail -1)
  fails=$(grep -c '^FAIL ' "$log")
  if grep -q ': SKIPPED (CH5)$' "$log"; then
    status=SKIPPED-CH5
  elif [ $rc -ne 0 ]; then
    status=FAILED
  elif [ "${counted:-0}" -lt "$kills" ]; then
    status=FEW-KILLS
  elif [ -z "$times" ]; then
    status=NO-TIME
  fi
  report "${times:-- - -} $COMPONENT exit=$rc counted=${counted:-0}/$kills in-window=${counted:-0}/${own:-0} kills=${total:-0} inside=${inside:--} failures=$fails status=$status"
  local line
  while IFS= read -r line; do
    report "  $COMPONENT $line"
  done < <(grep -E '^reorg [0-9]+ \(positions ' "$log")
  local cpu wall
  cpu=$(echo "${times:-0 0 0}" | awk '{printf "%.0f", $1 + $2}')
  wall=$(echo "${times:-0 0 0}" | awk '{printf "%.0f", $3}')
  ledger "$scenario" "$fs" "${counted:-0}" "${own:-0}" "${total:-0}" "${inside:--}" "$fails" "$cpu" "$wall" "$SEED" "$status"
  case $status in ok | SKIPPED-CH5) return 0 ;; *) return 1 ;; esac
}

# The week's leak check (only the runs its components printed: `run <id>`) and the DONE line.
finish() {
  local failed=0 leaked=0 c id
  : > "$OUT/leaks.log"
  for c in "${COMPONENTS[@]}"; do
    while read -r _ id; do
      "$CRASH" leaks --vm "$id" >> "$OUT/leaks.log" 2>&1 || leaked=1
    done < <(grep -oE '^run [0-9]+-[0-9]+' "$OUT/$c.log" 2> /dev/null)
  done
  if [ $leaked = 1 ]; then
    report "LEAKS (leaks.log)"
    failed=1
  fi
  # A component that failed, timed out or was busy has its row; DONE counts them.
  failed=$((failed + $(grep -cE 'status=(FAILED|FEW-KILLS|NO-TIME|UNKNOWN|BUSY)|^TIMEOUT ' "$SUMMARY")))
  report "DONE failed=$failed unwritten=$unwritten"
  [ $failed -eq 0 ] && [ $unwritten -eq 0 ]
}

case ${1:-} in
  --prepare)
    [ $# -eq 2 ] || usage
    prepare "$(out_dir "$2")"
    ;;
  --component)
    [ $# -ge 2 ] && [ $# -le 3 ] || usage
    load_week "$(out_dir "${3:-}")"
    component "$2"
    exit $?
    ;;
  --finish)
    [ $# -le 2 ] || usage
    load_week "$(out_dir "${2:-}")"
    finish
    ;;
  --sweep)
    [ $# -le 2 ] || usage
    load_week "$(out_dir "${2:-}")"
    exec "$CRASH" sweep --stale-vm
    ;;
  --units)
    [ $# -eq 2 ] || usage
    OUT=$(out_dir "$2")
    prepare "$OUT"
    for c in "${COMPONENTS[@]}"; do
      # Each its own unit, timeout and record; a failed one fails the week, not the rest.
      systemctl --user start "st3b-vm-weekly@$c.service" ||
        echo "weekly-vm: st3b-vm-weekly@$c.service failed" >&2
    done
    load_week "$OUT"
    finish
    ;;
  -* | '') usage ;;
  *)
    OUT=$(out_dir "$1")
    shift
    [ $# -eq 0 ] && set -- "${COMPONENTS[@]}"
    prepare "$OUT"
    load_week "$OUT"
    for c in "$@"; do component "$c"; done
    finish
    ;;
esac
