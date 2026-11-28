#!/usr/bin/env python3
"""Unit tests for func-compat.py (the ratchet, parsers, tripwire) and ft-map.py (the mapping).

Run with `func-compat.py selftest` or `python3 -m unittest test_func_compat` from this directory.
"""

import argparse
import contextlib
import fcntl
import hashlib
import importlib.util
import io
import json
import os
import re
import shutil
import signal
import stat
import subprocess
import tarfile
import tempfile
import textwrap
import threading
import time
import unittest
import unittest.mock
from pathlib import Path

HERE = Path(__file__).resolve().parent


def load_module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


ftmap = load_module("ft_map", HERE / "ft-map.py")
fc = ftmap.compat

SCRIPTS = ["feature_logging.py", "rpc_bind.py --ipv4", "rpc_bind.py --ipv6", "rpc_uptime.py",
           "interface_rpc.py", "wallet_basic.py --legacy-wallet"]
OWNERS = {"RP1a", "CH5", "P2a-net", "BL-1"}


def alive(pid):
    """Whether `pid` is a live process (a zombie is not)."""
    try:
        return Path(f"/proc/{pid}/stat").read_text().rsplit(") ", 1)[1][0] != "Z"
    except (FileNotFoundError, ProcessLookupError):
        return False


def lock_free(path):
    with open(path, "a") as f:
        try:
            fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)
            return True
        except BlockingIOError:
            return False


def sigterm_when(path, then=None, timeout=20):
    """A thread that SIGTERMs this process once `path` exists, then calls `then` (if given)."""
    def run():
        deadline = time.monotonic() + timeout
        while not path.exists() and time.monotonic() < deadline:
            time.sleep(0.05)
        os.kill(os.getpid(), signal.SIGTERM)
        if then:
            then()
    thread = threading.Thread(target=run, daemon=True)
    thread.start()
    return thread


def matrix(status, targets=None, complete=True, starts=None):
    """A run's matrix; every test started rdashd once unless `starts` says otherwise."""
    return {"status": status, "targets": list(status) if targets is None else targets,
            "complete": complete, "rdashd_starts": {n: 1 for n in status} | (starts or {})}


class Build(unittest.TestCase):
    def test_the_export_links_the_release_it_is_given(self):
        work = Path(self.enterContext(tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR"))))
        tree = fc.tree_dir(work, "v24.0", True)
        (tree / "src/test").mkdir(parents=True)
        (tree / "src/test/config.ini.in").write_text("[environment]\n")
        (tree / "tree.json").write_text(json.dumps({"commit": "c0ffee", "paths": fc.EXPORT_PATHS}))  # already exported
        asked = []

        def release_bin(release):
            asked.append(release)
            return work / f"dashcore-{release}/bin"
        self.enterContext(unittest.mock.patch.object(fc, "git", lambda core, *a: "c0ffee\n"))
        self.enterContext(unittest.mock.patch.object(fc, "release_bin", release_bin))
        for release, want in ((None, fc.PINS["v24.0"].release), ("1.2.3", "1.2.3")):
            fc.build(None, work, "v24.0", True, None, release)
            self.assertEqual(asked.pop(), want)
            self.assertEqual(json.loads((tree / "tree.json").read_text())["release"], want)
            self.assertEqual(os.readlink(tree / "build/src/dashd"), str(work / f"dashcore-{want}/bin/dashd"))

    def exported(self, stamp):
        """`build` over a tree stamped `stamp` (none if None): the `git archive` argvs it ran, and
        the stamp it wrote."""
        work = Path(self.enterContext(tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR"))))
        tree = fc.tree_dir(work, "v24.0", True)
        (tree / "src/test").mkdir(parents=True)
        (tree / "src/test/config.ini.in").write_text("[environment]\n")
        if stamp is not None:
            (tree / "tree.json").write_text(json.dumps(stamp))
        tar_bytes = io.BytesIO()
        with tarfile.open(fileobj=tar_bytes, mode="w") as tar:
            data = b"[environment]\n"
            info = tarfile.TarInfo("test/config.ini.in")
            info.size = len(data)
            tar.addfile(info, io.BytesIO(data))
        archives = []

        def run(argv, **kwargs):
            self.assertEqual(argv[3], "archive", argv)
            archives.append(argv)
            return subprocess.CompletedProcess(argv, 0, stdout=tar_bytes.getvalue())
        self.enterContext(unittest.mock.patch.object(fc, "git", lambda core, *a: "c0ffee\n"))
        self.enterContext(unittest.mock.patch.object(fc, "release_bin", lambda release: work / "bin"))
        self.enterContext(unittest.mock.patch.object(fc.subprocess, "run", run))
        fc.build("core", work, "v24.0", True)
        return archives, json.loads((tree / "tree.json").read_text())

    def test_the_export_holds_contrib_linearize(self):
        archives, stamp = self.exported(None)
        self.assertEqual(archives, [["git", "-C", "core", "archive", "c0ffee", "test", "share/rpcauth",
                                     "contrib/linearize", "src/rpc/client.cpp",
                                     "src/test/data/asmap.raw"]])
        self.assertEqual(stamp["paths"], fc.EXPORT_PATHS)
        self.assertEqual(stamp["commit"], "c0ffee")

    def test_a_tree_exported_with_other_paths_is_exported_again(self):
        for stamp, again in (({"commit": "c0ffee"}, True),
                             ({"commit": "c0ffee", "paths": ["test", "share/rpcauth"]}, True),
                             ({"commit": "c0ffee", "paths": ["test", "share/rpcauth", "contrib/linearize"]},
                              True),
                             ({"commit": "c0ffee", "paths": fc.EXPORT_PATHS[:-1]}, True),
                             ({"commit": "c0ffee", "paths": fc.EXPORT_PATHS}, False)):
            with self.subTest(stamp=stamp):
                archives, written = self.exported(stamp)
                self.assertEqual(len(archives), int(again))
                self.assertEqual(written["paths"], fc.EXPORT_PATHS)

    def test_each_pin_tag_names_its_release(self):
        for name, pin in fc.PINS.items():
            self.assertEqual(pin.tag, f"v{pin.release}", name)

class Lists(unittest.TestCase):
    def test_passlist_drops_comments_and_normalizes_spaces(self):
        text = "# head\nfeature_logging.py\n\nrpc_bind.py   --ipv4  # why\n"
        self.assertEqual(fc.parse_passlist(text), ["feature_logging.py", "rpc_bind.py --ipv4"])

    def test_xfail_parses_three_fields(self):
        text = "# head\nrpc_uptime.py | setmocktime | RP1a\n\n"
        self.assertEqual(fc.parse_xfail(text), {"rpc_uptime.py": ("setmocktime", "RP1a")})

    def test_xfail_refuses_missing_fields_and_duplicates(self):
        for bad in ("rpc_uptime.py | setmocktime", "rpc_uptime.py | | RP1a",
                    "a.py | r | RP1a\na.py | r2 | CH5"):
            with self.assertRaises(ValueError, msg=bad):
                fc.parse_xfail(bad)

    def test_owner_ids_split_on_commas_and_spaces(self):
        self.assertEqual(fc.owner_ids("RP-V24"), ["RP-V24"])
        self.assertEqual(fc.owner_ids("CH5, RP1a BL-1"), ["CH5", "RP1a", "BL-1"])
        self.assertEqual(fc.owner_ids("  "), [])

    def test_known_owners_reads_first_columns(self):
        with tempfile.TemporaryDirectory() as d:
            Path(d, "BACKLOG.md").write_text("| ID | x |\n|---|---|\n| BL-1 | row |\n| **BL-2** | b |\n"
                                             "| Status | y |\n| :--- | --- |\n| Q4 | header |\n|---|---|\n")
            Path(d, "PLAN.md").write_text("text | not a row\n| RP-V24 | task |\n| Date | 10-22 |\n| SW-OPS | s |\n")
            owners = fc.known_owners(d)
        self.assertTrue({"BL-1", "BL-2", "RP-V24"} <= owners)
        self.assertNotIn("text", owners)
        self.assertNotIn("ID", owners)
        self.assertNotIn("Status", owners)
        self.assertNotIn("Date", owners)
        self.assertNotIn("SW-OPS", owners)
        self.assertNotIn("Q4", owners)

    def test_header_keeps_leading_comments_only(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d, "list.txt")
            path.write_text("# one\n# two\na.py\n# later\n")
            self.assertEqual(fc.header(path), "# one\n# two\n")
            self.assertEqual(fc.header(Path(d, "missing.txt")), "")


class Results(unittest.TestCase):
    SUMMARY = textwrap.dedent("""\
        TEST                     | STATUS    | DURATION

        feature_logging.py       | ✓ Passed  | 3 s
        rpc_bind.py --ipv4       | ✓ Passed  | 2 s
        rpc_bind.py --ipv6       | ○ Skipped | 0 s
        rpc_uptime.py            | ✖ Failed  | 1 s

        ALL                      | ✖ Failed  | 6 s (accumulated)
        """)

    def test_parse_runner_output(self):
        self.assertEqual(fc.parse_runner_output(self.SUMMARY), {
            "feature_logging.py": "Passed", "rpc_bind.py --ipv4": "Passed",
            "rpc_bind.py --ipv6": "Skipped", "rpc_uptime.py": "Failed"})

    def test_parse_runner_output_refuses_disagreeing_duplicates(self):
        twice = "a.py --x | ✓ Passed  | 1 s\na.py --x | ✓ Passed  | 1 s\n"
        self.assertEqual(fc.parse_runner_output(twice), {"a.py --x": "Passed"})
        with self.assertRaises(ValueError):
            fc.parse_runner_output(twice + "a.py --x | ✖ Failed  | 1 s\n")

    def test_selection_drops_names_the_runner_runs_by_prefix(self):
        names = ["feature_llmq_signing.py", "feature_llmq_signing.py --spork21", "rpc_bind.py --ipv4"]
        self.assertEqual(fc.selection(names), ["feature_llmq_signing.py", "rpc_bind.py --ipv4"])

    def test_classify(self):
        results = fc.parse_runner_output(self.SUMMARY)
        status = fc.classify(results, {"rpc_uptime.py": ("r", "RP1a")})
        self.assertEqual(status["rpc_uptime.py"], "expected-fail")
        self.assertEqual(status["rpc_bind.py --ipv6"], "skip")
        self.assertEqual(fc.classify(results, {})["rpc_uptime.py"], "fail")

    def test_stem_and_phase_tests(self):
        self.assertEqual(fc.stem("rpc_bind.py --ipv4"), "rpc_bind")
        phases = {"A": {"tests": ["rpc_bind"]}, "B": {"tests": ["wallet_*"]}}
        self.assertEqual(fc.phase_tests(phases, "A", SCRIPTS), ["rpc_bind.py --ipv4", "rpc_bind.py --ipv6"])
        self.assertEqual(fc.phase_tests(phases, "B", SCRIPTS),
                         ["rpc_bind.py --ipv4", "rpc_bind.py --ipv6", "wallet_basic.py --legacy-wallet"])
        with self.assertRaises(SystemExit):
            fc.phase_tests(phases, "C", SCRIPTS)
        phases["B"]["tests"].append("feature_gone")
        self.assertEqual(fc.unmatched(phases, "B", SCRIPTS), ["feature_gone"])
        # A blocked test stays in its phase but is no target, in its phase or a later one's run.
        phases["A"]["blocked"] = {"rpc_bind": "X1"}
        self.assertEqual(fc.phase_blocked(phases, "B"), {"rpc_bind": "X1"})
        self.assertEqual(fc.phase_tests(phases, "A", SCRIPTS), [])
        self.assertNotIn("rpc_bind.py --ipv4", fc.phase_tests(phases, "B", SCRIPTS))

    def test_phases_toml_is_runnable_appendix_d(self):
        phases = fc.load_phases()
        self.assertEqual(list(phases)[0], "M2")
        for name, phase in phases.items():
            self.assertIsInstance(phase["wallet"], bool, name)
            self.assertTrue(phase["tests"], name)
            for test, why in phase.get("blocked", {}).items():
                self.assertIn(test, phase["tests"], name)
                self.assertTrue(why, test)
        self.assertNotIn("rust_nodes", phases["mixed-a"])


class Ratchet(unittest.TestCase):
    PASS = ["rpc_bind.py --ipv4"]
    XFAIL = {"rpc_uptime.py": ("setmocktime", "RP1a")}

    def judge(self, status, passlist=None, xfail=None, history=(), **kw):
        return fc.judge(matrix(status, **kw), self.PASS if passlist is None else passlist,
                        self.XFAIL if xfail is None else xfail, SCRIPTS, OWNERS, history)

    def test_accepts_a_clean_run(self):
        self.assertEqual(self.judge({"rpc_bind.py --ipv4": "pass", "rpc_uptime.py": "expected-fail",
                                     "rpc_bind.py --ipv6": "skip"}), [])

    def test_refuses_a_passlisted_failure(self):
        errors = self.judge({"rpc_bind.py --ipv4": "fail", "rpc_uptime.py": "expected-fail"})
        self.assertEqual(len(errors), 1)
        self.assertIn("REGRESSION", errors[0])

    def test_refuses_a_passlisted_skip(self):
        errors = self.judge({"rpc_bind.py --ipv4": "skip", "rpc_uptime.py": "expected-fail"})
        self.assertIn("REGRESSION", "".join(errors))

    def test_refuses_a_passlisted_test_missing_from_a_complete_run_only(self):
        self.assertIn("not run", "".join(self.judge({"rpc_uptime.py": "expected-fail"})))
        self.assertEqual(self.judge({"rpc_uptime.py": "expected-fail"}, complete=False), [])

    def test_refuses_a_test_removed_from_the_passlist(self):
        status = {"rpc_bind.py --ipv4": "pass", "rpc_uptime.py": "expected-fail"}
        self.assertEqual(self.judge(status, history={"rpc_bind.py --ipv4"}), [])
        errors = self.judge(status, history={"rpc_bind.py --ipv4", "interface_rpc.py"})
        self.assertEqual(errors, ["interface_rpc.py: REGRESSION, removed from the passlist"])

    def test_update_is_refused_unless_only_unexpected_passes(self):
        m = {**matrix({"rpc_uptime.py": "pass"}), "rust_nodes": "all", "phase": "M2"}
        pin = fc.PINS["v24.0"]
        self.assertIn("not the phase's", fc.check_update(None, pin, {**m, "rust_nodes": "0"}, []))
        errors = self.judge({"rpc_bind.py --ipv4": "fail", "rpc_uptime.py": "pass"})
        self.assertIn("besides", fc.check_update(None, pin, m, errors))

    def test_refuses_a_passlisted_pass_on_dashd_alone(self):
        status = {"rpc_bind.py --ipv4": "pass", "rpc_uptime.py": "expected-fail"}
        refusal = ["rpc_bind.py --ipv4: passlisted, but passed without starting rdashd"]
        # An empty --rust-nodes mask: no node started rdashd, in a partial run too.
        none = {n: 0 for n in status}
        self.assertEqual(self.judge(status, starts=none), refusal)
        self.assertEqual(self.judge(status, starts=none, complete=False), refusal)
        # Mask "all", but the test passes its own binary= for every node: its count alone is 0.
        self.assertEqual(self.judge(status, starts={"rpc_bind.py --ipv4": 0}), refusal)
        # A matrix without counts (an older run) proves nothing either.
        self.assertEqual(fc.judge({**matrix(status), "rdashd_starts": {}}, self.PASS, self.XFAIL, SCRIPTS, OWNERS),
                         refusal)

    def test_an_expected_failure_passing_on_dashd_is_not_moved(self):
        status = {"rpc_bind.py --ipv4": "pass", "rpc_uptime.py": "pass"}
        m = matrix(status, starts={"rpc_uptime.py": 0})
        self.assertEqual(fc.judge(m, self.PASS, self.XFAIL, SCRIPTS, OWNERS), [])
        self.assertEqual(fc.update(self.PASS, self.XFAIL, m), (self.PASS, self.XFAIL))

    def test_count_starts(self):
        self.assertEqual(fc.count_starts([], ["a.py"]), {"a.py": 0})
        self.assertEqual(fc.count_starts(["a.py", "rpc_bind.py --ipv4", "a.py"],
                                         ["a.py", "rpc_bind.py --ipv4", "rpc_bind.py --ipv6"]),
                         {"a.py": 2, "rpc_bind.py --ipv4": 1, "rpc_bind.py --ipv6": 0})

    def test_refuses_an_unexpected_pass(self):
        errors = self.judge({"rpc_bind.py --ipv4": "pass", "rpc_uptime.py": "pass"})
        self.assertIn("expected to fail but passed", "".join(errors))

    def test_refuses_an_unlisted_target_failure_but_not_a_skip(self):
        status = {"rpc_bind.py --ipv4": "pass", "rpc_uptime.py": "expected-fail", "interface_rpc.py": "fail"}
        self.assertIn("no expected-failures entry", "".join(self.judge(status)))
        status["interface_rpc.py"] = "skip"
        self.assertEqual(self.judge(status), [])

    def test_ignores_an_unlisted_failure_outside_the_targets(self):
        status = {"rpc_bind.py --ipv4": "pass", "rpc_uptime.py": "expected-fail", "interface_rpc.py": "fail"}
        self.assertEqual(self.judge(status, targets=["rpc_bind.py --ipv4"]), [])

    def test_refuses_an_unknown_owner(self):
        for owner in ("XX9", "RP1a, XX9", "", "see manager"):
            xfail = {"rpc_uptime.py": ("r", owner)}
            errors = self.judge({"rpc_bind.py --ipv4": "pass", "rpc_uptime.py": "expected-fail"}, xfail=xfail)
            self.assertIn("names no BACKLOG row", "".join(errors), owner)

    def test_refuses_both_lists_and_unknown_tests(self):
        xfail = {"rpc_bind.py --ipv4": ("r", "RP1a"), "nope.py": ("r", "RP1a")}
        errors = "".join(self.judge({"rpc_bind.py --ipv4": "pass"}, xfail=xfail))
        self.assertIn("on both", errors)
        self.assertIn("no such test", errors)

    def test_update_moves_passes_and_only_grows(self):
        status = {"rpc_bind.py --ipv4": "pass", "rpc_uptime.py": "pass", "interface_rpc.py": "pass",
                  "rpc_bind.py --ipv6": "skip", "wallet_basic.py --legacy-wallet": "pass"}
        passlist, xfail = fc.update(self.PASS, self.XFAIL,
                                    matrix(status, targets=["rpc_bind.py --ipv4", "interface_rpc.py"]))
        self.assertEqual(passlist, ["rpc_bind.py --ipv4", "interface_rpc.py", "rpc_uptime.py"])
        self.assertEqual(xfail, {})
        self.assertEqual(fc.judge(matrix(status, targets=["rpc_bind.py --ipv4", "interface_rpc.py"]),
                                  passlist, xfail, SCRIPTS, OWNERS), [])

    def test_render_round_trips(self):
        self.assertEqual(fc.parse_passlist(fc.render_passlist(self.PASS)), self.PASS)
        self.assertEqual(fc.parse_xfail(fc.render_xfail(self.XFAIL)), self.XFAIL)


# rdashd: refuses `-opt=value` unless FAKE_ACCEPT names `opt=value` (or fails with FAKE_OPT_FAIL's
# text instead of the refusal); with no option it runs
# until rdash-cli's stop, unless FAKE_DIE; a second one on a running datadir (-noserver) gives
# the lock error with FAKE_NAME (default rdashd).
FAKE_RDASHD = """#!/bin/sh
for a in "$@"; do
  case "$a" in
    -datadir=*) d="${a#-datadir=}" ;;
    -noserver) [ -s "$d/pid" ] && {
          echo "Error: Cannot obtain a lock on data directory $d. ${FAKE_NAME:-rdashd} is probably already running." >&2
          exit 1; } ;;
    -listen=0) nolisten=1 ;;
    -regtest|-disablewallet|-rpcport=*) ;;
    -*=*) case " $FAKE_ACCEPT " in *" ${a#-} "*) exit 0 ;; esac
          [ -n "$FAKE_OPT_FAIL" ] && { echo "$FAKE_OPT_FAIL" >&2; exit 1; }
          echo "Error: ${a%%=*} is not supported by rdashd yet (XX)." >&2; exit 1 ;;
  esac
done
# As when another node holds the regtest p2p port: the tripwire's nodes must not listen.
[ -z "$nolisten" ] && { echo "Error: Unable to bind to 0.0.0.0:19899 on this computer." >&2; touch "$d/died"; exit 1; }
[ -n "$FAKE_DIE" ] && { touch "$d/died"; exit 3; }
# A node slow to take its datadir: a probe started before it would take the datadir itself.
[ -n "$FAKE_SLOW" ] && sleep 1
echo $$ > "$d/pid"
exec sleep 60
"""

FAKE_CLI = """#!/bin/sh
for a in "$@"; do case "$a" in -datadir=*) d="${a#-datadir=}" ;; esac; m="$a"; done
# Wait for the node as -rpcwait does: its pid, or (FAKE_DIE) its exit.
until [ -s "$d/pid" ] || [ -e "$d/died" ]; do sleep 0.05; done
[ -e "$d/died" ] && sleep 0.5
[ "$m" = stop ] && { kill "$(cat "$d/pid")"; exit 0; }
[ -e "$d/died" ] && { echo "error: Could not connect to the server" >&2; exit 1; }
# FAKE_LATE_COOKIE=n: the first n calls find no cookie yet, which -rpcwait does not retry.
if [ -n "$FAKE_LATE_COOKIE" ]; then
  n=$(cat "$d/calls" 2>/dev/null || echo 0); echo $((n + 1)) > "$d/calls"
  [ "$n" -lt "$FAKE_LATE_COOKIE" ] && { echo "error: Could not locate RPC credentials." >&2; exit 1; }
fi
[ "$m" = getblockcount ] && { echo 0; exit 0; }
case " $FAKE_SERVE " in *" $m "*) echo '{}'; exit 0 ;; esac
# FAKE_CLI_FAIL's text instead of an answer, for the first FAKE_CLI_FAILS calls (default every call).
if [ -n "$FAKE_CLI_FAIL" ]; then
  n=$(cat "$d/fails" 2>/dev/null || echo 0); echo $((n + 1)) > "$d/fails"
  [ "$n" -lt "${FAKE_CLI_FAILS:-1000000}" ] && { printf '%b\n' "$FAKE_CLI_FAIL" >&2; exit 1; }
fi
echo "error code: -32601" >&2; echo "error message: Method not found" >&2; exit 1
"""


class Tripwire(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        d = Path(self.dir.name)
        self.rdashd, self.cli = d / "rdashd", d / "rdash-cli"
        for path, body in ((self.rdashd, FAKE_RDASHD), (self.cli, FAKE_CLI)):
            path.write_text(body)
            path.chmod(path.stat().st_mode | stat.S_IXUSR)
        self.env = dict(os.environ)

    def tearDown(self):
        os.environ.clear()
        os.environ.update(self.env)
        self.dir.cleanup()

    MESSAGES = {"datadir-lock": ("ND1", "Dash Core is probably already running.",
                                 "rdashd is probably already running.")}

    def wire(self, accept="", serve="", die="", name="rdashd", messages=MESSAGES, methods=None, slow="", late="",
             opt_fail="", cli_fail="", cli_fails=""):
        os.environ.update(FAKE_ACCEPT=accept, FAKE_SERVE=serve, FAKE_DIE=die, FAKE_NAME=name, FAKE_SLOW=slow,
                          FAKE_LATE_COOKIE=late, FAKE_OPT_FAIL=opt_fail, FAKE_CLI_FAIL=cli_fail,
                          FAKE_CLI_FAILS=cli_fails)
        return fc.tripwire(self.rdashd, self.cli, {"port": ("P2b", "11000"), "createwalletbackups": ("WL2a", "0")},
                           {"getmempoolinfo": "MP1a"} if methods is None else methods, self.dir.name, messages)

    def test_the_probe_waits_for_a_slow_node_with_no_methods(self):
        self.assertEqual(self.wire(methods={}, slow="1"), [])

    def test_a_late_cookie_is_retried_not_a_served_method(self):
        self.assertEqual(self.wire(late="3"), [])

    def test_a_node_that_dies_with_no_methods_stops_the_tripwire(self):
        self.assertIn("tripwire: rdashd exited", self.stopped(die="1", methods={}))

    def test_quiet_while_rdashd_refuses(self):
        self.assertEqual(self.wire(), [])

    def test_flags_an_accepted_option(self):
        stale = self.wire(accept="createwalletbackups=0")
        self.assertEqual(len(stale), 1)
        self.assertIn("-createwalletbackups=0", stale[0])

    def test_tests_the_frameworks_value(self):
        self.assertEqual(self.wire(accept="createwalletbackups=1"), [])

    def test_flags_a_message_that_no_longer_matches(self):
        stale = self.wire(name="Dash Core")
        self.assertEqual(len(stale), 1)
        self.assertIn("datadir-lock", stale[0])

    def test_flags_a_message_without_a_probe(self):
        stale = self.wire(messages={"nosuch": ("ND1", "a", "b")})
        self.assertEqual(stale, ["no probe for MESSAGE_ADAPTATIONS 'nosuch' (ND1)"])

    def stopped(self, **kwargs):
        """The tripwire's stderr for a run that stops as a harness error (exit 2, not 1: "differs")."""
        with contextlib.redirect_stderr(io.StringIO()) as err, self.assertRaises(SystemExit) as e:
            self.wire(**kwargs)
        self.assertEqual(e.exception.code, 2)
        return err.getvalue().strip()

    def test_a_node_that_dies_is_not_a_served_method(self):
        self.assertIn("tripwire: rdashd exited", self.stopped(die="1"))

    def test_flags_a_served_method(self):
        stale = self.wire(serve="getmempoolinfo")
        self.assertEqual(len(stale), 1)
        self.assertIn("getmempoolinfo", stale[0])

    def test_a_startup_race_is_retried(self):
        credentials = "error: Could not locate RPC credentials. No authentication cookie could be found"
        self.assertEqual(self.wire(cli_fail=credentials, cli_fails="1"), [])
        self.assertEqual(self.wire(cli_fail=credentials, cli_fails="1", serve="getmempoolinfo"),
                         ["rdashd serves getmempoolinfo now (MP1a): drop it from RPC_ADAPTATIONS"])

    @unittest.mock.patch.object(fc, "TRIPWIRE_RETRY", 1.5)
    def test_an_answer_neither_served_nor_missing_stops_it(self):
        for fail, quoted in (("error code: -28\\nerror message:\\nLoading", "error code: -28 error message: Loading"),
                             ("error: timeout on transient error: Could not connect to the server 127.0.0.1:1",
                              "error: timeout on transient error: Could not connect"),
                             ("error: Could not locate RPC credentials. No authentication cookie could be found",
                              "error: Could not locate RPC credentials.")):
            with self.subTest(fail=fail):
                self.assertIn("tripwire: rdash-cli getmempoolinfo answered neither a result nor -32601 in 1.5 s: "
                              + quoted, self.stopped(cli_fail=fail))

    def test_an_option_that_fails_without_its_refusal_stops_it(self):
        self.assertEqual(self.stopped(opt_fail="Error: Unable to bind to 127.0.0.1:1 on this computer."),
                         "tripwire: rdashd -createwalletbackups=0 exited 1 without its refusal:"
                         " Error: Unable to bind to 127.0.0.1:1 on this computer.")


PRD = textwrap.dedent("""\
    # PRD
    ## 1. Intro
    | US-NODE-99 | not in §2 | x | x | x | x | x |
    ## 2. Stories
    | ID | Title | a | b | Dash Core | c | Acceptance |
    |---|---|---|---|---|---|---|
    | US-NODE-12 | Logging | x | x | `-debug`, `logging` | x | `feature_logging.py` passes |
    | US-INT-14 | RPC | x | x | `getrpcinfo` | x | `rpc_*` pass |
    | US-DEV-07 | Tests | x | x | x | x | x |
    ## 3. Next
    """)


class MixedA(unittest.TestCase):
    # The shape of a framework traceback (Python 3.14, with its caret lines), as the test's
    # stdout and test_framework.log carry it.
    TB = textwrap.dedent("""\
        2026-10-26T02:48:38.131000Z TestFramework (ERROR): JSONRPC error
        Traceback (most recent call last):
          File "/t/functional/test_framework/test_framework.py", line 164, in main
            self.setup()
            ~~~~~~~~~~^^
          File "/t/functional/{script}", line 42, in run_test
            {code}
          File "/t/functional/test_framework/test_framework.py", line 736, in connect_nodes
            to_connection_subver = to_connection.getnetworkinfo()['subversion']
                                   ~~~~~~~~~~~~~~~~~~~~~~~~~~~~^^
          File "/t/functional/test_framework/authproxy.py", line 129, in __call__
            raise JSONRPCException(response['error'], status)
        {exc}
        2026-10-26T02:48:38.293000Z TestFramework (ERROR): https://github.com/dashpay/dash/issues
        """)

    def tb(self, exc, script="rpc_net.py", code="self.connect_nodes(0, 1)"):
        return self.TB.format(exc=exc, script=script, code=code)

    def test_dtf_scripts_follow_the_class_line(self):
        with tempfile.TemporaryDirectory() as d:
            functional = Path(d) / "src/test/functional"
            functional.mkdir(parents=True)
            for name, line in {"a": "class A (DashTestFramework):", "b": "class B(BitcoinTestFramework):",
                               "c": "class C(Mixin, DashTestFramework):",
                               "d": "from test_framework.test_framework import DashTestFramework"}.items():
                (functional / f"{name}.py").write_text(line + "\n")
            scripts = ["a.py", "a.py --x", "b.py", "c.py", "d.py"]
            self.assertEqual(fc.dtf_scripts(Path(d), scripts), ["a.py", "a.py --x", "c.py"])

    def test_failure_sections_split_on_failures_and_stop_at_the_summary(self):
        text = textwrap.dedent("""\
            Temporary test directory at /x
            1/3 - rpc_net.py --v1transport failed, Duration: 5 s

            stdout:
            one
            3/3 - p2p_instantsend.py failed, Duration: 7 s
            two

            TEST                   | STATUS    | DURATION
            rpc_net.py --v1transport | ✖ Failed  | 5 s
            """)
        sections = fc.failure_sections(text)
        self.assertEqual(sorted(sections), ["p2p_instantsend.py", "rpc_net.py --v1transport"])
        self.assertIn("one", sections["rpc_net.py --v1transport"])
        self.assertNotIn("two", sections["rpc_net.py --v1transport"])
        self.assertNotIn("Failed", sections["p2p_instantsend.py"])

    def test_a_missing_method_names_the_last_call(self):
        text = self.tb("test_framework.authproxy.JSONRPCException: Method not found (-32601)")
        self.assertEqual(fc.classify_failure(text), ("unimplemented-rpc", "getnetworkinfo"))

    def test_a_refused_option_wins_and_is_listed_once(self):
        err = "Error: -dip3params is not supported by rdashd yet (CS4)"
        text = self.tb(f"test_framework.test_node.FailedToStartError: {err}") + err + "\n"
        self.assertEqual(fc.classify_failure(text), ("unimplemented-option", "-dip3params"))

    def test_an_assertion_or_rpc_error_is_a_divergence_at_the_test_line(self):
        for exc in ("AssertionError: not(1 == 2)", "test_framework.authproxy.JSONRPCException: bad (-8)"):
            self.assertEqual(fc.classify_failure(self.tb(exc)), ("divergence", "rpc_net.py:42"), exc)

    def test_a_wait_is_a_timeout(self):
        # wait_until()'s own shape (util.py): an ERROR line with the predicate's source, then
        # main()'s "Assertion failed" and an exception line that shows only the source's start.
        text = textwrap.dedent("""\
            2026-10-26T03:00:00.000000Z TestFramework.utils (ERROR): wait_until() failed. Predicate: ''''
                    self.wait_until(lambda: node.getbestblockhash() == tip)
            '''
            2026-10-26T03:00:00.001000Z TestFramework (ERROR): Assertion failed
            Traceback (most recent call last):
              File "/t/functional/test_framework/test_framework.py", line 165, in main
                self.run_test()
              File "/t/functional/feature_x.py", line 42, in run_test
                self.wait_until(lambda: node.getbestblockhash() == tip)
              File "/t/functional/test_framework/util.py", line 300, in wait_until_helper_internal
                raise AssertionError("Predicate {} not true after {} seconds".format(predicate_source, timeout))
            AssertionError: Predicate ''''
                    self.wait_until(lambda: node.getbestblockhash() == tip)
            ''' not true after 60.0 seconds
            2026-10-26T03:00:00.002000Z TestFramework (INFO): Stopping nodes
            """)
        self.assertEqual(fc.classify_failure(text), ("timeout", "feature_x.py:42"))
        for exc in ("AssertionError: Block sync timed out after 60s:", "TimeoutError: timed out"):
            self.assertEqual(fc.classify_failure(self.tb(exc))[0], "timeout", exc)

    def test_the_cause_wins_over_a_shutdown_traceback(self):
        cause = self.tb("AssertionError: not(1 == 2)").replace("JSONRPC error", "Assertion failed")
        shutdown = textwrap.dedent("""\
            2026-10-26T02:48:39.000000Z TestFramework (ERROR): Unexpected exception caught during shutdown
            Traceback (most recent call last):
              File "/t/functional/test_framework/test_framework.py", line 360, in shutdown
                self.stop_nodes()
            ConnectionRefusedError: [Errno 111] Connection refused
            """)
        self.assertEqual(fc.classify_failure(cause + shutdown), ("divergence", "rpc_net.py:42"))

    def test_a_wrapped_rpc_is_named_through_getattr(self):
        # TestNode.createwallet and friends call the proxy as self.__getattr__('name')(...).
        text = self.tb("test_framework.authproxy.JSONRPCException: Method not found (-32601)").replace(
            "to_connection_subver = to_connection.getnetworkinfo()['subversion']",
            "return self.__getattr__('createwallet')(wallet_name, disable_private_keys)")
        self.assertEqual(fc.classify_failure(text), ("unimplemented-rpc", "createwallet"))

    def test_the_rpc_is_the_one_the_proxy_is_called_for(self):
        text = self.tb("test_framework.authproxy.JSONRPCException: Method not found (-32601)",
                       code="self.generate(self.nodes[1], 1)").replace(
            "to_connection_subver = to_connection.getnetworkinfo()['subversion']",
            "return self.generatetoaddress(nblocks, address)").replace(
            'test_framework.py", line 736, in connect_nodes', 'test_node.py", line 9, in generate').replace(
            "authproxy.py", "coverage.py")
        self.assertEqual(fc.classify_failure(text), ("unimplemented-rpc", "generatetoaddress"))

    # feature_llmq_is_retroactive.py:99 on a node without sendrawtransaction (v24.0, Python 3.14).
    NESTED = textwrap.dedent("""\
        2026-11-22T05:05:04.493000Z TestFramework (ERROR): JSONRPC error
        Traceback (most recent call last):
          File "/t/functional/test_framework/test_framework.py", line 167, in main
            self.run_test()
            ~~~~~~~~~~~~~^^
          File "/t/functional/feature_llmq_is_retroactive.py", line 99, in run_test
            self.nodes[3].sendrawtransaction(self.nodes[0].getrawtransaction(txid))
        {marker}
          File "/t/functional/test_framework/coverage.py", line 49, in __call__
            return_val = self.auth_service_proxy_instance.__call__(*args, **kwargs)
          File "/t/functional/test_framework/authproxy.py", line 129, in __call__
            raise JSONRPCException(response['error'], status)
        test_framework.authproxy.JSONRPCException: Method not found (-32601)
        2026-11-22T05:05:04.551000Z TestFramework (INFO): Stopping nodes
        """)
    OUTER = "    ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^"
    INNER = "                                     ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~^^^^^^"

    def nested(self, marker):
        return self.NESTED.format(marker=marker).replace("\n\n", "\n")

    def test_a_nested_call_names_the_call_at_the_marker(self):
        self.assertEqual(fc.classify_failure(self.nested(self.OUTER)), ("unimplemented-rpc", "sendrawtransaction"))
        self.assertEqual(fc.classify_failure(self.nested(self.INNER)), ("unimplemented-rpc", "getrawtransaction"))
        self.assertEqual(fc.first_failure(self.nested(self.OUTER))["rpc"]["method"], "sendrawtransaction")

    def test_a_nested_call_without_a_marker_names_the_outermost_call(self):
        self.assertEqual(fc.classify_failure(self.nested("")), ("unimplemented-rpc", "sendrawtransaction"))

    def test_a_marker_on_no_call_falls_back_to_the_outermost_call_in_its_span(self):
        caret = "    ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^"
        self.assertEqual(fc.classify_failure(self.nested(caret)), ("unimplemented-rpc", "sendrawtransaction"))

    def test_a_lambda_frame_names_the_call_in_its_body(self):
        code = "self.wait_until(lambda: self.nodes[3].getconnectioncount() == 0)"
        text = self.nested("").replace("self.nodes[3].sendrawtransaction(self.nodes[0].getrawtransaction(txid))",
                                       code).replace("line 99, in run_test", "line 99, in <lambda>")
        self.assertEqual(fc.classify_failure(text), ("unimplemented-rpc", "getconnectioncount"))

    def test_a_chain_names_its_last_call(self):
        text = self.nested("").replace("self.nodes[3].sendrawtransaction(self.nodes[0].getrawtransaction(txid))",
                                       "self.nodes[0].get_wallet_rpc('w').sendrawtransaction(raw)")
        self.assertEqual(fc.classify_failure(text), ("unimplemented-rpc", "sendrawtransaction"))

    def test_anything_else_is_the_harness_at_its_line_only(self):
        self.assertEqual(fc.classify_failure(self.tb("ConnectionRefusedError: [Errno 111]")), ("harness", "rpc_net.py:42"))
        self.assertEqual(fc.classify_failure("no trace here"), ("harness", "no traceback"))
        self.assertEqual(fc.classify_failure("Traceback (most recent call last):\nKeyError: 1"),
                         ("harness", "no traceback"))

    def test_results_need_an_rdashd_start(self):
        results = {"a.py": "Passed", "b.py": "Passed", "c.py": "Skipped", "d.py": "Failed", "e.py": "Failed"}
        sections = {n: self.tb("AssertionError: x", script=n) for n in ("d.py", "e.py")}
        starts = fc.count_starts(["a.py", "a.py", "d.py"], results)
        self.assertEqual(fc.mixed_a_entries(results, sections, starts), {
            "a.py": {"status": "pass"},
            "b.py": {"status": "skip", "detail": "no rdashd node"},
            "c.py": {"status": "skip", "detail": "skipped by the test"},
            "d.py": {"status": "fail", "class": "divergence", "detail": "d.py:42"},
            "e.py": {"status": "skip", "detail": "no rdashd node"},  # failed on dashd alone
        })

    def test_keep_crashed_moves_only_crashed_datadirs(self):
        with tempfile.TemporaryDirectory() as d:
            run = Path(d) / "run/test_runner_x"
            for name, err in (("a_1", "thread 'main' panicked at src/x.rs:1"), ("b_2", "")):
                (run / name / "node1/stderr").mkdir(parents=True)
                (run / name / "node1/stderr/tmp1").write_text(err)
            gone = Path(d) / "gone/test_runner_y/c_3/node1/stderr"  # an rdashd that went: kept whatever stderr says
            gone.mkdir(parents=True)
            (gone / "tmp1").write_text("")
            # A kept datadir is named exactly when known, not by its stem (c.py has two variants).
            self.assertEqual(fc.keep_crashed(Path(d) / "gone", Path(d) / "kept", ["c.py --y", "c.py --z"],
                                             {"c.py --y"}, {gone.parent.parent: "c.py --y"}), ["c.py --y"])
            self.assertEqual(len(list((Path(d) / "kept").glob("c_3-*/c_3/node1"))), 1)
            evidence = Path(d) / "out/possible-crashes"
            evidence.mkdir(parents=True, mode=0o755)  # a reused --out: tightened, nothing nested
            (evidence / "a_1").mkdir()
            self.assertEqual(fc.keep_crashed(Path(d) / "run", evidence, ["a.py --x", "b.py"]), ["a.py --x"])
            self.assertEqual(len(list(evidence.glob("a_1-*/a_1/node1"))), 1)
            self.assertEqual(stat.S_IMODE(evidence.stat().st_mode), 0o700)
            self.assertTrue((run / "b_2").is_dir())

    def test_a_peer_count_assert_is_the_observers_harness(self):
        count = self.tb("AssertionError: not(4 == 3)", code="assert_equal(self.nodes[0].getconnectioncount(), 3)")
        self.assertEqual(fc.classify_failure(count), ("divergence", "rpc_net.py:42"))
        self.assertEqual(fc.classify_failure(count, observer=True), ("harness", "rpc_net.py:42"))
        wait = self.tb("AssertionError: Predicate 'x' not true after 60 seconds",
                       code="self.wait_until(lambda: len(node.getpeerinfo()) == 2)")
        self.assertEqual(fc.classify_failure(wait), ("timeout", "rpc_net.py:42"))
        self.assertEqual(fc.classify_failure(wait, observer=True), ("harness", "rpc_net.py:42"))
        for code in ("self.mine_quorum(expected_connections=2)", "assert_equal(tip, best)"):
            other = self.tb("AssertionError: not(1 == 2)", code=code)
            self.assertEqual(fc.classify_failure(other, observer=True), ("divergence", "rpc_net.py:42"), code)
        # An RPC error from the count read is rdashd's answer, not the count.
        rpc = self.tb("test_framework.authproxy.JSONRPCException: bad (-1)", code="n = len(node.getpeerinfo())")
        self.assertEqual(fc.classify_failure(rpc, observer=True), ("divergence", "rpc_net.py:42"))

    def test_a_stopped_node_is_the_observers_harness(self):
        text = self.tb("AssertionError: [node 11] Error: no RPC connection", code="self.bump_mocktime(1)")
        self.assertEqual(fc.classify_failure(text), ("divergence", "rpc_net.py:42"))
        self.assertEqual(fc.classify_failure(text, observer=True), ("harness", "rpc_net.py:42"))

    def test_the_framework_frames_are_not_the_tests(self):
        rpc = self.tb("test_framework.authproxy.JSONRPCException: Method not found (-32601)")
        self.assertEqual(fc.classify_failure(rpc, observer=True), ("unimplemented-rpc", "getnetworkinfo"))

    def test_confirm_reruns_tell_flakes_from_changes(self):
        fail = {"status": "fail", "class": "timeout", "detail": "a.py:1"}
        divergence = {"status": "fail", "class": "divergence", "detail": "a.py:2"}
        committed = {"a.py": {"status": "pass"}, "b.py": {"status": "pass"}, "c.py": {"status": "pass"}}
        first = {"a.py": fail, "b.py": fail, "c.py": fail, "d.py": {"status": "pass"}}
        rerun = {"a.py": fail, "b.py": {"status": "pass"}, "c.py": divergence}
        entries, outcome = fc.confirm(committed, first, rerun)
        self.assertEqual(entries, {"a.py": fail, "b.py": {"status": "pass"}, "c.py": divergence,
                                   "d.py": {"status": "pass"}})
        self.assertEqual(outcome, {"a.py": "confirmed", "b.py": "flaky", "c.py": "unstable"})

    def test_an_exception_on_an_rpc_result_is_a_divergence(self):
        # Sol r1 #2: the test indexing, unpacking or parsing what rdashd answered.
        for exc in ("KeyError: 'subversion'", "TypeError: 'NoneType' object is not subscriptable",
                    "ValueError: invalid literal for int() with base 10: 'x'", "IndexError: list index out of range"):
            self.assertEqual(fc.classify_failure(self.tb(exc)), ("divergence", "rpc_net.py:42"), exc)

    def test_infrastructure_or_no_test_frame_is_the_harness(self):
        for exc in ("test_framework.test_node.FailedToStartError: [node 1] rdashd exited with status 1",
                    "subprocess.CalledProcessError: Command '['x']' returned non-zero exit status 1.",
                    "ConnectionResetError: [Errno 104] Connection reset by peer",
                    "http.client.RemoteDisconnected: Remote end closed connection without response",
                    "http.client.BadStatusLine: ''"):
            self.assertEqual(fc.classify_failure(self.tb(exc)), ("harness", "rpc_net.py:42"), exc)
        setup = self.tb("KeyError: 'subversion'").replace(
            '  File "/t/functional/rpc_net.py", line 42, in run_test\n    self.connect_nodes(0, 1)\n', "")
        self.assertEqual(fc.classify_failure(setup), ("harness", "no test frame"))

    def test_a_transport_error_is_rdashds_once_its_node_exited(self):
        # DEC-457: the Core harness logs the rdashd node a failing RPC was to (rust_nodes.RPC_FAILURE).
        reset = self.tb("ConnectionResetError: [Errno 104] Connection reset by peer")
        line = "2026-10-26T02:48:38.300000Z TestFramework (ERROR): rdashd RPC failure: node 2 {}\n"
        for state, cls in (("exited -6", "divergence"), ("exited 0", "divergence"), ("running", "harness")):
            self.assertEqual(fc.classify_failure(reset + line.format(state)), (cls, "rpc_net.py:42"), state)
        self.assertEqual(fc.classify_failure(reset), ("harness", "rpc_net.py:42"))  # a dashd node's RPC
        # A setup RPC (no test frame) to an rdashd that went is rdashd's too.
        setup = reset.replace('  File "/t/functional/rpc_net.py", line 42, in run_test\n    self.connect_nodes(0, 1)\n', "")
        self.assertEqual(fc.classify_failure(setup + line.format("exited 1")), ("divergence", "no test frame"))
        self.assertEqual(fc.classify_failure(setup), ("harness", "no test frame"))
        started = self.tb("test_framework.test_node.FailedToStartError: [node 2] rdashd exited")
        self.assertEqual(fc.classify_failure(started + line.format("exited 1")), ("harness", "rpc_net.py:42"))

    # Exception lines chosen apart from the implementation (Sol r2 #5): (line, class, flagged in
    # possible_crashes). http.client.LineTooLong is deliberately not in fc.TRANSPORT.
    EXCEPTIONS = (
        ("ConnectionResetError: [Errno 104] Connection reset by peer", "harness", True),
        ("ConnectionRefusedError: [Errno 111] Connection refused", "harness", True),
        ("ConnectionAbortedError: [Errno 103] Software caused connection abort", "harness", True),
        ("BrokenPipeError: [Errno 32] Broken pipe", "harness", True),
        ("http.client.RemoteDisconnected: Remote end closed connection without response", "harness", True),
        ("http.client.IncompleteRead: IncompleteRead(0 bytes read)", "harness", True),
        ("http.client.BadStatusLine: ''", "harness", True),
        ("http.client.LineTooLong: got more than 65536 bytes when reading header line", "harness", True),
        ("OSError: [Errno 28] No space left on device", "harness", False),
        ("subprocess.CalledProcessError: Command 'x' returned non-zero exit status 2.", "harness", False),
        ("KeyError: 'connections'", "divergence", False),
        ("test_framework.authproxy.JSONRPCException: Invalid parameter (-8)", "divergence", False),
    )

    def test_each_exception_line_is_classed_and_flagged(self):
        self.assertNotIn("LineTooLong", fc.TRANSPORT)
        gone = "2026-10-26T02:48:38.300000Z TestFramework (ERROR): rdashd RPC failure: node 2 exited -6\n"
        for exc, cls, flagged in self.EXCEPTIONS:
            text = self.tb(exc)
            self.assertEqual(fc.classify_failure(text), (cls, "rpc_net.py:42"), exc)
            self.assertEqual(bool(fc.CRASH.search(text)), flagged, exc)
            if flagged:  # a transport error on an rdashd that exited is rdashd's
                self.assertEqual(fc.classify_failure(text + gone)[0], "divergence", exc)
        # TestNode.stop_node logs this after an RPC that timed out: classed, but not flagged.
        shutdown = self.tb("http.client.CannotSendRequest: Request-sent")
        self.assertEqual((fc.classify_failure(shutdown)[0], bool(fc.CRASH.search(shutdown))), ("harness", False))
        # The observer's own death (Core's rejoin_rust_observer) is rdashd's, and flagged.
        died = self.tb("AssertionError: [node 5] the rdashd observer exited with status 1")
        self.assertEqual((fc.classify_failure(died)[0], bool(fc.CRASH.search(died))), ("divergence", True))

    def test_mode_a_exports_beside_the_pins_tree(self):
        w = Path("/w")
        self.assertEqual(fc.tree_dir(w, "v24.0", True), w / "v24.0-wallet")
        self.assertEqual(fc.tree_dir(w, "v24.0", True, fc.PINS["v24.0"].branch), w / "v24.0-wallet")
        self.assertEqual(fc.tree_dir(w, "v24.0", True, fc.MIXED_A_CORE_BRANCH), w / "v24.0-wallet-node_o12a-mixed-a-v2400")

    def test_a_rerun_without_rdashd_keeps_the_first_entry(self):
        rerun = {"a.py": fc.NO_RDASHD, "b.py": fc.NO_RDASHD, "c.py": {"status": "pass"}}
        self.assertEqual(fc.merge_rerun(rerun, {"c.py"}), ({"c.py": {"status": "pass"}}, ["a.py", "b.py"]))
        # DEC-457: unless it gives back a committed "no rdashd node".
        for committed, held in ((fc.NO_RDASHD, ["b.py"]), ({"status": "pass"}, ["a.py", "b.py"])):
            self.assertEqual(fc.merge_rerun(rerun, {"c.py"}, {"a.py": committed})[1], held, committed)

    def test_diff_and_totals(self):
        old = {"a.py": {"status": "pass"}, "b.py": {"status": "fail", "class": "timeout", "detail": "b.py:1"}}
        new = {"a.py": {"status": "pass"}, "c.py": {"status": "skip", "detail": "no rdashd node"}}
        self.assertEqual(fc.mixed_a_diff(old, new), ["b.py: fail timeout b.py:1 -> (absent)",
                                                     "c.py: (absent) -> skip no rdashd node"])
        self.assertEqual(fc.mixed_a_diff(old, old), [])
        self.assertEqual(fc.mixed_a_totals(old | new), {"pass": 1, "skip": 1, "unimplemented-rpc": 0,
                                                        "unimplemented-option": 0, "divergence": 0,
                                                        "harness": 0, "timeout": 1, "allowed-diff": 0})

    @staticmethod
    def refusal(capability="evo validation (CH7a) for DIP0003"):
        """refusal_text's line as rdashd logs it, from its format string in dashd-chainstate."""
        source = (HERE.parents[2] / "dashd-chainstate/src/connect/guard.rs").read_text()
        body = re.search(r'pub fn refusal_text.*?format!\(\s*"(.*?)",', source, re.S).group(1)
        text = re.sub(r"\\\n\s*", "", body)  # Rust's line continuations
        for value in ("ab" * 32, "2", "regtest"):
            text = text.replace("{}", value, 1)
        return ("2026-10-26T13:18:40Z (mocktime: 1417713366) [   dashd-v] [:0] [] *** "
                + text.replace("{capability}", capability) + "\n")

    def test_the_readiness_pattern_matches_only_the_abort_and_startup_forms(self):
        # The prefixes rdashd's logger may put before a message (dashd-node logging/mod.rs): none
        # (-logtimestamps=0), the fixture's, microseconds, source locations, a category or level.
        prefixes = ("", "2026-10-26T13:18:40Z (mocktime: 1417713366) [   dashd-v] [:0] [] ",
                    "2026-10-26T13:18:40.123456Z ", "2026-10-26T13:18:40Z [guard.rs:56] [refusal_text] ",
                    "2026-10-26T13:18:40Z [error] ", "[error] ",
                    "2026-10-26T13:18:40Z (mocktime: 2014-12-04T17:16:06Z) ")
        text = self.refusal().split("*** ", 1)[1].rstrip("\n")
        g2 = (text.replace("evo validation (CH7a) for DIP0003", "superblock and budget validation (SP1, GV1b)")
              + "; waiting for a ChainLock at or above 2")
        self.assertTrue(fc.READINESS_REFUSAL.search(self.refusal()))
        self.assertTrue(fc.READINESS_REFUSAL.search(self.refusal("quorum checks (CH7b)")))
        for prefix in prefixes:
            # DEC-1874: G2's wait is a warning behind ConnectTip's line, not the declared exit 1.
            wait = f"{prefix}ConnectTip: ConnectBlock {'ab' * 32} failed, {g2}"
            self.assertFalse(fc.READINESS_REFUSAL.search(f"before\n{wait}\nafter\n"), wait)
            for line in (f"{prefix}*** {text}", f"{prefix}Error: {text}"):
                self.assertTrue(fc.READINESS_REFUSAL.search(f"before\n{line}\nafter\n"), line)
            # Neither marker: the message alone is not the logged refusal.
            bare = f"{prefix}{text}"
            self.assertFalse(fc.READINESS_REFUSAL.search(f"before\n{bare}\nafter\n"), bare)
            # F1: VerifyDB's skip line carries the refusal, but it is not one.
            skip = f"{prefix}Skipped verification of level >=3: {text}"
            self.assertFalse(fc.READINESS_REFUSAL.search(f"before\n{skip}\nafter\n"), skip)
        for line in (f"foo *** {text}", f"x Error: {text}", f"2026-10-26T13:18:40Z foo *** {text}",
                     "*** " + text.replace("AD-CH3-4", "D18"), "*** " + text.replace("AD-CH3-4", "D17")):
            self.assertFalse(fc.READINESS_REFUSAL.search(line + "\n"), line)

    def test_the_allowed_class_names_guard_rs_row(self):
        fixture = self.refusal()
        self.assertNotRegex(fixture, r"[{}]", "guard.rs's format string has an unresolved placeholder")
        row = re.search(r"\(ALLOWED-DIFFS (AD-[A-Z0-9]+-\d+)\)", fixture)
        self.assertIsNotNone(row, fixture)
        self.assertEqual(row.group(1), fc.ALLOWED_READINESS["detail"])
        self.assertTrue(fc.READINESS_REFUSAL.search(fixture))

    def test_a_skip_line_alone_is_not_the_allowed_difference(self):
        skip = self.refusal().replace("*** ", "Skipped verification of level >=3: ")
        with tempfile.TemporaryDirectory() as d:
            datadir = Path(d) / "test_runner_x/a_1"
            (datadir / "node1/regtest").mkdir(parents=True)
            (datadir / "node1/regtest/debug.log").write_text(skip)
            section = (self.tb("test_framework.authproxy.JSONRPCException: Method not found (-32601)")
                       + f"x\nTestFramework (WARNING): Not cleaning up dir {datadir}\n")
            refused = fc.logged_refusal(fc.failed_datadirs({"a.py": section}))
        self.assertEqual(refused, set())
        entries = fc.mixed_a_entries({"a.py": "Failed"}, {"a.py": section}, {"a.py": 1}, True, refused)
        self.assertEqual(entries["a.py"]["class"], "unimplemented-rpc")

    def test_a_readiness_refusal_in_debug_log_is_the_allowed_difference(self):
        with tempfile.TemporaryDirectory() as d:
            sections = {}
            for name, log, why in (("a.py", self.refusal(), " due to perf data"), ("b.py", "nothing\n", ""),
                                   ("c.py", None, "")):
                datadir = Path(d) / f"test runner_x/{fc.stem(name)}_1"  # paths may hold spaces
                (datadir / "node1/regtest").mkdir(parents=True)
                if log is not None:
                    (datadir / "node1/regtest/debug.log").write_text(log)
                sections[name] = f"x\nTestFramework (WARNING): Not cleaning up dir {datadir}{why}\n"
            sections["d.py"] = "no datadir line"
            dirs = fc.failed_datadirs(sections)
            self.assertEqual(sorted(dirs), ["a.py", "b.py", "c.py"])
            self.assertEqual(fc.logged_refusal(dirs), {"a.py"})
        # It wins over whatever else the run failed with, an exit 1 at stop included.
        results = {n: "Failed" for n in ("a.py", "b.py")} | {"c.py": "Passed", "e.py": "Failed"}
        sections = {"a.py": self.tb("test_framework.authproxy.JSONRPCException: Method not found (-32601)")
                    + "AssertionError: [node 1] Node returned non-zero exit code (1) when stopping\n",
                    "b.py": self.tb("BrokenPipeError: [Errno 32] Broken pipe"), "e.py": "x"}
        starts = {"a.py": 1, "b.py": 1, "c.py": 1, "e.py": 0}
        entries = fc.mixed_a_entries(results, sections, starts, True, {"a.py", "b.py", "c.py", "e.py"})
        self.assertEqual(entries, {"a.py": fc.ALLOWED_READINESS, "b.py": fc.ALLOWED_READINESS, "c.py": {"status": "pass"},
                                   "e.py": fc.NO_RDASHD})
        self.assertEqual(fc.mixed_a_entries(results, sections, starts, True)["a.py"]["class"], "unimplemented-rpc")


class MixedARun(unittest.TestCase):
    """`mixed-a` end to end over a fake test_runner.py (FAKE_RUNNER): each runner name takes the
    next outcome of its list in the spec."""

    FAKE_RUNNER = textwrap.dedent("""\
        import json, os, sys
        from pathlib import Path
        ALL_SCRIPTS = ["a.py", "b.py"]
        if __name__ == "__main__":
            spec_file = Path(os.environ["FAKE_SPEC"])
            spec = json.loads(spec_file.read_text())
            names = [a for a in sys.argv[1:] if not a.startswith("--")]
            outcomes = {n: spec[n].pop(0) for n in names if spec[n]}  # none: left out of the summary
            spec_file.write_text(json.dumps(spec))
            if "no summary" in outcomes.values():
                sys.exit("runner died")
            with open(os.environ["RDASHD_STARTS"], "a") as starts:
                starts.write("".join(f"{n}\\n" * o["starts"] for n, o in outcomes.items()))
            tmp = Path(next(a for a in sys.argv if a.startswith("--tmpdirprefix=")).split("=", 1)[1])
            for i, (n, o) in enumerate(outcomes.items(), 1):
                if o["result"] == "Failed":
                    extra = ""
                    if "debuglog" in o:  # the datadir test_runner.py leaves for a failed test
                        datadir = tmp / "test_runner_x" / f"{n.split('.')[0]}_{i}"
                        (datadir / "node1/regtest").mkdir(parents=True)
                        (datadir / "node1/regtest/debug.log").write_text(o["debuglog"])
                        extra = f"\\nTestFramework (WARNING): Not cleaning up dir {datadir}"
                    print(f"{i}/{len(names)} - {n} failed, Duration: 1 s\\n{o['output']}{extra}")
            print("TEST | STATUS | DURATION")
            for n, o in outcomes.items():
                print(f"{n} | x {o['result']} | 1 s")
        """)
    TIMEOUT = MixedA.TB.format(exc="AssertionError: Predicate 'x' not true after 60 seconds",
                               script="a.py", code="self.wait_until(lambda: x)")
    BROKEN = MixedA.TB.format(exc="BrokenPipeError: [Errno 32] Broken pipe", script="a.py", code="x()")
    DIVERGENCE = MixedA.TB.format(exc="AssertionError: not(1 == 2)", script="a.py", code="assert_equal(1, 2)")

    def setUp(self):
        self.dir = Path(self.enterContext(tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR"))))
        tree = self.dir / "tree"
        functional = tree / "src/test/functional"
        functional.mkdir(parents=True)
        (tree / "tree.json").write_text(json.dumps({"commit": "c0ffee", "branch": fc.MIXED_A_CORE_BRANCH}))
        for path, tokens in fc.MIXED_A_HOOKS.items():
            (tree / "src" / path).parent.mkdir(parents=True, exist_ok=True)
            (tree / "src" / path).write_text("\n".join(tokens) + "\n")
        self.tree = tree
        (functional / "test_runner.py").write_text(self.FAKE_RUNNER)
        for name in ("a", "b"):
            (functional / f"{name}.py").write_text(f"class {name.upper()}(DashTestFramework):\n")
        self.passlists = self.dir / "passlists"  # HERE: where mixed_a reads and writes the passlist
        self.passlists.mkdir()
        self.enterContext(unittest.mock.patch.dict(os.environ, {"FAKE_SPEC": str(self.dir / "spec.json")}))
        for attr, value in {"build": lambda *a: tree, "pylib": lambda work: self.dir,
                            "check_adaptations": lambda tree, d, c, s: (d, c, []), "HERE": self.passlists,
                            "tag_of": lambda core, branch: f"v{fc.MIXED_A_RELEASE}"}.items():
            self.enterContext(unittest.mock.patch.object(fc, attr, value))

    def run_mixed_a(self, spec, committed=None, update=False):
        """mixed_a's exit status (or SystemExit) after a run over `spec`; self.out holds its files."""
        (self.dir / "spec.json").write_text(json.dumps(spec))
        if committed is not None:
            (self.passlists / "mixed-a.json").write_text(json.dumps({"tests": committed}))
        self.out = self.dir / "out"
        args = argparse.Namespace(core=None, work=self.dir, pin="v24.0", branch=fc.MIXED_A_CORE_BRANCH, rdashd="d", rdash_cli="c",
                                  tmpdirprefix=self.dir / "tmp", jobs=2, out=self.out, update=update,
                                  observer=False, tests=[])
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            return fc.mixed_a(args)

    def result(self):
        return json.loads((self.out / "mixed-a-v24.0.json").read_text())["tests"]

    def run_meta(self):
        return json.loads((self.out / "mixed-a-v24.0.run.json").read_text())

    def test_a_timeout_whose_rerun_starts_no_rdashd_stays_a_timeout(self):
        spec = {"a.py": [{"result": "Failed", "starts": 1, "output": self.TIMEOUT},
                         {"result": "Passed", "starts": 0}],
                "b.py": [{"result": "Passed", "starts": 0}]}
        self.assertEqual(self.run_mixed_a(spec), 1)  # no committed passlist: everything differs
        self.assertEqual(self.result(), {"a.py": {"status": "fail", "class": "timeout", "detail": "a.py:42"},
                                         "b.py": fc.NO_RDASHD})
        self.assertEqual(self.run_meta()["rerun_without_rdashd"], ["a.py"])

    def test_a_confirming_rerun_without_rdashd_is_unconfirmed(self):
        spec = {"a.py": [{"result": "Failed", "starts": 2, "output": self.DIVERGENCE},
                         {"result": "Failed", "starts": 0, "output": self.DIVERGENCE}],
                "b.py": [{"result": "Passed", "starts": 1}]}
        committed = {"a.py": {"status": "pass"}, "b.py": {"status": "pass"}}
        self.assertEqual(self.run_mixed_a(spec, committed), 1)
        self.assertEqual(self.result()["a.py"], {"status": "fail", "class": "divergence", "detail": "a.py:42"})
        self.assertEqual(self.run_meta()["confirm"], {"a.py": "unconfirmed"})

    def test_a_start_then_a_startup_failure_is_the_harness(self):
        startup = textwrap.dedent("""\
            2026-10-26T02:48:38.131000Z TestFramework (ERROR): Unexpected exception caught during testing
            Traceback (most recent call last):
              File "/t/functional/test_framework/test_framework.py", line 164, in main
                self.setup()
              File "/t/functional/test_framework/test_node.py", line 254, in wait_for_rpc_connection
                raise FailedToStartError(self._node_msg(
            test_framework.test_node.FailedToStartError: [node 1] rdashd exited with status 1 during initialization
            """)
        spec = {"a.py": [{"result": "Failed", "starts": 1, "output": startup}],
                "b.py": [{"result": "Passed", "starts": 1}]}
        committed = {"a.py": {"status": "fail", "class": "harness", "detail": "no test frame"},
                     "b.py": {"status": "pass"}}
        self.assertEqual(self.run_mixed_a(spec, committed), 0)
        self.assertEqual(self.result(), committed)

    def test_a_rerun_replaced_later_is_not_reported_as_held(self):
        spec = {"a.py": [{"result": "Failed", "starts": 1, "output": self.TIMEOUT},
                         {"result": "Passed", "starts": 0},  # the timeout's rerun: held
                         {"result": "Failed", "starts": 1, "output": self.DIVERGENCE}],  # the confirm's: taken
                "b.py": [{"result": "Passed", "starts": 1}]}
        committed = {"a.py": {"status": "pass"}, "b.py": {"status": "pass"}}
        self.assertEqual(self.run_mixed_a(spec, committed), 1)
        self.assertEqual(self.result()["a.py"], {"status": "fail", "class": "divergence", "detail": "a.py:42"})
        self.assertEqual(self.run_meta()["confirm"], {"a.py": "unstable"})
        self.assertEqual(self.run_meta()["rerun_without_rdashd"], [])

    def test_a_confirming_rerun_that_gives_back_no_rdashd_is_flaky(self):
        spec = {"a.py": [{"result": "Passed", "starts": 1}, {"result": "Passed", "starts": 0}],
                "b.py": [{"result": "Passed", "starts": 1}]}
        committed = {"a.py": fc.NO_RDASHD, "b.py": {"status": "pass"}}
        self.assertEqual(self.run_mixed_a(spec, committed), 0)
        self.assertEqual(self.result(), committed)
        self.assertEqual(self.run_meta()["confirm"], {"a.py": "flaky"})
        self.assertEqual(self.run_meta()["rerun_without_rdashd"], [])

    def test_a_skip_rerun_against_a_committed_pass_is_unconfirmed(self):
        # Sol r2 #4: committed pass, first run and rerun both without rdashd.
        spec = {"a.py": [{"result": "Passed", "starts": 0}, {"result": "Passed", "starts": 0}],
                "b.py": [{"result": "Passed", "starts": 1}]}
        committed = {"a.py": {"status": "pass"}, "b.py": {"status": "pass"}}
        self.assertEqual(self.run_mixed_a(spec, committed), 1)
        self.assertEqual(self.result()["a.py"], fc.NO_RDASHD)
        self.assertEqual(self.run_meta()["confirm"], {"a.py": "unconfirmed"})
        self.assertEqual(self.run_meta()["rerun_without_rdashd"], ["a.py"])

    def test_possible_crashes_flags_transport_errors_only(self):
        line = MixedA.TB.format(exc="http.client.LineTooLong: got more than 65536 bytes", script="a.py", code="x()")
        spec = {"a.py": [{"result": "Failed", "starts": 1, "output": line}],
                "b.py": [{"result": "Failed", "starts": 1, "output": self.DIVERGENCE.replace("a.py", "b.py")}]}
        self.run_mixed_a(spec)
        self.assertEqual(self.run_meta()["possible_crashes"], ["a.py"])
        self.assertEqual(self.result()["a.py"], {"status": "fail", "class": "harness", "detail": "a.py:42"})

    def test_a_tree_without_mode_as_hooks_is_a_broken_run(self):
        hooks = self.tree / "src/test/functional/test_framework/rust_nodes.py"
        hooks.write_text(hooks.read_text().replace("RPC_FAILURE", ""))
        spec = {n: [{"result": "Passed", "starts": 1}] for n in ("a.py", "b.py")}
        self.assertEqual(self.run_mixed_a(spec), 2)
        self.assertFalse((self.out / "mixed-a-v24.0.json").exists())
        hooks.write_text(hooks.read_text() + "RPC_FAILURE\n")
        (self.tree / "tree.json").write_text('{"commit": "c0ffee", "branch": "node/func-compat-v24.0"}')
        self.assertEqual(self.run_mixed_a(spec), 2)  # the wrong branch, hooks or not

    def test_mode_a_runs_its_own_release_not_the_pins(self):
        releases = []

        def build(core, work, pin, wallet, branch=None, release=None):
            releases.append(release)
            return self.tree
        moved = {"v24.0": fc.Pin("node/func-compat-v99.0", "v99.0.0", "99.0.0")}
        with unittest.mock.patch.object(fc, "build", build), unittest.mock.patch.object(fc, "PINS", moved):
            self.run_mixed_a({n: [{"result": "Passed", "starts": 1}] for n in ("a.py", "b.py")})
        self.assertEqual(releases, [fc.MIXED_A_RELEASE])

    def test_a_branch_from_another_release_is_refused_before_the_export(self):
        exported = []
        with unittest.mock.patch.object(fc, "tag_of", lambda core, branch: "v24.0.0-rc.4"), \
                unittest.mock.patch.object(fc, "build", lambda *a: exported.append(a)):
            self.assertEqual(self.run_mixed_a({}), 2)
        self.assertEqual(exported, [])

    def test_mixed_a_exports_the_mode_a_branch_by_default(self):
        with unittest.mock.patch.object(fc, "mixed_a", lambda args: args.branch):
            self.assertEqual(fc.main(["mixed-a", "--pin", "v24.0"]), fc.MIXED_A_CORE_BRANCH)
            self.assertEqual(fc.main(["mixed-a", "--pin", "v24.0", "--branch", "x"]), "x")

    def test_refused_runs_are_allowed_and_a_stop_exit_is_kept(self):
        stop = "AssertionError: [node 1] Node returned non-zero exit code (1) when stopping\n"
        broken = self.BROKEN
        spec = {"a.py": [{"result": "Failed", "starts": 1, "output": broken + stop, "debuglog": MixedA.refusal()}],
                "b.py": [{"result": "Failed", "starts": 1, "output": self.DIVERGENCE.replace("a.py", "b.py") + stop,
                          "debuglog": "no refusal\n"}]}
        self.run_mixed_a(spec)
        self.assertEqual(self.result()["a.py"], fc.ALLOWED_READINESS)
        self.assertEqual(self.result()["b.py"], {"status": "fail", "class": "divergence", "detail": "b.py:42"})
        # The refusal's abort is neither flagged nor kept; the unexplained exit at stop is kept.
        self.assertEqual(self.run_meta()["possible_crashes"], ["b.py"])
        self.assertEqual([p.name.rsplit("-", 1)[0] for p in (self.out / "possible-crashes").iterdir()], ["b_2"])

    def test_a_crash_beyond_the_refusal_is_still_flagged_and_kept(self):
        broken = self.BROKEN
        for extra, flagged in (("AssertionError: [node 1] Node returned non-zero exit code (1) when stopping\n", False),
                               ("AssertionError: [node 1] Node returned non-zero exit code (-6) when stopping\n", True),
                               ("thread 'main' panicked at dash-p2p/src/x.rs:1\n", True),
                               ("2026 TestFramework (ERROR): rdashd RPC failure: node 1 exited 1\n", False),
                               ("2026 TestFramework (ERROR): rdashd RPC failure: node 2 exited -11\n", True),
                               ("2026 TestFramework (ERROR): rdashd RPC failure: node 2 exited 101\n", True)):
            with self.subTest(extra=extra):
                spec = {"a.py": [{"result": "Failed", "starts": 1, "output": broken + extra,
                                  "debuglog": MixedA.refusal()}], "b.py": [{"result": "Passed", "starts": 1}]}
                self.run_mixed_a(spec)
                self.assertEqual(self.result()["a.py"], fc.ALLOWED_READINESS)
                self.assertEqual(self.run_meta()["possible_crashes"], ["a.py"] if flagged else [])
                crashes = self.out / "possible-crashes"
                self.assertEqual(len(list(crashes.iterdir())) if crashes.exists() else 0, int(flagged))
                shutil.rmtree(self.out)

    def test_an_rpc_to_an_exited_rdashd_keeps_the_datadir(self):
        gone = (self.BROKEN
                + "2026 TestFramework (ERROR): rdashd RPC failure: node 2 exited 1\n")
        spec = {"a.py": [{"result": "Failed", "starts": 1, "output": gone, "debuglog": "no refusal\n"}],
                "b.py": [{"result": "Passed", "starts": 1}]}
        self.run_mixed_a(spec)
        self.assertEqual(self.result()["a.py"], {"status": "fail", "class": "divergence", "detail": "a.py:42"})
        self.assertEqual(self.run_meta()["possible_crashes"], ["a.py"])
        self.assertEqual(len(list((self.out / "possible-crashes").iterdir())), 1)

    def test_a_failure_after_the_run_report_writes_no_result(self):
        spec = {n: [{"result": "Passed", "starts": 1}] for n in ("a.py", "b.py")}
        with unittest.mock.patch.object(fc, "mixed_a_diff", side_effect=RuntimeError), \
                self.assertRaises(RuntimeError):
            self.run_mixed_a(spec)
        self.assertTrue((self.out / "mixed-a-v24.0.run.json").exists())
        self.assertEqual(sorted(p.name for p in self.out.glob("mixed-a-v24.0.json*")), [])

    def test_a_runner_without_a_summary_writes_no_result(self):
        for died in ({"a.py": ["no summary"], "b.py": [{"result": "Passed", "starts": 1}]},
                     {"a.py": [{"result": "Passed", "starts": 1}]}):  # b.py missing from the summary
            with self.subTest(died=died), self.assertRaises(SystemExit) as e:
                self.run_mixed_a({"b.py": [], **died}, update=True)
            self.assertIn("reported no result", str(e.exception.code))
            self.assertFalse((self.out / "mixed-a-v24.0.json").exists())
            self.assertFalse((self.passlists / "mixed-a.json").exists())

    def test_update_then_a_matching_run(self):
        spec = {"a.py": [{"result": "Passed", "starts": 1}], "b.py": [{"result": "Passed", "starts": 1}]}
        self.assertEqual(self.run_mixed_a(spec, update=True), 0)
        self.assertEqual(self.result(), {"a.py": {"status": "pass"}, "b.py": {"status": "pass"}})
        spec = {n: [{"result": "Passed", "starts": 1}] for n in ("a.py", "b.py")}
        self.assertEqual(self.run_mixed_a(spec), 0)
        self.assertEqual(list(self.out.glob("*.partial")), [])


class G2(unittest.TestCase):
    # A masternode that did not come up (start_masternodes' pool re-raises it from start_node).
    START = textwrap.dedent("""\
        2026-11-20T22:18:25.300000Z TestFramework (ERROR): Unexpected exception caught during testing
        Traceback (most recent call last):
          File "/t/functional/test_framework/test_framework.py", line 166, in main
            self.setup()
            ~~~~~~~~~~^^
          File "/t/functional/test_framework/test_framework.py", line 1952, in setup_network
            self.start_masternodes()
          File "/t/functional/test_framework/test_framework.py", line 1901, in start_masternode
            self.start_node(
          File "/t/functional/test_framework/test_framework.py", line 683, in start_node
            node.wait_for_rpc_connection()
          File "/t/functional/test_framework/test_node.py", line 303, in wait_for_rpc_connection
            raise FailedToStartError(self._node_msg(
        test_framework.test_node.FailedToStartError: [node 3] dashd exited with status 1 during initialization. {err}
        2026-11-20T22:18:25.310000Z TestFramework (INFO): Stopping nodes
        """)
    # An RPC the framework calls in setup, through a generator expression.
    SETUP_RPC = textwrap.dedent("""\
        2026-11-20T22:19:25.300000Z TestFramework (ERROR): JSONRPC error
        Traceback (most recent call last):
          File "/t/functional/test_framework/test_framework.py", line 166, in main
            self.setup()
          File "/t/functional/test_framework/test_framework.py", line 1963, in setup_network
            self.wait_for_sporks_same()
          File "/t/functional/test_framework/test_framework.py", line 2170, in check_sporks_same
            return all(node.spork('show') == sporks for node in self.nodes[1:])
          File "/t/functional/test_framework/test_framework.py", line 2170, in <genexpr>
            return all(node.spork('show') == sporks for node in self.nodes[1:])
          File "/t/functional/test_framework/coverage.py", line 50, in __call__
            return_val = self.auth_service_proxy_instance.__call__(*args, **kwargs)
          File "/t/functional/test_framework/authproxy.py", line 129, in __call__
            raise JSONRPCException(response['error'], status)
        test_framework.authproxy.JSONRPCException: Method not found (-32601)
        """)
    # A probe run with the spork skip, stopped by rdashd's unserved `quorum dkgstatus` subcommand.
    UNSERVED_DKGSTATUS = textwrap.dedent("""\
        2026-11-21T00:10:25.300000Z TestFramework (INFO): rdashd probe skip: spork on node 3
        2026-11-21T00:11:25.300000Z TestFramework (ERROR): JSONRPC error
        Traceback (most recent call last):
          File "/t/functional/test_framework/test_framework.py", line 167, in main
            self.run_test()
          File "/t/functional/feature_llmq_g2_dkg.py", line 29, in run_test
            q = self.mine_quorum()
          File "/t/functional/test_framework/test_framework.py", line 2394, in mine_quorum
            self.wait_for_quorum_phase(q, 1, expected_members, None, 0, mninfos_online, llmq_type_name=llmq_type_name)
          File "/t/functional/test_framework/test_framework.py", line 2268, in check_dkg_session
            s = self.quorum_dkgstatus(mn.get_node(self))
          File "/t/functional/test_framework/test_framework.py", line 2181, in quorum_dkgstatus
            return self.rust_nodes.probe_read(node, "quorum.dkgstatus", lambda: node.quorum("dkgstatus"))
          File "/t/functional/test_framework/rust_nodes.py", line 401, in probe_read
            return read()
          File "/t/functional/test_framework/test_framework.py", line 2181, in <lambda>
            return self.rust_nodes.probe_read(node, "quorum.dkgstatus", lambda: node.quorum("dkgstatus"))
          File "/t/functional/test_framework/coverage.py", line 50, in __call__
            return_val = self.auth_service_proxy_instance.__call__(*args, **kwargs)
          File "/t/functional/test_framework/authproxy.py", line 129, in __call__
            raise JSONRPCException(response['error'], status)
        test_framework.authproxy.JSONRPCException: Must be a valid command (-8)
        """)
    # run_test passed; then the rdashd node exits non-zero when stopped.
    SHUTDOWN = textwrap.dedent("""\
        2026-11-20T22:18:25.300000Z TestFramework (INFO): Stopping nodes
        2026-11-20T22:18:26.300000Z TestFramework (ERROR): Unexpected exception caught during shutdown
        Traceback (most recent call last):
          File "/t/functional/test_framework/test_framework.py", line 372, in shutdown
            self.stop_nodes()
          File "/t/functional/test_framework/test_framework.py", line 700, in stop_nodes
            node.wait_until_stopped()
          File "/t/functional/test_framework/test_node.py", line 480, in is_node_stopped
            assert return_code == 0, self._node_msg(
        AssertionError: [node 3] Node returned non-zero exit code (1) when stopping
        2026-11-20T22:18:26.400000Z TestFramework (WARNING): Not cleaning up dir /x
        """)
    # The test timeout fired with an RPC blocked on its socket, before rust_nodes.TestTimeout:
    # authproxy turned the TimeoutError into a -344 JSONRPCException.
    TIMEOUT_IN_RPC = textwrap.dedent("""\
        2026-11-20T22:18:25.300000Z TestFramework (ERROR): JSONRPC error
        Traceback (most recent call last):
          File "/t/functional/test_framework/authproxy.py", line 151, in _get_response
            http_response = self.__conn.getresponse()
          File "/t/functional/test_framework/rust_nodes.py", line 185, in expire
            raise TimeoutError(f"test timed out after {seconds} s (RDASHD_TEST_TIMEOUT)")
        TimeoutError: test timed out after 900 s (RDASHD_TEST_TIMEOUT)

        During handling of the above exception, another exception occurred:

        Traceback (most recent call last):
          File "/t/functional/test_framework/test_framework.py", line 167, in main
            self.run_test()
          File "/t/functional/feature_llmq_signing.py", line 88, in run_test
            sig_share_rpc_1 = self.mninfo[2].get_node(self).quorum("sign", q_type, id, msgHash, quorumHash, False)
          File "/t/functional/test_framework/coverage.py", line 50, in __call__
            return_val = self.auth_service_proxy_instance.__call__(*args, **kwargs)
          File "/t/functional/test_framework/authproxy.py", line 153, in _get_response
            raise JSONRPCException({
        test_framework.authproxy.JSONRPCException: 'quorum' RPC took longer than 60.000000 seconds. Consider using larger timeout for calls that take longer to return. (-344)
        """)
    # The same with rust_nodes.TestTimeout, which the proxy does not catch.
    TEST_TIMEOUT = textwrap.dedent("""\
        2026-11-20T22:18:25.300000Z TestFramework (ERROR): Unexpected exception caught during testing
        Traceback (most recent call last):
          File "/t/functional/test_framework/test_framework.py", line 167, in main
            self.run_test()
          File "/t/functional/feature_llmq_signing.py", line 88, in run_test
            sig_share_rpc_1 = self.mninfo[2].get_node(self).quorum("sign", q_type, id, msgHash, quorumHash, False)
          File "/t/functional/test_framework/authproxy.py", line 151, in _get_response
            http_response = self.__conn.getresponse()
          File "/t/functional/test_framework/rust_nodes.py", line 190, in expire
            raise TestTimeout(f"test timed out after {seconds} s (RDASHD_TEST_TIMEOUT)")
        test_framework.rust_nodes.TestTimeout: test timed out after 900 s (RDASHD_TEST_TIMEOUT)
        """)

    def test_stage_follows_the_frames(self):
        self.assertEqual(fc.g2_stage(self.START.format(err="Error: x")), "start")
        self.assertEqual(fc.g2_stage(self.SETUP_RPC), "setup")
        self.assertEqual(fc.g2_stage(MixedA.TB.format(exc="AssertionError: no", script="a.py", code="x()")), "run")
        self.assertEqual(fc.g2_stage("no traceback here"), fc.UNKNOWN)
        # Failing to stop after run_test is further than run, with or without a traceback.
        self.assertEqual(fc.g2_stage(self.SHUTDOWN), "stop")
        self.assertEqual(fc.g2_stage("Node returned non-zero exit code (1) when stopping"), "stop")
        # A run_test failure followed by a failed shutdown is the run's.
        run = MixedA.TB.format(exc="AssertionError: no", script="a.py", code="x()")
        self.assertEqual(fc.g2_stage(run + self.SHUTDOWN), "run")
        # A node that fails to restart inside run_test is the run's.
        restart = self.START.replace("in setup_network", "in run_test").format(err="Error: x")
        self.assertEqual(fc.g2_stage(restart), "run")

    def test_entries(self):
        refused = "Error: -masternodeblsprivkey is not supported by rdashd yet (DK1)."
        self.assertEqual(fc.g2_entry("Failed", self.START.format(err=refused) + refused + "\n"),
                         {"status": "fail", "stage": "start", "class": "unimplemented-option",
                          "detail": "-masternodeblsprivkey"})
        refusal = self.START.format(err="Error: Masternode must relay transactions")
        self.assertEqual(fc.g2_entry("Failed", refusal, {3}),
                         {"status": "fail", "stage": "start", "class": fc.INIT_REFUSAL,
                          "detail": "test_framework.py:start_node"})
        # Only the rdashd node's exit is rdashd's: a dashd node that did not start (node 3 is
        # not the rdashd node here, or no node is known to be) is the harness's.
        for rdashd_nodes in ({4}, set()):
            self.assertEqual(fc.g2_entry("Failed", refusal, rdashd_nodes)["class"], "harness", rdashd_nodes)
        self.assertEqual(fc.g2_entry("Failed", self.SETUP_RPC),
                         {"status": "fail", "stage": "setup", "class": "unimplemented-rpc", "detail": "spork"})
        self.assertEqual(fc.g2_entry("Failed", MixedA.TB.format(exc="AssertionError: no", script="a.py", code="x()")),
                         {"status": "fail", "stage": "run", "class": "divergence", "detail": "a.py:42"})
        self.assertEqual(fc.g2_entry("Passed", ""), {"status": "pass", "stage": "pass"})
        self.assertEqual(fc.g2_entry("Skipped", "")["status"], "skip")

    def test_an_unserved_subcommand_is_an_unimplemented_rpc(self):
        self.assertEqual(fc.g2_entry("Failed", self.UNSERVED_DKGSTATUS),
                         {"status": "fail", "stage": "run", "class": "unimplemented-rpc", "detail": "quorum dkgstatus"})
        # No literal subcommand to name: the test line's divergence, as before.
        bare = self.UNSERVED_DKGSTATUS.replace('node.quorum("dkgstatus")', "node.quorum(sub)")
        self.assertEqual(fc.g2_entry("Failed", bare)["class"], "divergence")
        other = self.UNSERVED_DKGSTATUS.replace("Must be a valid command (-8)", "Invalid LLMQ type (-8)")
        self.assertEqual(fc.g2_entry("Failed", other)["detail"], "feature_llmq_g2_dkg.py:29")

    def test_probe_list(self):
        self.assertEqual(fc.probe_list(" spork,quorum.dkgstatus,, spork"), ["quorum.dkgstatus", "spork"])
        self.assertEqual(fc.probe_list("-spentindex,masternodeblsprivkey"), ["masternodeblsprivkey", "spentindex"])
        self.assertEqual(fc.probe_list(None), [])
        for name in ("spork", "quorum.dkgstatus"):
            self.assertTrue(fc.PROBE_SKIP_NAME.fullmatch(name))
        for name in ("quorum dkgstatus", "Spork", "quorum.", "a.b.c", "spork=1"):
            self.assertFalse(fc.PROBE_SKIP_NAME.fullmatch(name), name)

    def test_the_milestone_test_is_registered_with_its_committed_entry(self):
        self.assertIn("feature_llmq_g2_dkg", fc.G2_STEMS)
        committed = json.loads((fc.HERE / "g2-llmq.json").read_text())["tests"]
        self.assertEqual({fc.stem(name) for name in committed}, set(fc.G2_STEMS))
        self.assertEqual(fc.G2_CORE_BRANCH, "node/func-compat-g2dkg-v2400")

    def test_a_test_timeout_is_a_timeout_even_through_an_rpc(self):
        for text in (self.TIMEOUT_IN_RPC, self.TEST_TIMEOUT):
            self.assertEqual(fc.g2_entry("Failed", text),
                             {"status": "fail", "stage": "run", "class": "timeout",
                              "detail": "feature_llmq_signing.py:88"})

    def test_where_skips_anonymous_framework_frames(self):
        frames, _ = fc.cause_traceback(self.SETUP_RPC)
        self.assertEqual(fc.g2_where(frames), "test_framework.py:check_sporks_same")

    def test_compare(self):
        start = {"status": "fail", "stage": "start", "class": "unimplemented-option", "detail": "-a"}
        start_b = dict(start, detail="-b")
        setup = {"status": "fail", "stage": "setup", "class": "unimplemented-rpc", "detail": "spork"}
        passed = {"status": "pass", "stage": "pass"}
        regressions, progress, changes = fc.g2_compare(
            {"down.py": setup, "gone.py": passed, "up.py": start, "same.py": start, "moved.py": start, "lost.py": start},
            {"down.py": start, "gone.py": setup, "up.py": setup, "same.py": start, "moved.py": start_b, "new.py": start})
        self.assertEqual([l.split(":")[0] for l in regressions], ["down.py", "gone.py", "lost.py"])
        self.assertEqual(progress, ["up.py: fail start unimplemented-option -a -> fail setup unimplemented-rpc spork"])
        self.assertEqual([l.split(":")[0] for l in changes], ["moved.py", "new.py"])
        # A failure no traceback places is neither progress nor a regression, either way.
        unknown = {"status": "fail", "stage": fc.UNKNOWN, "class": "harness", "detail": "no traceback"}
        stop = {"status": "fail", "stage": "stop", "class": "harness", "detail": "test_framework.py:stop_nodes"}
        run = {"status": "fail", "stage": "run", "class": "divergence", "detail": "a.py:1"}
        regressions, progress, changes = fc.g2_compare(
            {"u1.py": start, "u2.py": setup, "u3.py": passed, "s.py": run, "r.py": stop},
            {"u1.py": unknown, "u2.py": unknown, "u3.py": unknown, "s.py": stop, "r.py": run})
        self.assertEqual([l.split(":")[0] for l in changes], ["u1.py", "u2.py", "u3.py"])
        self.assertEqual([l.split(":")[0] for l in progress], ["s.py"])
        self.assertEqual([l.split(":")[0] for l in regressions], ["r.py"])
        # A partial run says nothing about what it did not run.
        self.assertEqual(fc.g2_compare({"lost.py": start}, {}, complete=False), ([], [], []))

    def test_first_failure_and_rdashd_logs(self):
        text = self.SETUP_RPC + "rdashd RPC failure: node 3 exited -6\n"
        first = fc.first_failure(text)
        self.assertEqual(first["rpc"], {"method": "spork", "error": "Method not found (-32601)"})
        self.assertEqual(first["where"], "test_framework.py:check_sporks_same")
        self.assertEqual(first["node_exits"], {"3": -6})
        with tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR")) as d:
            for i in (0, 3):
                (Path(d) / f"node{i}/regtest").mkdir(parents=True)
                (Path(d) / f"node{i}/stderr").mkdir()
                (Path(d) / f"node{i}/regtest/debug.log").write_text("".join(f"line {n}\n" for n in range(50)))
            (Path(d) / "node3/stderr/tmp1").write_text("Error: boom\n")
            (Path(d) / f"node3/{fc.G2_NODE_MARKER}").touch()
            nodes = fc.rdashd_logs(d, text)
        self.assertEqual([(n["node"], n["exit"], n["stderr_tail"]) for n in nodes], [(3, -6, ["Error: boom"])])
        self.assertEqual(nodes[0]["debug_log_tail"][-1], "line 49")
        self.assertEqual(len(nodes[0]["debug_log_tail"]), fc.TAIL)

    def test_runner_durations(self):
        text = "TEST | STATUS | DURATION\n\nfeature_llmq_signing.py --spork21 | ✖ Failed  | 9 s\na.py | ✓ Passed | 120 s\n"
        self.assertEqual(fc.runner_durations(text), {"feature_llmq_signing.py --spork21": 9, "a.py": 120})

    def test_hooks_name_the_core_branch_tokens(self):
        self.assertIn("test/functional/test_framework/rust_nodes.py", fc.G2_HOOKS)
        self.assertIn("RDASHD_PROBE_SKIP", fc.G2_HOOKS["test/functional/test_framework/rust_nodes.py"])
        self.assertIn("quorum_dkgstatus", fc.G2_HOOKS["test/functional/test_framework/test_framework.py"])
        self.assertIn("MASTERNODE_DROPS", fc.G2_HOOKS["test/functional/test_framework/rust_nodes.py"])
        self.assertIn(fc.G2_NODE_MARKER, ("rdashd.node",))


class G2Run(unittest.TestCase):
    """`g2` end to end over MixedARun's fake test_runner.py, the failed tests' rdashd node marked."""

    FAKE_RUNNER = MixedARun.FAKE_RUNNER.replace(
        '''(datadir / "node1/regtest/debug.log").write_text(o["debuglog"])''',
        '''(datadir / "node1/regtest/debug.log").write_text(o["debuglog"]); (datadir / "node1/rdashd.node").touch()''')
    REFUSED = G2.START.format(err="Error: -k is not supported by rdashd yet (DK1).") + \
        "Error: -k is not supported by rdashd yet (DK1).\n"

    def setUp(self):
        self.dir = Path(self.enterContext(tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR"))))
        tree = self.dir / "tree"
        functional = tree / "src/test/functional"
        functional.mkdir(parents=True)
        (tree / "tree.json").write_text(json.dumps({"commit": "c0ffee", "branch": fc.G2_CORE_BRANCH}))
        for path, tokens in fc.G2_HOOKS.items():
            (tree / "src" / path).parent.mkdir(parents=True, exist_ok=True)
            (tree / "src" / path).write_text("\n".join(tokens) + "\n")
        with (tree / "src/test/functional/test_framework/rust_nodes.py").open("a") as f:
            f.write('PROBE_SKIPS = {"spork": "RP2b", "quorum.dkgstatus": "DK1"}\n')
        (functional / "test_runner.py").write_text(self.FAKE_RUNNER.replace('["a.py", "b.py"]', '["a.py", "b.py", "c.py"]'))
        for name in ("a", "b", "c"):
            (functional / f"{name}.py").write_text(f"class {name.upper()}(DashTestFramework):\n")
        self.tree, self.here = tree, self.dir / "here"
        self.here.mkdir()
        self.enterContext(unittest.mock.patch.dict(os.environ, {"FAKE_SPEC": str(self.dir / "spec.json"),
                                                                "G2_PORT_LOCK": str(self.dir / "ports.lock")}))
        for attr, value in {"build": lambda *a: tree, "pylib": lambda work: self.dir,
                            "check_adaptations": lambda tree, d, c, s: (d, c, []), "HERE": self.here,
                            "tag_of": lambda core, branch: f"v{fc.G2_RELEASE}", "G2_STEMS": ("a", "b")}.items():
            self.enterContext(unittest.mock.patch.object(fc, attr, value))

    def run_g2(self, spec, committed=None, update=False, probe=None, mn=fc.G2_DEFAULT_MN, tests=(), skip=None):
        (self.dir / "spec.json").write_text(json.dumps(spec))
        if committed is not None:
            (self.here / "g2-llmq.json").write_text(json.dumps({"tests": committed}))
        self.out = self.dir / "out"
        args = argparse.Namespace(core=None, work=self.dir, pin="v24.0", branch=fc.G2_CORE_BRANCH, rdashd="d",
                                  rdash_cli="c", tmpdirprefix=self.dir / "tmp", jobs=2, out=self.out, update=update,
                                  mn=mn, test_timeout=60, probe_drop=probe, probe_skip=skip, tests=list(tests))
        self.stdout = io.StringIO()
        with contextlib.redirect_stdout(self.stdout), contextlib.redirect_stderr(io.StringIO()):
            return fc.g2(args)

    def committed(self):
        return json.loads((self.here / "g2-llmq.json").read_text())

    def refused(self, starts=1):
        return {"result": "Failed", "starts": starts, "output": self.REFUSED, "debuglog": "rdashd version v0\n"}

    # A runner that leaves a grandchild (as test_runner.py leaves its tests' nodes) and runs on.
    SLEEPER = textwrap.dedent("""\
        import os, subprocess, time
        from pathlib import Path
        ALL_SCRIPTS = ["a.py", "b.py", "c.py"]
        if __name__ == "__main__":
            child = subprocess.Popen(["sleep", "300"])
            Path(os.environ["FAKE_GRANDCHILD"]).write_text(str(child.pid))
            time.sleep(30)
        """)

    def test_a_sigterm_stops_the_runners_group_before_the_port_lock_goes(self):
        (self.tree / "src/test/functional/test_runner.py").write_text(self.SLEEPER)
        pidfile, lock = self.dir / "grandchild", self.dir / "ports.lock"
        self.enterContext(unittest.mock.patch.dict(os.environ, {"FAKE_GRANDCHILD": str(pidfile)}))
        # Swallowed here, so a g2 without its own SIGTERM handling fails this test instead of ending it.
        self.addCleanup(signal.signal, signal.SIGTERM, signal.signal(signal.SIGTERM, lambda *_: None))
        seen = {}

        def watch():
            """The grandchild's state the moment the port lock is free again."""
            pid = int(pidfile.read_text())
            deadline = time.monotonic() + 60
            while not lock_free(lock) and time.monotonic() < deadline:
                time.sleep(0.01)
            seen["alive at release"] = alive(pid)
        thread = sigterm_when(pidfile, watch)
        try:
            status = self.run_g2({})
        except SystemExit as e:
            status = e.code
        thread.join(60)
        pid = int(pidfile.read_text())
        self.addCleanup(lambda: alive(pid) and os.kill(pid, signal.SIGKILL))
        self.assertEqual(status, 143)
        self.assertEqual(seen, {"alive at release": False})
        self.assertFalse(alive(pid))
        self.assertTrue(lock_free(lock))

    def test_update_writes_the_default_runs_entries_and_report(self):
        spec = {"a.py": [self.refused()], "b.py": [{"result": "Passed", "starts": 1}]}
        self.assertEqual(self.run_g2(spec, update=True), 0)
        self.assertEqual(self.committed(), {"mask": "mn:2", "pin": "v24.0", "tests": {
            "a.py": {"status": "fail", "stage": "start", "class": "unimplemented-option", "detail": "-k"},
            "b.py": {"status": "pass", "stage": "pass"}}})
        report = json.loads((self.out / "g2-llmq-v24.0.run.json").read_text())
        self.assertEqual(report["tests"]["a.py"]["first_failure"]["refused_options"], ["-k"])
        self.assertEqual(report["tests"]["a.py"]["rdashd_nodes"][0]["debug_log_tail"], ["rdashd version v0"])
        self.assertEqual(report["tests"]["b.py"]["runtime_s"], 1)
        self.assertEqual(stat.S_IMODE((self.out / "g2-llmq-v24.0.run.json").stat().st_mode), 0o600)
        self.assertNotIn("c.py", report["tests"])  # not a G2 stem

    def test_a_matching_run_is_quiet_and_progress_differs(self):
        spec = {"a.py": [self.refused()], "b.py": [{"result": "Passed", "starts": 1}]}
        self.run_g2(spec, update=True)
        committed = self.committed()["tests"]
        self.assertEqual(self.run_g2({"a.py": [self.refused()], "b.py": [{"result": "Passed", "starts": 1}]}), 0)
        progress = {"result": "Failed", "starts": 1, "output": G2.SETUP_RPC}
        # A change is confirmed by a rerun alone: two outcomes per changed test.
        self.assertEqual(self.run_g2({"a.py": [progress, progress], "b.py": [{"result": "Passed", "starts": 1}]}), 1)
        self.assertIn("progress: a.py", self.stdout.getvalue())
        self.assertEqual(self.committed()["tests"], committed)
        self.assertEqual(self.run_g2({"a.py": [progress, progress], "b.py": [{"result": "Passed", "starts": 1}]},
                                     update=True), 0)
        self.assertEqual(self.committed()["tests"]["a.py"]["stage"], "setup")

    def test_a_failed_confirm_keeps_its_datadir(self):
        self.run_g2({"a.py": [self.refused()], "b.py": [{"result": "Passed", "starts": 1}]}, update=True)
        progress = {"result": "Failed", "starts": 1, "output": G2.SETUP_RPC}
        spec = {"a.py": [dict(progress, debuglog="first\n"), dict(progress, debuglog="confirm\n")],
                "b.py": [{"result": "Passed", "starts": 1}]}
        self.assertEqual(self.run_g2(spec), 1)
        kept = self.out / fc.G2_FAILED_CONFIRM
        self.assertEqual([p.read_text() for p in kept.glob("a_1-*/a_1/node1/regtest/debug.log")], ["confirm\n"])
        self.assertEqual(stat.S_IMODE(kept.stat().st_mode), 0o700)
        self.assertIn("first\n", self.kept_logs(fc.G2_FAILED_MAIN, "a_1"))
        self.assertFalse((self.out / "possible-crashes").exists())
        self.assertIn(f"kept the confirm pass's datadirs of a.py in {kept}\n", self.stdout.getvalue())
        self.assertNotIn("left no datadir", self.stdout.getvalue())

    def kept_logs(self, where, test):
        return sorted(p.read_text() for p in (self.out / where).glob(f"{test}-*/{test}/node1/regtest/debug.log"))

    def test_a_failed_main_pass_keeps_its_datadir_and_a_passed_test_none(self):
        failed = {"result": "Failed", "starts": 1, "output": G2.SETUP_RPC, "debuglog": "main\n"}
        self.assertEqual(self.run_g2({"a.py": [failed], "b.py": [{"result": "Passed", "starts": 1, "debuglog": "b\n"}]},
                                     update=True), 0)
        kept = self.out / fc.G2_FAILED_MAIN
        self.assertEqual(self.kept_logs(fc.G2_FAILED_MAIN, "a_1"), ["main\n"])
        self.assertEqual([p.name.split("-")[0] for p in kept.iterdir()], ["a_1"])
        self.assertEqual(stat.S_IMODE(kept.stat().st_mode), 0o700)
        self.assertFalse((self.out / fc.G2_FAILED_CONFIRM).exists())
        self.assertFalse((self.out / "possible-crashes").exists())
        self.assertIn(f"kept the main pass's datadirs of a.py in {kept}\n", self.stdout.getvalue())
        self.assertNotIn("left no datadir", self.stdout.getvalue())

    def test_a_crashed_main_pass_keeps_its_datadir_with_the_crashes_only(self):
        gone = {"result": "Failed", "starts": 1, "debuglog": "main\n",
                "output": G2.SETUP_RPC + "2026 TestFramework (ERROR): rdashd RPC failure: node 2 exited 1\n"}
        self.run_g2({"a.py": [gone], "b.py": [{"result": "Passed", "starts": 1}]}, update=True)
        self.assertEqual(self.kept_logs("possible-crashes", "a_1"), ["main\n"])
        self.assertFalse((self.out / fc.G2_FAILED_MAIN).exists())
        self.assertIn(f"kept the main pass's datadirs of a.py in {self.out / 'possible-crashes'}\n",
                      self.stdout.getvalue())

    def test_a_crashed_confirm_keeps_its_datadir_with_the_crashes(self):
        self.run_g2({"a.py": [self.refused()], "b.py": [{"result": "Passed", "starts": 1}]}, update=True)
        gone = {"result": "Failed", "starts": 1,
                "output": G2.SETUP_RPC + "2026 TestFramework (ERROR): rdashd RPC failure: node 2 exited 1\n"}
        spec = {"a.py": [dict(gone, debuglog="first\n"), dict(gone, debuglog="confirm\n")],
                "b.py": [{"result": "Passed", "starts": 1}]}
        self.assertEqual(self.run_g2(spec), 1)
        crashes = self.out / "possible-crashes"  # the first pass's too, as before BL-944
        self.assertEqual(sorted(p.read_text() for p in crashes.glob("a_1-*/a_1/node1/regtest/debug.log")),
                         ["confirm\n", "first\n"])
        self.assertFalse((self.out / fc.G2_FAILED_CONFIRM).exists())
        self.assertIn(f"kept the confirm pass's datadirs of a.py in {crashes}\n", self.stdout.getvalue())
        self.assertNotIn("left no datadir", self.stdout.getvalue())

    def test_a_failed_confirm_without_a_datadir_says_so(self):
        self.run_g2({"a.py": [self.refused()], "b.py": [{"result": "Passed", "starts": 1}]}, update=True)
        progress = {"result": "Failed", "starts": 1, "output": G2.SETUP_RPC}
        self.assertEqual(self.run_g2({"a.py": [progress, progress], "b.py": [{"result": "Passed", "starts": 1}]}), 1)
        self.assertFalse((self.out / fc.G2_FAILED_CONFIRM).exists())
        self.assertIn("a.py failed in the main pass and left no datadir", self.stdout.getvalue())
        self.assertIn("a.py failed in the confirm pass and left no datadir", self.stdout.getvalue())

    def test_a_timeout_rerun_keeps_no_datadir(self):
        timeout = {"result": "Failed", "starts": 1, "output": MixedARun.TIMEOUT, "debuglog": "x\n"}
        self.run_g2({"a.py": [timeout, timeout], "b.py": [{"result": "Passed", "starts": 1}]})
        self.assertIn("rerunning 1 timed-out test(s) alone: a.py", self.stdout.getvalue())
        self.assertEqual(self.kept_logs(fc.G2_FAILED_MAIN, "a_1"), ["x\n"])  # the main pass's only
        self.assertFalse((self.out / fc.G2_FAILED_CONFIRM).exists())
        self.assertFalse((self.out / "possible-crashes").exists())

    def test_a_later_note_is_never_compared_and_kept_by_update(self):
        spec = {"a.py": [self.refused()], "b.py": [{"result": "Passed", "starts": 1}]}
        self.run_g2(spec, update=True)
        noted = self.committed()
        noted["tests"]["a.py"][fc.LATER] = "rdashd mines on isolated node 3; MP1a"
        (self.here / "g2-llmq.json").write_text(json.dumps(noted))
        self.assertEqual(self.run_g2(spec), 0)
        self.assertIn("same as g2-llmq.json", self.stdout.getvalue())
        progress = {"result": "Failed", "starts": 1, "output": G2.SETUP_RPC}
        self.assertEqual(self.run_g2({"a.py": [progress, progress], "b.py": [{"result": "Passed", "starts": 1}]},
                                     update=True), 0)
        entry = self.committed()["tests"]["a.py"]
        self.assertEqual((entry["stage"], entry[fc.LATER]), ("setup", "rdashd mines on isolated node 3; MP1a"))
        self.assertNotIn(fc.LATER, self.committed()["tests"]["b.py"])
        passed = {"result": "Passed", "starts": 1}
        self.assertEqual(self.run_g2({"a.py": [passed, passed], "b.py": [passed]}, update=True), 0)
        self.assertEqual(self.committed()["tests"]["a.py"], {"stage": "pass", "status": "pass"})

    def test_a_regression_is_refused_and_never_written(self):
        self.run_g2({"a.py": [self.refused()], "b.py": [{"result": "Passed", "starts": 1}]}, update=True)
        before = self.committed()
        spec = {"a.py": [self.refused()], "b.py": [self.refused(), self.refused()]}
        self.assertEqual(self.run_g2(spec, update=True), 1)
        self.assertIn("REGRESSION: b.py: pass -> fail start", self.stdout.getvalue())
        self.assertEqual(self.committed(), before)

    def test_a_flaky_change_keeps_the_committed_entry(self):
        self.run_g2({"a.py": [self.refused()], "b.py": [{"result": "Passed", "starts": 1}]}, update=True)
        spec = {"a.py": [self.refused()], "b.py": [self.refused(), {"result": "Passed", "starts": 1}]}
        self.assertEqual(self.run_g2(spec), 0)
        self.assertIn("b.py: change flaky", self.stdout.getvalue())

    def test_a_probe_or_another_mask_is_never_compared_or_written(self):
        spec = {"a.py": [{"result": "Failed", "starts": 1, "output": G2.SETUP_RPC}], "b.py": [{"result": "Passed", "starts": 1}]}
        self.assertEqual(self.run_g2(spec, probe="masternodeblsprivkey,-spentindex"), 0)
        self.assertFalse((self.here / "g2-llmq.json").exists())
        report = json.loads((self.out / "g2-llmq-probe-v24.0.run.json").read_text())
        self.assertEqual(report["probe_drop"], ["masternodeblsprivkey", "spentindex"])
        self.assertTrue((self.out / "g2-llmq-probe-v24.0.json").exists())
        for kwargs in ({"probe": "x"}, {"skip": "spork"}, {"mn": 1}, {"tests": ["a.py"]}):
            with self.subTest(**kwargs), self.assertRaises(SystemExit):
                self.run_g2(spec, update=True, **kwargs)
        (self.dir / "spec.json").write_text(json.dumps(spec))
        self.assertEqual(self.run_g2(spec, mn=1), 0)
        self.assertEqual(json.loads((self.out / "g2-llmq-v24.0.json").read_text())["mask"], "mn:1")

    def test_a_skip_is_a_labelled_probe_that_reaches_the_framework(self):
        seen = {}
        real = fc.run_runner

        def runner(tree, env, *args, **kwargs):
            seen.update(env)
            return real(tree, env, *args, **kwargs)
        spec = {"a.py": [{"result": "Failed", "starts": 1, "output": G2.UNSERVED_DKGSTATUS}],
                "b.py": [{"result": "Passed", "starts": 1}]}
        with unittest.mock.patch.object(fc, "run_runner", runner):
            self.assertEqual(self.run_g2(spec, skip=" spork,quorum.dkgstatus,spork"), 0)
        self.assertEqual((seen["RDASHD_PROBE_SKIP"], seen["RDASHD_PROBE_DROP"]), ("quorum.dkgstatus,spork", ""))
        self.assertFalse((self.here / "g2-llmq.json").exists())
        self.assertIn("probe run, not compared", self.stdout.getvalue())
        report = json.loads((self.out / "g2-llmq-probe-v24.0.run.json").read_text())
        self.assertEqual((report["kind"], report["probe_skip"], report["probe_drop"]),
                         ("g2-llmq-probe", ["quorum.dkgstatus", "spork"], []))
        self.assertEqual(report["tests"]["a.py"]["probe_skips"], ["spork (node 3)"])
        self.assertEqual(report["tests"]["a.py"]["entry"]["detail"], "quorum dkgstatus")
        md = (self.out / "g2-llmq-probe-v24.0.md").read_text()
        self.assertIn("probe drop: none; probe skip: quorum.dkgstatus, spork.", md)
        self.assertIn("(skipped: spork (node 3))", md)
        self.assertIn("-spentindex dropped (MASTERNODE_DROPS) (IX2)", md)
        self.assertEqual(report["adaptations"], fc.G2_ADAPTATIONS)
        for bad in ("quorum dkgstatus", "spork=1"):
            with self.subTest(bad=bad), self.assertRaises(SystemExit):
                self.run_g2(spec, skip=bad)
        self.assertEqual(self.run_g2(spec, skip="quorum.dkginfo"), 2)  # no such skip in the tree

    def test_update_refuses_a_failure_no_traceback_places(self):
        spec = {"a.py": [{"result": "Failed", "starts": 1, "output": "died"}], "b.py": [{"result": "Passed", "starts": 1}]}
        self.assertEqual(self.run_g2(spec, update=True), 1)
        self.assertFalse((self.here / "g2-llmq.json").exists())
        self.assertIn("(stage unknown)", self.stdout.getvalue())
        self.assertEqual(json.loads((self.out / "g2-llmq-v24.0.json").read_text())["tests"]["a.py"]["stage"], fc.UNKNOWN)

    def test_an_rdashd_init_refusal_is_named_from_the_marked_node(self):
        # The fake runner marks node 1 as the rdashd node of a failed test's datadir.
        out = G2.START.format(err="Error: Masternode must relay transactions").replace("[node 3]", "[node {i}]")
        for node, cls in ((1, fc.INIT_REFUSAL), (2, "harness")):
            spec = {"a.py": [{"result": "Failed", "starts": 1, "output": out.format(i=node), "debuglog": "x\n"}],
                    "b.py": [{"result": "Passed", "starts": 1}]}
            self.run_g2(spec)
            self.assertEqual(json.loads((self.out / "g2-llmq-v24.0.json").read_text())["tests"]["a.py"]["class"], cls)

    def test_a_test_without_an_rdashd_start_is_a_broken_run(self):
        spec = {"a.py": [{"result": "Passed", "starts": 0}], "b.py": [{"result": "Passed", "starts": 1}]}
        self.assertEqual(self.run_g2(spec, update=True), 2)
        self.assertFalse((self.here / "g2-llmq.json").exists())
        self.assertFalse((self.out / "g2-llmq-v24.0.json").exists())

    def test_a_tree_without_g2s_hooks_is_a_broken_run(self):
        (self.tree / "src/test/functional/test_framework/test_node.py").write_text("")
        self.assertEqual(self.run_g2({}), 2)

    def test_the_probe_reaches_the_framework_through_the_environment(self):
        seen = {}

        def runner(tree, env, *args, **kwargs):
            seen.update(env)
            raise SystemExit("stop")
        with unittest.mock.patch.object(fc, "run_runner", runner), self.assertRaises(SystemExit):
            self.run_g2({}, probe="masternodeblsprivkey", mn=3)
        self.assertEqual((seen["RDASHD_NODES"], seen["RDASHD_PROBE_DROP"], seen["RDASHD_TEST_TIMEOUT"]),
                         ("mn:3", "masternodeblsprivkey", "60"))
        # Its own ports: a concurrent mixed-a run cannot take them.
        self.assertEqual(seen["TEST_RUNNER_PORT_MIN"], str(fc.G2_PORT_MIN))
        self.assertNotEqual(fc.G2_PORT_MIN, fc.MIXED_A_PORT_MIN)


class MixedBRun(unittest.TestCase):
    scripts = ["feature_llmq_signing.py", "feature_llmq_signing.py --spork21", "a.py", "b.py"]
    artifacts = ("criteria.json", "nodes.json", "obs.jsonl", "watch.log", "PAGE-1.json")

    def setUp(self):
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR"))))
        self.tree, self.out = self.root / "tree", self.root / "out"
        for path, tokens in fc.MIXED_B_HOOKS.items():
            file = self.tree / "src" / path
            file.parent.mkdir(parents=True, exist_ok=True)
            file.write_text("\n".join(tokens))
        self.build = self.enterContext(unittest.mock.patch.object(fc, "build", return_value=self.tree))
        self.enterContext(unittest.mock.patch.object(fc, "RDC", self.root))
        self.roles = ["dashd"]
        self.starts = []
        def build_or_plan(cmd, **kwargs):
            if cmd[0] == "nice":
                if "rdashd" in cmd:
                    self.assertEqual(cmd, ["nice", "-n", "10", "cargo", "build", "--locked", "-p", "rdashd",
                                           "--bin", "rdashd", "--features", "test-hooks"])
                    (self.root / "target/debug/rdashd").write_bytes(b"test-hooks")
                    return
                self.assertEqual(cmd, ["nice", "-n", "10", "cargo", "build", "--locked", "-p",
                                       "dash-testkit", "--bins", "-p", "dash-conformance"])
                self.assertEqual(kwargs["cwd"], self.root)
                bins = self.root / "target/debug"
                bins.mkdir(parents=True, exist_ok=True)
                for name in ("mixnet-plan", "mixnet-watch", "mixnet-judge", "dash-conformance"):
                    (bins / name).write_bytes(name.encode())
            else:
                caps = cmd[cmd.index("--caps") + 1].split(",")
                json.dump({"caps": caps, "slots": [{"runs": role, "twin": role == "rust" and "mn" in caps
                                                    and "dkg" not in caps} for role in self.roles]}, kwargs["stdout"])
        self.plan = self.enterContext(unittest.mock.patch.object(fc.subprocess, "run", side_effect=build_or_plan))
        self.enterContext(unittest.mock.patch.object(fc, "scripts_of", return_value=self.scripts))
        self.enterContext(unittest.mock.patch.object(fc, "G2_STEMS", ("a",)))
        self.enterContext(unittest.mock.patch.object(fc, "pylib", return_value=self.root))
        self.runner = self.enterContext(unittest.mock.patch.object(fc, "run_group", side_effect=self.run_group))
        self.lock = self.root / "ports.lock"
        self.enterContext(unittest.mock.patch.dict(os.environ, {"G2_PORT_LOCK": str(self.lock),
                                                               "RDASHD_MN_CAPS": "inherited",
                                                               "MIXNET_GATE": "inherited"}))
        self.stdout = self.enterContext(contextlib.redirect_stdout(io.StringIO()))
        self.stderr = self.enterContext(contextlib.redirect_stderr(io.StringIO()))
        self.results = {self.scripts[0]: "Passed"}
        self.criteria = True
        self.report = {"gate": "rehearsal", "passed": True}
        self.rc = 0

    def run_group(self, cmd, log, grace, *, env):
        self.assertFalse(lock_free(self.lock))
        self.tmpdir = Path(cmd[3].removeprefix("--tmpdirprefix="))
        self.assertEqual(self.tmpdir.parent, self.root / "tmp")
        (self.tmpdir / "datadir").mkdir()
        (self.tmpdir / "datadir/seam").mkdir()
        (self.tmpdir / "datadir/seam/reg0-1.json").write_text("{}")
        Path(env["RDASHD_STARTS"]).write_text("".join(name + "\n" for name in self.starts))
        for name, status in self.results.items():
            print(f"{name} | ✓ {status} | 1 s", file=log)
            directory = Path(env["MIXNET_OUT"]) / name
            directory.mkdir(parents=True, exist_ok=True)
            for artifact in self.artifacts:
                if artifact != "criteria.json":
                    (directory / artifact).write_text(name)
                elif self.criteria:
                    (directory / artifact).write_text(json.dumps({"report": self.report}))
        return self.rc

    def mixed_b(self, preset, *args):
        return fc.main(["mixed-b", "--pin", "v24.0", "--preset", preset, "--out", str(self.out),
                        "--tmpdirprefix", str(self.root / "tmp"), *args])

    def test_arguments_environment_and_copy_out(self):
        bins = fc.RDC / "target/debug"
        name = self.scripts[0]
        for preset, mask, digest in (("alt5", "alt", False), ("alt8", "alt", False), ("modeb5", "mn", True)):
            with self.subTest(preset=preset):
                args = ["--caps", "mn,dkg", "--digest-dashd", str(self.root / "digest-dashd")] if digest else []
                self.assertEqual(self.mixed_b(preset, "--tests", name, *args), 0)
                self.assertEqual(self.build.call_args.args[2:],
                                 ("v24.0", True, "node/o12b-mixed-b4-v2400", fc.G2_RELEASE))
                self.assertEqual(self.plan.call_args.args[0], [bins / "mixnet-plan", "--preset", preset,
                                 "--caps", "mn,dkg" if digest else "none", "--context", "regtest"])
                self.assertEqual(self.plan.call_args.kwargs["stdout"].name, str(self.out / "plan.json"))
                cmd, _, grace = self.runner.call_args.args
                self.assertEqual(cmd[2:], ["--jobs=1", f"--tmpdirprefix={self.tmpdir}",
                                          f"--filter=^(?:{re.escape(name)})$", name])
                self.assertEqual(grace, fc.RUNNER_TERM_GRACE)
                env = self.runner.call_args.kwargs["env"]
                self.assertEqual(env["RDASHD_NODES"], mask)
                self.assertEqual(env["RDASHD_ROLEPLAN"], str(self.out / "plan.json"))
                self.assertNotIn("RDASHD_MN_CAPS", env)
                self.assertNotIn("MIXNET_GATE", env)
                self.assertEqual(env["MIXNET_WATCH"], str(bins / "mixnet-watch"))
                self.assertEqual(env["MIXNET_JUDGE"], str(bins / "mixnet-judge"))
                self.assertEqual(env["MIXNET_DIGEST"], "1" if digest else "0")
                self.assertEqual(env["DASHD"], str(self.root / "digest-dashd" if digest else self.tree / "build/src/dashd"))
                self.assertEqual(env["TEST_RUNNER_PORT_MIN"], str(fc.G2_PORT_MIN))
                self.assertEqual({p.name for p in (self.out / name).iterdir()}, set(self.artifacts))
                self.assertEqual((self.out / name / "obs.jsonl").read_text(), name)
                self.assertIn(f"{name}: Passed; rehearsal passed=True", self.stdout.getvalue())

                self.assertFalse(self.tmpdir.exists())
                hashes = (self.out / "binaries.sha256").read_text().splitlines()
                self.assertEqual(hashes, [f"{hashlib.sha256(n.encode()).hexdigest()}  {n}" for n in
                                          ("mixnet-plan", "mixnet-watch", "mixnet-judge", "dash-conformance")])

    def test_twin_dry_run(self):
        self.roles.append("rust")
        self.starts = [self.scripts[0]]
        bindir = self.tree / "build/src"
        bindir.mkdir(parents=True)
        for name in ("dashd", "dash-cli"):
            target = self.root / name
            target.touch()
            (bindir / name).symlink_to(target)
        self.assertEqual(self.mixed_b("modeb5", "--caps", "mn,qdata", "--tests", self.scripts[0], "--twin-dry-run"), 0)
        env = self.runner.call_args.kwargs["env"]
        self.assertEqual(env["DASHD"], str(bindir / "dashd"))
        self.assertEqual(env["RDASHD"], str(self.root / "dashd"))
        self.assertEqual(env["RDASH_CLI"], str(self.root / "dash-cli"))
        self.assertEqual(env["RDASHD_NODES"], "mn")
        self.assertEqual(env["MIXNET_GATE"], "rehearsal")
        self.assertEqual(env["MIXNET_TWIN_DRY_RUN"], "1")
        self.assertFalse((self.out / "seam-summary.txt").exists())
        self.assertIn(f"{self.scripts[0]}: Passed; rehearsal passed=True; rdashd starts: 1 (Core binaries, --twin-dry-run)",
                      self.stdout.getvalue().splitlines())
        with self.assertRaises(SystemExit):
            self.mixed_b("modeb5", "--twin-dry-run", "--digest-dashd", "digest")
        self.runner.assert_called_once()

    def test_twin_dry_run_refuses_the_seam(self):
        self.roles.append("rust")
        with self.assertRaisesRegex(SystemExit, "the seam load needs rdashd; a twin dry run needs qdata"):
            self.mixed_b("modeb5", "--caps", "mn", "--tests", self.scripts[0], "--twin-dry-run")
        self.assertFalse(any("--features" in call.args[0] for call in self.plan.call_args_list))
        self.runner.assert_not_called()

    def test_seam_build_selection(self):
        self.roles.append("rust")
        self.starts = [self.scripts[0]]
        for caps, features in (("mn", True), ("mn,qdata", False), ("mn,dkg", False)):
            with self.subTest(caps=caps), unittest.mock.patch.object(fc.subprocess, "check_output",
                                                                     return_value="-testquorumshares=<file>") as help:
                self.plan.reset_mock()
                self.assertEqual(self.mixed_b("modeb5", "--caps", caps, "--tests", self.scripts[0]), 0)
                self.assertEqual(help.called, features)
                self.assertEqual(any("--features" in call.args[0] for call in self.plan.call_args_list), features)
                self.assertEqual((self.out / "seam-summary.txt").exists(), features)
                if features:
                    self.assertIn("test-hooks", (self.out / "seam-summary.txt").read_text())
                    self.assertIn("  rdashd", (self.out / "binaries.sha256").read_text())

    def test_rust_plan_requires_starts_per_test(self):
        names = self.scripts[:2]
        self.results = dict.fromkeys(names, "Passed")
        self.roles.append("rust")
        self.starts = list(names)
        self.assertEqual(self.mixed_b("alt5", "--tests", ",".join(names)), 0)
        self.starts.pop()
        self.assertEqual(self.mixed_b("alt5", "--tests", ",".join(names)), 1)
        self.assertIn(f"no rdashd start in {names[1]}", self.stderr.getvalue())

    def test_interruption_keeps_finished_artifacts(self):
        def interrupt(*args, **kwargs):
            self.run_group(*args, **kwargs)
            raise fc.Terminated()
        self.runner.side_effect = interrupt
        for keep in (False, True):
            with self.subTest(keep=keep):
                self.assertEqual(self.mixed_b("alt5", *(["--keep-failed"] if keep else [])), fc.TERMINATED)
                self.assertTrue((self.out / self.scripts[0] / "criteria.json").is_file())
                self.assertEqual((self.tmpdir / "datadir").exists(), keep)
                self.assertFalse((self.tmpdir / "datadir/seam/reg0-1.json").exists())

    def test_default_and_comma_separated_selections(self):
        for names, args in (([self.scripts[0], "a.py"], []),
                            (self.scripts[1:3], ["--tests", ",".join(self.scripts[1:3])])):
            self.results = dict.fromkeys(names, "Passed")
            self.assertEqual(self.mixed_b("alt5", *args), 0)
            cmd = self.runner.call_args.args[0]
            selected = re.compile(cmd[4].removeprefix("--filter="))
            self.assertEqual([s for s in self.scripts if selected.search(s)], names)
            self.assertEqual(cmd[5:], fc.selection(names))
            for name in names:
                self.assertEqual((self.out / name / "obs.jsonl").read_text(), name)

    def test_failed_gate_exit_code(self):
        for gate, expected in (("rehearsal", 1), ("stage-a", 0)):
            with self.subTest(gate=gate):
                self.report = {"gate": gate, "passed": False}
                self.assertEqual(self.mixed_b("alt5", "--tests", self.scripts[0]), expected)
                self.assertIn(f"Passed; {gate} passed=False", self.stdout.getvalue())

    def test_criteria_driver_hooks(self):
        path = "test/functional/rust_mixed_b_criteria.py"
        self.assertEqual(set(fc.MIXED_B_HOOKS[path]), {"ProbeRecovery", "ProbeSkip", "DkgCase", "Proposal"})
        self.assertIn("def mark(", fc.MIXED_B_HOOKS["test/functional/test_framework/mixnet.py"])

    def test_refuses_missing_mixed_b_hooks(self):
        (self.tree / "src/test/functional/test_framework/mixnet.py").unlink()
        self.assertEqual(self.mixed_b("modeb5"), 1)
        self.assertIn("mixnet.py: MIXNET_WATCH", self.stderr.getvalue())
        self.plan.assert_not_called()
        self.runner.assert_not_called()

    def test_exit_requires_a_pass_and_fresh_criteria_for_each_test(self):
        names = self.scripts[:2]
        for status, criteria, rc, expected in (("Passed", True, 0, 0), ("Passed", False, 0, 1),
                                               ("Failed", True, 0, 1), ("Skipped", True, 0, 1),
                                               (None, True, 0, 1), ("Passed", True, 1, 1)):
            with self.subTest(status=status, criteria=criteria, rc=rc):
                self.results = {names[0]: "Passed"}
                if status:
                    self.results[names[1]] = status
                self.criteria, self.rc = criteria, rc
                self.assertEqual(self.mixed_b("alt5", "--tests", ",".join(names)), expected)
                self.assertEqual((self.out / names[1] / "criteria.json").exists(), bool(status and criteria))


CORE = Path(os.environ.get("MIXED_A_CORE", Path.home() / "workspace/dash"))


RELEASE = Path(os.environ.get("RUST_DASHCORE_TEST_CACHE", Path.home() / ".rust-dashcore-test"))


@unittest.skipUnless(subprocess.run(["git", "-C", str(CORE), "rev-parse", "--verify", "--quiet",
                                     fc.MIXED_A_CORE_BRANCH], capture_output=True).returncode == 0
                     and all((RELEASE / f"dashcore-{r}/bin/dashd").exists()
                             for r in (fc.PINS["v24.0"].release, fc.MIXED_A_RELEASE)),
                     f"no Dash Core repository with {fc.MIXED_A_CORE_BRANCH}, or no cached dashd (no download here)")
class MixedAExport(unittest.TestCase):
    def test_the_mode_a_branch_exports_with_its_hooks(self):
        # The real export mixed-a runs by default (Sol r2 #1): build() of MIXED_A_CORE_BRANCH.
        with tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR")) as d:
            tree = fc.build(CORE, d, "v24.0", True, fc.MIXED_A_CORE_BRANCH, fc.MIXED_A_RELEASE)
            self.assertEqual(json.loads((tree / "tree.json").read_text())["branch"], fc.MIXED_A_CORE_BRANCH)
            # Its Core nodes run the release its tree starts from, as the pin's do.
            self.assertEqual(fc.tag_of(CORE, fc.MIXED_A_CORE_BRANCH), f"v{fc.MIXED_A_RELEASE}")
            self.assertEqual(fc.tag_of(CORE, fc.PINS["v24.0"].branch), fc.PINS["v24.0"].tag)
            self.assertEqual((tree / "build/src/dashd").resolve(),
                             (RELEASE / f"dashcore-{fc.MIXED_A_RELEASE}/bin/dashd").resolve())
            self.assertEqual(fc.missing_hooks(tree), [])
            self.assertNotEqual(fc.missing_hooks(fc.build(CORE, d, "v24.0", True, "node/func-compat-v24.0")), [])


@unittest.skipUnless(subprocess.run(["git", "-C", str(CORE), "rev-parse", "--verify", "--quiet", fc.G2_CORE_BRANCH],
                                    capture_output=True).returncode == 0
                     and (RELEASE / f"dashcore-{fc.G2_RELEASE}/bin/dashd").exists(),
                     f"no Dash Core repository with {fc.G2_CORE_BRANCH}, or no cached dashd (no download here)")
class G2Export(unittest.TestCase):
    def test_the_g2_branch_exports_with_its_hooks_and_tests(self):
        with tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR")) as d:
            tree = fc.build(CORE, d, "v24.0", True, fc.G2_CORE_BRANCH, fc.G2_RELEASE)
            self.assertEqual(fc.missing_hooks(tree, fc.G2_HOOKS), [])
            self.assertEqual(fc.tag_of(CORE, fc.G2_CORE_BRANCH), f"v{fc.G2_RELEASE}")
            stems = {fc.stem(s) for s in fc.dtf_scripts(tree, fc.scripts_of(tree))}
            self.assertLessEqual(set(fc.G2_STEMS), stems)
            self.assertNotIn("feature_llmq_is", stems)  # v24.0 has none: is_retroactive and is_cl_conflicts stand in


class MixedANightly(unittest.TestCase):
    """mixed-a-nightly.sh's exit status over a fake func-compat.py: FAKE_MIXED_A maps a variant to
    its exit status, and to ":result" when it writes its result first."""

    FAKE_FUNC_COMPAT = textwrap.dedent("""\
        import os, sys
        from pathlib import Path
        args = sys.argv[1:]
        variant = "mixed-a-obs" if "--observer" in args else "mixed-a"
        out = Path(args[args.index("--out") + 1])
        plan = dict(v.split("=") for v in os.environ["FAKE_MIXED_A"].split(","))[variant]
        status, _, result = plan.partition(":")
        (out / "args").write_text("\\n".join(args))
        if result:
            (out / f"{variant}-v24.0.json").write_text("{}\\n")
        sys.exit(int(status))
        """)

    def nightly(self, plan, **env):
        """The nightly's exit status, and the mixed-a variant's func-compat.py arguments."""
        with tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR")) as d:
            here = Path(d) / "contrib/node/func-compat"
            here.mkdir(parents=True)
            (here / "mixed-a-nightly.sh").write_text((HERE / "mixed-a-nightly.sh").read_text())
            (here / "func-compat.py").write_text(self.FAKE_FUNC_COMPAT)
            bin_dir = Path(d) / "bin"  # a git that knows HEAD, so no repository is needed
            bin_dir.mkdir()
            (bin_dir / "git").write_text("#!/bin/sh\necho c0ffee\n")
            (bin_dir / "git").chmod(0o755)
            inherited = {k: v for k, v in os.environ.items() if k != "MIXED_A_CORE_BRANCH"}
            env = {**inherited, "PATH": f"{bin_dir}:{os.environ['PATH']}", "MIXED_A_BIN_DIR": str(bin_dir),
                   "FAKE_MIXED_A": plan, **env}
            status = subprocess.run(["bash", str(here / "mixed-a-nightly.sh"), str(Path(d) / "out")], env=env,
                                    capture_output=True).returncode
            args = (Path(d) / "out/mixed-a/args").read_text().split("\n")
            return status, args

    def test_exit_status(self):
        for plan, status in (("mixed-a=0:result,mixed-a-obs=0:result", 0),
                             ("mixed-a=0:result,mixed-a-obs=1:result", 1),
                             ("mixed-a=1:result,mixed-a-obs=1:result", 1),
                             ("mixed-a=1,mixed-a-obs=0:result", 2),  # a harness error: exit 1, no result
                             ("mixed-a=0:result,mixed-a-obs=0", 2),  # exit 0 but no result
                             ("mixed-a=2:result,mixed-a-obs=0:result", 2),
                             ("mixed-a=1:result,mixed-a-obs=2", 2)):
            with self.subTest(plan=plan):
                self.assertEqual(self.nightly(plan)[0], status)

    def test_the_branch_comes_from_func_compat_unless_overridden(self):
        plan = "mixed-a=0:result,mixed-a-obs=0:result"
        self.assertNotIn("--branch", self.nightly(plan)[1])  # func-compat.py's MIXED_A_CORE_BRANCH
        args = self.nightly(plan, MIXED_A_CORE_BRANCH="node/x")[1]
        self.assertEqual(args[args.index("--branch") + 1], "node/x")


class FtMap(unittest.TestCase):
    # Enough tests that one test's token is under NOISE.
    TESTS = ["feature_logging", "interface_rpc", "rpc_help", "rpc_users"] + [f"tool_{n}" for n in range(16)]

    def test_stories_reads_section_two_only(self):
        prd = ftmap.stories(PRD)
        self.assertEqual(list(prd), ["US-NODE-12", "US-INT-14", "US-DEV-07"])
        self.assertEqual(prd["US-INT-14"][1], "`getrpcinfo`")

    def test_core_tokens(self):
        self.assertEqual(ftmap.core_tokens("`-debug`, `getblockchaininfo.blocks` `foo`", {"getblockchaininfo"}),
                         {"-debug", "getblockchaininfo"})

    def test_test_tokens_skip_loggers_and_unknown_calls(self):
        source = 'self.log.info("x"); node.getrpcinfo(); node.start(); self.extra_args = [["-debug=1"]]'
        self.assertEqual(ftmap.test_tokens(source, {"getrpcinfo", "info"}), {"getrpcinfo", "-debug"})

    def mapped(self, rules):
        sources = {t: "" for t in self.TESTS}
        sources["interface_rpc"] = "node.getrpcinfo()"
        return ftmap.mapping(self.TESTS, sources, ftmap.stories(PRD), rules, {"getrpcinfo"})

    def test_mapping_marks_each_source(self):
        m = self.mapped({"*": ["US-DEV-07"], "feature_logging": ["US-NODE-12"]})
        self.assertEqual(m["feature_logging"]["US-NODE-12"], {"a", "r"})
        self.assertEqual(m["rpc_help"]["US-INT-14"], {"a"})
        self.assertEqual(m["interface_rpc"]["US-INT-14"], {"t"})
        self.assertEqual(m["rpc_users"]["US-DEV-07"], {"r"})

    def test_noise_tokens_are_dropped(self):
        sources = {t: "node.getrpcinfo()" for t in self.TESTS}
        m = ftmap.mapping(self.TESTS, sources, ftmap.stories(PRD), {}, {"getrpcinfo"})
        self.assertNotIn("t", m["interface_rpc"].get("US-INT-14", set()))

    def test_bad_rules_fail(self):
        with self.assertRaisesRegex(ValueError, "no such story"):
            self.mapped({"rpc_help": ["US-NOPE-1"]})
        with self.assertRaisesRegex(ValueError, "matches no test"):
            self.mapped({"p2p_*": ["US-DEV-07"]})

    def test_scripts_in_reads_both_lists(self):
        runner = 'BASE_SCRIPTS = ["a.py", "b.py --x"]\nEXTENDED_SCRIPTS = ["c.py"]\n'
        self.assertEqual(ftmap.scripts_in(runner), ["c.py", "a.py", "b.py --x"])
        with self.assertRaises(ValueError):
            ftmap.scripts_in('BASE_SCRIPTS = []\n')

    def test_rules_file_names_only_prd_stories(self):
        import tomllib
        prd = Path.home() / "workspace/node-program/PRD.md"
        if not prd.exists():
            self.skipTest("no node-program checkout")
        stories = ftmap.stories(prd.read_text(encoding="utf8"))
        with open(HERE / "ft-map-rules.toml", "rb") as f:
            rules = tomllib.load(f)
        self.assertEqual([i for ids in rules.values() for i in ids if i not in stories], [])


if __name__ == "__main__":
    unittest.main()
