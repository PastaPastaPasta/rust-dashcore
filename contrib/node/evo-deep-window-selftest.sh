#!/usr/bin/env bash
# contrib/node/evo-deep-window.sh's self-test: the guard passes only on complete, counted runs.
#
# Usage: contrib/node/evo-deep-window-selftest.sh SCRATCH [CHAIN]
#   With a fake bench and a fake perf (EVO_DEEP_WINDOW_BENCH, EVO_DEEP_WINDOW_PERF): counters
#   perf did not count, missing or extra records, failing, early or stalled children (also
#   stalled on exit), an
#   interrupted guard, and a flush the record shows each give FAIL, leaving no child and no
#   handshake directory; good runs give PASS.
#   With CHAIN (`record.sh deep`'s chain.txt), also the real bench (built by the guard) on CHAIN's
#   first 3,000 blocks with W held at the first full flush after the forced one
#   (EVO_DEEP_WINDOW_HOLD_W): a flush in flight in the segment, its durable block unmoved, must
#   give FAIL.
set -euo pipefail
unset EVO_DEEP_WINDOW_CHAIN

scratch=$(realpath -m "$1")
chain=${2:-}
guard=$(realpath "$(dirname "$0")/evo-deep-window.sh")
mkdir -p "$scratch"
fakes=$scratch/fakes
mkdir -p "$fakes"

# The fake bench: `evo_deep_window`'s handshake and record, shaped by FAKE_BENCH.
cat > "$fakes/bench" << 'EOF'
#!/usr/bin/env bash
set -eu
echo $$ >> "$FAKE_PIDS"
mode=$2 sync=$EVO_DEEP_WINDOW_SYNC
h() { printf '%064x' "$1"; }
if [ -n "${FAKE_SOURCE:-}" ]; then
    [ "$(cat "$1")" = main ] && [ "$(cat "$3")" = side ] || exit 4
fi
[ "$FAKE_BENCH" != early ] || exit 3
echo $$ > "$sync/tid.part"
mv "$sync/tid.part" "$sync/tid"
until [ -e "$sync/go" ]; do sleep 0.05; done
[ "$FAKE_BENCH" != stall ] || exec sleep 1000
touch "$sync/done"
until [ -e "$sync/end" ]; do sleep 0.05; done
f=$(h 3) durable=$(h 3) frozen=0 begins=2
case $FAKE_BENCH in
    no-record) exit 0 ;;
    durable) durable=$(h 4) ;;
    frozen) frozen=1 ;;
    begins) begins=3 ;;
esac
line="segment mode=$mode from=1001 $(h 1) to=2000 $(h 2) flushed_at=1000 $f begins_flushed=2 durable_before=$f frozen_before=0 begins_before=2 durable_after=$durable frozen_after=$frozen begins_after=$begins mined_before=24 tombstones_before=6 mined_expected=24 tombstones_expected=6 wall_ms=5"
case $FAKE_BENCH in
    sparse) echo "${line/mined_before=24/mined_before=22}" ;;
    overwritten) echo "${line/tombstones_before=6/tombstones_before=4}" ;;
    flushed-sparse) echo "${line/begins_before=2/begins_before=3}" | sed 's/mined_before=24/mined_before=22/' ;;
    flushed-varying) echo "${line/begins_before=2/begins_before=3}" | sed "s/mined_before=24/mined_before=$((100 + $(wc -l < "$FAKE_PIDS")))/" ;;
    bad-counts) echo "${line/mined_before=24/mined_before=1}" ;;
    missing-counts) echo "${line/ mined_before=24 tombstones_before=6/}" ;;
    truncated) echo "${line% begins_after=*}" ;;
    two-records) echo "$line"; echo "$line" ;;
    *) echo "$line" ;;
esac
[ "$FAKE_BENCH" != fail-late ] || exit 1
[ "$FAKE_BENCH" != hang-late ] || exec sleep 1000
EOF

# The fake perf: `perf stat -x, -e instructions:u -t TID -D -1 --control fifo:CTL,ACK -o OUT`,
# acking each command and writing OUT on SIGINT, shaped by FAKE_PERF. The count is the bench
# mode's FAKE_<MODE>. Python, as a background shell starts it with SIGINT ignored, which bash
# cannot trap and perf's own handler overrides.
cat > "$fakes/perf" << 'EOF'
#!/usr/bin/env python3
import os, select, signal, sys, time

with open(os.environ["FAKE_PIDS"], "a") as f:
    print(os.getpid(), file=f)
args = sys.argv[1:]
tid = args[args.index("-t") + 1]
out = args[args.index("-o") + 1]
ctl, ack = args[args.index("--control") + 1].removeprefix("fifo:").split(",")
with open(f"/proc/{tid}/cmdline", "rb") as f:
    mode = next(arg.decode() for arg in f.read().split(b"\0") if arg in (b"shallow", b"deep"))
count = int(os.environ[f"FAKE_{mode.upper()}"])
shape = os.environ["FAKE_PERF"]
if shape == "fail-start":
    sys.exit(1)
lines = {
    "not-counted": ["<not counted>,,instructions:u,0,100.00,,"],
    "not-supported": ["<not supported>,,instructions:u,0,100.00,,"],
    "zero": ["0,,instructions:u,1000,100.00,,"],
    "empty": [],
    "hybrid": [
        f"{count // 2},,cpu_core/instructions:u/,1000,100.00,,",
        f"{count - count // 2},,cpu_atom/instructions:u/,1000,100.00,,",
    ],
}.get(shape, [f"{count},,instructions:u,1000,100.00,,"])


def report(*_):
    with open(out, "w") as f:
        print(f"# started on {time.ctime()}\n", file=f)
        for line in lines:
            print(line, file=f)
    if shape == "hang-on-sigint":
        time.sleep(1000)
    sys.exit(1 if shape == "int-exit-1" else 130)


signal.signal(signal.SIGINT, report)
c = os.open(ctl, os.O_RDONLY | os.O_NONBLOCK)
a = os.open(ack, os.O_WRONLY)
while True:
    ready, _, _ = select.select([c], [], [], 0.1)
    data = os.read(c, 64) if ready else b""
    if not data:
        time.sleep(0.05)
        continue
    for cmd in data.decode().split():
        if shape == "no-ack":
            continue
        os.write(a, b"ack\n")
        if shape == "exit-after-enable" and cmd == "enable":
            sys.exit(1)
EOF
chmod +x "$fakes/bench" "$fakes/perf"

alive() {
    local state
    state=$(sed 's/.*) //' "/proc/$1/stat" 2> /dev/null | cut -d' ' -f1) || return 1
    [ -n "$state" ] && [ "$state" != Z ] && [ "$state" != X ]
}

failures=0
# CASE EXPECT WHY [VAR=VALUE...]: the guard with the fakes gives EXPECT (PASS or FAIL) within two
# minutes, its summary matching WHY (an extended regex), leaving no fake running and no handshake
# directory.
check() {
    local name=$1 expect=$2 why=$3 work=$scratch/$1 rc=0 got p
    shift 3
    rm -rf "$work"
    mkdir -p "$work"
    echo fake > "$work/chain.txt"
    : > "$work/pids"
    env FAKE_PIDS="$work/pids" FAKE_BENCH=ok FAKE_PERF=ok FAKE_SHALLOW=180336000 FAKE_DEEP=184901000 \
        EVO_DEEP_WINDOW_BENCH="$fakes/bench" EVO_DEEP_WINDOW_PERF="$fakes/perf" EVO_DEEP_WINDOW_STALL=60 \
        "$@" timeout 120 "$guard" "$work" > "$work/guard.log" 2>&1 || rc=$?
    got=$(tail -n 1 "$work/summary.txt" 2> /dev/null || true)
    local verdict=ok
    if [ "$expect" = PASS ] && { [ "$rc" != 0 ] || [ "$got" != PASS ]; }; then verdict="rc=$rc, last line '$got'"; fi
    if [ "$expect" = FAIL ] && { [ "$rc" != 1 ] || [ "$got" != FAIL ]; }; then verdict="rc=$rc, last line '$got'"; fi
    grep -Eq "$why" "$work/summary.txt" || verdict="$verdict; no '$why' in the summary"
    while read -r p; do
        if alive "$p"; then
            verdict="$verdict; process $p survived"
            kill -KILL "$p"
        fi
    done < "$work/pids"
    if compgen -G "$work/sync.*" > /dev/null; then verdict="$verdict; a handshake directory survived"; fi
    if [ "$verdict" = ok ]; then
        echo "ok   $name: $expect ($(grep -Em 1 "$why" "$work/summary.txt"))"
    else
        echo "BAD  $name: expected $expect, $verdict"
        failures=$((failures + 1))
    fi
}

check good PASS '^ratio 1.0253 '
mkdir -p "$scratch/inputs/plain" "$scratch/inputs/zst" "$scratch/inputs/missing"
printf 'main\n' > "$scratch/inputs/plain/chain.txt"
printf 'side\n' > "$scratch/inputs/plain/fork.txt"
zstd -q -c "$scratch/inputs/plain/chain.txt" > "$scratch/inputs/zst/chain.txt.zst"
zstd -q -c "$scratch/inputs/plain/fork.txt" > "$scratch/inputs/zst/fork.txt.zst"
check source-plain PASS '^ratio 1.0253 ' FAKE_SOURCE=1 EVO_DEEP_WINDOW_CHAIN="$scratch/inputs/plain"
check source-zst PASS '^ratio 1.0253 ' FAKE_SOURCE=1 EVO_DEEP_WINDOW_CHAIN="$scratch/inputs/zst"
check source-sparse FAIL 'index population differs from the recording' FAKE_BENCH=sparse EVO_DEEP_WINDOW_CHAIN="$scratch/inputs/plain"
check source-overwritten FAIL 'index population differs from the recording' FAKE_BENCH=overwritten EVO_DEEP_WINDOW_CHAIN="$scratch/inputs/plain"
check source-flushed-sparse FAIL 'a flush after the forced one:' FAKE_BENCH=flushed-sparse EVO_DEEP_WINDOW_CHAIN="$scratch/inputs/plain"
check source-flushed-varying FAIL '^ratio 1.0253 ' FAKE_BENCH=flushed-varying EVO_DEEP_WINDOW_CHAIN="$scratch/inputs/plain"
check source-missing FAIL 'no recorded chain.txt' EVO_DEEP_WINDOW_CHAIN="$scratch/inputs/missing"
check hybrid PASS '^ratio 1.0253 ' FAKE_PERF=hybrid
check ratio-rounding FAIL '^ratio 1.0500 ' FAKE_DEEP=189355000
check bad-counts FAIL 'more tombstones than mined entries' FAKE_BENCH=bad-counts
check missing-counts FAIL 'an incomplete record' FAKE_BENCH=missing-counts
check slower-deep FAIL '^ratio 1.1090 ' FAKE_DEEP=200000000
check not-counted FAIL 'perf counted nothing valid: +<not counted>' FAKE_PERF=not-counted
check not-supported FAIL 'perf counted nothing valid: +<not supported>' FAKE_PERF=not-supported
check zero-count FAIL 'perf counted nothing valid: +0,' FAKE_PERF=zero
check no-perf-output FAIL 'perf counted nothing valid: *$' FAKE_PERF=empty
check perf-fails-to-start FAIL 'perf exited before its enable ack' FAKE_PERF=fail-start
check perf-exits-mid-segment FAIL '^done: process [0-9]+ exited first' FAKE_PERF=exit-after-enable
check perf-exits-1-on-sigint FAIL 'perf exited 1$' FAKE_PERF=int-exit-1
check perf-never-acks FAIL 'no enable ack in 2s' FAKE_PERF=no-ack EVO_DEEP_WINDOW_STALL=2
check bench-exits-before-tid FAIL '^tid: process [0-9]+ exited first' FAKE_BENCH=early
check bench-stalls FAIL '^done: no progress in 2s' FAKE_BENCH=stall EVO_DEEP_WINDOW_STALL=2
check bench-fails-after-record FAIL 'the bench exited 1' FAKE_BENCH=fail-late
check bench-hangs-after-record FAIL 'the bench: no exit in 2s' FAKE_BENCH=hang-late EVO_DEEP_WINDOW_STALL=2
check perf-hangs-on-sigint FAIL 'perf: no exit in 2s' FAKE_PERF=hang-on-sigint EVO_DEEP_WINDOW_STALL=2
check no-record FAIL '0 segment records, not 1' FAKE_BENCH=no-record
check two-records FAIL '2 segment records, not 1' FAKE_BENCH=two-records
check truncated-record FAIL 'an incomplete record' FAKE_BENCH=truncated
check durable-moved FAIL 'a flush after the forced one: .*durable_after=0+4 ' FAKE_BENCH=durable
check frozen-after FAIL 'a flush after the forced one: .*frozen_after=1 ' FAKE_BENCH=frozen
check begins-moved FAIL 'a flush after the forced one: .*begins_after=3 ' FAKE_BENCH=begins
check no-bench FAIL 'no bench binary' EVO_DEEP_WINDOW_BENCH="$fakes/missing"

# A guard stopped mid-run (SIGTERM; a background shell ignores SIGINT) stops and reaps its
# children and removes its handshake directory.
work=$scratch/terminated
rm -rf "$work"
mkdir -p "$work"
echo fake > "$work/chain.txt"
: > "$work/pids"
FAKE_PIDS="$work/pids" FAKE_BENCH=stall FAKE_PERF=ok FAKE_SHALLOW=1 FAKE_DEEP=1 \
    EVO_DEEP_WINDOW_BENCH="$fakes/bench" EVO_DEEP_WINDOW_PERF="$fakes/perf" \
    "$guard" "$work" > "$work/guard.log" 2>&1 &
g=$!
for _ in $(seq 100); do [ "$(wc -l < "$work/pids")" -ge 2 ] && break; sleep 0.1; done
sleep 0.5
kill -TERM "$g"
rc=0
wait "$g" || rc=$?
verdict=ok
[ "$(wc -l < "$work/pids")" = 2 ] || verdict="$(wc -l < "$work/pids") fakes started, not 2"
[ "$rc" = 143 ] || verdict="$verdict; rc=$rc"
while read -r p; do
    if alive "$p"; then
        verdict="$verdict; process $p survived"
        kill -KILL "$p"
    fi
done < "$work/pids"
if compgen -G "$work/sync.*" > /dev/null; then verdict="$verdict; a handshake directory survived"; fi
if [ "$verdict" = ok ]; then echo "ok   terminated: rc 143, no child left"; else
    echo "BAD  terminated: $verdict"
    failures=$((failures + 1))
fi

if [ -n "$chain" ]; then
    # The real bench on CHAIN's first 3,000 blocks with a 3 MiB -dbcache (and, held, THRESHOLD at a
    # sixteenth of it): deep (flushed at 1,000) freezes once more in its segment, 2,001-3,000, and
    # shallow (flushed at 2,000) does not; 2.5-4 MiB do the same. W is held at that flush, so its
    # layer stays frozen and the durable block the forced flush's to the segment's end: each deep
    # run must fail, on its frozen layer and W's count, with the durable check alone passing it.
    work=$scratch/held-w
    rm -rf "$work"
    mkdir -p "$work"
    awk '/^#/ { print; next } ++n <= 3000' "$chain" > "$work/chain.txt"
    rc=0
    EVO_DEEP_WINDOW_HOLD_W=1 EVO_DEEP_WINDOW_DBCACHE=3072k EVO_DEEP_WINDOW_STALL=300 \
        "$guard" "$work" > "$work/guard.log" 2>&1 || rc=$?
    flagged=$(grep 'a flush after the forced one' "$work/summary.txt" || true)
    # A flagged run the durable check would also catch, or one with no frozen layer or no move in
    # W's count.
    other=$(echo "$flagged" | awk 'NF {
        for (i = 1; i <= NF; i++) {
            if ($i ~ /^flushed_at=/) f = $(i + 1)
            if ($i ~ /^durable_after=/) d = substr($i, 15)
            if ($i ~ /^frozen_after=/) z = substr($i, 14)
            if ($i ~ /^begins_before=/) b = substr($i, 15)
            if ($i ~ /^begins_after=/) a = substr($i, 14)
        }
        if (z != 1 || d != f || a + 0 <= b + 0) print
    }')
    if [ "$rc" = 1 ] && [ "$(tail -n 1 "$work/summary.txt")" = FAIL ] &&
        [ "$(echo "$flagged" | grep -c '^deep run [123]:')" = 3 ] && [ "$(echo "$flagged" | grep -c .)" = 3 ] &&
        [ -z "$other" ]; then
        echo "ok   held-w: FAIL, the 3 deep runs on a frozen layer and W's count, the durable block unmoved"
    else
        echo "BAD  held-w: rc=$rc: $(cat "$work/summary.txt")"
        failures=$((failures + 1))
    fi
fi

if [ "$failures" = 0 ]; then echo "selftest PASS"; else
    echo "selftest FAIL ($failures)"
    exit 1
fi
