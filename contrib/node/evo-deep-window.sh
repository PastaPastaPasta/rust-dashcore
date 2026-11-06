#!/usr/bin/env bash
# CH7 §7.1's `evo-deep-window` growth guard (D22, DEC-527 6): user-space instructions per block of
# dashd-chainstate/benches/evo_deep_window.rs's segment, the chain's last 1,000 blocks, connected on an
# empty evo window (shallow: flushed just before) and on a deep one (deep: flushed at 1,000, about
# 19,000 blocks unflushed). Each configuration is the median of 3 fresh processes, counted on V's
# thread alone around the segment. Passes when deep costs at most 1.05 x shallow and no run
# flushed after its forced flush: the coins' durable block stayed the forced flush's, no layer was
# frozen at either end of the segment, and W began no full flush.
#
# Any failure fails the guard: a bench or perf that fails, stalls or leaves no record, a counter
# perf did not count, or a run without exactly one complete record. contrib/node/evo-deep-window-selftest.sh
# checks that.
#
# Usage: contrib/node/evo-deep-window.sh WORKDIR [BLOCKS]
#   WORKDIR keeps the chain, `record.sh deep`'s BLOCKS (default 20100) from dashd v24.0.0-rc.4
#   (reused when WORKDIR/chain.txt exists), each run's output, and summary.txt.
# Needs perf with access to the PMU (perf_event_paranoid <= 2) and its --control.
# env:
#   EVO_DEEP_WINDOW_DBCACHE  passed to the bench (its -dbcache: MiB, or KiB with a `k` suffix)
#   EVO_DEEP_WINDOW_HOLD_W   passed to the bench (its self-test hold)
#   EVO_DEEP_WINDOW_STALL    seconds a live bench or perf may take to reach each handshake or
#                            to exit after it
#                            (default 3600)
#   EVO_DEEP_WINDOW_BENCH    the bench binary (default: built by cargo)
#   EVO_DEEP_WINDOW_PERF     perf (default: perf)
set -euo pipefail

work=$(realpath -m "$1")
blocks=${2:-20100}
stall=${EVO_DEEP_WINDOW_STALL:-3600}
perf=${EVO_DEEP_WINDOW_PERF:-perf}
cd "$(dirname "$0")/../.."
mkdir -p "$work"
summary=$work/summary.txt
: > "$summary"

# This invocation's bench and perf, and its handshake directory: stopped, reaped and removed on
# any exit.
children=()
sync=
cleanup() {
    local p
    for p in "${children[@]}"; do kill -KILL "$p" 2> /dev/null || true; done
    for p in "${children[@]}"; do wait "$p" 2> /dev/null || true; done
    children=()
    if [ -n "$sync" ]; then rm -rf "$sync"; fi
    sync=
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

die() {
    echo "$*" | tee -a "$summary"
    echo FAIL | tee -a "$summary"
    exit 1
}

if [ ! -s "$work/chain.txt" ]; then
    dashd-node/tests/data/ch7a4/record.sh deep "$work/dashd" "$work/chain.part" "$blocks" || die "the recording failed"
    mv "$work/chain.part" "$work/chain.txt"
fi
bin=${EVO_DEEP_WINDOW_BENCH:-}
if [ -z "$bin" ]; then
    cargo bench -q -p dashd-chainstate --bench evo_deep_window --no-run --message-format=json > "$work/build.json" ||
        die "the bench build failed"
    bin=$(python3 -c 'import json, sys
for line in open(sys.argv[1]):
    m = json.loads(line)
    if m.get("reason") == "compiler-artifact" and m["target"]["name"] == "evo_deep_window" and m["executable"]:
        print(m["executable"])' "$work/build.json")
fi
[ -x "$bin" ] || die "no bench binary: '$bin'"

# PID runs (a reaped or zombie process does not).
alive() {
    local state
    state=$(sed 's/.*) //' "/proc/$1/stat" 2> /dev/null | cut -d' ' -f1) || return 1
    [ -n "$state" ] && [ "$state" != Z ] && [ "$state" != X ]
}

# Waits for FILE while each PID runs, at most EVO_DEEP_WINDOW_STALL seconds.
wait_for() {
    local file=$1 p
    shift
    local deadline=$((SECONDS + stall))
    until [ -e "$file" ]; do
        for p in "$@"; do alive "$p" || die "$(basename "$file"): process $p exited first"; done
        [ "$SECONDS" -lt "$deadline" ] || die "$(basename "$file"): no progress in ${stall}s"
        sleep 0.1
    done
}

# Waits for PID to exit, at most EVO_DEEP_WINDOW_STALL seconds, and sets `status` to its exit status.
reap() {
    local pid=$1 what=$2 deadline=$((SECONDS + stall))
    while alive "$pid"; do
        [ "$SECONDS" -lt "$deadline" ] || die "$what: no exit in ${stall}s"
        sleep 0.1
    done
    status=0
    wait "$pid" || status=$?
}

# Sends perf's control fd CMD and waits for its ack, while perf runs.
control() {
    local cmd=$1 reply deadline=$((SECONDS + stall))
    echo "$cmd" >&"$ctl"
    until read -r -t 1 -u "$ack" reply; do
        alive "$perf_pid" || die "perf exited before its $cmd ack"
        [ "$SECONDS" -lt "$deadline" ] || die "perf: no $cmd ack in ${stall}s"
    done
    [ "$reply" = ack ] || die "perf answered $cmd with '$reply'"
}

# The run's record, its one `segment` line, parsed into `rec`.
record_re='^segment mode=([a-z]+) from=([0-9]+ [0-9a-f]{64}) to=([0-9]+ [0-9a-f]{64}) flushed_at=[0-9]+ ([0-9a-f]{64}) begins_flushed=([0-9]+) durable_before=([0-9a-f]{64}) frozen_before=([01]) begins_before=([0-9]+) durable_after=([0-9a-f]{64}) frozen_after=([01]) begins_after=([0-9]+) wall_ms=[0-9]+$'
declare -A rec
parse_record() {
    local out=$1 mode=$2 lines
    lines=$(grep -c '^segment ' "$out" || true)
    [ "$lines" = 1 ] || die "$(basename "$out"): $lines segment records, not 1"
    [[ "$(grep '^segment ' "$out")" =~ $record_re ]] || die "$(basename "$out"): an incomplete record: $(grep '^segment ' "$out")"
    [ "${BASH_REMATCH[1]}" = "$mode" ] || die "$(basename "$out"): mode ${BASH_REMATCH[1]}, not $mode"
    rec=(
        [segment]="${BASH_REMATCH[2]} ${BASH_REMATCH[3]}" [flushed]=${BASH_REMATCH[4]} [begins_flushed]=${BASH_REMATCH[5]}
        [durable_before]=${BASH_REMATCH[6]} [frozen_before]=${BASH_REMATCH[7]} [begins_before]=${BASH_REMATCH[8]}
        [durable_after]=${BASH_REMATCH[9]} [frozen_after]=${BASH_REMATCH[10]} [begins_after]=${BASH_REMATCH[11]}
    )
}

# perf's count: every instructions line a positive integer. Hybrid CPUs report one line per core
# type (cpu_core/, cpu_atom/): summed.
perf_count() {
    awk -F, '
        /^#/ || /^$/ { next }
        $3 ~ /instructions/ { if ($1 !~ /^[0-9]+$/ || $1 + 0 <= 0) bad = 1; n += $1; lines++ }
        END { if (bad || !lines) exit 1; printf "%d\n", n }' "$1"
}

# One fresh process in MODE, run N: sets `count` to its instructions:u per segment block and `rec`
# to its record. Its output and perf's go to WORKDIR/MODE-N.{out,err,perf}.
count=
run() {
    local mode=$1 n=$2 pid perf_pid ctl ack tid status total
    local out=$work/$mode-$n.out perf_out=$work/$mode-$n.perf
    rm -f "$out" "$perf_out"
    sync=$(mktemp -d "$work/sync.XXXXXX")
    mkfifo "$sync/ctl" "$sync/ack"
    EVO_DEEP_WINDOW_SYNC=$sync "$bin" "$work/chain.txt" "$mode" > "$out" 2> "$work/$mode-$n.err" &
    pid=$!
    children+=("$pid")
    wait_for "$sync/tid" "$pid"
    tid=$(cat "$sync/tid")
    [[ "$tid" =~ ^[0-9]+$ ]] || die "$mode run $n: a bad thread id '$tid'"
    # Read-write ends: neither open blocks, and perf's opens find a peer.
    exec {ctl}<> "$sync/ctl" {ack}<> "$sync/ack"
    "$perf" stat -x, -e instructions:u -t "$tid" -D -1 --control "fifo:$sync/ctl,$sync/ack" -o "$perf_out" \
        {ctl}>&- {ack}>&- 2>> "$work/$mode-$n.err" &
    perf_pid=$!
    children+=("$perf_pid")
    # Counting from perf's ack of `enable`, on V's thread alone, to its ack of `disable`.
    control enable
    touch "$sync/go"
    wait_for "$sync/done" "$pid" "$perf_pid"
    control disable
    exec {ctl}>&- {ack}>&-
    kill -INT "$perf_pid"
    reap "$perf_pid" "$mode run $n: perf"
    # perf stat exits on the SIGINT it was sent.
    [ "$status" = 0 ] || [ "$status" = 130 ] || die "$mode run $n: perf exited $status"
    touch "$sync/end"
    reap "$pid" "$mode run $n: the bench"
    [ "$status" = 0 ] || die "$mode run $n: the bench exited $status: $(tail -n 3 "$work/$mode-$n.err")"
    children=()
    rm -r "$sync"
    sync=
    total=$(perf_count "$perf_out") || die "$mode run $n: perf counted nothing valid: $(grep -v '^#' "$perf_out" | tr '\n' ' ')"
    parse_record "$out" "$mode"
    count=$(awk -v t="$total" 'BEGIN { printf "%.3f\n", t / 1000 }')
}

# No flush after the forced one: the coins' durable block is the forced flush's, no layer is
# frozen (a flush V submitted that W has not released), and W began no full flush.
flush_ok() {
    [ "${rec[flushed]}" = "${rec[durable_before]}" ] && [ "${rec[flushed]}" = "${rec[durable_after]}" ] &&
        [ "${rec[frozen_before]}" = 0 ] && [ "${rec[frozen_after]}" = 0 ] &&
        [ "${rec[begins_flushed]}" = "${rec[begins_before]}" ] && [ "${rec[begins_flushed]}" = "${rec[begins_after]}" ]
}

median3() { printf '%s\n' "$@" | sort -g | sed -n 2p; }

fail=0
segment=
declare -A per_block
for mode in shallow deep; do
    counts=()
    for n in 1 2 3; do
        run "$mode" "$n"
        counts+=("$count")
        if ! flush_ok; then
            echo "$mode run $n: a flush after the forced one: $(grep '^segment ' "$work/$mode-$n.out")" | tee -a "$summary"
            fail=1
        fi
        # Every run measured the same blocks.
        : "${segment:=${rec[segment]}}"
        [ "${rec[segment]}" = "$segment" ] || die "$mode run $n measured ${rec[segment]}, not $segment"
    done
    per_block[$mode]=$(median3 "${counts[@]}")
    printf '%-7s %12.0f instructions:u per block (runs %s)\n' "$mode" "${per_block[$mode]}" "${counts[*]}" | tee -a "$summary"
done
ratio=$(awk -v d="${per_block[deep]}" -v s="${per_block[shallow]}" 'BEGIN { printf "%.4f\n", d / s }')
echo "ratio $ratio (pass at <= 1.05)" | tee -a "$summary"
awk -v r="$ratio" 'BEGIN { exit !(r <= 1.05) }' || fail=1
if [ "$fail" = 0 ]; then echo PASS | tee -a "$summary"; else echo FAIL | tee -a "$summary"; exit 1; fi
