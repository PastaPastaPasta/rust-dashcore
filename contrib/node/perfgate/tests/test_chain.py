"""Tests for the gate holder and the bounded invocation layer (plan rev4.1 §2, §3, §6, §11; D13, D40, D50, D53,
D56–D58, D64, D67, D75, D80, D82).

Every chain is the production one (holder → scope → nsenter → unshare → outer_init → broker and the view) with a
fake rdashd as the side's exe; node-bench's place is taken by a shell in the real view that runs the stub. Units
are `pfnt-<rootid>-` (D54), so a test run never touches a gate's.

    python3 -m unittest discover -s contrib/node/perfgate/tests
"""

import contextlib
import hashlib
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.realpath(__file__))))

from perfgate import invocation as iv  # noqa: E402
from perfgate import node  # noqa: E402

HERE = os.path.dirname(os.path.realpath(__file__))
FAKE_NODE = os.path.join(HERE, "fake_rdashd.py")
FAKE_HOLDER = os.path.join(HERE, "fake_holder.py")
HOOKED = os.path.join(HERE, "broker_hooked.py")
FAST = {"deadline_version": 60, "deadline_preflight": 60, "deadline_start": 60, "deadline_prep": 60,
        "deadline_run": 60, "deadline_cleanup": 30}


def missing():
    """Why this host can't run a chain, or None."""
    for tool in ("bwrap", "perf", "systemd-run", "setarch", "cc", "unshare", "nsenter", "setpriv"):
        if not shutil.which(tool):
            return f"no {tool}"
    p = subprocess.run(["systemd-run", "--user", "--scope", "-q", "-p", "Delegate=yes", "--", "true"],
                       capture_output=True)
    if p.returncode != 0:
        return "no delegated user scopes"
    if subprocess.run(["unshare", "--user", "--map-root-user", "--mount", "true"], capture_output=True).returncode:
        return "no unprivileged user namespaces"
    if not os.path.isdir("/sys/devices/system/node/node0") or not {0, 1} <= os.sched_getaffinity(0):
        return "CPUs 0-1 or NUMA node 0 unavailable"
    return None


SKIP = missing()
_ROOT = None
_ENV = None


def setUpModule():
    global _ROOT, _ENV
    if SKIP:
        return
    _ROOT = tempfile.mkdtemp(prefix="chain-", dir=os.environ.get("PERFGATE_TEST_DIR"))
    os.chmod(_ROOT, 0o700)
    _ENV = mock.patch.dict(os.environ, {"PERFGATE_ROOT": _ROOT})
    _ENV.start()
    node.node_launch(), node.node_launch(test_hooks=True), node.preload_lib()  # built before any snapshot


def live_units(prefix):
    out = subprocess.run(["systemctl", "--user", "list-units", "--all", "--no-legend", "--plain", f"{prefix}*"],
                         capture_output=True, text=True).stdout
    return [line.split()[0] for line in out.splitlines() if line.strip()]


def tearDownModule():
    if not _ROOT:
        return
    prefix = iv.unit_prefix(test=True, root=_ROOT)
    end = time.monotonic() + 120  # D51: poll, never a short upper bound
    while live_units(prefix) and time.monotonic() < end:
        time.sleep(0.5)
    left = live_units(prefix)
    _ENV.stop()
    shutil.rmtree(_ROOT, ignore_errors=True)
    if left:
        raise AssertionError(f"test units left behind: {left}")


def stub(cls, measure, *argv):
    return " ".join(["/pf0/bin/node-launch", "--client", "--class", cls, "--measure", measure, "--", "x", *argv])


def sh(script):
    return ["/bin/sh", "-c", script]


def tree_state(top, skip=()):
    """{relative path: (sha256, mtime_ns)} of every regular file under top, but under the `skip` dirs."""
    out = {}
    for d, dirs, files in os.walk(top):
        dirs[:] = [x for x in dirs if os.path.join(d, x) not in skip]
        for name in files:
            p = os.path.join(d, name)
            st = os.lstat(p)
            with open(p, "rb") as f:
                out[os.path.relpath(p, top)] = (hashlib.sha256(f.read()).hexdigest(), st.st_mtime_ns)
    return out


def jsonl(text):
    return [json.loads(line) for line in text.splitlines() if line.startswith("{")]


class Gate:
    """One holder (a gate) and its invocations."""

    n = 0

    def __init__(self, cleanup, timeout=900, unshare_argv=None):
        Gate.n += 1
        self.prefix = iv.unit_prefix(test=True, root=_ROOT)
        self.dir = os.path.join(_ROOT, "node", f"g-test-{os.getpid()}-{Gate.n}")
        os.makedirs(self.dir, 0o700)
        self.tl = iv.tools()
        self.holder = iv.Holder(self.dir, f"{self.prefix}holder-{Gate.n}", {"head": FAKE_NODE}, timeout, self.tl,
                                unshare_argv=unshare_argv).start()
        cleanup(self.holder.stop)
        self.invs = 0

    def invocation(self, cmd, hooks=None, limits=None, **kw):
        self.invs += 1
        n = self.invs
        unit = f"{self.prefix}inv-{Gate.n}-{n}"
        if hooks is not None:
            tj = os.path.join(self.dir, f"test-{n}.json")
            with open(tj, "w") as f:
                json.dump(hooks, f)
            bcfg = os.path.join(self.dir, "inv", f"head-{n}", "cfg", "broker.json")
            kw["outer_extra"] = {**kw.get("outer_extra", {}), "broker": [
                iv.INTERPRETER, "-I", "-B", HOOKED, "--broker", node.BROKER, "--test", tj, "--",
                "--config", bcfg, "--report", "{report}"]}
            kw.setdefault("node_launch", node.node_launch(test_hooks=True))
        return iv.Invocation(self.holder, "head", n, unit, limits={**FAST, **(limits or {})}, view_cmd=cmd, **kw)

    def run(self, cmd, **kw):
        return self.invocation(cmd, **kw).run()


@unittest.skipIf(SKIP, SKIP or "")
class ChainTest(unittest.TestCase):
    """The whole chain, once per behaviour."""

    @classmethod
    def setUpClass(cls):
        cls.gate = Gate(cls.addClassCleanup)

    def test_version_then_measured_run_counted(self):
        r = self.gate.run(sh(f"{stub('version', 'none', '-datadir=/pf0/run/version', '-version')} && "
                             f"{stub('run', 'counter', '-datadir=/pf0/run/node', '-orphan')}"),
                          budget={"version": 1, "run": 1})
        self.assertEqual((r.rc, r.errors()), (0, []), r.text("view.err"))
        self.assertEqual(r.outer["view"], {"exit": 0})
        self.assertGreater(r.outer["census_scans"], 0)
        self.assertEqual([r.spawn(k)["status"] for k in (0, 1)], [{"exit": 0}, {"exit": 0}])
        with open(os.path.join(r.res, "spawn-1.perf")) as f:
            self.assertGreater(node.parse_perf_csv(f.read())["instructions"], 0)
        seen = jsonl(r.text("view.out"))
        self.assertEqual(seen[1]["datadir"], [], "a measured spawn starts on an empty datadir (D46)")

    def test_own_tmpfs_superblocks(self):
        """D53 (a)–(c): RUN, CTL, GATE and PREP are tmpfs superblocks of their own; the view binds nothing of host
        /tmp or /dev/shm; nothing with the test's marker lands there."""
        marker = f"pf0-marker-{os.getpid()}-{time.monotonic_ns()}"
        host = {os.stat(p).st_dev for p in ("/tmp", "/dev/shm")}
        prep = self.gate.holder.mount_prep("head", "d53", 8 << 20, 1000)
        inv = self.gate.invocation(sh(f"stat -f -c '%T' /pf0/run /pf0/ctl; stat -c '%d' /pf0/run /pf0/ctl; "
                                      f"touch /pf0/run/{marker}; {stub('version', 'none', '-datadir=/pf0/run/version', '-version')}"))
        r = inv.run()
        self.assertEqual((r.rc, r.errors()), (0, []), r.text("view.err"))
        lines = r.text("view.out").splitlines()
        self.assertEqual(lines[:2], ["tmpfs", "tmpfs"])
        devs = {int(x) for x in lines[2:4]}
        self.assertEqual(len(devs), 2, "RUN and CTL are two superblocks")
        p = self.gate.holder.run_in(["stat", "-f", "-c", "%T", self.gate.holder.gate, prep])
        self.assertEqual(p.stdout.split(), ["tmpfs", "tmpfs"], p.stderr)
        p = self.gate.holder.run_in(["stat", "-c", "%d", self.gate.holder.gate, prep])
        devs |= {int(x) for x in p.stdout.split()}
        self.assertEqual(len(devs), 4)
        self.assertFalse(devs & host, "a node tmpfs on host /tmp's or /dev/shm's superblock")
        with open(inv.outer_cfg) as f:
            view = json.load(f)["view"]
        sources = [view[i + 1] for i, a in enumerate(view) if a in ("--bind", "--ro-bind")]
        self.assertFalse([s for s in sources if s.startswith(("/tmp", "/dev/shm"))])
        for top in ("/tmp", "/dev/shm"):
            found = subprocess.run(["find", top, "-name", marker], capture_output=True, text=True).stdout
            self.assertEqual(found, "")

    def test_host_read_only_outside_res(self):
        """D80: outer_init makes the host read-only first; only res (and the mounts it makes) stay writable."""
        probe = os.path.join(self.gate.dir, "host-write")
        code = ("import os, sys\n"
                "for p in sys.argv[1:]:\n"
                "    try:\n"
                "        open(p, 'w').close(); print(p, 'ok')\n"
                "    except OSError as e:\n"
                "        print(p, e.errno)\n")
        inv = self.gate.invocation([])
        res_file = os.path.join(inv.res, "own")
        with open(inv.outer_cfg) as f:
            outer = json.load(f)
        outer["view"] = [iv.INTERPRETER, "-I", "-c", code, probe, res_file, "/tmp/pf0-chain-ro",
                         os.path.join(os.path.expanduser("~"), ".pf0-chain-ro")]
        os.unlink(inv.outer_cfg)
        iv._write_json(inv.outer_cfg, outer)
        r = inv.run()
        out = dict(line.rsplit(" ", 1) for line in r.text("view.out").splitlines())
        self.assertEqual(out, {probe: "30", res_file: "ok", "/tmp/pf0-chain-ro": "30",
                               outer["view"][-1]: "30"})  # EROFS
        self.assertFalse(os.path.exists(probe))

    def test_run_full_then_prep_full(self):
        """§11 N3: a full RUN, then a full PREP: each an ERROR naming the limit, and nothing outside the gate's own
        dir changes on the host."""
        sibling = os.path.join(_ROOT, "node", "g-sibling")
        os.makedirs(sibling, 0o700, exist_ok=True)
        with open(os.path.join(sibling, "keep"), "w") as f:
            f.write("sibling")
        before = tree_state(_ROOT, skip=(self.gate.dir,))
        r = self.gate.run(sh(stub("run", "counter", "-datadir=/pf0/run/node", "-fill")),
                          limits={"tmpfs": 8 << 20, "tmpfs_inodes": 1000})
        self.assertIn("spawn-0: RUN tmpfs full (8M / 1k inodes)", r.errors())
        self.assertNotEqual(r.rc, 0)
        prep = self.gate.holder.mount_prep("head", "full", 8 << 20, 1000)
        r = self.gate.run(sh("mkdir /pf0/prep/full/datadir && "
                             + stub("prep", "none", "-datadir=/pf0/prep/full/datadir", "-fill")),
                          budget={"prep": 1}, prep=("full", prep, True))
        self.assertIn("spawn-0: prep tmpfs full (8M / 1k inodes)", r.errors())
        self.assertEqual(tree_state(_ROOT, skip=(self.gate.dir,)), before)
        res_bytes = sum(os.path.getsize(os.path.join(d, f)) for d, _, fs in os.walk(os.path.join(self.gate.dir, "inv"))
                        for f in fs)
        self.assertLess(res_bytes, 16 << 20, "res/ stays small though RUN and PREP filled up")

    def test_prep_then_read_only_copy(self):
        """D11, D46, D60: the prep spawn writes PREP; after the read-only remount nothing writes it, and a
        measured spawn's datadir is a copy of it."""
        prep = self.gate.holder.mount_prep("head", "r1", 8 << 20, 1000)
        r = self.gate.run(sh("mkdir /pf0/prep/r1/datadir && echo master > /pf0/prep/r1/datadir/blocks && "
                             + stub("prep", "none", "-datadir=/pf0/prep/r1/datadir", "-litter")),
                          budget={"prep": 1}, prep=("r1", prep, True))
        self.assertEqual((r.rc, r.errors()), (0, []), r.text("view.err"))
        self.gate.holder.prep_readonly(prep)
        r = self.gate.run(sh("touch /pf0/prep/r1/x 2>/dev/null; echo view-write=$?; "
                             + stub("run", "counter", "-datadir=/pf0/run/node", "-orphan")),
                          prep=("r1", prep, False))
        self.assertEqual((r.rc, r.errors()), (0, []), r.text("view.err"))
        lines = r.text("view.out").splitlines()
        self.assertEqual(lines[0], "view-write=1")
        self.assertEqual(jsonl(r.text("view.out"))[0]["datadir"], ["blocks", "settings.json"])
        p = self.gate.holder.run_in(["touch", os.path.join(prep, "y")])
        self.assertNotEqual(p.returncode, 0)
        self.assertIn("Read-only file system", p.stderr)

    def test_census_rejects_strays(self):
        """D40, D64: a process of the outer pid ns that is neither registered nor a chain member is an ERROR, by
        identity: a second python3 passes an exe check, a broker child outlives the hello window."""
        for bait, names in (("sleep", ("sleep", "python3")), ("python3", ("python3",)), ("child", ("python3",))):
            with self.subTest(bait=bait):
                r = self.gate.run(sh("sleep 20"), hooks={"bait": bait}, census_hello_s=8)
                found = [e for e in r.errors() if e.startswith("census: unexpected process")]
                self.assertTrue(found, r.errors())
                # comm or exe; a sleep bait may be caught as the hook's python3 fork before its exec
                self.assertTrue(any(n in found[0] for n in names), found)
                self.assertNotEqual(r.rc, 0)

    def test_view_sees_no_outer_process(self):
        """D50: from inside the view no process has the broker's or node-launch's (starttime, comm); the hooked
        broker carries a test-token comm, so a shared starttime tick can't collide (D72)."""
        code = ("import os\n"
                "for d in os.listdir('/proc'):\n"
                "    if d.isdigit():\n"
                "        try:\n"
                "            s = open(f'/proc/{d}/stat').read()\n"
                "        except OSError:\n"
                "            continue\n"
                "        print(s[s.index('(') + 1:s.rindex(')')], s.rsplit(')', 1)[1].split()[19], d)\n")
        live = (f"{stub('run', 'counter', '-datadir=/pf0/run/node')} >/dev/null & s=$!; "
                "while [ ! -e /pf0/run/node/ready ]; do sleep 0.05; done; "
                f"/usr/bin/python3 -I -c \"$PF0_CODE\"; echo stub $s; kill -TERM $s; wait $s")
        r = self.gate.run(sh(live), view_env={"PF0_CODE": code}, hooks={"comm": "pf0tok-broker"})
        self.assertEqual((r.rc, r.errors()), (0, []), r.text("view.err"))
        rows = [line.split() for line in r.text("view.out").splitlines()]
        stub_pid = next(x[1] for x in rows if x[0] == "stub")
        procs = [(comm, pid) for comm, _, pid in (x for x in rows if x[0] != "stub")]
        comms = [c for c, _ in procs]
        self.assertTrue(comms)
        self.assertNotIn("pf0tok-broker", comms)
        # a spawn is live while /proc is listed: of its chain only the view's own stub (node-launch --client) is
        # visible, not the helper, perf or the jail's bwrap (the one bwrap is the view's own pid 1)
        self.assertEqual([p for c, p in procs if "node-launch" in c], [stub_pid])
        self.assertFalse([c for c in comms if "perfgate_node" in c or c == "perf"], comms)
        self.assertLessEqual(comms.count("bwrap"), 1, comms)

    def test_view_streams_capped(self):
        """§6, D6: node-bench and, through the stub's inherited stdout, the node write the view's streams; each is a
        pipe outer_init keeps 256 KiB of and counts the rest, so res stays small."""
        r = self.gate.run(sh("head -c 67108864 /dev/zero; head -c 33554432 /dev/zero >&2; "
                             + stub("run", "counter", "-datadir=/pf0/run/node", "-orphan", "-spew-mib=32")))
        self.assertEqual((r.rc, r.errors()), (0, []), r.text("view.err")[-500:])
        cap = 256 * 1024
        self.assertEqual([os.path.getsize(os.path.join(r.res, n)) for n in ("view.out", "view.err")], [cap, cap])
        dropped = r.outer["view_log_dropped"]
        self.assertGreaterEqual(dropped["view.out"], (96 << 20) - cap)
        self.assertEqual(dropped["view.err"], (32 << 20) - cap)

    def test_stdio_file_off_run_refused(self):
        """§6: a regular file goes to the jail only when it is on RUN (or PREP); the view's own /tmp is not."""
        r = self.gate.run(sh(f"{stub('version', 'none', '-datadir=/pf0/run/version', '-version')} >/tmp/o; "
                             "echo tmp=$?; "
                             f"{stub('version', 'none', '-datadir=/pf0/run/version', '-version')} >/pf0/run/o; "
                             "echo run=$?"), budget={"version": 2})
        self.assertEqual(r.text("view.out").splitlines()[-2:], ["tmp=125", "run=0"], r.text("view.err"))
        self.assertIn("descriptor 1 is a regular file outside RUN and PREP", r.text("view.err"))

    def test_spawn_memory_and_tasks_bounds(self):
        """§6: a spawn's own memory.max and pids.max hold inside the production chain."""
        r = self.gate.run(sh(stub("run", "counter", "-datadir=/pf0/run/node", "-alloc-mib=512")),
                          limits={"mem_spawn": 64 << 20})
        self.assertIn("spawn hit memory.max 64 MiB", r.spawn(0)["death"] or "", r.spawn(0))
        r = self.gate.run(sh(stub("run", "counter", "-datadir=/pf0/run/node", "-forks=100")),
                          limits={"tasks_spawn": 16})
        self.assertNotEqual(r.spawn(0)["status"], {"exit": 0})
        self.assertIn("Resource temporarily unavailable", r.text("view.err"))

    def test_many_spawns_one_invocation(self):
        """D64 stress: 200 spawns (4 invocations of 50; the broker's cap is 64), each placed, run and cleaned up,
        with no census error."""
        n = 50
        for _ in range(4):
            r = self.gate.run(sh(f"i=0; while [ $i -lt {n} ]; do "
                                 f"{stub('version', 'none', '-datadir=/pf0/run/version', '-version')} >/dev/null "
                                 "|| exit 1; i=$((i+1)); done"),
                              budget={"version": n}, limits={"deadline_version": 30, "deadline_cleanup": 10})
            self.assertEqual((r.rc, r.errors()), (0, []), r.text("view.err"))
            self.assertEqual({json.dumps(r.spawn(k)["status"]) for k in range(n)}, {'{"exit": 0}'})

    def test_probe_names_placement(self):
        """D58: a chain whose clone3 is refused is a self-probe failure naming the placement step."""
        r = self.gate.run(sh(stub("run", "counter", "-datadir=/pf0/run/node", "-version")),
                          hooks={"node_launch_hooks": {"0": {"into": "clone3-einval"}}})
        self.assertNotEqual(r.rc, 0)
        self.assertEqual(iv.probe_step(r), "placement", r.errors())


@unittest.skipIf(SKIP, SKIP or "")
class HolderTest(unittest.TestCase):
    """D13, D56, D57, D67: the holder's start, its readiness and the gate-start GC."""

    def test_ready_after_a_slow_unshare(self):
        tl = iv.tools()
        slow = ["/bin/sh", "-c", 'sleep 3; exec "$@"', "sh", tl["unshare"], "--user", "--map-root-user", "--mount",
                "--fork", "--kill-child", "--"]
        g = Gate(self.addCleanup, unshare_argv=slow)
        p = g.holder.run_in(["ls", os.path.join(g.holder.gate, "exe")])
        self.assertEqual(p.stdout.split(), ["head"])
        self.assertFalse(os.path.exists(os.path.join(g.holder.gate, "exe")), "GATE is not on the host")

    def test_holder_in_the_harness_namespaces_refused(self):
        with self.assertRaisesRegex(iv.ChainError, "holder namespaces equal the harness's"):
            Gate(self.addCleanup, unshare_argv=[iv.INTERPRETER, "-I", FAKE_HOLDER])

    def test_holder_never_ready(self):
        with mock.patch.object(iv, "READY_S", 3), self.assertRaisesRegex(iv.ChainError, "holder: timed out"):
            Gate(self.addCleanup, unshare_argv=["/bin/sh", "-c", "sleep 60", "sh"])

    def test_holder_exits_early(self):
        with self.assertRaisesRegex(iv.ChainError, "holder: exited before it was ready"):
            Gate(self.addCleanup, unshare_argv=[iv.INTERPRETER, "-I", "-c", "raise SystemExit(3)"])

    def test_gc_stops_own_units_only(self):
        """D13: a killed gate's holder scope and g-* dir are removed at the next start; another root's unit is
        left alone."""
        root = tempfile.mkdtemp(prefix="gc-", dir=_ROOT)
        prefix = iv.unit_prefix(test=True, root=root)
        other = f"{iv.unit_prefix(test=True, root=_ROOT + '-other')}gc-{os.getpid()}"
        subprocess.run(["systemd-run", "--user", "--quiet", "--collect", f"--unit={other}", "sleep", "300"],
                       check=True)
        self.addCleanup(subprocess.run, ["systemctl", "--user", "stop", other], capture_output=True)
        gd = os.path.join(root, "node", "g-killed")
        os.makedirs(gd, 0o700)
        h = iv.Holder(gd, f"{prefix}holder", {"head": FAKE_NODE}, 600, iv.tools()).start()
        os.close(h.pidfd)  # the killed perfgate's fds are gone; its holder scope is not
        h.pidfd = None
        units, removed = iv.gc(prefix, root=root)
        self.assertEqual(units, [f"{prefix}holder.scope"])
        self.assertEqual(removed, ["g-killed"])
        h.proc.wait(60)
        h.stop()
        self.assertEqual(live_units(prefix), [])
        self.assertEqual(subprocess.run(["systemctl", "--user", "is-active", other], capture_output=True,
                                        text=True).stdout.strip(), "active")


@unittest.skipIf(SKIP, SKIP or "")
class SelfProbeTest(unittest.TestCase):
    """D58: the whole chain once at gate start; a failure names its step and nothing is measured."""

    def probe(self, **kw):
        os.makedirs(os.path.join(_ROOT, "node"), 0o700, exist_ok=True)
        gd = tempfile.mkdtemp(prefix="g-probe-", dir=os.path.join(_ROOT, "node"))
        return iv.self_probe(gd, iv.unit_prefix(test=True, root=_ROOT) + f"p{os.getpid()}-{time.monotonic_ns()}-",
                             iv.tools(), **kw)

    def test_probe_counts_true(self):
        out = self.probe()
        self.assertGreater(out["instructions"], 0)
        self.assertEqual(out["cpus"], "0,1")

    def test_paranoid_3(self):
        p = os.path.join(_ROOT, "paranoid")
        with open(p, "w") as f:
            f.write("3\n")
        with self.assertRaisesRegex(iv.ChainError, "self-probe: perf_event_paranoid above 2"):
            self.probe(paranoid_path=p)

    def test_no_user_namespace(self):
        fail = ["/bin/sh", "-c", "echo 'unshare: unshare failed: Operation not permitted' >&2; exit 1", "sh"]
        with self.assertRaisesRegex(iv.ChainError, "self-probe: holder: .*Operation not permitted"):
            self.probe(unshare_argv=fail)


class MeasureDashdLockTest(unittest.TestCase):
    def test_measure_dashd_holds_the_root_lock(self):
        """D54: `measure --dashd` runs under paths.locked(), so a gate start's GC never meets its live units."""
        import fcntl
        import types

        from perfgate import cli, gate, measure

        root = tempfile.mkdtemp(prefix="lock-", dir=os.environ.get("PERFGATE_TEST_DIR"))
        self.addCleanup(shutil.rmtree, root, True)
        held = []

        def fake(*_a, **_k):
            with open(os.path.join(root, ".lock"), "w") as f:
                try:
                    fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    held.append(False)
                except BlockingIOError:
                    held.append(True)
            return {}

        args = types.SimpleNamespace(dashd="/bin/true", repeat=1, json=None, md=None)
        with mock.patch.dict(os.environ, {"PERFGATE_ROOT": root}), \
                mock.patch.object(measure, "measure_dashd", fake), mock.patch.object(cli, "selected", lambda *a: []), \
                mock.patch.object(measure, "summary", lambda m: ""), mock.patch.object(gate, "noise_table", lambda m: ""):
            with contextlib.redirect_stdout(io.StringIO()):
                cli.cmd_measure(args)
        self.assertEqual(held, [True])


class TmpfsTotalTest(unittest.TestCase):
    def test_aggregate(self):
        lim = dict(iv.LIMITS)
        self.assertEqual(iv.tmpfs_total_check([("genesis", lim, False)]), lim["tmpfs"] + 64 * iv.MIB)
        rows = [(f"r{i}", lim, True) for i in range(3)]
        with self.assertRaisesRegex(iv.ChainError, "exceeds PF0_NODE_TMPFS_TOTAL 12288 MiB"):
            iv.tmpfs_total_check(rows)

    def test_spawn_bound(self):
        lim = dict(iv.LIMITS)
        self.assertEqual(iv.spawn_bound({"version": 1, "run": 1}, lim), 300 + 60 + 600 + 60)


class CensusForkWindowTest(unittest.TestCase):
    def test_a_child_seen_in_the_root_cgroup_is_judged_by_the_next_pass(self):
        """D104: fork shows a child in the root cgroup until cgroup_post_fork places it; a loaded host can keep it
        there past the confirming rescan. Such a process waits for the next full pass; one still there is an error."""
        import types

        from perfgate import outer_init as oi

        harness = "/sys/fs/cgroup/scope/harness"
        procs = {1: ("outer", 1, "/scope/harness"), 2: ("python3", 1, "/scope/harness"),
                 221: ("python3", 2, "/"), 222: ("python3", 2, "/")}

        def read(path):
            if path == "/proc/uptime":
                return "100.00 0.00\n"
            pid, name = int(path.split("/")[2]), path.split("/")[3]
            comm, ppid, cg = procs[pid]
            return {"status": f"Name:\t{comm}\nState:\tS (sleeping)\nPPid:\t{ppid}\nNSpid:\t{pid}\n",
                    "stat": f"{pid} ({comm}) S " + " ".join(["0"] * 18) + f" {9000 + pid} 0\n",
                    "cgroup": f"0::{cg}\n"}[name]

        fake_os = types.SimpleNamespace(listdir=lambda _: [str(p) for p in procs] + ["self"],
                                        readlink=lambda p: "/usr/bin/python3")
        with mock.patch.object(oi, "read", read), mock.patch.object(oi, "os", fake_os):
            c = oi.Census({"exes": {"node_launch": "/x/node-launch", "perf": "/usr/bin/perf", "bwrap": "/usr/bin/bwrap",
                                    "interpreter": "/usr/bin/python3"}, "hello_s": 60}, harness)
            c.register(2)
            c.broker = (2, oi.starttime(2))
            self.assertEqual(c.scan(), {})  # both just forked: wait for the next pass
            self.assertEqual(c.scan([221, 222]), {})
            procs[221] = ("python3", 2, "/scope/harness")  # placed: a fresh broker child in harness
            first = c.scan()
            self.assertEqual(list(first), [222])
            self.assertRegex(first[222], r"^census: unexpected process pid 222 \(python3, /usr/bin/python3; cgroup "
                                         r"/sys/fs/cgroup/, ppid 2")
            self.assertEqual(list(c.scan([222])), [222])
            procs[223] = ("python3", 2, "/")
            self.assertEqual(list(c.scan(final=True)), [222, 223])  # D106: the final pass defers nothing

    def test_the_final_pass_judges_what_the_loop_deferred(self):
        """D106: outer_init's loop can end, the view and the broker reaped, with a process the last pass deferred
        or one started since; the final pass after the loop judges it. Here a stray in the root cgroup is new on
        every pass (a new starttime each read), so only a pass that defers nothing can judge it. A run that already
        failed (the broker exits 1) gets no final pass (D111): its chain may still be forking."""
        import textwrap

        prog = textwrap.dedent(f"""
            import json, os, sys, tempfile
            sys.path.insert(0, {os.path.dirname(os.path.dirname(os.path.realpath(__file__)))!r})
            from perfgate import outer_init as oi

            stray, reads = sys.argv[1] != "none", [0]
            real_read, real_listdir, real_readlink = oi.read, os.listdir, os.readlink

            def read(path):
                if not path.startswith("/proc/9001/"):
                    return real_read(path)
                reads[0] += 1
                return {{"status": "Name:\\tpython3\\nState:\\tS\\nPPid:\\t1\\nNSpid:\\t9001\\n",
                         "stat": "9001 (python3) S " + " ".join(["0"] * 18) + f" {{reads[0]}} 0",
                         "cgroup": "0::/\\n"}}[path.rsplit("/", 1)[1]]

            oi.read = read
            os.listdir = lambda p: (["9001"] if stray else []) if p == "/proc" else real_listdir(p)
            os.readlink = lambda p: "/usr/bin/python3" if p.startswith("/proc/9001/") else real_readlink(p)
            broker = ("import json, os, signal, sys, time; "
                      "os.write(int(sys.argv[1]), json.dumps(dict(event='listening')).encode() + bytes([10])); "
                      f"signal.signal(signal.SIGTERM, lambda *a: sys.exit({{int(sys.argv[1] == 'failed')}})); "
                      "time.sleep(60)")
            o = oi.OuterInit({{"broker": [sys.executable, "-I", "-c", broker, "{{report}}"], "view": ["/bin/true"],
                              "res": tempfile.mkdtemp(dir=sys.argv[2]), "manifest": None, "census_interval_s": 0.25,
                              "hello_s": 60, "broker_stop_s": 30,
                              "exes": {{"node_launch": "/x", "perf": "/x", "bwrap": "/x", "interpreter": sys.executable}}}})
            rc = o.run()
            print(json.dumps([rc, o.result["view"], o.result["broker"], [e.split(" (")[0] for e in o.errors]]))
            """)
        tmp = tempfile.mkdtemp(prefix="census-", dir=os.environ.get("PERFGATE_TEST_DIR"))
        self.addCleanup(shutil.rmtree, tmp, True)
        out = {}
        for case in ("stray", "none", "failed"):
            p = subprocess.run([sys.executable, "-I", "-c", prog, case, tmp], capture_output=True, text=True,
                               timeout=120)
            self.assertEqual(p.returncode, 0, p.stderr)
            out[case] = json.loads(p.stdout)
        self.assertEqual(out["stray"], [1, {"exit": 0}, {"exit": 0}, ["census: unexpected process pid 9001"]])
        self.assertEqual(out["none"], [0, {"exit": 0}, {"exit": 0}, []])
        self.assertEqual(out["failed"], [1, {"exit": 0}, {"exit": 1}, ["the broker exited {'exit': 1}"]])


if __name__ == "__main__":
    unittest.main()
