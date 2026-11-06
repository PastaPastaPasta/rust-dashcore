"""The gate holder and the bounded invocation layer (PF0-N plan rev4.1 §2, §3, §6, D13, D53–D58, D67, D75).

One holder per gate (holder.py in its own user and mount ns, holding `GATE`) and one chain per node-bench
invocation:

    setpriv --pdeathsig KILL -- systemd-run --user --scope … -- python3 cgsetup.py --
      nsenter --user=/proc/self/fd/U --mount=/proc/self/fd/M --preserve-credentials --
        unshare --net --pid --ipc --uts --mount --fork --kill-child --mount-proc -- python3 outer_init.py

Both are started from the main thread with a PDEATHSIG prefix, never a preexec_fn (D55), and joined by the ns fds
the harness opened after the holder's ready byte (D56, D67). Units are `pfn-<rootid>-…` (`pfnt-` in tests, D54);
a gate's start stops every unit of its own prefix and rootid and removes stale `g-*` dirs (D13).
"""

import hashlib
import json
import re
import os
import select
import shutil
import signal
import subprocess
import threading
import time

from . import node, paths

_HERE = os.path.dirname(os.path.realpath(__file__))
HOLDER = os.path.join(_HERE, "holder.py")
OUTER_INIT = os.path.join(_HERE, "outer_init.py")
INTERPRETER = "/usr/bin/python3"  # the trusted side's interpreter (census, D64)
MIB = 1 << 20
READY_S = 120
LIMITS = {"mem_run": 8 << 30, "mem_spawn": 4 << 30, "tmpfs": 2 << 30, "tmpfs_inodes": 200_000,
          "prep_tmpfs": 2 << 30, "prep_inodes": 200_000, "tasks_run": 512, "tasks_spawn": 256, "cpus": 2,
          "deadline_version": 300, "deadline_preflight": 300, "deadline_start": 600, "deadline_prep": 3600,
          "deadline_run": 600, "deadline_cleanup": 60}
TMPFS_TOTAL = 12 << 30  # PF0_NODE_TMPFS_TOTAL's default (§6)
VIEW_FLAGS = ("--cap-drop", "ALL", "--unshare-user", "--disable-userns", "--unshare-pid", "--unshare-ipc",
              "--new-session", "--die-with-parent", "--clearenv")
JAIL_ETC = ("/etc/ld.so.cache",)
JAIL_ENV = {"PATH": "/usr/bin:/bin", "LANG": "C"}
VIEW_ENV = {"PATH": "/usr/bin:/bin", "LANG": "C", "HOME": "/pf0/run", "TMPDIR": "/pf0/run"}


class ChainError(Exception):
    """A gate ERROR on the node path, naming the step."""


def rootid(root=None):
    return hashlib.sha256(os.path.realpath(root or paths.root()).encode()).hexdigest()[:8]


def unit_prefix(test=False, root=None):
    return f"{'pfnt' if test else 'pfn'}-{rootid(root)}-"


def tools():
    """The node path's host tools, resolved once (their paths are what the census accepts by exe)."""
    out = {}
    for t in ("setpriv", "systemd-run", "systemctl", "unshare", "nsenter", "bwrap", "perf", "setarch"):
        p = shutil.which(t)
        if not p:
            raise ChainError(f"{t} not found")
        out[t] = os.path.realpath(p)
    return out


def _main_thread():
    if threading.current_thread() is not threading.main_thread():
        raise ChainError("node-path processes are started from the main thread only (D55)")


def _write_json(path, obj):
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "w") as f:
        json.dump(obj, f)


def _tail(path, n=1500):
    try:
        with open(path, "rb") as f:
            f.seek(max(0, os.fstat(f.fileno()).st_size - n))
            return f.read().decode(errors="replace").strip()
    except OSError:
        return ""


def gc(prefix, root=None):
    """D13: stop every unit of this prefix and rootid (gates on one root run one at a time under paths.locked(), so
    any found now is stale) and remove stale gate dirs."""
    p = subprocess.run(["systemctl", "--user", "list-units", "--all", "--plain", "--no-legend", f"{prefix}*"],
                       capture_output=True, text=True)
    units = [line.split()[0] for line in p.stdout.splitlines() if line.split()]
    if units:
        subprocess.run(["systemctl", "--user", "stop", *units], capture_output=True)
        subprocess.run(["systemctl", "--user", "reset-failed", *units], capture_output=True)
    base = os.path.join(root or paths.root(), "node")
    removed = []
    if os.path.isdir(base):
        for d in sorted(os.listdir(base)):
            if d.startswith("g-"):
                shutil.rmtree(os.path.join(base, d), ignore_errors=True)
                removed.append(d)
    return units, removed


def gate_size(exes):
    """D57: GATE holds each side's exe; twice the largest, plus 16 MiB."""
    return 2 * max(os.path.getsize(e) for e in exes) + 16 * MIB


def tmpfs_total_check(rows, total=None, gate=0):
    """§6: the box-level worst case per gate, GATE (D57) + 2 × Σ prep_tmpfs over prepped rows + one RUN + 64 MiB,
    must not exceed PF0_NODE_TMPFS_TOTAL. `rows` are (name, limits, prepped) triples."""
    total = total if total is not None else int(os.environ.get("PF0_NODE_TMPFS_TOTAL", TMPFS_TOTAL))
    prep = sum(lim["prep_tmpfs"] for _, lim, prepped in rows if prepped)
    run = max((lim["tmpfs"] for _, lim, _ in rows), default=0)
    need = gate + 2 * prep + run + 64 * MIB
    if need > total:
        raise ChainError(f"node tmpfs worst case {need >> 20} MiB exceeds PF0_NODE_TMPFS_TOTAL {total >> 20} MiB")
    return need


class Holder:
    """The gate holder (D13, D56, D57, D67): `GATE` with each side's exe, held by its user and mount ns fds."""

    def __init__(self, gate_dir, unit, exes, timeout, tl, unshare_argv=None):
        self.dir, self.unit, self.exes, self.timeout, self.tools = gate_dir, unit, exes, timeout, tl
        self.gate = os.path.join(gate_dir, "gate")
        self.unshare_argv = unshare_argv or holder_unshare(tl)
        self.proc = self.pidfd = self.userns = self.mntns = None

    def start(self):
        _main_thread()
        os.makedirs(self.gate, 0o700)
        size = gate_size(self.exes.values())
        exes = {side: {"path": p, "sha256": _sha(p)} for side, p in self.exes.items()}
        self.sha256 = {side: e["sha256"] for side, e in exes.items()}  # what the holder verifies GATE against
        r, w = os.pipe2(os.O_CLOEXEC)
        cfg = os.path.join(self.dir, "holder.json")
        _write_json(cfg, {"gate": self.gate, "size": size, "inodes": 1024, "exes": exes, "ready_fd": w})
        argv = holder_argv(self.tools, self.unit, self.timeout, self.unshare_argv, cfg)
        self.log = os.path.join(self.dir, "holder.log")
        with open(self.log, "wb") as log:
            self.proc = subprocess.Popen(argv, pass_fds=(w,), stdin=subprocess.DEVNULL, stdout=log, stderr=log)
        os.close(w)
        self.pidfd = os.pidfd_open(self.proc.pid)  # the child is unreaped: its pid can't be recycled (D67)
        try:
            ready, _, _ = select.select([r], [], [], READY_S)
            byte = os.read(r, 1) if ready else None
        finally:
            os.close(r)
        if byte != b"R":
            self.stop()
            why = "timed out" if byte is None else "exited"
            raise ChainError(f"holder: {why} before it was ready: {_tail(self.log)}")
        try:
            self.userns = os.open(f"/proc/{self.proc.pid}/ns/user", os.O_RDONLY | os.O_CLOEXEC)
            self.mntns = os.open(f"/proc/{self.proc.pid}/ns/mnt", os.O_RDONLY | os.O_CLOEXEC)
        except OSError as e:
            self.stop()
            raise ChainError(f"holder: its namespaces: {e}: {_tail(self.log)}") from None
        for fd, ns in ((self.userns, "user"), (self.mntns, "mnt")):
            if os.fstat(fd).st_ino == os.stat(f"/proc/self/ns/{ns}").st_ino:
                self.stop()
                raise ChainError("holder namespaces equal the harness's")
        signal.pidfd_send_signal(self.pidfd, 0)
        return self

    def alive(self):
        return self.pidfd is not None and not select.select([self.pidfd], [], [], 0)[0]

    def check(self):
        if not self.alive():
            raise ChainError(f"the gate holder died: {_tail(self.log)}")

    def nsenter(self):
        """The argv prefix that joins the holder's namespaces by the held fds (pass both fds to the child)."""
        return nsenter_argv(self.tools, self.userns, self.mntns)

    def exe(self, side):
        return os.path.join(self.gate, "exe", side)

    def run_in(self, argv, timeout=60):
        """A short trusted command in the holder's namespaces (PREP mounts and their read-only remount)."""
        _main_thread()
        return subprocess.run([*self.nsenter(), *argv], pass_fds=(self.userns, self.mntns), capture_output=True,
                              text=True, timeout=timeout)

    def mount_prep(self, side, row, size, inodes):
        """A row's PREP tmpfs in the holder (§2), visible to every later invocation; returns its path."""
        path = os.path.join(self.gate, "prep", side, row)
        p = self.run_in([INTERPRETER, "-I", "-B", HOLDER, "--prep-mount", path, str(size), str(inodes)])
        if p.returncode:
            raise ChainError(f"PREP mount for {side}/{row}: {p.stderr.strip()[-500:]}")
        return path

    def prep_readonly(self, path):
        """After the prep invocation: the PREP superblock read-only for the rest of the gate."""
        p = self.run_in([INTERPRETER, "-I", "-B", HOLDER, "--prep-ro", path])
        if p.returncode:
            raise ChainError(f"PREP read-only remount of {path}: {p.stderr.strip()[-500:]}")

    def stop(self):
        if self.proc is not None:
            subprocess.run([self.tools["systemctl"], "--user", "stop", f"{self.unit}.scope"], capture_output=True)
            try:
                self.proc.wait(30)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()
        for name in ("pidfd", "userns", "mntns"):
            if getattr(self, name) is not None:
                os.close(getattr(self, name))
                setattr(self, name, None)


def holder_unshare(tl):
    """Slave propagation (BL-510): a mount the host removes while a gate runs leaves the holder's namespace too, so
    the holder doesn't pin it (an image's loop device stayed attached until the holder exited). The holder's own
    mounts still don't reach the host."""
    return [tl["unshare"], "--user", "--map-root-user", "--mount", "--propagation", "slave", "--fork", "--kill-child",
            "--"]


def holder_argv(tl, unit, timeout, unshare_argv, cfg):
    """The holder's command line (§8 component 11 renders it with placeholders)."""
    return [tl["setpriv"], "--pdeathsig", "KILL", "--", tl["systemd-run"], "--user", "--scope", "--quiet",
            "--collect", f"--unit={unit}", "-p", f"RuntimeMaxSec={timeout}", "--", *unshare_argv, INTERPRETER, "-I",
            "-B", HOLDER, "--config", cfg]


def nsenter_argv(tl, userns, mntns):
    return [tl["nsenter"], f"--user=/proc/self/fd/{userns}", f"--mount=/proc/self/fd/{mntns}",
            "--preserve-credentials", "--"]


def invocation_argv(tl, unit, limits, runtime, nsenter, outer_cfg):
    """An invocation's command line: its scope (`runtime` = RuntimeMaxSec), cgsetup, the holder's namespaces by
    fd, unshare and outer_init (§3; §8 component 11 renders it with placeholders)."""
    return [tl["setpriv"], "--pdeathsig", "KILL", "--", tl["systemd-run"], "--user", "--scope", "--quiet",
            "--collect", f"--unit={unit}", "-p", "Delegate=yes", "-p", f"MemoryMax={limits['mem_run']}",
            "-p", "MemorySwapMax=0", "-p", f"TasksMax={limits['tasks_run']}", "-p", f"RuntimeMaxSec={runtime}",
            "-p", "OOMPolicy=kill", "--", INTERPRETER, "-I", "-B", node.CGSETUP, "--", *nsenter,
            tl["unshare"], "--net", "--pid", "--ipc", "--uts", "--mount", "--fork", "--kill-child",
            "--mount-proc", "--", INTERPRETER, "-I", "-B", OUTER_INIT, "--config", outer_cfg]


def configs(tl, gate_exe, exe_sha256, res, mnt, cfgdir, node_launch, preload, cpu_list, limits, budget, view_cmd,
            view_env=None, ro_binds=(), prep=None):
    """The broker's and outer_init's configs of one invocation, built from its paths alone (no file is touched),
    so §8 renders them with placeholder paths through this same function (D48)."""
    broker = {"socket": os.path.join(mnt, "ctl", "broker"), "res": res, "exe": gate_exe,
              "exe_sha256": exe_sha256, "preload": preload,
              "node_launch": node_launch, "bwrap": tl["bwrap"], "perf": tl["perf"],
              "setarch": tl["setarch"], "arch": os.uname().machine, "perf_events": list(node.PERF_EVENTS),
              "paths": {"/pf0/run": os.path.join(mnt, "run")}, "prep_row": None, "budget": budget,
              "jail_env": dict(JAIL_ENV), "jail_etc": list(JAIL_ETC), "cpu_list": cpu_list, "numa_node": 0,
              "limits": {k: v for k, v in limits.items()
                         if k == "mem_spawn" or k == "tasks_spawn" or k.startswith("deadline_")}}
    rw_binds, rw = [], []
    if prep is not None:
        row, path, writable = prep
        if writable:
            broker["paths"][f"/pf0/prep/{row}"] = path
            broker["prep_row"] = row
            rw_binds, rw = [(path, f"/pf0/prep/{row}")], [path]
        else:
            broker["prep_source"] = os.path.join(path, "datadir")
            ro_binds = [*ro_binds, (path, f"/pf0/prep/{row}")]
    view = view_argv(tl, mnt, node_launch, list(view_cmd), view_env or {}, ro_binds, rw_binds)
    outer = {"res": res, "rw": rw, "mnt": mnt, "run_size": limits["tmpfs"], "run_inodes": limits["tmpfs_inodes"],
             "broker": [INTERPRETER, "-I", "-B", node.BROKER, "--config", os.path.join(cfgdir, "broker.json"),
                        "--report", "{report}"],
             "view": view, "manifest": "manifest.json", "census_interval_s": 0.25, "hello_s": 60,
             "broker_stop_s": limits["deadline_cleanup"] + 30,
             "exes": {"node_launch": os.path.realpath(node_launch), "perf": tl["perf"], "bwrap": tl["bwrap"],
                      "interpreter": os.path.realpath(INTERPRETER)}}
    return broker, outer


def _sha(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def spawn_bound(budget, limits):
    """D75: D = Σ over the spawn budget of (the class deadline + deadline_cleanup)."""
    return sum(n * (limits[f"deadline_{cls}"] + limits["deadline_cleanup"]) for cls, n in budget.items())


def view_argv(tl, mnt, stub, cmd, env, ro_binds=(), rw_binds=()):
    """node-bench's view (§2 table): /usr, the stub, RUN writable at /pf0/run, CTL read-only at /pf0/ctl, the
    given binds (PREP is writable in its prep invocation only), and nothing of /work, $HOME, /run, /sys, GATE or
    res."""
    a = [tl["bwrap"], "--ro-bind", "/usr", "/usr"]
    for link, target in (("/bin", "usr/bin"), ("/sbin", "usr/sbin"), ("/lib", "usr/lib"), ("/lib64", "usr/lib64")):
        a += ["--symlink", target, link]
    for f in JAIL_ETC:
        a += ["--ro-bind", f, f]
    a += ["--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp", "--bind", os.path.join(mnt, "run"), "/pf0/run",
          "--ro-bind", os.path.join(mnt, "ctl"), "/pf0/ctl", "--ro-bind", stub, "/pf0/bin/node-launch"]
    for src, dst in ro_binds:
        a += ["--ro-bind", src, dst]
    for src, dst in rw_binds:
        a += ["--bind", src, dst]
    a += list(VIEW_FLAGS)
    for k, v in sorted({**VIEW_ENV, **env}.items()):
        a += ["--setenv", k, v]
    return a + ["--chdir", "/pf0/run", "--", *cmd]


class Invocation:
    """One chain (§3) for one side: its scope, its res dir, its configs. `prep` is `(row, PREP path, writable)`:
    writable for the row's prep invocation (a class-prep spawn on `/pf0/prep/<row>/datadir`), read-only after it
    (each spawn's `/pf0/run/node` is a copy of `PREP/datadir`, D60)."""

    def __init__(self, holder, side, n, unit, limits=None, budget=None, view_cmd=(), view_env=None, ro_binds=(),
                 broker_extra=None, outer_extra=None, node_launch=None, census_hello_s=60, prep=None):
        self.holder, self.side, self.n, self.unit = holder, side, n, unit
        self.limits = {**LIMITS, **(limits or {})}
        self.budget = budget or {"version": 1, "run": 1}
        base = os.path.join(holder.dir, "inv", f"{side}-{n}")
        self.res, self.mnt, self.cfgdir = (os.path.join(base, d) for d in ("res", "mnt", "cfg"))
        for d in (self.res, self.mnt, self.cfgdir):
            os.makedirs(d, 0o700)
        self.node_launch = node_launch or node.node_launch()
        self.cpu_list = ",".join(str(c) for c in numa0_cpus(self.limits["cpus"]))
        broker, outer = configs(holder.tools, holder.exe(side), holder.sha256[side], self.res, self.mnt, self.cfgdir,
                                self.node_launch, node.preload_lib(), self.cpu_list, self.limits, self.budget,
                                view_cmd, view_env, ro_binds, prep)
        broker.update(broker_extra or {})
        self.broker_cfg = os.path.join(self.cfgdir, "broker.json")
        _write_json(self.broker_cfg, broker)
        outer["hello_s"] = census_hello_s
        outer.update(outer_extra or {})
        self.outer_cfg = os.path.join(self.cfgdir, "outer.json")
        _write_json(self.outer_cfg, outer)
        self.d = spawn_bound(self.budget, self.limits)
        self.log = os.path.join(base, "chain.log")

    def argv(self):
        return invocation_argv(self.holder.tools, self.unit, self.limits, self.d + 30, self.holder.nsenter(),
                               self.outer_cfg)

    def start(self):
        _main_thread()
        self.holder.check()
        with open(self.log, "wb") as log:
            self.proc = subprocess.Popen(self.argv(), pass_fds=(self.holder.userns, self.holder.mntns),
                                         stdin=subprocess.DEVNULL, stdout=log, stderr=log)
        self.t0 = time.monotonic()
        return self

    def wait(self):
        """The chain's results; the harness timeout is D + 60 (D75)."""
        try:
            rc = self.proc.wait(self.d + 60)
            timed_out = False
        except subprocess.TimeoutExpired:
            self.kill()
            rc, timed_out = self.proc.wait(), True
        return Result(self, rc, timed_out, self.holder.alive())

    def run(self):
        return self.start().wait()

    def kill(self):
        self.proc.kill()
        subprocess.run([self.holder.tools["systemctl"], "--user", "kill", "--signal=KILL", f"{self.unit}.scope"],
                       capture_output=True)


class Result:
    def __init__(self, inv, rc, timed_out, holder_alive=True):
        self.rc, self.timed_out, self.res, self.holder_alive = rc, timed_out, inv.res, holder_alive
        self.log = _tail(inv.log)
        self.outer = self.json("outer.json")
        self.broker = self.json("broker.json")

    def json(self, name):
        try:
            with open(os.path.join(self.res, name)) as f:
                return json.load(f)
        except (OSError, ValueError):
            return None

    def spawn(self, k):
        return self.json(f"spawn-{k}.result.json")

    def text(self, name):
        return _tail(os.path.join(self.res, name), 64 * 1024)

    def errors(self):
        """Every error the chain reported, outermost first."""
        out = []
        if self.timed_out:
            out.append("invocation timeout")
        if not self.holder_alive:
            out.append("the gate holder died during the invocation")
        if self.outer is None:
            out.append(f"outer_init wrote no result (exit {self.rc}): {self.log}")
        else:
            out += self.outer["errors"]
            if self.outer["view"] != {"exit": 0}:
                out.append(f"node-bench ended {self.outer['view']}")
        if self.broker is None:
            out.append("the broker wrote no result")
        else:
            out += self.broker["errors"]
            for k, errs in sorted(self.broker.get("spawn_errors", {}).items()):
                out += [f"spawn-{k}: {e}" for e in errs]
        if self.rc != 0 and not out:
            out.append(f"the chain exited {self.rc}: {self.log}")
        return out


def numa0_cpus(n):
    """The first `n` of NUMA node 0's online CPUs in order (the placement list, D37); fewer is a ChainError."""
    try:
        with open("/sys/devices/system/node/node0/cpulist") as f:
            text = f.read().strip()
    except OSError:
        text = None
    out = []
    if text is None:
        out = sorted(os.sched_getaffinity(0))
    else:
        for part in text.split(","):
            lo, _, hi = part.partition("-")
            out += range(int(lo), int(hi or lo) + 1)
    if len(out) < n:
        raise ChainError(f"placement needs {n} CPUs, NUMA node 0 has {len(out)}")
    return out[:n]


PROBE_STEPS = (
    ("perf_event_paranoid", r"perf_event_paranoid above 2"),
    ("holder", r"holder:"),
    ("nsenter", r"nsenter:"),
    ("unshare", r"unshare:"),
    ("outer_init", r"outer_init: setup"),
    ("view", r"bwrap:"),
    ("placement", r"placement unavailable"),
    ("jail", r"\bjail\b"),
    ("perf", r"\bperf\b"),
)


def probe_step(r):
    """The step a failed probe chain names (D58), from what it reported: the logs and errors, and of the spawn
    only its errors (its other fields name every step, `answered: [into, jail]`)."""
    sp = r.spawn(0) or {}
    text = "\n".join([r.log, r.text("view.err"), *r.errors(), *sp.get("errors", []), sp.get("kill_reason") or ""])
    return next((s for s, needle in PROBE_STEPS[2:] if re.search(needle, text)), "chain")


def self_probe(gate_dir, prefix, tl, paranoid_path="/proc/sys/kernel/perf_event_paranoid", unshare_argv=None,
               node_launch=None):
    """D58: the whole chain once, before any bench: the holder's user ns, the ns-fd nsenter, unshare, the view,
    the hardened jail, clone3 placement and `perf stat -e instructions:u` counting `true` inside. A failure is a
    ChainError naming the step."""
    with open(paranoid_path) as f:
        if int(f.read().strip()) > 2:
            raise ChainError("self-probe: perf_event_paranoid above 2")
    d = os.path.join(gate_dir, "probe")
    os.makedirs(d, 0o700)
    try:
        holder = Holder(d, f"{prefix}probe-holder", {"probe": shutil.which("true")}, 300, tl,
                        unshare_argv=unshare_argv).start()
    except ChainError as e:
        raise ChainError(f"self-probe: holder: {e}") from None
    try:
        inv = Invocation(holder, "probe", 0, f"{prefix}probe-0", budget={"run": 1},
                         limits={"deadline_run": 120, "deadline_cleanup": 30},
                         view_cmd=["/pf0/bin/node-launch", "--client", "--class", "run", "--measure", "counter", "--",
                                   "/pf0/bin/node-launch", "-datadir=/pf0/run/node"],
                         node_launch=node_launch)
        r = inv.run()
        sp = r.spawn(0)
        perf = None
        if sp and sp.get("status") == {"exit": 0}:
            try:
                with open(os.path.join(r.res, "spawn-0.perf")) as f:
                    perf = node.parse_perf_csv(f.read())
            except (OSError, ValueError):
                perf = None
        if r.rc == 0 and not r.errors() and perf and perf.get("instructions", 0) > 0:
            return {"instructions": perf["instructions"], "cpus": sp["cpus"]}
        raise ChainError(f"self-probe: {probe_step(r)}: {(r.errors() or [r.log or 'no output'])[0][:500]}")
    finally:
        holder.stop()
