#!/usr/bin/env python3
"""Captures BL-503's `getblockfrompeer` dashd transcript fixtures (`FetchBlock`,
net_processing.cpp:2057-2089) from regtest dashd v24.0.0-rc.5.

The format, the node setup, the peers and the synchronisation are P2a-core's
(`contrib/node/p2a-transcripts/capture.py`, whose docstring defines them), with P3's all-peer
quiet wait and helpers (`contrib/node/p3-transcripts/capture.py`). This tool adds two records:

  getblockfrompeer HASH PEER = RESULT
                  input: the tool called dashd's `getblockfrompeer HASH ID` and dashd answered
                  RESULT, `{}` or the error's message. HASH is in RPC (display) order. PEER is
                  `N` (scripted peer N, whose dashd id the tool read from `getpeerinfo`'s
                  `addr` while it was connected), `raw:ID` (the id as given), or `next` (one
                  past the highest scripted peer's id; every connection dashd has is scripted,
                  so it is dashd's next id). Only calls that reach `FetchBlock` are
                  recorded (the header is known and has no data)
  inflight N HEIGHTS
                  check: `getpeerinfo`'s `inflight` for peer N, comma-separated, `-` for none

Every scenario runs on a node that mined 4 blocks at SIM_START (chain A, out of IBD). Its
scripted peers are V1 without `NODE_HEADERS_COMPRESSED`; outbound ones are dialled by
`addconnection`. Fork F is 3 blocks on genesis, less work than A, from a second regtest dashd at
the same mock time that mined them to `raw(52)` (`fork_blocks`), so they differ from A's:

  fetch-fork         peer 1 announces F by `headers`; F3 is fetched from it (`{}` and one
                     `getdata(MSG_BLOCK)`); the peer sends F3; nothing is left in flight
  fetch-twice        F3 is fetched from peer 1 twice (`{}`, `{}`, two `getdata`s), then from peer 2,
                     which takes the request over (`FetchBlock`'s remove-all) and serves it
  fetch-unconnected  inbound peer 3 has sent nothing: "Peer not fully connected", no frame, and
                     the entry kept; peer 3 completes its handshake and at +151 s dashd times it
                     out (the block download timeout, 150 s for one downloading peer)
  fetch-nopeer       with F3 in flight from peer 1: `raw:-1`, `next`, and peer 2 once closed
                     and finalized all answer "Peer does not exist"; peer 1's entry is untouched
  fetch-timeout      peer 1 answers the `getdata` with `notfound`, which clears nothing; at
                     +151 s dashd times it out

The tool refuses to run unless dashd's sha256 is DASHD_SHA256 and records it in each fixture's
header.

Usage:
  capture.py OUT_DIR [SCENARIO...]

`P2A_DASHD` defaults to ~/.rust-dashcore-test/dashcore-24.0.0-rc.5/bin/dashd (fetched by
`DASHVERSION=24.0.0-rc.5 contrib/setup-dashd.py`); `P2A_CAPTURE_DIR` defaults to
/work/scratch/p2a-net/bl503/capture and `P2A_PORT_BASE` to 32700. It never touches /work/chains
or /work/bench.
"""

import contextlib
import hashlib
import importlib.util
import os
import pathlib
import re
import struct
import sys
import time

os.environ.setdefault("P2A_CAPTURE_DIR", "/work/scratch/p2a-net/bl503/capture")
os.environ.setdefault("P2A_PORT_BASE", "32700")
os.environ.setdefault("P2A_DASHD",
                      str(pathlib.Path.home() / ".rust-dashcore-test/dashcore-24.0.0-rc.5/bin/dashd"))

_spec = importlib.util.spec_from_file_location(
    "p3_capture", pathlib.Path(__file__).resolve().parent.parent / "p3-transcripts" / "capture.py")
p3 = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(p3)
p2a = p3.p2a

SIM_START, FULL = p2a.SIM_START, p2a.FULL
DASHD_SHA256 = "fa6f61de98950d17adc1065155f783b189bc40b4694534df85354511bc8b8a3c"
MSG_BLOCK = 2
PREMINE = 4


def fork_blocks():
    """F: 3 blocks on genesis as (display hash, raw block), mined to `raw(52)` by a separate
    regtest dashd at SIM_START."""
    cache = p2a.ROOT / f"fork-3-{DASHD_SHA256[:16]}.txt"
    if not cache.exists():
        node = p2a.Node("miner-fork", 9, SIM_START)
        try:
            hashes = node.rpc("generatetodescriptor", 3, "raw(52)")
            cache.write_text("".join(f"{h} {node.rpc('getblock', h, 0)}\n" for h in hashes))
        finally:
            node.stop()
    return [(h, bytes.fromhex(raw)) for h, raw in (l.split() for l in cache.read_text().split("\n") if l)]


class Fetches:
    """The two records over a transcript: peer ids from `getpeerinfo`, kept after a close."""

    def __init__(self, t):
        self.t = t
        self.ids = {}

    def _scan(self):
        info = self.t.node.rpc("getpeerinfo")
        for n, p in self.t.peers.items():
            if n in self.ids or p.closed:
                continue
            addr = "127.0.0.1:%d" % p.sock.getsockname()[1]
            match = [i["id"] for i in info if i["addr"] == addr]
            if match:
                self.ids[n] = match[0]
        return info

    def peer_id(self, peer):
        self._scan()
        if peer == "next":
            return max(self.ids.values()) + 1
        if peer.startswith("raw:"):
            return int(peer[4:])
        return self.ids[int(peer)]

    def fetch(self, block_hash, peer, want):
        """`getblockfrompeer`, recorded with dashd's answer, which must be `want`."""
        peer_id = self.peer_id(peer)
        try:
            got = self.t.node.rpc("getblockfrompeer", block_hash, peer_id)
            result = "{}" if got == {} else repr(got)
        except RuntimeError as e:
            m = re.search(r"'message': '([^']*)'", str(e))
            assert m, str(e)
            result = m.group(1)
        assert result == want, f"getblockfrompeer {block_hash} {peer}: {result}, not {want}"
        self.t.lines.append(f"getblockfrompeer {block_hash} {peer} = {result}")
        self.t.settle()

    def inflight(self, n, want):
        """The `inflight` check for peer `n`, which must be `want` (heights)."""
        info = {i["id"]: i for i in self._scan()}
        got = info[self.ids[n]]["inflight"]
        assert got == want, f"inflight {n}: {got}, not {want}"
        self.t.lines.append(f"inflight {n} {','.join(map(str, got)) or '-'}")


@contextlib.contextmanager
def scenario(out, name, index, notes):
    """A fresh node that mined PREMINE blocks at SIM_START (out of IBD); the fixture records the
    dashd sha256."""
    node = p2a.Node(name, index, SIM_START)
    try:
        node.mine(PREMINE)
        t = p2a.Transcript(name, node, notes, ibd=False)
        t.lines.insert(1, f"# dashd sha256 {DASHD_SHA256}")
        yield t, Fetches(t)
        t.write(out)
    finally:
        node.stop()


def announce_fork(t, n, fork):
    t.send(n, "headers", p2a.headers_payload([raw[:80] for _, raw in fork]))


def connected(t, n):
    t.connect(n, "outbound-full-relay")
    t.handshake(n, FULL)


def debug_log(t):
    return (t.node.dir / "regtest" / "debug.log").read_text(errors="replace")


def close(t, f, n):
    """The tool closes peer `n`, then waits until dashd has finalized it (`FinalizeNode`'s
    "Cleared nodestate" line)."""
    peer_id = f.peer_id(str(n))
    p = t.peers[n]
    p.sock.close()
    p.closed = True
    t.lines.append(f"close {n}")
    end = time.time() + 120
    while f"Cleared nodestate for peer={peer_id}\n" not in debug_log(t) and time.time() < end:
        time.sleep(0.05)
    assert f"Cleared nodestate for peer={peer_id}\n" in debug_log(t), "dashd kept the closed peer"
    t.settle()


NOTES_FORK = ("Fork F (3 blocks on genesis, less work than the node's 4) comes from a separate "
              "regtest dashd v24 at SIM_START mining to raw(52).")


def scenario_fork(out, index):
    fork = fork_blocks()
    f3 = fork[2][0]
    with scenario(out, "fetch-fork", index, [
            "The node mined 4 blocks (out of IBD). Outbound peer 1 announces fork F by headers;",
            "getblockfrompeer F3 from peer 1 answers {} and dashd sends getdata(MSG_BLOCK F3); the",
            "peer sends F3 (stored, the tip stays); nothing is left in flight.", NOTES_FORK]) as (t, f):
        connected(t, 1)
        announce_fork(t, 1, fork)
        f.fetch(f3, "1", "{}")
        t.send(1, "block", fork[2][1])
        f.inflight(1, [])
        assert t.node.rpc("getblockcount") == PREMINE


def scenario_twice(out, index):
    fork = fork_blocks()
    f3 = fork[2][0]
    with scenario(out, "fetch-twice", index, [
            "Outbound peers 1 and 2; peer 1 announces fork F. F3 is fetched from peer 1 twice ({},",
            "{}, two getdatas: the remove-all erases peer 1's own entry first), then from peer 2,",
            "which takes the only request; peer 2 serves it.", NOTES_FORK]) as (t, f):
        connected(t, 1)
        connected(t, 2)
        announce_fork(t, 1, fork)
        f.fetch(f3, "1", "{}")
        f.fetch(f3, "1", "{}")
        f.inflight(1, [3])
        f.fetch(f3, "2", "{}")
        f.inflight(1, [])
        f.inflight(2, [3])
        t.send(2, "block", fork[2][1])
        f.inflight(2, [])


def scenario_unconnected(out, index):
    fork = fork_blocks()
    f3 = fork[2][0]
    with scenario(out, "fetch-unconnected", index, [
            "Outbound peer 1 announces fork F. Inbound peer 3 connects and sends nothing: fetching",
            "F3 from it answers \"Peer not fully connected\" and sends nothing, but the request is",
            "recorded. Peer 3 then completes its handshake; at +151 s (past 150 s, one downloading",
            "peer) dashd disconnects it for the block download timeout.", NOTES_FORK]) as (t, f):
        connected(t, 1)
        announce_fork(t, 1, fork)
        t.accept(3)
        f.fetch(f3, "3", "Peer not fully connected")
        f.inflight(3, [3])
        p3.handshake_inbound(t, 3)
        f.inflight(3, [3])
        t.set_time(SIM_START + 151)
        assert t.peers[3].closed, "peer 3 was not timed out"
        assert f"Timeout downloading block {f3} from peer=" in debug_log(t)


def scenario_nopeer(out, index):
    fork = fork_blocks()
    f3 = fork[2][0]
    with scenario(out, "fetch-nopeer", index, [
            "Outbound peers 1 and 2; peer 1 announces fork F and F3 is fetched from it. The tool",
            "closes peer 2 and waits until dashd finalized it. Fetching F3 from id -1, from one",
            "past the highest id, and from peer 2 each answers \"Peer does not exist\" before the",
            "remove-all, so peer 1's request stays.", NOTES_FORK]) as (t, f):
        connected(t, 1)
        connected(t, 2)
        f.peer_id("2")
        announce_fork(t, 1, fork)
        f.fetch(f3, "1", "{}")
        close(t, f, 2)
        for peer in ("raw:-1", "next", "2"):
            f.fetch(f3, peer, "Peer does not exist")
        f.inflight(1, [3])


def scenario_timeout(out, index):
    fork = fork_blocks()
    f3 = fork[2][0]
    with scenario(out, "fetch-timeout", index, [
            "Outbound peer 1 announces fork F; F3 is fetched from it, and the peer answers the",
            "getdata with notfound, which clears no block request; at +151 s dashd disconnects it",
            "for the block download timeout.", NOTES_FORK]) as (t, f):
        connected(t, 1)
        announce_fork(t, 1, fork)
        f.fetch(f3, "1", "{}")
        inv = p2a.compact(1) + struct.pack("<I", MSG_BLOCK) + bytes.fromhex(f3)[::-1]
        t.send(1, "notfound", inv)
        f.inflight(1, [3])
        t.set_time(SIM_START + 151)
        assert t.peers[1].closed, "peer 1 was not timed out"


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    got = hashlib.sha256(pathlib.Path(p2a.DASHD).read_bytes()).hexdigest()
    if got != DASHD_SHA256:
        sys.exit(f"{p2a.DASHD}: sha256 {got}, not the release dashd v24.0.0-rc.5 ({DASHD_SHA256})")
    out = sys.argv[1]
    want = set(sys.argv[2:])
    pathlib.Path(out).mkdir(parents=True, exist_ok=True)
    p2a.ROOT.mkdir(parents=True, exist_ok=True, mode=0o700)
    jobs = [
        ("fetch-fork", lambda o: scenario_fork(o, 0)),
        ("fetch-twice", lambda o: scenario_twice(o, 1)),
        ("fetch-unconnected", lambda o: scenario_unconnected(o, 2)),
        ("fetch-nopeer", lambda o: scenario_nopeer(o, 3)),
        ("fetch-timeout", lambda o: scenario_timeout(o, 4)),
    ]
    for name, job in jobs:
        if not want or name in want:
            print(f"== {name}", flush=True)
            job(out)


if __name__ == "__main__":
    main()
