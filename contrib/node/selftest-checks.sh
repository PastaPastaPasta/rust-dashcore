#!/usr/bin/env bash
# Proves the node checks catch what they are meant to. Plants each violation
# in a throwaway worktree of HEAD and asserts that the check fails with the
# expected message; the clean tree must pass. Cases are derived from the
# current manifests and lockfile, so they hold as base SDK use grows. Needs
# the base SDK fork at the path named by the [patch]. Uses GNU sed (agentbox).
#
# Usage: contrib/node/selftest-checks.sh [--shard I/K | --shards K | --list]
#   --shard I/K  run only the cases this shard owns: case n (counted from 1 in
#                the order the script reaches them) belongs to shard
#                (n - 1) % K + 1. Every shard also runs the clean-tree cases
#                (`expect pass "clean ..."`), so each shard proves its own
#                worktree passes before its plants count.
#   --shards K   run K shards in parallel, each with its own TMPDIR (so its own
#                worktree, CARGO_HOME, target/ and temporary files), wait for
#                all, and merge: every case of the full list must have its
#                owner's verdict, and a shard that dies counts as a failure.
#   --list       print the case list (number and description) without running
#                any check.
#   VERBOSE=1  show each check's output, to see why it failed.
set -euo pipefail
# A shard the supervisor (below) starts first records its pid, which is also its
# session and process group id, by an atomic rename (D28).
if [ -n "${SELFTEST_SUPERVISOR:-}" ] && [ -n "${SELFTEST_RESULTS:-}" ]; then
    { echo "$$" > "${SELFTEST_RESULTS%/*}/pid.$$" && mv -f "${SELFTEST_RESULTS%/*}/pid.$$" "${SELFTEST_RESULTS%/*}/pid"; } || {
        rm -f "${SELFTEST_RESULTS%/*}/pid.$$"
        echo "shard: could not record its pid (the driver finds it by its nonce)" >&2
    }
    # The gate's disk ledger keeps this step's reservation while a shard's session has a member,
    # even once the step's own process group is gone (D34). Recorded before any trap, so a shard
    # stopped before this line does not outlive the signal; one that can't record does not start.
    if [ -n "${GATE_SELFTEST_SESSIONS:-}" ]; then
        # The whole record: a command name may hold a newline or ") " (D36); `read -d ''` stops at
        # the end of the file, and returns 1 there.
        stat=; read -r -d '' stat < "/proc/$$/stat" || :
        stat=(${stat##*) })
        [ "${stat[3]}" = "$$" ] || { echo "shard: does not lead its session (${stat[3]})" >&2; exit 1; }
        echo "$$ ${stat[19]}" >> "$GATE_SELFTEST_SESSIONS" ||  # its session, its start time
            { echo "shard: could not record its session in $GATE_SELFTEST_SESSIONS" >&2; exit 1; }
        unset stat
    fi
fi
# reset() keeps ignored files, so no bytecode: a __pycache__ entry for a .py a
# case edits (same size, same second) would outlive the edit. `python3 -I`
# ignores the variable, so those runs pass -B.
export PYTHONDONTWRITEBYTECODE=1

src="$(cd "$(dirname "$0")/../.." && pwd)"
self="$src/contrib/node/$(basename "$0")"

shard= shards= list=
while [ $# -gt 0 ]; do
    case "$1" in
        --shard) shard="${2:-}"; shift 2 || { echo "--shard needs I/K" >&2; exit 2; } ;;
        --shard=*) shard="${1#*=}"; shift ;;
        --shards) shards="${2:-}"; shift 2 || { echo "--shards needs K" >&2; exit 2; } ;;
        --shards=*) shards="${1#*=}"; shift ;;
        --list) list=1; shift ;;
        *) echo "usage: $0 [--shard I/K | --shards K | --list]" >&2; exit 2 ;;
    esac
done
[ $(( ${shard:+1}+0 + ${shards:+1}+0 + ${list:+1}+0 )) -le 1 ] || { echo "--shard, --shards and --list exclude each other" >&2; exit 2; }
if [ -n "$shard" ]; then
    [[ "$shard" =~ ^([1-9][0-9]*)/([1-9][0-9]*)$ ]] && [ "${BASH_REMATCH[1]}" -le "${BASH_REMATCH[2]}" ] ||
        { echo "bad shard '$shard': want I/K with 1 <= I <= K" >&2; exit 2; }
    shard_i="${BASH_REMATCH[1]}" shard_k="${BASH_REMATCH[2]}"
fi
# BL-133: every nested tree links one prebuilt RocksDB instead of compiling its C++; see
# selftest-rocksdb.py. Taken here, before any shard starts, so shards and their checks inherit it.
# The trees are HEAD's, so a working tree whose lock, manifests, toolchain or Cargo config differ
# from HEAD builds from source; so does NODE_SELFTEST_ROCKSDB=source or an environment that already
# names a RocksDB. ROCKSDB_LIB_DIR holds for every target and profile: a case that builds and links
# RocksDB for another target or with other features must unset it.
if [ -z "$list" ] && [ -z "${ROCKSDB_LIB_DIR+set}${ROCKSDB_COMPILE+set}" ] && [ "${NODE_SELFTEST_ROCKSDB:-}" != source ]; then
    if [ -n "$(git -C "$src" status --porcelain -- Cargo.lock ':(glob)**/Cargo.toml' 'rust-toolchain*' ':(glob)**/.cargo/**')" ]; then
        echo "== RocksDB: the working tree's lock, manifests, toolchain or Cargo config differ from HEAD, so each tree builds it from source"
    elif rocksdb="$(python3 -I -B "$src/contrib/node/selftest-rocksdb.py" "$src")"; then
        if [ -n "$rocksdb" ]; then
            export ROCKSDB_LIB_DIR="$rocksdb" ROCKSDB_STATIC=1
            echo "== RocksDB: prebuilt $rocksdb"
        fi
    else
        echo "== RocksDB: no prebuilt library (above), so each tree builds it from source"
    fi
fi
if [ -n "$shards" ]; then
    [[ "$shards" =~ ^[1-9][0-9]*$ ]] || { echo "bad shard count '$shards'" >&2; exit 2; }
    # An sccache server is shared by every build on the box, and an sccache
    # client starts one at the address it uses whenever none answers there. One
    # a shard started would carry the run's nonce (below) and live in the gate
    # unit's cgroup until systemd stops the unit. So no shard's client uses the
    # shared server's address: the supervisor relays a port of its own to it
    # for the whole run (the shards' SCCACHE_SERVER_PORT), and drops a
    # connection when the shared server does not answer, so a client there
    # never starts a server while the supervisor runs (D27, BL-185).
    sccache= upstream=
    wrapper="${RUSTC_WRAPPER-${CARGO_BUILD_RUSTC_WRAPPER-sccache}}"
    case "$wrapper" in
        *sccache) if command -v "$wrapper" > /dev/null; then sccache="$wrapper"; fi ;;
    esac
    # Each shard gets a private TMPDIR under $run: its worktree, CARGO_HOME,
    # target/ and every temporary file the checks make (mktemp) live there, so
    # no two shards share a writable path. A supervisor runs the shards and owns
    # their lifetimes; this script only waits for it. The supervisor gets
    # SIGTERM when this script dies, even by SIGKILL (parent-death signal), and
    # then stops every shard as it does when told to.
    run="$(mktemp -d "${TMPDIR:-/tmp}/node-selftest-shards.XXXXXX")"
    # target/ is the shard's own worktree's.
    unset CARGO_TARGET_DIR CARGO_BUILD_TARGET_DIR
    for i in $(seq "$shards"); do mkdir -m 700 "$run/$i" "$run/$i/tmp"; done
    if [ -n "$sccache" ]; then
        # The shared server's address, by sccache's own rules: a socket, else a
        # port (4226 unless a number is given). The relay connects there, and
        # the wrapper looks there to see whether the server answers.
        if [ -n "${SCCACHE_SERVER_UDS:-}" ]; then
            upstream="unix:$SCCACHE_SERVER_UDS"
            answers="[ -S $(printf %q "$SCCACHE_SERVER_UDS") ]"
        else
            port="${SCCACHE_SERVER_PORT:-4226}"
            [[ "$port" =~ ^[0-9]{1,5}$ ]] && [ "$((10#$port))" -le 65535 ] || port=4226
            port=$((10#$port))
            upstream="tcp:$port"
            answers="{ : < /dev/tcp/127.0.0.1/$port; } 2> /dev/null"
        fi
        # Uncached compiles each add a line to $run/uncached (why), which the
        # driver sums up after the run: a selftest that ran slow says why.
        # stdout passes straight through (fd 3); stderr is streamed, since cargo
        # reads rustc's notices there as they come, and kept, to tell a failure
        # of sccache's own from rustc's.
        log="$(printf %q "$run/uncached")"
        printf '%s\n' '#!/usr/bin/env bash' \
            "# The shards' rustc wrapper (D27, BL-185): sccache, which reaches the shared" \
            "# server only through the run's relay, while the relay's supervisor runs and" \
            '# the server answers; rustc itself, uncached, when not, or when sccache fails.' \
            '[ -n "${SELFTEST_SUPERVISOR:-}" ] && kill -0 "$SELFTEST_SUPERVISOR" 2> /dev/null ||' \
            "    { echo 'the supervisor (and its relay) is gone' >> $log; exec \"\$@\"; }" \
            "$answers || { echo 'the shared server did not answer' >> $log; exec \"\$@\"; }" \
            'err="$(mktemp)" || exec "$@"' \
            "{ $(printf %q "$sccache")"' "$@" 2>&1 1>&3 3>&- | tee "$err" >&2; r=${PIPESTATUS[0]}; } 3>&1' \
            'if [ "$r" != 0 ] && why="$(grep -m1 "^sccache: error: " "$err")"; then' \
            "    rm -f \"\$err\"; printf '%s\\n' \"\$why\" >> $log; exec \"\$@\"" \
            'fi' \
            'rm -f "$err"; exit "$r"' > "$run/rustc-wrapper"
        chmod 755 "$run/rustc-wrapper"
    fi
    # Only the supervisor's line carries this run's nonce (it is not exported
    # here), so the reaper below finds every process of the run by it.
    nonce="$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')"
    reap() {
        python3 -I - "$nonce" "$run" "$src" <<'EOF'
# The run's reaper (D28), run after the supervisor on every way out. Every
# process of the run carries its nonce in its environment (the supervisor got
# it; shards and their checks inherit it), so a shard is found even without a
# pid file, say when the supervisor died just after starting it. The sessions
# holding such processes (each shard leads one) are killed, group by group,
# until no process of the run, and none left in those sessions, runs; only
# then are the run's worktrees and tmp/ removed. A process in the driver's own
# session is killed by pid only. An sccache server (sccache started it with
# SCCACHE_START_SERVER=1, and it has left for a session of its own, whose leader
# is gone and which is no shard's) is taken for the box's shared one, even if a
# shard started it (the relay keeps the shards' clients off the shared address,
# but a check that sets its own can still start one): its session is left
# running, and the run fails. Only this uid's processes started since the
# driver are read: on a box where every agent runs as one uid, reading another
# process's environment can wait on its memory map lock. Exit 1 if it found
# any, or could not clean up.
import glob, os, shutil, signal, subprocess, sys, time

nonce, run, src = sys.argv[1:4]
if len(nonce) < 32:
    sys.exit("FAIL  the reaper got no nonce")
tag, uid, mine = f"SELFTEST_RUN_NONCE={nonce}".encode(), os.getuid(), os.getsid(0)


def fields(stat):  # the fields after the command name, from the state on
    return stat[stat.rindex(")") + 2:].split()


since = int(fields(open(f"/proc/{os.getppid()}/stat", errors="replace", newline="").read())[19])  # the driver's start time


def scan(sessions):  # {pid: (pgrp, sid, comm, is an sccache server)} of live processes with the nonce or in sessions
    found = {}
    for p in os.listdir("/proc"):
        try:
            if not p.isdigit() or os.stat(f"/proc/{p}").st_uid != uid:
                continue
            stat = open(f"/proc/{p}/stat", errors="replace", newline="").read()
            f, comm = fields(stat), stat[stat.index("(") + 1:stat.rindex(")")].replace("\n", "?")
            if f[0] == "Z" or int(f[19]) < since:
                continue
            env = open(f"/proc/{p}/environ", "rb").read().split(b"\0") if comm == "sccache" or int(
                f[3]) not in sessions else []
            if int(f[3]) in sessions or tag in env:
                server = comm == "sccache" and f[2] == f[3] and b"SCCACHE_START_SERVER=1" in env
                found[int(p)] = (int(f[2]), int(f[3]), comm, server)
        except (OSError, ValueError, IndexError):
            continue
    return found


def alive(pid):
    try:
        return fields(open(f"/proc/{pid}/stat", errors="replace", newline="").read())[0] != "Z"
    except (OSError, ValueError, IndexError):
        return False


shards = set()  # the shards' sessions, as far as their pid files tell
for f in glob.glob(f"{run}/*/pid"):
    try:
        shards.add(int(open(f).read()))
    except (OSError, ValueError):
        pass
seen, spared, sessions, waited = {}, set(), set(), time.monotonic()
while True:
    found = scan(sessions)
    # A server still starting in a shard's session is that shard's work.
    spared |= {s for p, (_, s, _, server) in found.items()
               if server and s != mine and s not in shards and not alive(s)}
    found = {p: v for p, v in found.items() if v[1] not in spared}
    if not found:
        break
    seen.update(found)
    # Only sessions that still hold a process: an emptied one's id can pass on.
    sessions = {s for _, s, _, _ in found.values() if s != mine}
    for p, (g, s, _, _) in found.items():
        try:
            if s == mine:
                os.kill(p, signal.SIGKILL)
            else:
                os.killpg(g, signal.SIGKILL)
        except ProcessLookupError:
            pass
    if time.monotonic() - waited > 60:
        print(f"reaper: still waiting for {sorted(found)} to exit after SIGKILL", flush=True)
        waited = time.monotonic()
    time.sleep(0.2)
bad = []
if seen:
    bad.append(f"FAIL  {len(seen)} process(es) of this run still ran after its supervisor "
               f"({' '.join(sorted({c for _, _, c, _ in seen.values()}))}); killed")
if spared:
    bad.append(f"FAIL  a shard started the shared sccache server (session {' '.join(map(str, sorted(spared)))}), "
               "left running (D27)")
def trees():  # the run's worktrees, or None if git cannot list them
    out = subprocess.run(["git", "-C", src, "worktree", "list", "--porcelain"], capture_output=True, text=True)
    return None if out.returncode else [line[len("worktree "):] for line in out.stdout.splitlines()
                                        if line.startswith(f"worktree {run}/")]


for tree in trees() or []:
    subprocess.run(["git", "-C", src, "worktree", "remove", "-f", "-f", tree], stderr=subprocess.DEVNULL)
for d in glob.glob(f"{run}/*/tmp"):
    shutil.rmtree(d, ignore_errors=True)
    if os.path.exists(d):
        bad.append(f"FAIL  could not delete {d}")
# A tree without its .git file (a shard was stopped inside its own removal)
# goes once its files are gone; one git still lists after that is a failure.
for tree in trees() or []:
    subprocess.run(["git", "-C", src, "worktree", "remove", "-f", "-f", tree])
left = trees()
bad += ["FAIL  could not list the worktrees"] if left is None else [f"FAIL  could not remove the worktree {w}" for w in left]
if bad:
    print("\n".join(bad))
    sys.exit(1)
EOF
    }
    sup= reaped=
    # Once stopping, further signals (a unit stop sends TERM to every process,
    # and the gate passes one on too) must not cut the cleanup short.
    trap 'trap "" TERM INT HUP; [ -z "$sup" ] || { kill -TERM "$sup" 2> /dev/null; wait "$sup" 2> /dev/null || true; }; [ -n "$reaped" ] || reap || true' EXIT
    trap 'trap "" TERM INT HUP; exit 143' TERM
    trap 'trap "" TERM INT HUP; exit 130' INT
    trap 'trap "" TERM INT HUP; exit 129' HUP
    echo "== $shards shards, logs in $run/<shard>/log"
    SELFTEST_RUN_NONCE="$nonce" setpriv --pdeathsig TERM python3 -I - "$src" "$self" "$run" "$shards" "$$" "$sccache" "$upstream" <<'EOF' &
# The shard supervisor. Each shard is the leader of its own session, which
# holds every process its checks start (unless one starts its own session; no
# check run here does, and the reaper finds those by the nonce). When a leader
# exits, its session is killed, group by group, until no process of it runs,
# and only then are the shard's worktree and TMPDIR removed: a check can outlive
# its shard's shell, and must not lose its tree while it runs. A shard that
# exited 0 with anything else still in its session fails (D26). A leader is
# reaped only after that (waitid with WNOWAIT until then), so its session id
# cannot pass to another process while it is signalled. Shards are handled in
# the order they finish. On SIGTERM, SIGINT or SIGHUP, or once the driver is
# gone, every running session gets SIGKILL first. A failed cleanup is reported
# and fails the run, but the rest goes on. Given an sccache, it relays a port of
# its own to the shared server for the shards' sccache clients, and asks the
# server for its stats every 2 minutes, from the driver's session and without
# the nonce, so the server does not idle out (10 minutes) mid-run; neither
# starts a server (D27, BL-185). Should it die abruptly, a shard client could
# start one on the relay's port, never the shared address: the wrapper then
# compiles uncached, and the reaper fails the run on any it finds.
import os, shutil, signal, socket, subprocess, sys, threading, time

src, script, run, k, driver = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4]), int(sys.argv[5])
sccache, kept, asked = sys.argv[6], None, time.monotonic()
stop, errors = [], []
for s in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
    signal.signal(s, lambda signum, _: stop.append(signum))


def say(msg):  # the driver's output may have gone with the driver
    try:
        os.write(1, f"{msg}\n".encode())
    except OSError:
        pass


def running(sid):  # the session's processes as (pid, pgrp, comm), less zombies (a zombie holds no files)
    found = []
    for p in os.listdir("/proc"):
        try:
            stat = open(f"/proc/{p}/stat", errors="replace", newline="").read() if p.isdigit() else ""
        except OSError:
            continue
        if stat:
            state, _, pgrp, session = stat[stat.rindex(")") + 2:].split()[:4]
            if int(session) == sid and state != "Z":
                found.append((int(p), int(pgrp), stat[stat.index("(") + 1:stat.rindex(")")].replace("\n", "?")))
    return found


def trees(prefix):  # the repository's worktrees under prefix
    out = subprocess.run(["git", "-C", src, "worktree", "list", "--porcelain"],
                         capture_output=True, text=True, check=True).stdout
    return [line[len("worktree "):] for line in out.splitlines() if line.startswith(f"worktree {prefix}")]


def last_case(i):  # the last case the shard recorded running, for a report
    try:
        rows = [r.split("\t") for r in open(f"{run}/{i}/results", errors="replace").read().splitlines()]
    except OSError:
        return "none"
    ran = [r for r in rows if len(r) >= 3 and r[0].isdigit() and r[1] != "skip"]
    return f"case {ran[-1][0]}: {ran[-1][2]}" if ran else "none"


def finish(i, ok):
    pid = leaders.pop(i)
    left, stray = running(pid), None
    if ok and left:
        stray = (f"FAIL  shard {i}/{k} exited 0 but left {' '.join(sorted({c for _, _, c in left}))} running "
                 f"after its last case ({last_case(i)}); killed")
    # Never a deadline: a tree is not removed under live work.
    waited = time.monotonic()
    while left := running(pid):
        for g in {g for _, g, _ in left}:  # every group in the session is the shard's
            try:
                os.killpg(g, signal.SIGKILL)
            except ProcessLookupError:
                pass
        if time.monotonic() - waited > 60:
            say(f"shard {i}/{k}: still waiting for process(es) {[p for p, _, _ in left]} to exit after SIGKILL")
            waited = time.monotonic()
        time.sleep(0.2)
    code = os.waitstatus_to_exitcode(os.waitpid(pid, 0)[1])
    try:
        with open(f"{run}/{i}/status", "w") as f:
            f.write(f"{code if code >= 0 else 128 - code}\n")
        if stray:  # for the merge, which prints it
            with open(f"{run}/{i}/stray", "w") as f:
                f.write(f"{stray}\n")
        # A shard stopped before its own EXIT trap, or with work left, leaves its
        # worktree behind, locked if it was stopped inside `git worktree add`.
        # One stopped inside its own `git worktree remove` can leave a tree
        # without its .git file, which git removes only once the files are gone.
        for tree in trees(f"{run}/{i}/"):
            subprocess.run(["git", "-C", src, "worktree", "remove", "-f", "-f", tree], stderr=subprocess.DEVNULL)
        shutil.rmtree(f"{run}/{i}/tmp", ignore_errors=True)
        for tree in trees(f"{run}/{i}/"):
            subprocess.run(["git", "-C", src, "worktree", "remove", "-f", "-f", tree])
            if tree in trees(tree):
                raise OSError(f"git could not remove the worktree {tree}")
    except (OSError, subprocess.CalledProcessError) as e:
        say(f"FAIL  shard {i}/{k}: cleanup failed: {e}")
        errors.append(i)
    shutil.rmtree(f"{run}/{i}/tmp", ignore_errors=True)  # again, if the above failed early
    if os.path.exists(f"{run}/{i}/tmp"):
        say(f"FAIL  shard {i}/{k}: could not delete {run}/{i}/tmp")
        errors.append(i)


def pump(a, b):  # a's bytes to b, then a's end of input; any error ends both ways
    try:
        while data := a.recv(1 << 16):
            b.sendall(data)
        b.shutdown(socket.SHUT_WR)
    except OSError:
        for s in (a, b):
            try:
                s.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass


def relay(client, upstream):  # one shard connection to the shared server; dropped if that does not answer in 10 s
    with client:
        try:
            server = socket.socket(upstream[0])
        except OSError:
            return  # as below
        with server:
            try:
                server.settimeout(10)
                server.connect(upstream[1])
                server.settimeout(None)
                for s in (client, server) if upstream[0] == socket.AF_INET else (client,):  # no Nagle delay on the extra hop
                    s.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
            except OSError:
                return  # the client sees its connection reset, and its wrapper compiles uncached
            back = threading.Thread(target=pump, args=(server, client), daemon=True)
            back.start()
            pump(client, server)
            back.join()


def relaying(listener, upstream):
    while True:
        try:
            client, _ = listener.accept()
        except OSError:  # say, out of file descriptors: the shards' clients wait in the backlog
            time.sleep(0.1)
            continue
        try:
            threading.Thread(target=relay, args=(client, upstream), daemon=True).start()
        except RuntimeError:  # no thread to be had: dropped, as if the server did not answer
            client.close()


env = dict(os.environ, SELFTEST_SUPERVISOR=str(os.getpid()))
if sccache:  # D27, BL-185: the shards' sccache clients reach the shared server only through the relay
    env["RUSTC_WRAPPER"] = f"{run}/rustc-wrapper"
    kind, _, where = sys.argv[7].partition(":")  # the shared server's address, as the driver found it
    upstream = (socket.AF_UNIX, where) if kind == "unix" else (socket.AF_INET, ("127.0.0.1", int(where)))
    try:
        listener = socket.create_server(("127.0.0.1", 0), backlog=128)
    except OSError as e:
        say(f"FAIL  supervisor: could not listen for the shards' sccache clients: {e}")
        sys.exit(1)
    threading.Thread(target=relaying, args=(listener, upstream), daemon=True).start()
    env.pop("SCCACHE_SERVER_UDS", None)
    env["SCCACHE_SERVER_PORT"] = str(listener.getsockname()[1])
# No detached git maintenance from the cases' commits: it would leave the shard's
# session and be stopped as a process of the run.
n = int(env.get("GIT_CONFIG_COUNT", "0") or 0)
env.update({"GIT_CONFIG_COUNT": str(n + 2), f"GIT_CONFIG_KEY_{n}": "gc.auto", f"GIT_CONFIG_VALUE_{n}": "0",
            f"GIT_CONFIG_KEY_{n + 1}": "maintenance.auto", f"GIT_CONFIG_VALUE_{n + 1}": "false"})
leaders = {}
for i in range(1, k + 1):
    if stop:
        break
    d = f"{run}/{i}"
    # A shard whose supervisor dies gets SIGTERM, and finishes its current
    # check before its EXIT trap runs (it traps TERM). It writes its own pid
    # file (D28); the environment passes on this run's nonce.
    leaders[i] = os.posix_spawnp(
        "setpriv", ["setpriv", "--pdeathsig", "TERM", script, "--shard", f"{i}/{k}"],
        dict(env, TMPDIR=f"{d}/tmp", SELFTEST_RESULTS=f"{d}/results"),
        file_actions=[(os.POSIX_SPAWN_OPEN, 0, os.devnull, os.O_RDONLY, 0),
                      (os.POSIX_SPAWN_OPEN, 1, f"{d}/log", os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644),
                      (os.POSIX_SPAWN_DUP2, 1, 2)],
        setsid=True, setsigdef=(signal.SIGPIPE, signal.SIGXFSZ, signal.SIGQUIT))  # which Python or bash ignored
while leaders and not stop:
    for i in sorted(leaders):
        if info := os.waitid(os.P_PID, leaders[i], os.WEXITED | os.WNOHANG | os.WNOWAIT):
            finish(i, ok=(info.si_code, info.si_status) == (os.CLD_EXITED, 0))
    if os.getppid() != driver:  # the parent-death signal can predate setpriv
        stop.append(signal.SIGTERM)
    if sccache and time.monotonic() - asked > 120 and (kept is None or kept.poll() is not None):
        asked = time.monotonic()
        try:
            kept = subprocess.Popen(["timeout", "60", sccache, "--show-stats"], stdin=subprocess.DEVNULL,
                                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                    env={n: v for n, v in os.environ.items() if n != "SELFTEST_RUN_NONCE"})
        except OSError as e:  # try again next time
            say(f"supervisor: could not ask sccache for its stats: {e}")
    time.sleep(1)
if kept is not None and kept.poll() is None:
    kept.terminate()  # timeout passes it on to its sccache
    kept.wait()
if leaders:
    say(f"stopping shard(s) {', '.join(f'{i}/{k}' for i in sorted(leaders))}: "
        + ("the driver is gone" if os.getppid() != driver else f"got {signal.Signals(stop[0]).name}"))
    for pid in leaders.values():
        try:
            os.killpg(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    for i in sorted(leaders):
        finish(i, ok=False)
sys.exit(128 + stop[0] if stop else 1 if errors else 0)
EOF
    sup=$!
    # (No job notice from wait: it would print this whole here-document.)
    sup_rc=0; wait "$sup" 2> /dev/null || sup_rc=$?
    sup=
    reap_rc=0; reap || reap_rc=$?
    reaped=1
    if [ -s "$run/uncached" ]; then  # (BL-185)
        echo "sccache: $(wc -l < "$run/uncached") compile(s) in the shards ran uncached, by reason:"
        sort "$run/uncached" | uniq -c | sort -rn | awk "NR <= 5"  # (reads all: an early exit fails pipefail)
    fi
    rc=0
    python3 -I - "$run" "$shards" <<'EOF' || rc=$?
# Merges the shards' results into one report over the full case list. Each
# results line is `<n>\t<ok|FAIL|skip>\t<description>\t<verdict line>`, and a
# shard that reaches the end writes `end\t<failures>`. Exit 1 on any failure.
import os, re, sys

run, k = sys.argv[1], int(sys.argv[2])
CLEAN = re.compile(r"^(?:[^:]*: )?clean(?!\w)")  # runs_here's test; a mismatch fails closed below
problems = []          # FAIL lines that are not a case's own verdict
shards = {}
for i in range(1, k + 1):
    d = os.path.join(run, str(i))
    try:
        status = open(os.path.join(d, "status")).read().strip()
    except OSError:
        status = "?"
    cases, end, bad = {}, None, []
    try:
        text = open(os.path.join(d, "results"), encoding="utf-8", errors="replace").read()
    except OSError:
        text = ""
    for line in text.split("\n"):
        if not line:
            continue
        f = line.split("\t", 3)
        if f[0] == "end" and len(f) > 1 and f[1].isdigit() and end is None:
            end = int(f[1])
        elif len(f) == 4 and f[0].isdigit() and f[1] in ("ok", "FAIL", "skip") and int(f[0]) not in cases and end is None:
            cases[int(f[0])] = (f[1], f[2], f[3])
        else:
            bad.append(line)
    fails = sum(v[0] == "FAIL" for v in cases.values())
    try:  # the supervisor's report of work a green shard left running (D26)
        problems += [s for s in open(os.path.join(d, "stray"), errors="replace").read().splitlines() if s]
    except OSError:
        pass
    if bad:
        problems.append(f"FAIL  shard {i}/{k} wrote {len(bad)} malformed, repeated or late result line(s), e.g. {bad[0]!r}")
        end = None
    elif end is not None and (end != fails or status != ("0" if end == 0 else "1")):
        problems.append(f"FAIL  shard {i}/{k} ended with {end} failure(s) and exit {status}, but recorded {fails} FAIL verdict(s)")
        end = None
    if end is None:
        problems.append(f"FAIL  shard {i}/{k} did not finish cleanly (exit {status}); its log: {d}/log")
        try:
            tail = open(os.path.join(d, "log"), encoding="utf-8", errors="replace").read().splitlines()[-15:]
        except OSError:
            tail = ["(no log)"]
        problems += [f"      | {t}" for t in tail]
    shards[i] = (status, cases, end)

# The full case list comes from the shards that finished, which must agree
# case for case: the list is a function of HEAD, not of the shard.
full = None
for i, (_, cases, end) in shards.items():
    if end is None:
        continue
    names = {n: v[1] for n, v in cases.items()}
    if full is None:
        full, ref = names, i
    elif names != full:
        diff = sorted(set(names.items()) ^ set(full.items()))[:3]
        problems.append(f"FAIL  shards {ref}/{k} and {i}/{k} disagree on the case list, e.g. {diff}")
if not full:
    problems.append("FAIL  no shard finished with a case list, so there is nothing to vouch for")
    full = {}
elif sorted(full) != list(range(1, len(full) + 1)):
    problems.append(f"FAIL  the case list is not numbered 1..{len(full)}")
for i, (_, cases, end) in shards.items():
    # A shard that died early still has to agree on the cases it reached.
    if end is None and full and any(full.get(n) != v[1] for n, v in cases.items()):
        problems.append(f"FAIL  shard {i}/{k}, which did not finish, disagrees with the case list")
    # And no shard runs a case that is neither its own nor clean.
    stray = [n for n, v in cases.items() if v[0] != "skip" and (n - 1) % k + 1 != i and not CLEAN.match(v[1])]
    if stray:
        problems.append(f"FAIL  shard {i}/{k} ran case(s) {stray[:5]} that it does not own")

ok = fail = 0
for n in sorted(full):
    what = full[n]
    owner = (n - 1) % k + 1
    must = set(range(1, k + 1)) if CLEAN.match(what) else {owner}
    lines = []
    for i in range(1, k + 1):
        v = shards[i][1].get(n)
        if v is None or v[0] == "skip":
            if i in must:
                lines.append(f"FAIL  case {n} was not run: shard {i}/{k} did not reach it: {what}")
        elif v[0] != "ok":
            lines.append(v[2])
    if lines:
        fail += 1
    else:
        lines = [shards[owner][1][n][2]]
        ok += 1
    print("\n".join(dict.fromkeys(lines)))

for line in problems:
    print(line)
for i, (status, cases, end) in shards.items():
    run_here = [v for v in cases.values() if v[0] != "skip"]
    print(f"shard {i}/{k}: {sum(v[0] == 'ok' for v in run_here)} ok, {sum(v[0] == 'FAIL' for v in run_here)} FAIL, "
          f"{len(run_here)} run, exit {status}{'' if end is not None else ', did not finish cleanly'}")
print(f"total: {ok} ok, {fail} FAIL, {len(full)} cases (union of {k} shards)")
failures = fail + sum(1 for p in problems if p.startswith("FAIL"))
if failures:
    print(f"{failures} selftest(s) failed", file=sys.stderr)
    sys.exit(1)
print("all selftests passed")
EOF
    if [ -n "${VERBOSE:-}" ]; then
        for i in $(seq "$shards"); do echo "== shard $i/$shards log"; cat "$run/$i/log"; done
    fi
    [ "$sup_rc" = 0 ] || { echo "FAIL  the shard supervisor exited $sup_rc" >&2; rc=1; }
    [ "$reap_rc" = 0 ] || rc=1
    exit "$rc"
fi
# Appends one line to the shard's results (see the merge above), if asked to.
record() {  # record <case> <ok|FAIL|skip> <description> [verdict line]
    [ -n "${SELFTEST_RESULTS:-}" ] || return 0
    local what="${3:-}" line="${4:-}"
    what="${what//$'\t'/ }"
    printf '%s\t%s\t%s\t%s\n' "$1" "$2" "${what//$'\n'/ }" "${line//$'\n'/ }" >> "$SELFTEST_RESULTS"
}
# Whether this run executes case $1, described as $2.
runs_here() {
    [ -z "$list" ] || return 1
    [ -n "$shard" ] || return 0
    [ $(( ($1 - 1) % shard_k + 1 )) -eq "$shard_i" ] || [[ "$2" =~ ^([^:]*:\ )?clean([^[:alnum:]_]|$) ]]
}
# The worktree sits two levels below $base, so Cargo config files can be
# planted in an ancestor directory without touching anything shared.
# A shard told to stop with SIGTERM (its supervisor died) finishes the command
# it is running first: bash runs the trap once a foreground command returns. A
# shard whose supervisor died before its parent-death signal was set does not
# start.
if [ -n "$shard" ]; then
    trap 'exit 143' TERM
    [ -z "${SELFTEST_SUPERVISOR:-}" ] || [ "$PPID" = "$SELFTEST_SUPERVISOR" ] ||
        { echo "shard $shard: its supervisor is gone" >&2; exit 143; }
fi
# Sets $strays to the names (comm) of the live processes in a supervised
# shard's session (it leads one) other than the shard itself (D26). Builtins
# only, so the scan adds no process of its own to the session.
find_strays() {
    local f s comm
    strays=
    for f in /proc/[0-9]*/stat; do
        s=; { read -r -d '' s < "$f" || :; } 2> /dev/null; [ -n "$s" ] || continue  # the whole record (D36)
        comm="${s#*(}" s="${s##*) }"
        comm="${comm%)*}" comm="${comm//[[:space:]]/?}"
        set -- $s
        if [ "$4" = "$$" ] && [ "$1" != Z ] && [ "${f:6:-5}" != "$$" ] && [ "${f:6:-5}" != "$BASHPID" ]; then
            strays="$strays $comm"
        fi
    done
    strays="${strays# }"
}
base="$(mktemp -d "${TMPDIR:-/tmp}/node-selftest.XXXXXX")"
wt="$base/nest/wt"
# A supervised shard leaves its tree to the supervisor while anything else of
# its session still runs, and fails; the supervisor removes the tree once the
# session is empty.
cleanup() {
    local rc=$?
    if [ -n "${SELFTEST_SUPERVISOR:-}" ]; then
        find_strays
        if [ -n "$strays" ]; then
            echo "FAIL  shard $shard left $strays running at its exit, after case ${cases:-0}" |
                tee -a "${SELFTEST_RESULTS%/*}/stray"
            exit $((rc ? rc : 1))
        fi
    fi
    git -C "$src" worktree remove --force "$wt" || true
    rm -rf "$base"
}
trap cleanup EXIT
git -C "$src" worktree add -q --detach "$wt" HEAD
cd "$wt"
start="$(git rev-parse HEAD)"

read -r fork_url fork_rev upstream_rev < <(python3 contrib/node/base_sdk_pins.py show)
fork="${fork_url#file://}"
upstream_url="https://github.com/dashpay/base-sdk"
failures=0

# A private CARGO_HOME that shares the registry and git database but has its
# own git checkouts and config.
home="$base/cargo-home"
real_home="${CARGO_HOME:-$HOME/.cargo}"
mkdir -p "$home/git/checkouts"
ln -s "$real_home/registry" "$home/registry"
ln -s "$real_home/git/db" "$home/git/db"
# Offline, so it never downloads into the shared registry or database, which
# its own lock does not cover.
in_home() { CARGO_HOME="$home" CARGO_NET_OFFLINE=true "$@"; }

# Also the ignored lockfiles of nested workspaces (spv-feature-off resolves
# afresh): a re-pin case relocks them, and git clean -d keeps ignored files.
# Only those files: `git clean -X -- '*Cargo.lock'` (seen with git 2.53)
# removes every ignored directory the pathspec could reach, target/ and
# __pycache__/ among them, so every case built from nothing (about 4x slower).
# target/ stays warm across cases, and Cargo's fingerprints rebuild what a case
# planted. The canary, an ignored file in target/, fails the selftest once it
# is gone after a reset.
canary="$wt/target/.selftest-reset-canary"
mkdir -p "$wt/target" && : > "$canary"
reset() {
    git reset -q --hard "$start" && git clean -qfd &&
        git ls-files -z -oi --exclude-standard -- '*Cargo.lock' ':(exclude,glob)**/target/**' | xargs -0r rm -f -- &&
        rm -rf "$base/.cargo" "$home/config.toml" || return
    [ -e "$canary" ] || { echo "FAIL  target/ was deleted during a case or its reset (its canary $canary is gone)"; exit 1; }
}
# Re-resolves Cargo.lock, as a developer would after editing a manifest;
# `pins_locked` and `bans` check the lockfile as it is.
relock() { cargo metadata --format-version 1 > /dev/null 2>&1 || true; }
pins() { relock; contrib/node/check-base-sdk-pins.sh; }
pins_locked() { contrib/node/check-base-sdk-pins.sh; }
bans() { cargo deny check bans; }
pure() { relock; contrib/node/check-pure.sh; }
layers() { relock; python3 contrib/node/check-layers.py; }
mobile() { relock; python3 contrib/node/check-layers.py mobile; }
shared() { relock; contrib/node/check-shared.sh; }
spv() { relock; contrib/node/check-spv.sh; }
groups() { python3 .github/scripts/ci_config.py verify-groups; }

cases=0
expect() {  # expect pass|<error regex> <description> <check>
    local want="$1" what="$2" out got line; shift 2
    cases=$((cases + 1))
    # The planting before this call runs in every shard, so the cases that
    # follow see the same tree; only the check is skipped.
    if ! runs_here "$cases" "$what"; then
        [ -z "$list" ] || printf '%s\t%s\n' "$cases" "$what"
        record "$cases" skip "$what"
        reset
        return
    fi
    if out="$("$@" 2>&1)"; then got=pass; else got=fail; fi
    # A check must not leave work running (D26): a supervised shard fails the
    # case and stops, and its supervisor kills the session before removing the tree.
    if [ -n "${SELFTEST_SUPERVISOR:-}" ]; then
        find_strays
        if [ -n "$strays" ]; then
            line="FAIL  left $strays running after its check: $what"
            echo "$line"
            record "$cases" FAIL "$what" "$line"
            exit 1
        fi
    fi
    [ -n "${VERBOSE:-}" ] && printf '%s\n' "$out"
    if [ "$want" = pass ] && [ "$got" = pass ]; then
        line="ok    pass: $what"
    elif [ "$want" != pass ] && [ "$got" = fail ] && grep -Eq -- "$want" <<< "$out"; then
        line="ok    fail: $what"
    else
        line="FAIL  wanted \"$want\", got $got: $what"
        failures=$((failures + 1))
    fi
    echo "$line"
    record "$cases" "${line%% *}" "$what" "$line"
    reset
}

member_dep() { sed -i "/^\[dependencies\]/a $1" internals/Cargo.toml; }
upstream_decl() { sed -i "/^dash-num = { git = \"https:/a $1" Cargo.toml; }
patch_entry() { sed -i "/^dash-num = { git = \"file:/a $1" Cargo.toml; }

# portable_tables <manifest> <deps toml>: the base SDK builds only `portable`
# without the host features it needs, so a crate declaring one of its packages
# takes the target tables a real pure crate (dash-filters) declares for it.
portable_tables() {
    awk -v deps=$'\n'"$2" '/^\[/ { t = /^\[target\./; hdr = $0; next }
        t && $2 == "=" && index(deps, "\n" $1 " =") { print hdr; print }' dash-filters/Cargo.toml >> "$1"
}

add_member() {  # add_member <dir> <deps toml> <lib.rs body>
    mkdir -p "$1/src"
    printf '[package]\nname = "%s"\nversion = "0.0.0"\nedition = "2024"\n\n[features]\ndefault = []\nstd = []\n\n[dependencies]\n%s\n' \
        "$1" "$2" > "$1/Cargo.toml"
    portable_tables "$1/Cargo.toml" "$2"
    printf '%s\n' "$3" > "$1/src/lib.rs"
    sed -i "s/^members = \[/members = [\"$1\", /" Cargo.toml
}

add_pure_member() {  # add_pure_member <deps toml> <lib.rs body>
    add_member dash-selftest-pure "$1" "$2"
    sed -i '/^\[crates\]/a dash-selftest-pure = "P"' contrib/node/layers.toml
}

# Fork packages the root manifest does not declare, and of those the ones
# Cargo.lock does not contain at all; small ones first, then any other.
read -r unpatched unused < <(python3 - "$fork" "$fork_rev" <<'EOF'
import sys, tomllib
from pathlib import Path
sys.path.insert(0, "contrib/node")
import base_sdk_pins as pins
packages = pins.fork_packages(Path(sys.argv[1]), sys.argv[2], [])
declared = set(tomllib.load(open("Cargo.toml", "rb"))["workspace"]["dependencies"])
locked = {p["name"] for p in tomllib.load(open("Cargo.lock", "rb"))["package"]}
first = ["dash-pow", "dash-script", "dash-primitives", "dash-params", "dash-p2p-core"]
order = first + sorted(packages - set(first) - {"dash-dev"})
undeclared = [p for p in order if p in packages and p not in declared]
unlocked = [p for p in undeclared if p not in locked]
print((unlocked or undeclared or ["-"])[0], (unlocked or ["-"])[0])
EOF
)

prev_rev="$(git -C "$fork" rev-parse "$fork_rev~1")"
pinned_itself='pins base SDK itself'
banned='error\[banned\]'

echo "== base SDK pin check"
expect pass "clean tree" pins

member_dep "dash-types = { git = \"$upstream_url\", rev = \"$upstream_rev\" }"
expect "$pinned_itself" "a member pins base SDK through git (the patch would absorb it silently)" pins

member_dep "dash-types = { path = \"$fork/pkgs/types\" }"
expect "$pinned_itself" "a member pins base SDK through a path (a second source)" pins

member_dep "types = { package = \"dash-types\", git = \"$upstream_url\", rev = \"$upstream_rev\" }"
expect "$pinned_itself" "a member pins base SDK under another name" pins

printf '\n[target."cfg(unix)".dependencies]\ndash-types = { git = "%s", rev = "%s" }\n' "$upstream_url" "$upstream_rev" >> internals/Cargo.toml
expect "$pinned_itself" "a member pins base SDK in a target-specific table" pins

sed -i "/^dash-num = { git = \"file:/s/rev = \"$fork_rev\"/rev = \"$prev_rev\"/" Cargo.toml
expect 'several fork sources' "one [patch] entry at a second fork revision" pins

sed -i "/^dash-num = { git = \"https:/s/rev = \"$upstream_rev\"/rev = \"$prev_rev\"/" Cargo.toml
expect 'several upstream revs' "one upstream declaration at a second revision" pins

if [ "$unpatched" != - ]; then
    upstream_decl "$unpatched = { git = \"$upstream_url\", rev = \"$upstream_rev\", default-features = false }"
    member_dep "$unpatched = { workspace = true }"
    expect "$unpatched\` comes from git\\+https" "a base SDK package ($unpatched) used without a [patch] entry (resolves from GitHub)" pins
else
    echo "skip  every base SDK package is declared; no unpatched case to plant"
fi

if [ "$unused" != - ]; then
    upstream_decl "$unused = { git = \"$upstream_url\", rev = \"$upstream_rev\", default-features = false }"
    patch_entry "$unused = { git = \"$fork_url\", rev = \"$fork_rev\" }"
    expect "patch for \`$unused\` is unused" "an unused [patch] entry ($unused)" pins
else
    echo "skip  every base SDK package is in Cargo.lock; no unused patch to plant"
fi

sed -i "s|git+$fork_url?rev=$fork_rev#$fork_rev|git+$upstream_url?rev=$upstream_rev#$upstream_rev|" Cargo.lock
expect 'metadata --locked failed' "Cargo.lock edited to another source" pins_locked

echo "== nested workspaces (node-fuzz, cs2-diff, any new one)"
# The root's [patch] entries, and a new workspace of its own (not a member)
# whose manifest ends with the given tables.
root_patch="$(grep -E "^[a-z0-9-]+ = \{ git = \"$fork_url\"" Cargo.toml)"
nested() {
    mkdir -p selftest-nested/src
    printf '[package]\nname = "selftest-nested"\nversion = "0.0.0"\nedition = "2024"\n\n[workspace]\n\n%s\n' "$1" > selftest-nested/Cargo.toml
    : > selftest-nested/src/lib.rs
}
nested_patch() { nested "$(printf '[patch."%s"]\n%s' "$upstream_url" "$1")"; }
in_nested='selftest-nested/Cargo.toml \[patch\]'

nested_patch "$root_patch"
expect pass "a new workspace repeating the root's [patch]" pins_locked

nested_patch "${root_patch//$fork_rev/$prev_rev}"
expect "$in_nested \`dash-num\` is .*@$prev_rev, the root's is" "a new workspace whose [patch] is at another fork revision" pins_locked

nested_patch "$(grep -v '^dash-num ' <<< "$root_patch")"
expect "$in_nested \`dash-num\` is missing" "a new workspace whose [patch] leaves out a root entry" pins_locked

nested_patch "${root_patch//$fork_url/file://$base/another-fork}"
expect "$in_nested \`dash-num\` is file://$base/another-fork@" "a new workspace whose [patch] points at another fork" pins_locked

if [ "$unpatched" != - ]; then
    nested_patch "$root_patch"$'\n'"$unpatched = { git = \"$fork_url\", rev = \"$fork_rev\" }"
    expect pass "a new workspace patching a package the root does not ($unpatched), at the root's fork source" pins_locked

    nested_patch "$root_patch"$'\n'"$unpatched = { git = \"$fork_url\", rev = \"$prev_rev\" }"
    expect "$in_nested \`$unpatched\` is .*@$prev_rev; an entry the root lacks" "a new workspace patching a package the root does not ($unpatched), at another fork revision" pins_locked
fi

nested "$(printf '[dependencies]\ndash-num = { git = "%s", rev = "%s" }' "$upstream_url" "$upstream_rev")"
expect "selftest-nested/Cargo.toml \[dependencies\] \`dash-num\` comes from $upstream_url without a base SDK \[patch\]" "a new workspace using base SDK from upstream without a [patch]" pins_locked

nested "$(printf '[workspace.dependencies]\ndash-num = { git = "%s", rev = "%s" }' "$upstream_url" "$upstream_rev")"
expect "selftest-nested/Cargo.toml \[workspace.dependencies\] \`dash-num\` comes from $upstream_url" "a new workspace declaring base SDK from upstream in [workspace.dependencies] without a [patch]" pins_locked

nested "$(printf '[patch."%s.git"]\n%s' "$upstream_url" "$root_patch")"
expect "selftest-nested/Cargo.toml \[patch.'$upstream_url.git'\] patches base SDK; spell its key" "a new workspace keying its [patch] …/base-sdk.git (Cargo applies it; the check would miss it)" pins_locked

nested "$(printf '[dependencies]\ndash-num = { git = "%s", rev = "%s" }\n\n[patch."%s"]\n%s' "$fork_url" "$prev_rev" "$upstream_url" "$root_patch")"
expect "selftest-nested/Cargo.toml \[dependencies\] \`dash-num\` is not declared from $upstream_url, so the \[patch\] does not apply" "a new workspace with a [patch] and a base SDK dependency named by fork revision, which the [patch] does not reach" pins_locked

# dde-probe/bs7 builds from the root's pin like any other workspace with a
# [patch]; its manifest and lockfile pass at any path, and nothing exempts
# a second fork revision there.
mkdir -p selftest-nested
cp contrib/node/dde-probe/bs7/Cargo.toml contrib/node/dde-probe/bs7/Cargo.lock selftest-nested/
expect pass "dde-probe/bs7's manifest and lockfile at another path" pins_locked

printf '\n[[package]]\nname = "dash-num"\nversion = "0.1.0"\nsource = "git+%s?rev=%s#%s"\n' "$fork_url" "$prev_rev" "$prev_rev" >> contrib/node/dde-probe/bs7/Cargo.lock
expect "contrib/node/dde-probe/bs7/Cargo.lock: \`dash-num\` comes from git\+$fork_url\?rev=$prev_rev" "dde-probe/bs7 locking a base SDK revision other than the root's" pins_locked

nested "$(printf '[patch.crates-io]\ndash-num = { path = "%s/pkgs/num" }' "$fork")"
expect "selftest-nested/Cargo.toml \[patch.'crates-io'\] redirects base SDK package \`dash-num\`" "a new workspace redirecting a base SDK package in [patch.crates-io]" pins_locked

nested "$(printf '[dependencies]\ndash-num = { git = "%s", rev = "%s" }' "$fork_url" "$prev_rev")"
expect pass "a new workspace without a [patch] naming a fork revision (as dde-probe does)" pins_locked

nested "$(printf '[dependencies]\ndash-num = { path = "%s/pkgs/num" }' "$fork")"
expect "selftest-nested/Cargo.toml \[dependencies\] \`dash-num\` must name a fork revision" "a new workspace without a [patch] using base SDK through a path" pins_locked

nested ""
printf 'version = 4\n\n[[package]]\nname = "dash-num"\nversion = "0.1.0"\nsource = "git+%s?rev=%s#%s"\n' "$upstream_url" "$upstream_rev" "$upstream_rev" > selftest-nested/Cargo.lock
expect "selftest-nested/Cargo.lock: \`dash-num\` comes from git\+$upstream_url.*, not a revision of the fork" "a new workspace without a [patch] whose Cargo.lock names base SDK from upstream" pins_locked

# What bump-base-sdk.sh does once it has a revision: the workspaces with a base
# SDK [patch] follow the root, relocked.
bump_to_parent() {
    python3 contrib/node/base_sdk_pins.py set "$prev_rev" "$upstream_rev" || return 1
    relock
    for dir in $(python3 contrib/node/base_sdk_pins.py nested); do
        cargo metadata --manifest-path "$dir/Cargo.toml" --format-version 1 > /dev/null || return 1
    done
    contrib/node/check-base-sdk-pins.sh
}
expect pass "a re-pin to the fork revision's parent with base_sdk_pins.py set, relocked as bump-base-sdk.sh does" bump_to_parent

# An existing workspace with a base SDK [patch], so the committed ones are covered.
existing="$(python3 - <<'EOF'
import sys
sys.path.insert(0, "contrib/node")
import base_sdk_pins as pins
found = [p for p, m in pins.nested_workspaces([]) if pins.UPSTREAM_URL in m.get("patch", {})]
print(found[0].parent.relative_to(pins.ROOT) if found else "-")
EOF
)"
if [ "$existing" != - ]; then
    sed -i "/^dash-num = { git = \"file:/s/rev = \"$fork_rev\"/rev = \"$prev_rev\"/" "$existing/Cargo.toml"
    expect "$existing/Cargo.toml \[patch\] \`dash-num\` is .*@$prev_rev" "$existing's [patch] at another fork revision" pins_locked
    if [ -f "$existing/Cargo.lock" ]; then
        sed -i "s|git+$fork_url?rev=$fork_rev#$fork_rev|git+$fork_url?rev=$prev_rev#$prev_rev|" "$existing/Cargo.lock"
        expect "$existing/Cargo.lock: \`[a-z-]+\` comes from .*, not the pinned fork" "$existing's Cargo.lock not relocked after a re-pin" pins_locked
    fi
else
    echo "skip  no nested workspace has a base SDK [patch]"
fi

echo "== Cargo overrides of the pinned source (Cargo.lock unchanged)"
mkdir -p "$base/override"
git -C "$fork" archive "$fork_rev" | tar -x -C "$base/override"
paths_override() { mkdir -p "$(dirname "$1")"; printf 'paths = ["%s/override/pkgs/num"]\n' "$base" > "$1"; }
built_elsewhere='`dash-num` is built from [^ ]*/override/pkgs/num'

paths_override .cargo/config.toml
expect "$built_elsewhere" "a path override in the workspace's .cargo/config.toml" pins_locked

paths_override "$base/.cargo/config.toml"
expect "Cargo config $base/.cargo/config.toml: \`paths\`" "a path override in an ancestor directory's .cargo/config.toml" pins_locked

mkdir -p internals/.cargo
printf '[source.fork]\ngit = "%s"\nreplace-with = "vendored"\n' "$fork_url" > internals/.cargo/config.toml
expect 'internals/.cargo/config.toml: \[source.fork\] replaces' "a source replacement in a member directory's .cargo/config.toml" pins_locked

expect 'environment variable CARGO_UNSTABLE_CONFIG_INCLUDE' "a CARGO_UNSTABLE_* override in the environment" \
    env CARGO_UNSTABLE_CONFIG_INCLUDE=true contrib/node/check-base-sdk-pins.sh

expect pass "clean tree, private CARGO_HOME" in_home pins_locked
paths_override "$home/config.toml"
expect "$built_elsewhere" "a path override in \$CARGO_HOME/config.toml" in_home pins_locked

# The clean-tree case above checks the fork out into $home, but a --list run
# runs no check: make the checkout here, so the plants below never depend on
# a check having run.
in_home cargo metadata --locked --format-version 1 > /dev/null
checkout="$(echo "$home"/git/checkouts/base-sdk-*/"${fork_rev:0:7}")"
echo '// edited' >> "$checkout/pkgs/num/src/lib.rs"
expect 'is not a clean checkout' "Cargo's checkout of the fork edited in place" in_home pins_locked
git -C "$checkout" checkout -q -- .

# Cargo builds a build.rs it finds, tracked or not.
printf '[status]\n\tshowUntrackedFiles = no\n' > "$base/gitconfig"
touch "$checkout/pkgs/num/build.rs"
GIT_CONFIG_GLOBAL="$base/gitconfig" expect 'is not a clean checkout' "an untracked file in Cargo's checkout, with status.showUntrackedFiles = no" in_home pins_locked
rm "$checkout/pkgs/num/build.rs"

# Cargo checks the fork's submodules out too, each with a `.cargo-ok` of its own.
touch "$checkout/fuzz/pow/sphlib/planted.rs"
expect 'is not a clean checkout' "an untracked file in a submodule of Cargo's checkout" in_home pins_locked
rm "$checkout/fuzz/pow/sphlib/planted.rs"

# A replace ref makes Git show an edited tree for the pinned commit, so plain
# `git status` sees a clean checkout. Out of the threat model (the guide), but
# the check reads Git with replace refs off.
old_tree="$(git -C "$checkout" rev-parse 'HEAD^{tree}')"
echo '// replaced' >> "$checkout/pkgs/num/src/lib.rs"
git -C "$checkout" add pkgs/num/src/lib.rs
git -C "$checkout" replace "$old_tree" "$(git -C "$checkout" write-tree)"
expect 'is not a clean checkout' "Cargo's checkout of the fork edited behind a git replace ref" in_home pins_locked
git -C "$checkout" replace -d "$old_tree" > /dev/null
git -C "$checkout" reset -q --hard

echo "== pure-crate bans"
# Every base SDK package the root manifest declares, as a pure crate would use them.
sdk_deps="$(python3 -c '
import tomllib
for name, spec in tomllib.load(open("Cargo.toml", "rb"))["workspace"]["dependencies"].items():
    if "base-sdk" in str(spec):
        print(f"{name} = {{ workspace = true }}")
')"
fs_use='pub fn f() -> String { std::fs::read_to_string("x").unwrap_or_default() }'
expect pass "clean tree" pure
add_pure_member "$sdk_deps" 'pub fn f(d: core::time::Duration) -> std::collections::BTreeMap<u64, u8> { [(d.as_secs(), 1)].into() }'
printf '\n[dev-dependencies]\ntokio = "1"\ntempfile = "3"\n' >> dash-selftest-pure/Cargo.toml
expect pass "a pure crate using the base SDK, Duration and std collections, with tokio and tempfile for tests" pure
add_pure_member 'tokio = "1"' ''
expect "$banned: crate 'tokio" "a pure crate depending on tokio" pure
add_member dash-selftest-helper 'tempfile = "3"' ''
add_pure_member 'dash-selftest-helper = { path = "../dash-selftest-helper" }' ''
expect "$banned: crate 'tempfile" "a pure crate reaching tempfile through another workspace crate" pure
add_pure_member 'rayon = { version = "1", optional = true }' ''
expect "$banned: crate 'rayon" "a pure crate with an optional rayon dependency" pure
add_pure_member 'chrono = { version = "0.4", default-features = false }' ''
expect "$banned: crate 'chrono" "a pure crate depending on chrono" pure
add_pure_member '' "$fs_use"
expect 'disallowed method `std::fs::read_to_string`' "a pure crate reading a file with std::fs" pure
add_pure_member '' 'pub fn f() -> u64 { std::time::SystemTime::now().elapsed().map(|d| d.as_secs()).unwrap_or(0) }'
expect 'disallowed method `std::time::SystemTime::now`' "a pure crate reading the clock" pure
add_pure_member '' 'pub fn f() { std::thread::spawn(|| ()); }'
expect 'disallowed method `std::thread::spawn`' "a pure crate spawning a thread" pure
add_pure_member '' 'pub fn f(_: &std::net::TcpStream) {}'
expect 'disallowed type `std::net::TcpStream`' "a pure crate taking a socket" pure
add_pure_member '' "pub fn f<'scope, 'env>(scope: &'scope std::thread::Scope<'scope, 'env>) { drop(scope.spawn(|| ())); }"
expect 'disallowed (method|type) `std::thread::Scope' "a pure crate spawning a scoped thread on a scope it is given" pure
add_pure_member '' '#[cfg(target_os = "android")] pub fn f() { drop(std::thread::spawn(|| ())); }'
expect 'disallowed method `std::thread::spawn`' "a pure crate spawning a thread on Android only" pure
mkdir -p "$base/selftest-effects/src"
printf '[package]\nname = "selftest-effects"\nversion = "0.0.0"\nedition = "2024"\n' > "$base/selftest-effects/Cargo.toml"
printf '%s\n' '#[macro_export] macro_rules! effects { () => { drop(std::fs::read("x")); }; }' > "$base/selftest-effects/src/lib.rs"
add_pure_member "selftest-effects = { path = \"$base/selftest-effects\" }" 'pub fn f() { selftest_effects::effects!(); }'
expect 'disallowed method `std::fs::read`' "a pure crate reading a file through another crate's macro" pure
add_pure_member '' ''
printf 'msrv = "1.0"\n' > dash-selftest-pure/clippy.toml
expect 'conflicts with an earlier clippy.toml' "a crate clippy.toml that conflicts with the root's" pure
add_pure_member '' "#[cfg(feature = \"std\")] $fs_use"
expect 'disallowed method `std::fs::read_to_string`' "a pure crate using std::fs behind a feature" pure
add_pure_member '' "#[cfg(not(feature = \"std\"))] $fs_use"
expect 'disallowed method `std::fs::read_to_string`' "a pure crate using std::fs when a feature is off" pure
add_pure_member '' "#[allow(clippy::disallowed_methods)] $fs_use"
expect 'incompatible with previous forbid' "a pure crate allowing the lint it breaks" pure
add_pure_member '' 'pub fn f() -> bool { std::path::Path::new("x").exists() }'
expect 'disallowed method `std::path::Path::exists`' "a pure crate probing the file system through Path" pure
add_pure_member '' 'pub fn f() -> bool { std::time::UNIX_EPOCH.elapsed().is_ok() }'
expect 'disallowed method `std::time::SystemTime::elapsed`' "a pure crate reading the clock through UNIX_EPOCH" pure
add_member dash-selftest-helper '' "$fs_use"
add_pure_member 'dash-selftest-helper = { path = "../dash-selftest-helper" }' ''
expect 'disallowed method `std::fs::read_to_string`' "a pure crate depending on a workspace crate that uses std::fs" pure
add_pure_member '' 'pub fn f() -> std::collections::HashMap<u8, u8> { Default::default() }'
expect 'disallowed type `std::collections::HashMap`' "a pure crate using a HashMap (iteration order)" pure
add_pure_member '' 'pub fn f() -> usize { std::env::args().count() }'
printf 'disallowed-methods = [\n    { path = "std::env::args", reason = "selftest" },\n]\n' > dash-selftest-pure/clippy.toml
expect 'disallowed method `std::env::args`' "a pure crate breaking a ban of its own clippy.toml" pure

# blst's thread pool (DEC-68/71): reachable through blst and dash-pkc only, and
# its entry points banned whether called directly, through a trait or through
# a dash-pkc wrapper.
pkc_bls='dash-pkc = { workspace = true, features = ["bls"] }'
blst_ty='dash_pkc::__deps::blst::min_pk'
ietf='dash_pkc::bls::BlsScIetf'
# The check passes, and with the warnings `allow-invalid` hides turned back on,
# the crate's lint names no ban that resolves to nothing.
bans_resolve() {
    sed -i 's/, allow-invalid = true//' contrib/node/pure-clippy.toml
    local out
    out="$(pure 2>&1)" || { printf '%s\n' "$out"; return 1; }
    ! awk '/^== /{own = $2 == "dash-selftest-pure"} own' <<< "$out" | grep 'does not refer to a reachable function'
}
add_pure_member "$pkc_bls" ''
expect pass "a pure crate using dash-pkc's BLS backend (blst and its threadpool), every blst and dash-pkc ban resolving" bans_resolve
add_pure_member "$pkc_bls" ''
sed -i 's/^# audited: blst [0-9.]*,/# audited: blst 0.0.0,/' contrib/node/pure-clippy.toml
expect 'links blst [0-9.]+; .*audited against blst 0\.0\.0' "a pure crate linking a blst release other than the audited one" pure
# The stamp also names the base SDK revision the bans were audited at: a
# re-pin as bump-base-sdk.sh makes it fails until the stamp follows. Only the
# pin, the relocked lockfiles and the stamp are checked, without a build: the
# workspace need not compile at the fork revision's parent, whose features
# the code may already use.
repin() { bump_to_parent && contrib/node/check-pure.sh --stamp; }
expect "base SDK pin is $prev_rev, but .*audited at $fork_rev" "a base SDK re-pin without a new audit stamp" repin
sed -i "s/^\(# audited: blst [0-9.]*, base-sdk \).*/\1$prev_rev/" contrib/node/pure-clippy.toml
# Only the selftest crate: other pure crates may use base SDK APIs newer than
# the parent revision this goes back to (dash-p2p-transport uses BS7's).
python3 - <<'EOF'
import yaml
p = ".github/ci-groups.yml"
config = yaml.safe_load(open(p))
config["groups"]["node-pure"] = ["dash-selftest-pure"]
yaml.safe_dump(config, open(p, "w"))
EOF
expect pass "a base SDK re-pin with its audit stamp updated" repin
add_pure_member "$pkc_bls" ''
sed -i '/^# audited: /d' contrib/node/pure-clippy.toml
expect 'pure-clippy.toml has no `# audited: ' "pure-clippy.toml without its audit stamp" pure
add_pure_member 'threadpool = "1"' ''
expect "$banned: crate 'threadpool" "a pure crate depending on threadpool itself" pure
add_member dash-selftest-helper 'threadpool = "1"' ''
add_pure_member "$pkc_bls"$'\n''dash-selftest-helper = { path = "../dash-selftest-helper" }' ''
expect "$banned: crate 'threadpool" "a pure crate reaching threadpool through a crate other than blst" pure
add_pure_member 'blst = "0.3"' ''
expect "$banned: crate 'blst" "a pure crate depending on blst itself" pure
add_pure_member "$pkc_bls" "pub fn f(pk: &dash_pkc::bls::BlsPublicKey<$ietf>, sig: &dash_pkc::bls::BlsSignature<$ietf>) -> bool { pk.verify_digest(&[0u8; 32], sig).is_ok() }"
expect 'disallowed method `dash_pkc::bls::BlsPublicKey::verify_digest`' "a pure crate verifying through a dash-pkc wrapper on blst's pool" pure
add_pure_member "$pkc_bls" "pub fn f(sig: &dash_pkc::bls::BlsSignature<$ietf>, pks: &[&dash_pkc::bls::BlsPublicKey<$ietf>]) -> bool { sig.fast_verify_aggregates_message(&[0u8; 32], pks).is_ok() }"
expect 'disallowed method `dash_pkc::bls::BlsSignature::fast_verify_aggregates_message`' "a pure crate aggregate-verifying through a dash-pkc wrapper" pure
add_pure_member "$pkc_bls" "pub fn f(sig: &$blst_ty::Signature, pk: &$blst_ty::PublicKey) -> bool { <$ietf as dash_pkc::bls::BlsScheme>::verify(sig, &[0u8; 32][..], pk).is_ok() }"
expect 'disallowed method `dash_pkc::bls::BlsScheme::verify`' "a pure crate verifying through dash-pkc's scheme trait" pure
add_pure_member "$pkc_bls" "pub fn f(sig: &$blst_ty::Signature, pk: &$blst_ty::PublicKey) -> bool { sig.verify(true, b\"m\", b\"d\", &[], pk, true) == dash_pkc::__deps::blst::BLST_ERROR::BLST_SUCCESS }"
expect 'disallowed method `blst::min_pk::Signature::verify`' "a pure crate calling blst's verify through dash-pkc's re-export" pure
add_pure_member "$pkc_bls" "pub fn f(pks: &[$blst_ty::PublicKey]) -> bool { dash_pkc::__deps::blst::MultiPoint::validate(pks).is_ok() }"
expect 'disallowed method `blst::MultiPoint::validate`' "a pure crate calling blst's pooled trait method" pure
add_pure_member "dash-types = { workspace = true }" "pub fn f<T: dash_types::Signable>(t: &T, key: &T::Key, ctx: &T::Context) -> bool { t.verify(key, ctx).is_none() }"
expect 'disallowed method `dash_types::Signable::verify`' "a pure crate verifying through the base SDK's Signable trait" pure
# DEC-79: Core's serial verifications are allowed, next to a banned pooled one.
core_verify='use dash_pkc::bls::{BlsSchemeKind, core as bls}; pub fn f(sig: &bls::BlsSigValue, pk: &bls::BlsPkValue, pks: &[&bls::BlsPkValue], h: &[u8; 32]) -> bool { bls::verify_insecure(sig, pk, h, BlsSchemeKind::Basic).is_valid() && bls::verify_insecure_aggregated(sig, pks, &[*h], BlsSchemeKind::Legacy).is_valid() && bls::verify_secure_aggregated(sig, pks, h, BlsSchemeKind::Basic).is_valid() }'
add_pure_member "$pkc_bls" "$core_verify"
expect pass "a pure crate verifying through dash-pkc's serial bls::core" pure
add_pure_member "$pkc_bls" "$core_verify pub fn g(pk: &dash_pkc::bls::BlsPublicKey<$ietf>, sig: &dash_pkc::bls::BlsSignature<$ietf>) -> bool { pk.verify_digest(&[0u8; 32], sig).is_ok() }"
expect 'disallowed method `dash_pkc::bls::BlsPublicKey::verify_digest`' "a pure crate calling bls::core and a pooled dash-pkc verify" pure

echo "== layering: dash-evo never reaches imbl (EV addendum §1.1; EV0's cases on check-layers.py)"
# dash-evo is P, so the [bans] imbl entry (rule L2) covers its closure in the
# graph Cargo resolves for the workspace with every feature and target, widened
# by the declared dependencies of path packages: a forbidden package counts
# however it is reached (a feature of a shared dependency, review EV0-reshape-r3
# R3-M2's bypass; a feature of dash-evo, a rename, a re-export, a target-only
# table). A path to dash-dml is also a Cargo cycle, as dash-dml depends on
# dash-evo, so the cases plant imbl. Dev edges never link into the crate and pass.
renamed_imbl='review_imbl = { package = "imbl", version = "=7.0.2", optional = true }'
consensus_leak() {  # an optional imbl behind a dash-consensus feature, re-exported through dash-evo
    sed -i "/^\[dependencies\]/a $renamed_imbl" dash-consensus/Cargo.toml
    # Into dash-consensus's own [features] table where it has one (SB12a's dash-x11): a second
    # table would fail the manifest parse, not the layer rule.
    if grep -q '^\[features\]$' dash-consensus/Cargo.toml; then
        sed -i 's/^\[features\]$/&\nreview-leak = ["dep:review_imbl"]/' dash-consensus/Cargo.toml
    else
        sed -i 's/^\[dev-dependencies\]$/[features]\nreview-leak = ["dep:review_imbl"]\n\n&/' dash-consensus/Cargo.toml
    fi
    printf '%s\n' '#[cfg(feature = "review-leak")]' 'pub use review_imbl::OrdMap as ReviewMap;' >> dash-consensus/src/lib.rs
    printf '%s\n' 'pub use dash_consensus as ReviewBridge;' >> dash-evo/src/lib.rs
}
expect pass "clean tree" layers
consensus_leak
expect 'L2 \[dash-consensus\] dash-consensus -> imbl v7.0.2 \(crates.io\) \[normal\]' "imbl through a feature of dash-consensus, re-exported by dash-evo" layers
sed -i "/^\[dependencies\]/a $renamed_imbl" dash-evo/Cargo.toml
sed -i 's/^test-utils = \[\]$/test-utils = ["dep:review_imbl"]/' dash-evo/Cargo.toml
printf '%s\n' '#[cfg(feature = "test-utils")]' 'pub use review_imbl::OrdMap as ReviewMap;' >> dash-evo/src/lib.rs
expect 'L2 \[dash-evo\] dash-evo -> imbl v7.0.2 \(crates.io\) \[normal\]' "a renamed imbl behind dash-evo's test-utils, re-exported" layers
printf '\n[target.'"'"'cfg(target_os = "android")'"'"'.dependencies]\nimbl = "=7.0.2"\n' >> dash-evo/Cargo.toml
expect 'L2 \[dash-evo\] dash-evo -> imbl v7.0.2 \(crates.io\) \[normal, cfg\(target_os = "android"\)\]' "imbl only on Android" layers
sed -i '/^\[dev-dependencies\]/a imbl = "=7.0.2"' dash-evo/Cargo.toml
expect pass "imbl as a dev-dependency of dash-evo" layers
# A crate outside the workspace, so --all-features leaves its features off; only
# dash-dml turns on the one that pulls in imbl, and Cargo unifies it.
outside_leak() {
    mkdir -p "$base/review-outside/src"
    printf '[package]\nname = "review-outside"\nversion = "0.0.0"\nedition = "2024"\n\n[features]\nleak = ["dep:review_imbl"]\n\n[dependencies]\n%s\n' \
        "$renamed_imbl" > "$base/review-outside/Cargo.toml"
    echo '#[cfg(feature = "leak")] pub use review_imbl::OrdMap;' > "$base/review-outside/src/lib.rs"
    sed -i "/^\[dependencies\]/a review-outside = { path = \"$base/review-outside\" }" dash-evo/Cargo.toml
    sed -i "/^\[dependencies\]/a review-outside = { path = \"$base/review-outside\", features = [\"leak\"] }" dash-dml/Cargo.toml
}
outside_leak
expect 'L2 \[dash-evo\] review-outside v0.0.0 \(path [^)]*\) -> imbl .*: .*dash-evo → review-outside .*→ imbl' "imbl behind a non-member's feature that only dash-dml enables" layers
# The same feature with nothing in the workspace enabling it (review EV0-reshape-opus-high
# m5): not in the resolved graph, but a consumer outside the workspace can turn it on.
# The case is void if the sed stops matching, so it then passes, which `expect` reports.
unenabled() { if grep -q review-outside dash-dml/Cargo.toml; then echo "dash-dml still enables leak"; else layers; fi; }
outside_leak
sed -i '/^review-outside = .*"leak"/d' dash-dml/Cargo.toml
expect 'L2 \[dash-evo\] review-outside v0.0.0 \(path [^)]*\) -> imbl .*: .*dash-evo → review-outside .*→ imbl' "imbl behind a non-member's feature nothing in the workspace enables" unenabled
# One level further: the optional dependency is a path crate no build resolves, which
# depends on imbl itself. Its dependencies are unknown, so the check fails on it.
outside_leak
sed -i '/^review-outside = .*"leak"/d' dash-dml/Cargo.toml
mkdir -p "$base/review-mid/src" && touch "$base/review-mid/src/lib.rs"
printf '[package]\nname = "review-mid"\nversion = "0.0.0"\nedition = "2024"\n\n[dependencies]\nimbl = "=7.0.2"\n' > "$base/review-mid/Cargo.toml"
sed -i "s|^review_imbl = .*|review-mid = { path = \"$base/review-mid\", optional = true }|; s|dep:review_imbl|dep:review-mid|" "$base/review-outside/Cargo.toml"
echo > "$base/review-outside/src/lib.rs"
expect 'L2 \[dash-evo\] review-outside .*-> review-mid \[normal\]: review-mid is a path or git dependency no build resolves' "an unresolved path crate behind a non-member's feature nothing enables" unenabled
rm -rf "$base/review-outside" "$base/review-mid"

echo "== layer rules (SB0 §2.3; check-layers.py)"
# set_layer <crate> <layer>: classify a planted crate in layers.toml.
set_layer() { sed -i "/^\[crates\]/a $1 = \"$2\"" contrib/node/layers.toml; }
# outside <name> <deps toml>: a path crate outside the workspace, so nothing
# classifies it and a path through it is a transitive one.
outside() {
    mkdir -p "$base/$1/src"
    printf '[package]\nname = "%s"\nversion = "0.0.0"\nedition = "2024"\n\n[dependencies]\n%s\n' "$1" "$2" > "$base/$1/Cargo.toml"
    : > "$base/$1/src/lib.rs"
}
# build_dep <crate dir> <deps toml>: a build-dependency (and a build script),
# in the crate's own [build-dependencies] when it has one (dash-spv does).
build_dep() {
    if grep -q '^\[build-dependencies\]$' "$1/Cargo.toml"; then
        sed -i "/^\[build-dependencies\]$/a $2" "$1/Cargo.toml"
    else
        printf '\n[build-dependencies]\n%s\n' "$2" >> "$1/Cargo.toml"
    fi
    [ -f "$1/build.rs" ] || echo 'fn main() {}' > "$1/build.rs"
}
node_crate() {  # node_crate <name> <deps toml>: an N crate
    add_member "$1" "$2" ''
    set_layer "$1" N
}
spv_dep() { sed -i "/^\[dependencies\]/a $1" dash-spv/Cargo.toml; }
pn_crate() { add_member dash-selftest-pn '' ''; set_layer dash-selftest-pn Pn; }
spv_path="dash-spv = { path = \"$wt/dash-spv\" }"

expect pass "clean tree (the W → W chain key-wallet-manager → key-wallet → dashcore included)" layers
add_member dash-selftest-w 'key-wallet-manager = { path = "../key-wallet-manager" }' ''
set_layer dash-selftest-w W
expect pass "a new W crate on the W → W chain (key-wallet-manager → key-wallet → dashcore)" layers

# L1
add_member dash-selftest-x '' ''
expect 'L1 \[dash-selftest-x\] unlisted' "a workspace crate layers.toml does not list" layers
set_layer dash-selftest-gone N
expect 'L1 \[dash-selftest-gone\] stale' "layers.toml listing a crate that is not a member and not pending" layers
sed -i 's/^dash-consensus = "P"$/dash-consensus = { layer = "P", pending = "selftest" }/' contrib/node/layers.toml
expect 'L1 \[dash-consensus\] pending' "a member still marked pending" layers
sed -i 's/^dash-consensus = "P"$/dash-consensus = "Q"/' contrib/node/layers.toml
expect "\`dash-consensus\` has layer 'Q'" "an unknown layer" layers
sed -i 's/^dash-consensus = "P"$/dash-consensus = { layer = "P", tier = "x" }/' contrib/node/layers.toml
expect "\`dash-consensus\` has unknown keys" "an unknown key in a crate's entry" layers
add_member dash-selftest-s '' "$fs_use"
sed -i '/^\[crates\]/a dash-selftest-s = { layer = "S", pure = true }' contrib/node/layers.toml
expect 'disallowed method `std::fs::read_to_string`' "an S crate marked pure using std::fs (L4)" pure

# L2: N → V, V → N, V → Pn; normal and build edges, direct and transitive.
node_crate dashd-selftest 'dash-spv = { path = "../dash-spv" }'
expect 'L2 \[dashd-selftest\] .*-> dash-spv' "N → V, direct normal edge" layers
node_crate dashd-selftest ''
build_dep dashd-selftest 'dash-spv = { path = "../dash-spv" }'
expect 'L2 \[dashd-selftest\] .*-> dash-spv' "N → V, direct build edge" layers
outside selftest-outside "$spv_path"
node_crate dashd-selftest "selftest-outside = { path = \"$base/selftest-outside\" }"
expect 'L2 \[dashd-selftest\] .*-> dash-spv .*: .*dashd-selftest → selftest-outside .*→ dash-spv' "N → V, transitive normal edge (through a crate outside the workspace)" layers
outside selftest-outside "$spv_path"
node_crate dashd-selftest ''
build_dep dashd-selftest "selftest-outside = { path = \"$base/selftest-outside\" }"
expect 'L2 \[dashd-selftest\] .*-> dash-spv .*: .*dashd-selftest → selftest-outside .*→ dash-spv' "N → V, transitive build edge" layers
node_crate dashd-selftest-mid 'dash-spv = { path = "../dash-spv" }'
node_crate dashd-selftest 'dashd-selftest-mid = { path = "../dashd-selftest-mid" }'
expect 'L2 \[dashd-selftest-mid\] .*-> dash-spv' "N → V, transitive through another N crate (reported where the edge leaves N)" layers
spv_dep 'dashd-store = { path = "../dashd-store" }'
expect 'L2 \[dash-spv\] .*-> dashd-store' "V → N, direct normal edge" layers
build_dep dash-spv 'dashd-store = { path = "../dashd-store" }'
expect 'L2 \[dash-spv\] .*-> dashd-store' "V → N, direct build edge" layers
outside selftest-outside "dashd-store = { path = \"$wt/dashd-store\" }"
spv_dep "selftest-outside = { path = \"$base/selftest-outside\" }"
expect 'L2 \[dash-spv\] .*-> dashd-store .*: .*dash-spv → selftest-outside .*→ dashd-store' "V → N, transitive normal edge" layers
outside selftest-outside "dashd-store = { path = \"$wt/dashd-store\" }"
build_dep dash-spv "selftest-outside = { path = \"$base/selftest-outside\" }"
expect 'L2 \[dash-spv\] .*-> dashd-store .*: .*dash-spv → selftest-outside .*→ dashd-store' "V → N, transitive build edge" layers
pn_crate
spv_dep 'dash-selftest-pn = { path = "../dash-selftest-pn" }'
expect 'L2 \[dash-spv\] .*-> dash-selftest-pn' "V → Pn, direct normal edge" layers
pn_crate
build_dep dash-spv 'dash-selftest-pn = { path = "../dash-selftest-pn" }'
expect 'L2 \[dash-spv\] .*-> dash-selftest-pn' "V → Pn, direct build edge" layers
pn_crate
outside selftest-outside "dash-selftest-pn = { path = \"$wt/dash-selftest-pn\" }"
spv_dep "selftest-outside = { path = \"$base/selftest-outside\" }"
expect 'L2 \[dash-spv\] .*-> dash-selftest-pn .*: .*dash-spv → selftest-outside .*→ dash-selftest-pn' "V → Pn, transitive normal edge" layers
pn_crate
outside selftest-outside "dash-selftest-pn = { path = \"$wt/dash-selftest-pn\" }"
build_dep dash-spv "selftest-outside = { path = \"$base/selftest-outside\" }"
expect 'L2 \[dash-spv\] .*-> dash-selftest-pn .*: .*dash-spv → selftest-outside .*→ dash-selftest-pn' "V → Pn, transitive build edge" layers
spv_dep 'dashd-store = { path = "../dashd-store", optional = true }'
expect 'L2 \[dash-spv\] .*-> dashd-store' "V → N behind an optional dependency" layers
# git-state, not dashcore: dashcore reaches dash-consensus through dash-evo (SB7), so
# dash-consensus → dashcore is a cargo cycle, not an L2 finding.
sed -i '/^\[dependencies\]/a git-state = { path = "../git-state" }' dash-consensus/Cargo.toml
expect 'L2 \[dash-consensus\] .*-> git-state' "P → W" layers
add_member dash-selftest-dev '' ''
set_layer dash-selftest-dev N
printf '\n[dev-dependencies]\ndash-spv = { path = "../dash-spv" }\n' >> dash-selftest-dev/Cargo.toml
expect pass "N dev-depending on V (dev edges never reach a dependent)" layers
# An optional path dependency of a crate outside the workspace that nothing
# turns on: no build resolves it, so its layer is unknown (EV0's case).
outside selftest-mid 'imbl = "=7.0.2"'
outside selftest-outside "selftest-mid = { path = \"$base/selftest-mid\", optional = true }"
sed -i "/^\[dependencies\]/a selftest-outside = { path = \"$base/selftest-outside\" }" dash-consensus/Cargo.toml
expect 'L2 \[dash-consensus\] .*-> selftest-mid \[normal\]: selftest-mid is a path or git dependency no build resolves' "an unresolved path crate behind an option nothing enables" layers
# Review r1 FP3: a declared path dependency matches its own package, not
# another of the same name (serde from crates.io).
outside serde "dashd-store = { path = \"$wt/dashd-store\" }"
outside selftest-mid "serde = { path = \"$base/serde\", optional = true }"
spv_dep "selftest-mid = { path = \"$base/selftest-mid\" }"
expect 'L2 \[dash-spv\] .*-> serde \[normal\]: serde is a path or git dependency no build resolves' "an unresolved path crate named like a resolved one (serde)" layers

# An unlisted wrapper crate: L1 names it, and the path through it is the N crate's L3.
add_member dash-selftest-wrap 'dashcore = { path = "../dash" }' ''
node_crate dashd-selftest 'dash-selftest-wrap = { path = "../dash-selftest-wrap" }'
expect 'L1 \[dash-selftest-wrap\] unlisted' "an unlisted wrapper crate (L1)" layers
add_member dash-selftest-wrap 'dashcore = { path = "../dash" }' ''
node_crate dashd-selftest 'dash-selftest-wrap = { path = "../dash-selftest-wrap" }'
expect 'L3 \[dashd-selftest\] .*-> dashcore .*: .*dashd-selftest → dash-selftest-wrap → dashcore' "N reaching W through an unlisted wrapper crate (L3)" layers

# Sol r1 M1: an outside copy of an allowed crate is checked as a root of
# its own, and an outside dashd-* crate is N without a [crates] entry.
outside dash-sha256 ''
build_dep "$base/dash-sha256" "dash-args = { path = \"$wt/dash-args\" }"
spv_dep "sha_external = { package = \"dash-sha256\", path = \"$base/dash-sha256\" }"
expect 'L2 \[dash-sha256 v0.0.0 \(path [^]]*\)\] .*-> dash-args \[build\]' "V → an outside copy of a P crate → N by a build edge" layers
outside dash-dml ''
outside dash-sha256 "dash-dml = { path = \"$base/dash-dml\" }"
spv_dep "sha_external = { package = \"dash-sha256\", path = \"$base/dash-sha256\" }"
expect 'L2 \[dash-sha256 v0.0.0 \(path [^]]*\)\] .*-> dash-dml v0.0.0' "V → an outside copy of a P crate → an outside copy of a Pn crate" layers
outside dashd-selftest-out ''
spv_dep "dashd-selftest-out = { path = \"$base/dashd-selftest-out\" }"
expect 'L2 \[dash-spv\] dash-spv -> dashd-selftest-out v0.0.0' "V → an unlisted dashd-* crate outside the workspace" layers
outside dashd-selftest-out ''
spv_dep "dashd-selftest-out = { path = \"$base/dashd-selftest-out\" }"
expect 'L6 \[dash-spv-ffi \[aarch64-linux-android\]\] dash-spv -> dashd-selftest-out v0.0.0' "an unlisted dashd-* crate outside the workspace in the mobile closure" mobile
add_member dashd-selftest '' ''
set_layer dashd-selftest T
expect 'L1 \[dashd-selftest\] pattern' "a dashd-* crate listed outside N" layers
# Review r1 FP4: names compare as crates.io does (case, `-` and `_`).
for name in dashd_selftest_out Dashd-selftest-out; do
    outside "$name" ''
    spv_dep "selftest_out = { package = \"$name\", path = \"$base/$name\" }"
    expect "L2 \\[dash-spv\\] dash-spv -> $name v0.0.0" "V → an outside crate named $name (dashd-* up to case and separator)" layers
done
# Review r1 FP8: L4 and L5 lint members only, so an outside P or S copy fails.
outside dash-x11 ''
spv_dep "x11_copy = { package = \"dash-x11\", path = \"$base/dash-x11\" }"
expect 'L1 \[dash-x11 v0.0.0 \(path [^]]*\)\] outside' "V → an outside copy of a P crate (dash-x11), which check-pure cannot lint" layers
outside dash-chain ''
spv_dep "chain_copy = { package = \"dash-chain\", path = \"$base/dash-chain\" }"
expect 'L1 \[dash-chain v0.0.0 \(path [^]]*\)\] outside' "V → an outside copy of an S crate (dash-chain), which check-shared cannot lint" layers
# Sol r3 R3-3 (DEC-170): `pure = true` holds for every package of the name.
sed -i '/^\[crates\]/a dashd-selftest-pure = { layer = "N", pure = true, pending = "selftest" }' contrib/node/layers.toml
outside dashd-selftest-pure ''
echo "$fs_use" > "$base/dashd-selftest-pure/src/lib.rs"
sed -i "/^\[dependencies\]/a dashd-selftest-pure = { path = \"$base/dashd-selftest-pure\" }" dash-rpc-spec/Cargo.toml
expect 'L1 \[dashd-selftest-pure v0.0.0 \(path [^]]*\)\] outside' "N → an outside copy of an N crate marked pure, which check-pure cannot lint" layers
# B: the base SDK's packages depend on nothing of this repository.
outside selftest-b "dashcore = { path = \"$wt/dash\" }"
sed -i "s#^sources = \[#sources = [\"path+file://$base/selftest-b\", #" contrib/node/layers.toml
spv_dep "selftest-b = { path = \"$base/selftest-b\" }"
expect 'L2 \[selftest-b v0.0.0 \(path [^]]*\)\] selftest-b .*-> dashcore \[normal\]' "a base SDK package (B) depending on W" layers
outside selftest-b ''
sed -i "s#^sources = \[#sources = [\"path+file://$base/selftest-b\", #" contrib/node/layers.toml
sed -i "/^\[dependencies\]/a selftest-b = { path = \"$base/selftest-b\" }" dash-consensus/Cargo.toml
expect pass "a P crate depending on B" layers

# L3: N reaches W only through allow_w; the hot path never.
node_crate dashd-selftest 'dashcore = { path = "../dash" }'
expect 'L3 \[dashd-selftest\] .*-> dashcore' "N → W outside allow_w" layers
outside dashcore ''
sed -i "/^\[dependencies\]/a dashcore = { path = \"$base/dashcore\" }" dash-consensus/Cargo.toml
expect 'L2 \[dash-consensus\] .*-> dashcore' "P → a copy of a W crate from outside the workspace" layers
node_crate dashd-wallet 'key-wallet = { path = "../key-wallet" }'
node_crate dashd-selftest 'dashd-wallet = { path = "../dashd-wallet" }'
expect pass "N reaching W through dashd-wallet (allow_w)" layers
node_crate dashd-wallet 'key-wallet = { path = "../key-wallet" }'
node_crate dashd-mempool 'dashd-wallet = { path = "../dashd-wallet" }'
expect 'L3 \[dashd-mempool \(hot path\)\] .*-> (dashcore|key-wallet).*: .*dashd-mempool → dashd-wallet → ' "a hot-path crate reaching W through allow_w" layers
# Review r1 FP1: allow_w names the workspace crate, not any crate of that name.
outside dashd-wallet "dashcore = { path = \"$wt/dash\" }"
sed -i "/^\[dependencies\]/a dashd-wallet = { path = \"$base/dashd-wallet\" }" dash-rpc-spec/Cargo.toml
expect 'L3 \[dashd-wallet v0.0.0 \(path [^]]*\)\] .*-> dashcore \[normal\]: dashd-wallet \(N\) reaches W outside' "an outside crate named dashd-wallet (allow_w) reaching W" layers
# Sol r2 R2-2 (DEC-170): the hot path is every package of that name.
node_crate dashd-wallet 'key-wallet = { path = "../key-wallet" }'
outside dashd-p2p "dashd-wallet = { path = \"$wt/dashd-wallet\" }"
sed -i "/^\[dependencies\]/a dashd-p2p = { path = \"$base/dashd-p2p\" }" dash-rpc-spec/Cargo.toml
expect 'L3 \[dashd-p2p v0.0.0 \(path [^]]*\) \(hot path\)\] .*-> (dashcore|key-wallet).*: .*dashd-p2p v0.0.0 .*→ dashd-wallet → ' "an outside copy of a hot-path crate reaching W through allow_w" layers
outside selftest-outside 'key-wallet = { path = "'"$wt"'/key-wallet" }'
node_crate dashd-mempool "selftest-outside = { path = \"$base/selftest-outside\" }"
expect 'L3 \[dashd-mempool \(hot path\)\] .*-> (dashcore|key-wallet)' "a hot-path crate reaching W transitively" layers
# dash-coreimport has its own [features] since P8b (default = ["full"]), so the planted features go
# into that table: a second [features] is a duplicate key, and `full` is taken.
coreimport_wallet() {  # coreimport_wallet <dependency toml> <features toml>
    sed -i "/^\[dependencies\]/a $1" dash-coreimport/Cargo.toml
    grep -q '^\[features\]$' dash-coreimport/Cargo.toml || { echo "FAIL  dash-coreimport has no [features] to plant into"; exit 1; }
    sed -i "/^\[features\]$/a $2" dash-coreimport/Cargo.toml
}
coreimport_wallet 'key-wallet = { path = "../key-wallet", optional = true }' 'wallet = ["dep:key-wallet"]'
expect pass "dash-coreimport reaching W only through its wallet feature" layers
coreimport_wallet 'key-wallet = { path = "../key-wallet", optional = true }' 'wallet = ["dep:key-wallet"]\nextra = ["wallet"]'
expect pass "the same, with another feature that turns wallet on" layers
coreimport_wallet 'key-wallet = { path = "../key-wallet" }' 'wallet = []'
expect 'L3 \[dash-coreimport without wallet\] .*-> key-wallet' "dash-coreimport reaching W with its wallet feature off" layers
coreimport_wallet 'key-wallet = { path = "../key-wallet", optional = true }' 'wallet = []\nother = ["dep:key-wallet"]'
expect 'L3 \[dash-coreimport without wallet\] .*-> key-wallet' "dash-coreimport reaching W through a feature other than wallet" layers
# Sol r1 M2: a feature another workspace crate unifies onto the hot path's
# closure counts, through N and allow_w alike.
coreimport_wallet 'key-wallet = { path = "../key-wallet", optional = true }' 'wallet = ["dep:key-wallet"]'
add_member dashd-selftest-bridge 'dash-coreimport = { path = "../dash-coreimport", optional = true }' ''
sed -i 's/^std = \[\]$/&\nlegacy = ["dep:dash-coreimport", "dash-coreimport\/wallet"]/' dashd-selftest-bridge/Cargo.toml
set_layer dashd-selftest-bridge N
node_crate dashd-mempool 'dashd-selftest-bridge = { path = "../dashd-selftest-bridge" }'
add_member dash-selftest-driver 'dashd-selftest-bridge = { path = "../dashd-selftest-bridge", features = ["legacy"] }' ''
set_layer dash-selftest-driver T
expect 'L3 \[dashd-mempool \(hot path\)\] dash-coreimport -> key-wallet \[normal\]: .*dashd-mempool → dashd-selftest-bridge → dash-coreimport → key-wallet' "a hot-path crate reaching W through allow_w by a feature another crate turns on" layers
node_crate dashd-selftest-bridge 'dash-args = { path = "../dash-args" }'
node_crate dashd-mempool 'dashd-selftest-bridge = { path = "../dashd-selftest-bridge" }'
expect pass "a hot-path crate on an N crate that reaches no W" layers
outside selftest-helper "dash-args = { path = \"$wt/dash-args\", optional = true }

[features]
legacy = [\"dep:dash-args\"]"
spv_dep "selftest-helper = { path = \"$base/selftest-helper\" }"
add_member dash-selftest-driver "selftest-helper = { path = \"$base/selftest-helper\", features = [\"legacy\"] }" ''
set_layer dash-selftest-driver T
expect 'L2 \[dash-spv\] selftest-helper v0.0.0 .*-> dash-args \[normal\]' "V → N through a feature another crate turns on in an unlayered helper" layers

# [bans]
sed -i '/^\[dependencies\]/a bincode = "=2.0.1"' dashd-store/Cargo.toml
expect 'L8 \[dashd-store\] .*-> bincode' "an N crate depending on bincode (L8)" layers
sed -i '/^\[dependencies\]/a bincode = { workspace = true }' dashd-store/Cargo.toml
expect 'L8 \[dashd-store\] dashd-store -> grovedb-bincode v' "an N crate depending on the workspace's bincode (grovedb-bincode)" layers
renamed_imbl='selftest_imbl = { package = "imbl", version = "=7.0.2", optional = true }'
sed -i "/^\[dependencies\]/a $renamed_imbl" dash-consensus/Cargo.toml
expect 'L2 \[dash-consensus\] .*-> imbl' "a P crate with an optional, renamed imbl (EV addendum §1.1)" layers
add_member dash-selftest-pn "$renamed_imbl" ''
set_layer dash-selftest-pn Pn
expect pass "a Pn crate using imbl" layers
# Review r1 FP2: a ban walk passes W (bincode's exemption only) and checks behind it.
sed -i '/^\[dependencies\]/a imbl = { version = "=7.0.2", optional = true }' key-wallet/Cargo.toml
sed -i 's/^\[features\]$/&\nmaps = ["dep:imbl"]/' key-wallet/Cargo.toml
sed -i 's/^\[features\]$/&\nmaps = ["key-wallet\/maps"]/' dash-spv/Cargo.toml
expect 'L2 \[dash-spv\] key-wallet -> imbl' "V → W → imbl through a W feature" layers

# The baseline ratchet: every offending edge, per rule and root.
baseline() {  # baseline <rule> <root> <edge> <closes>
    printf '\n[[violation]]\nrule = "%s"\nroot = "%s"\nedges = ["%s"]\ncloses = "%s"\n' "$@" >> contrib/node/layers-baseline.toml
}
node_crate dashd-selftest 'dash-spv = { path = "../dash-spv" }'
baseline L2 dashd-selftest 'dashd-selftest -> dash-spv [normal]' selftest
expect pass "a listed violation" layers
baseline L2 dashd-selftest 'dashd-selftest -> dash-spv [normal]' selftest
expect 'L2 \[dashd-selftest\] dashd-selftest -> dash-spv \[normal\] is listed in layers-baseline.toml .* no longer occurs' "a stale baseline entry (L2)" layers
baseline L1 dash-selftest-gone unlisted selftest
expect 'L1 \[dash-selftest-gone\] unlisted is listed .* no longer occurs' "a stale baseline entry (L1)" layers
baseline L3 dashd-selftest 'dashd-selftest -> dashcore [normal]' selftest
expect 'L3 \[dashd-selftest\] dashd-selftest -> dashcore \[normal\] is listed .* no longer occurs' "a stale baseline entry (L3)" layers
baseline L8 dashd-store 'dashd-store -> bincode v2.0.1 (crates.io) [normal]' selftest
expect 'L8 \[dashd-store\] dashd-store -> bincode v2.0.1 \(crates.io\) \[normal\] is listed .* no longer occurs' "a stale baseline entry (L8)" layers
baseline L6 'dash-spv-ffi [aarch64-linux-android]' 'dash-spv-ffi -> rocksdb v0.24.0 (crates.io) [normal]' selftest
expect 'L6 \[dash-spv-ffi \[aarch64-linux-android\]\] dash-spv-ffi -> rocksdb .* no longer occurs' "a stale baseline entry (L6)" mobile
printf '\n[[violation]]\nrule = "L2"\nroot = "x"\nedges = ["x -> y [normal]"]\n' >> contrib/node/layers-baseline.toml
expect 'an entry needs `rule`' "a baseline entry without its closing row" layers
baseline L5 dash-chain 'thread' selftest
expect 'an entry needs `rule`' "a baseline entry for a rule that cannot be baselined" layers
# Sol r1 M7: the identity is the offending edge, so another edge to a listed
# package fails: the same endpoints by a build edge, or a parallel route.
spv_dep 'dashd-store = { path = "../dashd-store" }'
build_dep dash-spv 'dashd-store = { path = "../dashd-store" }'
baseline L2 dash-spv 'dash-spv -> dashd-store [normal]' selftest
expect 'L2 \[dash-spv\] dash-spv -> dashd-store \[build\]' "a listed V → N edge joined by a build edge between the same crates" layers
spv_dep 'dashd-store = { path = "../dashd-store" }'
outside selftest-route "[build-dependencies]
dashd-store = { path = \"$wt/dashd-store\" }"
echo 'fn main() {}' > "$base/selftest-route/build.rs"
spv_dep "selftest-route = { path = \"$base/selftest-route\" }"
baseline L2 dash-spv 'dash-spv -> dashd-store [normal]' selftest
expect 'L2 \[dash-spv\] selftest-route v0.0.0 .*-> dashd-store \[build\]' "a listed V → N edge joined by a parallel route through a build edge" layers

echo "== mobile closure (L6) and size (L7)"
expect pass "clean tree" mobile
# O5 (DEC-112): blst's `no-threads` only from dash-spv-ffi, never in an N graph.
sed -i '/^\[dependencies\]/a selftest_blst = { package = "blst", version = "0.3.17", features = ["no-threads"] }' dashd-store/Cargo.toml
expect 'O5 \[dashd-store\] blst v0\.3\.17 feature "no-threads": dashd-store \(N\) turns on blst' "blst no-threads planted in an N package" mobile
sed -i '/^\[dependencies\]/a dash-spv-ffi = { path = "../dash-spv-ffi", features = ["blst-no-threads"] }' dashd-store/Cargo.toml
expect 'O5 \[dashd-store\] blst v0\.3\.17 feature "no-threads": .* ← dash-spv-ffi v' "an N package turning on dash-spv-ffi's blst-no-threads" mobile
sed -i '/^\[dependencies\]/a dashd-store = { path = "../dashd-store" }' dash-spv-ffi/Cargo.toml
expect 'L6 \[dash-spv-ffi \[aarch64-linux-android\]\] .*-> dashd-store' "dash-spv-ffi depending on an N crate" mobile
sed -i '/^\[dependencies\]/a dashd-store = { path = "../dashd-store" }' dash-spv-ffi/Cargo.toml
expect 'L6 \[dash-spv-ffi \[aarch64-apple-ios\]\] .*-> rocksdb .*: .*dash-spv-ffi → dashd-store → rocksdb' "RocksDB in dash-spv-ffi's closure (iOS)" mobile
spv_dep 'tikv-jemallocator = "=0.7.0"'
expect 'L6 \[dash-spv-ffi \[aarch64-linux-android\]\] .*-> tikv-jemallocator .*: .*dash-spv-ffi → dash-spv → tikv-jemallocator' "jemalloc through dash-spv" mobile
printf '\n[target.'"'"'cfg(target_os = "android")'"'"'.dependencies]\ntikv-jemallocator = "=0.7.0"\n' >> key-wallet-ffi/Cargo.toml
expect 'L6 \[key-wallet-ffi \[aarch64-linux-android\]\] .*-> tikv-jemallocator' "jemalloc in key-wallet-ffi on Android only" mobile
printf '\n[target.'"'"'cfg(target_os = "linux")'"'"'.dependencies]\ntikv-jemallocator = "=0.7.0"\n' >> key-wallet-ffi/Cargo.toml
expect pass "jemalloc in key-wallet-ffi on desktop Linux only" mobile
# Sol r2 R2-4 (DEC-170): a desktop-only declaration of a package that is
# also an Android edge is not an Android edge.
outside clap ''
target_dep() { printf '\n[target.'"'"'cfg(target_os = "%s")'"'"'.dependencies]\nselftest_clap = { package = "clap", path = "%s" }\n' "$1" "$base/clap" >> key-wallet-ffi/Cargo.toml; }
target_dep android
for root in key-wallet-ffi dash-spv-ffi; do
    printf '\n[[violation]]\nrule = "L6"\nroot = "%s [aarch64-linux-android]"\nedges = [%s]\ncloses = "selftest"\n' \
        "$root" "'key-wallet-ffi -> clap v0.0.0 (path $base/clap) [normal, cfg(target_os = \"android\")]'" >> contrib/node/layers-baseline.toml
done
android_then_linux() { mobile && target_dep linux && mobile; }
expect pass "clap on Android, listed, then declared again for desktop Linux" android_then_linux
sed -i '/^\[dev-dependencies\]/a tikv-jemallocator = "=0.7.0"' key-wallet-ffi/Cargo.toml
expect pass "jemalloc as a dev-dependency of key-wallet-ffi" mobile
outside clap ''
sed -i "/^\[dependencies\]/a clap_copy = { package = \"clap\", path = \"$base/clap\" }" dash-spv-ffi/Cargo.toml
expect 'L6 \[dash-spv-ffi \[aarch64-linux-android\]\] dash-spv-ffi -> clap v0.0.0 \(path ' "a second source of a listed L6 package (clap from a path)" mobile
size_bytes="$(sed -n 's/^bytes = //p' contrib/node/mobile-size.txt)"
size_env=(--profile "$(sed -n 's/^profile = //p' contrib/node/mobile-size.txt)"
    --ndk "$(sed -n 's/^ndk = //p' contrib/node/mobile-size.txt)"
    --api "$(sed -n 's/^api = //p' contrib/node/mobile-size.txt)")
size() {  # size <bytes> [flag value]...: the recorded environment, flags override it
    local bytes="$1"; shift
    python3 contrib/node/check-layers.py size "$bytes" "${size_env[@]}" "$@"
}
expect pass "the recorded size + 100 KiB" size $((size_bytes + 100 * 1024))
expect 'L7: libdash_spv_ffi.so grew by 307200 B, over' "the recorded size + 300 KiB" size $((size_bytes + 300 * 1024))
expect 'L7: measured with profile .*baseline migration' "a size measured with another profile" size "$size_bytes" --profile selftest
expect 'L7: measured with ndk .*baseline migration' "a size measured with another NDK" size "$size_bytes" --ndk 30.0.0
expect 'L7: measured with api .*baseline migration' "a size measured with another API level" size "$size_bytes" --api 35
expect 'needs a new ruling' "recording +300 KiB without a ruling" size $((size_bytes + 300 * 1024)) --record
expect 'needs a new ruling' "recording another NDK without a ruling" size "$size_bytes" --ndk 30.0.0 --record
# Sol r1 M5: re-recording within the allowance does not move the reference.
# One step fits the reference's allowance, min(2 %, 256 KiB), and two do not,
# whatever the record's profile (BL-329: the mobile record's is 185 KiB).
allowance=$((size_bytes * 2 / 100 < 256 * 1024 ? size_bytes * 2 / 100 : 256 * 1024))
step=$((allowance / 1024 * 1024))
rerecord() { size $((size_bytes + step)) --record && size $((size_bytes + 2 * step)) --record; }
expect 'needs a new ruling' "recording one step twice (two in total) without a ruling" rerecord
recheck() { size $((size_bytes + step)) --record && size $((size_bytes + 2 * step)); }
expect "L7: libdash_spv_ffi.so grew by $((2 * step)) B" "two steps measured after recording one" recheck
# Sol r2 R2-1 (DEC-170): nor does committing a record.
committed() {
    size $((size_bytes + step)) --record && git commit -qm selftest contrib/node/mobile-size.txt &&
        size $((size_bytes + 2 * step)) --record
}
expect 'needs a new ruling' "recording one step, committing it, then recording two without a ruling" committed
# Sol r3 R3-1: nor does naming another base in the environment.
redirected() {
    size $((size_bytes + step)) --record && git commit -qm selftest contrib/node/mobile-size.txt &&
        git commit -q --allow-empty -m selftest && L7_BASE_REF=HEAD size $((size_bytes + 2 * step))
}
expect "L7: libdash_spv_ffi.so grew by $((2 * step)) B" "two steps after committing one, with L7_BASE_REF=HEAD" redirected
sed -i "s/^bytes = .*/bytes = $((size_bytes + 300 * 1024))/" contrib/node/mobile-size.txt
expect "L7: contrib/node/mobile-size.txt moved from the reference" "a hand-edited record +300 KiB without a ruling" size $((size_bytes + 300 * 1024))
# A ruling is a decision new to this change that states what it covers
# (DEC-170), in its decision log.
decision() { printf '| %s | selftest: %s | 2026-10-10 |\n' "$1" "$2" >> docs/node-program/DECISIONS.md; }
ruled() { decision DEC-99998 "L7 at $1 B" && size $((size_bytes + 300 * 1024)) --record --ruling DEC-99998 && size $((size_bytes + 300 * 1024)); }
expect pass "+300 KiB recorded with a new ruling" ruled $((size_bytes + 300 * 1024))
expect 'needs a new ruling' "+300 KiB recorded with a new ruling that states another size" ruled $((size_bytes + 200 * 1024))
ruled_over() { decision DEC-99998 "L7 at $((size_bytes + 300 * 1024)) B" && size $((size_bytes + 300 * 1024)) --record --ruling DEC-99998 && size $((size_bytes + 310 * 1024)); }
expect 'over the .* B that ruling DEC-99998 covers' "more than the ruled size" ruled_over
migrated() { decision DEC-99998 "L7 at $size_bytes B with NDK $1" && size "$size_bytes" --ndk 30.0.0 --record --ruling DEC-99998 && size "$size_bytes" --ndk 30.0.0; }
expect pass "another NDK recorded with a new ruling" migrated 30.0.0
expect 'needs a new ruling' "another NDK recorded with a new ruling that states another NDK" migrated 31.0.0
expect 'needs a new ruling' "+300 KiB recorded with a ruling older than this change" size $((size_bytes + 300 * 1024)) --record --ruling DEC-81
# Review r1 FP9: a ruling is a decision of the log.
for ruling in n/a DEC-99999; do
    expect 'names no decision in docs/node-program/DECISIONS.md' "+300 KiB recorded with ruling $ruling" size $((size_bytes + 300 * 1024)) --record --ruling "$ruling"
    sed -i -e "s/^bytes = .*/bytes = $((size_bytes + 300 * 1024))/" -e "s#^ruling = .*#ruling = $ruling#" contrib/node/mobile-size.txt
    expect 'moved from the reference' "a hand-edited record +300 KiB with ruling $ruling" size $((size_bytes + 300 * 1024))
done
# DEC-170: while the merge base has no record, the reference is layers.toml's
# [size], whose value the decision it cites states. This repository's
# node/integration has carried a record since train 10, so these cases run in
# a fixture repository whose node/integration holds HEAD's decision log and
# layers.toml (without [size] unless asked: a base from before SB1) but no
# record, and whose HEAD (`change`) adds both; the case's own edits stay
# uncommitted, as a change's would. The fixture's reference is [size], not the
# record (which SB2 moved to the mobile profile, DEC-318), so these cases take
# the environment, the decision text and the bytes from HEAD's [size], and the
# fixture restates the record in [size]'s environment (BL-329, DEC-326).
constant() { sed -i "/^\[size\]/,\$ s/^$1 = .*/$1 = $2/" contrib/node/layers.toml; }
const_value() { git show HEAD:contrib/node/layers.toml | sed -n "/^\[size\]/,/^\$/ s/^$1 = //p" | tr -d '"'; }
const_bytes="$(const_value bytes)"
const_env=(--profile "$(const_value profile)" --ndk "$(const_value ndk)" --api "$(const_value api)")
size_text="$(const_value library) $(const_value target) $(const_value profile) API $(const_value api) "
big=$((const_bytes + 300 * 1024))
const_size() {  # const_size <bytes> [flag value]...: [size]'s environment, flags override it
    local bytes="$1"; shift
    python3 contrib/node/check-layers.py size "$bytes" "${const_env[@]}" "$@"
}
no_record_base() {  # no_record_base [with-size]: cd into a fresh fixture repository on `change`
    local f="$base/l7-fixture"
    rm -rf "$f" && mkdir -p "$f/contrib/node" "$f/docs/node-program" && cd "$f" &&
        cp "$wt/contrib/node/check-layers.py" contrib/node/ &&
        git -C "$wt" show HEAD:contrib/node/layers.toml > contrib/node/layers.toml &&
        git -C "$wt" show HEAD:docs/node-program/DECISIONS.md > docs/node-program/DECISIONS.md &&
        { [ "${1:-}" = with-size ] || sed -i '/^\[size\]/,/^$/d' contrib/node/layers.toml; } &&
        git init -q -b node/integration && git add contrib docs && git commit -qm base &&
        git checkout -qb change && git -C "$wt" show HEAD:contrib/node/layers.toml > contrib/node/layers.toml &&
        git -C "$wt" show HEAD:contrib/node/mobile-size.txt > contrib/node/mobile-size.txt &&
        sed -i -e "s/^profile = .*/profile = ${const_env[1]}/" -e "s/^ndk = .*/ndk = ${const_env[3]}/" \
            -e "s/^api = .*/api = ${const_env[5]}/" -e "s/^bytes = .*/bytes = $const_bytes/" contrib/node/mobile-size.txt &&
        git add contrib && git commit -qm record
}
dec170_moved() { no_record_base && constant bytes $big && const_size $big; }
expect "layers.toml's \[size\] gives .*bytes $big, which DEC-170 does not state" "layers.toml's [size] +300 KiB, still citing DEC-170" dec170_moved
dec170_rewritten() { no_record_base && constant bytes $big && decision DEC-170 "L7 is $size_text NDK ${const_env[3]}, $big B" && const_size $big; }
expect "layers.toml's \[size\] gives .*bytes $big, which DEC-170 does not state" "layers.toml's [size] +300 KiB, with DEC-170's row rewritten in the tree" dec170_rewritten
dec170_base_ref() {
    no_record_base && constant bytes $big && decision DEC-170 "L7 is $size_text NDK ${const_env[3]}, $big B" &&
        L7_BASE_REF="$(git merge-base HEAD node/integration)" const_size $big
}
expect "layers.toml's \[size\] gives .*bytes $big, which DEC-170 does not state" "the same, with L7_BASE_REF at the merge base" dec170_base_ref
cited() {  # cited <ndk text>: [size] +300 KiB citing a new decision that states the size and that text
    no_record_base && constant bytes $big && constant ruling '"DEC-99998"' &&
        decision DEC-99998 "L7 is $size_text$1, $big B" && const_size $big
}
expect pass "layers.toml's [size] +300 KiB, citing a new decision that states it" cited " NDK ${const_env[3]}"
expect "layers.toml's \[size\] gives ndk ${const_env[3]}, which DEC-99998 does not state" "layers.toml's [size] +300 KiB, citing a new decision that leaves out its NDK" cited ""
uncited() { no_record_base && constant ruling '"DEC-99999"' && const_size "$const_bytes"; }
expect "layers.toml's \[size\] cites DEC-99999, which names no decision" "layers.toml's [size] citing no decision" uncited
# The base's [size] itself moved: NDK 30 recorded against such a merge base, at
# [size]'s own bytes (BL-310: the record's differ since DEC-298, and the cited
# decision states the table's values, not the record's).
moved_constant() {  # moved_constant <ruling>
    no_record_base with-size &&
        sed -i -e 's/^ndk = .*/ndk = 30.0.0/' -e "s/^bytes = .*/bytes = $const_bytes/" -e 's/^ruling = .*/ruling = -/' \
            contrib/node/mobile-size.txt &&
        constant ndk '"30.0.0"' && constant ruling "\"$1\"" && decision DEC-99998 "L7 is $size_text NDK 30.0.0, $const_bytes B" &&
        git add contrib docs && git commit -qm change && const_size "$const_bytes" --ndk 30.0.0
}
expect "layers.toml's \[size\] moved from the merge base .* without a decision new to this change" "layers.toml's [size] moved, still citing DEC-170" moved_constant DEC-170
expect pass "layers.toml's [size] moved, citing a new decision that states it" moved_constant DEC-99998

echo "== the SPV client's threads (O5; check-spv.sh)"
expect pass "clean dash-spv" spv
printf '%s\n' 'pub fn selftest() { rayon::spawn(|| ()); }' >> dash-spv/src/lib.rs
expect 'disallowed method `rayon::spawn`' "dash-spv on rayon's global pool" spv
printf '%s\n' 'pub fn selftest() -> Result<(), rayon::ThreadPoolBuildError> { rayon::ThreadPoolBuilder::new().build_global() }' >> dash-spv/src/lib.rs
expect 'disallowed method `rayon::ThreadPoolBuilder::build_global`' "dash-spv building rayon's global pool" spv
printf '%s\n' 'pub fn selftest(v: &[u32]) -> u32 { use rayon::prelude::*; v.par_iter().sum() }' >> dash-spv/src/lib.rs
expect 'disallowed method `rayon::iter::IntoParallelRefIterator::par_iter`' "dash-spv iterating on rayon's global pool" spv

echo "== shared-crate hygiene (L5; check-shared.sh)"
# dash-chain as SB4 lands it, or a stub of it until then. The plants below
# edit the stub's empty `default` and `std` features, which the real crate
# lacks: give it them.
shared_plant() {  # shared_plant <deps toml> <lib.rs code>
    if [ -f dash-chain/Cargo.toml ]; then
        grep -q '^std = \[' dash-chain/Cargo.toml ||
            sed -i 's/^\[features\]$/&\ndefault = []\nstd = []/' dash-chain/Cargo.toml
        [ -z "$1" ] || sed -i "/^\[dependencies\]/a $1" dash-chain/Cargo.toml
        portable_tables dash-chain/Cargo.toml "$1"
        printf '%s\n' "$2" >> dash-chain/src/lib.rs
    else
        add_member dash-chain "$1" "$2"
    fi
}
tokio_rt='tokio = { version = "1", features = ["rt"] }'
shared_plant '' ''
expect pass "clean dash-chain" shared
shared_plant '' 'pub fn selftest() { std::thread::spawn(|| ()); }'
expect 'disallowed method `std::thread::spawn`' "dash-chain spawning a thread" shared
shared_plant '' 'pub fn selftest() -> std::io::Result<std::thread::JoinHandle<()>> { std::thread::Builder::new().spawn(|| ()) }'
expect 'disallowed type `std::thread::Builder`' "dash-chain building a thread" shared
shared_plant "$tokio_rt" 'pub fn selftest() { drop(tokio::task::spawn_blocking(|| ())); }'
expect 'disallowed method `tokio::task::spawn_blocking`' "dash-chain offloading with spawn_blocking" shared
shared_plant "$tokio_rt" ''
expect 'dash-chain depends on tokio; only dash-p2p' "dash-chain depending on tokio" shared
shared_plant "$tokio_rt" 'pub fn selftest() -> std::io::Result<tokio::runtime::Runtime> { tokio::runtime::Builder::new_current_thread().build() }'
expect 'disallowed type `tokio::runtime::(Builder|Runtime)`' "dash-chain building a runtime" shared
# dash-chain depends on rayon since SB5 (the host's injected pool); a stub does not.
grep -q '^rayon = ' dash-chain/Cargo.toml 2>/dev/null && rayon_dep= || rayon_dep='rayon = { workspace = true }'
shared_plant "$rayon_dep" 'pub fn selftest() -> Result<rayon::ThreadPool, rayon::ThreadPoolBuildError> { rayon::ThreadPoolBuilder::new().build() }'
expect 'disallowed type `rayon::ThreadPoolBuilder`' "dash-chain building a rayon pool" shared
shared_plant '' 'pub fn selftest() -> ! { std::process::exit(1) }'
expect 'disallowed method `std::process::exit`' "dash-chain exiting the process" shared
shared_plant '' 'pub fn selftest() -> std::io::Result<usize> { std::net::ToSocketAddrs::to_socket_addrs("x:1").map(Iterator::count) }'
expect 'disallowed (method|type) `std::net::ToSocketAddrs' "dash-chain resolving a name" shared
shared_plant '' '#[allow(clippy::disallowed_methods)] pub fn selftest() { std::thread::spawn(|| ()); }'
expect 'incompatible with previous forbid' "dash-chain allowing the lint it breaks" shared
# A reviewed [[l5-exception]] exempts its one call and nothing else (DEC-205).
l5_record() {  # l5_record <crate> <path>
    printf '\n[[l5-exception]]\ncrate = "%s"\npath = "%s"\nfile = "src/lib.rs"\nfunction = "selftest_fatal"\ndecision = "DEC-205"\nuntil = "BL-118"\nnote = "selftest"\n' "$1" "$2" >> contrib/node/layers.toml
}
l5_record dash-chain std::process::abort
shared_plant '' 'pub fn selftest_fatal() -> ! { std::process::abort() }'
expect pass "dash-chain's one abort under an l5-exception, with dash-mnlist building on dash-chain" shared
# Each S crate is linted alone (--no-deps, DEC-385): a hit in dash-chain fails
# dash-chain's own check, not dash-mnlist's, and does not keep dash-mnlist
# from being linted.
shared_plant '' 'pub fn selftest() -> ! { std::process::abort() }'
expect 'shared crates check failed for: dash-chain$' "an abort in dash-chain without an l5-exception, failing dash-chain only" shared
l5_record dash-chain std::process::abort
shared_plant '' $'pub fn selftest_fatal() -> ! { std::process::abort() }\npub fn selftest_other() -> ! { std::process::abort() }'
expect 'shared crates check failed for: dash-chain$' "a second abort in dash-chain beyond its l5-exception, failing dash-chain only" shared
l5_record dash-chain std::process::abort
shared_plant '' 'pub fn selftest_fatal() -> ! { std::process::abort() }'
printf '%s\n' 'pub fn selftest() { std::thread::spawn(|| ()); }' >> dash-mnlist/src/lib.rs
expect 'disallowed method `std::thread::spawn`' "dash-mnlist spawning a thread while dash-chain carries an exempt abort" shared
# --no-deps leaves a workspace dependency to its own check, so it must have one.
add_member dash-selftest-helper '' 'pub fn selftest() { std::thread::spawn(|| ()); }'
shared_plant 'dash-selftest-helper = { path = "../dash-selftest-helper" }' ''
expect 'dash-chain depends on dash-selftest-helper, a workspace crate outside S and P' "dash-chain depending on a workspace crate outside S and P" shared
l5_record dash-chain std::process::abort
shared_plant '' 'pub fn selftest_fatal() {}'
expect 'stale l5-exception \(DEC-205\): no direct call to `std::process::abort` in src/lib.rs fn selftest_fatal' "an l5-exception whose function no longer aborts" shared
l5_record dash-chain std::process::abort
expect 'stale l5-exception \(DEC-205\): src/lib.rs has no fn selftest_fatal' "an l5-exception whose function is gone" shared
l5_record dash-chain std::process::abort
shared_plant '' $'pub fn selftest_fatal() -> ! { std::process::abort() }\npub fn selftest_other() -> ! { std::process::abort() }'
expect 'disallowed method `std::process::abort`' "an abort outside the l5-exception's function" shared
l5_record dash-chain std::process::abort
shared_plant '' $'pub fn selftest_fatal() {}\npub fn selftest_other() -> ! { std::process::abort() }'
expect 'stale l5-exception \(DEC-205\): no direct call to' "an abort moved out of the l5-exception's function" shared
l5_record dash-chain std::process::abort
shared_plant '' $'pub fn selftest_fatal(x: bool) {\n    if x { std::process::abort() }\n    std::process::abort()\n}'
expect 'covers one call, but `std::process::abort` in src/lib.rs fn selftest_fatal has 2' "a second abort in the l5-exception's function" shared
l5_record dash-chain std::process::abort
shared_plant '' 'pub fn selftest_fatal() -> ! { std::process::exit(1) }'
expect 'disallowed method `std::process::exit`' "an exit where the l5-exception names abort" shared
l5_record dash-chain std::process::abort
shared_plant '' $'pub fn selftest_fatal() -> ! { let f = std::process::abort; f() }'
expect 'no direct call to `std::process::abort`' "the l5-exception's abort taken as a function pointer" shared
l5_record dash-chain std::process::abort
shared_plant '' $'pub fn selftest_fatal() {\n    #[macro_export]\n    macro_rules! selftest_die { () => { std::process::abort() } }\n}\npub fn selftest_other() -> ! { crate::selftest_die!() }'
expect 'declares an item .* in its body' "a macro declared in the l5-exception's function" shared
l5_record dash-chain std::process::abort
shared_plant '' $'pub fn selftest_fatal() -> ! { std::process::abort() }\npub mod selftest_m { pub fn selftest_fatal() -> ! { std::process::abort() } }'
expect 'has 2 fns named selftest_fatal' "two functions of the l5-exception's name" shared
l5_record dash-spv std::process::abort
expect 'dash-spv is not an S crate' "an l5-exception for a crate outside S" shared
# L5's allocator rule judges a consumer binary linked against the library
# (contrib/node/link-probe.py; Sol r2 R2-3/R2-5, DEC-170): host and Android,
# all, default and no features. Whatever puts an allocator in the product fails.
alloc='#[global_allocator] static A: std::alloc::System = std::alloc::System;'
# The source scan is advisory (DEC-188 (4)): its hits are warnings and the
# check passes. `advisory` reports them only when check-shared.sh passed.
advisory() {
    local out
    if out="$(shared 2>&1)"; then grep 'warning: ' <<< "$out" | sed 's/^/passed with /'; else printf '%s\n' "$out"; fi
    return 1
}
alloc_dep() { shared_plant "selftest-alloc = { path = \"$base/selftest-alloc\" }" 'pub use selftest_alloc as _selftest;'; }
shared_plant '' "$alloc"
expect 'dash-chain \(host, --all-features\): installs a global allocator: dash_chain' "dash-chain setting a global allocator" shared
shared_plant '' "#[cfg_attr(feature = \"std\", global_allocator)] static A: std::alloc::System = std::alloc::System;"
expect 'installs a global allocator: dash_chain' "dash-chain setting a global allocator through cfg_attr" shared
shared_plant '' "#[path = \"$base/selftest-alloc.inc\"] mod selftest_alloc;"
printf '#[cfg(target_os = "android")] %s\n' "$alloc" > "$base/selftest-alloc.inc"
expect 'dash-chain \(aarch64-linux-android, .*installs a global allocator: dash_chain' "dash-chain setting a global allocator on Android only, in a #[path] module outside the package" shared
outside selftest-alloc ''
printf '%s\n' "#[macro_export] macro_rules! install { () => { $alloc }; }" > "$base/selftest-alloc/src/lib.rs"
shared_plant "selftest-alloc = { path = \"$base/selftest-alloc\" }" 'selftest_alloc::install!();'
expect 'installs a global allocator: dash_chain' "dash-chain setting a global allocator through another crate's macro" shared
printf '%s\n' "$alloc" > "$base/selftest-alloc/src/lib.rs"
alloc_dep
expect 'installs a global allocator: selftest_alloc' "dash-chain linking a crate that sets a global allocator" shared
printf '%s\n' "$alloc" > "$base/selftest-alloc/src/lib.rs"
printf '[lib]\ncrate-type = ["dylib"]\n' >> "$base/selftest-alloc/Cargo.toml"
alloc_dep
expect 'installs a global allocator: selftest_alloc' "dash-chain linking a dylib that sets a global allocator" shared
outside selftest-alloc ''
printf '[features]\non = []\n' >> "$base/selftest-alloc/Cargo.toml"
printf '#[cfg(not(feature = "on"))] %s\n' "$alloc" > "$base/selftest-alloc/src/lib.rs"
alloc_dep
sed -i 's/^std = \[/std = ["selftest-alloc\/on", /' dash-chain/Cargo.toml
expect 'dash-chain \(host, --no-default-features\): installs a global allocator: selftest_alloc' "dash-chain linking a crate that sets a global allocator with a feature off" shared
# Sol r3 R3-2: the default features, which neither extreme selects.
shared_plant '' "#[cfg(all(feature = \"std\", not(feature = \"selftest-off\")))] $alloc"
sed -i -e 's/^default = \[\]/default = ["std"]/' -e 's/^std = \[/selftest-off = []\n&/' dash-chain/Cargo.toml
expect 'dash-chain \(host, default features\): installs a global allocator: dash_chain' "dash-chain setting a global allocator with its default features only" shared
# Sol r4 R4-1 (DEC-176): a product may select any feature subset, which no
# sample of builds covers; the all-features closure's sources are banned.
shared_plant '' "#[cfg(all(feature = \"allocator\", not(feature = \"std\")))] $alloc"
sed -i -e 's/^default = \[\]/default = ["std"]/' -e 's/^std = \[/allocator = []\n&/' dash-chain/Cargo.toml
expect 'passed with warning: dash-chain \(host\): global_allocator in its all-features closure: dash-chain v[^:]+: .*/src/lib.rs:[0-9]+ \(advisory' "dash-chain setting a global allocator with defaults off and one feature on: the check passes, with the scan's warning" advisory
outside selftest-alloc ''
printf '%s\n' "#[macro_export] macro_rules! install { () => { $alloc }; }" > "$base/selftest-alloc/src/lib.rs"
shared_plant "selftest-alloc = { path = \"$base/selftest-alloc\" }" '#[cfg(all(feature = "allocator", not(feature = "std")))] selftest_alloc::install!();'
sed -i -e 's/^default = \[\]/default = ["std"]/' -e 's/^std = \[/allocator = []\n&/' dash-chain/Cargo.toml
expect 'passed with warning: dash-chain \(aarch64-linux-android\): global_allocator in its all-features closure: selftest-alloc v0.0.0: ' "dash-chain setting a global allocator through another crate's macro, with defaults off and one feature on: the check passes, with the scan's warning" advisory
shared_plant '' 'include!(concat!(env!("OUT_DIR"), "/selftest.rs"));'
printf '%s\n' 'fn main() {' \
    '    let on = std::env::var_os("CARGO_FEATURE_ALLOCATOR").is_some() && std::env::var_os("CARGO_FEATURE_STD").is_none();' \
    "    let code = if on { \"$alloc\" } else { \"\" };" \
    '    std::fs::write(std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("selftest.rs"), code).unwrap();' \
    '}' > dash-chain/build.rs
sed -i -e 's/^default = \[\]/default = ["std"]/' -e 's/^std = \[/allocator = []\n&/' dash-chain/Cargo.toml
expect 'passed with warning: dash-chain \(host\): global_allocator in its all-features closure: dash-chain v[^:]+: .*/build.rs:[0-9]+ ' "dash-chain's build script generating a global allocator with defaults off and one feature on: the check passes, with the scan's warning" advisory
outside selftest-alloc ''
printf '%s\n' "$alloc" 'pub fn touch() {}' > "$base/selftest-alloc/src/lib.rs"
shared_plant '' ''
build_dep dash-chain "selftest-alloc = { path = \"$base/selftest-alloc\" }"
echo 'fn main() { selftest_alloc::touch(); }' > dash-chain/build.rs
expect pass "dash-chain's build script using a crate that sets a global allocator" shared
# Sol r6 R6-3 (DEC-188 (3)): a native package can replace or interpose malloc
# with no #[global_allocator]; every native package in the closure needs a
# reviewed [[native]] record. selftest-cmalloc compiles a malloc/free
# interposer in C (on the host) and links it, as Sol's r6-cmalloc did.
cmalloc() {  # cmalloc [links key]
    outside selftest-cmalloc ''
    [ -z "${1:-}" ] || sed -i "/^edition/a links = \"$1\"" "$base/selftest-cmalloc/Cargo.toml"
    echo 'pub fn touch() {}' > "$base/selftest-cmalloc/src/lib.rs"
    cat > "$base/selftest-cmalloc/build.rs" <<'EOF'
use std::{env, fs, process::Command};
fn main() {
    if env::var("TARGET").unwrap() != env::var("HOST").unwrap() { return; }
    let out = env::var("OUT_DIR").unwrap();
    fs::write(format!("{out}/m.c"), "#include <stddef.h>\nvoid *__libc_malloc(size_t);\nvoid __libc_free(void *);\n\
        void *malloc(size_t n) { return __libc_malloc(n); }\nvoid free(void *p) { __libc_free(p); }\n").unwrap();
    assert!(Command::new("cc").args(["-c", "-fPIC", "-o", &format!("{out}/m.o"), &format!("{out}/m.c")]).status().unwrap().success());
    assert!(Command::new("ar").args(["crs", &format!("{out}/libselftest_malloc.a"), &format!("{out}/m.o")]).status().unwrap().success());
    println!("cargo:rustc-link-search=native={out}");
    println!("cargo:rustc-link-lib=static=selftest_malloc");
}
EOF
    shared_plant "selftest-cmalloc = { path = \"$base/selftest-cmalloc\" }" 'pub fn selftest() { selftest_cmalloc::touch(); }'
}
native_missing="in its all-features closure is not reviewed in layers.toml's \[\[native\]\] \(SB0 L5; DEC-188\)"
cmalloc mimalloc
expect "dash-chain \(host\): native package selftest-cmalloc v0.0.0 \(links = 'mimalloc'; its build script links native code\) $native_missing" "dash-chain reaching an unreviewed native package that interposes malloc" shared
cmalloc mimalloc
printf '\n[[native]]\npackage = "selftest-cmalloc"\nversion = "0.0.0"\nnote = "selftest: reviewed"\n' >> contrib/node/layers.toml
expect pass "dash-chain reaching a native package with a [[native]] record" shared
cmalloc
build_dep "$base/selftest-cmalloc" 'cc = "1"'
expect "dash-chain \(aarch64-linux-android\): native package selftest-cmalloc v0.0.0 \(builds with cc\) $native_missing" "dash-chain reaching an unreviewed package that builds C with cc and has no links key" shared
cmalloc
expect "dash-chain \(host\): native package selftest-cmalloc v0.0.0 \(its build script links native code\) $native_missing" "dash-chain reaching an unreviewed package whose build script compiles and links C without cc or a links key" shared
cmalloc mimalloc
printf '\n[[native]]\npackage = "selftest-cmalloc"\nversion = "0.0.0"\nnote = " "\n' >> contrib/node/layers.toml
expect "layers.toml's \[\[native\]\] .* must give package, version and a review note" "a [[native]] record without a note" shared
shared_plant '' "pub fn selftest<'scope, 'env>(scope: &'scope std::thread::Scope<'scope, 'env>) { drop(scope.spawn(|| ())); }"
expect 'disallowed (method|type) `std::thread::Scope' "dash-chain spawning a scoped thread on a scope it is given" shared
shared_plant 'threadpool = "1"' 'pub fn selftest() { threadpool::ThreadPool::new(2).execute(|| ()); }'
expect 'disallowed (method|type) `threadpool::' "dash-chain building a threadpool pool" shared
shared_plant '' '#[cfg(target_os = "android")] pub fn selftest() { drop(std::thread::spawn(|| ())); }'
expect 'disallowed method `std::thread::spawn`' "dash-chain spawning a thread on Android only" shared
shared_plant "$pkc_bls" "pub fn selftest(pks: &[$blst_ty::PublicKey]) -> bool { dash_pkc::__deps::blst::MultiPoint::validate(pks).is_ok() }"
expect 'disallowed method `blst::MultiPoint::validate`' "dash-chain calling blst's pooled trait method" shared

echo "== product registry and allocator canary (L5; check-products.py, DEC-181)"
# The allocator proof: every registered product links a canary that sets its
# own allocator. Sol r5's escapes from the source scan (R5-1, R5-2) are planted
# in dash-chain and judged through a registered product that selects them.
products() { relock; contrib/node/check-products.sh "$@"; }
registry() { relock; python3 contrib/node/check-products.py --registry; }
row() {  # row <package> <targets> <triples> [extra toml]
    printf '\n[[product]]\npackage = "%s"\ntargets = [%s]\ntriples = [%s]\n%s\n' "$1" "$2" "$3" "${4:-}" >> contrib/node/layers.toml
}
canary_product() {  # canary_product <deps toml> [triples]: a registered cdylib of layer A using dash-chain
    add_member selftest-product "$1" 'pub use dash_chain as _selftest;'
    printf '\n[lib]\ncrate-type = ["cdylib"]\n' >> selftest-product/Cargo.toml
    set_layer selftest-product A
    row selftest-product '"selftest_product"' "${2:-\"host\"}"
}
tool_product() {  # tool_product <deps toml> <main.rs> [row toml]: a registered bin of layer T
    mkdir -p selftest-tool/src
    printf '[package]\nname = "selftest-tool"\nversion = "0.0.0"\nedition = "2024"\n\n[dependencies]\n%s\n' "$1" > selftest-tool/Cargo.toml
    printf '%s\n' "$2" > selftest-tool/src/main.rs
    sed -i 's/^members = \[/members = ["selftest-tool", /' Cargo.toml
    set_layer selftest-tool T
    row selftest-tool '"selftest-tool"' '"host"' "${3:-}"
}
chain_allocator() { sed -i -e 's/^default = \[\]/default = ["std"]/' -e 's/^std = \[/allocator = []\n&/' dash-chain/Cargo.toml; }
on_chain='dash-chain = { path = "../dash-chain", default-features = false, features = ["allocator"] }'
gated="#[cfg(all(feature = \"allocator\", not(feature = \"std\")))]"
canary_fail() { printf 'selftest-product selftest_product \\(%s, default features\\): installs a global allocator:' "$1"; }
shared_plant '' ''
chain_allocator
canary_product "$on_chain" '"host", "aarch64-linux-android"'
expect pass "a registered product selecting dash-chain's defaults off and allocator on" products selftest-product
# R5-1: an allocator in a #[path] file outside src/, which the scan does not read.
shared_plant '' '#[path = "../allocator.rs"] mod allocator;'
printf '%s %s\n' "$gated" "$alloc" > dash-chain/allocator.rs
chain_allocator
canary_product "$on_chain"
expect "$(canary_fail host) dash_chain \(dash-chain v[^ ]+ \(path\+file://[^)]*\)\) \(SB0 L5; DEC-181, DEC-188\)" "a product selecting dash-chain's allocator from a #[path] file outside src/" products selftest-product
# R5-1: an allocator attribute a proc macro assembles, with no token to find.
outside selftest-alloc-macro ''
printf '\n[lib]\nproc-macro = true\n' >> "$base/selftest-alloc-macro/Cargo.toml"
printf '%s\n' 'use proc_macro::TokenStream;' '#[proc_macro_attribute]' \
    'pub fn allocator(_: TokenStream, item: TokenStream) -> TokenStream {' \
    '    let attribute = ["global", "allocator"].join("_");' \
    '    format!("#[{attribute}] {item}").parse().unwrap()' '}' > "$base/selftest-alloc-macro/src/lib.rs"
shared_plant "selftest-alloc-macro = { path = \"$base/selftest-alloc-macro\" }" \
    "$gated #[selftest_alloc_macro::allocator] static A: std::alloc::System = std::alloc::System;"
chain_allocator
canary_product "$on_chain" '"host", "aarch64-linux-android"'
expect "$(canary_fail aarch64-linux-android) dash_chain " "a product selecting dash-chain's allocator assembled by a proc macro, on Android" products selftest-product
# R5-2: a product enabling a dependency's feature that pulls in an allocator
# crate the root's all-features closure does not hold.
outside selftest-hidden ''
printf '%s\n' "$alloc" > "$base/selftest-hidden/src/lib.rs"
outside selftest-facade "selftest-hidden = { path = \"$base/selftest-hidden\", optional = true }"
printf '\n[features]\nallocator = ["dep:selftest-hidden"]\n' >> "$base/selftest-facade/Cargo.toml"
printf '#[cfg(feature = "allocator")] extern crate selftest_hidden;\n' > "$base/selftest-facade/src/lib.rs"
shared_plant "selftest-facade = { path = \"$base/selftest-facade\" }" 'pub use selftest_facade as _facade;'
canary_product "$(printf 'dash-chain = { path = "../dash-chain" }\nselftest-facade = { path = "%s/selftest-facade", features = ["allocator"] }' "$base")"
expect "$(canary_fail host) selftest_hidden " "a product enabling a dependency's feature that adds an allocator crate" products selftest-product
# A bin is compiled again as an rlib for its canary, so its own code counts.
tool_product '' "$alloc fn main() {}"
expect 'selftest-tool selftest-tool \(host, default features\): installs a global allocator: selftest_tool ' "a registered bin setting a global allocator" products selftest-tool
tool_product '' "$alloc fn main() {}" 'allocator = "own"'
expect pass "a registered bin declaring the global allocator it sets" products selftest-tool
tool_product '' 'fn main() {}' 'allocator = "own"'
expect 'selftest-tool .*: declares its own allocator, but the canary links' "a registered bin declaring an allocator it does not set" products selftest-tool
outside selftest-alloc ''
printf '%s\n' "$alloc" 'pub fn touch() {}' > "$base/selftest-alloc/src/lib.rs"
tool_product "selftest-alloc = { path = \"$base/selftest-alloc\" }" 'fn main() { selftest_alloc::touch(); }' 'allocator = "own"'
expect "selftest-tool .*: a global allocator in selftest_alloc \(selftest-alloc v0.0.0 \(path\+file://[^)]*\)\), not the product's own package selftest-tool v0.0.0 " "a registered bin declaring its own allocator, linking another crate's" products selftest-tool
add_member selftest-unregistered '' 'pub fn selftest() {}'
printf '\n[lib]\ncrate-type = ["cdylib"]\n' >> selftest-unregistered/Cargo.toml
expect "selftest-unregistered's cdylib target selftest_unregistered is not in layers.toml's \[\[product\]\] registry or its exclusions \(SB0 L5; DEC-181, DEC-188\)" "an unregistered cdylib" registry
row dash-spv '"dash-spv-gone"' '"host"'
expect "\[\[product\]\] 'dash-spv' names dash-spv-gone, which is no cdylib, staticlib or bin target" "a registry row naming no target" registry
# A bin named like its package's lib is still judged as the bin.
same_name() {  # same_name <lib toml> <row toml>: selftestbin's lib and bin share the name; the bin sets an allocator
    mkdir -p selftestbin/src
    printf '[package]\nname = "selftestbin"\nversion = "0.0.0"\nedition = "2024"\n%s\n' "$1" > selftestbin/Cargo.toml
    echo 'pub fn touch() {}' > selftestbin/src/lib.rs
    printf '%s fn main() { selftestbin::touch(); }\n' "$alloc" > selftestbin/src/main.rs
    sed -i 's/^members = \[/members = ["selftestbin", /' Cargo.toml
    set_layer selftestbin T
    row selftestbin '"selftestbin"' '"host"' "$2"
}
same_name '' ''
expect 'selftestbin selftestbin \(host, default features\): installs a global allocator: selftestbin ' "a registered bin named like its package's lib, setting a global allocator" products selftestbin
same_name "$(printf '\n[lib]\ncrate-type = ["cdylib", "rlib"]')" ''
expect "'selftestbin' names selftestbin, which is a target of more than one kind \(lib, bin\): give kind" "a row naming a cdylib and a bin of one name without kind" registry
same_name '' 'allocator = "own"'
expect pass "a declared allocator a bin named like its package's lib sets" products selftestbin
same_name '' 'allocator = "own"'
printf 'fn main() { selftestbin::touch(); }\n' > selftestbin/src/main.rs
printf '%s\npub fn touch() {}\n' "$alloc" > selftestbin/src/lib.rs
expect pass "a declared allocator a bin's own package's lib sets" products selftestbin
outside selftest-alloc ''
printf '%s\npub fn touch() {}\n' "$alloc" > "$base/selftest-alloc/src/lib.rs"
same_name "$(printf '\n[dependencies]\nselftest-alloc = { path = "%s/selftest-alloc" }' "$base")" 'allocator = "own"'
printf 'fn main() { selftestbin::touch(); }\n' > selftestbin/src/main.rs
printf 'pub fn touch() { selftest_alloc::touch(); }\n' > selftestbin/src/lib.rs
expect "selftestbin .*: a global allocator in selftest_alloc \(selftest-alloc v0.0.0 \(path\+file://[^)]*\)\), not the product's own package selftestbin v0.0.0 " "a declared allocator set by a dependency only the bin's own lib names" products selftestbin
# Sol r6 R6-1 (DEC-188 (1)): ownership is the package, not the crate name. A
# dependency whose lib takes the product's crate name sets the allocator.
mkdir -p selftest-tool/src "$base/selftest-namesake/src"
printf '[package]\nname = "selftest-namesake"\nversion = "0.0.0"\nedition = "2024"\n\n[lib]\nname = "selftest_tool"\n' > "$base/selftest-namesake/Cargo.toml"
printf '%s\npub fn touch() {}\n' "$alloc" > "$base/selftest-namesake/src/lib.rs"
tool_product "alias-dep = { package = \"selftest-namesake\", path = \"$base/selftest-namesake\" }" 'fn main() { alias_dep::touch(); }' 'allocator = "own"'
expect "selftest-tool .*: a global allocator in selftest_tool \(selftest-namesake v0.0.0 \(path\+file://[^)]*\)\), not the product's own package selftest-tool v0.0.0 " "a declared allocator set by a dependency that takes the product's crate name" products selftest-tool
# Sol r7 R7-1 (DEC-188 (1)): a build script's crate search path lets a bare
# `extern crate` load an allocator no --extern names, so an own row refuses
# one; a native search path stays allowed.
# rustflags <toml array items>: the host's target.<triple>.rustflags, which
# cargo prefers to build.rustflags and merges with any the Cargo home sets.
rustflags() {
    mkdir -p "$base/.cargo"
    printf '[target.%s]\nrustflags = [%s]\n' "$(rustc -vV | sed -n 's/^host: //p')" "$1" > "$base/.cargo/config.toml"
}
hidden_tool() {  # hidden_tool <search path> <main.rs>: selftest-tool's build script builds hidden_alloc into OUT_DIR ({out})
    tool_product '' "$2" 'allocator = "own"'
    printf '%s\npub fn touch() {}\n' "$alloc" > selftest-tool/hidden.rs
    printf '%s\n' 'fn main() {' \
        '    let out = std::env::var("OUT_DIR").unwrap();' \
        '    let rustc = std::env::var("RUSTC").unwrap();' \
        '    assert!(std::process::Command::new(rustc).args(["--edition=2024", "--crate-name", "hidden_alloc", "--crate-type", "rlib", "hidden.rs", "--out-dir", &out]).status().unwrap().success());' \
        "    println!(\"cargo:rustc-link-search=$1\");" \
        '    println!("cargo:rerun-if-changed=hidden.rs");' \
        '}' > selftest-tool/build.rs
}
hidden_tool 'all={out}' 'extern crate hidden_alloc;
fn main() { hidden_alloc::touch(); }'
expect "selftest-tool .*[:;] selftest-tool v0.0.0 \(path\+file://[^)]*\)'s build script gives rustc \`-L all=[^\`]*/out\`: a crate search path other than a dependency path to cargo's deps/, through which a bare \`extern crate\` can load an allocator no --extern names" "a declared allocator a bare extern crate loads through a build script's search path" products selftest-tool
hidden_tool 'crate={out}/../../../deps' "$alloc
fn main() {}"
expect "selftest-tool .*'s build script gives rustc \`-L crate=[^\`]*/out/\.\./\.\./\.\./deps\`: a crate search path" "a declared allocator of a bin whose build script adds a crate search path into deps/" products selftest-tool
hidden_tool 'native={out}' "$alloc
fn main() {}"
mkdir -p "$base/selftest-search"
rustflags "\"-L$base/selftest-search\""
expect "selftest-tool .*: the product unit gives rustc \`-L$base/selftest-search\`: a crate search path" "a declared allocator of a bin given a bare -L in rustflags" products selftest-tool
hidden_tool 'native={out}' "$alloc
fn main() {}"
expect pass "a declared allocator of a bin whose build script adds a native search path" products selftest-tool
# Sol r8 R8-1 (DEC-188 am. 1): the rustc argv is allow-listed. rustflags give
# the product, by a joined --extern, an rlib built outside cargo into its own
# deps/, so both canaries would reach the ownership decision; every spelling
# is refused before them, as is any option off the list.
hidden_deps() {  # hidden_deps: an own bin loading hidden_alloc, built outside cargo into the release deps/
    tool_product '' 'extern crate hidden_alloc;
fn main() { hidden_alloc::touch(); }' 'allocator = "own"'
    mkdir -p target/release/deps
    printf '%s\npub fn touch() {}\n' "$alloc" > "$base/hidden.rs"
    rustc --edition=2024 --crate-name hidden_alloc --crate-type rlib "$base/hidden.rs" --out-dir target/release/deps
}
hidden_deps
rustflags "\"--extern=hidden_alloc=$PWD/target/release/deps/libhidden_alloc.rlib\""
expect "selftest-tool .*: the product unit gives rustc \`--extern=hidden_alloc=[^\`]*/libhidden_alloc.rlib\`: a file that is no Cargo artifact of a package in the unit graph" "a declared allocator a joined --extern in rustflags loads from deps/" products selftest-tool
hidden_deps
rustflags "\"--extern\", \"hidden_alloc=$PWD/target/release/deps/libhidden_alloc.rlib\""
expect "selftest-tool .*: the product unit gives rustc \`--extern hidden_alloc=[^\`]*/libhidden_alloc.rlib\`: a file that is no Cargo artifact of a package in the unit graph" "a declared allocator a separate --extern in rustflags loads from deps/" products selftest-tool
rm -f target/release/deps/libhidden_alloc.rlib
tool_product '' 'fn main() {}'
rustflags '"--remap-path-prefix=/selftest=/x"'
expect "selftest-tool .*: the product unit gives rustc \`--remap-path-prefix=/selftest=/x\`: --remap-path-prefix is not an allow-listed option" "a bin given an option off the allow-list in rustflags" products selftest-tool
tool_product '' 'fn main() {}'
rustflags "\"--sysroot\", \"$(rustc --print sysroot)\""
expect "selftest-tool .*: the product unit gives rustc \`--sysroot [^\`]*\`: --sysroot is not an allow-listed option" "a bin given --sysroot in rustflags" products selftest-tool
tool_product '' "$alloc
fn main() {}" 'allocator = "own"'
rustflags "\"--cfg=selftest_a\", \"--cfg\", \"selftest_b\", \"-Wunused\", \"--warn=unused\", \"-A\", \"dead_code\", \"-Cstrip=none\", \"--codegen\", \"strip=none\", \"-Lnative=$base\", \"-L\", \"native=$base\""
expect pass "a declared allocator of a bin given allow-listed options in every spelling in rustflags" products selftest-tool
expect pass "the tokenizer and allow-list tests: every spelling of every allow-listed option" python3 -I -B contrib/node/test-check-products.py
# Sol r6 R6-2 (DEC-188 (2)): an example's library artifact needs a row, and
# the row's canary judges it.
example_lib() {  # example_lib [row toml]: selftest-example ships a staticlib example that sets an allocator
    add_member selftest-example '' 'pub fn touch() {}'
    mkdir -p selftest-example/examples
    printf '%s\n#[unsafe(no_mangle)] pub extern "C" fn selftest_shipped() { selftest_example::touch(); }\n' "$alloc" > selftest-example/examples/shipped.rs
    printf '\n[[example]]\nname = "shipped"\ncrate-type = ["staticlib"]\n' >> selftest-example/Cargo.toml
    set_layer selftest-example T
    [ -z "${1:-}" ] || row selftest-example '"shipped"' '"host"' "$1"
}
example_lib
expect "selftest-example's staticlib target shipped \(example\) is not in layers.toml's \[\[product\]\] registry or its exclusions" "an unregistered staticlib example" registry
example_lib 'kind = "example"'
expect 'selftest-example shipped \(host, default features\): installs a global allocator: shipped ' "a registered staticlib example setting a global allocator" products selftest-example
row dash-spv '"dash-spv"' '"host"' 'features = "ab"'
expect "\[\[product\]\] 'dash-spv': targets and triples are lists of names" "a registry row giving features as a string" registry
expect "selftest-none matches no \[\[product\]\] row" "a canary run naming a package without a row" products selftest-none
sed -i 's/^exception = \[\]$/allow = {}/' contrib/node/layers.toml
expect "layers.toml's \[allocator\] takes only \[\[allocator.exception\]\] records" "the old allocator allow table" python3 contrib/node/link-probe.py --check-allow
# R5-3: an exception names its package and version as one exact token of a
# decision on node/integration, in a fixture repository whose node/integration
# holds the row.
allow_fixture() {  # allow_fixture <package> <version> <dec>
    local f="$base/allow-fixture"
    rm -rf "$f" && mkdir -p "$f/contrib/node" "$f/docs/node-program" && cd "$f" &&
        cp "$wt/contrib/node/check-layers.py" "$wt/contrib/node/link-probe.py" "$wt/contrib/node/layers.toml" contrib/node/ &&
        git -C "$wt" show node/integration:docs/node-program/DECISIONS.md > docs/node-program/DECISIONS.md &&
        decision DEC-99997 'allocator exceptions `blst@0.3.17` and `sb1-r5-allocator-extra@99.0.01`' &&
        git init -q -b node/integration && git add contrib docs && git commit -qm base &&
        sed -i 's/^exception = \[\]$//' contrib/node/layers.toml &&
        printf '\n[[allocator.exception]]\npackage = "%s"\nversion = "%s"\ndec = "%s"\n' "$1" "$2" "$3" >> contrib/node/layers.toml &&
        python3 contrib/node/link-probe.py --check-allow
}
not_held='holds the token package@version \(DEC-181\)'
expect "$not_held" "an exception for bls@0.3.1 citing DEC-79, which names blst 0.3.17" allow_fixture bls 0.3.1 DEC-79
expect "$not_held" "an exception for a name that prefixes the decision's (blst@0.3.17 for bls)" allow_fixture bls 0.3.17 DEC-99997
expect "$not_held" "an exception for a version that prefixes the decision's (blst@0.3.1)" allow_fixture blst 0.3.1 DEC-99997
expect "$not_held" "an exception for sb1-r5-allocator@99.0.0 against sb1-r5-allocator-extra@99.0.01" allow_fixture sb1-r5-allocator 99.0.0 DEC-99997
expect pass "an exception whose decision holds its exact token" allow_fixture blst 0.3.17 DEC-99997

echo "== CI groups match the layers"
add_member dash-selftest-pure '' ''
set_layer dash-selftest-pure P
sed -i 's/^  - dash-oracle-sys .*/&\n  - dash-selftest-pure/' .github/ci-groups.yml
expect 'dash-selftest-pure is a P crate but not in the node-pure group' "a P crate tested outside node-pure" groups

echo "== floating point in dash-consensus (CS2 §2.2, §2.6)"
# The escapes reviews CS2c-r1 demonstrated, planted in the crate. A float remainder is libm's
# fmod: clippy.toml bans the trait methods, tests/fp_confinement.rs the operator and the method
# names in fp and `%` in any macro_rules!, and check-no-libm.sh any libm call in the compiled
# crate. fp_confinement also keeps an ordering of floats in fp and floats out of macro arguments.
fp=dash-consensus/src/fp/mod.rs
pow=dash-consensus/src/pow/mod.rs
nolibm() { contrib/node/check-no-libm.sh; }
lint() { cargo clippy --locked -p dash-consensus --lib -- -D warnings; }
confine() { cargo test --locked -p dash-consensus --test fp_confinement; }
# what Difficulty::from_bits returns
returns() { sed -i "s/^        Self(difficulty)\$/        Self($1)/" "$fp"; }
glob_ops() { sed -i 's/^use kgw_pow::KGW_POW;$/&\nuse core::ops::*;/' "$fp"; }
remainder_macro() {
    printf '%s\n' 'macro_rules! remainder { ($a:expr, $b:expr) => { $a % $b }; }' \
        'pub(crate) use remainder;' >> "$pow"
    returns 'crate::pow::remainder!(difficulty, 1.0e300)'
}
expect pass "clean tree" nolibm
expect pass "clean tree" confine
glob_ops; returns 'difficulty.rem(1.0e300)'
expect 'calls libm: fmod' "a float rem() in fp through a glob import" nolibm
glob_ops; returns 'difficulty.rem(1.0e300)'
expect 'disallowed method `core::ops::Rem::rem`' "the same, to clippy" lint
glob_ops; returns 'difficulty.rem(1.0e300)'
expect 'fp/mod.rs:[0-9]+: Self\(difficulty\.rem' "the same, to fp_confinement" confine
glob_ops; returns '{ let mut d = difficulty; d.rem_assign(1.0e300); d }'
expect 'calls libm: fmod' "a float rem_assign() in fp through a glob import" nolibm
glob_ops; returns '{ let mut d = difficulty; d.rem_assign(1.0e300); d }'
expect 'disallowed method `core::ops::RemAssign::rem_assign`' "the same, to clippy" lint
remainder_macro
expect 'calls libm: fmod' "a float % in fp expanded from a macro_rules! outside fp" nolibm
remainder_macro
expect 'pow/mod.rs:[0-9]+: macro_rules! remainder' "the same, to fp_confinement" confine
printf '%s\n' 'use core::cmp::PartialOrd as Compare;' 'impl Compare for Difficulty {' \
    '    fn partial_cmp(&self, o: &Self) -> Option<core::cmp::Ordering> { self.0.partial_cmp(&o.0) }' \
    '}' >> "$fp"
expect 'fp/mod.rs:[0-9]+: use core::cmp::PartialOrd as Compare' "an ordering of Difficulty through a renamed PartialOrd" confine
sed -i 's/^#\[derive(Clone, Copy, Debug, PartialEq)\]$/&\n#[cfg_attr(target_arch = "x86_64", derive(PartialOrd))]/' "$fp"
expect 'fp/mod.rs:[0-9]+: #\[cfg_attr' "an ordering of Difficulty derived through cfg_attr" confine
printf '%s\n' 'macro_rules! less { ($ignore:tt $a:literal, $other:tt $b:literal) => { $a < $b }; }' \
    'pub fn outside() -> bool { less!(. 0.5, . 1.5) }' >> "$pow"
expect 'pow/mod.rs:[0-9]+: pub fn outside' "floats compared outside fp in a macro's arguments" confine

# BS7's differential commands exit non-zero on a mismatch, a harness error or
# an empty run: bs7_exit.py runs each with a stubbed runner.
bs7_exits() { python3 contrib/node/dde-probe/bs7_exit.py; }
expect pass "the BS7 differential commands' exit status" bs7_exits
sed -i 's/^    bs7_exit.check(bs7_exit.failures(summary))$/    pass/' contrib/node/dde-probe/bs7_diff_framing.py
expect 'FAIL framing mismatch: exit 0' "bs7_diff_framing.py exiting 0 after a mismatch" bs7_exits
echo "== the prebuilt RocksDB (BL-133)"
expect pass "the prebuilt RocksDB helper's tests: profiles, key inputs, a tree without it" python3 -I -B contrib/node/test-selftest-rocksdb.py
echo "== quirk register (O5)"
reg=docs/node-program/quirks.toml
scaf=dash-consensus/tests/quirks_scaffold.rs
# check-quirks.py builds every workspace test binary only to list its tests, so
# it builds them without debuginfo: with it, that build alone takes target/
# from about 2 GB to 41 GB, a shard's peak (BL-369; without, 5.7 GB).
quirks() { CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 python3 -I -B contrib/node/check-quirks.py; }
# set_field <id> <key> <value>: rewrites one field of one register entry
set_field() { sed -i "/^id = $1\$/,/^\[\[quirk\]\]/ s|^$2 = .*|$2 = $3|" "$reg"; }
expect pass "clean tree" quirks
sed -i '/^#\[ignore = "quirk 39: owner CS1"\]$/d' "$scaf"
expect 'quirk 39: status scaffold needs #\[ignore = "quirk 39: owner CS1"\], .* has no #\[ignore\]' "a scaffold un-ignored without updating the register" quirks
sed -i 's/^#\[ignore = "quirk 39: owner CS1"\]$/#[ignore = "quirk 39: owner CS4"]/' "$scaf"
expect "quirk 39: status scaffold needs .* has 'quirk 39: owner CS4'" "an ignore reason naming another task" quirks
sed -i 's/^#\[ignore = "quirk 39: owner CS1"\]$/#[ignore]/' "$scaf"
expect 'quirk 39: status scaffold needs .* has an ignore whose reason cannot be read exactly' "an ignore without a reason" quirks
printf '#[test]\n#[ignore = "quirk 39: owner CS1"]\nfn quirk_39_again() {}\n' >> "$scaf"
expect 'quirk 39: needs exactly one test, found 2' "a second test for one quirk" quirks
printf 'fn quirk_39_helper() {}\n' >> "$scaf"
expect 'quirk 39: needs exactly one test, found 2' "a helper taking a register test name" quirks
sed -i '/^\/\/\/ Quirk 6:/,/^}$/d' "$scaf"
expect 'quirk 6: needs exactly one test, found 0' "a registered quirk without a test" quirks
sed -i '/^fn quirk_04_checkdatasig() {$/i #[ignore = "quirk 4: owner CS1"]' dash-consensus/tests/quirks.rs
expect 'quirk 4: status tested, but .* has .quirk 4: owner CS1.' "a tested quirk ignored" quirks
q4() { sed -i "/^fn quirk_04_checkdatasig() {\$/i $1" dash-consensus/tests/quirks.rs; }
q4 '#[ignore = r"quirk 4: later"]'
expect 'quirk 4: status tested, but .* has an ignore whose reason cannot be read exactly' "a tested quirk ignored with a raw-string reason" quirks
q4 '#[ignore = "a \\"b\\" c"]'
expect 'quirk 4: status tested, but .* has an ignore whose reason cannot be read exactly' "a tested quirk ignored with an escaped quote in the reason" quirks
q4 '#[cfg_attr(not(feature = "x"), ignore)]'
expect 'quirk 4: status tested, but .* has an ignore whose reason cannot be read exactly' "a tested quirk ignored through cfg_attr" quirks
sed -i '/^#\[test\]$/{N;s/^#\[test\]\nfn quirk_04_checkdatasig/\/\/ #[test]\nfn quirk_04_checkdatasig/}' dash-consensus/tests/quirks.rs
expect 'quirk 4: .*quirk_04_checkdatasig is not a #\[test\]' "a tested quirk whose #[test] is commented out" quirks
q4 '#[cfg(any())]'
expect 'quirk 4: .* is conditionally compiled' "a tested quirk compiled out by #[cfg]" quirks
python3 - <<'EOF'
p = "dash-consensus/tests/quirks.rs"; t = open(p).read()
t = t.replace("#[test]\nfn quirk_04_checkdatasig", "/*\n#[test]\nfn quirk_04_checkdatasig", 1)
end = t.rindex("#[test]", 0, t.index("fn quirk_05"))
open(p, "w").write(t[:end] + "*/\n" + t[end:])
EOF
expect 'quirk 4: needs exactly one test, found 0' "a tested quirk inside a block comment" quirks
sed -i '1i #![cfg(any())]' dash-consensus/tests/quirks.rs
expect 'quirk 4: needs exactly one compiled test, found 0' "a test file compiled out by #![cfg]" quirks
python3 - "$scaf" <<'EOF'
import sys
p = sys.argv[1]; t = open(p).read()
start = t.index("/// Quirk 6:"); end = t.index("}\n", t.index("fn quirk_06")) + 2
open(p, "w").write(t[:start] + "#[cfg(any())]\nmod hidden {\n" + t[start:end] + "}\n" + t[end:])
EOF
expect 'quirk 6: needs exactly one compiled test, found 0' "a scaffold inside a module compiled out by #[cfg]" quirks
# macro_test <file> <name>: a test target whose test the source scan cannot see
macro_test() { printf 'macro_rules! t {\n    ($n:ident) => {\n        #[test]\n        fn $n() {}\n    };\n}\nt!(%s);\n' "$2" > "$1"; }
sed -i '1i #![cfg(any())]' dash-consensus/tests/quirks.rs
macro_test dash-consensus/tests/substitute.rs quirk_04_checkdatasig
expect 'quirk 4: the compiled test is quirk_04_checkdatasig in the test target dash-consensus/tests/substitute.rs, which does not compile it from dash-consensus/tests/quirks.rs' "a same-named test in another target standing in for a compiled-out one" quirks
mkdir -p dash-consensus/tests/support && mv "$scaf" dash-consensus/tests/support/
printf '#[path = "support/quirks_scaffold.rs"]\nmod scaffolds;\n' > dash-consensus/tests/scaffolds.rs
sed -i "s|$scaf::|dash-consensus/tests/support/quirks_scaffold.rs::|" "$reg"
expect pass "scaffolds included through a #[path] module" quirks
macro_test dash-sha256/tests/hidden.rs quirk_99_unregistered
expect 'dash-sha256/tests/hidden.rs: compiled test quirk_99_unregistered is not in the register' "a macro-generated quirk test in a package without quirk tests" quirks
set_field 33 status '"dropped (DEC-82)"'
sed -i '/^id = 33$/,/^\[\[quirk\]\]/{/^test = /d}' "$reg"
sed -i '/^\/\/\/ Quirk 33:/,/^}$/d' "$scaf"
macro_test dash-consensus/tests/dropped.rs quirk_33_hash_serialized_2_varint
expect 'quirk 33 is dropped, but the test target dash-consensus/tests/dropped.rs compiles quirk_33_hash_serialized_2_varint' "a dropped quirk with a macro-generated test" quirks
# The register's size and its last entry's test, so that a new entry does not break these cases.
n=$(grep -c '^\[\[quirk\]\]' "$reg")
last=$(python3 - "$reg" <<'EOF'
import re, sys
p = sys.argv[1]; t = open(p).read(); i = t.rindex("[[quirk]]")
open(p, "w").write(t[:i])
print(re.search(r'^test = ".*::(\w+)"$', t[i:], re.M).group(1))
EOF
)
expect "$last is not in the register" "a quirk test without a register entry" quirks
set_field 5 id 6
expect "ids must run 1\\.\\.$n in order" "a repeated id" quirks
set_field 13 owner '"EV9"'
expect "quirk 13: task 'EV9' is not a task row" "an owner that is not a PLAN task" quirks
set_field 13 also '["EV9"]'
expect "quirk 13: task 'EV9' is not a task row" "an also task that is not a PLAN task" quirks
set_field 13 also '"CH7a"'
expect 'quirk 13: `also` must be a list' "an also given as a string" quirks
set_field 13 owner '"ID"'
expect "quirk 13: task 'ID' is not a task row" "the PLAN table header as an owner" quirks
set_field 35 test '"base-sdk:pkgs/pkc/tests/bls_wrapper.rs::quirk_35_no_such_test"'
expect 'quirk 35: the register names base-sdk:.*quirk_35_no_such_test, the tree has base-sdk:' "a base SDK test the pinned fork does not have" quirks
set_field 35 test '"base-sdk:pkgs/pkc/tests/no_such_file.rs::quirk_35_eager_bls_decode_outcomes"'
expect 'quirk 35: cannot read pkgs/pkc/tests/no_such_file.rs from the base SDK fork at' "a base SDK file the pinned fork does not have" quirks
set_field 35 status '"scaffold"'
expect 'quirk 35: status scaffold needs .* has no #\[ignore\]' "a base SDK test that runs, registered as a scaffold" quirks
set_field 33 test '"dash-consensus/tests/quirks.rs::quirk_33_hash_serialized_2_varint"'
expect 'quirk 33: the register names dash-consensus/tests/quirks.rs::.*, the tree has dash-consensus/tests/quirks_scaffold.rs::' "a register entry naming the wrong file" quirks
set_field 33 test '"dash-consensus/tests/quirks_scaffold.rs::quirk_32_bip9_warning_floor_and_threshold"'
expect 'quirk 33: test .* is not named quirk_33_<slug>' "a register entry naming another quirk's test" quirks
set_field 33 status '"dropped (DEC-82)"'
expect 'quirk 33: a dropped quirk has no test' "a dropped quirk that still names a test" quirks
set_field 33 status '"done"'
expect "quirk 33: status 'done' is not one of" "an unknown status" quirks
set_field 1 title '"something else"'
expect 'quirk 1: its title and core must be exactly' "quirk 1's entry saying more than PLAN §7 allows" quirks
# No later case builds there, so the shard gives that build's space back now (BL-369).
rm -rf target/debug

# DEC-82 (v24 only): the guard that keeps per-release code out. It reads tracked files, so each
# plant is added with intent-to-add and removed again.
release_refs() {  # release_refs <rust source>: the guard with a tracked file holding it
    local f=dash-consensus/src/planted_release.rs rc=0
    printf '%s\n' "$1" > "$f"
    git add -N -- "$f"
    contrib/node/pin-delta/check-release-refs.sh || rc=$?
    git rm -q -f --cached -- "$f"
    rm -f "$f"
    return $rc
}
expect pass "release guard: clean tree" contrib/node/pin-delta/check-release-refs.sh
expect pass "release guard: its own selftest" contrib/node/pin-delta/check-release-refs.sh --selftest
expect pass "pin-delta inventory: a backport merge splits into its sub-PRs" python3 -I -B contrib/node/pin-delta/test-inventory.py
expect pass "pin-delta citations: changed, shifted and same Core cites between two pins" python3 -I -B contrib/node/pin-delta/test-check-citations.py
expect pass "setup-dashd: the release archive is verified against SHA256SUMS before extraction (BL-402)" python3 -I -B contrib/test-setup-dashd.py
expect pass "release guard: Release::V24_0 handed to the chain-parameter builder" \
    release_refs 'fn p() { ChainParamsBuilder::new(network, Release::V24_0); }'
expect 'planted_release.rs:[0-9]+:.*V23_1' "release guard: Release::V23_1" \
    release_refs 'fn p() { ChainParamsBuilder::new(network, Release::V23_1); }'
expect 'planted_release.rs:[0-9]+:.*UngatedRules' "release guard: an UngatedRules stub" \
    release_refs 'pub struct UngatedRules;'
expect 'planted_release.rs: a branch on the release' "release guard: matches! on Release::V24_0" \
    release_refs 'fn g(r: Release) -> bool { matches!(r, Release::V24_0) }'
expect 'planted_release.rs: a branch on the release' "release guard: Release::V24_0 on the left of ==" \
    release_refs 'fn g(r: Release) -> bool { Release::V24_0 == r }'
expect 'planted_release.rs: a branch on the release' "release guard: a multi-line matches!" \
    release_refs $'fn g(r: Release) -> bool {\n    matches!(\n        r,\n        Release::V24_0\n    )\n}'

[ -z "$list" ] || exit 0
record end "$failures"
if [ "$failures" != 0 ]; then
    echo "$failures selftest(s) failed${shard:+ (shard $shard)}" >&2
    exit 1
fi
echo "all selftests passed${shard:+ (shard $shard)}"
