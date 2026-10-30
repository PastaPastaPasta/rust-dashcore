#!/usr/bin/env python3
"""Stream an EV2 §6.4 window from EV2b's own O8 replica (D37, D38) for node-l1's `dml_window`.

The replica is the O8 patched node (`getcanonicalmnlist`, `-statedigestlog`) that EV2b runs
itself, so its non-pure RPCs are allowed: never point this at the live oracles. Prints, one per
line:
- `header <height> <hex>` for the headers from PARENT - PREFIX up to the parent, exclusive (the
  quorum base blocks the window's commitments name are at most a DKG interval plus its mining
  window back);
- `canonical <parent> <hex>`: the list after the parent block as `getcanonicalmnlist` gives it
  (the real list: Core's internal ids and nTotalRegisteredCount);
- `<parent> <hex>`: the parent block;
- for each block, `quorumh <llmqType> <quorum hash> <proTxHash>...` for every non-null
  commitment it mines whose quorum is new to the window (members in Core's order, from
  `quorum info`, rotated quorums included; hashes in internal byte order), then
  `<height> <hex>`: the block.

The per-block digests to compare with are the replica's own `digest.jsonl` records (`mnl`):
`dml-digest-compare.py`, after `dml_window` printed WINDOW OK.

The network is the replica's own; `dml_window` takes `--testnet` for a testnet replica.

Usage: dml-replica-fetch.py --port PORT --cookie PATH PARENT COUNT
"""
import argparse
import importlib.util
import sys
from pathlib import Path

CHECK = Path(__file__).resolve().parents[2] / "dash-oracle-sys" / "scripts" / "dml-listdiff-check.py"

# Headers kept before the parent: more than any quorum base's distance from the block that
# mines its commitment (LLMQ_400_85: a 576-block interval plus its mining window).
PREFIX = 1200


def load_check():
    sys.dont_write_bytecode = True  # no __pycache__ beside the imported script
    spec = importlib.util.spec_from_file_location("dml_listdiff_check", CHECK)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def is_null(qc):
    return qc["signersCount"] == 0 and qc["validMembersCount"] == 0 and int(qc["quorumVvecHash"], 16) == 0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, required=True)
    ap.add_argument("--cookie", required=True)
    ap.add_argument("parent", type=int)
    ap.add_argument("count", type=int)
    args = ap.parse_args()
    check = load_check()
    rpc = check.Rpc(args.port, args.cookie)
    out = sys.stdout
    block_hash = lambda h: rpc.call("getblockhash", h)
    for h in range(max(0, args.parent - PREFIX), args.parent):
        out.write(f"header {h} {rpc.call('getblockheader', block_hash(h), False)}\n")
    parent_hash = block_hash(args.parent)
    canonical = rpc.call("getcanonicalmnlist", parent_hash)
    out.write(f"canonical {args.parent} {canonical['hex']}\n")
    out.write(f"{args.parent} {rpc.call('getblock', parent_hash, 0)}\n")
    seen = set()
    for height in range(args.parent + 1, args.parent + args.count + 1):
        hash_ = block_hash(height)
        for tx in rpc.call("getblock", hash_, 2)["tx"]:
            if tx.get("type") != 6:
                continue
            qc = tx["qcTx"]["commitment"]
            key = (qc["llmqType"], qc["quorumHash"])
            if is_null(qc) or key in seen:
                continue
            seen.add(key)
            members = rpc.call("quorum", "info", qc["llmqType"], qc["quorumHash"])["members"]
            hashes = " ".join(check.internal(m["proTxHash"]).hex() for m in members)
            out.write(f"quorumh {qc['llmqType']} {check.internal(qc['quorumHash']).hex()} {hashes}\n")
        out.write(f"{height} {rpc.call('getblock', hash_, 0)}\n")
        if height % 1000 == 0:
            print(f"fetched through {height}, {len(seen)} quorums", file=sys.stderr, flush=True)
    out.flush()


if __name__ == "__main__":
    main()
