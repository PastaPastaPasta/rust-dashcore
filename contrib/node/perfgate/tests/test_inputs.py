"""A node row's anchor inputs (PF0-N plan rev4.1 §8, D28, D29, D48, D49): every component moves the hash, nothing
per-gate does, and the rendered argv is what the chain runs.

    python3 -m unittest discover -s contrib/node/perfgate/tests -p test_inputs.py
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

HERE = os.path.dirname(os.path.realpath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

from perfgate import anchor, build, gate, inputs, invocation, node, noderow, paths, spec  # noqa: E402

# The reviewed list of component 5 (D49): a change to the node path's imports changes this snapshot in review.
SOURCES = ["../run", "__init__.py", "broker.py", "build.py", "cgsetup.py", "holder.py", "invocation.py", "measure.py",
           "node.py", "nodegate.py", "noderow.py", "outer_init.py", "paths.py", "sandbox.py"]
TOOLS = {"bwrap": "bwrap 0.11.0", "perf": "perf version 7.0", "systemd": "systemd 259",
         "python": "3.14.0", "util-linux": "unshare 2.41", "cc": "cc 15.2"}
NODE_TREE = "f" * 40
W = spec.Workload(name="node-startup-genesis", kind="node", node="startup-genesis", bin="rdashd")


def scratch():
    d = tempfile.mkdtemp(prefix="inputs-", dir=os.environ.get("PERFGATE_TEST_DIR"))
    os.chmod(d, 0o700)
    return d


class Base(unittest.TestCase):
    def setUp(self):
        self.tmp = scratch()
        self.addCleanup(shutil.rmtree, self.tmp, True)
        git = mock.patch.object(build, "git", side_effect=self.git)
        git.start()
        self.addCleanup(git.stop)
        self.node_tree = NODE_TREE
        self.nb = noderow.node_bench_dir()

    def git(self, *args):
        self.assertEqual(args, ("rev-parse", f"HEAD:{build.NODE_BENCH_REL}"))
        return self.node_tree

    def tool_copy(self):
        d = os.path.join(self.tmp, "tool")
        shutil.copytree(paths.TOOL_DIR, d, ignore=shutil.ignore_patterns("__pycache__"))
        return d

    def inputs(self, w=W, **kw):
        kw.setdefault("nb_dir", self.nb)
        kw.setdefault("tools", TOOLS)
        return inputs.node_inputs(w, "HEAD", **kw)

    def changed(self, before, after):
        return sorted(k for k in inputs.COMPONENTS if before["components"][k] != after["components"][k])


class SourcesTest(Base):
    """Component 5 (D28, D29, D49)."""

    def test_snapshot(self):
        self.assertEqual(inputs.node_sources(), SOURCES)

    def test_static_guard_is_empty(self):
        self.assertEqual(inputs.unhashed_references(SOURCES), [])

    def test_static_guard_catches_an_unlisted_import_and_file(self):
        d = self.tool_copy()
        pkg = os.path.join(d, "perfgate")
        with open(os.path.join(pkg, "node.py"), "a") as f:
            f.write("\n\ndef late():\n    from . import gate  # noqa: F401\n")
        with open(os.path.join(pkg, "measure.py"), "a") as f:
            f.write('\nDECL = "workloads.toml"\n')
        self.assertEqual(inputs.unhashed_references(SOURCES, d),
                         ["measure.py names ../workloads.toml", "node.py names gate.py"])
        # the derivation follows the import, so the snapshot test fails as well
        self.assertIn("gate.py", inputs.node_sources(d))

    def test_every_file_moves_its_component_only(self):
        d = self.tool_copy()
        base = self.inputs(tool_dir=d)
        want = {**{n: "sources" for n in SOURCES}, "node_launch.c": "launcher", "launch.c": "launcher",
                "alloc_preload.c": "preload"}
        for name, comp in sorted(want.items()):
            with self.subTest(file=name):
                path = os.path.join(d, "perfgate", name)
                with open(path, "rb") as f:
                    data = f.read()
                with open(path, "wb") as f:
                    f.write(data + b"\n")
                try:
                    self.assertEqual(self.changed(base, self.inputs(tool_dir=d)), [comp])
                finally:
                    with open(path, "wb") as f:
                        f.write(data)
        self.assertEqual(self.inputs(tool_dir=d), base)


class ComponentTest(Base):
    """Every other component moves the hash, and a stale anchor names it (D48)."""

    def setUp(self):
        super().setUp()
        self.base = self.inputs()

    def moved(self, after, *want):
        self.assertEqual(self.changed(self.base, after), sorted(want))
        self.assertNotEqual(after["inputs"], self.base["inputs"])

    def test_tool_versions(self):
        after = self.inputs(tools={**TOOLS, "perf": "perf version 7.1"})
        self.moved(after, "tools")
        self.assertEqual(inputs.stale_components(self.base, after), ["tools (perf perf version 7.0 → perf version 7.1)"])
        self.moved(self.inputs(tools={**TOOLS, "cc": "cc 16"}), "launcher", "preload", "tools")

    def test_the_chains_tools(self):
        """Component 15 names the chain's interpreter, not the harness's, and not strace (no node strace pass)."""
        tv = inputs.tool_versions()
        self.assertNotIn("strace", tv)
        want = subprocess.run([invocation.INTERPRETER, "-c", "import sys; print(sys.version)"], capture_output=True,
                              text=True, check=True).stdout.splitlines()[0]
        self.assertEqual(tv["python"], want)

    def test_limit(self):
        with mock.patch.dict(invocation.LIMITS, mem_spawn=1 << 30):
            self.moved(self.inputs(), "limits", "outer_argv")  # the broker config carries mem_spawn
        with mock.patch.dict(invocation.LIMITS, mem_run=1 << 30):
            self.moved(self.inputs(), "limits", "outer_argv")  # the scope's MemoryMax

    def test_environment(self):
        with mock.patch.dict(invocation.VIEW_ENV, LANG="C.UTF-8"):
            self.moved(self.inputs(), "environment", "view_argv")
        with mock.patch.dict(invocation.JAIL_ENV, LANG="C.UTF-8"):
            self.moved(self.inputs(), "environment", "spec", "outer_argv")

    def test_view_flag(self):
        with mock.patch.object(invocation, "VIEW_FLAGS", (*invocation.VIEW_FLAGS, "--unshare-cgroup")):
            self.moved(self.inputs(), "view_argv")

    def test_perf_events(self):
        with mock.patch.object(node, "PERF_EVENTS", (*node.PERF_EVENTS, "cycles:u")):
            self.moved(self.inputs(), "perf", "jail_argv", "outer_argv")

    def test_declaration(self):
        self.moved(self.inputs(spec.Workload(**{**W.__dict__, "args": ("--x",)})), "declaration", "view_argv")

    def test_node_bench_tree(self):
        self.node_tree = "e" * 40
        self.moved(self.inputs(), "node_bench")

    def test_prep(self):
        with mock.patch.object(noderow, "prep", return_value={"master": "m1", "seed": "s1"}):
            self.moved(self.inputs(), "fixture", "seed")

    def test_preflight_contract(self):
        nb = os.path.join(self.tmp, "nb")
        os.makedirs(os.path.join(nb, "nodebench"))
        with open(os.path.join(self.nb, "nodebench", "pf0.py")) as f:
            text = f.read()
        self.assertEqual(text.count("SETTLE_S = 30"), 1)
        with open(os.path.join(nb, "nodebench", "pf0.py"), "w") as f:
            f.write(text.replace("SETTLE_S = 30", "SETTLE_S = 31"))
        self.moved(self.inputs(nb_dir=nb), "preflight")
        with open(os.path.join(nb, "nodebench", "pf0.py"), "w") as f:
            f.write(text.replace("MAX_RETRIES = 32", "MAX_RETRIES = 33"))
        self.moved(self.inputs(nb_dir=nb), "preflight", "limits", "outer_argv")  # the preflight spawn budget
        with open(os.path.join(nb, "nodebench", "pf0.py"), "w") as f:
            f.write(text.replace("MAX_RETRIES = 32", "RETRIES = 32"))
        with self.assertRaisesRegex(ValueError, "lacks MAX_RETRIES"):
            self.inputs(nb_dir=nb)

    def test_per_gate_values_do_not_move_it(self):
        # the kernel, the gate's paths and the PR exe are no input; the node-bench dir is a bind source only
        with mock.patch.object(os, "uname", return_value=os.uname_result(
                (os.uname().sysname, "other-host", "9.9.9-other", "#1", os.uname().machine))):
            self.assertEqual(self.inputs(), self.base)
        nb = os.path.join(self.tmp, "nb")
        shutil.copytree(self.nb, nb, ignore=shutil.ignore_patterns("__pycache__"))
        self.assertEqual(self.inputs(nb_dir=nb), self.base)

    def test_the_tool_checkout_place_does_not_move_it(self):
        # another gate's checkout of the same tree: a copy elsewhere, imported from there
        d = self.tool_copy()
        code = ("import json, sys; from unittest import mock; sys.path.insert(0, sys.argv[1]); "
                "from perfgate import build, inputs, spec; "
                "w = spec.Workload(**json.loads(sys.argv[2])); "
                "mock.patch.object(build, 'git', return_value=sys.argv[3]).start(); "
                "print(json.dumps(inputs.node_inputs(w, 'HEAD', nb_dir=sys.argv[4], tools=json.loads(sys.argv[5]))))")
        p = subprocess.run([sys.executable, "-I", "-B", "-c", code, d, json.dumps(W.__dict__), NODE_TREE, self.nb,
                            json.dumps(TOOLS)], capture_output=True, text=True, check=True)
        self.assertEqual(json.loads(p.stdout), self.base)


class RenderTest(Base):
    """Components 8-12 are what Invocation writes and runs, with its paths as placeholders (D48)."""

    class FakeHolder:
        def __init__(self, d):
            self.dir, self.tools = d, invocation.tools()
            self.gate = os.path.join(d, "gate")
            self.sha256 = {"head": "ab" * 32}

        def exe(self, side):
            return os.path.join(self.gate, "exe", side)

        def nsenter(self):
            return invocation.nsenter_argv(self.tools, 5, 6)

    def test_render_is_the_invocation(self):
        contract = noderow.contract(self.nb)
        r = inputs.render(W, self.nb, contract)
        holder = self.FakeHolder(os.path.join(self.tmp, "g-x"))
        stub = os.path.join(self.tmp, "nl")
        for n, (phase, (budget, cmd, binds)) in enumerate(sorted(noderow.phases(W, self.nb, contract).items())):
            with self.subTest(phase=phase):
                inv = invocation.Invocation(holder, "head", n, "pfg-unit-1", limits=noderow.limits(W), budget=budget,
                                            view_cmd=cmd, ro_binds=binds, node_launch=stub)
                subs = [(inv.res, "/<RES>"), (inv.mnt, "/<MNT>"), (inv.cfgdir, "/<CFG>"), (holder.exe("head"),
                        "/<GATE>/exe/<SIDE>"), (stub, "/<NODE_LAUNCH>"), (node.preload_lib(), "/<PRELOAD>"),
                        (self.nb, "/<NODE_BENCH>"), (paths.TOOL_DIR, "/<TOOL>")]

                def placeheld(path, subs=subs):
                    with open(path) as f:
                        obj = json.load(f)
                    for real, ph in subs:
                        obj = inputs._unpath(obj, real, ph)
                    return obj

                broker, outer = placeheld(inv.broker_cfg), placeheld(inv.outer_cfg)
                self.assertEqual((broker["cpu_list"], broker["exe_sha256"]), (inv.cpu_list, "ab" * 32))
                broker.update(cpu_list="/<CPUS>", exe_sha256="/<EXE_SHA256>")
                self.assertEqual(r["view_argv"][phase], outer.pop("view"))
                self.assertEqual(r["outer_argv"]["outer"][phase], outer)
                self.assertEqual(r["outer_argv"]["broker"][phase], broker)
                argv = inputs._unpath(inv.argv(), inv.cfgdir, "/<CFG>")
                argv = [{"--unit=pfg-unit-1": "--unit=/<UNIT>", f"RuntimeMaxSec={inv.d + 30}": "RuntimeMaxSec=/<RUNTIME>",
                         "--user=/proc/self/fd/5": "--user=/proc/self/fd//<USERNS>",
                         "--mount=/proc/self/fd/6": "--mount=/proc/self/fd//<MNTNS>"}.get(a, a) for a in argv]
                self.assertEqual(r["outer_argv"]["invocation"], inputs._unpath(argv, paths.TOOL_DIR, "/<TOOL>"))

    def test_render_names_no_real_path(self):
        text = json.dumps(inputs.render(W, self.nb, noderow.contract(self.nb)))
        for real in (paths.TOOL_DIR, paths.REPO, self.nb, os.path.expanduser("~"), "/work"):
            self.assertNotIn(real, text)


class AnchorTest(Base):
    FP = {"shim": "s", "rustc": "r", "cargo": "c", "cc": "cc", "cargo_config": "x", "libc": "l", "kernel": "k",
          "cpu": "p", "env": {}}

    def test_stale_anchor_names_the_components(self):
        a = self.inputs()
        with mock.patch.dict(invocation.LIMITS, mem_spawn=1 << 30):
            cur = self.inputs(tools={**TOOLS, "bwrap": "bwrap 0.12.0"})
        anc = {"rev": "a" * 40, "fingerprint": self.FP,
               "workloads": {W.name: {"iters": 1, "kind": "node", "key": W.key(), **a, "medians": {}}}}
        head = {"meta": {"rev": "b" * 40, "fingerprint": self.FP},
                "workloads": {W.name: {"status": "ok", "iters": 1, "kind": "node", "samples": {}}}}
        rows = gate.drift_rows([], head, anc, {W.name: {"key": W.key(), **cur}})
        self.assertEqual([r.status for r in rows], ["ERROR"])
        self.assertEqual(rows[0].note, "stale anchor (inputs: outer_argv, limits, tools (bwrap bwrap 0.11.0 → "
                         f"bwrap 0.12.0) changed since aaaaaaaaaaaa): re-anchor {W.name}")
        self.assertEqual(gate.drift_rows([], head, anc, {W.name: {"key": W.key(), **a}}), [])

    def test_refused_row_is_recorded_without_medians(self):
        root = os.path.join(self.tmp, "root")
        os.makedirs(root)
        record = {"status": "refused", "argv_sha256": "c" * 64, "attempts": 16, "accepted_by": "settled",
                  "refused_on": [{"option": "-txindex", "owner": "BL-1"}], "version": 1}
        meas = {"meta": {"rev": "c" * 40, "fingerprint": self.FP},
                "workloads": {"node-mempool": {"status": "refused", "kind": "node", "preflight": record}}}
        ins = self.inputs()
        with mock.patch.dict(os.environ, {"PERFGATE_ROOT": root}):
            entry = anchor.record(meas, "PF0-N", "t" * 40, {}, {"node-mempool": "k"}, {"node-mempool": ins})
        w = entry["workloads"]["node-mempool"]
        self.assertEqual(w["status"], "refused")
        self.assertNotIn("medians", w)
        self.assertEqual((w["inputs"], w["components"], w["tools"]), (ins["inputs"], ins["components"], TOOLS))
        self.assertEqual(w["preflight"], {k: v for k, v in record.items() if k != "status"})


if __name__ == "__main__":
    unittest.main()
