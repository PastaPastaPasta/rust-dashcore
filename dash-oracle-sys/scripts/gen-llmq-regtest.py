#!/usr/bin/env python3
"""Capture an LLMQ-context fixture and its expected answers from a regtest chain a release
dashd built (node-program O2a-LQ1):

- tests/data/llmq-regtest-<pin>.fixture.gz: an `llmq-fixture v1` (oracle.h "LLMQ fixture
  v1"), gzipped: what oracle_llmq_ctx_load seeds;
- tests/data/llmq-regtest-<pin>.json: provenance and Core's answers on the same chain.

The chain comes from Dash Core's own functional test feature_llmq_rotation, run against
the pin's release binary with --nocleanup (dash/contrib/core-vectors/run-functional-tests.sh
on node/v24a-dashcore-wire). Its nine nodes mine non-rotated LLMQ_TEST quorums before V20
(-testactivationheight=v20@300: members from the base block's list, the block-hash
modifier) and after it (the work block's list and the CbTx chainlock modifier), and
rotated LLMQ_TEST_DIP0024 cycles after it. This script copies node0's datadir, restarts it
alone (no peers) and reads, all from that private node:

- the fixture: the chain arguments the test gave (dash.conf's [regtest] section and the
  command line, as debug.log records them), the headers 0 .. tip, the masternode list after
  every block from DIP0003 (`protx listdiff 1 h`, re-encoded the way evodb stores it by
  dml-listdiff-check.py's encode_list), every block's coinbase transaction, and every
  non-null quorum commitment with its mined height (getblock 2);
- the answers: `quorum info` members, in order, of every mined quorum; the quorum snapshots
  `quorum rotationinfo` reports at H - C, H - 2C and H - 3C, requested at each block that
  mined a rotated cycle (CQuorumSnapshot bytes, keyed by the cycle's base block, the work
  block of its mnListDiffAtHMinus*C plus WORK_DIFF_DEPTH); and `quorum list <count>` at
  the tip (ScanQuorums from the tip, newest first).

Usage: gen-llmq-regtest.py --dashd <release>/bin/dashd --pin v24.0 \\
           --node0 <work>/ft/feature_llmq_rotation/node0 --datadir DIR --port PORT \\
           --out-dir tests/data
"""

import argparse
import gzip
import importlib.util
import json
import shutil
import subprocess
import time
from pathlib import Path


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(file))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


dml = load("dmlcheck", "dml-listdiff-check.py")
wire = load("contextual", "gen-contextual-regtest.py")

FIXTURE_VERSION = 1
SPECIAL_TX_QC = 6
WORK_DIFF_DEPTH = 8  # llmq::WORK_DIFF_DEPTH
# Options that shape the chain; the rest of the test's configuration is the node's.
CHAIN_OPTIONS = ("dip3params", "vbparams", "llmqtest", "testactivationheight")
SCAN_COUNTS = (1, 2, 4)


def chain_args(datadir):
    """The chain-shaping options of the node's last start, as debug.log records them: those
    from dash.conf's [regtest] section (which the copied datadir keeps) and those from the
    command line (which a restart must pass again)."""
    config, command_line = [], []
    for line in (datadir / "regtest" / "debug.log").read_text(errors="replace").splitlines():
        if "Dash Core version" in line:
            config, command_line = [], []
        for marker, into in (("Config file arg: [regtest] ", config), ("Command-line arg: ", command_line)):
            if marker in line:
                name, _, value = line.split(marker, 1)[1].strip().partition("=")
                if name.startswith(CHAIN_OPTIONS):
                    into.append(f"-{name}={value.strip(chr(34))}")
    return config, command_line


def start(dashd, node0, datadir, port, command_line):
    shutil.rmtree(datadir, ignore_errors=True)
    shutil.copytree(node0, datadir)
    # Alone: no peers, so the chain stays as the test left it. The test's dash.conf binds
    # 127.0.0.1, which -listen=0 refuses; -connect=0 keeps the node to itself.
    subprocess.run(
        [
            dashd,
            f"-datadir={datadir}",
            f"-port={port + 1}",
            f"-rpcport={port}",
            "-listen=1",
            "-connect=0",
            "-rpcdoccheck=0",
            "-checkblocks=1",
            "-checklevel=0",
            "-daemon=1",
            "-printtoconsole=0",
            *command_line,
        ],
        check=True,
    )
    rpc = dml.Rpc(port, datadir / "regtest" / ".cookie")
    for _ in range(120):
        try:
            rpc.call("getblockcount")
            return rpc
        except (RuntimeError, OSError):
            time.sleep(1)
    raise RuntimeError("dashd did not start")


def mined_commitments(block):
    """The non-null quorum commitments of a getblock-2 block: (its qcTx JSON, the commitment's
    bytes). Each block of a mining window holds a null commitment until the quorum's is mined;
    Core stores none for it."""
    for tx in block["tx"]:
        if tx.get("type") != SPECIAL_TX_QC:
            continue
        qc = tx["qcTx"]["commitment"]
        if qc["validMembersCount"] == 0 and qc["signersCount"] == 0:
            continue
        raw = bytes.fromhex(tx["hex"])
        size, i = wire.read_compact(raw, wire.tx_body_end(raw, 0))
        assert i + size == len(raw), "bytes after vExtraPayload"
        yield qc, raw[i + 6 :]  # the payload after nVersion, nHeight


def snapshot_bytes(s):
    """CQuorumSnapshot as Core serializes it: mnSkipListMode (int32), CompactSize(n) and the
    fixed bitset of activeQuorumMembers, CompactSize(m) and the int32 skip list."""
    bits = s["activeQuorumMembers"]
    packed = bytearray((len(bits) + 7) // 8)
    for i, bit in enumerate(bits):
        packed[i // 8] |= int(bit) << (i % 8)
    out = [
        dml.i32(s["mnSkipListMode"]),
        dml.compact_size(len(bits)),
        bytes(packed),
        dml.compact_size(len(s["mnSkipList"])),
    ]
    return b"".join(out + [dml.i32(x) for x in s["mnSkipList"]])


def fixture_bytes(chain, args, headers, lists, coinbases, mined):
    """An llmq-fixture v1 (no snapshot rows: Core builds them, as a node does)."""

    def sized(b):
        return dml.compact_size(len(b)) + b

    out = [bytes([FIXTURE_VERSION]), sized(chain.encode()), dml.compact_size(len(args))]
    out += [sized(a.encode()) for a in args]
    out.append(dml.compact_size(len(headers)))
    out += [dml.i32(h) + header for h, header in headers]
    out.append(dml.compact_size(len(lists)))
    out += [block + sized(lst) for block, lst in lists]
    out.append(dml.compact_size(len(coinbases)))
    out += [block + sized(tx) + b"\x01" for block, tx in coinbases]
    out.append(dml.compact_size(0))
    out.append(dml.compact_size(len(mined)))
    out += [bytes([t]) + sized(qc) + dml.i32(h) for t, qc, h in mined]
    return b"".join(out)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dashd", required=True)
    ap.add_argument("--pin", required=True)
    ap.add_argument("--node0", required=True, type=Path)
    ap.add_argument("--datadir", required=True, type=Path)
    ap.add_argument("--port", required=True, type=int)
    ap.add_argument("--out-dir", required=True, type=Path)
    # Another chain's fixture (O2a-LQ3: the lq3a-captures chain) under its own name.
    ap.add_argument("--stem", default="llmq-regtest")
    ap.add_argument(
        "--source",
        default="Dash Core functional test feature_llmq_rotation (--nocleanup), node0 restarted alone",
    )
    args = ap.parse_args()

    version = subprocess.run([args.dashd, "-version"], capture_output=True, text=True, check=True).stdout.splitlines()[
        0
    ]
    config, command_line = chain_args(args.node0)
    cargs = config + command_line
    dip3 = int(next(a for a in cargs if a.startswith("-dip3params=")).split("=")[1].split(":")[0])
    # Regtest's chain parameters (chainparams.cpp CRegTestParams) that encode_list reads.
    net = {"chain": "regtest", "dip3_height": dip3, "min_confirmations": 1, "pubkey_prefix": 140, "script_prefix": 19}
    rpc = start(args.dashd, args.node0, args.datadir, args.port, command_line)
    try:
        chain = dml.Chain(rpc, net)
        tip = rpc.call("getblockcount")
        headers, lists, coinbases, mined, mined_json = [], [], [], [], []
        for h in range(tip + 1):
            block_hash = dml.internal(chain.block_hash(h))
            headers.append((h, bytes.fromhex(rpc.call("getblockheader", chain.block_hash(h), False))))
            block = rpc.call("getblock", chain.block_hash(h), 2)
            coinbases.append((block_hash, bytes.fromhex(block["tx"][0]["hex"])))
            if h >= dip3:
                lists.append((block_hash, dml.encode_list(chain.list_json(h), h, chain)))
            for qc, commitment in mined_commitments(block):
                mined.append((qc["llmqType"], commitment, h))
                mined_json.append(
                    {
                        "type": qc["llmqType"],
                        "mined_height": h,
                        "quorum_hash": qc["quorumHash"],
                        "quorum_index": qc["quorumIndex"],
                    }
                )

        quorums, type_ids = [], {}
        for m in mined_json:
            info = rpc.call("quorum", "info", m["type"], m["quorum_hash"])
            type_ids[info["type"]] = m["type"]
            quorums.append({**m, "base_height": info["height"], "members": [x["proTxHash"] for x in info["members"]]})

        snapshots = {}
        for m in mined_json:
            if m["quorum_index"] != 0 or not any(o["type"] == m["type"] and o["quorum_index"] > 0 for o in mined_json):
                continue
            info = rpc.call("quorum", "rotationinfo", chain.block_hash(m["mined_height"]))
            for k in ("", "2", "3"):
                work = rpc.call("getblockheader", info[f"mnListDiffAtHMinus{k}C"]["blockHash"])["height"]
                cycle = work + WORK_DIFF_DEPTH
                row = snapshot_bytes(info[f"quorumSnapshotAtHMinus{k}C"]).hex()
                key = (m["type"], cycle)
                assert snapshots.get(key, row) == row, f"two snapshots for {key}"
                snapshots[key] = row
        rows = [{"type": t, "cycle_height": c, "snapshot": s} for (t, c), s in sorted(snapshots.items())]

        # The types with mined quorums (`quorum list` names them).
        scans = []
        for count in SCAN_COUNTS:
            for name, bases in sorted(rpc.call("quorum", "list", count).items()):
                if name in type_ids:
                    scans.append({"type": type_ids[name], "count": count, "bases": bases})

        fixture = fixture_bytes("regtest", cargs, headers, lists, coinbases, mined)
        args.out_dir.mkdir(parents=True, exist_ok=True)
        stem = args.out_dir / f"{args.stem}-{args.pin}"
        with gzip.GzipFile(f"{stem}.fixture.gz", "wb", mtime=0) as f:
            f.write(fixture)
        expected = {
            "provenance": {
                "dashd": version,
                "pin": args.pin,
                "source": args.source,
                "fixture_sha256": dml.sha256(fixture).hex(),
            },
            "tip": tip,
            "quorums": quorums,
            "rotation_snapshots": rows,
            "scans": scans,
        }
        Path(f"{stem}.json").write_text(json.dumps(expected, indent=1, sort_keys=True) + "\n")
    finally:
        rpc.call("stop")


if __name__ == "__main__":
    main()
