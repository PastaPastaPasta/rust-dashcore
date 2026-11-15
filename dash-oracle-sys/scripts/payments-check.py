#!/usr/bin/env python3
"""Check oracle_block_value_payee and oracle_credit_pool_step against mined blocks
(node-program O2a-GV).

Reads, from a reference dashd over its RPC, only calls that are pure reads, one at a time:
getblockhash, getblock (verbosity 0 and 2), getblockheader, getblockstats (totalfee),
`protx listdiff` and getcreditpoolinfo. getcreditpoolinfo builds the pool through
CCreditPoolManager::GetCreditPool(), whose only write, the snapshot, happens inside a
block-scoped evodb transaction, which an RPC never opens (evo/creditpool.cpp:140-150).

payments: for each sampled height h, the parent's header and masternode list (`protx listdiff
1 <h - 1>`, re-encoded as dml-listdiff-check.py does), block h, its fees and, for a
superblock height, the valid masternodes the example lets vote for the trigger it rebuilds
from the coinbase. The `payments_check` example runs the entry point in each mode (see its
docs) and compares the verdicts and expected outputs with the coinbase.

credit-pool: every block from --from to --to, with the blocks of the 576 heights below,
streamed to the `payments_check` example, which chains oracle_credit_pool_step from block to
block (the pool after each step is the next one's prev) and compares each pool with
getcreditpoolinfo. A run starts with getcreditpoolinfo's pool of --from - 1 and no unlock
indexes (the RPC prints none; mined blocks never repeat an index, so they change no verdict),
or with the empty pool at the V20 activation height.

Usage:
  payments-check.py payments --checker <target>/debug/examples/payments_check --out res.jsonl
      [--net mainnet] [--heights 1,2,3 | --plan mainnet] [--resume]
  payments-check.py credit-pool --checker <payments_check> --from 1987776 --to 2090000
      --out res.jsonl [--net mainnet]
"""

import argparse
import base64
import importlib.util
import json
import subprocess
import sys
import time
import urllib.error
import urllib.request
from decimal import Decimal
from pathlib import Path

HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("dml_listdiff_check", HERE / "dml-listdiff-check.py")
dml = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(dml)

NETWORKS = {
    "mainnet": dict(
        dml.NETWORKS["mainnet"],
        min_quorum=10,
        superblock_start=614820,
        superblock_cycle=16616,
        budget_start=328008,
        max_height=2_090_000,
    ),
    "testnet": {
        # From Core's chainparams (oracle_chainparams_json at the pin).
        "chain": "test",
        "rpc_port": 37998,
        "cookie": "/work/chains/testnet/rpc.cookie",
        "dip3_height": 7000,
        "dip3_enforcement_height": 7300,
        "v19_height": 850100,
        "v20_height": 905100,
        "min_confirmations": 1,
        "pubkey_prefix": 140,
        "script_prefix": 19,
        "llmqs": {},
        "rotated_llmqs": {},
        "min_quorum": 1,
        "superblock_start": 4200,
        "superblock_cycle": 24,
        "budget_start": 4100,
        "max_height": None,
    },
}
COIN = 100_000_000
WINDOW = 576  # CreditPoolPeriodBlocks() on mainnet and testnet
# V24 (DeploymentPos 1) is DEFINED in the live oracles' ranges (getdeploymentinfo).
DEPLOYMENTS = [[1, 0]]


class Rpc(dml.Rpc):
    """dml-listdiff-check's RPC client, with amounts parsed as Decimal."""

    def call(self, method, *params):
        for attempt in range(5):
            if self.auth is None:
                self.auth = base64.b64encode(Path(self.cookie_path).read_text().strip().encode()).decode()
            body = json.dumps({"jsonrpc": "1.0", "id": 0, "method": method, "params": list(params)}).encode()
            req = urllib.request.Request(self.url, data=body, headers={"Authorization": f"Basic {self.auth}"})
            try:
                with urllib.request.urlopen(req, timeout=600) as resp:
                    reply = json.load(resp, parse_float=Decimal)
            except urllib.error.HTTPError as e:
                if e.code == 401:
                    self.auth = None
                    continue
                reply = json.load(e, parse_float=Decimal)
            except (urllib.error.URLError, ConnectionError, TimeoutError):
                self.auth = None
                time.sleep(2 * (attempt + 1))
                continue
            if reply.get("error"):
                raise RuntimeError(f"{method}{params}: {reply['error']}")
            return reply["result"]
        raise RuntimeError(f"{method}{params}: no answer")


def open_chain(name):
    net = NETWORKS[name]
    return dml.Chain(Rpc(net["rpc_port"], net["cookie"]), net)


def sats(value):
    return int(Decimal(value) * COIN)


def is_superblock(net, h):
    return h >= net["superblock_start"] and h % net["superblock_cycle"] == 0


def plan(net):
    """The acceptance's mainnet heights: every 576th block from DIP3 activation, every
    superblock from 614,820, old-budget windows and blocks before the budgets."""
    top = net["max_height"]
    heights = set(range(net["dip3_height"], top + 1, 576))
    first_sb = -(-net["superblock_start"] // net["superblock_cycle"]) * net["superblock_cycle"]
    heights |= set(range(first_sb, top + 1, net["superblock_cycle"]))
    cycle = 16616
    for start in range(net["budget_start"] - net["budget_start"] % cycle + cycle, net["superblock_start"], cycle):
        heights |= {start, start + 50, start + 99, start + 100}  # in and just past a window
    heights |= {100_000, 200_000, 300_000, net["budget_start"] - 1}
    return sorted(heights)


def voters(mns, net):
    """Valid masternodes (by registration) whose weights first reach the funding threshold,
    max(nGovernanceMinQuorum, weighted / 10) (governance/object.cpp:964-976), as
    [collateral (internal byte order), vout, weight]."""
    valid = [mn for mn in dml.by_registration(mns) if mn["state"]["PoSeBanHeight"] == -1]
    weight = {"Regular": 1, "Evo": 4}
    weighted = sum(weight[mn["type"]] for mn in valid)
    need = max(net["min_quorum"], weighted // 10)
    out, total = [], 0
    for mn in valid:
        if total >= need:
            break
        out.append([dml.internal(mn["collateralHash"]).hex(), mn["collateralIndex"], weight[mn["type"]]])
        total += weight[mn["type"]]
    return out, weighted


def coinbase_addresses(chain, block_hash):
    verbose = chain.rpc.call("getblock", block_hash, 2)
    return [o["scriptPubKey"].get("address") for o in verbose["tx"][0]["vout"]]


def payments_sample(chain, height, deployments):
    net = chain.net
    block_hash = chain.block_hash(height)
    prev = height - 1
    mns = chain.list_json(prev)
    sample = {
        "kind": "payments",
        "chain": net["chain"],
        "height": height,
        "tip_header": chain.rpc.call("getblockheader", chain.block_hash(prev), False),
        "block": chain.rpc.call("getblock", block_hash, 0),
        "fee": chain.rpc.call("getblockstats", height, ["totalfee"])["totalfee"],
        "prev_list": dml.encode_list(mns, prev, chain).hex() if mns is not None else None,
        "deployments": deployments,
        "adjusted_time": chain.rpc.call("getblockheader", block_hash, True)["time"],
        "bls_legacy": height < net["v19_height"],
        "superblock": is_superblock(net, height),
        "enforced": height >= net["dip3_enforcement_height"],
        "voters": [],
        "addresses": [],
    }
    if sample["superblock"] and mns is not None:
        sample["voters"], sample["weighted"] = voters(mns, net)
        sample["addresses"] = coinbase_addresses(chain, block_hash)
    return sample


def run_checker(checker):
    return subprocess.Popen([checker], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)


def finish(checker, totals):
    """Close the checker, print the result counts and give the exit status."""
    checker.stdin.close()
    checker.wait()
    print(json.dumps(totals, sort_keys=True))
    return 0 if set(totals) <= {"match"} else 1


def cmd_payments(args, chain):
    net = chain.net
    if args.heights:
        heights = sorted(int(h) for h in args.heights.split(","))
    else:
        heights = plan(net)
    if net["max_height"] is not None and heights and heights[-1] > net["max_height"]:
        raise SystemExit(f"heights above {net['max_height']} are out of bounds for the live oracle")
    done = set()
    if args.resume and Path(args.out).exists():
        done = {json.loads(line)["height"] for line in Path(args.out).read_text().splitlines()}
    checker = run_checker(args.checker)
    totals = {}
    with open(args.out, "a" if args.resume else "w") as out:
        for n, height in enumerate(heights, 1):
            if height in done:
                continue
            try:
                sample = payments_sample(chain, height, args.deployments)
            except Exception as e:  # recorded, never silently skipped
                record = {"height": height, "result": "sample-error", "error": str(e)}
            else:
                checker.stdin.write(json.dumps(sample) + "\n")
                checker.stdin.flush()
                record = json.loads(checker.stdout.readline())
            out.write(json.dumps(record) + "\n")
            out.flush()
            totals[record["result"]] = totals.get(record["result"], 0) + 1
            if n % 50 == 0 or record["result"] != "match":
                print(f"[{n}/{len(heights)}] height {height}: {totals}", file=sys.stderr, flush=True)
    return finish(checker, totals)


def pool_of(info):
    return {
        "locked": sats(info["balance"]),
        "current_limit": sats(info["currentlimit"]),
        "lately_unlocked": sats(info["window"]["unlocked"]),
    }


def cmd_credit_pool(args, chain):
    net = chain.net
    if net["max_height"] is not None and args.to > net["max_height"]:
        raise SystemExit(f"--to above {net['max_height']} is out of bounds for the live oracle")
    start = args.start
    prev = None if start == net["v20_height"] else pool_of(chain.rpc.call("getcreditpoolinfo", start - 1))
    checker = run_checker(args.checker)
    totals = {}

    def send(line):
        checker.stdin.write(json.dumps(line) + "\n")
        checker.stdin.flush()

    send({"kind": "credit-start", "chain": net["chain"], "height": start, "prev": prev,
          "deployments": args.deployments, "bls_legacy": False})
    for h in range(start - WINDOW, start):
        send({"kind": "credit-prime", "height": h, "block": chain.rpc.call("getblock", chain.block_hash(h), 0)})
    with open(args.out, "w") as out:
        for h in range(start, args.to + 1):
            block_hash = chain.rpc.call("getblockhash", h)
            send({"kind": "credit-step", "height": h, "block": chain.rpc.call("getblock", block_hash, 0),
                  "expected": pool_of(chain.rpc.call("getcreditpoolinfo", h))})
            record = json.loads(checker.stdout.readline())
            totals[record["result"]] = totals.get(record["result"], 0) + 1
            if record["result"] != "match" or h % 1000 == 0 or h == args.to:
                out.write(json.dumps(record) + "\n")
                out.flush()
            if h % 5000 == 0 or record["result"] != "match":
                print(f"height {h}: {totals}", file=sys.stderr, flush=True)
    return finish(checker, totals)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    for name in ("payments", "credit-pool"):
        p = sub.add_parser(name)
        p.add_argument("--net", default="mainnet", choices=sorted(NETWORKS))
        p.add_argument("--checker", required=True, help="the payments_check example binary")
        p.add_argument("--out", required=True)
        p.add_argument("--deployments", type=json.loads, default=DEPLOYMENTS)
    sub.choices["payments"].add_argument("--heights", help="comma-separated heights (default: the plan)")
    sub.choices["payments"].add_argument("--resume", action="store_true")
    sub.choices["credit-pool"].add_argument("--from", dest="start", type=int, required=True)
    sub.choices["credit-pool"].add_argument("--to", type=int, required=True)
    args = ap.parse_args()
    chain = open_chain(args.net)
    return cmd_payments(args, chain) if args.cmd == "payments" else cmd_credit_pool(args, chain)


if __name__ == "__main__":
    sys.exit(main())
