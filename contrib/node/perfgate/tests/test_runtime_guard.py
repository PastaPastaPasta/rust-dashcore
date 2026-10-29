"""The runtime source guard (PF0-N plan rev4.1 §8, §11, D29 b, D36, D53 d, P17): one node row's gate pass with every
Python process of the node path audited and the whole invocation under strace, so a file the node path reads at run
time that §8 doesn't hash fails the test.

- Every Python process started with the node path's interpreter is the test venv's, whose `pf0audit.pth` installs an
  audit hook (site runs `.pth` lines under -I) that logs its argv and every open/exec as JSONL, one file per process
  (named by pid namespace, pid and a random tag: pid namespace inodes are reused).
- The chain runs under `strace -ff` (execve, execveat, open, openat, openat2): the Python execs must all be the
  venv's and each must have an event file (coverage); the non-Python processes' opens and execs are checked too.
- Every path opened or exec'd under TOOL_DIR must be in component 5 (or its C sources, the compiled artifacts mapped
  back to them); under the repo, anything else must be node-bench's tree (component 4); nothing on the node path but
  the node opens host /tmp or /dev/shm (D53 d).

Not covered: the harness process itself (test_inputs' import-closure snapshot covers what it loads); every syscall
of the process that execs /jail/rdashd, its bwrap and setarch steps before the exec included; and the holder's
non-Python children (it starts outside the traced invocation argv; its Python is audited). What these do reaches
the counts only through the rendered configs, which §8 hashes.

    python3 -m unittest discover -s contrib/node/perfgate/tests -p test_runtime_guard.py
"""

import glob
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

HERE = os.path.dirname(os.path.realpath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
sys.path.insert(0, HERE)

import test_chain  # noqa: E402

from perfgate import inputs, invocation, node, nodegate, paths, spec  # noqa: E402

SKIP = test_chain.SKIP or (None if shutil.which("strace") else "no strace")
AUDIT = '''# test-only (PF0-N D36): log this process's argv and every open/exec as JSONL, one file per process
import json, os, sys
_d = os.path.abspath(__file__)
for _ in range(5):  # site-packages, python3.x, lib, venv, the audit dir
    _d = os.path.dirname(_d)
_d = os.path.join(_d, "events")
_out = open(os.path.join(_d, f"{os.stat('/proc/self/ns/pid').st_ino}-{os.getpid()}-{os.urandom(4).hex()}.jsonl"), "a", buffering=1)
_out.write(json.dumps(["argv", [sys.executable, *sys.orig_argv[1:]]]) + "\\n")


def _hook(ev, args):
    if ev in ("open", "os.exec", "os.posix_spawn", "subprocess.Popen"):
        try:
            _out.write(json.dumps([ev, [str(a) for a in args[:2]]]) + "\\n")
        except Exception:
            pass


sys.addaudithook(_hook)
'''
SYSCALL = re.compile(r'^(execve|execveat|openat2|openat|open)\((?:[^,"]*, )?"([^"]*)"(?:, \[(.*?)\])?.*\) = (-?\d+)')
PY_EXE = re.compile(r"(^|/)python[0-9.]*$")


def venv(d):
    subprocess.run([sys.executable, "-m", "venv", "--without-pip", os.path.join(d, "venv")], check=True)
    os.makedirs(os.path.join(d, "events"), 0o700)
    site = glob.glob(os.path.join(d, "venv", "lib", "python3*", "site-packages"))[0]
    with open(os.path.join(site, "pf0audit.py"), "w") as f:
        f.write(AUDIT)
    with open(os.path.join(site, "pf0audit.pth"), "w") as f:
        f.write("import pf0audit\n")
    return os.path.join(d, "venv", "bin", "python3")


def strace_records(prefix):
    """{pid: [(call, path, argv or None, ret)]} from `strace -ff -o prefix` files."""
    out = {}
    for f in glob.glob(prefix + ".*"):
        pid = int(f.rsplit(".", 1)[1])
        with open(f, errors="replace") as fh:
            for line in fh:
                mt = SYSCALL.match(line)
                if mt:
                    argv = re.findall(r'"((?:[^"\\]|\\.)*)"', mt.group(3)) if mt.group(3) else None
                    out.setdefault(pid, []).append((mt.group(1), mt.group(2), argv, int(mt.group(4))))
    return out


def audit_files(events):
    out = {}
    for f in glob.glob(os.path.join(events, "*.jsonl")):
        with open(f) as fh:
            out[os.path.basename(f)] = [json.loads(line) for line in fh if line.strip()]
    return out


def guard_problems(audit, traces, py, hashed, artifacts, tool_dir, repo):
    """D36's checks over the audit events and the strace records; [] when the node path read only hashed files."""
    problems = []
    tool, nb = tool_dir + "/", os.path.join(repo, "node-bench") + "/"

    def check(who, path):
        if path.startswith(tool):
            rel = os.path.relpath(path, os.path.join(tool_dir, "perfgate"))
            rel = re.sub(r"^__pycache__/([^.]+)\.cpython-[^/]*\.pyc$", r"\1.py", rel)
            if rel not in hashed:
                problems.append(f"{who} read {path}, which §8 does not hash")
        elif path.startswith(repo + "/") and not path.startswith(nb):
            problems.append(f"{who} read {path}, outside the tool and node-bench")
        elif os.path.dirname(path) == os.path.join(paths.root(), "lib") and os.path.realpath(path) not in artifacts:
            problems.append(f"{who} ran {path}, not a compiled artifact of a hashed source")

    for name, evs in sorted(audit.items()):
        for ev, args in evs:
            if ev != "argv" and args and args[0].startswith("/"):
                check(f"audit {name}", args[0])
    jail = set()  # the node's processes (PR code: §8 doesn't hash what it reads)
    for pid, recs in sorted(traces.items()):
        for call, path, argv, ret in recs:
            if call.startswith("exec") and ret == 0 and (path == "/jail/rdashd" or "/jail/rdashd" in (argv or ())):
                jail.add(pid)
    py_execs = []
    for pid, recs in sorted(traces.items()):
        node_proc = pid in jail
        for call, path, argv, ret in recs:
            if ret < 0:
                continue
            if call.startswith("exec") and PY_EXE.search(path) and not node_proc:
                if path != py:
                    problems.append(f"pid {pid} exec'd {path}, not the audited interpreter")
                py_execs.append(argv or [path])
            if not node_proc:
                check(f"pid {pid}", path)
                if re.match(r"^/(tmp|dev/shm)(/|$)", path) and not call.startswith("exec"):
                    problems.append(f"pid {pid} opened host {path} (D53 d)")
    logged = sorted(json.dumps(evs[0][1][1:]) for evs in audit.values() if evs and evs[0][0] == "argv")
    for argv in py_execs:
        key = json.dumps(argv[1:])
        if key in logged:
            logged.remove(key)
        else:
            problems.append(f"a Python process with no audit events: {argv}")
    return problems


@unittest.skipIf(SKIP, SKIP or "")
class RuntimeGuardTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.root = tempfile.mkdtemp(prefix="guard-", dir=os.environ.get("PERFGATE_TEST_DIR"))
        os.chmod(cls.root, 0o700)
        cls.addClassCleanup(shutil.rmtree, cls.root, True)
        env = mock.patch.dict(os.environ, {"PERFGATE_ROOT": cls.root})
        env.start()
        cls.addClassCleanup(env.stop)
        cls.py = venv(os.path.join(cls.root, "audit"))
        cls.events = os.path.join(cls.root, "audit", "events")
        cls.trace = os.path.join(cls.root, "trace", "s")
        os.makedirs(os.path.dirname(cls.trace), 0o700)
        configs, argv = invocation.configs, invocation.Invocation.argv
        audit_dir = os.path.join(cls.root, "audit")

        def traced(self):
            return ["strace", "-ff", "-qq", "-s", "4096", "-o", cls.trace,
                    "-e", "trace=execve,execveat,open,openat,openat2", "--", *argv(self)]

        def bound(*args, **kw):
            # the audit dir stays writable for outer_init's children and in the view (the venv is in it)
            broker, outer = configs(*args, **kw)
            outer["rw"] = [*outer["rw"], audit_dir]
            outer["view"] = [outer["view"][0], "--bind", audit_dir, audit_dir, *outer["view"][1:]]
            return broker, outer

        w = spec.Workload(name="node-startup-genesis", kind="node", node="startup-genesis", bin="rdashd")
        exe = os.path.join(cls.root, "rdashd")
        shutil.copy(test_chain.FAKE_NODE, exe)
        with mock.patch.object(invocation, "INTERPRETER", cls.py), \
                mock.patch.object(invocation, "configs", bound), \
                mock.patch.object(invocation.Invocation, "argv", traced), \
                nodegate.Session({"head-rdashd": exe}, [w], 1, lambda _m: None, test=True) as s:
            cls.out = s.measure(["head-rdashd"], w, 1)["head-rdashd"]
        cls.audit = audit_files(cls.events)
        cls.traces = strace_records(cls.trace)
        cls.hashed = set(inputs.node_sources()) | set(inputs.C_SOURCES)
        cls.artifacts = {os.path.realpath(p) for p in (node.node_launch(), node.preload_lib())}

    def problems(self, audit=None, traces=None):
        return guard_problems(self.audit if audit is None else audit, self.traces if traces is None else traces,
                              self.py, self.hashed, self.artifacts, paths.TOOL_DIR, paths.REPO)

    def test_the_run_measured(self):
        self.assertEqual(self.out["status"], "ok", self.out)
        self.assertEqual(len(self.out["samples"]["page_faults"]), 1)

    def test_every_node_path_process_was_seen(self):
        argvs = [evs[0][1] for evs in self.audit.values()]
        for script in (invocation.HOLDER, invocation.OUTER_INIT, node.BROKER, node.CGSETUP, "/pf0/node-bench/run"):
            self.assertTrue(any(script in a for a in argvs), (script, argvs))
        self.assertTrue(any(c.startswith("exec") and p.endswith("bwrap") for recs in self.traces.values()
                            for c, p, _, _ in recs))
        # the guard had something to check: tool sources opened, seen by both mechanisms
        tool = os.path.join(paths.TOOL_DIR, "perfgate") + "/"
        self.assertTrue(any(p.startswith(tool) for recs in self.traces.values() for _, p, _, _ in recs))
        self.assertTrue(any(ev == "open" and a[0].startswith(tool) for evs in self.audit.values() for ev, a in evs))

    def test_guard_passes(self):
        self.assertEqual(self.problems(), [])

    def test_guard_catches_an_unhashed_python_read(self):
        """The broker reading gate.py fails, naming the broker's event file."""
        broker = next(n for n, evs in self.audit.items() if node.BROKER in evs[0][1])
        audit = {**self.audit, broker: [*self.audit[broker],
                                        ["open", [os.path.join(paths.TOOL_DIR, "perfgate", "gate.py"), "r"]]]}
        self.assertEqual(self.problems(audit=audit),
                         [f"audit {broker} read {paths.TOOL_DIR}/perfgate/gate.py, which §8 does not hash"])

    def test_guard_catches_a_non_python_read(self):
        """A non-Python process (perf) opening the same file fails through strace."""
        pid = next(p for p, recs in self.traces.items() if any(c == "execve" and r == 0 and p_.endswith("/perf")
                                                              for c, p_, _, r in recs))
        bad = os.path.join(paths.TOOL_DIR, "perfgate", "gate.py")
        traces = {**self.traces, pid: [*self.traces[pid], ("openat", bad, None, 3)]}
        self.assertEqual(self.problems(traces=traces), [f"pid {pid} read {bad}, which §8 does not hash"])

    def test_guard_catches_an_unaudited_interpreter(self):
        traces = {**self.traces, 1: [("execve", "/usr/bin/python3", ["/usr/bin/python3", "-I", "x.py"], 0)]}
        self.assertEqual(self.problems(traces=traces), [
            "pid 1 exec'd /usr/bin/python3, not the audited interpreter",
            "a Python process with no audit events: ['/usr/bin/python3', '-I', 'x.py']"])

    def test_guard_catches_host_tmp(self):
        pid = min(self.traces)
        traces = {**self.traces, pid: [*self.traces[pid], ("openat", "/tmp/pf0-marker", None, 3)]}
        self.assertEqual(self.problems(traces=traces), [f"pid {pid} opened host /tmp/pf0-marker (D53 d)"])


if __name__ == "__main__":
    unittest.main()
