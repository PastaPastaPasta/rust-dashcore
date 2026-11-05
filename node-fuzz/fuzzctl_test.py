"""Tests of fuzzctl (O3): `python3 -I node-fuzz/fuzzctl.py selftest`.

Everything runs on synthetic trees in a fresh temporary directory, with DASH_EMBARGO_DIR pointed
at it for the duration of a test; no fuzz target is built or run.
"""

import argparse
import contextlib
import functools
import getpass
import glob
import hashlib
import importlib.util
import inspect
import io
import json
import os
import re
import shlex
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import threading
import time
import types
import unittest
from unittest import mock

import fuzzctl
from dataclasses import replace as dataclasses_replace
from fuzzctl import Leg, Target

# The farm is generic over pins; the registry has one (DEC-82), so the tests patch in a second,
# fictitious one (`PINS` below) to keep the multi-pin logic covered.
TEST_PINS = ("v24.0", "v25.0")
PRIV = Target("diff_tx_decode", "l1", ("v25.0", "v24.0"), "private", 60, 4)
PUB = Target("kv_model", "model", ("v24.0",), "public", 60, 1)
SELF = Target("selftest_mismatch", "selftest", (), "public", 0, 0)
TARGETS = [PRIV, PUB, SELF]


class Tree(unittest.TestCase):
    """A private tree of its own, as DASH_EMBARGO_DIR. `behaviour` maps an input's sha256 to
    what it does when run: ("divergence", field) or ("crash", log)."""

    def setUp(self):
        self.root = tempfile.mkdtemp(prefix="fuzzctl-test-")
        os.chmod(self.root, 0o700)
        self.fdir = os.path.join(self.root, "fuzz")
        os.mkdir(self.fdir, 0o700)
        os.chmod(self.fdir, 0o700)
        self.behaviour = {}
        env = mock.patch.dict(os.environ, {"DASH_EMBARGO_DIR": self.root})
        env.start()
        self.addCleanup(env.stop)
        pins = mock.patch.object(fuzzctl, "PINS", TEST_PINS)
        pins.start()
        self.addCleanup(pins.stop)
        self.addCleanup(shutil.rmtree, self.root, True)

    def make_run(self, target, run_id, pin="v24.0", divergences=(), crashes=(), log="",
                 mode="run"):
        run = os.path.join(self.fdir, target, "runs", run_id)
        record(run, divergences, crashes, log, pin)
        for data, fld in divergences:
            self.behaviour[hashlib.sha256(data).hexdigest()] = ("divergence", fld)
        for _, data in crashes:
            self.behaviour[hashlib.sha256(data).hexdigest()] = ("crash", log)
        json.dump({"mode": mode, "pin": pin, "rust_dashcore": "abc", "divergences": len(divergences),
                   "crashes": len(crashes)}, open(os.path.join(run, "verdict.json"), "w"))
        return run

    def state(self, target):
        return json.load(open(os.path.join(self.fdir, target, "triage.json")))

    def cli(self, argv):
        out = io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(io.StringIO()), \
                mock.patch.object(fuzzctl, "load_registry", lambda: TARGETS):
            rc = fuzzctl.main(argv)
        return rc, out.getvalue()


def record(run, divergences=(), crashes=(), log="", pin="v24.0"):
    """Writes findings into a run directory as run.sh and the harness do."""
    for d in ("divergences", "crashes", "logs/segment-1"):
        os.makedirs(os.path.join(run, d), exist_ok=True)
    for data, fld in divergences:
        sha = hashlib.sha256(data).hexdigest()
        open(os.path.join(run, "divergences", sha), "wb").write(data)
        open(os.path.join(run, "divergences", sha + ".txt"), "w").write(
            f"target t\npin {pin}\nfield {fld}\n")
    for name, data in crashes:
        open(os.path.join(run, "crashes", name), "wb").write(data)
    open(os.path.join(run, "logs", "segment-1", "fuzz.out"), "w").write(log)


class FakeRunner:
    """Stands in for run.sh. Minimisation writes `crashes/minimized` (the input's first `shrink`
    bytes), which behaves like the input unless `keep_class` is off; a replay reproduces the
    input's behaviour unless its sha256 is in `fixed`."""

    def __init__(self, tree, shrink=2, minimize_ok=True, keep_class=True):
        self.tree, self.shrink = tree, shrink
        self.minimize_ok, self.keep_class = minimize_ok, keep_class
        self.calls, self.fixed = [], set()
        self.cmds = {}  # sha256 (or (sha256, pin)) -> the command a replay's divergence names

    def __call__(self, target, pin, args, on_start=None, timeout=None, cancel=None):
        assert args[0] == "--job" and len(args) == 2, args
        job = dict(row.split(" ", 1) for row in
                   open(os.path.join(self.tree.fdir, target, "jobs", args[1])).read().splitlines())
        mode = job["mode"]
        self.calls.append((target, pin, mode))
        run = os.path.join(self.tree.fdir, target, "runs", f"fake-{len(self.calls)}-{pin}-1")
        record(run)
        data = open(job["input"], "rb").read()
        sha = hashlib.sha256(data).hexdigest()
        if mode.startswith("minimize"):
            json.dump({"mode": "minimize"}, open(os.path.join(run, "verdict.json"), "w"))
            if not self.minimize_ok:
                return Leg(target, pin, "node-fuzz fake", 1, run, {"mode": "minimize"})
            small = data[: self.shrink]
            open(os.path.join(run, "crashes", "minimized"), "wb").write(small)
            if self.keep_class:
                self.tree.behaviour[hashlib.sha256(small).hexdigest()] = self.tree.behaviour[sha]
            return Leg(target, pin, "node-fuzz fake", 0, run, {"mode": "minimize"})
        assert mode == "replay", mode
        json.dump({"mode": "replay"}, open(os.path.join(run, "verdict.json"), "w"))
        found = None if sha in self.fixed else self.tree.behaviour.get(sha)
        if found and found[0] == "divergence":
            record(run, divergences=[(data, found[1])], pin=pin)
            cmd = self.cmds.get((sha, pin), self.cmds.get(sha))
            if cmd:
                with open(os.path.join(run, "divergences", sha + ".txt"), "a") as f:
                    f.write(f"cmd {cmd}\n")
        elif found:
            record(run, crashes=[("crash-aaa", data)], log=found[1], pin=pin)
        return Leg(target, pin, "node-fuzz fake", 1 if found else 0, run, {
            "rust_dashcore": "fixedrev", "divergences": int(bool(found) and found[0] == "divergence"),
            "crashes": int(bool(found) and found[0] == "crash")})


# Synthetic: its ERROR lines come before the artifact writes, as for a crash; a timeout's real order
# (write first) is `InterruptedSlices.ALARM`.
PANIC_LOG = """INFO: Running with entropic power schedule
thread '<unnamed>' panicked at /work/home/someone/checkout/node-fuzz/src/models/kv_model.rs:48:19:
assertion `left == right` failed: 0x1234 secret 99
stack backtrace:
   0: rust_begin_unwind
   1: core::panicking::panic_fmt
   2: node_fuzz::models::kv_model::check::h0123456789abcdef
   3: node_fuzz::Harness::run
   4: rust_fuzzer_test_input
==4242== ERROR: libFuzzer: deadly signal
MS: 1 ChangeByte-; base unit: 0
0x41,0x42,
AB
artifact_prefix='/x/crashes/'; Test unit written to /x/crashes/crash-aaa
Base64: QUI=
==4243== ERROR: libFuzzer: timeout after 1200 seconds
artifact_prefix='/x/crashes/'; Test unit written to /x/crashes/timeout-bbb
diff_tx_decode: divergence limit reached
==4244== ERROR: libFuzzer: fuzz target exited
artifact_prefix='/x/crashes/'; Test unit written to /x/crashes/crash-ccc
"""


class Registry(unittest.TestCase):
    def test_the_tree_registry_agrees(self):
        self.assertEqual(fuzzctl.check_registry(), [])

    def test_registered_names_parse_a_slice_constant(self):
        src = 'pub const TARGETS: &[Target] = &[\n Target { name: "a", compare: f },\n' \
              ' Target {\n name: "b_c", compare: g },\n];\npub fn x() { name: "zz" }'
        self.assertEqual(fuzzctl.registered_names(src, "TARGETS"), ["a", "b_c"])
        self.assertIsNone(fuzzctl.registered_names(src, "MODELS"))

    def test_registered_pins_parse_literals_and_consts_in_any_order(self):
        src = 'const ALL: &[Pin] = &Pin::ALL;\nconst V24: &[Pin] = &[Pin::V24_0];\n' \
              'pub const TARGETS: &[Target] = &[\n    Target {\n        name: "a",\n        pins: &Pin::ALL,\n' \
              '        compare: f,\n    },\n    Target {\n        name: "b_c",\n        compare: g,\n' \
              '        pins: V24,\n    },\n    Target {\n        name: "d",\n        compare: h,\n' \
              '        pins: &[Pin::V24_0],\n    },\n    Target {\n        name: "e",\n        compare: i,\n    },\n];\n'
        self.assertEqual(fuzzctl.registered_pins(src, "TARGETS"),
                         {"a": fuzzctl.PINS, "b_c": ("v24.0",), "d": ("v24.0",)})
        self.assertIsNone(fuzzctl.registered_pins(src, "MODELS"))

    def test_registered_pins_ignore_comments(self):
        src = 'const V24: &[Pin] = &[Pin::V24_0];\npub const TARGETS: &[Target] = &[\n    Target {\n' \
              '        name: "a",\n        // pins: &Pin::ALL, in a comment\n        compare: f,\n' \
              '        pins: V24, // not v23.1\n    },\n];\n'
        self.assertEqual(fuzzctl.registered_pins(src, "TARGETS"), {"a": ("v24.0",)})

    def write_registry(self, body):
        d = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, d)
        path = os.path.join(d, "targets.toml")
        open(path, "w").write(body)
        return path

    def test_malformed_entries_are_refused(self):
        ok = '[[target]]\nname = "a"\nowner = "dash-consensus"\nkind = "l1"\npins = ["v24.0"]\n'
        self.assertEqual(fuzzctl.load_registry(self.write_registry(ok))[0].route, "private")
        self.assertEqual(fuzzctl.load_registry(self.write_registry(ok))[0].min_certify_execs, 0)
        floor = fuzzctl.load_registry(self.write_registry(ok + "min_certify_execs = 20000\n"))
        self.assertEqual(floor[0].min_certify_execs, 20000)
        self.assertEqual(fuzzctl.load_registry(self.write_registry(ok))[0].plateau, fuzzctl.Plateau(0, True))
        rule = fuzzctl.load_registry(self.write_registry(ok + "plateau_cov_slack = 6\nplateau_ft = false\n"))
        self.assertEqual(rule[0].plateau, fuzzctl.Plateau(6, False))
        self.assertEqual(fuzzctl.load_registry(self.write_registry(ok))[0].replay, fuzzctl.Replay(0, 0))
        rule = fuzzctl.load_registry(self.write_registry(ok + "replay_cov_slack = 30\nreplay_ft_slack = 120\n"))
        self.assertEqual((rule[0].replay, rule[0].plateau), (fuzzctl.Replay(30, 120), fuzzctl.Plateau()))
        caps = lambda t: (t.smoke_corpus_max, t.smoke_corpus_max_bytes)
        self.assertEqual(caps(fuzzctl.load_registry(self.write_registry(ok))[0]),
                         (fuzzctl.SMOKE_CORPUS_MAX, fuzzctl.SMOKE_CORPUS_MAX_BYTES))
        sample = fuzzctl.load_registry(self.write_registry(ok + "smoke_corpus_max = 0\nsmoke_corpus_max_bytes = 512\n"))
        self.assertEqual(caps(sample[0]), (0, 512))
        for bad in (
            ok.replace('"l1"', '"other"'),
            ok.replace('"v24.0"', '"v25.0"'),
            ok + 'route = "secret"\n',
            ok + "smoke = -1\n",
            ok + "colour = 1\n",
            ok.replace('"l1"', '"l1"\nroute = "public"'),
            ok + ok,
            '[[target]]\nname = "s"\nowner = "x"\nkind = "selftest"\npins = ["v24.0"]\n',
            '[[target]]\nname = "s"\nowner = "x"\nkind = "selftest"\nsmoke = 5\n',
            '[[target]]\nname = "m"\nowner = "x"\nkind = "model"\n',
            ok.replace('owner = "dash-consensus"\n', ""),
            ok.replace('"dash-consensus"', '"Bad Owner"'),
            ok + "min_certify_execs = 0\n",
            ok + "min_certify_execs = 2.5\n",
            ok + 'min_certify_execs = "many"\n',
            ok + "plateau_cov_slack = -1\n",
            ok + "plateau_cov_slack = 2.5\n",
            ok + "plateau_cov_slack = 65\n",
            ok + "plateau_cov_slack = true\n",
            ok + "plateau_ft = 0\n",
            ok + "replay_cov_slack = -1\n",
            ok + "replay_cov_slack = 65\n",
            ok + "replay_cov_slack = true\n",
            ok + "replay_ft_slack = 1001\n",
            ok + "replay_ft_slack = 0.5\n",
            ok + 'replay_ft_slack = "auto"\n',
            ok + "smoke_corpus_max = -1\n",
            ok + "smoke_corpus_max = 2.5\n",
            ok + "smoke_corpus_max = true\n",
            ok + 'smoke_corpus_max_bytes = "1M"\n',
        ):
            with self.assertRaises(SystemExit, msg=bad), contextlib.redirect_stderr(io.StringIO()):
                fuzzctl.load_registry(self.write_registry(bad))

    def test_a_disagreeing_tree_is_reported(self):
        root = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, root)
        for d in ("node-fuzz", "node-l1/src"):
            shutil.copytree(os.path.join(fuzzctl.ROOT, d), os.path.join(root, d),
                            ignore=shutil.ignore_patterns("target"))
        for t in fuzzctl.load_registry():
            for d in t.public_seeds:
                # Seeds under node-fuzz/ are already in the copy of that directory.
                shutil.copytree(os.path.join(fuzzctl.ROOT, d), os.path.join(root, d),
                                dirs_exist_ok=True)
            os.makedirs(os.path.join(root, t.owner), exist_ok=True)
            shutil.copyfile(os.path.join(fuzzctl.ROOT, t.owner, "Cargo.toml"),
                            os.path.join(root, t.owner, "Cargo.toml"))
            if t.honggfuzz:
                os.makedirs(os.path.dirname(os.path.join(root, t.honggfuzz)), exist_ok=True)
                shutil.copyfile(os.path.join(fuzzctl.ROOT, t.honggfuzz), os.path.join(root, t.honggfuzz))
        self.assertEqual(fuzzctl.check_registry(root), [])
        # A node crate's honggfuzz target that no entry names; upstream's are not node crates'.
        for d in ("dashd_store", "hashes"):
            os.makedirs(os.path.join(root, "fuzz", "fuzz_targets", d), exist_ok=True)
            open(os.path.join(root, "fuzz", "fuzz_targets", d, "new.rs"), "w").write("")
        self.assertEqual(fuzzctl.check_registry(root), [
            "fuzz/fuzz_targets/dashd_store/new.rs is a node crate's fuzz target that no targets.toml entry names"])
        os.remove(os.path.join(root, "fuzz", "fuzz_targets", "dashd_store", "new.rs"))
        here = os.path.join(root, "node-fuzz")
        open(os.path.join(here, "fuzz_targets", "stray.rs"), "w").write("")
        reg = os.path.join(here, "targets.toml")
        open(reg, "a").write('\n[[target]]\nname = "ghost"\nowner = "node-fuzz"\nkind = "model"\npins = ["v24.0"]\n')
        kv = os.path.join(here, "fuzz_targets", "kv_model.rs")
        open(kv, "w").write('node_fuzz::l1_target!("kv_model");\n')
        problems = "\n".join(fuzzctl.check_registry(root))
        self.assertIn("stray.rs is not a [[bin]]", problems)
        self.assertIn("lists ghost, which is not a [[bin]]", problems)
        self.assertIn("ghost is a model target in targets.toml but not in MODELS", problems)
        self.assertIn("kv_model: fuzz_targets/kv_model.rs does not define one model target", problems)
        lib = os.path.join(root, "node-l1", "src", "lib.rs")
        src = open(lib).read()
        entry = 'name: "diff_payload_decode",\n        compare: compare_payload,\n        pins: V24,'
        self.assertIn(entry, src)
        open(lib, "w").write(src.replace(entry, entry.replace("pins: V24,", "pins: &[Pin::V23_1],")))
        self.assertIn("diff_payload_decode: the pins of its TARGETS entry", "\n".join(fuzzctl.check_registry(root)))

    def test_legs_follow_the_budgets(self):
        self.assertEqual([(t.name, p) for t, p in fuzzctl.legs(TARGETS, "smoke")],
                         [("diff_tx_decode", "v25.0"), ("diff_tx_decode", "v24.0"), ("kv_model", "v24.0")])


class Signatures(unittest.TestCase):
    def test_crash_classes_keep_code_locations_only(self):
        sigs = fuzzctl.crash_signatures(PANIC_LOG)
        self.assertEqual(sigs["crash-aaa"],
                         "panic node-fuzz/src/models/kv_model.rs:48 < node_fuzz::models::kv_model::check")
        self.assertEqual(sigs["timeout-bbb"], "timeout after N seconds")
        self.assertIsNone(sigs["crash-ccc"])  # the harness's divergence limit, not a finding
        for leaked in ("secret", "0x1234", "99", "QUI", "0x41", "someone"):
            self.assertNotIn(leaked, " ".join(filter(None, sigs.values())))

    def test_an_unexplained_crash_has_a_fixed_class(self):
        self.assertEqual(fuzzctl.signature(["something"]), "unknown")


class Windows(unittest.TestCase):
    def test_rows_comments_and_utc(self):
        w = fuzzctl.parse_windows("# reserved\n2026-10-10T00:00Z 2026-10-10T06:00Z m3\n\n"
                                  "2026-10-11T00:00 2026-10-11T01:00  # naive is UTC\n")
        self.assertEqual(len(w), 2)
        self.assertEqual(w[1][1] - w[1][0], 3600)
        t0 = w[0][0]
        self.assertEqual(fuzzctl.next_window(w, t0 - 600, 300), None)
        self.assertEqual(fuzzctl.next_window(w, t0 - 600, 900), w[0])
        self.assertEqual(fuzzctl.next_window(w, t0 + 60, 1), w[0])

    def test_malformed_rows_raise(self):
        for bad in ("2026-10-10T00:00Z", "2026-10-10T06:00Z 2026-10-10T00:00Z", "x y", "a b c d"):
            with self.assertRaises(ValueError, msg=bad):
                fuzzctl.parse_windows(bad)

    def test_bench_yields_to_windows_and_live_runs(self):
        d = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, d)
        proc = os.path.join(d, "proc")
        os.makedirs(os.path.join(proc, "100"))
        open(os.path.join(proc, "100", "environ"), "wb").write(b"PATH=/bin\0HOME=/h\0")
        path = os.path.join(d, "windows")
        bench = fuzzctl.Bench(path, proc=proc, clock=lambda: 1_000_000.0)
        self.assertIsNone(bench.blocked(3600))  # no file: nothing reserved
        open(path, "w").write("1970-01-12T15:00:00Z 1970-01-12T16:00:00Z\n")  # 1004400
        self.assertIsNone(bench.blocked(4400))
        reason, until = bench.blocked(5000)
        self.assertEqual((reason, until), ("a reserved bench window", 4400))
        open(path, "w").write("garbage\n")
        self.assertIn("unreadable", bench.blocked(10)[0])
        os.remove(path)
        os.makedirs(os.path.join(proc, "200"))
        open(os.path.join(proc, "200", "environ"), "wb").write(b"A=1\0NODE_BENCH_MARK=xyz\0")
        self.assertEqual(fuzzctl.live_bench_pids(proc), [200])
        self.assertEqual(bench.blocked(10), ("node-bench is running", 0))


class Planning(unittest.TestCase):
    def test_shares_follow_the_weights(self):
        plan = fuzzctl.plan_legs(TARGETS, 90)
        self.assertEqual(plan, {"diff_tx_decode@v25.0": 40, "diff_tx_decode@v24.0": 40,
                                "kv_model@v24.0": 10})

    def test_the_leg_furthest_behind_goes_next(self):
        plan = {"a": 10, "b": 10, "c": 1}
        self.assertEqual(fuzzctl.next_leg(plan, {"a": 5, "b": 2, "c": 0.9}, {}), "b")
        self.assertEqual(fuzzctl.next_leg(plan, {"a": 5, "b": 2, "c": 0.1}, {}), "c")
        self.assertEqual(fuzzctl.next_leg(plan, {"a": 5, "b": 2, "c": 0.1}, {"c": 1, "b": 1}), "a")
        self.assertIsNone(fuzzctl.next_leg(plan, {"a": 10, "b": 10, "c": 1}, {}))


class Triaging(Tree):
    def triage(self, runner, recheck=False):
        return fuzzctl.run_triage(TARGETS, recheck=recheck, runner=runner)

    def test_findings_cluster_minimise_and_route(self):
        self.make_run(PRIV.name, "r1", pin="v25.0", divergences=[(b"input-one", "txid")])
        self.make_run(PRIV.name, "r2", pin="v24.0",
                      divergences=[(b"input-two", "txid"), (b"other-3", "decode verdict")])
        self.make_run(PRIV.name, "m1", mode="minimize", crashes=[("crash-x", b"zz")])
        self.make_run(PUB.name, "k1", crashes=[("crash-aaa", b"kv-crash"), ("crash-ccc", b"limit")],
                      log=PANIC_LOG)
        runner = FakeRunner(self)
        tri = self.triage(runner)
        # m1 is triage's own kind of run; crash-ccc is the harness's divergence limit.
        self.assertEqual(tri.stats["runs"], 3)
        self.assertEqual((tri.stats["findings"], tri.stats["new_clusters"], tri.stats["minimized"]),
                         (4, 3, 3))
        txid = [c for c in self.state(PRIV.name)["clusters"].values() if c["class"] == "txid"][0]
        self.assertEqual((txid["hits"], sorted(txid["pins"]), txid["minimized"]),
                         (2, ["v24.0", "v25.0"], True))
        vec = os.path.join(self.fdir, PRIV.name, "regress", "open", txid["vector"])
        self.assertEqual(open(vec, "rb").read(), b"in")
        self.assertEqual(stat.S_IMODE(os.stat(vec).st_mode), 0o600)
        self.assertEqual(stat.S_IMODE(os.stat(os.path.dirname(vec)).st_mode), 0o700)
        self.assertEqual(os.listdir(os.path.dirname(vec)).count(txid["vector"] + ".json"), 0)
        meta = os.path.join(self.fdir, PRIV.name, "regress", "meta", txid["vector"] + ".json")
        self.assertEqual(json.load(open(meta))["minimized"], True)
        # The minimised result was replayed to check its class.
        self.assertIn(("diff_tx_decode", "v25.0", "minimize-divergence"), runner.calls)
        self.assertIn(("diff_tx_decode", "v25.0", "replay"), runner.calls)
        self.assertIn(("kv_model", "v24.0", "minimize"), runner.calls)
        kv = list(self.state(PUB.name)["clusters"].values())
        self.assertEqual([c["class"] for c in kv],
                         ["panic node-fuzz/src/models/kv_model.rs:48 < node_fuzz::models::kv_model::check"])
        # Already triaged runs are not triaged again.
        self.assertEqual(self.triage(runner).stats["runs"], 0)

        lines, _ = fuzzctl.status_lines(self.fdir, TARGETS)
        text = "\n".join(lines)
        self.assertEqual(lines[0], "SW-FUZZ open findings: 3 (private 2, public 1); fixed 0")
        self.assertIn("kv_model crash [panic node-fuzz/src/models/kv_model.rs:48", text)
        self.assertIn("TASK fuzz-private: 2 open", text)
        for private_detail in ("txid", "decode verdict", "input-", txid["vector"][:12], "v25.0"):
            self.assertNotIn(private_detail, text)

    def test_a_legacy_pin_is_counted_but_never_clustered(self):
        # DEC-82 dropped v23.1. A finding in a v23.1 run opens no cluster and starts no job, so
        # it cannot leave an open cluster that no vector can close.
        self.make_run(PRIV.name, "r1", pin="v23.1", divergences=[(b"input-one", "txid")])
        runner = FakeRunner(self)
        tri = self.triage(runner)
        self.assertEqual((tri.stats["findings"], tri.stats["new_clusters"]), (1, 0))
        self.assertEqual(self.state(PRIV.name)["clusters"], {})
        self.assertEqual(runner.calls, [])

    def test_triage_state_naming_a_legacy_pin_still_loads(self):
        self.make_run(PRIV.name, "r1", divergences=[(b"input-one", "txid")])
        self.triage(FakeRunner(self))
        state = self.state(PRIV.name)
        (cluster,) = state["clusters"].values()
        cluster["pins"] = ["v23.1", "v24.0"]
        with open(os.path.join(self.fdir, PRIV.name, "triage.json"), "w") as f:
            json.dump(state, f)
        fuzzctl.load_triage(self.fdir, PRIV.name)

    def test_a_minimisation_that_changes_class_is_not_kept(self):
        self.make_run(PRIV.name, "r1", divergences=[(b"abcdef", "size")])
        tri = self.triage(FakeRunner(self, keep_class=False))
        self.assertEqual((tri.stats["minimized"], tri.stats["minimize_failed"]), (0, 1))
        c = list(self.state(PRIV.name)["clusters"].values())[0]
        self.assertEqual((c["minimized"], c["attempts"]), (False, 1))
        self.assertEqual(open(os.path.join(self.fdir, PRIV.name, "regress", "open", c["vector"]),
                              "rb").read(), b"abcdef")

    def test_failed_minimisations_are_capped(self):
        runner = FakeRunner(self, minimize_ok=False)
        self.make_run(PRIV.name, "r1", divergences=[(b"abcdef", "size"), (b"abcdeg", "size")])
        self.triage(runner)
        self.assertEqual(len(runner.calls), 1)  # once per cluster per pass
        for n in range(2, 5):
            self.make_run(PRIV.name, f"r{n}", divergences=[(b"abcdef", "size")])
            self.triage(runner)
        self.assertEqual(len(runner.calls), fuzzctl.MAX_ATTEMPTS)
        self.assertEqual(list(self.state(PRIV.name)["clusters"].values())[0]["hits"], 5)

    def test_a_bad_state_file_is_counted_not_fatal(self):
        self.make_run(PRIV.name, "r1", divergences=[(b"abcdef", "size")])
        open(os.path.join(self.fdir, PRIV.name, "triage.json"), "w").write('{"clusters": []}')
        self.make_run(PUB.name, "k1", crashes=[("crash-aaa", b"kv-crash")], log=PANIC_LOG)
        tri = self.triage(FakeRunner(self))
        self.assertEqual((tri.stats["errors"], tri.stats["new_clusters"]), (1, 1))

    def test_recheck_fixes_and_a_regression_reopens(self):
        self.make_run(PRIV.name, "r1", divergences=[(b"abcdef", "size"), (b"other", "txid")])
        runner = FakeRunner(self)
        self.triage(runner)
        by = {c["class"]: c for c in self.state(PRIV.name)["clusters"].values()}
        runner.fixed.add(hashlib.sha256(open(self.vector_path(PRIV.name, by["size"]), "rb").read()).hexdigest())
        tri = self.triage(runner, recheck=True)
        self.assertEqual(tri.stats["fixed_now"], 1)
        regress = os.path.join(self.fdir, PRIV.name, "regress")
        self.assertTrue(os.path.isfile(os.path.join(regress, "fixed", by["size"]["vector"])))
        self.assertTrue(os.path.isfile(os.path.join(regress, "open", by["txid"]["vector"])))
        self.assertEqual(fuzzctl.status_lines(self.fdir, TARGETS)[0][0],
                         "SW-FUZZ open findings: 1 (private 1, public 0); fixed 1")
        # A run from before the fix that is triaged after it does not reopen the cluster.
        self.make_run(PRIV.name, "20000101T000000Z-v24.0-7", divergences=[(b"old-run", "size")])
        self.triage(runner)
        size_id = fuzzctl.cluster_id(PRIV.name, "divergence", "size")
        self.assertEqual(self.state(PRIV.name)["clusters"][size_id]["state"], "fixed")
        # A later run diverges in the same class on another input: the cluster reopens with the
        # new input as its open vector, and the old one stays in fixed/.
        self.make_run(PRIV.name, "r2", divergences=[(b"zz-new", "size")])
        self.triage(runner)
        size = self.state(PRIV.name)["clusters"][size_id]
        self.assertEqual((size["state"], open(os.path.join(regress, "open", size["vector"]), "rb").read()),
                         ("open", b"zz"))
        self.assertTrue(os.path.isfile(os.path.join(regress, "fixed", by["size"]["vector"])))
        self.assertEqual(fuzzctl.status_lines(self.fdir, TARGETS)[0][0],
                         "SW-FUZZ open findings: 2 (private 2, public 0); fixed 0")

    def unfinished(self, runner, line, code, verdict, pins=None):
        """`runner` with the replays on `pins` (all by default) ending as run.sh says in `line`."""
        def wrapped(target, pin, args, **kw):
            leg = runner(target, pin, args, **kw)
            if pins is not None and pin not in pins:
                return leg
            return dataclasses_replace(leg, line=line, code=code, verdict=verdict)
        return wrapped

    def test_an_unfinished_recheck_is_an_error_not_a_diverging_cluster(self):
        # BL-174: a replay that timed out, was cancelled or ended without a verdict is a triage
        # error with a status of its own; only a verdict makes a cluster diverging or fixed.
        self.make_run(PRIV.name, "r1", divergences=[(b"abcdef", "size")])
        fuzzctl.run_triage(TARGETS, minimize=False)
        cid = fuzzctl.cluster_id(PRIV.name, "divergence", "size")
        cases = {"timeout": ("run.sh: timed out", 124, {}), "cancel": ("run.sh: cancelled", 130, {}),
                 "no verdict": ("run.sh exited 1 with no summary", 1, {}),
                 "killed": ("run.sh: killed", -9, {"rust_dashcore": "x"}),
                 # run.sh fails a replay on a kill of the target or a refusal as well: no finding.
                 "failed without a finding": ("node-fuzz fake status=fail", 1, {"rust_dashcore": "x", "divergences": 0,
                                                                              "crashes": 0, "sink_failures": 0})}
        for name, case in cases.items():
            with self.subTest(name):
                runner = FakeRunner(self)
                runner.fixed.add(hashlib.sha256(b"abcdef").hexdigest())  # it would read as fixed
                tri = fuzzctl.run_triage(TARGETS, recheck=True, minimize=False, runner=self.unfinished(runner, *case))
                self.assertEqual((tri.stats["errors"], tri.stats["recheck_unfinished"], tri.stats["fixed_now"]),
                                 (1, 1, 0))
                self.assertIn("recheck_unfinished=1", tri.summary())
                c = self.state(PRIV.name)["clusters"][cid]
                self.assertEqual(c["state"], "open")
                self.assertTrue(os.path.isfile(self.vector_path(PRIV.name, c)))
        # The exit status of the command: non-zero.
        runner = self.unfinished(FakeRunner(self), "run.sh: timed out", 124, {})
        orig = fuzzctl.run_triage
        with mock.patch.object(fuzzctl, "run_triage", lambda *a, **k: orig(*a, runner=runner, **k)):
            self.assertEqual(self.cli(["triage", "--recheck", "--no-minimize"])[0], 1)
        # A real verdict still counts: a diverging replay is no error, a passing one fixes.
        runner = FakeRunner(self)
        tri = fuzzctl.run_triage(TARGETS, recheck=True, minimize=False, runner=runner)
        self.assertEqual((tri.stats["errors"], tri.stats["recheck_unfinished"], tri.stats["fixed_now"]), (0, 0, 0))
        runner.fixed.add(hashlib.sha256(b"abcdef").hexdigest())
        tri = fuzzctl.run_triage(TARGETS, recheck=True, minimize=False, runner=runner)
        self.assertEqual((tri.stats["errors"], tri.stats["fixed_now"]), (0, 1))

    def test_a_farm_stop_during_the_recheck_is_no_error_and_keeps_the_cluster_open(self):
        # The farm yields (a bench run starts, SIGTERM) while the launch recheck replays: run_leg
        # returns "cancelled". That is not a triage error, and no further pin or cluster is replayed.
        self.make_run(PRIV.name, "r1", pin="v25.0", divergences=[(b"abcdef", "size")])
        self.make_run(PRIV.name, "r2", pin="v24.0", divergences=[(b"abcdef", "size")])
        self.make_run(PRIV.name, "r3", pin="v24.0", divergences=[(b"other", "txid")])
        fuzzctl.run_triage(TARGETS, minimize=False)
        runner = FakeRunner(self)
        runner.fixed |= {hashlib.sha256(b"abcdef").hexdigest(), hashlib.sha256(b"other").hexdigest()}
        stopped = []

        def yielding(target, pin, args, **kw):
            leg = runner(target, pin, args, **kw)
            stopped.append(pin)
            return dataclasses_replace(leg, line="run.sh: cancelled", code=130, verdict={})

        tri = fuzzctl.run_triage(TARGETS, recheck=True, minimize=False, runner=yielding, stop=lambda: bool(stopped))
        self.assertEqual((tri.stats["errors"], tri.stats["recheck_unfinished"], tri.stats["fixed_now"]), (0, 0, 0))
        self.assertEqual(len(stopped), 1)
        self.assertEqual({c["state"] for c in self.state(PRIV.name)["clusters"].values()}, {"open"})
        for c in self.state(PRIV.name)["clusters"].values():
            self.assertTrue(os.path.isfile(self.vector_path(PRIV.name, c)))
        # The same without the farm stopping is an error: a timeout is.
        runner2 = self.unfinished(FakeRunner(self), "run.sh: timed out", 124, {})
        tri = fuzzctl.run_triage(TARGETS, recheck=True, minimize=False, runner=runner2)
        self.assertEqual((tri.stats["errors"], tri.stats["recheck_unfinished"]), (2, 2))

    def test_a_stop_after_one_pin_passed_fixes_nothing(self):
        # One pin passed with a verdict, then the farm stopped (and `stop` flipped back at once,
        # as a bench window can): the pins not replayed are unknown, so the cluster stays open.
        self.make_run(PRIV.name, "r1", pin="v25.0", divergences=[(b"abcdef", "size")])
        self.make_run(PRIV.name, "r2", pin="v24.0", divergences=[(b"abcdef", "size")])
        fuzzctl.run_triage(TARGETS, minimize=False)
        runner = FakeRunner(self)
        runner.fixed.add(hashlib.sha256(b"abcdef").hexdigest())  # every pin would pass
        seen, polls = [], iter([False, True, False, False])

        def runner_then_stop(target, pin, args, **kw):
            seen.append(pin)
            return runner(target, pin, args, **kw)

        tri = fuzzctl.run_triage(TARGETS, recheck=True, minimize=False, runner=runner_then_stop,
                                 stop=lambda: next(polls, False))
        self.assertEqual(len(seen), 1)
        self.assertEqual((tri.stats["errors"], tri.stats["recheck_unfinished"], tri.stats["fixed_now"]), (0, 0, 0))
        c = list(self.state(PRIV.name)["clusters"].values())[0]
        self.assertEqual(c["state"], "open")
        self.assertTrue(os.path.isfile(self.vector_path(PRIV.name, c)))

    def test_a_cluster_is_fixed_only_when_every_pin_has_a_passing_verdict(self):
        self.make_run(PRIV.name, "r1", pin="v25.0", divergences=[(b"abcdef", "size")])
        self.make_run(PRIV.name, "r2", pin="v24.0", divergences=[(b"abcdef", "size")])
        fuzzctl.run_triage(TARGETS, minimize=False)
        cid = fuzzctl.cluster_id(PRIV.name, "divergence", "size")
        runner = FakeRunner(self)
        runner.fixed.add(hashlib.sha256(b"abcdef").hexdigest())  # both pins would pass
        # v24.0 passes, v25.0 times out: nothing is known of v25.0, so the cluster stays open.
        mixed = self.unfinished(runner, "run.sh: timed out", 124, {}, pins=("v25.0",))
        tri = fuzzctl.run_triage(TARGETS, recheck=True, minimize=False, runner=mixed)
        self.assertEqual((tri.stats["errors"], tri.stats["recheck_unfinished"], tri.stats["fixed_now"]), (1, 1, 0))
        c = self.state(PRIV.name)["clusters"][cid]
        self.assertEqual(c["state"], "open")
        self.assertTrue(os.path.isfile(self.vector_path(PRIV.name, c)))

    def test_a_replay_that_diverges_wins_over_an_unfinished_one_on_another_pin(self):
        self.make_run(PRIV.name, "r1", pin="v25.0", divergences=[(b"abcdef", "size")])
        self.make_run(PRIV.name, "r2", pin="v24.0", divergences=[(b"abcdef", "size")])
        fuzzctl.run_triage(TARGETS, minimize=False)
        mixed = self.unfinished(FakeRunner(self), "run.sh: timed out", 124, {}, pins=("v25.0",))
        tri = fuzzctl.run_triage(TARGETS, recheck=True, minimize=False, runner=mixed)
        self.assertEqual((tri.stats["errors"], tri.stats["recheck_unfinished"], tri.stats["fixed_now"]), (0, 0, 0))
        self.assertEqual(list(self.state(PRIV.name)["clusters"].values())[0]["state"], "open")

    def test_an_unfinished_minimisation_does_not_use_up_an_attempt(self):
        self.make_run(PRIV.name, "r1", divergences=[(b"abcdef", "size")])
        runner = FakeRunner(self)

        def timed_out(target, pin, args, **kw):
            leg = runner(target, pin, args, **kw)
            if leg.verdict.get("mode") == "minimize":
                return dataclasses_replace(leg, line="run.sh: timed out", code=124, verdict={})
            return leg

        tri = fuzzctl.run_triage(TARGETS, runner=timed_out)
        c = list(self.state(PRIV.name)["clusters"].values())[0]
        self.assertEqual((tri.stats["minimize_failed"], c["attempts"], c["minimized"]), (1, 0, False))
        # A minimisation that ran to a verdict and failed does.
        runner.minimize_ok = False
        self.make_run(PRIV.name, "r2", divergences=[(b"abcdeg", "size")])
        fuzzctl.run_triage(TARGETS, runner=runner)
        self.assertEqual(list(self.state(PRIV.name)["clusters"].values())[0]["attempts"], 1)

    def test_model_clusters_are_keyed_by_cmd(self):
        # BL-174: divergences in one field on different commands are different clusters on a
        # model target; an L1 target keeps keying on the field alone.
        for target in (PUB, PRIV):
            run = self.make_run(target.name, "r1", divergences=[(b"in-ping", "log line"), (b"in-pong", "log line")])
            for data, cmd in ((b"in-ping", "ping"), (b"in-pong", "pong")):
                with open(os.path.join(run, "divergences", hashlib.sha256(data).hexdigest() + ".txt"), "a") as f:
                    f.write(f"cmd {cmd}\n")
        fuzzctl.run_triage(TARGETS, minimize=False)
        self.assertEqual(sorted(c["class"] for c in self.state(PUB.name)["clusters"].values()),
                         ["log line cmd=ping", "log line cmd=pong"])
        self.assertEqual([c["class"] for c in self.state(PRIV.name)["clusters"].values()], ["log line"])
        # Each keeps its own vector, and the state stays valid (the id is the hash of the class).
        for c in self.state(PUB.name)["clusters"].values():
            self.assertTrue(os.path.isfile(self.vector_path(PUB.name, c)))
        self.assertEqual(fuzzctl.status_lines(self.fdir, TARGETS)[1], 0)

    def field_only_model_cluster(self, *inputs, pin="v24.0"):
        """Triages divergences whose sidecars have no `cmd` (runs from before the sink wrote one)
        on the model target: field-only clusters, one per field."""
        run = self.make_run(PUB.name, "r1", pin=pin, divergences=list(inputs))
        fuzzctl.run_triage(TARGETS, minimize=False)
        return run

    def clusters_by_class(self, target=PUB.name):
        return {c["class"]: (cid, c) for cid, c in self.state(target)["clusters"].items()}

    def test_an_open_field_only_model_cluster_is_re_keyed_by_its_replayed_command(self):
        # BL-267 (d): the replay of its vector names the command; the cluster, vector and
        # provenance move to the `cmd=` id, with no duplicate left.
        self.field_only_model_cluster((b"in-ping", "log line"))
        old_cid, old = self.clusters_by_class()["log line"]
        runner = FakeRunner(self)
        runner.cmds[hashlib.sha256(b"in-ping").hexdigest()] = "ping"
        tri = fuzzctl.run_triage(TARGETS, recheck=True, minimize=False, runner=runner)
        self.assertEqual((tri.stats["migrated"], tri.stats["errors"]), (1, 0))
        by = self.clusters_by_class()
        self.assertEqual(list(by), ["log line cmd=ping"])
        cid, c = by["log line cmd=ping"]
        self.assertEqual(cid, fuzzctl.cluster_id(PUB.name, "divergence", "log line cmd=ping"))
        self.assertEqual((c["state"], c["hits"], c["pins"], c["first_seen"]),
                         ("open", old["hits"], old["pins"], old["first_seen"]))
        self.assertTrue(c["vector"].startswith(cid + "-"))
        regress = os.path.join(self.fdir, PUB.name, "regress")
        self.assertEqual(os.listdir(os.path.join(regress, "open")), [c["vector"]])
        self.assertEqual(os.listdir(os.path.join(regress, "meta")), [c["vector"] + ".json"])
        self.assertEqual(open(os.path.join(regress, "open", c["vector"]), "rb").read(), b"in-ping")
        meta = json.load(open(os.path.join(regress, "meta", c["vector"] + ".json")))
        self.assertEqual((meta["cluster"], meta["target"]), (cid, PUB.name))
        self.assertEqual(fuzzctl.status_lines(self.fdir, TARGETS)[1], 0)
        # Done once: the next recheck replays it as any open cluster and migrates nothing.
        tri = fuzzctl.run_triage(TARGETS, recheck=True, minimize=False, runner=runner)
        self.assertEqual((tri.stats["migrated"], tri.stats["errors"]), (0, 0))
        self.assertEqual(list(self.clusters_by_class()), ["log line cmd=ping"])

    def test_a_re_keyed_cluster_merges_into_the_cmd_cluster_that_exists(self):
        # The new sidecars opened `log line cmd=ping` from a run of the same input's class; the
        # old field-only cluster's vector replays to ping too: one cluster, hits summed, one vector.
        self.field_only_model_cluster((b"in-ping", "log line"))
        run = self.make_run(PUB.name, "r2", divergences=[(b"in-ping-2", "log line")])
        with open(os.path.join(run, "divergences", hashlib.sha256(b"in-ping-2").hexdigest() + ".txt"), "a") as f:
            f.write("cmd ping\n")
        fuzzctl.run_triage(TARGETS, minimize=False)
        by = self.clusters_by_class()
        self.assertEqual(sorted(by), ["log line", "log line cmd=ping"])
        hits = by["log line"][1]["hits"] + by["log line cmd=ping"][1]["hits"]
        kept_vector = by["log line cmd=ping"][1]["vector"]
        runner = FakeRunner(self)
        runner.cmds[hashlib.sha256(b"in-ping").hexdigest()] = "ping"
        runner.fixed.add(hashlib.sha256(b"in-ping-2").hexdigest())  # the other vector replays clean: it is fixed
        tri = fuzzctl.run_triage(TARGETS, recheck=True, minimize=False, runner=runner)
        self.assertEqual(tri.stats["migrated"], 1)
        by = self.clusters_by_class()
        self.assertEqual(list(by), ["log line cmd=ping"])
        cid, c = by["log line cmd=ping"]
        self.assertEqual(c["hits"], hits)
        self.assertEqual(c["state"], "open")
        # The destination was fixed by its own replay, so it reopens with the migrated vector.
        regress = os.path.join(self.fdir, PUB.name, "regress")
        self.assertEqual(open(os.path.join(regress, "open", c["vector"]), "rb").read(), b"in-ping")
        self.assertNotEqual(c["vector"], kept_vector)
        self.assertEqual(os.listdir(os.path.join(regress, "open")), [c["vector"]])
        self.assertEqual(fuzzctl.status_lines(self.fdir, TARGETS)[1], 0)

    def test_a_re_keyed_cluster_keeps_the_open_vector_of_the_cmd_cluster_that_exists(self):
        self.field_only_model_cluster((b"in-ping", "log line"))
        run = self.make_run(PUB.name, "r2", divergences=[(b"in-ping-2", "log line")])
        with open(os.path.join(run, "divergences", hashlib.sha256(b"in-ping-2").hexdigest() + ".txt"), "a") as f:
            f.write("cmd ping\n")
        fuzzctl.run_triage(TARGETS, minimize=False)
        kept = self.clusters_by_class()["log line cmd=ping"][1]["vector"]
        runner = FakeRunner(self)
        runner.cmds[hashlib.sha256(b"in-ping").hexdigest()] = "ping"
        tri = fuzzctl.run_triage(TARGETS, recheck=True, minimize=False, runner=runner)
        self.assertEqual((tri.stats["migrated"], tri.stats["errors"], tri.stats["fixed_now"]), (1, 0, 0))
        regress = os.path.join(self.fdir, PUB.name, "regress")
        self.assertEqual(os.listdir(os.path.join(regress, "open")), [kept])
        self.assertEqual(os.listdir(os.path.join(regress, "meta")), [kept + ".json"])
        self.assertEqual(list(self.clusters_by_class()), ["log line cmd=ping"])

    def test_clusters_that_are_not_open_field_only_model_divergences_are_not_re_keyed(self):
        # An L1 target (no commands), a replay naming two commands, and a replay naming none.
        self.make_run(PRIV.name, "r1", divergences=[(b"in-ping", "log line")])
        self.field_only_model_cluster((b"in-ping", "log line"), (b"in-two", "verdict"), (b"in-none", "peer state"))
        runner = FakeRunner(self)
        runner.cmds[hashlib.sha256(b"in-ping").hexdigest()] = "ping"
        tri = fuzzctl.run_triage(TARGETS, recheck=True, minimize=False, runner=runner)
        # `log line` on the model target migrates; the L1 target's cluster does not.
        self.assertEqual(tri.stats["migrated"], 1)
        self.assertEqual([c["class"] for c in self.state(PRIV.name)["clusters"].values()], ["log line"])
        self.assertEqual(sorted(self.clusters_by_class()), ["log line cmd=ping", "peer state", "verdict"])
        # Two pins that replay to two commands: ambiguous, so the cluster keeps its class.
        shutil.rmtree(os.path.join(self.fdir, PUB.name))
        self.make_run(PUB.name, "r1", pin="v24.0", divergences=[(b"in-two", "verdict")])
        self.make_run(PUB.name, "r2", pin="v25.0", divergences=[(b"in-two", "verdict")])
        two_pins = [PRIV, dataclasses_replace(PUB, pins=TEST_PINS), SELF]
        fuzzctl.run_triage(two_pins, minimize=False)
        sha = hashlib.sha256(b"in-two").hexdigest()
        two = FakeRunner(self)
        two.cmds[(sha, "v24.0")], two.cmds[(sha, "v25.0")] = "ping", "pong"
        tri = fuzzctl.run_triage(two_pins, recheck=True, minimize=False, runner=two)
        self.assertEqual((tri.stats["migrated"], tri.stats["errors"]), (0, 0))
        self.assertEqual(list(self.clusters_by_class()), ["verdict"])

    def test_a_cluster_whose_replays_did_not_all_finish_is_not_re_keyed(self):
        # One pin replays to ping, the other times out: the command is not known for the cluster.
        self.make_run(PUB.name, "r1", pin="v24.0", divergences=[(b"in-ping", "log line")])
        self.make_run(PUB.name, "r2", pin="v25.0", divergences=[(b"in-ping", "log line")])
        two_pins = [PRIV, dataclasses_replace(PUB, pins=TEST_PINS), SELF]
        fuzzctl.run_triage(two_pins, minimize=False)
        runner = FakeRunner(self)
        runner.cmds[hashlib.sha256(b"in-ping").hexdigest()] = "ping"

        def timing_out(target, pin, args, **kw):
            leg = runner(target, pin, args, **kw)
            return dataclasses_replace(leg, code=124, verdict={}) if pin == "v25.0" else leg

        tri = fuzzctl.run_triage(two_pins, recheck=True, minimize=False, runner=timing_out)
        self.assertEqual(tri.stats["migrated"], 0)
        self.assertEqual(list(self.clusters_by_class()), ["log line"])

    def test_a_minimisation_of_a_field_only_model_cluster_may_name_a_command(self):
        # BL-267: the minimised input replays to `log line cmd=ping`; that is still the cluster's field.
        self.make_run(PUB.name, "r1", divergences=[(b"in-ping-long", "log line")])
        runner = FakeRunner(self)
        runner.cmds[hashlib.sha256(b"in").hexdigest()] = "ping"
        tri = fuzzctl.run_triage(TARGETS, runner=runner)
        self.assertEqual((tri.stats["minimized"], tri.stats["minimize_failed"]), (1, 0))
        self.assertEqual(list(self.clusters_by_class()), ["log line"])

    def test_the_files_of_a_migration_killed_before_their_removal_are_removed(self):
        self.field_only_model_cluster((b"in-ping", "log line"))
        old_cid, old = self.clusters_by_class()["log line"]
        runner = FakeRunner(self)
        runner.cmds[hashlib.sha256(b"in-ping").hexdigest()] = "ping"
        fuzzctl.run_triage(TARGETS, recheck=True, minimize=False, runner=runner)
        regress = os.path.join(self.fdir, PUB.name, "regress")
        # The old vector and its provenance come back, as after a kill between the save and the removal.
        write = lambda sub, name: open(os.path.join(regress, sub, name), "w").write("x")
        write("open", old["vector"])
        write("meta", old["vector"] + ".json")
        tri = fuzzctl.run_triage(TARGETS, minimize=False)
        (c,) = self.state(PUB.name)["clusters"].values()
        self.assertEqual(os.listdir(os.path.join(regress, "open")), [c["vector"]])
        self.assertEqual(os.listdir(os.path.join(regress, "meta")), [c["vector"] + ".json"])
        self.assertEqual(tri.stats["errors"], 0)

    def vector_path(self, target, c):
        sub = "open" if c["state"] == "open" else "fixed"
        return os.path.join(self.fdir, target, "regress", sub, c["vector"])

    def test_clusters_sharing_an_input_each_keep_their_vector(self):
        # One input diverges in a different field on each pin: two clusters, one input.
        self.make_run(PRIV.name, "r1", pin="v25.0", divergences=[(b"same-input", "class-a")])
        self.make_run(PRIV.name, "r2", pin="v24.0", divergences=[(b"same-input", "class-b")])
        fuzzctl.run_triage(TARGETS, minimize=False)
        clusters = self.state(PRIV.name)["clusters"].values()
        self.assertEqual(len(clusters), 2)
        self.assertTrue(all(os.path.isfile(self.vector_path(PRIV.name, c)) for c in clusters))
        runner = FakeRunner(self)
        runner.fixed.add(hashlib.sha256(b"same-input").hexdigest())
        tri = fuzzctl.run_triage(TARGETS, recheck=True, minimize=False, runner=runner)
        self.assertEqual((tri.stats["fixed_now"], tri.stats["errors"]), (2, 0))
        for c in self.state(PRIV.name)["clusters"].values():
            self.assertEqual(c["state"], "fixed")
            self.assertTrue(os.path.isfile(self.vector_path(PRIV.name, c)), c["class"])

    def test_an_interrupted_promotion_is_repaired_and_a_lost_vector_is_an_error(self):
        self.make_run(PRIV.name, "r1", divergences=[(b"abcdef", "size"), (b"other", "txid")])
        fuzzctl.run_triage(TARGETS, minimize=False)
        state = self.state(PRIV.name)
        by = {c["class"]: c for c in state["clusters"].values()}
        # Killed after the state was saved, before the vector moved.
        by["size"].update(state="fixed", fixed_stamp="20261009T000000Z", fixed_at="abc")
        # And a vector lost outright.
        os.remove(self.vector_path(PRIV.name, by["txid"]))
        json.dump(state, open(os.path.join(self.fdir, PRIV.name, "triage.json"), "w"))
        tri = fuzzctl.run_triage(TARGETS, minimize=False)
        self.assertEqual((tri.stats["repaired"], tri.stats["errors"]), (1, 1))
        size = self.state(PRIV.name)["clusters"][fuzzctl.cluster_id(PRIV.name, "divergence", "size")]
        self.assertTrue(os.path.isfile(self.vector_path(PRIV.name, size)))

    def test_every_malformed_or_inconsistent_state_is_unknown_never_zero(self):
        self.make_run(PRIV.name, "r1", divergences=[(b"abcdef", "size")])
        fuzzctl.run_triage(TARGETS, minimize=False)
        path = os.path.join(self.fdir, PRIV.name, "triage.json")
        good = self.state(PRIV.name)
        cid, c = next(iter(good["clusters"].items()))
        meta = os.path.join(self.fdir, PRIV.name, "regress", "meta", c["vector"] + ".json")
        good_meta = open(meta).read()

        def with_cluster(**change):
            return {**good, "clusters": {cid: {**c, **change}}}

        cases = {
            "[]": [], "false": False, "0": 0, "empty string": "", "null": None,
            "clusters a list": {**good, "clusters": []},
            "runs not strings": {**good, "runs": [1]},
            "run_errors not counts": {**good, "run_errors": {"r9": "x"}},
            "cluster under a foreign id": {**good, "clusters": {"0" * 12: c}},
            "vector of another cluster": with_cluster(vector="0" * 12 + "-1"),
            "fixed without its promotion record": with_cluster(state="fixed"),
            "unknown state": with_cluster(state="maybe"),
        }
        for name, value in cases.items():
            json.dump(value, open(path, "w"))
            for argv in (["status-line"], ["triage", "--no-minimize"]):
                rc, out = self.cli(argv)
                self.assertNotEqual(rc, 0, (name, argv))
                self.assertIn("SW-FUZZ open findings: UNKNOWN", out, (name, argv))
        json.dump(good, open(path, "w"))
        for name, text in (("provenance missing", None), ("provenance of another cluster",
                           json.dumps({**json.loads(good_meta), "cluster": "0" * 12}))):
            if text is None:
                os.remove(meta)
            else:
                open(meta, "w").write(text)
            rc, out = self.cli(["status-line"])
            self.assertNotEqual(rc, 0, name)
            self.assertIn("UNKNOWN", out, name)
        open(meta, "w").write(good_meta)
        self.assertEqual(self.cli(["status-line"])[0], 0)

    def test_unreadable_state_fails_the_cli_and_is_never_reported_as_zero(self):
        self.make_run(PRIV.name, "r1", divergences=[(b"abcdef", "size")])
        fuzzctl.run_triage(TARGETS, minimize=False)
        open(os.path.join(self.fdir, PRIV.name, "triage.json"), "w").write("{broken")
        for argv in (["triage", "--no-minimize"], ["status-line"]):
            out = io.StringIO()
            with contextlib.redirect_stdout(out), mock.patch.object(fuzzctl, "load_registry", lambda: TARGETS):
                rc = fuzzctl.main(argv)
            self.assertNotEqual(rc, 0, argv)
            self.assertIn("SW-FUZZ open findings: UNKNOWN", out.getvalue(), argv)
            self.assertNotIn("SW-FUZZ open findings: 0", out.getvalue(), argv)

    def test_a_run_that_cannot_be_triaged_is_retried_then_kept_as_an_error(self):
        self.make_run(PRIV.name, "r1", divergences=[(b"abcdef", "size")])
        with mock.patch.object(fuzzctl, "findings_of", side_effect=OSError("gone")):
            tri = fuzzctl.run_triage(TARGETS, minimize=False)
        self.assertEqual(tri.stats["errors"], 1)
        self.assertNotIn("r1", self.state(PRIV.name)["runs"])  # pending, not lost
        tri = fuzzctl.run_triage(TARGETS, minimize=False)
        self.assertEqual((tri.stats["errors"], tri.stats["new_clusters"]), (0, 1))
        self.assertIn("r1", self.state(PRIV.name)["runs"])
        # A run that keeps failing becomes a persistent error, reported on every pass.
        self.make_run(PRIV.name, "r2", divergences=[(b"zzz", "txid")])
        with mock.patch.object(fuzzctl, "findings_of", side_effect=OSError("gone")):
            for _ in range(fuzzctl.RUN_RETRIES + 2):
                tri = fuzzctl.run_triage(TARGETS, minimize=False)
                self.assertEqual(tri.stats["errors"], 1)
        lines, errors = fuzzctl.status_lines(self.fdir, TARGETS)
        self.assertEqual(errors, 1)
        self.assertTrue(lines[0].startswith("SW-FUZZ open findings: UNKNOWN"), lines[0])

    def test_smoke_refuses_unknown_or_empty_selections(self):
        for only in (["typo_target"], ["selftest_mismatch"]):
            with contextlib.redirect_stdout(io.StringIO()) as out, \
                    mock.patch.object(fuzzctl, "run_leg") as run:
                self.assertFalse(fuzzctl.smoke(TARGETS, 2, selftest=False, only=only), only)
            run.assert_not_called()
            self.assertIn("status=fail", out.getvalue())

    def test_smoke_fails_when_its_triage_has_errors(self):
        failing = Leg(PUB.name, "v24.0", "node-fuzz fake", 1, "", {"crashes": 1})
        tri = mock.Mock(stats={"errors": 1})
        tri.summary.return_value = "node-fuzz triage errors=1"
        out = io.StringIO()
        with mock.patch.object(fuzzctl, "run_leg", return_value=failing), \
                mock.patch.object(fuzzctl, "run_triage", return_value=tri), \
                contextlib.redirect_stdout(out):
            self.assertFalse(fuzzctl.smoke(TARGETS, 2, selftest=False))
        self.assertIn("triage_errors=1", out.getvalue())

    def test_job_runs_are_never_triaged_as_fuzzing_runs(self):
        for name, marker in (("j1", "input"), ("j2", "mode")):
            run = os.path.join(self.fdir, PRIV.name, "runs", name)
            record(run, crashes=[("crash-xyz", b"aborted on purpose")],
                   log="==1== ERROR: libFuzzer: deadly signal\nTest unit written to /x/crash-xyz\n")
            if marker == "input":
                os.makedirs(os.path.join(run, "input"))
            else:
                open(os.path.join(run, "mode"), "w").write("minimize-divergence\n")
            old = time.time() - 2 * fuzzctl.Triage.stale_s
            os.utime(run, (old, old))
        tri = fuzzctl.run_triage(TARGETS, minimize=False)
        self.assertEqual((tri.stats["runs"], tri.stats["findings"]), (0, 0))
        self.assertEqual(sorted(self.state(PRIV.name)["runs"]), ["j1", "j2"])

    def test_a_kill_while_replacing_a_vector_is_repaired(self):
        self.make_run(PRIV.name, "r1", divergences=[(b"abcdef", "size")])
        fuzzctl.run_triage(TARGETS, minimize=False)
        state = self.state(PRIV.name)
        c = list(state["clusters"].values())[0]
        regress = os.path.join(self.fdir, PRIV.name, "regress", "open")
        # The new generation (and its provenance) was written and the old one deleted; the state
        # was not saved.
        newer = c["vector"][:-2] + "-2"
        os.replace(os.path.join(regress, c["vector"]), os.path.join(regress, newer))
        meta = os.path.join(self.fdir, PRIV.name, "regress", "meta")
        shutil.copyfile(os.path.join(meta, c["vector"] + ".json"), os.path.join(meta, newer + ".json"))
        tri = fuzzctl.run_triage(TARGETS, minimize=False)
        self.assertEqual((tri.stats["repaired"], tri.stats["errors"]), (1, 0))
        self.assertTrue(list(self.state(PRIV.name)["clusters"].values())[0]["vector"].endswith("-2"))

    def test_a_failed_run_triage_keeps_no_partial_state(self):
        self.make_run(PRIV.name, "r1", divergences=[(b"abcdef", "size")])
        with mock.patch.object(fuzzctl.Triage, "make_vector", side_effect=OSError("disk")):
            self.assertEqual(fuzzctl.run_triage(TARGETS, minimize=False).stats["errors"], 1)
        self.assertEqual(self.state(PRIV.name)["clusters"], {})
        fuzzctl.run_triage(TARGETS, minimize=False)
        self.assertEqual(list(self.state(PRIV.name)["clusters"].values())[0]["hits"], 1)

    def test_old_vector_names_and_malformed_clusters_are_errors(self):
        os.makedirs(os.path.join(self.fdir, PRIV.name))
        good = {"kind": "divergence", "class": "size", "pins": [], "hits": 1, "first_seen": "r",
                "state": "open", "vector": "a" * 64, "minimized": False, "attempts": 0}
        path = os.path.join(self.fdir, PRIV.name, "triage.json")
        json.dump({"clusters": {"0" * 12: good}, "runs": []}, open(path, "w"))
        self.assertGreaterEqual(fuzzctl.status_lines(self.fdir, TARGETS)[1], 1)
        json.dump({"clusters": {"0" * 12: {"kind": "divergence"}}, "runs": []}, open(path, "w"))
        with self.assertRaises(ValueError):
            fuzzctl.load_triage(self.fdir, PRIV.name)

    def test_the_cli_prints_no_exception_text(self):
        out, err = io.StringIO(), io.StringIO()
        with mock.patch.object(fuzzctl, "run_triage", side_effect=OSError("/private/secret-path")), \
                contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            self.assertEqual(fuzzctl.main(["triage"]), 3)
        self.assertNotIn("secret", out.getvalue() + err.getvalue())
        self.assertIn("OSError", err.getvalue())

    def test_runs_without_a_verdict_are_triaged_once_stale(self):
        run = self.make_run(PRIV.name, "20261009T000000Z-v25.0-9", pin="v25.0",
                            divergences=[(b"abcdef", "size")])
        os.remove(os.path.join(run, "verdict.json"))
        self.assertEqual(self.triage(FakeRunner(self)).stats["runs"], 0)
        old = time.time() - 2 * fuzzctl.Triage.stale_s
        os.utime(run, (old, old))
        self.assertEqual(self.triage(FakeRunner(self)).stats["runs"], 1)
        c = list(self.state(PRIV.name)["clusters"].values())[0]
        self.assertEqual(c["pins"], ["v25.0"])

    def test_export_copies_public_vectors_only(self):
        self.make_run(PRIV.name, "r1", divergences=[(b"private-bytes", "txid")])
        self.make_run(PUB.name, "k1", crashes=[("crash-aaa", b"kv-crash")], log=PANIC_LOG)
        self.triage(FakeRunner(self, shrink=4))
        dest = os.path.join(self.root, "export")
        with contextlib.redirect_stdout(open(os.devnull, "w")):
            fuzzctl.export_public(TARGETS, dest)
        self.assertEqual(sorted(os.listdir(dest)), ["kv_model"])
        files = os.listdir(os.path.join(dest, "kv_model", "open"))
        self.assertEqual(len(files), 1)
        self.assertEqual(open(os.path.join(dest, "kv_model", "open", files[0]), "rb").read(), b"kv-c")
        self.assertIn("| open |", open(os.path.join(dest, "kv_model", "PROVENANCE.md")).read())
        self.assertIn("| crash | v24.0 | True |", open(os.path.join(dest, "kv_model", "PROVENANCE.md")).read())


def descendants(root):
    """The pids whose parent chain reaches `root`."""
    parent = {}
    for pid in os.listdir("/proc"):
        with contextlib.suppress(OSError, ValueError, IndexError):
            stat = open(f"/proc/{pid}/stat").read()
            parent[int(pid)] = int(stat.rsplit(")", 1)[1].split()[1])
    out = set()
    for pid in parent:
        p, seen = pid, 0
        while p in parent and seen < 64:
            p, seen = parent[p], seen + 1
            if p == root:
                out.add(pid)
                break
    return out


class JobTimeouts(Tree):
    """run_leg's job timeout starts when run.sh's build is done (BL-174). The fake run.sh builds
    for `build` seconds, creates the run's `built` file (as run.sh does), runs for `run` seconds
    and writes a verdict."""

    def leg(self, build, run, timeout, build_timeout=60):
        script = os.path.join(self.root, "run.sh")
        with open(script, "w") as f:
            f.write(f"""#!/bin/bash
run="$DASH_EMBARGO_DIR/fuzz/$1/runs/$(date -u +%Y%m%dT%H%M%SZ)-$3-$$"
mkdir -p "$run"
sleep {build}
: >"$run/built"
sleep {run}
echo '{{"mode": "replay"}}' >"$run/verdict.json"
echo 'node-fuzz fake status=pass'
""")
        os.chmod(script, 0o700)
        with mock.patch.object(fuzzctl, "RUN_SH", script), mock.patch.object(fuzzctl, "BUILD_TIMEOUT", build_timeout, create=True):
            return fuzzctl.run_leg(PUB.name, "v24.0", [], timeout=timeout)

    def test_a_cold_build_does_not_use_up_the_job_timeout(self):
        leg = self.leg(build=6, run=0, timeout=2)  # the build alone is three times the timeout
        self.assertEqual((leg.code, leg.line), (0, "node-fuzz fake status=pass"))
        self.assertTrue(leg.has_verdict)

    def test_the_job_still_times_out_after_its_build(self):
        leg = self.leg(build=0, run=30, timeout=1)
        self.assertEqual((leg.code, leg.line, leg.has_verdict), (124, "run.sh: timed out", False))

    def test_a_build_has_a_timeout_of_its_own(self):
        leg = self.leg(build=30, run=0, timeout=60, build_timeout=1)
        self.assertEqual((leg.code, leg.line), (124, "run.sh: timed out"))


class ProcessScanner(threading.Thread):
    """Reads the command line and environment of every process descended from this one until
    stopped, recording any that contains a marker (PLAN §7.5: `ps` and /proc are public)."""

    def __init__(self, markers):
        super().__init__(daemon=True)
        self.markers, self.hits, self.observed = markers, [], set()
        self.done = threading.Event()

    def run(self):
        me = os.getpid()
        while not self.done.is_set():
            for pid in descendants(me):
                for part in ("cmdline", "environ"):
                    with contextlib.suppress(OSError):
                        blob = open(f"/proc/{pid}/{part}", "rb").read()
                        self.observed.add(pid)
                        self.hits += [(part, m) for m in self.markers if m in blob]
            time.sleep(0.01)


class PrivateIdentities(Tree):
    def test_private_findings_never_reach_process_arguments_or_environments(self):
        fake = os.path.join(self.root, "run.sh")
        # Long-lived enough for the scanner to be scheduled at the box's usual load.
        open(fake, "w").write("#!/bin/bash\nsleep 2\necho 'run.sh: fake'\nexit 1\n")
        os.chmod(fake, 0o700)
        data = b"private-input-bytes"
        self.make_run(PRIV.name, "r1", divergences=[(data, "secret field")])
        self.make_run(PRIV.name, "r2", crashes=[("crash-0123456789abcdef", b"crash-input")],
                      log="==1== ERROR: libFuzzer: deadly signal\n"
                          "artifact_prefix='/x/'; Test unit written to /x/crash-0123456789abcdef\n")
        markers = [hashlib.sha256(data).hexdigest().encode(), b"secret field", b"/divergences/",
                   b"/crashes/", b"crash-0123456789abcdef", b"/regress/"]
        scanner = ProcessScanner(markers)
        scanner.start()
        try:
            with mock.patch.object(fuzzctl, "RUN_SH", fake):
                fuzzctl.run_triage(TARGETS, recheck=True)  # minimisations, then replays
        finally:
            scanner.done.set()
            scanner.join()
        self.assertTrue(scanner.observed, "no run.sh process was observed")
        self.assertEqual(scanner.hits, [])


class SliceRunner:
    """Stands in for run.sh in the farm: a slice spends what it was asked for, after `delay`
    seconds or at once when interrupted (SIGINT is simulated through `on_start`)."""

    def __init__(self, delay=0.0, finding_on=None, fail_on=None):
        self.delay, self.finding_on, self.fail_on = delay, finding_on, fail_on
        self.calls, self.events = [], {}
        self.lock = threading.Lock()

    def interrupt(self, pid, _sig):
        self.events[pid].set()

    def __call__(self, target, pin, args, on_start=None, timeout=None, cancel=None):
        cpu = float(args[args.index("--cpu-hours") + 1])
        stop = threading.Event()
        with self.lock:
            self.calls.append((target, pin, cpu))
            pid = len(self.calls)
            self.events[pid] = stop

        if on_start:
            on_start(types.SimpleNamespace(pid=pid))
        interrupted = stop.wait(self.delay)
        leg = f"{target}@{pin}"
        if leg == self.fail_on:
            return Leg(target, pin, "run.sh: the build failed", 2, "", {})
        found = leg == self.finding_on
        verdict = {"status": "fail" if found or interrupted else "pass",
                   "cpu_hours": cpu * (0.5 if interrupted else 1.0), "executions": 10,
                   "divergences": int(found), "crashes": 0, "sink_failures": 0}
        # run.sh fails an interrupted run (budget not reached) as well as a finding.
        return Leg(target, pin, f"node-fuzz target={target} pin={pin}", int(found or interrupted),
                   os.path.join("/nonexistent", "run"), verdict)


class NoBench:
    def __init__(self, blocked=None):
        self.value = blocked

    def blocked(self, horizon):
        return self.value


def farm_args(**kw):
    defaults = dict(night="test", hours=1.0, cores=4, jobs=2, slice_minutes=30,
                    min_slice_minutes=0.001, poll=0.01, windows="/nonexistent", plateau_minutes=60,
                    only=None)
    return argparse.Namespace(**{**defaults, **kw})


class Farming(Tree):
    def farm(self, runner, bench=None, clock=time.time, **kw):
        logs = []
        f = fuzzctl.Farm(farm_args(**kw), TARGETS, bench or NoBench(), runner=runner,
                         clock=clock, log=logs.append)
        f.margin, f.lead, f.imminent = 0.05, 0.05, 0.05
        return f, logs

    def test_a_night_completes_every_share_and_resumes(self):
        runner = SliceRunner()
        f, logs = self.farm(runner)  # 4 CPU-h in slices of 1 CPU-h, each done at once
        self.assertEqual(f.work(), "complete")
        self.assertEqual(len(runner.calls), 5)
        spent = fuzzctl.credit_by_leg(fuzzctl.load_records(f.dir))
        for leg, budget in f.plan["legs"].items():
            self.assertAlmostEqual(spent[leg], budget, places=3)
        self.assertIn("outcome=complete", logs[-1])
        state = os.path.join(self.fdir, ".farm", "test")
        self.assertEqual(stat.S_IMODE(os.stat(state).st_mode), 0o700)
        # A second launch for the same night finds nothing left.
        n = len(runner.calls)
        f2, _ = self.farm(runner)
        self.assertEqual(f2.work(), "complete")
        self.assertEqual(len(runner.calls), n)

    def test_findings_and_broken_legs_retire(self):
        runner = SliceRunner(finding_on="kv_model@v24.0", fail_on="diff_tx_decode@v25.0")
        f, logs = self.farm(runner)
        with mock.patch.object(fuzzctl, "run_triage") as tri:
            tri.return_value.summary.return_value = "node-fuzz triage"
            tri.return_value.stats = {"errors": 0}
            self.assertEqual(f.work(), "legs-retired")
        legs = [f"{t}@{p}" for t, p, _ in runner.calls]
        self.assertEqual(legs.count("kv_model@v24.0"), 1)
        self.assertEqual(legs.count("diff_tx_decode@v25.0"), 2)
        self.assertIn("retired_legs=2", logs[-1])
        self.assertTrue(any(c.kwargs.get("only") == {"kv_model"} for c in tri.call_args_list))

    def test_a_window_holds_the_farm_until_the_deadline(self):
        runner = SliceRunner()
        f, logs = self.farm(runner, NoBench(("a reserved bench window", 0)), hours=0.0002)
        start = time.time()
        self.assertEqual(f.work(), "deadline")
        self.assertEqual(runner.calls, [])
        self.assertIn("node-fuzz farm: waiting: a reserved bench window", logs)
        self.assertLess(time.time() - start, 5)


    def kill_after_run(self, leg, sid, verdict=None, divergences=(), crashes=(), cpu_files=()):
        """A worker killed after run.sh started (or finished) a slice: the start is in the
        ledger, the run (tagged with the slice id) is in the tree, the done row is not."""
        target, pin = leg.split("@")
        f, _ = self.farm(SliceRunner())
        row = json.dumps({"event": "start", "slice": sid, "leg": leg, "time": 0}) + "\n"
        open(os.path.join(f.dir, "slices.jsonl"), "a").write(row)
        run = os.path.join(self.fdir, target, "runs", f"20261009T000000Z-{pin}-7-{sid}")
        record(run, divergences, crashes, PANIC_LOG if crashes else "", pin)
        for n, cpu in enumerate(cpu_files, 1):
            os.makedirs(os.path.join(run, "logs", f"segment-{n}"), exist_ok=True)
            open(os.path.join(run, "logs", f"segment-{n}", "cpu"), "w").write(cpu)
        if verdict is not None:
            json.dump(verdict, open(os.path.join(run, "verdict.json"), "w"))
        return f

    def test_a_slice_finished_before_a_worker_kill_is_accounted_once(self):
        budget = fuzzctl.plan_legs(TARGETS, 4)["kv_model@v24.0"]
        f = self.kill_after_run("kv_model@v24.0", "00000000000000a1",
                                verdict={"mode": "run", "pin": "v24.0", "status": "fail",
                                         "cpu_hours": budget, "executions": 10, "divergences": 0,
                                         "crashes": 1, "sink_failures": 0, "rust_dashcore": "abc"},
                                crashes=[("crash-aaa", b"boom")])
        with mock.patch.object(fuzzctl.Farm, "triage"):
            f.work()
        done = [r for r in fuzzctl.load_records(f.dir) if r["slice"] == "00000000000000a1"]
        self.assertEqual(len(done), 1)
        self.assertEqual((done[0]["status"], done[0]["crashes"], done[0]["recovered"]),
                         ("finding", 1, True))
        self.assertIn("crashes=1", open(os.path.join(f.dir, "summary")).read())
        # The recovered finding's run is not triaged yet (triage is mocked out): its leg is held,
        # not handed a fresh share.
        self.assertNotIn("kv_model", {t for t, _, _ in f.runner.calls})
        # A second launch does not recover it again.
        f2, _ = self.farm(SliceRunner())
        with mock.patch.object(fuzzctl.Farm, "triage"):
            f2.work()
        self.assertEqual(len([r for r in fuzzctl.load_records(f.dir) if r["slice"] == "00000000000000a1"]), 1)

    def test_a_slice_killed_mid_run_keeps_its_finished_segments(self):
        f = self.kill_after_run("diff_tx_decode@v25.0", "00000000000000b2",
                                cpu_files=["1.0 0.5\n", "0.25 0.25\n"])
        with mock.patch.object(fuzzctl.Farm, "triage"):
            f.work()
        done = [r for r in fuzzctl.load_records(f.dir) if r["slice"] == "00000000000000b2"][0]
        self.assertEqual((done["status"], done["cpu_hours"], done["recovered"]),
                         ("interrupted", round(2.0 / 3600, 6), True))

    def test_a_slice_with_no_run_is_lost(self):
        f, _ = self.farm(SliceRunner())
        open(os.path.join(f.dir, "slices.jsonl"), "a").write(
            json.dumps({"event": "start", "slice": "00000000000000c3", "leg": "kv_model@v24.0", "time": 0}) + "\n")
        with mock.patch.object(fuzzctl.Farm, "triage"):
            f.work()
        done = [r for r in fuzzctl.load_records(f.dir) if r["slice"] == "00000000000000c3"][0]
        self.assertEqual((done["status"], done["cpu_hours"]), ("lost", 0))

    def test_a_bad_plan_fails_closed(self):
        f, _ = self.farm(SliceRunner())
        f.release()
        path = os.path.join(f.dir, "plan.json")
        good = json.load(open(path))
        other = [Target("diff_tx_decode", "l1", ("v25.0", "v24.0"), "private", 60, 5), PUB, SELF]
        for bad in (
            {**good, "legs": {}},
            "{broken",
            {**good, "cpu_hours": float("nan")},
            {**good, "legs": {**good["legs"], "kv_model@v24.0": -1}},
            {**good, "legs": {**good["legs"], "ghost@v24.0": 0}},
            {**good, "cpu_hours": good["cpu_hours"] * 2},
            {**good, "night": "another"},
            {k: v for k, v in good.items() if k != "registry"},
            {**good, "legs": {**good["legs"], "kv_model@v24.0": 0,
                              "diff_tx_decode@v25.0": good["legs"]["diff_tx_decode@v25.0"]
                              + good["legs"]["kv_model@v24.0"]}},  # shares moved between legs
        ):
            open(path, "w").write(bad if isinstance(bad, str) else json.dumps(bad))
            with self.assertRaises(fuzzctl.FarmStateError, msg=str(bad)[:80]):
                self.farm(SliceRunner())
        json.dump(good, open(path, "w"))
        with self.assertRaises(fuzzctl.FarmStateError):  # the registry changed since the plan
            fuzzctl.Farm(farm_args(), other, NoBench(), runner=SliceRunner(), log=lambda _: None)
        self.farm(SliceRunner())  # the good plan still loads
        # The worker refuses with a non-zero exit.
        open(path, "w").write("{broken")
        with contextlib.redirect_stdout(io.StringIO()) as out:
            self.assertEqual(fuzzctl.farm_work(farm_args(), TARGETS), 2)
        self.assertIn("refusing", out.getvalue())

    def test_a_malformed_ledger_fails_closed_and_only_a_torn_last_row_is_repaired(self):
        f, _ = self.farm(SliceRunner())
        f.release()
        ledger = os.path.join(f.dir, "slices.jsonl")
        start = json.dumps({"event": "start", "slice": "00000000000000d4", "leg": "kv_model@v24.0", "time": 0})
        done = json.dumps({"event": "done", "slice": "00000000000000d4", "leg": "kv_model@v24.0",
                           "status": "pass", "cpu_hours": 0.1, "executions": 0, "divergences": 0,
                           "crashes": 0, "sink_failures": 0, "recovered": False})
        for bad in ("not json\n" + start + "\n",
                    done + "\n",  # done without its start
                    start + "\n" + done + "\n" + done + "\n",  # done twice
                    json.dumps({"event": "done", "slice": "00000000000000d4", "leg": "kv_model@v24.0",
                                "status": "pass", "cpu_hours": -1, "executions": 0, "divergences": 0,
                                "crashes": 0, "sink_failures": 0, "recovered": False}) + "\n",
                    json.dumps({"event": "start", "slice": "00000000000000d4", "leg": "ghost@v1", "time": 0}) + "\n"):
            open(ledger, "w").write(bad)
            with self.assertRaises(fuzzctl.FarmStateError, msg=bad[:60]):
                self.farm(SliceRunner())
        open(ledger, "w").write(start + "\n" + start[:20])  # killed during an append
        f, logs = self.farm(SliceRunner())
        self.assertEqual(open(ledger).read(), start + "\n")
        with mock.patch.object(fuzzctl.Farm, "triage"):
            f.work()
        self.assertIn("ledger_repaired=1", logs[-1])

    def record_slice(self, f, leg, status, cpu, n):
        """A slice's start and done rows; its run is `run-<n>`."""
        f.record({"event": "start", "slice": f"{n:016x}", "leg": leg, "time": 0})
        f.record({"event": "done", "slice": f"{n:016x}", "leg": leg, "status": status, "cpu_hours": cpu,
                  "executions": 1, "divergences": int(status == "finding"), "crashes": 0,
                  "sink_failures": 0, "recovered": False, "run": f"run-{n}", "rev": "old"})

    def test_a_finding_resets_the_legs_budget(self):
        f, _ = self.farm(SliceRunner())
        for n, (leg, budget) in enumerate(f.plan["legs"].items(), 1):
            # The finding came at once: the night's cap still has room for the fresh share.
            found = leg.startswith("kv_model")
            self.record_slice(f, leg, "finding" if found else "pass", 0.0 if found else budget, n)
            if leg.startswith("kv_model"):  # triaged, and since fixed
                os.makedirs(os.path.join(self.fdir, PUB.name), exist_ok=True)
                json.dump({"clusters": {}, "runs": [f"run-{n}"]},
                          open(os.path.join(self.fdir, PUB.name, "triage.json"), "w"))
        f.release()
        runner = SliceRunner()
        f, _ = self.farm(runner)
        with mock.patch.object(fuzzctl.Farm, "triage"):
            self.assertEqual(f.work(), "complete")
        fresh = sum(cpu for target, _, cpu in runner.calls if target == "kv_model")
        self.assertAlmostEqual(fresh, f.plan["legs"]["kv_model@v24.0"], places=3)
        self.assertEqual({t for t, _, _ in runner.calls}, {"kv_model"})

    def test_clean_time_before_a_finding_does_not_count(self):
        f, _ = self.farm(SliceRunner())
        leg = "kv_model@v24.0"
        for n, (status, cpu) in enumerate((("pass", 0.2), ("finding", 0.1), ("pass", 0.1)), 1):
            self.record_slice(f, leg, status, cpu, n)
        self.assertAlmostEqual(fuzzctl.credit_by_leg(f.records())[leg], 0.1)

    def test_a_leg_with_an_open_finding_is_held_until_its_fix(self):
        self.make_run(PUB.name, "k1", crashes=[("crash-aaa", b"kv-crash")], log=PANIC_LOG)
        fuzzctl.run_triage(TARGETS, minimize=False)
        runner = SliceRunner()
        f, logs = self.farm(runner)
        with mock.patch.object(fuzzctl.Farm, "triage"):
            self.assertEqual(f.work(), "legs-retired")
        self.assertNotIn("kv_model", {t for t, _, _ in runner.calls})
        self.assertTrue(any("1 legs held by open findings" in line for line in logs))
        # The recheck at the next launch finds it fixed: the leg runs its share.
        triage_runner = FakeRunner(self)
        triage_runner.fixed.add(hashlib.sha256(b"kv-crash").hexdigest())
        fuzzctl.run_triage(TARGETS, recheck=True, minimize=False, runner=triage_runner)
        runner = SliceRunner()
        f, _ = self.farm(runner)
        with mock.patch.object(fuzzctl.Farm, "triage"):
            self.assertEqual(f.work(), "complete")
        self.assertIn("kv_model", {t for t, _, _ in runner.calls})

    def test_a_target_with_unreadable_triage_state_is_held(self):
        os.makedirs(os.path.join(self.fdir, PUB.name))
        open(os.path.join(self.fdir, PUB.name, "triage.json"), "w").write("{broken")
        runner = SliceRunner()
        f, logs = self.farm(runner)
        with mock.patch.object(fuzzctl.Farm, "triage"):
            f.work()
        self.assertNotIn("kv_model", {t for t, _, _ in runner.calls})

    def test_reconcile_classifies_a_failed_verdict_as_failed_and_survives_a_bare_run(self):
        f = self.kill_after_run("kv_model@v24.0", "00000000000000e5",
                                verdict={"mode": "run", "pin": "v24.0", "status": "fail", "cpu_hours": 0.1,
                                         "executions": 1, "divergences": 0, "crashes": 0,
                                         "sink_failures": 0, "libfuzzer_status": 137})
        bare = os.path.join(self.fdir, "diff_tx_decode", "runs", "20261009T000000Z-v25.0-8-00000000000000f6")
        os.makedirs(bare)  # killed between run.sh's mkdirs
        f.record({"event": "start", "slice": "00000000000000f6", "leg": "diff_tx_decode@v25.0", "time": 0})
        with mock.patch.object(fuzzctl.Farm, "triage"):
            f.work()
        by = {r["slice"]: r for r in fuzzctl.load_records(f.dir)}
        self.assertEqual(by["00000000000000e5"]["status"], "fail")
        self.assertEqual(by["00000000000000f6"]["status"], "interrupted")

    def test_a_killed_slice_keeps_its_logged_sink_failure(self):
        f = self.kill_after_run("kv_model@v24.0", "00000000000000b2", cpu_files=["2880 0\n"])
        run = fuzzctl.tagged_run(self.fdir, "kv_model", "00000000000000b2")
        open(os.path.join(run, "logs", "segment-1", "fuzz.out"), "w").write(
            "kv_model: sink write failed\n==1== ERROR: libFuzzer: fuzz target exited\n")
        f.work()  # with startup triage
        row = [r for r in f.records() if r["slice"] == "00000000000000b2"][0]
        self.assertEqual((row["status"], row["sink_failures"], row["recovered"]), ("finding", 1, True))
        self.assertEqual(fuzzctl.credit_by_leg(f.records()).get("kv_model@v24.0", 0), 0)
        self.assertNotIn("kv_model", {t for t, _, _ in f.runner.calls})  # held
        self.assertIn("sink_failures=1", open(os.path.join(f.dir, "summary")).read())
        lines, errors = fuzzctl.status_lines(self.fdir, TARGETS)
        self.assertEqual((lines[0], errors), ("SW-FUZZ open findings: 1 (private 0, public 1); fixed 0", 0))
        # A later launch still holds the leg: the sink finding stays open until acknowledged.
        runner = SliceRunner()
        f2, _ = self.farm(runner)
        f2.work()
        self.assertNotIn("kv_model", {t for t, _, _ in runner.calls})

    def test_a_logged_sink_failure_is_an_open_finding_until_acknowledged(self):
        self.make_run(PUB.name, "k1", log="kv_model: sink write failed\n==1== ERROR: libFuzzer: deadly signal\n")
        tri = fuzzctl.run_triage(TARGETS, minimize=False)
        self.assertEqual(tri.stats["new_clusters"], 1)
        c = list(self.state(PUB.name)["clusters"].values())[0]
        self.assertEqual((c["kind"], c["state"], c["vector"]), ("sink", "open", None))
        rc, out = self.cli(["triage", "--ack-sink-failures"])
        self.assertEqual(rc, 0)
        self.assertIn("SW-FUZZ open findings: 0 (private 0, public 0); fixed 1", out)

    def test_an_active_startup_job_is_cancelled_by_a_bench_or_a_stop(self):
        self.cancel_startup_job()

    def test_a_job_is_cancelled_when_sigint_is_inherited_ignored(self):
        # A background job (GATE-FAST's nightly lanes) starts with SIGINT ignored, and so does every
        # child: the stub resets it, as libFuzzer installs its own handler.
        old = signal.signal(signal.SIGINT, signal.SIG_IGN)
        self.addCleanup(signal.signal, signal.SIGINT, old)
        self.cancel_startup_job()

    def cancel_startup_job(self):
        stub = os.path.join(self.root, "stub-run.sh")
        ready = os.path.join(self.root, "stub-ready")
        # Dies of SIGINT, as libFuzzer exits on it, whatever disposition it inherited.
        open(stub, "w").write("#!/usr/bin/env python3\nimport signal, time\n"
                              "signal.signal(signal.SIGINT, signal.SIG_DFL)\n"
                              f"open({ready!r}, 'w').close()\ntime.sleep(30)\n")
        os.chmod(stub, 0o700)

        class FlipBench:
            now = None

            def blocked(self, horizon):
                return self.now

        for trigger in ("bench", "stop"):
            self.make_run(PRIV.name, f"r-{trigger}", divergences=[(f"data-{trigger}".encode(), "class-a")])
            bench = FlipBench()
            farm = fuzzctl.Farm(farm_args(), TARGETS, bench, log=lambda _: None)
            with mock.patch.object(fuzzctl, "RUN_SH", stub):
                worker = threading.Thread(target=farm.triage)
                started = time.monotonic()
                worker.start()
                while not farm.procs and time.monotonic() - started < 5:
                    time.sleep(0.01)
                self.assertTrue(farm.procs, trigger)  # the job is running
                while not os.path.exists(ready) and time.monotonic() - started < 30:
                    time.sleep(0.01)  # and its stub is past its SIGINT reset
                self.assertTrue(os.path.exists(ready), trigger)
                os.remove(ready)
                started = time.monotonic()  # the cancellation is timed from here
                if trigger == "bench":
                    bench.now = ("node-bench is running", 0)
                else:
                    farm.stop.set()
                worker.join(10)
            self.assertFalse(worker.is_alive(), trigger)
            self.assertLess(time.monotonic() - started, 10, trigger)
            self.assertEqual(farm.procs, {}, trigger)
            farm.release()

    def test_one_worker_per_night(self):
        f, _ = self.farm(SliceRunner())
        with self.assertRaises(fuzzctl.FarmBusy):
            self.farm(SliceRunner())
        f.release()
        self.farm(SliceRunner())

    def test_stopping_interrupts_and_records(self):
        runner = SliceRunner(delay=30)
        f, logs = self.farm(runner)
        signalled = []
        def killpg(pid, sig):
            signalled.append(sig)
            runner.interrupt(pid, sig)

        with mock.patch.object(fuzzctl.os, "killpg", killpg):
            t = threading.Thread(target=f.work)
            t.start()
            while len(runner.calls) < 2:
                time.sleep(0.01)
            f.stop.set()
            t.join(20)
        self.assertFalse(t.is_alive())
        self.assertEqual(len(signalled), 2)
        self.assertIn("outcome=stopped", logs[-1])
        self.assertEqual(len(fuzzctl.load_records(f.dir)), 2)
        self.assertTrue(all(r["status"] == "interrupted" for r in fuzzctl.load_records(f.dir)))


class HarnessSmoke(unittest.TestCase):
    """harness_smoke runs regen.py --random twice (generate, then --check) and passes only when
    no sequence failed and the rerun reproduced every file."""

    def run_smoke(self, gen, check):
        calls = []

        def runner(cmd, **_):
            calls.append(cmd)
            stdout = gen if "--check" not in cmd else check
            return types.SimpleNamespace(returncode=0, stdout=stdout, stderr="")

        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            ok = fuzzctl.harness_smoke("/bin-dir", 5, 3, "/out", 2, False, runner=runner)
        return ok, out.getvalue(), calls

    LINE = "random seed=3 sequences=5 length=30 delivery=boundary-flush ok={ok} cut={cut} known={known} fail={fail} differ={differ}\n"

    def test_a_reproduced_run_passes(self):
        line = self.LINE.format(ok=4, cut=1, known=0, fail=0, differ=0)
        ok, out, calls = self.run_smoke("wrote\n" + line, line)
        self.assertTrue(ok)
        self.assertIn("ok=4 cut=1 known=0 fail=0 differ=0", out)
        self.assertEqual([c[-1] for c in calls], ["2", "--check"])
        self.assertIn("--random", calls[0])

    def test_a_finding_or_a_difference_fails(self):
        for gen, check in (({"fail": 1}, {"fail": 1}), ({}, {"differ": 2})):
            base = dict(ok=5, cut=0, known=0, fail=0, differ=0)
            fail = "FAIL random-3-1: the harness failed (see random-3-1.err)\n"
            ok, out, _ = self.run_smoke(fail + self.LINE.format(**{**base, **gen}), self.LINE.format(**{**base, **check}))
            self.assertFalse(ok)
            self.assertIn("status=fail", out)
            self.assertIn("  generate: " + fail.strip(), out)  # the sequence is named

    def test_a_known_finding_passes(self):
        line = self.LINE.format(ok=4, cut=0, known=1, fail=0, differ=0)
        known = "KNOWN random-3-2: findings/x.json (see random-3-2.err)\n"
        ok, out, _ = self.run_smoke(known + line, known + line)
        self.assertTrue(ok)
        self.assertIn("known=1 fail=0", out)
        self.assertIn("  generate: " + known.strip(), out)

    def test_no_summary_fails(self):
        ok, out, calls = self.run_smoke("Traceback\n", "")
        self.assertFalse(ok)
        self.assertEqual(len(calls), 1)
        self.assertIn("status=fail", out)


def load_regen():
    spec = importlib.util.spec_from_file_location("sb4_regen", fuzzctl.REGEN)
    regen = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(regen)
    return regen


class RegenKnownFindings(unittest.TestCase):
    """regen.py counts a harness failure as `known` only when its failure kind and
    terminal diagnostic (the last stderr line) match a recorded finding, and
    refuses a finding without a signature (Sol HX1 r1 R1-4's synthetic cases)."""

    def judge(self, returncode, stderr, known, runs=None):
        """random_job's outcome when the harness fails with `stderr`; a rerun
        (after a cut) fails with an unrelated error, and `runs` collects calls."""
        regen = load_regen()
        runs = [] if runs is None else runs

        def failing_run(bin_dir, argv, tmp, perturb=None):
            runs.append(argv)
            if len(runs) > 1:
                raise regen.HarnessFailure(argv, 1, "sb4_scenarios: the rerun\n")
            raise regen.HarnessFailure(argv, returncode, stderr)

        with tempfile.TemporaryDirectory() as out, tempfile.TemporaryDirectory() as tmp, \
                mock.patch.object(regen, "run", failing_run):
            known = [regen.known_finding(e, "SYNTHETIC", None) for e in known]
            return regen.random_job("unused", 7, 0, 5, out, tmp, "boundary-flush", known)[1]

    def test_an_earlier_line_does_not_match(self):
        stderr = "warning: synthetic recorded diagnostic\nsb4_scenarios: unrelated terminal failure\n"
        known = [{"kind": "error", "signature": "synthetic recorded diagnostic"}]
        self.assertEqual(self.judge(1, stderr, known), "fail")

    def test_an_empty_or_kindless_signature_is_refused(self):
        regen = load_regen()
        for entry in ({"kind": "error", "signature": ""}, {"kind": "error", "signature": "  "},
                      {"kind": "error"}, {"signature": "x"}, {"kind": "warning", "signature": "x"}):
            with self.assertRaises(ValueError):
                regen.known_finding(entry, "SYNTHETIC", None)

    def test_the_terminal_diagnostic_of_its_kind_matches(self):
        stderr = "note: earlier\nsb4_scenarios: synthetic terminal failure\n\n"
        known = [{"kind": "error", "signature": "synthetic terminal failure"}]
        self.assertEqual(self.judge(1, stderr, known), "known")
        self.assertEqual(self.judge(-6, stderr, known), "fail")  # an abort is another kind


class RegenPerturbedReplay(unittest.TestCase):
    """DEC-172/175: random_job replays every run under perturbed heaps and cuts
    before the first step at which any two runs differ; the harness's
    possibly_address_sensitive label neither cuts, accepts nor gates a replay."""

    def judge(self, plain, perturbed, length=5):
        """(outcome, calls, final ops) for a sequence whose plain run gives
        plain(n_ops) and whose perturbed run gives perturbed(n_ops, seed) (a
        vector, or "abort" for a harness abort)."""
        regen = load_regen()
        calls = []

        def fake_run(bin_dir, argv, tmp, perturb=None):
            with open(argv[1]) as f:
                n = len(json.load(f)["ops"])
            calls.append((n, perturb))
            v = plain(n) if perturb is None else perturbed(n, perturb)
            if v == "abort":
                raise regen.HarnessFailure(argv, -6, "sb4_scenarios: synthetic abort\n")
            return dict(v, delivery_policy="boundary-flush")

        with tempfile.TemporaryDirectory() as out, tempfile.TemporaryDirectory() as tmp, \
                mock.patch.object(regen, "run", fake_run), \
                mock.patch.object(regen, "provenance", lambda *a: {}):
            name, outcome, text = regen.random_job("unused", 7, 0, length, out, tmp, "boundary-flush", [])
            ops = json.loads(text)["scenario"]["ops"] if outcome in ("ok", "cut") else None
            return outcome, calls, ops, regen

    @staticmethod
    def vector(n, marked, tag=0):
        return {"steps": [{"op": "setup"}] + [{"op": k, "tag": tag} for k in range(n)],
                "possibly_address_sensitive": ["failed_blocks at step 1"] if marked else []}

    def test_an_unmarked_run_is_replayed_and_cut(self):  # R3-1: a live tie the label missed
        def perturbed(n, s):
            v = self.vector(n, False)
            if s == "down" and n >= 4:
                v["steps"][4]["tag"] = 1
            return v
        outcome, calls, ops, regen = self.judge(lambda n: self.vector(n, False), perturbed)
        self.assertEqual((outcome, len(ops)), ("cut", 3))
        self.assertEqual([s for _, s in calls[:3]], [None, "up", "down"])

    def test_a_marked_run_stable_under_replays_passes_uncut(self):
        outcome, calls, ops, regen = self.judge(lambda n: self.vector(n, True), lambda n, s: self.vector(n, True))
        self.assertEqual(outcome, "ok")
        self.assertEqual(len(calls), 3 + regen.PERTURB_REPLAYS)
        modes = [s for _, s in calls if s is not None]
        self.assertEqual(modes[:2], ["up", "down"])
        self.assertEqual(len(set(modes)), 2 + regen.PERTURB_REPLAYS)  # distinct seeds

    def test_a_difference_cuts_before_its_step(self):
        def perturbed(n, s):
            v = self.vector(n, True)
            if n >= 3:
                v["steps"][3]["tag"] = 1  # step 3 (op index 2) differs
            return v
        outcome, calls, ops, _ = self.judge(lambda n: self.vector(n, True), perturbed)
        self.assertEqual((outcome, len(ops)), ("cut", 2))

    def test_a_difference_in_the_transcript_cuts_at_its_step(self):
        def perturbed(n, s):
            return dict(self.vector(n, True), transcript=[{"step": 4, "x": 1}] if n >= 4 else [])
        outcome, calls, ops, _ = self.judge(lambda n: dict(self.vector(n, True), transcript=[]), perturbed)
        self.assertEqual((outcome, len(ops)), ("cut", 3))

    def test_the_earliest_difference_between_any_two_runs_cuts(self):
        def perturbed(n, s):  # up and down disagree at step 2, the plain run sides with up
            v = self.vector(n, True)
            if s == "down" and n >= 2:
                v["steps"][2]["tag"] = 1
            if s not in ("up", "down") and n >= 4:
                v["steps"][4]["tag"] = 2
            return v
        outcome, calls, ops, _ = self.judge(lambda n: self.vector(n, True), perturbed)
        self.assertEqual((outcome, len(ops)), ("cut", 1))

    def test_a_difference_is_attributed_to_its_step(self):
        regen = load_regen()
        a = dict(self.vector(5, True), transcript=[{"step": 2, "x": 0}])
        self.assertIsNone(regen.first_difference(a, json.loads(json.dumps(a))))
        tail = dict(a, transcript=a["transcript"] + [{"step": 3, "x": 1}])  # an entry only one run has
        self.assertEqual(regen.first_difference(a, tail), 3)
        self.assertEqual(regen.first_difference(tail, a), 3)
        self.assertEqual(regen.first_difference(a, dict(a, core_counts={"x": 1})), 5)  # elsewhere: the last step
        self.assertEqual(regen.first_difference({"rows": [1]}, {"rows": [2]}), 0)  # a header job's vector
        self.assertIsNone(regen.first_difference({"rows": [1]}, {"rows": [1]}))

    def test_the_earliest_step_across_channels_is_the_difference(self):  # R3-2, Sol's two pairs
        regen = load_regen()
        steps = [{"op": "setup"}] + [{"op": k, "result": {"ok": True}} for k in range(1, 6)]

        def pair(changes):
            a = {"steps": steps, "transcript": [], "accept_checks": []}
            b = json.loads(json.dumps(a))
            for key, entry_a, entry_b in changes:
                if key == "steps":
                    b["steps"][entry_b]["result"] = {"ok": False}
                else:
                    a[key].append(entry_a)
                    b[key].append(entry_b)
            return a, b

        # earlier-transcript-later-view: a transcript verdict at 2, a step result at 5
        a, b = pair([("transcript", {"step": 2, "block": "A2", "result": True},
                      {"step": 2, "block": "A2", "result": False}), ("steps", None, 5)])
        self.assertEqual((regen.first_difference(a, b), regen.first_difference(b, a)), (2, 2))
        # earlier-accept-later-transcript: an acceptance verdict at 2, a transcript entry at 4
        a, b = pair([("transcript", {"step": 4, "block": "A2", "result": True},
                      {"step": 4, "block": "A2", "result": False}),
                     ("accept_checks", {"step": 2, "block": "A2", "state": {"reason": "one"}},
                      {"step": 2, "block": "A2", "state": {"reason": "two"}})])
        self.assertEqual((regen.first_difference(a, b), regen.first_difference(b, a)), (2, 2))

    def test_a_failure_only_under_perturbation_or_at_setup_fails(self):
        self.assertEqual(self.judge(lambda n: self.vector(n, True), lambda n, s: "abort")[0], "fail")
        setup = lambda n, s: dict(self.vector(n, True), steps=[{"op": "setup", "x": 1}] + self.vector(n, True)["steps"][1:])
        self.assertEqual(self.judge(lambda n: self.vector(n, True), setup)[0], "fail")


class RegenAdmission(unittest.TestCase):
    """Sol HX1 r4 R4-1/R4-2: every entry point (random generation, writing and
    checking committed vectors, header jobs, each delivery schedule) admits a run
    only when the plain run and its replays under up, down and at least one
    seeded heap match. These drive regen.main and record the --perturb-heap
    modes each path actually runs."""

    STEPS = [{"op": "setup", "view": {"trace": [], "trace_transient_spans": []}},
             {"op": 1, "view": {"trace": [], "trace_transient_spans": []}},
             {"op": 2, "view": {"trace": [], "trace_transient_spans": []}}]

    def stable(self, argv, perturb):
        return {"steps": self.STEPS} if argv[0] == "run" else {"mode": argv[3], "instructions_u": 100}

    def differing(self, at_mode, at_delivery=None):
        """Vectors that differ at step 2 under `at_mode` (in runs under `at_delivery`, when given)."""
        def vector(argv, perturb):
            v = self.stable(argv, perturb)
            if perturb == at_mode and (at_delivery is None or argv[3] == at_delivery):
                v = json.loads(json.dumps(v))
                v["steps"][2]["view"]["x"] = 1
            return v
        return vector

    def main(self, args, vector=None, sensitive=None, reproducible=None):
        """(exit code, [(argv, perturb)] calls, written vector files, regen) of
        regen.main(args), with `sensitive` and `reproducible` as its
        SENSITIVE_CUTS and REPRODUCIBLE tables (a file per entry; none by default)."""
        regen = load_regen()
        vector = vector or self.stable
        calls = []

        def fake_run(bin_dir, argv, tmp, perturb=None):
            calls.append((tuple(argv), perturb))
            v = vector(argv, perturb)
            return dict(v, delivery_policy=argv[3]) if argv[0] == "run" else v

        with tempfile.TemporaryDirectory() as root:
            out, tmp, sdir, rdir, headers = (os.path.join(root, d) for d in ("out", "tmp", "sens", "repro", "headers"))
            for d in (out, tmp, sdir, rdir, headers):
                os.makedirs(d)
            for d, table in ((sdir, sensitive), (rdir, reproducible)):
                for name in table or {}:
                    with open(os.path.join(d, name + ".json"), "w") as f:
                        f.write("{}")
            with open(os.path.join(headers, "mainnet.bin"), "wb") as f:
                f.write(b"\0" * 80)
            with mock.patch.object(regen, "run", fake_run), \
                    mock.patch.object(regen, "verify_build", lambda *a: None), \
                    mock.patch.object(regen, "provenance", lambda *a: {}), \
                    mock.patch.object(regen, "compare", lambda *a, **k: None), \
                    mock.patch.object(regen, "OUT", out), \
                    mock.patch.object(regen, "SENSITIVE_DIR", sdir), \
                    mock.patch.object(regen, "SENSITIVE_CUTS", sensitive or {}), \
                    mock.patch.object(regen, "REPRODUCIBLE_DIR", rdir), \
                    mock.patch.object(regen, "REPRODUCIBLE", reproducible or {}), \
                    mock.patch.dict(os.environ, {"TMPDIR": tmp}), \
                    mock.patch("sys.argv", ["regen.py", "--bin", root, "--headers", headers,
                                            "--random-out", out] + args), \
                    contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                try:
                    regen.main()
                    code = 0
                except SystemExit as e:
                    code = e.code
            return code, calls, sorted(os.listdir(out)), regen

    @staticmethod
    def modes(calls, **match):
        """The perturb modes of the calls whose argv contains every value in `match`."""
        return [p for argv, p in calls if all(v in argv for v in match.values())]

    def test_writing_a_vector_admits_it_under_every_mode_first(self):
        code, calls, written, regen = self.main(["--only", "hx1-precious", "--no-headers"])
        self.assertEqual((code, written), (0, ["hx1-precious.json.gz"]))
        seed = regen.perturb_seed("scenario", "hx1-precious", "boundary-flush", 0)
        self.assertEqual(self.modes(calls), [None, "up", "down", seed])

    def test_an_unstable_scenario_is_not_written(self):
        for mode in ("down", "seed"):  # a seeded-only difference is a middle tie up and down miss
            with self.subTest(mode=mode):
                regen = load_regen()
                at = regen.perturb_seed("scenario", "hx1-precious", "boundary-flush", 0) if mode == "seed" else mode
                code, calls, written, _ = self.main(["--only", "hx1-precious", "--no-headers"], self.differing(at))
                self.assertNotEqual(code, 0)
                self.assertEqual(written, [])

    def test_check_admits_the_scenario_and_every_delivery_schedule(self):
        code, calls, written, regen = self.main(["--check", "--only", "hx1-enforce-fixed-point", "--schedules", "2"])
        self.assertEqual((code, written), (0, []))
        for delivery in ("boundary-flush", "random:1", "random:2"):
            seed = regen.perturb_seed("scenario", "hx1-enforce-fixed-point", delivery, 0)
            self.assertEqual(self.modes(calls, d=delivery), [None, "up", "down", seed], delivery)

    def test_an_unstable_delivery_schedule_fails_the_check(self):
        code, calls, _, _ = self.main(["--check", "--only", "hx1-enforce-fixed-point", "--schedules", "2"],
                                      self.differing("up", at_delivery="random:2"))
        self.assertNotEqual(code, 0)

    def test_a_header_job_is_admitted_apart_from_its_instruction_count(self):
        def bench(argv, perturb):  # the count moves with the heap
            return dict(self.stable(argv, perturb), instructions_u=100 + len(str(perturb)))
        code, calls, written, regen = self.main(["--only", "bench-pnbh-mainnet-dgw"], bench)
        self.assertEqual((code, written), (0, ["bench-pnbh-mainnet-dgw.json.gz"]))
        seed = regen.perturb_seed("headers", "bench-pnbh-mainnet-dgw", 0)
        self.assertEqual(self.modes(calls), [None, "up", "down", seed])

        def other(argv, perturb):
            return dict(self.stable(argv, perturb), mode="other" if perturb == "down" else argv[3])
        code, _, written, _ = self.main(["--only", "bench-pnbh-mainnet-dgw"], other)
        self.assertNotEqual(code, 0)
        self.assertEqual(written, [])

    def test_random_generation_admits_every_sequence(self):
        code, calls, written, regen = self.main(["--random", "1", "--seed", "3", "--random-length", "2"])
        self.assertEqual((code, written), (0, ["random-3-0.json"]))
        n_ops = len(regen.random_scenario(3, 0, 2)["ops"])
        self.assertEqual(self.modes(calls), [None, "up", "down", regen.perturb_seed(3, 0, n_ops, 0)])

    def test_an_address_sensitive_regression_must_be_refused_at_its_step(self):
        check = ["--check", "--only", "hx1-precious", "--no-headers"]

        def at(mode, step):  # the regression's runs differ at `step` under `mode`
            def vector(argv, perturb):
                v = self.stable(argv, perturb)
                if "/sens/" in argv[1] and perturb == mode:
                    v = json.loads(json.dumps(v))
                    v["steps"][step]["view"]["x"] = 1
                return v
            return vector

        code, calls, _, regen = self.main(check, at("down", 2), sensitive={"case": 2})
        self.assertEqual(code, 0)
        seed = regen.perturb_seed("address-sensitive", "case", "boundary-flush", 0)
        self.assertEqual([p for argv, p in calls if "/sens/" in argv[1]], [None, "up", "down", seed])
        self.assertNotEqual(self.main(check, at("down", 1), sensitive={"case": 2})[0], 0)  # an earlier step
        self.assertNotEqual(self.main(check, at("nothing", 1), sensitive={"case": 2})[0], 0)  # admitted

    def test_a_reproducible_regression_needs_one_vector_per_seed_and_its_step(self):  # R4-3
        check = ["--check", "--only", "hx1-precious", "--no-headers"]

        def runs(varying=False):
            seen = []

            def vector(argv, perturb):
                v = self.stable(argv, perturb)
                if "/repro/" in argv[1] and perturb == 11:
                    seen.append(1)
                    v = json.loads(json.dumps(v))
                    v["steps"][2]["view"]["x"] = len(seen) if varying else 1  # another process, another vector
                return v
            return vector

        code, calls, _, _ = self.main(check, runs(), reproducible={"case": {11: 2, 12: None}})
        self.assertEqual(code, 0)
        self.assertEqual([p for argv, p in calls if "/repro/" in argv[1]], ["up", "down"] + [11] * 4 + [12] * 4)
        self.assertNotEqual(self.main(check, runs(True), reproducible={"case": {11: 2}})[0], 0)
        self.assertNotEqual(self.main(check, runs(), reproducible={"case": {11: 1}})[0], 0)  # another step
        self.assertNotEqual(self.main(check, runs(), reproducible={"case": {12: 2}})[0], 0)  # no difference

    def test_the_seeded_replay_cannot_be_disabled(self):  # R4-2
        code, calls, written, _ = self.main(["--random", "1", "--seed", "3", "--perturb-replays", "0"])
        self.assertEqual((code, calls, written), (2, [], []))
        regen = load_regen()
        calls = []

        def fake_run(bin_dir, argv, tmp, perturb=None):
            calls.append(perturb)
            return {"steps": self.STEPS, "delivery_policy": "boundary-flush"}

        with tempfile.TemporaryDirectory() as out, tempfile.TemporaryDirectory() as tmp, \
                mock.patch.object(regen, "run", fake_run), mock.patch.object(regen, "provenance", lambda *a: {}):
            for replays in (0, -1):
                with self.assertRaises(ValueError):
                    regen.random_job("unused", 3, 0, 2, out, tmp, "boundary-flush", [], replays=replays)
                with self.assertRaises(ValueError):
                    regen.admission("unused", ["run", "x", "--delivery", "boundary-flush"], tmp, ("x",), replays)
                with self.assertRaises(ValueError):
                    regen.perturbations(replays, "x")
            self.assertEqual(calls, [])  # refused before any run
            regen.random_job("unused", 3, 0, 2, out, tmp, "boundary-flush", [], replays=2)
            self.assertEqual(calls[:3], [None, "up", "down"])
            self.assertEqual(len(set(calls[3:])), 2)  # more seeds stay optional


def cov_line(execs, cov, ft, event="pulse"):
    """A status line as libFuzzer's PrintStats writes it (its labels are six wide)."""
    return f"#{execs}\t{event:<6} cov: {cov} ft: {ft} corp: 10/1Kb lim: 4 exec/s: 100 rss: 40Mb\n"


def unit_line(execs, cov, ft, event="NEW"):
    """A NEW or REDUCE line (libFuzzer's PrintStatusForNewUnit)."""
    return cov_line(execs, cov, ft, event)[:-1] + " L: 5/10 MS: 2 ChangeBit-CopyPart-\n"


def final_stats(execs, added):
    return (f"stat::number_of_executed_units: {execs}\nstat::average_exec_per_sec:     540\n"
            f"stat::new_units_added:          {added}\nstat::slowest_unit_time_sec:    0\n"
            "stat::peak_rss_mb:              40\n")


def done_lines(execs, cov, ft, added):
    """libFuzzer's end at its stop file: the DONE line, `Done N runs`, its final stats."""
    return cov_line(execs, cov, ft, "DONE") + f"Done {execs} runs in 3700 second(s)\n" + final_stats(execs, added)


class Suspicion(unittest.TestCase):
    """The live stop heuristic (DEC-186 (1)): it suspects, it never certifies."""

    def test_a_flat_window_is_suspected_once_it_is_full(self):
        flat = [(0, 0, 0, 10000), (1800, 0, 5, 10005), (3600, 0, 10, 10010)]  # ft +0.1 %
        self.assertEqual(fuzzctl.suspected(flat, 3600),
                         {"window_minutes": 60, "fuzz_hours": 1.0, "ft": 10010, "ft_gain": 10})
        self.assertIsNone(fuzzctl.suspected(flat[:2] + [(3599, 0, 10, 10010)], 3600))  # not full
        self.assertIsNone(fuzzctl.suspected([], 3600))
        self.assertIsNone(fuzzctl.suspected(flat, float("inf")))
        self.assertNotIn("certifies", fuzzctl.suspected(flat, 3600))

    def test_one_edge_or_more_than_a_tenth_of_a_percent_of_ft_is_growth(self):
        self.assertIsNone(fuzzctl.suspected([(0, 0, 0, 10000), (3600, 1, 0, 10000)], 3600))
        self.assertIsNone(fuzzctl.suspected([(0, 0, 0, 10000), (3600, 0, 11, 10011)], 3600))
        # Growth before the window does not count.
        self.assertIsNotNone(fuzzctl.suspected([(0, 0, 0, 5000), (100, 50, 5000, 10000),
                                                (3700, 50, 5000, 10000)], 3600))

    def test_the_window_starts_at_or_before_its_nominal_start(self):
        # No row at 50 s: the window reaches back to the row at 0 s, and the growth there counts.
        self.assertIsNone(fuzzctl.suspected([(0, 0, 0, 9000), (3650, 10, 1000, 10000)], 3600))

    def test_a_leg_without_coverage_is_never_suspected(self):
        self.assertIsNone(fuzzctl.suspected([(0, 0, 0, 0), (7200, 0, 0, 0)], 3600))

    def test_a_target_s_rule_sets_its_cov_slack_and_can_leave_ft_out(self):
        # DEC-260: diff_chain's replays drift by a few edges and its ft by over 0.1 % an hour.
        rule = fuzzctl.Plateau(cov_slack=6, ft=False)
        drift = [(0, 0, 0, 10000), (3600, 6, 500, 10500)]
        self.assertIsNone(fuzzctl.suspected(drift, 3600))
        self.assertIsNotNone(fuzzctl.suspected(drift, 3600, rule))
        self.assertIsNone(fuzzctl.suspected([(0, 0, 0, 10000), (3600, 7, 0, 10000)], 3600, rule))
        # The slack alone keeps the ft test.
        self.assertIsNone(fuzzctl.suspected(drift, 3600, fuzzctl.Plateau(cov_slack=6)))
        self.assertEqual(fuzzctl.plateau_rule("ghost", [PUB]), fuzzctl.Plateau())
        self.assertEqual(fuzzctl.plateau_rule(PUB.name, [dataclasses_replace(PUB, plateau_cov_slack=6)]),
                         fuzzctl.Plateau(6, True))
        # The status reader's re-check of a stopped leg takes the same rule.
        leg = f"{PUB.name}@v24.0"
        marks = {leg: {"window_minutes": 60, "fuzz_hours": 1.0, "ft": 10500, "ft_gain": 500}}
        self.assertEqual(fuzzctl.standing({leg: drift}, marks, [PUB]), {})
        ruled = [dataclasses_replace(PUB, plateau_cov_slack=6, plateau_ft=False)]
        self.assertEqual(fuzzctl.standing({leg: drift}, marks, ruled), marks)


class LogScanning(unittest.TestCase):
    def setUp(self):
        self.d = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.d)

    def write(self, seg, name, *lines):
        path = os.path.join(self.d, "logs", f"segment-{seg}")
        os.makedirs(path, exist_ok=True)
        open(os.path.join(path, name), "a").write("".join(lines))

    def test_each_process_is_measured_from_its_own_inited(self):
        scan = fuzzctl.LogScan()
        self.assertEqual(scan(self.d), (0, 0, 0, None))
        self.write(1, "fuzz.out", "INFO: Running with entropic power schedule\n")
        self.write(1, "fuzz-0.log", cov_line(1, 10, 20), "#2\tNEW    cov: 12")
        self.write(1, "fuzz-1.log", cov_line(1, 11, 25))
        self.assertEqual(scan(self.d), (0, 0, 0, None))  # corpus loading is not growth
        self.write(1, "fuzz-0.log", " ft: 22 corp: 1\n", cov_line(3, 12, 22, "INITED"))
        # The parent's copy of an exited worker's log is not read (BL-86).
        self.write(1, "fuzz.out", cov_line(9, 999, 9999))
        self.assertEqual(scan(self.d), (0, 0, 22, None))  # fuzz-1 has not INITED
        self.write(1, "fuzz-1.log", cov_line(4, 11, 25, "INITED"), cov_line(5, 13, 30))
        dcov, dft, top, epoch = scan(self.d)
        self.assertEqual((dcov, dft, top), (2, 5, 30))
        self.assertIsNotNone(epoch)
        self.write(2, "fuzz.out", cov_line(3, 5, 9))  # a new segment reloads its corpus
        self.assertEqual(scan(self.d)[3], None)
        self.write(2, "fuzz.out", cov_line(4, 5, 9, "INITED"))
        self.assertNotIn(scan(self.d)[3], (None, epoch))
        scan.forget(self.d)
        self.assertEqual(scan.files, {})

    def test_a_lagging_process_shows_its_growth(self):
        # Sol r1 B1 (lower-job-growth): job 1 grows below job 0's flat maximum.
        scan = fuzzctl.LogScan()
        self.write(1, "fuzz-0.log", cov_line(1, 1000, 10000, "INITED"))
        self.write(1, "fuzz-1.log", cov_line(1, 900, 9000, "INITED"), cov_line(2, 960, 9600))
        self.assertEqual(scan(self.d)[:3], (60, 600, 10000))

    def test_a_restart_on_a_smaller_corpus_shows_its_growth(self):
        # Sol r1 B1 (reload-growth): segment 2 starts below segment 1 and grows.
        scan = fuzzctl.LogScan()
        self.write(1, "fuzz-0.log", cov_line(1, 1000, 10000, "INITED"))
        self.write(2, "fuzz-0.log", cov_line(1, 100, 1000, "INITED"), cov_line(2, 210, 1550))
        self.assertEqual(scan(self.d)[:2], (110, 550))


class ScriptRunner:
    """Stands in for run.sh in the live-heuristic tests: each slice writes a tagged run and calls
    `scenario(self)`, which writes worker logs (`append`) and samples the leg as the farm's poll
    would (`sample`, advancing the farm's monotonic fake clock)."""

    def __init__(self, tree, scenario, verdict=()):
        self.tree, self.scenario, self.extra = tree, scenario, None if verdict is None else dict(verdict)
        self.farm, self.calls, self.clock, self.run = None, [], 1_000_000.0, None

    def append(self, seg, worker, cov, ft, event="pulse"):
        d = os.path.join(self.run, "logs", f"segment-{seg}")
        os.makedirs(d, exist_ok=True)
        open(os.path.join(d, f"fuzz-{worker}.log"), "a").write(cov_line(1, cov, ft, event))

    def sample(self, dt=0):
        self.clock += dt
        self.farm.sample(Suspecting.LEG)

    def __call__(self, target, pin, args, on_start=None, timeout=None, cancel=None):
        sid = args[args.index("--run-tag") + 1]
        self.calls.append(sid)
        self.run = os.path.join(self.tree.fdir, target, "runs", f"20261011T000000Z-{pin}-1-{sid}")
        os.makedirs(self.run)
        self.scenario(self)
        if self.extra is None:  # killed before its verdict
            return Leg(target, pin, "run.sh: killed", -9, self.run, {})
        verdict = {"mode": "run", "pin": pin, "status": "pass", "cpu_hours": 0.1, "executions": 5,
                   "divergences": 0, "crashes": 0, "sink_failures": 0, "slow_units": 0, **self.extra}
        json.dump(verdict, open(os.path.join(self.run, "verdict.json"), "w"))
        found = sum(verdict[k] for k in fuzzctl.FINDING_KEYS)
        return Leg(target, pin, "node-fuzz fake", int(bool(found)), self.run, verdict)


def flat(minutes, seg=1):
    """A scenario: one process, INITED, then `minutes` of flat 30-second polls."""
    def scenario(r):
        r.append(seg, 0, 300, 1000, "INITED")
        r.sample()
        for _ in range(minutes * 2):
            r.append(seg, 0, 300, 1000)
            r.sample(30)
    return scenario


class Suspecting(Tree):
    LEG = "kv_model@v24.0"
    farm, record_slice = Farming.farm, Farming.record_slice

    def live_farm(self, scenario, verdict=(), **kw):
        runner = ScriptRunner(self, scenario, verdict)
        f, logs = self.farm(runner, clock=lambda: runner.clock, **kw)
        runner.farm = f
        return f, runner, logs

    def test_the_window_is_the_legs_own_fuzzing_time_across_rotated_slices(self):
        f, runner, _ = self.live_farm(flat(20))
        for n in range(3):
            self.assertEqual(f.slice(self.LEG, 0.1), "pass", n)
            runner.clock += 5000  # other legs' slices run in between: not this leg's time
        self.assertEqual(f.coverage[self.LEG][-1][0], 3600)
        p = f.suspicion(self.LEG)
        self.assertEqual((p["window_minutes"], p["fuzz_hours"], p["ft_gain"]), (60, 1.0, 0))
        # A suspicion is never a ledger status nor a certificate.
        self.assertTrue(all(r["status"] == "pass" for r in f.records()))
        self.assertNotIn("plateaued", fuzzctl.STATUSES)

    def test_growth_in_any_process_keeps_the_leg(self):
        def lower_job_grows(r):  # Sol r1 B1, lower-job-growth
            r.append(1, 0, 1000, 10000, "INITED")
            r.append(1, 1, 900, 9000, "INITED")
            r.sample()
            for n in range(1, 121):
                r.append(1, 1, 900 + n // 2, 9000 + 5 * n)
                r.sample(30)

        def reloaded_worker_grows(r):  # Sol r1 B1, reload-growth
            r.append(1, 0, 1000, 10000, "INITED")
            r.sample()
            for _ in range(10):
                r.sample(30)
            r.append(2, 0, 100, 1000, "INITED")
            r.sample()
            for n in range(1, 111):
                r.append(2, 0, 100 + n, 1000 + n * 5)
                r.sample(30)

        for name, scenario in (("lower", lower_job_grows), ("reload", reloaded_worker_grows)):
            f, _, _ = self.live_farm(scenario, night=name)
            self.assertEqual(f.slice(self.LEG, 0.1), "pass", name)
            self.assertGreaterEqual(f.coverage[self.LEG][-1][0], 3600, name)
            self.assertGreater(f.coverage[self.LEG][-1][1], 0, name)
            self.assertIsNone(f.suspicion(self.LEG), name)
            f.release()

    def test_an_interval_that_spans_a_corpus_load_is_not_fuzzing_time(self):
        def reload_between_polls(r):  # Sol r1 B2: every third interval holds a new segment's load
            r.append(1, 0, 100, 1000, "INITED")
            r.sample()
            seg = 1
            for n in range(1, 121):
                if n % 3 == 0:
                    seg += 1
                    r.append(seg, 0, 100, 1000, "INITED")
                r.sample(30)

        f, _, _ = self.live_farm(reload_between_polls)
        f.slice(self.LEG, 0.1)
        self.assertEqual(f.coverage[self.LEG][-1][0], 80 * 30)
        self.assertIsNone(f.suspicion(self.LEG))

    def test_polls_shorter_than_a_rows_resolution_still_add_up(self):
        # BL-134: rows keep fuzzing time to 0.1 s, and a poll that rounded to no change was lost,
        # so a fast poll never reached a window (the Suspecting cut test's slice ran to its bound).
        def short_polls(r):
            r.append(1, 0, 300, 1000, "INITED")
            r.sample()
            for _ in range(100):
                r.sample(0.03)

        f, _, _ = self.live_farm(short_polls)
        f.slice(self.LEG, 0.1)
        self.assertEqual(f.coverage[self.LEG][-1][0], 3.0)

    def test_durations_are_monotonic_seconds(self):
        # Sol r1 B3: the civil clock may step either way; the farm never reads it for a duration.
        self.assertIs(inspect.signature(fuzzctl.Farm.__init__).parameters["clock"].default, time.monotonic)
        civil = iter([0, 3600 * 5, -3600 * 5] * 1000)
        f, _ = self.farm(SliceRunner(), clock=time.monotonic)
        sid = "00000000000000a1"
        f.slice_ids[self.LEG] = sid
        seg = os.path.join(self.fdir, "kv_model", "runs", f"20261011T000000Z-v24.0-1-{sid}", "logs", "segment-1")
        os.makedirs(seg)
        with mock.patch.object(fuzzctl.time, "time", lambda: next(civil)):
            for n in range(3):
                open(os.path.join(seg, "fuzz.out"), "a").write(cov_line(n, 5, 5, "INITED" if not n else "pulse"))
                f.sample(self.LEG)
        self.assertLess(f.coverage[self.LEG][-1][0], 5)

    def test_a_suspected_leg_stops_for_the_night_and_its_slice_is_cut_within_a_poll(self):
        # The farm fsyncs each coverage and ledger row, which takes seconds on a loaded disk, so
        # the time to a suspicion is the disk's: the fake slice runs until it is cut (its own
        # bound only stops a broken farm), and the cut is timed from the suspicion.
        stop, suspected_at, cut_at = threading.Event(), [], []

        def runner(target, pin, args, on_start=None, **_):
            sid = args[args.index("--run-tag") + 1]
            run = os.path.join(self.fdir, target, "runs", f"20261011T000000Z-{pin}-1-{sid}")
            os.makedirs(os.path.join(run, "logs", "segment-1"))
            on_start(types.SimpleNamespace(pid=1))
            log = os.path.join(run, "logs", "segment-1", "fuzz.out")
            n, bound = 0, time.monotonic() + 600
            while True:
                open(log, "a").write(cov_line(n, 70, 140, "INITED" if n == 0 else "pulse"))
                if os.path.exists(os.path.join(run, "stop")):  # libFuzzer's loop reads the stop file (BL-173)
                    cut_at.append(time.monotonic())
                    stop.set()
                if stop.wait(0.01) or time.monotonic() > bound:
                    break
                n += 1
            v = {"status": "fail", "cpu_hours": 0.01, "executions": n, "divergences": 0, "crashes": 0,
                 "sink_failures": 0, "libfuzzer_status": 72}
            json.dump(v, open(os.path.join(run, "verdict.json"), "w"))
            return Leg(target, pin, "node-fuzz fake", 1, run, v)

        original = fuzzctl.Farm.suspect

        def suspect(farm, leg, p):
            original(farm, leg, p)
            suspected_at.append(time.monotonic())

        signalled = []
        f, logs = self.farm(runner, clock=time.monotonic, plateau_minutes=0.003, only=["kv_model"])  # 0.18 s
        with mock.patch.object(fuzzctl.os, "killpg", lambda pid, sig: signalled.append(sig)), \
                mock.patch.object(fuzzctl.Farm, "suspect", suspect), mock.patch.object(fuzzctl.Farm, "triage"):
            self.assertEqual(f.work(), "complete")
        self.assertEqual(signalled, [])  # the stop file was enough
        self.assertTrue(stop.is_set())
        self.assertEqual(len(suspected_at), 1)
        self.assertLess(cut_at[0] - suspected_at[0], 5)  # cut in the poll that suspected it
        (row,) = f.records()
        self.assertEqual(row["status"], "interrupted")  # DEC-186: a cut slice is plain interrupted
        self.assertNotIn("plateau", row)
        self.assertEqual(f.suspicions[self.LEG]["ft"], 140)
        self.assertTrue(any("kv_model@v24.0 is suspected of a plateau at ft 140" in line for line in logs))
        self.assertIn("suspected_legs=1", logs[-1])
        # The next launch finds it suspected from its history alone.
        f.release()
        runner2 = SliceRunner()
        f2, logs = self.farm(runner2, plateau_minutes=0.003, only=["kv_model"])
        with mock.patch.object(fuzzctl.Farm, "triage"):
            self.assertEqual(f2.work(), "complete")
        self.assertEqual(runner2.calls, [])
        self.assertTrue(any("1 legs suspected of a plateau earlier in the night" in line for line in logs))

    def test_a_finding_voids_the_history_and_the_suspicion(self):
        f, runner, _ = self.live_farm(flat(60))
        f.slice(self.LEG, 0.1)
        f.suspect(self.LEG, f.suspicion(self.LEG))
        runner.extra = {"divergences": 1, "status": "fail"}
        with mock.patch.object(fuzzctl.Farm, "triage"):
            self.assertEqual(f.slice(self.LEG, 0.1), "finding")
        self.assertEqual((f.suspicion(self.LEG), f.suspicions), (None, {}))
        f.release()
        f2, _ = self.farm(SliceRunner())
        self.assertEqual((f2.coverage[self.LEG], f2.suspicions), ([], {}))

    def test_a_live_slice_without_a_verdict_is_judged_by_what_its_run_left(self):
        def crashed(r):  # Sol r1 B4: missing-verdict-with-crash
            flat(10)(r)
            os.makedirs(os.path.join(r.run, "crashes"))
            open(os.path.join(r.run, "crashes", "crash-fixture"), "wb").write(b"boom")

        f, runner, _ = self.live_farm(crashed, verdict=None)
        f.interrupted.add(self.LEG)
        with mock.patch.object(fuzzctl.Farm, "triage") as tri:
            self.assertEqual(f.slice(self.LEG, 0.1), "finding")
        self.assertEqual(f.records()[-1]["crashes"], 1)
        self.assertTrue(tri.called)
        self.assertEqual(f.coverage[self.LEG], [])

    def test_history_survives_a_worker_restart_and_a_torn_row(self):
        f, runner, _ = self.live_farm(flat(20))
        f.slice(self.LEG, 0.1)
        history = list(f.coverage[self.LEG])
        open(os.path.join(f.dir, fuzzctl.COVERAGE), "a").write('{"leg": "kv_mo')  # torn
        f.release()
        f2, _ = self.farm(SliceRunner())
        self.assertEqual(f2.coverage[self.LEG], history)
        self.assertEqual(f2.ledger_repaired, 1)

    def test_a_bad_coverage_history_fails_closed(self):
        f, _ = self.farm(SliceRunner())
        f.release()
        path = os.path.join(f.dir, fuzzctl.COVERAGE)
        row = {"leg": self.LEG, "fuzz_s": 0, "cov_gain": 0, "ft_gain": 0, "ft": 9}
        good = json.dumps(row)
        for bad in ("not json\n", json.dumps({**row, "leg": "ghost@v1"}) + "\n",
                    json.dumps({**row, "leg": ["unhashable"]}) + "\n",
                    json.dumps({**row, "fuzz_s": 3600}) + "\n",  # Sol r1 m1: no 0-second start
                    json.dumps({**row, "fuzz_s": -1}) + "\n",
                    json.dumps({k: v for k, v in row.items() if k != "ft_gain"}) + "\n",
                    good + "\n" + json.dumps({**row, "fuzz_s": 11, "ft": 4}) + "\n",
                    good + "\n" + json.dumps({**row, "fuzz_s": 11, "cov_gain": -1}) + "\n",
                    json.dumps({"leg": self.LEG, "suspected": {"ft": 1}}) + "\n"):
            open(path, "w").write(bad)
            with self.assertRaises(fuzzctl.FarmStateError, msg=bad[:60]):
                self.farm(SliceRunner())
        open(path, "w").write(good + "\n")
        self.farm(SliceRunner())

    def test_status_names_only_public_legs(self):
        f, runner, _ = self.live_farm(flat(60))
        f.slice(self.LEG, 0.1)
        f.suspect(self.LEG, f.suspicion(self.LEG))
        f.release()
        # A night from before FZ1: no coverage history, rows without slow_units.
        old = fuzzctl.Farm(farm_args(night="old"), TARGETS, NoBench(), runner=SliceRunner(), log=lambda _: None)
        self.record_slice(old, self.LEG, "pass", 0.1, 1)
        old.release()
        out = io.StringIO()
        with mock.patch.object(fuzzctl, "unit_state", return_value="inactive"), \
                mock.patch.object(fuzzctl, "farm_units", return_value=[]), contextlib.redirect_stdout(out):
            self.assertTrue(fuzzctl.farm_status(farm_args(night=None), TARGETS))
        text = out.getvalue()
        self.assertIn("night=old ", text)
        self.assertIn("legs_suspected=1/3", text)
        self.assertIn("node-fuzz farm night=test kv_model@v24.0=plateau-suspected@ft1000(60m)", text)
        self.assertNotIn("certified", text.replace("0 certified", ""))
        os.utime(fuzzctl.ledger_path(f.dir))  # the latest night
        rc, out = self.cli(["status-line"])
        self.assertEqual(rc, 0)
        self.assertIn("SW-FUZZ farm night=test: 1/3 legs stopped on a suspected plateau (not certified; "
                      "see `fuzzctl certify`): kv_model@v24.0=plateau-suspected@ft1000(60m)", out)

    def test_a_removed_or_private_target_is_counted_never_named(self):
        # Sol r1 M3: a leg is named only when the current registry says its target is public.
        gone = Target("fixture_private", "l1", ("v24.0",), "private", 60, 1)
        f = fuzzctl.Farm(farm_args(night="m3"), TARGETS + [gone], NoBench(), runner=SliceRunner(),
                         log=lambda _: None)
        p = {"window_minutes": 60, "fuzz_hours": 1.0, "ft": 5, "ft_gain": 0}
        for leg in ("fixture_private@v24.0", "diff_tx_decode@v24.0", self.LEG):
            for fuzz_s in (0, 3600):
                fuzzctl.append_row(f.dir, {"leg": leg, "fuzz_s": fuzz_s, "cov_gain": 0, "ft_gain": 0, "ft": 5},
                                   fuzzctl.COVERAGE)
            f.suspect(leg, p)
        f.record({"event": "start", "slice": "00000000000000aa", "leg": self.LEG, "time": 0})
        f.release()
        for registry in ([PUB, SELF], [dataclasses_replace(PUB, route="private"), SELF]):
            line = fuzzctl.farm_plateau_line(self.fdir, registry)
            self.assertNotIn("fixture_private", line)
            self.assertNotIn("diff_tx_decode", line)
            public = registry[0].route == "public"
            self.assertIn("3/4 legs stopped", line)
            self.assertIn("; 2 other legs" if public else "; 3 other legs", line)
            self.assertEqual("kv_model" in line, public)
        self.assertEqual(fuzzctl.legs_named(["fixture_private@v24.0", "kv_model@v24.0"], [SELF]), [])


class SuspectingMore(Tree):
    LEG = Suspecting.LEG
    farm, live_farm = Farming.farm, Suspecting.live_farm

    def test_growth_before_every_process_has_inited_still_counts(self):
        def scenario(r):
            r.append(1, 0, 100, 1000, "INITED")
            r.append(1, 0, 105, 1010)
            r.sample()  # worker 1 is still loading: no epoch yet
            r.append(1, 1, 50, 500, "INITED")
            r.sample(30)

        f, _, _ = self.live_farm(scenario)
        f.slice(self.LEG, 0.1)
        self.assertEqual(f.coverage[self.LEG][-1][1:3], (5, 10))

    def test_a_verdict_does_not_hide_what_the_run_left(self):
        def scenario(r):
            flat(5)(r)
            open(os.path.join(r.run, "logs", "segment-1", "fuzz-0.log"), "a").write(PANIC_LOG.split("AB\n")[0])

        f, _, _ = self.live_farm(scenario, verdict={"status": "fail", "libfuzzer_status": 72})
        f.interrupted.add(self.LEG)
        with mock.patch.object(fuzzctl.Farm, "triage"):
            self.assertEqual(f.slice(self.LEG, 0.1), "finding")
        self.assertEqual(f.records()[-1]["crashes"], 1)

    def test_a_suspicion_stops_showing_once_the_leg_grows_again(self):
        f, runner, _ = self.live_farm(flat(15), plateau_minutes=15)
        f.slice(self.LEG, 0.1)
        f.suspect(self.LEG, f.suspicion(self.LEG))
        line = functools.partial(fuzzctl.farm_plateau_line, self.fdir, TARGETS)
        self.assertIn("1/3 legs stopped", line())

        def grows(r):
            r.append(1, 0, 300, 1000, "INITED")
            r.sample()
            for n in range(1, 31):
                r.append(1, 0, 300 + n, 1000 + n)
                r.sample(30)

        runner.scenario = grows  # a later launch with a longer window ran it again, and it grew
        f.slice(self.LEG, 0.1)
        self.assertIn("0/3 legs stopped", line())


class Sharing(Tree):
    """Sol r1 M4: a launch's legs split the night's cap; nothing is redistributed, and no sequence
    of launches spends more than the cap."""

    farm = Farming.farm

    def test_shares_split_the_cap_among_the_launchs_legs(self):
        plan = {"a": 10.0, "b": 10.0, "c": 10.0}
        self.assertEqual(fuzzctl.launch_shares(plan, 30, {"a", "b", "c"}), plan)
        self.assertEqual(fuzzctl.launch_shares(plan, 30, {"a"}), {"a": 30.0})
        self.assertEqual(fuzzctl.launch_shares(plan, 30, set()), {})

    def test_a_suspected_leg_leaves_its_share_unspent(self):
        # Sol's share fixture: C stops early; A and B still get only their own shares.
        f, _ = self.farm(SliceRunner())
        g = "diff_tx_decode@v24.0"
        f.coverage[g] = [(0, 0, 0, 100), (3600, 0, 0, 100)]
        runner = SliceRunner()
        f.runner = runner
        with mock.patch.object(fuzzctl.Farm, "triage"):
            self.assertEqual(f.work(), "complete")
        spent = fuzzctl.credit_by_leg(f.records())
        self.assertNotIn(g, spent)
        for leg in set(f.plan["legs"]) - {g}:
            self.assertAlmostEqual(spent[leg], f.plan["legs"][leg], places=3)
        self.assertLess(sum(spent.values()), f.plan["cpu_hours"] - 1)

    def test_no_sequence_of_launches_exceeds_the_cap(self):
        runner = SliceRunner()
        f, _ = self.farm(runner, only=["kv_model"])  # its share is the whole cap
        with mock.patch.object(fuzzctl.Farm, "triage"):
            self.assertEqual(f.work(), "complete")
        self.assertAlmostEqual(sum(c for _, _, c in runner.calls), f.plan["cpu_hours"], places=3)
        f.release()
        runner = SliceRunner()
        f, logs = self.farm(runner)  # every leg: their shares are unspent, but the cap is
        with mock.patch.object(fuzzctl.Farm, "triage"):
            self.assertEqual(f.work(), "cap")
        self.assertEqual(runner.calls, [])
        self.assertLessEqual(fuzzctl.total(f.records(), "cpu_hours"), f.plan["cpu_hours"] + 1e-6)

    def test_time_voided_by_a_finding_still_counts_toward_the_cap(self):
        f, _ = self.farm(SliceRunner())
        legs_ = list(f.plan["legs"])
        for n, leg in enumerate(legs_, 1):
            Farming.record_slice(self, f, leg, "finding" if n == 1 else "pass", f.plan["legs"][leg], n)
        os.makedirs(os.path.join(self.fdir, legs_[0].split("@")[0]), exist_ok=True)
        json.dump({"clusters": {}, "runs": ["run-1"]},  # triaged, since fixed
                  open(os.path.join(self.fdir, legs_[0].split("@")[0], "triage.json"), "w"))
        f.release()
        runner = SliceRunner()
        f, _ = self.farm(runner)
        with mock.patch.object(fuzzctl.Farm, "triage"):
            self.assertEqual(f.work(), "cap")
        self.assertEqual(runner.calls, [])


def certify_args(leg, **kw):
    return argparse.Namespace(**{"leg": leg, "seconds": 3600, "wall_cap_hours": 12.0, "launch": False,
                                 "print_only": False, **kw})


class Certifying(Tree):
    """`certify` (DEC-186 (2), amendment 2) on fixture runs as run.sh's certify mode leaves them:
    the merge, a replay pass, the window, a replay pass."""

    REV = "0123456789ab"
    BIN = "5" * 64

    def certify_run(self, target="kv_model", pin="v24.0", tag=None, window=3600,
                    base=(300, 1000), final=(300, 1000), new=0, wall_s=3700.4, cpu=(40.0, 3641.2),
                    execs=2_000_000, stopped_by="cpu-window", verdict=None, win=None, before=(), workers=1,
                    replays=(0, 0), source=None, window_log=None, stamp="20261011T000000Z", inited=None,
                    added=None, done=None, certificate=False):
        run = os.path.join(self.fdir, target, "runs", f"{stamp}-{pin}-7-{tag or secrets_hex()}")
        name = os.path.basename(run)
        record(run, pin=pin)  # segment-1: the merge
        open(os.path.join(run, "mode"), "w").write("certify\n")
        logs = os.path.join(run, "logs")
        # The replays: the binary once over the corpus, whose INITED line is the corpus's cov/ft.
        for seg, cov in ((fuzzctl.REPLAY_BEFORE, base), (fuzzctl.REPLAY_AFTER, final)):
            if cov is not None:
                os.makedirs(os.path.join(logs, seg))
                open(os.path.join(logs, seg, "fuzz.out"), "w").write(
                    cov_line(3, *cov, "INITED") + cov_line(3, *cov, "DONE  ") + "stat::number_of_executed_units: 3\n")
        seg = os.path.join(logs, fuzzctl.WINDOW)
        os.makedirs(seg)
        # The window as libFuzzer leaves it: ended at its stop file (the DONE line, `Done N runs`,
        # final stats), or by SIGINT (no DONE line; the wall cap).
        inited = inited or base or (300, 1000)
        top = (inited[0] + 1, inited[1] + 5) if new else inited
        units = new if added is None else added
        lines = list(before) + [cov_line(2, *inited, "INITED"), cov_line(4, *inited)]
        lines += [unit_line(5 + n, *top) for n in range(new)]
        if execs is not None and stopped_by == "cpu-window":
            lines += [done_lines(execs + 2, *(done or top), units)]
        elif execs is not None:
            lines += ["==7== libFuzzer: run interrupted; exiting\n" + final_stats(execs + 2, units)]
        status = 0 if stopped_by == "cpu-window" else 72
        names = ["fuzz.out"] if workers == 1 else [f"fuzz-{n}.log" for n in range(workers)]
        for name_ in names:
            open(os.path.join(seg, name_), "wb").write(window_log or "".join(lines).encode())
        json.dump({"target": target, "pin": pin, "run": name, "cpu_window": window, "wall_cap": 43200,
                   "elapsed": 12.0 + wall_s, "load_seconds": 12.0, "wall_seconds": wall_s,
                   "cpu_at_inited": cpu[0], "cpu_at_stop": cpu[1], "stopped_by": stopped_by, "killed": False,
                   "status": status, "stop_to_exit": None if stopped_by is None else 2.5, **(win or {})},
                  open(os.path.join(seg, "window.json"), "w"))
        v = {"mode": "certify", "target": target, "pin": pin, "run": name, "status": "pass",
             "libfuzzer_status": status, "divergences": 0, "crashes": 0, "sink_failures": 0, "refused": 0,
             "slow_units": 0, "rust_dashcore": self.REV, "rust_dashcore_dirty": False, "oracle_flavor": "fuzz",
             "certify": {"engine": "single", "jobs": 1, "seconds": window, "wall_cap": 43200, "merge_status": 0,
                         "replay_before_status": replays[0], "replay_after_status": replays[1]}}
        for k, val in (verdict or {}).items():
            if val is None:
                v.pop(k, None)
            else:
                v[k] = val
        json.dump(v, open(os.path.join(run, "verdict.json"), "w"))
        if source is not False:  # run.sh's record: commit, clean or dirty, run, the binary's sha256
            source = source or (self.REV, "clean" if v.get("rust_dashcore_dirty") is False else "dirty", name, self.BIN)
            open(os.path.join(run, "source"), "w").write("\n".join(source) + "\n")
        if certificate:  # as `certify` writes it once it completed, finding the leg eligible; a
            # Plateau or Replay is the rule it recorded, True a certificate from before DEC-260 (none)
            rule = {} if certificate is True else {
                "plateau" if isinstance(certificate, fuzzctl.Plateau) else "replay": certificate.record()}
            fuzzctl.write_private(os.path.join(run, fuzzctl.CERTIFICATE), json.dumps(
                {"evidence": fuzzctl.certify_evidence(run), "cutoff": stamp, "eligibility": [], **rule}))
        return run

    def judged(self, **kw):
        status, reasons, ev = fuzzctl.read_certificate(self.certify_run(**kw))
        return status

    def certs(self, targets=TARGETS):
        """The status reader's view once the leg's runs are triaged (an untriaged run's findings
        are unknown, so its leg cannot be certified: `eligibility`)."""
        fuzzctl.run_triage(targets, minimize=False)
        return fuzzctl.certificates(self.fdir, targets)

    def test_a_clean_flat_hour_certifies(self):
        status, reasons, ev = fuzzctl.read_certificate(self.certify_run(final=(300, 1001)))
        self.assertEqual((status, reasons), ("certified", []))
        self.assertEqual((ev["baseline"], ev["final"], ev["new_lines"], ev["cpu_seconds"], ev["processes"],
                          ev["executions"]), ((300, 1000), (300, 1001), 0, 3601.2, 1, 2_000_000))

    def test_coverage_is_the_replays_and_any_discovery_in_the_window_is_growth(self):
        self.assertEqual(self.judged(final=(301, 1000)), "growing")  # one edge
        self.assertEqual(self.judged(final=(300, 1002)), "growing")  # ft +0.2 %
        self.assertEqual(self.judged(new=1), "growing")  # a NEW line, the replays flat
        # DEC-186 amendment 3 (b): the DONE line, measured in the process at its end, against its
        # INITED: cov equal, ft within 0.1 %.
        ev = fuzzctl.certify_evidence(self.certify_run())
        for done, status in (((301, 1000), "growing"), ((300, 1002), "growing"), ((300, 1001), "certified")):
            with self.subTest(done=done):
                self.assertEqual(fuzzctl.judge({**ev, "window_done": done})[0], status)
        # A DONE line above every line printed before it means a lost line (each discovery prints
        # one): the log breaks its grammar.
        for done in ((301, 1000), (300, 1001)):
            with self.subTest(done=done):
                self.assertEqual(self.judged(done=done), "failed")
        # Sol r2 B1's torn NEW line (SIGINT cut it short of its ft) is no longer a window's end:
        # a log like it breaks the grammar, and a window that breaks it measures nothing.
        torn = (cov_line(2, 300, 1000, "INITED") + cov_line(1048576, 300, 1000) +
                "\tNEW_FUNC[1/1]: 0x602585dc9eb1\n#1200001\tNEW    cov: 301==1032383== libFuzzer: run interrupted; "
                "exiting\nstat::number_of_executed_units: 1200001\nstat::new_units_added:          0\n")
        self.assertEqual(self.judged(window_log=torn.encode()), "failed")
        self.assertEqual(self.judged(window_log=torn.replace("#1200001\tNEW    cov: 301", "").encode()), "failed")
        # Lines before INITED (the corpus load) are not the window's: they change nothing.
        self.assertEqual(self.judged(before=[cov_line(1, 10, 20), "INFO: anything\n", "#2\tREAD\n"]), "certified")

    def test_a_discovery_under_any_label_is_growth(self):
        # libFuzzer prints a discovery as REDUCE when its parent input was ever reduced (the real
        # smoke's window had 355 of them), and as RELOAD when it re-reads the corpus.
        def window(*lines, done=None):
            added = sum(fuzzctl.UNIT_TAIL.search(line.encode()) is not None for line in lines)
            last = fuzzctl.STATUS_FORM.fullmatch(lines[-1].encode().rstrip(b"\n")) if lines else None
            done = done or ((int(last.group(3)), int(last.group(4))) if last else (300, 1000))
            return (cov_line(2, 300, 1000, "INITED") + "".join(lines) + done_lines(2000002, *done, added)).encode()
        reduce = lambda n, ft: unit_line(n, 300, ft, "REDUCE")
        ev = fuzzctl.certify_evidence(self.certify_run(window_log=window(cov_line(6000, 301, 1000, "RELOAD"))))
        self.assertEqual((ev["new_lines"], ev["log_problems"]), (1, []))
        for log in (window(reduce(5000, 1001)), window(cov_line(6000, 301, 1000, "RELOAD")),
                    window(cov_line(8192, 300, 1001)), window(unit_line(9000, 301, 1003)),
                    window("\tNEW_FUNC[1/1]: 0x602585dc9eb1 in edge2 probe.cpp:3\n", unit_line(9000, 301, 1003))):
            with self.subTest(log=log[60:140]):
                self.assertEqual(self.judged(window_log=log), "growing")
        # A reduction that finds nothing (the same cov and ft, a smaller input) is no growth.
        self.assertEqual(self.judged(window_log=window(reduce(5000, 1000), cov_line(8192, 300, 1000))), "certified")
        # Each discovery counts once: the lines after it keep its totals.
        ev = fuzzctl.certify_evidence(self.certify_run(window_log=window(
            reduce(5000, 1001), reduce(5001, 1001), cov_line(8192, 300, 1001), unit_line(9000, 301, 1003),
            unit_line(9001, 301, 1003, "REDUCE"), unit_line(9002, 301, 1004, "REDUCE"))))
        self.assertEqual((ev["window_inited"], ev["window_done"], ev["new_lines"], ev["log_problems"]),
                         ((300, 1000), (301, 1004), 3, []))

    def test_a_window_log_that_breaks_its_grammar_fails(self):
        # DEC-186 amendment 3 (c): after INITED every line is whole, newline-terminated and of
        # one of libFuzzer's forms; the DONE line, `Done N runs` and each final stat appear once,
        # in order, and nothing follows them. Anything torn, repeated, unknown or out of place
        # fails, as does a log that lost lines (libFuzzer ignores its write errors).
        good = (cov_line(2, 300, 1000, "INITED") + unit_line(5000, 300, 1000, "REDUCE") + cov_line(8192, 300, 1000)
                + done_lines(2000002, 300, 1000, 1))
        self.assertEqual(self.judged(window_log=good.encode()), "certified")
        dictionary = ('###### Recommended dictionary. ######\n"\\001\\000ab\\"\\\\" # Uses: 7\n'
                      "###### End of recommended dictionary. ######\n")
        unit, pulse = unit_line(5000, 300, 1000, "REDUCE"), cov_line(8192, 300, 1000)
        harness = "kv_model: 64 inputs Core reads with a flag byte the node refuses (A-12, DEC-98)\n"
        for log in (good.replace("Done ", dictionary + "Done "), good.replace(unit, unit + harness),
                    good.replace(" MS: 2 ChangeBit-CopyPart-", ' MS: 1 CMP- DE: "\\377\\377ab"-'),
                    good.replace(" MS: 2 ChangeBit-CopyPart-", " MS: 12 " + "CopyPart-" * 10),
                    good.replace(unit, unit + "Slowest unit: 3 s:\nartifact_prefix='/p/crashes/'; Test unit written to "
                                 "/p/crashes/slow-unit-ab\nBase64: YWI=\n"),
                    # Sol r4 m1: libFuzzer prints the prefix as it is, a `'` in the sink path included.
                    good.replace(unit, unit + "Slowest unit: 3 s:\nartifact_prefix='/p/o'b/crashes/'; Test unit written "
                                 "to /p/o'b/crashes/slow-unit-ab\nBase64: YWI=\n")):
            with self.subTest(log=log[-260:-180]):
                self.assertEqual(self.judged(window_log=log.encode()), "certified")
        stats_end = good.index("stat::peak_rss_mb")
        for log in (
                # Sol r3 B2's small fixtures: an unterminated counter, and a stale one before a torn one.
                (cov_line(2, 300, 1000, "INITED") + cov_line(1024, 300, 1000)
                 + "==7== libFuzzer: run interrupted; exiting\nstat::number_of_executed_units: 2000002\n"
                 "stat::new_units_added: 0"),
                (cov_line(2, 300, 1000, "INITED") + cov_line(1024, 300, 1000)
                 + "==7== libFuzzer: run interrupted; exiting\nstat::number_of_executed_units: 2000002\n"
                 "stat::new_units_added: 0\nstat::new_units_added: "),
                good[:-1],  # unterminated
                good.replace("stat::new_units_added:          1", "stat::new_units_added:          11"),
                good.replace("stat::new_units_added:          1\n", "stat::new_units_added:          1"),
                good.replace("stat::new_units_added:          1\n", "stat::new_units_added:          1\n" * 2),
                good.replace("stat::new_units_added:          1\n", ""),
                good[:stats_end] + good[stats_end:].replace("stat::peak_rss_mb", "stat::peak_rss_mb:  1\nstat::peak_rss_mb"),
                good + "kv_model: anything\n",  # after the final stats
                good.replace("Done ", "Done 5 runs in 3 second(s)\nDone "),  # twice
                good.replace("Done 2000002", "Done 2000001"),  # counts that disagree
                good.replace(unit, ""),  # lost a line: 1 unit added, no NEW or REDUCE line
                # the unit balance holds in each of these: the grammar alone fails them
                good.replace(unit, unit.replace(" L: 5/10", "")), good.replace(pulse, pulse[:40] + "\n"),
                good.replace(unit, unit.replace("MS: 2", "MS: 3")),
                good.replace(pulse, "junk" + pulse), good.replace(pulse, "x" + pulse),
                good.replace(unit, unit + "#5000\n"), good.replace(unit, unit + "INFO: something\n"),
                good.replace(unit, unit + "==7== libFuzzer: run interrupted; exiting\n"),
                good.replace(unit, unit + cov_line(6000, 300, 1000, "INITED")),  # a second INITED
                good.replace(unit, unit + cov_line(4000, 300, 1000)),  # executions go backwards
                good.replace(unit, unit + cov_line(6000, 299, 1000)),  # so does cov
                good.replace(cov_line(8192, 300, 1000), cov_line(8192, 300, 1000).replace("Mb", "Mb tail")),
                good.replace(cov_line(2000002, 300, 1000, "DONE"), ""),  # no DONE line
                good.replace(cov_line(2, 300, 1000, "INITED"), cov_line(2, 300, 1000, "INITED")[:30]),
                good.replace("#2\tINITED", "#2\tINITED  "), good.replace(" exec/s: 100", " exec/s: 1 00")):
            with self.subTest(log=log[-200:]):
                self.assertEqual(self.judged(window_log=log.encode()), "failed")
        ev = fuzzctl.certify_evidence(self.certify_run(window_log=good.encode()))
        self.assertEqual((ev["log_problems"], ev["unit_lines"], ev["units_added"], ev["new_lines"]), ([], 1, 1, 0))

    def test_a_cpu_window_certifies_however_long_it_took_on_the_wall_clock(self):
        # DEC-186: the window is the process's CPU time; at Nice 19 an hour of it takes ten.
        self.assertEqual(self.judged(wall_s=10 * 3600.0), "certified")

    def test_the_wall_cap_is_starved_never_certified(self):
        for kw in (dict(), dict(final=(305, 1100)), dict(new=3), dict(replays=(0, None), final=None)):
            with self.subTest(**{k: str(v) for k, v in kw.items()}):
                self.assertEqual(self.judged(stopped_by="wall-cap", wall_s=43190.0, cpu=(40.0, 2000.0), **kw),
                                 "starved")
        # Sol r2 M4: a corpus load that never reached INITED, killed after ignoring the stop. With
        # the selftest's shorter grace it is starved; a kill after the constant 1500 s is always
        # later than it, so in production it fails (Sol r4 B1).
        stuck = {"load_seconds": None, "wall_seconds": None, "cpu_at_inited": None, "cpu_at_stop": 900.0,
                 "elapsed": 43200.4, "killed": True, "status": -9, "stop_to_exit": 1.2}
        kw = dict(stopped_by="wall-cap", verdict={"libfuzzer_status": 137}, execs=None, replays=(0, None), final=None)
        self.assertEqual(self.judged(win=stuck, **kw), "starved")
        self.assertEqual(self.judged(win={**stuck, "stop_to_exit": 1500.6}, **kw), "failed")

    def test_too_few_executions_are_insufficient_progress(self):
        self.assertEqual(self.judged(execs=999_999), "insufficient-progress")
        self.assertEqual(self.judged(execs=1_000_000), "certified")
        self.assertEqual(self.judged(execs=10, final=(301, 1000)), "growing")
        # A slow oracle target sets its own floor in targets.toml; a removed target gets the default.
        run = self.certify_run(execs=20_000, tag="ffffffffffffffff", certificate=True)  # the leg's latest run
        self.assertEqual(fuzzctl.read_certificate(run)[0], "insufficient-progress")
        slow = [dataclasses_replace(PUB, min_certify_execs=10_000), PRIV, SELF]
        self.assertEqual(self.certs(slow)["kv_model@v24.0"][0], "certified")
        self.assertEqual(fuzzctl.certify_floor("ghost", slow), fuzzctl.CERTIFY_EXECS)

    def test_a_target_s_rule_sets_its_cov_slack_and_can_leave_ft_out(self):
        # DEC-260: within the slack, the window's in-process cov is flat; without ft, neither ft
        # nor a discovery line is growth (DONE against INITED bounds the cov found in the window).
        # The plateau slack does not reach the replays, which are other processes (DEC-271);
        # leaving ft out of the growth tests leaves out the replayed ft gain too.
        rule = fuzzctl.Plateau(cov_slack=6, ft=False)
        ev = fuzzctl.certify_evidence(self.certify_run())
        for change, default, ruled in (
            ({"window_done": (306, 1000)}, "growing", "certified"),
            ({"window_done": (294, 1000)}, "growing", "certified"),
            ({"window_done": (307, 1000)}, "growing", "growing"),
            ({"window_done": (300, 1100)}, "growing", "certified"),
            ({"new_lines": 3}, "growing", "certified"),
            ({"final": (300, 1100)}, "growing", "certified"),
            ({"final": (301, 1000)}, "growing", "growing"),
            ({"final": (299, 1000)}, "failed", "failed"),
            ({"final": (300, 900)}, "failed", "failed"),
        ):
            with self.subTest(change=change):
                self.assertEqual(fuzzctl.judge({**ev, **change})[0], default)
                self.assertEqual(fuzzctl.judge({**ev, **change}, rule=rule)[0], ruled)
        # The status reader judges by the stricter of the registry's rule and the certificate's.
        # A window that found 1 cov and 5 ft in its process:
        ruled = [dataclasses_replace(PUB, plateau_cov_slack=6, plateau_ft=False), PRIV, SELF]
        tighter = [dataclasses_replace(PUB, plateau_cov_slack=0, plateau_ft=False), PRIV, SELF]
        self.certify_run(new=1, tag="ffffffffffffffff", certificate=rule)
        self.assertEqual(self.certs()["kv_model@v24.0"][0], "growing")
        self.assertEqual(self.certs(ruled)["kv_model@v24.0"][0], "certified")
        self.assertEqual(self.certs(tighter)["kv_model@v24.0"][0], "growing")
        # A certificate from before DEC-260 was judged by the default: a later rule does not certify
        # it; nor does one that recorded the ft test.
        for tag, certificate in (("fffffffffffffff0", True), ("fffffffffffffff2", fuzzctl.Plateau(6, True))):
            self.certify_run(new=1, tag=tag, certificate=certificate)
            self.assertEqual(self.certs(ruled)["kv_model@v24.0"][0], "growing", msg=tag)

    def test_a_target_s_replay_rule_bounds_the_comparisons_between_processes(self):
        # DEC-271: the two replays are processes of their own, which differ on one corpus. A
        # replay after the window may fall by the rule's cov and ft (and gain as much without
        # growing); falling further does not reproduce. The ft allowance is never under 0.1 %.
        replay = fuzzctl.Replay(cov_slack=30, ft_slack=120)
        ev = fuzzctl.certify_evidence(self.certify_run(base=(300, 100000), final=(300, 100000), done=(300, 100000)))
        self.assertEqual((replay.ft_allowance(100000), fuzzctl.Replay(ft_slack=50).ft_allowance(100000)), (120, 100))
        for change, default, ruled in (
            ({"final": (270, 100000)}, "failed", "certified"),
            ({"final": (269, 100000)}, "failed", "failed"),
            ({"final": (330, 100000)}, "growing", "certified"),
            ({"final": (331, 100000)}, "growing", "growing"),
            ({"final": (300, 99900)}, "certified", "certified"),
            ({"final": (300, 99880)}, "failed", "certified"),
            ({"final": (300, 99879)}, "failed", "failed"),
            ({"final": (300, 100120)}, "growing", "certified"),
            ({"final": (300, 100121)}, "growing", "growing"),
            ({"window_done": (301, 100000)}, "growing", "growing"),
            ({"window_done": (300, 100101)}, "growing", "growing"),
        ):
            with self.subTest(change=change):
                self.assertEqual(fuzzctl.judge({**ev, **change})[0], default)
                self.assertEqual(fuzzctl.judge({**ev, **change}, replay=replay)[0], ruled)
        self.assertEqual(fuzzctl.replay_rule("ghost", [PUB]), fuzzctl.Replay())
        self.assertEqual(fuzzctl.replay_rule(PUB.name, [dataclasses_replace(PUB, replay_cov_slack=30)]),
                         fuzzctl.Replay(30, 0))
        self.assertEqual(fuzzctl.Replay(30, 50).strictest(fuzzctl.Replay(20, 120)), fuzzctl.Replay(20, 50))
        # The status reader judges by the stricter of the registry's rule and the certificate's,
        # in cov and in ft; a certificate from before DEC-271 was judged by the default.
        ruled = [dataclasses_replace(PUB, replay_cov_slack=30, replay_ft_slack=120), PRIV, SELF]
        fewer_cov = [dataclasses_replace(PUB, replay_cov_slack=10, replay_ft_slack=120), PRIV, SELF]
        fewer_ft = [dataclasses_replace(PUB, replay_cov_slack=30), PRIV, SELF]
        for tag, final in (("fffffffffffffff3", (280, 100000)), ("fffffffffffffff4", (300, 99880))):
            self.certify_run(base=(300, 100000), final=final, done=(300, 100000), tag=tag, certificate=replay)
            self.assertEqual(self.certs()["kv_model@v24.0"][0], "failed", msg=tag)
            self.assertEqual(self.certs(ruled)["kv_model@v24.0"][0], "certified", msg=tag)
        self.assertEqual(self.certs(fewer_ft)["kv_model@v24.0"][0], "failed")
        self.certify_run(base=(300, 100000), final=(280, 100000), done=(300, 100000), tag="fffffffffffffff5",
                         certificate=replay)
        self.assertEqual(self.certs(fewer_cov)["kv_model@v24.0"][0], "failed")
        for tag, certificate in (("fffffffffffffff6", True), ("fffffffffffffff7", fuzzctl.Replay(10, 120)),
                                 ("fffffffffffffff8", fuzzctl.Plateau(6, False))):
            self.certify_run(base=(300, 100000), final=(280, 100000), done=(300, 100000), tag=tag,
                             certificate=certificate)
            self.assertEqual(self.certs(ruled)["kv_model@v24.0"][0], "failed", msg=tag)

    def test_a_missing_or_torn_verdict_is_no_certificate(self):
        # Sol r1 B4: fail closed when run.sh's verdict is missing or torn.
        run = self.certify_run()
        os.remove(os.path.join(run, "verdict.json"))
        self.assertEqual(fuzzctl.read_certificate(run)[0], "no-evidence")
        open(os.path.join(run, "verdict.json"), "w").write('{"mode": "certify", "status": "pa')
        self.assertEqual(fuzzctl.read_certificate(run)[0], "no-evidence")
        self.assertEqual(self.judged(tag="00000000000000c2", verdict={"crashes": None}), "no-evidence")
        self.assertEqual(self.judged(tag="00000000000000c3", verdict={"crashes": True}), "no-evidence")
        self.assertEqual(self.judged(tag="00000000000000c4", verdict={"mode": "run"}), "no-evidence")

    def test_a_saved_crash_or_logged_failure_is_a_finding_whatever_the_verdict_says(self):
        run = self.certify_run()
        open(os.path.join(run, "crashes", "crash-aaa"), "wb").write(b"boom")
        self.assertEqual(fuzzctl.read_certificate(run)[0], "finding")
        for seg in (fuzzctl.REPLAY_BEFORE, fuzzctl.WINDOW, fuzzctl.REPLAY_AFTER):
            run = self.certify_run()
            open(os.path.join(run, "logs", seg, "fuzz.out"), "a").write("kv_model: sink write failed\n")
            self.assertEqual(fuzzctl.read_certificate(run)[0], "finding", seg)
        run = self.certify_run(tag="00000000000000c3")
        open(os.path.join(run, "crashes", "slow-unit-0123"), "wb").write(b"slow")  # BL-83
        self.assertEqual(fuzzctl.read_certificate(run)[0], "certified")

    def test_only_a_clean_complete_single_process_window_between_replays_counts(self):
        for kw in (dict(workers=2), dict(verdict={"certify": {"engine": "fork", "jobs": 1, "seconds": 3600,
                                                               "wall_cap": 43200, "merge_status": 0,
                                                               "replay_before_status": 0, "replay_after_status": 0}}),
                   dict(verdict={"certify": {"engine": "single", "jobs": 1, "seconds": 3600, "wall_cap": 43200,
                                             "merge_status": 1, "replay_before_status": 0,
                                             "replay_after_status": 0}}),
                   dict(verdict={"status": "fail"}),
                   # DEC-186 amendment 3 (a): a CPU window ends at the stop file, with exit 0 and no
                   # kill; SIGINT's exit, or a process killed after outliving its stop, never certifies.
                   dict(verdict={"libfuzzer_status": 72}, win={"status": 72}),
                   dict(verdict={"libfuzzer_status": 137}, win={"status": -9, "killed": True}),
                   dict(win={"killed": True}),
                   dict(verdict={"refused": 1}), dict(stopped_by=None, win={"cpu_at_stop": None}),
                   dict(execs=None), dict(base=(0, 0), final=(0, 0)),
                   # DEC-186 amendment 2: a missing or failed replay never certifies.
                   dict(replays=(1, 0)), dict(replays=(0, 1)), dict(replays=(0, None)), dict(replays=(None, 0)),
                   dict(base=None), dict(final=None),
                   # The replay after the window is no lower than the one before it.
                   dict(final=(299, 1000)), dict(final=(300, 998))):
            with self.subTest(**{k: str(v)[:40] for k, v in kw.items()}):
                self.assertEqual(self.judged(**kw), "failed")
        # Loads of one corpus vary by a few features (real smokes: a window's INITED ft 3736
        # after a replay of 3738, then cov 1122 after 1123): the window's INITED is no test, and
        # the replay after it may be lower by noise within 0.1 % of ft.
        for kw in (dict(inited=(299, 997)), dict(inited=(301, 1003)), dict(final=(300, 999))):
            with self.subTest(**{k: str(v) for k, v in kw.items()}):
                self.assertEqual(self.judged(**kw), "certified")
        # A replay log with two INITED lines is no measurement.
        run = self.certify_run()
        log = os.path.join(run, "logs", fuzzctl.REPLAY_AFTER, "fuzz.out")
        open(log, "a").write(cov_line(9, 300, 1000, "INITED"))
        self.assertEqual(fuzzctl.read_certificate(run)[0], "failed")

    def test_an_exit_later_than_the_grace_after_the_stop_fails_whatever_its_status(self):
        # Sol r4 B1: window.json's stop_to_exit (monotonic, from the stop to the exit). An exit 0
        # at the stop file more than 1500 s after the stop, which the driver's kill missed, fails;
        # so does an overdue SIGINT exit at the wall cap, which would otherwise be starved.
        self.assertEqual(self.judged(win={"stop_to_exit": float(fuzzctl.STOP_GRACE)}), "certified")
        for kw in (dict(win={"stop_to_exit": fuzzctl.STOP_GRACE + 0.001}), dict(win={"stop_to_exit": 60000.0}),
                   dict(stopped_by="wall-cap", wall_s=43190.0, cpu=(40.0, 2000.0), win={"stop_to_exit": 1600.0})):
            with self.subTest(**{k: str(v) for k, v in kw.items()}):
                status, reasons, _ = fuzzctl.read_certificate(self.certify_run(**kw))
                self.assertEqual(status, "failed")
                self.assertTrue(any("after its stop, more than 1500 s" in r for r in reasons), reasons)
        # The status reader agrees, on the leg's latest run with its certificate.
        self.certify_run(tag="ffffffffffffffff", certificate=True, win={"stop_to_exit": 1500.5})
        self.assertEqual(self.certs()["kv_model@v24.0"][0], "failed")

    def test_impossible_or_contradictory_evidence_is_invalid(self):
        self.assertEqual(self.judged(cpu=(0.0, 2 * 3600.0)), "invalid")  # more CPU than wall time
        self.assertEqual(self.judged(cpu=(40.0, 2000.0)), "invalid")  # stopped for a window it did not use
        self.assertEqual(self.judged(cpu=(40.0, 3639.5)), "invalid")  # nor all of it
        self.assertEqual(self.judged(cpu=(40.0, 3640.0)), "certified")  # the stop file at the window's end
        # Sol r2 M3: the window's facts against the verdict's and against what stopped it.
        for win in ({"status": 1}, {"cpu_window": 60}, {"wall_cap": 3600}, {"cpu_at_stop": 41.0},
                    {"cpu_at_stop": None}, {"wall_seconds": None}, {"elapsed": 43200.0},
                    {"stopped_by": "elsewhere"}, {"cpu_at_stop": float("nan")}, {"status": None},
                    {"extra": 1}, {"run": "20261011T000000Z-v24.0-7-0000000000000000"}, {"target": "other"},
                    {"pin": "v25.0"}, {"stop_to_exit": None}, {"stop_to_exit": -1.0}, {"stop_to_exit": "1"},
                    {"stop_to_exit": float("inf")}, {"stop_to_exit": float("nan")}, {"stop_to_exit": True}):
            with self.subTest(**{k: str(v) for k, v in win.items()}):
                self.assertEqual(self.judged(win=win), "invalid")
        self.assertEqual(self.judged(stopped_by="wall-cap", wall_s=900.0, cpu=(40.0, 900.0)), "invalid")
        self.assertEqual(self.judged(stopped_by=None, win={"cpu_at_stop": None, "stop_to_exit": 1.0}), "invalid")
        run = self.certify_run()
        json.dump([], open(os.path.join(run, "logs", fuzzctl.WINDOW, "window.json"), "w"))
        self.assertEqual(fuzzctl.read_certificate(run)[0], "invalid")
        # The verdict and the source record are this run's, at the verdict's commit.
        self.assertEqual(self.judged(verdict={"run": "20261011T000000Z-v24.0-7-0000000000000000"}), "invalid")
        self.assertEqual(self.judged(verdict={"target": "diff_tx_decode"}), "invalid")
        self.assertEqual(self.judged(source=("fedcba987654", "clean", "x", self.BIN)), "invalid")
        run = self.certify_run()
        open(os.path.join(run, "source"), "w").write(f"{self.REV}\nclean\n20261011T000000Z-v24.0-7-0\n{self.BIN}\n")
        self.assertEqual(fuzzctl.read_certificate(run)[0], "invalid")
        self.assertEqual(self.judged(source=("fedcba987654", "clean")), "invalid")
        name = "20261011T000000Z-v24.0-7-00000000000000d1"
        for source, dirty in (((self.REV, "clean", name), False),  # no binary
                              ((self.REV, "clean", name, "xyz"), False), ((self.REV, "clean", name, self.BIN), True),
                              ((self.REV, "dirty", name, self.BIN), False), ((self.REV, "clean", name, self.BIN), None)):
            with self.subTest(source=source, dirty=dirty):  # the record and the verdict disagree
                shutil.rmtree(os.path.join(self.fdir, "kv_model", "runs", name), ignore_errors=True)
                self.assertEqual(self.judged(tag=name[-16:], source=source, verdict={"rust_dashcore_dirty": dirty}),
                                 "invalid")

    def test_a_run_certifies_only_its_own_leg(self):
        # Sol r2 M3's foreign-leg fixture: a qualifying run and its matching certificate, copied
        # under another target, certify nothing there.
        run = self.certify_run()
        ev = fuzzctl.certify_evidence(run)
        fuzzctl.write_private(os.path.join(run, fuzzctl.CERTIFICATE),
                              json.dumps({"evidence": ev, "cutoff": "20261011T000001Z", "eligibility": []}) + "\n")
        self.assertEqual(fuzzctl.read_certificate(run)[0], "certified")
        other = os.path.join(self.fdir, "diff_tx_decode", "runs", os.path.basename(run))
        shutil.copytree(run, other)
        self.assertEqual(fuzzctl.read_certificate(other)[0], "invalid")
        self.assertEqual(self.certs()["diff_tx_decode@v24.0"][0], "invalid")
        moved = os.path.join(self.fdir, "kv_model", "runs", "20261011T000000Z-v24.0-7-00000000000000c9")
        shutil.copytree(run, moved)
        self.assertEqual(fuzzctl.read_certificate(moved)[0], "invalid")  # another run's evidence

    def test_a_certificate_is_never_trusted_and_an_unreadable_one_is_invalid(self):
        run = self.certify_run()
        ev = fuzzctl.certify_evidence(run)
        path = os.path.join(run, fuzzctl.CERTIFICATE)
        cert = lambda evidence=ev, cutoff="20261011T000001Z", eligibility=(), **extra: json.dumps(
            {"evidence": evidence, "cutoff": cutoff, "eligibility": list(eligibility), **extra})
        open(path, "w").write(cert())
        self.assertEqual(fuzzctl.read_certificate(run)[0], "certified")
        self.assertEqual(fuzzctl.certificate_record(run)["cutoff"], "20261011T000001Z")
        open(path, "w").write(cert(plateau={"cov_slack": 6, "ft": False}))
        self.assertEqual(fuzzctl.read_certificate(run)[0], "certified")
        open(path, "w").write(cert(plateau={"cov_slack": 6, "ft": False}, replay={"cov_slack": 30, "ft_slack": 120}))
        self.assertEqual(fuzzctl.read_certificate(run)[0], "certified")
        for stored in (cert({**ev, "final": [300, 1000], "cpu_seconds": 9999}), '{"baseline":', "null",
                       cert({**ev, "window": {**ev["window"], "status": 1}}), json.dumps(ev),  # bare evidence
                       cert(cutoff="20261010T235959Z"), cert(cutoff="2026-10-11"), cert(cutoff=None),
                       cert(by="certify"), json.dumps({"evidence": ev, "cutoff": "20261011T000001Z"}),
                       cert(eligibility=[1]), cert().replace('"eligibility": []', '"eligibility": "none"'),
                       cert(plateau=None), cert(plateau={"cov_slack": 6}), cert(plateau={"cov_slack": 65, "ft": False}),
                       cert(plateau={"cov_slack": True, "ft": False}), cert(plateau={"cov_slack": 6, "ft": 0}),
                       cert(replay=None), cert(replay={"cov_slack": 30}), cert(replay={"cov_slack": 65, "ft_slack": 0}),
                       cert(replay={"cov_slack": 30, "ft_slack": 1001}),
                       cert(replay={"cov_slack": 30, "ft_slack": 0.5}),
                       cert(replay={"cov_slack": 30, "ft_slack": 0, "ft": False})):
            with self.subTest(stored=stored[:30]):
                open(path, "w").write(stored)
                self.assertEqual(fuzzctl.read_certificate(run)[:2], (
                    "invalid", ["certificate.json is unreadable or does not match the run's evidence"]))
        # A stored certificate is never trusted: a run that did not certify stays uncertified.
        run = self.certify_run(tag="00000000000000c4", window=900, wall_s=900.2, cpu=(1.0, 902.0))
        json.dump({"evidence": {"certified": True}, "cutoff": "20261011T000001Z", "eligibility": []},
                  open(os.path.join(run, fuzzctl.CERTIFICATE), "w"))
        self.assertEqual(fuzzctl.read_certificate(run)[0], "invalid")
        os.remove(os.path.join(run, fuzzctl.CERTIFICATE))
        self.assertEqual(fuzzctl.read_certificate(run)[0], "uncertified")

    def test_a_short_window_a_dirty_tree_or_an_unknown_commit_does_not_certify(self):
        # Sol r1's certificate fixture: a 15-minute run never certifies, flat or not.
        self.assertEqual(self.judged(window=900, wall_s=900.2, cpu=(1.0, 902.0)), "uncertified")
        self.assertEqual(self.judged(window=900, wall_s=900.2, cpu=(1.0, 902.0), final=(307, 1000)), "growing")
        self.assertEqual(self.judged(verdict={"rust_dashcore_dirty": True}), "uncertified")
        self.assertEqual(self.judged(verdict={"rust_dashcore_dirty": None}), "uncertified")  # no source record
        self.assertEqual(self.judged(verdict={"oracle_flavor": "plain"}), "uncertified")  # no Core coverage
        self.assertEqual(self.judged(verdict={"oracle_flavor": None}), "uncertified")
        # Sol r2 M3: no known commit (`certified@-`), with or without a source record.
        run = self.certify_run(source=False, verdict={"rust_dashcore": "-", "rust_dashcore_dirty": None})
        self.assertEqual(fuzzctl.read_certificate(run)[0], "uncertified")
        run = self.certify_run(tag="00000000000000c5", verdict={"rust_dashcore": "-"},
                               source=("-", "clean", "20261011T000000Z-v24.0-7-00000000000000c5", self.BIN))
        self.assertEqual(fuzzctl.read_certificate(run)[0], "uncertified")
        # No source record at all, whatever the verdict says.
        self.assertEqual(self.judged(tag="00000000000000c6", source=False), "uncertified")

    def test_certify_runs_one_process_and_writes_the_derived_certificate(self):
        calls = []

        def runner(target, pin, args, on_start=None, timeout=None, cancel=None):
            calls.append(args)
            tag = args[args.index("--run-tag") + 1]
            run = self.certify_run(target=target, pin=pin, tag=tag, window=int(args[args.index("--seconds") + 1]),
                                   wall_s=2400.5, cpu=(1.0, 602.5))
            return Leg(target, pin, "node-fuzz fake", 1, run, {})

        out = io.StringIO()
        with contextlib.redirect_stdout(out), mock.patch.object(fuzzctl.os, "nice"):
            rc = fuzzctl.certify(certify_args("kv_model@v24.0", seconds=600), TARGETS, runner=runner)
        self.assertEqual(rc, 1)  # a 10-minute window certifies nothing
        self.assertEqual(calls[0][:5], ["--certify", "--seconds", "600", "--wall-cap", "43200"])
        self.assertNotIn("--jobs", calls[0])
        self.assertIn("node-fuzz certify leg=kv_model@v24.0 status=uncertified cov=300->300 ft=1000->1000 "
                      "new_lines=0 cpu_seconds=601.5 wall_seconds=2400.5 executions=2e+06", out.getvalue())
        run = fuzzctl.tagged_run(self.fdir, "kv_model", calls[0][-1])
        cert = json.load(open(os.path.join(run, fuzzctl.CERTIFICATE)))
        self.assertEqual((set(cert), cert["eligibility"]),
                         ({"evidence", "cutoff", "eligibility", "plateau", "replay"}, []))
        self.assertEqual((cert["plateau"], cert["replay"]),
                         ({"cov_slack": 0, "ft": True}, {"cov_slack": 0, "ft_slack": 0}))
        self.assertEqual(cert["evidence"], json.loads(json.dumps(fuzzctl.certify_evidence(run))))
        self.assertRegex(cert["cutoff"], fuzzctl.RUN_STAMP)
        self.assertGreaterEqual(cert["cutoff"], os.path.basename(run).split("-")[0])
        self.assertNotIn("certified", cert["evidence"])
        self.assertEqual(stat.S_IMODE(os.stat(os.path.join(run, fuzzctl.CERTIFICATE)).st_mode), 0o600)
        # A private-route leg: its status only.
        out = io.StringIO()
        with contextlib.redirect_stdout(out), mock.patch.object(fuzzctl.os, "nice"):
            fuzzctl.certify(certify_args("diff_tx_decode@v24.0", seconds=600), TARGETS, runner=runner)
        self.assertEqual(out.getvalue(), "node-fuzz certify: a private-route leg: status=uncertified\n")
        # It judges by the target's rules and records them.
        ruled = [dataclasses_replace(PUB, plateau_cov_slack=6, plateau_ft=False, replay_cov_slack=30,
                                     replay_ft_slack=120), PRIV, SELF]
        with contextlib.redirect_stdout(io.StringIO()), mock.patch.object(fuzzctl.os, "nice"), \
                mock.patch.object(fuzzctl, "judge", wraps=fuzzctl.judge) as judge:
            fuzzctl.certify(certify_args("kv_model@v24.0", seconds=600), ruled, runner=runner)
        self.assertEqual(judge.call_args.args[3:], (fuzzctl.Plateau(6, False), fuzzctl.Replay(30, 120)))
        run = fuzzctl.tagged_run(self.fdir, "kv_model", calls[-1][-1])
        cert = json.load(open(os.path.join(run, fuzzctl.CERTIFICATE)))
        self.assertEqual((cert["plateau"], cert["replay"]), ({"cov_slack": 6, "ft": False},
                                                             {"cov_slack": 30, "ft_slack": 120}))

    def fixed_cluster(self, problem):
        """A fixed cluster of the leg whose regression vector is missing, or whose provenance is
        torn (Sol r3 B3's fixtures)."""
        c = {"kind": "crash", "class": "fixture regression", "pins": ["v24.0"], "hits": 1, "first_seen": "fixture",
             "state": "fixed", "minimized": True, "attempts": 1, "fixed_stamp": "20261009T000000Z",
             "fixed_at": "0123456789ab"}
        cid = fuzzctl.cluster_id(PUB.name, c["kind"], c["class"])
        c["vector"] = cid + "-1"
        regress = os.path.join(self.fdir, PUB.name, "regress")
        if problem == "torn-provenance":
            for sub in ("fixed", "meta"):
                os.makedirs(os.path.join(regress, sub), exist_ok=True)
            open(os.path.join(regress, "fixed", c["vector"]), "wb").write(b"fixture")
            open(os.path.join(regress, "meta", c["vector"] + ".json"), "w").write("{torn")
        os.makedirs(os.path.join(self.fdir, PUB.name), exist_ok=True)
        state = fuzzctl.load_triage(self.fdir, PUB.name)
        state["clusters"][cid] = c
        fuzzctl.write_private(fuzzctl.triage_path(self.fdir, PUB.name), json.dumps(state))

    def test_a_finding_opened_during_the_window_blocks_success(self):
        # Sol r2 B2 and r3 B3/M1: the farm opens a finding on the leg, or the leg's triage state
        # goes bad, while certify's window runs. The command's exit and status and the status
        # reader's, right after it, agree (DEC-186 amendment 3 (d)).
        def runner(target, pin, args, on_start=None, timeout=None, cancel=None):
            if self.finding in ("open", "untriaged"):
                self.make_run(PUB.name, "k1", crashes=[("crash-aaa", b"kv-crash")], log=PANIC_LOG)
                if self.finding == "open":
                    fuzzctl.run_triage(TARGETS, minimize=False)
            elif self.finding == "pending":  # a farm run with no verdict yet: its findings are unknown
                os.makedirs(os.path.join(self.fdir, PUB.name, "runs", "20200101T000001Z-v24.0-8", "logs"))
            elif self.finding == "unpublished":  # Sol r3: a job killed after its mkdir, during the window
                os.makedirs(os.path.join(self.fdir, PUB.name, "runs", fuzzctl.run_stamp() + "-v24.0-999"))
            elif self.finding == "later":  # a farm slice that started once run.sh released the leg
                os.makedirs(os.path.join(self.fdir, PUB.name, "runs", "29991231T000000Z-v24.0-9", "logs"))
            elif self.finding == "unreadable":
                os.makedirs(os.path.join(self.fdir, PUB.name), exist_ok=True)
                open(os.path.join(self.fdir, PUB.name, "triage.json"), "w").write("{broken")
            elif self.finding in ("missing-vector", "torn-provenance"):
                self.fixed_cluster(self.finding)
            run = self.certify_run(target=target, pin=pin, tag=args[args.index("--run-tag") + 1])
            return Leg(target, pin, "node-fuzz fake", 1, run, {})

        for self.finding, rc, status in ((None, 0, "certified"), ("open", 1, "finding"), ("untriaged", 1, "finding"),
                                         ("pending", 1, "finding"), ("unpublished", 1, "finding"),
                                         ("later", 0, "certified"), ("unreadable", 1, "finding"),
                                         ("missing-vector", 1, "finding"), ("torn-provenance", 1, "finding")):
            with self.subTest(finding=self.finding):
                shutil.rmtree(os.path.join(self.fdir, PUB.name), ignore_errors=True)
                out = io.StringIO()
                with contextlib.redirect_stdout(out), mock.patch.object(fuzzctl.os, "nice"):
                    self.assertEqual(fuzzctl.certify(certify_args("kv_model@v24.0"), TARGETS, runner=runner), rc)
                self.assertIn(f"status={status} ", out.getvalue())
                self.assertEqual(fuzzctl.certificates(self.fdir, TARGETS)["kv_model@v24.0"][0], status)

    def test_the_status_never_upgrades_a_refused_certify_or_a_run_without_a_certificate(self):
        # Sol r3 M1, later: a job's directory, killed after its mkdir during the window, blocks
        # certify. When the farm's triage later treats it as a clean run (an hour on), the status
        # still reports what the command did: the certificate records its reasons.
        def runner(target, pin, args, on_start=None, timeout=None, cancel=None):
            os.makedirs(os.path.join(self.fdir, PUB.name, "runs", fuzzctl.run_stamp() + "-v24.0-999"))
            run = self.certify_run(target=target, pin=pin, tag=args[args.index("--run-tag") + 1])
            return Leg(target, pin, "node-fuzz fake", 1, run, {})

        with contextlib.redirect_stdout(io.StringIO()), mock.patch.object(fuzzctl.os, "nice"):
            self.assertEqual(fuzzctl.certify(certify_args("kv_model@v24.0"), TARGETS, runner=runner), 1)
        state = fuzzctl.load_triage(self.fdir, PUB.name)
        state["runs"] = os.listdir(os.path.join(self.fdir, PUB.name, "runs"))  # all triaged, clean
        fuzzctl.write_private(fuzzctl.triage_path(self.fdir, PUB.name), json.dumps(state))
        self.assertEqual(fuzzctl.eligibility(self.fdir, PUB.name, "v24.0"), [])
        self.assertEqual(fuzzctl.certificates(self.fdir, TARGETS)["kv_model@v24.0"][:2],
                         ("finding", ["a run not triaged yet"]))
        # A certify run killed before it wrote its certificate never reads certified.
        shutil.rmtree(os.path.join(self.fdir, PUB.name))
        self.certify_run()
        self.assertEqual(fuzzctl.read_certificate(self.certify_run(tag="00000000000000c7"))[0], "certified")
        self.assertEqual(self.certs()["kv_model@v24.0"][:2],
                         ("uncertified", ["no certificate.json: certify did not complete"]))

    def test_triage_errors_before_the_window_refuse_certify_and_the_status_agrees(self):
        # Sol r3 B3's fixtures: a fixed cluster's vector is missing, or its provenance is torn.
        # The triage pass reports them; certify refuses to start, and a leg certified before
        # stops reading certified.
        for problem in ("missing-vector", "torn-provenance"):
            with self.subTest(problem=problem):
                shutil.rmtree(os.path.join(self.fdir, PUB.name), ignore_errors=True)
                self.certify_run(certificate=True)
                self.assertEqual(self.certs()["kv_model@v24.0"][0], "certified")
                self.fixed_cluster(problem)
                self.assertEqual(fuzzctl.run_triage(TARGETS, only=[PUB.name], minimize=False).stats["errors"], 1)
                self.assertEqual(fuzzctl.eligibility(self.fdir, PUB.name, "v24.0"),
                                 ["a regression vector or its provenance is missing or unreadable"])
                with mock.patch.object(fuzzctl.os, "nice"), self.assertRaises(SystemExit):
                    fuzzctl.certify(certify_args("kv_model@v24.0"), TARGETS, runner=None)
                self.assertEqual(fuzzctl.certificates(self.fdir, TARGETS)["kv_model@v24.0"][:2],
                                 ("finding", ["a regression vector or its provenance is missing or unreadable"]))

    def test_the_engine_is_one_process_while_the_harness_refuses_fork_mode(self):
        real = open(fuzzctl.SINK_SOURCE).read()
        self.assertEqual(fuzzctl.certify_engine(real), ("single", 1))
        flags = ", ".join(f'"{x}"' for x in ("runs", "max_total_time") + fuzzctl.FORK_FLAGS)
        forked = f"const ALLOWED_FLAGS: &[&str] = &[{flags}];"
        self.assertEqual(fuzzctl.certify_engine(forked), ("fork", None))
        self.assertEqual(fuzzctl.certify_engine(""), ("single", 1))
        # certify refuses a fork engine until one is built and verified.
        with mock.patch.object(fuzzctl, "certify_engine", return_value=("fork", None)):
            with self.assertRaises(SystemExit):
                fuzzctl.certify(certify_args("kv_model@v24.0"), TARGETS, runner=None)
        for leg in ("kv_model@v99.0", "ghost@v24.0", "selftest_mismatch@v24.0", "kv_model"):
            with self.assertRaises(SystemExit, msg=leg):
                fuzzctl.certify(certify_args(leg), TARGETS, runner=None)

    def test_launch_runs_at_the_farms_priority(self):
        rc, out = self.cli(["certify", "kv_model@v24.0", "--launch", "--print-only", "--seconds", "900"])
        self.assertEqual(rc, 0)
        for prop in ("--unit=node-fuzz-certify-kv_model-v24.0", "-p Nice=19", "-p IOSchedulingClass=idle",
                     "-p CPUWeight=10", "-p CPUQuota=100%", "-p KillMode=mixed"):
            self.assertIn(prop, out)
        self.assertIn("certify kv_model@v24.0 --seconds 900 --wall-cap-hours 12.0", out)
        self.assertIn("--description=node-fuzz certify", out)
        self.assertNotIn(self.root, out)
        # A private-route leg is in neither the unit's name nor its command line nor its description.
        rc, out = self.cli(["certify", "diff_tx_decode@v24.0", "--launch", "--print-only"])
        self.assertEqual(rc, 0)
        self.assertRegex(out, r"--unit=node-fuzz-certify-[0-9a-f]{16} ")
        self.assertIn("--setenv=NODE_FUZZ_CERTIFY_LEG=diff_tx_decode@v24.0", out)
        self.assertNotIn("diff_tx_decode", out.replace("--setenv=NODE_FUZZ_CERTIFY_LEG=diff_tx_decode@v24.0", ""))
        self.assertIn("fuzzctl.py certify - --seconds 3600", out)
        for bad in (["--seconds", "59"], ["--seconds", "7200", "--wall-cap-hours", "1"], ["--wall-cap-hours", "nan"]):
            with self.assertRaises(SystemExit, msg=bad):
                self.cli(["certify", "kv_model@v24.0", *bad])

    def test_status_reports_the_latest_certify_run_of_each_leg(self):
        self.certify_run(tag="00000000000000c1", final=(301, 1000))  # older: growing
        self.certify_run(tag="00000000000000c2", stamp="20261012T000000Z", certificate=True)
        self.certify_run(target="diff_tx_decode", tag="00000000000000c3", certificate=True)
        self.certify_run(target="fixture_gone", tag="00000000000000c4", final=(305, 1000))
        self.make_run(PUB.name, "20261013T000000Z-v24.0-1")  # a farm run is not a certify run
        # A removed target's runs, triaged while it was registered.
        fuzzctl.write_private(fuzzctl.triage_path(self.fdir, "fixture_gone"), json.dumps(
            {"clusters": {}, "runs": os.listdir(os.path.join(self.fdir, "fixture_gone", "runs")), "run_errors": {}}))
        certs = self.certs()
        self.assertEqual({leg: c[0] for leg, c in certs.items()},
                         {"kv_model@v24.0": "certified", "diff_tx_decode@v24.0": "certified",
                          "fixture_gone@v24.0": "growing"})
        line = fuzzctl.certify_line(self.fdir, TARGETS)
        self.assertEqual(line, "SW-FUZZ certify (DEC-186): 2/3 legs certified: kv_model@v24.0=certified@0123456789ab; "
                               "2 other legs, 1 certified")
        rc, out = self.cli(["status-line"])
        self.assertIn(line, out)
        self.assertNotIn("diff_tx_decode", out)
        self.assertNotIn("fixture_gone", out)
        # A certify run with a finding is triaged as a fuzzing run.
        run = self.certify_run(tag="00000000000000c5")
        self.assertFalse(fuzzctl.is_job_run(run))

    def test_the_latest_run_is_the_latest_started_not_the_greatest_name(self):
        # Two runs of one second: the pid and tag after the time in the id are no order; the
        # mode file each run writes first is.
        for tag, final, t in (("ff00000000000000", (301, 1000), 1000), ("0000000000000000", (300, 1000), 2000)):
            run = self.certify_run(tag=tag, final=final, stamp="20261012T000000Z", certificate=True)
            os.utime(os.path.join(run, "mode"), (t, t))
        self.assertEqual(self.certs()["kv_model@v24.0"][0], "certified")
        os.utime(os.path.join(run, "mode"), (1, 1))
        self.assertEqual(fuzzctl.certificates(self.fdir, TARGETS)["kv_model@v24.0"][0], "growing")

    def test_an_open_finding_outranks_a_certificate_and_blocks_certify(self):
        self.certify_run(certificate=True)
        self.assertEqual(self.certs()["kv_model@v24.0"][0], "certified")
        self.make_run(PUB.name, "k1", crashes=[("crash-aaa", b"kv-crash")], log=PANIC_LOG)
        fuzzctl.run_triage(TARGETS, minimize=False)
        self.assertEqual(fuzzctl.certificates(self.fdir, TARGETS)["kv_model@v24.0"][:2],
                         ("finding", ["an open finding"]))
        with self.assertRaises(SystemExit):
            fuzzctl.certify(certify_args("kv_model@v24.0"), TARGETS, runner=None)
        open(os.path.join(self.fdir, PUB.name, "triage.json"), "w").write("{broken")
        self.assertEqual(fuzzctl.certificates(self.fdir, TARGETS)["kv_model@v24.0"][0], "finding")

    def test_certify_triages_the_leg_before_it_runs(self):
        # A farm run's crash that triage has not seen yet: certify triages first, and refuses.
        self.make_run(PUB.name, "k1", crashes=[("crash-aaa", b"kv-crash")], log=PANIC_LOG)
        self.assertEqual(fuzzctl.eligibility(self.fdir, PUB.name, "v24.0"), ["a run not triaged yet"])
        with mock.patch.object(fuzzctl.os, "nice"), self.assertRaises(SystemExit):
            fuzzctl.certify(certify_args("kv_model@v24.0"), TARGETS, runner=None)
        self.assertEqual(fuzzctl.eligibility(self.fdir, PUB.name, "v24.0"), ["an open finding"])
        # A run whose triage failed blocks it too, of whatever pin (every triage error counts).
        shutil.rmtree(os.path.join(self.fdir, PUB.name))
        self.make_run(PUB.name, "k2")
        fuzzctl.write_private(fuzzctl.triage_path(self.fdir, PUB.name), json.dumps(
            {"clusters": {}, "runs": [], "run_errors": {"k2": 1}}))
        self.assertEqual(fuzzctl.eligibility(self.fdir, PUB.name, "v24.0"), ["a run whose triage failed"])
        fuzzctl.write_private(fuzzctl.triage_path(self.fdir, PUB.name), json.dumps(
            {"clusters": {}, "runs": ["k2"], "run_errors": {"20261011T000000Z-v23.1-5": 3}}))
        self.assertEqual(fuzzctl.eligibility(self.fdir, PUB.name, "v24.0"), ["a run whose triage failed"])
        # The cutoff: a run that started after it does not count; one with no stamp always does.
        self.assertEqual(fuzzctl.eligibility(self.fdir, PUB.name, "v24.0", "20261010T000000Z"), [])
        os.makedirs(os.path.join(self.fdir, PUB.name, "runs", "pending"))
        self.assertEqual(fuzzctl.eligibility(self.fdir, PUB.name, "v24.0", "20261010T000000Z"),
                         ["a run not triaged yet"])
        open(fuzzctl.triage_path(self.fdir, PUB.name), "w").write("{torn")
        self.assertEqual(fuzzctl.eligibility(self.fdir, PUB.name, "v24.0"), ["the triage state is unreadable"])


def secrets_hex():
    return os.urandom(8).hex()


def driver_script(root, patch=()):
    """The CPU-time driver run.sh starts (window_driver.py) as a script in `root`, with `patch`'s
    (old, new) replacements."""
    text = open(os.path.join(fuzzctl.HERE, "window_driver.py")).read()
    for old, new in patch:
        assert text.count(old) == 1, old
        text = text.replace(old, new)
    script = os.path.join(root, "driver.py")
    open(script, "w").write(text)
    return script


class RunShCertifyWindow(Tree):
    """run.sh's certify driver on stand-ins for libFuzzer: a window of the process's own CPU time
    from INITED, ended through its stop file, or the wall-clock cap's SIGINT."""

    # libFuzzer's SIGINT path (InterruptCallback): no DONE line, no cov or ft (Sol r2 B1).
    INTERRUPTED = ("def stop(*_):\n    print('==1== libFuzzer: run interrupted; exiting', flush=True)\n"
                   "    print('stat::number_of_executed_units: 99', flush=True)\n    sys.exit(72)\n"
                   "signal.signal(signal.SIGINT, stop)\n")
    # libFuzzer's stop file, read at the top of its fuzzing loop: the DONE line, its end, exit 0.
    STOP_FILE = ("STOP = next(a.split('=', 1)[1] for a in sys.argv if a.startswith('-stop_file='))\n"
                 "def stopped():\n    if os.path.exists(STOP):\n"
                 "        print('#99\\tDONE   cov: 5 ft: 9 corp: 1/1b exec/s: 1 rss: 1Mb', flush=True)\n"
                 "        print('Done 99 runs in 1 second(s)', flush=True)\n        sys.exit(0)\n")

    def drive(self, window, cap, body, rc=0, grace="300", patch=()):
        """Runs the driver (with `patch` applied) on a fake libFuzzer, Python `body` after the imports."""
        r, seg = self.run_driver(window, cap, body, patch, NODE_FUZZ_CERTIFY_POLL="0.2", NODE_FUZZ_CERTIFY_GRACE=grace)
        self.assertEqual(r.returncode, rc, r.stderr)
        self.log = open(os.path.join(seg, "fuzz.out")).read()
        w = json.load(open(os.path.join(seg, "window.json")))
        self.assertEqual(set(w), set(fuzzctl.WINDOW_KEYS))
        self.assertTrue(all(ok(w[k]) for k, ok in fuzzctl.WINDOW_KEYS.items()), w)
        self.assertEqual((w["target"], w["pin"], w["run"]), ("kv_model", "v24.0", "run"))
        return w

    def run_driver(self, window, cap, body, patch=(), **env):
        fake = os.path.join(self.root, "fake-fuzzer")
        open(fake, "w").write("#!/usr/bin/env python3\nimport os, signal, subprocess, sys, time\n" + body)
        os.chmod(fake, 0o700)
        seg = os.path.join(self.root, "seg")
        os.mkdir(seg)
        r = subprocess.run([sys.executable, "-I", driver_script(self.root, patch), seg, str(window), str(cap), self.root,
                            "kv_model", "v24.0", "run", fake, f"-stop_file={seg}/stop", "corpus"],
                           capture_output=True, text=True, timeout=300, env={**os.environ, **env})
        return r, seg

    def busy(self, duty, stop_file=True):
        """A fake that loads its corpus, prints INITED and burns CPU `duty` of its wall time,
        checking its stop file at each turn of its loop (unless `stop_file` is False)."""
        return (self.INTERRUPTED + self.STOP_FILE + "print('#1\\tREAD   cov: 1 ft: 1', flush=True)\n"
                "print('INFO: a diagnostic that mentions INITED', flush=True)\ntime.sleep(1.5)\n"
                "print('#2\\tINITED cov: 5 ft: 9', flush=True)\n"
                f"while True:\n    {'stopped()' if stop_file else 'pass'}\n    t = time.thread_time() + 0.05 * {duty}\n"
                "    while time.thread_time() < t:\n        pass\n"
                f"    time.sleep(0.05 * (1 - {duty}))\n")

    def test_the_window_is_cpu_time_from_inited_however_diluted(self):
        w = self.drive(2, 600, self.busy(0.3))  # 30 % of a core: 2 CPU-s take about 7 s
        self.assertEqual((w["cpu_window"], w["stopped_by"], w["status"], w["killed"]), (2, "cpu-window", 0, False))
        self.assertGreaterEqual(w["cpu_at_stop"] - w["cpu_at_inited"], 2)
        self.assertGreater(w["wall_seconds"], 1.5 * (w["cpu_at_stop"] - w["cpu_at_inited"]))
        self.assertGreaterEqual(w["load_seconds"], 1.5)  # INITED is the status line, not the diagnostic
        self.assertIn("DONE", self.log)
        self.assertNotIn("run interrupted", self.log)
        self.assertTrue(os.path.isfile(os.path.join(self.root, "seg", "stop")))
        self.assertLess(w["stop_to_exit"], 30)  # the fake reads its stop file at its loop's next turn

    def test_a_window_that_outlives_its_stop_file_is_killed(self):
        # DEC-186 amendment 3 (a): no DONE line within the grace, and the driver kills it.
        w = self.drive(1, 600, self.busy(0.5, stop_file=False), rc=137, grace="1")
        self.assertEqual((w["stopped_by"], w["killed"], w["status"]), ("cpu-window", True, -9))
        self.assertGreater(w["stop_to_exit"], 1)
        self.assertNotIn("DONE", self.log)

    def test_a_fuzzer_that_exits_late_records_how_late(self):
        # Sol r4 B1: window.json's stop_to_exit runs from the stop to the exit, on the monotonic
        # clock; the elapsed and wall times end at the stop.
        late = self.busy(1.0).replace("    if os.path.exists(STOP):\n", "    if os.path.exists(STOP):\n        time.sleep(3)\n")
        w = self.drive(1, 600, late)
        self.assertEqual((w["stopped_by"], w["killed"], w["status"]), ("cpu-window", False, 0))
        self.assertGreaterEqual(w["stop_to_exit"], 3)
        self.assertLess(w["wall_seconds"], w["elapsed"])
        # No stop, no interval: the fake exits by itself.
        shutil.rmtree(os.path.join(self.root, "seg"))
        w = self.drive(600, 600, "print('#2\\tINITED cov: 5 ft: 9', flush=True)\n")
        self.assertEqual((w["stopped_by"], w["stop_to_exit"], w["status"]), (None, None, 0))

    REV, BIN, certify_run, judged = Certifying.REV, Certifying.BIN, Certifying.certify_run, Certifying.judged

    def verdict(self, *args, **kw):  # run.sh's verdict on a run (RunShVerdict's, defined below)
        return RunShVerdict.verdict(self, *args, **kw)

    def test_the_real_window_probes_patches_still_apply(self):
        # BL-252: RealWindowStop skips unless node-fuzz is built, so a driver change that drops
        # the text its patches replace passed a clean tree's suite (skipped) and failed the
        # built one. driver_script fails unless each patch matches exactly once.
        for patch in (RealWindowStop.FAST, RealWindowStop.NEVER_KILLS):
            with self.subTest(patch=patch[0]):
                driver_script(self.root, [patch])

    def test_an_exit_just_past_the_grace_is_recorded_unrounded_and_fails(self):
        # Sol r5 B1: window.json kept the interval rounded to the ms, so an exit 1500.0004 s after
        # the stop read 1500.0 and passed. The driver's clock reads whole seconds until it reaps
        # the fake, then the stop plus `delay` (exact float sums at these magnitudes); the
        # interval it records goes through run.sh's verdict and the reader unchanged.
        for delay, passes in ((1500.0, True), (1500.0004, False), (1500.001, False)):
            with self.subTest(delay=delay):
                shutil.rmtree(os.path.join(self.root, "seg"), ignore_errors=True)
                clock = ("import ctypes, json, os, re, signal, subprocess, sys, time\n",
                         "import ctypes, json, os, re, signal, subprocess, sys, time\n_monotonic = time.monotonic\n"
                         f"time.monotonic = lambda: stopped + {delay!r} if globals().get('pid') and stopped is not "
                         "None else float(int(_monotonic()))\n")
                w = self.drive(1, 600, self.busy(1.0), patch=[clock])
                self.assertEqual((w["stopped_by"], w["killed"], w["status"]), ("cpu-window", False, 0))
                self.assertAlmostEqual(w["stop_to_exit"], delay, delta=1e-9)
                self.assertEqual(w["stop_to_exit"] > fuzzctl.STOP_GRACE, not passes, w["stop_to_exit"])
                run = self.certify_run(win={"stop_to_exit": w["stop_to_exit"]})
                self.assertEqual(fuzzctl.read_certificate(run)[0], "certified" if passes else "failed")
                rc, v = self.verdict(run, 0, mode="certify", window="3600", wall_cap="43200", merge_status="0",
                                     replay_before="0", replay_after="0")
                self.assertEqual((v["status"], rc), ("pass", 0) if passes else ("fail", 1))

    def test_the_grace_and_the_poll_may_only_be_shortened(self):
        # Sol r4 B1: the 1500 s grace (and the 10 s poll) are constants. The selftest may shorten
        # them; a longer, infinite, NaN, zero, negative or unreadable value stops the driver before
        # it starts libFuzzer.
        started = "open(os.path.join(os.path.dirname(sys.argv[0]), 'started'), 'w')\n"
        for name, value in (("NODE_FUZZ_CERTIFY_GRACE", "60000"), ("NODE_FUZZ_CERTIFY_GRACE", "nan"),
                            ("NODE_FUZZ_CERTIFY_GRACE", "NaN"), ("NODE_FUZZ_CERTIFY_GRACE", "inf"),
                            ("NODE_FUZZ_CERTIFY_GRACE", "-inf"), ("NODE_FUZZ_CERTIFY_GRACE", "1500.001"),
                            ("NODE_FUZZ_CERTIFY_GRACE", "0"), ("NODE_FUZZ_CERTIFY_GRACE", "-5"),
                            ("NODE_FUZZ_CERTIFY_GRACE", ""), ("NODE_FUZZ_CERTIFY_GRACE", "1500s"),
                            ("NODE_FUZZ_CERTIFY_POLL", "11"), ("NODE_FUZZ_CERTIFY_POLL", "nan"),
                            ("NODE_FUZZ_CERTIFY_POLL", "0")):
            with self.subTest(name=name, value=value):
                shutil.rmtree(os.path.join(self.root, "seg"), ignore_errors=True)
                r, seg = self.run_driver(1, 600, started, **{name: value})
                self.assertEqual(r.returncode, 1)
                self.assertIn(f"{name} must be above 0 and at most", r.stderr)
                self.assertEqual(os.listdir(seg), [])  # no fuzz.out, no window.json
                self.assertFalse(os.path.exists(os.path.join(self.root, "started")))
        # The bound itself, and anything shorter, is accepted.
        for grace in (str(fuzzctl.STOP_GRACE), "1e3", "0.25"):
            shutil.rmtree(os.path.join(self.root, "seg"))
            w = self.drive(600, 600, "print('#2\\tINITED cov: 5 ft: 9', flush=True)\n", grace=grace)
            self.assertEqual((w["stopped_by"], w["status"]), (None, 0))

    def test_run_sh_refuses_a_longer_or_unbounded_wait_before_it_builds(self):
        # Sol r4 B1's overrides, through run.sh as `fuzzctl certify` runs it: refused at its
        # argument checks, before the registry, the private tree or the build.
        argv = ["bash", fuzzctl.RUN_SH, "coreimport_settings_json", "--pin", "v24.0", "--certify", "--seconds", "60"]
        base = {"PATH": os.environ["PATH"], "HOME": os.environ["HOME"], "TMPDIR": self.root}
        for name, value in (("NODE_FUZZ_CERTIFY_GRACE", "60000"), ("NODE_FUZZ_CERTIFY_GRACE", "nan"),
                            ("NODE_FUZZ_CERTIFY_GRACE", "inf"), ("NODE_FUZZ_CERTIFY_GRACE", "0"),
                            ("NODE_FUZZ_CERTIFY_POLL", "3600")):
            with self.subTest(name=name, value=value):
                r = subprocess.run(argv, capture_output=True, text=True, timeout=60, env={**base, name: value})
                self.assertEqual(r.returncode, 2, r.stderr)
                self.assertIn("run.sh: NODE_FUZZ_CERTIFY_GRACE must be above 0 and at most 1500 seconds", r.stderr)
        # A shorter wait passes them: run.sh goes on, to its check of the private tree.
        r = subprocess.run(argv, capture_output=True, text=True, timeout=60,
                           env={**base, "NODE_FUZZ_CERTIFY_GRACE": "1", "NODE_FUZZ_CERTIFY_POLL": "0.1"})
        self.assertEqual(r.returncode, 2)
        self.assertIn("DASH_EMBARGO_DIR is unset", r.stderr)

    def test_the_grace_is_one_constant(self):
        self.assertEqual(open(fuzzctl.RUN_SH).read().count(f"\nstop_grace={fuzzctl.STOP_GRACE}\n"), 1)
        driver = open(os.path.join(fuzzctl.HERE, "window_driver.py")).read()
        self.assertEqual(driver.count(f"\nGRACE, POLL = {fuzzctl.STOP_GRACE}, 10 "), 1)

    def test_the_wall_cap_stops_a_starved_window(self):
        w = self.drive(600, 4, self.busy(0.05), rc=72)
        self.assertEqual((w["stopped_by"], w["status"], w["killed"]), ("wall-cap", 72, False))
        self.assertGreaterEqual(w["elapsed"], 4)

    def test_reaped_childrens_cpu_is_not_the_window(self):
        # Sol r2 M1: a process that reaps a child burning 3 CPU-s, then idles, has used none of
        # its own; the wall cap stops it.
        body = (self.INTERRUPTED + "print('#2\\tINITED cov: 5 ft: 9', flush=True)\n"
                "subprocess.run([sys.executable, '-c', 'import time\\nt = time.process_time() + 3\\n"
                "while time.process_time() < t: pass'])\n"
                "while True:\n    time.sleep(1)\n")
        w = self.drive(1, 8, body, rc=72)
        self.assertEqual((w["stopped_by"], w["status"]), ("wall-cap", 72))
        self.assertLess(w["cpu_at_stop"] - w["cpu_at_inited"], 1)

    def test_a_stuck_corpus_load_is_killed_after_the_wall_cap(self):
        # Sol r2 M4: no INITED, and SIGINT ignored: SIGKILL follows the wall cap's stop.
        body = "signal.signal(signal.SIGINT, signal.SIG_IGN)\nprint('INFO: loading', flush=True)\ntime.sleep(600)\n"
        w = self.drive(600, 2, body, rc=137, grace="1")
        self.assertEqual((w["stopped_by"], w["killed"], w["status"], w["cpu_at_inited"]), ("wall-cap", True, -9, None))


class RealWindowStop(Tree):
    """Sol r3's probes, kept as regressions: callbacks linked against the locked libFuzzer archive
    (node-fuzz's build of libfuzzer-sys), run by run.sh's certify driver, and judged as certify
    judges a window. The window ends at libFuzzer's loop boundary (its stop file), so a discovery
    in flight is published before it stops (DEC-186 amendment 3 (a))."""

    REV, BIN, certify_run, judged = Certifying.REV, Certifying.BIN, Certifying.certify_run, Certifying.judged
    # The driver patches the probes apply. The class skips unless node-fuzz is built, so
    # RunShCertifyWindow checks that they still apply to window_driver.py (BL-252).
    # Only the driver's monotonic clock runs 1000 times fast.
    FAST = ("import ctypes, json, os, re, signal, subprocess, sys, time\n",
            "import ctypes, json, os, re, signal, subprocess, sys, time\n_monotonic = time.monotonic\n"
            "time.monotonic = lambda: _monotonic() * 1000\n")
    # A driver that misses the kill after the grace.
    NEVER_KILLS = ("            kill_all()\n            killed = True\n", "            pass\n")

    def setUp(self):
        super().setUp()
        archives = sorted(glob.glob(os.path.join(fuzzctl.HERE, "target", "*", "release", "build",
                                                 "libfuzzer-sys-*", "out", "libfuzzer.a")), key=os.path.getmtime)
        if not archives:
            self.skipTest("node-fuzz is not built (no locked libfuzzer.a); run.sh --selftest builds it")
        self.archive = archives[-1]

    def window(self, source, seconds, grace, rc, *defines, wall_cap=600, patch=(), wrap=True, extra=()):
        """Builds a probe, runs the driver's window on it over an empty corpus (the driver with
        `patch` applied; its own grace when `grace` is None), and returns the window log,
        window.json, its INITED (cov, ft) and the corpus's files."""
        probe = os.path.join(self.root, "probe")
        wraps = ["-Wl,--wrap=vfprintf", "-Wl,--wrap=__vfprintf_chk"] if wrap else []
        subprocess.run(["c++", "-O1", *defines, os.path.join(fuzzctl.HERE, "probes", source), self.archive,
                        *wraps, "-lpthread", "-o", probe],
                       check=True, capture_output=True, env={**os.environ, "TMPDIR": self.root})
        seg, corpus = (os.path.join(self.root, n) for n in ("seg", "corpus"))
        for d in (seg, corpus):
            shutil.rmtree(d, ignore_errors=True)
            os.mkdir(d)
        env = {k: v for k, v in os.environ.items() if k != "NODE_FUZZ_CERTIFY_GRACE"}
        env.update(NODE_FUZZ_CERTIFY_POLL="0.1", **({} if grace is None else {"NODE_FUZZ_CERTIFY_GRACE": grace}))
        r = subprocess.run([sys.executable, "-I", driver_script(self.root, patch), seg, str(seconds), str(wall_cap),
                            self.root, "kv_model", "v24.0", "run", probe, "-print_final_stats=1", *extra,
                            f"-stop_file={seg}/stop", corpus],
                           capture_output=True, text=True, timeout=600, env=env)
        self.assertEqual(r.returncode, rc, r.stderr)
        log = open(os.path.join(seg, "fuzz.out"), "rb").read()
        w = json.load(open(os.path.join(seg, "window.json")))
        inited = fuzzctl.window_log(log)["inited"] or next(
            tuple(map(int, fuzzctl.COV_STATUS.search(line).groups()))
            for line in log.splitlines() if fuzzctl.INITED_EXECS.match(line))
        return log, w, inited, os.listdir(corpus)

    def judge(self, log, w, inited):
        """certify's status for this window log, as an hour's window at the window's INITED
        coverage (flat replays: the corpus they measure is the one the window started from)."""
        win = {"status": w["status"], "killed": w["killed"], "stop_to_exit": w["stop_to_exit"]}
        return self.judged(window_log=log, base=inited, final=inited, win=win,
                           verdict={"libfuzzer_status": w["status"] if w["status"] >= 0 else 128 - w["status"]})

    def test_a_flat_window_ends_at_its_stop_file_with_a_done_line(self):
        log, w, inited, _ = self.window("window_probe.cpp", 1, "120", 0, "-DAFTER=1000000000000", "-DSPIN=0")
        self.assertEqual((w["stopped_by"], w["killed"], w["status"]), ("cpu-window", False, 0))
        parsed = fuzzctl.window_log(log)
        self.assertEqual((parsed["problems"], parsed["done"], parsed["unit_lines"]), ([], inited, 0), log[-600:])
        self.assertEqual(log.count(b"\tDONE   cov: "), 1)
        self.assertNotIn(b"run interrupted", log)
        self.assertNotIn(b"exiting as requested", log)
        self.assertIn(self.judge(log, w, inited), ("certified", "insufficient-progress"))

    def test_a_discovery_in_flight_at_the_stop_is_published_or_the_window_fails(self):
        # Sol r3 B1: the stop lands in ReportNewCoverage, before its NEW line's first write.
        # libFuzzer reads its stop file only at its loop's top, so the discovery is printed,
        # saved and counted first, then DONE shows it: growing.
        defines = ("-DAFTER=100000", "-DSPIN=2")
        log, w, inited, corpus = self.window("window_probe.cpp", 1, "300", 0, *defines)
        self.assertEqual((w["stopped_by"], w["killed"], w["status"]), ("cpu-window", False, 0))
        self.assertLess(w["cpu_at_stop"] - w["cpu_at_inited"], 2)  # the stop came during the 2 CPU-s spin
        parsed = fuzzctl.window_log(log)
        self.assertEqual((parsed["problems"], parsed["unit_lines"], parsed["units_added"]), ([], 1, 1), log[-600:])
        self.assertEqual(parsed["done"][0], inited[0] + 1)
        self.assertEqual(len(corpus), 1)  # the discovery reached the corpus
        self.assertEqual(self.judge(log, w, inited), "growing")
        # A grace too short for it: the driver kills the process, and a killed window fails.
        log, w, inited, corpus = self.window("window_probe.cpp", 1, "0.3", 137, *defines)
        self.assertEqual((w["stopped_by"], w["killed"], w["status"]), ("cpu-window", True, -9))
        self.assertNotIn(b"\tDONE ", log)
        self.assertEqual(self.judge(log, w, inited), "failed")

    def test_an_exit_later_than_the_grace_after_the_stop_fails_however_it_ended(self):
        # Sol r4 B1's grace probe: inputs slower than the polls keep libFuzzer from its stop file.
        # Only the driver's monotonic clock runs 1000 times fast, so an exit about 3 s after the
        # stop is about 3000 s on it. The driver's own (constant) grace kills the process: failed.
        fast, never = self.FAST, self.NEVER_KILLS
        kw = dict(wall_cap=10**6, wrap=False)
        log, w, inited, _ = self.window("slow_stop_probe.cpp", 1, None, 137, "-DAFTER=100000", "-DSLEEP=3",
                                        patch=[fast], **kw)
        self.assertEqual((w["stopped_by"], w["killed"], w["status"]), ("cpu-window", True, -9))
        self.assertGreater(w["stop_to_exit"], fuzzctl.STOP_GRACE)
        self.assertEqual(self.judge(log, w, inited), "failed")
        # A driver that misses the kill (here, never kills): libFuzzer exits 0 at its stop file
        # with a complete, flat log, and the recorded stop_to_exit alone fails it.
        log, w, inited, _ = self.window("slow_stop_probe.cpp", 1, None, 0, "-DAFTER=100000", "-DSLEEP=3",
                                        patch=[fast, never], **kw)
        self.assertEqual((w["stopped_by"], w["killed"], w["status"]), ("cpu-window", False, 0))
        self.assertGreater(w["stop_to_exit"], fuzzctl.STOP_GRACE)
        parsed = fuzzctl.window_log(log)
        self.assertEqual((parsed["problems"], parsed["done"], parsed["unit_lines"]), ([], inited, 0), log[-600:])
        self.assertEqual(self.judge(log, w, inited), "failed")
        self.assertIn(self.judge(log, {**w, "stop_to_exit": 2.0}, inited), ("certified", "insufficient-progress"))

    def test_a_natural_slow_unit_reads_by_the_grammar_whatever_the_sink_path(self):
        # Sol r4 m1: one ordinary 11 s input (libFuzzer's -report_slow_units is 10 s) prints the
        # slow-unit lines with the artifact prefix as it is; a `'` in the sink path is no fault.
        for name in ("plain", "o'b"):
            with self.subTest(name=name):
                crashes = os.path.join(self.root, name, "crashes") + "/"
                os.makedirs(crashes)
                log, w, inited, _ = self.window("slow_stop_probe.cpp", 1, "120", 0, "-DAFTER=2", "-DSLEEP=11",
                                                "-DSLOW_CALLS=1", wrap=False, extra=[f"-artifact_prefix={crashes}"])
                self.assertEqual((w["stopped_by"], w["killed"], w["status"]), ("cpu-window", False, 0))
                self.assertIn(f"artifact_prefix='{crashes}'; Test unit written to {crashes}slow-unit-".encode(), log)
                parsed = fuzzctl.window_log(log)
                self.assertEqual((parsed["problems"], parsed["done"], parsed["unit_lines"]), ([], inited, 0),
                                 log[-600:])
                self.assertIn(self.judge(log, w, inited), ("certified", "insufficient-progress"))

    def test_a_window_whose_output_was_lost_fails(self):
        # Sol r3 B2: libFuzzer ignores write errors; the discoveries' lines and the DONE line are
        # lost, and the count of units added is written short ("1" for more, no newline).
        log, w, inited, corpus = self.window("lost_output_probe.cpp", 3, "120", 0)
        self.assertEqual((w["stopped_by"], w["killed"], w["status"]), ("cpu-window", False, 0))
        self.assertIn(b"stat::new_units_added:          1stat::slowest_unit_time_sec:", log)
        parsed = fuzzctl.window_log(log)
        self.assertIn("no DONE line, `Done N runs` line and complete final stats", parsed["problems"])
        self.assertEqual(self.judge(log, w, inited), "failed")


class TreeState(Tree):
    """run.sh's record of the source a binary is built from (Sol r2 M2): every change the build
    could see, whatever the user's git configuration."""

    def state(self, repo, **env):  # the lines tree_state prints
        text = open(fuzzctl.RUN_SH).read()
        func = text[text.index("tree_state() {\n"):text.index("\n}\n", text.index("tree_state() {\n")) + 3]
        r = subprocess.run(["bash", "-c", f"set -euo pipefail\nroot={shlex.quote(repo)}\n{func}tree_state"],
                           capture_output=True, text=True, check=True, env={**os.environ, **env})
        return r.stdout.splitlines()

    def test_untracked_files_count_unless_a_committed_gitignore_ignores_them(self):
        repo = os.path.join(self.root, "repo")
        os.mkdir(repo)
        env = {**os.environ, "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1",
               "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@t", "GIT_COMMITTER_NAME": "t",
               "GIT_COMMITTER_EMAIL": "t@t"}
        git = functools.partial(subprocess.run, cwd=repo, env=env, check=True, capture_output=True)
        git(["git", "init", "-q"])
        open(os.path.join(repo, "a.rs"), "w").write("a\n")
        open(os.path.join(repo, ".gitignore"), "w").write("target/\n")
        git(["git", "add", "a.rs", ".gitignore"])
        git(["git", "commit", "-qm", "fixture"])
        head = git(["git", "rev-parse", "--short=12", "HEAD"], text=True).stdout.strip()
        os.makedirs(os.path.join(repo, "target"))
        open(os.path.join(repo, "target", "out"), "w").write("built\n")
        self.assertEqual(self.state(repo), [head])  # clean: target/ is ignored by a committed rule
        # The caller's GIT_* variables are not git's: a work tree elsewhere (a clean copy) does
        # not hide this one's change, nor does another index.
        clean = os.path.join(self.root, "clean")
        shutil.copytree(repo, clean)
        open(os.path.join(repo, "a.rs"), "a").write("edited\n")
        for env in ({"GIT_WORK_TREE": clean}, {"GIT_INDEX_FILE": os.path.join(self.root, "none")}):
            with self.subTest(env=env):
                self.assertEqual(self.state(repo, **env), [head, " M a.rs"])
        git(["git", "checkout", "-q", "a.rs"])
        # Hidden by the repository's own settings: untracked files not shown, a personal ignore
        # file, info/exclude, an untracked .gitignore that ignores itself, a file marked
        # assume-unchanged.
        git(["git", "config", "status.showUntrackedFiles", "no"])
        open(os.path.join(self.root, "excludes"), "w").write("*.rs\nhidden/\n")
        git(["git", "config", "core.excludesFile", os.path.join(self.root, "excludes")])
        open(os.path.join(repo, ".git", "info", "exclude"), "a").write("*.rs\n")
        open(os.path.join(repo, "new.rs"), "w").write("fn main() {}\n")
        os.makedirs(os.path.join(repo, "hidden"))
        open(os.path.join(repo, "hidden", ".gitignore"), "w").write("*\n")
        open(os.path.join(repo, "hidden", "bin.rs"), "w").write("fn main() {}\n")
        git(["git", "update-index", "--assume-unchanged", "a.rs"])
        open(os.path.join(repo, "a.rs"), "a").write("changed\n")
        state = self.state(repo)
        self.assertEqual(state[0], head)
        self.assertIn("?? new.rs", state)
        self.assertIn("?? hidden/", state)
        self.assertIn("h a.rs", state)
        # Outside a repository the state is unknown, never clean.
        self.assertEqual(self.state(os.path.join(self.root, "excludes-dir-missing")), ["-", "unknown"])


class Launching(Tree):
    def launch(self, *extra):
        with mock.patch.object(fuzzctl, "check_registry", return_value=[]), \
                mock.patch.object(fuzzctl, "farm_units", return_value=[]):
            return self.cli(["farm", "launch", "--night", "n1", "--print-only", *extra])

    def test_the_unit_runs_at_the_lowest_priority_with_the_plateau_and_only_flags(self):
        rc, out = self.launch("--only", "kv_model", "--only", "diff_tx_decode", "--plateau-minutes", "15")
        self.assertEqual(rc, 0)
        for prop in ("-p Nice=19", "-p IOSchedulingClass=idle", "-p CPUWeight=10", "-p CPUQuota=800%",
                     "-p KillMode=mixed", "-p TimeoutStopSec=180"):
            self.assertIn(prop, out)
        self.assertNotIn("Nice=10", out)
        self.assertIn("--description=node-fuzz farm ", out)
        self.assertIn("farm work --night n1 ", out)
        self.assertIn("--plateau-minutes 15.0 --only kv_model diff_tx_decode", out)
        self.assertNotIn(self.root, out)
        rc, out = self.launch()
        self.assertIn("--plateau-minutes 60.0", out)
        self.assertNotIn("--only", out)

    def test_a_short_window_or_an_unknown_target_is_refused(self):
        for extra in (("--plateau-minutes", "14"), ("--plateau-minutes", "nan"), ("--only", "typo_target"),
                      ("--only", "selftest_mismatch")):
            with self.assertRaises(SystemExit, msg=extra):
                self.launch(*extra)
        with self.assertRaises(SystemExit):
            self.cli(["farm", "work", "--night", "n1", "--plateau-minutes", "14"])


class WorkerLogs(Tree):
    def test_triage_reads_each_workers_log_once(self):
        run = self.make_run(PUB.name, "k1", log="")
        seg = os.path.join(run, "logs", "segment-1")
        worker = "kv_model: sink write failed\n"
        open(os.path.join(seg, "fuzz-0.log"), "w").write(worker)
        open(os.path.join(seg, "fuzz.out"), "w").write("== Job 0 exited ==\n" + worker)  # the -jobs parent
        self.assertEqual(list(fuzzctl.run_logs(run)), [worker])
        self.assertEqual(fuzzctl.log_evidence(run)["sink_failures"], 1)
        os.remove(os.path.join(seg, "fuzz-0.log"))  # one process: its fuzz.out
        self.assertEqual(fuzzctl.log_evidence(run)["sink_failures"], 1)


class SlowUnits(Tree):
    # Synthetic log: its timeout's ERROR line precedes the write (a real one follows it; see
    # `InterruptedSlices.ALARM`), which `crash_signatures` reads as the class below.
    def test_slow_units_are_not_findings(self):
        run = self.make_run(PRIV.name, "r1", crashes=[("slow-unit-0123", b"slow"), ("timeout-4567", b"t")],
                            log="Slowest unit: 12 s:\nartifact_prefix='/x/'; Test unit written to /x/slow-unit-0123\n"
                                "==1== ERROR: libFuzzer: timeout after 1200 seconds\n"
                                "artifact_prefix='/x/'; Test unit written to /x/timeout-4567\n")
        self.assertEqual([(k, c) for k, c, _, _ in fuzzctl.findings_of(run)],
                         [("timeout", "timeout after N seconds")])
        self.assertEqual(fuzzctl.crash_files(run), ["timeout-4567"])


class InterruptedSlices(Tree):
    """BL-173: a worker can hang in libFuzzer's interrupt path after printing its final stats, until
    its ALARM writes an empty timeout artifact. The farm stops slices through the stop file, with
    SIGINT and SIGKILL as the fallback. Such an artifact stays a finding (D18): triage, the slice's
    verdict and its accounting count it like any timeout, and it carries an advisory
    `suspected_interrupt` label for a human, which never changes a count."""

    farm = Farming.farm
    # libFuzzer's AlarmCallback prints the ALARM lines, writes the artifact, and only then prints
    # its ERROR line, SUMMARY and final stats (FuzzerLoop.cpp), then exits 70.
    ALARM = ("ALARM: working on the last Unit for 1200 seconds\n"
             "       and the timeout value is 1200 (use -timeout=N to change)\n"
             "MS: 0 ; base unit: da39a3ee5e6b4b0d3255bfef95601890afd80709\n\n\n"
             "artifact_prefix='/x/crashes/'; Test unit written to /x/crashes/timeout-{name}\n"
             "Base64: \n==1== ERROR: libFuzzer: timeout after 1200 seconds\n"
             "    #0 0x1 in __sanitizer_print_stack_trace\nSUMMARY: libFuzzer: timeout\n"
             "stat::number_of_executed_units: 99\n")
    # The log of a worker that hung after SIGINT: the ALARM follows `run interrupted`.
    HUNG = "==1== libFuzzer: run interrupted; exiting\nstat::number_of_executed_units: 99\n" + ALARM
    # A timeout of a real input: no interrupt before it.
    REAL = "#5\tpulse  cov: 1 ft: 1 corp: 1/1b exec/s: 1 rss: 1Mb\n" + ALARM
    # The -jobs parent's line for a worker that exited with `code`.
    JOB = "================== Job {job} exited with exit code {code} ============\n"

    def slice_runner(self, honours_stop, fuzzing, phantom, started, ended):
        """A slice whose libFuzzer reads its stop file once `fuzzing` (INITED is in its log; before
        that it is building or loading) when `honours_stop`, and exits when `ended` is set (a signal
        it dies of). With `phantom` its ALARM wrote an empty timeout artifact anyway (a finding with a
        label, D18). `started` is set once it runs."""
        def runner(target, pin, args, on_start=None, **_):
            sid = args[args.index("--run-tag") + 1]
            run = os.path.join(self.fdir, target, "runs", f"20261011T000000Z-{pin}-1-{sid}")
            seg = os.path.join(run, "logs", "segment-1")
            os.makedirs(seg)
            os.makedirs(os.path.join(run, "crashes"))
            log = cov_line(0, 70, 140, "INITED") if fuzzing else "INFO: loading\n"
            if phantom:
                log += self.HUNG.format(name="0123")
                open(os.path.join(run, "crashes", "timeout-0123"), "wb").close()
            open(os.path.join(seg, "fuzz.out"), "w").write(log)
            on_start(types.SimpleNamespace(pid=1))
            started.set()
            bound = time.monotonic() + 60
            while time.monotonic() < bound and not ended.is_set():
                if honours_stop and os.path.exists(os.path.join(run, "stop")):
                    break
                time.sleep(0.01)
            v = {"status": "fail", "cpu_hours": 0.01, "executions": 5, "divergences": 0,
                 "crashes": int(phantom), "sink_failures": 0, "libfuzzer_status": 70 if phantom else 137}
            return Leg(target, pin, "node-fuzz fake", 1, run, v)
        return runner

    def stop_a_slice(self, honours_stop=False, sigint_ends=False, fuzzing=True, phantom=False, stop_wait=0.2):
        """Stops a running slice; returns the signals it was sent, its ledger row and its run. A
        SIGKILL always ends it, a SIGINT only when `sigint_ends`."""
        signalled, started, ended = [], threading.Event(), threading.Event()

        def killpg(pid, sig):
            signalled.append(sig)
            if sig == signal.SIGKILL or sigint_ends:
                ended.set()

        f, logs = self.farm(self.slice_runner(honours_stop, fuzzing, phantom, started, ended),
                            only=["kv_model"], clock=time.monotonic)
        f.stop_wait, f.int_wait = stop_wait, 0.2
        with mock.patch.object(fuzzctl.os, "killpg", killpg), mock.patch.object(fuzzctl.Farm, "triage"):
            t = threading.Thread(target=f.work)
            t.start()
            self.assertTrue(started.wait(30))
            f.stop.set()
            t.join(30)
        self.assertFalse(t.is_alive())
        (row,) = f.records()
        run = os.path.join(self.fdir, "kv_model", "runs", f"20261011T000000Z-v24.0-1-{row['slice']}")
        return signalled, row, run

    def test_a_slice_is_stopped_through_its_stop_file_and_needs_no_signal(self):
        signalled, row, run = self.stop_a_slice(honours_stop=True, stop_wait=60)
        self.assertEqual(signalled, [])
        self.assertEqual(open(os.path.join(run, "stop"), "rb").read(), b"stop\n")  # at least one byte
        self.assertEqual((row["status"], row["crashes"]), ("interrupted", 0))

    def test_a_worker_that_hangs_after_the_stop_is_signalled_and_its_timeout_is_a_labelled_finding(self):
        # Its ALARM fired before the fallback (a longer hang than the grace), as in BL-173. D18: the
        # empty timeout is a finding like any other, so the slice counts as one; the label says why.
        signalled, row, run = self.stop_a_slice(phantom=True)
        self.assertEqual(signalled, [signal.SIGINT, signal.SIGKILL])  # the stop file, then the fallback
        self.assertTrue(os.path.exists(os.path.join(run, "stop")))
        self.assertEqual((row["status"], row["crashes"]), ("finding", 1))
        self.assertEqual([k for k, _, _, _ in fuzzctl.findings_of(run)], ["timeout"])
        self.assertEqual(list(fuzzctl.suspected_interrupts(run)), ["timeout-0123"])

    def test_a_slice_still_building_or_loading_is_sent_the_sigint_at_once(self):
        # libFuzzer reads the stop file only once fuzzing: no wait for it before INITED, but the
        # file is there, so run.sh starts no segment.
        signalled, row, run = self.stop_a_slice(sigint_ends=True, fuzzing=False, stop_wait=60)
        self.assertEqual(signalled, [signal.SIGINT])
        self.assertTrue(os.path.exists(os.path.join(run, "stop")))
        self.assertEqual(row["status"], "interrupted")

    def test_a_slice_that_ignores_the_sigint_before_inited_is_killed(self):
        signalled, row, _ = self.stop_a_slice(fuzzing=False, stop_wait=60)
        self.assertEqual(signalled, [signal.SIGINT, signal.SIGKILL])
        self.assertEqual(row["status"], "interrupted")

    def test_an_empty_timeout_after_an_interrupt_is_a_finding_with_a_label(self):
        run = self.two_workers(self.HUNG.format(name="0123"), self.HUNG.format(name="0123"))
        label = {"segment": 1, "worker": 0, "interrupt": "==1== libFuzzer: run interrupted; exiting"}
        self.assertEqual(fuzzctl.suspected_interrupts(run), {"timeout-0123": label})
        self.assertEqual(fuzzctl.crash_files(run), ["timeout-0123"])
        self.assertEqual(fuzzctl.run_findings(run)["crashes"], 1)
        self.assertEqual([k for k, _, _, _ in fuzzctl.findings_of(run)], ["timeout"])
        tri = fuzzctl.run_triage(TARGETS, runner=FakeRunner(self))
        self.assertEqual((tri.stats["findings"], tri.stats["new_clusters"], tri.stats["suspected_interrupts"]), (1, 1, 1))
        (cluster,) = self.state(PUB.name)["clusters"].values()
        self.assertEqual((cluster["state"], cluster["hits"], cluster["interrupts"]), ("open", 1, 1))
        self.assertEqual(cluster["interrupt_evidence"], [{"run": "k1", **label}])
    def test_a_real_timeout_is_still_a_finding(self):
        # A timeout of an input with bytes, even one logged after an interrupt, and an empty input that
        # hangs on its own (no interrupt before its ALARM).
        hung = self.make_run(PUB.name, "k1", crashes=[("timeout-4567", b"t")], log=self.HUNG.format(name="4567"))
        empty = self.make_run(PUB.name, "k2", crashes=[("timeout-89ab", b"")], log=self.REAL.format(name="89ab"))
        for run, name in ((hung, "timeout-4567"), (empty, "timeout-89ab")):
            self.assertEqual(fuzzctl.suspected_interrupts(run), {})
            self.assertEqual(fuzzctl.crash_files(run), [name])
            self.assertEqual([(k, c) for k, c, _, _ in fuzzctl.findings_of(run)], [("timeout", "unknown")])
        tri = fuzzctl.run_triage(TARGETS, runner=FakeRunner(self))
        self.assertEqual((tri.stats["findings"], tri.stats["new_clusters"], tri.stats["suspected_interrupts"]), (2, 1, 0))
        (cluster,) = self.state(PUB.name)["clusters"].values()
        self.assertEqual((cluster["kind"], cluster["state"], cluster["hits"], cluster.get("interrupts", 0)),
                         ("timeout", "open", 2, 0))

    def jobs_run(self, logs, codes=None, run_id="k1", name="timeout-0123", target=PUB):
        """A -jobs run as run.sh leaves it: one `fuzz-<i>.log` per entry of `logs`, the parent's fuzz.out with the `Job <i> exited` line of each worker (exit 70 unless `codes`) followed by
        the parent's copy of that worker's log, and one empty artifact."""
        run = self.make_run(target.name, run_id, crashes=[(name, b"")])
        seg = os.path.join(run, "logs", "segment-1")
        for n, log in enumerate(logs):
            open(os.path.join(seg, f"fuzz-{n}.log"), "w").write(log)
        open(os.path.join(seg, "fuzz.out"), "w").write("".join(
            self.JOB.format(job=n, code=c) + log
            for n, (c, log) in enumerate(zip(codes or (70,) * len(logs), logs))))
        return run

    def two_workers(self, first, second, name="timeout-0123"):
        """A -jobs run whose workers logged `first` and `second` and left one empty artifact."""
        return self.jobs_run([first, second], name=name)

    def test_a_timeouts_report_is_no_second_crash_and_no_unsaved_one(self):
        # BL-173 D11: the ERROR line after a timeout artifact is that artifact's report. With
        # libFuzzer's real order it once counted as a crash libFuzzer never saved, so a hung worker's
        # timeout (and a real one) counted twice.
        run = self.two_workers(self.HUNG.format(name="0123"), self.HUNG.format(name="0123"))
        self.assertEqual(fuzzctl.log_evidence(run)["unsaved_crashes"], [])
        self.assertEqual(fuzzctl.run_findings(run)["crashes"], 1)  # the saved file, once
        self.assertEqual([k for k, _, _, _ in fuzzctl.findings_of(run)], ["timeout"])
        f, _ = self.farm(SliceRunner())
        verdict = {"status": "fail", "crashes": 1, "divergences": 0, "sink_failures": 0, "cpu_hours": 0.1,
                   "libfuzzer_status": 1}
        self.assertEqual(f.accounted(run, verdict, False)[0], "finding")
        real = self.make_run(PUB.name, "k2", crashes=[("timeout-4567", b"")], log=self.REAL.format(name="4567"))
        self.assertEqual(fuzzctl.run_findings(real)["crashes"], 1)  # once, not twice
        self.assertEqual([k for k, _, _, _ in fuzzctl.findings_of(real)], ["timeout"])

    def test_only_a_timeouts_own_report_is_skipped(self):
        # A saved timeout's ERROR and SUMMARY are its report (D11); an unsaved write, or a later
        # failure after the SUMMARY (a merge's next attempt), still counts.
        crash = "==9== ERROR: libFuzzer: deadly signal\n"
        later = self.make_run(PUB.name, "k1", crashes=[("timeout-0123", b"")],
                              log=self.REAL.format(name="0123") + crash)
        self.assertEqual(fuzzctl.log_evidence(later)["unsaved_crashes"], ["deadly signal"])
        unsaved = self.make_run(PUB.name, "k2", log=self.REAL.format(name="0123"))  # the write failed
        self.assertEqual(len(fuzzctl.log_evidence(unsaved)["unsaved_crashes"]), 1)

    def test_a_torn_timeout_report_hides_no_later_attempts_crash(self):
        # Sol r2 #1 (D12): a merge's child killed after its timeout write, before its SUMMARY, leaves
        # a torn report; the retry's unsaved crash lands in the same log and must still count.
        torn = self.ALARM.format(name="0123").split("SUMMARY")[0]
        crash = "==9== ERROR: libFuzzer: deadly signal\n"
        for retry in ("MERGE-OUTER: attempt 2\n", "MERGE-INNER: using the control file 'x'\n", ""):
            run = self.make_run(PUB.name, f"k{len(retry)}", crashes=[("timeout-0123", b"")],
                                log=torn + retry + crash + "SUMMARY: libFuzzer: deadly signal\n")
            self.assertEqual(fuzzctl.log_evidence(run)["unsaved_crashes"], ["deadly signal"], retry)
        # With its SUMMARY the same report is skipped, the crash still counts, and so does a second
        # ERROR inside the span (it is not the timeout's own).
        whole = self.make_run(PUB.name, "k8", crashes=[("timeout-0123", b"")],
                              log=self.ALARM.format(name="0123") + "MERGE-OUTER: attempt 2\n" + crash)
        self.assertEqual(fuzzctl.log_evidence(whole)["unsaved_crashes"], ["deadly signal"])
        twice = self.make_run(PUB.name, "k9", crashes=[("timeout-0123", b"")], log=torn + crash)
        self.assertEqual(fuzzctl.log_evidence(twice)["unsaved_crashes"], ["deadly signal"])
        alone = self.make_run(PUB.name, "k10", crashes=[("timeout-0123", b"")], log=torn)
        self.assertEqual(fuzzctl.log_evidence(alone)["unsaved_crashes"], [])  # its own, torn report
        # A later attempt's own timeout is not the first one's report either.
        timeout = "==9== ERROR: libFuzzer: timeout after 5 seconds\nSUMMARY: libFuzzer: timeout\n"
        for n, first in enumerate((self.ALARM.format(name="0123"), torn)):
            run = self.make_run(PUB.name, f"k1{n}", crashes=[("timeout-0123", b"")],
                                log=first + "MERGE-OUTER: attempt 2\n" + timeout)
            self.assertEqual(fuzzctl.log_evidence(run)["unsaved_crashes"], ["timeout after N seconds"], n)

    def test_the_label_is_for_a_timeout_every_logged_write_of_which_followed_an_interrupt(self):
        real, hung, idle = self.REAL.format(name="0123"), self.HUNG.format(name="0123"), "stat::number_of_executed_units: 5\n"
        # A worker's own log that times out, is interrupted, and times out again.
        both = real + self.HUNG.format(name="0123")
        for n, (logs, want) in enumerate((([hung, hung], True), ([hung, idle], True), ([real, hung], False),
                                          ([hung, real], False), ([both, hung], False), ([idle, idle], False),
                                          (["junk ==11== libFuzzer: run interrupted; exiting\n" + self.ALARM.format(name="0123")], False))):
            run = self.jobs_run(logs, run_id=f"l{n}")
            self.assertEqual(bool(fuzzctl.suspected_interrupts(run)), want, n)
            # Whatever the label, the timeout is a finding.
            self.assertEqual(fuzzctl.crash_files(run), ["timeout-0123"], n)
            self.assertEqual(fuzzctl.run_findings(run)["crashes"], 1, n)

    def test_nothing_in_the_logs_drops_a_timeout_as_a_finding(self):
        # D18, from Sol r2-r6: a missing or emptied worker log, a cut parent, a forged parent line in
        # worker output, a bad jobs file and a lone or merge log each made some inference call a real
        # timeout a phantom. No inference decides anything now: the artifact is a finding, labelled or not.
        real, hung = self.REAL.format(name="0123"), self.HUNG.format(name="0123")
        f, _ = self.farm(SliceRunner())
        verdict = {"status": "fail", "crashes": 1, "divergences": 0, "sink_failures": 0, "cpu_hours": 0.1,
                   "libfuzzer_status": 1}
        runs = []

        def run(variant, logs=(hung, real)):
            runs.append(self.jobs_run(list(logs), run_id=variant))
            return runs[-1], os.path.join(runs[-1], "logs", "segment-1")

        _, seg = run("missing-worker-log")
        os.remove(os.path.join(seg, "fuzz-1.log"))
        _, seg = run("emptied-worker-log")
        open(os.path.join(seg, "fuzz-1.log"), "w").write("")
        _, seg = run("last-worker-gone-from-both")  # Sol r5 #1
        os.remove(os.path.join(seg, "fuzz-1.log"))
        parent = open(os.path.join(seg, "fuzz.out")).read()
        open(os.path.join(seg, "fuzz.out"), "w").write(parent[:parent.index(self.JOB.format(job=1, code=70))])
        # Sol r6 #1: worker 0's output carries a line shaped like the parent's exit line for worker 1,
        # and worker 1's log and the parent's copy are gone.
        _, seg = run("forged-job-line", (self.JOB.format(job=1, code=70) + hung, real))
        open(os.path.join(seg, "fuzz-1.log"), "w").write("")
        parent = open(os.path.join(seg, "fuzz.out")).read()
        open(os.path.join(seg, "fuzz.out"), "w").write(parent[:parent.rindex(self.JOB.format(job=1, code=70))])
        _, seg = run("huge-jobs-file")  # Sol r6 #2
        open(os.path.join(seg, "jobs"), "w").write("9" * 5000 + "\n")
        _, seg = run("binary-jobs-file")
        open(os.path.join(seg, "jobs"), "wb").write(b"\xff\xfe\n")
        for n, log in enumerate(("MERGE-OUTER: attempt 1\n" + hung + "MERGE-OUTER: attempt 2\n" + real,  # Sol r5 #2
                                 "MERGE-OUTER: attempt 1\n" + hung, hung)):
            runs.append(self.make_run(PUB.name, f"lone{n}", crashes=[("timeout-0123", b"")], log=log))
        for run_dir in runs:
            with self.subTest(os.path.basename(run_dir)):
                self.assertEqual(fuzzctl.crash_files(run_dir), ["timeout-0123"])
                self.assertEqual(fuzzctl.run_findings(run_dir)["crashes"], 1)
                self.assertEqual([k for k, _, _, _ in fuzzctl.findings_of(run_dir)], ["timeout"])
                self.assertEqual(f.accounted(run_dir, dict(verdict), False)[0], "finding")
        tri = fuzzctl.run_triage(TARGETS, runner=FakeRunner(self))
        self.assertEqual(tri.stats["findings"], len(runs))
        clusters = self.state(PUB.name)["clusters"].values()  # the class follows the log's text, not the loss
        self.assertEqual(({c["kind"] for c in clusters}, {c["state"] for c in clusters}, sum(c["hits"] for c in clusters)),
                         ({"timeout"}, {"open"}, len(runs)))

    def test_an_unreadable_worker_log_keeps_the_finding(self):
        run = self.two_workers(self.HUNG.format(name="0123"), self.HUNG.format(name="0123"))
        os.chmod(os.path.join(run, "logs", "segment-1", "fuzz-1.log"), 0)
        if os.access(os.path.join(run, "logs", "segment-1", "fuzz-1.log"), os.R_OK):
            self.skipTest("the file stays readable (running as root)")
        self.assertEqual(fuzzctl.run_findings(run)["crashes"], 1)

    def test_a_recovered_slice_that_was_stopped_through_its_stop_file_is_interrupted(self):
        f, _ = self.farm(SliceRunner())
        run = self.make_run(PUB.name, "k1")
        open(os.path.join(run, "stop"), "w").write("stop\n")
        verdict = {"status": "fail", "crashes": 0, "divergences": 0, "sink_failures": 0, "cpu_hours": 0.1,
                   "libfuzzer_status": 0}
        self.assertEqual(f.accounted(run, verdict, False, live=False)[0], "interrupted")


class ClosePhantom(Tree):
    """BL-173 D13, D18: close-phantom is a manual command. It takes one cluster id, a written
    reason and a name; it closes only an open cluster every hit of which carries the
    `suspected_interrupt` label; and nothing but the CLI runs it."""

    HUNG, REAL = InterruptedSlices.HUNG, InterruptedSlices.REAL
    jobs_run = InterruptedSlices.jobs_run
    JOB = InterruptedSlices.JOB
    REASON = "empty input; the worker hung after the stop file until its ALARM"

    def labelled(self, target, run_id):
        """A -jobs run whose empty timeout every log shows written after an interrupt."""
        return self.jobs_run([self.HUNG.format(name="0123")] * 2, run_id=run_id, target=target)

    def triage(self):
        return fuzzctl.run_triage(TARGETS, runner=FakeRunner(self))

    def cluster_of(self, target):
        ((cid, c),) = self.state(target.name)["clusters"].items()
        return cid, c

    def audit(self, target):
        path = os.path.join(self.fdir, target.name, fuzzctl.AUDIT_NAME)
        return [json.loads(line) for line in open(path)] if os.path.exists(path) else []

    def close(self, cid, reason=REASON, by="pasta", targets=None):
        out = []
        return fuzzctl.close_phantom(targets or TARGETS, cid, reason, by, log=out.append), "\n".join(out)

    def snapshot(self, target):
        return (open(os.path.join(self.fdir, target.name, "triage.json")).read(), self.audit(target))

    def test_a_cluster_of_labelled_hits_is_closed_by_hand_with_who_when_and_why(self):
        self.labelled(PUB, "r1")
        self.triage()
        cid, c = self.cluster_of(PUB)
        self.assertEqual((c["state"], c["hits"], c["interrupts"]), ("open", 1, 1))
        self.assertEqual(fuzzctl.eligibility(self.fdir, PUB.name, "v24.0"), ["an open finding"])
        vector = c["vector"]
        self.assertTrue(os.path.isfile(os.path.join(self.fdir, PUB.name, "regress", "open", vector)))
        closed, out = self.close(cid)
        self.assertTrue(closed, out)
        cluster = self.state(PUB.name)["clusters"][cid]
        self.assertEqual((cluster["state"], cluster["fixed_at"], cluster["fixed_stamp"], cluster["closed_by"],
                          cluster["closed_reason"]), ("fixed", "close-phantom", "", "pasta", self.REASON))
        (row,) = self.audit(PUB)
        self.assertEqual((row["event"], row["by"], row["reason"], row["cluster"], row["class"], row["hits"], row["rule"]),
                         ("close-phantom", "pasta", self.REASON, cid, c["class"], 1, "BL-173 D18"))
        self.assertEqual(row["user"], getpass.getuser())
        self.assertRegex(row["at"], r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d")
        self.assertEqual(row["interrupt_evidence"], c["interrupt_evidence"])
        self.assertEqual(oct(os.stat(os.path.join(self.fdir, PUB.name, fuzzctl.AUDIT_NAME)).st_mode & 0o777), "0o600")
        self.assertTrue(os.path.isfile(os.path.join(self.fdir, PUB.name, "regress", "fixed", vector)))  # moved
        self.assertFalse(os.path.exists(os.path.join(self.fdir, PUB.name, "regress", "open", vector)))
        self.assertEqual(fuzzctl.eligibility(self.fdir, PUB.name, "v24.0"), [])
        # It is not open any more, and a later run that finds the class again reopens it.
        self.assertIn("it is not open", self.close(cid)[1])
        self.labelled(PUB, "r2")
        self.triage()
        self.assertEqual(self.state(PUB.name)["clusters"][cid]["state"], "open")

    def test_a_close_without_a_reason_or_a_name_changes_nothing(self):
        self.labelled(PUB, "r1")
        self.triage()
        cid, _ = self.cluster_of(PUB)
        before = self.snapshot(PUB)
        for reason, by in (("", "pasta"), ("   ", "pasta"), (None, "pasta"), (self.REASON, ""), (self.REASON, None)):
            closed, out = self.close(cid, reason, by)
            self.assertFalse(closed, (reason, by))
            self.assertIn("a written reason and who is closing it are required", out)
            self.assertEqual(self.snapshot(PUB), before)
        for argv in (["close-phantom", cid, "--by", "pasta"], ["close-phantom", cid, "--reason", self.REASON]):
            with self.assertRaises(SystemExit):  # argparse: both are required
                self.cli(argv)
        rc, out = self.cli(["close-phantom", cid, "--reason", self.REASON, "--by", "pasta"])
        self.assertEqual(rc, 0, out)
        self.assertEqual(self.state(PUB.name)["clusters"][cid]["state"], "fixed")

    def test_a_cluster_with_a_hit_the_label_does_not_cover_is_refused(self):
        self.labelled(PUB, "r1")
        self.jobs_run([self.REAL.format(name="0123")] * 2, run_id="r2")  # a real timeout of the empty input
        self.triage()
        cid, c = self.cluster_of(PUB)
        self.assertEqual((c["hits"], c["interrupts"]), (2, 1))
        before = self.snapshot(PUB)
        closed, out = self.close(cid)
        self.assertFalse(closed)
        self.assertIn("only 1 of its 2 hits carry a suspected_interrupt label", out)
        self.assertEqual(self.snapshot(PUB), before)
        # Another labelled hit does not cover the unlabelled one.
        self.labelled(PUB, "r3")
        self.triage()
        self.assertEqual(self.cluster_of(PUB)[1]["interrupts"], 2)
        self.assertFalse(self.close(cid)[0])

    def test_clusters_that_were_never_labelled_or_that_do_not_exist_are_refused(self):
        self.make_run(PUB.name, "r1", divergences=[(b"abcdef", "size")])
        self.jobs_run([self.REAL.format(name="4567")] * 2, run_id="r2", name="timeout-4567")
        self.triage()
        before = self.snapshot(PUB)
        for cid in self.state(PUB.name)["clusters"]:
            self.assertIn("carry a suspected_interrupt label", self.close(cid)[1])
        # A cluster of the state before the label: no `interrupts` at all.
        state = self.state(PUB.name)
        for c in state["clusters"].values():
            c.pop("interrupts", None)
        open(os.path.join(self.fdir, PUB.name, "triage.json"), "w").write(json.dumps(state))
        before = self.snapshot(PUB)
        for cid in state["clusters"]:
            self.assertFalse(self.close(cid)[0])
        closed, out = self.close("0" * 12)
        self.assertEqual((closed, out), (False, "node-fuzz close-phantom refused: no such cluster"))
        self.assertEqual(self.snapshot(PUB), before)

    def test_a_private_target_is_never_named(self):
        self.labelled(PRIV, "r1")
        self.triage()
        cid, c = self.cluster_of(PRIV)
        closed, out = self.close(cid)
        self.assertTrue(closed, out)
        for secret in (PRIV.name, c["class"], c["kind"]):
            self.assertNotIn(secret, out)
        self.assertEqual(self.state(PRIV.name)["clusters"][cid]["state"], "fixed")

    def test_no_automatic_route_reaches_close_phantom(self):
        # D18: only the CLI closes a cluster. Neither triage, the farm nor the nightly path calls it.
        source = open(fuzzctl.__file__).read()
        self.assertEqual(len(re.findall(r"\bclose_phantom\(", source)), 2)  # its definition and the CLI call
        for cls in (fuzzctl.Triage, fuzzctl.Farm):
            self.assertNotIn("close_phantom", inspect.getsource(cls))
        nightly = os.path.join(os.path.dirname(fuzzctl.__file__), "nightly.sh")
        if os.path.exists(nightly):
            self.assertNotIn("close-phantom", open(nightly).read())


class RunShStopFile(Tree):
    """run.sh's run-mode arguments and segment loop, on a stand-in for libFuzzer that runs until its
    stop file exists (as libFuzzer reads it at the top of its loop) and exits 0."""

    FUZZER = ("#!/usr/bin/env python3\nimport os, sys, time\n"
              "open(os.path.join(os.getcwd(), 'argv'), 'w').write('\\n'.join(sys.argv[1:]))\n"
              "stop = next(a.split('=', 1)[1] for a in sys.argv if a.startswith('-stop_file='))\n"
              "print('#1\\tINITED cov: 1 ft: 1', flush=True)\n"
              "while not os.path.exists(stop):\n    time.sleep(0.05)\n"
              "print('Done 9 runs in 1 second(s)', flush=True)\n")

    def start(self, fuzzer=None, cpu_hours="1", patch=(), prefix=(), jobs="1"):
        """A launcher of the segment loop on the stand-in `fuzzer` (Python source), `patch`ed with
        (old, new) replacements, behind the `prefix` command."""
        text = open(fuzzctl.RUN_SH).read()
        start = text.index('args=("-artifact_prefix=$run/crashes/")')
        end = text.index("# A certify run (DEC-186", start)
        script = text[start:end]
        for old, new in patch:
            assert script.count(old) == 1, old
            script = script.replace(old, new)
        self.run_dir = os.path.join(self.root, "run")
        for sub in ("tmp", "crashes", "divergences", "logs"):
            os.makedirs(os.path.join(self.run_dir, sub))
        fake = os.path.join(self.root, "fake-fuzzer")
        open(fake, "w").write(fuzzer or self.FUZZER)
        os.chmod(fake, 0o700)
        env = {**os.environ, "run": self.run_dir, "tdir": os.path.join(self.root, "target"), "mode": "run",
               "runs": "", "jobs": jobs, "cpu_hours": cpu_hours, "bin": fake, "pin": "v24.0", "run_id": "run",
               "here": fuzzctl.HERE, "target": "kv_model"}

        def launch():
            proc = subprocess.Popen([*prefix, "bash", "-c", "set -euo pipefail\n" + script], env=env)
            self.addCleanup(proc.kill)
            return proc

        return launch

    def segments(self):
        return sorted(os.listdir(os.path.join(self.run_dir, "logs")))

    def test_a_stop_file_ends_the_run_after_its_segment_short_of_the_budget(self):
        proc = self.start()()
        log = os.path.join(self.run_dir, "logs", "segment-1", "fuzz.out")
        deadline = time.monotonic() + 60
        while not (os.path.exists(log) and "INITED" in open(log).read()) and time.monotonic() < deadline:
            time.sleep(0.05)
        open(os.path.join(self.run_dir, "stop"), "w").write("stop\n")
        self.assertEqual(proc.wait(60), 0)
        self.assertEqual(self.segments(), ["segment-1"])  # no second segment for the missing budget
        self.assertIn("Done 9 runs", open(log).read())
        # The segment's own stop file is libFuzzer's; the driver copies the run's to it (BL-228).
        args = open(os.path.join(self.run_dir, "logs", "segment-1", "argv")).read().split("\n")
        self.assertIn(f"-stop_file={self.run_dir}/logs/segment-1/stop", args)
        self.assertEqual(json.load(open(os.path.join(self.run_dir, "logs", "segment-1", "segment.json")))["stopped_by"],
                         "run-stop")

    def test_a_run_stopped_before_its_first_segment_starts_none(self):
        launch = self.start()
        open(os.path.join(self.run_dir, "stop"), "w").write("stop\n")
        self.assertEqual(launch().wait(60), 0)
        self.assertEqual(self.segments(), [])


class RunShCpuSegments(Tree):
    """BL-228: a farm segment ends on its own CPU time, not on a wall clock. The stand-in libFuzzer
    keeps libFuzzer's -max_total_time (wall seconds) and its stop file, and burns CPU for a fraction
    of the wall time it runs, as a process starved by a loaded box does."""

    BUDGET = "0.0003"  # CPU-h: 1.08 CPU-s, an allotment of 3 (ceil, and the second that clears the budget)
    ALLOTMENT = 3

    def fuzzer(self, duty, extra=""):
        return ("#!/usr/bin/env python3\nimport os, sys, time\n"
                "args = sys.argv[1:]\n"
                "stop = next(a.split('=', 1)[1] for a in args if a.startswith('-stop_file='))\n"
                "limit = next(int(a.split('=', 1)[1]) for a in args if a.startswith('-max_total_time='))\n"
                "begin = time.monotonic()\n"
                "t = time.thread_time() + 0.3\nwhile time.thread_time() < t: pass  # the corpus load\n"
                "print('#1\\tINITED cov: 1 ft: 1', flush=True)\n" + extra +
                f"while not os.path.exists(stop):\n    if time.monotonic() - begin >= limit:\n"
                "        print('Done 9 runs in 1 second(s)', flush=True)\n        sys.exit(0)  # libFuzzer's own clock\n"
                f"    t = time.thread_time() + 0.05 * {duty}\n    while time.thread_time() < t: pass\n"
                f"    time.sleep(0.05 * (1 - {duty}))\n"
                "print('Done 9 runs in 1 second(s)', flush=True)\n")

    def run_scenario(self, duty, patch=(), prefix=(), timeout=300):
        """Runs the segment loop to its end; returns what it left, and the problems with it."""
        proc = RunShStopFile.start(self, self.fuzzer(duty), cpu_hours=self.BUDGET, patch=patch, prefix=prefix)()
        rc = proc.wait(timeout)
        logs = os.path.join(self.run_dir, "logs")
        cpu = sum(float(x) for f in glob.glob(os.path.join(logs, "*", "cpu")) for x in open(f).read().split()[-2:])
        segs = sorted(os.listdir(logs))
        problems = []
        if rc != 0:
            problems.append(f"exit {rc}")
        if segs != ["segment-1"]:
            problems.append(f"segments {segs}: a segment ended short of its CPU allotment and the next reloaded the corpus")
        if cpu < float(self.BUDGET) * 3600:
            problems.append(f"{cpu:.2f} CPU-s is short of the budget")
        if cpu > self.ALLOTMENT + 1.5:
            problems.append(f"{cpu:.2f} CPU-s is well past the allotment")
        record = os.path.join(logs, "segment-1", "segment.json")
        if not os.path.exists(record) or json.load(open(record))["stopped_by"] != "cpu-window":
            problems.append("the segment was not stopped on its CPU time")
        return cpu, segs, problems

    def test_a_starved_segment_ends_on_its_cpu_time_not_the_wall_clock(self):
        # 30 % of a core: 3 CPU-s take about 10 s of wall time. The wall-clock design gave the same
        # budget about max(60, ...) wall seconds, which a starved segment spends reloading.
        start = time.monotonic()
        cpu, segs, problems = self.run_scenario(0.3)
        self.assertEqual(problems, [])
        self.assertGreater(time.monotonic() - start, 1.5 * cpu)  # starved: wall time well past its CPU time

    def test_a_segment_pinned_with_busy_hogs_ends_on_its_cpu_time(self):
        # The real thing: the segment shares one CPU with two busy loops and runs at a higher nice
        # value, as the farm does on a loaded box.
        cpu_id = sorted(os.sched_getaffinity(0))[0]
        hogs = [subprocess.Popen(["taskset", "-c", str(cpu_id), sys.executable, "-c", "while True: pass"])
                for _ in range(2)]
        for hog in hogs:
            self.addCleanup(hog.kill)
        cpu, segs, problems = self.run_scenario(1.0, prefix=("taskset", "-c", str(cpu_id), "nice", "-n", "5"))
        self.assertEqual(problems, [])

    def test_the_wall_clock_stop_fails_the_scenario(self):
        # The mutant: the pre-BL-228 stop, libFuzzer's -max_total_time in wall seconds (the old
        # max(60, missing + 30)) with no driver. The starved segment ends at its wall clock, not
        # on its CPU time: here it overshoots the allotment, and with a larger budget it ends
        # short of it and the next segment reloads the corpus.
        mutant = [('segment_args+=("-stop_file=$seg/stop" "-max_total_time=$segment_backstop")',
                   'segment_args+=("-stop_file=$run/stop" "-max_total_time=$((cpu_seconds < 60 ? 60 : cpu_seconds + 29))")'),
                  ('python3 -I "$here/window_driver.py" --farm "$run/stop" "$seg" "$cpu_seconds" "$((segment_backstop + 3600))" \\\n'
                   '            "$run/tmp" "$target" "$pin" "$run_id" "$bin" "${segment_args[@]}" "${inputs[@]}" || status=$?',
                   '(cd "$seg" && /usr/bin/time -f \'%U %S\' -o "$seg/cpu" "$bin" "${segment_args[@]}" "${inputs[@]}" >"$seg/fuzz.out" 2>&1) || status=$?')]
        cpu, segs, problems = self.run_scenario(0.1, patch=mutant)
        self.assertIn("the segment was not stopped on its CPU time", problems)
        self.assertTrue(len(problems) > 1, (segs, cpu, problems))


class FarmSegmentDriver(Tree):
    """window_driver.py --farm (BL-228): a segment's process tree ends at its CPU allotment."""

    def run_farm(self, allotment, body, cap=600, run_stop=None, patch=(), **env):
        fake = os.path.join(self.root, "fake-fuzzer")
        open(fake, "w").write("#!/usr/bin/env python3\nimport os, signal, subprocess, sys, time\n" + body)
        os.chmod(fake, 0o700)
        self.seg = os.path.join(self.root, "seg")
        os.mkdir(self.seg)
        self.run_stop = run_stop or os.path.join(self.root, "stop")
        argv = [sys.executable, "-I", driver_script(self.root, patch), "--farm", self.run_stop, self.seg, str(allotment),
                str(cap), self.root, "kv_model", "v24.0", "run", fake, f"-stop_file={self.seg}/stop", "corpus"]
        return subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                                env={**os.environ, "NODE_FUZZ_CERTIFY_POLL": "0.2", "NODE_FUZZ_CERTIFY_GRACE": "300", **env})

    def finish(self, proc, rc=0):
        out, err = proc.communicate(timeout=300)
        self.assertEqual(proc.returncode, rc, err)
        return json.load(open(os.path.join(self.seg, "segment.json")))

    STOP_FILE = ("STOP = next(a.split('=', 1)[1] for a in sys.argv if a.startswith('-stop_file='))\n"
                 "print('#2\\tINITED cov: 5 ft: 9', flush=True)\n")

    def test_the_tree_s_cpu_ends_the_segment_children_included(self):
        # Two workers burn the CPU, as -jobs does; the parent only waits for the stop file. The
        # allotment is the tree's, so it ends the segment; the parent's own 0 CPU-s never would.
        body = (self.STOP_FILE + "kids = [subprocess.Popen([sys.executable, '-c', 'while True: pass']) for _ in range(2)]\n"
                "begin = time.monotonic()\nwhile not os.path.exists(STOP) and time.monotonic() - begin < 90:\n"
                "    time.sleep(0.05)\n[k.kill() for k in kids]\n[k.wait() for k in kids]\n")
        s = self.finish(self.run_farm(4, body))
        self.assertEqual((s["stopped_by"], s["status"], s["killed"]), ("cpu-window", 0, False))
        self.assertGreaterEqual(s["cpu_at_stop"], 4)
        self.assertLess(s["elapsed"], 60)
        used = sum(float(x) for x in open(os.path.join(self.seg, "cpu")).read().split())
        self.assertGreaterEqual(used, 4)  # the killed workers' CPU is the parent's reaped children's
        self.assertFalse(os.path.exists(os.path.join(self.seg, "window.json")))

    def test_the_cpu_of_reaped_children_counts(self):
        # A worker that burned 2 CPU-s and was reaped is in its parent's cutime, which the tree sum
        # reads (utime and stime alone would see an idle parent).
        body = (self.STOP_FILE + "subprocess.run([sys.executable, '-c', 'import time\\nt = time.process_time() + 2\\n"
                "while time.process_time() < t: pass'])\nbegin = time.monotonic()\n"
                "while not os.path.exists(STOP) and time.monotonic() - begin < 40:\n    time.sleep(0.05)\n")
        s = self.finish(self.run_farm(2, body))
        self.assertEqual((s["stopped_by"], s["status"]), ("cpu-window", 0))
        self.assertGreaterEqual(s["cpu_at_stop"], 2)

    def test_the_grace_kill_takes_the_workers_with_it(self):
        # Workers have no parent-death signal: one that outlives its killed parent would fuzz on.
        pidfile = os.path.join(self.root, "worker.pid")
        body = (self.STOP_FILE + f"kid = subprocess.Popen([sys.executable, '-c', 'import time\\ntime.sleep(300)'])\n"
                f"open({pidfile!r}, 'w').write(str(kid.pid))\n"
                "t = time.thread_time() + 1\nwhile time.thread_time() < t: pass\n"
                "while True:\n    time.sleep(0.05)  # ignores its stop file\n")
        s = self.finish(self.run_farm(1, body, NODE_FUZZ_CERTIFY_GRACE="1"), rc=137)
        self.assertEqual((s["stopped_by"], s["killed"], s["status"]), ("cpu-window", True, -9))
        pid = int(open(pidfile).read())
        deadline = time.monotonic() + 10
        while os.path.exists(f"/proc/{pid}") and time.monotonic() < deadline:
            try:
                if open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()[0] == "Z":
                    break  # killed, and waiting for init to reap it
            except OSError:
                break
            time.sleep(0.1)
        self.assertFalse(os.path.exists(f"/proc/{pid}") and
                         open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()[0] != "Z")

    def test_a_poll_valid_for_certify_is_valid_for_a_segment(self):
        body = self.STOP_FILE
        s = self.finish(self.run_farm(600, body, NODE_FUZZ_CERTIFY_POLL="7"))
        self.assertEqual(s["status"], 0)

    def test_the_allotment_counts_from_the_start_the_corpus_load_included(self):
        # A certify window counts after INITED; a segment's budget is total CPU, so the load is in.
        body = ("t = time.thread_time() + 2\nwhile time.thread_time() < t: pass\n"
                "print('#2\\tINITED cov: 5 ft: 9', flush=True)\n"
                "STOP = next(a.split('=', 1)[1] for a in sys.argv if a.startswith('-stop_file='))\n"
                "while not os.path.exists(STOP):\n    t = time.thread_time() + 0.05\n"
                "    while time.thread_time() < t: pass\n")
        s = self.finish(self.run_farm(3, body))
        self.assertEqual(s["stopped_by"], "cpu-window")
        self.assertEqual(s["cpu_at_inited"], 0.0)
        self.assertLess(s["cpu_at_stop"], 5)  # 2 s of load and about 1 s of fuzzing, not 2 + 3

    def test_the_runs_stop_file_reaches_the_process_and_an_empty_one_does_not(self):
        body = self.STOP_FILE + ("begin = time.monotonic()\nwhile not os.path.exists(STOP) and time.monotonic() - begin < 4:\n"
                                 "    time.sleep(0.05)\n")
        run_stop = os.path.join(self.root, "stop")
        open(run_stop, "w").close()  # libFuzzer ignores an empty stop file, so does the driver
        s = self.finish(self.run_farm(600, body, run_stop=run_stop))
        self.assertEqual((s["stopped_by"], s["status"]), (None, 0))
        shutil.rmtree(self.seg)
        body = self.STOP_FILE + "while not os.path.exists(STOP):\n    time.sleep(0.05)\n"
        proc = self.run_farm(600, body, run_stop=run_stop)
        time.sleep(1)
        open(run_stop, "w").write("stop\n")
        s = self.finish(proc)
        self.assertEqual((s["stopped_by"], s["status"], s["killed"]), ("run-stop", 0, False))

    def test_a_signal_to_the_driver_alone_does_not_kill_the_segment(self):
        # The farm signals the run's process group, which reaches libFuzzer itself. The driver
        # outlives it to write the record (a killed driver would SIGKILL libFuzzer through its
        # parent-death signal, with no final stats).
        body = (self.STOP_FILE + "signal.signal(signal.SIGINT, lambda *_: (print('run interrupted', flush=True), sys.exit(72)))\n"
                "while not os.path.exists(STOP):\n    time.sleep(0.05)\n")
        proc = self.run_farm(600, body)
        deadline = time.monotonic() + 30
        log = os.path.join(self.seg, "fuzz.out")
        while not (os.path.exists(log) and "INITED" in open(log).read()) and time.monotonic() < deadline:
            time.sleep(0.05)
        proc.send_signal(signal.SIGINT)
        proc.send_signal(signal.SIGTERM)
        time.sleep(1)
        self.assertIsNone(proc.poll())
        open(self.run_stop, "w").write("stop\n")
        s = self.finish(proc)
        self.assertEqual((s["stopped_by"], s["status"]), ("run-stop", 0))
        self.assertNotIn("run interrupted", open(log).read())

    def test_the_wall_backstop_interrupts_a_segment_that_never_reaches_its_allotment(self):
        body = (self.STOP_FILE + "signal.signal(signal.SIGINT, lambda *_: sys.exit(72))\ntime.sleep(600)\n")
        s = self.finish(self.run_farm(600, body, cap=3), rc=72)
        self.assertEqual((s["stopped_by"], s["status"]), ("wall-cap", 72))


RUN_SH_VERDICT = "# The verdict. (fuzzctl's selftest runs this part"


class RunShVerdict(Tree):
    """run.sh's verdict code, run on fixture runs (no target is built)."""

    def verdict(self, run, status=0, **extra):
        text = open(fuzzctl.RUN_SH).read()
        self.assertEqual(text.count(RUN_SH_VERDICT), 1)
        script = ("set -euo pipefail\n" + text[text.index(RUN_SH_VERDICT):])
        env = {**os.environ, "run": run, "target": "kv_model", "pin": "v24.0", "cpu_hours": "0",
               "mode": "run", "status": str(status), "root": fuzzctl.ROOT, "runs": "", "flavor": "fuzz",
               "stop_grace": str(fuzzctl.STOP_GRACE), **extra}
        r = subprocess.run(["bash", "-c", script], env=env, capture_output=True, text=True)
        self.assertIn("node-fuzz target=kv_model", r.stdout, r.stderr)
        return r.returncode, json.load(open(os.path.join(run, "verdict.json")))

    def fixture(self, crashes=()):
        run = os.path.join(self.fdir, "kv_model", "runs", "r1")
        record(run, crashes=crashes)
        seg1, seg2 = (os.path.join(run, "logs", f"segment-{n}") for n in (1, 2))
        os.makedirs(seg2)
        workers = ["stat::number_of_executed_units: 100\nkv_model: sink write failed\n",
                   "stat::number_of_executed_units: 250\n"]
        for n, log in enumerate(workers):
            open(os.path.join(seg1, f"fuzz-{n}.log"), "w").write(log)
        # The -jobs parent repeats each worker's log as it exits.
        open(os.path.join(seg1, "fuzz.out"), "w").write("".join(
            f"================== Job {n} exited with exit code 0 ============\n{log}"
            for n, log in enumerate(workers)))
        open(os.path.join(seg2, "fuzz.out"), "w").write("stat::number_of_executed_units: 7\n")  # one process
        for seg in (seg1, seg2):
            open(os.path.join(seg, "cpu"), "w").write("1.0 0.0\n")
        return run

    def test_executions_and_sink_failures_are_counted_once_under_jobs(self):
        rc, v = self.verdict(self.fixture())
        self.assertEqual((v["executions"], v["sink_failures"], rc), (357, 1, 1))

    def test_slow_units_are_counted_apart_and_do_not_fail(self):
        run = self.fixture(crashes=[("slow-unit-0123", b"s")])
        os.remove(os.path.join(run, "logs", "segment-1", "fuzz-0.log"))  # no sink failure
        open(os.path.join(run, "logs", "segment-1", "fuzz.out"), "w").write("")
        rc, v = self.verdict(run)
        self.assertEqual((v["status"], v["crashes"], v["slow_units"], rc), ("pass", 0, 1, 0))
        open(os.path.join(run, "crashes", "timeout-4567"), "wb").write(b"t")
        rc, v = self.verdict(run)
        self.assertEqual((v["status"], v["crashes"], v["slow_units"], rc), ("fail", 1, 1, 1))

    def test_a_certify_run_passes_only_when_every_step_ran_as_intended(self):
        run = os.path.join(self.fdir, "kv_model", "runs", "r2")
        record(run)
        for n in (2, 3, 4):
            os.makedirs(os.path.join(run, "logs", f"segment-{n}"))
            open(os.path.join(run, "logs", f"segment-{n}", "fuzz.out"), "w").write("stat::number_of_executed_units: 7\n")
        for n in (1, 2, 3, 4):
            open(os.path.join(run, "logs", f"segment-{n}", "cpu"), "w").write("1.0 0.0\n")
        seg = os.path.join(run, "logs", fuzzctl.WINDOW)
        window = {"cpu_window": 3600, "stopped_by": "cpu-window", "killed": False, "stop_to_exit": 2.5}
        json.dump(window, open(os.path.join(seg, "window.json"), "w"))
        certify = dict(mode="certify", window="3600", wall_cap="43200", merge_status="0", replay_before="0",
                       replay_after="0")
        rc, v = self.verdict(run, 0, **certify)  # no record of the build's source
        self.assertEqual((v["status"], v["rust_dashcore"], v["rust_dashcore_dirty"]), ("pass", "-", None))
        for source in ("0123456789ab\nclean\nr1\n" + "5" * 64 + "\n",  # another run's
                       "0123456789ab\nclean\nr2\n"):  # no binary: run.sh's record before amendment 2
            open(os.path.join(run, "source"), "w").write(source)
            self.assertEqual(self.verdict(run, 0, **certify)[1]["rust_dashcore"], "-")
        open(os.path.join(run, "source"), "w").write("0123456789ab\nclean\nr2\n" + "5" * 64 + "\n")
        rc, v = self.verdict(run, 0, **certify)  # the stop file's exit
        self.assertEqual((v["status"], v["mode"], v["run"], rc), ("pass", "certify", "r2", 0))
        self.assertEqual((v["rust_dashcore"], v["rust_dashcore_dirty"]), ("0123456789ab", False))
        self.assertEqual(v["certify"], {"engine": "single", "jobs": 1, "seconds": 3600, "wall_cap": 43200,
                                        "merge_status": 0, "replay_before_status": 0, "replay_after_status": 0})
        self.assertEqual(v["executions"], 21)  # both replays and the window
        # The wall cap: SIGINT's exit, or a kill after the process outlived the selftest's shorter
        # grace (fuzzctl judges both starved); no replay follows it.
        for status, killed in ((72, False), (137, True)):
            json.dump({**window, "stopped_by": "wall-cap", "killed": killed}, open(os.path.join(seg, "window.json"), "w"))
            self.assertEqual(self.verdict(run, status, **{**certify, "replay_after": ""})[1]["status"], "pass")
        # Sol r4 B1: an exit more than 1500 s after the stop fails, whatever its status: an exit 0
        # at the stop file the kill missed, SIGINT's 72, or a kill after the constant grace (always
        # later than it). So does a window.json without the interval.
        for status, stopped, killed, late, extra in (
                (0, "cpu-window", False, 1500.001, {}), (0, "cpu-window", False, 60000.0, {}),
                (72, "wall-cap", False, 1600.0, {"replay_after": ""}), (137, "wall-cap", True, 1500.4, {"replay_after": ""}),
                (0, "cpu-window", False, None, {}), (0, "cpu-window", False, "1", {})):
            json.dump({**window, "stopped_by": stopped, "killed": killed, "stop_to_exit": late},
                      open(os.path.join(seg, "window.json"), "w"))
            rc, v = self.verdict(run, status, **{**certify, **extra})
            self.assertEqual((v["status"], rc), ("fail", 1), (status, stopped, late))
        json.dump({k: v for k, v in window.items() if k != "stop_to_exit"}, open(os.path.join(seg, "window.json"), "w"))
        self.assertEqual(self.verdict(run, 0, **certify)[1]["status"], "fail")
        json.dump({**window, "stop_to_exit": 1500.0}, open(os.path.join(seg, "window.json"), "w"))
        self.assertEqual(self.verdict(run, 0, **certify)[1]["status"], "pass")
        # A CPU window that SIGINT or a kill ended, or that was killed after exiting 0, never passes.
        for status, extra, stopped, killed in (
                (72, {}, "cpu-window", False), (0, {"merge_status": "1"}, "cpu-window", False),
                (0, {}, None, False), (0, {"replay_before": "1"}, "cpu-window", False),
                (0, {"replay_after": ""}, "cpu-window", False), (0, {"replay_after": "1"}, "cpu-window", False),
                (137, {}, "wall-cap", False), (137, {}, "cpu-window", True), (0, {}, "cpu-window", True)):
            json.dump({**window, "stopped_by": stopped, "killed": killed}, open(os.path.join(seg, "window.json"), "w"))
            rc, v = self.verdict(run, status, **{**certify, **extra})
            self.assertEqual((v["status"], rc), ("fail", 1), (status, extra, stopped, killed))


class SmokeSample(Tree):
    """BL-333: a smoke leg fuzzes from a deterministic sample of the working corpus and the public
    seeds (run.sh --corpus-sample) with the private seeds and fixed vectors in full, and leaves
    the working corpus as it was; a leg whose budget ran out while it loaded its corpus says so.
    run.sh's corpus, segment-loop and verdict code run on a stand-in for a slow oracle-linked
    libFuzzer target (no target is built)."""

    # Loads every file of its corpus directories at COST CPU-s each, records what it loaded and
    # prints INITED; then, as libFuzzer may, writes a new input into its first directory, deletes
    # one there and rewrites another in place (a link in the sample would carry that to the working
    # corpus), and fuzzes (burns CPU) until its stop file. Like libFuzzer, it reads the
    # stop file only once it has loaded its corpus.
    FUZZER = """#!/usr/bin/env python3
import json, os, sys, time
args = sys.argv[1:]
stop = next(a.split("=", 1)[1] for a in args if a.startswith("-stop_file="))
dirs = [a for a in args if not a.startswith("-")]
loaded = {d: sorted(os.listdir(d)) for d in dirs}
json.dump({"dirs": dirs, "loaded": loaded}, open(os.path.join(os.getcwd(), "loaded.json"), "w"))
for name in [n for d in dirs for n in loaded[d]]:
    t = time.thread_time() + COST
    while time.thread_time() < t:
        pass
print("#%d\\tINITED cov: 1 ft: 1 corp: 1/1b exec/s: 1 rss: 1Mb" % sum(map(len, loaded.values())), flush=True)
open(os.path.join(dirs[0], "new-input"), "wb").write(b"found while fuzzing")
if loaded[dirs[0]]:
    os.remove(os.path.join(dirs[0], loaded[dirs[0]][0]))
if len(loaded[dirs[0]]) > 1:
    with open(os.path.join(dirs[0], loaded[dirs[0]][-1]), "r+b") as f:
        f.write(b"rewritten")
while not os.path.exists(stop):
    t = time.thread_time() + 0.05
    while time.thread_time() < t:
        pass
print("Done 9 runs in 1 second(s)\\nstat::number_of_executed_units: 9", flush=True)
"""
    PRIVATE = ("seeds", "regress/fixed")  # loaded in full
    POOL = ("corpus", "public-seeds")  # sampled

    def setUp(self):
        super().setUp()
        self.tdir = os.path.join(self.fdir, "kv_model")
        for sub in self.POOL + self.PRIVATE:
            os.makedirs(os.path.join(self.tdir, sub))
        self.runs = 0

    def plant(self, sub, n, size=lambda k: 10 + k % 7, order=1):
        """`n` files in `sub`, named as libFuzzer names them (their contents' sha1), created in
        `order`."""
        for k in list(range(n))[::order]:
            data = f"{sub} {k} ".encode().ljust(size(k), b"x")
            open(os.path.join(self.tdir, sub, hashlib.sha1(data).hexdigest()), "wb").write(data)

    def snapshot(self):
        """Every file of the target's corpora: its size, mtime, inode and contents."""
        out = {}
        for sub in self.POOL + self.PRIVATE:
            for name in os.listdir(os.path.join(self.tdir, sub)):
                path = os.path.join(self.tdir, sub, name)
                st = os.lstat(path)
                with open(path, "rb") as f:
                    out[sub, name] = (st.st_size, st.st_mtime_ns, st.st_ino, f.read())
        return out

    def pool_sizes(self):
        """The sizes of the files the sample is drawn from, by name."""
        return {name: os.path.getsize(os.path.join(self.tdir, sub, name))
                for sub in self.POOL for name in os.listdir(os.path.join(self.tdir, sub))}

    def expected(self, count, cap):
        """The sample the ruling asks for: the pool in the order of its names' sha256, each file
        taken while the count and the bytes stay within their caps."""
        pool = self.pool_sizes()
        taken, used = [], 0
        for name in sorted(pool, key=lambda n: hashlib.sha256(os.fsencode(n)).digest()):
            if len(taken) < count and used + pool[name] <= cap:
                taken, used = taken + [name], used + pool[name]
        return sorted(taken)

    def smoke_run(self, sample=None, cost=0.0, cpu_hours="0.0003", grace="300"):
        """run.sh from its corpus directories to its verdict for one budgeted run on the stand-in
        (`sample`: (count, bytes) for --corpus-sample). Returns its exit status, summary line,
        verdict, run directory and what segment-1's process loaded."""
        text = open(fuzzctl.RUN_SH).read()
        start = text.index('args=("-artifact_prefix=$run/crashes/")')
        script = text[start:text.index("# A certify run (DEC-186", start)] + text[text.index(RUN_SH_VERDICT):]
        self.runs += 1
        run = os.path.join(self.tdir, "runs", f"r{self.runs}")
        for sub in ("tmp", "crashes", "divergences", "logs"):
            os.makedirs(os.path.join(run, sub))
        fake = os.path.join(self.root, "slow-fuzzer")
        open(fake, "w").write(self.FUZZER.replace("COST", repr(cost)))
        os.chmod(fake, 0o700)
        env = {**os.environ, "run": run, "tdir": self.tdir, "mode": "run", "runs": "", "jobs": "1",
               "cpu_hours": cpu_hours, "bin": fake, "pin": "v24.0", "run_id": f"r{self.runs}", "here": fuzzctl.HERE,
               "target": "kv_model", "root": fuzzctl.ROOT, "flavor": "fuzz", "stop_grace": str(fuzzctl.STOP_GRACE),
               "NODE_FUZZ_CERTIFY_GRACE": grace, "NODE_FUZZ_CERTIFY_POLL": "0.5"}
        if sample is not None:
            env.update(sample_count=str(sample[0]), sample_bytes=str(sample[1]))
        r = subprocess.run(["bash", "-c", "set -euo pipefail\n" + script], env=env, capture_output=True, text=True,
                           timeout=600)
        lines = [x for x in r.stdout.splitlines() if x.startswith("node-fuzz ")]
        self.assertEqual(len(lines), 1, r.stdout + r.stderr)
        seg = os.path.join(run, "logs", "segment-1", "loaded.json")
        return (r.returncode, lines[0], json.load(open(os.path.join(run, "verdict.json"))), run,
                json.load(open(seg)) if os.path.exists(seg) else None)

    def test_the_sample_is_deterministic_and_within_both_caps(self):
        self.plant("corpus", 60)
        self.plant("public-seeds", 20, size=lambda k: 40)
        for caps in ((16, 1 << 20), (1000, 300), (0, 1 << 20)):  # the count binds, the bytes bind, nothing
            seen = []
            for attempt in range(2):
                if attempt:  # the same names, created anew in another order and with other mtimes
                    for sub in self.POOL:
                        shutil.rmtree(os.path.join(self.tdir, sub))
                        os.makedirs(os.path.join(self.tdir, sub))
                    self.plant("corpus", 60, order=-1)
                    self.plant("public-seeds", 20, size=lambda k: 40, order=-1)
                    for k, name in enumerate(sorted(os.listdir(os.path.join(self.tdir, "corpus")))):
                        mtime = 1e9 + 7919 * k % 1000
                        os.utime(os.path.join(self.tdir, "corpus", name), (mtime, mtime))
                rc, line, v, run, loaded = self.smoke_run(sample=caps)
                self.assertEqual((rc, v["status"], v["reason"]), (0, "pass", None), line)
                self.assertEqual(loaded["dirs"][0], os.path.join(run, "sample"))
                seen.append(loaded["loaded"][loaded["dirs"][0]])
            self.assertEqual(seen[0], seen[1], caps)
            self.assertEqual(seen[0], self.expected(*caps), caps)
            sizes = self.pool_sizes()
            self.assertLessEqual(len(seen[0]), caps[0])
            self.assertLessEqual(sum(sizes[n] for n in seen[0]), caps[1])
        self.assertEqual(len(self.expected(16, 1 << 20)), 16)
        self.assertLess(len(self.expected(1000, 300)), 80)  # the byte cap bound it

    def test_a_file_that_vanishes_while_sampling_is_passed_over_and_any_name_sorts(self):
        # A farm slice of the leg may delete a corpus file between the sample's listing and its
        # copy; the rest of the sample is what it would have been without that file. A name that
        # is not UTF-8 sorts by its bytes' sha256 like any other.
        text = open(fuzzctl.RUN_SH).read()
        start = text.index("<<'EOF' || fail \"sampling the working corpus failed\"\n")
        start = text.index("\n", start) + 1
        snippet = text[start:text.index("\nEOF\n", start) + 1]
        self.plant("corpus", 12)
        open(os.path.join(os.fsencode(self.tdir), b"corpus", b"\xff\xfe-not-utf8"), "wb").write(b"odd")
        names = sorted(os.listdir(os.path.join(self.tdir, "corpus")),
                       key=lambda n: hashlib.sha256(os.fsencode(n)).digest())
        gone = names[1]
        dest = os.path.join(self.root, "sample")
        os.mkdir(dest)
        vanish = ("import builtins, os\n_open = builtins.open\n"
                  "def open(path, *a, **k):\n"
                  f"    if isinstance(path, str) and path.endswith({gone!r}):\n        os.remove(path)\n"
                  "    return _open(path, *a, **k)\n")
        r = subprocess.run([sys.executable, "-I", "-", dest, "100", str(1 << 20), os.path.join(self.tdir, "corpus"),
                            os.path.join(self.tdir, "public-seeds")], input=vanish + snippet, capture_output=True,
                           text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(sorted(os.listdir(dest)), sorted(n for n in names if n != gone))
        self.assertIn("\udcff\udcfe-not-utf8", os.listdir(dest))

    def test_private_seeds_and_fixed_vectors_are_loaded_in_full_whatever_the_caps(self):
        self.plant("corpus", 30)
        self.plant("public-seeds", 5)
        self.plant("seeds", 7)
        self.plant("regress/fixed", 3)
        for caps in ((0, 0), (2, 1 << 20)):
            rc, line, v, run, loaded = self.smoke_run(sample=caps)
            self.assertEqual(rc, 0, line)
            sample, seeds, fixed = (os.path.join(run, "sample"), *(os.path.join(self.tdir, s) for s in self.PRIVATE))
            # The sample takes the place of the working corpus and the public seeds, first.
            self.assertEqual(loaded["dirs"], [sample, seeds, fixed])
            self.assertEqual(len(loaded["loaded"][sample]), caps[0])
            self.assertEqual(loaded["loaded"][seeds], sorted(os.listdir(seeds)))
            self.assertEqual(len(loaded["loaded"][seeds]), 7)
            self.assertEqual(loaded["loaded"][fixed], sorted(os.listdir(fixed)))
            self.assertEqual(len(loaded["loaded"][fixed]), 3)

    def test_the_working_corpus_is_only_read(self):
        self.plant("corpus", 30)
        self.plant("public-seeds", 5)
        self.plant("seeds", 2)
        before = self.snapshot()
        rc, line, v, run, loaded = self.smoke_run(sample=(8, 1 << 20))
        self.assertEqual(rc, 0, line)
        # The stand-in wrote a new input into the sample and deleted one of its files: the working
        # corpus, the public seeds and the seeds are as they were (same inodes: no file was replaced),
        # and the sample is gone.
        self.assertEqual(self.snapshot(), before)
        self.assertFalse(os.path.exists(os.path.join(run, "sample")))
        # The harness sees a change: an unsampled run (the farm's) writes into the working corpus.
        rc, line, v, run, loaded = self.smoke_run()
        self.assertEqual(rc, 0, line)
        self.assertNotEqual(self.snapshot(), before)

    def test_a_slow_target_reaches_inited_on_its_sample_and_names_the_load_otherwise(self):
        # 0.05 CPU-s an input: the whole corpus takes 10 CPU-s to load, the budget is 1.08 CPU-s (an
        # allotment of 3), so the unsampled leg is still loading at its stop and is killed at the
        # (shortened) grace; its 16-input sample loads in about 1 CPU-s.
        self.plant("corpus", 190)
        self.plant("seeds", 3)
        self.plant("regress/fixed", 2)
        rc, line, v, run, loaded = self.smoke_run(cost=0.05, grace="2")
        self.assertEqual((rc, v["status"], v["libfuzzer_status"]), (1, "fail", 137), line)
        self.assertEqual(v["reason"], "budget spent before INITED (corpus load)")
        self.assertTrue(line.endswith("status=fail (budget spent before INITED (corpus load))"), line)
        rc, line, v, run, loaded = self.smoke_run(sample=(16, 1 << 20), cost=0.05, grace="2")
        self.assertEqual((rc, v["status"], v["reason"], v["libfuzzer_status"]), (0, "pass", None, 0), line)
        self.assertTrue(line.endswith("status=pass"), line)
        self.assertIn("INITED", open(os.path.join(run, "logs", "segment-1", "fuzz.out")).read())

    def test_a_run_that_reached_inited_or_was_stopped_otherwise_has_another_reason(self):
        run = os.path.join(self.tdir, "runs", "v")
        record(run, log="#5\tINITED cov: 1 ft: 1\n")
        seg = os.path.join(run, "logs", "segment-1")
        open(os.path.join(seg, "cpu"), "w").write("1.0 0.0\n")
        verdict = RunShVerdict.verdict
        for log, stopped, status, reason in (
                ("#5\tINITED cov: 1 ft: 1\n", "cpu-window", 137, "libFuzzer exited 137"),
                ("#4\tpulse  cov: 1 ft: 1\n", "run-stop", 0, "budget not met"),
                ("#4\tpulse  cov: 1 ft: 1\n", "cpu-window", 137, "budget spent before INITED (corpus load)")):
            open(os.path.join(seg, "fuzz.out"), "w").write(log)
            json.dump({"stopped_by": stopped}, open(os.path.join(seg, "segment.json"), "w"))
            rc, v = verdict(self, run, status, cpu_hours="0.01" if stopped == "run-stop" else "0")
            self.assertEqual((rc, v["status"], v["reason"]), (1, "fail", reason), (log, stopped))
        open(os.path.join(seg, "fuzz.out"), "w").write("#5\tINITED cov: 1 ft: 1\n")
        rc, v = verdict(self, run, 0)  # a pass has none
        self.assertEqual((rc, v["status"], v["reason"]), (0, "pass", None))

    def test_smoke_passes_each_targets_caps_and_counts_failures_by_reason(self):
        calls = {}
        load = "budget spent before INITED (corpus load)"
        small = dataclasses_replace(PUB, smoke_corpus_max=4, smoke_corpus_max_bytes=512)

        def runner(target, pin, args, **kw):
            calls[target, pin] = args
            if target == PRIV.name:
                return Leg(target, pin, "node-fuzz fake status=fail (" + load + ")", 1, "",
                           {"status": "fail", "reason": load, "cpu_hours": 0.01})
            return Leg(target, pin, "run.sh: the build failed", 2, "", {})

        tri = mock.Mock(stats={"errors": 0})
        tri.summary.return_value = "node-fuzz triage errors=0"
        out = io.StringIO()
        with mock.patch.object(fuzzctl, "run_leg", runner), mock.patch.object(fuzzctl, "run_triage", return_value=tri), \
                mock.patch.object(fuzzctl, "status_lines", return_value=([], 0)), contextlib.redirect_stdout(out):
            self.assertFalse(fuzzctl.smoke([PRIV, small, SELF], 2, selftest=False))
        for (target, pin), args in calls.items():
            t = PRIV if target == PRIV.name else small
            self.assertEqual(args, ["--cpu-hours", f"{t.smoke / 3600:.6f}", "--jobs", "1", "--corpus-sample",
                                    str(t.smoke_corpus_max), "--corpus-sample-bytes", str(t.smoke_corpus_max_bytes)])
        self.assertEqual(calls[PRIV.name, "v24.0"][5:], ["64", "--corpus-sample-bytes", str(4 << 20)])
        summary = [x for x in out.getvalue().splitlines() if x.startswith("node-fuzz smoke ")]
        self.assertEqual(len(summary), 1)
        self.assertTrue(summary[0].endswith(f"status=fail ({load}: 2; no verdict, exit 2: 1)"), summary[0])
        self.assertNotIn(PRIV.name, out.getvalue())  # private-route legs are counted, never named

    def test_the_farm_and_certify_run_the_whole_corpus_as_before(self):
        # The farm's slices and certify's run carry no sample, and run.sh refuses one outside a
        # fuzzing run; an unsampled run loads the working corpus first, then the seeds, the public
        # seeds and the fixed vectors, as it always has.
        runner, args = SliceRunner(), []

        def recording(target, pin, a, **kw):
            args.append(a)
            return runner(target, pin, a, **kw)

        f = fuzzctl.Farm(farm_args(), TARGETS, NoBench(), runner=recording, log=lambda *_: None)
        f.margin, f.lead, f.imminent = 0.05, 0.05, 0.05
        self.assertEqual(f.work(), "complete")
        self.assertTrue(args)
        for a in args:
            self.assertEqual(a[::2], ["--cpu-hours", "--jobs", "--run-tag"], a)
        certify_calls = []

        def certify_runner(target, pin, a, **kw):
            certify_calls.append(a)
            return Leg(target, pin, "node-fuzz fake", 1, "", {})

        with contextlib.redirect_stdout(io.StringIO()), mock.patch.object(fuzzctl.os, "nice"):
            fuzzctl.certify(certify_args("kv_model@v24.0", seconds=600), TARGETS, runner=certify_runner)
        self.assertEqual(len(certify_calls), 1)
        self.assertEqual(certify_calls[0][:6], ["--certify", "--seconds", "600", "--wall-cap", "43200", "--run-tag"])
        self.assertEqual(len(certify_calls[0]), 7)
        for mode in (["--merge"], ["--certify", "--seconds", "60"], ["--job", "job-" + "0" * 32]):
            r = subprocess.run([fuzzctl.RUN_SH, "kv_model", "--pin", "v24.0", *mode, "--corpus-sample", "4",
                                "--corpus-sample-bytes", "64"], capture_output=True, text=True)
            self.assertEqual(r.returncode, 2, mode)
            self.assertIn("--corpus-sample is for fuzzing runs", r.stderr)
        r = subprocess.run([fuzzctl.RUN_SH, "kv_model", "--pin", "v24.0", "--cpu-hours", "1", "--corpus-sample", "4"],
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 2)
        self.assertIn("come together", r.stderr)
        self.plant("corpus", 4)
        self.plant("seeds", 1)
        self.plant("regress/fixed", 1)
        rc, line, v, run, loaded = self.smoke_run()
        self.assertEqual(rc, 0, line)
        self.assertEqual(loaded["dirs"], [os.path.join(self.tdir, s) for s in ("corpus", "seeds", "public-seeds",
                                                                              "regress/fixed")])
        self.assertEqual(len(loaded["loaded"][os.path.join(self.tdir, "corpus")]), 4)


SELFTEST_CASES = """
import os, unittest

def tried(name):
    path = os.path.join(os.environ["SELFTEST_CASES_DIR"], name)
    n = os.path.getsize(path) if os.path.exists(path) else 0
    open(path, "a").write("x")
    return n


class Cases(unittest.TestCase):
    def test_passes(self):
        pass

    def test_fails_once(self):
        self.assertGreater(tried("once"), 0)

    def test_always_fails(self):
        tried("always")
        if os.environ.get("SELFTEST_CASES_ALWAYS"):
            self.fail("broken")

    def test_skips_alone(self):
        if os.environ.get("SELFTEST_CASES_SKIPS"):
            if tried("skips"):
                self.skipTest("alone")
            self.fail("in the suite")

class Fixture(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if os.environ.get("SELFTEST_CASES_FIXTURE"):
            raise RuntimeError("fixture")

    def test_in_a_broken_fixture(self):
        pass
"""


class SelftestVerdict(unittest.TestCase):
    """BL-134: the selftest's last line names what failed, and only allow-listed cases rerun."""

    def setUp(self):
        d = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, d)
        open(os.path.join(d, "selftest_cases.py"), "w").write(SELFTEST_CASES)
        sys.path.insert(0, d)
        self.addCleanup(sys.path.remove, d)
        self.addCleanup(sys.modules.pop, "selftest_cases", None)
        self.dir = d

    def selftest(self, flaky, **cases):
        out = io.StringIO()
        env = {"SELFTEST_CASES_DIR": self.dir,
               **{f"SELFTEST_CASES_{k}": "1" if cases.get(k.lower()) else "" for k in ("ALWAYS", "SKIPS", "FIXTURE")}}
        with mock.patch.dict(os.environ, env), contextlib.redirect_stdout(out), \
                contextlib.redirect_stderr(io.StringIO()):
            rc = fuzzctl.selftest("selftest_cases", {f"selftest_cases.Cases.{n}" for n in flaky})
        tries = {n: os.path.getsize(p) if os.path.exists(p := os.path.join(self.dir, n)) else 0
                 for n in ("once", "always", "skips")}
        return rc, out.getvalue().splitlines()[-1], tries

    def test_an_allow_listed_case_that_passes_alone_passes_marked_as_a_rerun(self):
        rc, line, tries = self.selftest({"test_fails_once"})
        self.assertEqual(rc, 0)
        self.assertEqual(line, "fuzzctl selftest: ok-after-rerun (5 tests, 0 skipped; "
                               "failed selftest_cases.Cases.test_fails_once, passed alone on try 1)")
        self.assertEqual(tries["once"], 2)

    def test_a_case_not_allow_listed_fails_without_a_rerun(self):
        rc, line, tries = self.selftest(set())
        self.assertEqual(rc, 1)
        self.assertEqual(line, "fuzzctl selftest: FAIL (5 tests, 0 skipped; failed selftest_cases.Cases.test_fails_once)")
        self.assertEqual(tries["once"], 1)

    def test_an_allow_listed_case_that_fails_every_try_fails(self):
        rc, line, tries = self.selftest({"test_fails_once", "test_always_fails"}, always=True)
        self.assertEqual(rc, 1)
        self.assertIn("failed selftest_cases.Cases.test_always_fails, and all 3 tries alone", line)
        self.assertIn("failed selftest_cases.Cases.test_fails_once, passed alone on try 1", line)
        self.assertEqual(tries["always"], 4)

    def test_a_skip_alone_or_a_fixture_error_is_no_pass(self):
        flaky = {"test_fails_once", "test_skips_alone", "test_in_a_broken_fixture"}
        rc, line, tries = self.selftest(flaky, skips=True, fixture=True)
        self.assertEqual(rc, 1)
        self.assertIn("failed selftest_cases.Cases.test_skips_alone, and all 3 tries alone", line)
        self.assertTrue(line.endswith("; failed setUpClass (selftest_cases.Fixture))"), line)  # no test id: no rerun
        self.assertEqual(tries["skips"], 4)


if __name__ == "__main__":
    unittest.main()
