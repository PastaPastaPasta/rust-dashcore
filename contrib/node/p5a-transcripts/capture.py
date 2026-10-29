#!/usr/bin/env python3
"""Captures P5a's `-blocksonly=1` dashd transcript fixture from regtest dashd v24.0.0-rc.3 (P5a D23).

The format, the node setup, the peers and the synchronisation are P2a-core's
(`contrib/node/p2a-transcripts/capture.py`, whose docstring defines them); this tool only adds a
node started with `-blocksonly=1`, rdashd's default until P5 (DEC-322), and three outbound
full-relay peers: one completes the handshake (the node's `version` carries relay 0), one then
sends a `tx` (the message disconnect, net_processing.cpp:4381-4386), one an `inv` of a
transaction (the inv disconnect, net_processing.cpp:4547-4551). It writes
`dashd-p2p/tests/fixtures/dashd-v24/blocksonly-violations.txt`, which
`dashd-p2p/tests/dashd_transcripts.rs` replays.

Usage:
  P2A_DASHD=/path/to/dashd-24.0.0-rc.3/bin/dashd capture.py OUT_DIR

`P2A_CAPTURE_DIR` defaults to /work/scratch/p5a/capture here; `P2A_PORT_BASE` (default 34100)
moves the ports. It never touches /work/chains or /work/bench.
"""

import contextlib
import importlib.util
import os
import pathlib
import struct
import sys
import time

os.environ.setdefault("P2A_CAPTURE_DIR", "/work/scratch/p5a/capture")
os.environ.setdefault("P2A_PORT_BASE", "34100")

_spec = importlib.util.spec_from_file_location(
    "p2a_capture", pathlib.Path(__file__).resolve().parent.parent / "p2a-transcripts" / "capture.py")
p2a = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(p2a)

SIM_START, FULL, BIT11 = p2a.SIM_START, p2a.FULL, p2a.BIT11
MSG_TX = 1
# The `inv` the functional test p2p_blocksonly.py:36 sends.
TX_HASH = bytes.fromhex("0000000000000000000000000000000000000000000000000000000000001234")[::-1]


@contextlib.contextmanager
def scenario(out, name, index, notes, extra):
    """P2a-core's `scenario` with extra dashd arguments `extra`, genesis only (in IBD)."""
    node = p2a.Node(name, index, SIM_START, extra)
    try:
        t = p2a.Transcript(name, node, notes, ibd=True)
        yield t
        t.write(out)
    finally:
        node.stop()


def scenario_violations(out, index):
    with scenario(out, "blocksonly-violations", index, [
            "-blocksonly=1: peer 1 completes the handshake and stays; peer 2 then sends a tx,",
            "peer 3 an inv of a tx; the node disconnects both (P5a D23)."], ["-blocksonly=1"]) as t:
        for n in (1, 2, 3):
            t.connect(n, "outbound-full-relay")
            t.handshake(n, FULL | BIT11)
        # The gate disconnects before the payload is read: an empty one is enough.
        t.send(2, "tx")
        t.send(3, "inv", p2a.compact(1) + struct.pack("<I", MSG_TX) + TX_HASH)
        time.sleep(5)
        t.settle()


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    out = sys.argv[1]
    pathlib.Path(out).mkdir(parents=True, exist_ok=True)
    p2a.ROOT.mkdir(parents=True, exist_ok=True, mode=0o700)
    print("== blocksonly-violations", flush=True)
    scenario_violations(out, 0)


if __name__ == "__main__":
    main()
