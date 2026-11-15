#!/usr/bin/env python3
"""Captures P3's block-download dashd transcript fixtures (P3 §6.3 items 1-2) from regtest dashd
v24.0.0-rc.3.

The format, the node setup, the peers and the synchronisation are P2a-core's
(`contrib/node/p2a-transcripts/capture.py`, whose docstring defines them), with P2b's all-peer
quiet wait (`contrib/node/p2b-transcripts/capture.py`); this tool adds scenarios whose peers serve
blocks:

  blocks-1100   one outbound peer answers the node's `getheaders` with 1100 headers, then serves
                every block the node asks for
  blocks-stall  two outbound peers; peer 1 withholds the first block it is asked for until the
                1024-block window blocks, "Stall started", and dashd disconnects it once the mock
                time passes the 2 s stalling timeout; peer 2 then serves that block and the rest

The peers are reactive in the capture but scripted in the fixture: the tool keeps a queue of the
blocks dashd asked for (`getdata` of `MSG_BLOCK`), in the order the requests arrived, and sends
the head of the queue as one input (`> N block HEX`), so each block opens its own segment and the
replay delivers the same blocks in the same order. dashd processes a `block` and runs
`SendMessages` for that peer before it reads the tool's sync `ping` (net.cpp
ThreadMessageHandler: one message, then SendMessages, per peer per turn), so the `getdata` a
block causes is in its segment however short the quiet wait; the tool therefore
waits only BLOCK_QUIET (P3_BLOCK_QUIET, default 0.3 s) after a block, and P2a's QUIET after every
other input.

The blocks come from a second regtest dashd (`miner_blocks`) whose mock time is 30 days before
SIM_START, so the whole chain is older than dashd's maximum tip age: the node stays in IBD to the
end and `CanDirectFetch` stays false, so every request is section 12's `MSG_BLOCK` (no
`MSG_CMPCT_BLOCK`, which is S3's).

S3 adds two scenarios on a node out of IBD (P3 §6.3 items 3 and 5):

  cmpct-swap     an outbound peer that sent `sendcmpct(0, 1)` announces a block on the tip by
                 `headers`; dashd asks for it by `getdata(MSG_CMPCT_BLOCK)` (the lone direct
                 fetch's swap), and the peer's `cmpctblock` with the coinbase prefilled connects it
  cmpct-buckets  the bucket-table transition sets of §2.3.2: for every n in 13..=65535 next to a
                 change of libstdc++'s bucket count (n-1, n, n+1), a fresh inbound peer sends a
                 `cmpctblock` for one block on the tip whose n short IDs put 13 (or 12) in one
                 bucket, and dashd answers `getdata(MSG_BLOCK)` (`READ_STATUS_FAILED`) or
                 `getblocktxn`; then the tool closes the peer and waits until dashd dropped it

S4 adds one scenario (P3 §6.3 item 4's announcement half, and the encoder's Core vector):

  cmpct-announce  a node that mined 103 blocks to OP_TRUE; an outbound peer sends
                  `sendcmpct(1, 1)` and a `getheaders` from the tip, so dashd has sent it the
                  tip; then `generateblock` mines a block spending three coinbases, and dashd's
                  `NewPoWValidBlock` sends the peer that block's `cmpctblock` (a fresh nonce, the
                  coinbase prefilled, three short IDs). The record `submitblock HEX` stands for
                  the RPC, with the block's bytes

The block on the tip comes from a second regtest dashd (`lacking_block`) that is given the node's
chain and mines one block more. P3's records (`dashd_p2p::sim::transcript`'s docs): `close N`;
`> N cmpctblock gen:crowded:HEADER:N:CROWD:BUCKETS sha256:HEX`, the short IDs
`crowded(n, crowd, b)` (the CROWD values `1 + k * BUCKETS`, then the first `n - CROWD` of
`0..BUCKETS` other than 1) with nonce 0 and nothing prefilled, BUCKETS being `bucket_count(n)` as
read from `dashd-p2p/src/processing/compact.rs` (written out so that the replay builds the payload
without the code under test, and dashd's answers check it); and, in cmpct-buckets,
`< N CMD sha256:HEX:LEN` for dashd's payloads over DIGEST_OVER bytes. No transaction reaches
dashd in either scenario.

The tool refuses to run unless dashd's sha256 is DASHD_SHA256 (P3 D31) and records it in each
fixture's header.

Usage:
  capture.py OUT_DIR [SCENARIO...]

`P2A_DASHD` defaults to ~/.rust-dashcore-test/dashcore-24.0.0-rc.3/bin/dashd;
`P2A_CAPTURE_DIR` defaults to /work/scratch/p3/capture and `P2A_PORT_BASE` to 31900. It never
touches /work/chains or /work/bench.
"""

import bisect
import collections
import contextlib
import hashlib
import importlib.util
import os
import pathlib
import re
import select
import struct
import sys
import time

os.environ.setdefault("P2A_CAPTURE_DIR", "/work/scratch/p3/capture")
os.environ.setdefault("P2A_PORT_BASE", "31900")
os.environ.setdefault("P2A_DASHD",
                      str(pathlib.Path.home() / ".rust-dashcore-test/dashcore-24.0.0-rc.3/bin/dashd"))

_spec = importlib.util.spec_from_file_location(
    "p2a_capture", pathlib.Path(__file__).resolve().parent.parent / "p2a-transcripts" / "capture.py")
p2a = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(p2a)

SIM_START, FULL = p2a.SIM_START, p2a.FULL
DASHD_SHA256 = "e204e3163185b1bffcd3bf3532c0957d8cc11019eaf8f3dbc3b2fc82a7ab5bb4"
BLOCK_QUIET = float(os.environ.get("P3_BLOCK_QUIET", "0.3"))
# The miner's clock: 30 days before SIM_START, older than any maximum tip age.
MINER_AGE = 30 * 24 * 3600
MSG_BLOCK = 2
MSG_CMPCT_BLOCK = 20
NUM_BLOCKS = 1100
# cmpct-buckets' largest payload kept as hex; the rest are recorded by digest.
DIGEST_OVER = 4096
COMPACT_RS = pathlib.Path(__file__).resolve().parents[3] / "dashd-p2p/src/processing/compact.rs"


def settle(self, quiet=None):
    """P2b's all-peer settle (dashd sent nothing to any peer for `quiet` seconds, then a sync
    ping to every handshaken peer), with the quiet period a parameter."""
    quiet = p2a.QUIET if quiet is None else quiet
    while True:
        live = [p for p in self.peers.values() if not p.closed]
        ready, _, _ = select.select([p.sock for p in live], [], [], quiet) if live else ([], [], [])
        if not ready:
            break
        for p in live:
            if p.sock in ready:
                self._record(p, p.poll(0.2))
    for p in self.peers.values():
        if p.closed or not p.handshaken:
            continue
        p.send("ping", p.sync_pong)
        end = time.time() + 120
        done = False
        while not done and not p.closed and time.time() < end:
            msgs = p.poll(0.5)
            done = ("pong", p.sync_pong) in msgs
            self._record(p, msgs)
        if not done and not p.closed:
            raise RuntimeError(f"{self.name}: peer {p.n} sync ping not answered")


p2a.Transcript.settle = settle
_record = p2a.Transcript._record


def record(self, peer, msgs):
    """P2a's record, with payloads over the transcript's `digest_over` by their sha256."""
    over = getattr(self, "digest_over", None)
    for cmd, payload in msgs:
        if over is not None and payload is not None and len(payload) > over:
            self.lines.append(f"< {peer.n} {cmd} sha256:{hashlib.sha256(payload).hexdigest()}:{len(payload)}")
        else:
            _record(self, peer, [(cmd, payload)])


p2a.Transcript._record = record


def dashd_sha256():
    return hashlib.sha256(pathlib.Path(p2a.DASHD).read_bytes()).hexdigest()


def miner_blocks(n):
    """`n` blocks as (wire-order hash, raw block, raw header), mined by a separate regtest dashd
    with its mock time MINER_AGE before SIM_START."""
    cache = p2a.ROOT / f"blocks-{n}-{MINER_AGE}-{DASHD_SHA256[:16]}.txt"
    if not cache.exists():
        node = p2a.Node("miner", 9, SIM_START - MINER_AGE)
        try:
            lines = []
            for height in range(1, len(node.mine(n)) + 1):
                h = node.rpc("getblockhash", height)
                lines.append(f"{h} {node.rpc('getblock', h, 0)}")
        finally:
            node.stop()
        cache.write_text("\n".join(lines) + "\n")
    out = []
    for line in cache.read_text().split("\n"):
        if line:
            h, raw = line.split()
            raw = bytes.fromhex(raw)
            out.append((bytes.fromhex(h)[::-1], raw, raw[:80]))
    return out


@contextlib.contextmanager
def scenario(out, name, index, notes):
    """A fresh genesis-only node in IBD; the fixture records the dashd sha256."""
    node = p2a.Node(name, index, SIM_START)
    try:
        t = p2a.Transcript(name, node, notes, ibd=True)
        t.lines.insert(1, f"# dashd sha256 {DASHD_SHA256}")
        yield t
        t.write(out)
    finally:
        node.stop()


def block_requests(payload):
    """The hashes of a `getdata`'s entries, which must all be `MSG_BLOCK`."""
    n = payload[0]
    assert n < 0xFD and len(payload) == 1 + 36 * n, "a short inventory list"
    out = []
    for i in range(n):
        inv_type, = struct.unpack_from("<I", payload, 1 + 36 * i)
        assert inv_type == MSG_BLOCK, f"getdata of type {inv_type}: only MSG_BLOCK expected"
        out.append(payload[5 + 36 * i:37 + 36 * i])
    return out


class Server:
    """The scripted peers' block service: the requests dashd made, in order, served one block per
    input, except those `withhold` says to keep."""

    def __init__(self, t, blocks):
        self.t = t
        self.blocks = {h: raw for h, raw, _ in blocks}
        self.queue = collections.deque()
        self.seen = 0
        self.withheld = []

    def scan(self):
        """Queues the `getdata`s recorded since the last scan."""
        for line in self.t.lines[self.seen:]:
            m = re.fullmatch(r"< (\d+) getdata ([0-9a-f]*)", line)
            if m:
                for h in block_requests(bytes.fromhex(m.group(2))):
                    self.queue.append((int(m.group(1)), h))
        self.seen = len(self.t.lines)

    def serve(self, withhold=lambda n, h: False):
        """Serves the queue until it is empty; a request `withhold` names is kept back."""
        self.scan()
        while self.queue:
            n, h = self.queue.popleft()
            if self.t.peers[n].closed:
                continue
            if withhold(n, h):
                self.withheld.append((n, h))
                continue
            self.t.lines.append(f"> {n} block {self.blocks[h].hex()}")
            self.t.peers[n].send("block", self.blocks[h])
            self.t.settle(BLOCK_QUIET)
            self.scan()


def debug_log(t):
    return (t.node.dir / "regtest" / "debug.log").read_text(errors="replace")


def scenario_blocks(out, index):
    """P3 §6.3 item 1: one peer, 1100 blocks."""
    blocks = miner_blocks(NUM_BLOCKS)
    with scenario(out, "blocks-1100", index, [
            "One outbound peer without bit 11 answers the node's getheaders with 1100 headers, then",
            "serves each block the node asks for, one block per input, in the order asked. The",
            "blocks come from a separate regtest dashd v24 (miner, mock time SIM_START-2592000), so",
            "the node stays in IBD and never direct-fetches."]) as t:
        t.connect(1, "outbound-full-relay")
        t.handshake(1, FULL)
        t.send(1, "headers", p2a.headers_payload([hdr for _, _, hdr in blocks]))
        Server(t, blocks).serve()
        assert t.node.rpc("getblockcount") == NUM_BLOCKS, "the node did not sync"


def scenario_stall(out, index):
    """P3 §6.3 item 2: two peers, peer 1 withholding the first block it is asked for."""
    blocks = miner_blocks(NUM_BLOCKS)
    with scenario(out, "blocks-stall", index, [
            "Two outbound peers without bit 11. Peer 1 (the sync peer) sends the 1100 headers and",
            "the node asks it for blocks 1-16; then peer 2 sends them and is asked for 17-32. Both",
            "serve each block the node asks for, one block per input, in the order asked, except",
            "that peer 1 withholds block 1. Once the 1024-block window is all requested and peer 2",
            "has nothing in flight, dashd logs \"Stall started peer=<1>\" (not on the wire). The",
            "mock time moves +2 s (not yet past the 2 s stalling timeout), then +1 s more: dashd",
            "disconnects peer 1 and asks peer 2 for block 1, which it serves, and the rest.",
            "The blocks come from a separate regtest dashd v24 (miner, mock time SIM_START-2592000),",
            "so the node stays in IBD and never direct-fetches."]) as t:
        hdrs = p2a.headers_payload([hdr for _, _, hdr in blocks])
        t.connect(1, "outbound-full-relay")
        t.handshake(1, FULL)
        t.connect(2, "outbound-full-relay")
        t.handshake(2, FULL)
        t.send(1, "headers", hdrs)
        t.send(2, "headers", hdrs)
        server = Server(t, blocks)
        server.scan()
        first = next(h for n, h in server.queue if n == 1)
        assert first == blocks[0][0], "peer 1 is asked for block 1 first"
        server.serve(lambda n, h: n == 1 and h == first)
        assert server.withheld == [(1, first)], server.withheld
        assert "Stall started" in debug_log(t), "dashd did not log Stall started"
        t.set_time(SIM_START + 2)
        assert not t.peers[1].closed, "disconnected at the timeout, not past it"
        t.set_time(SIM_START + 3)
        assert t.peers[1].closed, "the staller was not disconnected"
        assert "is stalling block download, disconnecting" in debug_log(t)
        server.serve()
        assert t.node.rpc("getblockcount") == NUM_BLOCKS, "the node did not sync"


@contextlib.contextmanager
def out_of_ibd(out, name, index, notes, premine=1):
    """A fresh node that mined `premine` blocks at SIM_START (out of IBD); the fixture records the
    dashd sha256."""
    node = p2a.Node(name, index, SIM_START)
    try:
        node.mine(premine)
        t = p2a.Transcript(name, node, notes, ibd=False)
        t.lines.insert(1, f"# dashd sha256 {DASHD_SHA256}")
        yield t
        t.write(out)
    finally:
        node.stop()


def lacking_block(node):
    """A raw block on `node`'s tip that `node` has not seen: a second regtest dashd at the same
    mock time is given `node`'s blocks and mines one more."""
    miner = p2a.Node("miner-tip", 9, SIM_START)
    try:
        for height in range(1, node.rpc("getblockcount") + 1):
            raw = node.rpc("getblock", node.rpc("getblockhash", height), 0)
            assert miner.rpc("submitblock", raw) is None, "the miner took the node's block"
        miner.mine(1)
        return bytes.fromhex(miner.rpc("getblock", miner.rpc("getbestblockhash"), 0))
    finally:
        miner.stop()


def bucket_count(n, tables={}):
    """libstdc++'s `_M_next_bkt(n)` from the release's tables in compact.rs (P3 §2.3.2)."""
    if not tables:
        text = COMPACT_RS.read_text()
        for name in ("PRIME_LIST", "FAST_BKT"):
            body = re.search(name + r"[^=]*=\s*\[(.*?)\];", text, re.S).group(1)
            tables[name] = [int(x.replace("_", "")) for x in re.findall(r"[0-9_]+", body)]
    primes, fast = tables["PRIME_LIST"], tables["FAST_BKT"]
    if n == 0:
        return 1
    if n < len(fast):
        return fast[n]
    return primes[bisect.bisect_left(primes, n, 6, len(primes) - 2)]


def crowded(n, crowd, b):
    """`n` distinct short IDs, the first `crowd` in bucket 1 of `b`, the rest one per other bucket."""
    return [1 + k * b for k in range(crowd)] + [x for x in range(b) if x != 1][:n - crowd]


def crowded_cmpct(header, n, crowd, b):
    ids = b"".join(i.to_bytes(6, "little") for i in crowded(n, crowd, b))
    return header + bytes(8) + p2a.compact(n) + ids + p2a.compact(0)


def bucket_cases():
    """(n, crowd) for every n in 13..=65535 within one of a change of bucket count."""
    counts = [bucket_count(n) for n in range(65536)]
    changes = [n for n in range(1, 65536) if counts[n] != counts[n - 1]]
    ns = sorted({m for c in changes for m in (c - 1, c, c + 1) if 13 <= m <= 65535})
    return [(n, crowd) for n in ns for crowd in (13, 12)]


def last_from(t, n, since):
    """The commands and payloads dashd sent peer `n` in `t.lines[since:]`."""
    out = []
    for line in t.lines[since:]:
        f = line.split(" ")
        if f[0] == "<" and f[1] == str(n):
            out.append((f[2], f[3] if len(f) > 3 else ""))
    return out


def scenario_cmpct_swap(out, index):
    """P3 §6.3 item 3: the direct fetch's MSG_CMPCT_BLOCK swap."""
    with out_of_ibd(out, "cmpct-swap", index, [
            "The node mined 1 block before the peer connects, so it is out of IBD. An outbound",
            "peer without bit 11 sends sendcmpct(0, 1) and announces block 2 by headers; dashd asks",
            "for it by getdata(MSG_CMPCT_BLOCK), and the peer's cmpctblock (nonce 0, the coinbase",
            "prefilled, no short IDs) connects it. Block 2 comes from a separate regtest dashd v24",
            "given the node's block 1 (mock time SIM_START)."]) as t:
        block = lacking_block(t.node)
        assert block[80] == 1, "a coinbase-only block"
        t.connect(1, "outbound-full-relay")
        t.handshake(1, FULL)
        t.send(1, "sendcmpct", bytes([0]) + struct.pack("<Q", 1))
        since = len(t.lines)
        t.send(1, "headers", p2a.headers_payload([block[:80]]))
        gd = [p for c, p in last_from(t, 1, since) if c == "getdata"]
        assert len(gd) == 1 and gd[0][:10] == "01" + struct.pack("<I", MSG_CMPCT_BLOCK).hex(), gd
        cmpct = block[:80] + bytes(8) + p2a.compact(0) + p2a.compact(1) + p2a.compact(0) + block[81:]
        t.send(1, "cmpctblock", cmpct)
        assert t.node.rpc("getblockcount") == 2, "the compact block did not connect"


def op_true_spend(node, height):
    """A version 1 transaction spending the OP_TRUE output of block `height`'s coinbase with an
    empty scriptSig, paying all but 1000 duffs to OP_TRUE."""
    cb = node.rpc("getblock", node.rpc("getblockhash", height), 2)["tx"][0]
    (n, out), = [(i, o) for i, o in enumerate(cb["vout"]) if o["scriptPubKey"]["hex"] == "51"]
    value = out["valueSat"] - 1000
    return (struct.pack("<I", 1) + p2a.compact(1) + bytes.fromhex(cb["txid"])[::-1]
            + struct.pack("<I", n) + p2a.compact(0) + b"\xff" * 4 + p2a.compact(1)
            + struct.pack("<q", value) + p2a.compact(1) + b"\x51" + bytes(4)).hex()


def scenario_cmpct_announce(out, index):
    """P3 §6.3 item 4: `NewPoWValidBlock`'s `cmpctblock` to a high-bandwidth peer."""
    with out_of_ibd(out, "cmpct-announce", index, [
            "The node mined 103 blocks to OP_TRUE before the peer connects, so it is out of IBD and",
            "coinbases 1-3 are mature. An outbound peer sends sendcmpct(1, 1) and a getheaders from",
            "the tip (dashd sends no headers, and the tip becomes its pindexBestHeaderSent). Then",
            "the RPC generateblock mines a block spending those coinbases (`submitblock` records its",
            "bytes), and dashd's NewPoWValidBlock sends the peer its cmpctblock."], premine=103) as t:
        spends = [op_true_spend(t.node, h) for h in (1, 2, 3)]
        t.connect(1, "outbound-full-relay")
        t.handshake(1, FULL)
        t.send(1, "sendcmpct", bytes([1]) + struct.pack("<Q", 1))
        tip = bytes.fromhex(t.node.rpc("getbestblockhash"))[::-1]
        t.send(1, "getheaders", struct.pack("<I", 70242) + p2a.compact(1) + tip + bytes(32))
        mined = t.node.rpc("generateblock", "raw(51)", spends)["hash"]
        # dashd sent the cmpctblock inside the RPC; the tool reads it at the settle below.
        t.lines.append(f"submitblock {t.node.rpc('getblock', mined, 0)}")
        t.settle()
        assert t.node.rpc("getbestblockhash") == mined
        sent = [l.split()[2] for l in t.lines if l.startswith("< 1 ")]
        assert "cmpctblock" in sent, sent


def handshake_inbound(t, n):
    """`version`, then dashd's `version`/`sendaddrv2`/`verack` waited for (the short quiet
    alone would race them), then `verack`."""
    p = t.peers[n]
    payload = p2a.version_payload(FULL, p2a.VERSION_NONCE + n)
    t.lines.append(f"> {n} version {payload.hex()}")
    p.send("version", payload)
    end = time.time() + 120
    while not p.got_verack and not p.closed and time.time() < end:
        t._record(p, p.poll(0.5))
    assert p.got_verack, f"peer {n}: no verack from dashd"
    t.settle()
    t.send(n, "verack")


def close(t, n):
    """The tool closes peer `n`, then waits until dashd has finalized it."""
    p = t.peers[n]
    p.sock.close()
    p.closed = True
    t.lines.append(f"close {n}")
    end = time.time() + 120
    while t.node.rpc("getconnectioncount") and time.time() < end:
        time.sleep(0.05)
    assert t.node.rpc("getconnectioncount") == 0, "dashd kept the closed peer"


def scenario_cmpct_buckets(out, index):
    """P3 §6.3 item 5: the bucket-table transition sets, each from a fresh inbound peer."""
    cases = bucket_cases()
    with out_of_ibd(out, "cmpct-buckets", index, [
            "The node mined 1 block before the peers connect, so it is out of IBD. For each case a",
            "fresh inbound peer without bit 11 handshakes, sends a cmpctblock for block 2 (from a",
            "separate regtest dashd v24 given the node's block 1) with n short IDs, 13 or 12 of them",
            "in one libstdc++ bucket, and is closed by the tool; dashd answers getdata(MSG_BLOCK)",
            "for 13 (READ_STATUS_FAILED) and getblocktxn for 12. Peer numbers cycle through",
            f"1-200. {len(cases)} cases; payloads over {DIGEST_OVER} bytes by their sha256."]) as t:
        header = lacking_block(t.node)[:80]
        quiet, p2a.QUIET = p2a.QUIET, BLOCK_QUIET
        t.digest_over = DIGEST_OVER
        try:
            for i, (n, crowd) in enumerate(cases):
                peer = 1 + i % 200
                t.accept(peer)
                handshake_inbound(t, peer)
                b = bucket_count(n)
                payload = crowded_cmpct(header, n, crowd, b)
                since = len(t.lines)
                t.lines.append(f"> {peer} cmpctblock "
                               f"gen:crowded:{header.hex()}:{n}:{crowd}:{b} "
                               f"sha256:{hashlib.sha256(payload).hexdigest()}")
                t.peers[peer].send("cmpctblock", payload)
                t.settle()
                got = [c for c, _ in last_from(t, peer, since)]
                want = "getdata" if crowd == 13 else "getblocktxn"
                assert got == [want], f"n={n} crowd={crowd}: dashd sent {got}"
                close(t, peer)
                if i % 50 == 0:
                    print(f"  {i}/{len(cases)} n={n}", flush=True)
        finally:
            p2a.QUIET = quiet


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    got = dashd_sha256()
    if got != DASHD_SHA256:
        sys.exit(f"{p2a.DASHD}: sha256 {got}, not the release dashd v24.0.0-rc.3 ({DASHD_SHA256})")
    out = sys.argv[1]
    want = set(sys.argv[2:])
    pathlib.Path(out).mkdir(parents=True, exist_ok=True)
    p2a.ROOT.mkdir(parents=True, exist_ok=True, mode=0o700)
    jobs = [
        ("blocks-1100", lambda o: scenario_blocks(o, 0)),
        ("blocks-stall", lambda o: scenario_stall(o, 1)),
        ("cmpct-swap", lambda o: scenario_cmpct_swap(o, 2)),
        ("cmpct-buckets", lambda o: scenario_cmpct_buckets(o, 3)),
        ("cmpct-announce", lambda o: scenario_cmpct_announce(o, 4)),
    ]
    for name, job in jobs:
        if not want or name in want:
            print(f"== {name}", flush=True)
            job(out)


if __name__ == "__main__":
    main()
