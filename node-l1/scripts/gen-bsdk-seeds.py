#!/usr/bin/env python3
"""Seed corpora of the BS4b targets, cut from transactions and blocks Dash Core produced.

Writes node-l1/tests/data/bsdk-seeds/<target>/<name> for the four targets:

  diff_payload_decode      selector byte (type - 1) ++ the extra payload of a special transaction
  diff_bsdk_tx_decode      a whole transaction
  diff_bsdk_block_decode   a whole block
  diff_bsdk_header_decode  an 80-byte header

Nothing here is hand-written: every byte comes from the files below, which carry their own
provenance (dash-oracle-sys/tests/data/PROVENANCE.md, test-vectors/core/*/PROVENANCE.md,
conformance/tests/data = `getblock` of regtest chains built by Core's functional tests and of
two testnet blocks, all from dashd v24.0.0-rc.3):

  conformance/tests/data/*.json            block hex
  dash-oracle-sys/tests/data/{mainnet,testnet}.json   blocks[].hex
  dash-oracle-sys/tests/data/contextual-mainnet.json  dip3_block, proupserv
  dash-oracle-sys/tests/data/v24-asset-unlocks.json   asset unlock transactions
  dash/tests/data/core_v24_regtest.json    transactions[].hex and blocks[].hex of Core's functional
                                           tests (feature_asset_locks, _masternode_shares, ...)
  test-vectors/core/*/src/test/data/trivially_{valid,invalid}.json   ProTx transactions

Usage: gen-bsdk-seeds.py REPO_ROOT [--check]
  --check  fail unless the committed seeds are exactly what this would write
"""
import glob
import hashlib
import json
import os
import sys

PER_KIND = 3  # payload seeds kept per (type, payload version): those with the most non-zero bytes
PER_TX_KIND = 2  # transaction seeds kept per (type, tx version)
MAX_BLOCK = 60_000  # larger blocks are not seeds
MAX_VERSION = 0x7FFF  # transactions with a higher version are not seeds


class Reader:
    def __init__(self, data, pos=0):
        self.data, self.pos = data, pos

    def take(self, n):
        out = self.data[self.pos:self.pos + n]
        if len(out) != n:
            raise ValueError("short")
        self.pos += n
        return out

    def compact(self):
        first = self.take(1)[0]
        if first < 0xFD:
            return first
        return int.from_bytes(self.take({0xFD: 2, 0xFE: 4, 0xFF: 8}[first]), "little")


def payload_version(tx_type, payload):
    """The version a payload names: u16 first, except u8 for MnHF, asset lock and unlock, and
    the final commitment's own version after the payload's version and height."""
    if tx_type in (7, 8, 9):
        return payload[0]
    if tx_type == 6:
        return int.from_bytes(payload[6:8], "little")
    return int.from_bytes(payload[:2], "little")


def parse_tx(r):
    """(raw, type, version, payload) of the transaction at the reader."""
    start = r.pos
    version = int.from_bytes(r.take(2), "little")
    tx_type = int.from_bytes(r.take(2), "little")
    for _ in range(r.compact()):
        r.take(36)
        r.take(r.compact())
        r.take(4)
    for _ in range(r.compact()):
        r.take(8)
        r.take(r.compact())
    r.take(4)
    payload = b""
    if 3 <= version <= MAX_VERSION and tx_type != 0:
        payload = r.take(r.compact())
    return r.data[start:r.pos], tx_type, version, payload


def parse_block(raw):
    r = Reader(raw, 80)
    return [parse_tx(r) for _ in range(r.compact())]


def block_hexes(root):
    for block in json.load(open(f"{root}/dash/tests/data/core_v24_regtest.json"))["blocks"]:
        if block.get("hex"):
            yield block["hex"]
    for path in sorted(glob.glob(f"{root}/conformance/tests/data/*.json")):
        yield json.load(open(path))["hex"]
    for name in ("mainnet", "testnet"):
        for block in json.load(open(f"{root}/dash-oracle-sys/tests/data/{name}.json"))["blocks"]:
            yield block["hex"]
    yield json.load(open(f"{root}/dash-oracle-sys/tests/data/contextual-mainnet.json"))["dip3_block"]["block"]


def tx_hexes(root):
    for tx in json.load(open(f"{root}/dash/tests/data/core_v24_regtest.json"))["transactions"]:
        yield tx["hex"]
    ctx = json.load(open(f"{root}/dash-oracle-sys/tests/data/contextual-mainnet.json"))
    yield ctx["proupserv"]["tx"]
    for row in json.load(open(f"{root}/dash-oracle-sys/tests/data/v24-asset-unlocks.json"))["asset_unlocks"]:
        yield row["hex"]
    for core in sorted(glob.glob(f"{root}/test-vectors/core/*/src/test/data")):
        for name in ("trivially_valid.json", "trivially_invalid.json"):
            for row in json.load(open(f"{core}/{name}"))[1:]:
                yield row[3]


def seeds(root):
    """{target: {name: bytes}}"""
    out = {"diff_payload_decode": {}, "diff_bsdk_tx_decode": {}, "diff_bsdk_block_decode": {}, "diff_bsdk_header_decode": {}}
    tx_kinds = {}
    payloads = {}  # (type, payload version) -> {name: payload}

    def add_tx(raw, tx_type, version, payload):
        if tx_type == 0 or version > MAX_VERSION:
            return
        name = f"t{tx_type}-v{version}-{hashlib.sha256(raw).hexdigest()[:10]}"
        key = (tx_type, version)
        if tx_kinds.get(key, 0) < PER_TX_KIND and name not in out["diff_bsdk_tx_decode"]:
            tx_kinds[key] = tx_kinds.get(key, 0) + 1
            out["diff_bsdk_tx_decode"][name] = raw
        if not 1 <= tx_type <= 12 or version < 3 or len(payload) < 2:
            return
        pver = payload_version(tx_type, payload)
        name = f"t{tx_type}-p{pver}-{hashlib.sha256(payload).hexdigest()[:10]}"
        payloads.setdefault((tx_type, pver), {})[name] = payload

    for hexstr in block_hexes(root):
        raw = bytes.fromhex(hexstr)
        out["diff_bsdk_header_decode"][f"h-{hashlib.sha256(raw[:80]).hexdigest()[:10]}"] = raw[:80]
        txs = parse_block(raw)
        if len(raw) <= MAX_BLOCK and all(tx[2] <= MAX_VERSION for tx in txs):
            out["diff_bsdk_block_decode"][f"b-{hashlib.sha256(raw).hexdigest()[:10]}"] = raw
        for tx in txs:
            add_tx(*tx)
    for hexstr in tx_hexes(root):
        add_tx(*parse_tx(Reader(bytes.fromhex(hexstr))))
    # Payloads whose BLS fields are set are worth more than null ones (a regtest chain's
    # coinbase commitments carry zero signatures): keep the fullest of each kind.
    for (tx_type, _), found in payloads.items():
        fullest = sorted(found.items(), key=lambda kv: (-sum(b != 0 for b in kv[1]), kv[0]))[:PER_KIND]
        for name, payload in fullest:
            out["diff_payload_decode"][name] = bytes([tx_type - 1]) + payload
    return out


def main():
    root = os.path.abspath(sys.argv[1])
    check = "--check" in sys.argv[2:]
    base = f"{root}/node-l1/tests/data/bsdk-seeds"
    want = {f"{t}/{n}": d for t, files in seeds(root).items() for n, d in files.items()}
    have = {}
    for path in glob.glob(f"{base}/*/*"):
        have[os.path.relpath(path, base)] = open(path, "rb").read()
    if check:
        if have != want:
            sys.exit("gen-bsdk-seeds: the committed seeds differ from the generator's")
        print(f"gen-bsdk-seeds: {len(want)} seeds ok")
        return
    for rel in set(have) - set(want):
        os.remove(f"{base}/{rel}")
    for rel, data in want.items():
        os.makedirs(os.path.dirname(f"{base}/{rel}"), exist_ok=True)
        open(f"{base}/{rel}", "wb").write(data)
    print(f"gen-bsdk-seeds: wrote {len(want)} seeds")


if __name__ == "__main__":
    main()
