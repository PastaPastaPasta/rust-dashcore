#!/usr/bin/env bash
# SP1b's nightly (DEC-562; SP1-notes D18-D22): the masternode-sync tick's timers, dashd
# v24.0.0-rc.4 against rdashd, as two-sample KS tests over 10,000 independent sessions per node
# and measure (PRD NFR-FP-03). Not tier 1: a run takes hours of wall time.
#
# usage: contrib/node/sp1b-timing/nightly.sh OUT_DIR
#   OUT_DIR gets samples.tsv, errors.tsv, run-summary.json (wall time, CPU-hours), ks.tsv and
#   nightly.log. A rerun into the same OUT_DIR resumes from samples.tsv.
#
# The exit status is 0 only if the KS selftest passes, the run completes, and every gated
# measure passes (`timing.py ks`); 2 (with a "skipped: load" line) if the load never fell below
# SP1B_MAX_LOAD, which the unit counts as success (SuccessExitStatus=2).
#
# env:
#   SP1B_RDASHD   a release rdashd (required)
#   SP1B_DASHD    dashd v24.0.0-rc.4 (default: setup-dashd's cache in the home directory)
#   SP1B_CPUS     the cores cap, a taskset list (default 0-15)
#   SP1B_JOBS     concurrent sessions (default 128; ports 23000-23386 at the default base)
#   SP1B_SAMPLES  good sessions per node and kind (default 10000)
#   SP1B_MAX_LOAD start only once the 1-minute load is below this, and start no session while it
#                 is at or above it (an acceptance run: 40, DEC-562); unset, no ceiling (a
#                 development run)
#   SP1B_LOAD_WAIT_S  how long to wait for that load before exiting 2 (default 14400)
#   TMPDIR        the sessions' datadirs (default /work/tmp)
#   SP1B_SWEEP_TMP  1 (the unit sets it): at start, remove `timing.py run`'s earlier roots in
#                 TMPDIR (mkdtemp's sp1b-XXXXXXXX), all but the newest, which keeps the last
#                 night's failed-session datadirs. Only for a TMPDIR strictly below the working
#                 directory, so never a shared one like /work/tmp.
set -u

OUT=${1:?usage: nightly.sh OUT_DIR}
HERE=$(cd "$(dirname "$0")" && pwd)
CPUS=${SP1B_CPUS:-0-15}
JOBS=${SP1B_JOBS:-128}
SAMPLES=${SP1B_SAMPLES:-10000}
export TMPDIR=${TMPDIR:-/work/tmp}

mkdir -p "$OUT" "$TMPDIR" || exit 1
exec > >(tee -a "$OUT/nightly.log") 2>&1
echo "sp1b-timing: $(date -u +%FT%TZ) cpus=$CPUS jobs=$JOBS samples=$SAMPLES rdashd=${SP1B_RDASHD:-unset}"
# What the night's verdict is tied to: the rdashd build and the harness checkout.
if [ -n "${SP1B_RDASHD:-}" ]; then
  echo "sp1b-timing: rdashd sha256 $(sha256sum "$SP1B_RDASHD" | cut -d' ' -f1)"
fi
echo "sp1b-timing: checkout $(git -C "$HERE" describe --always --dirty --abbrev=40 2>/dev/null || echo 'not a git checkout')"

sweep_tmp() {
  local tmp here
  tmp=$(realpath -m "$TMPDIR") here=$(pwd -P)
  case $tmp in
    "$here"/?*) ;;
    *) echo "sp1b-timing: not sweeping $tmp: not below $here"; return ;;
  esac
  # Every earlier root but the newest, NUL-separated, so no name can split into another path.
  find "$tmp" -mindepth 1 -maxdepth 1 -type d -regextype posix-extended \
      -regex '.*/sp1b-[a-z0-9_]{8}' -printf '%T@ %p\0' | sort -zn | head -zn -1 |
    cut -zd' ' -f2- | xargs -0r sh -c 'for d; do echo "sp1b-timing: removing the earlier run root $d"; rm -rf -- "$d"; done' sweep
}
[ "${SP1B_SWEEP_TMP:-}" = 1 ] && sweep_tmp

run() { nice -n 10 taskset -c "$CPUS" python3 "$HERE/timing.py" "$@"; }
load1() { cut -d' ' -f1 /proc/loadavg; }

if [ -n "${SP1B_MAX_LOAD:-}" ]; then
  deadline=$((SECONDS + ${SP1B_LOAD_WAIT_S:-14400}))
  until awk -v l="$(load1)" -v m="$SP1B_MAX_LOAD" 'BEGIN { exit !(l < m) }'; do
    if [ "$SECONDS" -ge "$deadline" ]; then
      echo "sp1b-timing: skipped: load: the 1-minute load never fell below $SP1B_MAX_LOAD (now $(load1))"
      exit 2
    fi
    sleep 60
  done
fi

run selftest || exit 1
echo "uptime at start: $(uptime)" | tee -a "$OUT/uptime.txt"
run run "$OUT" --samples "$SAMPLES" --jobs "$JOBS"
status=$?
echo "uptime at end: $(uptime)" | tee -a "$OUT/uptime.txt"
[ "$status" -eq 0 ] || exit 1
run ks "$OUT" --samples "$SAMPLES"
