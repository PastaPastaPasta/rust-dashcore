#!/usr/bin/env python3
"""The inputs of node-l1's `versionbits_chain` example (CS4's real-chain check), from the testnet
reference node under /work/chains, pure reads only, concurrency 1 (getblockchaininfo,
getblockhash, getblockheader, getblock 2).

1. Extends CS2's header dump (contrib/node/cs2-diff/dump_headers.py testnet ... --until
   1568459: heights 0..=1568459, 80-byte records) to the node's block tip (or <tip>) with
   getblockhash + getblockheader <hash> false, checking each header's hashPrevBlock against the
   previous height's getblockhash.
2. Scans every block whose time is at most 2 h before v24's start time (1790208000) for MNHF
   signal transactions (type 7) and records (height, versionBit). Core's IsValidMNActivation
   accepts a bit-13/14 signal mined before its deployment's start too (as an unknown fork bit),
   but such a signal's MTP is below the start, so GetSignalsStage drops it from its next block
   on and it never decides a state answer; these are every signal that can.

Usage: cs4-versionbits-chain.py <cs2 testnet.bin> <out headers.bin> <out signals.json> [<tip>]
"""
import base64
import http.client
import json
import shutil
import sys
import time

START = 1790208000
src, out, sig_out = sys.argv[1:4]
until = int(sys.argv[4]) if len(sys.argv) > 4 else None


class Rpc:
    def __init__(self):
        self.conn = None

    def call(self, method, *params):
        body = json.dumps({"jsonrpc": "1.0", "id": 0, "method": method, "params": list(params)})
        for _ in range(600):
            try:
                if self.conn is None:
                    cookie = open("/work/chains/testnet/rpc.cookie").read().strip()
                    self.auth = "Basic " + base64.b64encode(cookie.encode()).decode()
                    self.conn = http.client.HTTPConnection("127.0.0.1", 37998, timeout=600)
                self.conn.request("POST", "/", body, {"Authorization": self.auth, "Content-Type": "text/plain"})
                res = json.loads(self.conn.getresponse().read())
                break
            except Exception:
                self.conn = None
                time.sleep(2)
        else:
            raise RuntimeError("oracle unreachable")
        if res["error"]:
            raise RuntimeError(res["error"])
        return res["result"]


rpc = Rpc()
shutil.copyfile(src, out)
with open(out, "rb") as f:
    data = f.read()
assert len(data) % 80 == 0
n = len(data) // 80
dump_tip = n - 1
tip = rpc.call("getblockchaininfo")["blocks"] if until is None else until
print(f"dump 0..={dump_tip}, node block tip {tip}", flush=True)
assert data[-80:] == bytes.fromhex(rpc.call("getblockheader", rpc.call("getblockhash", dump_tip), False))
prev_hash = rpc.call("getblockhash", dump_tip)
times = {}
with open(out, "ab") as f:
    for h in range(dump_tip + 1, tip + 1):
        bh = rpc.call("getblockhash", h)
        raw = bytes.fromhex(rpc.call("getblockheader", bh, False))
        assert raw[4:36][::-1].hex() == prev_hash, f"link at {h}"
        f.write(raw)
        prev_hash = bh
        if h % 1000 == 0:
            print(f"header {h}", flush=True)
with open(out, "rb") as f:
    data = f.read()
assert len(data) == 80 * (tip + 1)

# the first height whose time is within 2 h of the start (times are not monotonic: margin)
first = next(h for h in range(tip + 1) if int.from_bytes(data[80 * h + 68 : 80 * h + 72], "little") >= START - 7200)
print(f"scanning blocks {first}..={tip} for MNHF signals", flush=True)
signals = []
for h in range(first, tip + 1):
    block = rpc.call("getblock", rpc.call("getblockhash", h), 2)
    for tx in block["tx"]:
        if tx.get("type") == 7:
            signals.append({"height": h, "bit": tx["mnhfTx"]["signal"]["versionBit"], "txid": tx["txid"]})
    if h % 1000 == 0:
        print(f"block {h}: {len(signals)} signals", flush=True)
json.dump({"tip": tip, "scan_from": first, "signals": signals}, open(sig_out, "w"), indent=1)
print(f"DONE tip {tip} signals {signals}", flush=True)
