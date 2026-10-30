#!/usr/bin/env python3
"""Captures P4b's block-serving dashd transcript fixtures (P4b D12) from regtest dashd
v24.0.0-rc.3.

The format, the node, the peers and the synchronisation are P2a-core's
(`contrib/node/p2a-transcripts/capture.py`, whose docstring defines them) with P3's all-peer
settle and digests (`contrib/node/p3-transcripts/capture.py`, which also checks dashd's sha256).
P4b's node mines its blocks itself, at the frozen mock time, before any peer connects, and the
fixture records each one as `submitblock HEX` (its bytes, `getblock HASH 0`) instead of P2a's
`chain HEADER`, so the replayed node stores the same blocks dashd serves. The tip is then a few
minutes from the mock time: the node is out of IBD and `CanDirectFetch` holds.

Scenarios (each peer is inbound, from 127.0.0.1, advertising NODE_NETWORK | NODE_BLOOM |
NODE_NETWORK_LIMITED | NODE_HEADERS_COMPRESSED):

  serve-blocks      20 blocks. getdata MSG_BLOCK of the tip; MSG_CMPCT_BLOCK of the tip (a
                    fresh nonce, which the replay masks) and of tip-6 (a block); an unknown hash
                    with a known one (the known block alone, no notfound); MSG_FILTERED_BLOCK
                    without a filter (nothing); three blocks in one getdata (in order)
  serve-getblocks   600 blocks. getblocks from genesis (500 invs); getdata of the 500th (the
                    block, then the continuation inv of the tip); getblocks from 550 stopping at
                    560; a second peer's 102-entry locator (disconnected)
  serve-filters     10 blocks. A filter on block 5's coinbase txid, then MSG_FILTERED_BLOCK of 5
                    (merkleblock, the coinbase) and of 6 (an empty merkleblock); filterclear and
                    5 again (nothing); filteradd of 520 bytes without a filter (+100); a
                    36,000-byte, 50-function filter and a 520-byte filteradd (kept); a 51-function
                    filter (+100); a 36,001-byte filter (+100, misformatted); a 521-byte
                    filteradd (+100, the read)
  serve-nobloom     -peerbloomfilters=0, genesis only: filterload, filteradd and filterclear
                    each disconnect, unscored
  serve-getblocktxn 20 blocks. getblocktxn of the tip (the most recent block), 10 deep, 11 deep
                    (a full block through the getdata queue), an unknown block (nothing), and an
                    index past the tip's end (+100)
  serve-limited     -prune=1 (manual pruning: NODE_NETWORK off, nothing pruned), 300 blocks.
                    getdata 290 below the tip (served), then 291 below (disconnected)

The local peer is disconnected rather than discouraged at +100 (Core's `IsLocal`), so each
scored request has its own peer.

Usage:
  capture.py OUT_DIR [SCENARIO...]

`P2A_DASHD` defaults to ~/.rust-dashcore-test/dashcore-24.0.0-rc.3/bin/dashd;
`P2A_CAPTURE_DIR` defaults to /work/scratch/p4b/capture and `P2A_PORT_BASE` to 32700. It never
touches /work/chains or /work/bench.
"""

import contextlib
import importlib.util
import os
import pathlib
import struct
import sys

os.environ.setdefault("P2A_CAPTURE_DIR", "/work/scratch/p4b/capture")
os.environ.setdefault("P2A_PORT_BASE", "32700")

_spec = importlib.util.spec_from_file_location(
    "p3_capture", pathlib.Path(__file__).resolve().parent.parent / "p3-transcripts" / "capture.py")
p3 = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(p3)
p2a = p3.p2a

SIM_START, FULL, BIT11 = p2a.SIM_START, p2a.FULL, p2a.BIT11
MSG_BLOCK, MSG_FILTERED_BLOCK, MSG_CMPCT_BLOCK = 2, 3, 20


def ser_hash(display_hex):
    """A hash as the wire carries it (RPC hex is byte-reversed)."""
    return bytes.fromhex(display_hex)[::-1]


def getdata(items):
    return p2a.compact(len(items)) + b"".join(struct.pack("<I", t) + h for t, h in items)


def getblocks(locator, stop=bytes(32)):
    return struct.pack("<i", 70242) + p2a.compact(len(locator)) + b"".join(locator) + stop


def getblocktxn(block, indexes):
    """BIP152's differentially encoded indexes."""
    out, prev = [], -1
    for i in indexes:
        out.append(p2a.compact(i - prev - 1))
        prev = i
    return block + p2a.compact(len(indexes)) + b"".join(out)


def murmur3(seed, data):
    """Core's MurmurHash3 (hash.cpp)."""
    c1, c2, h = 0xCC9E2D51, 0x1B873593, seed & 0xFFFFFFFF
    rotl = lambda x, r: ((x << r) | (x >> (32 - r))) & 0xFFFFFFFF
    n = len(data) // 4
    for i in range(n):
        k, = struct.unpack_from("<I", data, 4 * i)
        k = rotl((k * c1) & 0xFFFFFFFF, 15) * c2 & 0xFFFFFFFF
        h = (rotl(h ^ k, 13) * 5 + 0xE6546B64) & 0xFFFFFFFF
    tail, k = data[4 * n:], 0
    for i, b in reversed(list(enumerate(tail))):
        k ^= b << (8 * i)
    if tail:
        h ^= rotl((k * c1) & 0xFFFFFFFF, 15) * c2 & 0xFFFFFFFF
    h ^= len(data)
    h ^= h >> 16
    h = h * 0x85EBCA6B & 0xFFFFFFFF
    h ^= h >> 13
    h = h * 0xC2B2AE35 & 0xFFFFFFFF
    return h ^ (h >> 16)


def filterload(size, funcs, keys=(), tweak=0, flags=0):
    """A `filterload` of `size` bytes and `funcs` hash functions holding `keys` (CBloomFilter's
    insert, bloom.cpp:53-63)."""
    data = bytearray(size)
    for key in keys:
        for i in range(funcs):
            bit = murmur3((i * 0xFBA4C795 + tweak) & 0xFFFFFFFF, key) % (size * 8)
            data[bit >> 3] |= 1 << (bit & 7)
    return p2a.compact(size) + bytes(data) + struct.pack("<IIB", funcs, tweak, flags)


def filteradd(n):
    return p2a.compact(n) + bytes([1]) * n


class Chain:
    """The node's blocks by height (wire-order hashes) and their coinbase txids."""

    def __init__(self, node):
        self.node = node
        self.hashes = [ser_hash(node.rpc("getblockhash", h)) for h in range(node.rpc("getblockcount") + 1)]

    def coinbase_txid(self, height):
        return ser_hash(self.node.rpc("getblock", self.hashes[height][::-1].hex(), 1)["tx"][0])


@contextlib.contextmanager
def scenario(out, name, index, notes, mine=0, extra=(), setup=None):
    """A fresh node that mines `mine` blocks at SIM_START (or runs `setup(node)`, which mines)
    before its transcript starts; the fixture records them as `submitblock` lines (not P2a's
    `chain` headers) and the dashd sha256."""
    if p3.dashd_sha256() != p3.DASHD_SHA256:
        sys.exit(f"dashd is not v24.0.0-rc.3 ({p3.DASHD_SHA256})")
    node = p2a.Node(name, index, SIM_START, extra)
    try:
        if mine:
            node.mine(mine)
        if setup:
            notes = notes + setup(node)
            mine = node.rpc("getblockcount")
        t = p2a.Transcript(name, node, notes, ibd=not mine)
        chain = Chain(node)
        blocks = [f"submitblock {node.rpc('getblock', h[::-1].hex(), 0)}" for h in chain.hashes[1:]]
        t.lines = [l for l in t.lines if not l.startswith("chain ")]
        at = t.lines.index(f"ibd {int(not mine)}")
        t.lines[at:at] = blocks
        t.lines.insert(1, f"# dashd sha256 {p3.DASHD_SHA256}")
        yield t, chain
        t.write(out)
    finally:
        node.stop()


def peer(t, n):
    t.accept(n)
    t.handshake(n, FULL | BIT11)


def serve_blocks(out):
    with scenario(out, "serve-blocks", 0, [
            "20 blocks mined by the node at the mock time: out of IBD, CanDirectFetch holds."], 20) as (t, c):
        peer(t, 1)
        t.send(1, "getdata", getdata([(MSG_BLOCK, c.hashes[20])]))
        t.send(1, "getdata", getdata([(MSG_CMPCT_BLOCK, c.hashes[20])]))
        t.send(1, "getdata", getdata([(MSG_CMPCT_BLOCK, c.hashes[14])]))
        t.send(1, "getdata", getdata([(MSG_BLOCK, bytes([7]) * 32), (MSG_BLOCK, c.hashes[5])]))
        t.send(1, "getdata", getdata([(MSG_FILTERED_BLOCK, c.hashes[10])]))
        t.send(1, "getdata", getdata([(MSG_BLOCK, c.hashes[h]) for h in (1, 2, 3)]))


def serve_getblocks(out):
    with scenario(out, "serve-getblocks", 1, ["600 blocks mined by the node at the mock time."], 600) as (t, c):
        peer(t, 1)
        t.send(1, "getblocks", getblocks([c.hashes[0]]))
        t.send(1, "getdata", getdata([(MSG_BLOCK, c.hashes[500])]))
        t.send(1, "getblocks", getblocks([c.hashes[550]], c.hashes[560]))
        peer(t, 2)
        t.send(2, "getblocks", getblocks([c.hashes[0]] * 102))


def serve_filters(out):
    with scenario(out, "serve-filters", 2, ["10 blocks mined by the node at the mock time."], 10) as (t, c):
        peer(t, 1)
        t.send(1, "filterload", filterload(64, 5, [c.coinbase_txid(5)]))
        t.send(1, "getdata", getdata([(MSG_FILTERED_BLOCK, c.hashes[5])]))
        t.send(1, "getdata", getdata([(MSG_FILTERED_BLOCK, c.hashes[6])]))
        t.send(1, "filterclear")
        t.send(1, "getdata", getdata([(MSG_FILTERED_BLOCK, c.hashes[5])]))
        peer(t, 2)
        t.send(2, "filteradd", filteradd(520))
        peer(t, 3)
        t.send(3, "filterload", filterload(36000, 50))
        t.send(3, "filteradd", filteradd(520))
        peer(t, 4)
        t.send(4, "filterload", filterload(10, 51))
        peer(t, 5)
        t.send(5, "filterload", filterload(36001, 1))
        peer(t, 6)
        t.send(6, "filteradd", filteradd(521))


def serve_nobloom(out):
    with scenario(out, "serve-nobloom", 3, ["-peerbloomfilters=0."], 0, ["-peerbloomfilters=0"]) as (t, _):
        for n, (cmd, payload) in enumerate([("filterload", filterload(10, 1)),
                                            ("filteradd", filteradd(8)), ("filterclear", b"")], 1):
            peer(t, n)
            t.send(n, cmd, payload)


def serve_getblocktxn(out):
    with scenario(out, "serve-getblocktxn", 4, ["20 blocks mined by the node at the mock time."], 20) as (t, c):
        peer(t, 1)
        t.send(1, "getblocktxn", getblocktxn(c.hashes[20], [0]))
        t.send(1, "getblocktxn", getblocktxn(c.hashes[10], [0]))
        t.send(1, "getblocktxn", getblocktxn(c.hashes[9], [0]))
        t.send(1, "getblocktxn", getblocktxn(bytes([7]) * 32, [0]))
        t.send(1, "getblocktxn", getblocktxn(c.hashes[20], [0, 1]))


def serve_limited(out):
    with scenario(out, "serve-limited", 5, ["-prune=1: manual pruning, NODE_NETWORK off, nothing pruned."],
                  300, ["-prune=1"]) as (t, c):
        peer(t, 1)
        t.send(1, "getdata", getdata([(MSG_BLOCK, c.hashes[10])]))
        t.send(1, "getdata", getdata([(MSG_BLOCK, c.hashes[9])]))


def key_id(node, address):
    """The 20-byte key ID of a P2PKH `address` (its scriptPubKey's push)."""
    script = node.rpc("getaddressinfo", address)["scriptPubKey"]
    assert script.startswith("76a914") and script.endswith("88ac"), script
    return bytes.fromhex(script[6:46])


def protx_setup(node):
    """Mines 510 blocks to the wallet (past DIP3 enforcement), then one block each holding a
    ProRegTx (`protx register_fund`), a ProUpRegTx, a ProUpServTx and a ProUpRevTx for it.
    Returns the notes naming them; `node.protx` keeps the heights and the owner's key ID."""
    node.rpc("createwallet", "w")
    addr = node.rpc("getnewaddress")
    node.rpc("generatetoaddress", 510, addr)
    owner, bls = node.rpc("getnewaddress"), node.rpc("bls", "generate")
    port = p2a.PORT_BASE + 98
    steps = [("register_fund", lambda: node.rpc(
        "protx", "register_fund", node.rpc("getnewaddress"), f"127.0.0.1:{port}", owner,
        bls["public"], node.rpc("getnewaddress"), 0, node.rpc("getnewaddress"), addr))]
    heights = {}
    protx = None
    for name, call in steps + [
            ("update_registrar", lambda: node.rpc("protx", "update_registrar", protx, "",
                                                  node.rpc("getnewaddress"), node.rpc("getnewaddress"), addr)),
            ("update_service", lambda: node.rpc("protx", "update_service", protx, f"127.0.0.1:{port + 1}",
                                                bls["secret"], "", addr)),
            ("revoke", lambda: node.rpc("protx", "revoke", protx, bls["secret"], 0, addr))]:
        txid = call()
        protx = protx or txid
        heights[name] = node.rpc("getblock", node.rpc("generatetoaddress", 1, addr)[0])["height"]
        assert txid in node.rpc("getblock", node.rpc("getblockhash", heights[name]))["tx"], name
    node.protx = {"heights": heights, "owner": key_id(node, owner)}
    return [f"{name} ({'ProRegTx' if name == 'register_fund' else 'its update'}) mined at {h}"
            for name, h in heights.items()] + [f"ProRegTx {protx}; owner key ID {node.protx['owner'].hex()}."]


def serve_filters_protx(out):
    with scenario(out, "serve-filters-protx", 6, [
            "510 blocks mined to the node's wallet, then the masternode's special transactions:"],
            setup=protx_setup) as (t, c):
        info = t.node.protx
        blocks = [c.hashes[h] for h in info["heights"].values()]
        # BLOOM_UPDATE_ALL (1): the ProRegTx matches on its owner key ID and inserts its hash, so
        # each update matches on the ProTx hash. BLOOM_UPDATE_NONE (0): the updates don't match.
        for n, flags in ((1, 1), (2, 0)):
            peer(t, n)
            t.send(n, "filterload", filterload(256, 11, [info["owner"]], tweak=n, flags=flags))
            for b in blocks:
                t.send(n, "getdata", getdata([(MSG_FILTERED_BLOCK, b)]))


SCENARIOS = {
    "serve-blocks": serve_blocks,
    "serve-getblocks": serve_getblocks,
    "serve-filters": serve_filters,
    "serve-nobloom": serve_nobloom,
    "serve-getblocktxn": serve_getblocktxn,
    "serve-limited": serve_limited,
    "serve-filters-protx": serve_filters_protx,
}


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    out = pathlib.Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=True)
    for name in sys.argv[2:] or SCENARIOS:
        SCENARIOS[name](out)


if __name__ == "__main__":
    main()
