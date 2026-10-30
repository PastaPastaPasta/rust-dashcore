#!/usr/bin/env python3
"""Capture the dash-oracle-sys smoke-test samples from a reference dashd.

Every expected value in the output comes from dashd: its RPC (read-only calls)
for raw blocks and headers, txids, prevouts, getblockstats subsidies, masternode
payments, superblock budgets and deployment heights, and its undo files
(blocks/rev*.dat, opened read-only) for the sampled blocks' CBlockUndo records.
Nothing is hand-written.

Usage:
  gen-samples.py --network mainnet --out dash-oracle-sys/tests/data/mainnet.json
  gen-samples.py --network testnet --out dash-oracle-sys/tests/data/testnet.json

Defaults talk to the BS2 reference oracles (/work/chains/<net>, see ORACLE.md).
Heights above the node's current tip are skipped, so re-running after the node
has synced to tip extends the samples.
"""

import argparse
import base64
import glob
import hashlib
import json
import os
import sys
import urllib.request
from decimal import Decimal

PORTS = {"mainnet": 27998, "testnet": 37998}

# Heights chosen to cover the eras of each consensus rule the v1 shim exposes.
SAMPLES = {
    "mainnet": {
        # genesis, the early-diff bug boundary (4500), the subsidy era boundaries
        # (5465, 17000, 24000), KGW start (15200), DGW start (34140), MN payment
        # increase steps (158000 + n * 17280), late blocks
        "blocks": [0, 1, 4500, 4501, 5464, 5465, 15200, 16999, 17000, 23999, 24000, 34139, 34140, 34141,
                   100000, 158001, 175281, 227121, 313521, 350000, 400000, 450000],
        # [first, last] inclusive; each range must hold the lookback of every
        # next-work sample inside it (BTC: 576, KGW: 4032, DGW: 24 headers)
        "header_ranges": [[0, 1200], [11000, 15400], [29900, 34300], [99960, 100000],
                          [299960, 300000], [449960, 450000]],
        "subsidy": [1, 2, 4500, 4501, 4502, 5465, 5466, 17000, 17001, 24000, 24001, 100000, 210240,
                    210241, 300000, 420480, 420481, 450000],
        # superblocks: multiples of the 16616-block cycle from 614820 (CSuperblock::IsValidBlockHeight)
        "superblocks": [631408, 648024, 1013576, 1993920],
        # DIP3 enforcement (1088640), BRR (1374912), V20 and MN_RR eras
        "payments": [1088640, 1100000, 1374912, 1400000, 1700000, 1987776, 2128896, 2300000],
    },
    "testnet": {
        # DIP3 (7000), DIP8 (78800), BRR (387500) and DIP20 (414100) eras; blocks with
        # special transactions are added by scanning (see find_special_blocks)
        "blocks": [0, 1, 4001, 4002, 7000, 7001, 78800, 387500, 414100, 430000],
        "header_ranges": [[0, 700], [3900, 4300], [399700, 400300], [429900, 430000]],
        "subsidy": [1, 2, 4001, 4002, 5000, 7000, 210240, 210241, 387500, 400000, 414100, 430000],
        # superblocks every 24 blocks from 4200
        "superblocks": [400008, 424224, 440016, 1000008],
        "payments": [7300, 10000, 100000, 300000, 387499, 387500, 391680, 395000, 410000, 430000],
    },
}


class Rpc:
    def __init__(self, url, cookie):
        with open(cookie) as f:
            auth = base64.b64encode(f.read().strip().encode()).decode()
        self.url = url
        self.headers = {"Authorization": "Basic " + auth, "Content-Type": "application/json"}

    def batch(self, calls):
        out = []
        for i in range(0, len(calls), 500):
            chunk = calls[i:i + 500]
            body = json.dumps([{"jsonrpc": "1.0", "id": n, "method": m, "params": p}
                               for n, (m, p) in enumerate(chunk)]).encode()
            req = urllib.request.Request(self.url, body, self.headers)
            with urllib.request.urlopen(req, timeout=600) as resp:
                replies = sorted(json.load(resp, parse_float=Decimal), key=lambda r: r["id"])
            for (m, p), r in zip(chunk, replies):
                if r["error"] is not None:
                    raise RuntimeError(f"{m} {p}: {r['error']}")
                out.append(r["result"])
        return out

    def call(self, method, *params):
        return self.batch([(method, list(params))])[0]

    def block_hashes(self, heights):
        return self.batch([("getblockhash", [h]) for h in heights])


def sats(value):
    return int(value * 100_000_000)


def read_compact_size(buf, pos):
    n = buf[pos]
    if n < 0xfd:
        return n, pos + 1
    width = {0xfd: 2, 0xfe: 4, 0xff: 8}[n]
    return int.from_bytes(buf[pos + 1:pos + 1 + width], "little"), pos + 1 + width


def find_block_undos(blocks_dir, blocks):
    """Raw CBlockUndo of each sampled block with inputs, from Core's undo files.

    rev*.dat holds records `magic | u32 size | CBlockUndo | SHA256d(prev block hash | CBlockUndo)`
    (Core: node/blockstorage.cpp UndoWriteToDisk). A record is matched to a block by its
    transaction count and first input count, then confirmed by the checksum.
    """
    with open(os.path.join(blocks_dir, "blk00000.dat"), "rb") as f:
        magic = f.read(4)
    wanted = {}
    for blk in blocks:
        if len(blk["tx"]) > 1:
            key = (len(blk["tx"]) - 1, len(blk["tx"][1]["vin"]))
            wanted.setdefault(key, []).append((blk["hash"], bytes.fromhex(blk["previousblockhash"])[::-1]))
    found = {}
    remaining = sum(len(v) for v in wanted.values())
    for path in sorted(glob.glob(os.path.join(blocks_dir, "rev*.dat"))):
        if not remaining:
            break
        with open(path, "rb") as f:
            data = f.read()
        pos = 0
        while pos + 8 <= len(data) and data[pos:pos + 4] == magic:
            size = int.from_bytes(data[pos + 4:pos + 8], "little")
            undo = data[pos + 8:pos + 8 + size]
            checksum = data[pos + 8 + size:pos + 40 + size]
            pos += 40 + size
            n, at = read_compact_size(undo, 0)
            if n == 0:
                continue
            for block_hash, prev in wanted.get((n, read_compact_size(undo, at)[0]), []):
                if block_hash not in found and \
                        hashlib.sha256(hashlib.sha256(prev + undo).digest()).digest() == checksum:
                    found[block_hash] = undo.hex()
                    remaining -= 1
    return found


def find_special_blocks(rpc, tip, step=331):
    """Heights of blocks holding special transactions: the first, middle and last hit per type."""
    found = {}
    heights = list(range(7000, tip + 1, step))
    blocks = rpc.batch([("getblock", [bh, 2]) for bh in rpc.block_hashes(heights)])
    for h, blk in zip(heights, blocks):
        for t in {tx.get("type", 0) for tx in blk["tx"]} - {0}:
            found.setdefault(t, []).append(h)
    picked = {t: sorted({hs[0], hs[len(hs) // 2], hs[-1]}) for t, hs in found.items()}
    return sorted({h for hs in picked.values() for h in hs}), {str(k): v for k, v in sorted(picked.items())}


def capture(rpc, network, blocks_dir):
    spec = SAMPLES[network]
    tip = rpc.call("getblockcount")
    info = rpc.call("getblockchaininfo")
    deployments = rpc.call("getdeploymentinfo")["deployments"]
    network_info = rpc.call("getnetworkinfo")

    def upto_tip(key):
        return [h for h in spec[key] if h <= tip]

    block_heights = sorted(set(upto_tip("blocks")))
    special_types = {}
    if network == "testnet":
        extra, special_types = find_special_blocks(rpc, tip)
        block_heights = sorted(set(block_heights) | set(extra))

    hashes = rpc.block_hashes(block_heights)
    raws = rpc.batch([("getblock", [bh, 0]) for bh in hashes])
    verbose = rpc.batch([("getblock", [bh, 2]) for bh in hashes])

    # spent outputs of every input of the sampled blocks
    prev_txids = sorted({vin["txid"] for blk in verbose for tx in blk["tx"] for vin in tx["vin"] if "txid" in vin})
    prev_txs = dict(zip(prev_txids, rpc.batch([("getrawtransaction", [t, 1]) for t in prev_txids])))
    prev_block_hashes = sorted({tx["blockhash"] for tx in prev_txs.values()})
    prev_heights = dict(zip(prev_block_hashes, (h["height"] for h in rpc.batch(
        [("getblockheader", [bh, True]) for bh in prev_block_hashes]))))

    undos = find_block_undos(blocks_dir, verbose)
    blocks = []
    for h, raw, blk in zip(block_heights, raws, verbose):
        txs = []
        for tx in blk["tx"]:
            prevouts = []
            for vin in tx["vin"]:
                if "txid" not in vin:
                    continue
                ptx = prev_txs[vin["txid"]]
                out = ptx["vout"][vin["vout"]]
                prevouts.append({
                    "script_pubkey": out["scriptPubKey"]["hex"],
                    "amount": sats(out["value"]),
                    "height": prev_heights[ptx["blockhash"]],
                    "coinbase": "coinbase" in ptx["vin"][0],
                })
            txs.append({
                "txid": tx["txid"],
                "instance_hash": tx.get("instanceHash", tx["txid"]),
                "type": tx.get("type", 0),
                "prevouts": prevouts,
            })
        blocks.append({"height": h, "hash": blk["hash"], "merkle_root": blk["merkleroot"],
                       "hex": raw, "txs": txs, "undo_hex": undos.get(blk["hash"])})

    header_ranges = []
    for first, last in spec["header_ranges"]:
        if last > tip:
            continue
        hs = rpc.block_hashes(range(first, last + 1))
        hdrs = rpc.batch([("getblockheader", [bh, False]) for bh in hs])
        header_ranges.append({"first_height": first, "hex": "".join(hdrs)})

    subsidy_heights = upto_tip("subsidy")
    stats = rpc.batch([("getblockstats", [h, ["height", "subsidy", "totalfee"]]) for h in subsidy_heights])
    prev_headers = rpc.batch([("getblockheader", [bh, True])
                              for bh in rpc.block_hashes(h - 1 for h in subsidy_heights)])
    subsidy = [{"height": s["height"], "subsidy": s["subsidy"], "total_fee": s["totalfee"],
                "prev_bits": int(p["bits"], 16)} for s, p in zip(stats, prev_headers)]

    superblock_heights = upto_tip("superblocks")
    superblocks = [{"height": h, "budget": sats(b)} for h, b in zip(
        superblock_heights, rpc.batch([("getsuperblockbudget", [h]) for h in superblock_heights]))]

    payment_heights = upto_tip("payments")
    pay = rpc.batch([("masternode", ["payments", bh, 1]) for bh in rpc.block_hashes(payment_heights)])
    pay_stats = rpc.batch([("getblockstats", [h, ["subsidy", "totalfee"]]) for h in payment_heights])
    payments = []
    for h, p, s in zip(payment_heights, pay, pay_stats):
        assert p[0]["height"] == h, p[0]["height"]
        mns = p[0]["masternodes"]
        payments.append({"height": h, "subsidy": s["subsidy"], "total_fee": s["totalfee"],
                         "mn_amount": sum(m["amount"] for m in mns), "masternodes": len(mns)})

    return {
        "provenance": {
            "generator": "dash-oracle-sys/scripts/gen-samples.py",
            "dashd_version": network_info["subversion"],
            "chain": info["chain"],
            "tip_height": tip,
            "tip_hash": info["bestblockhash"],
            "initial_block_download": info["initialblockdownload"],
        },
        "deployments": {k: v.get("height") for k, v in deployments.items()},
        "special_tx_blocks": special_types,
        "blocks": blocks,
        "header_ranges": header_ranges,
        "subsidy": subsidy,
        "superblocks": superblocks,
        "payments": payments,
    }


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--network", choices=sorted(SAMPLES), required=True)
    ap.add_argument("--url", help="RPC URL (default: the BS2 oracle)")
    ap.add_argument("--cookie", help="RPC cookie file (default: the BS2 oracle's)")
    ap.add_argument("--blocks-dir", help="the node's blocks/ directory (default: the BS2 oracle's)")
    ap.add_argument("--out", required=True)
    args = ap.parse_args()
    url = args.url or f"http://127.0.0.1:{PORTS[args.network]}/"
    cookie = args.cookie or f"/work/chains/{args.network}/rpc.cookie"
    blocks_dir = args.blocks_dir or os.path.join(
        "/work/chains", args.network, "data", "testnet3" if args.network == "testnet" else "", "blocks")
    data = capture(Rpc(url, cookie), args.network, blocks_dir)
    with open(args.out, "w") as f:
        json.dump(data, f, indent=1, sort_keys=True)
        f.write("\n")
    p = data["provenance"]
    print(f"{args.out}: {p['chain']} tip {p['tip_height']} ({p['dashd_version']}), "
          f"{len(data['blocks'])} blocks ({sum(b['undo_hex'] is not None for b in data['blocks'])} with undo), "
          f"{len(data['header_ranges'])} header ranges", file=sys.stderr)


if __name__ == "__main__":
    main()
