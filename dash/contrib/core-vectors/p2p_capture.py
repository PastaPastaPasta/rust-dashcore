#!/usr/bin/env python3
"""Minimal Dash P2P client (regtest, protocol 70242) to capture raw messages from a local dashd."""
import hashlib
import os
import socket
import struct
import time

MAGIC = bytes.fromhex("fcc1b7dc")
PROTOCOL_VERSION = 70242


def sha256d(b):
    return hashlib.sha256(hashlib.sha256(b).digest()).digest()


def msg(command, payload):
    return MAGIC + command.encode().ljust(12, b"\0") + struct.pack("<I", len(payload)) + sha256d(payload)[:4] + payload


def varint(n):
    if n < 0xfd:
        return bytes([n])
    return b"\xfd" + struct.pack("<H", n)


def net_addr():
    return struct.pack("<Q", 0) + bytes(10) + b"\xff\xff" + bytes([127, 0, 0, 1]) + struct.pack(">H", 0)


def version_payload():
    ua = b"/v24a-capture/"
    return (struct.pack("<iQq", PROTOCOL_VERSION, 0, int(time.time())) + net_addr() + net_addr() +
            os.urandom(8) + varint(len(ua)) + ua + struct.pack("<i", 0) + b"\x00")


def recv_exact(sock, n):
    data = b""
    while len(data) < n:
        chunk = sock.recv(n - len(data))
        if not chunk:
            raise EOFError
        data += chunk
    return data


def read_msg(sock):
    hdr = recv_exact(sock, 24)
    assert hdr[:4] == MAGIC, hdr
    command = hdr[4:16].rstrip(b"\0").decode()
    (length,) = struct.unpack("<I", hdr[16:20])
    payload = recv_exact(sock, length)
    assert sha256d(payload)[:4] == hdr[20:24]
    return command, payload


def wait_for(sock, wanted, timeout=30):
    sock.settimeout(timeout)
    while True:
        command, payload = read_msg(sock)
        if command == "ping":
            sock.sendall(msg("pong", payload))
        if command in wanted:
            return command, payload


def request(port, command, payload, reply, timeout=30):
    """The payload of the first `reply` to one `command` sent after the handshake."""
    sock = socket.create_connection(("127.0.0.1", port))
    try:
        sock.sendall(msg("version", version_payload()))
        wait_for(sock, {"version"})
        sock.sendall(msg("verack", b""))
        wait_for(sock, {"verack"})
        sock.sendall(msg(command, payload))
        return wait_for(sock, {reply}, timeout)[1]
    finally:
        sock.close()


def getmnlistd(port, base_hash, block_hash_hex):
    """Raw `mnlistdiff` payload for `getmnlistd(base_hash, block_hash)` (hashes in RPC hex order)."""
    return request(port, "getmnlistd", base_hash + bytes.fromhex(block_hash_hex)[::-1], "mnlistdiff")
