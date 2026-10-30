#!/usr/bin/env bash
# Proves that no selftest shard starts an sccache server, whatever the shared
# server's state, and that the reaper still flags one that does (BL-155 D27,
# BL-185). Runs the real driver, supervisor, reaper and merge of
# selftest-checks.sh, one shard, with every check replaced by a stub that
# returns the case's wanted verdict. Two scenarios, each against a private
# sccache server (its own port and cache directory, standing in for the shared
# one; nothing here touches 127.0.0.1:4226 or the box's cache):
#   race  case 2 compiles through the shards' rustc wrapper three times: with
#         the server up, to see the compile reach it; with the server stopped
#         after the wrapper found it answering and before the sccache client
#         connects (a stand-in `sccache` first on PATH stops it); and with the
#         server gone. The run must stay green, leave no server on the port,
#         and count the uncached compiles (the shard's cargo calls outside the
#         checks add some of their own once the server is gone).
#   leak  case 2 stops the server and runs an sccache client against the
#         server's own address, past the wrapper. The run must fail with the
#         reaper's D27 line, and leave that server running.
# Usage: contrib/node/test-selftest-sccache.sh [<rev>]  (default: the working
# tree's selftest-checks.sh). Needs sccache and rustc; about 5 minutes a
# scenario, most of it the 300-odd cases' resets. Exit 0 when both hold.
set -euo pipefail
repo="$(cd "$(dirname "$0")/../.." && pwd)"
rev="${1:-}"
real="$(command -v sccache)" || { echo "skip: no sccache on PATH"; exit 0; }
command -v rustc > /dev/null || { echo "skip: no rustc on PATH"; exit 0; }
export ORIG_PATH="$PATH"

work="$(mktemp -d "${TMPDIR:-/tmp}/test-selftest-sccache.XXXXXX")"
chmod 700 "$work"
port="$(python3 -I -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')"
listening() { { : < "/dev/tcp/127.0.0.1/$port"; } 2> /dev/null; }
stop_server() {  # (the port is named here, since this can run before it is exported)
    SCCACHE_SERVER_PORT="$port" PATH="$ORIG_PATH" "$real" --stop-server > /dev/null 2>&1 || true
    for _ in $(seq 100); do listening || return 0; sleep 0.1; done
    echo "something other than our server holds port $port"; return 1
}
cleanup() {
    stop_server || true
    git -C "$repo" worktree remove -f -f "$work/tree" 2> /dev/null || true
    rm -rf "$work"
}
trap cleanup EXIT
# Every sccache below, the shards' included, uses the private server.
export SCCACHE_SERVER_PORT="$port" SCCACHE_DIR="$work/cache" TMPDIR="$work/tmp"
unset SCCACHE_SERVER_UDS RUSTC_WRAPPER CARGO_BUILD_RUSTC_WRAPPER
# Its stub checks build nothing, so no prebuilt RocksDB (BL-133) is wanted, nor built on a cache miss.
export NODE_SELFTEST_ROCKSDB=source
mkdir -p "$TMPDIR" "$work/bin"

# The driver runs from a throwaway worktree, which its shards check out.
git -C "$repo" worktree add -q --detach "$work/tree" "${rev:-HEAD}"
checks="$work/tree/contrib/node/selftest-checks.sh"
[ -n "$rev" ] || checks="$repo/contrib/node/selftest-checks.sh"  # no rev: the working tree's
python3 -I - "$checks" "$work/tree/contrib/node/selftest-stub.sh" <<'EOF'
import sys
t = open(sys.argv[1]).read()
check = '    if out="$("$@" 2>&1)"; then got=pass; else got=fail; fi\n'
stub = ('    if [ "$cases" = 2 ]; then case "$SCENARIO" in\n'
        '        race) echo "$SCCACHE_SERVER_PORT" > "$WORK/shard-port"\n'
        '              "$RUSTC_WRAPPER" rustc --version > /dev/null 2>&1\n'
        '              SCCACHE_SERVER_PORT="$PORT" PATH="$ORIG_PATH" "$REAL" --show-stats --stats-format=json > "$WORK/stats.json"\n'
        '              : > "$STOP_FLAG"; "$RUSTC_WRAPPER" rustc --version > "$WORK/race.out" 2>&1\n'
        '              "$RUSTC_WRAPPER" rustc --version > "$WORK/down.out" 2>&1 ;;\n'
        '        leak) SCCACHE_SERVER_PORT="$PORT" PATH="$ORIG_PATH" "$REAL" --stop-server > /dev/null 2>&1 || :\n'
        '              SCCACHE_SERVER_PORT="$PORT" PATH="$ORIG_PATH" "$REAL" rustc --version > "$WORK/leak.out" 2>&1 ;;\n'
        '    esac; fi\n'
        '    if [ "$want" = pass ]; then out=; got=pass; else out="$want"; got=fail; fi\n')
verdict = 'grep -Eq -- "$want" <<< "$out"'  # the stub's out is the wanted message itself
assert t.count(check) == 1 and t.count(verdict) == 1, "the check call or its verdict moved"
open(sys.argv[2], "w").write(t.replace(check, stub).replace(verdict, 'true'))
EOF
chmod 755 "$work/tree/contrib/node/selftest-stub.sh"
# The stand-in sccache: with the flag present, a compile first stops the
# server (the shared one dying between the wrapper's look and the connect).
# Every real sccache runs with the PATH from before the stand-in: the box's
# sccache may itself be a script that runs the next sccache on PATH.
cat > "$work/bin/sccache" <<'EOF'
#!/usr/bin/env bash
if [ -e "$STOP_FLAG" ] && [ "${1#-}" = "$1" ]; then
    rm -f "$STOP_FLAG"
    SCCACHE_SERVER_PORT="$PORT" PATH="$ORIG_PATH" "$REAL" --stop-server > /dev/null 2>&1 || :
    for _ in $(seq 100); do { : < "/dev/tcp/127.0.0.1/$PORT"; } 2> /dev/null || break; sleep 0.1; done
    echo "stand-in: stopped the server on $PORT before connecting" >> "$WORK/stand-in.log"
fi
PATH="$ORIG_PATH" exec "$REAL" "$@"
EOF
chmod 755 "$work/bin/sccache"
export PATH="$work/bin:$PATH" STOP_FLAG="$work/stop" PORT="$port" REAL="$real" WORK="$work"

quiet() { grep -q '^rustc ' "$1" && ! grep -q '^sccache: ' "$1"; }  # compiled, and sccache said nothing
failed=0
check() {  # check <scenario> <what> <condition...>
    local s="$1" what="$2"; shift 2
    if "$@"; then echo "ok    $s: $what"; else echo "FAIL  $s: $what"; bad=1; failed=1; fi
}
for scenario in race leak; do
    rm -f "$work"/*.out "$work/shard-port" "$work/stats.json" "$work/stand-in.log" "$STOP_FLAG"
    PATH="$ORIG_PATH" "$real" --start-server > /dev/null
    rc=0 bad=0
    SCENARIO="$scenario" "$work/tree/contrib/node/selftest-stub.sh" --shards 1 > "$work/$scenario.log" 2>&1 || rc=$?
    log="$work/$scenario.log"
    d27="$(grep -E '^FAIL  a shard started the shared sccache server \(session [0-9]+\), left running \(D27\)$' "$log" || :)"
    up=no; listening && up=yes
    case "$scenario" in
        race)
            shard_port="$(cat "$work/shard-port" 2> /dev/null || :)"
            check race "the shard's sccache clients use another port than the server's ($shard_port)" [ "${shard_port:-$port}" != "$port" ]
            check race "a compile through that port reached the server" python3 -I -c \
                'import json, sys; sys.exit(json.load(open(sys.argv[1]))["stats"]["compile_requests"] < 1)' "$work/stats.json"
            check race "the server was stopped inside the window" grep -q '^stand-in: stopped' "$work/stand-in.log"
            check race "the compile in the window ran anyway ($(grep -m1 '^rustc ' "$work/race.out"))" grep -q '^rustc ' "$work/race.out"
            check race "the compile with the server gone ran quietly" quiet "$work/down.out"
            check race "no D27 line" [ -z "$d27" ]
            check race "no server on the port after the run (one: $up)" [ "$up" = no ]
            check race "the uncached compiles counted" grep -q '^sccache: [0-9]* compile(s) in the shards ran uncached' "$log"
            check race "  the one in the window" grep -qE '^ *1 sccache: error: ' "$log"
            check race "  those with the server gone" grep -qE '^ *[0-9]+ the shared server did not answer$' "$log"
            check race "the run passed (exit $rc)" [ "$rc" = 0 ] ;;
        leak)
            check leak "the reaper's D27 line ($d27)" [ -n "$d27" ]
            check leak "the server is left running (one: $up)" [ "$up" = yes ]
            check leak "the run failed (exit $rc)" [ "$rc" != 0 ] ;;
    esac
    [ "$bad" = 0 ] || { echo "--- $scenario run log, less ok lines:"; grep -v '^ok ' "$log" | head -40; }
    stop_server
done
exit "$failed"
