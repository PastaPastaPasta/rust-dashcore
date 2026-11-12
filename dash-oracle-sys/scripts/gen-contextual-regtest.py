#!/usr/bin/env python3
"""Capture Core's own verdicts on crafted regtest blocks and transactions for the
contextual smoke tests (tests/data/contextual-regtest-<pin>.json).

Runs a release dashd (`--dashd`, the pin's official binary) on a fresh regtest datadir
with the default chain parameters (BIP34/65/66, CSV and V19 from height 1; DIP0003 at
432), mines with a mock clock, and at a tip before DIP0003 (300) and one
after (440) records:
- the headers 0 .. tip, the node's adjusted (mock) time and the version-bits states for
  the block after the tip, the context of the checks;
- block and header verdicts: a block Core mined on the tip (accepted, as mined), taken
  back with invalidateblock, then mutated and submitted as a getblocktemplate proposal. Core
  answers with TestBlockValidity()'s reject reason (ContextualCheckBlockHeader(),
  CheckBlock(), ContextualCheckBlock() in that order; CheckBlock() is checked to pass
  in the tests through oracle_check_block);
- sequence-lock verdicts: version-2 transactions spending a mature coinbase coin with
  height- and time-based relative locks around the boundary, judged by
  testmempoolaccept (allowed, or "non-BIP68-final"), which evaluates the locks for the
  block after the tip.
The expected values are Core's answers; the mutations only aim at both outcomes.

Usage: gen-contextual-regtest.py --dashd <release>/bin/dashd --pin v24.0 --datadir DIR \\
           --port BASE > tests/data/contextual-regtest-v24.0.json
"""

import argparse
import base64
import hashlib
import json
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

MOCK_START = 1_700_000_000
SPACING = 150  # regtest nPowTargetSpacing
SEQUENCE_TYPE_FLAG = 1 << 22


def dsha256(b):
    return hashlib.sha256(hashlib.sha256(b).digest()).digest()


def compact_size(n):
    if n < 0xFD:
        return bytes([n])
    if n <= 0xFFFF:
        return b"\xfd" + n.to_bytes(2, "little")
    return b"\xfe" + n.to_bytes(4, "little")


def read_compact(b, i):
    first = b[i]
    if first < 0xFD:
        return first, i + 1
    width = {0xFD: 2, 0xFE: 4, 0xFF: 8}[first]
    return int.from_bytes(b[i + 1 : i + 1 + width], "little"), i + 1 + width


def tx_body_end(b, i):
    """The offset after the lock time of the transaction starting at b[i]."""
    j = i + 4
    n, j = read_compact(b, j)
    for _ in range(n):
        j += 36
        script, j = read_compact(b, j)
        j += script + 4
    n, j = read_compact(b, j)
    for _ in range(n):
        j += 8
        script, j = read_compact(b, j)
        j += script
    return j + 4


def split_tx(b, i):
    """The end offset of the transaction starting at b[i] (with its extra payload)."""
    version = int.from_bytes(b[i : i + 2], "little")
    tx_type = int.from_bytes(b[i + 2 : i + 4], "little")
    j = tx_body_end(b, i)
    if version >= 3 and tx_type != 0:
        payload, j = read_compact(b, j)
        j += payload
    return j


def split_block(raw):
    header = raw[:80]
    n, i = read_compact(raw, 80)
    txs = []
    for _ in range(n):
        j = split_tx(raw, i)
        txs.append(raw[i:j])
        i = j
    assert i == len(raw), "block parse"
    return header, txs


def merkle_root(txs):
    layer = [dsha256(tx) for tx in txs]
    while len(layer) > 1:
        if len(layer) % 2:
            layer.append(layer[-1])
        layer = [dsha256(layer[k] + layer[k + 1]) for k in range(0, len(layer), 2)]
    return layer[0]


def join_block(header, txs):
    header = bytearray(header)
    header[36:68] = merkle_root(txs)
    return bytes(header) + compact_size(len(txs)) + b"".join(txs)


def set_header(raw, version=None, time_=None, bits=None):
    raw = bytearray(raw)
    if version is not None:
        raw[0:4] = version.to_bytes(4, "little")
    if time_ is not None:
        raw[68:72] = time_.to_bytes(4, "little")
    if bits is not None:
        raw[72:76] = bits.to_bytes(4, "little")
    return bytes(raw)


def scriptnum_push(n):
    """CScript() << n for a positive height (CScriptNum, minimal push)."""
    if n <= 16:
        return bytes([0x50 + n])
    body = n.to_bytes((n.bit_length() + 8) // 8, "little")
    return bytes([len(body)]) + body


class Node:
    def __init__(self, dashd, datadir, port):
        self.datadir = Path(datadir)
        shutil.rmtree(self.datadir, ignore_errors=True)
        self.datadir.mkdir(parents=True)
        self.rpc_port = port + 1
        args = [dashd, "-regtest", f"-datadir={self.datadir}", f"-port={port}", f"-rpcport={self.rpc_port}",
                "-rpcuser=o2a", "-rpcpassword=o2a", "-listen=0", "-listenonion=0", "-server", "-fallbackfee=0.0001",
                "-printtoconsole=0", f"-mocktime={MOCK_START}"]
        self.proc = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.auth = base64.b64encode(b"o2a:o2a").decode()
        for _ in range(120):
            try:
                self.call("getblockcount")
                return
            except Exception:
                time.sleep(1)
        raise RuntimeError("dashd did not start")

    def call(self, method, *params, wallet=None):
        url = f"http://127.0.0.1:{self.rpc_port}/" + (f"wallet/{wallet}" if wallet else "")
        body = json.dumps({"jsonrpc": "1.0", "id": 0, "method": method, "params": list(params)}).encode()
        req = urllib.request.Request(url, data=body, headers={"Authorization": f"Basic {self.auth}"})
        try:
            with urllib.request.urlopen(req, timeout=300) as resp:
                reply = json.load(resp)
        except urllib.error.HTTPError as e:
            reply = json.load(e)
        if reply.get("error"):
            raise RuntimeError(f"{method}{params}: {reply['error']}")
        return reply["result"]

    def stop(self):
        try:
            self.call("stop")
        except Exception:
            pass
        self.proc.wait(timeout=120)
        shutil.rmtree(self.datadir, ignore_errors=True)


class Chain:
    def __init__(self, node):
        self.node = node
        self.now = MOCK_START
        node.call("createwallet", "w")
        self.address = node.call("getnewaddress", wallet="w")

    def mine(self, n):
        for _ in range(n):
            self.now += SPACING
            self.node.call("setmocktime", self.now)
            self.node.call("generatetoaddress", 1, self.address, wallet="w")

    def tip(self):
        return self.node.call("getblockcount")

    def header_hex(self, height):
        return self.node.call("getblockheader", self.node.call("getblockhash", height), False)

    def mtp(self, height):
        return self.node.call("getblockheader", self.node.call("getblockhash", height))["mediantime"]

    def next_block(self):
        """A block Core mined on the tip, then invalidated so the tip is unchanged."""
        self.now += SPACING
        self.node.call("setmocktime", self.now)
        block_hash = self.node.call("generatetoaddress", 1, self.address, wallet="w")[0]
        raw = bytes.fromhex(self.node.call("getblock", block_hash, 0))
        self.node.call("invalidateblock", block_hash)
        return raw

    def propose(self, raw):
        result = self.node.call("getblocktemplate", {"mode": "proposal", "data": raw.hex()})
        return result  # None when Core accepts it


def block_cases(chain, tip, post_dip3):
    """A block Core mined (and so accepted) on the tip, and mutations of it, each judged
    by a getblocktemplate proposal."""
    raw = chain.next_block()
    header, txs = split_block(raw)
    cases = []

    def add(name, kind, block):
        reason = chain.propose(block)
        cases.append({"name": name, "kind": kind, "block": block.hex(), "core_reason": reason})

    add("bad-diffbits", "header", set_header(raw, bits=0x207FFFFE))
    add("time-too-old", "header", set_header(raw, time_=chain.mtp(tip)))
    add("time-too-new", "header", set_header(raw, time_=chain.now + 2 * 60 * 60 + 1))
    add("bad-version", "header", set_header(raw, version=3))
    # A transaction with a height lock beyond the block, one non-final input.
    prev = dsha256(b"o2a-nonfinal")
    nonfinal = (
        (1).to_bytes(2, "little") + (0).to_bytes(2, "little") + b"\x01" + prev + (0).to_bytes(4, "little")
        + b"\x00" + (0).to_bytes(4, "little") + b"\x01" + (1000).to_bytes(8, "little") + b"\x01\x51"
        + (tip + 100).to_bytes(4, "little")
    )
    add("bad-txns-nonfinal", "block", join_block(header, txs + [nonfinal]))
    if not post_dip3:
        # The coinbase's height push off by one (BIP34 applies until DIP0003).
        coinbase = bytearray(txs[0])
        script_len, at = read_compact(coinbase, 4 + 1 + 36)
        push = scriptnum_push(tip + 1)
        assert coinbase[at : at + len(push)] == push, "coinbase height push"
        wrong = scriptnum_push(tip + 2)
        assert len(wrong) == len(push)
        coinbase[at : at + len(push)] = wrong
        add("bad-cb-height", "block", join_block(header, [bytes(coinbase)] + txs[1:]))
    else:
        # The coinbase as a version-3 normal transaction instead of a CbTx: the same
        # input, outputs and lock time, without the payload. A normal coinbase needs a
        # scriptSig of 2 bytes or more (CheckTransaction(), bad-cb-length; a CbTx's may
        # have 1), so a shorter one gets an OP_0 appended.
        coinbase = txs[0]
        assert int.from_bytes(coinbase[0:2], "little") == 3 and int.from_bytes(coinbase[2:4], "little") == 5
        assert coinbase[4] == 1, "one coinbase input"
        script_len, at = read_compact(coinbase, 4 + 1 + 36)
        script = coinbase[at : at + script_len]
        if len(script) < 2:
            script += b"\x00"
        plain = (
            (3).to_bytes(2, "little") + (0).to_bytes(2, "little") + coinbase[4 : 4 + 1 + 36]
            + compact_size(len(script)) + script + coinbase[at + script_len : tx_body_end(coinbase, 0)]
        )
        add("bad-cb-type", "block", join_block(header, [plain] + txs[1:]))
        # An unknown special transaction type.
        for tx_type in (13, 10):
            special = (
                (3).to_bytes(2, "little") + tx_type.to_bytes(2, "little") + b"\x01" + dsha256(b"o2a-type")
                + (0).to_bytes(4, "little") + b"\x00" + (0xFFFFFFFF).to_bytes(4, "little") + b"\x01"
                + (1000).to_bytes(8, "little") + b"\x01\x51" + (0).to_bytes(4, "little") + b"\x02\x01\x00"
            )
            add(f"type-{tx_type}", "block", join_block(header, txs + [special]))
    return raw, cases


def sequence_lock_cases(chain, tip):
    """Version-2 spends of the coinbase coin of block tip - 150 with relative locks on both
    sides of the boundary; Core's verdict for the block after the tip is testmempoolaccept's."""
    node = chain.node
    coin_height = tip - 150
    block = node.call("getblock", node.call("getblockhash", coin_height), 2)
    coinbase = block["tx"][0]
    value = coinbase["vout"][0]["value"]
    mtp_tip, mtp_coin = chain.mtp(tip), chain.mtp(coin_height - 1)
    k = (mtp_tip - mtp_coin) // 512
    locks = {
        "height-satisfied": tip + 1 - coin_height,
        "height-unsatisfied": tip + 2 - coin_height,
        "time-satisfied": SEQUENCE_TYPE_FLAG | k,
        "time-unsatisfied": SEQUENCE_TYPE_FLAG | (k + 1),
    }
    cases = []
    for name, sequence in locks.items():
        unsigned = node.call(
            "createrawtransaction",
            [{"txid": coinbase["txid"], "vout": 0, "sequence": sequence}],
            {chain.address: round(value - 0.001, 8)},
        )
        unsigned = (2).to_bytes(2, "little").hex() + unsigned[4:]  # nVersion 2, normal type
        signed = node.call("signrawtransactionwithwallet", unsigned, wallet="w")
        assert signed["complete"], signed
        verdict = node.call("testmempoolaccept", [signed["hex"]])[0]
        reason = None if verdict["allowed"] else verdict["reject-reason"]
        assert reason in (None, "non-BIP68-final"), verdict
        cases.append({"name": name, "tx": signed["hex"], "prev_heights": [coin_height], "satisfied": reason is None})
    outcomes = {c["name"]: c["satisfied"] for c in cases}
    assert outcomes == {"height-satisfied": True, "height-unsatisfied": False, "time-satisfied": True,
                        "time-unsatisfied": False}, outcomes
    return cases


# Consensus::DeploymentPos of the version-bits deployments (v23.1.8 has the first two).
DEPLOYMENT_POS = {"testdummy": 0, "v24": 1, "evo_shares": 2}
STATES = ["defined", "started", "locked_in", "active", "failed"]  # ThresholdState order


def deployments(node):
    """[[DeploymentPos, ThresholdState]] for the block after the tip: getdeploymentinfo's
    status_next (v24), or getblockchaininfo's softfork status (v23.1.8, which computes it
    from the tip, as status_next does)."""
    try:
        forks = node.call("getdeploymentinfo")["deployments"]
        key = "status_next"
    except RuntimeError:
        forks = node.call("getblockchaininfo")["softforks"]
        key = "status"
    return sorted(
        [DEPLOYMENT_POS[name], STATES.index(fork["bip9"][key])]
        for name, fork in forks.items()
        if fork["type"] == "bip9"
    )


def sample_tip(chain, post_dip3):
    tip = chain.tip()
    mined, blocks = block_cases(chain, tip, post_dip3)  # advances the mock clock by one block
    return {
        "tip": tip,
        "first_height": 0,
        "headers": "".join(chain.header_hex(h) for h in range(tip + 1)),
        "adjusted_time": chain.now,  # the node's adjusted time when it judged the proposals
        "deployments": deployments(chain.node),
        "bls_legacy": False,  # V19 from height 1
        "sequence_locks": sequence_lock_cases(chain, tip),
        "mined_block": mined.hex(),
        "blocks": blocks,
    }


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--dashd", required=True)
    ap.add_argument("--pin", required=True)
    ap.add_argument("--datadir", required=True)
    ap.add_argument("--port", type=int, required=True, help="P2P port; the RPC port is the next one")
    args = ap.parse_args()
    version = subprocess.run([args.dashd, "--version"], capture_output=True, text=True).stdout.splitlines()[0]
    node = Node(args.dashd, args.datadir, args.port)
    try:
        chain = Chain(node)
        chain.mine(300)
        before = sample_tip(chain, post_dip3=False)
        chain.mine(140)
        after = sample_tip(chain, post_dip3=True)
    finally:
        node.stop()
    out = {
        "provenance": f"{version}, regtest with default parameters; scripts/gen-contextual-regtest.py "
        "(see tests/data/PROVENANCE.md)",
        "pin": args.pin,
        "tips": [before, after],
    }
    json.dump(out, sys.stdout, indent=1)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
