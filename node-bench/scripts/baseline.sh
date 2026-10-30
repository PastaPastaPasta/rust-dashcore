#!/bin/bash
#
# Record the dashd baseline: every runnable scenario, in dependency order.
#
#   scripts/baseline.sh [--quiet --reservation ID] [--tag TAG] [--dashd-version V] [--skip-mainnet] [--runs N]
#
# --dashd-version runs the queue against another fetched release (`run fetch-dashd
# --version V`; its bench_dash via build-dashd-bench.sh v<V>); the tag defaults to
# baseline-v<V>. --skip-mainnet stops after the testnet steps (the v24 baseline:
# testnet runs v24, mainnet parity is pinned to 23.1.8).
#
# Resumable: each finished step leaves $NODE_BENCH_ROOT/baseline/<tag>/<step>.done holding
# its profile (the exact `run` arguments, quiet flags and dashd version) and the result
# files it wrote. A step is skipped only when its marker's profile matches the current one;
# a step that fails is retried on the next invocation, and so is a --quiet step whose
# measurement did not come out official. Markers without a profile (written
# before profiles existed) count as provisional runs: a --quiet queue redoes them.
#
# A baseline is a record of dashd's numbers, not a comparison: one run per step by
# default. Comparisons (the gate) come from `run pair` with its own repetitions (>= 4 for
# long scenarios, DEC-23); --runs N records N runs of every step here.
# Steps that need a synced chain wait for its seed (data/seed-test, data/seed-main),
# which the *-net IBD runs produce. --quiet adds --quiet-window to every run
# and --reservation (with --quiet: official numbers if the measurement stays quiet); without
# them results are provisional.
#
# Slices: while a mainnet IBD still occupies NUMA node 0 (the seed-main run),
# regtest/testnet steps use node 1 (16-27@1, aux 28-31); mainnet steps use the
# defaults (node numa:0, aux numa:1).
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT="${NODE_BENCH_ROOT:-/work/bench}"
TAG=""
QUIET=()
SKIP_MAINNET=0
RUNS=1
while [ $# -gt 0 ]; do
  case "$1" in
    --quiet) QUIET+=(--quiet-window --quiet-timeout 43200); shift ;;
    --reservation) QUIET+=(--reservation "$2"); shift 2 ;;
    --tag) TAG="$2"; shift 2 ;;
    --dashd-version) export NODE_BENCH_DASHD_VERSION="$2"; shift 2 ;;
    --skip-mainnet) SKIP_MAINNET=1; shift ;;
    --runs) RUNS="$2"; shift 2 ;;
    *) echo "unknown arg $1" >&2; exit 1 ;;
  esac
done
TAG="${TAG:-baseline-v${NODE_BENCH_DASHD_VERSION:-23.1.8}}"
DONE="${ROOT}/baseline/${TAG}"
mkdir -p "${DONE}"
NODE1=(--node-slice 16-27@1 --aux-slice 28-31)

log() { echo "[$(date -u +%FT%TZ)] $*"; }

step() {  # step <name> <run args...>: RUNS runs, each resumable on its own
  local name="$1"; shift
  local i
  for i in $(seq 1 "${RUNS}"); do
    one_step "${name}$([ "${RUNS}" = 1 ] || echo ".${i}")" "$@"
  done
}

results_of() { grep -o 'result /[^ ]*\.json' "$1" | cut -d' ' -f2; }

official() {  # every result the step wrote is official (quiet window, reservation, quiet measurement)
  results_of "$1" | python3 -c 'import json, sys
paths = [p.strip() for p in sys.stdin if p.strip()]
sys.exit(0 if paths and all(json.load(open(p)).get("load", {}).get("official") for p in paths) else 1)'
}

one_step() {
  local name="$1"; shift
  local marker="${DONE}/${name}.done"
  local profile="run $* --tag ${TAG} ${QUIET[*]} dashd=${NODE_BENCH_DASHD_VERSION:-23.1.8}"
  if [ -e "${marker}" ]; then
    local have; have="$(head -n 1 "${marker}")"
    if [ "${have}" = "${profile}" ] || { [ -z "${have}" ] && [ ${#QUIET[@]} = 0 ]; }; then
      log "skip ${name} (done: ${have:-legacy provisional marker})"; return 0
    fi
    log "redo ${name}: marker profile '${have:-none}' != '${profile}'"
  fi
  log "start ${name}: run $*"
  local out="${DONE}/${name}.log"
  if "${HERE}/run" run "$@" --tag "${TAG}" "${QUIET[@]}" 2>&1 | tee "${out}"; then
    if [ ${#QUIET[@]} -gt 0 ] && ! official "${out}"; then
      log "PROVISIONAL ${name}: the measurement was not official (see its noise line); will redo"; return 0
    fi
    { echo "${profile}"; results_of "${out}"; } > "${marker}.tmp"
    mv "${marker}.tmp" "${marker}"; log "done ${name}"
  else
    log "FAILED ${name} (will retry next invocation)"
  fi
}

wait_for() {  # wait_for <datadir>: present, with its height/tip recorded (bench-meta.json or its IBD result)
  until [ -d "$1" ] && { [ -e "$1/bench-meta.json" ] \
      || grep -qslF "\"datadir\": \"$1\"" "${ROOT}"/results/*.json 2>/dev/null; }; do
    log "waiting for $1"; sleep 600
  done
}

# Regtest and microbenchmarks (minutes).
step startup-regtest startup-regtest "${NODE1[@]}"
step mempool-accept mempool-accept "${NODE1[@]}"
step gbt-regtest gbt-regtest "${NODE1[@]}"
step rpc-regtest rpc-regtest "${NODE1[@]}"
step microbench microbench "${NODE1[@]}"

# Testnet (the chain from ibd-testnet-net).
wait_for "${ROOT}/data/seed-test"
step ibd-testnet-seed ibd-testnet-seed "${NODE1[@]}"
step ibd-testnet-seed-noav ibd-testnet-seed-noav "${NODE1[@]}"
step reindex-testnet reindex-testnet "${NODE1[@]}"
step reindex-chainstate-testnet-noav-bench reindex-chainstate-testnet-noav --bench-log 1 "${NODE1[@]}"
step spv-headers-testnet-dashd spv-headers-testnet "${NODE1[@]}"
step spv-headers-testnet-rust spv-headers-testnet --impl rust "${NODE1[@]}"

if [ "${SKIP_MAINNET}" = 1 ]; then log "skipping mainnet steps"; exit 0; fi

# Mainnet (the chain from ibd-mainnet-net; hours each).
wait_for "${ROOT}/data/seed-main"
step startup-mainnet startup-mainnet
step gbt-mainnet gbt-mainnet
step ibd-mainnet-seed ibd-mainnet-seed
step reindex-chainstate-mainnet reindex-chainstate-mainnet
step reindex-chainstate-mainnet-noav-bench reindex-chainstate-mainnet-noav --bench-log 1
step reindex-mainnet-indexed reindex-mainnet --addressindex 1 --keep-datadir "${ROOT}/data/indexed-main"
step rpc-mainnet rpc-mainnet
step rescan-mainnet rescan-mainnet
step ibd-mainnet-seed-noav ibd-mainnet-seed-noav
step spv-headers-mainnet-dashd spv-headers-mainnet
step spv-headers-mainnet-rust spv-headers-mainnet --impl rust
log "baseline queue finished; results in ${ROOT}/results (tag ${TAG})"
