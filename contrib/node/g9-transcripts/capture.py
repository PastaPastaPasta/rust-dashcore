#!/usr/bin/env python3
"""Capture the v24.0.0 final-regtest getqrinfo limit transcript.

This uses the P4b transcript tool's P2a node, peer, framing and settling helpers. The two
requests are emitted with Core's wire encoding: a truncated request and 4,097 base hashes.
Dynamic version nonces and ping payloads are normalized like transcript replay, and trailing whitespace is stripped, so reruns are byte-reproducible.

Usage: capture.py OUT_DIR
"""

import contextlib
import importlib.util
import os
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parents[3]
P4B = ROOT / "contrib/node/p4b-transcripts/capture.py"
spec = importlib.util.spec_from_file_location("p4b_capture", P4B)
p4b = importlib.util.module_from_spec(spec)
spec.loader.exec_module(p4b)
p2a = p4b.p2a

DASHD = os.environ.get(
    "P2A_DASHD", str(pathlib.Path.home() / ".rust-dashcore-test/dashcore-24.0.0/bin/dashd")
)
SIM_START = p2a.SIM_START


def oversized_getqrinfo():
    return b"\xfd\x01\x10" + bytes(4097 * 32) + bytes(32) + b"\x00"


@contextlib.contextmanager
def scenario(out_dir):
    node = p2a.Node("serve-qrinfo-limits", 0, SIM_START)
    try:
        t = p2a.Transcript(
            "serve-qrinfo-limits",
            node,
            ["Malformed and oversized getqrinfo requests from separate inbound peers."],
            ibd=True,
        )
        t.accept(1)
        t.handshake(1, p2a.FULL | p2a.BIT11)
        t.send(1, "getqrinfo", b"\x00")
        t.accept(2)
        t.handshake(2, p2a.FULL | p2a.BIT11)
        t.send(2, "getqrinfo", oversized_getqrinfo())
        yield t
    finally:
        node.stop()


def normalize(line):
    fields = line.split(" ")
    if len(fields) >= 4 and fields[0] == "<":
        command, payload = fields[2], bytearray.fromhex(fields[3])
        if command == "version":
            payload[72:80] = bytes(8)
            user_agent_len = payload[80]
            challenge = 81 + user_agent_len + 5
            payload[challenge:challenge + 32] = bytes(32)
            fields[3] = payload.hex()
        elif command == "ping":
            fields[3] = bytes(len(payload)).hex()
        line = " ".join(fields)
    return line.rstrip()


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: capture.py OUT_DIR")
    out_dir = pathlib.Path(sys.argv[1])
    out_dir.mkdir(parents=True, exist_ok=True)
    p2a.DASHD = DASHD
    p2a.ROOT = out_dir / "nodes"
    p2a.PORT_BASE = int(os.environ.get("P2A_PORT_BASE", "32900"))
    p2a.QUIET = float(os.environ.get("P2A_QUIET", "0.5"))
    if not pathlib.Path(DASHD).is_file():
        raise SystemExit(f"dashd not found: {DASHD}")
    with scenario(out_dir) as transcript:
        path = out_dir / "serve-qrinfo-limits.txt"
        path.write_text("\n".join(normalize(line) for line in transcript.lines) + "\n")
        print(f"{path}: {sum(1 for l in transcript.lines if l.startswith('<'))} node messages")


if __name__ == "__main__":
    main()
