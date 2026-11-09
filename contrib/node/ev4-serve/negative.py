#!/usr/bin/env python3
"""Sends `getmnlistd` requests a dashd should refuse and records what it does with each one.

Usage: negative.py HOST:PORT MAGIC_HEX DEBUG_LOG REQUESTS.jsonl > OUTCOMES.jsonl

Each request line is {"case": str, "base": hex, "block": hex} with the hashes in RPC (display)
order. Each is sent on its own fresh connection at protocol 70242, followed by a `ping`: a
`mnlistdiff` before the `pong` means Core answered, the `pong` alone means it stayed silent. The
outcome line adds "answered" (bool), "payload" (hex, when answered) and "log": the lines the
request added to DEBUG_LOG that mention `getmnlistdiff` or `Misbehaving` (Core scores a refusal
with `Misbehaving(peer, 1, ...)` and only logs an unavailable-data one,
net_processing.cpp v24:5622-5633; the node must run with -debug=net).
"""

import json
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from getmnlistd import Peer  # noqa: E402

PROTO = 70242


def ask(peer, base, block):
    peer.send("getmnlistd", bytes.fromhex(base)[::-1] + bytes.fromhex(block)[::-1])
    nonce = os.urandom(8)
    peer.send("ping", nonce)
    payload = None
    while True:
        cmd, body = peer.recv()
        if cmd == "mnlistdiff":
            payload = body
        elif cmd == "pong" and body == nonce:
            return payload
        elif cmd == "ping":
            peer.send("pong", body)


def main():
    addr, magic, log, path = sys.argv[1], bytes.fromhex(sys.argv[2]), sys.argv[3], sys.argv[4]
    for line in open(path):
        if not line.strip():
            continue
        req = json.loads(line)
        start = os.path.getsize(log)
        peer = Peer(addr, magic, PROTO)
        payload = ask(peer, req["base"], req["block"])
        peer.sock.close()
        time.sleep(1)  # let the node flush its log lines
        with open(log, "rb") as f:
            f.seek(start)
            added = f.read().decode(errors="replace").splitlines()
        req["answered"] = payload is not None
        if payload is not None:
            req["payload"] = payload.hex()
        req["log"] = [
            l.split(" ", 1)[1] if " " in l else l
            for l in added
            if "getmnlistdiff" in l or "Misbehaving" in l
        ]
        print(json.dumps(req), flush=True)


if __name__ == "__main__":
    main()
