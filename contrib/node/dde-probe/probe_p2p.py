#!/usr/bin/env python3
"""Sends P2P payload variants to both regtest dashds and records Core's outcome.

Usage: probe_p2p.py OUT.json
Needs both nodes running (nodes.py start). Connects the v23.1.8 node to the
v24 node first, so the base version payload is one Core itself sent.
"""
import json
import struct
import sys
import time

import nodes
from p2p import Peer, read_capture

V23, V24 = list(nodes.NODES)


def captured_version():
    """A version payload v23.1.8 sent to v24 (-capturemessages)."""
    nodes.cli(V23, "addnode", "127.0.0.1:%d" % nodes.p2p_port(V24), "onetry")
    time.sleep(3)
    for sent in (nodes.datadir(V23) / "regtest" / "message_capture").glob("*_%d/msgs_sent.dat" % nodes.p2p_port(V24)):
        for _, cmd, payload in read_capture(sent):
            if cmd == "version":
                return payload
    raise RuntimeError("no captured version")


def patch(b, at, new):
    return b[:at] + new + b[at + len(new):]


def version_cases(base):
    def subver(n):
        s = b"/" + b"x" * (n - 1)
        prefix = bytes([0xfd]) + struct.pack("<H", n) if n >= 0xfd else bytes([n])
        return base[:80] + prefix + s + base[99:]

    # Offsets in a 137-byte payload: mandatory fields end at 46, addrFrom and
    # nonce at 80, subver at 99, start height at 103, relay at 104, the mnauth
    # challenge at 136, fMasternode at 137.
    assert len(base) == 137
    return {
        "full": base,
        "cut_46_mandatory_only": base[:46],
        "cut_80_through_nonce": base[:80],
        "cut_99_through_subver": base[:99],
        "cut_103_through_height": base[:103],
        "cut_104_through_relay": base[:104],
        "cut_136_through_challenge": base[:136],
        "cut_60_inside_addr_from": base[:60],
        "cut_85_inside_subver": base[:85],
        "cut_101_inside_height": base[:101],
        "cut_72_inside_nonce": base[:72],
        # Core checks the version before the optional fields, so a broken tail
        # behind an obsolete version is a disconnect, not an exception.
        "obsolete_short_tail": patch(base, 0, struct.pack("<i", 70000))[:46] + b"\xee" * 10,
        "relay_00": patch(base, 103, b"\x00"),
        "relay_02": patch(base, 103, b"\x02"),
        "relay_ff": patch(base, 103, b"\xff"),
        "mn_02": patch(base, 136, b"\x02"),
        "trailing_5": base + b"\x01\x02\x03\x04\x05",
        "subver_256": subver(256),
        "subver_257": subver(257),
        "time_negative": patch(base, 12, struct.pack("<q", -5)),
        "protocol_version_minus_1": patch(base, 0, struct.pack("<i", -1)),
    }


KEYS = ("Exception", "Misbehaving", "Unknown command", "obsolete", "masternode connection", "headers message size")


def notable(peer):
    return [l for l in peer.log() if any(k in l for k in KEYS)]


def version_case(node, payload):
    p = Peer(node)
    p.send("version", payload)
    got = p.read(1.5)
    info, synced = None, False
    if "version" in got and not p.closed:
        p.send("verack")
        synced = p.sync()
        info = p.info()
    time.sleep(0.3)
    p.read(0.2)
    out = {"synced": synced, "closed": p.closed, "peer": info, "log": notable(p)}
    p.close()
    return out


def handshake(node, base, version=None):
    p = Peer(node)
    p.send("version", base if version is None else patch(base, 0, struct.pack("<i", version)))
    p.read(1.0)
    p.send("verack")
    assert p.sync(), "handshake"
    return p


def after_handshake(node, base, cmd, payload, version=None):
    p = handshake(node, base, version)
    p.send(cmd, payload)
    synced = p.sync(7)
    time.sleep(0.3)
    p.read(0.2)
    out = {"synced": synced, "closed": p.closed, "log": notable(p)}
    p.close()
    return out


def main():
    base = captured_version()
    genesis = bytes.fromhex(nodes.cli(V24, "getblockheader", nodes.cli(V24, "getblockhash", "0"), "false"))
    res = {"version_payload": base.hex(), "genesis_header": genesis.hex(), "nodes": {}}
    for node in nodes.NODES:
        r = {"version": {k: dict(version_case(node, v), raw=v.hex()) for k, v in version_cases(base).items()}}
        msgs = {
            "headers_txcount_5": ("headers", b"\x01" + genesis + b"\x05", None),
            "headers_txcount_fd": ("headers", b"\x01" + genesis + b"\xfd\xfd\x00", None),
            "headers_txcount_noncanonical": ("headers", b"\x01" + genesis + b"\xfd\x05\x00", None),
            "headers_count_2001": ("headers", b"\xfd\xd1\x07", None),
            "headers_count_2000_short": ("headers", b"\xfd\xd0\x07", None),
            "headers2_count_2001_short": ("headers2", b"\xfd\xd1\x07", None),
            "headers2_count_8001": ("headers2", b"\xfd\x41\x1f", None),
            "headers_trailing": ("headers", b"\x01" + genesis + b"\x00\x99\x99", None),
            "headers2_count_2001_at_70230": ("headers2", b"\xfd\xd1\x07", 70230),
            "headers2_count_2001_at_70234": ("headers2", b"\xfd\xd1\x07", 70234),
            "headers2_count_2001_at_70235": ("headers2", b"\xfd\xd1\x07", 70235),
            "unknown_command": ("bs4aprobe", b"\x00\x01\x02", None),
            "sendheaders2_with_payload": ("sendheaders2", b"\x07\x07", None),
            "senddsq_empty": ("senddsq", b"", None),
            "senddsq_02": ("senddsq", b"\x02", None),
            "qsendrecsigs_empty": ("qsendrecsigs", b"", None),
            "qsendrecsigs_02": ("qsendrecsigs", b"\x02", None),
        }
        r["messages"] = {k: dict(after_handshake(node, base, c, pl, v), command=c, raw=pl.hex(), version=v or 70240)
                         for k, (c, pl, v) in msgs.items()}
        # A verack carrying a payload: Core still completes the handshake and
        # sends its post-verack messages.
        p = Peer(node)
        p.send("version", base)
        p.read(1.0)
        p.send("verack", b"\xde\xad\xbe\xef")
        r["verack_with_payload"] = {"synced": p.sync(2), "replies": sorted({c for c, _ in p.received}),
                                    "log": notable(p)}
        p.close()
        res["nodes"][node] = r
    # What each release sent the other: v23.1.8's sent file, and v24's
    # messages as v23.1.8 received them.
    res["senddsq_payloads"] = {}
    for node, name in ((V23, "msgs_sent.dat"), (V24, "msgs_recv.dat")):
        caps = sorted((nodes.datadir(V23) / "regtest" / "message_capture").glob("*_%d/%s" % (nodes.p2p_port(V24), name)))
        res["senddsq_payloads"][node] = sorted({p.hex() for c in caps for _, cmd, p in read_capture(c) if cmd == "senddsq"})
    json.dump(res, open(sys.argv[1], "w"), indent=1)


if __name__ == "__main__":
    main()
