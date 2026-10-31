#!/bin/bash
#
# PF1: flamegraph SVGs from the folded stacks of run directories.
#
#   flamegraphs.sh <FlameGraph dir> <out dir> <run-dir>...
#
# One SVG per run and event (instructions:u weighted, and cpu-clock for the multi-threaded runs).
# FlameGraph: https://github.com/brendangregg/FlameGraph (commit recorded in the report).
set -euo pipefail
FG="$1"; OUT="$2"; shift 2
mkdir -p "$OUT"
for run in "$@"; do
  name=$(basename "$run")
  for ev in instructions_u cpu-clock; do
    f="$run/fold/$ev.folded"
    [ -s "$f" ] || continue
    case $ev in
      instructions_u) unit=instructions; sub="instructions:u (user-space instructions retired)" ;;
      cpu-clock)      unit=ns;           sub="cpu-clock (on-CPU time)" ;;
    esac
    # Only the instruction graphs for single-thread runs: the CPU-time graph is kept for parallel runs.
    case "$name" in *par8*|*c8*|*ibd*) ;; *) [ "$ev" = cpu-clock ] && continue ;; esac
    "$FG/flamegraph.pl" --title "dashd v24.0.0-rc.3: $name" --subtitle "$sub" --countname "$unit" --width 1800 \
      --minwidth 0.4 --fonttype monospace "$f" > "$OUT/$name.$ev.svg"
  done
done
