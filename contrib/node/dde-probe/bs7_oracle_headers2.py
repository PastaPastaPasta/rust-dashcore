#!/usr/bin/env python3
"""The whole mainnet or testnet header chain as Core's getheaders2 replies.

Usage: bs7_oracle_headers2.py mainnet|testnet OUT.json [SAMPLE_DIR]
Peers with the reference oracle (ORACLE.md: v24.0.0-rc.3, P2P inbound from
127.0.0.1 with noban) as a plain inbound peer that advertises compressed
headers, and walks the chain with getheaders2 from genesis. Every reply goes
through bs7-tool roundtrip: base-sdk must decode it, re-encode it byte for
byte and in Core's reply mode, and continue the previous reply's chain; the
last header of every reply must be the oracle's block at that height
(getblockhash over RPC). It only reads: no RPC call changes the oracle.
SAMPLE_DIR, if given, receives short replies (getheaders2 with a stop hash,
SAMPLE_LEN headers) at the two windows whose replies held the most distinct
versions, for the base-sdk corpus.
"""
import base64
import hashlib
import http.client
import json
import os
import socket
import struct
import subprocess
import sys
import time

NETS = {
    # magic, P2P port, RPC port, cookie
    "mainnet": (bytes.fromhex("bf0c6bbd"), 27999, 27998, "/work/chains/mainnet/rpc.cookie"),
    "testnet": (bytes.fromhex("cee2caff"), 37999, 37998, "/work/chains/testnet/rpc.cookie"),
}
TOOL = os.environ["BS7_TOOL"]
SAMPLE_LEN = 300


def sha256d(b):
    return hashlib.sha256(hashlib.sha256(b).digest()).digest()


def rpc(net, method, *params):
    _, _, port, cookie = NETS[net]
    auth = base64.b64encode(open(cookie).read().strip().encode()).decode()
    c = http.client.HTTPConnection("127.0.0.1", port, timeout=60)
    c.request("POST", "/", json.dumps({"method": method, "params": list(params)}),
              {"Authorization": "Basic " + auth, "Content-Type": "application/json"})
    r = json.loads(c.getresponse().read())
    if r.get("error"):
        raise RuntimeError(r["error"])
    return r["result"]


class Conn:
    def __init__(self, net):
        self.magic, port, _, _ = NETS[net]
        self.sock = socket.create_connection(("127.0.0.1", port), timeout=60)
        self.buf = b""

    def send(self, cmd, payload=b""):
        self.sock.sendall(self.magic + cmd.encode().ljust(12, b"\0") + struct.pack("<I", len(payload))
                          + sha256d(payload)[:4] + payload)

    def _fill(self, n):
        while len(self.buf) < n:
            chunk = self.sock.recv(1 << 20)
            if not chunk:
                raise RuntimeError("oracle closed the connection")
            self.buf += chunk

    def recv(self):
        while True:
            self._fill(24)
            n, = struct.unpack_from("<I", self.buf, 16)
            self._fill(24 + n)
            cmd = self.buf[4:16].rstrip(b"\0").decode()
            payload, self.buf = self.buf[24:24 + n], self.buf[24 + n:]
            if cmd == "ping":
                self.send("pong", payload)
                continue
            return cmd, payload

    def getheaders2(self, locator, stop=b"\0" * 32):
        """Core's reply to a getheaders2 from `locator` (display hex)."""
        self.send("getheaders2", struct.pack("<i", 70240) + b"\x01" + bytes.fromhex(locator)[::-1] + stop)
        while True:
            cmd, payload = self.recv()
            if cmd == "headers2":
                return payload


def version_payload():
    services = 1 << 11  # NODE_HEADERS_COMPRESSED
    addr = struct.pack("<Q", 0) + b"\0" * 10 + b"\xff\xff\x7f\x00\x00\x01" + struct.pack(">H", 0)
    ua = b"/bs7-probe:0.1/"
    return (struct.pack("<iQq", 70240, services, int(time.time())) + addr + addr
            + struct.pack("<Q", int.from_bytes(os.urandom(8), "little")) + bytes([len(ua)]) + ua
            + struct.pack("<i", 0) + b"\x00" + os.urandom(32) + b"\x00")


def main():
    net, out_path = sys.argv[1], sys.argv[2]
    sample_dir = sys.argv[3] if len(sys.argv) > 3 else None
    tool = subprocess.Popen([TOOL, "roundtrip"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    c = Conn(net)
    c.send("version", version_payload())
    got = set()
    while not {"version", "verack"} <= got:
        cmd, _ = c.recv()
        got.add(cmd)
    c.send("verack")
    tip_height = rpc(net, "getblockcount")
    prev, height, msgs, failures, versions, sizes = rpc(net, "getblockhash", 0), 0, 0, [], set(), []
    t0 = time.time()
    first = True
    per_msg = []
    while height < tip_height:
        payload = c.getheaders2(prev)
        tool.stdin.write(json.dumps({"id": msgs, "cmd": "headers2", "payload": payload.hex(), "version": 70240}) + "\n")
        tool.stdin.flush()
        r = json.loads(tool.stdout.readline())
        msgs += 1
        if not r.get("ok") or r["count"] == 0:
            failures.append({"height": height, "result": r})
            break
        start = height + 1
        height += r["count"]
        want_last = rpc(net, "getblockhash", height)
        bad = []
        if not r["reencodes"]:
            bad.append("reencode")
        if not r["reply_mode"]:
            bad.append("reply_mode")
        if r["first_prev"] != prev:
            bad.append("continuity")
        if r["last"] != want_last:
            bad.append("last_hash")
        if bad:
            failures.append({"height": start, "count": r["count"], "failed": bad})
        versions.add(r["versions"])
        per_msg.append((r["versions"], start))
        sizes.append(len(payload))
        prev = r["last"]
        if first or msgs % 50 == 0:
            print(net, "height", height, "msgs", msgs, "failures", len(failures), file=sys.stderr)
            first = False
    samples = []
    if sample_dir:
        for _, start in sorted(per_msg, reverse=True)[:2]:
            # A reply from `start`: locator at start - 1, stop SAMPLE_LEN later.
            stop = bytes.fromhex(rpc(net, "getblockhash", start - 1 + SAMPLE_LEN))[::-1]
            payload = c.getheaders2(rpc(net, "getblockhash", start - 1), stop)
            name = "%s-%d" % (net, start)
            with open(os.path.join(sample_dir, name + ".hex"), "w") as f:
                f.write(payload.hex())
            samples.append(name)
    out = {"net": net, "samples": samples, "tip_height": tip_height, "headers": height, "messages": msgs,
           "bytes": sum(sizes), "max_versions_in_a_message": max(versions or {0}),
           "failures": failures, "seconds": round(time.time() - t0, 1)}
    json.dump(out, open(out_path, "w"), indent=1)
    print(json.dumps(out))


if __name__ == "__main__":
    main()
