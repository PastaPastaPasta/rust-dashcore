#!/usr/bin/env bash
# PLAN-ST3b-VMW6: weekly-vm.sh's run-to-tip pick and its --units driver.
# - The pick: by the latest Sunday at or before the run date, its week mod 6, so over 70 Sundays
#   across a 52-week and a 53-week year end each W-40k component comes every 6 weeks, in the
#   fixed order, and a Monday to Saturday run (a catch-up) picks its Sunday's; the
#   ST3B_VM_TO_TIP override; a name outside the six, or a bad date, exits 2.
# - The driver, on stubs (st3b-crash, systemctl, docker, a fake kernel under ST3B_VM_DIR): a
#   whole week ends DONE with the to-tip component noted and no unit left recorded as running; a
#   TERM (or an INT) mid-week stops the running component (its TIMEOUT row), writes NOT-RUN rows
#   for the rest and INCOMPLETE, and exits 143 (130); --stop-started stops the recorded unit only
#   after an abnormal end.
# Runs no build, guest, unit or sudo; its scratch directory under /work/scratch is removed.
#
#   contrib/node/crash/weekly-vm-tests.sh
set -uo pipefail

weekly=$(realpath -e -- "$(dirname -- "$0")/weekly-vm.sh")
fail=0

# expect <label> <want stdout> <want exit> <command...>
expect() {
  local label=$1 want=$2 want_rc=$3 got rc=0
  shift 3
  got=$("$@" 2> /dev/null) || rc=$?
  if [ "$got" != "$want" ] || [ "$rc" != "$want_rc" ]; then
    echo "FAIL $label: \"$got\", exit $rc (want \"$want\", exit $want_rc)"
    fail=1
    return
  fi
  echo "ok   $label: ${got:-(none)}"
}

# check <label> <condition...>
check() {
  local label=$1
  shift
  if "$@"; then echo "ok   $label"; else
    echo "FAIL $label"
    fail=1
  fi
}

# shellcheck disable=SC2329 # run through expect.
pick() { env -u ST3B_VM_TO_TIP "$weekly" --to-tip-pick "$@"; }

order=(v-ibd-ext4 v-ibd-xfs v-flush-ext4 v-flush-xfs v-recovery-ext4 v-recovery-xfs)

# Pinned: 2026-11-29 is day 20786, a Sunday: ((20786 + 4) / 7 + 5) mod 6 = 2975 mod 6 = 5.
expect "week of 2026-11-29" v-recovery-xfs 0 pick 2026-11-29
expect "the epoch's first Sunday" v-ibd-xfs 0 pick 1970-01-11
expect "a run before it" v-recovery-xfs 0 pick 1970-01-03
expect "a bad date" "" 2 pick not-a-date

# 70 Sundays from 2025-11-02: ISO week 52 of 2025 to week 1 of 2026 (2025-12-28, 2026-01-04),
# and week 53 of 2026 to week 1 of 2027 (2027-01-03, 2027-01-10).
declare -A last
gaps_ok=1
prev=
for i in $(seq 0 69); do
  d=$(date -u -d "2025-11-02 + $((7 * i)) days" +%F)
  c=$(pick "$d") || { echo "FAIL pick $d"; fail=1; continue; }
  if [ -n "$prev" ]; then
    for j in "${!order[@]}"; do [ "${order[$j]}" = "$prev" ] && want=${order[$(((j + 1) % 6))]}; done
    if [ "$c" != "$want" ]; then
      echo "FAIL $d: $c after $prev (want $want)"
      gaps_ok=0
    fi
  fi
  if [ -n "${last[$c]:-}" ] && [ $((i - last[$c])) != 6 ]; then
    echo "FAIL $d: $c again after $((i - last[$c])) weeks"
    gaps_ok=0
  fi
  last[$c]=$i prev=$c
done
check "70 weeks across two year ends: the fixed order, each component every 6 weeks" [ $gaps_ok = 1 ]
expect "ISO week 52 of 2025" "${order[5]}" 0 pick 2025-12-28
expect "then week 1 of 2026" "${order[0]}" 0 pick 2026-01-04
expect "ISO week 53 of 2026" "${order[4]}" 0 pick 2027-01-03
expect "then week 1 of 2027" "${order[5]}" 0 pick 2027-01-10

# A catch-up run Monday to Saturday picks its Sunday's component, around both year ends too.
catch_up_ok=1
for sunday in 2025-12-28 2026-01-04 2026-11-29 2027-01-03 2027-01-10; do
  want=$(pick "$sunday")
  for d in 1 2 3 4 5 6; do
    day=$(date -u -d "$sunday + $d days" +%F)
    got=$(pick "$day")
    if [ "$got" != "$want" ]; then
      echo "FAIL $day: $got, not its Sunday $sunday's $want"
      catch_up_ok=0
    fi
  done
  next=$(date -u -d "$sunday + 7 days" +%F)
  if [ "$(pick "$next")" = "$want" ]; then
    echo "FAIL $next: the next Sunday kept $sunday's $want"
    catch_up_ok=0
  fi
done
check "Monday to Saturday pick their Sunday's component" [ $catch_up_ok = 1 ]

expect "override" v-flush-xfs 0 env ST3B_VM_TO_TIP=v-flush-xfs "$weekly" --to-tip-pick 2026-11-29
expect "override, colon form" v-recovery-ext4 0 \
  env ST3B_VM_TO_TIP=v-recovery:ext4 "$weekly" --to-tip-pick
expect "override outside the six" "" 2 env ST3B_VM_TO_TIP=v-reorg-ext4 "$weekly" --to-tip-pick
expect "too many arguments" "" 2 pick 2026-11-29 extra

# The driver on stubs.
T=$(mktemp -d /work/scratch/weekly-vm-tests.XXXXXX)
# shellcheck disable=SC2329 # run by the EXIT trap.
cleanup() {
  touch "$T/stop"
  sleep 0.3
  rm -rf -- "$T"
}
trap cleanup EXIT
mkdir -p "$T/bin" "$T/vm" "$T/fx"
cat > "$T/bin/st3b-crash" << 'EOF'
#!/usr/bin/env bash
case $1 in
  sweep | leaks) exit 0 ;;
  vm) ;;
  *) exit 2 ;;
esac
shift
fixture=$1 scenario= fs=
shift
args=("$@")
while [ $# -gt 0 ]; do
  case $1 in
    --scenario) scenario=${2,,} ;;
    --fs) fs=$2 ;;
  esac
  shift
done
c=$scenario-$fs
printf '%s\n' "${args[@]}" > "$STUB_DIR/args.$c"
echo "run 1-1 vm $fixture"
if [ "$scenario" = v-reindex ]; then
  echo "V-REINDEX $fs: SKIPPED (CH5)"
  exit 0
fi
if [ "$c" = "${STUB_SLOW:-}" ]; then
  echo "so far: kills 2, own 2, counted 1, inside 0"
  touch "$STUB_DIR/slow.started"
  for _ in $(seq 600); do
    [ -e "$STUB_DIR/stop" ] && exit 1
    sleep 0.1
  done
  exit 1
fi
echo "kills 3 (3 counted of 3), x kills 3: 3 in window (100%)"
EOF
cat > "$T/bin/systemctl" << 'EOF'
#!/usr/bin/env bash
# systemctl --user start|stop [--no-block] st3b-vm-weekly@<component>.service
[ "$1" = --user ] && shift
verb=$1
shift
[ "${1:-}" = --no-block ] && shift
c=${1#st3b-vm-weekly@}
c=${c%.service}
echo "$verb $c" >> "$STUB_DIR/systemctl.log"
case $verb in
  start)
    "$WEEKLY" --component "$c" &
    echo $! > "$STUB_DIR/pid.$c"
    wait $!
    ;;
  stop) [ ! -e "$STUB_DIR/pid.$c" ] || kill -TERM "$(cat "$STUB_DIR/pid.$c")" ;;
esac
EOF
printf '#!/bin/sh\nexit 0\n' > "$T/bin/docker"
printf '#!/bin/sh\nexit 0\n' > "$T/bin/rdashd"
printf '#!/bin/sh\nexit 0\n' > "$T/bin/dashd"
chmod 700 "$T/bin/"*
kernel=vmlinuz-$(uname -r)
echo fake > "$T/vm/$kernel"
(cd "$T/vm" && sha256sum "$kernel" > vmlinuz.sha256)

# week_env <name> [slow component]: sets `run` to a --units week in $T/<name> on the stubs (its
# state in $T/state-<name>).
week_env() {
  local guest
  guest=$T/$1/guest-$(sha256sum "$T/bin/rdashd" | cut -c1-12)
  mkdir -p "$guest" "$T/stub-$1"
  touch "$guest/manifest.txt"
  run=(env PATH="$T/bin:$PATH" WEEKLY="$weekly" STUB_DIR="$T/stub-$1" STUB_SLOW="${2:-}"
    INVOCATION_ID="inv-$1"
    ST3B_BIN_DIR="$T/bin" ST3B_DASHD="$T/bin/dashd" ST3B_FIXTURES="$T/fx" ST3B_SEED=7
    ST3B_VM_STATE="$T/state-$1" ST3B_VM_DIR="$T/vm" ST3B_VM_MIN_FREE_G=0 ST3B_VM_MAX_LOAD=1000000
    ST3B_VM_KILLS_EXT4=3 ST3B_VM_KILLS_XFS=3 ST3B_VM_TO_TIP=v-recovery-xfs ST3B_VM_LOCK_WAIT=5
    "$weekly" --units "$T/$1")
}

# wait_for <seconds> <condition...>
wait_for() {
  local n=$(($1 * 10))
  shift
  while ! "$@"; do
    n=$((n - 1))
    [ $n -gt 0 ] || return 1
    sleep 0.1
  done
}

rc=0
week_env whole
"${run[@]}" > "$T/whole.driver.log" 2>&1 || rc=$?
s=$T/whole/summary.txt
l=$T/state-whole/ledger.tsv
check "a whole week exits 0" [ $rc = 0 ]
check "a whole week ends DONE failed=0" grep -qx 'DONE failed=0 unwritten=0' "$s"
check "its summary names the to-tip component" grep -qx 'to_tip=v-recovery-xfs' "$s"
check "only the to-tip row is noted" [ "$(grep -c 'to_tip=1$' "$l")" = 1 ]
check "the noted row is v-recovery-xfs ok" grep -qP '\tv-recovery\txfs\t.*\tok to_tip=1$' "$l"
for c in v-recovery-xfs v-reorg-ext4 v-reorg-xfs; do
  check "$c runs --to-tip" grep -qx -- --to-tip "$T/stub-whole/args.$c"
done
check "v-ibd-ext4 has no --to-tip" bash -c "! grep -qx -- --to-tip '$T/stub-whole/args.v-ibd-ext4'"
check "no unit is left recorded as running" test ! -e "$T/state-whole/running"
check "a whole week stops no unit" test ! -e "$T/stub-whole/systemctl.log" -o -z \
  "$(grep -v '^start ' "$T/stub-whole/systemctl.log")"

week_env cut v-flush-ext4
"${run[@]}" > "$T/cut.driver.log" 2>&1 &
driver=$!
s=$T/cut/summary.txt
l=$T/state-cut/ledger.tsv
if wait_for 60 test -e "$T/stub-cut/slow.started"; then
  check "the running component is recorded" \
    grep -qx 'inv-cut st3b-vm-weekly@v-flush-ext4.service' "$T/state-cut/running"
  kill -TERM "$driver"
  rc=0
  wait "$driver" || rc=$?
  check "a cut driver exits 143" [ $rc = 143 ]
  check "the running component writes its TIMEOUT row" \
    wait_for 20 grep -q '^TIMEOUT v-flush-ext4 counted=1 own=2 kills=2' "$s"
  check "the components before it have their rows" \
    [ "$(grep -cE '^[0-9.]+ [0-9.]+ [0-9.]+ v-(ibd|reorg)-(ext4|xfs) .*status=ok$' "$s")" = 4 ]
  not_run=(v-flush-xfs v-recovery-ext4 v-recovery-xfs v-reindex-ext4 v-reindex-xfs)
  check "NOT-RUN rows for the components never started, in order" \
    [ "$(grep '^NOT-RUN ' "$s" | cut -d' ' -f2 | tr '\n' ' ')" = "${not_run[*]} " ]
  check "INCOMPLETE stands where DONE would be" grep -qx 'INCOMPLETE not_run=5 unwritten=0' "$s"
  check "no DONE" bash -c "! grep -q '^DONE' '$s'"
  check "five NOT-RUN ledger rows with the usual 13 columns" \
    [ "$(awk -F'\t' '$13 ~ /^NOT-RUN / && NF == 13' "$l" | wc -l)" = 5 ]
  check "the to-tip component's NOT-RUN row is noted" \
    grep -qP '\tv-recovery\txfs\t0\t0\t0\t-\t0\t0\t0\t7\tNOT-RUN v-recovery-xfs to_tip=1$' "$l"
  check "the ledger has the running component's TIMEOUT row" \
    grep -qP '\tv-flush\text4\t1\t2\t2\t-\t0\t0\t0\t7\tTIMEOUT v-flush-ext4$' "$l"
  check "the driver stopped the running component" grep -qx 'stop v-flush-ext4' "$T/stub-cut/systemctl.log"
else
  echo "FAIL the slow component never started (see $T/cut.driver.log)"
  cat "$T/cut.driver.log"
  fail=1
fi

# An INT does the same with exit 130. A background job of a script starts with SIGINT ignored,
# which bash cannot trap, so the driver starts with SIGINT back at its default.
week_env int v-flush-ext4
python3 -I -c 'import os, signal, sys
signal.signal(signal.SIGINT, signal.SIG_DFL)
os.execvp(sys.argv[1], sys.argv[1:])' "${run[@]}" > "$T/int.driver.log" 2>&1 &
driver=$!
if wait_for 60 test -e "$T/stub-int/slow.started"; then
  kill -INT "$driver"
  rc=0
  wait "$driver" || rc=$?
  check "an interrupted driver exits 130" [ $rc = 130 ]
  check "its running component writes its TIMEOUT row" \
    wait_for 20 grep -q '^TIMEOUT v-flush-ext4 ' "$T/int/summary.txt"
  check "INCOMPLETE after an INT" grep -qx 'INCOMPLETE not_run=5 unwritten=0' "$T/int/summary.txt"
else
  echo "FAIL the INT week's slow component never started (see $T/int.driver.log)"
  fail=1
fi

# --stop-started, the driver unit's ExecStopPost (invocation inv-1), with systemd's result
# variables; $1 is the record --units left.
# shellcheck disable=SC2329 # run through expect.
stop_started() {
  local state=$T/state-stop log=$T/stub-stop/systemctl.log rc=0
  mkdir -p "$state" "$T/stub-stop"
  rm -f "$log"
  [ -z "$1" ] || echo "$1" > "$state/running"
  shift
  env PATH="$T/bin:$PATH" STUB_DIR="$T/stub-stop" ST3B_VM_STATE="$state" INVOCATION_ID=inv-1 "$@" \
    "$weekly" --stop-started || rc=$?
  [ ! -e "$log" ] || cat "$log"
  return "$rc"
}
unit="inv-1 st3b-vm-weekly@v-flush-xfs.service"
expect "--stop-started after a success" "" 0 stop_started "$unit" SERVICE_RESULT=success
expect "--stop-started after a deferral" "" 0 \
  stop_started "$unit" SERVICE_RESULT=exit-code EXIT_STATUS=75
expect "--stop-started after the cap" "stop v-flush-xfs" 0 stop_started "$unit" SERVICE_RESULT=timeout
check "--stop-started clears the record" test ! -e "$T/state-stop/running"
expect "--stop-started after a failure" "stop v-flush-xfs" 0 \
  stop_started "$unit" SERVICE_RESULT=exit-code EXIT_STATUS=1
expect "--stop-started with nothing recorded" "" 0 stop_started "" SERVICE_RESULT=signal
expect "--stop-started ignores another unit" "" 0 \
  stop_started "inv-1 sshd.service" SERVICE_RESULT=signal
expect "--stop-started ignores another invocation's record" "" 0 \
  stop_started "inv-0 st3b-vm-weekly@v-flush-xfs.service" SERVICE_RESULT=timeout
check "--stop-started clears a stale record" test ! -e "$T/state-stop/running"
expect "--stop-started ignores a record from a shell" "" 0 \
  stop_started "- st3b-vm-weekly@v-flush-xfs.service" SERVICE_RESULT=timeout
expect "--stop-started outside a unit stops nothing" "" 0 \
  stop_started "$unit" SERVICE_RESULT=timeout INVOCATION_ID=
expect "--stop-started ignores an untagged record" "" 0 \
  stop_started st3b-vm-weekly@v-flush-xfs.service SERVICE_RESULT=timeout

[ $fail = 0 ] && echo "all passed"
exit $fail
