"""The reduced kill matrix (plan rev4.1 §11, D52, D68, D71, D84): 17 chains, each killed once at one lifecycle
point, then polled until 120 s for no survivors, and an ERROR wherever a result exists.

Points: 1 after clone3, before exec (the child stopped by the test build's hook); 2 after the perf exec (a fake
perf that never starts the jail step); 3 the jail step stopped by the `hold` hook right before its bwrap exec
(D84); 4 after ready; 5 during the stop (the node ignores SIGTERM); 6 during the cgroup-empty wait (held open by
the broker hook); 7 during the rmdir retry (rmdir reports EBUSY).

Each chain runs under tests/chain_driver.py, so the harness itself can be killed.

    python3 -m unittest discover -s contrib/node/perfgate/tests
"""

import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

HERE = os.path.dirname(os.path.realpath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
sys.path.insert(0, HERE)

import test_chain  # noqa: E402

from perfgate import invocation as iv  # noqa: E402
from perfgate import node  # noqa: E402

DRIVER = os.path.join(HERE, "chain_driver.py")
SKIP = test_chain.SKIP
LIMITS = {**test_chain.FAST, "deadline_run": 60, "deadline_cleanup": 30}
POLL_S = 120  # D51

_ROOT = None
_ENV = None
_FAKE_PERF = None


def setUpModule():
    global _ROOT, _ENV, _FAKE_PERF
    if SKIP:
        return
    _ROOT = tempfile.mkdtemp(prefix="kill-", dir=os.environ.get("PERFGATE_TEST_DIR"))
    os.chmod(_ROOT, 0o700)
    _ENV = mock.patch.dict(os.environ, {"PERFGATE_ROOT": _ROOT})
    _ENV.start()
    node.node_launch(), node.node_launch(test_hooks=True), node.preload_lib()
    src = os.path.join(_ROOT, "fake_perf.c")
    with open(src, "w") as f:
        f.write("#include <unistd.h>\nint main(void) { for (;;) pause(); }\n")
    _FAKE_PERF = os.path.join(_ROOT, "fake_perf")
    subprocess.run(["cc", "-O2", "-o", _FAKE_PERF, src], check=True)


def tearDownModule():
    if _ROOT:
        _ENV.stop()
        shutil.rmtree(_ROOT, ignore_errors=True)


def stub(*argv, cls="run"):
    """A measured run spawn, or (points 1 and 3, D68) an unmeasured start spawn."""
    return test_chain.stub(cls, "counter" if cls == "run" else "none", "-datadir=/pf0/run/node", *argv)


def await_ready(phase, *argv, then=""):
    """node-bench's side: the stub in the background, the phase marker once the node is ready (or stopping)."""
    return test_chain.sh(f"{stub(*argv)} & p=$!; until [ -e /pf0/run/node/ready ]; do sleep 0.1; done; "
                         f"{then}echo pf0-phase-{phase}; wait $p")


STOPPING = "kill -TERM $p; "
POINTS = {  # point: (view, hooks, test build, fake perf)
    1: (test_chain.sh(stub(cls="start")), {"node_launch_hooks": {"0": {"into": "stop-before-exec"}}}, True, False),
    2: (test_chain.sh(stub()), None, False, True),
    3: (test_chain.sh(stub(cls="start")), {"node_launch_hooks": {"0": {"jail": "hold"}}}, True, False),
    4: (await_ready(4), None, False, False),
    5: (await_ready(5, "-hang", then=STOPPING), None, False, False),
    6: (await_ready(6, then=STOPPING), {"hold_cleanup_s": 60}, False, False),
    7: (await_ready(7, then=STOPPING), {"rmdir_ebusy": 10 ** 6}, False, False),
}
ROWS = [("harness", p) for p in range(1, 8)] + [
    ("helper", 1), ("helper", 3), ("perf", 2), ("bwrap", 3), ("stub", 4), ("node-bench", 4), ("holder", 4),
    ("broker", 5), ("outer_init", 6), ("scope", 7)]


def procs(unit):
    """{pid: (argv, exe, state)} of the processes in a unit's cgroup tree, from the host."""
    out = {}
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        try:
            with open(f"/proc/{d}/cgroup") as f:
                cg = f.read().strip()
            if f"/{unit}.scope" not in cg:
                continue
            with open(f"/proc/{d}/cmdline", "rb") as f:
                argv = [a.decode(errors="replace") for a in f.read().split(b"\0") if a]
            with open(f"/proc/{d}/stat") as f:
                state = f.read().rsplit(")", 1)[1].split()[0]
            try:
                exe = os.readlink(f"/proc/{d}/exe")
            except OSError:
                exe = None
            out[int(d)] = (argv, exe, state)
        except OSError:
            continue
    return out


def poll(pred, what, bound=POLL_S):
    end = time.monotonic() + bound
    while time.monotonic() < end:
        got = pred()
        if got:
            return got
        time.sleep(0.1)
    raise AssertionError(f"{what} not within {bound} s")


def control_group(unit):
    p = subprocess.run(["systemctl", "--user", "show", "-p", "ControlGroup", "--value", f"{unit}.scope"],
                       capture_output=True, text=True)
    return p.stdout.strip()


@unittest.skipIf(SKIP, SKIP or "")
class KillMatrixTest(unittest.TestCase):
    def run_row(self, killer, point):
        view, hooks, test_build, fake_perf = POINTS[point]
        d = tempfile.mkdtemp(prefix=f"g-{killer}-{point}-", dir=_ROOT)
        prefix = f"{iv.unit_prefix(test=True, root=_ROOT)}km{os.getpid()}-{killer.replace('_', '')}{point}-"
        spec = {"dir": d, "prefix": prefix, "cmd": view, "budget": {"start" if point in (1, 3) else "run": 1},
                "limits": LIMITS, "hooks": hooks,
                "node_launch": node.node_launch(test_hooks=test_build), "perf": _FAKE_PERF if fake_perf else None}
        drv = subprocess.Popen([iv.INTERPRETER, "-I", "-B", DRIVER, json.dumps(spec)], stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, text=True)
        self.addCleanup(drv.stderr.close)
        self.addCleanup(drv.stdout.close)
        self.addCleanup(lambda: drv.poll() is None and drv.kill())
        head = json.loads(drv.stdout.readline() or "null") or self.fail(f"driver: {drv.stderr.read()}")
        inv, holder = head["unit"], head["holder_unit"]
        view_out = os.path.join(head["res"], "view.out")

        def phase():
            ps = procs(inv)
            if point == 1:
                return [pid for pid, (_, _, st) in ps.items() if st == "T"]
            if point == 2:
                return [pid for pid, (_, exe, _) in ps.items() if exe == _FAKE_PERF]
            if point == 3:
                return [pid for pid, (argv, _, st) in ps.items() if st == "T" and "--jail" in argv[:4]]
            try:
                with open(view_out) as f:
                    return f"pf0-phase-{point}" in f.read()
            except OSError:
                return False

        poll(phase, f"point {point}")
        if point >= 5:
            time.sleep(2)  # into the stop, the cgroup-empty wait or the rmdir retries
        self.kill(killer, point, drv, inv, holder)
        out = ""
        if killer != "harness":
            try:
                out, _ = drv.communicate(timeout=iv.spawn_bound(spec["budget"], LIMITS) + 120)
            except subprocess.TimeoutExpired:
                self.fail("the driver outlived its invocation bound")
        poll(lambda: not test_chain.live_units(prefix) and not procs(inv) and not procs(holder),
             "no survivors and no units")
        if killer != "harness":
            result = json.loads(out.strip().splitlines()[-1])
            self.assertTrue(result["errors"], f"{killer}@{point} produced no error text: {result}")

    def kill(self, killer, point, drv, inv, holder):
        if killer == "harness":
            drv.send_signal(signal.SIGKILL)
            drv.wait()
            return
        if killer == "scope":
            with open(f"/sys/fs/cgroup{control_group(inv)}/cgroup.kill", "w") as f:
                f.write("1")
            return
        unit = holder if killer == "holder" else inv

        def match(argv, exe, st):
            text = " ".join(argv)
            if argv and os.path.basename(argv[0]) == "unshare":  # its argv names what it forked (outer_init, holder)
                return False
            # the helper, not its stopped clone3 child (same argv until it execs)
            return {"helper": "--into" in argv[:4] and "--client" not in argv and st != "T",
                    "perf": exe == _FAKE_PERF,
                    "bwrap": "--jail" in argv[:4],
                    "stub": "--client" in argv[:4],
                    "node-bench": argv[:2] == ["/bin/sh", "-c"] and "pf0-phase" in text,
                    "holder": "holder.py" in text and "--config" in argv,
                    "broker": "broker_hooked.py" in text or "/broker.py" in text,
                    "outer_init": "outer_init.py" in text}[killer]

        pids = poll(lambda: [pid for pid, (argv, exe, st) in procs(unit).items() if match(argv, exe, st)],
                    f"the {killer} process")
        self.assertEqual(len(pids), 1, f"{killer}: more than one candidate {pids}")
        os.kill(pids[0], signal.SIGKILL)


def _row_test(killer, point):
    def test(self):
        self.run_row(killer, point)
    test.__doc__ = f"{killer} killed at point {point}"
    return test


for _killer, _point in ROWS:
    setattr(KillMatrixTest, f"test_{_killer.replace('-', '_')}_at_{_point}", _row_test(_killer, _point))


if __name__ == "__main__":
    unittest.main()
