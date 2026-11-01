#!/usr/bin/env bash
# Regenerate the vector sets that Dash Core itself computes, from an instrumented Core build.
#
#   contrib/node/regen-core-vectors.sh transcribed <test_dash>   # vectors embedded in C++ unit tests
#   contrib/node/regen-core-vectors.sh golden <test_dash>        # mnlistdiff / qrinfo ground truth
#   contrib/node/regen-core-vectors.sh functional <dash checkout> # values in Core's functional tests
#
# `functional` needs no build: it reads the literals of Core's Python functional tests at the
# pinned commit with `git show` and writes transcribed/bip39_functional.json.
#
# `<test_dash>` is the unit-test binary of a Dash Core build that contains the commits of the
# `node/o2a-ev-core-vectors` branch (`node/o4-core-vectors` plus the evo_snapshot_tests
# recorder; they are also kept as
# test-vectors/core/<commit>/instrumentation/o4-core.patch, which applies on that Core commit).
# The script cannot tell which source the binary was built from; build it from that branch.
# To build one (about 25 minutes on this machine, out of tree so the source stays clean):
#
#   git -C ~/workspace/dash worktree add ../dash-wt/o2a-ev-core-vectors node/o2a-ev-core-vectors
#   cd ~/workspace/dash-wt/o2a-ev-core-vectors && ./autogen.sh && mkdir build && cd build
#   ../configure --disable-wallet --without-gui --disable-bench --disable-fuzz-binary \
#                --without-miniupnpc --without-natpmp --disable-zmq
#   cp src/dashbls/depends/relic/include/relic_conf.h ../src/dashbls/depends/relic/include/
#   make -j"$(nproc)" -C src test/test_dash
#
# (The relic_conf.h copy works around a header the out-of-tree configure writes to the build dir.)
# Every test case stays a normal Core test: without the O4_* variables nothing is written.
set -euo pipefail

mode="${1:-}"
test_dash="${2:-}"
if [ "$mode" = functional ]; then
    if [ ! -d "$test_dash" ]; then
        sed -n '2,9p' "$0" >&2
        exit 2
    fi
elif [ -z "$mode" ] || [ ! -x "$test_dash" ]; then
    sed -n '2,9p' "$0" >&2
    exit 2
fi

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
core_commit="0364ed626ec2"
core_dir="$here/test-vectors/core/$core_commit"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

case "$mode" in
transcribed)
    O4_VECTOR_OUT="$tmp" "$test_dash" --run_test=pow_tests,subsidy_tests,evo_simplifiedmns_tests,evo_islock_tests,dip14_tests,quorum_proofs_tests,dip0020opcodes_tests,checkdatasig_tests,dynamic_activation_thresholds_tests,block_reward_reallocation_tests,o4_harness_audit_tests,evo_snapshot_tests
    expected=12
    produced="$(ls "$tmp"/*.json | wc -l)"
    if [ "$produced" -ne "$expected" ]; then
        echo "expected $expected output files, the test run produced $produced; nothing replaced" >&2
        exit 1
    fi
    mkdir -p "$core_dir/transcribed"
    # Keep the file the `functional` mode writes.
    find "$core_dir/transcribed" -maxdepth 1 -name '*.json' ! -name 'bip39_functional.json' -delete
    install -m 644 "$tmp"/*.json "$core_dir/transcribed/"
    (cd "$core_dir/transcribed" && sha256sum ./*.json | sed 's# \./# #' >SHA256SUMS)
    echo "wrote $(ls "$core_dir/transcribed" | wc -l) files to $core_dir/transcribed"
    ;;
golden)
    python3 - "$here" "$tmp" <<'EOF'
import glob, json, os, re, sys
root, tmp = sys.argv[1], sys.argv[2]
wire = "dash/tests/data/test_DML_diffs/core_wire/"
diffs = [
    {"name": "mainnet-0-2227096", "group": "main", "repoPath": "dash/tests/data/test_DML_diffs/mn_list_diff_0_2227096.bin"},
    {"name": "mainnet-2227096-2241332", "group": "main", "repoPath": "dash/tests/data/test_DML_diffs/mn_list_diff_2227096_2241332.bin"},
    {"name": "testnet-0-1296600", "group": "test", "repoPath": "dash/artifacts/mn_list_diff_testnet_0_1296600.bin"},
]
for f in sorted(glob.glob(os.path.join(root, wire, "mnlistdiff_*.bin"))):
    base = os.path.basename(f)
    m = re.match(r"mnlistdiff_(\d+)_(\d+)\.bin", base)
    name = "mainnet-%s-%s" % m.groups() if m else "mainnet-" + base[len("mnlistdiff_"):-4]
    # Independent captures that do not continue one chain of lists: round trip and entry hashes only.
    diffs.append({"name": name, "group": "wire", "repoPath": wire + base, "checkRoot": False})
for d in diffs:
    d["file"] = os.path.join(root, d["repoPath"])
qr = [{"name": "mainnet-qrinfo-" + n, "repoPath": wire + "qrinfo_%s.bin" % n,
       "file": os.path.join(root, wire, "qrinfo_%s.bin" % n)} for n in ("2240504", "2518986")]
json.dump(diffs, open(os.path.join(tmp, "sml-manifest.json"), "w"), indent=1)
json.dump(qr, open(os.path.join(tmp, "qr-manifest.json"), "w"), indent=1)
EOF
    if [ "$(ls "$here"/dash/tests/data/test_DML_diffs/core_wire/*.bin | wc -l)" -ne 34 ]; then
        echo "dash/tests/data/test_DML_diffs/core_wire/ does not hold the 34 captures" >&2
        exit 1
    fi
    mkdir -p "$tmp/out"
    O4_SML_MANIFEST="$tmp/sml-manifest.json" O4_QRINFO_MANIFEST="$tmp/qr-manifest.json" \
        O4_VECTOR_OUT="$tmp/out" "$test_dash" --run_test=o4_vector_dump_tests
    mkdir -p "$here/test-vectors/golden"
    install -m 644 "$tmp/out/mnlistdiff-core-ground-truth.json" "$tmp/out/qrinfo-core-ground-truth.json" \
        "$here/test-vectors/golden/"
    echo "wrote test-vectors/golden/*-core-ground-truth.json"
    ;;
functional)
    git -C "$test_dash" show "$core_commit:test/functional/wallet_upgradetohd.py" >"$tmp/upgradetohd.py"
    python3 -I "$here/contrib/node/upgradetohd-vectors.py" "$tmp/upgradetohd.py" "$tmp/bip39_functional.json"
    mkdir -p "$core_dir/transcribed"
    install -m 644 "$tmp/bip39_functional.json" "$core_dir/transcribed/"
    (cd "$core_dir/transcribed" && sha256sum ./*.json | sed 's# \./# #' >SHA256SUMS)
    echo "wrote $core_dir/transcribed/bip39_functional.json"
    ;;
*)
    echo "unknown mode: $mode" >&2
    exit 2
    ;;
esac
