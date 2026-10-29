#!/usr/bin/env python3
"""Capture regtest and devnet smoke-test samples from a dashd release binary.

The mainnet/testnet samples (gen-samples.py) come from the long-running reference
nodes. This script covers the chain contexts those cannot: it starts its own dashd
on a fresh datadir, mines a chain with it, and records what that dashd computed:

- every header (its nBits is Core's GetNextWorkRequired() for that block; Core
  requires equality outside mainnet),
- getblockstats subsidies with the previous block's nBits,
- getsuperblockbudget at superblock heights,
- deployment heights (getdeploymentinfo, or getblockchaininfo on v23).

Scenarios:
  regtest          default regtest parameters
  devnet-spacing   devnet with -powtargetspacing=86401 (above nPowTargetTimespan),
                   -minimumdifficultyblocks=4001 and a high-subsidy window.
                   Past 4001 each segment mines one block more than 2 hours after the
                   previous one (min-difficulty: powLimit), then records the block
                   template dashd builds a few seconds to ~2 hours later: its bits are
                   DarkGravityWave's answer. (DGW's averaging overflows arith_uint256 on
                   powLimit-sized targets, so those blocks cannot be mined; the template
                   still reports Core's result.) Without the latter Core's
                   GetNextWorkRequiredBTC divides by DifficultyAdjustmentInterval() == 0
                   at heights below nPowKGWHeight. Past 4001 the chain is mined in
                   segments: each starts more than 2 hours after the previous block
                   (the min-difficulty rule) and continues with DGW blocks.

  regtest-p2sh     default regtest with a (legacy) wallet: transactions spending P2SH
                   multisig and P2PKH outputs, each with the sigop count dashd's block
                   template reports for it (GetTransactionSigOpCount() with the standard
                   flags, P2SH included) and the outputs it spends.

Usage:
  gen-chain-samples.py --dashd PATH --scenario regtest --workdir DIR --out FILE

Only the given dashd is run, on 127.0.0.1 with ports derived from --port-base, and
it is stopped before the script exits. Nothing is hand-written.
"""

import argparse
import base64
import json
import os
import shutil
import subprocess
import sys
import time
import urllib.request
from decimal import Decimal

DEVNET_NAME = "o1spacing"


class Node:
    def __init__(self, dashd, datadir, chain_args, rpc_port, p2p_port):
        self.dashd, self.datadir, self.chain_args = dashd, datadir, chain_args
        self.rpc_port, self.p2p_port = rpc_port, p2p_port
        self.proc = None

    def start(self, extra=(), wallet=False):
        args = [self.dashd, f"-datadir={self.datadir}", f"-rpcport={self.rpc_port}", f"-port={self.p2p_port}",
                "-server=1", "-listen=0",
                "-connect=0", "-dnsseed=0", "-fixedseeds=0", "-upnp=0", "-natpmp=0", f"-disablewallet={int(not wallet)}",
                "-printtoconsole=0", *self.chain_args, *extra]
        self.proc = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        for _ in range(1500):  # up to 5 minutes on a loaded machine
            if self.proc.poll() is not None:
                raise RuntimeError(f"dashd exited with {self.proc.returncode}: {self.proc.stderr.read().decode()}")
            try:
                self.call("getblockcount")
                return
            except Exception:
                time.sleep(0.2)
        self.proc.terminate()
        self.proc.wait(timeout=120)
        raise RuntimeError("dashd did not answer RPC")

    def cookie(self):
        sub = {"-regtest": "regtest"}.get(self.chain_args[0], f"devnet-{DEVNET_NAME}")
        with open(os.path.join(self.datadir, sub, ".cookie")) as f:
            return base64.b64encode(f.read().strip().encode()).decode()

    def call(self, method, *params):
        body = json.dumps({"jsonrpc": "1.0", "id": 0, "method": method, "params": list(params)}).encode()
        req = urllib.request.Request(f"http://127.0.0.1:{self.rpc_port}/", body,
                                     {"Authorization": "Basic " + self.cookie(), "Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(req, timeout=600) as resp:
                reply = json.load(resp, parse_float=Decimal)
        except urllib.error.HTTPError as e:
            reply = json.load(e, parse_float=Decimal)
        if reply["error"] is not None:
            raise RuntimeError(f"{method}{params}: {reply['error']}")
        return reply["result"]

    def stop(self):
        if self.proc and self.proc.poll() is None:
            try:
                self.call("stop")
            except Exception:
                self.proc.terminate()
            self.proc.wait(timeout=120)


def sat(amount):
    return int(Decimal(amount) * 100_000_000)


def mine(node, n):
    # raw(51): OP_TRUE outputs; no wallet needed
    return node.call("generatetodescriptor", n, "raw(51)")


def header(node, height):
    return node.call("getblockheader", node.call("getblockhash", height), False)


def capture_p2sh(node):
    """Spend P2SH multisig outputs and return each mempool transaction's template sigops."""
    # legacy (descriptors=false, load_on_startup=false): addmultisigaddress needs it
    node.call("createwallet", "w", False, False, "", False, False, False)
    addr = node.call("getnewaddress")
    node.call("generatetoaddress", 110, addr)
    pubkeys = [node.call("getaddressinfo", node.call("getnewaddress"))["pubkey"] for _ in range(4)]
    p2sh = {
        "2-of-3": node.call("addmultisigaddress", 2, pubkeys[:3])["address"],
        "1-of-2": node.call("addmultisigaddress", 1, pubkeys[2:4])["address"],
        "3-of-4": node.call("addmultisigaddress", 3, pubkeys)["address"],
    }
    funding = {name: node.call("sendtoaddress", a, 10) for name, a in p2sh.items()}
    funding["p2pkh"] = node.call("sendtoaddress", node.call("getnewaddress"), 10)
    node.call("generatetoaddress", 1, addr)

    def outpoint(txid):
        tx = node.call("getrawtransaction", txid, True)
        n = next(o["n"] for o in tx["vout"] if Decimal(o["value"]) == 10)
        return {"txid": txid, "vout": n}

    spends = [["2-of-3"], ["1-of-2", "p2pkh"], ["3-of-4"]]
    for names in spends:
        inputs = [outpoint(funding[n]) for n in names]
        raw = node.call("createrawtransaction", inputs, {node.call("getnewaddress"): 5 * len(names),
                                                       p2sh["2-of-3"]: str(Decimal(5 * len(names)) - Decimal("0.001"))})
        signed = node.call("signrawtransactionwithwallet", raw)
        assert signed["complete"], signed
        node.call("sendrawtransaction", signed["hex"])
    template = node.call("getblocktemplate")
    txs = []
    for t in template["transactions"]:
        decoded = node.call("decoderawtransaction", t["data"])
        spent = []
        for i in decoded["vin"]:
            prev = node.call("getrawtransaction", i["txid"], True)["vout"][i["vout"]]
            spent.append({"amount": sat(prev["value"]), "script_pubkey": prev["scriptPubKey"]["hex"]})
        txs.append({"hex": t["data"], "sigops": t["sigops"], "spent": spent})
    return {"transactions": txs}


def capture(node, superblock_cycle, superblock_start, first_header, subsidy_heights):
    tip = node.call("getblockcount")
    headers = "".join(header(node, h) for h in range(first_header, tip + 1))
    subsidy = []
    for h in subsidy_heights:
        stats = node.call("getblockstats", h, ["subsidy", "totalfee"])
        prev = node.call("getblockheader", node.call("getblockhash", h - 1))
        subsidy.append({"height": h, "subsidy": stats["subsidy"], "prev_bits": int(prev["bits"], 16)})
    # getsuperblockbudget computes the budget of any height from the chain parameters
    superblocks = []
    for sb in range(superblock_start, superblock_start + 12 * superblock_cycle, superblock_cycle):
        superblocks.append({"height": sb, "budget": sat(node.call("getsuperblockbudget", sb))})
    # a non-superblock height has budget 0
    superblocks.append({"height": superblock_start + 1, "budget": sat(node.call("getsuperblockbudget", superblock_start + 1))})
    try:
        forks = node.call("getdeploymentinfo")["deployments"]
    except RuntimeError:  # v23 reports them in getblockchaininfo
        forks = node.call("getblockchaininfo")["softforks"]
    deployments = {name: d.get("height") for name, d in forks.items()}
    return {
        "tip": tip,
        "header_ranges": [{"first_height": first_header, "hex": headers}],
        "subsidy": subsidy,
        "superblocks": superblocks,
        "deployments": deployments,
    }


def run(args):
    workdir = os.path.abspath(args.workdir)
    if os.path.exists(workdir):
        shutil.rmtree(workdir)
    os.makedirs(workdir)
    rpc_port, p2p_port = args.port_base, args.port_base + 1

    if args.scenario == "regtest-p2sh":
        node = Node(args.dashd, workdir, ["-regtest", "-txindex=1"], rpc_port, p2p_port)
        node.start(wallet=True)
        try:
            version = node.call("getnetworkinfo")["subversion"]
            data = capture_p2sh(node)
        finally:
            node.stop()
        chain = {"name": "regtest", "args": []}
    elif args.scenario == "regtest":
        node = Node(args.dashd, workdir, ["-regtest"], rpc_port, p2p_port)
        node.start()
        try:
            version = node.call("getnetworkinfo")["subversion"]
            mine(node, 600)
            # regtest: superblocks every 20 blocks from 1500 (CSuperblock::IsValidBlockHeight)
            data = capture(node, 20, 1500, 0, [1, 2, 149, 150, 151, 300, 450, 451, 599, 600])
        finally:
            node.stop()
        chain = {"name": "regtest", "args": []}
    else:
        # -highsubsidyblocks/-highsubsidyfactor: the devnet genesis block (height 1) pays
        # 50 DASH, more than a default devnet allows at that height.
        chain_args = [f"-devnet={DEVNET_NAME}", "-powtargetspacing=86401", "-minimumdifficultyblocks=4001",
                      "-highsubsidyblocks=500", "-highsubsidyfactor=10"]
        node = Node(args.dashd, workdir, chain_args, rpc_port, p2p_port)
        node.start()
        try:
            version = node.call("getnetworkinfo")["subversion"]
            # Block 1 is the devnet genesis block, which dashd connects after startup.
            for _ in range(600):
                if node.call("getblockcount") >= 1:
                    break
                time.sleep(0.2)
            mine(node, 4001 - node.call("getblockcount"))
            last_time = node.call("getblockheader", node.call("getbestblockhash"))["time"]
        finally:
            node.stop()
        templates = []
        for k in range(args.segments):
            node.start([f"-mocktime={last_time + 2 * 60 * 60 + 1}"])
            try:
                mine(node, 1)
                last_time = node.call("getblockheader", node.call("getbestblockhash"))["time"]
            finally:
                node.stop()
            # spread over (0, 2h]: every one of these is a DGW block
            node.start([f"-mocktime={last_time + 1 + (k * 7199) // max(1, args.segments - 1)}"])
            try:
                t = node.call("getblocktemplate")
                templates.append({"height": t["height"], "time": t["curtime"], "bits": int(t["bits"], 16)})
            finally:
                node.stop()
        node.start([f"-mocktime={last_time}"])
        try:
            # devnet: superblocks every 24 blocks from 4200
            data = capture(node, 24, 4200, 3950, [2, 3, 499, 500, 501, 1000, 4001, 4002, 4003])
        finally:
            node.stop()
        data["templates"] = templates
        chain = {"name": "devnet", "args": chain_args}

    data["provenance"] = {
        "generator": "dash-oracle-sys/scripts/gen-chain-samples.py",
        "scenario": args.scenario,
        "dashd": version,
        "chain": chain,
    }
    with open(args.out, "w") as f:
        json.dump(data, f, indent=1, sort_keys=True, default=str)
        f.write("\n")
    shutil.rmtree(workdir)
    print(f"{args.out}: {version}, {args.scenario}", file=sys.stderr)


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--dashd", required=True)
    p.add_argument("--scenario", choices=["regtest", "regtest-p2sh", "devnet-spacing"], required=True)
    p.add_argument("--workdir", required=True, help="datadir to create (deleted afterwards)")
    p.add_argument("--out", required=True)
    p.add_argument("--port-base", type=int, default=41100)
    p.add_argument("--segments", type=int, default=12)
    run(p.parse_args())


if __name__ == "__main__":
    main()
