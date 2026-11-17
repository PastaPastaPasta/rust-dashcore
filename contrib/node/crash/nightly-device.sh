#!/usr/bin/env bash
# ST3b's device nightly (PLAN-ST3b §10): builds release rdashd and st3b-crash at the checkout's
# HEAD, sweeps stale runs, then runs each component alone and records it.
#
# usage: contrib/node/crash/nightly-device.sh OUT_DIR [COMPONENT...]
#   With no components, runs every one. First kill-smoke, labelled smoke (PLAN-ST3b §1 item 3):
#   rdashd SIGKILLed $ST3B_SMOKE_KILLS times (default 10) on plain disk during W-small's IBD,
#   C1-C4 after each restart, C2 at the tip; no devices, no sudo; its points are its checked kills
#   and its fs is `-`. The others are <scenario>:<fs>, the scenarios being
#   selftest (the device stack on a known file workload) and d-ibd-s (W-small's IBD over P2P and
#   submitblock at -dbcache=4 -dbbatchsize=20000: the night's shard of its flush, FUA, random and
#   torn points, C0-C4 on each, C5 on the marker points and one in 10 others, C6 drawn and
#   capped as below), and S6's fault cases fl-ibd, fl-flush, fl-wal, df-pre and df-mid (one
#   `st3b-crash fault` each on W-small, up to three seeded attempts for coverage; its points are
#   the attempts), the filesystems ext4 and xfs. OUT_DIR gets head.txt, build.log, a log and a time
#   file per component, and summary.txt: the seed, one row per component, then DONE. Each row is
#   also appended to $ST3B_LEDGER: UTC, SHA, component, fs, points, points by class, failures,
#   CPU-s (user + system), seed, status. A campaign's summary row adds the recording's point count
#   (recorded=), C0's mean CPU-s a point (c0=), and C6's runs and the points the cap skipped
#   (c6=RUNS/SKIPPED).
#
#   Not in the default list: d-ibd-40k:ext4, PLAN-ST3b Q8's 40k sampled set, which the weekly
#   st3b-device-40k.service runs (owner ruling, S8). W-40k's IBD over P2P is recorded once; C0 runs
#   on the points of shard (UTC week mod K)/K, and C1-C6 on one in M of those (`campaign --sample`),
#   K = $ST3B_40K_C0_EVERY, M = $ST3B_40K_SAMPLE.
#
# One device run at a time: after the build, the run holds $ST3B_LOCK, waiting up to
# $ST3B_LOCK_WAIT seconds for it, else it records BUSY and exits 75 (no component inherits the
# lock). A TERM, INT or HUP (a unit's timeout, a stop, Ctrl-C) writes a TIMEOUT row and exits 143:
# for the running component, with the points it had checked, after stopping its whole session
# (each component runs under setsid) and sweeping its run, whose directory joins the kept list;
# before any component, a row saying so. With $ST3B_CPU_CAP_S set, a component that takes the run's
# components past that many CPU-s is stopped the same way (a CPU-CAP row), and no later component
# starts.
#
# The exit status is non-zero if the build fails, any component fails (a component that checks no
# point fails), a run leaves a device or mount behind, or a required artifact cannot be written.
# Every component runs regardless. A failed run keeps its directory under /work/scratch/st3b/run
# for the repro; the newest are kept, at most $ST3B_KEEP_FAILED and $ST3B_KEEP_FAILED_G GB, older
# ones removed through $ST3B_RM.
#
# env:
#   ST3B_SEED        the points' seed; default: the UTC date (YYYYMMDD), so it rotates nightly.
#   ST3B_SHARD       the d-ibd-s shard I/N (DEC-718); default: (UTC day mod 24)/24, so every point
#                    index is checked once in 24 nights at <= 2 CPU-h a night. 0/1: every point.
#   ST3B_DIBD_C6_MID_EVERY  d-ibd-s's `campaign --c6-mid-every`, default 20: C6 on one in this
#                    many marker points, drawn from the seed. Under the production storage profile
#                    most of the shard is marker points, each C6 about 350 recovery points
#                    (0.12-0.17 CPU-h, 17-35 min wall, 11-04). 1: every one. Points are checked
#                    in recording order, so a binding cap favours early ones; at 20 it rarely binds.
#   ST3B_DIBD_C6_MAX d-ibd-s's `campaign --c6-max`, default 2: at most this many C6 runs a
#                    filesystem a night, the bound on C6's wall time. Every point keeps C0-C4.
#   ST3B_DASHD       the fixtures' dashd (default: the pinned v24.0.0-rc.4 under ~/.rust-dashcore-test)
#   ST3B_FIXTURES    the fixture directory (default: /work/scratch/st3b/fixtures)
#   ST3B_BIN_DIR     skip the build and run rdashd and st3b-crash from this directory
#   ST3B_DRY_RUN=1   no devices, no sudo: campaigns run with --dry-run, selftests and fault cases
#                    and the kill smoke are skipped.
#                    For the runner's own test; a dry night is never a passing night's evidence.
#   ST3B_LEDGER      default: /work/scratch/st3b/nightly/ledger.tsv
#   ST3B_KEEP_FAILED default 3
#   ST3B_KEEP_FAILED_G default 10 (GB, du of the kept run directories together)
#   ST3B_RM          default: /work/scratch/mgr/rm-if-idle.sh (one directory per call)
#   ST3B_SMOKE_KILLS the kill smoke's kills, default 10
#   ST3B_RUNS        the run directory root, default /work/scratch/st3b/run (the runner's own test)
#   ST3B_KEPT        the list of kept failure dirs, default: kept.txt beside $ST3B_LEDGER. The 40k
#                    unit shares the nightly's, so one 3-dir/10G cap covers both.
#   ST3B_40K_C0_EVERY  K above, default 4: PROVISIONAL until the owner's first measured run
#   ST3B_40K_SAMPLE  M above, default 10: PROVISIONAL likewise
#   ST3B_MIN_FREE_G  a heavy start (the 40k unit: 260): the run starts only with this many GB free
#                    on /work and a 5-minute load under $ST3B_MAX_LOAD (default 80), else it records
#                    DEFERRED and exits 75 (the unit tries again later). Unset: no check.
#   ST3B_CPU_CAP_S   the cap above (the 40k unit: 14400), counted on the run's own systemd service
#                    cgroup from the first component on; not enforced elsewhere (a hand run's
#                    cgroup is its session's scope, shared): use `systemd-run --user` for it
#   ST3B_LOCK        default /work/scratch/st3b/device.lock
#   ST3B_LOCK_WAIT   default 14400
set -u

COMPONENTS=(kill-smoke selftest:ext4 selftest:xfs d-ibd-s:ext4 d-ibd-s:xfs)
for f in fl-ibd fl-flush fl-wal df-pre df-mid; do COMPONENTS+=("$f:ext4" "$f:xfs"); done
RUNS=${ST3B_RUNS:-/work/scratch/st3b/run}
# rdashd's arguments beyond the fixture's chain, for every component.
NODE_ARGS=(-dbcache=4 -dbbatchsize=20000)

if [ $# -lt 1 ]; then
  sed -n '2,/^set -u$/p' "$0" | sed '$d' >&2
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
KEPT=${ST3B_KEPT:-$(dirname "$LEDGER")/kept.txt}

# One ledger row: scenario, fs, points, points by class, failures, CPU-s, status.
ledger() {
  if ! printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$(date -u +%FT%TZ)" "$SHA" \
      "$1" "$2" "$3" "$4" "$5" "$6" "$SEED" "$7$([ "$DRY" = 1 ] && echo -dry)" >> "$LEDGER"; then
    echo "nightly-device: cannot append to $LEDGER" >&2
    unwritten=$((unwritten + 1))
  fi
}

failed=0
CHILD=
LAST=
# A TERM, INT or HUP: the running component stopped and recorded, or a row saying where the run
# was. Further signals are ignored while it does that (systemd still KILLs at TimeoutStopSec).
on_stop() {
  trap '' TERM INT HUP
  if [ -n "$CHILD" ]; then
    stop_component
    stopped TIMEOUT
  else
    local where="before any component ran"
    [ -n "$LAST" ] && where="after $LAST (between components, or in the leak check or prune)"
    report "TIMEOUT $where"
    ledger - - 0 - 0 0 "TIMEOUT $where"
  fi
  report "DONE failed=$((failed + 1)) unwritten=$unwritten (stopped)"
  exit 143
}
trap on_stop TERM INT HUP

# A heavy start ($ST3B_MIN_FREE_G set): before the build, and again once the lock is held, since
# its wait can be hours.
heavy_start() {
  [ -n "${ST3B_MIN_FREE_G:-}" ] && [ "$DRY" != 1 ] || return 0
  local free load
  free=$(df -BG --output=avail /work | tail -1 | tr -dc 0-9)
  load=$(cut -d' ' -f2 /proc/loadavg)
  if [ "${free:-0}" -lt "$ST3B_MIN_FREE_G" ] ||
    ! awk -v l="$load" -v m="${ST3B_MAX_LOAD:-80}" 'BEGIN { exit !(l < m) }'; then
    report "DEFERRED free=${free}G load5=$load"
    ledger - - 0 - 0 0 "DEFERRED free=${free}G load5=$load"
    exit 75
  fi
}
heavy_start

if ! echo "$SHA" > "$OUT/head.txt"; then
  unwritten=$((unwritten + 1))
fi
if [ -n "${ST3B_BIN_DIR:-}" ]; then
  BIN=$(realpath -m "$ST3B_BIN_DIR")
else
  if ! (cd "$ROOT" && cargo build --release -p rdashd -p dash-testkit --features dash-testkit/crash \
      --bin rdashd --bin st3b-crash > "$OUT/build.log" 2>&1); then
    report "BUILD FAILED (build.log)"
    ledger - - 0 - 0 0 BUILD-FAILED
    exit 1
  fi
  BIN=$ROOT/target/release
fi
CRASH=$BIN/st3b-crash
for b in "$CRASH" "$BIN/rdashd"; do
  if [ ! -x "$b" ]; then
    report "NO BINARY $b"
    ledger - - 0 - 0 0 "NO BINARY $b"
    exit 1
  fi
done
export ST3B_PRIV=$ROOT/contrib/node/crash/st3b-priv

if [ "$DRY" != 1 ]; then
  LOCK=${ST3B_LOCK:-/work/scratch/st3b/device.lock}
  exec 9>> "$LOCK"
  if ! flock -w "${ST3B_LOCK_WAIT:-14400}" 9; then
    report "BUSY $LOCK"
    ledger - - 0 - 0 0 "BUSY $LOCK"
    exit 75
  fi
  heavy_start
fi

# CPU from the run's cgroup, when that is its own systemd service's (a hand run's is its session's
# scope, shared with whatever else runs there): the cap, and a stopped component's CPU-s.
CAP=${ST3B_CPU_CAP_S:-}
CG=/sys/fs/cgroup$(sed -n 's/^0:://p' /proc/self/cgroup 2> /dev/null)
OWN_CG=
[[ $CG == *.service ]] && [ -r "$CG/cpu.stat" ] && OWN_CG=1
cpu_used() { awk '/^usage_usec / { printf "%d", $2 / 1000000 }' "$CG/cpu.stat"; }
if [ -n "$CAP" ] && [ -z "$OWN_CG" ]; then
  report "CPU CAP NOT ENFORCED: not a systemd service of its own"
  CAP=
fi
CPU0=$([ -n "$OWN_CG" ] && cpu_used)

# The points a component had checked when it was stopped: a campaign's point lines, a fault
# case's attempts, the kill smoke's kills so far.
partial_points() {
  case $scenario in
    kill-smoke) sed -nE 's/^lap [0-9]+: [0-9]+ kills \(([0-9]+) so far\).*/\1/p' "$log" | tail -1 ;;
    fl-* | df-*) grep -c '^run ' "$log" ;;
    *) grep -c '^point [0-9]' "$log" ;;
  esac
}

# Stops the running component: TERM to its session (it runs under setsid, so rdashd and the peer
# dashd go too), up to 60 s for every live process in it to end (rdashd's own shutdown), then
# KILL. A live rdashd holds the mount, so the sweep comes only after this.
stop_component() {
  kill -TERM -- "-$CHILD" 2> /dev/null
  local i
  for ((i = 0; i < 60; i++)); do
    # Live members only: a zombie (the reaped-later time process) is not.
    pgrep -s "$CHILD" -r R,S,D,T,t,I > /dev/null || break
    sleep 1
  done
  kill -KILL -- "-$CHILD" 2> /dev/null
  wait "$CHILD" 2> /dev/null
}

# A component stopped part way (TIMEOUT, CPU-CAP): its live run (a fault case's last attempt)
# swept, its directory listed for the prune (it printed no `kept` line), a row with what it had.
stopped() {
  local id pts cpu=-
  id=$(grep -oE '^run [0-9]+-[0-9]+' "$log" 2> /dev/null | tail -1 | cut -d' ' -f2)
  if [ -n "$id" ]; then
    [ "$DRY" = 1 ] || "$CRASH" sweep "$id" >> "$OUT/sweep.log" 2>&1
    [ -d "$RUNS/$id" ] && echo "$RUNS/$id" >> "$KEPT"
  fi
  [ -n "$OWN_CG" ] && cpu=$(($(cpu_used) - COMPONENT_CPU0))
  pts=$(partial_points)
  report "$1 $c$(campaign_fields) points=${pts:-0} cpu=${cpu}$([ "$cpu" = - ] || echo s) status=$1"
  ledger "$scenario" "$fs" "${pts:-0}" - "$(grep -c '^FAIL ' "$log")" "$cpu" "$1"
}

# A campaign's recording size and C0's CPU-s a point: the final line's, else the point lines'.
campaign_fields() {
  [ "${args[0]:-}" = campaign ] || return 0
  local rec c0
  rec=$(sed -nE 's/^recorded [0-9]+ log entries, ([0-9]+) points.*/\1/p' "$log" | tail -1)
  c0=$(sed -nE 's/.*C0 ([0-9.]+) CPU-s a point$/\1/p' "$log" | tail -1)
  [ -n "$c0" ] || c0=$(grep -oE 'C0 [0-9.]+ CPU-s\)' "$log" |
    awk '{ s += $2; n++ } END { if (n) printf "%.2f", s / n }')
  printf ' recorded=%s c0=%s' "${rec:--}" "${c0:+${c0}s}"
  [ -n "$c0" ] || printf -- -
  local c6
  c6=$(sed -nE 's/.*C6 runs ([0-9]+) \([0-9]+ points\), C6 skipped ([0-9]+),.*/\1\/\2/p' "$log" | tail -1)
  printf ' c6=%s' "${c6:--}"
}

# A run killed on an earlier night leaves its devices: tear them down before anything starts.
if [ "$DRY" != 1 ] && ! "$CRASH" sweep --stale > "$OUT/sweep.log" 2>&1; then
  report "SWEEP FAILED (sweep.log)"
  failed=$((failed + 1))
fi

for c in "$@"; do
  scenario=${c%%:*}
  fs=-
  [[ $c == *:* ]] && fs=${c#*:}
  log=$OUT/$c.log
  args=()
  case $scenario in
    kill-smoke)
      args=(kill-smoke "$FIXTURES/w-small" --rdashd "$BIN/rdashd" --dashd "$DASHD"
        --kills "${ST3B_SMOKE_KILLS:-10}" --seed "$SEED" -- "${NODE_ARGS[@]}")
      ;;
    selftest) args=(selftest --fs "$fs" --seed "$SEED") ;;
    d-ibd-s)
      k=${ST3B_DIBD_C6_MID_EVERY:-20} n=${ST3B_DIBD_C6_MAX:-2}
      if ! [[ $k =~ ^[1-9][0-9]{0,5}$ && $n =~ ^[0-9]{1,6}$ ]]; then
        report "- - - $c status=BAD-ENV ST3B_DIBD_C6_MID_EVERY=$k ST3B_DIBD_C6_MAX=$n"
        ledger "$scenario" "$fs" 0 - 0 0 "BAD-ENV ST3B_DIBD_C6_MID_EVERY=$k ST3B_DIBD_C6_MAX=$n"
        failed=$((failed + 1))
        continue
      fi
      args=(campaign "$FIXTURES/w-small" --rdashd "$BIN/rdashd" --dashd "$DASHD" --fs "$fs"
        --seed "$SEED" --shard "$SHARD" --c6-mid-every "$k" --c6-max "$n")
      [ "$DRY" = 1 ] && args+=(--dry-run)
      args+=(-- "${NODE_ARGS[@]}")
      ;;
    d-ibd-40k)
      k=${ST3B_40K_C0_EVERY:-4} m=${ST3B_40K_SAMPLE:-10}
      if ! [[ $k =~ ^[1-9][0-9]*$ && $m =~ ^[1-9][0-9]*$ ]]; then
        report "- - - $c status=BAD-ENV ST3B_40K_C0_EVERY=$k ST3B_40K_SAMPLE=$m"
        ledger "$scenario" "$fs" 0 - 0 0 "BAD-ENV ST3B_40K_C0_EVERY=$k ST3B_40K_SAMPLE=$m"
        failed=$((failed + 1))
        continue
      fi
      shard40k=$(($(date -u +%s) / 604800 % k))/$k
      report "$c: C0 on shard $shard40k, C1-C6 on 1 in $m of those (K and M provisional)"
      args=(campaign "$FIXTURES/w-40k" --rdashd "$BIN/rdashd" --dashd "$DASHD" --fs "$fs"
        --seed "$SEED" --shard "$shard40k" --sample "$m")
      [ "$DRY" = 1 ] && args+=(--dry-run)
      args+=(-- "${NODE_ARGS[@]}")
      ;;
    fl-ibd | fl-flush | fl-wal | df-pre | df-mid)
      args=(fault "$FIXTURES/w-small" --case "${scenario^^}" --rdashd "$BIN/rdashd" --dashd "$DASHD"
        --fs "$fs" --seed "$SEED" -- "${NODE_ARGS[@]}")
      ;;
    *)
      report "- - - $c status=UNKNOWN"
      failed=$((failed + 1))
      continue
      ;;
  esac
  if [ "$DRY" = 1 ] && [ "${args[0]}" != campaign ]; then
    report "- - - $c status=SKIPPED-DRY"
    continue
  fi
  # A reused OUT_DIR's old times must not stand for this run's.
  rm -f "$OUT/$c.time" "$OUT/$c.cap"
  # In the background, so a TERM is handled at once (bash defers traps until a foreground child
  # exits), and so the CPU cap can watch it; in a session of its own (setsid execs in place: a
  # background child is no group leader, so $! is the session), so a stop reaches rdashd and the
  # peer too; without fd 9, so nothing it leaves behind holds the device lock.
  COMPONENT_CPU0=$([ -n "$OWN_CG" ] && cpu_used)
  setsid /usr/bin/time -f "%U %S %e" -o "$OUT/$c.time" "$CRASH" "${args[@]}" > "$log" 2>&1 9>&- &
  CHILD=$! LAST=$c
  watcher=
  if [ -n "$CAP" ]; then
    (
      while kill -0 "$CHILD" 2> /dev/null; do
        if [ $(($(cpu_used) - CPU0)) -gt "$CAP" ]; then
          : > "$OUT/$c.cap"
          kill -TERM -- "-$CHILD"
          exit
        fi
        sleep 30
      done
    ) 9>&- &
    watcher=$!
  fi
  wait "$CHILD"
  rc=$?
  [ -n "$watcher" ] && kill "$watcher" 2> /dev/null
  if [ -f "$OUT/$c.cap" ]; then
    # Its own stop: a signal now would record it twice.
    trap '' TERM INT HUP
    stop_component
    stopped CPU-CAP
    CHILD=
    failed=$((failed + 1))
    report "CPU CAP ${CAP} CPU-s reached: no later component starts"
    trap on_stop TERM INT HUP
    break
  fi
  CHILD=
  times=$(tail -1 "$OUT/$c.time" 2> /dev/null)
  points=$(grep -oE 'points [0-9]+' "$log" | tail -1 | cut -d' ' -f2)
  [ "${args[0]}" = fault ] && points=$(grep -c '^run ' "$log")
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
  label=
  [ "$scenario" = kill-smoke ] && label=" [smoke]"
  report "${times:-- - -} $c$label$(campaign_fields) exit=$rc points=${points:-0} failures=$fails status=$status"
  cpu=$(echo "${times:-0 0 0}" | awk '{printf "%.0f", $1 + $2}')
  ledger "$scenario" "$fs" "${points:-0}" "${kinds:--}" "$fails" "$cpu" "$status"
  kept=$(grep -oE "^kept $RUNS/[0-9]+-[0-9]+" "$log" | cut -d' ' -f2)
  [ -n "$kept" ] && echo "$kept" >> "$KEPT"
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

# The newest failed runs stay for their repros, at most $ST3B_KEEP_FAILED of them and
# $ST3B_KEEP_FAILED_G GB together (PLAN-ST3b §10: 3 and 10G); older ones go, each a directory of
# ours, through $ST3B_RM. From the newest back, runs stay while both caps hold; the first that
# does not fit goes, and every older one with it. The newest always stays (its repro), and a
# summary line says so when it alone is over the size cap.
if [ -f "$KEPT" ]; then
  keep=${ST3B_KEEP_FAILED:-3}
  cap_g=${ST3B_KEEP_FAILED_G:-10}
  listed=()
  while read -r d; do
    [[ $d =~ ^$RUNS/[0-9]+-[0-9]+$ ]] && [ -d "$d" ] && listed+=("$d")
  done < "$KEPT"
  stay=() drop=() used=0 full=0
  for ((i = ${#listed[@]} - 1; i >= 0; i--)); do
    d=${listed[i]}
    g=$(du -s -BG -- "$d" 2> /dev/null | cut -f1 | tr -dc 0-9)
    if [ ${#stay[@]} -eq 0 ] ||
      { [ $full = 0 ] && [ ${#stay[@]} -lt "$keep" ] && [ $((used + ${g:-0})) -le "$cap_g" ]; }; then
      stay=("$d" "${stay[@]}")
      used=$((used + ${g:-0}))
    else
      full=1
      drop+=("$d")
    fi
  done
  held=()
  for d in "${drop[@]}"; do
    # A run that still has devices or mounts stays, and stays listed: removing its directory
    # would leave an orphan the sweep refuses (rm-if-idle sees processes, not loop devices).
    if [ "$DRY" != 1 ] && ! "$CRASH" leaks "${d##*/}" >> "$OUT/prune.log" 2>&1; then
      echo "kept $d: it still has devices or mounts" >> "$OUT/prune.log"
      held+=("$d")
      continue
    fi
    "${ST3B_RM:-/work/scratch/mgr/rm-if-idle.sh}" "$d" >> "$OUT/prune.log" 2>&1
  done
  { [ ${#held[@]} -eq 0 ] || printf '%s\n' "${held[@]}"
    [ ${#stay[@]} -eq 0 ] || printf '%s\n' "${stay[@]}"; } > "$KEPT.new" && mv "$KEPT.new" "$KEPT"
  report "kept failure dirs: ${#stay[@]} (${used}G), held ${#held[@]}, removed $((${#drop[@]} - ${#held[@]}))"
  [ "$used" -gt "$cap_g" ] && report "KEPT OVER CAP: the newest failure dir alone is ${used}G (cap ${cap_g}G)"
fi

report "DONE failed=$failed unwritten=$unwritten"
[ $failed -eq 0 ] && [ $unwritten -eq 0 ]
