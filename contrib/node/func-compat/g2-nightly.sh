#!/usr/bin/env bash
# G2's nightly (CH5), box-local, never on Actions: g2-at with the default G2 set at node/integration,
# compared with the committed g2-llmq.json of the checkout this script is in (the harness). It
# never passes --update. rdashd is built with the release profile, as the deployed binaries are
# (g2-at --release). The worktree and its target/ stay in G2_NIGHTLY_DIR/src (g2-at --reuse), so
# each night builds incrementally. The unit runs it through g2-nightly-bootstrap.sh, from the
# nightly's own harness worktree (G2_NIGHTLY_DIR/harness at G2_NIGHTLY_HARNESS), never from a
# live checkout.
#
# usage: contrib/node/func-compat/g2-nightly.sh
#   Writes g2-at's report to G2_NIGHTLY_REPORTS, points latest.md there at it, and prints one
#   summary line (the counts per delta, naming the progress and regression rows, the harness sha
#   and how rdashd was built: dkg-runtime, or no-dkg-runtime when the head has no such feature;
#   g2-at picks it, DEC-1881), which it also appends to G2_NIGHTLY_DIR/summary.log with the time.
#
# Exit status: 0 when no row regressed (same, progress or change), or when the run was skipped
# because another g2 run held the g2 port lock for G2_NIGHTLY_LOCK_WAIT ("skipped: g2 ports
# busy"); 1 on a regression against the committed matrix or a stale adaptation; 2 when the run
# gave no result otherwise (the disk or load gate, the build, a harness error, a tripwire error,
# whose line the summary quotes, a SIGTERM, a harness checkout with uncommitted changes, or
# one of g2-nightly-bootstrap.sh's refusals).
#
# Install, from the repository (nothing runs from a checkout afterwards):
#   git=(git -C ~/workspace/rust-dashcore) ref=node/integration fc=contrib/node/func-compat
#   mkdir -p ~/.local/libexec ~/.config/systemd/user
#   "${git[@]}" show "$ref:$fc/g2-nightly-bootstrap.sh" > ~/.local/libexec/g2-nightly-bootstrap.sh
#   for u in service timer; do "${git[@]}" show "$ref:$fc/systemd/g2-llmq-nightly.$u" \
#     > ~/.config/systemd/user/g2-llmq-nightly.$u; done
#   systemctl --user daemon-reload && systemctl --user enable --now g2-llmq-nightly.timer
#
# env:
#   G2_NIGHTLY_REF        the head to run; default node/integration
#   G2_NIGHTLY_DIR        src/ (the kept worktree), tmp/ and g2-at.log; default /work/scratch/g2-nightly
#   G2_NIGHTLY_REPORTS    the report directory; default /work/scratch/g2-at/reports
#   G2_NIGHTLY_LOCK_WAIT  seconds g2 waits for the g2 port lock; default 900 (the unit's cap is
#                         3600 s: a cold build up to ~20 min, 569 s measured for a cold release
#                         build without sccache, this wait, and a main + rerun + confirm g2 run
#                         of ~20 min)
#   G2_NIGHTLY_KEEP_DATADIRS  how many of G2_NIGHTLY_REPORTS' newest <report>.d dirs keep the failed
#                         tests' datadirs g2's main and confirm passes kept (failed-main,
#                         failed-confirm); after each run the older ones lose them; default 7.
#                         possible-crashes is never removed.
set -u

HERE=$(cd "$(dirname "$0")" && pwd)
REF=${G2_NIGHTLY_REF:-node/integration}
DIR=${G2_NIGHTLY_DIR:-/work/scratch/g2-nightly}
REPORTS=${G2_NIGHTLY_REPORTS:-/work/scratch/g2-at/reports}
LOG=$DIR/g2-at.log
LOCK_WAIT=${G2_NIGHTLY_LOCK_WAIT:-900}
KEEP_DATADIRS=${G2_NIGHTLY_KEEP_DATADIRS:-7}
[[ $KEEP_DATADIRS =~ ^[0-9]+$ ]] || KEEP_DATADIRS=7
mkdir -p "$DIR/tmp" || exit 2
export TMPDIR=$DIR/tmp
say() { echo "$1"; echo "$(date -u +%FT%TZ) $1" >> "$DIR/summary.log"; }

# The harness (g2-at, func-compat.py, g2-llmq.json, contrib/node) is this script's checkout.
if ! harness=$(git -C "$HERE" rev-parse --short=12 HEAD 2>/dev/null) \
    || ! changes=$(git -C "$HERE" status --porcelain -- .. 2>/dev/null); then
  say "g2-nightly: $REF: no result: the harness $HERE is not a git checkout"
  exit 2
fi
if [ -n "$changes" ]; then
  say "g2-nightly: $REF: no result: the harness $HERE at $harness has uncommitted changes"
  exit 2
fi

# g2-at writes its report on a SIGTERM; forward it and wait for the report.
"$HERE/g2-at" --release --reuse "$DIR/src" --out "$REPORTS" --lock-wait "$LOCK_WAIT" "$REF" > "$LOG" 2>&1 &
pid=$!
terminated=0
trap 'terminated=1; kill -TERM "$pid" 2>/dev/null' TERM
wait "$pid"
status=$?
if [ $terminated = 1 ]; then
  wait "$pid" 2>/dev/null
  again=$?
  [ $again != 127 ] && status=$again
fi

# Report names start with their UTC time, so name order is age order.
mapfile -t dirs < <(find "$REPORTS" -mindepth 1 -maxdepth 1 -type d -name '*.d' 2>/dev/null | sort)
for ((i = 0; i < ${#dirs[@]} - KEEP_DATADIRS; i++)); do
  rm -rf -- "${dirs[i]}/failed-main" "${dirs[i]}/failed-confirm"
done

report=$(grep -E '^/.*\.md$' "$LOG" | tail -n 1)
if [ -n "$report" ] && [ -f "$report" ]; then
  ln -sfn "$(basename "$report")" "$REPORTS/latest.md"
  head=$(sed -n '1s/^HEAD //p' "$report" | cut -c1-12)
  rows=$(grep '^| `' "$report")
  built=$(grep -m1 '^built ' "$report")
else
  report="" head="" rows="" built=""
fi
case $built in
  "") features="features unknown" ;;
  *"--features "*dkg-runtime*) features=dkg-runtime ;;
  *) features=no-dkg-runtime ;;
esac

count() { local n=0; [ -n "$rows" ] && n=$(grep -cE "\| $1 \|\$" <<< "$rows"); echo "$n"; }
# shellcheck disable=SC2016 # the backticks are the report's, not command substitution
names() { [ -n "$rows" ] && grep -E "\| $1 \|\$" <<< "$rows" | sed -E 's/^\| `([^`]*)`.*/\1/' | paste -sd, - | sed 's/,/, /g'; }
with_names() { local n; n=$(count "$1"); if [ "$n" -gt 0 ]; then echo "$n $1 ($(names "$1"))"; else echo "0 $1"; fi; }

where="harness $harness $features; report ${report:-none}; log $LOG"
if [ "$status" = 75 ]; then
  say "g2-nightly: $REF ${head:-?}: skipped: g2 ports busy (the g2 port lock was not free within $LOCK_WAIT s); $where"
  exit 0
fi
if [ -z "$rows" ]; then
  trip=""
  [ -n "$report" ] && trip=$(grep -m1 -o 'tripwire: .*' "$report")
  say "g2-nightly: $REF ${head:-?}: no result (g2-at exit $status${trip:+; $trip}); $where"
  exit 2
fi
stale=""
grep -q "adaptations: STALE" "$report" && stale="; adaptations STALE"
say "g2-nightly: $REF $head: $(count same) same, $(with_names progress), $(with_names regression), $(with_names change); g2-at exit $status$stale; $where"
if [ "$(count regression)" -gt 0 ] || [ -n "$stale" ]; then
  exit 1
fi
case $status in
  0|1) exit 0 ;;
  *) exit 2 ;;
esac
