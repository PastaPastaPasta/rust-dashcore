#!/usr/bin/env python3
"""Records Core-written undo records on a regtest chain, for dash-consensus's coin codec tests (CS5a).

Starts a release dashd on a fresh regtest datadir, mines to OP_TRUE, creates outputs of every
script kind the coins database treats differently (P2PKH, P2SH, compressed and uncompressed
P2PK, each with even and odd y, a hybrid key, bare multisig, bare OP_TRUE, a zero value, a script of
exactly MAX_SCRIPT_SIZE bytes) and spends them, including an output spent in the block that
creates it. Transactions go straight into blocks through `generateblock`, so policy does not
apply, only consensus. Signatures are RFC 6979, so a run is reproducible.

After stopping dashd it copies, for the chosen blocks, the raw block, Core's undo record from
rev00000.dat (with its checksum) and `getblock <hash> 3`'s prevouts, which Core reads from the
same undo record.

Usage:
  coins-undo-vectors.py --dashd <path to dashd> --framework <Core's test/functional dir>
                        --datadir <empty dir> --port <p2p port> --rpcport <rpc port> --out <json>

`--framework` is Core's `test/functional` directory at the release's tag (its test_framework
package builds and signs the transactions). The framework imports the `dash_hash` extension for
X11 block hashes, which this script never computes; a module of that name on PYTHONPATH whose
`getPoWHash` raises is enough.
"""

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import time
from decimal import Decimal


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
from test_framework.key import ECKey  # noqa: E402
from test_framework.messages import COIN, COutPoint, CTransaction, CTxIn, CTxOut  # noqa: E402
from test_framework.script import (  # noqa: E402
    OP_0, OP_1, OP_CHECKMULTISIG, OP_CHECKSIG, OP_DROP, OP_DUP, OP_EQUAL, OP_EQUALVERIFY,
    OP_HASH160, OP_RETURN, OP_TRUE, SIGHASH_ALL, CScript, SignatureHash, hash160,
)

MAX_SCRIPT_SIZE = 10_000


def sha256d(b):
    return hashlib.sha256(hashlib.sha256(b).digest()).digest()


def key(label, compressed, odd_y=None):
    """A deterministic key; with `odd_y`, the first whose y has that parity."""
    i = 0
    while True:
        k = ECKey()
        k.set(hashlib.sha256(f"cs5a {label} {i}".encode()).digest(), compressed)
        uncompressed = ECKey()
        uncompressed.set(k.get_bytes(), False)
        y_odd = uncompressed.get_pubkey().get_bytes()[64] & 1
        if odd_y is None or bool(y_odd) == odd_y:
            return k
        i += 1


def hybrid_bytes(k):
    u = ECKey()
    u.set(k.get_bytes(), False)
    raw = u.get_pubkey().get_bytes()
    return bytes([6 | (raw[64] & 1)]) + raw[1:]


def sign(tx, index, script_code, k):
    sighash, err = SignatureHash(script_code, tx, index, SIGHASH_ALL)
    assert err is None
    return k.sign_ecdsa(sighash, rfc6979=True) + bytes([SIGHASH_ALL])


def duffs(value):
    return int(Decimal(str(value)) * COIN)


def big_script():
    """Exactly MAX_SCRIPT_SIZE bytes that evaluate to true: 19 × (520-byte push, OP_DROP), a
    41-byte push, OP_DROP, OP_TRUE (20 counted opcodes, under the 201 limit)."""
    s = CScript([bytes(520), OP_DROP] * 19 + [bytes(41), OP_DROP, OP_TRUE])
    assert len(s) == MAX_SCRIPT_SIZE, len(s)
    return s


def start():
    os.makedirs(args.datadir, exist_ok=True)
    conf = os.path.join(args.datadir, "dash.conf")
    with open(conf, "w") as f:
        f.write("regtest=1\n[regtest]\n")
        f.write(f"port={args.port}\nrpcport={args.rpcport}\nbind=127.0.0.1\nrpcbind=127.0.0.1\nrpcallowip=127.0.0.1\n")
        f.write("listenonion=0\ndiscover=0\ndnsseed=0\n")
        f.write("server=1\ndisablewallet=1\nprinttoconsole=0\nfallbackfee=0.0001\n")
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


def main():
    proc, rpc = start()
    try:
        record(rpc)
    finally:
        try:
            rpc.stop()
        except Exception:
            pass
        proc.wait(timeout=300)
    write_vectors()


state = {}


def record(rpc):
    op_true = CScript([OP_TRUE])
    desc = rpc.getdescriptorinfo("raw(51)")["descriptor"]
    rpc.generatetodescriptor(101, desc)

    def coinbase_outpoint(height):
        block = rpc.getblock(rpc.getblockhash(height), 2)
        cb = block["tx"][0]
        n = next(o["n"] for o in cb["vout"] if o["scriptPubKey"]["hex"] == op_true.hex())
        return COutPoint(int(cb["txid"], 16), n), duffs(cb["vout"][n]["value"])

    k_pkh_c = key("p2pkh compressed", True)
    k_pkh_u = key("p2pkh uncompressed", False)
    k_pk_c_even = key("p2pk compressed even", True, odd_y=False)
    k_pk_c_odd = key("p2pk compressed odd", True, odd_y=True)
    k_pk_even = key("p2pk uncompressed even", False, odd_y=False)
    k_pk_odd = key("p2pk uncompressed odd", False, odd_y=True)
    k_hybrid = key("p2pk hybrid", False)
    k_multi = key("bare multisig", True)
    redeem = CScript([OP_TRUE])

    def p2pkh(k):
        return CScript([OP_DUP, OP_HASH160, hash160(k.get_pubkey().get_bytes()), OP_EQUALVERIFY, OP_CHECKSIG])

    outputs = [
        ("p2pkh compressed", p2pkh(k_pkh_c), k_pkh_c),
        ("p2pkh uncompressed", p2pkh(k_pkh_u), k_pkh_u),
        ("p2sh", CScript([OP_HASH160, hash160(redeem), OP_EQUAL]), None),
        ("p2pk compressed even y", CScript([k_pk_c_even.get_pubkey().get_bytes(), OP_CHECKSIG]), k_pk_c_even),
        ("p2pk compressed odd y", CScript([k_pk_c_odd.get_pubkey().get_bytes(), OP_CHECKSIG]), k_pk_c_odd),
        ("p2pk uncompressed even y", CScript([k_pk_even.get_pubkey().get_bytes(), OP_CHECKSIG]), k_pk_even),
        ("p2pk uncompressed odd y", CScript([k_pk_odd.get_pubkey().get_bytes(), OP_CHECKSIG]), k_pk_odd),
        ("p2pk hybrid key", CScript([hybrid_bytes(k_hybrid), OP_CHECKSIG]), k_hybrid),
        ("bare 1-of-1 multisig", CScript([OP_1, k_multi.get_pubkey().get_bytes(), OP_1, OP_CHECKMULTISIG]), k_multi),
        ("bare op_true", op_true, None),
        ("op_true, zero value", op_true, None),
        ("MAX_SCRIPT_SIZE script", big_script(), None),
    ]

    # Block 102: fund every kind from two coinbases; also an OP_RETURN and a script one byte past
    # MAX_SCRIPT_SIZE, which never enter the UTXO set.
    fund = CTransaction()
    total = 0
    for h in (1, 2):
        outpoint, value = coinbase_outpoint(h)
        fund.vin.append(CTxIn(outpoint, b""))
        total += value
    share = (total - COIN) // len(outputs)
    for label, spk, _ in outputs:
        fund.vout.append(CTxOut(0 if "zero value" in label else share, spk))
    fund.vout.append(CTxOut(0, CScript([OP_RETURN, b"cs5a"])))
    fund.vout.append(CTxOut(1000, CScript([bytes(MAX_SCRIPT_SIZE - 2)])))
    fund.rehash()
    rpc.generateblock(desc, [fund.serialize().hex()])

    # Block 103: spend every kind, plus a coinbase, and spend the first spend's output in the
    # same block.
    spend = CTransaction()
    for n in range(len(outputs)):
        spend.vin.append(CTxIn(COutPoint(fund.sha256, n), b""))
    outpoint, value = coinbase_outpoint(3)
    spend.vin.append(CTxIn(outpoint, b""))
    spend.vout.append(CTxOut(share * (len(outputs) - 1) + value - COIN, op_true))
    for n, (label, spk, k) in enumerate(outputs):
        if label.startswith("p2pkh"):
            sig = sign(spend, n, spk, k)
            spend.vin[n].scriptSig = CScript([sig, k.get_pubkey().get_bytes()])
        elif label.startswith("p2pk"):
            spend.vin[n].scriptSig = CScript([sign(spend, n, spk, k)])
        elif label == "p2sh":
            spend.vin[n].scriptSig = CScript([bytes(redeem)])
        elif label.startswith("bare 1-of-1"):
            spend.vin[n].scriptSig = CScript([OP_0, sign(spend, n, spk, k)])
    spend.rehash()
    chained = CTransaction()
    chained.vin.append(CTxIn(COutPoint(spend.sha256, 0), b""))
    chained.vout.append(CTxOut(spend.vout[0].nValue - COIN, op_true))
    chained.rehash()
    rpc.generateblock(desc, [spend.serialize().hex(), chained.serialize().hex()])

    state["kinds"] = [label for label, _, _ in outputs]
    state["heights"] = [101, 102, 103]
    state["blocks"] = {}
    for h in state["heights"]:
        bh = rpc.getblockhash(h)
        state["blocks"][h] = {
            "hash": bh,
            "block": rpc.getblock(bh, 0),
            "verbose": rpc.getblock(bh, 3),
        }
    state["subversion"] = rpc.getnetworkinfo()["subversion"]


def write_vectors():
    rev = open(os.path.join(args.datadir, "regtest", "blocks", "rev00000.dat"), "rb").read()
    magic = rev[:4]
    records = []
    pos = 0
    while pos + 8 <= len(rev) and rev[pos:pos + 4] == magic:
        size = int.from_bytes(rev[pos + 4:pos + 8], "little")
        undo = rev[pos + 8:pos + 8 + size]
        checksum = rev[pos + 8 + size:pos + 8 + size + 32]
        records.append((undo, checksum))
        pos += 8 + size + 32

    blocks = []
    for h in state["heights"]:
        b = state["blocks"][h]
        prev = bytes.fromhex(b["verbose"]["previousblockhash"])[::-1]
        matches = [(u, c) for u, c in records if sha256d(prev + u) == c]
        assert len(matches) == 1, f"height {h}: {len(matches)} undo records match"
        undo, checksum = matches[0]
        prevouts = []
        for tx in b["verbose"]["tx"][1:]:
            prevouts.append([{
                "height": vin["prevout"]["height"],
                "coinbase": vin["prevout"]["generated"],
                "value": duffs(vin["prevout"]["value"]),
                "script_pubkey": vin["prevout"]["scriptPubKey"]["hex"],
            } for vin in tx["vin"]])
        blocks.append({
            "height": h,
            "hash": b["hash"],
            "prev_hash": b["verbose"]["previousblockhash"],
            "block": b["block"],
            "undo": undo.hex(),
            "checksum": checksum.hex(),
            "prevouts": prevouts,
        })

    with open(args.dashd, "rb") as f:
        dashd_sha256 = hashlib.sha256(f.read()).hexdigest()
    out = {
        "core": state["subversion"],
        "dashd_sha256": dashd_sha256,
        "chain": "regtest",
        "script_kinds": state["kinds"],
        "blocks": blocks,
    }
    with open(args.out, "w") as f:
        json.dump(out, f, indent=1)
        f.write("\n")


if __name__ == "__main__":
    main()
