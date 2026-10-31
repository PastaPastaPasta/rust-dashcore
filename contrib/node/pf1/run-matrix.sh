#!/bin/bash
#
# PF1: the testnet window matrix. For each snapshot height W (from walk-snapshots.sh) and each configuration, one
# perf-record run over W+1..W+N. Skips runs that already have run.json (resumable).
#
#   run-matrix.sh <dashd> <snap-dir> <out-dir> <blocks> <height>...
#
# Configurations (names are used in the report):
#   serial-noav   -par=1 -parbls=1 -assumevalid=0       every script and BLS check, one thread: the instruction profile
#   serial-av     -par=1 -parbls=1 (default assumevalid)   what a default IBD pays below the assumevalid block
#   par8-noav     -par=8 -parbls=8 -assumevalid=0       parallel: CPU time per thread group, serial fraction
#   *-nobench     the same without -debug=bench: the headline instruction counts (the log costs 2-10% of the instructions)
# Runs with -debug=bench carry the per-phase millisecond breakdown; runs without carry the clean counters.
set -euo pipefail
DASHD="$1"; SNAPS="$2"; OUT="$3"; N="$4"; shift 4
HERE="$(cd "$(dirname "$0")" && pwd)"
for W in "$@"; do
  for cfg in serial-noav-nobench serial-av-nobench serial-noav serial-av par8-noav; do
    run="$OUT/w$W-$cfg"
    # Resume: a run counts as done only if it recorded no error (a transient perf mlock failure leaves one behind).
    python3 -c 'import json,sys; sys.exit(0 if "error" not in json.load(open(sys.argv[1])) else 1)' "$run/run.json" 2>/dev/null && continue
    case $cfg in
      serial-noav) args=(--par 1 --parbls 1 --assumevalid 0) ;;
      serial-av)   args=(--par 1 --parbls 1) ;;
      serial-noav-nobench) args=(--par 1 --parbls 1 --assumevalid 0 --bench 0) ;;
      serial-av-nobench)   args=(--par 1 --parbls 1 --bench 0) ;;
      par8-noav)   args=(--par 8 --parbls 8 --assumevalid 0) ;;
    esac
    for attempt in 1 2 3; do  # retry: perf can fail to mmap when another session holds the mlock budget
      python3 "$HERE/run_window.py" --dashd "$DASHD" --snap "$SNAPS/snap-$W" --start "$W" --blocks "$N" \
        --out "$run" --cpus 16-27 "${args[@]}" >"$OUT/w$W-$cfg.log" 2>&1 && break
      echo "FAILED $run (attempt $attempt)"; sleep 30
    done
    echo "done $run"
  done
done
