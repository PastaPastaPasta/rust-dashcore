#!/usr/bin/env python3
"""Asks a dashd for `mnlistdiff` replies over P2P at chosen protocol versions.

Usage: getmnlistd.py HOST:PORT MAGIC_HEX REQUESTS.jsonl > REPLIES.jsonl

Each request line is {"base": hex, "block": hex, "proto": int} with the hashes in RPC (display)
order; each reply line adds "payload" (hex of the mnlistdiff message payload). One connection per
protocol version: Core serializes the reply at the version both sides agreed on
(`msgMaker(pfrom.GetCommonVersion())`, net_processing.cpp v24:5611-5633).
"""

import hashlib
import json
import socket
import struct
import sys
import time


def sha256d(b):
    return hashlib.sha256(hashlib.sha256(b).digest()).digest()


def compact(n):
    if n < 0xFD:
        return bytes([n])
    if n <= 0xFFFF:
        return b"\xfd" + struct.pack("<H", n)
    return b"\xfe" + struct.pack("<I", n)


class Peer:
    def __init__(self, addr, magic, proto):
        host, port = addr.rsplit(":", 1)
        self.sock = socket.create_connection((host, int(port)), timeout=60)
        self.magic = magic
        self.buf = b""
        net_addr = struct.pack("<Q", 0) + b"\x00" * 10 + b"\xff\xff\x7f\x00\x00\x01" + struct.pack(">H", 0)
        version = (
            struct.pack("<iQq", proto, 0, int(time.time()))
            + net_addr
            + net_addr
            + struct.pack("<Q", 0x5EED5EED5EED5EED)
            + compact(len(b"/ev4s:0.1/"))
            + b"/ev4s:0.1/"
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

    def getmnlistd(self, base, block):
        self.send("getmnlistd", bytes.fromhex(base)[::-1] + bytes.fromhex(block)[::-1])
        while True:
            cmd, payload = self.recv()
            if cmd == "mnlistdiff":
                return payload
            if cmd == "ping":
                self.send("pong", payload)


def main():
    addr, magic, path = sys.argv[1], bytes.fromhex(sys.argv[2]), sys.argv[3]
    requests = [json.loads(line) for line in open(path) if line.strip()]
    peers = {}
    for req in requests:
        proto = req["proto"]
        if proto not in peers:
            peers[proto] = Peer(addr, magic, proto)
        req["payload"] = peers[proto].getmnlistd(req["base"], req["block"]).hex()
        print(json.dumps(req), flush=True)


if __name__ == "__main__":
    main()
