"""An invocation's pid 1 (PF0-N plan rev4.1 §2, §3, D40, D53, D64, D80).

    unshare --net --pid --ipc --uts --mount --fork --kill-child --mount-proc -- python3 -I -B outer_init.py --config C

It runs in the invocation's own mount ns, inside the gate holder's user ns. Its setup is all in-process (D64):

- bring lo up;
- make the host read-only, except the invocation's `res` dir, its own scope's cgroupfs and this pid ns's /proc;
- mount `RUN` and `CTL` under the invocation's host mount dir.

Then it starts the broker and the node-bench view and keeps the census (D40, D64) until either exits. When the view
exits, the broker is stopped (it kills what is still live) and node-bench's manifest is copied out of `RUN` with a
64 KiB cap. The view's stdout and stderr are pipes drained into `res` with a cap each (§6: node-bench, and through
the stub the node, write them). When it exits itself, the pid ns goes, and so does everything else in the
invocation. The result is `res/outer.json`. Self-contained: it runs under `python3 -I`.
"""

import ctypes
import errno
import fcntl
import json
import os
import selectors
import signal
import socket
import struct
import subprocess
import sys
import time

_libc = ctypes.CDLL(None, use_errno=True)
SYS_MOUNT_SETATTR = 442  # x86_64 and the generic table (aarch64)
AT_FDCWD = -100
AT_RECURSIVE = 0x8000
MOUNT_ATTR_RDONLY = 0x1
MS_BIND = 0x1000
SIOCGIFFLAGS, SIOCSIFFLAGS, IFF_UP = 0x8913, 0x8914, 0x1
CLK_TCK = os.sysconf("SC_CLK_TCK")
ERRORS_KEPT = 20
REPORT_LINE = 4096
MANIFEST_CAP = 64 * 1024
VIEW_LOG_CAP = 256 * 1024
DIR_FLAGS = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
ROOT_CGROUP = "/sys/fs/cgroup/"  # how the census spells a process in the root cgroup


class SetupError(Exception):
    pass


class _MountAttr(ctypes.Structure):
    _fields_ = [("attr_set", ctypes.c_uint64), ("attr_clr", ctypes.c_uint64), ("propagation", ctypes.c_uint64),
                ("userns_fd", ctypes.c_uint64)]


def mount(src, target, fstype, flags, opts):
    if _libc.mount(src.encode(), target.encode(), fstype.encode() if fstype else None, flags,
                   opts.encode() if opts else None) != 0:
        e = ctypes.get_errno()
        raise SetupError(f"mount {fstype or 'bind'} on {target}: {os.strerror(e)}")


def mount_setattr(path, recursive, set_=0, clr=0):
    attr = _MountAttr(set_, clr, 0, 0)
    if _libc.syscall(SYS_MOUNT_SETATTR, AT_FDCWD, path.encode(), AT_RECURSIVE if recursive else 0,
                     ctypes.byref(attr), ctypes.sizeof(attr)) != 0:
        e = ctypes.get_errno()
        raise SetupError(f"mount_setattr {path}: {os.strerror(e)}")


def lo_up():
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        req = struct.pack("16sh14x", b"lo", 0)
        flags = struct.unpack("16sh14x", fcntl.ioctl(s, SIOCGIFFLAGS, req))[1]
        fcntl.ioctl(s, SIOCSIFFLAGS, struct.pack("16sh14x", b"lo", flags | IFF_UP))


def read(path):
    with open(path) as f:
        return f.read()


def starttime(pid):
    return int(read(f"/proc/{pid}/stat").rsplit(")", 1)[1].split()[19])


def own_scope():
    """The invocation's scope: the parent of the harness cgroup cgsetup moved this process into."""
    return os.path.dirname("/sys/fs/cgroup" + read("/proc/self/cgroup").strip().removeprefix("0::"))


def setup(cfg):
    """D80: the host goes read-only first (res is bound onto itself so it can be made writable again on its own,
    and so is a prep invocation's PREP, and the invocation's own scope in cgroupfs); RUN and CTL are mounted
    afterwards, under the invocation's mount dir."""
    lo_up()
    rw = [cfg["res"], *cfg.get("rw", ()), own_scope()]
    for path in rw:
        mount(path, path, None, MS_BIND, None)
    mount_setattr("/", True, set_=MOUNT_ATTR_RDONLY)
    for path in (*rw, "/proc"):
        mount_setattr(path, False, clr=MOUNT_ATTR_RDONLY)
    mnt = cfg["mnt"]
    mount("pf0", mnt, "tmpfs", 0, "size=64k,nr_inodes=16,mode=0700")
    for name, opts in (("run", f"size={cfg['run_size']},nr_inodes={cfg['run_inodes']},mode=0700"),
                       ("ctl", "size=1m,nr_inodes=64,mode=0700")):
        os.mkdir(os.path.join(mnt, name), 0o700)
        mount("pf0", os.path.join(mnt, name), "tmpfs", 0, opts)


class Census:
    """D40, D64: every process of the outer pid ns is a registered `(pid, starttime)` identity, or a spawn chain
    member (in a reported spawn-k, exe node-launch, perf or bwrap), or a fresh child of the broker (in harness, exe
    the interpreter or node-launch, younger than the hello deadline). Nested pid namespaces (the view, the jail) are
    not members.

    D83: a chain member that is exiting has no exe any more (`exit_mm`) and then leaves its cgroup (`cgroup_exit`)
    before it is a zombie, and the broker may remove spawn-k as soon as only such processes are left. So a
    reported spawn-k counts though it is gone, and in one an exe of None counts as a chain member: nothing but
    the chain enters spawn-k (the broker places it there with clone3), and only an exiting process has no exe.

    D50: a nested process is exactly one level down. `NSpid` lists a process's pid in every pid ns from this /proc's
    (this invocation's, `--mount-proc`) down to its own, so two entries means its pid ns is a child of this one; a
    deeper one is an error. (`NS_GET_PARENT` would say the same, but a non-dumpable chain process's `ns/pid` can't
    be opened.)"""

    def __init__(self, cfg, harness):
        self.registered = {(1, starttime(1))}
        self.spawn_cgroups = set()
        self.harness = harness
        self.chain_exes = {cfg["exes"][k] for k in ("node_launch", "perf", "bwrap")}
        self.broker_exes = {cfg["exes"]["interpreter"], cfg["exes"]["node_launch"]}
        self.hello_s = cfg["hello_s"]
        self.broker = None
        self.scans = 0
        self.passes = 0  # full passes
        self.unplaced = {}  # (pid, starttime) -> the full pass that first saw it in the root cgroup (D104)

    def register(self, pid, st=None):
        self.registered.add((pid, starttime(pid) if st is None else st))

    def scan(self, pids=None, final=False):
        """The unexpected processes as {pid: error text}; `pids` limits the scan to those. A `final` scan defers
        nothing (D106)."""
        self.scans += 1
        if pids is None:
            self.passes += 1
        uptime = float(read("/proc/uptime").split()[0])
        broker_live = self.broker is not None and self._alive(*self.broker)
        out = {}
        for d in os.listdir("/proc") if pids is None else map(str, pids):
            if not d.isdigit():
                continue
            pid = int(d)
            try:
                status = dict(line.split(":\t", 1) for line in read(f"/proc/{pid}/status").splitlines()
                              if ":\t" in line)
                if status.get("State", "").startswith(("Z", "X")):
                    continue
                depth = len(status["NSpid"].split())
                if depth > 1:
                    if depth > 2:
                        out[pid] = (f"census: pid {pid} ({status.get('Name', '?').strip()}) is {depth - 1} pid "
                                    "namespaces below the invocation's")
                    continue
                st = starttime(pid)
                if (pid, st) in self.registered:
                    continue
                cg = "/sys/fs/cgroup" + read(f"/proc/{pid}/cgroup").strip().removeprefix("0::").removesuffix(
                    " (deleted)")
                if not final and cg == ROOT_CGROUP and self.unplaced.get((pid, st), self.passes) == self.passes:
                    # D104: fork shows a child in the root cgroup (init_css_set) from the moment its pid is visible
                    # until cgroup_post_fork places it, and a loaded host can stretch that past the confirming
                    # rescan. Nothing of the invocation stays there, so such a process is judged by the next full
                    # pass, wherever it is then, or by the final one (D106).
                    self.unplaced.setdefault((pid, st), self.passes)
                    continue
                try:
                    exe = os.readlink(f"/proc/{pid}/exe")
                except OSError:
                    exe = None
                if cg in self.spawn_cgroups and (exe in self.chain_exes or exe is None):
                    continue
                if (broker_live and int(status["PPid"]) == self.broker[0] and cg == self.harness
                        and exe in self.broker_exes and uptime - st / CLK_TCK < self.hello_s):
                    continue
                comm = status.get("Name", "?").strip()
                where = cg.removeprefix(self.harness.rsplit("/", 1)[0] + "/")
                out[pid] = (f"census: unexpected process pid {pid} ({comm}, {exe}; cgroup {where}, "
                            f"ppid {status['PPid'].strip()}, age {uptime - st / CLK_TCK:.2f} s)")
            except (FileNotFoundError, ProcessLookupError, KeyError, ValueError, IndexError):
                continue  # gone mid-read
        return out

    @staticmethod
    def _alive(pid, st):
        try:
            return starttime(pid) == st
        except OSError:
            return False


def status_of(ws):
    return {"exit": os.waitstatus_to_exitcode(ws)} if os.WIFEXITED(ws) else {"signal": os.WTERMSIG(ws)}


class OuterInit:
    def __init__(self, cfg):
        self.cfg = cfg
        self.errors = []
        self.dropped = 0
        self.result = {"view": None, "broker": None, "manifest": None}
        self.logs = {}  # read fd -> [res fd, bytes kept, bytes dropped, name]
        self.log_dropped = {}

    def error(self, text):
        if len(self.errors) < ERRORS_KEPT:
            self.errors.append(text)
        else:
            self.dropped += 1

    def run(self):
        cfg = self.cfg
        harness = "/sys/fs/cgroup" + read("/proc/self/cgroup").strip().removeprefix("0::")
        self.census = Census(cfg, harness)
        sel = selectors.DefaultSelector()
        wake_r, wake_w = os.pipe2(os.O_NONBLOCK | os.O_CLOEXEC)
        signal.set_wakeup_fd(wake_w)
        signal.signal(signal.SIGCHLD, lambda *_: None)
        stop = []
        signal.signal(signal.SIGTERM, lambda *_: stop.append(1))
        sel.register(wake_r, selectors.EVENT_READ, "wake")
        self.sel, self.buf = sel, b""
        rep_r, rep_w = os.pipe2(os.O_CLOEXEC)
        argv = [a.replace("{report}", str(rep_w)) for a in cfg["broker"]]
        broker = subprocess.Popen(argv, pass_fds=(rep_w,), stdin=subprocess.DEVNULL)
        os.close(rep_w)
        self.census.register(broker.pid)
        self.census.broker = (broker.pid, starttime(broker.pid))
        os.set_blocking(rep_r, False)
        self.rep_r = rep_r
        sel.register(rep_r, selectors.EVENT_READ, "report")
        procs = {broker.pid: "broker"}
        self.listening = False
        view = None
        t0 = time.monotonic()
        stopping_since = None
        next_scan = 0
        failed = False
        while True:
            now = time.monotonic()
            if now >= next_scan:
                next_scan = now + cfg["census_interval_s"]
                failed |= self._check()
            for key, _ in sel.select(max(0.0, next_scan - time.monotonic())):
                if key.data == "wake":
                    try:
                        os.read(wake_r, 4096)
                    except BlockingIOError:
                        pass
                elif key.data == "report" and self._drain():
                    failed |= self._check()
                elif key.data == "log":
                    self._pump(key.fd)
            for pid, ws in self._reap():
                who = procs.pop(pid, None)
                if who:
                    self.result[who] = status_of(ws)
            if failed or stop:
                break
            if view is None and self.listening:
                out_w, err_w = self._log("view.out"), self._log("view.err")
                view = subprocess.Popen(cfg["view"], stdin=subprocess.DEVNULL, stdout=out_w, stderr=err_w)
                os.close(out_w)
                os.close(err_w)
                self.census.register(view.pid)
                procs[view.pid] = "view"
            elif view is None and now - t0 > cfg["hello_s"]:
                self.error(f"the broker did not listen within {cfg['hello_s']} s")
                break
            if self.result["broker"] is not None:
                if view is None:
                    self.error(f"the broker exited {self.result['broker']} before node-bench started")
                elif self.result["view"] is None:
                    self.error(f"the broker exited {self.result['broker']} before node-bench")
                elif self.result["broker"] != {"exit": 0}:
                    self.error(f"the broker exited {self.result['broker']}")
                break
            if self.result["view"] is not None and stopping_since is None:
                self._copy_manifest()
                os.kill(broker.pid, signal.SIGTERM)  # not Popen.send_signal: its poll() could reap the status
                stopping_since = now
            if stopping_since is not None and now - stopping_since > cfg["broker_stop_s"]:
                self.error(f"the broker did not stop within {cfg['broker_stop_s']} s")
                break
        # D106: one last full pass once the loop is over, deferring nothing: the loop can end (both children
        # reaped) before the tick that judges a process deferred above, or one started since the last pass. Only on
        # the way to success (D111): a failed or stopped run may still have a live broker forking its chain.
        if not failed and not stop and not self.errors and self.result["broker"] is not None:
            self._check(final=True)
        if stop:
            self.error("stopped by SIGTERM (the invocation's bound or the holder's stop)")
        for fd in list(self.logs):
            self._pump(fd, final=True)
        self.result.update(errors=self.errors + ([f"... and {self.dropped} more"] if self.dropped else []),
                           census_scans=self.census.scans, stopped=bool(stop), view_log_dropped=self.log_dropped)
        self._write("outer.json", json.dumps(self.result, separators=(",", ":")).encode())
        ok = not self.errors and self.result["view"] == {"exit": 0} and self.result["broker"] == {"exit": 0}
        return 0 if ok else 1

    def _check(self, final=False):
        """A census pass. The broker reports a spawn cgroup before it starts the chain in it, so a candidate is
        confirmed only after the reports written so far are read (D64)."""
        bad = self.census.scan(final=final)
        if bad:
            self._drain()
            bad = self.census.scan(bad, final=final)
        for text in bad.values():
            self.error(text)
        return bool(bad)

    def _drain(self):
        """Read every report line written so far; whether any came."""
        got = False
        while self.rep_r is not None:
            try:
                chunk = os.read(self.rep_r, 65536)
            except BlockingIOError:
                break
            if not chunk:
                self.sel.unregister(self.rep_r)
                os.close(self.rep_r)
                self.rep_r = None
                break
            got = True
            self.buf = self._report(self.buf + chunk)
        return got

    def _report(self, buf):
        """The broker's registrations (helpers by `(pid, starttime)`, spawn cgroups) and census triggers."""
        lines = buf.split(b"\n")
        for line in lines[:-1]:
            try:
                msg = json.loads(line)
            except ValueError:
                self.error("the broker's report is not JSON")
                continue
            if "helper" in msg:
                self.census.register(msg["helper"], msg["starttime"])
            if "cgroup" in msg:
                self.census.spawn_cgroups.add(msg["cgroup"])
            if msg.get("event") == "listening":
                self.listening = True
        rest = lines[-1]
        if len(rest) > REPORT_LINE:
            self.error("a broker report line over 4 KiB")
            rest = b""
        return rest

    @staticmethod
    def _reap():
        out = []
        while True:
            try:
                pid, ws = os.waitpid(-1, os.WNOHANG)
            except ChildProcessError:
                return out
            if pid == 0:
                return out
            out.append((pid, ws))

    def _log(self, name):
        """A pipe for one of the view's streams; its read end drains into `res/<name>` (VIEW_LOG_CAP, D6)."""
        fd = os.open(os.path.join(self.cfg["res"], name),
                     os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
        r, w = os.pipe2(os.O_CLOEXEC)
        os.set_blocking(r, False)
        self.logs[r] = [fd, 0, 0, name]
        self.sel.register(r, selectors.EVENT_READ, "log")
        return w

    def _pump(self, r, final=False):
        """Copy what the pipe holds into its file up to the cap and count the rest, at most 64 reads per wake-up (a
        writer that never pauses can't hold the loop); close it at EOF (or when `final`: a node may still hold the
        write end, and the dropped count is then a lower bound)."""
        log = self.logs[r]
        chunk = None
        for _ in range(64):
            try:
                chunk = os.read(r, 65536)
            except BlockingIOError:
                chunk = None
            if not chunk:
                break
            keep = chunk[:max(0, VIEW_LOG_CAP - log[1])]
            if keep:
                os.write(log[0], keep)
            log[1] += len(keep)
            log[2] += len(chunk) - len(keep)
        if chunk == b"" or final:
            self.sel.unregister(r)
            os.close(r)
            os.close(log[0])
            self.log_dropped[log[3]] = log[2]
            del self.logs[r]

    def _write(self, name, data):
        fd = os.open(os.path.join(self.cfg["res"], name), os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, "wb") as f:
            f.write(data)

    def _copy_manifest(self):
        """node-bench's manifest, from RUN (written by node-bench, so opened without following links)."""
        name = self.cfg.get("manifest")
        if not name:
            return
        run = os.open(os.path.join(self.cfg["mnt"], "run"), DIR_FLAGS)
        try:
            fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC, dir_fd=run)
        except OSError as e:
            self.result["manifest"] = f"missing ({errno.errorcode.get(e.errno, e.errno)})"
            return
        finally:
            os.close(run)
        with os.fdopen(fd, "rb") as f:
            st = os.fstat(f.fileno())
            if not (st.st_mode & 0o170000 == 0o100000) or st.st_size > MANIFEST_CAP:
                self.result["manifest"] = f"refused (not a regular file of at most {MANIFEST_CAP} B)"
                return
            data = f.read(MANIFEST_CAP + 1)
        if len(data) > MANIFEST_CAP:
            self.result["manifest"] = f"refused (over {MANIFEST_CAP} B)"
            return
        self._write("manifest.json", data)
        self.result["manifest"] = "copied"


def main(argv):
    os.closerange(3, os.sysconf("SC_OPEN_MAX"))  # nothing inherited: no fd to the holder's namespaces (D56)
    if len(argv) != 3 or argv[1] != "--config":
        sys.exit("usage: outer_init.py --config CONFIG")
    with open(argv[2]) as f:
        cfg = json.load(f)
    if os.getpid() != 1:
        sys.exit("outer_init: not pid 1 of its pid namespace")
    try:
        setup(cfg)
    except (SetupError, OSError) as e:
        sys.exit(f"outer_init: setup: {e}")
    sys.exit(OuterInit(cfg).run())


if __name__ == "__main__":
    main(sys.argv)
