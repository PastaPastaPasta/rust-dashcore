#!/usr/bin/env python3
"""Capture Dash Core v24 `qrinfo` messages (DIP-0024 quorum rotation info) from a regtest chain.

Restarts dashd on a copy of the node0 datadir that Core's functional test `feature_llmq_rotation`
left behind (with the chain parameters the test ran it with, see contrib/core-vectors/extract.py),
then for each request asks the node over P2P (`getqrinfo`) and over RPC (`quorum rotationinfo`)
and keeps the raw `qrinfo` payload with Core's JSON for the same request. The capturing peer
speaks protocol 70242, so Core builds the reply like the RPC does (EFFICIENT_QRINFO_VERSION).

Usage: capture_qrinfo.py <dashd-bin-dir> <node0-datadir> <work-dir> <out-json>
Core's full JSON for each request is also written to <work-dir>/rotationinfo-<n>.json.
"""
import json
import os
import socket
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import extract  # noqa: E402
import p2p_capture as p2p  # noqa: E402

PORT = 31750  # RPC; P2P is PORT + 1
TEST = "feature_llmq_rotation"


def getqrinfo(port, base_hashes, request_hash, extra_share):
    """Raw `qrinfo` payload for a `getqrinfo` request (hashes in RPC hex order), None if unanswered."""
    payload = (p2p.varint(len(base_hashes)) + b"".join(bytes.fromhex(h)[::-1] for h in base_hashes) +
               bytes.fromhex(request_hash)[::-1] + bytes([extra_share]))
    try:
        return p2p.request(port, "getqrinfo", payload, "qrinfo", timeout=60)
    except (socket.timeout, EOFError):
        return None


def slim_diff(d):
    """The parts of an mnlistdiff's JSON a decoder test compares."""
    keep = ("nVersion", "baseBlockHash", "blockHash", "cbTxMerkleTree", "deletedMNs", "deletedQuorums",
            "merkleRootMNList", "merkleRootQuorums", "quorumsCLSigs")
    out = {k: d[k] for k in keep if k in d}
    out["mnList"] = [{k: m[k] for k in ("nVersion", "nType", "proRegTxHash", "isValid")} for m in d["mnList"]]
    out["newQuorums"] = [{k: q[k] for k in ("version", "llmqType", "quorumHash", "quorumIndex")}
                         for q in d["newQuorums"]]
    return out


def slim(info):
    """Core's `quorum rotationinfo` JSON with the diffs cut down to what a test compares."""
    out = {}
    for k, v in info.items():
        if k.startswith("mnListDiff") and k != "mnListDiffList":
            out[k] = slim_diff(v)
        elif k == "mnListDiffList":
            out[k] = [slim_diff(d) for d in v]
        else:  # extraShare, snapshots, lastCommitmentPerIndex: kept whole
            out[k] = v
    return out


def main():
    bindir, src, work, out = sys.argv[1:5]
    os.makedirs(work, exist_ok=True)
    rpc = extract.start_node(bindir, src, os.path.join(work, "node0"), PORT)
    try:
        tip = rpc("getblockcount")
        genesis = rpc("getblockhash", 0)
        tip_hash = rpc("getbestblockhash")
        cycle = 24  # llmq_test_dip0024 dkgInterval
        # The test's own base: the work block of the cycle before the tip's (H-C).
        hmc_base = rpc("getblockhash", tip - tip % cycle - cycle - 8)
        # Before the tip cycle's quorums are mined, the last commitment per index is a cycle older,
        # so Core also needs the snapshot and diff 4 cycles back: quorumSnapshotList/mnListDiffList.
        before_mining = tip - tip % cycle + 8
        requests = [(tip, [genesis], False), (tip, [genesis], True), (tip, [hmc_base, genesis], True),
                    (before_mining, [genesis], False)]
        vectors = []
        for n, (height, bases, extra) in enumerate(requests):
            block = rpc("getblockhash", height)
            try:
                core = rpc("quorum", "rotationinfo", block, extra, bases)
            except RuntimeError as e:
                print(f"@{height} extraShare={extra}: RPC refused: {e}", file=sys.stderr)
                continue
            message = getqrinfo(PORT + 1, bases, block, extra)
            if message is None:
                print(f"@{height} extraShare={extra}: no qrinfo over P2P", file=sys.stderr)
                continue
            json.dump(core, open(os.path.join(work, f"rotationinfo-{n}.json"), "w"), indent=1)
            vectors.append({"source": f"{TEST}@{height}",
                            "request": {"baseBlockHashes": bases, "blockRequestHash": block, "extraShare": extra},
                            "message": message.hex(), "core": slim(core)})
            print(f"@{height} extraShare={extra} bases={len(bases)}: {len(message)} bytes", file=sys.stderr)
    finally:
        rpc("stop")
        time.sleep(3)
    version = extract.dashd_version(bindir)
    generator = f"{version} (`dashd -version`) on regtest: capture_qrinfo.py on a copy of {TEST}/node0"
    with open(out, "w") as f:
        json.dump({"generator": generator, "qrinfo": vectors}, f, indent=1)
        f.write("\n")


if __name__ == "__main__":
    main()
