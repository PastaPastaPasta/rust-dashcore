#!/usr/bin/env python3
"""Tests of sb12a-captures.py's height guards (SB12a Sol r1 F3): with the script imported under
`python3 -O`, which strips `assert`s, a mainnet read above PLAN §7's cap and an unbounded
mainnet read still exit before any request. Nothing here reads an oracle.

Run: python3 -I -B contrib/node/test-sb12a-captures.py"""

import subprocess
import sys
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("sb12a-captures.py")

# Imports the script as a module under -O and evaluates one call in it, with every RPC and P2P
# read replaced by a RuntimeError; prints "returned" if the call returns.
PROBE = """
import importlib.util, sys
spec = importlib.util.spec_from_file_location("captures", sys.argv[1])
captures = importlib.util.module_from_spec(spec)
spec.loader.exec_module(captures)
if not sys.flags.optimize:
    raise SystemExit("not optimized")
captures.rpc = captures.Conn = lambda *a, **k: (_ for _ in ()).throw(RuntimeError("read"))
print(eval(sys.argv[2], vars(captures)) is None and "returned")
"""


def probe(call):
    return subprocess.run([sys.executable, "-O", "-I", "-B", "-c", PROBE, str(SCRIPT), call],
                          capture_output=True, text=True)


class Guards(unittest.TestCase):
    def refuses(self, call, message):
        result = probe(call)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn(message, result.stderr)
        self.assertNotIn("RuntimeError", result.stderr, "it read before refusing")

    def test_a_mainnet_capture_above_the_cap_is_refused_under_O(self):
        self.refuses("under_cap('mainnet', 2090001)", "above the cap")

    def test_every_mainnet_read_path_checks_the_cap_first(self):
        self.refuses("headers_replies(None, 'mainnet', 2082001)", "above the cap")
        self.refuses("special_blocks(None, 'mainnet', 2090001)", "above the cap")
        self.refuses("filters(None, 'mainnet', 2089002, 1000)", "above the cap")
        self.refuses("coinbase_clsig(None, 'mainnet', 2090001)", "above the cap")
        self.refuses("rpc_locks(None, 'mainnet', 2000000)", "only testnet")
        self.refuses("listen(None, 'mainnet', 1)", "only testnet")

    def test_reads_at_the_cap_and_on_testnet_pass_the_guard(self):
        for call in ["under_cap('mainnet', 2090000)", "under_cap('testnet', 3000000)",
                     "testnet_only('testnet')"]:
            result = probe(call)
            self.assertEqual((result.returncode, result.stdout.strip()), (0, "returned"),
                             result.stderr)
        # Each path's last capture at the cap gets past its guard to the (stubbed) read.
        for call in ["headers_replies(None, 'mainnet', 2082000)",
                     "special_blocks(None, 'mainnet', 2090000)",
                     "filters(None, 'mainnet', 2089001, 1000)",
                     "coinbase_clsig(None, 'mainnet', 2090000)"]:
            result = probe(call)
            self.assertIn("RuntimeError: read", result.stderr, call)


if __name__ == "__main__":
    unittest.main()
