#!/usr/bin/env bash
# BL-407: Dash Core v24's shutdown after a non-store `AbortNode` with LevelDB healthy.
#
# A regtest dashd mines two blocks and invalidates the second, whose data stays in
# blk00000.dat. One byte of that block's stored header is flipped, and `reconsiderblock` makes
# `ConnectTip` read it back: the read fails, and `AbortNode(state, "Failed to read block")`
# starts the shutdown (validation.cpp:3415-3418). Both of `PrepareShutdown`'s flushes then run
# on a healthy database (init.cpp:379-386, :449-456).
#
# Usage: capture.sh <work-dir> <out-file>
#   <work-dir>  must not exist; it receives the datadir (mode 0700) and debug.log.
#   <out-file>  receives debug.log from the `***` line to `Shutdown: done`, timestamps stripped.
# BL407_DASHD names the dashd (default: setup-dashd.py's v24.0.0-rc.4); dash-cli is taken from
# its directory. BL407_RPC_PORT (default 31407) is the RPC port; the node does not listen.
set -euo pipefail

work=${1:?usage: capture.sh <work-dir> <out-file>}
out=${2:?usage: capture.sh <work-dir> <out-file>}
dashd=${BL407_DASHD:-$HOME/.rust-dashcore-test/dashcore-24.0.0-rc.4/bin/dashd}
cli="$(dirname "$dashd")/dash-cli"
port=${BL407_RPC_PORT:-31407}
want="Dash Core version v24.0.0-rc.4"

version=$("$dashd" -version | head -n 1)
if [ "$version" != "$want" ]; then
    echo "capture.sh: $dashd is '$version', not '$want'" >&2
    exit 2
fi
if [ -e "$work" ]; then
    echo "capture.sh: $work exists" >&2
    exit 2
fi
mkdir -m 0700 -p "$work/data"
data="$work/data"
args=(-regtest "-datadir=$data" "-rpcport=$port" -rpcuser=bl407 -rpcpassword=bl407)
rpc() { "$cli" "${args[@]}" "$@"; }

"$dashd" "${args[@]}" -server -listen=0 -disablewallet -printtoconsole=0 \
    -port=$((port + 1)) >"$work/stdout.txt" 2>"$work/stderr.txt" &
pid=$!
rpc -rpcwait -rpcwaittimeout=120 getblockcount >/dev/null

# rpc_generate.py's regtest address of the generator point's key.
rpc generatetoaddress 2 yWziQMcwmKjRdzi7eWjwiQX8EjWcd6dSg6 >/dev/null
h2=$(rpc getbestblockhash)
rpc getblock "$h2" 0 >"$work/h2.hex"
rpc invalidateblock "$h2"
# The block's 80-byte header, found once in blk00000.dat; its time's low byte is flipped.
python3 -I - "$work/h2.hex" "$data/regtest/blocks/blk00000.dat" <<'EOF'
import sys
header = bytes.fromhex(open(sys.argv[1]).read().strip())[:80]
path = sys.argv[2]
raw = bytearray(open(path, "rb").read())
at = raw.find(header)
assert at > 0 and raw.find(header, at + 1) < 0, "the header once in blk00000.dat"
with open(path, "r+b") as f:
    f.seek(at + 68)
    f.write(bytes([raw[at + 68] ^ 0x01]))
print(f"flipped byte {at + 68} of blk00000.dat (header at {at})", file=sys.stderr)
EOF
# The read fails inside the call, which then answers or loses its connection as the node stops.
rpc reconsiderblock "$h2" >/dev/null 2>&1 || true

status=0
for _ in $(seq 120); do
    kill -0 "$pid" 2>/dev/null || break
    sleep 1
done
if kill -0 "$pid" 2>/dev/null; then
    echo "capture.sh: dashd did not stop after the abort; stopping it" >&2
    rpc stop || true
fi
wait "$pid" || status=$?
echo "dashd exit status $status" >&2

log="$data/regtest/debug.log"
awk '/^[^ ]+ \*\*\* /{on=1} on{sub(/^[^ ]+ /, ""); print} /Shutdown: done/{if (on) exit}' \
    "$log" >"$out"
echo "$version" >"$work/version.txt"
echo "$status" >"$work/exit-status.txt"
