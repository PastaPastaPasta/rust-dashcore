#!/usr/bin/env python3
"""Capture G0's handshake-sporks fixture with sha-pinned dashd v24.0.0 final.

Usage: capture.py OUT_DIR
Uses P4b's P2a-format capture machinery and Core's public functional-test spork key.
P2A_DASHD selects the binary; P2A_CAPTURE_DIR holds this capture's fresh datadir.
"""

import hashlib
import importlib.util
import os
import pathlib
import struct
import sys

os.environ.setdefault("P2A_CAPTURE_DIR", "/work/scratch/p3/laneg/tmp-g0/capture")
os.environ.setdefault("P2A_PORT_BASE", "31900")
os.environ.setdefault("P2A_DASHD", str(pathlib.Path.home() / ".rust-dashcore-test/dashcore-24.0.0/bin/dashd"))

spec = importlib.util.spec_from_file_location(
    "p4b_capture", pathlib.Path(__file__).resolve().parent.parent / "p4b-transcripts/capture.py")
p4b = importlib.util.module_from_spec(spec)
spec.loader.exec_module(p4b)
p2a = p4b.p2a

DASHD_SHA256 = "2ad05ccb0951bc127d1048bf0bfea0956a968482644ed130f8f59fbfcd732223"
SPORK_KEY = "cP4EKFyJsHT39LDqgdcB43Y3YXjNyjb5Fuas1GQSeAtjnZWmZEQK"


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    if hashlib.sha256(pathlib.Path(p2a.DASHD).read_bytes()).hexdigest() != DASHD_SHA256:
        sys.exit(f"dashd does not match v24.0.0 final sha256 {DASHD_SHA256}")
    out = pathlib.Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=True)
    node = p2a.Node("handshake-sporks", 8, p2a.SIM_START, [f"-sporkkey={SPORK_KEY}"])
    try:
        transcript = p2a.Transcript("handshake-sporks", node, [
            f"dashd sha256 {DASHD_SHA256}",
            "Two sporkupdate RPC calls before the inbound handshake; no inventory is masked.",
        ], ibd=True)
        for name in ("SPORK_2_INSTANTSEND_ENABLED", "SPORK_19_CHAINLOCKS_ENABLED"):
            assert node.rpc("sporkupdate", name, 0) == "success"
            transcript.lines.append(f"sporkupdate {name} 0")
        transcript.accept(1)
        transcript.handshake(1, p2a.FULL)
        invs = [bytes.fromhex(line.split()[3]) for line in transcript.lines if line.startswith("< 1 inv ")]
        assert len(invs) == 1 and len(invs[0]) == 73 and invs[0][0] == 2
        assert all(struct.unpack_from("<I", invs[0], at)[0] == 6 for at in (1, 37))
        transcript.write(out)
    finally:
        node.stop()


if __name__ == "__main__":
    main()
