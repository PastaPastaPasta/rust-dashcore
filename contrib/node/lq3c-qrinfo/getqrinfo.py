#!/usr/bin/env python3
"""Asks a dashd for `qrinfo` replies over P2P at chosen protocol versions, and records refusals.

Usage: getqrinfo.py HOST:PORT MAGIC_HEX DEBUG_LOG REQUESTS.jsonl > OUTCOMES.jsonl

Each request line is {"case": str, "bases": [hex], "block": hex, "extra": bool, "proto": int} with the
hashes in RPC (display) order, plus "fresh": true for a request that gets its own connection. The
others share one connection per protocol version: Core serializes the reply at the version both
sides agreed on (`msgMaker(pfrom.GetCommonVersion())`, net_processing.cpp v24:5695) and picks the
legacy construction below EFFICIENT_QRINFO_VERSION 70236 (:5693).

Every request is followed by a `ping`: a `qrinfo` before the `pong` means Core answered, the `pong`
alone that it refused. The outcome line adds "answered", "payload" (hex, when answered) and "log":
the lines the request added to DEBUG_LOG that mention `getquorumrotationinfo` or `Misbehaving`
(Core scores a refusal with `Misbehaving(peer, 1, ...)` and only logs an unavailable-data one,
:5697-5701; the node runs with -debug=net).

The peer is a copy of contrib/node/ev4-serve/getmnlistd.py's, with `getqrinfo` added; its
hashing and CompactSize helpers are getmnlistd.py's own.
"""

import json
import os
import socket
import struct
import sys
import time

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "ev4-serve"))
from getmnlistd import compact, sha256d  # noqa: E402


class Peer:
    def __init__(self, addr, magic, proto):
        host, port = addr.rsplit(":", 1)
        self.sock = socket.create_connection((host, int(port)), timeout=300)
        self.magic = magic
        self.buf = b""
        net_addr = struct.pack("<Q", 0) + b"\x00" * 10 + b"\xff\xff\x7f\x00\x00\x01" + struct.pack(">H", 0)
        version = (
            struct.pack("<iQq", proto, 0, int(time.time()))
            + net_addr
            + net_addr
            + struct.pack("<Q", 0x5EED5EED5EED5EED)
            + compact(len(b"/lq3c:0.1/"))
            + b"/lq3c:0.1/"
            + struct.pack("<i", 0)
            + b"\x00"
        )
        self.send("version", version)
        got_verack = got_version = False
        while not (got_verack and got_version):
            cmd, payload = self.recv()
            if cmd == "version":
                got_version = True
                self.send("verack", b"")
            elif cmd == "verack":
                got_verack = True
            elif cmd == "ping":
                self.send("pong", payload)

    def send(self, cmd, payload):
        header = self.magic + cmd.encode().ljust(12, b"\x00") + struct.pack("<I", len(payload)) + sha256d(payload)[:4]
        self.sock.sendall(header + payload)

    def recv(self):
        while True:
            if len(self.buf) >= 24:
                length = struct.unpack("<I", self.buf[16:20])[0]
                if len(self.buf) >= 24 + length:
                    if self.buf[:4] != self.magic:
                        raise RuntimeError("bad magic")
                    cmd = self.buf[4:16].rstrip(b"\x00").decode()
                    payload = self.buf[24 : 24 + length]
                    if sha256d(payload)[:4] != self.buf[20:24]:
                        raise RuntimeError("bad checksum")
                    self.buf = self.buf[24 + length :]
                    return cmd, payload
            chunk = self.sock.recv(1 << 20)
            if not chunk:
                raise RuntimeError("connection closed")
            self.buf += chunk

    def getqrinfo(self, bases, block, extra):
        """`CGetQuorumRotationInfo` (llmq/snapshot.h v24:110-113), then a ping; the reply or None."""
        body = compact(len(bases)) + b"".join(bytes.fromhex(b)[::-1] for b in bases)
        body += bytes.fromhex(block)[::-1] + bytes([1 if extra else 0])
        self.send("getqrinfo", body)
        nonce = os.urandom(8)
        self.send("ping", nonce)
        payload = None
        while True:
            cmd, got = self.recv()
            if cmd == "qrinfo":
                payload = got
            elif cmd == "pong" and got == nonce:
                return payload
            elif cmd == "ping":
                self.send("pong", got)


def log_lines(log, start):
    with open(log, "rb") as f:
        f.seek(start)
        added = f.read().decode(errors="replace").splitlines()
    return [
        l.split(" ", 1)[1] if " " in l else l
        for l in added
        if "getquorumrotationinfo" in l or "Misbehaving" in l
    ]


def main():
    addr, magic, log, path = sys.argv[1], bytes.fromhex(sys.argv[2]), sys.argv[3], sys.argv[4]
    requests = [json.loads(line) for line in open(path) if line.strip()]
    peers = {}
    for req in requests:
        proto = req["proto"]
        start = os.path.getsize(log)
        if req.get("fresh"):
            peer = Peer(addr, magic, proto)
        else:
            if proto not in peers:
                peers[proto] = Peer(addr, magic, proto)
            peer = peers[proto]
        payload = peer.getqrinfo(req["bases"], req["block"], req["extra"])
        if req.get("fresh"):
            peer.sock.close()
        req["answered"] = payload is not None
        if payload is not None:
            req["payload"] = payload.hex()
        else:
            time.sleep(1)  # let the node flush its log lines
            req["log"] = log_lines(log, start)
            if not req.get("fresh"):
                # a shared connection carries no refusal: Core would score it
                raise RuntimeError(f"refused on a shared connection: {req['case']} {req['log']}")
        print(json.dumps(req), flush=True)


if __name__ == "__main__":
    main()
