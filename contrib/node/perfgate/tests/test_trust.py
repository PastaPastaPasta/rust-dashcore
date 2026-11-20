"""The malicious-node-bench test (plan rev4.1 §11, D6, D86): an attacker in node-bench's view tries every way to
the PR binary (its argv and env, /proc, the exe's host paths, a walk of every visible filesystem, refused broker
requests, capabilities). The PR binary is a marker that reports each start, so the test proves every execution
happened inside a spawn jail.

    python3 -m unittest discover -s contrib/node/perfgate/tests
"""

import hashlib
import json
import os
import random
import shutil
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

SKIP = test_chain.SKIP
ATTACKER = os.path.join(HERE, "attacker_node_bench.py")
MARKER_SRC = os.path.join(HERE, "marker_rdashd.c")
VERSIONS = 63  # with the one measured run, the broker's 64-spawn cap


@unittest.skipIf(SKIP, SKIP or "")
class MaliciousNodeBenchTest(unittest.TestCase):
    def setUp(self):
        self.root = tempfile.mkdtemp(prefix="trust-", dir=os.environ.get("PERFGATE_TEST_DIR"))
        os.chmod(self.root, 0o700)
        self.addCleanup(shutil.rmtree, self.root, True)
        env = mock.patch.dict(os.environ, {"PERFGATE_ROOT": self.root})
        env.start()
        self.addCleanup(env.stop)
        self.prefix = iv.unit_prefix(test=True, root=self.root)
        self.addCleanup(self.no_units_left)

    def no_units_left(self):
        end = time.monotonic() + 120  # D51
        while test_chain.live_units(self.prefix) and time.monotonic() < end:
            time.sleep(0.5)
        self.assertEqual(test_chain.live_units(self.prefix), [])

    def test_attacker_reaches_no_pr_binary(self):
        marker = f"PF0-MARKER-{random.getrandbits(64):016x}"
        port = random.randrange(20000, 30000)
        exe = os.path.join(self.root, "build", "rdashd")
        os.makedirs(os.path.dirname(exe), 0o700)
        subprocess.run(["cc", "-O2", f'-DMARKER="{marker}"', f"-DMARKER_PORT={port}", "-o", exe, MARKER_SRC],
                       check=True)
        with open(exe, "rb") as f:
            data = f.read()
        gd = os.path.join(self.root, "node", "g-trust")
        os.makedirs(gd, 0o700)
        holder = iv.Holder(gd, f"{self.prefix}trust-holder", {"head": exe}, 900, iv.tools()).start()
        self.addCleanup(holder.stop)
        cfg = os.path.join(self.root, "attacker.json")
        with open(cfg, "w") as f:
            json.dump({"port": port, "size": len(data), "sha256": hashlib.sha256(data).hexdigest(),
                       "marker": marker, "versions": VERSIONS,
                       "candidates": [holder.exe("head"), exe,
                                      f"/proc/{holder.proc.pid}/root{holder.exe('head')}"]}, f)
        inv = iv.Invocation(holder, "head", 0, f"{self.prefix}trust-0", budget={"version": VERSIONS, "run": 1},
                            limits={"deadline_version": 30, "deadline_run": 120, "deadline_cleanup": 10},
                            view_cmd=["/usr/bin/python3", "-I", "/pf0/attacker.py", "/pf0/attacker.json"],
                            ro_binds=[(ATTACKER, "/pf0/attacker.py"), (cfg, "/pf0/attacker.json")])
        r = inv.run()
        self.assertEqual(r.outer and r.outer["view"], {"exit": 0}, f"{r.errors()} {r.text('view.err')[:4000]}")
        out = json.loads(r.text("view.out").splitlines()[-1])

        # argv and env name nothing but the view's own paths
        allowed = ("/pf0/", "/usr/bin", "/bin", "/usr/bin/python3")
        self.assertEqual([p for p in out["paths"] if not p.startswith(allowed) and p not in ("/bin",)], [])
        # /proc shows nothing of the exe; no exec path works; no file anywhere is it
        # (the view's own stdout and stderr are res files: their names show, the files aren't mounted)
        exe_places = (holder.gate, os.path.dirname(exe), "/jail")
        self.assertFalse([x for x in out["proc_links"] if "rdashd" in x or x.startswith(exe_places)])
        self.assertEqual({p: v for p, v in out["exec"].items() if v == "ok"}, {})
        self.assertEqual(out["found"], [])
        # no capability
        self.assertTrue(all(v != "ok" for k, v in out["caps"].items() if k != "ptrace_init"), out["caps"])
        # every bad request refused
        for what, reply in out["refused"].items():
            with self.subTest(request=what):
                self.assertTrue(reply is None or "error" in reply or "oserror" in reply, reply)
        self.assertEqual(out["symlink"], {"canary": True, "left": False})
        # 2 GiB to the view's stderr: res keeps the cap
        self.assertEqual(os.path.getsize(os.path.join(r.res, "view.err")), 256 * 1024)
        self.assertEqual(r.outer["view_log_dropped"]["view.err"], (2 << 30) - 256 * 1024)
        res_bytes = sum(os.path.getsize(os.path.join(r.res, n)) for n in os.listdir(r.res))
        self.assertLess(res_bytes, 4 << 20)
        self.assertEqual(out["run_exit"], 0)
        self.assertEqual(out["version_exits"], [0])

        # the proof: one datagram per spawn, each from inside a jail
        spawns = len([n for n in os.listdir(r.res) if n.endswith(".result.json")])
        self.assertEqual(spawns, VERSIONS + 1)
        self.assertEqual(len(out["grams"]), spawns)
        for g in out["grams"]:
            self.assertEqual((g["marker"], g["cgroup"], g["exe"]), (marker, "0::/", "/jail/rdashd"))
            self.assertNotEqual(g["pidns"], out["own_pidns"])
        self.assertEqual([e for e in r.errors() if not e.startswith("broker refused")], [], r.errors())


class NodeBenchPinTest(unittest.TestCase):
    """D1, D2: node-bench is pinned to the base like the harness; an override is never authoritative."""

    def setUp(self):
        from perfgate import build, paths

        self.build, self.paths = build, paths
        self.dir = tempfile.TemporaryDirectory()
        self.repo = os.path.realpath(self.dir.name)
        env = {"GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1", **build.MERGE_ENV}
        for patch in (mock.patch.dict(os.environ, env), mock.patch.object(paths, "REPO", self.repo),
                      mock.patch.object(paths, "TOOL_DIR", os.path.join(self.repo, build.TOOL_REL))):
            patch.start()
            self.addCleanup(patch.stop)
        os.environ.pop("PERFGATE_NODE_BENCH", None)
        self.addCleanup(self.dir.cleanup)
        self.git("init", "-q", "-b", "main")
        self.base = self.commit({"contrib/node/perfgate/run": "tool\n", "node-bench/run": "bench\n"})

    def git(self, *args):
        return subprocess.run(["git", *args], cwd=self.repo, check=True, capture_output=True, text=True).stdout.strip()

    def commit(self, files):
        for path, text in files.items():
            full = os.path.join(self.repo, path)
            os.makedirs(os.path.dirname(full), exist_ok=True)
            with open(full, "w") as f:
                f.write(text)
        self.git("add", *files)
        self.git("commit", "-q", "-m", "c")
        return self.git("rev-parse", "HEAD")

    def test_clean_base_checkout(self):
        self.assertEqual(self.build.harness_problems(self.base), [])
        self.assertIsNone(self.build.node_bench_override())

    def test_node_bench_differs_from_the_base(self):
        self.commit({"node-bench/run": "patched\n"})
        self.assertTrue(any("node-bench" in p and "is not the base's" in p
                            for p in self.build.harness_problems(self.base)))

    def test_node_bench_dirty(self):
        with open(os.path.join(self.repo, "node-bench", "extra.py"), "w") as f:
            f.write("x = 1\n")
        self.assertIn(f"node-bench has uncommitted changes ({self.repo}/node-bench)",
                      self.build.harness_problems(self.base))

    def test_override_is_non_authoritative(self):
        with mock.patch.dict(os.environ, {"PERFGATE_NODE_BENCH": "/elsewhere/node-bench"}):
            self.assertEqual(self.build.node_bench_override(), "node-bench overridden: /elsewhere/node-bench")


if __name__ == "__main__":
    unittest.main()
