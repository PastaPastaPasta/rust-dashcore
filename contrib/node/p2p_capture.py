#!/usr/bin/env python3
"""Capture raw `mnlistdiff` / `qrinfo` wire payloads from a real Dash Core peer.

This is the only way to get these messages: dashd has no RPC that returns the wire encoding
(`protx diff` returns JSON). The payload bytes are written exactly as received, so the file is
byte-for-byte what Dash Core's `CSimplifiedMNListDiff` / `CQuorumRotationInfo` serializer produced
for this peer's negotiated protocol version.

Usage:
  p2p_capture.py --network mainnet --peer 127.0.0.1:27999 mnlistd BASE_HASH BLOCK_HASH OUT.bin
  p2p_capture.py --network mainnet --peer HOST:PORT qrinfo BLOCK_HASH EXTRA_SHARE OUT.bin [BASE_HASH...]
  (add --protocol N to announce another protocol version; see DEFAULT_PROTOCOL_VERSION)

IPv4 peers only. Hashes are given in the usual RPC (display, big-endian) hex form. A base hash of 64 zeros means
"from scratch". The tool prints the peer's user agent and protocol version so they can be recorded
in PROVENANCE.md. It sends no transactions, requests nothing but the one message, and disconnects.
"""

import argparse
import hashlib
import os
import socket
import struct
import sys
import time

MAGIC = {
    "mainnet": bytes.fromhex("bf0c6bbd"),
    "testnet": bytes.fromhex("cee2caff"),
}
# Protocol version announced in our `version` message. 70240 is what dash-spv sends. The version
# the peer settles on is min(ours, theirs) and changes what it replies: below 70236
# (EFFICIENT_QRINFO_VERSION) a `qrinfo` is built the legacy way: diffs are based only on the base
# hashes of the request, whereas the efficient construction also chains on the work blocks of the
# diffs it has already built (see BuildQuorumRotationInfo in Dash Core's llmq/snapshot.cpp).
DEFAULT_PROTOCOL_VERSION = 70240


def sha256d(b):
    return hashlib.sha256(hashlib.sha256(b).digest()).digest()


def compact(n):
    if n < 0xFD:
        return bytes([n])
    if n <= 0xFFFF:
        return b"\xfd" + struct.pack("<H", n)
    return b"\xfe" + struct.pack("<I", n)


def read_compact(buf, pos):
    first = buf[pos]
    if first < 0xFD:
        return first, pos + 1
    if first == 0xFD:
        return struct.unpack_from("<H", buf, pos + 1)[0], pos + 3
    return struct.unpack_from("<I", buf, pos + 1)[0], pos + 5


def netaddr(ip, port):
    return struct.pack("<Q", 0) + b"\x00" * 10 + b"\xff\xff" + socket.inet_aton(ip) + struct.pack(">H", port)


def frame(magic, command, payload):
    return (
        magic
        + command.encode().ljust(12, b"\x00")
        + struct.pack("<I", len(payload))
        + sha256d(payload)[:4]
        + payload
    )


def internal(display_hex):
    """RPC/display hex -> the 32 bytes in wire order."""
    return bytes.fromhex(display_hex)[::-1]


class Peer:
    def __init__(self, network, host, port, protocol=DEFAULT_PROTOCOL_VERSION, timeout=60):
        self.protocol = protocol
        self.magic = MAGIC[network]
        self.sock = socket.create_connection((host, port), timeout=timeout)
        self.buf = b""
        self.host, self.port = host, port
        self.user_agent = None
        self.version = None

    def send(self, command, payload=b""):
        self.sock.sendall(frame(self.magic, command, payload))

    def _read(self, n):
        while len(self.buf) < n:
            chunk = self.sock.recv(1 << 20)
            if not chunk:
                raise ConnectionError("peer closed the connection")
            self.buf += chunk
        out, self.buf = self.buf[:n], self.buf[n:]
        return out

    def recv(self):
        header = self._read(24)
        if header[:4] != self.magic:
            raise ValueError("bad network magic %s" % header[:4].hex())
        command = header[4:16].rstrip(b"\x00").decode()
        (length,) = struct.unpack("<I", header[16:20])
        payload = self._read(length)
        if sha256d(payload)[:4] != header[20:24]:
            raise ValueError("bad checksum on %s" % command)
        return command, payload

    def handshake(self):
        ua = b"/o4-vector-capture:0.1/"
        payload = (
            struct.pack("<iQq", self.protocol, 0, int(time.time()))
            + netaddr(self.host if self.host[0].isdigit() else "0.0.0.0", self.port)
            + netaddr("0.0.0.0", 0)
            + os.urandom(8)
            + compact(len(ua))
            + ua
            + struct.pack("<i", 0)
            + b"\x00"  # fRelay
            + os.urandom(32)  # mnauth challenge
            + b"\x00"  # not a masternode
        )
        self.send("version", payload)
        got_version = got_verack = False
        while not (got_version and got_verack):
            command, body = self.recv()
            if command == "version":
                self.version = struct.unpack_from("<i", body, 0)[0]
                pos = 4 + 8 + 8 + 26 + 26 + 8
                n, pos = read_compact(body, pos)
                self.user_agent = body[pos : pos + n].decode(errors="replace")
                got_version = True
                self.send("verack")
            elif command == "verack":
                got_verack = True
            elif command == "ping":
                self.send("pong", body)

    def request(self, command, payload, reply):
        self.send(command, payload)
        deadline = time.time() + 120
        while time.time() < deadline:
            cmd, body = self.recv()
            if cmd == reply:
                return body
            if cmd == "ping":
                self.send("pong", body)
            elif cmd == "reject":
                raise RuntimeError("peer rejected the request: %r" % body)
        raise TimeoutError("no %s reply" % reply)

    def close(self):
        try:
            self.sock.close()
        except OSError:
            pass


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--network", choices=sorted(MAGIC), required=True)
    ap.add_argument("--peer", required=True, help="host:port of a Dash Core node")
    ap.add_argument("--protocol", type=int, default=DEFAULT_PROTOCOL_VERSION,
                    help="protocol version to announce (default %(default)s)")
    sub = ap.add_subparsers(dest="what", required=True)
    m = sub.add_parser("mnlistd")
    m.add_argument("base")
    m.add_argument("block")
    m.add_argument("out")
    q = sub.add_parser("qrinfo")
    q.add_argument("block")
    q.add_argument("extra_share", type=int, choices=(0, 1))
    q.add_argument("out")
    q.add_argument("bases", nargs="*")
    args = ap.parse_args()

    host, port = args.peer.rsplit(":", 1)
    peer = Peer(args.network, host, int(port), args.protocol)
    try:
        peer.handshake()
        print("peer %s:%s subver=%s protocol=%d" % (host, port, peer.user_agent, peer.version), file=sys.stderr)
        if args.what == "mnlistd":
            body = peer.request("getmnlistd", internal(args.base) + internal(args.block), "mnlistdiff")
            # version u16, then baseBlockHash and blockHash: the reply must be for this request.
            if body[2:34] != internal(args.base) or body[34:66] != internal(args.block):
                sys.exit("the peer answered a different request than the one sent")
        else:
            payload = compact(len(args.bases)) + b"".join(internal(b) for b in args.bases)
            payload += internal(args.block) + bytes([args.extra_share])
            body = peer.request("getqrinfo", payload, "qrinfo")
        with open(args.out, "wb") as f:
            f.write(body)
        print("%s: %d bytes sha256=%s" % (args.out, len(body), hashlib.sha256(body).hexdigest()), file=sys.stderr)
    finally:
        peer.close()


if __name__ == "__main__":
    main()
