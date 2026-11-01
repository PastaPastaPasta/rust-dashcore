#!/usr/bin/env python3
"""Capture the mainnet samples of the contextual smoke tests (tests/data/contextual-mainnet.json).

Read-only RPC to the reference mainnet dashd (one request at a time; see
dml-listdiff-check.py, whose helpers this reuses). Every sample is data Core accepted
as part of the chain, so each expected verdict is "accept"; expected values the RPC
prints (protx listdiff, fees) are copied from it:

- "dip3_block": the first block after DIP0003 activation that holds a ProRegTx whose
  collateral is another transaction's output, with the context to check it: the
  headers from 30 blocks below it (or its quorum base block, if lower), the masternode
  list after its parent, the coins its ProRegTxs reference (getrawtransaction), the
  expected `protx listdiff`, and one ordinary transaction with the coins it spends
  (getblock verbosity 3) and its fee.
- "commitment": a mined quorum commitment that punishes a member, with the headers
  from its quorum base block and the quorum's members (computed as the DML check
  does; the BLS members signature only verifies over the right members).
- "proupserv": a ProUpServTx with the context to check it (the headers from 30 blocks
  below it and the masternode list after its parent), its operator key from that list,
  and the hash its BLS signature signs (SerializeHash of the payload without it).

Usage: gen-contextual-samples.py > tests/data/contextual-mainnet.json
"""

import importlib.util
import json
import sys
from pathlib import Path

spec = importlib.util.spec_from_file_location("dmlcheck", Path(__file__).with_name("dml-listdiff-check.py"))
dml = importlib.util.module_from_spec(spec)
spec.loader.exec_module(dml)

COMMITMENT_HEIGHT = 1298963  # LLMQ_50_60, 49 of 50 members valid


def block(chain, height):
    return chain.rpc.call("getblock", chain.block_hash(height), 2)


def dip3_block(chain):
    net = chain.net
    for height in range(net["dip3_height"], net["dip3_height"] + 2000):
        verbose = block(chain, height)
        external = [
            t["proRegTx"] for t in verbose["tx"] if t.get("type") == 1 and int(t["proRegTx"]["collateralHash"], 16) != 0
        ]
        if not external:
            continue
        sample = dml.build_sample(chain, height)
        first = min(sample["first_height"], height - 30)
        coins = [dml.coin_of(chain, reg["collateralHash"], reg["collateralIndex"]) for reg in external]
        special = [{"txid": t["txid"], "type": t["type"], "hex": t["hex"]} for t in verbose["tx"] if t.get("type", 0) != 0]
        full = chain.rpc.call("getblock", chain.block_hash(height), 3)
        normal = next(t for t in full["tx"] if t.get("type", 0) == 0 and "coinbase" not in t["vin"][0])
        spent = [
            {
                "txid": i["txid"],
                "vout": i["vout"],
                "height": i["prevout"]["height"],
                "coinbase": i["prevout"]["generated"],
                "amount": i["prevout"]["valueSat"] if "valueSat" in i["prevout"] else round(i["prevout"]["value"] * 1e8),
                "script": i["prevout"]["scriptPubKey"]["hex"],
            }
            for i in normal["vin"]
        ]
        return {
            "height": height,
            "block": sample["block"],
            "first_height": first,
            "headers": chain.headers_hex(first, height),
            "header": chain.rpc.call("getblockheader", chain.block_hash(height), False),
            "adjusted_time": verbose["time"],
            "prev_list": sample["prev_list"],
            "expected_listdiff": sample["expected"],
            "special_txs": special,
            "coins": coins,
            "normal_tx": {"hex": normal["hex"], "fee": round(normal["fee"] * 1e8), "coins": spent},
        }
    raise RuntimeError("no ProRegTx with an external collateral after DIP0003")


def commitment(chain):
    height = COMMITMENT_HEIGHT
    tx = next(t for t in block(chain, height)["tx"] if t.get("type") == 6)
    payload = tx["qcTx"]
    qc = payload["commitment"]
    _, interval = chain.net["llmqs"][qc["llmqType"]]
    base = dml.quorum_base_height(payload, interval)
    extra = bytes.fromhex(tx["extraPayload"])
    return {
        "height": height,
        "commitment": extra[6:].hex(),  # CFinalCommitmentTxPayload: u16 version, u32 height, commitment
        "first_height": base,
        "headers": chain.headers_hex(base, height),
        "llmq_type": qc["llmqType"],
        "quorum_hash": dml.internal(qc["quorumHash"]).hex(),
        "valid_members": qc["validMembersCount"],
        "members": dml.quorum_members(chain, qc["llmqType"], base),
    }


def proupserv(chain):
    for height in range(chain.net["dip3_height"], chain.net["dip3_height"] + 5000):
        for t in block(chain, height)["tx"]:
            if t.get("type") != 2:
                continue
            payload = bytes.fromhex(t["extraPayload"])
            protx = t["proUpServTx"]["proTxHash"]
            mns = chain.list_json(height - 1)
            mn = next(m for m in mns if m["proTxHash"] == protx)
            if mn["state"]["version"] != 1:
                continue
            tip_header = chain.rpc.call("getblockheader", chain.block_hash(height - 1), True)
            return {
                "height": height,
                "tx": t["hex"],
                "first_height": height - 30,
                "headers": chain.headers_hex(height - 30, height),
                "adjusted_time": tip_header["time"],
                "prev_list": dml.encode_list(mns, height - 1, chain).hex(),
                "payload": payload.hex(),
                "pubkey_operator": mn["state"]["pubKeyOperator"],
                "sig": payload[-96:].hex(),
                "msg": dml.dsha256(payload[:-96]).hex(),
            }
    raise RuntimeError("no ProUpServTx found")


def main():
    net = dml.NETWORKS["mainnet"]
    chain = dml.Chain(dml.Rpc(net["rpc_port"], net["cookie"]), net)
    out = {
        "provenance": "mainnet blocks accepted by Dash Core (reference dashd v24.0.0-rc.3 RPC), captured by "
        "scripts/gen-contextual-samples.py; see tests/data/PROVENANCE.md",
        "dip3_block": dip3_block(chain),
        "commitment": commitment(chain),
        "proupserv": proupserv(chain),
    }
    json.dump(out, sys.stdout, indent=1)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
