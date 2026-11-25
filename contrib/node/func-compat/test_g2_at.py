#!/usr/bin/env python3
"""Unit tests for g2-at (G2 at given rust-dashcore heads).

Run with `func-compat.py selftest` or `python3 -m unittest test_g2_at` from this directory.
"""

import argparse
import contextlib
import fcntl
import importlib.util
import io
import json
import os
import re
import signal
import subprocess
import sys
import tempfile
import textwrap
import threading
import time
import unittest
import unittest.mock
from importlib.machinery import SourceFileLoader
from pathlib import Path

from test_func_compat import alive, lock_free, sigterm_when

HERE = Path(__file__).resolve().parent
loader = SourceFileLoader("g2_at", str(HERE / "g2-at"))
spec = importlib.util.spec_from_loader("g2_at", loader)
at = importlib.util.module_from_spec(spec)
loader.exec_module(at)
REAL_RUN_G2 = at.run_g2

COMMITTED = {
    "feature_llmq_signing.py": {"status": "fail", "stage": "start", "class": "unimplemented-option",
                                "detail": "-masternodeblsprivkey"},
    "feature_llmq_signing.py --spork21": {"status": "fail", "stage": "run", "class": "assertion",
                                          "detail": "feature_llmq_signing.py:80"},
    "feature_llmq_chainlocks.py": {"status": "pass", "stage": "pass"},
    "feature_llmq_dkgerrors.py": {"status": "fail", "stage": "setup", "class": "harness", "detail": "x"},
}
GIT_ENV = {"GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1"}
CARGO_TOML = '[package]\nname = "rdashd"\n\n[features]\noracle = []\n'
BUILT_DKG = "built with --features rdashd/dkg-runtime"
BUILT_NONE = "built without dkg-runtime: rdashd has no such feature at this head"
BUILT_DKG_RELEASE = "built with --release --features rdashd/dkg-runtime"


def tmpdir(test):
    return Path(test.enterContext(tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR"))))


class Repo:
    """A throwaway repository: `base`, and branches that each change one thing on it."""

    def __init__(self, root):
        self.root = root
        self.git("init", "-q", "-b", "base")
        self.commit({"a.txt": "1\n2\n3\n", "b.txt": "x\n"}, "base")
        for branch, files in (("left", {"a.txt": "one\n2\n3\n"}), ("right", {"b.txt": "y\n"}),
                              ("third", {"c.txt": "c\n"}), ("clash", {"a.txt": "uno\n2\n3\n"}),
                              ("dkg", {"rdashd/Cargo.toml": CARGO_TOML + 'dkg-runtime = ["dashd-node/dkg-runtime"]\n'}),
                              ("nodkg", {"rdashd/Cargo.toml": CARGO_TOML}),
                              ("badtoml", {"rdashd/Cargo.toml": "[features\n"})):
            self.git("checkout", "-q", "-b", branch, "base")
            self.commit(files, branch)
        self.git("checkout", "-q", "base")

    def git(self, *args):
        return subprocess.run(["git", "-C", str(self.root), "-c", "user.name=t", "-c", "user.email=t@example.com",
                               *args], check=True, capture_output=True, text=True).stdout.strip()

    def commit(self, files, message):
        for name, text in files.items():
            (self.root / name).parent.mkdir(parents=True, exist_ok=True)
            (self.root / name).write_text(text)
        self.git("add", *files)
        self.git("commit", "-q", "-m", message)

    def blob(self, tree, path):
        return self.git("cat-file", "-p", f"{tree}:{path}")

    def heads(self, *names):
        return at.resolve(self.root, names)


class GitCase(unittest.TestCase):
    def setUp(self):
        self.enterContext(unittest.mock.patch.dict(os.environ, GIT_ENV))
        self.repo = Repo(tmpdir(self))


class Resolve(GitCase):
    def test_refs_resolve_to_full_shas(self):
        left = self.repo.git("rev-parse", "left")
        self.assertEqual(at.resolve(self.repo.root, ["left", left[:9]]), [("left", left), (left[:9], left)])

    def test_an_unknown_ref_stops_naming_it(self):
        with self.assertRaises(at.Stop) as e:
            at.resolve(self.repo.root, ["left", "no-such-lane"])
        self.assertIn("'no-such-lane'", str(e.exception))
        self.assertEqual(e.exception.status, 2)


class Compose(GitCase):
    def test_one_ref_is_its_own_tree(self):
        self.assertEqual(at.compose(self.repo.root, self.repo.heads("left")), self.repo.git("rev-parse", "left^{tree}"))

    def test_two_refs_merge_without_a_commit_or_ref(self):
        refs, commits = self.repo.git("for-each-ref"), self.repo.git("rev-list", "--all", "--count")
        tree = at.compose(self.repo.root, self.repo.heads("left", "right"))
        self.assertEqual(self.repo.blob(tree, "a.txt"), "one\n2\n3")
        self.assertEqual(self.repo.blob(tree, "b.txt"), "y")
        self.assertEqual(self.repo.git("for-each-ref"), refs)
        self.assertEqual(self.repo.git("rev-list", "--all", "--count"), commits)

    def test_three_refs_fold_in_order(self):
        tree = at.compose(self.repo.root, self.repo.heads("left", "right", "third"))
        self.assertEqual([self.repo.blob(tree, p) for p in ("a.txt", "b.txt", "c.txt")], ["one\n2\n3", "y", "c"])

    def test_a_conflict_lists_its_paths_and_the_pair(self):
        with self.assertRaises(at.Stop) as e:
            at.compose(self.repo.root, self.repo.heads("left", "right", "clash"))
        message = str(e.exception)
        left, right, clash = (self.repo.git("rev-parse", b)[:12] for b in ("left", "right", "clash"))
        self.assertIn(f"between left ({left}) + right ({right}) and clash ({clash})", message)
        self.assertEqual(message.splitlines()[1:], ["  a.txt"])

    def test_the_worktree_holds_the_composed_tree_until_removed(self):
        heads = self.repo.heads("left", "right")
        tree = at.compose(self.repo.root, heads)
        dest = tmpdir(self) / "src"
        at.materialise(self.repo.root, heads[0][1], tree, dest)
        self.assertEqual((dest / "b.txt").read_text(), "y\n")
        self.assertEqual(self.repo.git("-C", str(dest), "write-tree"), tree)
        self.assertEqual(self.repo.git("-C", str(dest), "rev-parse", "HEAD"), heads[0][1])
        (dest / "target").mkdir()
        at.remove_worktree(self.repo.root, dest)
        self.assertFalse(dest.exists())
        self.assertNotIn(str(dest), self.repo.git("worktree", "list"))


class Features(GitCase):
    def tree(self, ref):
        return self.repo.git("rev-parse", f"{ref}^{{tree}}")

    def test_the_feature_is_read_from_the_trees_rdashd_cargo_toml(self):
        self.assertTrue(at.has_dkg_runtime(self.repo.root, self.tree("dkg")))
        self.assertFalse(at.has_dkg_runtime(self.repo.root, self.tree("nodkg")))
        self.assertFalse(at.has_dkg_runtime(self.repo.root, self.tree("left")))
        with self.assertRaisesRegex(at.Stop, "rdashd/Cargo.toml in"):
            at.has_dkg_runtime(self.repo.root, self.tree("badtoml"))

    def test_the_choice_and_its_line(self):
        for ref, given, want in (("dkg", None, ("rdashd/dkg-runtime", BUILT_DKG)),
                                 ("nodkg", None, ("", BUILT_NONE)),
                                 ("left", None, ("", BUILT_NONE)),
                                 ("nodkg", "a,b", ("a,b", "built with --features a,b (--features given)")),
                                 ("dkg", "", ("", "built without features (--features '' given)"))):
            with self.subTest(ref=ref, given=given):
                self.assertEqual(at.build_features(self.repo.root, self.tree(ref), given), want)
        for ref, given, want in (("dkg", None, ("rdashd/dkg-runtime", BUILT_DKG_RELEASE)),
                                 ("nodkg", None, ("", "built with --release, " + BUILT_NONE.removeprefix("built "))),
                                 ("nodkg", "a,b", ("a,b", "built with --release --features a,b (--features given)")),
                                 ("dkg", "", ("", "built with --release, without features (--features '' given)"))):
            with self.subTest(ref=ref, given=given, release=True):
                self.assertEqual(at.build_features(self.repo.root, self.tree(ref), given, release=True), want)

    def test_the_cargo_command_in_each_case(self):
        base = ["nice", "-n", "19", "cargo", "build", "-p", "rdashd", "-p", "rdash-cli"]
        self.assertEqual(at.cargo_command("rdashd/dkg-runtime"), base + ["--features", "rdashd/dkg-runtime"])
        self.assertEqual(at.cargo_command("a,b"), base + ["--features", "a,b"])
        self.assertEqual(at.cargo_command(""), base)
        self.assertEqual(at.cargo_command("rdashd/dkg-runtime", release=True),
                         base + ["--release", "--features", "rdashd/dkg-runtime"])
        self.assertEqual(at.cargo_command("", release=True), base + ["--release"])

    def test_the_build_runs_and_logs_that_command(self):
        src = tmpdir(self)
        (src / "target/debug").mkdir(parents=True)
        for b in ("rdashd", "rdash-cli"):
            (src / "target/debug" / b).write_text("")
        ran = []
        with unittest.mock.patch.object(at.fc, "run_group", lambda cmd, f, grace, **kw: ran.append(cmd) or 0):
            at.build(src, src / "build.log", "rdashd/dkg-runtime")
        self.assertEqual(ran, [at.cargo_command("rdashd/dkg-runtime")])
        self.assertEqual((src / "build.log").read_text().splitlines()[0],
                         f"g2-at: nice -n 19 cargo build -p rdashd -p rdash-cli --features rdashd/dkg-runtime (in {src})")

    def test_a_release_build_logs_its_profile_and_needs_target_release(self):
        src = tmpdir(self)
        (src / "target/debug").mkdir(parents=True)
        for b in ("rdashd", "rdash-cli"):
            (src / "target/debug" / b).write_text("")
        with unittest.mock.patch.object(at.fc, "run_group", lambda cmd, f, grace, **kw: 0):
            with self.assertRaisesRegex(at.Stop, f"no rdashd and rdash-cli in {src / 'target/release'}"):
                at.build(src, src / "build.log", "rdashd/dkg-runtime", release=True)
            (src / "target/release").mkdir()
            for b in ("rdashd", "rdash-cli"):
                (src / "target/release" / b).write_text("")
            at.build(src, src / "build.log", "rdashd/dkg-runtime", release=True)
        self.assertEqual((src / "build.log").read_text().splitlines()[0],
                         f"g2-at: nice -n 19 cargo build -p rdashd -p rdash-cli --release --features rdashd/dkg-runtime"
                         f" (in {src})")


class Args(unittest.TestCase):
    def test_update_and_what_g2_at_sets_are_refused(self):
        for extra in (["--update"], ["--upd"], ["--u"], ["--rdashd", "x"], ["--out", "o"], ["--pin", "v24.0"], ["--lock-wait", "5"],
                      ["feature_llmq_signing", "--update"]):
            with self.subTest(extra=extra), self.assertRaises(at.Stop) as e:
                at.g2_args(extra, COMMITTED)
            self.assertEqual(e.exception.status, 2)

    def test_options_pass_on_in_full_and_stems_expand(self):
        passed, ns = at.g2_args(["--mn", "3", "--test-t", "60", "feature_llmq_signing", "-j", "2"], COMMITTED)
        self.assertEqual(passed, ["--mn", "3", "--jobs", "2", "--test-timeout", "60",
                                  "feature_llmq_signing.py", "feature_llmq_signing.py --spork21"])
        self.assertEqual(ns.mn, 3)

    def test_runner_names_go_to_g2_unchanged(self):
        passed, _ = at.g2_args(["feature_llmq_signing.py", "feature_llmq_new.py", "rpc_x.py --flag"], COMMITTED)
        self.assertEqual(passed, ["feature_llmq_signing.py", "feature_llmq_new.py", "rpc_x.py --flag"])

    def test_a_stem_not_in_the_matrix_stops(self):
        with self.assertRaises(at.Stop) as e:
            at.g2_args(["feature_llmq_signing", "feature_llmq_new"], COMMITTED)
        self.assertIn("'feature_llmq_new'", str(e.exception))
        self.assertEqual(e.exception.status, 2)

    def test_an_unknown_option_is_a_usage_error(self):
        with self.assertRaises(at.Stop):
            at.g2_args(["--nope"], COMMITTED)


class Gate(unittest.TestCase):
    def test_disk_and_load(self):
        self.assertIsNone(at.gate(200, 299.9))
        self.assertIn("199G free", at.gate(199, 10))
        self.assertIn("load is 300", at.gate(500, 300))

    def test_main_stops_before_any_worktree(self):
        with unittest.mock.patch.dict(os.environ, GIT_ENV):
            repo = Repo(tmpdir(self))
        scratch = tmpdir(self) / "scratch"
        for free, load in ((150, 1.0), (300, 450.0)):
            with self.subTest(free=free, load=load), \
                    unittest.mock.patch.object(at, "free_gb", lambda: free), \
                    unittest.mock.patch.object(at, "load5", lambda: load), \
                    contextlib.redirect_stderr(io.StringIO()) as err:
                status = at.main(["--repo", str(repo.root), "--scratch", str(scratch), "left"])
            self.assertEqual(status, 3)
            self.assertIn("g2-at:", err.getvalue())
            self.assertFalse(scratch.exists())


class Report(unittest.TestCase):
    HEADS = [("node/lane-a", "a" * 40), ("node/lane-b", "b" * 40)]

    def g2_out(self, kind, tests):
        out = tmpdir(self)
        (out / f"{kind}-v24.0.json").write_text(json.dumps({"pin": "v24.0", "mask": "mn:2", "tests": tests}))
        failures = {n: {"first_failure": {"exception": "AssertionError: boom | x"}} for n in tests}
        (out / f"{kind}-v24.0.run.json").write_text(json.dumps({"core_commit": "c" * 40, "tests": failures}))
        return out

    def test_rows_and_delta_against_the_matrix(self):
        tests = {"feature_llmq_signing.py": {"status": "fail", "stage": "setup", "class": "harness", "detail": "d"},
                 "feature_llmq_signing.py --spork21": COMMITTED["feature_llmq_signing.py --spork21"],
                 "feature_llmq_chainlocks.py": {"status": "fail", "stage": "run", "class": "assertion", "detail": "c"},
                 "feature_llmq_new.py": {"status": "pass", "stage": "pass"}}
        entries, meta = at.outcome(self.g2_out("g2-llmq", tests), probe=False)
        rows = {r[0]: r[1:] for r in at.rows(entries, meta, COMMITTED, compared=True, complete=True)}
        self.assertEqual(rows["feature_llmq_signing.py"], ("setup", "harness", "d; AssertionError: boom | x", "progress"))
        self.assertEqual(rows["feature_llmq_signing.py --spork21"][3], "same")
        self.assertEqual(rows["feature_llmq_chainlocks.py"][3], "regression")
        self.assertEqual(rows["feature_llmq_dkgerrors.py"], ("(absent)", "-", "-", "regression"))
        self.assertEqual(rows["feature_llmq_new.py"][3], "not in matrix")
        partial = at.rows(entries, meta, COMMITTED, compared=True, complete=False)
        self.assertNotIn("feature_llmq_dkgerrors.py", [r[0] for r in partial])

        report = at.render({"heads": self.HEADS, "tree": "d" * 40, "harness": "e" * 40, "args": ["--mn", "2"],
                            "core": {"branch": "node/func-compat-g2-v2400", "head": "f" * 40,
                                     "exported": meta["core_commit"]},
                            "timings": {"build": "5 s"}, "notes": [], "rows": at.rows(entries, meta, COMMITTED, True, True),
                            "status": 1, "files": {"g2 log": "/x/g2.log"}})
        lines = report.splitlines()
        self.assertEqual(lines[:2], [f"HEAD {'a' * 40}", f"REF node/lane-b {'b' * 40}"])
        self.assertIn(f"- composed tree: `{'d' * 40}`", lines)
        self.assertIn(f"- Core: `node/func-compat-g2-v2400` at `{'f' * 40}`; g2 exported `{'c' * 40}`", lines)
        self.assertIn("| `feature_llmq_signing.py` | setup | harness | d; AssertionError: boom \\| x | progress |", lines)
        self.assertIn("g2 exit status: 1 (differs from the matrix, or an adaptation is stale)", lines)

    def test_a_known_later_blocker_is_not_compared(self):
        entry = {"status": "fail", "stage": "start", "class": "unimplemented-option", "detail": "-x"}
        noted = {"feature_llmq_signing.py": {**entry, at.fc.LATER: "y; MP1a"}}
        path = tmpdir(self) / "g2-llmq.json"
        path.write_text(json.dumps({"tests": noted}))
        committed = at.matrix(path)
        self.assertEqual(committed, {"feature_llmq_signing.py": entry})
        entries, meta = at.outcome(self.g2_out("g2-llmq", noted), probe=False)
        self.assertEqual(entries, committed)
        self.assertEqual(at.rows(entries, meta, committed, compared=True, complete=False)[0][4], "same")
        later = {"feature_llmq_signing.py": {**entry, "stage": "setup", at.fc.LATER: "y; MP1a"}}
        entries, meta = at.outcome(self.g2_out("g2-llmq", later), probe=False)
        self.assertEqual(at.rows(entries, meta, committed, compared=True, complete=False)[0][4], "progress")

    def test_a_probe_is_not_compared(self):
        tests = {"feature_llmq_signing.py": {"status": "pass", "stage": "pass"}}
        entries, meta = at.outcome(self.g2_out("g2-llmq-probe", tests), probe=True)
        self.assertEqual(at.rows(entries, meta, COMMITTED, compared=False, complete=False),
                         [("feature_llmq_signing.py", "pass", "-", "-", "not compared")])

    def test_a_run_without_its_result_did_not_finish(self):
        entries, meta = at.outcome(tmpdir(self), probe=False)
        self.assertIsNone(entries)
        report = at.render({"heads": self.HEADS[:1], "tree": "d" * 40, "harness": "e", "args": [], "rows": None,
                            "tail": ["Traceback", "boom"], "status": 2, "files": {}})
        self.assertTrue(report.startswith(f"HEAD {'a' * 40}\n\n"))
        self.assertIn("g2 did not finish", report)
        self.assertIn("boom", report)

    def test_the_harness_line_says_when_it_has_uncommitted_changes(self):
        for dirty, said in ((True, " with uncommitted changes"), (False, "")):
            report = at.render({"heads": self.HEADS[:1], "tree": "d" * 40, "harness": "e" * 40, "dirty": dirty,
                                "args": [], "rows": None, "tail": [], "status": 2, "files": {}})
            self.assertIn(f"- harness: `{at.HERE}` at `{'e' * 40}`{said} (func-compat.py and g2-llmq.json)",
                          report.splitlines())

    def test_sccache_resets_alone_allow_one_rebuild(self):
        reset = "sccache: error: Connection reset by peer (os error 104)\nerror: could not compile `x`\n"
        self.assertTrue(at.sccache_reset_only(reset))
        self.assertFalse(at.sccache_reset_only(reset + "error[E0308]: mismatched types\n"))
        self.assertFalse(at.sccache_reset_only("error: could not compile `x`\n"))


class Main(unittest.TestCase):
    def setUp(self):
        self.enterContext(unittest.mock.patch.dict(os.environ, GIT_ENV))
        self.repo = Repo(tmpdir(self))
        self.scratch, self.out = tmpdir(self) / "scratch", tmpdir(self) / "reports"
        self.calls, self.features, self.releases = [], [], []
        self.enterContext(unittest.mock.patch.object(at, "free_gb", lambda: 1000))
        self.enterContext(unittest.mock.patch.object(at, "load5", lambda: 1.0))
        self.enterContext(unittest.mock.patch.object(at, "build", self.build))
        self.enterContext(unittest.mock.patch.object(at, "run_g2", self.run_g2))
        self.committed = json.loads((HERE / "g2-llmq.json").read_text())["tests"]

    def build(self, src, log, features="", release=False):
        self.calls.append(("build", Path(src)))
        self.features.append(features)
        self.releases.append(release)
        Path(log).write_text("built\n")
        at.bins_dir(src, release).mkdir(parents=True, exist_ok=True)
        return ""

    def run_g2(self, cmd, out):
        self.calls.append(("g2", cmd))
        names = [n for n in self.committed if n.startswith("feature_llmq_simplepose")]
        kind = "g2-llmq-probe" if {"--probe-drop", "--probe-skip"} & set(cmd) else "g2-llmq"
        (out / f"{kind}-v24.0.json").write_text(json.dumps({"tests": {n: self.committed[n] for n in names}}))
        return 0

    def main(self, *argv):
        with contextlib.redirect_stdout(io.StringIO()) as stdout, contextlib.redirect_stderr(io.StringIO()):
            status = at.main(["--repo", str(self.repo.root), "--scratch", str(self.scratch), "--out", str(self.out),
                              *argv])
        return status, Path(stdout.getvalue().splitlines()[-1])

    def test_a_composed_run_reports_and_cleans_up(self):
        status, report = self.main("left", "right", "--", "feature_llmq_simplepose")
        self.assertEqual(status, 0)
        src = self.calls[0][1]
        self.assertFalse(src.exists())
        cmd = self.calls[1][1]
        self.assertEqual(cmd[:8], [sys.executable, "-u", str(HERE / "func-compat.py"), "g2", "--pin", "v24.0",
                                   "--lock-wait", "7200"])
        self.assertNotIn("flock", cmd)
        self.assertEqual(cmd[cmd.index("--rdashd") + 1], str(src / "target/debug/rdashd"))
        self.assertNotIn("--update", cmd)
        self.assertEqual(cmd[-2:], ["feature_llmq_simplepose.py", "feature_llmq_simplepose.py --disable-spork23"])
        left, right = (self.repo.git("rev-parse", b) for b in ("left", "right"))
        self.assertTrue(report.name.endswith(f"-{left[:12]}-{right[:12]}-{os.getpid()}.md"))
        self.assertEqual(report.stat().st_mode & 0o777, 0o644)
        self.assertEqual(self.out.stat().st_mode & 0o777, 0o755)
        lines = report.read_text().splitlines()
        self.assertEqual(lines[:2], [f"HEAD {left}", f"REF right {right}"])
        self.assertEqual(sum(l.endswith("| same |") for l in lines), 2)
        self.assertIn("g2 exit status: 0 (same as the committed matrix)", lines)

    def test_runs_in_the_same_second_at_the_same_refs_do_not_collide(self):
        second = time.gmtime(0)
        with unittest.mock.patch.object(at.time, "gmtime", lambda now=None: second), \
                unittest.mock.patch.object(at.os, "getpid", side_effect=[1111, 2222]):
            reports = [self.main("left", "--", "feature_llmq_simplepose") for _ in range(2)]
        self.assertEqual([s for s, _ in reports], [0, 0])
        names = [r.name for _, r in reports]
        self.assertEqual([n.rsplit("-", 1)[1] for n in names], ["1111.md", "2222.md"])
        self.assertTrue(all(r.exists() and r.with_suffix(".d").is_dir() for _, r in reports))

    def test_probe_mn_and_branch_runs_are_not_compared(self):
        for extra, delta in ((["--branch", "node/other"], "not compared"), (["--mn", "3"], "not compared"),
                             (["--probe-drop", "spentindex"], "not compared"),
                             (["--probe-skip", "quorum.dkgstatus"], "not compared"),
                             (["--branch", at.fc.G2_CORE_BRANCH], "same")):
            with self.subTest(extra=extra):
                self.out = tmpdir(self) / "reports"
                _, report = self.main("left", "--", *extra, "feature_llmq_simplepose")
                rows = [l for l in report.read_text().splitlines() if l.startswith("| `feature_llmq_simplepose")]
                self.assertEqual(len(rows), 2)
                self.assertTrue(all(r.endswith(f"| {delta} |") for r in rows), rows)

    def test_reuse_keeps_the_worktree_and_target_across_runs(self):
        dest = tmpdir(self) / "nightly/src"
        self.main("--reuse", str(dest), "left")
        self.assertTrue((Path(self.repo.git("-C", str(dest), "rev-parse", "--absolute-git-dir")) / at.REUSE_MARKER).exists())
        (dest / "target/debug/built-before").write_text("")
        status, report = self.main("--reuse", str(dest), "--lock-wait", "600", "right")
        self.assertEqual(status, 0)
        self.assertEqual([c[1] for c in self.calls if c[0] == "build"], [dest, dest])
        cmd = self.calls[-1][1]
        self.assertEqual(cmd[cmd.index("--lock-wait") + 1], "600")
        self.assertTrue((dest / "target/debug/built-before").exists())
        self.assertEqual(((dest / "a.txt").read_text(), (dest / "b.txt").read_text()), ("1\n2\n3\n", "y\n"))
        self.assertEqual(self.repo.git("-C", str(dest), "rev-parse", "HEAD"), self.repo.git("rev-parse", "right"))
        self.assertEqual(self.repo.git("-C", str(dest), "status", "--porcelain", "--untracked-files=no"), "")
        self.assertIn(f"worktree kept across runs at {dest} (--reuse)", report.read_text())
        at.remove_worktree(self.repo.root, dest)

    def test_reuse_refuses_a_directory_it_did_not_make(self):
        plain = tmpdir(self) / "plain"
        plain.mkdir()
        other = tmpdir(self) / "other"
        self.repo.git("worktree", "add", "--detach", str(other), "left")
        for dest in (plain, self.repo.root, other):
            with self.subTest(dest=dest), contextlib.redirect_stderr(io.StringIO()) as err:
                status = at.main(["--repo", str(self.repo.root), "--out", str(self.out), "--reuse", str(dest), "left"])
            self.assertEqual(status, 2)
            self.assertIn("refusing --reuse", err.getvalue())
        self.assertEqual(self.calls, [])
        self.assertFalse(self.out.exists())
        self.repo.git("worktree", "remove", "--force", str(other))

    def test_reuse_refuses_a_worktree_in_use(self):
        dest = tmpdir(self) / "src"
        self.main("--reuse", str(dest), "left")
        self.calls.clear()
        with open(Path(self.repo.git("-C", str(dest), "rev-parse", "--absolute-git-dir")) / at.REUSE_MARKER) as held:
            fcntl.flock(held, fcntl.LOCK_EX)
            self.out = tmpdir(self) / "reports"
            status, report = self.main("--reuse", str(dest), "right")
        self.assertEqual(status, 2)
        self.assertIn("in use by another g2-at run", report.read_text())
        self.assertEqual(self.calls, [])
        self.assertEqual(self.repo.git("-C", str(dest), "rev-parse", "HEAD"), self.repo.git("rev-parse", "left"))
        at.remove_worktree(self.repo.root, dest)

    def test_build_only_runs_no_g2_and_writes_no_report(self):
        dest = tmpdir(self) / "src"
        status, _ = self.main("--build-only", "--reuse", str(dest), "left")
        self.assertEqual(status, 0)
        self.assertEqual(self.calls, [("build", dest)])
        self.assertFalse(self.out.exists())
        at.remove_worktree(self.repo.root, dest)

    def test_keep_leaves_the_worktree(self):
        self.main("--keep", "left")
        src = self.calls[0][1]
        self.assertTrue((src / "a.txt").exists())
        at.remove_worktree(self.repo.root, src)

    def test_a_conflict_writes_a_report_and_builds_nothing(self):
        status, report = self.main("left", "clash")
        self.assertEqual(status, 2)
        self.assertEqual(self.calls, [])
        self.assertIn("conflict between left", report.read_text())
        self.assertFalse(self.scratch.exists())

    def test_a_tripwire_error_is_noted_and_gives_no_rows(self):
        line = "tripwire: rdash-cli getmempoolinfo answered neither a result nor -32601 in 60 s: error code: -28"

        for g2_status in (2, 1, 0):
            def stopped(cmd, out):
                (out / "g2.log").write_text(f"{line}\n")
                return g2_status
            with self.subTest(g2_status=g2_status), unittest.mock.patch.object(at, "run_g2", stopped):
                self.out = tmpdir(self) / "reports"
                status, report = self.main("left", "--", "feature_llmq_simplepose")
                text = report.read_text()
                self.assertEqual(status, 2)
                self.assertIn(f"- note: {line}", text.splitlines())
                self.assertIn("g2 did not finish", text)
                self.assertIn(f"g2 exit status: {g2_status} (no result; g2-at exits 2)", text.splitlines())
                self.assertNotIn("stale", text.lower())

    def test_a_terminated_or_skipped_g2_keeps_its_status(self):
        for g2_status in (143, 75):
            with self.subTest(g2_status=g2_status), \
                    unittest.mock.patch.object(at, "run_g2", lambda cmd, out, s=g2_status: s):
                self.out = tmpdir(self) / "reports"
                status, _ = self.main("left", "--", "feature_llmq_simplepose")
                self.assertEqual(status, g2_status)

    def test_a_sigterm_reports_the_step_and_cleans_up(self):
        def killed(src, log, features="", release=False):
            self.calls.append(("build", Path(src)))
            os.kill(os.getpid(), signal.SIGTERM)
        self.enterContext(unittest.mock.patch.object(at, "build", killed))
        status, report = self.main("left")
        self.assertEqual(status, 143)
        self.assertIn("terminated (SIGTERM) during build", report.read_text())
        self.assertFalse(self.calls[0][1].exists())
        self.assertIs(signal.getsignal(signal.SIGTERM), signal.SIG_DFL)

    def test_a_sigterm_stops_g2s_group_before_its_port_lock_goes(self):
        tmp = tmpdir(self)
        lock, pid_file = tmp / "ports.lock", tmp / "grandchild"
        fake = tmp / "g2"
        fake.write_text(textwrap.dedent(f"""\
            import fcntl, subprocess, time
            from pathlib import Path
            held = open({str(lock)!r}, "a")
            fcntl.flock(held, fcntl.LOCK_EX)
            child = subprocess.Popen(["sleep", "300"])
            Path({str(pid_file)!r}).write_text(str(child.pid))
            time.sleep(30)
            """))
        self.enterContext(unittest.mock.patch.object(at, "run_g2", REAL_RUN_G2))
        self.enterContext(unittest.mock.patch.object(at, "g2_command", lambda *a, **k: [sys.executable, str(fake)]))
        seen = {}

        def watch():
            deadline = time.monotonic() + 30
            while not lock_free(lock) and time.monotonic() < deadline:
                time.sleep(0.01)
            seen["alive at release"] = alive(int(pid_file.read_text()))
        sigterm_when(pid_file, then=watch)
        status, report = self.main("left", "--", "feature_llmq_simplepose")
        grandchild = int(pid_file.read_text())
        self.addCleanup(lambda: alive(grandchild) and os.kill(grandchild, signal.SIGKILL))
        self.assertEqual(status, 143)
        self.assertIn("terminated (SIGTERM) during g2", report.read_text())
        deadline = time.monotonic() + 30
        while "alive at release" not in seen and time.monotonic() < deadline:
            time.sleep(0.05)
        self.assertEqual(seen, {"alive at release": False})
        self.assertFalse(alive(grandchild))
        self.assertTrue(lock_free(lock))
        self.assertIs(signal.getsignal(signal.SIGTERM), signal.SIG_DFL)

    def test_the_report_says_how_rdashd_was_built(self):
        for refs, given, features, line in ((["dkg"], [], "rdashd/dkg-runtime", BUILT_DKG),
                                            (["nodkg"], [], "", BUILT_NONE),
                                            (["left", "dkg"], [], "rdashd/dkg-runtime", BUILT_DKG),
                                            (["dkg"], ["--features", ""], "",
                                             "built without features (--features '' given)"),
                                            (["nodkg"], ["--features", "x,y"], "x,y",
                                             "built with --features x,y (--features given)")):
            with self.subTest(refs=refs, given=given):
                self.out, self.features = tmpdir(self) / "reports", []
                status, report = self.main(*given, *refs, "--", "feature_llmq_simplepose")
                self.assertEqual(status, 0)
                self.assertEqual(self.features, [features])
                self.assertEqual(report.read_text().splitlines()[len(refs)], line)

    def test_reuse_builds_each_run_with_its_own_features(self):
        dest = tmpdir(self) / "src"
        lines = []
        for argv in (["dkg"], ["nodkg"], ["--features", "", "dkg"], ["dkg"]):
            self.out = tmpdir(self) / "reports"
            _, report = self.main("--reuse", str(dest), *argv)
            lines.append(report.read_text().splitlines()[1])
        self.assertEqual(self.features, ["rdashd/dkg-runtime", "", "", "rdashd/dkg-runtime"])
        self.assertEqual(lines, [BUILT_DKG, BUILT_NONE, "built without features (--features '' given)", BUILT_DKG])
        with contextlib.redirect_stdout(io.StringIO()) as stdout:
            at.main(["--repo", str(self.repo.root), "--build-only", "--reuse", str(dest), "nodkg"])
        self.assertTrue(stdout.getvalue().startswith(f"g2-at: {BUILT_NONE}: {dest / 'target/debug'} in "))
        with contextlib.redirect_stdout(io.StringIO()) as stdout:
            at.main(["--repo", str(self.repo.root), "--build-only", "--release", "--reuse", str(dest), "dkg"])
        self.assertTrue(stdout.getvalue().startswith(f"g2-at: {BUILT_DKG_RELEASE}: {dest / 'target/release'} in "))
        at.remove_worktree(self.repo.root, dest)

    def test_a_release_run_uses_target_release_and_says_so(self):
        status, report = self.main("--release", "dkg", "--", "feature_llmq_simplepose")
        self.assertEqual(status, 0)
        self.assertEqual(self.releases, [True])
        src, cmd = self.calls[0][1], self.calls[1][1]
        self.assertEqual(cmd[cmd.index("--rdashd") + 1], str(src / "target/release/rdashd"))
        self.assertEqual(cmd[cmd.index("--rdash-cli") + 1], str(src / "target/release/rdash-cli"))
        self.assertEqual(report.read_text().splitlines()[1], BUILT_DKG_RELEASE)
        self.calls, self.releases, self.out = [], [], tmpdir(self) / "reports"
        self.main("dkg", "--", "feature_llmq_simplepose")
        self.assertEqual(self.releases, [False])
        self.assertIn(str(self.calls[0][1] / "target/debug/rdashd"), self.calls[1][1])

    def test_update_is_refused_before_anything_runs(self):
        with contextlib.redirect_stderr(io.StringIO()) as err:
            status = at.main(["--repo", str(self.repo.root), "--out", str(self.out), "left", "--", "--update"])
        self.assertEqual(status, 2)
        self.assertIn("--update", err.getvalue())
        self.assertEqual(self.calls, [])
        self.assertFalse(self.out.exists())


class PortLock(unittest.TestCase):
    """func-compat.py g2's port lock: one g2 run at a time on G2_PORT_MIN's ports."""

    def setUp(self):
        self.path = tmpdir(self) / "locks/g2-ports.lock"
        self.enterContext(unittest.mock.patch.dict(os.environ, {"G2_PORT_LOCK": str(self.path)}))
        self.ran = []
        self.enterContext(unittest.mock.patch.object(at.fc, "g2_ports_held", self.held_run))

    def held_run(self, args):
        with open(self.path) as other, self.assertRaises(BlockingIOError):
            fcntl.flock(other, fcntl.LOCK_EX | fcntl.LOCK_NB)
        self.ran.append(args)
        return 1

    def holder(self):
        self.path.parent.mkdir(parents=True, exist_ok=True)
        f = self.enterContext(open(self.path, "a"))
        fcntl.flock(f, fcntl.LOCK_EX)
        return f

    def test_a_held_lock_waits_once_then_times_out_with_75(self):
        self.holder()
        with contextlib.redirect_stderr(io.StringIO()) as err:
            with at.fc.port_lock(self.path, 0.3, poll=0.05) as held:
                self.assertFalse(held)
            status = at.fc.g2(argparse.Namespace(lock_wait=0.2))
        self.assertEqual(status, 75)
        self.assertEqual(self.ran, [])
        self.assertEqual(err.getvalue().count("waiting for g2 port lock (held)"), 2)
        self.assertIn("was not free within 0.2 s; nothing ran", err.getvalue())

    def test_the_run_holds_the_lock_and_releases_it(self):
        self.assertEqual(at.fc.g2(argparse.Namespace(lock_wait=5)), 1)
        self.assertEqual(len(self.ran), 1)
        with open(self.path) as f:
            fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)

    def test_a_lock_freed_while_waiting_is_taken(self):
        f = self.holder()
        threading.Timer(0.3, fcntl.flock, (f, fcntl.LOCK_UN)).start()
        with contextlib.redirect_stderr(io.StringIO()) as err:
            self.assertEqual(at.fc.g2(argparse.Namespace(lock_wait=10)), 1)
        self.assertIn("waiting for g2 port lock (held)", err.getvalue())

    def test_a_lock_timeout_report_says_skipped(self):
        report = at.render({"heads": [("h", "a" * 40)], "tree": "t", "harness": "e", "args": [], "rows": None,
                            "tail": [], "status": 75, "files": {}})
        self.assertIn("Skipped: g2 ports busy", report)
        self.assertIn("g2 exit status: 75 (skipped: the g2 port lock was not free within --lock-wait)", report)


class NightlyCap(unittest.TestCase):
    def test_the_units_cap_covers_the_lock_wait_and_a_cold_build_and_full_night(self):
        unit = (HERE / "systemd/g2-llmq-nightly.service").read_text()
        script = (HERE / "g2-nightly.sh").read_text()
        caps = re.findall(r"^RuntimeMaxSec=(\d+)$", unit, re.M)
        waits = re.findall(r"^LOCK_WAIT=\$\{G2_NIGHTLY_LOCK_WAIT:-(\d+)\}$", script, re.M)
        self.assertEqual((len(caps), len(waits)), (1, 1))
        self.assertGreaterEqual(int(caps[0]), int(waits[0]) + 2400)


class Nightly(unittest.TestCase):
    """g2-nightly.sh, and g2-nightly-bootstrap.sh in front of it, over a fake g2-at committed in a
    throwaway harness repository: FAKE_ROWS lists the report's deltas ("none": no report),
    FAKE_STATUS is its exit status, FAKE_STALE adds the stale-adaptation note and FAKE_BUILT is
    the report's build line ("none": no line)."""

    FAKE_G2_AT = textwrap.dedent("""\
        #!/usr/bin/env python3
        import os, sys
        from pathlib import Path
        args = sys.argv[1:]
        out = Path(args[args.index("--out") + 1])
        out.mkdir(parents=True, exist_ok=True)
        (out.parent / "args").write_text("\\n".join(args))
        rows, status = os.environ["FAKE_ROWS"], int(os.environ["FAKE_STATUS"])
        if rows != "none":
            built = os.environ.get("FAKE_BUILT", "built with --release --features rdashd/dkg-runtime")
            lines = ["HEAD " + "a" * 40] + ([built] if built != "none" else []) + ["", "# G2", ""]
            if os.environ.get("FAKE_STALE"):
                lines.append("- note: adaptations: STALE: x")
            if os.environ.get("FAKE_TRIPWIRE"):
                lines.append("- note: tripwire: rdash-cli m answered neither a result nor -32601 in 60 s: error code: -28")
            lines += ["| test | stage | class | first blocker | delta |", "|---|---|---|---|---|"]
            lines += [f"| `t{i}.py` | start | c | a \\\\| same | {d} |" for i, d in enumerate(filter(None, rows.split(",")))]
            report = out / "r.md"
            report.write_text("\\n".join(lines) + "\\n")
            print("g2-at: g2 at x")
            print(report)
        sys.exit(status)
        """)

    def setUp(self):
        self.enterContext(unittest.mock.patch.dict(os.environ, GIT_ENV))
        self.repo = tmpdir(self) / "repo"
        self.fc = self.repo / "contrib/node/func-compat"
        self.fc.mkdir(parents=True)
        self.git("init", "-q", "-b", "bare")
        (self.repo / "README").write_text("x\n")
        self.git("add", "README")
        self.git("commit", "-q", "-m", "bare")
        self.git("checkout", "-q", "-b", "harness")
        for name in ("g2-nightly.sh", "g2-nightly-bootstrap.sh"):
            (self.fc / name).write_text((HERE / name).read_text())
        (self.fc / "g2-at").write_text(self.FAKE_G2_AT)
        (self.fc / "g2-at").chmod(0o755)
        self.git("add", "contrib")
        self.git("commit", "-q", "-m", "harness")
        self.bootstrap = tmpdir(self) / "libexec/g2-nightly-bootstrap.sh"
        self.bootstrap.parent.mkdir()
        self.bootstrap.write_text((HERE / "g2-nightly-bootstrap.sh").read_text())

    def git(self, *args, cwd=None):
        return subprocess.run(["git", "-C", str(cwd or self.repo), "-c", "user.name=t", "-c", "user.email=t@example.com",
                               *args], check=True, capture_output=True, text=True).stdout.strip()

    def sha(self, ref="harness"):
        return self.git("rev-parse", ref)

    def nightly(self, rows, status, script=None, before=None, **env):
        d = tmpdir(self)
        if before:
            before(d)
        env = {**{k: v for k, v in os.environ.items() if not k.startswith("G2_NIGHTLY")},
               "G2_NIGHTLY_DIR": str(d / "state"), "G2_NIGHTLY_REPORTS": str(d / "reports"),
               "FAKE_ROWS": rows, "FAKE_STATUS": str(status), **env}
        r = subprocess.run(["bash", str(script or self.fc / "g2-nightly.sh")], env=env, capture_output=True, text=True)
        return r.returncode, r.stdout.strip(), d

    def boot(self, rows="same", status=0, state=None, **env):
        env = {"G2_NIGHTLY_REPO": str(self.repo), "G2_NIGHTLY_HARNESS": "harness", **env}
        if state:
            env["G2_NIGHTLY_DIR"] = str(state)
        return self.nightly(rows, status, script=self.bootstrap, **env)

    def test_exit_status_and_summary(self):
        harness = self.sha()[:12]
        for rows, status, stale, code, summary in (
                ("same,same", 0, "", 0, f"2 same, 0 progress, 0 regression, 0 change; g2-at exit 0; harness {harness} dkg-runtime;"),
                ("same,progress", 1, "", 0, "1 same, 1 progress (t1.py), 0 regression"),
                ("progress,regression,regression", 1, "", 1, "1 progress (t0.py), 2 regression (t1.py, t2.py)"),
                ("same", 1, "1", 1, "g2-at exit 1; adaptations STALE"),
                ("same", 2, "", 2, "1 same"),
                ("", 143, "", 2, "no result (g2-at exit 143)"),
                ("none", 3, "", 2, f"no result (g2-at exit 3); harness {harness} features unknown; report none"),
                ("", 2, "", 2, f"no result (g2-at exit 2); harness {harness} dkg-runtime; report"),
                ("", 1, "", 2, "no result (g2-at exit 1); harness"),
                ("", 75, "", 0, "skipped: g2 ports busy (the g2 port lock was not free within 900 s); harness")):
            with self.subTest(rows=rows, status=status):
                got, out, _ = self.nightly(rows, status, FAKE_STALE=stale)
                self.assertEqual(got, code, out)
                self.assertEqual(len(out.splitlines()), 1)
                self.assertTrue(out.startswith("g2-nightly: node/integration "), out)
                self.assertIn(summary, out)

    def test_the_summary_names_how_rdashd_was_built(self):
        harness = self.sha()[:12]
        for built, tag in ((BUILT_DKG, "dkg-runtime"), (BUILT_NONE, "no-dkg-runtime"),
                           (BUILT_DKG_RELEASE, "dkg-runtime"),
                           ("built with --release, without dkg-runtime: rdashd has no such feature at this head",
                            "no-dkg-runtime"),
                           ("built with --release, without features (--features '' given)", "no-dkg-runtime"),
                           ("built without features (--features '' given)", "no-dkg-runtime"),
                           ("built with --features x,y (--features given)", "no-dkg-runtime"),
                           ("built with --features rdashd/dkg-runtime,x (--features given)", "dkg-runtime"),
                           ("none", "features unknown")):
            with self.subTest(built=built):
                got, out, _ = self.nightly("same", 0, FAKE_BUILT=built)
                self.assertEqual(got, 0, out)
                self.assertIn(f"; harness {harness} {tag}; report ", out)

    def test_a_tripwire_error_is_no_result_quoting_it(self):
        got, out, _ = self.nightly("", 2, FAKE_TRIPWIRE="1")
        self.assertEqual(got, 2, out)
        self.assertIn("no result (g2-at exit 2; tripwire: rdash-cli m answered neither a result nor -32601 in 60 s:"
                      " error code: -28); harness", out)
        self.assertNotIn("stale", out.lower())

    def test_arguments_and_latest(self):
        _, out, d = self.nightly("same", 0)
        self.assertEqual((d / "args").read_text().split("\n"),
                         ["--release", "--reuse", str(d / "state/src"), "--out", str(d / "reports"),
                          "--lock-wait", "900", "node/integration"])
        self.assertEqual(os.readlink(d / "reports/latest.md"), "r.md")
        self.assertIn(f"report {d / 'reports/r.md'}", out)
        self.assertIn("node/integration aaaaaaaaaaaa:", out)
        self.assertTrue((d / "state/summary.log").read_text().rstrip().endswith(out))

    def test_only_the_newest_reports_keep_the_failed_datadirs(self):
        names = [f"20261122T0{i}0000Z-aaaaaaaaaaaa-{i}.d" for i in range(10)]

        def reports(d):
            for name in names:
                for kept in ("failed-main", "failed-confirm", "possible-crashes"):
                    (d / "reports" / name / kept / "t_1-x/t_1/node0").mkdir(parents=True)
            (d / "reports/other").mkdir()
            (d / "reports/other/failed-main").mkdir()

        for env, keep, rows, status in (({}, 7, "same", 0), ({"G2_NIGHTLY_KEEP_DATADIRS": "3"}, 3, "", 75),
                                        ({"G2_NIGHTLY_KEEP_DATADIRS": "x"}, 7, "same", 0),
                                        ({"G2_NIGHTLY_KEEP_DATADIRS": "0"}, 0, "", 143)):
            with self.subTest(env=env, status=status):
                _, out, d = self.nightly(rows, status, before=reports, **env)
                for i, name in enumerate(names):
                    left = sorted(p.name for p in (d / "reports" / name).iterdir())
                    want = ["failed-confirm", "failed-main"] if i >= len(names) - keep else []
                    self.assertEqual(left, sorted(want + ["possible-crashes"]), (name, out))
                self.assertTrue((d / "reports/other/failed-main").is_dir())

    def test_a_harness_with_changes_or_no_checkout_is_no_result(self):
        plain = tmpdir(self) / "plain/g2-nightly.sh"
        plain.parent.mkdir()
        plain.write_text((HERE / "g2-nightly.sh").read_text())
        harness = self.sha()[:12]
        for path, script, said in (
                (self.fc / "notes.txt", None, f"the harness {self.fc} at {harness} has uncommitted changes"),
                (self.repo / "contrib/node/x.py", None, "has uncommitted changes"),
                (self.fc / "g2-at", None, "has uncommitted changes"),
                (None, plain, f"the harness {plain.parent} is not a git checkout")):
            with self.subTest(path=path, script=script):
                if path:
                    path.write_text("edit\n")
                got, out, d = self.nightly("same", 0, script=script)
                if path:
                    self.git("checkout", "-q", "--", ".")
                    self.git("clean", "-qfd")
                self.assertEqual(got, 2, out)
                self.assertTrue(out.startswith("g2-nightly: node/integration: no result: "), out)
                self.assertIn(said, out)
                self.assertFalse((d / "args").exists())
                self.assertTrue((d / "state/summary.log").read_text().rstrip().endswith(out))

    def test_the_bootstrap_runs_its_own_harness_worktree_at_the_harness_ref(self):
        state = tmpdir(self) / "state"
        got, out, d = self.boot(state=state)
        tree = state / "harness"
        self.assertEqual(got, 0, out)
        self.assertEqual(self.git("rev-parse", "HEAD", cwd=tree), self.sha())
        self.assertIn(f"harness {self.sha()[:12]} dkg-runtime;", out)
        self.assertEqual((d / "args").read_text().split("\n")[-1], "node/integration")
        (self.fc / "g2-at").write_text(self.FAKE_G2_AT.replace("# G2", "# G2 moved"))
        self.git("commit", "-qam", "next")
        got, out, _ = self.boot(state=state)
        self.assertEqual(got, 0, out)
        self.assertEqual(self.git("rev-parse", "HEAD", cwd=tree), self.sha())
        self.assertIn(f"harness {self.sha()[:12]} dkg-runtime;", out)
        self.assertEqual(self.git("status", "--porcelain", cwd=tree), "")
        got, out, _ = self.boot(state=state, G2_NIGHTLY_HARNESS="", G2_NIGHTLY_REF="harness")
        self.assertEqual(got, 0, out)
        self.assertTrue(out.startswith("g2-nightly: harness "), out)

    def test_the_bootstrap_refuses_with_one_summary_line(self):
        bare = self.sha("bare")[:12]
        for harness, prepare, said in (
                ("bare", None, f"no result: harness bare has no g2-at ({bare})"),
                ("nosuch", None, f"no result: harness nosuch does not resolve in {self.repo}"),
                ("harness", "plain", "is not a worktree of"),
                ("harness", "dirty", "has uncommitted changes; not moved to harness"),
                ("harness", "other", "is not a worktree of")):
            with self.subTest(harness=harness, prepare=prepare):
                state = tmpdir(self) / "state"
                tree = state / "harness"
                if prepare == "plain":
                    tree.mkdir(parents=True)
                elif prepare == "dirty":
                    self.boot(state=state)
                    self.git("checkout", "-q", "--detach", "bare", cwd=tree)
                    (tree / "junk").write_text("x\n")
                elif prepare == "other":
                    other = tmpdir(self) / "other"
                    self.git("init", "-q", str(other))
                    state.mkdir()
                    tree.symlink_to(other)
                got, out, d = self.boot(state=state, G2_NIGHTLY_HARNESS=harness)
                self.assertEqual(got, 2, out)
                self.assertEqual(len(out.splitlines()), 1, out)
                self.assertTrue(out.startswith("g2-nightly: node/integration: no result: "), out)
                self.assertIn(said, out)
                self.assertFalse((d / "args").exists())
                self.assertTrue((state / "summary.log").read_text().rstrip().endswith(out))
                if prepare == "dirty":
                    self.assertEqual(self.git("rev-parse", "HEAD", cwd=tree), self.sha("bare"))
                if prepare is None:
                    self.assertFalse(tree.exists())


if __name__ == "__main__":
    unittest.main()
