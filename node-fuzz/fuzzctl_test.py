"""Tests of fuzzctl (O3): `python3 -I node-fuzz/fuzzctl.py selftest`.

Everything runs on synthetic trees in a fresh temporary directory, with DASH_EMBARGO_DIR pointed
at it for the duration of a test; no fuzz target is built or run.
"""

import argparse
import ast
import contextlib
import fcntl
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
import resource
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
        self.assertEqual(fuzzctl.load_registry(self.write_registry(ok))[0].features, ())
        built = fuzzctl.load_registry(self.write_registry(ok + 'features = ["chain-diff", "x_1"]\n'))
        self.assertEqual(built[0].features, ("chain-diff", "x_1"))
        self.assertEqual(fuzzctl.load_registry(self.write_registry(ok))[0].encoding, "")
        coded = fuzzctl.load_registry(self.write_registry(ok + 'encoding = "chain-v12"\n'))
        self.assertEqual(coded[0].encoding, "chain-v12")
        self.assertEqual(fuzzctl.load_registry(self.write_registry(ok))[0].under_test, ())
        scoped = fuzzctl.load_registry(self.write_registry(ok + 'under_test = ["dash-chain", "node-l1"]\n'))
        self.assertEqual(scoped[0].under_test, ("dash-chain", "node-l1"))
        for bad in (
            ok + 'features = "chain-diff"\n',
            ok + 'features = ["Chain Diff"]\n',
            ok + 'features = ["a", "a"]\n',
            ok + "features = [1]\n",
            ok + 'features = ["-x"]\n',
            # An unhashable entry is refused as malformed, not raised from the duplicate check.
            ok + 'features = [["a"]]\n',
            ok + 'encoding = ""\n',
            ok + 'encoding = "chain-v0"\n',
            ok + 'encoding = "v2"\n',
            ok + 'encoding = "chain-v2 "\n',
            ok + "encoding = 2\n",
            ok + 'encoding = ["chain-v2"]\n',
            ok + "under_test = []\n",
            ok + 'under_test = "node-l1"\n',
            ok + 'under_test = ["node-l1", "node-l1"]\n',
            ok + 'under_test = ["Node L1"]\n',
            ok + "under_test = [1]\n",
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

    def fixture_root(self):
        """A copy of the parts of the tree `check_registry` reads."""
        root = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, root)
        for d in ("node-fuzz", "node-l1/src"):
            shutil.copytree(os.path.join(fuzzctl.ROOT, d), os.path.join(root, d),
                            ignore=shutil.ignore_patterns("target"))
        shutil.copyfile(os.path.join(fuzzctl.ROOT, "node-l1", "chain-encodings.toml"),
                        os.path.join(root, "node-l1", "chain-encodings.toml"))
        # Every crate manifest (under_test's check walks node-fuzz's path dependencies, BL-812).
        for manifest in glob.glob(os.path.join(fuzzctl.ROOT, "*", "Cargo.toml")) + [
                os.path.join(fuzzctl.ROOT, "Cargo.toml")]:
            dest = os.path.join(root, os.path.relpath(manifest, fuzzctl.ROOT))
            os.makedirs(os.path.dirname(dest), exist_ok=True)
            if not os.path.exists(dest):
                shutil.copyfile(manifest, dest)
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
        return root

    def test_a_disagreeing_tree_is_reported(self):
        root = self.fixture_root()
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
        # D40: a bin's registry features are exactly its required-features, each declared.
        text = open(reg).read()
        entry = ('name = "diff_chain_s0"\nowner = "node-l1"\nkind = "l1"\npins = ["v24.0"]\nroute = "private"\n'
                 'encoding = "chain-v2"\n')
        self.assertIn(entry + 'features = ["chain-diff"]\n', text)
        open(reg, "w").write(text.replace(entry + 'features = ["chain-diff"]\n', entry))
        self.assertIn("diff_chain_s0: its features [] are not its [[bin]]'s required-features ['chain-diff']",
                      "\n".join(fuzzctl.check_registry(root)))
        open(reg, "w").write(text.replace(entry + 'features = ["chain-diff"]\n',
                                          entry + 'features = ["chain-diff", "nonesuch"]\n'))
        problems = "\n".join(fuzzctl.check_registry(root))
        self.assertIn("diff_chain_s0: feature nonesuch is not one of node-fuzz/Cargo.toml's [features]", problems)
        self.assertIn("diff_chain_s0: its features ['chain-diff', 'nonesuch'] are not its", problems)
        # D55: a chain target's public seeds are its own encoding's directory alone; a v2 target
        # pointed at the v1 seeds is reported even while the two compiles are the same bytes.
        open(reg, "w").write(text)
        self.assertNotIn("public seeds", "\n".join(fuzzctl.check_registry(root)))
        own = 'public_seeds = ["node-fuzz/public-seeds/diff_chain_s0"]\n'
        self.assertEqual(text.count(own), 1)
        open(reg, "w").write(text.replace(own, 'public_seeds = ["node-fuzz/public-seeds/diff_chain"]\n'))
        self.assertIn("diff_chain_s0: a chain target's public seeds are node-fuzz/public-seeds/diff_chain_s0 alone",
                      fuzzctl.check_registry(root))
        # Its own directory with the v1 one beside it is refused too.
        open(reg, "w").write(text.replace(own, own.replace('"]', '", "node-fuzz/public-seeds/diff_chain"]')))
        self.assertIn("diff_chain_s0: a chain target's public seeds are node-fuzz/public-seeds/diff_chain_s0 alone",
                      fuzzctl.check_registry(root))
        # And so is the v1 target pointed at a v2 target's seeds.
        v1 = 'public_seeds = ["node-fuzz/public-seeds/diff_chain"]\n'
        self.assertEqual(text.count(v1), 1)
        open(reg, "w").write(text.replace(v1, 'public_seeds = ["node-fuzz/public-seeds/diff_chain_s0"]\n'))
        self.assertIn("diff_chain: a chain target's public seeds are node-fuzz/public-seeds/diff_chain alone",
                      fuzzctl.check_registry(root))
        # O2b-4 4b: each planted search is bound to its own directory as any chain target is;
        # pointed at the unplanted target's seeds of the same encoding it is reported.
        for name in ("diff_chain_par_p3a", "diff_chain_p3b"):
            own = f'public_seeds = ["node-fuzz/public-seeds/{name}"]\n'
            self.assertEqual(text.count(own), 1, name)
            open(reg, "w").write(text.replace(own, 'public_seeds = ["node-fuzz/public-seeds/diff_chain_s0"]\n'))
            self.assertIn(f"{name}: a chain target's public seeds are node-fuzz/public-seeds/{name} alone",
                          fuzzctl.check_registry(root))
        # D41: a planted row has no budget and no certify metadata, and node-l1 knows its name.
        row = 'name = "diff_chain_p3b"\nowner = "node-l1"\nkind = "l1"\npins = ["v24.0"]\nroute = "private"\n' \
              'encoding = "chain-v2"\nfeatures = ["planted"]\nsmoke = 0\nnightly = 0\n'
        self.assertEqual(text.count(row), 1)
        for change, problem in (
            (("nightly = 0\n", "nightly = 1\n"), "diff_chain_p3b: a planted search has smoke 0 and nightly 0"),
            (("smoke = 0\n", "smoke = 60\n"), "diff_chain_p3b: a planted search has smoke 0 and nightly 0"),
            (("nightly = 0\n", "nightly = 0\nmin_certify_execs = 1000000\n"),
             "diff_chain_p3b: a planted search has no min_certify_execs"),
            (("nightly = 0\n", "nightly = 0\nreplay_cov_slack = 2\n"),
             "diff_chain_p3b: a planted search has no plateau or replay slacks"),
            (("nightly = 0\n", "nightly = 0\nplateau_cov_slack = 2\n"),
             "diff_chain_p3b: a planted search has no plateau or replay slacks"),
            (("nightly = 0\n", "nightly = 0\nreplay_ft_slack = 2\n"),
             "diff_chain_p3b: a planted search has no plateau or replay slacks"),
            (("nightly = 0\n", "nightly = 0\nplateau_ft = false\n"),
             "diff_chain_p3b: a planted search has no plateau or replay slacks"),
        ):
            open(reg, "w").write(text.replace(row, row.replace(*change)))
            self.assertIn(problem, "\n".join(fuzzctl.check_registry(root)), change)
        open(reg, "w").write(text)
        intent = os.path.join(root, "node-l1", "src", "chain", "intent.rs")
        src = open(intent).read()
        listed = '["diff_chain_par_p3a", "diff_chain_p3b"]'
        self.assertEqual(src.count(listed), 1)
        open(intent, "w").write(src.replace(listed, '["diff_chain_par_p3a", "diff_chain_s0"]'))
        problems = fuzzctl.check_registry(root)
        self.assertIn("diff_chain_p3b: built with planted but not in node-l1's PLANTED_CHAIN_TARGETS", problems)
        self.assertIn("diff_chain_s0 is in node-l1's PLANTED_CHAIN_TARGETS but not built with planted", problems)
        open(intent, "w").write(src.replace("PLANTED_CHAIN_TARGETS", "PLANTED_TARGETS"))
        self.assertIn("node-l1/src/chain/intent.rs: cannot find PLANTED_CHAIN_TARGETS", fuzzctl.check_registry(root))
        open(intent, "w").write(src)
        self.assertEqual(sorted(fuzzctl.PLANT_FIELDS), ["diff_chain_p3b", "diff_chain_par_p3a"])
        with mock.patch.dict(fuzzctl.PLANT_FIELDS, {"diff_chain_s0": "reply"}):
            self.assertIn("diff_chain_s0 is in fuzzctl's PLANT_FIELDS but is not a planted row",
                          fuzzctl.check_registry(root))
        with mock.patch.dict(fuzzctl.PLANT_FIELDS, clear=True):
            self.assertIn("diff_chain_p3b: built with planted but has no plant field in fuzzctl's PLANT_FIELDS",
                          fuzzctl.check_registry(root))
        os.remove(os.path.join(root, "node-l1", "chain-encodings.toml"))
        self.assertIn("node-l1/chain-encodings.toml is missing or unreadable", fuzzctl.check_registry(root))

    def test_a_chain_v2_bin_may_get_chain_diff_through_another_feature(self):
        # M1 (4b): a row and its bin naming only a feature that turns chain-diff on (not `planted`, which
        # node-fuzz has and whose rows D41 binds).
        root = self.fixture_root()
        here = os.path.join(root, "node-fuzz")
        reg, manifest = os.path.join(here, "targets.toml"), os.path.join(here, "Cargo.toml")
        text, cargo = open(reg).read(), open(manifest).read()
        row = ('name = "diff_chain_par"\nowner = "node-l1"\nkind = "l1"\npins = ["v24.0"]\nroute = "private"\n'
               'encoding = "chain-v2"\nfeatures = ["chain-diff"]\n')
        bin_entry = 'name = "diff_chain_par"\npath = "fuzz_targets/diff_chain_par.rs"\nrequired-features = ["chain-diff"]\n'
        self.assertEqual((text.count(row), cargo.count(bin_entry)), (1, 1))
        open(reg, "w").write(text.replace(row, row.replace('["chain-diff"]', '["outer"]')))
        for outer, problems in (('["chain-diff", "node-l1/sut"]', []),
                                ('["inner"]\ninner = ["chain-diff"]', []),
                                ('["node-l1/sut", "dep:node-l1"]',
                                 ["diff_chain_par: a chain-v2 target is built with the chain-diff feature"])):
            with self.subTest(outer=outer):
                open(manifest, "w").write(
                    cargo.replace(bin_entry, bin_entry.replace('["chain-diff"]', '["outer"]'))
                    .replace('chain-diff = ["node-l1/sut"]\n', f'chain-diff = ["node-l1/sut"]\nouter = {outer}\n'))
                self.assertEqual(fuzzctl.check_registry(root), problems)
        enables = {"a": ["b", "x/y", "dep:z"], "b": ["a", "c"], "c": [], "d": ["e"]}
        self.assertEqual(fuzzctl.feature_closure(["a"], enables), {"a", "b", "c"})
        self.assertEqual(fuzzctl.feature_closure([], enables), set())
        self.assertEqual(fuzzctl.feature_closure(["q"], {"q": "not a list"}), {"q"})

    def test_check_holds_encoding_equal_to_chain_encodings_toml(self):
        root = self.fixture_root()
        here = os.path.join(root, "node-fuzz")
        reg = os.path.join(here, "targets.toml")
        text = open(reg).read()
        # §3a.3: every chain target's row names chain-encodings.toml's encoding for it.
        coded = 'name = "diff_chain_par"\nowner = "node-l1"\nkind = "l1"\npins = ["v24.0"]\nroute = "private"\n'
        self.assertEqual(text.count(coded + 'encoding = "chain-v2"\n'), 1)
        # (A row that is not chain-v2 may not keep its under_test either, BL-812.)
        not_diff = ("diff_chain_par: under_test is for the chain Diff rows (chain-v2) and the selftest; "
                    "this l1 row's system under test is Core")
        open(reg, "w").write(text.replace(coded + 'encoding = "chain-v2"\n', coded))
        self.assertEqual(fuzzctl.check_registry(root),
                         ["diff_chain_par: its encoding '' is not node-l1/chain-encodings.toml's 'chain-v2'", not_diff])
        open(reg, "w").write(text.replace(coded + 'encoding = "chain-v2"\n', coded + 'encoding = "chain-v1"\n'))
        self.assertEqual(fuzzctl.check_registry(root),
                         ["diff_chain_par: its encoding 'chain-v1' is not node-l1/chain-encodings.toml's 'chain-v2'",
                          not_diff])
        # D40 with D55: a chain-v2 row and its bin that drop chain-diff together agree with each
        # other, and are still refused.
        open(reg, "w").write(text.replace(coded + 'encoding = "chain-v2"\nfeatures = ["chain-diff"]\n',
                                          coded + 'encoding = "chain-v2"\n'))
        manifest = os.path.join(here, "Cargo.toml")
        cargo = open(manifest).read()
        bin_entry = 'name = "diff_chain_par"\npath = "fuzz_targets/diff_chain_par.rs"\n'
        self.assertEqual(cargo.count(bin_entry + 'required-features = ["chain-diff"]\n'), 1)
        open(manifest, "w").write(cargo.replace(bin_entry + 'required-features = ["chain-diff"]\n', bin_entry))
        self.assertEqual(fuzzctl.check_registry(root),
                         ["diff_chain_par: a chain-v2 target is built with the chain-diff feature"])
        open(manifest, "w").write(cargo)
        # A name in chain-encodings.toml with no row, and a chain row with no name there.
        encodings = os.path.join(root, "node-l1", "chain-encodings.toml")
        listed = open(encodings).read()
        open(reg, "w").write(text.replace(coded, coded.replace("diff_chain_par", "diff_chain_parx")))
        problems = fuzzctl.check_registry(root)
        self.assertIn("diff_chain_par is in node-l1/chain-encodings.toml but not in targets.toml", problems)
        self.assertIn("diff_chain_parx is a chain target in targets.toml but not in node-l1/chain-encodings.toml",
                      problems)
        open(reg, "w").write(text.replace('name = "kv_model"\n', 'name = "kv_model"\nencoding = "chain-v2"\n', 1))
        self.assertEqual(fuzzctl.check_registry(root),
                         ["kv_model is a chain target in targets.toml but not in node-l1/chain-encodings.toml"])
        self.assertEqual(open(encodings).read(), listed)
        open(reg, "w").write(text)
        self.assertEqual(fuzzctl.check_registry(root), [])

    def test_under_test_names_node_fuzz_s_path_crates_but_never_the_core_side(self):
        """BL-812: each under_test package is one of node-fuzz's path crates and never
        dash-oracle-sys; `under-test` prints their rustc crate names."""
        crates = fuzzctl.path_crates()
        for name in ("node-fuzz", "node-l1", "dash-chain", "dashd-node", "dashd-chainstate", "dash-oracle-sys"):
            self.assertIn(name, crates)
        self.assertEqual(crates["node-l1"], "node_l1")
        self.assertNotIn("dash-spv", crates)  # a workspace crate node-fuzz does not reach
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            self.assertEqual(fuzzctl.cli(["under-test", "diff_chain_s0"]), 0)
        self.assertIn("dash_chain", out.getvalue().split())
        self.assertNotIn("dash_oracle_sys", out.getvalue().split())
        root = self.fixture_root()
        # node-fuzz's Cargo.lock decides, not the manifests: dash-chain, still reached through a path
        # dependency, is not listed once the lock has no path entry for it (none, or a registry's).
        lock = os.path.join(root, "node-fuzz", "Cargo.lock")
        locked = open(lock).read()
        self.assertIn("dash-chain", fuzzctl.path_crates(root))
        entry = re.search(r'\[\[package\]\]\nname = "dash-chain"\n.*?(?=\n\[\[package\]\]|\Z)', locked, re.S).group(0)
        sourced = entry.replace("\n", '\nsource = "registry+https://github.com/rust-lang/crates.io-index"\n', 1)
        for edited in (locked.replace(entry, ""), locked.replace(entry, sourced)):
            open(lock, "w").write(edited)
            self.assertNotIn("dash-chain", fuzzctl.path_crates(root))
        open(lock, "w").write(locked)
        reg = os.path.join(root, "node-fuzz", "targets.toml")
        text = open(reg).read()
        entry = 'under_test = ["node-sink"]\n'
        self.assertIn(entry, text)
        for bad, why in (('under_test = ["dash-spv"]\n', "dash-spv, which is not one of node-fuzz's path crates"),
                         ('under_test = ["dash-oracle-sys"]\n', "dash-oracle-sys, the Core side")):
            open(reg, "w").write(text.replace(entry, bad))
            self.assertIn(f"selftest_mismatch: under_test names {why}", "\n".join(fuzzctl.check_registry(root)))
        # A row whose system under test is Core: diff_chain (CoreOnly, chain-v1) and a model row.
        for name, kind in (("diff_chain", "l1"), ("kv_model", "model")):
            head = f'name = "{name}"\n'
            self.assertEqual(text.count(head), 1)
            open(reg, "w").write(text.replace(head, head + 'under_test = ["node-sink"]\n'))
            self.assertIn(f"{name}: under_test is for the chain Diff rows (chain-v2) and the selftest; "
                          f"this {kind} row's system under test is Core", fuzzctl.check_registry(root))
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            self.assertEqual(fuzzctl.cli(["under-test", "selftest_mismatch"]), 0)
        self.assertEqual(out.getvalue(), "node_sink\n")
        with contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(fuzzctl.cli(["under-test", "diff_tx_decode"]), 4)
            self.assertEqual(fuzzctl.cli(["under-test", "no_such_target"]), 4)

    def test_run_sh_checks_the_build_s_identity_under_the_lock_before_cargo(self):
        """BL-812 (OD-L3-127 F2): run.sh takes the build lock, snapshots the tree, reads the
        under_test list and sancov-rustc again (same_build), then builds; once each, in that order."""
        text = open(fuzzctl.RUN_SH).read()
        main = text[text.index('\ntarget="${1:-}"'):]  # past the selftest
        marks = ['\nexec 9>>"$here/target/.node-fuzz-build.lock"\n', "\nflock -x 9\n", '\nbefore="$(tree_state)"\n',
                 '\nsame_build "$under_test:$target_dir"\n', "cargo build --release"]
        self.assertEqual([main.count(m) for m in marks], [1] * len(marks))
        at = [main.index(m) for m in marks]
        self.assertEqual(at, sorted(at))

    def test_under_test_exits_5_on_a_list_the_check_refuses(self):
        """BL-812: `under-test` fails (5) on a list `fuzzctl check` refuses, naming the problem;
        run.sh then refuses the build as not usable."""
        root = self.fixture_root()
        reg = os.path.join(root, "node-fuzz", "targets.toml")
        text = open(reg).read()
        open(reg, "w").write(text.replace('under_test = ["node-sink"]\n', 'under_test = ["dash-spv"]\n', 1))
        load, err = fuzzctl.load_registry, io.StringIO()
        with mock.patch.object(fuzzctl, "load_registry", lambda *a: load(reg)), contextlib.redirect_stderr(err), \
                contextlib.redirect_stdout(io.StringIO()) as out:
            self.assertEqual(fuzzctl.cli(["under-test", "selftest_mismatch"]), 5)
        self.assertIn("dash-spv, which is not one of node-fuzz's path crates", err.getvalue())
        self.assertEqual(out.getvalue(), "")

    def test_the_check_says_when_node_fuzz_s_path_dependencies_cannot_be_read(self):
        """BL-812: a path crate's manifest that is missing or not TOML is a problem of its own."""
        for broken in (os.remove, lambda m: open(m, "w").write("[")):
            root = self.fixture_root()
            broken(os.path.join(root, "dash-chain", "Cargo.toml"))
            self.assertIn("node-fuzz's path dependencies cannot be read", fuzzctl.check_registry(root))

    def test_run_sh_reads_a_target_s_features_and_seeds_from_the_registry(self):
        """run.sh's registry unit, its snippet and the split of its output, run as run.sh runs it
        (D40, D55): the features line, the encoding line, then the seed directories; exit 5 on
        malformed features, 6 on a malformed encoding."""
        lines = open(os.path.join(fuzzctl.HERE, "run.sh")).read().splitlines()
        first = [i for i, l in enumerate(lines) if l.startswith('registry="$(python3 -I - ')]
        last = [i for i, l in enumerate(lines) if l.startswith('encoding="$(sed -n 2p ')]
        self.assertEqual((len(first), len(last)), (1, 1))
        self.assertLess(first[0], last[0])
        unit = "\n".join(lines[first[0]:last[0] + 1])
        script = ('set -u\nhere=$1 target=$2 pin=$3\nfail() { printf "fail: %s\\n" "$*"; exit 9; }\n'
                  + unit + '\nprintf "features=<%s>\\nencoding=<%s>\\nseeds=<%s>\\n" "$registry_features" "$encoding" '
                  '"$registry_seeds"\n')
        here = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, here)
        row = '[[target]]\nname = "t"\nowner = "node-l1"\nkind = "l1"\npins = ["v24.0"]\nroute = "private"\n'

        def run(extra):
            with open(os.path.join(here, "targets.toml"), "w") as f:
                f.write(row + extra)
            r = subprocess.run(["bash", "-c", script, "unit", here, "t", "v24.0"],
                               capture_output=True, text=True)
            return r.returncode, r.stdout

        seeds = 'public_seeds = ["node-fuzz/public-seeds/a", "node-fuzz/public-seeds/b"]\n'
        self.assertEqual(run('features = ["chain-diff"]\n' + seeds),
                         (0, "features=<chain-diff>\nencoding=<>\nseeds=<node-fuzz/public-seeds/a\nnode-fuzz/public-seeds/b>\n"))
        self.assertEqual(run(seeds),
                         (0, "features=<>\nencoding=<>\nseeds=<node-fuzz/public-seeds/a\nnode-fuzz/public-seeds/b>\n"))
        self.assertEqual(run('features = ["chain-diff", "x_1"]\n'),
                         (0, "features=<chain-diff,x_1>\nencoding=<>\nseeds=<>\n"))
        self.assertEqual(run('encoding = "chain-v2"\nfeatures = ["chain-diff"]\n' + seeds),
                         (0, "features=<chain-diff>\nencoding=<chain-v2>\nseeds=<node-fuzz/public-seeds/a\n"
                             "node-fuzz/public-seeds/b>\n"))
        malformed = "fail: t's features in node-fuzz/targets.toml are not a list of distinct cargo feature names\n"
        for bad in ('features = "chain-diff"\n', 'features = ["a", "a"]\n', 'features = ["-x"]\n',
                    'features = ["Chain"]\n', "features = [1]\n", 'features = [["a"]]\n'):
            self.assertEqual(run(bad + seeds), (9, malformed), bad)
        malformed = "fail: t's encoding in node-fuzz/targets.toml is not a chain encoding\n"
        for bad in ('encoding = ""\n', 'encoding = "chain-v0"\n', 'encoding = "v2"\n', "encoding = 2\n",
                    'encoding = "chain-v2\\nx"\n'):
            self.assertEqual(run(bad + seeds), (9, malformed), bad)

    def test_legs_follow_the_budgets(self):
        self.assertEqual([(t.name, p) for t, p in fuzzctl.legs(TARGETS, "smoke")],
                         [("diff_tx_decode", "v25.0"), ("diff_tx_decode", "v24.0"), ("kv_model", "v24.0")])

    def test_a_planted_row_is_never_a_leg_whatever_its_budget(self):
        # D41: a timer's `farm work` plans without `check_registry`, so a planted row that a
        # later commit gives a budget still farms and smokes nothing.
        bad = Target("diff_chain_p3b", "l1", ("v24.0",), "private", 60, 1, features=(fuzzctl.PLANTED,))
        for budget in ("smoke", "nightly"):
            self.assertEqual([(t.name, p) for t, p in fuzzctl.legs([bad, PUB], budget)], [("kv_model", "v24.0")])
        self.assertEqual(fuzzctl.plan_legs([bad, PUB], 10), {"kv_model@v24.0": 10})


P3B = Target("diff_chain_p3b", "l1", ("v24.0",), "private", 0, 0, features=(fuzzctl.PLANTED,))


def put_cluster(fdir, kind, klass, hits=1, interrupts=0, vector=True, state="open"):
    """Files one cluster in P3B's triage state, with its vector and provenance when `vector`."""
    cid = fuzzctl.cluster_id(P3B.name, kind, klass)
    c = {"kind": kind, "class": klass, "pins": ["v24.0"], "hits": hits, "first_seen": "r1",
         "state": state, "minimized": False, "attempts": 0, "interrupts": interrupts, "vector": None}
    if state == "fixed":
        c.update(fixed_stamp="abc", fixed_at="r2")
    if vector:
        c["vector"] = f"{cid}-1"
        sub = "open" if state == "open" else "fixed"
        for d in (sub, "meta"):
            os.makedirs(os.path.join(fdir, P3B.name, "regress", d), exist_ok=True)
        open(os.path.join(fdir, P3B.name, "regress", sub, c["vector"]), "wb").write(b"in")
        json.dump({"cluster": cid, "target": P3B.name},
                  open(os.path.join(fdir, P3B.name, "regress", "meta", c["vector"] + ".json"), "w"))
    path = fuzzctl.triage_path(fdir, P3B.name)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    state_ = json.load(open(path)) if os.path.exists(path) else {"clusters": {}, "runs": [], "run_errors": {}}
    state_["clusters"][cid] = c
    json.dump(state_, open(path, "w"))
    return cid, c


class PlantedStatus(Tree):
    """D41, PLAN §3: a planted search counts as found only on an open divergence at its plant
    field (`post.persisted` for P3b) with its input kept and a hit that is not a suspected
    interrupt. Every other cluster counts as any target's."""

    def put(self, kind, klass, hits=1, interrupts=0, vector=True, state="open"):
        return put_cluster(self.fdir, kind, klass, hits, interrupts, vector, state)

    def status(self):
        lines, errors = fuzzctl.status_lines(self.fdir, [P3B, PUB, SELF])
        planted = [x for x in lines if x.startswith("SW-FUZZ planted searches")]
        private = "private 1" in lines[0] and any(x.startswith("TASK fuzz-private: 1 ") for x in lines)
        return planted, private, errors, lines

    def test_a_timeout_alone_is_not_the_plant(self):
        for interrupts in (0, 1):
            with self.subTest(labelled=bool(interrupts)):
                shutil.rmtree(os.path.join(self.fdir, P3B.name), True)
                self.put("timeout", "timeout after N seconds", interrupts=interrupts)
                planted, private, errors, lines = self.status()
                self.assertEqual((planted, private, errors), (["SW-FUZZ planted searches: 0/1 found"], True, 0),
                                 lines)

    def test_a_sink_failure_is_not_the_plant(self):
        self.put("sink", "sink write failed", vector=False)
        planted, private, errors, lines = self.status()
        self.assertEqual((planted, private, errors), (["SW-FUZZ planted searches: 0/1 found"], True, 0), lines)

    def test_a_kept_divergence_at_the_plant_field_is_the_plant(self):
        self.put("divergence", "post.persisted")
        planted, private, errors, lines = self.status()
        self.assertEqual((planted, private, errors), (["SW-FUZZ planted searches: 1/1 found"], False, 0), lines)
        self.assertEqual(lines[0], "SW-FUZZ open findings: 0 (private 0, public 0); fixed 0")
        self.assertNotIn("TASK fuzz-private", "\n".join(lines))
        # Its interrupt-labelled-only twin is not found.
        shutil.rmtree(os.path.join(self.fdir, P3B.name), True)
        self.put("divergence", "post.persisted", hits=2, interrupts=2)
        self.assertEqual(self.status()[:2], (["SW-FUZZ planted searches: 0/1 found"], True))

    def test_the_plant_field_without_its_input_is_a_triage_error(self):
        self.put("divergence", "post.persisted", vector=False)
        planted, _, errors, lines = self.status()
        self.assertEqual((planted, errors), (["SW-FUZZ planted searches: UNKNOWN"], 1), lines)
        self.assertTrue(lines[0].startswith("SW-FUZZ open findings: UNKNOWN (triage errors: 1)"), lines)

    def test_a_fixed_plant_field_cluster_is_not_the_plant(self):
        self.put("divergence", "post.persisted", state="fixed")
        planted, private, errors, lines = self.status()
        self.assertEqual((planted, private, errors), (["SW-FUZZ planted searches: 0/1 found"], False, 0), lines)
        self.assertEqual(lines[0], "SW-FUZZ open findings: 0 (private 0, public 0); fixed 1")

    def test_a_plant_whose_vector_file_is_lost_is_not_found(self):
        cid, c = self.put("divergence", "post.persisted")
        os.remove(os.path.join(self.fdir, P3B.name, "regress", "open", c["vector"]))
        planted, _, errors, lines = self.status()
        self.assertEqual((planted, errors), (["SW-FUZZ planted searches: UNKNOWN"], 1), lines)

    def test_runs_not_triaged_make_the_planted_line_unknown(self):
        self.put("timeout", "timeout after N seconds")
        path = fuzzctl.triage_path(self.fdir, P3B.name)
        state = json.load(open(path))
        state["run_errors"] = {"20261108T000000Z-v24.0-1-x": 3}
        json.dump(state, open(path, "w"))
        planted, _, errors, lines = self.status()
        self.assertEqual((planted, errors), (["SW-FUZZ planted searches: UNKNOWN"], 1), lines)
        # Once the plant is found, the untriaged run cannot hide it.
        self.put("divergence", "post.persisted")
        self.assertEqual(self.status()[0], ["SW-FUZZ planted searches: 1/1 found"])

    def test_a_divergence_at_another_field_is_not_the_plant(self):
        self.put("divergence", "reply")
        self.put("divergence", "post.persisted")
        planted, private, errors, lines = self.status()
        self.assertEqual((planted, private, errors), (["SW-FUZZ planted searches: 1/1 found"], True, 0), lines)
        shutil.rmtree(os.path.join(self.fdir, P3B.name), True)
        self.put("divergence", "reply")
        self.assertEqual(self.status()[:3], (["SW-FUZZ planted searches: 0/1 found"], True, 0))

    def test_an_unreadable_planted_state_makes_the_planted_line_unknown(self):
        p3a = Target("diff_chain_par_p3a", "l1", ("v24.0",), "private", 0, 0, features=(fuzzctl.PLANTED,))
        self.put("divergence", "post.persisted")
        os.makedirs(os.path.join(self.fdir, p3a.name))
        open(fuzzctl.triage_path(self.fdir, p3a.name), "w").write("{broken")
        lines, errors = fuzzctl.status_lines(self.fdir, [P3B, p3a, PUB, SELF])
        self.assertEqual((lines[1], errors), ("SW-FUZZ planted searches: UNKNOWN", 1))


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
        # O2b-4 D41: the planted line is additive, and a planted search's clusters that are not
        # its plant found (here: no plant field at all) stay open; the other lines keep their format.
        planted = dataclasses_replace(PRIV, features=(fuzzctl.PLANTED,))
        idle = Target("diff_chain_p3b", "l1", ("v24.0",), "private", 0, 0, features=(fuzzctl.PLANTED,))
        lines, errors = fuzzctl.status_lines(self.fdir, [planted, idle, PUB, SELF])
        self.assertEqual((lines[0], errors), ("SW-FUZZ open findings: 3 (private 2, public 1); fixed 0", 0))
        self.assertEqual(lines[1], "SW-FUZZ planted searches: 0/2 found")
        self.assertIn("TASK fuzz-private: 2 open", "\n".join(lines))
        self.assertNotIn("planted searches", "\n".join(fuzzctl.status_lines(self.fdir, TARGETS)[0]))

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

    def stale_live_run(self):
        """A stale run without a verdict whose mode file run.sh still holds (a long minimisation)."""
        run = self.make_run(PRIV.name, "20261009T000000Z-v25.0-9", pin="v25.0", divergences=[(b"abcdef", "size")])
        os.remove(os.path.join(run, "verdict.json"))
        open(os.path.join(run, "mode"), "w").write("minimize-corpus\n")
        os.link(os.path.join(run, "mode"), os.path.join(run, "live"))
        old = time.time() - 2 * fuzzctl.Triage.stale_s
        os.utime(run, (old, old))
        fd = os.open(os.path.join(run, "mode"), os.O_RDONLY)
        fcntl.flock(fd, fcntl.LOCK_EX)
        return run, fd

    def runs_recorded(self):
        path = os.path.join(self.fdir, PRIV.name, "triage.json")
        return json.load(open(path))["runs"] if os.path.exists(path) else []

    def test_a_running_run_without_a_verdict_is_never_triaged_however_old(self):
        run, fd = self.stale_live_run()
        self.addCleanup(os.close, fd)
        self.assertEqual(self.triage(FakeRunner(self)).stats["runs"], 0)
        self.assertNotIn(os.path.basename(run), self.runs_recorded())

    def test_a_run_whose_lock_is_free_is_triaged_at_once(self):
        run, fd = self.stale_live_run()
        self.assertEqual(self.triage(FakeRunner(self)).stats["runs"], 0)
        os.close(fd)
        os.utime(run)  # fresh: no wait for stale_s once the lock is free
        self.assertEqual(self.triage(FakeRunner(self)).stats["runs"], 1)
        self.assertIn(os.path.basename(run), self.runs_recorded())
        self.assertEqual(len(self.state(PRIV.name)["clusters"]), 1)

    def test_a_run_from_an_older_run_sh_waits_for_stale_s(self):
        # An unlocked mode and no live: an older run.sh, which held no lock, may still be running.
        run = self.make_run(PRIV.name, "20261009T000000Z-v25.0-9", pin="v25.0", divergences=[(b"abcdef", "size")])
        os.remove(os.path.join(run, "verdict.json"))
        open(os.path.join(run, "mode"), "w").write("run\n")
        self.assertIsNone(fuzzctl.run_live(run))
        self.assertEqual(self.triage(FakeRunner(self)).stats["runs"], 0)
        self.assertNotIn(os.path.basename(run), self.runs_recorded())
        old = time.time() - 2 * fuzzctl.Triage.stale_s
        os.utime(run, (old, old))
        self.assertEqual(self.triage(FakeRunner(self)).stats["runs"], 1)
        self.assertIn(os.path.basename(run), self.runs_recorded())

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

    def test_a_leg_never_inherits_an_opt_in_build(self):
        """BL-812: run.sh sees neither NODE_FUZZ_COVERAGE nor NODE_FUZZ_TRACE_CMP from fuzzctl's
        environment, so a leg is always the default build."""
        script = os.path.join(self.root, "run.sh")
        with open(script, "w") as f:
            f.write("""#!/bin/bash
run="$DASH_EMBARGO_DIR/fuzz/$1/runs/$(date -u +%Y%m%dT%H%M%SZ)-$3-$$"
mkdir -p "$run"
: >"$run/built"
echo '{"mode": "replay"}' >"$run/verdict.json"
echo "node-fuzz fake ${NODE_FUZZ_COVERAGE-unset} ${NODE_FUZZ_TRACE_CMP-unset}"
""")
        os.chmod(script, 0o700)
        with mock.patch.object(fuzzctl, "RUN_SH", script), \
                mock.patch.dict(os.environ, {"NODE_FUZZ_COVERAGE": "under-test", "NODE_FUZZ_TRACE_CMP": "0"}):
            leg = fuzzctl.run_leg(PUB.name, "v24.0", [], timeout=30)
        self.assertEqual((leg.code, leg.line), (0, "node-fuzz fake unset unset"))

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
                    only=None, max_slice_minutes=360, replay_target=0.20, cmin=False, cmin_growth=0.25)
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

    def test_an_only_launch_is_credited_by_its_launch_shares(self):
        # BL-826: under --only the selected legs share the whole cap (`launch_shares`), and the
        # summary credits each by that share, not by its share of the plan among every farmed leg.
        f, logs = self.farm(SliceRunner(), only=["kv_model"])
        self.assertEqual(f.work(), "complete")
        self.assertLess(sum(f.plan["legs"][leg] for leg in f.selected), 3)
        self.assertAlmostEqual(sum(fuzzctl.credit_by_leg(fuzzctl.load_records(f.dir)).values()), 4, places=3)
        self.assertIn(" credited_cpu_hours=4.00/4 ", logs[-1])

    def test_a_launch_of_every_leg_prints_the_plan_s_own_credit(self):
        # BL-820 (OD-L3-129 F2): without --only each leg is capped by its plan share, out of the
        # plan's cap. plan_legs rounds the shares to 4 dp; launch_shares renormalises them to the
        # cap, which reads 0.01 off on some nights: the first legs in name order spent past their
        # shares, as many as it takes for the two to differ (40 at 4 CPU-h: 1.50 against 1.49).
        f = fuzzctl.Farm(farm_args(hours=1.0, cores=4), fuzzctl.load_registry(), NoBench(),
                         runner=SliceRunner(), log=lambda _: None)
        legs, cap = f.plan["legs"], f.plan["cpu_hours"]
        shares = fuzzctl.launch_shares(legs, cap, f.selected)

        def lines(k):
            credit = {leg: legs[leg] * 1.01 for leg in sorted(legs)[:k]}
            return (credit, f"{sum(min(c, legs[leg]) for leg, c in credit.items()):.2f}/{cap:g}",
                    f"{sum(min(credit.get(leg, 0.0), s) for leg, s in shares.items()):.2f}/{sum(shares.values()):g}")
        credit, own, launch = next((x for x in map(lines, range(1, len(legs) + 1)) if x[1] != x[2]), (None, None, None))
        self.assertIsNotNone(credit, "no night in the registry where the two lines differ")
        with mock.patch.object(fuzzctl, "credit_by_leg", return_value=credit):
            self.assertIn(f" credited_cpu_hours={own} ", f.summary("complete"))

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

    def test_the_farm_never_triages_a_planted_search(self):
        # D41: a planted search's triage and recheck run a planted build, so they are the
        # manager's O2B4-RUN (`fuzzctl triage --only`), never the farm's.
        put_cluster(self.fdir, "divergence", "post.persisted")
        self.make_run(P3B.name, "p1", divergences=[(b"plant-again", "post.persisted")])
        self.make_run(PUB.name, "k1", crashes=[("crash-aaa", b"kv-crash")], log=PANIC_LOG)
        runner = FakeRunner(self)
        f = fuzzctl.Farm(farm_args(), TARGETS + [P3B], NoBench(), runner=runner, log=lambda _: None)
        f.triage(recheck=True)
        self.assertTrue(any(c[0] == PUB.name for c in runner.calls), runner.calls)
        self.assertEqual([c for c in runner.calls if c[0] == P3B.name], [])
        self.assertEqual(self.state(P3B.name)["runs"], [])
        lines, _ = fuzzctl.status_lines(self.fdir, [P3B, PUB, SELF])
        self.assertIn("SW-FUZZ planted searches: 1/1 found", lines)

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

    def test_shares_split_the_cap_among_the_launches_legs(self):
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
        self.assertEqual(self.judged(verdict={"coverage": "under-test"}), "uncertified")  # BL-812
        self.assertEqual(self.judged(verdict={"rust_trace_cmp": False}), "uncertified")
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
        # O2b-4 D41: a planted search is never certified, launched or not; refused before any
        # triage or run.
        planted = [dataclasses_replace(PUB, features=(fuzzctl.PLANTED,)), PRIV, SELF]
        for launch in (False, True):
            err = io.StringIO()
            with mock.patch.object(fuzzctl, "run_triage") as triage, contextlib.redirect_stderr(err), \
                    self.assertRaises(SystemExit, msg=launch):
                fuzzctl.certify(certify_args("kv_model@v24.0", launch=launch), planted, runner=None)
            triage.assert_not_called()
            self.assertIn("certify refuses a planted search (O2b-4 D41)", err.getvalue())

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

    def test_a_planted_rows_certify_run_is_no_leg(self):
        # D41: run.sh can still be handed --certify for a planted row; its run never enters
        # certify_line's denominator.
        self.certify_run(certificate=True)
        self.assertIn("kv_model@v24.0", self.certs())
        planted = [dataclasses_replace(PUB, features=(fuzzctl.PLANTED,)), PRIV, SELF]
        self.assertNotIn("kv_model@v24.0", fuzzctl.certificates(self.fdir, planted))

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
                  ('python3 -I "$here/window_driver.py" ${encoding:+--encoding "$encoding"} --farm "$run/stop" "$seg" \\\n'
                   '            "$cpu_seconds" "$((segment_backstop + 3600))" \\\n'
                   '            "$run/tmp" "$target" "$pin" "$run_id" "$bin" "${segment_args[@]}" "${inputs[@]}" || status=$?',
                   '(cd "$seg" && /usr/bin/time -f \'%U %S\' -o "$seg/cpu" "$bin" "${segment_args[@]}" "${inputs[@]}" >"$seg/fuzz.out" 2>&1) || status=$?')]
        cpu, segs, problems = self.run_scenario(0.1, patch=mutant)
        self.assertIn("the segment was not stopped on its CPU time", problems)
        self.assertTrue(len(problems) > 1, (segs, cpu, problems))


class FarmSegmentDriver(Tree):
    """window_driver.py --farm (BL-228): a segment's process tree ends at its CPU allotment."""

    def run_farm(self, allotment, body, cap=600, run_stop=None, patch=(), flags=(), session=False, **env):
        fake = os.path.join(self.root, "fake-fuzzer")
        open(fake, "w").write("#!/usr/bin/env python3\nimport os, signal, subprocess, sys, time\n" + body)
        os.chmod(fake, 0o700)
        self.seg = os.path.join(self.root, "seg")
        os.mkdir(self.seg)
        self.run_stop = run_stop or os.path.join(self.root, "stop")
        argv = [sys.executable, "-I", driver_script(self.root, patch), "--farm", self.run_stop, self.seg, str(allotment),
                str(cap), self.root, "kv_model", "v24.0", "run", fake, f"-stop_file={self.seg}/stop", *flags, "corpus"]
        return subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, start_new_session=session,
                                env={**os.environ, "NODE_FUZZ_CERTIFY_POLL": "0.2", "NODE_FUZZ_CERTIFY_GRACE": "300",
                                     "NODE_FUZZ_ORPHAN_WAIT": "1", **env})

    def finish(self, proc, rc=0):
        out, err = proc.communicate(timeout=300)
        self.assertEqual(proc.returncode, rc, err)
        s = json.load(open(os.path.join(self.seg, "segment.json")))
        # Checked after the test's own assertions, which say more about what went wrong.
        self.addCleanup(self.check_record, s)
        return s

    def check_record(self, s):
        self.assertEqual(set(s), set(fuzzctl.SEGMENT_KEYS))
        self.assertTrue(all(ok(s[k]) for k, ok in fuzzctl.SEGMENT_KEYS.items()), s)

    # libFuzzer's -jobs (FuzzerDriver.cpp WorkerThread): each job is `system("<argv> >fuzz-C.log 2>&1")`
    # from a thread of its own, and its log is copied to the parent's output when the job exits. The
    # parent prints no INITED. WORKER is the job's body (C is its number), PARENT what the parent does
    # while its jobs run.
    JOBS = ("STOP = next(a.split('=', 1)[1] for a in sys.argv if a.startswith('-stop_file='))\n"
            "if '--worker' not in sys.argv:\n"
            "    import threading\n"
            "    n = int(next(a.split('=', 1)[1] for a in sys.argv if a.startswith('-jobs=')))\n"
            "    base = [a for a in sys.argv[1:] if not a.startswith(('-jobs=', '-workers='))]\n"
            "    def job(c):\n"
            "        os.system(' '.join([sys.executable, sys.argv[0], *base, '--worker', str(c), f'>fuzz-{c}.log', '2>&1']))\n"
            "        print(f'================== Job {c} exited ============', flush=True)\n"
            "        print(open(f'fuzz-{c}.log').read(), end='', flush=True)\n"
            "    jobs = [threading.Thread(target=job, args=(c,)) for c in range(n)]\n"
            "    [t.start() for t in jobs]\n"
            "{PARENT}"
            "    [t.join() for t in jobs]\n"
            "    sys.exit(0)\n"
            "C = int(sys.argv[sys.argv.index('--worker') + 1])\n"
            "def burn(seconds):\n"
            "    t = time.process_time() + seconds\n"
            "    while time.process_time() < t: pass\n"
            "{WORKER}")

    def jobs_body(self, parent, worker):
        return self.JOBS.replace("{PARENT}", parent).replace("{WORKER}", worker)

    def side(self, name):
        return os.path.join(self.root, name)

    def worker_pids(self, n):
        deadline = time.monotonic() + 60
        # Written whole by a worker that may still be writing it: wait for a pid, not for the file.
        while not all(os.path.exists(f) and os.path.getsize(f) for f in (self.side(f"pid-{c}") for c in range(n))) \
                and time.monotonic() < deadline:
            time.sleep(0.05)
        pids = [int(open(self.side(f"pid-{c}")).read()) for c in range(n)]
        for pid in pids:
            self.addCleanup(self.kill_if_alive, pid)  # a worker the driver failed to kill
        return pids

    @staticmethod
    def kill_if_alive(pid):
        with contextlib.suppress(OSError):
            os.kill(pid, signal.SIGKILL)

    def gone(self, pid, wait=10):
        """`pid` is no longer a running process (gone, or a zombie its new parent has yet to reap),
        now or within `wait` seconds."""
        deadline = time.monotonic() + wait
        while True:
            try:
                if open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()[0] == "Z":
                    return True
            except OSError:
                return True
            if time.monotonic() >= deadline:
                return False
            time.sleep(0.1)

    def seg_cpu(self):
        with open(os.path.join(self.seg, "cpu")) as f:
            return sum(float(x) for x in f.read().split())

    def worker_cpu(self, n):
        return sum(float(open(self.side(f"cpu-{c}")).read() or 0) for c in range(n))

    def test_an_aborted_segment_counts_its_workers_and_kills_them(self):
        # BL-820: libFuzzer aborts with its -jobs workers alive. They were never reaped by it, so its
        # rusage misses them; the tree the driver tracked has them, and the driver kills them.
        worker = (f"open({self.side('pid-')!r} + str(C), 'w').write(str(os.getpid()))\n"
                  f"burn(1.5)\nopen({self.side('cpu-')!r} + str(C), 'w').write(str(time.process_time()))\n"
                  "time.sleep(120)\n")
        parent = (f"    while not all(os.path.exists({self.side('cpu-')!r} + str(c)) for c in range(n)):\n"
                  "        time.sleep(0.05)\n    time.sleep(1)\n    os.abort()\n")
        proc = self.run_farm(600, self.jobs_body(parent, worker), flags=("-jobs=2", "-workers=2"))
        pids = self.worker_pids(2)
        s = self.finish(proc, rc=134)
        used = self.seg_cpu()
        self.assertGreaterEqual(used, self.worker_cpu(2) - 0.1, s)
        self.assertGreaterEqual(s["cpu_adopted"], self.worker_cpu(2) - 0.1, s)  # reaped by the driver
        self.assertEqual(s["status"], -6)
        self.assertGreaterEqual(s["orphans_killed"], 2)
        self.assertTrue(all(self.gone(pid, wait=0) for pid in pids))  # the leg lock goes with the driver

    # A worker that burns CPU until it is killed, writing its CPU time as it goes.
    BURNING = ("fd = os.open({cpu!r} + str(C), os.O_WRONLY | os.O_CREAT)\n"
               "while True:\n    burn(0.05)\n    os.pwrite(fd, f'{{time.process_time():<20}}'.encode(), 0)\n")

    def test_workers_of_a_fuzzer_that_dies_inside_the_first_poll_are_counted_and_killed(self):
        # BL-820 (OD-L3-129 F1, the review's probe): libFuzzer aborts 0.5 s in, before the first
        # poll's sample, with its jobs started; no process of the tree was ever tracked. The driver
        # adopted them: they are killed, reaped and counted.
        worker = (f"open({self.side('pid-')!r} + str(C), 'w').write(str(os.getpid()))\n"
                  + self.BURNING.format(cpu=self.side("cpu-")))
        before = resource.getrusage(resource.RUSAGE_CHILDREN)
        proc = self.run_farm(600, self.jobs_body("    time.sleep(0.5)\n    os.abort()\n", worker),
                             flags=("-jobs=2", "-workers=2"), NODE_FUZZ_CERTIFY_POLL="2", NODE_FUZZ_ORPHAN_WAIT="5")
        pids = self.worker_pids(2)
        s = self.finish(proc, rc=134)
        after = resource.getrusage(resource.RUSAGE_CHILDREN)
        self.assertTrue(all(self.gone(pid, wait=0) for pid in pids))
        self.assertGreaterEqual(s["orphans_killed"], 2, s)
        used = self.seg_cpu()
        self.assertGreaterEqual(used, self.worker_cpu(2) - 0.1, s)
        # Everything the driver and its descendants used, the driver included.
        total = after.ru_utime + after.ru_stime - before.ru_utime - before.ru_stime
        self.assertLessEqual(used, total + 0.5, s)

    def test_a_process_orphaned_and_ended_between_two_samples_counts(self):
        # BL-820 (OD-L3-129 F1): M starts W and exits at once; W burns 1 CPU-s and exits, all
        # between two samples. No sample ever read W, and nothing in libFuzzer's rusage holds it:
        # the driver adopted and reaped it.
        w_cpu, w, middle = self.side("w-cpu"), self.side("w.py"), self.side("middle.py")
        open(w, "w").write("import time\nt = time.process_time() + 1\nwhile time.process_time() < t: pass\n"
                           f"open({w_cpu!r}, 'w').write(str(time.process_time()))\n")
        open(middle, "w").write(f"import subprocess, sys\nsubprocess.Popen([sys.executable, {w!r}])\n")
        body = (self.STOP_FILE + f"subprocess.run([sys.executable, {middle!r}])\n"
                f"while not os.path.exists({w_cpu!r}):\n    time.sleep(0.05)\ntime.sleep(0.5)\n")
        s = self.finish(self.run_farm(600, body, NODE_FUZZ_CERTIFY_POLL="10"))
        used = float(open(w_cpu).read())
        total = self.seg_cpu()
        self.assertGreaterEqual(s["cpu_adopted"], used - 0.05, s)
        self.assertGreaterEqual(total, s["cpu_wait4"] + used - 0.05, s)

    def test_an_adopted_process_that_ends_between_two_samples_counts_by_its_rusage(self):
        # BL-820 (OD-L3-129 F1): W is read idle while its parent M lives, then adopted when M exits;
        # it burns 1.5 CPU-s and exits before the next sample (poll 10). The tracked figure keeps
        # W's idle reading; only the driver's wait4 of W holds the 1.5 s.
        w_cpu, w, middle, go = self.side("w-cpu"), self.side("w.py"), self.side("middle.py"), self.side("go")
        open(w, "w").write(f"import os, time\nwhile not os.path.exists({go!r}): time.sleep(0.05)\n"
                           "time.sleep(0.5)\nt = time.process_time() + 1.5\nwhile time.process_time() < t: pass\n"
                           f"open({w_cpu!r}, 'w').write(str(time.process_time()))\n")
        open(middle, "w").write(f"import os, subprocess, sys, time\nsubprocess.Popen([sys.executable, {w!r}])\n"
                                f"while not os.path.exists({go!r}): time.sleep(0.05)\n")
        body = (self.STOP_FILE + f"m = subprocess.Popen([sys.executable, {middle!r}])\ntime.sleep(2.5)\n"
                f"open({go!r}, 'w')\nwhile not os.path.exists({w_cpu!r}):\n    time.sleep(0.05)\ntime.sleep(0.5)\n")
        s = self.finish(self.run_farm(600, body, NODE_FUZZ_CERTIFY_POLL="10"))
        used = float(open(w_cpu).read())
        self.assertGreaterEqual(s["cpu_adopted"], used - 0.1, s)
        self.assertGreaterEqual(self.seg_cpu(), s["cpu_wait4"] + used - 0.1, s)

    def test_a_process_adopted_while_libfuzzer_runs_counts_toward_the_allotment(self):
        # BL-820 (OD-L3-129 F1): M starts W and exits at once, so W is the driver's before any sample
        # can read it under libFuzzer. W's CPU still ends the segment on its allotment.
        stop, w, middle = os.path.join(self.root, "seg", "stop"), self.side("w.py"), self.side("middle.py")
        open(w, "w").write(f"import os, time\nopen({self.side('pid-0')!r}, 'w').write(str(os.getpid()))\n"
                           f"while not os.path.exists({stop!r}):\n    t = time.process_time() + 0.05\n"
                           "    while time.process_time() < t: pass\n")
        open(middle, "w").write(f"import os, subprocess, sys\nsubprocess.Popen([sys.executable, {w!r}])\nos._exit(0)\n")
        body = (self.STOP_FILE + f"time.sleep(1.5)\nsubprocess.run([sys.executable, {middle!r}])\n"
                "while not os.path.exists(STOP):\n    time.sleep(0.05)\n")
        proc = self.run_farm(1, body, cap=30)
        pids = self.worker_pids(1)
        s = self.finish(proc)
        self.assertEqual((s["stopped_by"], s["status"]), ("cpu-window", 0), s)
        self.assertGreaterEqual(s["cpu_at_stop"], 1, s)
        self.assertTrue(self.gone(pids[0], wait=0))

    def test_workers_left_running_get_the_stop_file_before_any_kill(self):
        # BL-820 (OD-L3-129 F3): libFuzzer aborts with its jobs fuzzing. They get the segment's stop
        # file, as from libFuzzer, and stop by themselves: their final stats are printed and nothing
        # is killed. (Those that ignore it are killed and counted: the aborted-segment test.)
        worker = (f"open({self.side('pid-')!r} + str(C), 'w').write(str(os.getpid()))\n"
                  "while not os.path.exists(STOP):\n    burn(0.05)\n"
                  "print('stat::number_of_executed_units: 7', flush=True)\n")
        parent = (f"    while not all(os.path.exists({self.side('pid-')!r} + str(c)) for c in range(n)):\n"
                  "        time.sleep(0.05)\n    time.sleep(0.5)\n    os.abort()\n")
        proc = self.run_farm(600, self.jobs_body(parent, worker), flags=("-jobs=2", "-workers=2"),
                             NODE_FUZZ_ORPHAN_WAIT="15")
        pids = self.worker_pids(2)
        s = self.finish(proc, rc=134)
        self.assertEqual(s["orphans_killed"], 0, s)
        self.assertTrue(all(self.gone(pid, wait=0) for pid in pids))
        for c in range(2):
            self.assertIn("stat::number_of_executed_units: 7", open(os.path.join(self.seg, f"fuzz-{c}.log")).read())

    def test_a_process_the_stop_starts_is_waited_for_too(self):
        # BL-820 (OD-L3-129 F3): W, left running, answers the stop file by starting X and exiting a
        # second later; X stops by itself two seconds on. The wait samples every poll, so it waits for
        # X as well: nothing is killed.
        stop, w, x, x_done = os.path.join(self.root, "seg", "stop"), self.side("w.py"), self.side("x.py"), self.side("x-done")
        open(x, "w").write(f"import time\ntime.sleep(2)\nopen({x_done!r}, 'w')\n")
        open(w, "w").write(f"import os, subprocess, sys, time\nwhile not os.path.exists({stop!r}):\n    time.sleep(0.05)\n"
                           f"subprocess.Popen([sys.executable, {x!r}])\ntime.sleep(1)\n")
        body = self.STOP_FILE + f"subprocess.Popen([sys.executable, {w!r}])\ntime.sleep(1.5)\nos.abort()\n"
        s = self.finish(self.run_farm(600, body, NODE_FUZZ_ORPHAN_WAIT="15"), rc=134)
        self.assertEqual(s["orphans_killed"], 0, s)
        self.assertTrue(os.path.exists(x_done))

    def test_the_kill_takes_a_process_started_since_the_last_sample(self):
        # BL-820 (OD-L3-129 F1): W ignores the stop file but starts X when it appears, after the
        # exit's first sample; no poll samples in the wait (poll 10). The kill samples first, so it
        # finds and kills X too.
        stop, w, x = os.path.join(self.root, "seg", "stop"), self.side("w.py"), self.side("x.py")
        spin = "while True:\n    t = time.process_time() + 0.05\n    while time.process_time() < t: pass\n"
        open(x, "w").write("import time\n" + spin)
        open(w, "w").write(f"import os, subprocess, sys, time\nopen({self.side('pid-0')!r}, 'w').write(str(os.getpid()))\n"
                           f"while not os.path.exists({stop!r}):\n    time.sleep(0.05)\n"
                           f"open({self.side('pid-1')!r}, 'w').write(str(subprocess.Popen([sys.executable, {x!r}]).pid))\n" + spin)
        body = self.STOP_FILE + f"subprocess.Popen([sys.executable, {w!r}])\ntime.sleep(1.5)\nos.abort()\n"
        proc = self.run_farm(600, body, NODE_FUZZ_CERTIFY_POLL="10")
        pids = self.worker_pids(2)
        s = self.finish(proc, rc=134)
        self.assertEqual(s["orphans_killed"], 2, s)
        self.assertTrue(all(self.gone(pid, wait=0) for pid in pids))

    def test_a_reap_that_ends_at_its_bound_falls_back_to_the_tracked_figure(self):
        # BL-820 (OD-L3-129 F1): the exit's reap is bounded (10 s); a child it has not reaped by then
        # is missing from wait4 + adopted. The patch makes the reap collect nothing; the tracked
        # figure, sampled up to the kill, still holds the killed workers.
        worker = (f"open({self.side('pid-')!r} + str(C), 'w').write(str(os.getpid()))\n"
                  + self.BURNING.format(cpu=self.side("cpu-")))
        parent = (f"    while not all(os.path.exists({self.side('pid-')!r} + str(c)) for c in range(n)):\n"
                  "        time.sleep(0.05)\n    time.sleep(1)\n    os.abort()\n")
        proc = self.run_farm(600, self.jobs_body(parent, worker), flags=("-jobs=2", "-workers=2"),
                             patch=[("    adopted = reap()\n", "    adopted = (0.0, 0.0)\n")])
        self.worker_pids(2)
        s = self.finish(proc, rc=134)
        self.assertEqual((s["cpu_source"], s["cpu_adopted"]), ("tree", 0), s)
        self.assertGreaterEqual(self.seg_cpu(), self.worker_cpu(2) - 0.1, s)

    def test_a_signalled_segment_skips_the_stop_wait_and_records_itself(self):
        # BL-820 (OD-L3-135 R1): the farm stops a slice with a SIGINT to its process group and a
        # SIGKILL Farm.int_wait later. libFuzzer dies of the SIGINT; a worker hung in its interrupt
        # path answers neither it nor the stop file. The driver, signalled too, kills it at once
        # rather than waiting out its stop wait, and records the segment well before the SIGKILL.
        w = self.side("w.py")
        open(w, "w").write(f"import os, signal, time\nsignal.signal(signal.SIGINT, signal.SIG_IGN)\n"
                           f"open({self.side('pid-0')!r}, 'w').write(str(os.getpid()))\n"
                           "while True:\n    t = time.process_time() + 0.05\n    while time.process_time() < t: pass\n")
        body = (self.STOP_FILE + "signal.signal(signal.SIGINT, signal.SIG_DFL)\n"
                f"subprocess.Popen([sys.executable, {w!r}])\nwhile True:\n    time.sleep(0.05)\n")
        proc = self.run_farm(600, body, session=True, NODE_FUZZ_ORPHAN_WAIT="15")
        pids = self.worker_pids(1)
        sent = time.monotonic()
        os.killpg(proc.pid, signal.SIGINT)
        s = self.finish(proc, rc=130)
        self.assertLess(time.monotonic() - sent, 10, s)
        self.assertGreaterEqual(s["orphans_killed"], 1, s)
        self.assertTrue(self.gone(pids[0], wait=0))

    def test_the_exit_s_waits_fit_inside_the_farm_s_sigint_to_sigkill_step(self):
        # BL-820 (OD-L3-135 R1): an unsignalled exit's stop wait and reap, plus a loop's latency (1 s),
        # end before the farm's SIGKILL could follow a SIGINT, so the segment is always recorded.
        tree = ast.parse(open(os.path.join(fuzzctl.HERE, "window_driver.py")).read())
        consts = next(ast.literal_eval(n.value) for n in tree.body if isinstance(n, ast.Assign)
                      and isinstance(n.targets[0], ast.Tuple) and [getattr(e, "id", None) for e in n.targets[0].elts] == ["ORPHAN_WAIT", "REAP_WAIT"])
        self.assertLess(sum(consts) + 1, fuzzctl.Farm.int_wait)

    def test_an_inited_written_just_before_the_exit_is_read(self):
        # BL-820 (OD-L3-135 I2, MH): libFuzzer prints INITED and exits at once; each loop iteration
        # waits for it before scanning, so only the exit's final scan reads that line.
        s = self.finish(self.run_farm(600, "time.sleep(1)\nprint('#2\\tINITED cov: 5 ft: 9', flush=True)\nos._exit(0)\n"))
        self.assertIsNotNone(s["process_inited"][0]["seconds_to_inited"], s)

    def test_a_segment_that_cannot_adopt_its_orphans_does_not_start(self):
        # BL-820 (OD-L3-129 F1): without the subreaper the driver could not count or kill workers
        # libFuzzer leaves behind, so it fails closed, before libFuzzer starts.
        refuse = [("libc.prctl(36, 1, 0, 0, 0) != 0", "True")]
        proc = self.run_farm(600, self.STOP_FILE + "open(sys.argv[0] + '.ran', 'w')\n", patch=refuse)
        out, err = proc.communicate(timeout=60)
        self.assertEqual(proc.returncode, 1, err)
        self.assertIn("the segment cannot adopt its orphans", err)
        self.assertEqual(os.listdir(self.seg), [])
        self.assertFalse(os.path.exists(os.path.join(self.root, "fake-fuzzer.ran")))

    def test_the_grace_kill_counts_the_workers_cpu_up_to_the_kill(self):
        # BL-820: the stop comes on a poll, the kill GRACE seconds later with no poll in between.
        # The workers' CPU in that time is in the sample taken just before the kill.
        worker = (f"open({self.side('pid-')!r} + str(C), 'w').write(str(os.getpid()))\n"
                  + self.BURNING.format(cpu=self.side("cpu-")))
        parent = "    while True:\n        time.sleep(0.05)  # ignores its stop file\n"
        proc = self.run_farm(1, self.jobs_body(parent, worker), flags=("-jobs=2", "-workers=2"),
                             NODE_FUZZ_CERTIFY_POLL="10", NODE_FUZZ_CERTIFY_GRACE="3")
        pids = self.worker_pids(2)
        s = self.finish(proc, rc=137)
        self.assertTrue(all(self.gone(pid, wait=0) for pid in pids))
        used = self.seg_cpu()
        self.assertGreaterEqual(used, self.worker_cpu(2) - 0.2, s)
        self.assertEqual((s["stopped_by"], s["killed"], s["status"]), ("cpu-window", True, -9))

    def test_a_job_that_ends_in_the_grace_counts_when_the_fuzzer_then_aborts(self):
        # BL-820 (OD-L3-129 F4, MF): libFuzzer ignores the stop; job 0 fuzzes on for 1.5 CPU-s and
        # exits in the grace, reaped by libFuzzer; libFuzzer then aborts with job 1 alive. Both
        # figures hold both jobs: wait4 through libFuzzer's rusage and the adopted job 1, the
        # tracked tree through the samples taken in the grace.
        exited = self.side("exited-0")
        worker = (f"open({self.side('pid-')!r} + str(C), 'w').write(str(os.getpid()))\n"
                  "if C == 0:\n    while not os.path.exists(STOP):\n        burn(0.05)\n    burn(1.5)\n"
                  f"    open({self.side('cpu-')!r} + str(C), 'w').write(str(time.process_time()))\n"
                  f"    open({exited!r}, 'w')\n    sys.exit(0)\n"
                  + self.BURNING.format(cpu=self.side("cpu-")))
        parent = f"    while not os.path.exists({exited!r}):\n        time.sleep(0.05)\n    time.sleep(0.5)\n    os.abort()\n"
        proc = self.run_farm(1, self.jobs_body(parent, worker), flags=("-jobs=2", "-workers=2"))
        pids = self.worker_pids(2)
        s = self.finish(proc, rc=134)
        self.assertTrue(all(self.gone(pid, wait=0) for pid in pids))
        both = self.worker_cpu(2)
        used = self.seg_cpu()
        self.assertEqual((s["stopped_by"], s["killed"], s["status"]), ("cpu-window", False, -6))
        self.assertGreaterEqual(used, both - 0.2, s)
        self.assertGreaterEqual(s["cpu_tree"], both - 0.2, s)

    def test_a_fuzzer_read_as_a_zombie_does_not_count_its_reaped_jobs_twice(self):
        # BL-820 (r1 delta D1): libFuzzer exits between the loop's wait4 and a sample, which reads it
        # as a zombie whose times now hold the jobs it reaped. The patch makes that sample happen.
        zombie = [("    pid, wstatus, usage = os.wait4(proc.pid, os.WNOHANG)\n",
                   "    if farm and os.waitid(os.P_PID, proc.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT):\n"
                   "        sample()\n"
                   "    pid, wstatus, usage = os.wait4(proc.pid, os.WNOHANG)\n")]
        worker = (f"open({self.side('pid-')!r} + str(C), 'w').write(str(os.getpid()))\n"
                  "print('#2\\tINITED cov: 5 ft: 9', flush=True)\n"
                  "while not os.path.exists(STOP):\n    burn(0.05)\n")
        s = self.finish(self.run_farm(2, self.jobs_body("", worker), flags=("-jobs=2", "-workers=2"), patch=zombie))
        self.assertEqual((s["stopped_by"], s["status"]), ("cpu-window", 0))
        self.assertGreaterEqual(s["cpu_wait4"], 2)
        self.assertLessEqual(s["cpu_tree"], s["cpu_wait4"] + 0.1, s)

    def test_a_reused_pid_after_the_reap_is_left_alone(self):
        # BL-820 (r1 M1): once wait4 has reaped libFuzzer its pid may name another process, which
        # the exit's kill and sample must not take for the tree. The patch stands one in for it.
        other = subprocess.Popen([sys.executable, "-c", "import subprocess, sys, time\n"
                                  "kid = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(120)'])\n"
                                  "t = time.process_time() + 1\nwhile time.process_time() < t: pass\n"
                                  "print(kid.pid, flush=True)\ntime.sleep(120)\n"], stdout=subprocess.PIPE, text=True)
        kid = int(other.stdout.readline())
        self.addCleanup(other.kill)
        self.addCleanup(self.kill_if_alive, kid)
        reuse = [("fuzzer_reaped = True\n", "fuzzer_reaped = True\nproc.pid = int(os.environ['REUSED_PID'])\n")]
        s = self.finish(self.run_farm(600, self.STOP_FILE, patch=reuse, REUSED_PID=str(other.pid)))
        self.assertIsNone(other.poll())
        self.assertFalse(self.gone(kid, wait=0.5))
        self.assertEqual(s["orphans_killed"], 0)
        self.assertLess(s["cpu_tree"], 0.9, s)

    def test_a_process_reparented_to_the_driver_counts_by_its_rusage(self):
        # BL-820 (r1 L1, OD-L3-129 F1): a child starts W and exits; W is reparented to the driver,
        # which reaps it, so its CPU is in cpu_adopted. The tracked figure, the cross-check, holds W
        # too, less up to a sample interval of its CPU.
        w_cpu, w, middle = self.side("w-cpu"), self.side("w.py"), self.side("middle.py")
        open(w, "w").write("import time\ntime.sleep(1)\nt = time.process_time() + 2\n"
                           "while time.process_time() < t: pass\n"
                           f"open({w_cpu!r}, 'w').write(str(time.process_time()))\n")
        open(middle, "w").write(f"import subprocess, sys, time\nsubprocess.Popen([sys.executable, {w!r}])\n"
                                "time.sleep(0.6)\n")
        body = (self.STOP_FILE + f"subprocess.run([sys.executable, {middle!r}])\n"
                f"while not os.path.exists({w_cpu!r}):\n    time.sleep(0.05)\ntime.sleep(1)\n")
        s = self.finish(self.run_farm(600, body))
        used = float(open(w_cpu).read())
        # A W absorbed into an ancestor would be missing all of its 2 CPU-s; 1 s covers the samples' lag at load.
        self.assertGreaterEqual(s["cpu_tree"], s["cpu_wait4"] + used - 1.0, s)
        # W was reparented to the driver, which reaped it (OD-L3-129 F1).
        self.assertGreaterEqual(s["cpu_adopted"], used - 0.1, s)
        self.assertLessEqual(s["cpu_tree"], s["cpu_wait4"] + s["cpu_adopted"] + 0.1, s)

    def test_a_zombie_child_is_not_absorbed_by_a_parent_that_has_not_reaped_it(self):
        # BL-820 (r1 delta 2 E1): M reaped its child W and is a zombie its parent, libFuzzer, has yet
        # to reap; libFuzzer read running later has not absorbed M, whose frozen figure holds W.
        # libFuzzer then reaps M (so wait4 has W) and dies with an orphan O alive: the tree must have
        # O as well as W.
        o_cpu, w_cpu, w, middle = self.side("o-cpu"), self.side("w-cpu"), self.side("w.py"), self.side("middle.py")
        o = self.side("o.py")
        open(o, "w").write("import sys, time\nt = time.process_time() + 1\nwhile time.process_time() < t: pass\n"
                           "open(sys.argv[1], 'w').write(str(time.process_time()))\ntime.sleep(120)\n")
        open(w, "w").write("import time\nt = time.process_time() + 2\nwhile time.process_time() < t: pass\n"
                           f"open({w_cpu!r}, 'w').write(str(time.process_time()))\n")
        open(middle, "w").write(f"import subprocess, sys\nsubprocess.run([sys.executable, {w!r}])\n")
        body = (self.STOP_FILE
                + f"o = subprocess.Popen([sys.executable, {o!r}, {o_cpu!r}])\n"
                + f"open({self.side('pid-0')!r}, 'w').write(str(o.pid))\n"
                + f"m = subprocess.Popen([sys.executable, {middle!r}])\n"
                + f"while not (os.path.exists({o_cpu!r}) and os.path.getsize({o_cpu!r})):\n    time.sleep(0.05)\n"
                + "while open(f'/proc/{m.pid}/stat').read().rsplit(')', 1)[1].split()[0] != 'Z':\n    time.sleep(0.01)\n"
                + "time.sleep(1.5)\nos.waitpid(m.pid, 0)\nos._exit(3)\n")
        proc = self.run_farm(600, body)
        self.worker_pids(1)
        s = self.finish(proc, rc=3)
        used = float(open(o_cpu).read())
        self.assertGreaterEqual(s["cpu_wait4"], 1.9, s)
        self.assertGreaterEqual(s["cpu_tree"], s["cpu_wait4"] + used - 0.5, s)

    def test_each_job_s_inited_is_read_from_its_own_log(self):
        # BL-826: each worker's INITED is in its fuzz-C.log; the parent's output repeats it only once
        # the job has exited. The allotment still counts from the start, the loads included.
        worker = (f"open({self.side('pid-')!r} + str(C), 'w').write(str(os.getpid()))\n"
                  "burn(2)\n"
                  f"open({self.side('inited-')!r} + str(C), 'w').write(f'{{os.getpid()}} {{time.process_time()}}')\n"
                  "print('#2\\tINITED cov: 5 ft: 9', flush=True)\n"
                  "while not os.path.exists(STOP):\n    burn(0.05)\n")
        proc = self.run_farm(10, self.jobs_body("", worker), flags=("-jobs=2", "-workers=2"))
        self.worker_pids(2)
        s = self.finish(proc)
        self.assertEqual((s["stopped_by"], s["status"]), ("cpu-window", 0))
        self.assertEqual([p["log"] for p in s["process_inited"]], ["fuzz-0.log", "fuzz-1.log"], s)
        for c, p in enumerate(s["process_inited"]):
            pid, cpu = open(self.side(f"inited-{c}")).read().split()
            self.assertEqual(p["pid"], int(pid), s)
            self.assertGreaterEqual(p["cpu_at_inited"], float(cpu) - 0.02, s)
            # Read within a loop iteration, which a loaded box stretches; each stops at about 5.
            self.assertLessEqual(p["cpu_at_inited"], float(cpu) + 2.5, s)
            self.assertTrue(0 < p["seconds_to_inited"] <= s["elapsed"] + s["stop_to_exit"], s)
        self.assertEqual((s["load_seconds"], s["wall_seconds"], s["cpu_at_inited"]), (None, None, None))
        self.assertLess(s["cpu_at_stop"], 4 + 10)  # from INITED it would be 4 + 10 and more
        # Each finished job is reaped through its sh by libFuzzer, and counted once (BL-820).
        self.assertLessEqual(s["cpu_tree"], s["cpu_wait4"] + 0.1, s)
        self.assertEqual(s["orphans_killed"], 0)

    def test_a_job_that_never_reaches_inited_has_nulls(self):
        worker = (f"open({self.side('pid-')!r} + str(C), 'w').write(str(os.getpid()))\n"
                  "if C == 0:\n    print('#2\\tINITED cov: 5 ft: 9', flush=True)\n"
                  "while not os.path.exists(STOP):\n    burn(0.05)\n")
        # An allotment that leaves the driver many scans between job 0's INITED and its exit.
        s = self.finish(self.run_farm(3, self.jobs_body("", worker), flags=("-jobs=2", "-workers=2")))
        self.assertIsNotNone(s["process_inited"][0]["cpu_at_inited"])
        self.assertEqual(s["process_inited"][1], {"log": "fuzz-1.log", "pid": None, "seconds_to_inited": None,
                                                  "cpu_at_inited": None})

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
        used = self.seg_cpu()
        self.assertGreaterEqual(used, 4)  # the killed workers' CPU is the parent's reaped children's
        self.assertFalse(os.path.exists(os.path.join(self.seg, "window.json")))
        self.assertLessEqual(s["cpu_tree"], s["cpu_wait4"] + 0.1)  # a reaped child counts once (BL-820)

    def test_the_cpu_of_reaped_children_counts(self):
        # A worker that burned 2 CPU-s and was reaped is in its parent's cutime, which the tree sum
        # reads (utime and stime alone would see an idle parent).
        body = (self.STOP_FILE + "subprocess.run([sys.executable, '-c', 'import time\\nt = time.process_time() + 2\\n"
                "while time.process_time() < t: pass'])\nbegin = time.monotonic()\n"
                "while not os.path.exists(STOP) and time.monotonic() - begin < 40:\n    time.sleep(0.05)\n")
        s = self.finish(self.run_farm(2, body))
        self.assertEqual((s["stopped_by"], s["status"]), ("cpu-window", 0))
        self.assertGreaterEqual(s["cpu_at_stop"], 2)
        self.assertLessEqual(s["cpu_tree"], s["cpu_wait4"] + 0.1)

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
        s = self.finish(self.run_farm(3, f"open({self.side('pid-0')!r}, 'w').write(str(os.getpid()))\n" + body))
        self.assertEqual(s["stopped_by"], "cpu-window")
        self.assertLess(s["cpu_at_stop"], 5)  # 2 s of load and about 1 s of fuzzing, not 2 + 3
        # BL-826: the segment's INITED fields are not measured (null); its one process's are.
        self.assertEqual((s["load_seconds"], s["wall_seconds"], s["cpu_at_inited"]), (None, None, None))
        [p] = s["process_inited"]
        self.assertEqual((p["log"], p["pid"]), ("fuzz.out", int(open(self.side("pid-0")).read())))
        self.assertGreaterEqual(p["cpu_at_inited"], 1.98)
        self.assertLessEqual(p["cpu_at_inited"], s["cpu_at_stop"])
        self.assertTrue(0 < p["seconds_to_inited"] <= s["elapsed"])

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
        self.summary = r.stdout
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

    def test_the_verdict_records_the_build_s_coverage(self):
        """BL-812: `coverage` and `rust_trace_cmp`, the default build's when run.sh sets neither;
        the summary line names a build other than the default before the status."""
        run = self.fixture()
        _, v = self.verdict(run)
        self.assertEqual((v["coverage"], v["rust_trace_cmp"]), ("all", True))
        self.assertIn(" oracle_flavor=fuzz status=", self.summary)
        _, v = self.verdict(run, coverage="under-test", trace_cmp="0")
        self.assertEqual((v["coverage"], v["rust_trace_cmp"]), ("under-test", False))
        self.assertIn(" oracle_flavor=fuzz coverage=under-test rust_trace_cmp=0 status=", self.summary)
        _, v = self.verdict(run, coverage="all", trace_cmp="0")
        self.assertEqual((v["coverage"], v["rust_trace_cmp"]), ("all", False))
        self.assertIn(" oracle_flavor=fuzz coverage=all rust_trace_cmp=0 status=", self.summary)

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

    def test_a_census_floor_miss_fails_the_verdict_with_its_reason(self):
        run = os.path.join(self.fdir, "kv_model", "runs", "r3")
        record(run)
        seg2 = os.path.join(run, "logs", "segment-2")
        os.makedirs(seg2)
        open(os.path.join(seg2, "fuzz.out"), "w").write("stat::number_of_executed_units: 3\n")
        for n in (1, 2):
            open(os.path.join(run, "logs", f"segment-{n}", "cpu"), "w").write("1.0 0.0\n")
        certify = dict(mode="certify", window="3600", wall_cap="43200", merge_status="0", replay_before="0",
                       replay_after="")
        # The gate missed or failed: run.sh launched no window (no segment-3).
        for census_status, floor in (("1", "missed"), ("2", "error"), ("137", "error")):
            rc, v = self.verdict(run, 0, census_status=census_status, **certify)
            self.assertEqual((rc, v["status"], v["reason"], v["census_floor"]),
                             (1, "fail", "census floor not met", floor), census_status)
        # No gate (no chain-v2 target): n/a, and the missing window fails the run as before.
        rc, v = self.verdict(run, 0, **certify)
        self.assertEqual((rc, v["reason"], v["census_floor"]), (1, "the run did not end as intended", "n/a"))
        # A window that ran after a met floor passes; the same window with a missed floor does not.
        for n in (3, 4):
            os.makedirs(os.path.join(run, "logs", f"segment-{n}"))
            open(os.path.join(run, "logs", f"segment-{n}", "fuzz.out"), "w").write("stat::number_of_executed_units: 7\n")
        json.dump({"cpu_window": 3600, "stopped_by": "cpu-window", "killed": False, "stop_to_exit": 2.5},
                  open(os.path.join(run, "logs", fuzzctl.WINDOW, "window.json"), "w"))
        certify["replay_after"] = "0"
        rc, v = self.verdict(run, 0, census_status="0", **certify)
        self.assertEqual((rc, v["status"], v["census_floor"]), (0, "pass", "met"))
        rc, v = self.verdict(run, 0, census_status="1", **certify)
        self.assertEqual((rc, v["reason"]), (1, "census floor not met"))
        # Only a certify run's verdict has the key.
        self.assertNotIn("census_floor", self.verdict(self.fixture())[1])


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

    def smoke_run(self, sample=None, cost=0.0, cpu_hours="0.0003", grace="300", **build):
        """run.sh from its corpus directories to its verdict for one budgeted run on the stand-in
        (`sample`: (count, bytes) for --corpus-sample; `build`: run.sh's coverage and trace_cmp).
        Returns its exit status, summary line, verdict, run directory and what segment-1's process
        loaded."""
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
        env.update(build)
        r = subprocess.run(["bash", "-c", "set -euo pipefail\n" + script], env=env, capture_output=True, text=True,
                           timeout=600)
        lines = [x for x in r.stdout.splitlines() if x.startswith("node-fuzz ")]
        self.assertEqual(len(lines), 1, r.stdout + r.stderr)
        seg = os.path.join(run, "logs", "segment-1", "loaded.json")
        return (r.returncode, lines[0], json.load(open(os.path.join(run, "verdict.json"))), run,
                json.load(open(seg)) if os.path.exists(seg) else None)

    def test_an_opt_in_build_fuzzes_a_copy_of_every_input(self):
        """BL-812 (OD-L3-127 F1): a fuzzing run of a build other than the default loads a run-local
        copy of every working-corpus input and public seed first, leaves the working corpus as it
        was (the stand-in adds, deletes and rewrites in its first directory) and no copy behind."""
        self.plant("corpus", 30)
        self.plant("public-seeds", 10, size=lambda k: 40)
        before = self.snapshot()
        for coverage, trace_cmp in (("under-test", "1"), ("all", "0"), ("under-test", "0")):
            rc, line, v, run, loaded = self.smoke_run(coverage=coverage, trace_cmp=trace_cmp)
            self.assertEqual((rc, v["status"]), (0, "pass"), line)
            self.assertEqual(loaded["dirs"][0], os.path.join(run, "sample"))
            self.assertNotIn(os.path.join(self.tdir, "corpus"), loaded["dirs"])
            self.assertEqual(loaded["loaded"][loaded["dirs"][0]], sorted(self.pool_sizes()))
            self.assertEqual(self.snapshot(), before, (coverage, trace_cmp))
            self.assertFalse(os.path.exists(os.path.join(run, "sample")))

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


# ---------------------------------------------------------------------------------------------
# O2b-4 4e2: the census floor gate, phase-bound evidence and encoding enforcement (DESIGN §2)
CHAIN_V2 = "diff_chain_s0"
CHAIN_TARGETS = [Target(CHAIN_V2, "l1", ("v24.0",), "private", 60, 1, encoding="chain-v2")]


def floor_fixture(target=CHAIN_V2, **over):
    """A floor of schema o2b4-census-floor/1 that `census_fixture` meets."""
    floor = {"schema": "o2b4-census-floor/1", "floor_version": 1, "target": target, "profile": "S0",
             "encoding": "chain-v2", "inventory_sha256": "a" * 64, "overlay_sha256": "b" * 64,
             "require": {"inventory": ["validation.cpp:CheckBlockHeader#1:high-hash"], "ops": ["block:ok"],
                         "rejects": ["bad-prevblk"], "rows": ["reorg_3"]},
             "bounds": {"discard_rate_max": 0.01, "unexpected_max": 0, "replay_mismatches_max": 0,
                        "queue_drain_share_max": 0.05, "legb_share_min": None},
             "report_only": ["unresolved-legb-refused-open"],
             "declared_gaps": {"inventory": ["validation.cpp:AcceptBlockHeader#1:duplicate"],
                               "ops": ["block:duplicate"], "rejects": ["duplicate"]}}
    floor.update(over)
    return floor


def census_fixture(run_id, target=CHAIN_V2, **over):
    """A complete pre-replay census of `run_id`, as node-l1's phase writer leaves it: three
    executions (the replay fixtures' INITED counts three units)."""
    census = {"target": target, "schedule": "S0", "phase": "pre-replay", "run": run_id, "pin": "v24.0",
              "encoding": "chain-v2", "executions": 3, "complete": True, "inputs": 3, "discarded": 0,
              "discard_rate": 0.0, "replay_mismatches": 0,
              "ops": {"block:ok": 40, "header:ok": 2, "block:unsupported-queue-drain": 2},
              "rejects": {"bad-prevblk": 1}, "rows": {"reorg_3": 2, "unresolved-legb-refused-open": 1},
              "inventory": {"validation.cpp:CheckBlockHeader#1:high-hash": 1}, "unexpected": {}}
    census.update(over)
    return census


def write_bytes(path, data):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    open(path, "wb").write(data if isinstance(data, bytes) else json.dumps(data).encode())


class CensusFloor(unittest.TestCase):
    """census_floor.py's gate on a run's segment-2 (DESIGN §2.3, tests 8-15)."""

    RUN = "20261108T000000Z-v24.0-7"

    def setUp(self):
        self.root = tempfile.mkdtemp(prefix="census-floor-")
        self.addCleanup(shutil.rmtree, self.root, True)
        self.run = os.path.join(self.root, "runs", self.RUN)
        self.seg = os.path.join(self.run, "logs", "segment-2")
        os.makedirs(self.seg)
        self.src = os.path.join(self.root, "floor.json")
        self.census(census_fixture(self.RUN))
        open(os.path.join(self.seg, "fuzz.out"), "w").write(cov_line(3, 300, 1000, "INITED") +
                                                             "stat::number_of_executed_units: 3\n")
        self.floor(floor_fixture())

    def census(self, data):
        write_bytes(os.path.join(self.seg, "census.json"), data)

    def floor(self, data, built=None):
        write_bytes(self.src, data)
        sha = built or hashlib.sha256(open(self.src, "rb").read()).hexdigest()
        open(os.path.join(self.run, "floor.sha256"), "w").write(sha + "\n")

    def gate(self):
        rc = fuzzctl.census_floor.gate(self.run, CHAIN_V2, "v24.0", "chain-v2", self.src)
        return rc, json.load(open(os.path.join(self.seg, "floor-result.json")))

    def test_a_census_that_meets_its_floor_passes(self):
        rc, result = self.gate()
        self.assertEqual((rc, result["result"], result["problems"], result["missing"], result["bounds"]),
                         (0, "met", [], [], []))
        self.assertEqual(result["report_only"], {"unresolved-legb-refused-open": 1})
        self.assertEqual(result["reached_while_declared"], {})
        self.assertEqual(result["floor_version"], 1)
        copy = os.path.join(self.seg, "floor.json")
        self.assertEqual(open(copy, "rb").read(), open(self.src, "rb").read())
        self.assertEqual(result["floor_sha256"], hashlib.sha256(open(copy, "rb").read()).hexdigest())
        for name in ("floor.json", "floor-result.json"):
            self.assertEqual(stat.S_IMODE(os.stat(os.path.join(self.seg, name)).st_mode), 0o600, name)
        self.assertEqual(fuzzctl.census_floor.main(["gate", "--run", self.run, "--target", CHAIN_V2, "--pin", "v24.0",
                                                    "--encoding", "chain-v2", "--floor-src", self.src]), 0)

    def test_each_missing_requirement_is_named(self):
        for m, key in (("inventory", "validation.cpp:CheckBlockHeader#1:high-hash"), ("ops", "block:ok"),
                       ("rejects", "bad-prevblk"), ("rows", "reorg_3")):
            with self.subTest(m=m):
                full = census_fixture(self.RUN)
                self.census({**full, m: {k: n for k, n in full[m].items() if k != key}})
                rc, result = self.gate()
                self.assertEqual((rc, result["result"], result["missing"]), (1, "missed", [f"{m}:{key}"]))
        # A report-only key is never credited: a floor may not require one.
        self.census(census_fixture(self.RUN))
        floor = floor_fixture()
        floor["require"]["rows"].append("unresolved-legb-refused-open")
        self.floor(floor)
        self.assertEqual(self.gate()[0], 2)

    def test_each_bound_is_enforced(self):
        full = census_fixture(self.RUN)
        for over, bound in (({"discard_rate": 0.02}, "discard_rate_max"), ({"unexpected": {"x": 1}}, "unexpected_max"),
                            ({"replay_mismatches": 1}, "replay_mismatches_max"),
                            ({"ops": {**full["ops"], "header:unsupported-queue-drain": 2}}, "queue_drain_share_max")):
            with self.subTest(bound=bound):
                self.census({**full, **over})
                rc, result = self.gate()
                self.assertEqual((rc, result["result"]), (1, "missed"))
                self.assertEqual([b.split(":")[0] for b in result["bounds"]], [bound])
        # The share is of the ops that ran: an op refused before it ran (`:unsupported`) is none.
        self.census({**full, "ops": {"block:ok": 40, "header:unsupported-queue-drain": 5, "header:unsupported": 60}})
        rc, result = self.gate()
        self.assertEqual((rc, [b.split(":")[0] for b in result["bounds"]]), (1, ["queue_drain_share_max"]))
        self.assertIn(repr(5 / 45), result["bounds"][0])
        # legb_share on S0p: the census's own field, the bound failing when it is absent.
        bounds = {**floor_fixture()["bounds"], "legb_share_min": 0.2}
        self.floor(floor_fixture(profile="S0p", bounds=bounds))
        for share, rc in ((None, 1), (0.1, 1), (0.2, 0), (0.3, 0)):
            with self.subTest(legb_share=share):
                census = census_fixture(self.RUN, schedule="S0p")
                if share is not None:
                    census["legb_share"] = share
                self.census(census)
                self.assertEqual(self.gate()[0], rc)

    def test_a_malformed_or_foreign_floor_fails(self):
        floor = floor_fixture()
        bounds = ("the floor's bounds are not ['discard_rate_max', 'legb_share_min', 'queue_drain_share_max', "
                  "'replay_mismatches_max', 'unexpected_max'], each null, a count or a share")
        require = "the floor's require is not ['inventory', 'ops', 'rejects', 'rows'], each a list of distinct names"
        report_only = "the floor's report_only is not a list of distinct names"
        gaps = "the floor's declared_gaps is not ['inventory', 'ops', 'rejects'], each a list of distinct names"
        for name, bad, want in (
            ("schema", {**floor, "schema": "o2b4-census-floor/2"}, "the floor's schema is not o2b4-census-floor/1"),
            ("unknown key", {**floor, "extra": 1}, "the floor has unknown keys ['extra']"),
            ("absent key", {k: v for k, v in floor.items() if k != "bounds"}, "the floor lacks ['bounds']"),
            ("type", {**floor, "floor_version": "1"}, "the floor's floor_version is not a whole number from 1"),
            ("bound type", {**floor, "bounds": {**floor["bounds"], "discard_rate_max": 2}}, bounds),
            ("target", {**floor, "target": "diff_chain_par"}, "the floor is not diff_chain_s0's"),
            ("encoding", {**floor, "encoding": "chain-v1"}, "the floor's encoding is not chain-v2"),
            ("not an object", [floor], "the floor is not an object"),
            ("torn", json.dumps(floor)[:40].encode(), "the floor is unreadable (JSONDecodeError)"),
            ("profile", {**floor, "profile": "S3"}, "the floor's profile is not one of ['S0', 'S1', 'S2', 'S0p']"),
            ("inventory_sha256", {**floor, "inventory_sha256": "A" * 64},
             "the floor's inventory_sha256 is not 64 hex digits"),
            ("overlay_sha256", {**floor, "overlay_sha256": "b" * 63}, "the floor's overlay_sha256 is not 64 hex digits"),
            ("require missing a key", {**floor, "require": {k: v for k, v in floor["require"].items() if k != "rows"}},
             require),
            ("require with an extra key", {**floor, "require": {**floor["require"], "events": []}}, require),
            ("a duplicate name", {**floor, "require": {**floor["require"], "rows": ["reorg_3", "reorg_3"]}}, require),
            ("an empty name", {**floor, "require": {**floor["require"], "rows": [""]}}, require),
            ("not a list", {**floor, "require": {**floor["require"], "rows": "reorg_3"}}, require),
            ("report_only not a list", {**floor, "report_only": "x"}, report_only),
            ("report_only duplicated", {**floor, "report_only": ["x", "x"]}, report_only),
            ("declared_gaps absent at version 2",
             {k: v for k, v in floor.items() if k != "declared_gaps"} | {"floor_version": 2},
             "the floor lacks ['declared_gaps']"),
            ("declared_gaps not an object", {**floor, "declared_gaps": ["block:duplicate"]}, gaps),
            ("declared_gaps without a map", {**floor, "declared_gaps": {"ops": [], "rejects": []}}, gaps),
            ("declared_gaps with rows", {**floor, "declared_gaps": {**floor["declared_gaps"], "rows": []}}, gaps),
            ("a declared gap duplicated", {**floor, "declared_gaps": {**floor["declared_gaps"], "ops": ["x", "x"]}},
             gaps),
            ("a declared gap required", {**floor, "declared_gaps": {**floor["declared_gaps"], "ops": ["block:ok"]}},
             "the floor requires one of its declared gaps"),
            ("a count bound as a float", {**floor, "bounds": {**floor["bounds"], "unexpected_max": 0.5}}, bounds),
            ("no op required", {**floor, "require": {**floor["require"], "ops": []}}, "the floor requires no op"),
            ("nothing required", {**floor, "require": {k: [] for k in floor["require"]},
                                  "bounds": {k: None for k in floor["bounds"]}}, "the floor requires no op"),
        ):
            with self.subTest(name=name):
                self.floor(bad)
                rc, result = self.gate()
                self.assertEqual((rc, result["result"]), (2, "error"))
                self.assertIn(want, result["problems"])
        self.assertEqual(fuzzctl.census_floor.load_floor(self.src, CHAIN_V2, "chain-v2")[0], None)
        # A vacuous floor (nothing required, no bound) is refused for its ops.
        self.assertEqual(fuzzctl.census_floor.load_floor(self.src, CHAIN_V2, "chain-v2")[1],
                         ["the floor requires no op"])

    def test_a_report_only_key_counts_in_every_map(self):
        key = "validation.cpp:CheckBlockHeader#2:time-too-new"
        self.floor(floor_fixture(report_only=[key]))
        full = census_fixture(self.RUN)
        self.census({**full, "inventory": {**full["inventory"], key: 2}, "rows": {**full["rows"], key: 1}})
        rc, result = self.gate()
        self.assertEqual((rc, result["report_only"]), (0, {key: 3}))

    def test_a_census_reaching_a_declared_gap_reports_it_and_still_meets(self):
        """DEC-1567: a census counting a declared gap (block:duplicate, its reject or inventory key)
        reports it reached_while_declared with its count, by exact key in its own map; it is not a
        miss, and the floor is still met."""
        full = census_fixture(self.RUN)
        self.census({**full, "ops": {**full["ops"], "block:duplicate": 2, "header:duplicate": 5}})
        rc, result = self.gate()
        self.assertEqual((rc, result["result"], result["missing"]), (0, "met", []))
        self.assertEqual(result["reached_while_declared"], {"ops:block:duplicate": 2})
        # Exact keys: another kind's duplicate op or a duplicate-* reject is no gap's.
        self.census({**full, "ops": {**full["ops"], "header:duplicate": 5},
                     "rejects": {**full["rejects"], "duplicate-inconclusive": 1}})
        self.assertEqual(self.gate()[1]["reached_while_declared"], {})
        key = "validation.cpp:AcceptBlockHeader#1:duplicate"
        self.census({**full, "rejects": {**full["rejects"], "duplicate": 1},
                     "inventory": {**full["inventory"], key: 3}})
        rc, result = self.gate()
        self.assertEqual((rc, result["reached_while_declared"]), (0, {"rejects:duplicate": 1, f"inventory:{key}": 3}))

    def test_a_floor_before_version_2_may_omit_its_declared_gaps(self):
        """A floor copy written before floor_version 2 has no declared_gaps: it loads with none
        declared and evaluates as before, reaching none; a census counting block:duplicate is then
        not reported."""
        v1 = {k: v for k, v in floor_fixture().items() if k != "declared_gaps"}
        self.floor(v1)
        full = census_fixture(self.RUN)
        self.census({**full, "ops": {**full["ops"], "block:duplicate": 2}})
        rc, result = self.gate()
        self.assertEqual((rc, result["result"], result["problems"], result["missing"]), (0, "met", [], []))
        self.assertEqual(result["reached_while_declared"], {})
        floor, problems = fuzzctl.census_floor.load_floor(self.src, CHAIN_V2, "chain-v2")
        self.assertEqual((problems, floor["declared_gaps"]), ([], {"inventory": [], "ops": [], "rejects": []}))

    def test_a_floor_from_version_2_must_declare_its_gaps(self):
        self.floor({k: v for k, v in floor_fixture(floor_version=2).items() if k != "declared_gaps"})
        rc, result = self.gate()
        self.assertEqual((rc, result["result"]), (2, "error"))
        self.assertIn("the floor lacks ['declared_gaps']", result["problems"])

    def test_a_census_or_floor_that_raises_is_an_error(self):
        # P1: a census nested past Python's recursion limit.
        nested = b"[" * 200000 + b"]" * 200000
        self.census(nested)
        args = ["gate", "--run", self.run, "--target", CHAIN_V2, "--pin", "v24.0", "--encoding", "chain-v2",
                "--floor-src", self.src]
        self.assertEqual(fuzzctl.census_floor.main(args), 2)
        result = json.load(open(os.path.join(self.seg, "floor-result.json")))
        self.assertEqual((result["result"], result["problems"]), ("error", ["the census is unreadable (RecursionError)"]))
        self.assertEqual(fuzzctl.census_floor.read_json(os.path.join(self.seg, "census.json"))[1],
                         "unreadable (RecursionError)")
        # P2: a floor that exists but cannot be read, judged again as fuzzctl does.
        self.census(census_fixture(self.RUN))
        os.chmod(self.src, 0)
        self.addCleanup(os.chmod, self.src, 0o600)
        if os.access(self.src, os.R_OK):
            self.skipTest("running as root: a mode-0 file stays readable")
        result, report = fuzzctl.census_floor.judge_run(self.run, target=CHAIN_V2, pin="v24.0", encoding="chain-v2",
                                                        floor_path=self.src)
        self.assertEqual(result, "error")
        self.assertIn("the floor cannot be hashed (PermissionError)", report["problems"])
        # Anything the gate raises is its error, with its result written: never "missed".
        os.chmod(self.src, 0o600)
        with mock.patch.object(fuzzctl.census_floor, "judge_run", side_effect=RuntimeError("x")):
            self.assertEqual(fuzzctl.census_floor.main(args), 2)
        result = json.load(open(os.path.join(self.seg, "floor-result.json")))
        self.assertEqual((result["result"], result["problems"]), ("error", ["the gate failed (RuntimeError)"]))

    def write_failing(self, suffix, fail):
        """census_floor's writer, with `fail(path, data)` for the file whose path ends `suffix`."""
        real = fuzzctl.census_floor.write_private
        return mock.patch.object(fuzzctl.census_floor, "write_private",
                                 lambda path, data: fail(path, data) if path.endswith(suffix) else real(path, data))

    def test_the_gate_writes_its_own_result_or_none(self):
        stale = os.path.join(self.seg, "floor-result.json")
        write_bytes(stale, {"result": "met"})

        def read_only(path, data):
            raise OSError("read-only")
        with self.write_failing("/floor-result.json", read_only):
            self.assertEqual(fuzzctl.census_floor.gate(self.run, CHAIN_V2, "v24.0", "chain-v2", self.src), 2)
        self.assertFalse(os.path.exists(stale), "an older result stands for this gate's")

    def test_a_floor_that_cannot_be_copied_is_named_so(self):
        real = fuzzctl.census_floor.write_private

        def no_segment(path, data):
            raise FileNotFoundError(path)

        def differs(path, data):
            real(path, data + b" ")
        for fail, want in ((no_segment, "the floor could not be copied (FileNotFoundError)"),
                           (differs, "the floor's copy is not the floor")):
            with self.subTest(want=want), self.write_failing("/floor.json", fail):
                rc, result = self.gate()
                self.assertEqual((rc, result["problems"]), (2, [want]))

    def test_a_missing_floor_fails(self):
        os.remove(self.src)
        open(os.path.join(self.run, "floor.sha256"), "w").write("missing\n")
        rc, result = self.gate()
        self.assertEqual((rc, result["result"], result["problems"]), (2, "error", ["the target has no floor"]))
        # A floor other than the one the run was built with.
        self.floor(floor_fixture(), built="0" * 64)
        rc, result = self.gate()
        self.assertEqual((rc, result["problems"]), (2, ["the floor is not the one the run was built with"]))
        os.remove(os.path.join(self.run, "floor.sha256"))
        self.assertEqual(self.gate()[0], 2)

    def test_a_torn_or_incomplete_census_fails(self):
        whole = json.dumps(census_fixture(self.RUN)).encode()
        for name, data in (("torn", whole[:len(whole) // 2]), ("complete false", census_fixture(self.RUN, complete=False)),
                           ("no complete", {k: v for k, v in census_fixture(self.RUN).items() if k != "complete"}),
                           ("not an object", [1])):
            with self.subTest(name=name):
                self.census(data)
                rc, result = self.gate()
                self.assertEqual((rc, result["result"]), (2, "error"))
        self.census(census_fixture(self.RUN))
        open(os.path.join(self.seg, "census.failed"), "w").close()
        self.assertEqual(self.gate()[1]["problems"], ["the census write failed"])
        os.remove(os.path.join(self.seg, "census.failed"))
        os.remove(os.path.join(self.seg, "census.json"))
        self.assertEqual(self.gate()[1]["problems"], ["the census is missing"])

    def test_provenance_is_bound(self):
        for key, other in (("target", "diff_chain_par"), ("phase", "post-replay"), ("run", "20261107T000000Z-v24.0-7"),
                           ("pin", "v23.1"), ("encoding", "chain-v1"), ("schedule", "S1")):
            with self.subTest(key=key):
                self.census(census_fixture(self.RUN, **{key: other}))
                rc, result = self.gate()
                self.assertEqual((rc, result["result"]), (2, "error"))
                self.assertEqual([p for p in result["problems"] if f"census's {key} " in p], result["problems"])

    def test_executions_must_equal_the_replays_inited_units(self):
        for executions, rc in ((3, 0), (2, 2), (4, 2), (None, 2), ("3", 2)):
            with self.subTest(executions=executions):
                self.census(census_fixture(self.RUN, executions=executions))
                self.assertEqual(self.gate()[0], rc)
        self.census(census_fixture(self.RUN))
        out = os.path.join(self.seg, "fuzz.out")
        for log in ("", cov_line(3, 300, 1000, "INITED") * 2):
            with self.subTest(log=log[:10]):
                open(out, "w").write(log)
                rc, result = self.gate()
                self.assertEqual((rc, result["problems"]), (2, ["the replay's log has no single INITED line"]))



def overlay_census(overlay, profile):
    """A census crediting each cell the o2b4 overlay requires on `profile` once, with no discard,
    mismatch or unexpected reply: what a run reaching every required cell would count."""
    def required(section, name, key=lambda k: k):
        return {key(e[name]): 1 for e in overlay[section] if e[profile]["status"] == "required"}
    return {"discard_rate": 0.0, "replay_mismatches": 0, "unexpected": {}, "legb_share": 1.0,
            "inventory": {**required("rows", "key"), **required("script_errors", "error", lambda e: f"script:{e}")},
            "ops": required("ops", "op"), "rejects": required("rejects", "reject"),
            "rows": required("named_rows", "row")}


class CommittedFloors(unittest.TestCase):
    """O2b-4 4e1 (DESIGN §6.2): each committed floor in node-l1/census loads through
    census_floor.load_floor, and a census built from the overlay's required cells on its profile,
    not from the floor, meets it: a floor requiring a cell the overlay does not is caught."""

    def test_each_committed_floor_loads_and_the_overlay_s_cells_meet_it(self):
        overlay_path = os.path.join(fuzzctl.ROOT, "contrib", "node", "l3-census", "o2b4-overlay-v24.0.0.json")
        with open(overlay_path) as f:
            overlay = json.load(f)
        paths = sorted(glob.glob(os.path.join(fuzzctl.CENSUS_DIR, "*.floor.json")))
        targets = [os.path.basename(p)[:-len(".floor.json")] for p in paths]
        self.assertEqual(targets, ["diff_chain_av", "diff_chain_par", "diff_chain_prodflush", "diff_chain_s0"])
        for path, target in zip(paths, targets):
            with self.subTest(target=target):
                floor, problems = fuzzctl.census_floor.load_floor(path, target, "chain-v2")
                self.assertEqual(problems, [])
                self.assertEqual(overlay["profiles"][floor["profile"]]["target"], target)
                result = fuzzctl.census_floor.evaluate(overlay_census(overlay, floor["profile"]), floor)
                self.assertEqual((result["missing"], result["bounds"]), ([], []))
                self.assertTrue(result["met"])

    def test_each_committed_floor_requires_exactly_the_overlay_s_cells(self):
        """OD-L3-171 nit 5: a committed floor's required keys are the overlay's required cells on
        its profile, section by section, so a floor that drops a cell fails here too, not only in
        o2b4-check's byte-for-byte regeneration."""
        overlay_path = os.path.join(fuzzctl.ROOT, "contrib", "node", "l3-census", "o2b4-overlay-v24.0.0.json")
        with open(overlay_path) as f:
            overlay = json.load(f)
        for path in sorted(glob.glob(os.path.join(fuzzctl.CENSUS_DIR, "*.floor.json"))):
            target = os.path.basename(path)[:-len(".floor.json")]
            with self.subTest(target=target):
                floor, problems = fuzzctl.census_floor.load_floor(path, target, "chain-v2")
                self.assertEqual(problems, [])
                cells = overlay_census(overlay, floor["profile"])
                for section in ("inventory", "ops", "rejects", "rows"):
                    self.assertEqual(sorted(floor["require"][section]), sorted(cells[section]), section)
                # DEC-1567: the floor declares exactly the overlay's encoding gaps on its profile.
                gaps = {m: sorted(e[name] for e in overlay[sec] if e[floor["profile"]].get("code") == "encoding_gap")
                        for sec, name, m in (("rows", "key", "inventory"), ("ops", "op", "ops"),
                                             ("rejects", "reject", "rejects"))}
                self.assertEqual(floor["declared_gaps"], gaps)
                self.assertIn("block:duplicate", floor["declared_gaps"]["ops"])

    def test_a_census_without_one_run_basis_row_misses_prodflush_s_floor(self):
        """OD-L3-165 (A) 2: the leg-B rows on the run basis have no seed, and the floor still holds a
        run to each: a census lacking any one of them misses, by that row."""
        overlay_path = os.path.join(fuzzctl.ROOT, "contrib", "node", "l3-census", "o2b4-overlay-v24.0.0.json")
        with open(overlay_path) as f:
            overlay = json.load(f)
        target = "diff_chain_prodflush"
        floor, problems = fuzzctl.census_floor.load_floor(
            os.path.join(fuzzctl.CENSUS_DIR, f"{target}.floor.json"), target, "chain-v2")
        self.assertEqual(problems, [])
        run_rows = sorted(e["row"] for e in overlay["named_rows"] if e["S0p"].get("basis") == "run")
        self.assertEqual(len(run_rows), 12)
        self.assertTrue(set(run_rows) <= set(floor["require"]["rows"]))
        for row in run_rows:
            with self.subTest(row=row):
                census = overlay_census(overlay, "S0p")
                del census["rows"][row]
                result = fuzzctl.census_floor.evaluate(census, floor)
                self.assertFalse(result["met"])
                self.assertEqual(result["missing"], [f"rows:{row}"])

class CensusCertify(Tree):
    """`certify`'s judgement of a chain-v2 run by its census (DESIGN §2.4, tests 16-27 and 40):
    fixture runs as run.sh leaves them, the gate run on them as run.sh runs it."""

    REV, BIN = Certifying.REV, Certifying.BIN
    certify_run = Certifying.certify_run

    def setUp(self):
        super().setUp()
        census_dir = os.path.join(self.root, "census")
        os.mkdir(census_dir)
        patch = mock.patch.object(fuzzctl, "CENSUS_DIR", census_dir)
        patch.start()
        self.addCleanup(patch.stop)
        self.current = os.path.join(census_dir, f"{CHAIN_V2}.floor.json")
        write_bytes(self.current, floor_fixture())

    def chain_run(self, census=None, encoding="chain-v2", gate=True, certificate=False, target=CHAIN_V2, **kw):
        """A certify run of `target` with its encoding record, its floor's sha256 as its build saw
        it, its segment-2 census (`census` maps the run id to the census, or None for none) and
        the gate's result."""
        run = self.certify_run(target=target, **kw)
        name = os.path.basename(run)
        if encoding is not None:
            open(os.path.join(run, "encoding"), "w").write(encoding + "\n")
        sha = hashlib.sha256(open(self.current, "rb").read()).hexdigest() if os.path.exists(self.current) else "missing"
        open(os.path.join(run, "floor.sha256"), "w").write(sha + "\n")
        data = census_fixture(name, target=target) if census is None else census(name)
        if data is not None:
            write_bytes(os.path.join(run, "logs", "segment-2", "census.json"), data)
        if gate:
            fuzzctl.census_floor.gate(run, target, "v24.0", "chain-v2", self.current)
        if certificate:
            fuzzctl.write_private(os.path.join(run, fuzzctl.CERTIFICATE), json.dumps(
                {"evidence": fuzzctl.certify_evidence(run), "cutoff": kw.get("stamp", "20261011T000000Z"),
                 "eligibility": []}))
        return run

    def read(self, run, **kw):
        status, reasons, _ = fuzzctl.read_certificate(run, **kw)
        return status, reasons

    def test_a_chain_v2_run_that_meets_its_floor_certifies(self):
        run = self.chain_run()
        self.assertEqual(self.read(run), ("certified", []))
        ev = fuzzctl.certify_evidence(run)
        self.assertEqual(ev["encoding"], "chain-v2")
        self.assertEqual(ev["census_floor"], {
            "floor_sha256": hashlib.sha256(open(self.current, "rb").read()).hexdigest(), "floor_version": 1,
            "result": "met", "missing": [], "reached_while_declared": {}})
        run = self.chain_run(certificate=True)
        self.assertEqual(self.certs(CHAIN_TARGETS)[f"{CHAIN_V2}@v24.0"][:2], ("certified", []))

    def test_a_certificate_from_before_declared_gaps_stays_certified(self):
        """A run gated before floors declared their gaps: its floor-result.json and its
        certificate's evidence have no reached_while_declared, and its certificate still matches
        its evidence."""
        run = self.chain_run()
        result = os.path.join(run, "logs", "segment-2", "floor-result.json")
        old = {k: v for k, v in json.load(open(result)).items() if k != "reached_while_declared"}
        write_bytes(result, old)
        ev = fuzzctl.certify_evidence(run)
        self.assertNotIn("reached_while_declared", ev["census_floor"])
        old_ev = json.loads(json.dumps(ev))
        old_ev["census_floor"].pop("reached_while_declared", None)
        fuzzctl.write_private(os.path.join(run, fuzzctl.CERTIFICATE), json.dumps(
            {"evidence": old_ev, "cutoff": "20261011T000000Z", "eligibility": []}))
        self.assertEqual(self.read(run), ("certified", []))

    def test_a_reached_declared_gap_is_counted_on_the_certify_line_and_kept(self):
        """DEC-1567 (3): the private-route certify line counts the declared gaps the pre-window
        replay reached, census_rejudge keeps them, and they never change the status."""
        def certify(ops):
            def runner(target, pin, args, **kw):
                tag = args[args.index("--run-tag") + 1]
                run = self.chain_run(census=lambda r: census_fixture(r, ops={**census_fixture(r)["ops"], **ops}), tag=tag)
                return Leg(target, pin, "node-fuzz fake", 1, run, {})
            out = io.StringIO()
            with contextlib.redirect_stdout(out), mock.patch.object(fuzzctl.os, "nice"):
                rc = fuzzctl.certify(certify_args(f"{CHAIN_V2}@v24.0"), CHAIN_TARGETS, runner=runner)
            return rc, out.getvalue()
        floor = floor_fixture(floor_version=2)
        write_bytes(self.current, floor)
        clean = certify({})
        self.assertEqual(clean, (0, "node-fuzz certify: a private-route leg: status=certified reached_while_declared=0\n"))
        reached = certify({"block:duplicate": 2})
        self.assertEqual(reached, (0, "node-fuzz certify: a private-route leg: status=certified reached_while_declared=1\n"))
        run = self.chain_run(census=lambda r: census_fixture(r, ops={**census_fixture(r)["ops"], "block:duplicate": 2}))
        ev = fuzzctl.certify_evidence(run)
        self.assertEqual(ev["census_floor"]["reached_while_declared"], {"ops:block:duplicate": 2})
        census = fuzzctl.census_rejudge(run, ev, "chain-v2")
        self.assertEqual((census["failed"], census["changed"], census["reached_while_declared"]),
                         ([], [], {"ops:block:duplicate": 2}))
        self.assertEqual(self.read(run), ("certified", []))

    def certs(self, targets):
        fuzzctl.run_triage(targets, minimize=False)
        return fuzzctl.certificates(self.fdir, targets)

    def assert_failed(self, run, why):
        status, reasons = self.read(run)
        self.assertEqual(status, "failed", reasons)
        self.assertTrue(any(why in r for r in reasons), reasons)

    def test_a_stale_valid_census_from_an_earlier_run_is_not_certified(self):
        # An earlier run's whole segment-2 (its census, the floor copy and its gate's `met`) in a
        # later run whose own replay left none.
        earlier = self.chain_run(stamp="20261010T000000Z")
        self.assertEqual(self.read(earlier)[0], "certified")
        run = self.chain_run(census=lambda _: None, gate=False)
        for name in ("census.json", "floor.json", "floor-result.json"):
            shutil.copyfile(os.path.join(earlier, "logs", "segment-2", name), os.path.join(run, "logs", "segment-2", name))
        self.assert_failed(run, "the census's run is not")

    def test_a_census_of_the_wrong_phase_is_not_certified(self):
        self.assert_failed(self.chain_run(census=lambda r: census_fixture(r, phase="post-replay")),
                           "the census's phase is not")

    def test_a_census_of_another_target_is_not_certified(self):
        self.assert_failed(self.chain_run(census=lambda r: census_fixture(r, target="diff_chain_par")),
                           "the census's target is not")

    def test_a_census_of_another_run_id_is_not_certified(self):
        self.assert_failed(self.chain_run(census=lambda r: census_fixture(r + "0")), "the census's run is not")

    def test_a_missing_floor_is_not_certified(self):
        os.remove(self.current)
        self.assert_failed(self.chain_run(), "the census floor gate did not pass (error)")

    def test_a_malformed_floor_is_not_certified(self):
        write_bytes(self.current, {**floor_fixture(), "extra": 1})
        self.assert_failed(self.chain_run(), "the census floor gate did not pass (error)")

    def test_a_failed_census_write_is_not_certified(self):
        def failed(run_id):
            open(os.path.join(self.fdir, CHAIN_V2, "runs", run_id, "logs", "segment-2", "census.failed"), "w").close()
        self.assert_failed(self.chain_run(census=failed), "the census write failed")

    def test_a_read_only_segment_2_is_not_certified(self):
        # node-l1 could write neither the census nor the marker beside it.
        self.assert_failed(self.chain_run(census=lambda _: None), "the census is missing")

    def test_a_census_that_cannot_be_judged_again_fails(self):
        run = self.chain_run()
        with mock.patch.object(fuzzctl.census_floor, "judge_run", side_effect=RecursionError("deep")):
            self.assert_failed(run, "the census could not be judged again (RecursionError)")

    def test_an_unreadable_chain_encodings_toml_fails_closed(self):
        run = self.chain_run()
        with mock.patch.object(fuzzctl, "CHAIN_ENCODINGS", os.path.join(self.root, "absent.toml")):
            status, reasons = self.read(run)
            self.assertEqual(status, "invalid", reasons)
            self.assertIn("the chain target's encoding is unknown", reasons[0])
            # A run of no chain target is judged as before.
            self.assertEqual(self.read(self.certify_run()), ("certified", []))

    def test_a_torn_census_is_not_certified(self):
        torn = lambda r: json.dumps(census_fixture(r)).encode()[:60]
        self.assert_failed(self.chain_run(census=torn), "the census is unreadable")

    def test_a_missing_row_is_not_certified(self):
        census = lambda r: census_fixture(r, rows={"unresolved-legb-refused-open": 1})
        run = self.chain_run(census=census)
        self.assert_failed(run, "the census floor gate did not pass (missed)")
        self.assertEqual(fuzzctl.certify_evidence(run)["census_floor"]["missing"], ["rows:reorg_3"])
        # A forged `met` in the gate's record does not pass the re-judge of the census.
        result = os.path.join(run, "logs", "segment-2", "floor-result.json")
        json.dump({**json.load(open(result)), "result": "met", "missing": []}, open(result, "w"))
        self.assert_failed(run, "the census does not meet the floor the run recorded (missed: rows:reorg_3)")

    def test_a_status_reread_after_a_floor_change_is_not_certified(self):
        run = self.chain_run(certificate=True)
        self.assertEqual(self.certs(CHAIN_TARGETS)[f"{CHAIN_V2}@v24.0"][0], "certified")
        stricter = floor_fixture(floor_version=2)
        stricter["require"]["rows"].append("reorg_9")
        write_bytes(self.current, stricter)
        status, reasons, _ = self.certs(CHAIN_TARGETS)[f"{CHAIN_V2}@v24.0"]
        self.assertEqual(status, "uncertified")
        self.assertTrue(all("the floor changed since the run" in r for r in reasons), reasons)
        self.assertIn("rows:reorg_9", reasons[0])
        # A looser floor now changes nothing: the run met the one it recorded.
        looser = floor_fixture(floor_version=3)
        looser["require"]["rows"] = []
        write_bytes(self.current, looser)
        self.assertEqual(self.read(run)[0], "certified")
        # The recorded copy tampered with: the run's own record no longer holds.
        copy = os.path.join(run, "logs", "segment-2", "floor.json")
        write_bytes(copy, looser)
        self.assertEqual(self.read(run)[0], "invalid")  # its certificate recorded the copy's sha256
        status, reasons = self.read(run, cert=None)
        self.assertEqual(status, "failed")
        self.assertIn("the recorded floor is not the one the run was built with", reasons)

    def test_non_chain_evidence_keeps_its_key_set(self):
        before_4e2 = {
            "target", "pin", "run", "rev", "dirty", "flavor", "engine", "jobs", "window_seconds", "wall_cap",
            "merge_status", "replay_before_status", "replay_after_status", "libfuzzer_status", "verdict_status",
            "processes", "baseline", "final", "log_problems", "window_inited", "window_done", "new_lines",
            "units_added", "unit_lines", "executions", "cpu_seconds", "window", "source", "divergences", "crashes",
            "sink_failures", "refused"}
        run = self.certify_run()
        self.assertEqual(set(fuzzctl.certify_evidence(run)), before_4e2)
        self.assertEqual(self.read(run), ("certified", []))
        # The legacy chain target: its encoding only, never a census.
        run = self.chain_run(target="diff_chain", encoding="chain-v1", gate=False, census=lambda _: None)
        self.assertEqual(set(fuzzctl.certify_evidence(run)), before_4e2 | {"encoding"})
        self.assertEqual(self.read(run), ("certified", []))

    def test_evidence_with_another_encoding_is_invalid(self):
        for encoding in ("chain-v1", None, "chain-v2\nchain-v2"):
            with self.subTest(encoding=encoding):
                run = self.chain_run(encoding=encoding)
                status, reasons = self.read(run)
                self.assertEqual(status, "invalid", reasons)
                self.assertTrue(any("is not the registry's 'chain-v2'" in r for r in reasons), reasons)
        # A run of a target the registry no longer lists as a chain target, whose run recorded one.
        run = self.chain_run()
        self.assertEqual(self.read(run, encoding="")[0], "invalid")
        # Q8: a legacy diff_chain run without the record fails closed.
        run = self.chain_run(target="diff_chain", encoding=None, gate=False, census=lambda _: None)
        self.assertEqual(self.read(run)[0], "invalid")


class TreeEncoding(unittest.TestCase):
    """tree_encoding.py, the ENCODING marker run.sh adopts before it touches a chain tree (§3a.3
    (3), tests 35-37)."""

    def setUp(self):
        self.root = tempfile.mkdtemp(prefix="tree-encoding-")
        self.addCleanup(shutil.rmtree, self.root, True)
        self.tdir = os.path.join(self.root, "diff_chain_s0")

    def cli(self, action, encoding, tdir=None):
        r = subprocess.run([sys.executable, "-I", os.path.join(fuzzctl.HERE, "tree_encoding.py"), action,
                            tdir or self.tdir, encoding], capture_output=True, text=True)
        return r.returncode, r.stderr

    def marker(self):
        path = os.path.join(self.tdir, "ENCODING")
        return open(path).read() if os.path.exists(path) else None

    def test_a_legacy_tree_adopts_v1_once(self):
        for d in ("corpus", "regress/open"):
            os.makedirs(os.path.join(self.tdir, d))
        open(os.path.join(self.tdir, "corpus", "a"), "w").write("x")
        self.assertEqual(self.cli("adopt", "chain-v1"), (0, ""))
        self.assertEqual(self.marker(), "chain-v1\n")
        path = os.path.join(self.tdir, "ENCODING")
        self.assertEqual(stat.S_IMODE(os.stat(path).st_mode), 0o600)
        seen = os.stat(path).st_mtime_ns
        self.assertEqual(self.cli("adopt", "chain-v1"), (0, ""))
        self.assertEqual((self.marker(), os.stat(path).st_mtime_ns), ("chain-v1\n", seen))
        # Marked, it is no other encoding's.
        self.assertEqual(self.cli("check", "chain-v2")[0], 3)
        self.assertEqual(self.cli("adopt", "chain-v2")[0], 3)
        self.assertEqual(self.marker(), "chain-v1\n")

    def test_a_fresh_v2_tree_is_marked(self):
        self.assertEqual(self.cli("check", "chain-v2"), (0, ""))
        self.assertFalse(os.path.exists(self.tdir))  # check writes nothing
        self.assertEqual(self.cli("adopt", "chain-v2"), (0, ""))
        self.assertEqual(self.marker(), "chain-v2\n")
        self.assertEqual(stat.S_IMODE(os.stat(self.tdir).st_mode), 0o700)
        # Empty input directories, and runs/, are no inputs.
        other = os.path.join(self.root, "diff_chain_av")
        for d in ("corpus", "seeds/x", "regress/open", "public-seeds", "runs/r1"):
            os.makedirs(os.path.join(other, d))
        open(os.path.join(other, "runs", "r1", "verdict.json"), "w").write("{}")
        self.assertEqual(self.cli("adopt", "chain-v2", other), (0, ""))
        self.assertEqual(open(os.path.join(other, "ENCODING")).read(), "chain-v2\n")

    def test_a_v2_tree_with_any_input_directory_nonempty_is_refused(self):
        for d in ("corpus", "seeds", "regress", "public-seeds"):
            for kind in ("file", "nested", "link", "not a directory"):
                with self.subTest(dir=d, kind=kind):
                    shutil.rmtree(self.tdir, ignore_errors=True)
                    os.makedirs(self.tdir)
                    path = os.path.join(self.tdir, d)
                    if kind == "not a directory":
                        open(path, "w").write("x")
                    else:
                        os.makedirs(os.path.join(path, "sub"))
                        if kind == "file":
                            open(os.path.join(path, "a"), "w").write("x")
                        elif kind == "nested":
                            open(os.path.join(path, "sub", "a"), "w").write("x")
                        else:
                            os.symlink(self.root, os.path.join(path, "l"))
                    rc, err = self.cli("adopt", "chain-v2")
                    self.assertEqual(rc, 4, err)
                    self.assertIn(d, err)
                    self.assertIsNone(self.marker())
        # A marker that is not a plain file, or names nothing known.
        shutil.rmtree(self.tdir)
        os.makedirs(self.tdir)
        os.symlink(os.path.join(self.root, "elsewhere"), os.path.join(self.tdir, "ENCODING"))
        self.assertEqual(self.cli("adopt", "chain-v2")[0], 3)
        self.assertEqual(self.cli("adopt", "v2")[0], 2)
        self.assertEqual(self.cli("mark", "chain-v2")[0], 2)
        # A marker is exactly one encoding's name and a newline (P5).
        os.remove(os.path.join(self.tdir, "ENCODING"))
        for data in (b"chain-v2" + b" " * 56 + b"chain-v1\n", b"chain-v2\n" + b"x" * 60, b"chain-v2",
                     b" chain-v2\n", b"chain-v2\n\n", b"chain-v2 \n", b"", b"\xffchain-v2\n"):
            with self.subTest(marker=data):
                open(os.path.join(self.tdir, "ENCODING"), "wb").write(data)
                rc, err = self.cli("check", "chain-v2")
                self.assertEqual(rc, 3, err)
                self.assertIn("is not an encoding's name and a newline", err)
        open(os.path.join(self.tdir, "ENCODING"), "wb").write(b"chain-v2\n")
        self.assertEqual(self.cli("check", "chain-v2"), (0, ""))


class ChainPassThrough(Tree):
    """The encoding and the phase reach the target (tests 38 and 39)."""

    def test_window_driver_passes_the_encoding(self):
        fake = os.path.join(self.root, "fake-fuzzer")
        open(fake, "w").write(
            "#!/usr/bin/env python3\nimport json, os, sys\n"
            "STOP = next(a.split('=', 1)[1] for a in sys.argv if a.startswith('-stop_file='))\n"
            "json.dump(dict(os.environ), open(os.path.join(os.path.dirname(STOP), 'env.json'), 'w'))\n"
            "print('#2\\tINITED cov: 5 ft: 9', flush=True)\n")
        os.chmod(fake, 0o700)
        for prefix, want in ((("--encoding", "chain-v2", "--farm", os.path.join(self.root, "stop")), "chain-v2"),
                             (("--encoding", "chain-v1"), "chain-v1"),
                             (("--farm", os.path.join(self.root, "stop")), None)):
            with self.subTest(prefix=prefix):
                seg = os.path.join(self.root, "seg")
                shutil.rmtree(seg, ignore_errors=True)
                os.mkdir(seg)
                r = subprocess.run([sys.executable, "-I", driver_script(self.root), *prefix, seg, "2", "600", self.root,
                                    CHAIN_V2, "v24.0", "run", fake, f"-stop_file={seg}/stop", "corpus"],
                                   capture_output=True, text=True, timeout=300,
                                   env={**os.environ, "NODE_FUZZ_CERTIFY_POLL": "0.2", "NODE_FUZZ_CERTIFY_GRACE": "300",
                                        "NODE_L1_CHAIN_ENCODING": "chain-v9"})
                env = json.load(open(os.path.join(seg, "env.json")))
                self.assertEqual(env.get("NODE_L1_CHAIN_ENCODING"), want, r.stderr)
                self.assertEqual((env["NODE_FUZZ_PIN"], env["NODE_FUZZ_RUN"]), ("v24.0", "run"))

    def test_run_sh_passes_the_encoding_and_the_phase(self):
        text = open(fuzzctl.RUN_SH).read()
        # Every clean environment the target runs in: the segment loop's and the replays'.
        sites = [m.start() for m in re.finditer(r"env -i ", text)]
        self.assertEqual(len(sites), 2)
        for at in sites:
            site = text[at:text.index("/usr/bin/time", at)]
            self.assertTrue('${encoding:+NODE_L1_CHAIN_ENCODING="$encoding"}' in site or '"${census[@]}"' in site, site)
        replay = text[text.index("replay_corpus() {"):text.index("no_findings() {")]
        self.assertIn('census=(NODE_L1_CHAIN_ENCODING="$encoding" NODE_L1_CHAIN_STATS="$seg/census.json"', replay)
        self.assertIn('NODE_L1_CHAIN_PHASE="$2")', replay)
        self.assertIn('"${census[@]}"', replay)
        self.assertIn('mkdir -m 700 "$seg"', replay)
        self.assertEqual(text.count('replay_before="$(replay_corpus segment-2 pre-replay)"'), 1)
        self.assertEqual(text.count('replay_after="$(replay_corpus segment-4 post-replay)"'), 1)
        self.assertEqual(text.count('replay_corpus segment-'), 2)
        # Both driver calls, the farm's and the certify window's.
        drivers = [m.start() for m in re.finditer(r'python3 -I "\$here/window_driver.py"', text)]
        self.assertEqual(len(drivers), 2)
        for at in drivers:
            self.assertTrue(text.startswith('python3 -I "$here/window_driver.py" ${encoding:+--encoding "$encoding"} ', at))
        # The window starts only when the census floor gate passed.
        gate = text.index('python3 -I "$here/census_floor.py" gate')
        window = text.index('seg="$run/logs/segment-3"')
        self.assertLess(gate, window)
        self.assertIn('[ "${census_status:-0}" = 0 ]; then\n        seg="$run/logs/segment-3"', text)
        # At build time, under the build lock and before the run counts as built: the encoding the
        # binary is built for, and the sha256 of the floor the gate judges by (`missing` without
        # one), hashed from the tree, not from the gate's copy.
        floor = '"$root/node-l1/census/$target.floor.json"'
        self.assertIn(f'floor_src={floor}', text)
        self.assertIn(f'--floor-src {floor}', text)
        record = text.index('>"$run/encoding"')
        sha = text.index('>"$run/floor.sha256"')
        self.assertIn('if [ -f "$floor_src" ]; then sha256sum <"$floor_src" | cut -c1-64; else echo missing; fi',
                      text[record:sha])
        self.assertLess(text.index('bin="$run/bin"'), record)
        self.assertLess(sha, text.index('exec 9>&-', sha))
        self.assertLess(sha, text.index(': >"$run/built"'))
        self.assertEqual(text.count('"$run/floor.sha256"'), 1)


class EncodingTriage(Tree):
    """Triage moves no vector of a chain target across encodings (test 41)."""

    RUN = "20261108T000000Z-v24.0-1"

    def finding(self, marker, run_encoding):
        tdir = os.path.join(self.fdir, CHAIN_V2)
        os.makedirs(tdir, exist_ok=True)
        if marker:
            open(os.path.join(tdir, "ENCODING"), "w").write(marker + "\n")
        run = self.make_run(CHAIN_V2, self.RUN, divergences=[(b"chain input", "outcome")])
        if run_encoding:
            open(os.path.join(run, "encoding"), "w").write(run_encoding + "\n")
        return tdir

    def vectors(self, sub):
        return os.listdir(os.path.join(self.fdir, CHAIN_V2, "regress", sub)) if os.path.isdir(
            os.path.join(self.fdir, CHAIN_V2, "regress", sub)) else []

    def test_triage_refuses_a_vector_across_encodings(self):
        for marker, run_encoding in (("chain-v1", "chain-v2"), ("chain-v2", None), ("chain-v2", "chain-v1"), (None, "chain-v2")):
            with self.subTest(marker=marker, run=run_encoding):
                shutil.rmtree(os.path.join(self.fdir, CHAIN_V2), ignore_errors=True)
                self.finding(marker, run_encoding)
                tri = fuzzctl.run_triage(CHAIN_TARGETS, minimize=False)
                self.assertEqual(tri.stats["errors"], 1)
                self.assertEqual(self.state(CHAIN_V2)["run_errors"], {self.RUN: 1})
                self.assertEqual(self.vectors("open"), [])
                self.assertIn("a run whose triage failed", fuzzctl.eligibility(self.fdir, CHAIN_V2, "v24.0"))
        # Within its encoding the vector is made, and moves to fixed/ only while the tree is still
        # of the registry's encoding.
        shutil.rmtree(os.path.join(self.fdir, CHAIN_V2))
        tdir = self.finding("chain-v2", "chain-v2")
        tri = fuzzctl.run_triage(CHAIN_TARGETS, minimize=False)
        self.assertEqual(tri.stats["errors"], 0)
        [vector] = self.vectors("open")
        path = os.path.join(tdir, "triage.json")
        state = json.load(open(path))
        [c] = state["clusters"].values()
        c.update(state="fixed", fixed_stamp="20261108T010000Z", fixed_at="0123456789ab")
        fuzzctl.write_private(path, json.dumps(state))
        open(os.path.join(tdir, "ENCODING"), "w").write("chain-v1\n")
        tri = fuzzctl.run_triage(CHAIN_TARGETS, minimize=False)
        self.assertEqual((tri.stats["errors"], tri.stats["repaired"]), (1, 0))
        self.assertEqual((self.vectors("open"), self.vectors("fixed")), ([vector], []))
        # The fixed cluster's vector stays in open/: the leg is not eligible (D4).
        self.assertTrue(any("regression vector" in r for r in fuzzctl.eligibility(self.fdir, CHAIN_V2, "v24.0")))
        open(os.path.join(tdir, "ENCODING"), "w").write("chain-v2\n")
        tri = fuzzctl.run_triage(CHAIN_TARGETS, minimize=False)
        self.assertEqual((tri.stats["errors"], tri.stats["repaired"]), (0, 1))
        self.assertEqual((self.vectors("open"), self.vectors("fixed")), ([], [vector]))

    def test_recheck_fixes_no_vector_across_encodings(self):
        tdir = self.finding("chain-v2", "chain-v2")
        fuzzctl.run_triage(CHAIN_TARGETS, minimize=False)
        [vector] = self.vectors("open")
        # The vector no longer diverges: a recheck would fix it, but the tree is now marked with
        # another encoding.
        runner = FakeRunner(self)
        runner.fixed.add(hashlib.sha256(b"chain input").hexdigest())
        open(os.path.join(tdir, "ENCODING"), "w").write("chain-v1\n")
        tri = fuzzctl.run_triage(CHAIN_TARGETS, recheck=True, minimize=False, runner=runner)
        self.assertEqual((tri.stats["fixed_now"], tri.stats["errors"]), (0, 1))
        self.assertEqual((self.vectors("open"), self.vectors("fixed")), ([vector], []))
        self.assertEqual([c["state"] for c in self.state(CHAIN_V2)["clusters"].values()], ["open"])
        self.assertIn("an open finding", fuzzctl.eligibility(self.fdir, CHAIN_V2, "v24.0"))
        open(os.path.join(tdir, "ENCODING"), "w").write("chain-v2\n")
        tri = fuzzctl.run_triage(CHAIN_TARGETS, recheck=True, minimize=False, runner=runner)
        self.assertEqual((tri.stats["fixed_now"], tri.stats["errors"]), (1, 0))
        self.assertEqual((self.vectors("open"), self.vectors("fixed")), ([], [vector]))


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


# ---------------------------------------------------------------------------------------------
# BL-841: the replay share, slice lengths from it, and the farm's corpus minimisation.

def worker(cpu, log="fuzz-0.log"):
    """A process_inited entry as window_driver.py writes it; `cpu` None: no INITED seen."""
    if cpu is None:
        return {"log": log, "pid": None, "seconds_to_inited": None, "cpu_at_inited": None}
    return {"log": log, "pid": 4242, "seconds_to_inited": 10.0, "cpu_at_inited": cpu}


def segment(run, n, procs):
    seg = os.path.join(run, "logs", f"segment-{n}")
    os.makedirs(seg, exist_ok=True)
    json.dump({"process_inited": procs}, open(os.path.join(seg, "segment.json"), "w"))


def done(leg, status="pass", cpu_hours=0.5, replay=None, jobs=2, wall_s=1000, kind=None, sid=None):
    """A done row; `replay` (sum, max, workers, inited) adds a slice's replay fields."""
    row = {"event": "done", "slice": sid or secrets_hex(), "leg": leg, "status": status, "run": "r",
           "rev": "-", "recovered": False, "cpu_hours": cpu_hours, "executions": 1, "slow_units": 0,
           **{k: 0 for k in fuzzctl.FINDING_KEYS}}
    if kind:
        row["kind"] = kind
    else:
        row.update(jobs=jobs, wall_s=wall_s)
    if replay:
        row.update(zip(fuzzctl.REPLAY_KEYS, replay))
    return row


def secrets_hex():
    return os.urandom(8).hex()


class ReplayCpu(Tree):
    def run_dir(self):
        return os.path.join(self.fdir, PUB.name, "runs", secrets_hex())

    def test_the_replay_is_each_worker_s_cpu_at_inited_over_every_segment(self):
        run = self.run_dir()
        segment(run, 1, [worker(100.0), worker(150.5, "fuzz-1.log")])
        self.assertEqual(fuzzctl.replay_cpu(run), {"replay_cpu_s": 250.5, "replay_max_s": 150.5,
                                                   "workers": 2, "inited_workers": 2})
        segment(run, 2, [worker(90.0), worker(95.0, "fuzz-1.log")])
        self.assertEqual(fuzzctl.replay_cpu(run), {"replay_cpu_s": 435.5, "replay_max_s": 150.5,
                                                   "workers": 4, "inited_workers": 4})

    def test_a_worker_without_inited_leaves_the_slice_unknown(self):
        run = self.run_dir()
        segment(run, 1, [worker(100.0), worker(None, "fuzz-1.log")])
        r = fuzzctl.replay_cpu(run)
        self.assertEqual((r["workers"], r["inited_workers"], r["replay_cpu_s"]), (2, 1, 100.0))
        self.assertFalse(fuzzctl.replay_counted(done(PUB.name + "@v24.0", replay=tuple(r.values()))))

    def test_a_segment_without_its_record_is_unknown_and_a_malformed_one_is_an_error(self):
        run = self.run_dir()
        segment(run, 1, [worker(100.0)])
        os.makedirs(os.path.join(run, "logs", "segment-2"))
        self.assertIsNone(fuzzctl.replay_cpu(run))
        self.assertIsNone(fuzzctl.replay_cpu(os.path.join(self.fdir, "nowhere")))
        for bad in ([{"log": "fuzz-0.log", "pid": 1, "cpu_at_inited": 3.0}],  # a key missing
                    [{**worker(5.0), "pid": None}],  # a CPU figure without its process
                    []):  # a farm segment always has a process
            run = self.run_dir()
            segment(run, 1, bad)
            with self.assertRaises(ValueError):
                fuzzctl.replay_cpu(run)
        run = self.run_dir()
        os.makedirs(os.path.join(run, "logs", "segment-1"))
        open(os.path.join(run, "logs", "segment-1", "segment.json"), "w").write("{")
        with self.assertRaises(ValueError):
            fuzzctl.replay_cpu(run)


class ReplayRows(Tree):
    LEG = "kv_model@v24.0"

    def test_the_share_counts_whole_passing_slices_and_every_minimisation(self):
        rows = [done(self.LEG, cpu_hours=1.0, replay=(720.0, 400.0, 2, 2)),  # 0.2
                done(self.LEG, cpu_hours=1.0, replay=(1080.0, 600.0, 2, 2)),  # 0.3, the worst
                done(self.LEG, status="interrupted", replay=(100.0, 100.0, 2, 2)),  # unknown
                done(self.LEG, replay=(100.0, 100.0, 2, 1)),  # a worker without INITED: unknown
                done(self.LEG)]  # an older row, no replay: unknown
        r = fuzzctl.replay_share(rows)
        self.assertAlmostEqual(r["share"], 1800 / 7200)
        self.assertAlmostEqual(r["max"], 0.3)
        self.assertEqual((r["counted"], r["unknown"]), (2, 3))
        rows.append(done(self.LEG, cpu_hours=0.2, kind=fuzzctl.CMIN))  # 720 CPU-s, all replay
        r = fuzzctl.replay_share(rows)
        self.assertAlmostEqual(r["share"], (1800 + 720) / (7200 + 720))
        self.assertEqual((r["counted"], r["unknown"], r["cmin_cpu_s"]), (2, 3, 720))
        self.assertEqual(fuzzctl.replay_share([done(self.LEG)])["share"], None)

    def test_old_and_new_rows_load_and_bad_replay_fields_fail_closed(self):
        legs_ = {self.LEG: 1.0}
        start = lambda sid, **kw: {"event": "start", "slice": sid, "leg": self.LEG, "time": 0, **kw}
        old, new, cmin = "0" * 15 + "1", "0" * 15 + "2", "0" * 15 + "3"
        old_row = done(self.LEG, sid=old)
        del old_row["jobs"], old_row["wall_s"]
        rows = [start(old), old_row, start(new), done(self.LEG, sid=new, replay=(1.0, 1.0, 2, 2)),
                start(cmin, kind="cmin"), done(self.LEG, sid=cmin, kind="cmin")]
        d = os.path.join(self.root, "night")
        os.makedirs(d)
        open(fuzzctl.ledger_path(d), "w").write("".join(json.dumps(r) + "\n" for r in rows))
        self.assertEqual(len(fuzzctl.load_records(d, legs_)), 3)
        for bad in ({"replay_cpu_s": 1.0}, {"inited_workers": 3, "workers": 2, "replay_cpu_s": 1.0, "replay_max_s": 1.0},
                    {"kind": "other"}, {"jobs": 0}):
            self.assertFalse(fuzzctl.valid_row({**done(self.LEG), **bad}, legs_), bad)
        # A done row's kind is its start's.
        open(fuzzctl.ledger_path(d), "w").write(json.dumps(start(old)) + "\n" + json.dumps(done(self.LEG, sid=old, kind="cmin")) + "\n")
        with self.assertRaises(fuzzctl.FarmStateError):
            fuzzctl.load_records(d, legs_)

    def test_cmin_rows_count_in_the_cap_but_earn_no_credit(self):
        rows = [done(self.LEG, cpu_hours=0.5), done(self.LEG, cpu_hours=0.25, kind=fuzzctl.CMIN)]
        self.assertEqual(fuzzctl.credit_by_leg(rows), {self.LEG: 0.5})
        self.assertEqual(fuzzctl.total(rows, "cpu_hours"), 0.75)
        rows.append(done(self.LEG, status="finding", kind=fuzzctl.CMIN))  # a finding still voids it
        self.assertEqual(fuzzctl.credit_by_leg(rows), {self.LEG: 0.0})


class ReplayRunner(SliceRunner):
    """A slice that leaves a run with one farm segment whose workers each replayed `replay` CPU-s;
    a minimisation that passes with `files_after` files (or fails)."""

    def __init__(self, tree, replay=100.0, cmin_ok=True, files_after=3, **kw):
        super().__init__(**kw)
        self.tree, self.replay, self.cmin_ok, self.files_after = tree, replay, cmin_ok, files_after
        self.cmins = []

    def __call__(self, target, pin, args, on_start=None, timeout=None, cancel=None):
        if "--minimize-corpus" in args:
            self.cmins.append((target, pin))
            verdict = {"status": "pass" if self.cmin_ok else "fail", "cpu_hours": 0.05, "executions": 0,
                       "divergences": 0, "crashes": 0, "sink_failures": 0, "rust_dashcore": "abc",
                       "cmin": {"files_after": self.files_after}}
            return Leg(target, pin, "node-fuzz cmin", 0 if self.cmin_ok else 1, "", verdict)
        leg = super().__call__(target, pin, args, on_start, timeout, cancel)
        run = os.path.join(self.tree.fdir, target, "runs", f"r{len(self.calls)}-{pin}")
        record(run)
        segment(run, 1, [worker(self.replay, f"fuzz-{i}.log") for i in range(2)])
        return Leg(leg.target, leg.pin, leg.line, leg.code, run, leg.verdict)


class ReplayFarm(Tree):
    farm = Farming.farm

    def test_a_slice_s_done_row_carries_its_replay_and_the_summary_its_share(self):
        f, logs = self.farm(ReplayRunner(self, replay=360.0), slice_minutes=30)
        self.assertEqual(f.work(), "complete")
        rows = [r for r in fuzzctl.load_records(f.dir) if r.get("kind") != fuzzctl.CMIN]
        self.assertTrue(rows)
        self.assertTrue(all(r["replay_cpu_s"] == 720.0 and r["replay_max_s"] == 360.0 and r["workers"] == 2
                            and r["inited_workers"] == 2 and r["jobs"] == 2 for r in rows), rows)
        want = fuzzctl.replay_share(fuzzctl.load_records(f.dir))
        self.assertIn(f" replay_share={want['share']:.3f} replay_share_max={want['max']:.3f} replay_unknown=0 ",
                      logs[-1] + " ")

    def test_a_malformed_segment_record_leaves_the_slice_unknown(self):
        runner = ReplayRunner(self)
        real = runner.__call__

        def broken(*a, **kw):
            leg = real(*a, **kw)
            if leg.run_dir:
                segment(leg.run_dir, 1, [{"log": "x"}])
            return leg
        f, logs = self.farm(broken)
        f.work()
        self.assertTrue(all("replay_cpu_s" not in r for r in fuzzctl.load_records(f.dir)))
        self.assertIn("node-fuzz farm: a slice's segment record is malformed; its replay is unknown", logs)
        self.assertNotIn("replay_unknown=0 ", logs[-1])

    def sized(self, records, leg="diff_tx_decode@v24.0", **kw):
        f, logs = self.farm(SliceRunner(), **kw)
        return f.slice_seconds(leg, records), logs

    def test_a_leg_s_slice_is_sized_by_its_last_counted_slice_and_clamped(self):
        leg = "diff_tx_decode@v24.0"
        # 0.5 CPU-h over 2 jobs and 1000 s: 0.9 CPU-s per wall second per worker; 600 CPU-s of
        # replay at 0.20 needs 3333 s.
        rows = [done(leg, replay=(1200.0, 600.0, 2, 2))]
        self.assertAlmostEqual(self.sized(rows)[0], 600 / (0.2 * 0.9))
        self.assertAlmostEqual(self.sized(rows, replay_target=0.25)[0], 600 / (0.25 * 0.9))
        self.assertEqual(self.sized([done(leg, replay=(10.0, 5.0, 2, 2))])[0], 30 * 60)  # the floor
        self.assertEqual(self.sized([])[0], 30 * 60)  # no history: --slice-minutes
        # An interrupted slice's replay is not counted, but each worker's figure is still a bound.
        self.assertAlmostEqual(self.sized([done(leg, status="interrupted", replay=(1200.0, 600.0, 2, 2))])[0],
                               600 / (0.2 * 0.9))
        self.assertEqual(self.sized([done("diff_tx_decode@v25.0", replay=(1200.0, 600.0, 2, 2))])[0], 30 * 60)
        # The cap binds: replay-bound, logged once.
        big = [done(leg, replay=(9000.0, 4500.0, 2, 2))]
        f, logs = self.farm(SliceRunner(), max_slice_minutes=120)
        self.assertEqual(f.slice_seconds(leg, big), 120 * 60)
        self.assertEqual(f.slice_seconds(leg, big), 120 * 60)
        bound = [x for x in logs if "is replay-bound" in x]
        self.assertEqual(bound, ["node-fuzz farm: a private-route leg is replay-bound: replay 4500 CPU-s needs a "
                                 "417 min slice, cap 120"])

    def test_the_estimate_restarts_after_the_target_s_minimisation(self):
        leg = "diff_tx_decode@v24.0"
        before = done(leg, replay=(1200.0, 600.0, 2, 2))
        after = done(leg, replay=(720.0, 360.0, 2, 2))
        cmin = lambda status="pass", on=leg: done(on, status=status, kind=fuzzctl.CMIN)
        self.assertEqual(self.sized([before, cmin()])[0], 30 * 60)
        self.assertAlmostEqual(self.sized([before, cmin(), after])[0], 360 / (0.2 * 0.9))
        # A failed minimisation changed nothing, and another target's is not this one's; one on
        # the target's other pin is (they share its corpus).
        self.assertAlmostEqual(self.sized([before, cmin("fail")])[0], 600 / (0.2 * 0.9))
        self.assertAlmostEqual(self.sized([before, cmin(on="kv_model@v24.0")])[0], 600 / (0.2 * 0.9))
        self.assertEqual(self.sized([before, cmin(on="diff_tx_decode@v25.0")])[0], 30 * 60)

    def test_a_slice_that_ran_out_during_its_replay_sizes_the_next_from_a_lower_bound(self):
        leg = "diff_tx_decode@v24.0"
        # No worker reached INITED: 0.5 CPU-h over 2 workers is 900 CPU-s each, all of it replay,
        # at 0.9 CPU-s per wall second.
        ended = done(leg, status="fail", replay=(0.0, 0.0, 2, 0))
        self.assertAlmostEqual(self.sized([ended])[0], 900 / (0.2 * 0.9))
        # One did: its replay is the bound.
        self.assertAlmostEqual(self.sized([done(leg, status="fail", replay=(700.0, 700.0, 2, 1))])[0], 700 / (0.2 * 0.9))
        # A later bound raises a counted slice's figure, never lowers it; a later counted slice replaces
        # it; a passing minimisation clears it.
        counted = done(leg, replay=(1200.0, 600.0, 2, 2))
        self.assertAlmostEqual(self.sized([counted, ended])[0], 900 / (0.2 * 0.9))
        low = done(leg, status="fail", cpu_hours=0.1, replay=(0.0, 0.0, 2, 0))  # 180 CPU-s each
        self.assertAlmostEqual(self.sized([counted, low])[0], 600 / (0.2 * 0.9))
        self.assertAlmostEqual(self.sized([ended, counted])[0], 600 / (0.2 * 0.9))
        self.assertEqual(self.sized([ended, done(leg, kind=fuzzctl.CMIN)])[0], 30 * 60)
        # Over the cap: replay-bound, logged.
        f, logs = self.farm(SliceRunner(), max_slice_minutes=60)
        self.assertEqual(f.slice_seconds(leg, [ended]), 60 * 60)
        self.assertIn("node-fuzz farm: a private-route leg is replay-bound: replay 900 CPU-s needs a 83 min slice, "
                      "cap 60", logs)

    def test_replay_floor(self):
        leg = "diff_tx_decode@v24.0"
        self.assertEqual(fuzzctl.replay_floor(done(leg, status="interrupted", replay=(0.0, 0.0, 4, 0))), 450.0)
        self.assertEqual(fuzzctl.replay_floor(done(leg, status="fail", replay=(900.0, 500.0, 4, 2))), 500.0)
        for row in (done(leg, replay=(1200.0, 600.0, 2, 2)), done(leg, status="fail"),
                    done(leg, status="fail", replay=(0.0, 0.0, 0, 0)), done(leg, kind=fuzzctl.CMIN)):
            self.assertEqual(fuzzctl.replay_floor(row), 0.0, row)

    def night_rows(self, night, rows):
        """A night of its own with `rows` (start time, done row) in its ledger."""
        f, _ = self.farm(SliceRunner(), night=night)
        with open(fuzzctl.ledger_path(f.dir), "a") as out:
            for t, r in rows:
                out.write(json.dumps({"event": "start", "slice": r["slice"], "leg": r["leg"], "time": t,
                                      **({"kind": r["kind"]} if "kind" in r else {})}) + "\n")
                out.write(json.dumps(r) + "\n")
        f.release()

    def test_the_estimate_carries_across_nights_in_start_order_until_a_minimisation(self):
        leg = "diff_tx_decode@v24.0"
        self.night_rows("n2", [(10, done(leg, replay=(1200.0, 600.0, 2, 2)))])
        f, logs = self.farm(SliceRunner(), night="n3")
        self.assertAlmostEqual(f.slice_seconds(leg, []), 600 / (0.2 * 0.9))
        # Tonight's slices come after the earlier nights'.
        self.assertAlmostEqual(f.slice_seconds(leg, [done(leg, replay=(720.0, 360.0, 2, 2))]), 360 / (0.2 * 0.9))
        f.release()
        # A night named earlier whose minimisation started later restarts the estimate; a lower
        # bound from a night after that sizes it.
        self.night_rows("n1", [(20, done(leg, kind=fuzzctl.CMIN))])
        f, _ = self.farm(SliceRunner(), night="n4")
        self.assertEqual(f.slice_seconds(leg, []), 30 * 60)
        f.release()
        self.night_rows("n5", [(30, done(leg, status="fail", replay=(0.0, 0.0, 2, 0)))])
        # A night without a plan, or with a bad ledger, is skipped and counted.
        os.makedirs(fuzzctl.farm_state_dir(self.fdir, "n6"))
        os.makedirs(fuzzctl.farm_state_dir(self.fdir, "n7"))
        shutil.copy(os.path.join(fuzzctl.farm_state_dir(self.fdir, "n5"), "plan.json"),
                    fuzzctl.farm_state_dir(self.fdir, "n7"))
        open(fuzzctl.ledger_path(fuzzctl.farm_state_dir(self.fdir, "n7")), "w").write("{\n")
        f, logs = self.farm(SliceRunner(), night="n8")
        self.assertAlmostEqual(f.slice_seconds(leg, []), 900 / (0.2 * 0.9))
        self.assertIn("node-fuzz farm: 2 earlier nights are unreadable; their slices size none of this one's", logs)

    def test_a_leg_whose_slices_run_out_replaying_grows_them_and_retires_only_at_the_cap(self):
        for cap, want in ((360, [1800, 9000, 21600, 21600]), (30, [1800, 1800])):
            with self.subTest(cap=cap):
                clock = FakeClock()
                runner = ReplayEndedRunner(self, clock)
                f, logs = self.farm(runner, clock=clock, night=f"cap{cap}", only=["kv_model"], cores=2, jobs=2,
                                    hours=24, max_slice_minutes=cap)
                self.assertEqual(f.work(), "legs-retired")
                self.assertEqual([round(c * 3600 / 2) for _, _, c in runner.calls], want)
                rows = fuzzctl.load_records(f.dir)
                self.assertEqual({r["status"] for r in rows}, {"fail"})
                self.assertTrue(all(r["inited_workers"] == 0 and r["workers"] == 2 for r in rows), rows)

    def seed(self, f, rows):
        with open(fuzzctl.ledger_path(f.dir), "a") as out:
            for r in rows:
                out.write(json.dumps({"event": "start", "slice": r["slice"], "leg": r["leg"], "time": 0,
                                      **({"kind": r["kind"]} if "kind" in r else {})}) + "\n")
                out.write(json.dumps(r) + "\n")

    def first_cpu(self, runner, leg):
        return next(c for t, p, c in runner.calls if f"{t}@{p}" == leg)

    def test_two_legs_get_their_own_slices_within_the_deadline_and_windows(self):
        sized, plain = "diff_tx_decode@v24.0", "diff_tx_decode@v25.0"
        # 1800 s wanted against a 600 s floor.
        # 0.5 CPU-h over 2 jobs and 1000 s is 0.9 CPU-s per wall second; 324 CPU-s of replay at
        # 0.20 wants 1800 s, against a 600 s floor. Then capped by a 540 s deadline, and by a
        # window 900 s away.
        history = [done(sized, cpu_hours=0.5, replay=(648.0, 324.0, 2, 2))]
        cases = ((dict(), 1.0, 1 / 3), (dict(hours=0.15, cores=40), 0.3, 0.3),
                 (dict(bench=NoBench(("a reserved bench window", 900))), 0.5, 1 / 3))
        for n, (kw, want_sized, want_plain) in enumerate(cases):
            with self.subTest(kw=kw):
                runner = SliceRunner()
                bench = kw.pop("bench", None)
                f, _ = self.farm(runner, bench, slice_minutes=10, night=f"n{n}", **kw)
                self.seed(f, history)
                f.work()
                self.assertAlmostEqual(self.first_cpu(runner, sized), want_sized, places=1)
                self.assertAlmostEqual(self.first_cpu(runner, plain), want_plain, places=2)

    def test_the_bench_is_asked_about_the_longest_slice_a_leg_wants(self):
        horizons = []

        class Bench(NoBench):
            def blocked(self, horizon):
                horizons.append(horizon)
                return None
        f, _ = self.farm(SliceRunner(), Bench(), slice_minutes=10)
        self.seed(f, [done("diff_tx_decode@v24.0", replay=(1200.0, 600.0, 2, 2))])
        f.work()
        self.assertGreaterEqual(max(horizons), 600 / (0.2 * 0.9))


class FakeClock:
    def __init__(self):
        self.t = 0.0

    def __call__(self):
        return self.t


class ReplayEndedRunner(ReplayRunner):
    """Every slice runs out during its corpus load (run.sh's failed verdict and its reason; no
    worker reached INITED), and moves the clock by the slice's planned length."""

    def __init__(self, tree, clock, jobs=2):
        super().__init__(tree)
        self.clock, self.jobs = clock, jobs

    def __call__(self, target, pin, args, on_start=None, timeout=None, cancel=None):
        cpu = float(args[args.index("--cpu-hours") + 1])
        with self.lock:
            self.calls.append((target, pin, cpu))
            run = os.path.join(self.tree.fdir, target, "runs", f"r{len(self.calls)}-{pin}")
        self.clock.t += cpu * 3600 / self.jobs
        record(run)
        segment(run, 1, [worker(None, f"fuzz-{i}.log") for i in range(2)])
        verdict = {"status": "fail", "reason": fuzzctl.BEFORE_INITED, "cpu_hours": cpu, "executions": 0,
                   "divergences": 0, "crashes": 0, "sink_failures": 0}
        return Leg(target, pin, f"node-fuzz target={target} pin={pin}", 1, run, verdict)


class Minimising(Tree):
    farm = Farming.farm

    LEG = "kv_model@v24.0"

    def corpus(self, n, target=PUB.name, pre=False):
        tdir = os.path.join(self.fdir, target)
        for d in ("corpus",) + (("corpus.pre-cmin",) if pre else ()):
            os.makedirs(os.path.join(tdir, d), exist_ok=True)
        for k in range(n):
            open(os.path.join(tdir, "corpus", f"in-{k}"), "w").write(str(k))
        return tdir

    def history(self, files):
        fuzzctl.append_row(self.fdir, {"target": PUB.name, "files_after": files, "night": "x", "time": 0},
                           name=fuzzctl.CMIN_HISTORY)

    def test_a_minimisation_is_due_without_one_and_after_the_corpus_grows_by_the_threshold(self):
        f, _ = self.farm(SliceRunner())
        self.assertFalse(f.cmin_due(self.LEG))  # no corpus
        self.corpus(100)
        self.assertTrue(f.cmin_due(self.LEG))  # no minimisation yet
        self.history(80)
        self.assertTrue(f.cmin_due(self.LEG))  # 100 >= 80 x 1.25
        self.history(81)
        self.assertFalse(f.cmin_due(self.LEG))  # 100 < 101.25: grown 23%
        self.history(100)
        self.corpus(124)
        self.assertFalse(f.cmin_due(self.LEG))  # 24%
        self.corpus(125)
        self.assertTrue(f.cmin_due(self.LEG))  # 25%

    def test_no_minimisation_of_a_floor_gated_target_or_while_the_old_corpus_is_kept(self):
        gated = Target("diff_chain_s0", "l1", ("v24.0",), "private", 60, 1, encoding="chain-v2")
        legacy = Target("diff_chain", "l1", ("v24.0",), "private", 60, 1, encoding="chain-v1")
        f = fuzzctl.Farm(farm_args(night="gated"), TARGETS + [gated, legacy], NoBench(), runner=SliceRunner(),
                         log=lambda _: None)
        self.corpus(10, gated.name)
        self.corpus(10, legacy.name)
        self.assertFalse(f.cmin_due("diff_chain_s0@v24.0"))
        self.assertTrue(f.cmin_due("diff_chain@v24.0"))
        f.release()
        f, _ = self.farm(SliceRunner())
        tdir = self.corpus(10, pre=True)
        self.assertFalse(f.cmin_due(self.LEG))  # the last one's old corpus is still kept
        shutil.rmtree(os.path.join(tdir, "corpus"))
        self.assertTrue(f.cmin_due(self.LEG))  # cut between its renames: the mode rolls it back

    def test_the_farm_minimises_before_a_leg_s_first_slice_and_counts_it(self):
        self.corpus(4)
        runner = ReplayRunner(self, files_after=3)
        f, logs = self.farm(runner, cmin=True)
        self.assertEqual(f.work(), "cap")  # the minimisation's CPU-h came out of the night's cap
        self.assertEqual(runner.cmins, [("kv_model", "v24.0")])
        records = fuzzctl.load_records(f.dir)
        cmin = [r for r in records if r.get("kind") == fuzzctl.CMIN]
        self.assertEqual([(r["leg"], r["status"], r["cpu_hours"]) for r in cmin], [(self.LEG, "pass", 0.05)])
        first = next(i for i, r in enumerate(records) if r["leg"] == self.LEG)
        self.assertEqual(records[first].get("kind"), fuzzctl.CMIN)
        self.assertEqual(fuzzctl.load_cmin_history(self.fdir), {PUB.name: 3})
        self.assertIn(" cmin=1 ", logs[-1])
        self.assertAlmostEqual(fuzzctl.total(records, "cpu_hours"), f.plan["cpu_hours"], places=3)
        self.assertAlmostEqual(sum(fuzzctl.credit_by_leg(records).values()), f.plan["cpu_hours"] - 0.05, places=3)
        # 3 files after, 4 now: not grown enough for another.
        f2, _ = self.farm(runner, cmin=True)
        f2.work()
        self.assertEqual(len(runner.cmins), 1)

    def test_a_launch_without_cmin_never_minimises(self):
        self.corpus(4)
        runner = ReplayRunner(self)
        f, logs = self.farm(runner)
        self.assertEqual(f.work(), "complete")
        self.assertEqual(runner.cmins, [])
        self.assertFalse(any(r.get("kind") == fuzzctl.CMIN for r in fuzzctl.load_records(f.dir)))
        self.assertIn(" cmin=0 ", logs[-1])

    def test_a_failed_minimisation_records_no_count_and_the_leg_still_fuzzes(self):
        self.corpus(4)
        runner = ReplayRunner(self, cmin_ok=False)
        f, _ = self.farm(runner, cmin=True)
        self.assertEqual(f.work(), "cap")
        self.assertEqual(fuzzctl.load_cmin_history(self.fdir), {})
        self.assertTrue(any(t == "kv_model" for t, _, _ in runner.calls))

    def fake_rm(self, rc=0):
        path = os.path.join(self.root, "rm-if-idle")
        open(path, "w").write(f'#!/bin/sh\n[ {rc} = 0 ] && rm -rf "$1"\nexit {rc}\n')
        os.chmod(path, 0o700)
        return mock.patch.object(fuzzctl, "RM_IF_IDLE", path)

    def test_the_old_corpus_goes_through_rm_if_idle_after_a_passing_slice_only(self):
        tdir = self.corpus(3, pre=True)
        pre = os.path.join(tdir, "corpus.pre-cmin")
        with self.fake_rm():
            f, _ = self.farm(SliceRunner(fail_on=self.LEG))
            f.work()
            self.assertTrue(os.path.isdir(pre))  # its slices failed
            f, _ = self.farm(SliceRunner(), night="next")
            f.work()
            self.assertFalse(os.path.exists(pre))
        os.makedirs(pre)
        with self.fake_rm(rc=1):
            f, logs = self.farm(SliceRunner(), night="third")
            f.work()
        self.assertTrue(os.path.isdir(pre))
        self.assertIn("node-fuzz farm: a minimisation's old corpus could not be removed; it is kept", logs)

    def test_the_old_corpus_is_never_removed_while_the_corpus_is_missing_or_empty(self):
        tdir = self.corpus(0, pre=True)
        pre = os.path.join(tdir, "corpus.pre-cmin")
        with self.fake_rm():
            f, _ = self.farm(SliceRunner())
            f.work()
            self.assertTrue(os.path.isdir(pre))  # empty
            os.rmdir(os.path.join(tdir, "corpus"))
            f, _ = self.farm(SliceRunner(), night="next")
            f.work()
            self.assertTrue(os.path.isdir(pre))  # missing

    def test_the_rm_if_idle_path_can_be_set_in_the_environment(self):
        tdir = self.corpus(3, pre=True)
        path = os.path.join(self.root, "rm-elsewhere")
        open(path, "w").write('#!/bin/sh\nrm -rf "$1"\n')
        os.chmod(path, 0o700)
        with mock.patch.object(fuzzctl, "RM_IF_IDLE", "/nonexistent/rm-if-idle.sh"), \
                mock.patch.dict(os.environ, {"NODE_FUZZ_RM_IF_IDLE": path}):
            f, _ = self.farm(SliceRunner())
            f.work()
        self.assertFalse(os.path.exists(os.path.join(tdir, "corpus.pre-cmin")))

    def test_farm_status_counts_the_old_corpora_kept_and_the_cut_swaps(self):
        self.corpus(3, pre=True)
        os.makedirs(os.path.join(self.fdir, PRIV.name, "corpus.pre-cmin"))
        with mock.patch.object(fuzzctl, "farm_units", return_value=[]):
            rc, out = self.cli(["farm", "status"])
        self.assertIn("node-fuzz farm minimisations: old_corpora_kept=1 cut_swaps=1\n", out)
        self.assertNotIn(PRIV.name, out)

    def test_a_minimisation_s_findings_are_new_findings(self):
        run = self.make_run(PUB.name, "m1", crashes=[("crash-aaa", b"kv-crash")], log=PANIC_LOG,
                            mode="minimize-corpus")
        open(os.path.join(run, "mode"), "w").write("minimize-corpus\n")
        self.assertFalse(fuzzctl.is_job_run(run))
        tri = fuzzctl.run_triage(TARGETS, runner=FakeRunner(self))
        self.assertEqual(tri.stats["errors"], 0)
        self.assertEqual(len(self.state(PUB.name)["clusters"]), 1)

    def test_cmin_ready_refuses_an_open_cluster_a_job_and_an_unreadable_state(self):
        os.makedirs(os.path.join(self.fdir, P3B.name), exist_ok=True)
        self.assertEqual(fuzzctl.cmin_ready(self.fdir, P3B.name), 0)
        put_cluster(self.fdir, "divergence", "post.persisted", state="fixed")
        self.assertEqual(fuzzctl.cmin_ready(self.fdir, P3B.name), 0)
        put_cluster(self.fdir, "divergence", "post.other")
        self.assertEqual(fuzzctl.cmin_ready(self.fdir, P3B.name), 3)
        os.makedirs(os.path.join(self.fdir, PUB.name, "jobs"))
        self.assertEqual(fuzzctl.cmin_ready(self.fdir, PUB.name), 0)
        open(os.path.join(self.fdir, PUB.name, "jobs", "job-x"), "w").close()
        self.assertEqual(fuzzctl.cmin_ready(self.fdir, PUB.name), 4)
        open(fuzzctl.triage_path(self.fdir, PUB.name), "w").write("{")
        self.assertEqual(fuzzctl.cmin_ready(self.fdir, PUB.name), 5)


class ReplayShareCli(Tree):
    farm = Farming.farm

    PUBLIC, PRIVATE = "kv_model@v24.0", "diff_tx_decode@v24.0"

    def night(self, rows):
        f, _ = self.farm(SliceRunner())
        ReplayFarm.seed(self, f, rows)
        f.release()

    def test_exit_codes_and_lines(self):
        under = [done(self.PUBLIC, cpu_hours=1.0, replay=(720.0, 360.0, 2, 2))]  # 0.2
        self.night(under)
        rc, out = self.cli(["replay-share", "test", "--leg", self.PUBLIC])
        self.assertEqual(rc, 0, out)
        self.assertIn("leg=kv_model@v24.0 slices=1 counted=1 replay_share=0.200 replay_share_max=0.200 replay_unknown=0 "
                      "cmin_cpu_s=0 target=0.25 result=pass", out)
        self.assertIn("kind=fuzz status=pass cpu_s=3600 replay_s=720.0 workers=2/2 share=0.200", out)
        self.assertEqual(self.cli(["replay-share", "test", "--leg", self.PUBLIC, "--target", "0.2"])[0], 1)
        # A minimisation's CPU brings the leg over.
        self.night([done(self.PUBLIC, cpu_hours=0.1, kind=fuzzctl.CMIN)])
        rc, out = self.cli(["replay-share", "test", "--leg", self.PUBLIC])
        self.assertEqual(rc, 1, out)
        self.assertIn("result=over", out)

    def test_one_slice_over_fails_and_no_counted_slice_is_unknown(self):
        self.night([done(self.PUBLIC, cpu_hours=1.0, replay=(360.0, 180.0, 2, 2)),
                    done(self.PUBLIC, cpu_hours=1.0, replay=(1080.0, 540.0, 2, 2)),  # 0.3
                    done(self.PRIVATE, status="interrupted", replay=(1.0, 1.0, 2, 2))])
        self.assertEqual(self.cli(["replay-share", "test", "--leg", self.PUBLIC])[0], 1)
        rc, out = self.cli(["replay-share", "test", "--leg", self.PRIVATE])
        self.assertEqual(rc, 2, out)
        rc, out = self.cli(["replay-share", "test"])
        self.assertEqual(rc, 1, out)
        self.assertIn("leg=private-route ", out)
        self.assertNotIn("diff_tx_decode", out)

    def test_a_bad_leg_or_night_is_refused(self):
        self.night([])
        for argv in (["replay-share", "test", "--leg", "nothing@v24.0"], ["replay-share", "../x"],
                     ["replay-share", "none"], ["replay-share", "test", "--target", "1"]):
            with self.subTest(argv=argv), self.assertRaises(SystemExit):
                self.cli(argv)


class FarmArguments(Tree):
    def test_the_replay_arguments_are_checked(self):
        base = ["farm", "work", "--night", "x"]
        for extra in (["--replay-target", "0"], ["--replay-target", "1"], ["--max-slice-minutes", "20"],
                      ["--cmin-growth", "0"]):
            with self.subTest(extra=extra), self.assertRaises(SystemExit) as e:
                self.cli(base + extra)
            self.assertEqual(e.exception.code, 2)

    def launched(self, *extra):
        with mock.patch.object(fuzzctl, "systemd_launch", return_value=False) as launch, \
                mock.patch.object(fuzzctl, "check_registry", return_value=[]), \
                mock.patch.object(fuzzctl, "farm_units", return_value=[]):
            self.cli(["farm", "launch", "--night", "x", *extra])
        return launch.call_args[0][3]

    def test_launch_passes_them_to_the_worker(self):
        worker = self.launched("--replay-target", "0.15", "--max-slice-minutes", "240", "--cmin-growth", "0.5")
        for flag, value in (("--replay-target", "0.15"), ("--max-slice-minutes", "240.0"), ("--cmin-growth", "0.5")):
            self.assertEqual(worker[worker.index(flag) + 1], value)
        self.assertNotIn("--cmin", worker)
        self.assertIn("--cmin", self.launched("--cmin"))


if __name__ == "__main__":
    unittest.main()
