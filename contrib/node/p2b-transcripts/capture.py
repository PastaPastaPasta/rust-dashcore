#!/usr/bin/env python3
"""Captures P2b's inbound dashd transcript fixtures from regtest dashd v24.0.0-rc.3.

The format, the node setup, the peers and the synchronisation are P2a-core's
(`contrib/node/p2a-transcripts/capture.py`, whose docstring defines them); this tool only adds
inbound scenarios: the gates before `version` and `verack`, the responder handshake against
peers of other versions and services, `getheaders`/`getheaders2` serving (also below the
minimum chain work, with and without the download permission), and inbound eviction at a full
inbound table. Each writes `dashd-p2p/tests/fixtures/dashd-v24/<scenario>.txt`, which
`dashd-p2p/tests/dashd_inbound_transcripts.rs` replays.

Usage:
  P2A_DASHD=/path/to/dashd-24.0.0-rc.3/bin/dashd capture.py OUT_DIR [SCENARIO...]

`P2A_CAPTURE_DIR` defaults to /work/scratch/p2b-inbound/capture here; `P2A_PORT_BASE`
(default 31800) moves the ports. It never touches /work/chains or /work/bench.
"""

import contextlib
import importlib.util
import os
import pathlib
import select
import struct
import sys

os.environ.setdefault("P2A_CAPTURE_DIR", "/work/scratch/p2b-inbound/capture")
os.environ.setdefault("P2A_PORT_BASE", "31800")

_spec = importlib.util.spec_from_file_location(
    "p2a_capture", pathlib.Path(__file__).resolve().parent.parent / "p2a-transcripts" / "capture.py")
p2a = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(p2a)

SIM_START, FULL, BIT11, UA = p2a.SIM_START, p2a.FULL, p2a.BIT11, p2a.UA
compact = p2a.compact


def settle(self):
    """P2a's `Transcript.settle`, with the quiet period over all peers at once rather than per
    peer (the same condition: dashd sent nothing to any peer for QUIET seconds), so a scenario
    with dozens of peers is not quadratic in QUIET."""
    while True:
        live = [p for p in self.peers.values() if not p.closed]
        ready, _, _ = select.select([p.sock for p in live], [], [], p2a.QUIET) if live else ([], [], [])
        if not ready:
            break
        for p in live:
            if p.sock in ready:
                self._record(p, p.poll(0.2))
    for p in self.peers.values():
        if p.closed or not p.handshaken:
            continue
        p.send("ping", p.sync_pong)
        end = p2a.time.time() + 120
        done = False
        while not done and not p.closed and p2a.time.time() < end:
            msgs = p.poll(0.5)
            done = ("pong", p.sync_pong) in msgs
            self._record(p, msgs)
        if not done and not p.closed:
            raise RuntimeError(f"{self.name}: peer {p.n} sync ping not answered")


p2a.Transcript.settle = settle

# 000008ca1832a4baf228eb1553c03d3a2c8e02399550dd6ea8d65cec3ef23d2e, in uint256 (wire) byte order.
GENESIS_REGTEST = bytes.fromhex("2e3df23eec5cd6a86edd509539028e2c3a3dc05315eb28f2baa43218ca080000")
MAX_LOCATOR_SZ = 101


@contextlib.contextmanager
def scenario(out, name, index, notes, extra=(), premine=0):
    """P2a-core's `scenario` with extra dashd arguments `extra`."""
    node = p2a.Node(name, index, SIM_START, extra)
    try:
        if premine:
            node.mine(premine)
        t = p2a.Transcript(name, node, notes, ibd=not premine)
        yield t
        t.write(out)
    finally:
        node.stop()


def version_payload(n, services, protocol=70242, relay=True, ua=UA, masternode=False, cut=None):
    """A peer's `version`: Core's layout with `protocol`, `services`, `relay`, user agent `ua`,
    and the MNAUTH challenge and masternode flag; `cut` keeps only that many bytes."""
    addr = struct.pack("<Q", services) + bytes(18)
    p = (struct.pack("<iQq", protocol, services, SIM_START) + addr + addr
         + struct.pack("<Q", p2a.VERSION_NONCE + n) + compact(len(ua)) + ua
         + bytes(4) + bytes([int(relay)]) + bytes(32) + bytes([int(masternode)]))
    return p if cut is None else p[:cut]


def locator_payload(hashes, stop=bytes(32), protocol=70242):
    """`getheaders`/`getheaders2`: CBlockLocator (version, hashes), then the stop hash."""
    return struct.pack("<i", protocol) + compact(len(hashes)) + b"".join(hashes) + stop


def hash_of(t, height):
    """The node's block hash at `height`, in uint256 byte order (wire order)."""
    return bytes.fromhex(t.node.rpc("getblockhash", height))[::-1]


def handshake(t, n, services, verack=True, **kw):
    t.send(n, "version", version_payload(n, services, **kw))
    if verack and not t.peers[n].closed:
        t.send(n, "verack")


def scenario_gates(out, index):
    """Behaviour 6 inbound: what dashd does with messages before `version` and, after it,
    before `verack`; a redundant `version` before and after `verack`; then the replies of a
    genesis-only node to `getheaders`/`getheaders2` (empty, or the stop block for a null
    locator).

    `-v2transport=0`: with V2 on (the default), an inbound stream whose first 16 bytes are not a
    V1 `version` header is read as a V2 handshake, so no message before `version` reaches the
    gates (P2b D10)."""
    with scenario(out, "inbound-gates", index, [
            "-v2transport=0. Peer 1 sends messages before version, then before verack, then a",
            "second version."], ("-v2transport=0",)) as t:
        t.accept(1)
        genesis_locator = locator_payload([GENESIS_REGTEST])
        for cmd, payload in [("ping", struct.pack("<Q", 7)), ("getheaders", genesis_locator),
                             ("verack", b""), ("sendheaders", b""), ("getaddr", b""),
                             ("mempool", b""), ("bogus", b"\x01")]:
            t.send(1, cmd, payload)
        # The tool counts any verack as the peer's; that one was ignored.
        t.peers[1].sent_verack = False
        t.send(1, "version", version_payload(1, FULL | BIT11))
        for cmd, payload in [("ping", struct.pack("<Q", 8)), ("getheaders", genesis_locator),
                             ("getaddr", b""), ("inv", compact(1) + struct.pack("<I", 2) + bytes(range(32))),
                             ("sendheaders2", b""), ("sendcmpct", b"\x00" + struct.pack("<Q", 1)),
                             ("sendaddrv2", b""), ("version", version_payload(1, FULL))]:
            t.send(1, cmd, payload)
        t.send(1, "verack")
        t.send(1, "version", version_payload(1, FULL))
        t.send(1, "getheaders", genesis_locator)
        t.send(1, "getheaders2", genesis_locator)
        t.send(1, "getheaders", locator_payload([], GENESIS_REGTEST))
        t.send(1, "getheaders2", locator_payload([], GENESIS_REGTEST))
        t.send(1, "getheaders", locator_payload([], bytes(range(32))))
        t.send(1, "getheaders", locator_payload([GENESIS_REGTEST] * (MAX_LOCATOR_SZ + 1)))


def scenario_versions(out, index):
    """Behaviours 3-5 inbound: the responder handshake against peers of other protocol
    versions, services and `version` shapes, including the ones dashd refuses."""
    with scenario(out, "inbound-versions", index, [
            "Each peer connects, then sends its version and verack (refused peers stop early)."]) as t:
        peers = [
            (1, FULL, dict(protocol=70222)),                  # below sendaddrv2
            (2, FULL | BIT11, dict(protocol=70235)),          # headers2 limit 8000
            (3, FULL, dict(protocol=70234, relay=False)),     # headers2 limit 2000, no tx relay
            (4, 0, dict()),                                   # NODE_NONE: serves no blocks
            (5, FULL, dict(protocol=70220)),                  # obsolete
            (6, FULL, dict(masternode=True)),                 # inbound masternode, we are not one
            (7, FULL | BIT11, dict(cut=46)),                  # stops after addr_me
            (8, FULL, dict(ua=b"x" * 257)),                   # user agent over 256: Core throws
            (9, 0x4 | BIT11, dict(relay=False)),              # NODE_BLOOM only
        ]
        for n, services, kw in peers:
            t.accept(n)
            # Peer 8's version throws, so its verack would come before any version.
            handshake(t, n, services, verack=n != 8, **kw)
        # Peer 8's version threw, so it is still before version: a good version now works.
        handshake(t, 8, FULL)


def scenario_serve(out, index, name, notes, premine, extra=(), min_work=False):
    """`getheaders`/`getheaders2` serving from the node's own chain of `premine` blocks."""
    with scenario(out, name, index, notes, extra, premine) as t:
        tip = t.node.rpc("getblockcount")

        def h(height):
            return hash_of(t, height)

        t.accept(1)
        handshake(t, 1, FULL | BIT11)
        t.accept(2)
        handshake(t, 2, FULL, protocol=70234)
        if min_work:
            for n in (1, 2):
                t.send(n, "getheaders", locator_payload([GENESIS_REGTEST]))
                t.send(n, "getheaders2", locator_payload([GENESIS_REGTEST]))
                t.send(n, "getheaders", locator_payload([], GENESIS_REGTEST))
        else:
            t.send(1, "getheaders", locator_payload([GENESIS_REGTEST]))        # the 2000 limit
            t.send(1, "getheaders", locator_payload([h(2000)]))               # the rest
            t.send(1, "getheaders2", locator_payload([GENESIS_REGTEST]))       # 8000: all of it
            t.send(1, "getheaders2", locator_payload([h(tip)]))               # nothing new
            t.send(1, "getheaders", locator_payload([], h(5)))                # the stop block alone
            t.send(1, "getheaders", locator_payload([GENESIS_REGTEST], h(10)))  # through the stop
            t.send(1, "getheaders", locator_payload([bytes(range(32)), h(50), h(40)]))
            t.send(1, "getheaders", locator_payload([], bytes(range(32))))     # unknown stop
            t.send(2, "getheaders2", locator_payload([h(tip - 3)]))           # a non-bit-11 peer
            t.send(2, "getheaders2", locator_payload([GENESIS_REGTEST]))       # 2000 below 70235
            t.send(2, "sendheaders")
            t.send(1, "getheaders", locator_payload([h(tip - 1)]))            # best header sent
            t.mine()
            t.send(1, "getheaders", locator_payload([h(tip)], h(tip)))
            t.send(2, "getheaders", locator_payload([GENESIS_REGTEST] * (MAX_LOCATOR_SZ + 1)))


def scenario_evict(out, index, name, max_inbound):
    """Inbound eviction: `max_inbound` inbound slots (-maxconnections=max_inbound + 11, 11 kept
    for outbound), all held, then four more peers connect and each evicts one
    (`AttemptToEvictConnection`) or, when every candidate is protected, is dropped.
    `SelectNodeToEvict` protects up to 4 + 8 + 4 + 8 + 4 candidates, then half the rest, so 21
    slots leave nothing to evict (`inbound-evict-21`) and 29 leave some (`inbound-evict`)."""
    with scenario(out, name, index, [
            f"-maxconnections={max_inbound + 11}: {max_inbound} inbound slots. Peers vary in tx relay,",
            "services, whether they answer the node's ping, and connect time (the mock time moves",
            "by 1 s before some of them connect)."], (f"-maxconnections={max_inbound + 11}",)) as t:
        now = SIM_START
        for n in range(1, max_inbound + 5):
            if n % 3 == 0:
                now += 1
                t.set_time(now)
            t.accept(n)
            if t.peers[n].closed:
                continue
            services = FULL if n % 4 else 0x4
            handshake(t, n, services, relay=n % 5 != 0)
            p = t.peers[n]
            if n % 2 and p.last_ping is not None and not p.closed:
                t.pong(n)


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    out = sys.argv[1]
    want = set(sys.argv[2:])
    pathlib.Path(out).mkdir(parents=True, exist_ok=True)
    p2a.ROOT.mkdir(parents=True, exist_ok=True, mode=0o700)
    many = "f" * 64
    jobs = [
        ("inbound-gates", lambda o: scenario_gates(o, 0)),
        ("inbound-versions", lambda o: scenario_versions(o, 1)),
        ("inbound-serve", lambda o: scenario_serve(o, 2, "inbound-serve", [
            "The node mined 2100 blocks before the peers connect (out of IBD).",
            "Peer 1 has bit 11 at 70242; peer 2 lacks it, at 70234."], 2100)),
        ("inbound-serve-minwork", lambda o: scenario_serve(o, 3, "inbound-serve-minwork", [
            "-minimumchainwork above any chain: every request gets an empty reply typed by the",
            "peer's bit 11. Peer 1 has bit 11 at 70242; peer 2 lacks it, at 70234."], 0,
            (f"-minimumchainwork={many}",), min_work=True)),
        ("inbound-serve-download", lambda o: scenario_serve(o, 4, "inbound-serve-download", [
            "-minimumchainwork above any chain with -whitelist=download@127.0.0.1: the download",
            "permission lifts the minimum-work rule. Peer 1 has bit 11; peer 2 lacks it."], 0,
            (f"-minimumchainwork={many}", "-whitelist=download@127.0.0.1"), min_work=True)),
        ("inbound-evict", lambda o: scenario_evict(o, 5, "inbound-evict", 29)),
        ("inbound-evict-21", lambda o: scenario_evict(o, 6, "inbound-evict-21", 21)),
    ]
    for name, job in jobs:
        if not want or name in want:
            print(f"== {name}", flush=True)
            job(out)


if __name__ == "__main__":
    main()
