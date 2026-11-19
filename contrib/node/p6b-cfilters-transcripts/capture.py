#!/usr/bin/env python3
"""Capture compact-filter serving from SHA-pinned Dash Core v24.0.0 final.

Usage: capture.py OUT_DIR
P2A_DASHD defaults to ~/.rust-dashcore-test/dashcore-24.0.0/bin/dashd.
P2A_CAPTURE_DIR and P2A_PORT_BASE may override the capture defaults.
The capture format records real blocks and waits for all peers to settle.
"""

import hashlib
import importlib.util
import os
import pathlib
import struct
import sys
import time

os.environ.setdefault("P2A_DASHD", str(pathlib.Path.home() / ".rust-dashcore-test/dashcore-24.0.0/bin/dashd"))
os.environ.setdefault("P2A_CAPTURE_DIR", "/work/scratch/p3/laneg/tmp-g10/capture")
os.environ.setdefault("P2A_PORT_BASE", "34900")

_spec = importlib.util.spec_from_file_location(
    "p4b_capture", pathlib.Path(__file__).resolve().parent.parent / "p4b-transcripts/capture.py")
p4b = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(p4b)
p2a = p4b.p2a
DASHD_SHA256 = "2ad05ccb0951bc127d1048bf0bfea0956a968482644ed130f8f59fbfcd732223"
# The scenario checks this pin before starting any node.
p4b.p3.DASHD_SHA256 = DASHD_SHA256


def request(start, stop, filter_type=0):
    return bytes([filter_type]) + (b"" if start is None else struct.pack("<I", start)) + stop


def wait_index(node):
    end = time.monotonic() + 300
    while time.monotonic() < end:
        info = node.rpc("getindexinfo").get("basic block filter index", {})
        if info.get("synced") and info.get("best_block_height") == node.rpc("getblockcount"):
            return
        time.sleep(0.1)
    raise RuntimeError("basic filter index did not synchronize")


def node_height(node, block_hash):
    return node.rpc("getblockheader", block_hash[::-1].hex())["height"]


def serve_cfilters(out):
    stale = {}

    def setup(node):
        node.mine(999)
        node.mine(1)
        stale_hash = node.rpc("getbestblockhash")
        stale["hash"] = bytes.fromhex(stale_hash)[::-1]
        stale["block"] = node.rpc("getblock", stale_hash, 0)
        node.rpc("invalidateblock", stale_hash)
        node.rpc("setmocktime", p2a.SIM_START + 1)
        node.rpc("generatetodescriptor", 1, "raw(52)")
        node.mine(100)
        assert node.rpc("getblockcount") == 1100
        # Extend the 1,100-block setup to exercise 2,000/2,001 and two checkpoints.
        node.mine(900)
        node.rpc("reconsiderblock", stale_hash)
        node.rpc("setmocktime", p2a.SIM_START)
        wait_index(node)
        return ["A formerly active height-1000 block is recorded after its parent, before the active fork."]

    with p4b.scenario(out, "serve-cfilters", 0, [
            "v24.0.0 final, basic filter index and compact-filter service enabled.",
            "1,100-block setup extended to 2,000 for the header limit and two checkpoint intervals."],
            extra=("-blockfilterindex=1", "-peerblockfilters=1"), setup=setup) as (t, c):
        at = [i for i, line in enumerate(t.lines) if line.startswith("submitblock ")][999]
        t.lines.insert(at, "submitblock " + stale["block"])
        t.digest_over = 4096
        p4b.peer(t, 1)
        for cmd, start, stop in [
                ("getcfilters", 1, c.hashes[1000]),
                ("getcfilters", 0, c.hashes[999]),
                ("getcfheaders", 1, c.hashes[2000]),
                ("getcfheaders", 0, c.hashes[1999]),
                ("getcfheaders", 1001, c.hashes[1100]),
                ("getcfcheckpt", None, c.hashes[999]),
                ("getcfcheckpt", None, c.hashes[1000]),
                ("getcfcheckpt", None, c.hashes[2000]),
                ("getcfcheckpt", None, stale["hash"]),
                ("getcfcheckpt", None, c.hashes[2000]),
                ("getcfheaders", 999, stale["hash"]),
                ("getcfilters", 1000, stale["hash"])]:
            before = len(t.lines)
            t.send(1, cmd, request(start, stop))
            reply = {"getcfilters": "cfilter", "getcfheaders": "cfheaders", "getcfcheckpt": "cfcheckpt"}[cmd]
            count = sum(line.startswith(f"< 1 {reply} ") for line in t.lines[before:])
            expected = (node_height(t.node, stop) - start + 1) if cmd == "getcfilters" else 1
            assert count == expected and not t.peers[1].closed, f"{cmd}: {count} replies, expected {expected}"
        cases = [
            ("getcfilters", 0, c.hashes[1000], 0),
            ("getcfheaders", 0, c.hashes[2000], 0),
            ("getcfilters", 1001, c.hashes[1000], 0),
            ("getcfheaders", 1001, c.hashes[1000], 0),
            ("getcfcheckpt", None, bytes([7]) * 32, 0),
            ("getcfilters", 0, c.hashes[0], 255),
            ("getcfheaders", 0, c.hashes[0], 255),
            ("getcfcheckpt", None, c.hashes[0], 255),
        ]
        for n, (cmd, start, stop, ty) in enumerate(cases, 2):
            p4b.peer(t, n)
            t.send(n, cmd, request(start, stop, ty))
            assert t.peers[n].closed, f"{cmd} invalid request kept peer"


def serve_no_cfilters(out):
    with p4b.scenario(out, "serve-no-cfilters", 1, [
            "Basic index enabled, compact-filter service disabled: all three requests disconnect."],
            extra=("-blockfilterindex=1", "-peerblockfilters=0")) as (t, c):
        for n, (cmd, start) in enumerate([
                ("getcfilters", 0), ("getcfheaders", 0), ("getcfcheckpt", None)], 1):
            p4b.peer(t, n)
            t.send(n, cmd, request(start, c.hashes[0]))
            assert t.peers[n].closed, f"{cmd} unsupported service kept peer"


if __name__ == "__main__":
    if hashlib.sha256(pathlib.Path(p2a.DASHD).read_bytes()).hexdigest() != DASHD_SHA256:
        sys.exit(f"dashd is not the pinned v24.0.0 final binary ({DASHD_SHA256})")
    out = pathlib.Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=True)
    serve_cfilters(out)
    serve_no_cfilters(out)
