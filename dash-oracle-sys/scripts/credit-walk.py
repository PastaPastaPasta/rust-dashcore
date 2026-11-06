#!/usr/bin/env python3
"""The live getcreditpoolinfo walk of EV3-2 (EV3-notes §4.3, DEC-276 A-4): dash-dml's credit
pool, chained from block to block by the `credit_walk` example, against the reference dashd's
getcreditpoolinfo at every height.

Reads, from the v24 reference nodes' RPC, only pure reads, one at a time: getblockcount,
getblockhash, getblock (verbosity 0), getcreditpoolinfo and getdeploymentinfo (once, for V24's
state). getcreditpoolinfo's only write, the pool snapshot, happens inside a block-scoped evodb
transaction, which an RPC never opens (O2a-GV D12).

Mainnet runs from V20's activation (1,987,776) to 2,090,000 at most (DEC-276); testnet from its
V20 height (905,100) to the tip at the start. Each starts from the empty pool, after the 576
window blocks below; the example checkpoints its state to <dir>/credit-walk-<net>.state every
1000 heights and at the end, so a rerun resumes. Progress and every non-match go to
<dir>/credit-walk-<net>.jsonl. When the node's tip is below the next height, or a read is slow,
the walk waits.

Usage: credit-walk.py --walker <target>/release/examples/credit_walk --dir /work/scratch/ev3/walk
           [--net mainnet --net testnet]
"""

import argparse
import importlib.util
import json
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("payments_check", HERE / "payments-check.py")
pc = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(pc)

MAINNET_CAP = 2_090_000  # DEC-276: mainnet comparisons stay below
SLOW_READ = 5.0  # seconds: a read this slow means the node is busy; back off
BACKOFF = 30


def v24_height(rpc):
    """V24's activation height when the node reports it active, else None."""
    info = rpc.call("getdeploymentinfo")["deployments"].get("v24", {})
    if info.get("active") and "height" in info:
        return int(info["height"])
    if info.get("active"):
        return int(info["bip9"]["since"])
    return None


class Reader:
    """One read at a time; waits while the node lags."""

    def __init__(self, chain, log):
        self.chain = chain
        self.log = log
        self.tip = chain.rpc.call("getblockcount")

    def call(self, method, *params):
        start = time.monotonic()
        result = self.chain.rpc.call(method, *params)
        if time.monotonic() - start > SLOW_READ:
            self.log({"event": "slow-read", "method": method, "seconds": round(time.monotonic() - start, 1)})
            time.sleep(BACKOFF)
        return result

    def wait_for(self, height):
        while self.tip < height:
            self.tip = self.call("getblockcount")
            if self.tip < height:
                self.log({"event": "lag", "tip": self.tip, "want": height})
                time.sleep(60)

    def block(self, height):
        self.wait_for(height)
        return self.call("getblock", self.call("getblockhash", height), 0)


def walk(net_name, args):
    net = pc.NETWORKS[net_name]
    chain = pc.open_chain(net_name)
    out_path = Path(args.dir) / f"credit-walk-{net_name}.jsonl"
    state = Path(args.dir) / f"credit-walk-{net_name}.state"
    out = open(out_path, "a")

    def log(record):
        record = {"net": net_name, "time": int(time.time()), **record}
        out.write(json.dumps(record) + "\n")
        out.flush()
        if record.get("event") != "progress" or record.get("height", 0) % 10_000 == 0:
            print(json.dumps(record), file=sys.stderr, flush=True)

    reader = Reader(chain, log)
    start = net["v20_height"]
    end = min(MAINNET_CAP, reader.tip) if net_name == "mainnet" else reader.tip
    v24 = v24_height(chain.rpc)
    walker = subprocess.Popen([args.walker], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1)

    def send(line):
        walker.stdin.write(line + "\n")
        walker.stdin.flush()

    def answer():
        line = walker.stdout.readline()
        if not line:
            raise SystemExit(f"{net_name}: the walker exited ({walker.wait()})")
        return line.split()

    send(" ".join(["start", net["chain"], str(start), str(state)] + ([str(v24)] if v24 is not None else [])))
    reply = answer()
    assert reply[0] == "ready", reply
    h = int(reply[1])
    log({"event": "start", "from": h, "to": end, "v24": v24})
    totals = {"match": 0, "mismatch": 0, "reject": 0}
    while h < start:
        send(f"prime {h} {reader.block(h)}")
        h += 1
    for h in range(h, end + 1):
        block = reader.block(h)
        info = reader.call("getcreditpoolinfo", h)
        assert info["height"] == h, info
        window = info["window"]
        send(" ".join(str(v) for v in (
            "step", h, block, pc.sats(info["balance"]), pc.sats(info["currentlimit"]),
            window["height"], pc.sats(window["balance"]), pc.sats(window["unlocked"]))))
        reply = answer()
        totals[reply[0]] += 1
        if reply[0] != "match":
            log({"event": reply[0], "height": h, "detail": " ".join(reply[2:])})
        if h % 1000 == 0:
            log({"event": "progress", "height": h, **totals})
    send("end")
    assert answer() == ["done"]
    walker.stdin.close()
    walker.wait()
    log({"event": "done", "to": end, **totals})
    return totals["mismatch"] + totals["reject"]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--walker", required=True, help="the credit_walk example binary")
    ap.add_argument("--dir", required=True, help="where the logs and checkpoints go")
    ap.add_argument("--net", action="append", choices=sorted(pc.NETWORKS))
    args = ap.parse_args()
    failures = 0
    for net in args.net or ["mainnet", "testnet"]:
        failures += walk(net, args)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
