"""node-bench's PF0 mode (nodebench/pf0.py; PF0-N plan rev4.1 §1, D31, D46): the daemon is the broker stub,
every per-run argv field is pinned, the broker makes the datadirs, RPC responses are capped.

    python3 -m unittest discover -s node-bench/tests
"""

import hashlib
import http.server
import os
import re
import subprocess
import sys
import threading
import types
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from nodebench import impls, pf0, rpc, workloads  # noqa: E402


class Pf0Mode(unittest.TestCase):
    def setUp(self):
        patcher = mock.patch.object(pf0, "_on", True)
        patcher.start()
        self.addCleanup(patcher.stop)

    def config(self, **kw):
        return impls.NodeConfig(datadir=pf0.NODE_DATADIR, rpcport=pf0.NODE_RPC, p2pport=pf0.NODE_P2P, connect=["0"],
                                **kw)

    def test_free_port_refused(self):
        with self.assertRaisesRegex(pf0.Pf0Error, "pins the daemon's ports"):
            impls.free_port()
        with self.assertRaises(pf0.Pf0Error):
            impls.NodeConfig(datadir=pf0.NODE_DATADIR)  # the defaults would pick free ports

    def test_argv_is_the_stub(self):
        node = impls.Node(impls.RustImpl(pf0.STUB), self.config(), spawn_class="run")
        argv = node.argv()
        self.assertEqual(argv[:8], [pf0.STUB, "--client", "--class", "run", "--measure", "counter", "--", "rdashd"])
        self.assertIn(f"-datadir={pf0.NODE_DATADIR}", argv)
        self.assertIn(f"-rpcport={pf0.NODE_RPC}", argv)
        self.assertEqual(node.stdout_path, os.path.join(pf0.RUN, "stdout-run.log"), "outside the datadir (D46)")
        start = impls.Node(impls.RustImpl(pf0.STUB), self.config(), spawn_class="start").argv()
        self.assertEqual(start[3:6], ["start", "--measure", "none"])

    def test_no_wrap_or_slice(self):
        for kw in ({"wrap": ["perf", "stat", "--"]}, {"cpu_slice": "numa:0"}):
            with self.subTest(**kw), self.assertRaisesRegex(pf0.Pf0Error, "the broker counts and places"):
                impls.Node(impls.RustImpl(pf0.STUB), self.config(), **kw)

    def test_start_never_makes_the_datadir(self):
        node = impls.Node(impls.RustImpl(pf0.STUB), self.config(), stdout_path=os.devnull)
        with mock.patch.object(impls.os, "makedirs") as makedirs, \
                mock.patch.object(impls, "spawn", side_effect=RuntimeError("stop here")) as spawn:
            with self.assertRaisesRegex(RuntimeError, "stop here"):
                node.start()
        node._out.close()
        makedirs.assert_not_called()
        self.assertEqual(spawn.call_args.args[0][0], pf0.STUB)
        self.assertIsNone(spawn.call_args.args[1])

    def test_version_is_a_version_spawn(self):
        done = subprocess.CompletedProcess([], 0, stdout="Dash Core RPC client version v0\nmore\n", stderr="")
        with mock.patch.object(impls.subprocess, "run", return_value=done) as run:
            self.assertEqual(impls.RustImpl(pf0.STUB).version(), "Dash Core RPC client version v0")
        self.assertEqual(run.call_args.args[0], pf0.stub_argv("version", [f"-datadir={pf0.VERSION_DATADIR}",
                                                                           "-version"]))
        self.assertIs(run.call_args.kwargs["stdin"], subprocess.DEVNULL)
        with mock.patch.object(impls.subprocess, "run", return_value=done):
            d = impls.RustImpl(pf0.STUB).describe()
        self.assertIsNone(d["sha256"], "node-bench can't see the exe; the broker records it")

    def test_rpc_response_cap(self):
        class Big(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                self.rfile.read(int(self.headers["Content-Length"]))
                body = b'{"result": "' + b"x" * 4096 + b'", "error": null, "id": 1}'
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *a):
                pass

        srv = http.server.HTTPServer(("127.0.0.1", 0), Big)
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        self.addCleanup(srv.server_close)
        self.addCleanup(srv.shutdown)
        r = rpc.Rpc(srv.server_address[1], "u", "p", timeout=10)
        self.addCleanup(r.close)
        with mock.patch.object(pf0, "RPC_RESPONSE_CAP", 1024), self.assertRaisesRegex(pf0.Pf0Error, "PF0 cap"):
            r.call("getblockcount")
        self.assertEqual(len(r.call("getblockcount")), 4096)


class Pf0Args(unittest.TestCase):
    def args(self, **kw):
        base = {"impl": "rust", "rust_bin": None, "wrap": None, "ctl_fifo": None, "node_slice": "", "aux_slice": "",
                "keep": False}
        return types.SimpleNamespace(**{**base, **kw})

    def test_accepted(self):
        a = self.args()
        pf0.check_args(a)
        self.assertEqual(a.rust_bin, pf0.STUB)

    def test_refused(self):
        for kw in ({"impl": "dashd"}, {"rust_bin": "/work/x/rdashd"}, {"wrap": "perf stat --"},
                   {"node_slice": "numa:0"}, {"aux_slice": "numa:1"}, {"keep": True}, {"ctl_fifo": "/tmp/c"}):
            with self.subTest(**kw), self.assertRaisesRegex(SystemExit, "workload --pf0 needs"):
                pf0.check_args(self.args(**kw))

    def test_workload_without_a_pf0_path(self):
        for name, why in (("ibd-testnet-seed", "has no PF0 path"), ("mempool-accept", "has only a PF0 preflight")):
            a = types.SimpleNamespace(list=False, name=name, pf0=True, **{k: None for k in workloads.DEFAULTS},
                                      impl="rust", rust_bin=None, wrap=None, ctl_fifo=None, node_slice="",
                                      aux_slice="", keep=False)
            with self.subTest(name), mock.patch.object(pf0, "_on", False), self.assertRaisesRegex(SystemExit, why):
                workloads.cmd_workload(a)
            self.assertFalse(pf0.on())


SUPPORT_RS = os.path.join(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))),
                          "dashd-node", "src", "init", "support.rs")
ARGV = [pf0.STUB, "-datadir=/pf0/run/node", "-regtest", "-listen=0", "-mocktime=1700000000", "-startupnotify=x"]


def refusing(*options, then="ready", owner="XX1"):
    """An attempt function: refuse `options` in turn (each while it is still in the argv), then `then`."""
    calls = []

    def attempt(argv, i, **_):
        calls.append(list(argv))
        for o in options:
            if any(a == o or a.startswith(o + "=") for a in argv):
                return "exit", (1, ["init message: loading", f"Error: {o} is not supported by rdashd yet ({owner})."])
        return then if isinstance(then, tuple) else (then, "ready")

    return attempt, calls


class Preflight(unittest.TestCase):
    def test_refusal_contract_matches_rdashd(self):
        """Every refusal rdashd can print (the owners and refusable options of init/support.rs, through
        not_supported()'s format) matches the regex: the contract can't drift silently."""
        with open(SUPPORT_RS) as f:
            src = f.read()
        fmt = re.search(r'format!\("\{name\} (is not supported by rdashd yet) \(\{owner\}\)\."\)', src)
        self.assertIsNotNone(fmt, "not_supported() changed its format")
        owners = set(re.findall(r'Unsupported\("([^"]+)"\)', src)) | set(re.findall(r'=> "([^"]+)"', src)) | \
            set(re.findall(r'Some\("([^"]+)"\)', src))
        options = {n for n, kind in re.findall(r'\("(-[^"]+)", (\w+)', src) if kind != "Implemented"}
        self.assertGreater(len(owners), 20)
        self.assertGreater(len(options), 50)
        for o in sorted(options):
            for owner in sorted(owners):
                line = f"Error: {o} is not supported by rdashd yet ({owner})."
                self.assertEqual(pf0.refusal([line]), (o, owner), line)

    def test_near_misses_are_not_refusals(self):
        for line in ("Error: -txindex is not supported by rdashd yet (CH8)",  # no full stop
                     "-txindex is not supported by rdashd yet (CH8).",  # no Error: prefix
                     "Error: -txindex is not supported by rdashd (CH8).",
                     "Error: txindex is not supported by rdashd yet (CH8).",
                     "Error: -TxIndex is not supported by rdashd yet (CH8).",
                     "Error: -txindex is not supported by rdashd yet (CH 8).",
                     "Error: -txindex is not supported by rdashd yet (CH8). trailing"):
            with self.subTest(line=line):
                self.assertIsNone(pf0.refusal([line]))
        self.assertIsNone(pf0.refusal(["Error: -a is not supported by rdashd yet (X1).", "Shutdown: done"]),
                          "only the last line counts")

    def test_accepted_first_time(self):
        attempt, calls = refusing()
        rec = pf0.preflight(ARGV, attempt)
        self.assertEqual(rec, {"status": "ok", "argv_sha256": pf0.argv_sha256(ARGV), "refused_on": [], "attempts": 1,
                               "accepted_by": "ready", "version": pf0.PREFLIGHT_VERSION})
        self.assertEqual(calls, [ARGV])

    def test_each_retry_drops_exactly_the_refused_token(self):
        attempt, calls = refusing("-mocktime", "-listen", owner="ND1")
        rec = pf0.preflight(ARGV, attempt)
        self.assertEqual(rec["status"], "refused")
        self.assertEqual(rec["refused_on"], [{"option": "-mocktime", "owner": "ND1"},
                                             {"option": "-listen", "owner": "ND1"}])
        self.assertEqual(rec["argv_sha256"], pf0.argv_sha256(ARGV), "the hash is of the original argv")
        self.assertEqual(calls[1], [a for a in ARGV if not a.startswith("-mocktime")])
        self.assertEqual(calls[2], [a for a in ARGV if not a.startswith(("-mocktime", "-listen"))])

    def test_retry_errors(self):
        cases = {
            "not in the row's argv": lambda argv, i: ("exit", (1, ["Error: -txindex is not supported by rdashd yet "
                                                                   "(CH8)."])),
            "exit 2, not a refusal": lambda argv, i: ("exit", (2, ["Error: -listen is not supported by rdashd yet "
                                                                   "(P2b)."])),
            "exit 1, not a refusal": lambda argv, i: ("exit", (1, ["Error: something else"])),
            "no output": lambda argv, i: ("exit", (0, [])),
        }
        for what, attempt in cases.items():
            with self.subTest(what), self.assertRaisesRegex(pf0.PreflightError, what):
                pf0.preflight(ARGV, attempt)
        twice = [pf0.STUB, "-datadir=/pf0/run/node", "-debug=net", "-debug=rpc"]
        attempt, _ = refusing("-debug")
        with self.assertRaisesRegex(pf0.PreflightError, "sets 2 times"):
            pf0.preflight(twice, attempt)

    def test_retry_budget(self):
        many = [pf0.STUB] + [f"-opt{i}=1" for i in range(40)]
        attempt, calls = refusing(*[f"-opt{i}" for i in range(40)])
        with self.assertRaisesRegex(pf0.PreflightError, "still refuses after 33 attempts"):
            pf0.preflight(many, attempt)
        self.assertEqual(len(calls), 1 + pf0.MAX_RETRIES)
        # fewer options than the budget: the options run out first, and one more refusal names none of them
        few = [pf0.STUB, "-a=1", "-b=1"]
        attempt, calls = refusing("-a", "-b", then=("exit", (1, ["Error: -c is not supported by rdashd yet (X)."])))
        with self.assertRaisesRegex(pf0.PreflightError, "-c, which is not in the row's argv"):
            pf0.preflight(few, attempt)
        self.assertEqual(calls, [few, few[:1] + few[2:], few[:1]])

    def test_argv_hash(self):
        self.assertEqual(pf0.ARGV_VOLATILE, ())
        self.assertEqual(pf0.argv_sha256(["a", "b"]), hashlib.sha256(b"a\0b").hexdigest())
        self.assertNotEqual(pf0.argv_sha256(ARGV), pf0.argv_sha256(ARGV[:-1]))


class RunAttempt(unittest.TestCase):
    """run_attempt against a fake stub: every path returns or raises PreflightError with the stub gone."""

    def setUp(self):
        import socket
        import tempfile

        d = tempfile.TemporaryDirectory()
        self.addCleanup(d.cleanup)
        with socket.socket() as s:
            s.bind(("127.0.0.1", 0))
            port = s.getsockname()[1]
        self.datadir = os.path.join(d.name, "node")
        for patch in (mock.patch.object(pf0, "STUB", os.path.join(os.path.dirname(__file__), "fake_stub.py")),
                      mock.patch.object(pf0, "RUN", d.name), mock.patch.object(pf0, "NODE_RPC", port),
                      mock.patch.object(pf0, "NODE_DATADIR", self.datadir), mock.patch.object(pf0, "SETTLE_S", 1),
                      mock.patch.object(pf0, "STOP_WAIT", 3),
                      mock.patch.dict(os.environ, {"PF0_TEST_PORT": str(port)})):
            patch.start()
            self.addCleanup(patch.stop)
        self.spawned = []
        real = subprocess.Popen

        def popen(*a, **kw):
            p = real(*a, **kw)
            self.spawned.append(p)
            return p

        patch = mock.patch.object(pf0.subprocess, "Popen", popen)
        patch.start()
        self.addCleanup(patch.stop)

    def attempt(self, mode, i=0, stale=False, **kw):
        import shutil

        shutil.rmtree(self.datadir, ignore_errors=True)  # the broker gives every spawn a fresh datadir (D46)
        if stale:  # the previous attempt's marker, before the broker has replaced the datadir
            os.makedirs(self.datadir)
            open(os.path.join(self.datadir, "ready"), "w").close()
        try:
            return pf0.run_attempt([pf0.STUB, f"-datadir={self.datadir}", f"-mode={mode}"], i, **kw)
        finally:
            self.assertTrue(self.spawned and all(p.poll() is not None for p in self.spawned), "the stub is left")

    def test_ready(self):
        self.assertEqual(self.attempt("serve", deadline=30), ("ready", "ready"))

    def test_ready_file(self):
        self.assertEqual(self.attempt("touch", ready="file", deadline=30), ("ready", "ready"))
        with self.assertRaisesRegex(pf0.PreflightError, "no ready signal in 2 s"):
            self.attempt("quiet", ready="file", deadline=2)

    def test_settled_only_after_a_refusal(self):
        """D89: a start that runs SETTLE_S without refusing accepted the remaining argv, but only on a retry."""
        self.assertEqual(self.attempt("quiet", i=1, ready="file", deadline=30), ("ready", "settled"))
        with self.assertRaisesRegex(pf0.PreflightError, "no ready signal"):
            self.attempt("quiet", i=0, ready="file", deadline=3)

    def test_settled_start_must_stop_with_exit_0(self):
        """D93: a settled start (silent, as rdashd is) must stop with exit 0; one that stops with its refusal is that
        refusal, one killed before its handlers or stopping otherwise is an error."""
        kind, (code, lines) = self.attempt("late", i=1, ready="file", deadline=30)
        self.assertEqual((kind, code, pf0.refusal(lines)), ("exit", 1, ("-mode", "XX1")))
        with self.assertRaisesRegex(pf0.PreflightError,
                                    r"settled, but stopped with 3 \(init not done in 1 s: load\?\): init message: loading"):
            self.attempt("dirty", i=1, ready="file", deadline=30)
        with self.assertRaisesRegex(pf0.PreflightError, r"settled, but stopped with -15 .*: no output"):
            self.attempt("killed", i=1, ready="file", deadline=30)

    def test_a_ready_start_must_stop_with_exit_0(self):
        """D99: a ready signal is not trusted on its own (a PR node can answer RPC before an option's refusal check):
        a ready start that stops with its refusal is that refusal, any other non-zero exit an error."""
        kind, (code, lines) = self.attempt("serve-late", deadline=30)
        self.assertEqual((kind, code, pf0.refusal(lines)), ("exit", 1, ("-mode", "XX1")))
        with self.assertRaisesRegex(pf0.PreflightError, r"attempt 1: ready, but stopped with 3: shutdown: dirty"):
            self.attempt("serve-dirty", deadline=30)
        kind, (code, lines) = self.attempt("touch-late", ready="file", deadline=30)
        self.assertEqual((kind, code), ("exit", 1))

    def test_a_stale_marker_is_not_ready(self):
        """D99: a marker counts only in this spawn's datadir; the previous attempt's, before the broker replaced
        the datadir, is not this start's ready signal."""
        with self.assertRaisesRegex(pf0.PreflightError, "no ready signal in 2 s"):
            self.attempt("quiet", stale=True, ready="file", deadline=2)
        with self.assertRaisesRegex(pf0.PreflightError, "no ready signal in 2 s"):
            self.attempt("dirty", stale=True, ready="file", deadline=2)
        self.assertEqual(self.attempt("fresh", stale=True, ready="file", deadline=30), ("ready", "ready"))

    def test_a_stale_marker_across_retries(self):
        """D99: the retry after a refusal starts with the refused attempt's marker in place (no broker here to
        replace the datadir): it settles, it is not ready at once."""
        import shutil

        shutil.rmtree(self.datadir, ignore_errors=True)
        argv = [pf0.STUB, f"-datadir={self.datadir}", "-mode=retry", "-txindex"]
        rec = pf0.preflight(argv, lambda cur, i: pf0.run_attempt(cur, i, ready="file", deadline=30))
        self.assertEqual((rec["status"], rec["refused_on"], rec["accepted_by"]),
                         ("refused", [{"option": "-txindex", "owner": "XX1"}], "settled"))
        self.assertTrue(all(p.poll() is not None for p in self.spawned))

    def test_node_text_is_escaped(self):
        self.assertEqual(pf0.printable("a\n| PASS |\x1b[2J"), "a\\n| PASS |\\x1b[2J")

    def test_refusal(self):
        kind, (code, lines) = self.attempt("refuse", deadline=30)
        self.assertEqual((kind, code), ("exit", 1))
        self.assertEqual(pf0.refusal(lines), ("-mode", "XX1"))

    def test_log_tail_only(self):
        kind, (code, lines) = self.attempt("spew", deadline=30)
        self.assertEqual(pf0.refusal(lines), ("-mode", "XX1"))
        self.assertLessEqual(sum(len(x) + 1 for x in lines), pf0.LOG_TAIL)

    def test_rpc_not_a_node(self):
        # the garbage answer ends the attempt when it arrives; the deadline only bounds a fake that never listens
        with self.assertRaisesRegex(pf0.PreflightError, "running, but not ready: http: HTTP 500"):
            self.attempt("garbage", deadline=60)

    def test_a_missing_method_is_a_refusal_of_the_row(self):
        """D113: a node past warmup that answers getblockcount, or a method the row calls, with -32601 accepted the
        argv; the attempt names what it lacks. Any other JSON-RPC answer (a parameter error) is the method's."""
        self.assertEqual(self.attempt("nomethod", deadline=30), ("missing", ["getblockcount"]))
        self.assertEqual(self.attempt("nomethod", deadline=30, methods=(("getblocktemplate",),)),
                         ("missing", ["getblockcount", "getblocktemplate"]))
        calls = (("sendrawtransaction",), ("getblocktemplate",), ("protx", "list"), ("quorum", "list"))
        self.assertEqual(self.attempt("partial", deadline=30, methods=calls),
                         ("missing", ["getblocktemplate", "protx list"]))
        self.assertEqual(self.attempt("partial", deadline=30, methods=(("sendrawtransaction",),)), ("ready", "ready"))

    def test_a_failing_ready_signal_is_an_error(self):
        """D113 needs getblockcount to answer, or to be missing: any other JSON-RPC error is the node's."""
        with self.assertRaisesRegex(pf0.PreflightError, "running, but not ready: getblockcount: internal error"):
            self.attempt("blockcount-error", deadline=30)

    def test_a_missing_method_start_must_stop_with_exit_0(self):
        """D113 keeps D99: a start that lacks a method but stops with its refusal is that refusal."""
        kind, (code, lines) = self.attempt("serve-late", deadline=30, methods=(("getblocktemplate",),))
        self.assertEqual((kind, code), ("exit", 1))

    def test_a_stopping_server_is_not_ready_yet(self):
        """D114: an HTTP 503 is an interrupted server (rdashd stopping after an init error) or a full work queue:
        not ready yet, so the start's exit decides (here its refusal); one that never gets past it is not ready."""
        kind, (code, lines) = self.attempt("stopping", deadline=30)
        self.assertEqual((kind, code, pf0.refusal(lines)), ("exit", 1, ("-mode", "XX1")))
        with self.assertRaisesRegex(pf0.PreflightError, "no ready signal in 2 s"):
            self.attempt("unavailable", deadline=2)

    def test_the_record_names_missing_methods(self):
        """D113: refusals, then a start that answers without getblockcount: REFUSED on both, the method owned by
        PENDING, accepted by "answered"."""
        import shutil

        shutil.rmtree(self.datadir, ignore_errors=True)
        argv = [pf0.STUB, f"-datadir={self.datadir}", "-mode=nomethod-txindex", "-txindex"]
        rec = pf0.preflight(argv, lambda cur, i: pf0.run_attempt(cur, i, deadline=30))
        self.assertEqual((rec["status"], rec["refused_on"], rec["accepted_by"], rec["attempts"], rec["version"]),
                         ("refused", [{"option": "-txindex", "owner": "XX1"},
                                      {"option": "rpc:getblockcount", "owner": "PENDING"}], "answered", 2, 2))

    def test_ignores_sigterm(self):
        with self.assertRaisesRegex(pf0.PreflightError, "did not stop within 3 s"):
            self.attempt("stubborn", deadline=30)


class StartupGenesis(unittest.TestCase):
    """§12: ND1's recipe token for token, and the workload end to end against a fake daemon (outside PF0)."""

    def test_argv_is_nd1s_recipe(self):
        impl = impls.RustImpl(pf0.STUB)
        self.assertEqual(impl.argv(workloads.genesis_config(types.SimpleNamespace())),
                         [pf0.STUB, "-datadir=/pf0/run/node", "-regtest", "-listen=0", "-dnsseed=0", "-connect=0",
                          "-fixedseeds=0", "-printtoconsole=0", "-par=16", "-mocktime=1700000000",
                          "-startupnotify=touch /pf0/run/node/ready"])

    def test_registry(self):
        """§12: startup-genesis is measured with the file signal; the fixture rows are preflight-only over the
        builder their measured run uses (the exact measured argv, §7)."""
        self.assertEqual(sorted(workloads.PF0_WORKLOADS),
                         ["gbt", "mempool-accept", "rpc", "startup", "startup-genesis"])
        self.assertEqual([n for n, w in workloads.PF0_WORKLOADS.items() if w.measured], ["startup-genesis"])
        a = types.SimpleNamespace(par=1)
        with mock.patch.object(pf0, "_on", True):
            for name, idx in (("startup", False), ("rpc", True)):
                self.assertEqual(workloads.PF0_WORKLOADS[name].config(a),
                                 workloads.fixture_config(a, pf0.NODE_DATADIR, idx))
                self.assertEqual((workloads.PF0_WORKLOADS[name].config(a).rpcport,
                                  workloads.PF0_WORKLOADS[name].config(a).p2pport), (pf0.NODE_RPC, pf0.NODE_P2P))

    def run_workload(self, env=None):
        import argparse
        import contextlib
        import io
        import json
        import tempfile

        from nodebench import paths

        d = tempfile.TemporaryDirectory()
        self.addCleanup(d.cleanup)
        args = argparse.Namespace(list=False, name="startup-genesis", impl="rust", dashd_bin=None,
                                  rust_bin=os.path.join(os.path.dirname(__file__), "fake_genesis.py"), rust_cli=None,
                                  wrap=None, json=os.path.join(d.name, "m.json"), keep=False, node_slice=None,
                                  aux_slice=None, out=os.path.join(d.name, "r"), ctl_fifo=None, pf0=False,
                                  **{k: None for k in workloads.DEFAULTS})
        with mock.patch.object(paths, "ROOT", d.name), mock.patch.dict(os.environ, env or {}), \
                contextlib.redirect_stdout(io.StringIO()):
            rc = workloads.cmd_workload(args)
        with open(args.json) as f:
            return rc, json.load(f)

    def test_end_to_end(self):
        rc, m = self.run_workload()
        self.assertEqual((rc, m["status"], m["exit_code"]), (0, "ok", 0), m.get("error"))
        self.assertEqual([e["event"] for e in m["markers"]], ["spawn", "begin", "ready", "stop", "end", "exit"])
        self.assertTrue(m["argv"][-1].startswith("-startupnotify=touch ") and m["argv"][-1].endswith("/ready"))
        self.assertEqual(m["inputs"]["mocktime"], 1700000000)

    def test_nonzero_exit_after_sigterm_is_an_error(self):
        rc, m = self.run_workload({"FAKE_GENESIS_EXIT": "2"})
        self.assertEqual((rc, m["status"]), (1, "error"))
        self.assertIn("exited with 2 after SIGTERM", m["error"])


class RpcRowCalls(unittest.TestCase):
    """D113: the rpc row's preflight probes the call of each build_workload label (CALL_METHODS), so the tables must
    agree."""

    def test_call_methods_match_build_workload(self):
        import random

        from nodebench.scenarios import rpc as scenario

        block = {"tx": [{"txid": "t", "vout": [{"n": 0, "scriptPubKey": {"address": "a"}}]}]}
        fake = types.SimpleNamespace(batch=lambda calls: ["h" if m == "getblockhash" else block for m, _ in calls],
                                     quorum=lambda *p: {"llmq_test": ["q"]})
        w = scenario.build_workload(fake, [1, 2], random.Random(1), True)
        self.assertEqual(set(w), set(scenario.CALL_METHODS))
        for label, calls in w.items():
            method, *sub = scenario.CALL_METHODS[label]
            with self.subTest(label):
                self.assertEqual({(m, *p[:len(sub)]) for m, p in calls}, {(method, *sub)})


class PreflightCommand(unittest.TestCase):
    """`workload NAME --pf0 --preflight`: the batch runs over the row's measured argv; the manifest has its record."""

    def setUp(self):
        import json
        import tempfile

        self.json = json
        d = tempfile.TemporaryDirectory()
        self.addCleanup(d.cleanup)
        self.manifest = os.path.join(d.name, "manifest.json")
        reg = mock.patch.dict(workloads.PF0_WORKLOADS,
                              {"fake": workloads.Pf0Workload(lambda args: "CFG", "file", True)})
        reg.start()
        self.addCleanup(reg.stop)
        self.impl = types.SimpleNamespace(argv=lambda cfg: list(ARGV) if cfg == "CFG" else None)
        self.args = types.SimpleNamespace(name="fake", json=self.manifest, call="getblock_v2")

    def run_with(self, attempt):
        with mock.patch.object(pf0, "run_attempt", attempt), mock.patch("sys.stdout"):
            rc = workloads.cmd_preflight(self.args, self.impl)
        with open(self.manifest) as f:
            return rc, self.json.load(f)

    def test_ok_and_refused_records(self):
        rc, m = self.run_with(refusing()[0])
        self.assertEqual((rc, m["status"], m["preflight"]["refused_on"]), (0, "ok", []))
        self.assertEqual(m["preflight_argv"], ARGV)
        rc, m = self.run_with(refusing("-startupnotify")[0])
        self.assertEqual((rc, m["status"]), (0, "refused"))
        self.assertEqual(m["preflight"]["refused_on"], [{"option": "-startupnotify", "owner": "XX1"}])
        self.assertEqual(m["preflight"]["argv_sha256"], pf0.argv_sha256(ARGV))

    def test_measured_flag_and_ready_signal(self):
        """The manifest says whether perfgate may run the row (D92), and the attempts use the row's ready signal and
        RPC methods (D113: what the row's work calls, each once)."""
        info = ("getblockchaininfo",)
        methods = {"startup-genesis": (), "startup": (info,),
                   "mempool-accept": (info, ("sendrawtransaction",), ("getmempoolinfo",)),
                   "gbt": (info, ("sendrawtransaction",), ("getblocktemplate",)),
                   "rpc": (info, ("getblockhash",), ("getblock",))}
        for name, w in [(n, w) for n, w in workloads.PF0_WORKLOADS.items() if n != "fake"]:
            fake = workloads.Pf0Workload(lambda args: "CFG", w.ready, w.measured, w.methods)
            with self.subTest(name), mock.patch.dict(workloads.PF0_WORKLOADS, {"fake": fake}):
                seen = []
                attempt = refusing()[0]
                rc, m = self.run_with(lambda argv, i, seen=seen, attempt=attempt, **kw:
                                      seen.append((kw.get("ready"), kw.get("methods"))) or attempt(argv, i))
                self.assertEqual((rc, m["measured"]), (0, w.measured))
                self.assertEqual(seen, [("file" if name == "startup-genesis" else "rpc", methods[name])])

    def test_a_missing_method_record(self):
        """D113: an attempt that answers without a method ends the batch: REFUSED, the method owned by PENDING."""
        rc, m = self.run_with(lambda argv, i, **kw: ("missing", ["getblockcount"]))
        self.assertEqual((rc, m["status"], m["preflight"]["refused_on"], m["preflight"]["accepted_by"]),
                         (0, "refused", [{"option": "rpc:getblockcount", "owner": "PENDING"}], "answered"))

    def test_an_unknown_rpc_call_is_an_error_record(self):
        self.args.call = "getnothing"
        w = workloads.PF0_WORKLOADS["rpc"]
        with mock.patch.dict(workloads.PF0_WORKLOADS, {"fake": workloads.Pf0Workload(lambda args: "CFG", w.ready,
                                                                                       w.measured, w.methods)}):
            rc, m = self.run_with(refusing()[0])
        self.assertEqual((rc, m["status"]), (1, "error"))
        self.assertIn("unknown --call 'getnothing'", m["error"])

    def test_error_record(self):
        rc, m = self.run_with(refusing(then=("exit", (3, ["boom"])))[0])
        self.assertEqual((rc, m["status"]), (1, "error"))
        self.assertIn("exit 3, not a refusal: boom", m["error"])

    def test_preflight_needs_pf0(self):
        a = types.SimpleNamespace(list=False, name="mempool-accept", pf0=False, preflight=True,
                                  **{k: None for k in workloads.DEFAULTS})
        with self.assertRaisesRegex(SystemExit, "--preflight is a PF0 mode step"):
            workloads.cmd_workload(a)


if __name__ == "__main__":
    unittest.main()
