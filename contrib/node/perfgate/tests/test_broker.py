"""Tests for the PF0-N spawn broker and node-launch (plan rev4.1 §4, §5, D24, D25, D37, D46, D47, D66, D70, D74).

Each test runs a broker in its own delegated `pfnt-` scope, as an invocation does (the holder, the outer
namespaces and the census come in the next commit), with a fake rdashd and a fake node-bench: the real stub
(`node-launch --client`) in a small bwrap view, or a raw client of the broker protocol.

    python3 -m unittest discover -s contrib/node/perfgate/tests
"""

import fcntl
import hashlib
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import types
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.realpath(__file__))))

from perfgate import measure, node, sandbox  # noqa: E402

HERE = os.path.dirname(os.path.realpath(__file__))
FAKE_NODE = os.path.join(HERE, "fake_rdashd.py")
HOOKED = os.path.join(HERE, "broker_hooked.py")
LIMITS = {"mem_spawn": 1 << 30, "tasks_spawn": 64, "deadline_version": 60, "deadline_preflight": 60,
          "deadline_start": 60, "deadline_prep": 60, "deadline_run": 60, "deadline_cleanup": 30}
BUDGET = {"version": 2, "preflight": 1, "start": 1, "run": 1}
REFUSAL = "Error: -refuse is not supported by rdashd yet (XX1)."
LAUNCH_C_SHA256 = "fb43626d41dfb85854de229468e05da1f6f94f0ab7bb5c511b480271f380ab51"  # at 285e46b4f
COUNTING_CONSTANTS_SHA256 = "cbe8bb3aca04392eacac3dc01f6a13aff8506107c552bac57e3fd1d697f200ac"  # at 1b + 1c


def missing():
    """Why this host can't run an invocation, or None."""
    for tool in ("bwrap", "perf", "systemd-run", "setarch", "cc"):
        if not shutil.which(tool):
            return f"no {tool}"
    p = subprocess.run(["systemd-run", "--user", "--scope", "-q", "-p", "Delegate=yes", "--", "true"],
                       capture_output=True)
    if p.returncode != 0:
        return "no delegated user scopes"
    if not os.path.isdir("/sys/devices/system/node/node0") or not {0, 1} <= os.sched_getaffinity(0):
        return "CPUs 0-1 or NUMA node 0 unavailable"
    return None


SKIP = missing()
_ROOT = None
_UNITS = None  # pfnt-<rootid>- (D54)


def setUpModule():
    global _ROOT, _UNITS
    if SKIP:
        return
    _ROOT = tempfile.mkdtemp(prefix="broker-", dir=os.environ.get("PERFGATE_TEST_DIR"))
    os.chmod(_ROOT, 0o700)
    _UNITS = f"pfnt-{hashlib.sha256(os.path.realpath(_ROOT).encode()).hexdigest()[:8]}-"


def live_units():
    out = subprocess.run(["systemctl", "--user", "list-units", "--all", "--no-legend", "--plain", f"{_UNITS}*"],
                         capture_output=True, text=True).stdout
    return [line.split()[0] for line in out.splitlines() if line.strip()]


def tearDownModule():
    if not _ROOT:
        return
    end = time.monotonic() + 120  # D51: poll, never a short upper bound
    while live_units() and time.monotonic() < end:
        time.sleep(0.5)
    left = live_units()
    shutil.rmtree(_ROOT, ignore_errors=True)
    if left:
        raise AssertionError(f"test units left behind: {left}")


def built(test_hooks=False):
    with mock.patch.dict(os.environ, {"PERFGATE_ROOT": _ROOT}):
        return node.node_launch(test_hooks), node.preload_lib()


def read(path, mode="r"):
    with open(path, mode) as f:
        return f.read()


def file_sha256(path):
    return hashlib.sha256(read(path, "rb")).hexdigest()


def descendants(pid):
    kids = {}
    for d in os.listdir("/proc"):
        if d.isdigit():
            try:
                with open(f"/proc/{d}/stat") as f:
                    ppid = int(f.read().rsplit(")", 1)[1].split()[1])
            except (OSError, ValueError, IndexError):
                continue
            kids.setdefault(ppid, []).append(int(d))
    out, todo = [], [pid]
    while todo:
        p = todo.pop()
        out += kids.get(p, [])
        todo += kids.get(p, [])
    return out


class Invocation:
    """A broker in a delegated scope with its RUN, CTL and res/ directories."""

    n = 0

    def __init__(self, test, budget=BUDGET, limits=None, props=(), prefix=(), hooks=None):
        Invocation.n += 1
        self.w = tempfile.mkdtemp(prefix="inv-", dir=_ROOT)
        for d in ("run", "ctl", "res"):
            os.mkdir(os.path.join(self.w, d), 0o700)
        self.node_launch, preload = built()
        node_launch = built(test_hooks=True)[0] if hooks else self.node_launch
        cfg = {"socket": self.path("ctl/broker"), "res": self.path("res"),
               "exe": FAKE_NODE, "exe_sha256": file_sha256(FAKE_NODE),
               "preload": preload, "node_launch": node_launch, "bwrap": shutil.which("bwrap"),
               "perf": shutil.which("perf"), "setarch": shutil.which("setarch"), "arch": os.uname().machine,
               "perf_events": list(node.PERF_EVENTS), "paths": {"/pf0/run": self.path("run")}, "prep_row": None,
               "budget": budget, "jail_env": {"PATH": "/usr/bin:/bin", "LANG": "C"},
               "jail_etc": ["/etc/ld.so.cache"], "cpu_list": "0,1", "numa_node": 0,
               "limits": {**LIMITS, **(limits or {})}}
        with open(self.path("cfg.json"), "w") as f:
            json.dump(cfg, f)
        broker = [sys.executable, "-I", "-B", node.BROKER]
        if hooks:
            with open(self.path("test.json"), "w") as f:
                json.dump(hooks, f)
            broker = [sys.executable, "-I", "-B", HOOKED, "--broker", node.BROKER, "--test", self.path("test.json"),
                      "--"]
        self.unit = f"{_UNITS}broker-{os.getpid()}-{Invocation.n}"
        argv = ["systemd-run", "--user", "--scope", "-q", "--collect", f"--unit={self.unit}", "-p", "Delegate=yes"]
        for p in props:
            argv += ["-p", p]
        argv += ["--", *prefix, sys.executable, "-I", "-B", node.CGSETUP, "--", *broker, "--config",
                 self.path("cfg.json"), "--report", "1"]
        with open(self.path("report"), "w") as out, open(self.path("broker.err"), "w") as err:
            self.proc = subprocess.Popen(argv, stdout=out, stderr=err)
        test.addCleanup(self.stop)
        for _ in range(200):
            if os.path.exists(self.path("ctl/broker")) or self.proc.poll() is not None:
                break
            time.sleep(0.05)
        if not os.path.exists(self.path("ctl/broker")):
            raise AssertionError(f"broker did not start: {self.text('broker.err')}")

    def path(self, rel):
        return os.path.join(self.w, rel)

    def text(self, rel):
        with open(self.path(rel)) as f:
            return f.read()

    def view(self):
        """node-bench's side, cut down: the stub at /pf0/bin/node-launch, RUN and a read-only CTL."""
        return ["bwrap", "--ro-bind", "/usr", "/usr", "--symlink", "usr/bin", "/bin", "--symlink", "usr/lib", "/lib",
                "--symlink", "usr/lib64", "/lib64", "--proc", "/proc", "--dev", "/dev",
                "--bind", self.path("run"), "/pf0/run", "--ro-bind", self.path("ctl"), "/pf0/ctl",
                "--ro-bind", self.node_launch, "/pf0/bin/node-launch", "--cap-drop", "ALL", "--unshare-user",
                "--disable-userns", "--unshare-pid", "--die-with-parent", "--chdir", "/pf0/run", "--"]

    def stub(self, cls, measure_, *args):
        return [*self.view(), "/pf0/bin/node-launch", "--client", "--class", cls, "--measure", measure_, "--",
                "/pf0/bin/node-launch", *args]

    def client(self, cls, measure_, *args, timeout=120):
        return subprocess.run(self.stub(cls, measure_, *args), stdin=subprocess.DEVNULL, capture_output=True, text=True,
                              timeout=timeout)

    def run_until_stopped(self, *args, measure_="counter", cls="run", ready="run/node/ready", sig=signal.SIGTERM):
        """A spawn the stub stops (as node-bench does at the end of a workload) once the node is ready."""
        p = self.popen(cls, measure_, *args)
        self.wait_for(lambda: os.path.exists(self.path(ready)), "the node's ready file")
        os.kill(self.stub_pid(p), sig)
        out, err = p.communicate(timeout=120)
        return subprocess.CompletedProcess(p.args, p.returncode, out, err)

    def popen(self, cls, measure_, *args):
        return subprocess.Popen(self.stub(cls, measure_, *args), stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE,
                                text=True)

    def stub_pid(self, p):
        return next(d for d in descendants(p.pid) if read(f"/proc/{d}/cmdline", "rb").startswith(
            b"/pf0/bin/node-launch\0--client"))

    def helper(self):
        self.wait_for(lambda: any("helper" in m for m in self.report()), "the helper's registration")
        return next(m["helper"] for m in self.report() if "helper" in m)

    def cgroup(self):
        self.wait_for(lambda: any("cgroup" in m for m in self.report()), "the spawn's cgroup")
        return next(m["cgroup"] for m in self.report() if "cgroup" in m)

    def stopped(self, pred, what):
        """The pid of a stopped process in spawn-0 whose cmdline matches `pred`."""
        found = []

        def look():
            try:
                procs = read(os.path.join(self.cgroup(), "cgroup.procs")).split()
            except FileNotFoundError:
                raise AssertionError(f"spawn-0 ended before {what}") from None
            for pid in procs:
                try:
                    if pred(read(f"/proc/{pid}/cmdline", "rb").split(b"\0")) and "T (stopped)" in read(
                            f"/proc/{pid}/status"):
                        found.append(int(pid))
                        return True
                except OSError:
                    pass
            return False
        self.wait_for(look, what)
        return found[0]

    def wait_for(self, cond, what, timeout=60):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            if cond():
                return
            time.sleep(0.05)
        raise AssertionError(f"timed out waiting for {what}")

    def report(self):
        return [json.loads(line) for line in self.text("report").splitlines() if line.strip()]

    def result(self, k):
        self.wait_for(lambda: os.path.exists(self.path(f"res/spawn-{k}.result.json")), f"spawn-{k}'s result")
        with open(self.path(f"res/spawn-{k}.result.json")) as f:
            return json.load(f)

    def stop(self):
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(60)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()
        subprocess.run(["systemctl", "--user", "stop", f"{self.unit}.scope"], capture_output=True)

    def summary(self):
        self.stop()
        with open(self.path("res/broker.json")) as f:
            return json.load(f)


def named_fds(cmdline):
    """The descriptors a node-launch command line hands on: the values of its fd flags."""
    flags = {b"--into", b"--jail", b"--info", b"--alloc", b"--log-fd"}
    return {int(v) for f, v in zip(cmdline, cmdline[1:], strict=False) if f in flags}


def fds_of(pid):
    return {int(x) for x in os.listdir(f"/proc/{pid}/fd")}


def seen(proc):
    """The fake node's JSON line."""
    return json.loads(proc.stdout.splitlines()[0])


def raw_request(sock_path, payload, nfds=3, fds=None):
    """A fake node-bench's request: `payload` (bytes or an object) with `nfds` descriptors (/dev/null read for 0,
    written for 1 and 2), or exactly `fds`; the broker's reply."""
    s = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
    s.settimeout(60)
    s.connect(sock_path)
    data = payload if isinstance(payload, bytes) else json.dumps(payload).encode()
    with open(os.devnull, "rb") as r, open(os.devnull, "wb") as w:
        send = fds if fds is not None else [r.fileno(), w.fileno(), w.fileno()][:nfds]
        socket.send_fds(s, [data], send) if send else s.send(data)
        try:
            reply = s.recv(65536)
        finally:
            s.close()
    return json.loads(reply) if reply else None


def request(cls="version", measure_="none", argv=("-datadir=/pf0/run/version", "-version"), cwd="/pf0/run"):
    return {"class": cls, "measure": measure_, "cwd": cwd, "argv": list(argv)}


def in_broker(code):
    """Run `code` with the broker module imported as `b`, in a child (importing it makes a process
    non-dumpable); returns what the code printed, as JSON."""
    prog = ("import importlib.util, json, sys\n"
            f"spec = importlib.util.spec_from_file_location('broker', {node.BROKER!r})\n"
            "b = importlib.util.module_from_spec(spec); spec.loader.exec_module(b)\n" + code)
    p = subprocess.run([sys.executable, "-I", "-c", prog], capture_output=True, text=True, timeout=60)
    if p.returncode != 0:
        raise AssertionError(p.stderr)
    return json.loads(p.stdout)


@unittest.skipIf(SKIP, SKIP or "")
class SpawnTest(unittest.TestCase):
    """The chain from the stub's request to the node's exit, and what the node sees of it."""

    def test_a_version_spawn_runs_in_the_hardened_jail(self):
        inv = Invocation(self)
        p = inv.client("version", "none", "-datadir=/pf0/run/version", "-version")
        self.assertEqual(p.returncode, 0, p.stderr)
        n = seen(p)
        self.assertEqual(n["pid"], 1)
        self.assertEqual(n["cpus"], [0, 1])
        self.assertEqual(n["status"]["Cpus_allowed_list"], "0-1")
        self.assertEqual(int(n["personality"], 16) & 0x0040000, 0x0040000, "ADDR_NO_RANDOMIZE")
        self.assertEqual(n["status"]["CapBnd"], "0000000000000000")
        self.assertEqual(n["status"]["CapEff"], "0000000000000000")
        self.assertEqual(n["status"]["NoNewPrivs"], "1")
        self.assertNotEqual(n["nested_userns"], 0, "a nested user namespace was allowed")
        self.assertEqual(n["cgroup"], "0::/", "the jail sees its cgroup as the root")
        self.assertFalse(n["sysfs_cgroup"])
        self.assertEqual(n["mount_cgroup2"], "EPERM")
        self.assertIn(n["proc_sys_write"], ("EROFS", "EACCES", "EPERM"))
        self.assertEqual(n["mempolicy"], [2, 1], "MPOL_BIND to node 0")
        self.assertEqual(n["fds"], [0, 1, 2, 3, 5], "stdio, the listing's own fd and the alloc fd")
        self.assertEqual(n["datadir"], [])
        self.assertEqual(n["cwd"], "/pf0/run/version")
        env = {k: v for k, v in n["env"].items() if k != "LC_CTYPE"}  # python's own C-locale coercion
        self.assertEqual(env, {"LANG": "C", "PATH": "/usr/bin:/bin", "LD_PRELOAD": "/jail/alloc.so",
                                    "PERFGATE_ALLOC_FD": "5", "HOME": "/pf0/run/version",
                                    "TMPDIR": "/pf0/run/version", "PWD": "/pf0/run/version"})
        r = inv.result(0)
        self.assertEqual(r["status"], {"exit": 0})
        self.assertEqual(r["errors"], [])
        self.assertGreater(r["memory_peak"], 0)
        self.assertEqual(r["info"]["child-pid"] > 1, True)
        with open(inv.path("res/spawn-0.alloc.json")) as f:
            self.assertEqual([a["pid"] for a in json.load(f)], [1])
        self.assertFalse(os.path.exists(inv.path("run/version")), "the version datadir outlived its spawn")
        self.assertFalse(os.path.exists(inv.path("res/spawn-0.perf")))

    def test_a_measured_run_is_counted_and_stopped_after_ready(self):
        inv = Invocation(self)
        p = inv.run_until_stopped("-datadir=/pf0/run/node")
        self.assertEqual(p.returncode, 0, p.stderr)
        r = inv.result(0)
        self.assertEqual((r["status"], r["stop_sent"], r["measured"], r["errors"]), ({"exit": 0}, True, True, []))
        perf = node.parse_perf_csv(inv.text("res/spawn-0.perf"))
        self.assertGreater(perf["instructions"], 0)
        self.assertEqual(set(perf), {"instructions", "page-faults"})
        self.assertIsNone(r["death"])
        s = inv.summary()
        self.assertEqual((s["spawns"], s["measured"], s["fatal"]), (1, 1, False))

    def test_a_refusal_reaches_the_stub_as_exit_1(self):
        inv = Invocation(self)
        p = inv.client("preflight", "none", "-datadir=/pf0/run/node", "-refuse")
        self.assertEqual(p.returncode, 1)
        self.assertIn(REFUSAL, p.stderr)
        self.assertEqual(inv.result(0)["status"], {"exit": 1})

    def test_every_node_datadir_starts_empty(self):
        """D46: a version spawn that leaves files, junk node-bench put in RUN, then a measured spawn."""
        inv = Invocation(self)
        self.assertEqual(inv.client("version", "none", "-datadir=/pf0/run/version", "-litter").returncode, 0)
        os.makedirs(inv.path("run/node/blocks"))
        os.symlink("/etc", inv.path("run/node/blocks/link"))
        p = inv.run_until_stopped("-datadir=/pf0/run/node")
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertEqual(seen(p)["datadir"], [])
        self.assertFalse(os.path.exists(inv.path("run/version")))
        self.assertEqual(inv.result(1)["errors"], [])

    def test_an_alloc_flood_is_drained_and_reported(self):
        inv = Invocation(self)
        p = inv.client("version", "none", "-datadir=/pf0/run/version", "-version", "-spam-alloc")
        self.assertEqual(p.returncode, 0, p.stderr)
        r = inv.result(0)
        self.assertTrue(r["alloc_errors"])
        self.assertEqual(r["status"], {"exit": 0})

    def test_a_node_ignoring_sigterm_meets_its_deadline(self):
        inv = Invocation(self, limits={"deadline_run": 8})
        p = inv.run_until_stopped("-datadir=/pf0/run/node", "-hang")
        self.assertNotEqual(p.returncode, 0)
        self.assertIn("deadline_run 8 s", p.stderr)
        r = inv.result(0)
        self.assertEqual(r["kill_reason"], "deadline_run 8 s")
        self.assertIsNone(r["death"], "a kill by the broker is not classified as a death")
        self.assertFalse(os.path.exists(inv.cgroup()))

    def test_a_datadir_swapped_after_validation_is_not_followed(self):
        """Review B1: the node's datadir is bound by descriptor, so replacing RUN/node with a symlink while the
        spawn is starting can't send its writes anywhere else."""
        inv = Invocation(self, hooks={"node_launch_hooks": {"0": {"into": "stop-before-exec"}}})
        p = inv.popen("run", "counter", "-datadir=/pf0/run/node")
        child = inv.stopped(lambda argv: True, "the stopped child")
        os.mkdir(inv.path("run/decoy"))
        os.rename(inv.path("run/node"), inv.path("run/node.orig"))
        os.symlink("decoy", inv.path("run/node"))
        os.kill(child, signal.SIGCONT)
        inv.wait_for(lambda: os.path.exists(inv.path("run/node.orig/ready")), "ready in the original datadir")
        os.kill(inv.stub_pid(p), signal.SIGTERM)
        out, err = p.communicate(timeout=120)
        self.assertEqual(p.returncode, 0, err)
        self.assertEqual(os.listdir(inv.path("run/decoy")), [])
        self.assertEqual(json.loads(out.splitlines()[0])["datadir"], [])
        r = inv.result(0)
        self.assertEqual((r["status"], r["stop_sent"], r["errors"]), ({"exit": 0}, True, []))

    def test_a_chain_that_ends_before_the_jail_ran_is_an_error(self):
        """D81, deterministically (Opus final N2): the jail step says its hello and takes the spec, then exits 0
        without bwrap, so the chain exits 0 with no bwrap info record; a status from the helper alone is never a
        node's run."""
        inv = Invocation(self, hooks={"jail_hello_only": ["0"]})
        p = inv.client("version", "none", "-datadir=/pf0/run/version", "-version")
        self.assertNotEqual(p.returncode, 0, p.stderr)
        r = inv.result(0)
        self.assertEqual(r["status"], {"exit": 0})
        self.assertIn("the chain ended {'exit': 0} before the jail ran (hello: True, bwrap info: False)",
                      r["errors"], r)

    def test_a_vanished_client_kills_its_spawn(self):
        inv = Invocation(self)
        p = inv.popen("run", "counter", "-datadir=/pf0/run/node")
        inv.wait_for(lambda: os.path.exists(inv.path("run/node/ready")), "the node's ready file")
        cg = inv.cgroup()
        os.kill(inv.stub_pid(p), signal.SIGKILL)
        p.communicate(timeout=120)
        r = inv.result(0)
        self.assertEqual(r["kill_reason"], "client closed the connection")
        self.assertEqual(r["status"], {"signal": 9})
        self.assertFalse(os.path.exists(cg))
        self.assertFalse(inv.summary()["fatal"])

    def test_one_spawn_at_a_time_and_one_measured_spawn(self):
        """Review m1."""
        inv = Invocation(self)
        p = inv.popen("run", "counter", "-datadir=/pf0/run/node")
        inv.wait_for(lambda: os.path.exists(inv.path("run/node/ready")), "the node's ready file")
        reply = raw_request(inv.path("ctl/broker"), request())
        self.assertEqual(reply["error"], "broker refused: another spawn is live")
        os.kill(inv.stub_pid(p), signal.SIGTERM)
        p.communicate(timeout=120)
        inv.result(0)
        reply = raw_request(inv.path("ctl/broker"), request(cls="run", measure_="counter",
                                                            argv=["-datadir=/pf0/run/node"]))
        self.assertEqual(reply["error"], "broker refused: one measured spawn per invocation")
        self.assertEqual(inv.client("version", "none", "-datadir=/pf0/run/version", "-version").returncode, 0)

    def test_a_setsid_grandchild_goes_with_its_spawn(self):
        inv = Invocation(self)
        p = inv.client("version", "none", "-datadir=/pf0/run/version", "-orphan")
        self.assertEqual(p.returncode, 0, p.stderr)
        r = inv.result(0)
        self.assertEqual((r["status"], r["errors"]), ({"exit": 0}, []))
        self.assertFalse(os.path.exists(inv.cgroup()))
        self.assertFalse(inv.summary()["fatal"])

    def test_an_alloc_line_flood_keeps_the_result_small(self):
        """Review M5: 64 Ki bad lines give a capped error list, and the result stays valid JSON."""
        inv = Invocation(self)
        p = inv.client("version", "none", "-datadir=/pf0/run/version", "-version", "-spam-lines")
        self.assertEqual(p.returncode, 0, p.stderr)
        r = inv.result(0)
        self.assertEqual(len(r["alloc_errors"]), 21)
        self.assertRegex(r["alloc_errors"][-1], r"^\.\.\. and \d+ more$")
        self.assertLess(os.path.getsize(inv.path("res/spawn-0.result.json")), 16384)

    def test_the_alloc_line_never_lands_in_a_reused_fd_5(self):
        """Review minor 5: a node that puts a datadir file on fd 5 gets no alloc line in it (and so no pid-1
        record)."""
        inv = Invocation(self)
        p = inv.run_until_stopped("-datadir=/pf0/run/node", "-reuse5")
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertEqual(read(inv.path("run/node/data")), "")
        self.assertIn("no alloc record from the jail's pid 1", inv.result(0)["alloc_errors"])

    def test_each_step_inherits_only_the_descriptors_it_names(self):
        """Stopped at entry, node-launch --into holds stdio plus the fds on its command line (its socket, and
        the jail's socket, pipes and, when measured, perf's memfd); node-launch --jail holds stdio plus its own and
        perf's (perf doesn't set CLOEXEC on --log-fd; the jail closes it); the node gets stdio and fd 5."""
        for cls, measure_, datadir in (("version", "none", "/pf0/run/version"), ("run", "counter", "/pf0/run/node")):
            with self.subTest(cls):
                for _ in range(3):
                    if self.held_steps(cls, measure_, datadir):
                        break
                else:
                    self.fail("the measured chain ended before its held jail step three times (D81)")

    def held_steps(self, cls, measure_, datadir):
        """One try; False when the chain ended early with no jail hello (D81, seen about once in 16 measured runs
        under the entry-hold hook), which the broker reports as an error."""
        inv = Invocation(self, hooks={"node_launch_hooks": {"0": {"into": "entry-hold", "jail": "entry-hold"}}})
        args = [f"-datadir={datadir}"] + (["-version"] if cls == "version" else [])
        p = inv.popen(cls, measure_, *args)
        helper = inv.helper()
        inv.wait_for(lambda: "T (stopped)" in read(f"/proc/{helper}/status"), "the held helper")
        argv = read(f"/proc/{helper}/cmdline", "rb").split(b"\0")
        self.assertEqual(fds_of(helper) - {0, 1, 2}, named_fds(argv), argv)
        self.assertEqual(b"--log-fd" in argv, measure_ == "counter")
        os.kill(helper, signal.SIGCONT)
        try:
            jail = inv.stopped(lambda a: b"--jail" in a, "the held jail step")
        except AssertionError:
            p.kill()
            p.communicate()
            r = inv.result(0)
            self.assertIn("before the jail ran", " ".join(r["errors"]), r)
            return False
        argv = read(f"/proc/{jail}/cmdline", "rb").split(b"\0")
        ppid = int(read(f"/proc/{jail}/stat").rsplit(")", 1)[1].split()[1])
        parent = named_fds(read(f"/proc/{ppid}/cmdline", "rb").split(b"\0")) if measure_ == "counter" else set()
        self.assertEqual(fds_of(jail) - {0, 1, 2}, named_fds(argv) | parent, argv)
        os.kill(jail, signal.SIGCONT)
        if cls == "run":
            inv.wait_for(lambda: os.path.exists(inv.path("run/node/ready")), "the node's ready file")
            os.kill(inv.stub_pid(p), signal.SIGTERM)
        out, err = p.communicate(timeout=120)
        self.assertEqual(p.returncode, 0, err)
        self.assertEqual(json.loads(out.splitlines()[0])["fds"], [0, 1, 2, 3, 5])
        return True

@unittest.skipIf(SKIP, SKIP or "")
class PlacementTest(unittest.TestCase):
    """D37, D66: placement is done in the clone3 child, read back, and fails closed."""

    def test_the_child_is_placed_and_stopped_before_exec(self):
        """The stopped child (the test hook) sits in spawn-0, pinned, while node-launch and the broker can't
        be read by their own uid; the deadline then kills it through cgroup.kill."""
        inv = Invocation(self, limits={"deadline_version": 4},
                         hooks={"node_launch_hooks": {"0": {"into": "stop-before-exec"}}})
        p = inv.popen("version", "none", "-datadir=/pf0/run/version", "-version")
        inv.wait_for(lambda: any("helper" in m for m in inv.report()), "the helper's registration")
        cg = next(m["cgroup"] for m in inv.report() if "cgroup" in m)
        helper = next(m["helper"] for m in inv.report() if "helper" in m)
        inv.wait_for(lambda: read(os.path.join(cg, "cgroup.procs")).split(), "the child in spawn-0")
        child = int(read(os.path.join(cg, "cgroup.procs")).split()[0])
        inv.wait_for(lambda: "T (stopped)" in read(f"/proc/{child}/status"), "the stopped child")
        status = read(f"/proc/{child}/status")
        self.assertIn("Cpus_allowed_list:\t0-1\n", status)
        self.assertNotIn(str(helper), read(os.path.join(cg, "cgroup.procs")).split())
        control = subprocess.Popen(["sleep", "30"])
        self.addCleanup(control.wait)
        self.addCleanup(control.kill)
        with open(f"/proc/{control.pid}/environ", "rb") as f:
            f.read()  # the control: an ordinary same-uid process is readable
        for who, pid in (("broker", inv.proc.pid), ("node-launch --into", helper), ("the child", child)):
            with self.subTest(who), self.assertRaises(PermissionError):
                with open(f"/proc/{pid}/environ", "rb") as f:
                    f.read()
        out, err = p.communicate(timeout=60)
        self.assertIn("deadline_version 4 s", err)
        r = inv.result(0)
        self.assertEqual(r["status"], {"signal": 9})
        self.assertFalse(os.path.exists(cg))

    def test_placement_outside_the_allowed_cpus_fails_closed(self):
        inv = Invocation(self, props=("AllowedCPUs=4-5",))
        p = inv.client("version", "none", "-datadir=/pf0/run/version", "-version")
        self.assertNotEqual(p.returncode, 0)
        self.assertIn("placement unavailable", p.stderr)
        self.assertEqual(p.stdout, "", "the node ran")

    def test_placement_ignores_the_callers_mask(self):
        inv = Invocation(self, prefix=("taskset", "-c", "5"))
        p = inv.client("version", "none", "-datadir=/pf0/run/version", "-version")
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertEqual(seen(p)["cpus"], [0, 1])


@unittest.skipIf(SKIP, SKIP or "")
class RequestTest(unittest.TestCase):
    """What a request may name, and that nothing a client sends stalls the broker."""

    @classmethod
    def setUpClass(cls):
        cls.inv = Invocation(types.SimpleNamespace(addCleanup=cls.addClassCleanup), budget={"version": 1, "run": 1})

    def refused(self, payload, nfds=3):
        reply = raw_request(self.inv.path("ctl/broker"), payload, nfds)
        self.assertIn("error", reply)
        self.assertTrue(reply["error"].startswith("broker refused: "), reply)
        return reply["error"]

    def test_requests_outside_the_protocol_are_refused(self):
        cases = {
            "class": request(cls="shell"),
            "syscalls": request(cls="run", measure_="syscalls", argv=["-datadir=/pf0/run/node"]),
            "exactly the run class": request(measure_="counter"),
            "exactly one -datadir": request(argv=["-version"]),
            "exactly one -datadir ": request(argv=["-datadir=/pf0/run/version", "-datadir=/pf0/run/node"]),
            "-datadir /pf0/run/node for class version": request(argv=["-datadir=/pf0/run/node"]),
            "-datadir /pf0/run/version for class run": request(cls="run", measure_="counter",
                                                               argv=["-datadir=/pf0/run/version"]),
            "cwd outside /pf0/run": request(cwd="/pf0/runner"),
            "keys must be": {**request(), "exe": "/bin/sh"},
            "argv must be": {**request(), "argv": "-version"},
            "not JSON": b"{",
            "duplicate keys": b'{"class":"version","class":"run","measure":"none","cwd":"/pf0/run","argv":[]}',
            "-datadir /pf0/prep/x/datadir for class prep": request(cls="prep", argv=["-datadir=/pf0/prep/x/datadir"]),
        }
        for what, payload in cases.items():
            with self.subTest(what):
                self.assertIn(what.strip(), self.refused(payload))
        for what, payload in (("a top-level array", b'[["class","version"]]'), ("nested arrays", b"[[[1],2]]"),
                              ("a triple", b"[[1,2,3]]"), ("a number", b"7")):
            with self.subTest(what):
                self.assertIn("not a JSON object", self.refused(payload))
        with self.subTest("deep nesting"):
            self.assertIn("not JSON", self.refused(b"[" * 60000))
        with self.subTest("an escaped lone surrogate"):
            self.assertIn("valid UTF-8", self.refused(
                json.dumps(request(argv=["-datadir=/pf0/run/version", "\ud800"])).encode()))
        with self.subTest("an encoded surrogate"):
            raw = json.dumps(request(argv=["-datadir=/pf0/run/version", "-x"])).encode().replace(b"-x", b"\xed\xa0\x80")
            self.assertIn("not JSON", self.refused(raw))
        with self.subTest("long client strings are clipped"):
            reply = self.refused(request(cls="x" * 60000))
            self.assertLess(len(reply), 200)
        with self.subTest("--datadir"):
            self.assertIn("exactly one -datadir", self.refused(
                request(argv=["-datadir=/pf0/run/version", "--datadir=/pf0/run/node"])))
        with self.subTest("argv size"):
            self.assertIn("argv over", self.refused(request(argv=["-datadir=/pf0/run/version", "-x" * 3000])))
            self.assertIn("argv over", self.refused(request(argv=["-datadir=/pf0/run/version", *["-x"] * 300])))
        with self.subTest("descriptors"):
            self.assertIn("exactly the stub's stdin", self.refused(request(), nfds=2))
        with tempfile.TemporaryDirectory(dir=_ROOT) as d, open(os.devnull, "rb") as r, open(os.devnull, "wb") as w:
            dirfd = os.open(d, os.O_RDONLY | os.O_DIRECTORY)
            self.addCleanup(os.close, dirfd)
            peer = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
            peer.connect(self.inv.path("ctl/broker"))
            self.addCleanup(peer.close)
            for what, fds, why in (
                    ("a directory", [r.fileno(), dirfd, w.fileno()], "descriptor 1 is not a pipe"),
                    ("a socket to the broker", [r.fileno(), w.fileno(), peer.fileno()], "descriptor 2 is not a pipe"),
                    ("stdin open for writing", [w.fileno(), w.fileno(), w.fileno()],
                     "descriptor 0 is not open for reading")):
                with self.subTest(what):
                    self.assertIn(why, raw_request(self.inv.path("ctl/broker"), request(), fds=fds)["error"])

    def test_a_flood_does_not_stall_the_broker(self):
        socks = []
        for _ in range(200):
            s = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
            s.connect(self.inv.path("ctl/broker"))
            s.setblocking(False)
            try:
                s.send(os.urandom(64) if len(socks) % 2 else b"x" * (65 * 1024))
            except OSError:
                pass
            socks.append(s)
        for s in socks:
            s.close()
        p = self.inv.client("version", "none", "-datadir=/pf0/run/version", "-version")
        self.assertEqual(p.returncode, 0, p.stderr)
        with self.subTest("budget"):
            self.assertIn("no more version spawns", self.refused(request()))


@unittest.skipIf(SKIP, SKIP or "")
class LaunchHandshakeTest(unittest.TestCase):
    def test_a_dumpable_hello_kills_the_spawn(self):
        inv = Invocation(self, hooks={"node_launch_hooks": {"0": {"into": "dumpable"}}})
        p = inv.client("version", "none", "-datadir=/pf0/run/version", "-version")
        self.assertNotEqual(p.returncode, 0)
        self.assertIn('launch handshake: into said {\'hello\': \'into\', \'dumpable\': 1}', p.stderr)
        self.assertEqual(p.stdout, "", "the node ran")
        r = inv.result(0)
        self.assertIsNone(r["status"])
        self.assertEqual(r["answered"], [], "a descriptor went to the dumpable helper")

    def test_no_clone3_is_no_placement(self):
        inv = Invocation(self, hooks={"node_launch_hooks": {"0": {"into": "clone3-einval"}}})
        p = inv.client("version", "none", "-datadir=/pf0/run/version", "-version")
        self.assertNotEqual(p.returncode, 0)
        self.assertIn("atomic cgroup placement unavailable", p.stderr)
        self.assertEqual(p.stdout, "", "the node ran")

    def test_a_helper_that_outlives_its_child_is_killed_and_fatal(self):
        """Review M2: node-launch --into never exits after its child; the deadline kills the spawn, and the
        helper (outside spawn-k) is SIGKILLed deadline_cleanup later, and the invocation is torn down at once:
        the spawn's errors land in the broker's summary (D10); a result exists only if the helper's end was seen
        in the same loop turn."""
        inv = Invocation(self, limits={"deadline_run": 6, "deadline_cleanup": 3},
                         hooks={"node_launch_hooks": {"0": {"into": "hang-after-child"}}})
        p = inv.popen("run", "counter", "-datadir=/pf0/run/node")
        helper = inv.helper()
        out, err = p.communicate(timeout=120)
        self.assertNotEqual(p.returncode, 0)
        s = inv.summary()
        self.assertTrue(s["fatal"])
        self.assertEqual(s["spawn_errors"]["0"][:2], ["deadline_run 6 s",
                                                     "spawn-0 cleanup deadline (the placement helper still runs)"])
        inv.wait_for(lambda: not os.path.exists(f"/proc/{helper}"), "the helper's end")


@unittest.skipIf(SKIP, SKIP or "")
class CleanupTest(unittest.TestCase):
    def test_a_cgroup_that_wont_go_is_fatal(self):
        """Kill-matrix point 7: rmdir keeps failing with EBUSY past deadline_cleanup."""
        inv = Invocation(self, limits={"deadline_cleanup": 2}, hooks={"rmdir_ebusy": 10 ** 6})
        p = inv.client("version", "none", "-datadir=/pf0/run/version", "-version")
        self.assertNotEqual(p.returncode, 0)
        self.assertIn("spawn-0 cleanup deadline (rmdir: Device or resource busy)", p.stderr)
        self.assertIn("spawn-0 cleanup deadline (rmdir: Device or resource busy)", inv.result(0)["errors"])
        self.assertTrue(inv.summary()["fatal"])

    def test_clients_past_the_cap_are_turned_away(self):
        """Review m4: 64 idle connections; the 65th is closed at once, and the broker still serves."""
        inv = Invocation(self)
        held = []
        for _ in range(64):
            s = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
            s.connect(inv.path("ctl/broker"))
            held.append(s)
        extra = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
        extra.settimeout(30)
        deadline = time.monotonic() + 30
        while True:  # EAGAIN while the held connects still fill the listen backlog
            try:
                extra.connect(inv.path("ctl/broker"))
                break
            except BlockingIOError:
                self.assertLess(time.monotonic(), deadline, "the broker never drained its backlog")
                time.sleep(0.1)
        self.assertEqual(extra.recv(16), b"")
        extra.close()
        for s in held:
            s.close()
        self.assertEqual(inv.client("version", "none", "-datadir=/pf0/run/version", "-version").returncode, 0)
        self.assertIn("broker refused: more than 64 open clients", inv.summary()["errors"])


@unittest.skipIf(SKIP, SKIP or "")
class NonDumpableTest(unittest.TestCase):
    """D24-D26(b): each personality, stopped while it holds its material, can't be read by its own uid; the same
    with the prctl skipped can (the control)."""

    def held(self, personality, dumpable):
        hooks = "hold" + (",dumpable" if dumpable else "")
        exe = built(test_hooks=True)[0]
        a, b = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
        env = None
        with tempfile.TemporaryDirectory(dir=_ROOT) as d:
            if personality == "client":
                listener = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
                listener.bind(os.path.join(d, "s"))
                listener.listen(1)
                self.addCleanup(listener.close)
                env = {"PF0_BROKER_SOCKET": os.path.join(d, "s")}
                argv = [exe, "--test-hooks", hooks, "--client", "--class", "version", "--measure", "none", "--",
                        "x", "-datadir=/pf0/run/version"]
            elif personality == "into":
                argv = [exe, "--test-hooks", hooks, "--into", str(b.fileno()), "--cpus", "0", "--node", "0", "--",
                        "true"]
            else:
                argv = [exe, "--test-hooks", hooks, "--jail", str(b.fileno()), "--info", "1", "--alloc", "2",
                        "--bwrap", "/bin/false"]
            p = subprocess.Popen(argv, pass_fds=(b.fileno(),), env=env)
            self.addCleanup(p.wait)
            self.addCleanup(p.kill)
            b.close()
            if personality == "client":
                conn, _ = listener.accept()
                self.addCleanup(conn.close)
                conn.recv(65536)
            elif personality == "into":
                a.recv(256)
                fd = os.open(d, os.O_RDONLY | os.O_DIRECTORY)
                socket.send_fds(a, [b'{"cgroup":1}'], [fd])
                os.close(fd)
            else:
                a.recv(256)
                fds = [os.memfd_create("spec", os.MFD_ALLOW_SEALING) for _ in range(2)]
                for fd in fds:
                    os.write(fd, b"x\0")
                    fcntl.fcntl(fd, fcntl.F_ADD_SEALS, fcntl.F_SEAL_SEAL | fcntl.F_SEAL_SHRINK | fcntl.F_SEAL_GROW |
                                fcntl.F_SEAL_WRITE)
                fds.append(os.open(d, os.O_RDONLY | os.O_DIRECTORY))
                socket.send_fds(a, [b'{"spec":3}'], fds)
                for fd in fds:
                    os.close(fd)
            deadline = time.monotonic() + 60
            while "T (stopped)" not in read(f"/proc/{p.pid}/status"):
                self.assertLess(time.monotonic(), deadline, "the hold hook never stopped it")
                time.sleep(0.05)
            out = {}
            for what in ("environ", "mem", "root", "exe", "fd"):
                path = f"/proc/{p.pid}/{what}"
                try:
                    if what in ("root", "fd"):
                        os.listdir(path)
                        if what == "fd":
                            for n in os.listdir(path):
                                os.readlink(f"{path}/{n}")
                    elif what == "exe":
                        os.readlink(path)
                    else:
                        read(path, "rb") if what == "environ" else os.close(os.open(path, os.O_RDONLY))
                    out[what] = "ok"
                except PermissionError:
                    out[what] = "EACCES"
            a.close()
            return out

    def test_every_personality_is_unreadable_while_it_holds_its_material(self):
        for personality in ("into", "jail", "client"):
            with self.subTest(personality):
                held = self.held(personality, dumpable=False)
                fd = held.pop("fd")  # listing the numbers is allowed by kernel rule; their readlinks are not
                self.assertEqual(set(held.values()), {"EACCES"}, held)
                self.assertEqual(fd, "EACCES")
            with self.subTest(personality + " control"):
                self.assertEqual(set(self.held(personality, dumpable=True).values()), {"ok"})


class BrokerUnitTest(unittest.TestCase):
    def test_the_oom_classifier(self):
        """D74 with injected counters: memcg OOM, a kernel OOM outside the memcg (NUMA bind), plain SIGKILL."""
        out = in_broker(
            "c = b.classify_death\n"
            "print(json.dumps([\n"
            "  c({'signal': 9}, {'oom_kill': 1}, 5, 6, 1 << 30, 0, 100),\n"
            "  c({'exit': 137}, {'oom_kill': 0}, 5, 6, 1 << 30, 0, 100),\n"
            "  c({'signal': 9}, {}, 5, 5, 1 << 30, 0, 100),\n"
            "  c({'exit': 0}, {}, 5, 6, 1 << 30, 0, 100),\n"
            "  c({'signal': 15}, {}, 5, 6, 1 << 30, 0, 100),\n"
            "]))\n")
        self.assertEqual(out, ["spawn hit memory.max 1024 MiB (oom_kill 1)",
                               "killed by a kernel OOM outside the memcg (node 0 MPOL_BIND; node0 MemFree 100 kB)",
                               "death by signal 9", None, None])

    CLEANUP = (
        "import os, selectors\n"
        "br = b.Broker.__new__(b.Broker)\n"
        "br.cfg, br.fatal, br.finished, br.hooks = {'limits': {'deadline_cleanup': 1}}, False, [], b.NoHooks()\n"
        "br.sel = selectors.DefaultSelector()\n"
        "br.finish = lambda sp: br.finished.append(sp.k)\n"
        "sp = b.Spawn(0, None, 'run', True, [], '/pf0/run/node')\n"
        "sp.cg, sp.finishing, sp.killed_at = CG, True, 0\n"
        "br.spawns = {0: sp}\n"
        "r, w = os.pipe2(os.O_NONBLOCK)  # a writer that outlived the chain\n"
        "sp.info = r\n"
        "br.sel.register(r, selectors.EVENT_READ)\n"
        "br.cleanup_step(sp)\n"
        "print(json.dumps([br.fatal, sp.errors.doc(), br.finished, sp.info]))\n")

    def test_the_cleanup_deadline_is_fatal_and_waits_for_nothing(self):
        """D10, review M1: procs left past deadline_cleanup → fatal, and the pipes are closed, not drained (a
        survivor holds a writer)."""
        with tempfile.TemporaryDirectory(dir=os.environ.get("PERFGATE_TEST_DIR")) as d:
            os.mkdir(os.path.join(d, "spawn-0"))
            with open(os.path.join(d, "spawn-0", "cgroup.procs"), "w") as f:
                f.write("12345\n")
            fatal, errors, finished, info = in_broker(f"CG = {os.path.join(d, 'spawn-0')!r}\n" + self.CLEANUP)
        self.assertTrue(fatal)
        self.assertIn("spawn-0 cleanup deadline (1 procs left)", errors)
        self.assertEqual((finished, info), ([0], None))

    def test_a_writer_left_after_a_clean_rmdir_is_not_waited_for(self):
        with tempfile.TemporaryDirectory(dir=os.environ.get("PERFGATE_TEST_DIR")) as d:
            os.mkdir(os.path.join(d, "spawn-0"))
            fatal, errors, finished, info = in_broker(f"CG = {os.path.join(d, 'spawn-0')!r}\n" + self.CLEANUP)
        self.assertEqual((fatal, errors, finished, info), (False, [], [0], None))

    def test_results_stay_valid_json_within_their_caps(self):
        """Review M5: bounded lists, compact output, and an over-cap document replaced by a small error."""
        with tempfile.TemporaryDirectory(dir=os.environ.get("PERFGATE_TEST_DIR")) as d:
            out = in_broker(
                "br = b.Broker.__new__(b.Broker)\n"
                f"br.res = {d!r}\n"
                "e = b.Bounded()\n"
                "for i in range(1000): e.append(f'error {i}')\n"
                "br.write_res('a.json', {'errors': e.doc()}, 16384)\n"
                "br.write_res('b.json', {'x': 'y' * 20000}, 16384)\n"
                f"print(json.dumps([json.load(open({d!r} + '/' + n)) for n in ('a.json', 'b.json')]))\n")
        a, b = out
        self.assertEqual(len(a["errors"]), 21)
        self.assertEqual(a["errors"][-1], "... and 980 more")
        self.assertEqual(b, {"error": "b.json over its 16384 B cap (20008 B)"})

    def test_a_deep_datadir_is_refused_not_a_crash(self):
        """Review major 1(c): the tree walks are iterative and capped."""
        with tempfile.TemporaryDirectory(dir=os.environ.get("PERFGATE_TEST_DIR")) as d:
            os.makedirs(os.path.join(d, "src", *["d"] * 70))
            os.makedirs(os.path.join(d, "ok", *["d"] * 10, "x"))
            open(os.path.join(d, "ok", "f"), "w").close()
            os.mkdir(os.path.join(d, "dst"))
            out = in_broker(
                f"import os\nd = {d!r}\n"
                "fd = lambda p: os.open(p, b.DIR_FLAGS)\n"
                "out = []\n"
                "for what in ('checked', 'copied'):\n"
                "    try:\n"
                "        b.checked_tree(fd(d + '/src')) if what == 'checked' else b.copy_tree(fd(d + '/src'), fd(d + '/dst'))\n"
                "        out.append('accepted')\n"
                "    except b.Refused as e:\n"
                "        out.append(str(e))\n"
                "os.mkdir(d + '/dst2')\n"
                "b.copy_tree(fd(d + '/ok'), fd(d + '/dst2'))\n"
                "out.append(b.checked_tree(fd(d + '/dst2')) == b.checked_tree(fd(d + '/ok')))\n"
                "print(json.dumps(out))\n")
        self.assertEqual(out, ["a datadir nested deeper than 64 levels"] * 2 + [True])

    def test_a_writer_that_keeps_writing_does_not_hold_cleanup(self):
        """Review minor 2: a live writer (a copy of the alloc fd outside spawn-k) gets a bounded number of reads."""
        with tempfile.TemporaryDirectory(dir=os.environ.get("PERFGATE_TEST_DIR")) as d:
            os.mkdir(os.path.join(d, "spawn-0"))
            out = in_broker(
                f"CG = {os.path.join(d, 'spawn-0')!r}\n"
                "import os, selectors, threading\n"
                "br = b.Broker.__new__(b.Broker)\n"
                "br.cfg, br.fatal, br.finished, br.hooks = {'limits': {'deadline_cleanup': 60}}, False, [], b.NoHooks()\n"
                "br.sel = selectors.DefaultSelector()\n"
                "br.finish = lambda sp: br.finished.append(sp.k)\n"
                "sp = b.Spawn(0, None, 'run', True, [], '/pf0/run/node')\n"
                "sp.cg, sp.finishing, sp.killed_at = CG, True, 0\n"
                "br.spawns = {0: sp}\n"
                "r, w = os.pipe2(os.O_NONBLOCK)\n"
                "os.set_blocking(w, True)\n"
                "threading.Thread(target=lambda: [os.write(w, b'x' * 4096) for _ in iter(int, 1)], daemon=True).start()\n"
                "sp.alloc = r\n"
                "br.sel.register(r, selectors.EVENT_READ)\n"
                "br.cleanup_step(sp)\n"
                "print(json.dumps([br.finished, sp.alloc]), flush=True)\n"
                "os._exit(0)\n")
        self.assertEqual(out, [[0], None])

    def test_a_jail_gone_before_its_info_record_is_read_is_no_error(self):
        """D103: under load bwrap can reap a quick jail (a refusal, -version) before the broker reads its pid: that
        pid is then gone, or (reused) a process that has exited too. Only a live process outside spawn-k is a kill."""
        out = in_broker(
            "import os, selectors, subprocess, time\n"
            "br = b.Broker.__new__(b.Broker)\n"
            "br.report, br.sel = None, selectors.DefaultSelector()\n"
            "reaped = subprocess.Popen(['true']); reaped.wait()\n"
            "zombie = subprocess.Popen(['true'])\n"
            "while open(f'/proc/{zombie.pid}/stat').read().split(') ')[1][0] != 'Z': time.sleep(0.01)\n"
            "live = subprocess.Popen(['sleep', '60'])\n"
            "out = {}\n"
            "for name, pid in (('reaped', reaped.pid), ('zombie', zombie.pid), ('live', live.pid)):\n"
            "    sp = b.Spawn(0, None, 'preflight', False, [], '/pf0/run/node')\n"
            "    br.spawns = {0: sp}\n"
            "    r, w = os.pipe2(os.O_NONBLOCK)\n"
            "    os.write(w, json.dumps({'child-pid': pid}).encode()); os.close(w)\n"
            "    sp.info = r\n"
            "    br.info_ready(0)\n"
            "    out[name] = [sp.info_record == {'child-pid': pid}, sp.node_pidfd is None, sp.kill_reason]\n"
            "live.kill(); live.wait(); zombie.wait()\n"
            "print(json.dumps(out))\n")
        self.assertEqual(out["reaped"], [True, True, None])
        self.assertEqual(out["zombie"], [True, True, None])
        self.assertEqual(out["live"][:2], [True, False])
        self.assertRegex(out["live"][2], r"^the jail's pid \d+ is not in spawn-0 \(0::/")

    def test_an_over_cap_summary_keeps_its_verdict(self):
        """Review minor 4: the error lists go first; fatal and the counts stay."""
        with tempfile.TemporaryDirectory(dir=os.environ.get("PERFGATE_TEST_DIR")) as d:
            out = in_broker(
                "br = b.Broker.__new__(b.Broker)\n"
                f"br.res = {d!r}\n"
                "br.write_res('broker.json', {'spawns': 3, 'errors': ['e' * 900] * 20, 'fatal': True}, 16384)\n"
                f"print(open({d!r} + '/broker.json').read())\n")
        self.assertEqual(out, {"spawns": 3, "errors": ["... dropped: over the cap"], "fatal": True})

    def test_a_production_config_with_test_settings_is_refused(self):
        """Review minor 3 (D9): only tests/broker_hooked.py can pass hooks."""
        with tempfile.TemporaryDirectory(dir=os.environ.get("PERFGATE_TEST_DIR")) as d:
            for key in ("hooks", "test"):
                with open(os.path.join(d, f"{key}.json"), "w") as f:
                    json.dump({"cpu_list": "0", key: {}}, f)
                p = subprocess.run([sys.executable, "-I", "-B", node.BROKER, "--config", os.path.join(d, f"{key}.json"),
                                    "--report", "1"], capture_output=True, text=True, timeout=60)
                self.assertEqual((p.returncode, p.stderr.strip()), (1, "broker: test settings in a config (D9)"))

    def test_the_jail_spec_names_only_config(self):
        """The request's argv lands after the exe in the command, never among bwrap's options."""
        out = in_broker(
            "cfg = {'jail_env': {'PATH': '/usr/bin'}, 'jail_etc': [], 'exe': '/x/rdashd', 'preload': '/x/a.so',\n"
            "       'setarch': '/usr/bin/setarch', 'arch': 'x86_64'}\n"
            "print(json.dumps(b.jail_args(cfg, '/pf0/run/node', ['--bind', '/', '/x'])))\n")
        opts, cmd = out
        self.assertEqual(cmd, ["/usr/bin/setarch", "x86_64", "-R", "/jail/rdashd", "--bind", "/", "/x"])
        self.assertNotIn(["--bind", "/", "/x"], [opts[i:i + 3] for i in range(len(opts))])
        for flag in ("--cap-drop", "--disable-userns", "--unshare-cgroup", "--as-pid-1", "--clearenv"):
            self.assertIn(flag, opts)
        self.assertIn(["--bind-fd", "8", "/pf0/run/node"], [opts[i:i + 3] for i in range(len(opts))])
        self.assertNotIn("--bind", opts, "the datadir is bound by descriptor only (D80)")


class BenchPathPinTest(unittest.TestCase):
    """D44, D45, D70: the node code leaves the bench rows' counting path and its hash alone."""

    def test_launch_c_is_byte_identical(self):
        with open(sandbox.LAUNCH_SRC, "rb") as f:
            self.assertEqual(hashlib.sha256(f.read()).hexdigest(), LAUNCH_C_SHA256)

    def test_counter_hash_is_pinned_apart_from_the_host(self):
        constant = [measure._COUNTING_SOURCES, sandbox.CC_FLAGS, measure.PERF_COUNTER, measure.STRACE_COUNTER,
                    sorted(measure.SCHED_SYSCALLS), sorted(measure.FS_SYSCALLS), measure.INTROSPECTION.pattern,
                    measure.SYSCALL_LINE.pattern, measure.MARKER.pattern]
        self.assertEqual(hashlib.sha256(json.dumps(constant).encode()).hexdigest(), COUNTING_CONSTANTS_SHA256)
        host = [len(sandbox.readonly_binds()),
                *(measure._version(*c) for c in (("perf", "--version"), ("strace", "-V"), ("bwrap", "--version"),
                                                 ("taskset", "--version")))]
        measure.counter_hash.cache_clear()
        self.addCleanup(measure.counter_hash.cache_clear)
        self.assertEqual(measure.counter_hash(), hashlib.sha256(json.dumps([*constant, *host]).encode()).hexdigest()[:16])

    def test_the_bench_launcher_build_key_is_unchanged(self):
        """`compiled()` keys the bench launcher by launch.c, CC_FLAGS, no flags or libs and the compiler, as at 1b."""
        cc = subprocess.run(["cc", "--version"], capture_output=True, text=True).stdout
        with open(sandbox.LAUNCH_SRC, "rb") as f:
            h = hashlib.sha256(f.read() + repr((("-O2", "-Wall"), (), (), cc)).encode()).hexdigest()[:12]
        with tempfile.TemporaryDirectory(dir=os.environ.get("PERFGATE_TEST_DIR")) as root, \
                mock.patch.dict(os.environ, {"PERFGATE_ROOT": root}):
            self.assertEqual(os.path.basename(sandbox.launcher()), f"perfgate_launch-{h}")
            self.assertTrue(os.path.basename(node.node_launch()).startswith("perfgate_node_launch-"))


if __name__ == "__main__":
    unittest.main()
