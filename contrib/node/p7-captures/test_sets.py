#!/usr/bin/env python3
"""Core v24.0.0 rpc/node.cpp:156-245; llmq/net_quorum.cpp:813-827."""

import argparse
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import call, patch

import sets


class CoreSetsCapture(unittest.TestCase):
    # Core: rpc/node.cpp:207-245
    def test_core_sporkupdate_rpc(self):
        with tempfile.TemporaryDirectory(prefix="sets-rpc-") as tmp:
            root = Path(tmp)
            log = root / "regtest/debug.log"
            log.parent.mkdir()
            log.write_text("")
            node = {"datadir": str(root), "rpc_port": 19998}
            nodes = root / "nodes.json"
            nodes.write_text(json.dumps([node]))
            args = argparse.Namespace(core=root, cli=root / "dash-cli", nodes=nodes, spork_value=0)
            with (
                patch.object(sets.subprocess, "check_output", return_value=sets.CORE_COMMIT),
                patch.object(sets, "rpc", side_effect=[{"version": 240000}, None]) as rpc,
            ):
                with self.assertRaisesRegex(RuntimeError, "spork 21 update was rejected"):
                    sets.capture(args)
            self.assertEqual(rpc.call_args_list, [
                call(args.cli, node, "getnetworkinfo"),
                call(args.cli, node, "sporkupdate", sets.SPORK, 0),
            ])

    # Core: llmq/net_quorum.cpp:813-827
    def test_core_multiline_connection_log(self):
        header = "EnsureQuorumConnections -- adding masternodes quorum connections for quorum " + "01" * 32 + ":"
        entry = "  " + "02" * 32 + " (not in valid MN set anymore)"
        source = "timestamp [netconn] " + header + "\n" + entry + "\nunrelated text\n"
        self.assertEqual(sets.connection_lines(source), sorted([header, entry]))


if __name__ == "__main__":
    unittest.main()
