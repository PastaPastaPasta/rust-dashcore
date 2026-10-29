#!/bin/bash
#
# PF1: replay testnet once with -reindex-chainstate, stopping at each window start
# and keeping a reflink snapshot of the whole datadir there. Chainstate at a given
# height does not depend on -assumevalid or -par, so one walk serves every window run.
#
#   walk-snapshots.sh <dashd> <seed-datadir> <out-dir> <height>...
#
# Snapshots: <out-dir>/snap-<height>. Resumable: an existing snapshot is skipped.
set -euo pipefail
DASHD="$1"; SEED="$2"; OUT="$3"; shift 3
WALK="$OUT/walk"
mkdir -p "$OUT"
if [ ! -d "$WALK" ]; then
  cp -a --reflink=always "$SEED" "$WALK"
  rm -rf "$WALK/dash.conf" "$WALK/testnet3/debug.log" "$WALK/testnet3/indexes"  # -reindex-chainstate refuses -txindex, and dashd refuses -txindex=0 with governance on
fi
first=1
for H in "$@"; do
  [ -d "$OUT/snap-$H" ] && { first=0; continue; }
  extra=()
  [ -e "$OUT/.walked" ] || extra=(-reindex-chainstate)
  "$DASHD" -testnet -datadir="$WALK" -connect=0 -listen=0 -dnsseed=0 -fixedseeds=0 -disablewallet -txindex=0 -disablegovernance \
    -dbcache=2048 -par=8 -checkblockindex=0 -stopatheight="$H" "${extra[@]}" >>"$OUT/walk.stdout" 2>&1
  touch "$OUT/.walked"
  tail -n 400 "$WALK/testnet3/debug.log" | grep -E "UpdateTip: new best=.* height=$H " | tail -1
  cp -a --reflink=always "$WALK" "$OUT/snap-$H"
  rm -f "$OUT/snap-$H/testnet3/debug.log"
  echo "snapshot $H done $(date -u +%T)"
done
