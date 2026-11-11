#!/usr/bin/env python3
"""Records what Dash Core reports for blocks with several invalid transactions, serially (-par=1)
and with script-check queues of several sizes, for dashd-chainstate's L3 test (CS6, FR-CHS-10, F20;
CH3a a9, DEC-323).

Starts five release dashd nodes on fresh regtest datadirs with the same mock clock, not connected,
one per `-par` in MODES: `par1` (no script-check threads), `par2` (one, so `ConnectBlock` queues
its script checks), `par16` (15, the most), `par-40` (leaves 40 cores free: 7 threads on 48
cores) and `par-1000` (no threads on any host up to 1000 cores). Each node's "Script verification
uses N additional threads" line gives its threads. Mines 101 blocks to OP_TRUE on `par1`, then a
funding block whose transaction pays 1 DASH to each of 240 P2PKH outputs over eight keys, and
copies every block to the other nodes.

Each case is a block on that tip with a coinbase and some transactions, built and solved here
(`blocktools.create_block`, X11 through the `dash_hash` extension) so that no RPC validates it
first. Each node gets, in order:
- `generateblock raw(51) [txs] false`: `TestBlockValidity`, which runs `ConnectBlock` with
  `fJustCheck`, so it stores in both caches; its error is `TestBlockValidity failed: <reason>`;
- `submitblock <the solved block>`: block connection, which consults the caches without storing;
  its result is the reject reason (or null when the block connects).
The `debug.log` lines either writes from `ConnectBlock` are kept with each result. Inputs are
spent one of these ways: a valid signature, a signature over another digest (EVAL_FALSE), a
valid signature with another key's pubkey (EQUALVERIFY), a signature with a byte after its DER
encoding (SIG_DER, DERSIG being active), or an outpoint that does not exist
(bad-txns-inputs-missingorspent); one case also spends, with an empty scriptSig, the OP_TRUE
output of an earlier transaction in its block. One case first puts a valid transaction in every
mempool, so its signatures and script execution are cached when the block connects. The last case
is a valid block, which every node connects. Signatures are RFC 6979 and the clock is mocked, so a
run reproduces byte for byte on a host with as many online processors (`num_cores`, which only
`par-40`'s threads depend on).

Usage:
  cs6-checkqueue-vectors.py --dashd <path to dashd> --framework <Core's test/functional dir>
                            --datadir <empty dir> --port <p2p port> --rpcport <rpc port> --out <json>

`--framework` is Core's `test/functional` directory at the release's tag. Each node uses
`--port`/`--rpcport` plus its index in MODES. `dash_hash` (the X11 extension Core's functional
tests use) must be importable.
"""

import argparse
import hashlib
import json
import os
import re
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

from test_framework.authproxy import AuthServiceProxy, JSONRPCException  # noqa: E402
from test_framework.blocktools import create_block, create_coinbase  # noqa: E402
from test_framework.key import ECKey  # noqa: E402
from test_framework.messages import COIN, COutPoint, CTransaction, CTxIn, CTxOut  # noqa: E402
from test_framework.script import (  # noqa: E402
    OP_CHECKSIG, OP_DUP, OP_EQUALVERIFY, OP_HASH160, OP_TRUE, SIGHASH_ALL, CScript, SignatureHash,
    hash160,
)

MOCK_TIME = 1_760_000_000
FUNDED = 240
KEYS = 8
MODES = {"par1": 1, "par2": 2, "par16": 16, "par-40": -40, "par-1000": -1000}


def key(i):
    k = ECKey()
    k.set(hashlib.sha256(f"cs6 key {i}".encode()).digest(), True)
    return k


def p2pkh(k):
    return CScript([OP_DUP, OP_HASH160, hash160(k.get_pubkey().get_bytes()), OP_EQUALVERIFY, OP_CHECKSIG])


def duffs(value):
    return int(Decimal(str(value)) * COIN)


class Node:
    def __init__(self, name, par, offset):
        self.name = name
        self.datadir = os.path.join(args.datadir, name)
        os.makedirs(self.datadir, exist_ok=True)
        rpcport = args.rpcport + offset
        conf = os.path.join(self.datadir, "dash.conf")
        with open(conf, "w") as f:
            f.write("regtest=1\n[regtest]\n")
            f.write(f"port={args.port + offset}\nrpcport={rpcport}\nbind=127.0.0.1\nrpcbind=127.0.0.1\n")
            f.write("rpcallowip=127.0.0.1\nlistenonion=0\ndiscover=0\ndnsseed=0\nconnect=0\n")
            f.write("server=1\ndisablewallet=1\nprinttoconsole=0\nlogtimestamps=0\n")
            f.write(f"par={par}\n")
        self.proc = subprocess.Popen(
            [args.dashd, f"-datadir={self.datadir}", f"-conf={conf}", f"-mocktime={MOCK_TIME}"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        cookie = os.path.join(self.datadir, "regtest", ".cookie")
        self.log = os.path.join(self.datadir, "regtest", "debug.log")
        for _ in range(600):
            if self.proc.poll() is not None:
                sys.exit(f"{name}: dashd exited with {self.proc.returncode}")
            if os.path.exists(cookie):
                try:
                    user, password = open(cookie).read().strip().split(":", 1)
                    self.rpc = AuthServiceProxy(f"http://{user}:{password}@127.0.0.1:{rpcport}", timeout=600)
                    self.rpc.getblockcount()
                    return
                except Exception:
                    pass
            time.sleep(0.5)
        self.proc.kill()
        sys.exit(f"{name}: dashd did not start")

    def stop(self):
        try:
            self.rpc.stop()
        except Exception:
            pass
        self.proc.wait(timeout=300)

    def script_threads(self):
        """N from the node's "Script verification uses N additional threads" line (init step 4a)."""
        with open(self.log) as f:
            found = re.findall(r"Script verification uses (\d+) additional threads", f.read())
        assert len(found) == 1, f"{self.name}: {found}"
        return int(found[0])

    def log_offset(self):
        return os.path.getsize(self.log)

    def connect_lines(self, offset):
        """The lines `ConnectBlock` and the validation interface wrote after `offset`."""
        with open(self.log, "rb") as f:
            f.seek(offset)
            text = f.read().decode()
        keep = ("ConnectBlock", "CheckQueue failed", "CheckTxInputs", "CheckInputScripts", "InvalidChainFound",
                "TestBlockValidity", "AcceptBlock")
        return [line for line in text.splitlines() if any(k in line for k in keep)]


def main():
    nodes = [Node(name, par, i) for i, (name, par) in enumerate(MODES.items())]
    try:
        state["threads"] = {node.name: node.script_threads() for node in nodes}
        record(nodes)
    finally:
        for node in nodes:
            node.stop()
    write_vectors()


state = {"cases": []}


def record(nodes):
    a = nodes[0]
    desc = a.rpc.getdescriptorinfo("raw(51)")["descriptor"]
    a.rpc.generatetodescriptor(101, desc)
    keys = [key(i) for i in range(KEYS)]

    def coinbase_outpoint(height):
        cb = a.rpc.getblock(a.rpc.getblockhash(height), 2)["tx"][0]
        n = next(o["n"] for o in cb["vout"] if o["scriptPubKey"]["hex"] == "51")
        return COutPoint(int(cb["txid"], 16), n), duffs(cb["vout"][n]["value"])

    # Block 102: 240 outputs of 1 DASH, output n paying key n % 8, and change to OP_TRUE.
    fund = CTransaction()
    total = 0
    for h in (1, 2):
        outpoint, value = coinbase_outpoint(h)
        fund.vin.append(CTxIn(outpoint, b""))
        total += value
    for n in range(FUNDED):
        fund.vout.append(CTxOut(COIN, p2pkh(keys[n % KEYS])))
    fund.vout.append(CTxOut(total - FUNDED * COIN - COIN, CScript([OP_TRUE])))
    fund.rehash()
    a.rpc.generateblock(desc, [fund.serialize().hex()])
    for h in range(1, a.rpc.getblockcount() + 1):
        raw = a.rpc.getblock(a.rpc.getblockhash(h), 0)
        for node in nodes[1:]:
            assert node.rpc.submitblock(raw) is None, f"{node.name}: block {h}"
    tip = a.rpc.getbestblockhash()
    for node in nodes:
        assert node.rpc.getbestblockhash() == tip, node.name
    state["funding"] = {"height": 102, "tx": fund.serialize().hex()}
    state["deployments"] = a.rpc.getdeploymentinfo()["deployments"]

    used = set()

    def spend(unlocks, fee=10_000, parent=None, pay=CScript([OP_TRUE])):
        """A transaction spending one funded output per entry of `unlocks` (`(n, how)`), after
        `parent`'s OP_TRUE output 0 if given (with an empty scriptSig), paying everything but
        `fee` to `pay`."""
        tx = CTransaction()
        value = 0
        if parent is not None:
            tx.vin.append(CTxIn(COutPoint(parent.sha256, 0), b""))
            value += parent.vout[0].nValue
        first = len(tx.vin)
        for n, how in unlocks:
            if how == "missing":
                tx.vin.append(CTxIn(COutPoint(int.from_bytes(hashlib.sha256(f"cs6 missing {n}".encode()).digest(), "little"), 0), b""))
                continue
            assert n not in used, f"output {n} spent twice"
            used.add(n)
            tx.vin.append(CTxIn(COutPoint(fund.sha256, n), b""))
            value += COIN
        tx.vout.append(CTxOut(max(value - fee, 0), pay))
        for i, (n, how) in enumerate(unlocks, start=first):
            if how == "missing":
                continue
            k = keys[n % KEYS]
            spk = p2pkh(k)
            sighash, err = SignatureHash(spk, tx, i, SIGHASH_ALL)
            assert err is None
            pubkey = k.get_pubkey().get_bytes()
            if how == "wrong-digest":
                sighash = hashlib.sha256(sighash).digest()
            elif how == "wrong-key":
                pubkey = keys[(n + 1) % KEYS].get_pubkey().get_bytes()
            else:
                assert how in ("ok", "bad-der"), how
            der = k.sign_ecdsa(sighash, rfc6979=True)
            if how == "bad-der":
                der += b"\x00"
            tx.vin[i].scriptSig = CScript([der + bytes([SIGHASH_ALL]), pubkey])
        tx.rehash()
        return tx

    def case(name, txs, mempool=()):
        """Builds and solves a block with `txs` on the tip, and records each node's verdicts."""
        tip = a.rpc.getbestblockhash()
        header = a.rpc.getblockheader(tip)
        height = header["height"] + 1
        for tx in mempool:
            for node in nodes:
                node.rpc.sendrawtransaction(tx.serialize().hex())
        block = create_block(int(tip, 16), create_coinbase(height), header["time"] + 1,
                             version=0x20000000, txlist=list(txs))
        block.solve()
        hexblock = block.serialize().hex()
        verdicts = {}
        for node in nodes:
            offset = node.log_offset()
            try:
                node.rpc.generateblock(desc, [tx.serialize().hex() for tx in txs], False)
                test = None
            except JSONRPCException as e:
                test = e.error["message"]
            tested = node.connect_lines(offset)
            offset = node.log_offset()
            connect = node.rpc.submitblock(hexblock)
            # The validation interface logs after submitblock returns.
            time.sleep(0.5)
            verdicts[node.name] = {
                "test_block_validity": test,
                "test_block_validity_log": tested,
                "submitblock": connect,
                "submitblock_log": node.connect_lines(offset),
                "tip": node.rpc.getbestblockhash(),
            }
        state["cases"].append({
            "name": name,
            "height": height,
            "block": hexblock,
            "mempool": [tx.serialize().hex() for tx in mempool],
            "verdicts": verdicts,
        })

    outputs = iter(range(FUNDED))

    def take(*hows):
        return [(next(outputs), how) for how in hows]

    case("two failing transactions", [spend(take("ok", "wrong-digest")), spend(take("wrong-key"))])
    case("two failing transactions, other order", [spend(take("wrong-key")), spend(take("ok", "wrong-digest"))])
    case("script failure, then a transaction with a missing input",
         [spend(take("ok")), spend(take("wrong-digest")), spend(take("ok") + [(0, "missing")])])
    case("missing input, then a script failure", [spend([(1, "missing")]), spend(take("wrong-digest"))])
    case("one transaction, inputs failing three ways",
         [spend(take("ok", "wrong-digest", "bad-der", "wrong-key"))])
    case("one transaction, inputs failing three ways, other order",
         [spend(take("ok", "bad-der", "wrong-key", "wrong-digest"))])
    many = [spend(take(*["ok"] * 40)) for _ in range(3)]
    case("130 inputs, the last input failing", many + [spend(take(*["ok"] * 9, "bad-der"))])
    parent = spend(take("ok", "ok"))
    case("a failing transaction spending an output of a passing one in the block",
         [parent, spend(take("wrong-key"), parent=parent)])
    # The mempool's policy wants a standard output.
    warm = spend(take("ok", "ok", "ok"), pay=p2pkh(keys[0]))
    case("a mempool transaction, then a failing one", [warm, spend(take("wrong-digest"))], mempool=[warm])
    good = [warm, spend(take("ok", "ok")), spend(take(*["ok"] * 5))]
    case("a valid block", good)
    for node in nodes:
        assert node.rpc.getblockcount() == 103, node.name
    state["subversion"] = a.rpc.getnetworkinfo()["subversion"]


def write_vectors():
    with open(args.dashd, "rb") as f:
        dashd_sha256 = hashlib.sha256(f.read()).hexdigest()
    out = {
        "core": state["subversion"],
        "dashd_sha256": dashd_sha256,
        "chain": "regtest",
        "mock_time": MOCK_TIME,
        "modes": MODES,
        "num_cores": os.sysconf("SC_NPROCESSORS_ONLN"),
        "threads": state["threads"],
        "deployments": state["deployments"],
        "funding": state["funding"],
        "cases": state["cases"],
    }
    with open(args.out, "w") as f:
        json.dump(out, f, indent=1)
        f.write("\n")


if __name__ == "__main__":
    main()
