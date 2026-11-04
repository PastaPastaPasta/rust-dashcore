#!/usr/bin/env python3
"""Captures SP0's spork transcript fixtures from regtest dashd v24.0.0-rc.3.

The machinery and the fixture format are P2a-core's (`contrib/node/p2a-transcripts/capture.py`,
whose docstring defines the records), plus one input record:
  sporkupdate NAME VALUE          the node signs and stores spork NAME (`sporkupdate` RPC) at its
                                  mock time, with Core's functional-test spork key

Scenarios, each on a fresh regtest dashd at the frozen mock time SIM_START:
  sporks-serve    a node with the spork key: `getsporks` before and after `sporkupdate`, the
                  v24 repeat dedup, a second peer, `getdata MSG_SPORK` for a replaced and an
                  unknown spork
  sporks-receive  a node without the key: sporks a separate signer dashd produced, sent as is,
                  with an aliased header byte, with a high-S signature, with a bad signature,
                  signed too far in the future, with a 66-byte signature, and truncated
  sporks-sync-tick  an outbound peer while the mock time moves: when dashd's own `getsporks`
                  goes out (SP1's sync tick; evidence only, not replayed)

Usage:
  P2A_DASHD=/path/to/dashd-24.0.0-rc.3/bin/dashd capture.py OUT_DIR [SCENARIO...]

`P2A_CAPTURE_DIR` (here default /work/scratch/sp0/capture) holds the datadirs; `P2A_PORT_BASE`
(here default 31900) moves the ports. It never touches /work/chains or /work/bench.
"""

import importlib.util
import os
import pathlib
import struct
import sys

os.environ.setdefault("P2A_CAPTURE_DIR", "/work/scratch/sp0/capture")
os.environ.setdefault("P2A_PORT_BASE", "31900")
_p2a = pathlib.Path(__file__).resolve().parent.parent / "p2a-transcripts" / "capture.py"
_spec = importlib.util.spec_from_file_location("p2a_capture", _p2a)
p2a = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(p2a)

SIM_START = p2a.SIM_START
FULL, BIT11 = p2a.FULL, p2a.BIT11

# Core's functional-test spork key (test_framework.py), regtest's spork address's key.
SPORK_KEY = "cP4EKFyJsHT39LDqgdcB43Y3YXjNyjb5Fuas1GQSeAtjnZWmZEQK"
SPORKS = ["SPORK_2_INSTANTSEND_ENABLED", "SPORK_17_QUORUM_DKG_ENABLED", "SPORK_19_CHAINLOCKS_ENABLED",
          "SPORK_21_QUORUM_ALL_CONNECTED", "SPORK_23_QUORUM_POSE"]
MSG_SPORK = 6
# secp256k1's group order.
N = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141


def spork_hash(payload):
    """`CSporkMessage::GetHash`: SHA256d of the whole serialization, signature included."""
    return p2a.sha256d(payload)


def getdata(*hashes):
    return p2a.compact(len(hashes)) + b"".join(struct.pack("<I", MSG_SPORK) + h for h in hashes)


def split(payload):
    """(id, value, time, signature) of a `spork` payload."""
    sid, value, time_ = struct.unpack_from("<iqq", payload, 0)
    n = payload[20]
    assert n < 0xFD and len(payload) == 21 + n
    return sid, value, time_, payload[21:]


def with_sig(payload, sig):
    return payload[:20] + p2a.compact(len(sig)) + sig


def high_s(payload):
    """The same signature with s negated and the recovery ID's parity flipped: still valid."""
    sig = split(payload)[3]
    s = int.from_bytes(sig[33:65], "big")
    header = 27 + ((sig[0] - 27) ^ 1)
    return with_sig(payload, bytes([header]) + sig[1:33] + (N - s).to_bytes(32, "big"))


def alias(payload):
    """The header byte plus 8: Core's `RecoverCompact` masks it, so the signature still holds."""
    sig = split(payload)[3]
    return with_sig(payload, bytes([sig[0] + 8]) + sig[1:])


def bad_sig(payload):
    sig = bytearray(split(payload)[3])
    sig[10] ^= 0x01
    return with_sig(payload, bytes(sig))


class Transcript(p2a.Transcript):
    def sporkupdate(self, name, value):
        self.lines.append(f"sporkupdate {name} {value}")
        assert self.node.rpc("sporkupdate", name, value) == "success"
        self.settle()


def node(name, index, mocktime=SIM_START, key=False):
    return p2a.Node(name, index, mocktime, [f"-sporkkey={SPORK_KEY}"] if key else [])


def signed(index, mocktime, updates):
    """`getsporks`' reply from a separate signer dashd at `mocktime` after `updates`."""
    n = node(f"signer-{mocktime}", index, mocktime, key=True)
    try:
        for name, value in updates:
            assert n.rpc("sporkupdate", name, value) == "success"
        t = Transcript("signer", n, [], ibd=True)
        t.accept(1)
        t.handshake(1, FULL)
        start = len(t.lines)
        t.send(1, "getsporks")
        return [bytes.fromhex(l.split(" ")[3]) for l in t.lines[start:] if l.startswith("< 1 spork ")]
    finally:
        n.stop()


def run(out, name, n, notes, body):
    t = Transcript(name, n, notes, ibd=True)
    try:
        body(t)
        t.write(out)
    finally:
        n.stop()


def scenario_serve(out, index):
    def body(t):
        t.accept(1)
        t.handshake(1, FULL)
        t.send(1, "getsporks")
        t.send(1, "getsporks")
        for name in SPORKS:
            t.sporkupdate(name, 1 if name == "SPORK_21_QUORUM_ALL_CONNECTED" else 0)
        start = len(t.lines)
        t.send(1, "getsporks")
        first = {split(p)[0]: p for p in (bytes.fromhex(l.split(" ")[3]) for l in t.lines[start:]
                                          if l.startswith("< 1 spork "))}
        assert len(first) == 5, t.lines[start:]
        t.send(1, "getsporks")
        t.sporkupdate("SPORK_2_INSTANTSEND_ENABLED", 4070908800)
        t.send(1, "getsporks")
        t.send(1, "getsporks")
        t.accept(2)
        t.handshake(2, FULL)
        t.send(2, "getsporks")
        t.send(2, "getdata", getdata(spork_hash(first[10001])))
        t.send(2, "getdata", getdata(bytes(32)))

    run(out, "sporks-serve", node("sporks-serve", index, key=True), [
        f"The node runs with -sporkkey (Core's functional-test key); sporkupdate signs at the mock time."], body)


def scenario_receive(out, index):
    good = signed(index + 1, SIM_START, [("SPORK_19_CHAINLOCKS_ENABLED", 0), ("SPORK_2_INSTANTSEND_ENABLED", 0),
                                         ("SPORK_21_QUORUM_ALL_CONNECTED", 1)])
    by_id = {split(p)[0]: p for p in good}
    future = signed(index + 2, SIM_START + 7201, [("SPORK_23_QUORUM_POSE", 0)])[0]

    def body(t):
        s19, s2, s21 = by_id[10018], by_id[10001], by_id[10020]
        t.accept(1)
        t.handshake(1, FULL)
        t.send(1, "spork", s19)
        t.send(1, "spork", s19)
        t.send(1, "spork", alias(s19))
        t.send(1, "getsporks")
        t.send(1, "spork", high_s(s2))
        t.send(1, "getsporks")
        t.send(1, "getdata", getdata(spork_hash(alias(s19)), spork_hash(high_s(s2))))
        t.send(1, "spork", bad_sig(s21))
        t.accept(2)
        t.handshake(2, FULL)
        t.send(2, "spork", future)
        t.accept(3)
        t.handshake(3, FULL)
        t.send(3, "spork", s21[:20] + bytes([66]) + split(s21)[3] + b"\x00")
        t.accept(4)
        t.handshake(4, FULL)
        t.send(4, "spork", s21[:-1])
        t.accept(5)
        t.handshake(5, FULL)
        t.send(5, "spork", s21)
        t.send(5, "getsporks")

    run(out, "sporks-receive", node("sporks-receive", index), [
        "The node runs without a spork key. Its sporks were signed by a separate regtest dashd v24 with",
        "-sporkkey (Core's functional-test key) at the mock time SIM_START, and one at SIM_START+7201;",
        "the tool alters some of them: the header byte +8, s negated (high-S), a flipped bit in r, a",
        "66-byte signature, and the last byte cut off."], body)


def scenario_sync_tick(out, index):
    def body(t):
        t.connect(1, "outbound-full-relay")
        t.handshake(1, FULL | BIT11)
        t.send(1, "headers2", p2a.compact(0))
        for s in range(1, 16):
            t.set_time(SIM_START + s)
            if t.peers[1].last_ping is not None:
                t.pong(1)

    run(out, "sporks-sync-tick", node("sporks-sync-tick", index), [
        "Evidence for SP1 (not replayed): dashd's own getsporks once the mock time moves in 1 s steps."], body)


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    out = sys.argv[1]
    want = set(sys.argv[2:])
    pathlib.Path(out).mkdir(parents=True, exist_ok=True)
    p2a.ROOT.mkdir(parents=True, exist_ok=True, mode=0o700)
    jobs = [("sporks-serve", lambda o: scenario_serve(o, 0)),
            ("sporks-receive", lambda o: scenario_receive(o, 1)),
            ("sporks-sync-tick", lambda o: scenario_sync_tick(o, 4))]
    for name, job in jobs:
        if not want or name in want:
            print(f"== {name}", flush=True)
            job(out)


if __name__ == "__main__":
    main()
