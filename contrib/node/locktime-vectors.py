#!/usr/bin/env python3
"""Records Core's verdicts on relative and absolute lock times on a regtest chain, for
dash-consensus's BIP68/112/113 replay (CS5b).

Starts a release dashd on a fresh regtest datadir with a mock clock (one block every 600 s, blocks
100 and 114 8 s and 313 s late so that time locks end exactly at the cutoff and one second before
it), mines to OP_TRUE, and funds coins of known heights: P2SH(OP_TRUE) outputs at two heights, and
P2SH outputs of `<n> OP_CHECKSEQUENCEVERIFY OP_DROP OP_TRUE` for several `n`. At the tip it
builds transactions around each boundary (height and time locks met exactly and one unit short,
disabled locks, version 1, two inputs; lock times one below and at the cutoff; CSV operands
against the spending sequence) and records two verdicts for each, judged by Core for the block
after the tip:

- `testmempoolaccept` (`CheckFinalTxAtTip`, `CheckSequenceLocksAtTip`, then the scripts);
- `generateblock` with `submit` off (`TestBlockValidity`: `ContextualCheckBlock`'s
  finality, then `ConnectBlock`'s sequence locks and scripts).

Neither changes the chain, so every case spends the same coins. The output holds the chain's
headers, so the replay rebuilds the median times past Core read.

Usage:
  locktime-vectors.py --dashd <path to dashd> --framework <Core's test/functional dir>
                      --datadir <empty dir> --port <p2p port> --rpcport <rpc port> --out <json>

`--framework` is Core's `test/functional` directory at the release's tag. The framework imports
the `dash_hash` extension, which this script never needs; a module of that name on PYTHONPATH
whose `getPoWHash` raises is enough.
"""

import argparse
import json
import os
import subprocess
import sys
import time


def parse_args():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--dashd", required=True)
    p.add_argument("--framework", required=True)
    p.add_argument("--datadir", required=True)
    p.add_argument("--port", type=int, required=True)
    p.add_argument("--rpcport", type=int, required=True)
    p.add_argument("--out", required=True)
    return p.parse_args()


args = parse_args()
sys.path.insert(0, os.path.abspath(args.framework))

from test_framework.authproxy import AuthServiceProxy, JSONRPCException  # noqa: E402
from test_framework.messages import COIN, COutPoint, CTransaction, CTxIn, CTxOut  # noqa: E402
from test_framework.script import (  # noqa: E402
    OP_CHECKSEQUENCEVERIFY, OP_DROP, OP_EQUAL, OP_HASH160, OP_TRUE, CScript, hash160,
)

DISABLE = 1 << 31
TYPE = 1 << 22
START_TIME = 1_700_000_000
SPACING = 600
FEE = 10_000

# The heights the chain below reaches: coin `a` in block 106, coin `b` in block 120, the tip at
# 140. A time lock on a coin runs from the median time past of the block before the coin's
# (the time of coin height - 6) and is judged against the tip's (the time of 135); it ends at
# `start + 512 * units - 1`. Delaying the two start blocks puts the gap at 0 (mod 512) for `a`,
# so its longest met lock ends one second before the cutoff, and at 511 for `b`, so its shortest
# unmet lock ends exactly at the cutoff.
A_HEIGHT, B_HEIGHT, TIP = 106, 120, 140


def delay(coin_height, residue):
    return (SPACING * ((TIP - 5) - (coin_height - 6)) - residue) % 512


DELAYS = {A_HEIGHT - 6: delay(A_HEIGHT, 0), B_HEIGHT - 6: delay(B_HEIGHT, 511)}


def start():
    os.makedirs(args.datadir, exist_ok=True)
    conf = os.path.join(args.datadir, "dash.conf")
    with open(conf, "w") as f:
        f.write("regtest=1\n[regtest]\n")
        f.write(f"port={args.port}\nrpcport={args.rpcport}\nbind=127.0.0.1\nrpcbind=127.0.0.1\nrpcallowip=127.0.0.1\n")
        f.write("listenonion=0\ndiscover=0\ndnsseed=0\n")
        f.write("server=1\ndisablewallet=1\nprinttoconsole=0\nacceptnonstdtxn=1\n")
    proc = subprocess.Popen([args.dashd, f"-datadir={args.datadir}", f"-conf={conf}"],
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    cookie = os.path.join(args.datadir, "regtest", ".cookie")
    for _ in range(600):
        if proc.poll() is not None:
            sys.exit(f"dashd exited with {proc.returncode}")
        if os.path.exists(cookie):
            try:
                user, password = open(cookie).read().strip().split(":", 1)
                rpc = AuthServiceProxy(f"http://{user}:{password}@127.0.0.1:{args.rpcport}", timeout=600)
                rpc.getblockcount()
                return proc, rpc
            except Exception:
                pass
        time.sleep(0.5)
    proc.kill()
    sys.exit("dashd did not start")


def p2sh(redeem):
    return CScript([OP_HASH160, hash160(redeem), OP_EQUAL])


def csv_redeem(n):
    return CScript([n, OP_CHECKSEQUENCEVERIFY, OP_DROP, OP_TRUE])


class Chain:
    def __init__(self, rpc):
        self.rpc = rpc
        self.desc = rpc.getdescriptorinfo("raw(51)")["descriptor"]

    def clock(self, height):
        self.rpc.setmocktime(START_TIME + SPACING * height + DELAYS.get(height, 0))

    def mine(self, n, txs=None):
        for _ in range(n):
            self.clock(self.rpc.getblockcount() + 1)
            if txs:
                self.rpc.generateblock(self.desc, txs)
                txs = None
            else:
                self.rpc.generatetodescriptor(1, self.desc)

    def coinbase(self, height):
        cb = self.rpc.getblock(self.rpc.getblockhash(height), 2)["tx"][0]
        n = next(o["n"] for o in cb["vout"] if o["scriptPubKey"]["hex"] == "51")
        return COutPoint(int(cb["txid"], 16), n), int(round(cb["vout"][n]["value"] * COIN))

    def mtp(self, height):
        return self.rpc.getblockheader(self.rpc.getblockhash(height))["mediantime"]


def fund(chain, coinbase_height, scripts):
    """Mines a transaction paying `scripts` from a mature coinbase; returns its outpoints."""
    outpoint, value = chain.coinbase(coinbase_height)
    tx = CTransaction()
    tx.vin.append(CTxIn(outpoint, b""))
    share = (value - FEE) // len(scripts)
    for spk in scripts:
        tx.vout.append(CTxOut(share, spk))
    tx.rehash()
    chain.mine(1, [tx.serialize().hex()])
    height = chain.rpc.getblockcount()
    return [(COutPoint(tx.sha256, i), share, spk, height) for i, spk in enumerate(scripts)]


def spend(coins, version=2, lock_time=0):
    """A transaction spending `coins` (each `(coin, redeem, sequence)`) to one P2SH(OP_TRUE)."""
    tx = CTransaction()
    tx.nVersion = version
    tx.nLockTime = lock_time
    total = 0
    for (outpoint, value, _, _), redeem, sequence in coins:
        tx.vin.append(CTxIn(outpoint, CScript([bytes(redeem)]), sequence))
        total += value
    tx.vout.append(CTxOut(total - FEE, p2sh(CScript([OP_TRUE]))))
    tx.rehash()
    return tx


def verdicts(chain, tx):
    hex_tx = tx.serialize().hex()
    mempool = chain.rpc.testmempoolaccept([hex_tx])[0]
    try:
        chain.rpc.generateblock(chain.desc, [hex_tx], False)
        block = {"valid": True, "reason": None}
    except JSONRPCException as e:
        block = {"valid": False, "reason": e.error["message"]}
    return {
        "mempool": {"allowed": mempool["allowed"], "reason": mempool.get("reject-reason")},
        "block": block,
    }


def record(rpc):
    chain = Chain(rpc)
    chain.mine(105)

    op_true = CScript([OP_TRUE])
    csv_operands = [10, TYPE | 5, DISABLE, -1, (0x3f << 16) | 10]
    coins = fund(chain, 1, [p2sh(op_true)] + [p2sh(csv_redeem(n)) for n in csv_operands])
    a, csv = coins[0], dict(zip(csv_operands, coins[1:]))
    chain.mine(13)
    b = fund(chain, 2, [p2sh(op_true)])[0]
    chain.mine(20)

    tip = rpc.getblockcount()
    assert (a[3], b[3], tip) == (A_HEIGHT, B_HEIGHT, TIP)
    block = tip + 1
    chain.clock(block)
    mtp = chain.mtp(tip)
    depth_a, depth_b = block - a[3], block - b[3]
    # The most 512-second units a time lock on each coin can ask for at this block: for `a` it
    # ends one second before the cutoff, and one more unit for `b` ends at it.
    gap_a, gap_b = mtp - chain.mtp(a[3] - 1), mtp - chain.mtp(b[3] - 1)
    assert (gap_a % 512, gap_b % 512) == (0, 511)
    units_a, units_b = gap_a // 512, gap_b // 512

    def plain(coin, sequence):
        return (coin, op_true, sequence)

    def csv_spend(n, sequence):
        return (csv[n], csv_redeem(n), sequence)

    cases = [
        ("height lock met exactly", [plain(a, depth_a)], {}),
        ("height lock one block short", [plain(a, depth_a + 1)], {}),
        ("height lock of zero", [plain(a, 0)], {}),
        ("height lock under version 1", [plain(a, depth_a + 1)], {"version": 1}),
        ("height lock under version 3", [plain(a, depth_a + 1)], {"version": 3}),
        ("height lock disabled", [plain(a, DISABLE | (depth_a + 1))], {}),
        ("height lock with unused bits set, met", [plain(a, (0x3f << 16) | depth_a)], {}),
        ("height lock with unused bits set, short", [plain(a, (0x3f << 16) | (depth_a + 1))], {}),
        ("two inputs, both met", [plain(a, depth_a), plain(b, depth_b)], {}),
        ("two inputs, the later coin one block short", [plain(a, depth_a), plain(b, depth_b + 1)], {}),
        ("time lock met exactly, ending one second before the cutoff", [plain(a, TYPE | units_a)], {}),
        ("time lock one unit short", [plain(a, TYPE | (units_a + 1))], {}),
        ("time lock under version 1", [plain(a, TYPE | (units_a + 1))], {"version": 1}),
        ("time lock on the later coin, met", [plain(b, TYPE | units_b)], {}),
        ("time lock on the later coin ending at the cutoff", [plain(b, TYPE | (units_b + 1))], {}),
        ("height met, time short", [plain(a, depth_a), plain(b, TYPE | (units_b + 1))], {}),
        ("lock time below the next height", [plain(a, 0xfffffffe)], {"lock_time": tip}),
        ("lock time at the next height", [plain(a, 0xfffffffe)], {"lock_time": block}),
        ("lock time just below the median time past", [plain(a, 0xfffffffe)], {"lock_time": mtp - 1}),
        ("lock time at the median time past", [plain(a, 0xfffffffe)], {"lock_time": mtp}),
        ("lock time at the median time past, final sequence", [plain(a, 0xffffffff)], {"lock_time": mtp}),
        ("csv height operand met", [csv_spend(10, 10)], {}),
        ("csv height operand above the sequence", [csv_spend(10, 9)], {}),
        ("csv height operand, sequence disabled", [csv_spend(10, DISABLE | 10)], {}),
        ("csv height operand under version 1", [csv_spend(10, 10)], {"version": 1}),
        ("csv height operand, time sequence", [csv_spend(10, TYPE | 10)], {}),
        ("csv height operand met, height lock short", [csv_spend(10, depth_a + 1)], {}),
        ("csv time operand met", [csv_spend(TYPE | 5, TYPE | 5)], {}),
        ("csv time operand, height sequence", [csv_spend(TYPE | 5, 5)], {}),
        ("csv time operand above the sequence", [csv_spend(TYPE | 5, TYPE | 4)], {}),
        ("csv operand disabled", [csv_spend(DISABLE, 0)], {}),
        ("csv operand disabled under version 1", [csv_spend(DISABLE, 0)], {"version": 1}),
        ("csv negative operand", [csv_spend(-1, 0)], {}),
        ("csv operand with unused bits set", [csv_spend((0x3f << 16) | 10, 10)], {}),
    ]
    out_cases = []
    for name, inputs, fields in cases:
        tx = spend(inputs, **fields)
        out_cases.append({
            "name": name,
            "tx": tx.serialize().hex(),
            "prev_heights": [coin[3] for coin, _, _ in inputs],
            "spent": [{"amount": coin[1], "script": coin[2].hex()} for coin, _, _ in inputs],
            **verdicts(chain, tx),
        })

    headers = [rpc.getblockheader(rpc.getblockhash(h), False) for h in range(tip + 1)]
    return {
        "provenance": (
            f"Dash Core {rpc.getnetworkinfo()['subversion']} regtest, recorded by "
            "contrib/node/locktime-vectors.py; see tests/data/PROVENANCE.md"
        ),
        "headers": headers,
        "cases": out_cases,
    }


def main():
    proc, rpc = start()
    try:
        data = record(rpc)
    finally:
        try:
            rpc.stop()
        except Exception:
            pass
        proc.wait(timeout=300)
    with open(args.out, "w") as f:
        json.dump(data, f, indent=1)
        f.write("\n")
    rejected = sum(not c["mempool"]["allowed"] for c in data["cases"])
    print(f"{len(data['cases'])} cases ({rejected} rejected), {len(data['headers'])} headers")


if __name__ == "__main__":
    main()
