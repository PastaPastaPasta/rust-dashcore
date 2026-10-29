#!/usr/bin/env python3
"""Fetches the live sporks of Dash mainnet and testnet: `getsporks` to public peers.

Each line written is `NETWORK HOST:PORT SPORK_PAYLOAD_HEX`, one per spork a peer sent; a spork
several peers sent is written once. The payloads are what the networks' spork signers signed
(mainnet: the message form, testnet: the hash form), as relayed by the peers' dashd.

Usage: live_sporks.py OUT_FILE [PEERS_PER_NETWORK]
"""

import hashlib
import os
import random
import socket
import struct
import sys
import time

NETS = {
    # network: (pchMessageStart, port, DNS seeds)
    "mainnet": (bytes.fromhex("bf0c6bbd"), 9999, ["dnsseed.dash.org", "dnsseed.dashdot.io"]),
    "testnet": (bytes.fromhex("cee2caff"), 19999, ["testnet-seed.dashdot.io"]),
}
UA = b"/sp0-live-sporks:1/"  # DEC-83: the tool says who it is


def sha256d(b):
    return hashlib.sha256(hashlib.sha256(b).digest()).digest()


def frame(magic, cmd, payload=b""):
    return magic + cmd.encode().ljust(12, b"\0") + struct.pack("<I", len(payload)) + sha256d(payload)[:4] + payload


def version_payload():
    addr = struct.pack("<Q", 0) + bytes(18)
    return (struct.pack("<iQq", 70242, 0, int(time.time())) + addr + addr
            + struct.pack("<Q", random.getrandbits(64)) + bytes([len(UA)]) + UA
            + struct.pack("<i", 0) + b"\x00" + os.urandom(32) + b"\x00")


def fetch(magic, host, port, seconds=20):
    s = socket.create_connection((host, port), timeout=10)
    s.sendall(frame(magic, "version", version_payload()))
    buf, sporks, end, asked = b"", [], time.time() + seconds, False
    while time.time() < end:
        s.settimeout(max(0.1, end - time.time()))
        try:
            chunk = s.recv(1 << 20)
        except socket.timeout:
            break
        if not chunk:
            break
        buf += chunk
        while len(buf) >= 24:
            n, = struct.unpack_from("<I", buf, 16)
            if len(buf) < 24 + n:
                break
            cmd, payload, buf = buf[4:16].rstrip(b"\0").decode(), buf[24:24 + n], buf[24 + n:]
            if cmd == "version":
                s.sendall(frame(magic, "verack"))
            elif cmd == "verack" and not asked:
                s.sendall(frame(magic, "getsporks"))
                asked, end = True, time.time() + 10
            elif cmd == "ping":
                s.sendall(frame(magic, "pong", payload))
            elif cmd == "spork":
                sporks.append(payload)
    s.close()
    return sporks


def seed_file(net):
    """The masternode addresses (`TYPE IP:PORT`) in `dash-network-seeds/seeds/NETWORK.txt`, IPv4 only."""
    path = os.path.join(os.path.dirname(__file__), "..", "..", "..", "dash-network-seeds", "seeds", f"{net}.txt")
    out = []
    for line in open(path):
        fields = line.split("#")[0].split()
        if fields and fields[-1].count(":") == 1:
            out.append(fields[-1].split(":")[0])
    return out


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    want = int(sys.argv[2]) if len(sys.argv) > 2 else 3
    seen, lines = set(), []
    for net, (magic, port, seeds) in NETS.items():
        hosts = set()
        for seed in seeds:
            try:
                hosts |= {a[4][0] for a in socket.getaddrinfo(seed, port, socket.AF_INET)}
            except OSError as e:
                print(f"{net} {seed}: {e}", file=sys.stderr)
        hosts |= set(seed_file(net))
        hosts = sorted(hosts)
        random.shuffle(hosts)
        ok = 0
        for host in hosts:
            if ok >= want:
                break
            try:
                sporks = fetch(magic, host, port)
            except OSError as e:
                print(f"{net} {host}: {e}", file=sys.stderr)
                continue
            print(f"{net} {host}:{port}: {len(sporks)} sporks", file=sys.stderr)
            if not sporks:
                continue
            ok += 1
            for p in sporks:
                if (net, p) not in seen:
                    seen.add((net, p))
                    lines.append(f"{net} {host}:{port} {p.hex()}")
    with open(sys.argv[1], "w") as f:
        f.write("\n".join(lines) + "\n")


if __name__ == "__main__":
    main()
