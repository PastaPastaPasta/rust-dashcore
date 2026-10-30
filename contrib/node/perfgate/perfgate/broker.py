"""The PF0-N spawn broker (BL-128; plan rev4.1 §4, §5, D24-D27, D37, D39, D46, D60, D64, D68, D74, D75, D80).

    python3 -I -B broker.py --config CONFIG --report FD

It runs in an invocation's outer pid namespace, next to the node-bench view, as a child of outer_init. It is
the only process that holds the PR binary and the jail spec: node-bench's stub (`node-launch --client`) asks
it for a spawn over the SOCK_SEQPACKET socket in CTL, and the broker builds the whole chain itself:

    node-launch --into (placement helper) -> [perf stat] -> node-launch --jail -> bwrap (the jail) -> rdashd

A request names only the daemon's arguments, a class and whether it is measured; it cannot name an
executable, a bind or a bwrap option. Everything else comes from CONFIG, which perfgate wrote.

Nothing a client can rename is used by path after it is checked (D80): datadirs are made and copied relative
to the mount roots' descriptors and reach bwrap as a descriptor (`--bind-fd`); perf writes into a memfd only
the broker reads. One poll loop serves every socket, pipe and pidfd, so a flooded pipe cannot stall the others.
Results go to the invocation's res/ directory, each file created once (O_EXCL, O_NOFOLLOW), compact and capped
(a result over its cap is replaced by a small error document); perfgate reads them after the invocation.
Self-contained (stdlib only): it runs under `python3 -I`, which does not import from its own directory.
"""

import ctypes
import errno
import fcntl
import hashlib
import json
import os
import re
import select
import selectors
import shutil
import signal
import socket
import stat
import struct
import subprocess
import sys
import time

# Before anything protected exists in this process (D24): no same-uid process may read its memory or
# descriptors, or ptrace it.
_libc = ctypes.CDLL(None, use_errno=True)
PR_SET_DUMPABLE, PR_GET_DUMPABLE = 4, 3
if _libc.prctl(PR_SET_DUMPABLE, 0, 0, 0, 0) != 0 or _libc.prctl(PR_GET_DUMPABLE, 0, 0, 0, 0) != 0:
    sys.exit("broker: PR_SET_DUMPABLE failed")

CLASSES = ("version", "preflight", "start", "prep", "run")
MEASURES = ("none", "counter")  # node rows have no strace pass (D39)
REQUEST_KEYS = {"argv", "cwd", "measure", "class"}
REQUEST_MAX = 64 * 1024
ARGS_MAX, ARG_BYTES, ARGV_BYTES = 256, 4096, 32 * 1024
MAX_SPAWNS, MAX_CLIENTS = 64, 64
HELLO_S = 60  # D51: launch hello and the bwrap info record
INFO_S = 60
LINE_MAX, LINES_MAX, ALLOC_MAX = 512, 256, 64 * 1024
ALLOC_KEYS = ("pid", "allocs", "alloc_bytes", "read_bytes", "write_bytes", "read_syscalls", "write_syscalls",
              "peak_rss_kib")
PERF_CAP, RESULT_CAP, ALLOC_FILE_CAP = 4096, 16 * 1024, 64 * 1024
TREE_DEPTH = 64  # datadir nesting the broker walks or copies (deeper → refused)
FINAL_READS = 64  # reads of a pipe at cleanup: a writer that outlived the chain can't hold the broker
ERRORS_KEPT = 20  # per list in a result; the rest are counted
MFD_CLOEXEC, MFD_ALLOW_SEALING = 1, 2
F_ADD_SEALS = 1033
SEALS = 0x0001 | 0x0002 | 0x0004 | 0x0008  # SEAL, SHRINK, GROW, WRITE
DIR_FLAGS = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
JAIL_DATADIR_FD = 8  # where node-launch --jail puts the datadir descriptor for bwrap's --bind-fd


def clip(v, n=100):
    """A client-supplied value in a refusal or error text, bounded (a string as is, anything else as its repr)."""
    r = v if isinstance(v, str) else repr(v)
    return r if len(r) <= n else r[:n] + "..."


def _object(pairs):
    if len({k for k, _ in pairs}) != len(pairs):
        raise Refused("duplicate keys")
    return dict(pairs)


class Refused(Exception):
    pass


def pidfd_open(pid):
    fd = _libc.syscall(434, pid, 0)  # SYS_pidfd_open
    if fd < 0:
        raise OSError(ctypes.get_errno(), "pidfd_open")
    return fd


def pidfd_exited(pidfd):
    """True once the process behind `pidfd` has exited (a pidfd polls readable then)."""
    p = select.poll()
    p.register(pidfd, select.POLLIN)
    return bool(p.poll(0))


def pidfd_signal(pidfd, sig):
    if _libc.syscall(424, pidfd, sig, None, 0) != 0:  # SYS_pidfd_send_signal
        err = ctypes.get_errno()
        if err != errno.ESRCH:
            raise OSError(err, "pidfd_send_signal")


def starttime(pid):
    with open(f"/proc/{pid}/stat") as f:
        return int(f.read().rsplit(")", 1)[1].split()[19])


def size_text(n):
    return f"{n >> 30}G" if n and n % (1 << 30) == 0 else f"{n >> 20}M"


def count_text(n):
    return f"{n // 1000}k" if n and n % 1000 == 0 else str(n)


def read_text(path, default=""):
    try:
        with open(path) as f:
            return f.read()
    except OSError:
        return default


def dumps(obj):
    return json.dumps(obj, separators=(",", ":"))


class Bounded(list):
    """A list of messages that keeps the first ERRORS_KEPT and counts the rest (the jail can cause many)."""

    def __init__(self):
        super().__init__()
        self.dropped = 0

    def append(self, item):
        if len(self) < ERRORS_KEPT:
            super().append(item)
        else:
            self.dropped += 1

    def doc(self):
        return list(self) + ([f"... and {self.dropped} more"] if self.dropped else [])


def vmstat_oom_kills():
    for line in read_text("/proc/vmstat").splitlines():
        if line.startswith("oom_kill "):
            return int(line.split()[1])
    return None


def memory_events(cg):
    out = {}
    for line in read_text(os.path.join(cg, "memory.events")).splitlines():
        k, _, v = line.partition(" ")
        if v.isdigit():
            out[k] = int(v)
    return out


def own_scope():
    """The invocation's delegated scope: cgsetup put this process in `<scope>/harness`, and the spawn cgroups
    are its siblings."""
    line = read_text("/proc/self/cgroup").strip()
    harness = "/sys/fs/cgroup" + line[3:] if line.startswith("0::/") else ""
    scope = os.path.dirname(harness)
    if os.path.basename(harness) != "harness" or not scope.endswith(".scope"):
        raise SystemExit(f"broker: not in a delegated scope's harness cgroup: {line!r}")
    return scope


def node_memfree_kib(node):
    for line in read_text(f"/sys/devices/system/node/node{node}/meminfo").splitlines():
        if "MemFree:" in line:
            return int(line.split()[-2])
    return None


def classify_death(status, events, oom_before, oom_after, mem_spawn, node, memfree):
    """D74: what killed a chain that died by SIGKILL (directly, or as bwrap's or perf's 128 + 9), when the broker
    did not. memory.events of the spawn's own cgroup first; then the host's vmstat oom_kill delta, which is how a
    kernel OOM of the MPOL_BIND node shows (the kernel log is not readable, dmesg_restrict = 1; the counter is
    host-wide, so another process's OOM in the window reads the same, which errs towards ERROR). None for any
    other status: perfgate classifies those."""
    if events.get("oom_kill", 0) > 0:
        return f"spawn hit memory.max {mem_spawn >> 20} MiB (oom_kill {events['oom_kill']})"
    if status not in ({"signal": signal.SIGKILL}, {"exit": 128 + signal.SIGKILL}):
        return None
    if oom_before is not None and oom_after is not None and oom_after > oom_before:
        return (f"killed by a kernel OOM outside the memcg (node {node} MPOL_BIND; node{node} MemFree "
                f"{memfree} kB)")
    return "death by signal 9"


def jail_args(cfg, datadir, argv):
    """The jail's bwrap options (the sealed spec) and its command: hardened like the node-bench view (D47), the
    network shared with the invocation, one writable bind (the datadir, by descriptor: D80), the exe at
    /jail/rdashd under `setarch -R` (bwrap's exec resets the personality, D37/D68), the alloc counter on fd 5 and
    the info record on fd 6. Called with placeholder paths to hash the template into node_inputs (§8)."""
    a = ["--cap-drop", "ALL", "--unshare-user", "--disable-userns", "--unshare-pid", "--unshare-ipc",
         "--unshare-uts", "--unshare-cgroup", "--as-pid-1", "--new-session", "--info-fd", "6", "--clearenv"]
    for k, v in sorted(cfg["jail_env"].items()):
        a += ["--setenv", k, v]
    a += ["--setenv", "LD_PRELOAD", "/jail/alloc.so", "--setenv", "PERFGATE_ALLOC_FD", "5",
          "--setenv", "HOME", datadir, "--setenv", "TMPDIR", datadir]
    a += ["--ro-bind", "/usr", "/usr"]
    for link, target in (("/bin", "usr/bin"), ("/sbin", "usr/sbin"), ("/lib", "usr/lib"), ("/lib64", "usr/lib64")):
        a += ["--symlink", target, link]
    for f in cfg["jail_etc"]:
        a += ["--ro-bind", f, f]
    a += ["--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp",
          "--bind-fd", str(JAIL_DATADIR_FD), datadir,
          "--ro-bind", cfg["exe"], "/jail/rdashd", "--ro-bind", cfg["preload"], "/jail/alloc.so",
          "--chdir", datadir]
    return a, [cfg["setarch"], cfg["arch"], "-R", "/jail/rdashd", *argv]


def chain_argv(cfg, socks, info_w, alloc_w, perf_fd):
    """The helper's command line: node-launch --into pins the clone3 child and execs [perf stat ... --]
    node-launch --jail. perf writes its CSV to `perf_fd`, a memfd only the broker reads (never a path, D80)."""
    jail = [cfg["node_launch"], "--jail", str(socks[1]), "--info", str(info_w), "--alloc", str(alloc_w),
            "--bwrap", cfg["bwrap"]]
    if perf_fd is not None:
        jail = [cfg["perf"], "stat", "-x,", "--no-big-num", "-e", ",".join(cfg["perf_events"]), "--log-fd",
                str(perf_fd), "--", *jail]
    into = [cfg["node_launch"], "--into", str(socks[0]), "--cpus", cfg["cpu_list"], "--node", str(cfg["numa_node"])]
    return into + ["--", *jail]


def sealed_memfd(args):
    """NUL-separated `args` in a memfd sealed against any change, for `node-launch --jail`."""
    fd = _libc.memfd_create(b"pf0-jail-spec", MFD_CLOEXEC | MFD_ALLOW_SEALING)
    if fd < 0:
        raise OSError(ctypes.get_errno(), "memfd_create")
    os.write(fd, b"".join(a.encode() + b"\0" for a in args))
    os.lseek(fd, 0, os.SEEK_SET)
    fcntl.fcntl(fd, F_ADD_SEALS, SEALS)
    return fd


def _walk(dirfd):
    """(relpath, stat, open subdir fd or None) for every entry under `dirfd`, depth-first in name order, without
    recursion; subdir fds are closed by the caller's iteration. Deeper than TREE_DEPTH → Refused."""
    stack = [(dirfd, "", iter(sorted(os.listdir(dirfd))), False)]
    try:
        while stack:
            fd, prefix, names, own = stack[-1]
            name = next(names, None)
            if name is None:
                stack.pop()
                if own:
                    os.close(fd)
                continue
            st = os.stat(name, dir_fd=fd, follow_symlinks=False)
            rel = prefix + name
            if stat.S_ISDIR(st.st_mode):
                if len(stack) > TREE_DEPTH:
                    raise Refused(f"a datadir nested deeper than {TREE_DEPTH} levels")
                sub = os.open(name, DIR_FLAGS, dir_fd=fd)
                stack.append((sub, rel + "/", iter(sorted(os.listdir(sub))), True))
                yield rel, st, fd, name, True
            else:
                yield rel, st, fd, name, False
    finally:
        for fd, _, _, own in stack:
            if own:
                os.close(fd)


def checked_tree(dirfd):
    """(relpath, kind, size) of every entry under the directory `dirfd`; only directories and regular files may
    be there (D60: a symlink, FIFO, socket or device in a datadir is an error, never followed or copied)."""
    out = []
    for rel, st, _, _, is_dir in _walk(dirfd):
        if is_dir:
            out.append((rel, "d", 0))
        elif stat.S_ISREG(st.st_mode):
            out.append((rel, "f", st.st_size))
        else:
            raise Refused(f"{rel[:200]} in a datadir is not a regular file or directory")
    return out


def copy_tree(src, dst):
    """Copy a datadir tmpfs-to-tmpfs between directory descriptors: plain bytes (a sparse file costs its full
    size, D60), modes 0700/0600, nothing followed."""
    dsts = {"": dst}
    try:
        for rel, st, fd, name, is_dir in _walk(src):
            parent = dsts[rel[:len(rel) - len(name)]]
            if is_dir:
                os.mkdir(name, 0o700, dir_fd=parent)
                dsts[rel + "/"] = os.open(name, DIR_FLAGS, dir_fd=parent)
            elif stat.S_ISREG(st.st_mode):
                fi = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=fd)
                fo = os.open(name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600,
                             dir_fd=parent)
                try:
                    while chunk := os.read(fi, 1 << 20):
                        os.write(fo, chunk)
                finally:
                    os.close(fi)
                    os.close(fo)
            else:
                raise Refused(f"{rel[:200]} in the prep datadir is not a regular file or directory")
    finally:
        for rel, fd in dsts.items():
            if rel:
                os.close(fd)


def remove_at(dirfd, name):
    """Remove `name` in `dirfd`, whatever it is, without following it."""
    try:
        st = os.stat(name, dir_fd=dirfd, follow_symlinks=False)
    except FileNotFoundError:
        return
    if stat.S_ISDIR(st.st_mode):
        shutil.rmtree(name, dir_fd=dirfd)  # the fd-based rmtree: symlinks inside are unlinked, never followed
    else:
        os.unlink(name, dir_fd=dirfd)


def fresh_dir_at(dirfd, name):
    remove_at(dirfd, name)
    os.mkdir(name, 0o700, dir_fd=dirfd)
    return os.open(name, DIR_FLAGS, dir_fd=dirfd)


def stdio_problem(i, fd, devs):
    """Why a client's stdin (i = 0), stdout or stderr can't go to the jail, or None. It must be a pipe, a character
    device or a regular file on one of the bounded tmpfs roots (`devs`: RUN, PREP), open for reading (0) or writing
    (1, 2); anything else (a directory, a socket to the broker, a host file with no bound, §6) would reach the
    node."""
    st = os.fstat(fd)
    if not (stat.S_ISFIFO(st.st_mode) or stat.S_ISCHR(st.st_mode) or stat.S_ISREG(st.st_mode)):
        return f"descriptor {i} is not a pipe, character device or regular file (mode {st.st_mode:o})"
    if stat.S_ISREG(st.st_mode) and st.st_dev not in devs:
        return f"descriptor {i} is a regular file outside RUN and PREP"
    mode = fcntl.fcntl(fd, fcntl.F_GETFL) & os.O_ACCMODE
    if mode not in (os.O_RDWR, os.O_RDONLY if i == 0 else os.O_WRONLY):
        return f"descriptor {i} is not open for {'reading' if i == 0 else 'writing'}"
    return None


def close_received(fds):
    """Close descriptors a client sent; a socket first loses any SO_LINGER, so close() can't block the loop."""
    for fd in fds:
        try:
            if stat.S_ISSOCK(os.fstat(fd).st_mode):
                s = socket.socket(fileno=fd)
                try:
                    s.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 0, 0))
                except OSError:
                    pass
                s.detach()
        except OSError:
            pass
        os.close(fd)


class NoHooks:
    """The production broker: no test hooks (D9). Only the tests' entrypoint (tests/broker_hooked.py) passes
    others; a config with test settings is refused."""

    def chain_argv(self, k, argv):
        return argv

    def hold_cleanup(self, sp):
        return False

    def rmdir(self, path):
        os.rmdir(path)

    def started(self):
        pass


class Spawn:
    def __init__(self, k, client, cls, measured, args, datadir):
        self.k, self.client, self.cls, self.measured = k, client, cls, measured
        self.args, self.datadir = args, datadir
        self.t0 = time.monotonic()
        self.cg = self.cgfd = None
        self.datadir_fd = self.perf_fd = None
        self.socks = {}  # personality -> broker end
        self.hellos = set()
        self.helper = self.helper_pidfd = None
        self.child = None
        self.node_pidfd = None
        self.info = self.alloc = None
        self.info_buf, self.alloc_buf = b"", b""
        self.alloc_records, self.alloc_errors, self.alloc_bytes = {}, Bounded(), 0
        self.info_record = None
        self.status = None  # {"exit": n} or {"signal": s}, from the helper
        self.rusage = None
        self.errors = Bounded()
        self.stop_pending = self.stop_sent = False
        self.killed_at = self.kill_reason = None
        self.helper_killed = False
        self.memory_peak, self.memory_events = None, {}
        self.oom0 = vmstat_oom_kills()
        self.finished = self.finishing = self.abandoned = False


class Broker:
    def __init__(self, cfg, report_fd, hooks=None):
        self.cfg = cfg
        self.report = os.fdopen(report_fd, "w", buffering=1) if report_fd is not None else None
        self.sel = selectors.DefaultSelector()
        self.spawns = {}
        self.count = {c: 0 for c in CLASSES}
        self.measured = 0
        self.next_k = 0
        self.errors = Bounded()
        self.clients = {}
        self.exe_sha = cfg["exe_sha256"]
        self.res = cfg["res"]
        self.scope = own_scope()
        # The mount roots datadirs live in, held open: everything below is reached relative to them (D80).
        self.roots = {virt: os.open(real, DIR_FLAGS) for virt, real in cfg["paths"].items()}
        self.root_devs = {os.fstat(fd).st_dev for fd in self.roots.values()}
        self.prep_source = os.open(cfg["prep_source"], DIR_FLAGS) if cfg.get("prep_source") else None
        self.listen = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET | socket.SOCK_CLOEXEC)
        path = cfg["socket"]
        if os.path.lexists(path):
            os.unlink(path)
        self.listen.bind(path)
        os.chmod(path, 0o600)
        self.listen.listen(8)
        self.listen.setblocking(False)
        self.sel.register(self.listen, selectors.EVENT_READ, ("listen", None))
        self.wake_r, self.wake_w = os.pipe2(os.O_NONBLOCK | os.O_CLOEXEC)
        signal.set_wakeup_fd(self.wake_w)
        signal.signal(signal.SIGCHLD, lambda *_: None)
        self.sel.register(self.wake_r, selectors.EVENT_READ, ("wake", None))
        self.terminate = self.fatal = False
        signal.signal(signal.SIGTERM, self._term)
        self.hooks = hooks or NoHooks()
        self.hooks.started()
        self.tell(event="listening")

    def _term(self, *_):
        self.terminate = True

    # --- reporting ---------------------------------------------------------------------------------------

    def tell(self, **msg):
        """A line to outer_init (census registrations and triggers, D40/D64)."""
        if self.report:
            try:
                self.report.write(dumps(msg) + "\n")
            except OSError:
                pass

    def write_res(self, name, obj, cap):
        """One compact JSON file (or raw bytes). Over `cap`, a document's error lists are dropped first (its
        scalars, such as `fatal` and the counts, stay); still over, it is replaced by a small error document."""
        data = obj if isinstance(obj, bytes) else dumps(obj).encode()
        if len(data) > cap and isinstance(obj, dict):
            slim = {k: (["... dropped: over the cap"] if k.endswith("errors") and v else v) for k, v in obj.items()}
            data = dumps(slim).encode()
        if len(data) > cap:
            data = dumps({"error": f"{name} over its {cap} B cap ({len(data)} B)"}).encode()
        fd = os.open(os.path.join(self.res, name), os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, "wb") as f:
            f.write(data)

    # --- requests ----------------------------------------------------------------------------------------

    def accept(self):
        try:
            conn, _ = self.listen.accept()
        except OSError:
            return
        if len(self.clients) >= MAX_CLIENTS:
            self.errors.append(f"broker refused: more than {MAX_CLIENTS} open clients")
            conn.close()
            return
        conn.setblocking(False)
        self.clients[conn.fileno()] = {"sock": conn, "spawn": None}
        self.sel.register(conn, selectors.EVENT_READ, ("client", conn.fileno()))

    def client_msg(self, fd):
        c = self.clients[fd]
        sock = c["sock"]
        try:
            data, anc, flags, _ = sock.recvmsg(REQUEST_MAX + 1, socket.CMSG_SPACE(4 * 8))
        except BlockingIOError:
            return
        except OSError:
            data, anc, flags = b"", [], 0
        fds = []
        for level, typ, payload in anc:
            if level == socket.SOL_SOCKET and typ == socket.SCM_RIGHTS:
                fds += list(memoryview(payload).cast("i"))
        if not data:  # EOF: a client gone (SIGKILLed stub, node-bench) kills its spawn
            close_received(fds)
            self.drop_client(fd, "client closed the connection")
            return
        if c["spawn"] is None:
            try:
                self.request(fd, data, flags, fds)
            except Exception as e:  # noqa: BLE001 (a bad request never takes the broker down)
                text = f"broker refused: {e}" if isinstance(e, Refused) else f"broker error: {type(e).__name__}: {e}"
                self.errors.append(text[:300])
                self.send(sock, {"error": text[:1000]})
                self.drop_client(fd, None if c["spawn"] is None else text[:300])
            finally:
                close_received(fds)
            return
        close_received(fds)
        sp = self.spawns[c["spawn"]]
        try:
            msg = json.loads(data)
        except ValueError:
            msg = None
        if msg == {"stop": True}:
            sp.stop_pending = True
            self.maybe_stop(sp)
        else:
            self.kill(sp, "client sent an unexpected message")

    def request(self, fd, data, flags, fds):
        if flags & (socket.MSG_TRUNC | socket.MSG_CTRUNC) or len(data) > REQUEST_MAX:
            raise Refused("request too large")
        if len(fds) != 3:
            raise Refused("a request carries exactly the stub's stdin, stdout and stderr")
        for i, f in enumerate(fds):
            if problem := stdio_problem(i, f, self.root_devs):
                raise Refused(problem)
        try:
            req = json.loads(data.decode(), object_pairs_hook=_object)  # strict UTF-8, not json's surrogatepass
        except (ValueError, RecursionError):
            raise Refused("not JSON") from None
        if not isinstance(req, dict):
            raise Refused("not a JSON object")
        if set(req) != REQUEST_KEYS:
            raise Refused(f"keys must be {sorted(REQUEST_KEYS)}")
        cls, measure, argv, cwd = req["class"], req["measure"], req["argv"], req["cwd"]
        if cls not in CLASSES:
            raise Refused(f"class {clip(cls)}")
        if measure not in MEASURES:
            raise Refused(f"measure {clip(measure)}" + (" (node rows have no strace pass)" if measure == "syscalls"
                                                        else ""))
        if not isinstance(argv, list) or not all(isinstance(a, str) and "\0" not in a for a in argv):
            raise Refused("argv must be a list of strings")
        try:
            sizes = [len(a.encode()) for a in argv]
            if isinstance(cwd, str):
                cwd.encode()
        except UnicodeEncodeError:
            raise Refused("argv and cwd must be valid UTF-8 (no lone surrogates)") from None
        if len(argv) > ARGS_MAX or max(sizes, default=0) > ARG_BYTES or sum(sizes) > ARGV_BYTES:
            raise Refused(f"argv over {ARGS_MAX} arguments, {ARG_BYTES} B each or {ARGV_BYTES} B in all")
        if not isinstance(cwd, str) or not (cwd == "/pf0/run" or cwd.startswith("/pf0/run/")):
            raise Refused("cwd outside /pf0/run")
        if (measure == "counter") != (cls == "run"):
            raise Refused("exactly the run class is measured")
        datadirs = [a.split("=", 1)[1] for a in argv if a.startswith(("-datadir=", "--datadir="))]
        if len(datadirs) != 1:
            raise Refused("exactly one -datadir")
        datadir = datadirs[0]
        row = self.cfg.get("prep_row")
        allowed = {"version": "/pf0/run/version", "prep": f"/pf0/prep/{row}/datadir"}
        if datadir != allowed.get(cls, "/pf0/run/node") or (cls == "prep" and not row):
            raise Refused(f"-datadir {clip(datadir)} for class {cls}")
        if any(not sp.finished for sp in self.spawns.values()):
            raise Refused("another spawn is live")
        if measure == "counter" and self.measured:
            raise Refused("one measured spawn per invocation")
        if self.next_k >= MAX_SPAWNS or self.count[cls] >= self.cfg["budget"].get(cls, 0):
            raise Refused(f"spawn budget: no more {cls} spawns in this invocation")
        self.count[cls] += 1
        self.measured += measure == "counter"
        k = self.next_k
        self.next_k += 1
        sp = Spawn(k, fd, cls, measure == "counter", argv, datadir)
        self.spawns[k] = sp
        self.clients[fd]["spawn"] = k
        try:
            self.prepare_datadir(sp)
            self.start(sp, fds)
        except Exception as e:  # noqa: BLE001
            self.kill(sp, f"spawn setup: {e}"[:300])
        self.send(self.clients[fd]["sock"], {"spawn": k})

    def root_of(self, path):
        """(descriptor of the mount root, entry name) of a fixed /pf0 datadir path."""
        parent, name = os.path.split(path)
        if parent not in self.roots:
            raise Refused(f"{path} is not under a /pf0 mount")
        return self.roots[parent], name

    def prepare_datadir(self, sp):
        """D46: every node datadir is made here, fresh, and held by descriptor from then on (D80); node-bench
        never chooses its state."""
        root, name = self.root_of(sp.datadir)
        if sp.cls == "prep":
            try:
                sp.datadir_fd = os.open(name, DIR_FLAGS, dir_fd=root)
            except OSError as e:
                raise Refused(f"the prep datadir must be a directory node-bench copied the master into ({e})") from None
            checked_tree(sp.datadir_fd)
            return
        sp.datadir_fd = fresh_dir_at(root, name)
        if sp.cls != "version" and self.prep_source is not None:
            copy_tree(self.prep_source, sp.datadir_fd)
            if checked_tree(sp.datadir_fd) != checked_tree(self.prep_source):
                raise Refused("the run datadir does not match PREP after the copy")
        elif os.listdir(sp.datadir_fd):
            raise Refused("the datadir is not empty")

    # --- the chain ---------------------------------------------------------------------------------------

    def start(self, sp, fds):
        cfg, lim = self.cfg, self.cfg["limits"]
        if sha256(cfg["exe"]) != self.exe_sha:
            raise Refused("the exe copy changed since the gate copied it")
        sp.cg = os.path.join(self.scope, f"spawn-{sp.k}")
        os.mkdir(sp.cg)
        for name, value in (("memory.max", lim["mem_spawn"]), ("memory.swap.max", 0), ("pids.max", lim["tasks_spawn"])):
            with open(os.path.join(sp.cg, name), "w") as f:
                f.write(str(value))
        self.tell(spawn=sp.k, cgroup=sp.cg, event="start")
        sp.cgfd = os.open(sp.cg, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
        mine, theirs = [], []  # the broker's ends (closed on failure) and the helper's (always closed here)
        try:
            into_b, into_c = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET | socket.SOCK_CLOEXEC)
            mine.append(into_b)
            theirs.append(into_c)
            jail_b, jail_c = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET | socket.SOCK_CLOEXEC)
            mine.append(jail_b)
            theirs.append(jail_c)
            info_r, info_w = os.pipe2(os.O_CLOEXEC | os.O_NONBLOCK)
            mine.append(info_r)
            theirs.append(info_w)
            alloc_r, alloc_w = os.pipe2(os.O_CLOEXEC | os.O_NONBLOCK)
            mine.append(alloc_r)
            theirs.append(alloc_w)
            os.set_blocking(info_w, True)
            os.set_blocking(alloc_w, True)
            if sp.measured:
                sp.perf_fd = _libc.memfd_create(b"pf0-perf", MFD_CLOEXEC)
                if sp.perf_fd < 0:
                    raise OSError(ctypes.get_errno(), "memfd_create")
            passed = (into_c.fileno(), jail_c.fileno(), info_w, alloc_w) + ((sp.perf_fd,) if sp.measured else ())
            argv = self.hooks.chain_argv(
                sp.k, chain_argv(cfg, (into_c.fileno(), jail_c.fileno()), info_w, alloc_w, sp.perf_fd))
            sp.helper = subprocess.Popen(argv, stdin=fds[0], stdout=fds[1], stderr=fds[2], close_fds=True,
                                         pass_fds=passed, env={})
            sp.t0 = time.monotonic()  # the hello and class deadlines run from here, not from the datadir setup
        except BaseException:
            for x in mine:
                x.close() if isinstance(x, socket.socket) else os.close(x)
            raise
        finally:
            for x in theirs:
                x.close() if isinstance(x, socket.socket) else os.close(x)
        self.tell(helper=sp.helper.pid, starttime=starttime(sp.helper.pid), spawn=sp.k)
        sp.info, sp.alloc = info_r, alloc_r
        self.sel.register(info_r, selectors.EVENT_READ, ("info", sp.k))
        self.sel.register(alloc_r, selectors.EVENT_READ, ("alloc", sp.k))
        try:
            sp.helper_pidfd = pidfd_open(sp.helper.pid)
        except OSError:
            sp.helper.kill()  # never registered for its exit: the kill path finishes the spawn without it
            for s in (into_b, jail_b):
                s.close()
            raise
        for name, s in (("into", into_b), ("jail", jail_b)):
            s.setblocking(False)
            sp.socks[name] = s
            self.sel.register(s, selectors.EVENT_READ, ("launch", (sp.k, name)))
        self.sel.register(sp.helper_pidfd, selectors.EVENT_READ, ("helper", sp.k))

    def close_sock(self, sp, name):
        s = sp.socks.pop(name, None)
        if s is not None:
            self.sel.unregister(s)
            s.close()

    def launch_msg(self, k, name):
        sp = self.spawns[k]
        s = sp.socks.get(name)
        if s is None:
            return
        try:
            data = s.recv(4096)
        except BlockingIOError:
            return
        except OSError:
            data = b""
        if not data:
            self.close_sock(sp, name)
            if name not in sp.hellos and not sp.finished and sp.status is None:
                self.kill(sp, f"launch handshake: {name} closed before its hello")
            return
        try:
            msg = json.loads(data)
        except ValueError:
            return self.kill(sp, f"launch handshake: {name} sent no JSON")
        if not isinstance(msg, dict):
            return self.kill(sp, f"launch handshake: {name} sent {data[:100]!r}")
        if "hello" in msg:
            if msg != {"hello": name, "dumpable": 0} or name in sp.hellos:
                return self.kill(sp, f"launch handshake: {name} said {msg}")
            if sp.killed_at is not None:  # nothing protected goes to a spawn being killed
                return self.close_sock(sp, name)
            sp.hellos.add(name)
            try:
                if name == "into":
                    socket.send_fds(s, [b'{"cgroup":1}'], [sp.cgfd])
                    os.close(sp.cgfd)
                    sp.cgfd = None
                else:
                    opts, cmd = jail_args(self.cfg, sp.datadir, sp.args)
                    fds = [sealed_memfd(opts), sealed_memfd(cmd)]
                    try:
                        socket.send_fds(s, [b'{"spec":3}'], [*fds, sp.datadir_fd])
                    finally:
                        for f in fds:
                            os.close(f)
            except OSError as e:
                self.kill(sp, f"launch handshake: {name}: {e}")
            return
        if name == "into" and "child" in msg and name in sp.hellos:
            sp.child = msg["child"]
            return
        if name == "into" and ("exit" in msg or "signal" in msg) and sp.status is None:
            sp.status = {x: v for x, v in msg.items() if x in ("exit", "signal")}
            sp.rusage = msg.get("rusage")
            return
        if name == "into" and "error" in msg:
            sp.errors.append(str(msg["error"]))
            return
        self.kill(sp, f"launch handshake: unexpected {msg}")

    def info_ready(self, k, final=False):
        sp = self.spawns[k]
        try:
            chunk = os.read(sp.info, 4096)
        except BlockingIOError:
            chunk = b"" if final else None  # final: a writer that outlived the chain is not waited for
        if chunk is None:
            return
        if not chunk:
            self.sel.unregister(sp.info)
            os.close(sp.info)
            sp.info = None
            return
        if len(sp.info_buf) < 4096:
            sp.info_buf += chunk
        if sp.info_record is None:
            try:
                rec = json.loads(sp.info_buf)
            except ValueError:
                return  # bwrap writes the object in pieces
            pid = rec.get("child-pid") if isinstance(rec, dict) else None
            if not isinstance(pid, int) or isinstance(pid, bool) or pid <= 1:
                return self.kill(sp, f"bwrap info record {sp.info_buf[:200]!r}")
            sp.info_record = {"child-pid": pid}
            if sp.finishing:
                return
            try:
                sp.node_pidfd = pidfd_open(pid)
                cg = read_text(f"/proc/{pid}/cgroup").strip()
            except ProcessLookupError:
                return  # bwrap already reaped the jail (a quick exit read late, D103): its status is the chain's
            except OSError as e:
                return self.kill(sp, f"pidfd of the jail: {e}")
            if not cg.endswith(f"/spawn-{k}"):
                if pidfd_exited(sp.node_pidfd):  # the jail, or whatever reused its pid after it, is gone (D103)
                    os.close(sp.node_pidfd)
                    sp.node_pidfd = None
                    return
                # the pid was reused before the pidfd was opened
                return self.kill(sp, f"the jail's pid {pid} is not in spawn-{k} ({cg[:200]})")
            self.tell(event="ready", spawn=k)
            self.maybe_stop(sp)

    def alloc_ready(self, k, final=False):
        """Alloc records: ≤ 512 B/line, ≤ 256 lines, ≤ 64 KiB, drained past overflow; exact keys, ints, one per
        pid, one from the jail's pid 1 (self-reportable, DEC-76)."""
        sp = self.spawns[k]
        try:
            chunk = os.read(sp.alloc, 65536)
        except BlockingIOError:
            chunk = b"" if final else None
        if chunk is None:
            return
        if not chunk:
            self.sel.unregister(sp.alloc)
            os.close(sp.alloc)
            sp.alloc = None
            return
        sp.alloc_bytes += len(chunk)
        if sp.alloc_bytes > ALLOC_MAX:
            if "alloc records over 64 KiB" not in sp.alloc_errors:
                sp.alloc_errors.append("alloc records over 64 KiB")
            return
        sp.alloc_buf += chunk
        *lines, sp.alloc_buf = sp.alloc_buf.split(b"\n")
        for line in lines:
            self.alloc_line(sp, line)
        if len(sp.alloc_buf) > LINE_MAX:
            sp.alloc_errors.append("alloc line over 512 B")
            sp.alloc_buf = b""

    def alloc_line(self, sp, line):
        if len(line) > LINE_MAX or len(sp.alloc_records) >= LINES_MAX:
            sp.alloc_errors.append("alloc line over 512 B or over 256 lines")
            return
        try:
            rec = json.loads(line)
        except ValueError:
            sp.alloc_errors.append("alloc record not JSON")
            return
        if not isinstance(rec, dict) or list(rec) != list(ALLOC_KEYS) or any(
                type(v) is not int or not 0 <= v < 2 ** 63 for v in rec.values()):
            sp.alloc_errors.append(f"alloc record with other keys or values: {line[:120]!r}")
            return
        if rec["pid"] in sp.alloc_records:
            sp.alloc_errors.append(f"two alloc records for pid {rec['pid']}")
            return
        sp.alloc_records[rec["pid"]] = rec

    def helper_exit(self, k):
        sp = self.spawns[k]
        self.sel.unregister(sp.helper_pidfd)
        sp.helper.wait()
        # The helper's last message may still be queued: read it before judging.
        while "into" in sp.socks:
            try:
                data = sp.socks["into"].recv(4096)
            except OSError:
                break
            if not data:
                break
            try:
                msg = json.loads(data)
            except ValueError:
                break
            if not isinstance(msg, dict):
                break
            if ("exit" in msg or "signal" in msg) and sp.status is None:
                sp.status = {x: v for x, v in msg.items() if x in ("exit", "signal")}
                sp.rusage = msg.get("rusage")
            elif "error" in msg:
                sp.errors.append(str(msg["error"]))
        if sp.helper.returncode != 0 and not sp.helper_killed:
            sp.errors.append(f"placement helper exited {sp.helper.returncode}")
        if sp.status is None and sp.killed_at is None:
            sp.errors.append("the placement helper ended without the chain's status")
        self.kill(sp, sp.kill_reason, final=True)

    def maybe_stop(self, sp):
        """ND1: SIGTERM by pidfd only once the node is up (its ready file, or a class without one); a pending
        stop waits for that or the deadline. Never SIGINT."""
        if not sp.stop_pending or sp.stop_sent or sp.node_pidfd is None or sp.killed_at is not None:
            return
        if sp.cls in ("start", "run", "prep"):
            try:
                os.stat("ready", dir_fd=sp.datadir_fd, follow_symlinks=False)
            except OSError:
                return
        pidfd_signal(sp.node_pidfd, signal.SIGTERM)
        sp.stop_sent = True

    def kill(self, sp, reason, final=False):
        """cgroup.kill (idempotent, even for a chain that looks done: a grandchild may live on), then the
        cleanup deadline runs until rmdir (D10). A helper with no child yet is outside spawn-k: SIGKILL it."""
        if reason and not sp.kill_reason:
            sp.kill_reason = reason
            sp.errors.append(reason)
        if sp.cg and os.path.isdir(sp.cg):
            try:
                with open(os.path.join(sp.cg, "cgroup.kill"), "w") as f:
                    f.write("1")
            except OSError as e:
                sp.errors.append(f"cgroup.kill: {e}")
        if sp.child is None and sp.helper is not None and sp.helper.poll() is None and not sp.helper_killed:
            if sp.helper_pidfd is not None:
                pidfd_signal(sp.helper_pidfd, signal.SIGKILL)
            else:
                sp.helper.kill()
            sp.helper_killed = True
        if sp.killed_at is None:
            sp.killed_at = time.monotonic()
        if final or sp.helper is None or sp.helper_pidfd is None:
            sp.finishing = True

    def cleanup_step(self, sp):
        """Advance a finishing spawn: empty cgroup → stats → rmdir → drain pipes → result → reply. Past
        deadline_cleanup the invocation is torn down (fatal) and nothing more is waited for."""
        if not sp.finishing or sp.finished:
            return
        late = time.monotonic() - sp.killed_at > self.cfg["limits"]["deadline_cleanup"]
        if sp.cg and os.path.isdir(sp.cg):
            procs = read_text(os.path.join(sp.cg, "cgroup.procs")).split()
            if procs or self.hooks.hold_cleanup(sp):
                try:
                    with open(os.path.join(sp.cg, "cgroup.kill"), "w") as f:
                        f.write("1")
                except OSError:
                    pass
                if not late:
                    return
                sp.errors.append(f"spawn-{sp.k} cleanup deadline ({len(procs)} procs left)")
                sp.abandoned = self.fatal = True
            sp.memory_peak = int(read_text(os.path.join(sp.cg, "memory.peak"), "0").strip() or 0)
            sp.memory_events = memory_events(sp.cg)
            if not sp.abandoned:
                try:
                    self.hooks.rmdir(sp.cg)
                except OSError as e:
                    if e.errno == errno.EBUSY and not late:
                        return
                    sp.errors.append(f"spawn-{sp.k} cleanup deadline (rmdir: {e.strerror})")
                    sp.abandoned = self.fatal = True
            sp.cg = None
        for name in ("info", "alloc"):
            # Drain what the chain wrote, a bounded number of reads: a writer that outlived it (abandoned, or
            # holding a copy of the fd outside spawn-k) is not waited for.
            for _ in range(0 if sp.abandoned else FINAL_READS):
                if getattr(sp, name) is None:
                    break
                (self.info_ready if name == "info" else self.alloc_ready)(sp.k, final=True)
            if getattr(sp, name) is not None:
                self.sel.unregister(getattr(sp, name))
                os.close(getattr(sp, name))
                setattr(sp, name, None)
        for name in list(sp.socks):
            self.close_sock(sp, name)
        for name in ("node_pidfd", "helper_pidfd", "cgfd", "datadir_fd"):
            if getattr(sp, name) is not None:
                os.close(getattr(sp, name))
                setattr(sp, name, None)
        sp.finished = True
        self.finish(sp)

    def finish(self, sp):
        oom1 = vmstat_oom_kills()
        death = None if sp.kill_reason else classify_death(
            sp.status, sp.memory_events, sp.oom0, oom1, self.cfg["limits"]["mem_spawn"], self.cfg["numa_node"],
            node_memfree_kib(self.cfg["numa_node"]))
        if sp.status and not sp.kill_reason and 1 not in sp.alloc_records:
            sp.alloc_errors.append("no alloc record from the jail's pid 1")
        for virt, fd in self.roots.items():  # §6: a hit limit is an ERROR naming it, with statvfs as the evidence
            st = os.fstatvfs(fd)
            if st.f_bavail == 0 or st.f_favail == 0:
                sp.errors.append(f"{'RUN' if virt == '/pf0/run' else 'prep'} tmpfs full "
                                 f"({size_text(st.f_blocks * st.f_frsize)} / {count_text(st.f_files)} inodes)")
        if sp.status is not None and not sp.kill_reason and ("jail" not in sp.hellos or sp.info_record is None):
            # The chain ended without the jail starting (a status from perf alone is never a node's run, D81).
            sp.errors.append(f"the chain ended {sp.status} before the jail ran (hello: {'jail' in sp.hellos}, "
                             f"bwrap info: {sp.info_record is not None})")
        perf = None
        if sp.perf_fd is not None:
            perf = os.pread(sp.perf_fd, PERF_CAP + 1, 0)
            os.close(sp.perf_fd)
            sp.perf_fd = None
            if len(perf) > PERF_CAP:
                sp.errors.append(f"perf output over {PERF_CAP} B")
                perf = None
        result = {"spawn": sp.k, "class": sp.cls, "measured": sp.measured, "datadir": sp.datadir,
                  "status": sp.status, "rusage": sp.rusage, "errors": sp.errors.doc(), "kill_reason": sp.kill_reason,
                  "memory_peak": sp.memory_peak, "memory_events": sp.memory_events, "vmstat_oom_kill": [sp.oom0, oom1],
                  "death": death, "info": sp.info_record, "stop_sent": sp.stop_sent, "answered": sorted(sp.hellos),
                  "wall_s": round(time.monotonic() - sp.t0, 3), "exe_sha256": self.exe_sha,
                  "alloc_errors": sp.alloc_errors.doc(), "cpus": self.cfg["cpu_list"],
                  "numa_node": self.cfg["numa_node"]}
        self.write_res(f"spawn-{sp.k}.result.json", result, RESULT_CAP)
        self.write_res(f"spawn-{sp.k}.alloc.json", list(sp.alloc_records.values()), ALLOC_FILE_CAP)
        if perf is not None:
            self.write_res(f"spawn-{sp.k}.perf", perf, PERF_CAP)
        if sp.cls == "version":
            try:
                remove_at(*self.root_of(sp.datadir))
            except OSError as e:
                sp.errors.append(f"remove {sp.datadir}: {e}")
        self.tell(event="stop", spawn=sp.k)
        c = self.clients.get(sp.client)
        if c:
            if sp.errors:
                self.send(c["sock"], {"error": "; ".join(sp.errors.doc())[:2000]})
            elif sp.status:
                self.send(c["sock"], sp.status)
            else:
                self.send(c["sock"], {"error": "no status"})
            self.drop_client(sp.client, None)

    def send(self, sock, msg):
        try:
            sock.send(dumps(msg).encode())
        except OSError:
            pass

    def drop_client(self, fd, reason):
        c = self.clients.pop(fd, None)
        if not c:
            return
        try:
            self.sel.unregister(c["sock"])
        except (KeyError, ValueError):
            pass
        c["sock"].close()
        if reason and c["spawn"] is not None:
            sp = self.spawns[c["spawn"]]
            if not sp.finished:
                sp.client = None
                self.kill(sp, reason)

    # --- the loop -------------------------------------------------------------------------------------------

    def deadlines(self):
        now = time.monotonic()
        lim = self.cfg["limits"]
        for sp in self.spawns.values():
            if sp.finished:
                continue
            if sp.killed_at is not None:
                # D78(a): the helper must be gone within deadline_cleanup of the kill, or the invocation goes.
                if (now - sp.killed_at > lim["deadline_cleanup"] and sp.helper_pidfd is not None
                        and sp.helper.poll() is None and not sp.finishing):
                    sp.errors.append(f"spawn-{sp.k} cleanup deadline (the placement helper still runs)")
                    pidfd_signal(sp.helper_pidfd, signal.SIGKILL)
                    sp.helper_killed = True
                    self.fatal = True
                continue
            age = now - sp.t0
            if len(sp.hellos) < 2 and age > HELLO_S and sp.status is None:
                self.kill(sp, f"launch handshake: no hello within {HELLO_S} s")
            elif sp.info_record is None and age > INFO_S and sp.status is None and "jail" in sp.hellos:
                self.kill(sp, f"bwrap info record not within {INFO_S} s")
            elif age > lim[f"deadline_{sp.cls}"]:
                self.kill(sp, f"deadline_{sp.cls} {lim[f'deadline_{sp.cls}']} s")
            self.maybe_stop(sp)
        return 0.25

    def dispatch(self, kind, arg):
        if kind == "listen":
            self.accept()
        elif kind == "wake":
            try:
                os.read(self.wake_r, 4096)
            except BlockingIOError:
                pass
        elif kind == "client":
            if arg in self.clients:
                self.client_msg(arg)
        elif kind == "launch":
            self.launch_msg(*arg)
        elif kind == "info":
            if self.spawns[arg].info is not None:
                self.info_ready(arg)
        elif kind == "alloc":
            if self.spawns[arg].alloc is not None:
                self.alloc_ready(arg)
        elif kind == "helper":
            self.helper_exit(arg)

    def internal(self, data, e):
        """An unexpected exception in an event handler: the spawn it belongs to is killed with the text (a client
        event is dropped); without one the invocation is torn down."""
        kind, arg = data
        text = f"broker error: {type(e).__name__}: {e}"[:300]
        k = arg[0] if kind == "launch" else arg if kind in ("info", "alloc", "helper") else None
        if kind == "client" and arg in self.clients:
            k = self.clients[arg]["spawn"]
            self.drop_client(arg, text)
        if k is not None and k in self.spawns and not self.spawns[k].finished:
            self.kill(self.spawns[k], text)
            if kind == "helper":
                self.kill(self.spawns[k], None, final=True)
        else:
            self.errors.append(text)
            self.fatal = True

    def run(self):
        while True:
            timeout = self.deadlines()
            for key, _ in self.sel.select(timeout):
                try:
                    self.dispatch(*key.data)
                except Exception as e:  # noqa: BLE001 (one spawn's fault never takes the broker down)
                    self.internal(key.data, e)
            for sp in list(self.spawns.values()):
                try:
                    self.cleanup_step(sp)
                except Exception as e:  # noqa: BLE001
                    sp.errors.append(f"cleanup: {type(e).__name__}: {e}"[:300])
                    self.fatal = True
            if self.fatal:
                break
            if self.terminate:
                for sp in self.spawns.values():
                    if not sp.finished:
                        self.kill(sp, "the broker was stopped")
                if all(sp.finished for sp in self.spawns.values()):
                    break
        summary = {"spawns": self.next_k, "measured": self.measured, "errors": self.errors.doc(),
                   "spawn_errors": {sp.k: sp.errors.doc() for sp in self.spawns.values() if sp.errors},
                   "fatal": self.fatal}
        self.write_res("broker.json", summary, RESULT_CAP)
        return 1 if self.fatal else 0


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def main(argv, hooks=None):
    if len(argv) != 5 or argv[1] != "--config" or argv[3] != "--report":
        sys.exit("usage: broker.py --config CONFIG --report FD")
    with open(argv[2]) as f:
        cfg = json.load(f)
    if {"hooks", "test"} & set(cfg):
        sys.exit("broker: test settings in a config (D9)")
    if not re.fullmatch(r"\d+(,\d+)*", cfg["cpu_list"]):
        sys.exit("broker: bad cpu_list")
    return Broker(cfg, int(argv[4]), hooks).run()


if __name__ == "__main__":
    sys.exit(main(sys.argv))
