#!/usr/bin/env python3
"""Core's handler state and pre-decode gates behind DDE-SWEEP §4, at both pins.

Usage: probe_gates.py PROBE.json OUT.json
PROBE.json is probe_p2p.py's output (its captured version payload is reused).
Restarts both nodes (nodes.py) in each mode: default, then -blocksonly=1, then
-txreconciliation=1. Leaves them running in the default mode. Each case records
what Core did, and main() fails unless both releases match the expectation the
sweep states.
"""
import json
import struct
import sys
import time

import nodes
from p2p import COMMANDS, UNKNOWN, Peer

# Commands Core's NetMessageViolatesBlocksOnly names (protocol.cpp:185-212 at
# both pins), probed with an empty payload from a -blocksonly node's peer.
BLOCKS_ONLY = ["qsendrecsigs", "tx", "dsq", "qwatch"]


def handshake(node, base, version=None, verack=True):
    p = Peer(node)
    p.send("version", base if version is None else struct.pack("<i", version) + base[4:])
    p.read(1.0)
    if verack:
        p.send("verack")
        assert p.sync(), "handshake"
    return p


def notable(p, *keys):
    keys = keys or ("Exception", "Misbehaving", "violation", "disconnect", "pong peer=", "sendaddrv2", "sendtxrcncl",
                    "Unknown command", "Header error")
    return [l for l in p.log() if any(k in l for k in keys)]


def outcome(p, synced):
    """accept: Core answered a later ping and kept the peer; disconnect: it closed."""
    if p.closed:
        return "disconnect"
    return "accept" if synced else "no answer"


def send_case(node, base, cmd, payload, version=None):
    p = handshake(node, base, version)
    p.send(cmd, payload)
    synced = p.sync(7)
    time.sleep(0.3)
    p.read(0.3)
    out = {"command": cmd, "raw": payload.hex(), "core": outcome(p, synced), "log": notable(p)}
    p.close()
    return out


def core_ping(p):
    """Waits for the ping Core sends after the handshake; returns its nonce."""
    end = time.time() + 10
    while time.time() < end:
        for cmd, payload in p.received:
            if cmd == "ping":
                return payload
        p.read(0.2)
    raise RuntimeError("no ping from Core")


def pong_case(node, base, make):
    """Sends a pong while Core's own ping is outstanding; reads getpeerinfo."""
    p = handshake(node, base)
    nonce = core_ping(p)
    for _ in range(20):
        before = p.peerinfo()
        if "pingwait" in before:
            break
        time.sleep(0.1)
    payload = make(nonce)
    p.send("pong", payload)
    synced = p.sync(9)
    after = p.peerinfo() or {}
    if "pingwait" in after:
        core = "ping pending"
    elif "pingtime" in after:
        core = "ping measured"
    else:
        core = "ping cancelled"
    out = {"raw": payload.hex(), "core": core if synced and not p.closed else outcome(p, synced),
           "pingwait_before": "pingwait" in before, "log": notable(p, "pong peer=")}
    p.close()
    return out


PONGS = {
    "empty": lambda n: b"",
    "len_1": lambda n: b"\x2a",
    "len_7": lambda n: b"\x2a" + bytes(6),
    "zero_nonce": lambda n: bytes(8),
    "mismatched_nonce": lambda n: struct.pack("<Q", (struct.unpack("<Q", n)[0] ^ 0xFF) or 1),
    "matching_nonce_trailing": lambda n: n + b"\x99",
}


def sendaddrv2_case(node, base, version, post_verack):
    """sendaddrv2 with a payload, before or after our verack, at `version`."""
    p = handshake(node, base, version, verack=post_verack)
    p.send("sendaddrv2", b"\xde\xad")
    if not post_verack:
        p.send("verack")
    synced = p.sync(9)
    time.sleep(0.3)
    out = {"version": version, "post_verack": post_verack, "core": outcome(p, synced), "log": notable(p)}
    p.close()
    return out


def sendtxrcncl_case(node, base, post_verack, payload):
    p = handshake(node, base, verack=post_verack)
    p.send("sendtxrcncl", payload)
    if not post_verack:
        p.send("verack")
    synced = p.sync(9)
    time.sleep(0.3)
    out = {"post_verack": post_verack, "raw": payload.hex(), "core": outcome(p, synced), "log": notable(p)}
    p.close()
    return out


RCNCL = struct.pack("<IQ", 1, 0x1122334455667788)


def command_case(node, base, name):
    """A V1 message with one of p2p.COMMANDS as its command."""
    p = handshake(node, base)
    p.send(COMMANDS[name], b"\x01\x02\x03")
    synced = p.sync(7)
    time.sleep(0.3)
    p.read(0.3)
    log = notable(p)
    unknown = [m.group(1) for l in log for m in [UNKNOWN.search(l)] if m]
    out = {"command": COMMANDS[name].hex(), "core": outcome(p, synced), "log": log,
           "unknown_command": unknown[0] if unknown else None}
    p.close()
    return out


# V1 command -> (outcome, the type Core's "Unknown command" line names).
V1_COMMANDS = {
    "after_nul": ("dropped", None),
    "byte_7f": ("dropped", None),
    "byte_1f": ("dropped", None),
    "byte_80": ("dropped", None),
    "space": ("accept", "pi ng"),
    "empty": ("accept", ""),
}


def default_mode(node, base):
    r = {"pong": {k: pong_case(node, base, f) for k, f in PONGS.items()}}
    r["sendaddrv2"] = [sendaddrv2_case(node, base, v, post) for v in (70221, 70222, 70223, 70240)
                       for post in (False, True)]
    r["sendtxrcncl"] = [sendtxrcncl_case(node, base, post, pl) for post in (False, True) for pl in (RCNCL, b"\xde\xad")]
    # -peerbloomfilters defaults to on, so the handler reads the filter.
    r["bloom"] = {
        "filterload_truncated": send_case(node, base, "filterload", b"\x05\xaa\xbb"),
        "filterload_empty": send_case(node, base, "filterload", b""),
        "filteradd_truncated": send_case(node, base, "filteradd", b"\x05\xaa"),
        "filteradd_no_filter": send_case(node, base, "filteradd", b"\x01\xaa"),
    }
    r["v1_commands"] = {k: command_case(node, base, k) for k in V1_COMMANDS}
    # The same payloads Core rejects in blocks-only mode, as a control.
    r["blocks_only_control"] = {c: send_case(node, base, c, b"") for c in BLOCKS_ONLY}
    return r


def blocksonly_mode(node, base):
    return {"blocks_only": {c: send_case(node, base, c, b"") for c in BLOCKS_ONLY}}


def txrcncl_mode(node, base):
    return {"sendtxrcncl_enabled": [sendtxrcncl_case(node, base, post, RCNCL) for post in (False, True)]}


MODES = [("default", (), default_mode), ("blocksonly", ("-blocksonly=1",), blocksonly_mode),
         ("txreconciliation", ("-txreconciliation=1",), txrcncl_mode)]


def check(res):
    """The outcomes DDE-SWEEP §4 states; fails unless both releases show them."""
    for node, r in res["nodes"].items():
        def expect(label, got, want):
            assert got == want, (node, label, got, want)

        for name, case in r["pong"].items():
            want = {"mismatched_nonce": "ping pending", "matching_nonce_trailing": "ping measured"}.get(name, "ping cancelled")
            expect("pong " + name, case["core"], want)
            assert case["pingwait_before"], (node, name)
        for case in r["sendaddrv2"]:
            late = case["version"] >= 70223 and case["post_verack"]
            expect("sendaddrv2 %(version)s %(post_verack)s" % case, case["core"], "disconnect" if late else "accept")
            assert late == any("sendaddrv2 received after verack" in l for l in case["log"]), (node, case)
        for case in r["sendtxrcncl"]:
            expect("sendtxrcncl disabled", case["core"], "accept")
            assert any("does not have txreconciliation enabled" in l for l in case["log"]), (node, case)
            assert not any("Exception" in l for l in case["log"]), (node, case)
        for case in r["sendtxrcncl_enabled"]:
            late = case["post_verack"]
            expect("sendtxrcncl enabled", case["core"], "disconnect" if late else "accept")
            assert late == any("sendtxrcncl received after verack" in l for l in case["log"]), (node, case)
        reasons = {"filterload": "misformatted bloom filter", "filteradd": "bad filteradd message"}
        for name, case in r["bloom"].items():
            expect(name, case["core"], "disconnect")
            reason = reasons[name.split("_")[0]]
            assert any("(0 -> 100)" in l and reason in l for l in case["log"]), (node, name, case["log"])
        for name, case in r["v1_commands"].items():
            want, unknown = V1_COMMANDS[name]
            expect("command " + name, case["core"], "accept")
            dropped = any("Header error: Invalid message type" in l for l in case["log"])
            expect("command dropped " + name, dropped, want == "dropped")
            expect("command unknown " + name, case["unknown_command"], unknown)
        for cmd, case in r["blocks_only"].items():
            expect("blocks-only " + cmd, case["core"], "disconnect")
            assert any("violation of protocol" in l for l in case["log"]), (node, cmd, case["log"])
            assert not any("Exception" in l for l in case["log"]), (node, cmd, case["log"])
        controls = r["blocks_only_control"]
        for cmd, case in controls.items():
            expect("control " + cmd, case["core"], "accept")
        # qsendrecsigs is read and throws; qwatch scores a plain node +10;
        # tx (IBD) and dsq (not synced) are not read.
        assert any("ProcessMessages(qsendrecsigs," in l and "Exception" in l for l in controls["qsendrecsigs"]["log"])
        assert any("Misbehaving" in l and "(0 -> 10)" in l for l in controls["qwatch"]["log"]), controls["qwatch"]
        for cmd in ("tx", "dsq"):
            assert not any("Exception" in l or "Misbehaving" in l for l in controls[cmd]["log"]), (node, cmd)


def main():
    base = bytes.fromhex(json.load(open(sys.argv[1]))["version_payload"])
    res = {"nodes": {node: {} for node in nodes.NODES}}
    for mode, args, run in MODES:
        nodes.restart(*args)
        for node in nodes.NODES:
            res["nodes"][node].update(run(node, base))
    nodes.restart()
    json.dump(res, open(sys.argv[2], "w"), indent=1)
    check(res)
    print("all gate outcomes match DDE-SWEEP §4 at both pins")


if __name__ == "__main__":
    main()
