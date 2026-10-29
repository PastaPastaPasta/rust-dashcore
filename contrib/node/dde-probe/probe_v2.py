#!/usr/bin/env python3
"""Core's V2 (BIP324) contents limit at both pins: short-ID vs long-form type.

Usage: probe_v2.py OUT.json
Needs both nodes running (nodes.py start) and Core's functional test framework
for the encryption: DDE_CORE_TEST_FRAMEWORK names a test/functional directory
from the v24.0.0-rc.3 tag (git archive v24.0.0-rc.3 test/functional).

Each case sends one encrypted packet whose contents are a ping type (short ID
18, or the 13-byte long form) and a payload of the given length: the nonce
0x01245678, then zeros Core's handler ignores. Unassigned short IDs (0xfe, 33)
show which check runs first, and short ID 5, a slot Core reserves without a
message, what an empty type does. The "long_*" cases send p2p.COMMANDS as
long-form types. main() fails unless both releases show the outcomes
DDE-SWEEP §4.0 states.
"""
import json
import os
import struct
import sys
import time

import nodes
from p2p import COMMANDS, UNKNOWN, peer_lines

sys.path.insert(0, os.environ["DDE_CORE_TEST_FRAMEWORK"])
from test_framework.p2p import NetworkThread, P2PInterface  # noqa: E402

MIB3 = 3 * 1024 * 1024
# (type form, payload length) -> the outcome both releases must show.
CASES = {
    ("short", MIB3): "accept",
    ("short", MIB3 + 1): "accept",
    ("short", MIB3 + 12): "accept",
    ("short", MIB3 + 13): "disconnect: packet too large",
    ("long", MIB3): "accept",
    ("long", MIB3 + 1): "disconnect: packet too large",
    ("unassigned", 8): "dropped: invalid message type",
    ("unassigned", MIB3 + 13): "disconnect: packet too large",
    ("unassigned_33", 8): "dropped: invalid message type",
    ("reserved_5", 8): "accept: Unknown command",
    ("long_after_nul", 8): "dropped: invalid message type",
    ("long_byte_7f", 8): "accept: Unknown command",
    ("long_byte_1f", 8): "dropped: invalid message type",
    ("long_byte_80", 8): "dropped: invalid message type",
    ("long_space", 8): "accept: Unknown command",
    ("long_empty", 8): "accept: Unknown command",
}
TYPES = {"short": bytes([18]), "long": b"\0" + b"ping".ljust(12, b"\0"), "unassigned": bytes([0xFE]),
         "unassigned_33": bytes([33]), "reserved_5": bytes([5]),
         **{"long_" + k: b"\0" + v.ljust(12, b"\0") for k, v in COMMANDS.items()}}
# The type Core's "Unknown command" line must name (SanitizeString drops 0x7f).
UNKNOWN_TYPE = {"reserved_5": "", "long_byte_7f": "ping", "long_space": "pi ng", "long_empty": ""}


class Quiet(P2PInterface):
    """Sends nothing on its own once connected.

    The framework encrypts a packet in the calling thread and writes it from
    the network thread, so an automatic pong could take the next cipher
    counter between encrypting a probe packet and writing it.
    """

    def on_senddsq(self, message):
        pass

    def on_ping(self, message):
        pass


def case(node, form, size):
    p = Quiet()
    p.p2p_connected_to_node = True
    p.peer_connect(dstaddr="127.0.0.1", dstport=nodes.p2p_port(node), net="regtest", timeout_factor=1,
                   supports_v2_p2p=True, send_version=True)()
    p.wait_for_connect(timeout=30)
    p.wait_for_verack(timeout=30)
    local = "127.0.0.1:%d" % p._transport.get_extra_info("sockname")[1]
    peer_id = next(x["id"] for x in nodes.cli(node, "getpeerinfo") if x["addr"] == local)
    pos = nodes.log_path(node).stat().st_size
    contents = TYPES[form] + struct.pack("<Q", 0x01245678) + bytes(size - 8)
    p.send_raw_message(p.v2_state.v2_enc_packet(contents))
    try:
        p.sync_with_ping(timeout=20)
        synced = True
    except Exception:
        synced = False
    time.sleep(0.2)
    closed = not p.is_connected
    lines = peer_lines(nodes.log_path(node).read_bytes()[pos:].decode(errors="replace").splitlines(), peer_id)
    log = [l.split(" ", 1)[-1] for l in lines
           if "V2 transport error" in l or "received: ping (%d bytes)" % size in l or "Unknown command" in l
           or "Exception" in l or "Misbehaving" in l]
    if closed:
        core = "disconnect: packet too large" if any("packet too large" in l for l in log) else "disconnect"
    elif not synced:
        core = "no answer"
    elif any("invalid message type" in l for l in log):
        core = "dropped: invalid message type"
    elif any("Unknown command" in l for l in log):
        core = "accept: Unknown command"
    elif form in ("short", "long") and any("received: ping" in l for l in log):
        core = "accept"
    else:
        core = "unknown"
    if p.is_connected:
        p.peer_disconnect()
        p.wait_for_disconnect()
    unknown = [m.group(1) for l in log for m in [UNKNOWN.search(l)] if m]
    return {"form": form, "payload_len": size, "contents_len": len(contents), "core": core,
            "unknown_command": unknown[0] if unknown else None, "log": log}


def main():
    network = NetworkThread()
    network.start()
    res = {"nodes": {node: [case(node, f, n) for f, n in CASES] for node in nodes.NODES}}
    network.close()
    json.dump(res, open(sys.argv[1], "w"), indent=1)
    for node, rows in res["nodes"].items():
        for r in rows:
            print(node, r["form"], r["payload_len"], r["core"])
            assert r["core"] == CASES[(r["form"], r["payload_len"])], (node, r)
            assert r["unknown_command"] == UNKNOWN_TYPE.get(r["form"]), (node, r)
            assert not any("Exception" in l or "Misbehaving" in l for l in r["log"]), (node, r)
    print("all V2 outcomes match DDE-SWEEP §4.0 at both pins")


if __name__ == "__main__":
    main()
