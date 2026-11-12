#!/usr/bin/env python3
"""Generate the pinned multi-peer block fixture with release Dash Core."""

import argparse
import hashlib
import importlib.util
import os
from pathlib import Path
import struct
import subprocess
import sys
from datetime import datetime, timezone

BINARY_SHA256 = "2ad05ccb0951bc127d1048bf0bfea0956a968482644ed130f8f59fbfcd732223"
SIM_START = 1700000000
COUNTS = [2, 1, 1, 2, 3, 1, 5, 9, 1, 2, 17, 1, 33, 1, 1, 2, 4, 1, 3, 1, 2, 1, 1, 6]
ROOT = Path(__file__).resolve().parents[3]


def spend(txid, index, value, outputs=1):
    amounts = [value // outputs] * outputs
    amounts[-1] += value % outputs
    return (struct.pack("<I", 1) + b"\x01" + bytes.fromhex(txid)[::-1]
            + struct.pack("<I", index) + b"\x00" + b"\xff" * 4 + bytes([outputs])
            + b"".join(struct.pack("<q", amount) + b"\x01\x51" for amount in amounts)
            + bytes(4)).hex()


def raw(node, block_hash):
    return bytes.fromhex(node.rpc("getblock", block_hash, 0))


def generate(dashd, scratch, output):
    digest = hashlib.sha256(dashd.read_bytes()).hexdigest()
    if digest != BINARY_SHA256:
        raise ValueError(f"dashd digest mismatch: {digest}")
    os.environ["P2A_DASHD"] = str(dashd)
    os.environ["P2A_CAPTURE_DIR"] = str(scratch)
    os.environ["P2A_PORT_BASE"] = "38420"
    spec = importlib.util.spec_from_file_location("p2a", ROOT / "contrib/node/p2a-transcripts/capture.py")
    helper = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(helper)
    nodes = []
    try:
        a = helper.Node("multi-a", 0, SIM_START - 2592000, extra=("-listen=0",))
        nodes.append(a)
        a_hashes = a.rpc("generatetodescriptor", 1100, "raw(51)")
        chain_a = [raw(a, h) for h in a_hashes]
        b = helper.Node("multi-b", 1, SIM_START - 1000, extra=("-listen=0",))
        nodes.append(b)
        prefix = []
        for height in range(1, 101):
            b.rpc("setmocktime", SIM_START - 1000 + height - 1)
            prefix.append(raw(b, b.rpc("generatetodescriptor", 1, "raw(51)")[0]))
        cb = b.rpc("getblock", b.rpc("getblockhash", 1), 2)["tx"][0]
        index, coin = next((i, o) for i, o in enumerate(cb["vout"]) if o["scriptPubKey"]["hex"] == "51")
        fanout = spend(cb["txid"], index, coin["valueSat"] - 10000, 96)
        fanout_id = b.rpc("decoderawtransaction", fanout)["txid"]
        amount = (coin["valueSat"] - 10000) // 96
        workload = []
        next_output = 0
        for offset, count in enumerate(COUNTS):
            b.rpc("setmocktime", SIM_START - 900 + offset)
            txs = [fanout] if offset == 0 else []
            if offset:
                for _ in range(count - 1):
                    txs.append(spend(fanout_id, next_output, amount - 1000))
                    next_output += 1
            h = b.rpc("generateblock", "raw(51)", txs)["hash"]
            block = raw(b, h)
            assert block[80] == count
            workload.append(block)
        side_node = helper.Node("multi-side", 2, SIM_START - 880, extra=("-listen=0",))
        nodes.append(side_node)
        for block in prefix:
            assert side_node.rpc("submitblock", block.hex()) is None
        side = []
        for offset in range(3):
            side_node.rpc("setmocktime", SIM_START - 880 + offset)
            side.append(raw(side_node, side_node.rpc("generatetodescriptor", 1, "raw(51)")[0]))
        sections = [chain_a, prefix, workload, side]
        data = b"P3MB01\x00\x00" + struct.pack("<4I", *(len(s) for s in sections))
        data += b"".join(struct.pack("<I", len(block)) + block for section in sections for block in section)
        output.write_bytes(data)
        transcript = ROOT / "dashd-p2p/tests/fixtures/dashd-v24/blocks-1100.txt"
        old = [bytes.fromhex(line.split()[3]) for line in transcript.read_text().splitlines()
               if line.startswith("> ") and line.split()[2] == "block"]
        difference = next((i + 1 for i, (x, y) in enumerate(zip(old, chain_a)) if x != y), None)
        comparison = "equal" if old == chain_a else f"first differing height {difference}; old count {len(old)}"
        version = subprocess.check_output([str(dashd), "--version"], text=True)
        generator_sha = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
        head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
        provenance = f"""# Multi-peer block fixture provenance

Generated {datetime.now(timezone.utc).isoformat()}.

- Generator: `contrib/node/p3-multi-blocks/make_fixture.py`, source base commit `{head}`.
- Generator SHA256: `{generator_sha}`.
- Binary: `{dashd}`; SHA256 `{digest}`.
- Version output SHA256: `{hashlib.sha256(version.encode()).hexdigest()}`.
- Version: `{version.splitlines()[0]}`.
- Command: `python3 contrib/node/p3-multi-blocks/make_fixture.py --dashd {dashd} --scratch {scratch}`.
- Output: `node-l1/fixtures/p3_multi_blocks.bin`, SHA256 `{hashlib.sha256(data).hexdigest()}`, {len(data)} bytes.
- Chain A: 1100 blocks, mock time {SIM_START - 2592000} before generation; `blocks-1100.txt`: {comparison}.
- B prefix: heights 1..100, mock time `SIM_START - 1000 + height - 1` before each block.
- B workload: heights 101..124, mock time `SIM_START - 900 + height - 101` before each block.
- Workload transaction counts (including coinbase): {COUNTS}.
- B side: heights 101..103 on prefix tip, mock time `SIM_START - 880 + height - 101`.
- Prefix and coinbase-only blocks use `generatetodescriptor raw(51)`; workload uses `generateblock raw(51)`.
- Block 101 fans coinbase 1 into 96 OP_TRUE outputs; later spends consume distinct outputs.
- No workload transaction is 64 bytes long.
"""
        (output.parent / "PROVENANCE.md").write_text(provenance)
        print(provenance, flush=True)
    finally:
        for node in reversed(nodes):
            node.stop()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dashd", type=Path, default=Path.home() / ".rust-dashcore-test/dashcore-24.0.0/bin/dashd")
    parser.add_argument("--scratch", type=Path, required=True)
    parser.add_argument("--output", type=Path, default=ROOT / "node-l1/fixtures/p3_multi_blocks.bin")
    args = parser.parse_args()
    generate(args.dashd, args.scratch, args.output)


if __name__ == "__main__":
    main()
