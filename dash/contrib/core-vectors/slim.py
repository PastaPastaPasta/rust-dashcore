#!/usr/bin/env python3
"""Slim the raw extraction to a committed fixture: keep every transaction vector (with the parts of
Core's verbose JSON the tests assert on) and the small blocks that carry v24 payloads.

Usage: slim.py <vectors-raw.json> <out.json>
Then copy <out.json> to dash/tests/data/core_v24_regtest.json and <work-dir>/mnlistdiffs.json to
dash/tests/data/core_v24_regtest_mnlistdiff.json.
"""
import json
import sys

MAX_BLOCK = 4000


def is_v24_shape(s):
    return (s[0] in (1, 2, 3, 4) and s[1] == 3) or s[0] in (10, 11, 12) or \
        (s[0] in (8, 9) and s[1] == 2) or (s[0] == 5 and s[1] == 4)


def slim_tx(core):
    out = {k: core[k] for k in ("txid", "version", "type", "size", "locktime", "instanceHash") if k in core}
    out["vin"] = [{"coinbase": i["coinbase"]} if "coinbase" in i else {"txid": i["txid"], "vout": i["vout"]}
                  for i in core["vin"]]
    out["vout"] = [{"valueSat": o["valueSat"], "script": o["scriptPubKey"]["hex"]} for o in core["vout"]]
    for k in ("extraPayloadSize", "extraPayload", "proRegTx", "proUpServTx", "proUpRegTx", "proUpRevTx", "cbTx",
              "qcTx", "mnhfTx", "assetLockTx", "assetUnlockTx", "proDisTx", "proUpShareTx", "proUpSharedRegTx"):
        if k in core:
            out[k] = core[k]
    return out


def main():
    raw = json.load(open(sys.argv[1]))
    txs = [{"source": t["source"], "shape": t["shape"], "block_hash": t["block_hash"], "hex": t["hex"],
            "core": slim_tx(t["core"]), **({"signers": t["signers"]} if t["signers"] else {})}
           for t in raw["transactions"]]
    blocks = []
    for b in raw["blocks"]:
        shapes = [t["shape"] for t in raw["transactions"] if t["block_hash"] == b["hash"]]
        if len(b["hex"]) // 2 <= MAX_BLOCK and (b["cbtx_version"] == 4 or any(map(is_v24_shape, shapes))):
            blocks.append({k: b[k] for k in ("source", "height", "hash", "merkleroot", "txids", "hex")})
    json.dump({"generator": raw["generator"], "transactions": txs, "blocks": blocks}, open(sys.argv[2], "w"),
              indent=1)
    print(f"{len(txs)} txs, {len(blocks)} blocks", file=sys.stderr)


if __name__ == "__main__":
    main()
