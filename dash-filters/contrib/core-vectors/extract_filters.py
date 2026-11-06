#!/usr/bin/env python3
"""Core's basic block filters for regtest blocks that hold special transactions (SB9).

Dash Core's own functional tests build regtest chains with every special-transaction payload
(feature_asset_locks, feature_masternode_shares, feature_masternode_payout_shares,
feature_protx_version, rpc_netinfo, feature_llmq_evo, feature_evo_shares_activation; see
dash/contrib/core-vectors/run-functional-tests.sh on branch node/v24a-dashcore-wire, which keeps
every node's datadir with --nocleanup). This tool restarts node0 of such a chain, on a copy of
its datadir, with -blockfilterindex=1 and reads back, for every block that holds a transaction
of a type other than 0 (plain), 5 (coinbase) and 6 (quorum commitment, which adds no element and
is in nearly every block), and for the first SAMPLES blocks per chain of each of the other two
kinds (a quorum commitment; a plain spend):

  * the raw block (getblock <hash> 0),
  * the script of every output its non-coinbase transactions spend, in input order
    (getblock <hash> 3, vin[].prevout.scriptPubKey.hex),
  * Core's filter, its filter header and the previous block's filter header
    (getblockfilter <hash>).

Nothing is computed here: the filters are the node's. Usage:

    extract_filters.py <dashd> <ft-dir> <work-dir> <out.json> [test ...]

<ft-dir> holds one directory per functional test (the --tmpdir of its run); <work-dir> receives
the datadir copies. Stdlib only; run with `python3 -I`. Stop every node it started on exit.
"""
import base64
import hashlib
import json
import os
import shutil
import socket
import subprocess
import sys
import time
import urllib.request

KEEP_ARGS = ("dip3params", "vbparams", "llmqtest", "testactivationheight", "budgetparams", "highsubsidy")
ADDS_NOTHING = {0, 5, 6}  # normal transaction, coinbase, quorum commitment
SAMPLES = 3
USER = PASSWORD = "sb9"


def chain_args(debug_log):
    """The chain-parameter command-line args of the last dashd start in a debug.log (the tests
    pass them on the command line, not in dash.conf). The start-time -mocktime is left out: the
    tests bump mocktime over RPC, so it would predate later blocks."""
    starts, current = [], []
    for line in open(debug_log, errors="replace"):
        if "Dash Core version" in line and current:
            starts.append(current)
            current = []
        if "Command-line arg: " in line:
            name, _, value = line.split("Command-line arg: ", 1)[1].strip().partition("=")
            if name.startswith(KEEP_ARGS):
                current.append(f"-{name}={value.strip(chr(34))}")
    if current:
        starts.append(current)
    return starts[-1] if starts else []


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class Rpc:
    def __init__(self, port):
        self.url = f"http://127.0.0.1:{port}/"
        self.auth = "Basic " + base64.b64encode(f"{USER}:{PASSWORD}".encode()).decode()

    def call(self, method, *params):
        body = json.dumps({"jsonrpc": "1.0", "id": "sb9", "method": method, "params": list(params)}).encode()
        req = urllib.request.Request(self.url, body, {"Authorization": self.auth, "Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(req, timeout=600) as resp:
                reply = json.load(resp)
        except urllib.error.HTTPError as e:  # Core answers errors with HTTP 500 and a JSON body
            reply = json.load(e)
        if reply.get("error"):
            raise RuntimeError(f"{method}: {reply['error']}")
        return reply["result"]


def wait_for(fn, what, timeout=900):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            result = fn()
            if result:
                return result
        except Exception:  # noqa: BLE001 - the node is still starting
            pass
        time.sleep(1)
    sys.exit(f"timed out waiting for {what}")


def extract(dashd, src, work, test):
    node = os.path.join(work, test, "node0")
    shutil.rmtree(os.path.dirname(node), ignore_errors=True)
    os.makedirs(os.path.dirname(node))
    shutil.copytree(os.path.join(src, "node0"), node, symlinks=True)
    for stale in (".cookie", ".lock"):
        for root, _, files in os.walk(node):
            if stale in files:
                os.remove(os.path.join(root, stale))
    args = chain_args(os.path.join(node, "regtest", "debug.log"))
    port, rpcport = free_port(), free_port()
    cmd = [dashd, f"-datadir={node}", f"-port={port}", f"-rpcport={rpcport}", f"-rpcuser={USER}", f"-rpcpassword={PASSWORD}",
           "-blockfilterindex=1", "-rpcdoccheck=0", "-disablewallet=1", "-printtoconsole=0", *args]
    with open(os.path.join(work, test, "stderr"), "wb") as err:
        proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=err)
    rpc = Rpc(rpcport)
    try:
        wait_for(lambda: rpc.call("getblockcount") >= 0, f"{test}: RPC")
        tip = rpc.call("getblockcount")
        info = wait_for(lambda: (lambda i: i if i["basic block filter index"]["synced"] else None)(rpc.call("getindexinfo")),
                        f"{test}: block filter index")
        assert info["basic block filter index"]["best_block_height"] == tip, (test, info, tip)
        blocks = []
        sampled = {"quorum commitment": 0, "plain spend": 0}
        prev_header = "00" * 32
        for height in range(tip + 1):
            block_hash = rpc.call("getblockhash", height)
            filt = rpc.call("getblockfilter", block_hash)
            verbose = rpc.call("getblock", block_hash, 3)
            types = sorted({tx.get("type", 0) for tx in verbose["tx"]})
            kind = None
            if set(types) - ADDS_NOTHING:
                kind = "special"
            elif 6 in types and sampled["quorum commitment"] < SAMPLES:
                kind = "quorum commitment"
            elif any(tx.get("type", 0) == 0 and tx["vin"] and "coinbase" not in tx["vin"][0] for tx in verbose["tx"]) \
                    and sampled["plain spend"] < SAMPLES:
                kind = "plain spend"
            if kind:
                if kind in sampled:
                    sampled[kind] += 1
                scripts = [vin["prevout"]["scriptPubKey"]["hex"]
                           for tx in verbose["tx"][1:] for vin in tx["vin"]]
                blocks.append({
                    "height": height,
                    "hash": block_hash,
                    "block": rpc.call("getblock", block_hash, 0),
                    "prev_scripts": scripts,
                    "prev_header": prev_header,
                    "filter": filt["filter"],
                    "header": filt["header"],
                    "tx_types": types,
                    "kind": kind,
                })
            prev_header = filt["header"]
        return {"test": test, "tip": tip, "args": args, "blocks": blocks}
    finally:
        try:
            rpc.call("stop")
        except Exception:  # noqa: BLE001
            pass
        try:
            proc.wait(timeout=300)
        except subprocess.TimeoutExpired:
            proc.kill()


def main():
    dashd, ft, work, out, *tests = sys.argv[1:]
    version = subprocess.run([dashd, "-version"], capture_output=True, text=True, check=True).stdout.splitlines()[0]
    sha = hashlib.sha256(open(dashd, "rb").read()).hexdigest()
    tests = tests or sorted(d for d in os.listdir(ft) if os.path.isdir(os.path.join(ft, d)))
    result = {"dashd": version, "dashd_sha256": sha, "chains": []}
    for test in tests:
        chain = extract(dashd, os.path.join(ft, test), work, test)
        print(f"{test}: tip {chain['tip']}, {len(chain['blocks'])} blocks kept", file=sys.stderr)
        result["chains"].append(chain)
    with open(out, "w") as f:
        json.dump(result, f, indent=1)


if __name__ == "__main__":
    main()
