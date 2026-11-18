#!/usr/bin/env bash
# Runs capture_sb11.py against a Dash Core release and writes the SB11 regtest capture (gzipped JSON).
#
# Usage: run.sh <dash-core-checkout> <tag> <bin-dir> <work-dir> <out-json.gz>
#   e.g. run.sh ~/workspace/dash v24.0.0-rc.3 ~/.rust-dashcore-test/dashcore-24.0.0-rc.3/bin \
#            /work/scratch/sb11/capture dash-mnlist/tests/data/sb11-regtest-v24.0.captures.json.gz
#
# SB11_SCRIPT=capture_sb11_transitions.py writes the V19 and DIP0008 transition capture instead.
# The harness is lq3a-captures/run.sh (version check, exported tests, venv); this adds capture_sb11.py.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LQ3A_EXTRA_DIR="$here" LQ3A_SCRIPT="${SB11_SCRIPT:-capture_sb11.py}" LQ3A_PORTSEED="${LQ3A_PORTSEED:-2411}" \
    exec "$here/../lq3a-captures/run.sh" "$@"
