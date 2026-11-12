#!/usr/bin/env bash
# P3 §6.7: testnet IBD from public peers to -stopatheight=H (docs/node-program/designs/
# P3-block-download.md §6.7, P3-notes D112/D113).
#
# Usage: contrib/node/p3-ibd/run.sh [--run NAME] [--height H] [--expect HASH] [--rdashd PATH]
#                                   [--timeout SECONDS] [--memory-max SIZE]
#
# Starts itself as `systemd-run --user --unit=p3-ibd-<run> --same-dir --collect --wait` with a
# MemoryMax and a RuntimeMaxSec, so the node outlives this shell and is bounded either way; follow
# it with `journalctl --user -u p3-ibd-<run>`. One run at a time (the unit name is the lock).
#
# Pure reads against public peers: rdashd does not listen, uses no -connect (DNS seeds, fixed
# seeds and addrman), and never touches /work/chains or /work/bench. The hash at H is pinned below
# from the testnet reference oracle (`contrib/oracle-cli testnet getblockhash 6500`, one pure read,
# Dash Core 24.0.0 on 2026-10-20); --expect pins another H.
#
# Pass: rdashd exits 0, its last tip is at or above H, and the hash at H is the pinned one. The
# summary (<datadir>.json) also has the per-peer `Requesting block` counts, the stalls, the block
# and header timeouts, the misbehaviour lines and the wall time, all from debug.log: rdashd has no
# `getpeerinfo` or `getblockhash` yet (ND1, DEC-279).
set -euo pipefail

PINNED_HEIGHT=6500
PINNED_HASH=000000028268e1d23b65ae751749563402003635fa4bc76bef9cab38b0f657bc

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
run="$(date -u +%Y%m%dT%H%M%SZ)"
height=$PINNED_HEIGHT
expect=""
rdashd="$root/target/release/rdashd"
timeout=7200
memory_max=8G
in_unit=0
while [ $# -gt 0 ]; do
    case "$1" in
        --run) run="$2"; shift 2 ;;
        --height) height="$2"; shift 2 ;;
        --expect) expect="$2"; shift 2 ;;
        --rdashd) rdashd="$2"; shift 2 ;;
        --timeout) timeout="$2"; shift 2 ;;
        --memory-max) memory_max="$2"; shift 2 ;;
        --in-unit) in_unit=1; shift ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done
if [ -z "$expect" ]; then
    if [ "$height" != "$PINNED_HEIGHT" ]; then
        echo "no pinned hash for height $height: pass --expect" >&2
        exit 2
    fi
    expect=$PINNED_HASH
fi
base=/work/scratch/p3/ibd
datadir="$base/$run"

if [ "$in_unit" = 0 ]; then
    [ -x "$rdashd" ] || { echo "no rdashd at $rdashd (cargo build --release -p rdashd)" >&2; exit 2; }
    mkdir -p "$base"
    chmod 0700 "$base"
    exec systemd-run --user --unit="p3-ibd-$run" --same-dir --collect --wait --quiet \
        -p MemoryMax="$memory_max" -p RuntimeMaxSec="$timeout" \
        "$root/contrib/node/p3-ibd/run.sh" --in-unit --run "$run" --height "$height" \
        --expect "$expect" --rdashd "$rdashd"
fi

if [ -e "$datadir" ]; then
    echo "$datadir exists: pick another --run" >&2
    exit 2
fi
mkdir -m 0700 "$datadir"
argv=(-testnet "-datadir=$datadir" -listen=0 "-stopatheight=$height" -debug=net -logips=0
      -printtoconsole=0)
echo "p3-ibd $run: $rdashd ${argv[*]}"
start=$(date +%s.%N)
set +e
"$rdashd" "${argv[@]}"
status=$?
set -e
end=$(date +%s.%N)

python3 -I - "$datadir/testnet3/debug.log" "$height" "$expect" "$status" "$start" "$end" \
    "${argv[*]}" > "$datadir.json" <<'EOF'
import collections, json, re, sys

log, height, expect, status, start, end, argv = sys.argv[1:]
height, status = int(height), int(status)
update_tip = re.compile(r"UpdateTip: new best=(\w+) height=(\d+) ")
requesting = re.compile(r"Requesting block \w+ .*peer=(\d+)")
at, last = None, None
per_peer = collections.Counter()
lines = collections.defaultdict(list)
patterns = {
    "stalls": "is stalling block download",
    "block_timeouts": "Timeout downloading block",
    "header_timeouts": "Timeout downloading headers",
    "misbehaving": "Misbehaving: ",
}
try:
    with open(log, errors="replace") as f:
        for line in f:
            m = update_tip.search(line)
            if m:
                last = (int(m[2]), m[1])
                if int(m[2]) == height:
                    at = m[1]
                continue
            m = requesting.search(line)
            if m:
                per_peer[m[1]] += 1
                continue
            for key, needle in patterns.items():
                if needle in line:
                    lines[key].append(line.rstrip("\n"))
except FileNotFoundError:
    pass
checks = {
    "exit_0": status == 0,
    "tip_at_or_above_h": last is not None and last[0] >= height,
    "hash_at_h_matches": at == expect,
}
print(json.dumps({
    "argv": argv,
    "exit_status": status,
    "wall_s": round(float(end) - float(start), 1),
    "height": height,
    "hash_at_h": at,
    "expected_hash_at_h": expect,
    "last_tip": {"height": last[0], "hash": last[1]} if last else None,
    "overshoot_blocks": last[0] - height if last else None,
    "requesting_block_per_peer": dict(sorted(per_peer.items(), key=lambda kv: int(kv[0]))),
    "requesting_block_total": sum(per_peer.values()),
    **{k: len(lines[k]) for k in patterns},
    "lines": {k: v[:50] for k, v in lines.items()},
    "checks": checks,
    "pass": all(checks.values()),
}, indent=1))
EOF
cat "$datadir.json"
python3 -I -c 'import json,sys; sys.exit(0 if json.load(open(sys.argv[1]))["pass"] else 1)' \
    "$datadir.json"
