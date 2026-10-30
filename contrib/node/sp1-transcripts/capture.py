#!/usr/bin/env python3
"""Captures SP1's masternode-sync transcript fixtures from regtest dashd v24.0.0-rc.4.

The machinery and the fixture format are P2a-core's (`contrib/node/p2a-transcripts/capture.py`,
whose docstring defines the records), plus:
  mnsync ID NAME START ATTEMPT BCSYNCED SYNCED
                                  a check (no segment): dashd's `mnsync status` after the
                                  segment before it settled
  mnsyncreset                     an input: the `mnsync reset` RPC (`Reset(true)`)

Every scenario starts a fresh regtest dashd at the frozen mock time SIM_START, mines one block
before any peer connects (so the tip is the best header out of IBD, as the sync tick needs), then
moves the mock time in 1 s steps so that `SyncManager::ProcessTick` (every second of real time,
gated to 6 s of mock time) runs at each step. dashd's own `mempool` and `govsync` are what SP1
compares.

  mnsync-disabled     -disablegovernance=1 (rdashd's default until GV1b, DEC-510): an outbound,
                      a block-relay-only and an inbound peer; BLOCKCHAIN switches 30 s after the
                      last block (3 peers) with `mempool` to the outbound peer only, GOVERNANCE
                      ends at the next tick; then `mnsync reset` and the `full-sync` disconnects
  mnsync-four-peers   -disablegovernance=1 with four outbound peers: the 6 s timeout
  mnsync-blocksonly   -disablegovernance=1 -blocksonly=1 (rdashd's node defaults): no `mempool`
  mnsync-governance   governance on (Core's regtest default): `govsync` to one peer per tick, then
                      the 30 s GOVERNANCE timeout
  mnsync-allow-sync   -disablegovernance=1: an outbound peer, then (after the BLOCKCHAIN switch) a
                      manual, a `-whitebind=noban` and a second outbound peer; after `mnsync
                      reset` the tick's `allow-sync` keeps the manual and noban peers and
                      disconnects the outbound ones (SP1b, DEC-562)

After each scenario dashd is stopped and its `netfulfilled.dat` and `mncache.dat` are copied next
to the transcript as `<scenario>.netfulfilled.dat` and `<scenario>.mncache.dat`.

Usage:
  P2A_DASHD=/path/to/dashd-24.0.0-rc.4/bin/dashd capture.py OUT_DIR [SCENARIO...]

`P2A_CAPTURE_DIR` (here default /work/scratch/sp1/capture) holds the datadirs; `P2A_PORT_BASE`
(here default 30900) moves the ports. It never touches /work/chains or /work/bench.
"""

import importlib.util
import os
import pathlib
import shutil
import socket
import sys

os.environ.setdefault("P2A_CAPTURE_DIR", "/work/scratch/sp1/capture")
os.environ.setdefault("P2A_PORT_BASE", "30900")
_p2a = pathlib.Path(__file__).resolve().parent.parent / "p2a-transcripts" / "capture.py"
_spec = importlib.util.spec_from_file_location("p2a_capture", _p2a)
p2a = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(p2a)

SIM_START = p2a.SIM_START
FULL = p2a.FULL


def status(t):
    return t.node.rpc("mnsync", "status")


def mnsync(t):
    """Records dashd's `mnsync status` as a check line."""
    s = status(t)
    t.lines.append(f"mnsync {s['AssetID']} {s['AssetName']} {s['AssetStartTime']} {s['Attempt']} "
                   f"{int(s['IsBlockchainSynced'])} {int(s['IsSynced'])}")


def steps(t, start, end):
    """The mock time in 1 s steps over [start, end], with the sync state after each; a ping from
    dashd is answered."""
    for s in range(start, end + 1):
        t.set_time(SIM_START + s)
        for n, p in t.peers.items():
            if p.last_ping is not None and not p.closed:
                t.pong(n)
        mnsync(t)


def mnsync_reset(t):
    t.lines.append("mnsyncreset")
    t.node.rpc("mnsync", "reset")
    t.settle()


def run(out, name, index, extra, notes, body):
    node = p2a.Node(name, index, SIM_START, extra=extra)
    try:
        node.mine(1)
        t = p2a.Transcript(name, node, notes, ibd=False)
        mnsync(t)
        body(t)
        t.write(out)
    finally:
        node.stop()
    for f in ("netfulfilled.dat", "mncache.dat"):
        shutil.copyfile(node.dir / "regtest" / f, pathlib.Path(out) / f"{name}.{f}")


def scenario_disabled(out, index):
    def body(t):
        t.connect(1, "outbound-full-relay")
        t.handshake(1, FULL)
        t.connect(2, "block-relay-only")
        t.handshake(2, FULL)
        t.accept(3)
        t.handshake(3, FULL)
        steps(t, 1, 50)
        mnsync_reset(t)
        mnsync(t)
        steps(t, 51, 58)

    run(out, "mnsync-disabled", index, ["-disablegovernance=1"], [
        "-disablegovernance=1: BLOCKCHAIN times out 30 s after the block (3 peers), then GOVERNANCE",
        "ends at once; `mnsync reset` makes the next tick disconnect the full-sync peers."], body)


def scenario_four_peers(out, index):
    def body(t):
        for n in range(1, 5):
            t.connect(n, "outbound-full-relay")
            t.handshake(n, FULL)
        steps(t, 1, 20)

    run(out, "mnsync-four-peers", index, ["-disablegovernance=1"], [
        "-disablegovernance=1 with four outbound peers: BLOCKCHAIN's 6 s timeout."], body)


def scenario_blocksonly(out, index):
    def body(t):
        t.connect(1, "outbound-full-relay")
        t.handshake(1, FULL)
        steps(t, 1, 44)

    run(out, "mnsync-blocksonly", index, ["-disablegovernance=1", "-blocksonly=1"], [
        "rdashd's node defaults (-disablegovernance=1, -blocksonly=1): no mempool request."], body)


def steps_until(t, start, asset):
    """`steps` from `start` until the sync is at `asset`; returns the next step."""
    s = start
    while status(t)["AssetID"] != asset:
        assert s < start + 60, f"no asset {asset} by step {s}"
        steps(t, s, s)
        s += 1
    return s


# The allow-sync scenario's `-whitebind=noban` listener, and the noban peer's source port (both
# below the ephemeral range; P4a's records name port 40000+N, which the replay uses and which no
# compared byte shows: `addr_recv` is all zeros for 127.0.0.1, P2a's masks).
NOBAN_PORT = p2a.PORT_BASE + 60
NOBAN_SOURCE_PORT = p2a.PORT_BASE + 70


def accept_noban(t, n):
    """Peer `n` dials the `noban` listener from 127.0.0.1 (P4a's `accept N from ADDR perm`)."""
    sock = socket.socket()
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    sock.bind(("127.0.0.1", NOBAN_SOURCE_PORT + n))
    sock.settimeout(30)
    sock.connect(("127.0.0.1", NOBAN_PORT))
    t.lines.append(f"accept {n} from 127.0.0.1 perm noban")
    t.peers[n] = p2a.Peer(n, sock)
    t.settle()


def scenario_allow_sync(out, index):
    def body(t):
        t.connect(1, "outbound-full-relay")
        t.handshake(1, FULL)
        s = steps_until(t, 1, 4)
        # Between the BLOCKCHAIN switch and the tick that finishes: three peers the tick has
        # not seen, so FINISHED gives them `full-sync` before their first `allow-sync` check.
        t.connect(2, "manual")
        t.handshake(2, FULL)
        accept_noban(t, 3)
        t.handshake(3, FULL)
        t.connect(4, "outbound-full-relay")
        t.handshake(4, FULL)
        s = steps_until(t, s, 999)
        mnsync_reset(t)
        mnsync(t)
        steps(t, s, s + 20)

    run(out, "mnsync-allow-sync", index, ["-disablegovernance=1", f"-whitebind=noban@127.0.0.1:{NOBAN_PORT}"], [
        "-disablegovernance=1 with an outbound peer, then, after the BLOCKCHAIN switch, a manual, a",
        "noban (-whitebind) and a second outbound peer. After `mnsync reset` the next tick gives the",
        "manual and noban peers `allow-sync` (their requests removed, `full-sync` too) and keeps them;",
        "it disconnects both outbound peers as recently synced (node/sync_manager.cpp:186-202)."], body)


def scenario_governance(out, index):
    def body(t):
        t.connect(1, "outbound-full-relay")
        t.handshake(1, FULL)
        t.connect(2, "outbound-full-relay")
        t.handshake(2, FULL)
        steps(t, 1, 80)

    run(out, "mnsync-governance", index, [], [
        "Governance on (Core's regtest default): govsync(0) to one peer per tick, then the",
        "GOVERNANCE timeout 30 s after its start (no object arrives to bump it)."], body)


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    out = sys.argv[1]
    want = set(sys.argv[2:])
    pathlib.Path(out).mkdir(parents=True, exist_ok=True)
    p2a.ROOT.mkdir(parents=True, exist_ok=True, mode=0o700)
    jobs = [("mnsync-disabled", lambda o: scenario_disabled(o, 0)),
            ("mnsync-four-peers", lambda o: scenario_four_peers(o, 1)),
            ("mnsync-blocksonly", lambda o: scenario_blocksonly(o, 2)),
            ("mnsync-governance", lambda o: scenario_governance(o, 3)),
            ("mnsync-allow-sync", lambda o: scenario_allow_sync(o, 4))]
    for name, job in jobs:
        if not want or name in want:
            print(f"== {name}", flush=True)
            job(out)


if __name__ == "__main__":
    main()
