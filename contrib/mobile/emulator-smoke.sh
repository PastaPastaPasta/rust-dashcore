#!/usr/bin/env bash
# The SPV library's emulator and device smoke (SB17 §2.3): dash-spv-ffi's
# examples/mobile_probe, built with profile.mobile, runs an SPV sync on Android
# and records its threads, RSS phases and writes in one JSON file. The thread
# bound (SB0 §7) is a hard assertion: threads_peak <= baseline +
# MOBILE_WORKER_THREADS + MOBILE_MAX_BLOCKING_THREADS, and no thread name
# outside the baseline's and dash-spv-worker. RSS is recorded, not gated, and
# comes from the example build (the JSON's build_features: dash-spv's
# test-utils, key-wallet-ffi's keep-finalized-transactions), so it overstates
# the shipped library's.
#
# Usage:
#   ANDROID_TARGET=x86_64-linux-android contrib/mobile/emulator-smoke.sh \
#       [--avd agentbox-api36] [--chain regtest-40000] [--allow-no-reorgs] [--out DIR]
#     Emulator: regtest dashd on the host, on a copy of setup-dashd.py's test
#     data under ${TMPDIR:-/work/tmp}, reached through `adb reverse`; the wait
#     for the probe's sync ends when it syncs or exits, so the probe's own sync
#     timeout is the only one. After the probe's sync a reorg
#     (invalidateblock + 2 blocks), which the probe must count (reorgs >= 1)
#     unless --allow-no-reorgs. Writes DIR/probe-emulator.json.
#   ANDROID_TARGET=aarch64-linux-android contrib/mobile/emulator-smoke.sh \
#       --device SERIAL [--mainnet | --testnet] [--minutes N] [--out DIR]
#     Device: arm64-v8a, peers from DNS, N minutes (default 20) on testnet
#     (default). Writes DIR/probe-device.json.
# DIR defaults to target/emulator-smoke. The run directory on the device is
# deleted only on success.
set -euo pipefail

cd "$(dirname "$0")/../.."
usage() { sed -n '2,/^set -euo/p' "$0" | sed '$d' >&2; exit 2; }

avd=agentbox-api36 chain=regtest-40000 serial="" network=testnet minutes=20 allow_no_reorgs=0
out=target/emulator-smoke
while [ $# -gt 0 ]; do
    case "$1" in
        --avd) avd="$2"; shift ;;
        --chain) chain="$2"; shift ;;
        --device) serial="$2"; shift ;;
        --mainnet) network=mainnet ;;
        --testnet) network=testnet ;;
        --minutes) minutes="$2"; shift ;;
        --allow-no-reorgs) allow_no_reorgs=1 ;;
        --out) out="$2"; shift ;;
        *) usage ;;
    esac
    shift
done
mode=emulator want=x86_64-linux-android
[ -n "$serial" ] && mode=device want=aarch64-linux-android
if [ "${ANDROID_TARGET:-}" != "$want" ]; then
    echo "emulator-smoke.sh: $mode mode needs ANDROID_TARGET=$want" >&2
    exit 2
fi
. contrib/mobile/android-env.sh
mkdir -p "$out"
out="$(cd "$out" && pwd)"

cargo build --locked --example mobile_probe -p dash-spv-ffi --profile mobile --target "$ANDROID_TARGET"
probe="${CARGO_TARGET_DIR:-target}/$ANDROID_TARGET/mobile/examples/mobile_probe"

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
run_dir="/data/local/tmp/spv/run-$stamp"
dashd_pid="" work="" emulator_pid="" probe_pid=""
free_port() { python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])'; }

cleanup() {
    [ -n "$probe_pid" ] && kill "$probe_pid" 2> /dev/null || true
    [ "$mode" = device ] && { adb -s "$serial" shell pkill -f mobile_probe > /dev/null 2>&1 || true; }
    if [ -n "$emulator_pid" ]; then
        adb -s "$serial" emu kill > /dev/null 2>&1 || true
        for _ in $(seq 60); do kill -0 "$emulator_pid" 2> /dev/null || break; sleep 1; done
        kill "$emulator_pid" 2> /dev/null || true
    fi
    if [ -n "$dashd_pid" ]; then
        cli stop > /dev/null 2>&1 || kill "$dashd_pid" 2> /dev/null || true
        for _ in $(seq 60); do kill -0 "$dashd_pid" 2> /dev/null || break; sleep 1; done
        kill -9 "$dashd_pid" 2> /dev/null || true
    fi
    [ -n "$work" ] && rm -rf "$work"
}
trap cleanup EXIT

if [ "$mode" = emulator ]; then
    eval "$(python3 contrib/setup-dashd.py)"
    work="$(mktemp -d "${TMPDIR:-/work/tmp}/emulator-smoke.XXXXXX")"
    cp -a "$DASHD_TEST_DATA/$chain/." "$work/"
    p2p="$(free_port)" rpc="$(free_port)"
    cli() { "$(dirname "$DASHD_PATH")/dash-cli" -regtest -datadir="$work" -rpcport="$rpc" "$@"; }
    "$DASHD_PATH" -regtest -datadir="$work" -port="$p2p" -rpcport="$rpc" -server=1 -daemon=0 -daemonwait=0 \
        -rpcbind=127.0.0.1 -rpcallowip=127.0.0.1 -bind=127.0.0.1 -listen=1 -listenonion=0 -txindex=0 \
        -blockfilterindex=1 -peerblockfilters=1 -peerbloomfilters=1 -whitelist=127.0.0.1 \
        -fallbackfee=0.00001 -wallet=default > "$out/dashd.out" 2>&1 &
    dashd_pid=$!
    for _ in $(seq 120); do cli getblockcount > /dev/null 2>&1 && break; sleep 1; done
    height="$(cli getblockcount)"
    echo "dashd: $chain at height $height, p2p $p2p"
    mnemonic="$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["mnemonic"])' \
        "$DASHD_TEST_DATA/$chain/wallets/wallet.json")"

    eport="$(python3 -c '
import socket
for p in range(5600, 5684, 2):
    try:
        for q in (p, p + 1):
            s = socket.socket(); s.bind(("127.0.0.1", q)); s.close()
        print(p); break
    except OSError:
        pass')"
    [ -n "$eport" ] || { echo "emulator-smoke.sh: no free console port in 5600-5682" >&2; exit 1; }
    serial="emulator-$eport"
    emulator -avd "$avd" -port "$eport" -no-window -no-audio -no-boot-anim -no-snapshot-save -read-only \
        > "$out/emulator.out" 2>&1 &
    emulator_pid=$!
    timeout 300 adb -s "$serial" wait-for-device
    for _ in $(seq 300); do
        [ "$(adb -s "$serial" shell getprop sys.boot_completed 2> /dev/null | tr -d '\r')" = 1 ] && break
        sleep 1
    done
    [ "$(adb -s "$serial" shell getprop sys.boot_completed | tr -d '\r')" = 1 ] || {
        echo "emulator-smoke.sh: $serial did not boot" >&2
        exit 1
    }
    adb -s "$serial" reverse "tcp:$p2p" "tcp:$p2p"
    probe_env="PROBE_NETWORK=regtest PROBE_PEER=127.0.0.1:$p2p PROBE_SECONDS=0 PROBE_MIN_REORGS=1 PROBE_MNEMONIC='$mnemonic'"
else
    abi="$(adb -s "$serial" shell getprop ro.product.cpu.abi | tr -d '\r')"
    if [ "$abi" != arm64-v8a ]; then
        echo "emulator-smoke.sh: $serial is $abi, not arm64-v8a" >&2
        exit 2
    fi
    probe_env="PROBE_NETWORK=$network PROBE_PEER= PROBE_SECONDS=$((minutes * 60)) PROBE_MIN_REORGS=0"
fi

sh_adb() { adb -s "$serial" shell "$@"; }
adb -s "$serial" push "$probe" /data/local/tmp/mobile_probe > /dev/null
sh_adb chmod 0755 /data/local/tmp/mobile_probe
sh_adb "rm -rf $run_dir && mkdir -p $run_dir && chmod 0700 $run_dir"
sh_adb "cd $run_dir && $probe_env PROBE_DATA_DIR=$run_dir PROBE_JSON=$run_dir/probe.json PROBE_MODE=$mode \
PROBE_TARGET=$ANDROID_TARGET PROBE_STAMP=$stamp /data/local/tmp/mobile_probe > probe.out 2>&1; echo \$? > exit_code" &
probe_pid=$!

if [ "$mode" = emulator ]; then
    while kill -0 "$probe_pid" 2> /dev/null; do
        sh_adb test -e "$run_dir/synced" && break
        sleep 1
    done
    if sh_adb test -e "$run_dir/synced"; then
        cli invalidateblock "$(cli getbestblockhash)"
        cli generatetoaddress 2 "$(cli -rpcwallet=default getnewaddress)" > /dev/null
        echo "dashd: reorg to height $(cli getblockcount)"
    else
        echo "emulator-smoke.sh: the probe never synced" >&2
    fi
fi
wait "$probe_pid" || true
probe_pid=""

json="$out/probe-$mode.json"
rm -f "$json"
adb -s "$serial" pull "$run_dir/probe.out" "$out/probe-$mode.out" > /dev/null 2>&1 || true
adb -s "$serial" pull "$run_dir/probe.json" "$json" > /dev/null 2>&1 || true
code="$(sh_adb cat "$run_dir/exit_code" 2> /dev/null | tr -d '\r' || true)"
python3 - "$json" "$stamp" "${code:-missing}" "$mode" "$allow_no_reorgs" << 'EOF'
import json, sys
path, stamp, code, mode, allow_no_reorgs = sys.argv[1:]
errors = []
if code != "0":
    errors.append(f"the probe exited with {code}")
try:
    doc = json.load(open(path))
except (OSError, ValueError) as e:
    sys.exit(f"emulator-smoke.sh: no probe JSON: {e}; {'; '.join(errors)}")
if doc.get("stamp") != stamp:
    errors.append(f"the JSON is from run {doc.get('stamp')!r}, not {stamp!r}")
if doc["threads_peak"] > doc["thread_bound"]:
    errors.append(f"threads_peak {doc['threads_peak']} is over the bound {doc['thread_bound']}")
stray = sorted(set(doc["thread_names"]) - set(doc["baseline_thread_names"]) - {"dash-spv-worker"})
if stray:
    errors.append(f"threads outside the baseline and dash-spv-worker: {stray}")
if mode == "emulator" and allow_no_reorgs != "1" and not (doc.get("reorgs") or 0) >= 1:
    errors.append(f"reorgs is {doc.get('reorgs')}, not >= 1")
print(f"threads: baseline {doc['baseline_threads']}, peak {doc['threads_peak']}, bound {doc['thread_bound']}; "
      f"rss MiB: baseline {doc['baseline_rss_mib']}, idle {doc['idle_rss_mib']}, synced {doc['synced_rss_mib']}, "
      f"peak {doc['peak_rss_mib']} (recorded, not gated; example build with {doc.get('build_features')}, "
      f"so it overstates the shipped library); reorgs {doc['reorgs']}")
for e in errors:
    print(f"emulator-smoke.sh: {e}", file=sys.stderr)
sys.exit(1 if errors else 0)
EOF
sh_adb rm -rf "$run_dir"
echo "emulator-smoke.sh: ok, $json"
