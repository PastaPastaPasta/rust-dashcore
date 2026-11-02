#!/usr/bin/env python3
"""Unit tests for func-compat.py (the ratchet, parsers, tripwire) and ft-map.py (the mapping).

Run with `func-compat.py selftest` or `python3 -m unittest test_func_compat` from this directory.
"""

import argparse
import contextlib
import importlib.util
import io
import json
import os
import re
import shutil
import stat
import subprocess
import tempfile
import textwrap
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


def matrix(status, targets=None, complete=True, starts=None):
    """A run's matrix; every test started rdashd once unless `starts` says otherwise."""
    return {"status": status, "targets": list(status) if targets is None else targets,
            "complete": complete, "rdashd_starts": {n: 1 for n in status} | (starts or {})}


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


# rdashd: refuses `-opt=value` unless FAKE_ACCEPT names `opt=value`; with no option it runs
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
          echo "Error: ${a%%=*} is not supported by rdashd yet (XX)." >&2; exit 1 ;;
  esac
done
# As when another node holds the regtest p2p port: the tripwire's nodes must not listen.
[ -z "$nolisten" ] && { echo "Error: Unable to bind to 0.0.0.0:19899 on this computer." >&2; touch "$d/died"; exit 1; }
[ -n "$FAKE_DIE" ] && { touch "$d/died"; exit 3; }
echo $$ > "$d/pid"
exec sleep 60
"""

FAKE_CLI = """#!/bin/sh
for a in "$@"; do case "$a" in -datadir=*) d="${a#-datadir=}" ;; esac; m="$a"; done
# Wait for the node as -rpcwait does: its pid, or (FAKE_DIE) its exit.
until [ -s "$d/pid" ] || [ -e "$d/died" ]; do sleep 0.05; done
[ -e "$d/died" ] && sleep 0.5
[ "$m" = stop ] && { kill "$(cat "$d/pid")"; exit 0; }
case " $FAKE_SERVE " in *" $m "*) echo '{}'; exit 0 ;; esac
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

    def wire(self, accept="", serve="", die="", name="rdashd", messages=MESSAGES):
        os.environ.update(FAKE_ACCEPT=accept, FAKE_SERVE=serve, FAKE_DIE=die, FAKE_NAME=name)
        return fc.tripwire(self.rdashd, self.cli, {"port": ("P2b", "11000"), "createwalletbackups": ("WL2a", "0")},
                           {"getmempoolinfo": "MP1a"}, self.dir.name, messages)

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

    def test_a_node_that_dies_is_not_a_served_method(self):
        with self.assertRaisesRegex(SystemExit, "exited"):
            self.wire(die="1")

    def test_flags_a_served_method(self):
        stale = self.wire(serve="getmempoolinfo")
        self.assertEqual(len(stale), 1)
        self.assertIn("getmempoolinfo", stale[0])


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
        self.assertEqual(fc.tree_dir(w, "v24.0", True, fc.MIXED_A_CORE_BRANCH), w / "v24.0-wallet-node_o12a-mixed-a")

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

    def test_the_d18_pattern_follows_rdashds_refusal_text(self):
        self.assertTrue(fc.D18_REFUSAL.search(self.refusal()))
        self.assertTrue(fc.D18_REFUSAL.search(self.refusal("quorum checks (CH7b)")))
        # G2's deferral form ends past the row: still the same refusal.
        self.assertTrue(fc.D18_REFUSAL.search(self.refusal().rstrip() + "; it connects once the tip is older than -maxtipage"))
        self.assertFalse(fc.D18_REFUSAL.search("rdashd cannot validate block x at height 2 on regtest: y (ALLOWED-DIFFS D17)"))

    def test_a_d18_refusal_in_debug_log_is_the_allowed_difference(self):
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
            self.assertEqual(fc.logged_d18(dirs), {"a.py"})
        # It wins over whatever else the run failed with, an exit 1 at stop included.
        results = {n: "Failed" for n in ("a.py", "b.py")} | {"c.py": "Passed", "e.py": "Failed"}
        sections = {"a.py": self.tb("test_framework.authproxy.JSONRPCException: Method not found (-32601)")
                    + "AssertionError: [node 1] Node returned non-zero exit code (1) when stopping\n",
                    "b.py": self.tb("BrokenPipeError: [Errno 32] Broken pipe"), "e.py": "x"}
        starts = {"a.py": 1, "b.py": 1, "c.py": 1, "e.py": 0}
        entries = fc.mixed_a_entries(results, sections, starts, True, {"a.py", "b.py", "c.py", "e.py"})
        self.assertEqual(entries, {"a.py": fc.ALLOWED_D18, "b.py": fc.ALLOWED_D18, "c.py": {"status": "pass"},
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
                            "check_adaptations": lambda tree, d, c, s: (d, c, []), "HERE": self.passlists}.items():
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

    def test_mixed_a_exports_the_mode_a_branch_by_default(self):
        with unittest.mock.patch.object(fc, "mixed_a", lambda args: args.branch):
            self.assertEqual(fc.main(["mixed-a", "--pin", "v24.0"]), fc.MIXED_A_CORE_BRANCH)
            self.assertEqual(fc.main(["mixed-a", "--pin", "v24.0", "--branch", "x"]), "x")

    def test_d18_runs_are_allowed_and_a_stop_exit_is_kept(self):
        stop = "AssertionError: [node 1] Node returned non-zero exit code (1) when stopping\n"
        broken = self.BROKEN
        spec = {"a.py": [{"result": "Failed", "starts": 1, "output": broken + stop, "debuglog": MixedA.refusal()}],
                "b.py": [{"result": "Failed", "starts": 1, "output": self.DIVERGENCE.replace("a.py", "b.py") + stop,
                          "debuglog": "no refusal\n"}]}
        self.run_mixed_a(spec)
        self.assertEqual(self.result()["a.py"], fc.ALLOWED_D18)
        self.assertEqual(self.result()["b.py"], {"status": "fail", "class": "divergence", "detail": "b.py:42"})
        # The D18 abort is neither flagged nor kept; the unexplained exit at stop is kept.
        self.assertEqual(self.run_meta()["possible_crashes"], ["b.py"])
        self.assertEqual([p.name.rsplit("-", 1)[0] for p in (self.out / "possible-crashes").iterdir()], ["b_2"])

    def test_a_crash_beyond_the_d18_abort_is_still_flagged_and_kept(self):
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
                self.assertEqual(self.result()["a.py"], fc.ALLOWED_D18)
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


CORE = Path(os.environ.get("MIXED_A_CORE", Path.home() / "workspace/dash"))


RELEASE = Path(os.environ.get("RUST_DASHCORE_TEST_CACHE", Path.home() / ".rust-dashcore-test"))


@unittest.skipUnless(subprocess.run(["git", "-C", str(CORE), "rev-parse", "--verify", "--quiet",
                                     fc.MIXED_A_CORE_BRANCH], capture_output=True).returncode == 0
                     and (RELEASE / f"dashcore-{fc.PINS['v24.0'].release}/bin/dashd").exists(),
                     f"no Dash Core repository with {fc.MIXED_A_CORE_BRANCH}, or no cached dashd (no download here)")
class MixedAExport(unittest.TestCase):
    def test_the_mode_a_branch_exports_with_its_hooks(self):
        # The real export mixed-a runs by default (Sol r2 #1): build() of MIXED_A_CORE_BRANCH.
        with tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR")) as d:
            tree = fc.build(CORE, d, "v24.0", True, fc.MIXED_A_CORE_BRANCH)
            self.assertEqual(json.loads((tree / "tree.json").read_text())["branch"], fc.MIXED_A_CORE_BRANCH)
            self.assertEqual(fc.missing_hooks(tree), [])
            self.assertNotEqual(fc.missing_hooks(fc.build(CORE, d, "v24.0", True, "node/func-compat-v24.0")), [])


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
