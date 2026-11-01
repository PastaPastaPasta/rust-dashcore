#!/usr/bin/env python3
"""Records Core's UTXO-set hashes on a regtest chain, for dash-consensus's MuHash3072 and
hash_serialized_2 replay (CS5b).

Starts a release dashd on a fresh regtest datadir with a mock clock, mines to OP_TRUE and, at
chosen heights, records `dumptxoutset` (Core's snapshot of the coins database, in its cursor order)
together with `gettxoutsetinfo hash_serialized_2` and `gettxoutsetinfo muhash` for the same tip.
Between the heights it builds a set the hashes treat differently: mature and immature coinbases, a
transaction with 200 outputs (two-byte indices) including zero values, an OP_RETURN (never in the
set) and a P2SH output, outputs of every script type Core's coins database compresses (P2PKH, P2SH,
and P2PK with a compressed or a valid uncompressed key of either parity) and a P2PK with an
uncompressed key off the curve (stored as is), partial spends, an output spent in the block that
creates it, and a transaction whose outputs are all spent. Transactions go straight into blocks
through `generateblock`, so only consensus applies.

Usage:
  utxo-hash-vectors.py --dashd <path to dashd> --framework <Core's test/functional dir>
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

from test_framework.authproxy import AuthServiceProxy  # noqa: E402
from test_framework.messages import COIN, COutPoint, CTransaction, CTxIn, CTxOut  # noqa: E402
from test_framework.key import ECKey  # noqa: E402
from test_framework.script import (  # noqa: E402
    OP_CHECKSIG, OP_DUP, OP_EQUAL, OP_EQUALVERIFY, OP_HASH160, OP_RETURN, OP_TRUE, CScript, hash160,
)

START_TIME = 1_700_000_000
SPACING = 600
FEE = 10_000


def start():
    os.makedirs(args.datadir, exist_ok=True)
    conf = os.path.join(args.datadir, "dash.conf")
    with open(conf, "w") as f:
        f.write("regtest=1\n[regtest]\n")
        f.write(f"port={args.port}\nrpcport={args.rpcport}\nbind=127.0.0.1\nrpcbind=127.0.0.1\nrpcallowip=127.0.0.1\n")
        f.write("listenonion=0\ndiscover=0\ndnsseed=0\n")
        f.write("server=1\ndisablewallet=1\nprinttoconsole=0\n")
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


def record(rpc):
    desc = rpc.getdescriptorinfo("raw(51)")["descriptor"]
    op_true = CScript([OP_TRUE])
    samples = []

    def mine(n, txs=None):
        for _ in range(n):
            rpc.setmocktime(START_TIME + SPACING * (rpc.getblockcount() + 1))
            if txs:
                rpc.generateblock(desc, [tx.serialize().hex() for tx in txs])
                txs = None
            else:
                rpc.generatetodescriptor(1, desc)

    def sample():
        height = rpc.getblockcount()
        path = os.path.join(args.datadir, f"utxo-{height}.dat")
        dump = rpc.dumptxoutset(path)
        with open(path, "rb") as f:
            snapshot = f.read()
        legacy = rpc.gettxoutsetinfo("hash_serialized_2")
        muhash = rpc.gettxoutsetinfo("muhash")
        assert legacy["bestblock"] == muhash["bestblock"] == dump["base_hash"]
        samples.append({
            "height": height,
            "best_block": dump["base_hash"],
            "coins": dump["coins_written"],
            "hash_serialized_2": legacy["hash_serialized_2"],
            "muhash": muhash["muhash"],
            "snapshot": snapshot.hex(),
        })

    def coinbase(height):
        cb = rpc.getblock(rpc.getblockhash(height), 2)["tx"][0]
        n = next(o["n"] for o in cb["vout"] if o["scriptPubKey"]["hex"] == op_true.hex())
        return COutPoint(int(cb["txid"], 16), n), int(round(cb["vout"][n]["value"] * COIN))

    def tx(inputs, outputs):
        t = CTransaction()
        t.vin = [CTxIn(outpoint, b"") if spk is None else CTxIn(outpoint, CScript([bytes(spk)]))
                 for outpoint, spk in inputs]
        t.vout = [CTxOut(value, spk) for value, spk in outputs]
        t.rehash()
        return t

    sample()
    mine(1)
    sample()
    mine(100)
    sample()

    # Block 102: 200 outputs (some zero, one P2SH(OP_TRUE), one OP_RETURN), and a pair.
    (cb1, v1), (cb2, v2) = coinbase(1), coinbase(2)
    p2sh = CScript([OP_HASH160, hash160(op_true), OP_EQUAL])
    share = (v1 - FEE) // 200
    wide_outputs = [(0 if i % 37 == 0 else share, p2sh if i == 3 else op_true) for i in range(200)]
    wide_outputs.append((0, CScript([OP_RETURN, b"cs5b"])))
    wide = tx([(cb1, None)], wide_outputs)
    pair = tx([(cb2, None)], [((v2 - FEE) // 2, op_true)] * 2)
    mine(1, [wide, pair])
    sample()

    # Block 103: spend some of each, one output spent in the same block.
    def out(t, n):
        return COutPoint(t.sha256, n)

    def pubkey(secret, compressed):
        key = ECKey()
        key.set(secret.to_bytes(32, "big"), compressed)
        return key.get_pubkey().get_bytes()

    # Keys of both parities (y even or odd: compressed codes 2 and 3, uncompressed 4 and 5), and
    # an uncompressed "key" off the curve, which Core's compressor leaves alone.
    def with_parity(compressed, odd):
        keys = (pubkey(n, compressed) for n in range(1, 64))
        return next(k for k in keys if (k[0] == 3 if compressed else k[64] & 1) == odd)

    off_curve = bytes([4]) + bytes(range(1, 65))
    typed = [
        CScript([OP_DUP, OP_HASH160, hash160(with_parity(True, False)), OP_EQUALVERIFY, OP_CHECKSIG]),
        p2sh,
        CScript([with_parity(True, False), OP_CHECKSIG]),
        CScript([with_parity(True, True), OP_CHECKSIG]),
        CScript([with_parity(False, False), OP_CHECKSIG]),
        CScript([with_parity(False, True), OP_CHECKSIG]),
        CScript([off_curve, OP_CHECKSIG]),
    ]
    partial = tx([(out(wide, 0), None), (out(wide, 3), op_true), (out(wide, 150), None), (out(pair, 1), None)],
                 [(share, op_true), (share, op_true)] + [(share // 8, spk) for spk in typed])
    chained = tx([(out(partial, 0), None)], [(share - FEE, op_true)])
    mine(1, [partial, chained])
    sample()

    # Block 104: the pair's last output, so its txid leaves the set; then six more blocks.
    rest = tx([(out(pair, 0), None)], [((v2 - FEE) // 2 - FEE, op_true)])
    mine(1, [rest])
    mine(6)
    sample()
    return samples


def main():
    proc, rpc = start()
    try:
        samples = record(rpc)
        version = rpc.getnetworkinfo()["subversion"]
    finally:
        try:
            rpc.stop()
        except Exception:
            pass
        proc.wait(timeout=300)
    data = {
        "provenance": (
            f"Dash Core {version} regtest, recorded by contrib/node/utxo-hash-vectors.py; "
            "see tests/data/PROVENANCE.md"
        ),
        "samples": samples,
    }
    with open(args.out, "w") as f:
        json.dump(data, f, indent=1)
        f.write("\n")
    print(", ".join(f"{s['height']}: {s['coins']} coins" for s in samples))


if __name__ == "__main__":
    main()
