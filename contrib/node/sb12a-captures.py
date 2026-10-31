#!/usr/bin/env python3
"""Capture the mainnet and testnet messages `dash-spv::bridge`'s round trips run over (SB12a).

Usage: sb12a-captures.py OUT_DIR [--listen SECONDS]

Reads the shared reference oracles only (docs/node-program/ORACLE.md), one request at a time,
with calls from the pure-read list: RPC `getblockhash`, `getblock` (verbosity 0 and 2),
`getblockfilter`, `getbestchainlock` and `getislocks`, and over P2P a plain inbound peer that
sends `getheaders`, `getheaders2` and `getdata` for what the oracle announces. Nothing here
builds quorums or writes to the oracle (no `protx diff`, `quorum *`, `getmnlistdiff` or
`getqrinfo`).

What each file is (OUT_DIR/<net>/<kind>-<label>.bin, the payload of one P2P message):

- `headers`, `headers2`: Core's replies to `getheaders` / `getheaders2`, byte for byte.
- `clsig`, `isdlock`: Core's replies to `getdata` after it announced them (testnet, `--listen`).
- `clsig` from RPC: `getbestchainlock`'s `hex` (testnet), or for mainnet, whose oracle is in IBD
  and holds no ChainLock, the one a coinbase carries (`bestCLSignature` for height
  h - `bestCLHeightDiff` - 1), written as Core serializes `CChainLockSig`.
- `isdlock` from RPC: `getislocks`' `hex` (testnet).
- `block`: `getblock <hash> 0`, Core's serialization of the block.
- `cfilter`, `cfheaders`: the oracles serve no BIP157 messages (no `-peerblockfilters`), so these
  are written from `getblockfilter`'s filter and headers as Core's `CFilter` / `CFHeaders`
  serialize them (filter type 0, the filter hashes being SHA256d of each filter).

Mainnet stays at or below height 2,090,000 (PLAN §7 handling). A manifest with every file's
size, sha256 and source is written to OUT_DIR/manifest.json.
"""
import base64
import hashlib
import http.client
import json
import os
import socket
import struct
import sys
import time

NETS = {
    # magic, P2P port, RPC port, cookie
    "mainnet": (bytes.fromhex("bf0c6bbd"), 27999, 27998, "/work/chains/mainnet/rpc.cookie"),
    "testnet": (bytes.fromhex("cee2caff"), 37999, 37998, "/work/chains/testnet/rpc.cookie"),
}
MAINNET_CAP = 2_090_000
PROTOCOL = 70240
MSG_CLSIG = 29
MSG_ISDLOCK = 31


def sha256d(b):
    return hashlib.sha256(hashlib.sha256(b).digest()).digest()


def compact_size(n):
    if n < 0xFD:
        return bytes([n])
    if n <= 0xFFFF:
        return b"\xfd" + struct.pack("<H", n)
    if n <= 0xFFFFFFFF:
        return b"\xfe" + struct.pack("<I", n)
    return b"\xff" + struct.pack("<Q", n)


def rpc(net, method, *params):
    _, _, port, cookie = NETS[net]
    auth = base64.b64encode(open(cookie).read().strip().encode()).decode()
    c = http.client.HTTPConnection("127.0.0.1", port, timeout=120)
    c.request("POST", "/", json.dumps({"method": method, "params": list(params)}),
              {"Authorization": "Basic " + auth, "Content-Type": "application/json"})
    r = json.loads(c.getresponse().read())
    if r.get("error"):
        raise RuntimeError("%s %s: %s" % (method, params, r["error"]))
    return r["result"]


def internal(display_hex):
    return bytes.fromhex(display_hex)[::-1]


class Conn:
    def __init__(self, net):
        self.magic, port, _, _ = NETS[net]
        self.sock = socket.create_connection(("127.0.0.1", port), timeout=60)
        self.buf = b""
        self.send("version", version_payload())
        got = set()
        while not {"version", "verack"} <= got:
            cmd, _ = self.recv()
            got.add(cmd)
        self.send("verack")

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

    def request(self, cmd, payload, want):
        self.send(cmd, payload)
        while True:
            got, reply = self.recv()
            if got == want:
                return reply

    def close(self):
        self.sock.close()


def version_payload():
    services = 1 << 11  # NODE_HEADERS_COMPRESSED
    addr = struct.pack("<Q", 0) + b"\0" * 10 + b"\xff\xff\x7f\x00\x00\x01" + struct.pack(">H", 0)
    ua = b"/sb12a-capture:0.1/"
    # start height 0, relay 1 (so the oracle announces transactions and their locks), the
    # masternode auth challenge, not a masternode.
    return (struct.pack("<iQq", PROTOCOL, services, int(time.time())) + addr + addr
            + struct.pack("<Q", int.from_bytes(os.urandom(8), "little")) + bytes([len(ua)]) + ua
            + struct.pack("<i", 0) + b"\x01" + os.urandom(32) + b"\x00")


class Out:
    def __init__(self, root):
        self.root = root
        self.manifest = []

    def write(self, net, kind, label, payload, source):
        d = os.path.join(self.root, net)
        os.makedirs(d, exist_ok=True)
        name = "%s-%s.bin" % (kind, label)
        with open(os.path.join(d, name), "wb") as f:
            f.write(payload)
        self.manifest.append({"net": net, "kind": kind, "file": "%s/%s" % (net, name),
                              "size": len(payload), "sha256": hashlib.sha256(payload).hexdigest(),
                              "source": source})
        print(net, kind, label, len(payload), file=sys.stderr, flush=True)


def under_cap(net, top):
    """Refuse, before any read, a mainnet capture reaching above the PLAN §7 cap; `top` is the
    highest block the capture can cover. An explicit exit, not an `assert`, so `python3 -O`
    keeps it (test-sb12a-captures.py)."""
    if net == "mainnet" and top > MAINNET_CAP:
        raise SystemExit("mainnet capture to %d, above the cap %d" % (top, MAINNET_CAP))


def testnet_only(net):
    """Refuse a mainnet read whose height has no bound (a lock at the tip)."""
    if net != "testnet":
        raise SystemExit("%s: only testnet reads are unbounded by height" % net)


def headers_replies(out, net, height):
    """Core's `headers` and `headers2` replies from the block at `height`."""
    under_cap(net, height + 8000)  # a headers2 reply holds up to 8,000 headers
    locator = internal(rpc(net, "getblockhash", height))
    stop = b"\0" * 32
    c = Conn(net)
    try:
        h = c.request("getheaders", struct.pack("<i", PROTOCOL) + b"\x01" + locator + stop, "headers")
        h2 = c.request("getheaders2", struct.pack("<i", PROTOCOL) + b"\x01" + locator + stop, "headers2")
    finally:
        c.close()
    out.write(net, "headers", str(height + 1), h, "P2P reply to getheaders, locator = block %d" % height)
    out.write(net, "headers2", str(height + 1), h2, "P2P reply to getheaders2, locator = block %d" % height)


def special_blocks(out, net, top, want=(6, 8, 9), scan=400):
    """The first block at or below `top` holding each special transaction type in `want`, plus
    `top` itself."""
    under_cap(net, top)
    picked = {}
    height = top
    for _ in range(scan):
        block = rpc(net, "getblock", rpc(net, "getblockhash", height), 2)
        types = {tx.get("type", 0) for tx in block["tx"]}
        for t in want:
            if t in types and t not in picked:
                picked[t] = (height, block["hash"])
        if len(picked) == len(want):
            break
        height -= 1
    blocks = {top: rpc(net, "getblockhash", top)}
    for height, hash_ in picked.values():
        blocks[height] = hash_
    for height, hash_ in sorted(blocks.items()):
        raw = bytes.fromhex(rpc(net, "getblock", hash_, 0))
        out.write(net, "block", str(height), raw, "getblock %s 0 (height %d)" % (hash_, height))
    return sorted(blocks)


def filters(out, net, start, count):
    """`cfilter` for block `start` and `cfheaders` for `start..start+count-1`, from
    `getblockfilter`."""
    under_cap(net, start + count - 1)
    hashes = [rpc(net, "getblockhash", h) for h in range(start - 1, start + count)]
    prev = rpc(net, "getblockfilter", hashes[0])
    filters_ = [rpc(net, "getblockfilter", h) for h in hashes[1:]]
    first = bytes.fromhex(filters_[0]["filter"])
    out.write(net, "cfilter", str(start),
              b"\x00" + internal(hashes[1]) + compact_size(len(first)) + first,
              "getblockfilter %s (height %d) as CFilter" % (hashes[1], start))
    filter_hashes = [sha256d(bytes.fromhex(f["filter"])) for f in filters_]
    # The chain from the previous header must reach the last block's header, as Core's
    # cfheaders reply does.
    header = internal(prev["header"])
    for fh in filter_hashes:
        header = sha256d(fh + header)
    if header != internal(filters_[-1]["header"]):
        raise SystemExit("filter header chain mismatch")
    out.write(net, "cfheaders", "%d-%d" % (start, start + count - 1),
              b"\x00" + internal(hashes[-1]) + internal(prev["header"])
              + compact_size(len(filter_hashes)) + b"".join(filter_hashes),
              "getblockfilter for heights %d..%d as CFHeaders" % (start - 1, start + count - 1))


def coinbase_clsig(out, net, height):
    """The ChainLock the coinbase of block `height` carries, as Core serializes CChainLockSig."""
    under_cap(net, height)
    block = rpc(net, "getblock", rpc(net, "getblockhash", height), 2)
    cb = block["tx"][0]["cbTx"]
    locked = height - cb["bestCLHeightDiff"] - 1
    payload = (struct.pack("<i", locked) + internal(rpc(net, "getblockhash", locked))
               + bytes.fromhex(cb["bestCLSignature"]))
    out.write(net, "clsig", "cbtx-%d" % locked, payload,
              "bestCLSignature of block %d's coinbase, for height %d" % (height, locked))


def rpc_locks(out, net, tip):
    testnet_only(net)
    cl = rpc(net, "getbestchainlock")
    out.write(net, "clsig", "rpc-%d" % cl["height"], bytes.fromhex(cl["hex"]), "getbestchainlock hex")
    found = 0
    for height in range(tip, tip - 200, -1):
        txids = rpc(net, "getblock", rpc(net, "getblockhash", height), 1)["tx"][1:]
        if not txids:
            continue
        for lock in rpc(net, "getislocks", txids[:100]):
            if isinstance(lock, dict) and lock.get("hex"):
                out.write(net, "isdlock", "rpc-%s" % lock["txid"][:16], bytes.fromhex(lock["hex"]),
                          "getislocks [%s] hex (block %d)" % (lock["txid"], height))
                found += 1
                if found == 3:
                    return


def listen(out, net, seconds):
    """`clsig` and `isdlock` messages the oracle sends a peer, until `seconds` pass or two of
    each arrived."""
    testnet_only(net)
    c = Conn(net)
    c.sock.settimeout(5)
    want = {MSG_CLSIG: "clsig", MSG_ISDLOCK: "isdlock"}
    have = {"clsig": 0, "isdlock": 0}
    deadline = time.time() + seconds
    try:
        while time.time() < deadline and min(have.values()) < 2:
            try:
                cmd, payload = c.recv()
            except socket.timeout:
                continue
            if cmd == "inv":
                items, n = [], payload[0]
                if n >= 0xFD:
                    continue  # skip an inv of 253 items or more; the oracle sends a few at a time
                for i in range(n):
                    t, = struct.unpack_from("<I", payload, 1 + 36 * i)
                    if t in want and have[want[t]] < 2:
                        items.append(payload[1 + 36 * i:1 + 36 * (i + 1)])
                if items:
                    c.send("getdata", compact_size(len(items)) + b"".join(items))
            elif cmd in have and have[cmd] < 2:
                label = "p2p-" + hashlib.sha256(payload).hexdigest()[:16]
                out.write(net, cmd, label, payload, "P2P %s after the oracle's inv" % cmd)
                have[cmd] += 1
    finally:
        c.close()
    return have


def main():
    root = sys.argv[1]
    seconds = int(sys.argv[sys.argv.index("--listen") + 1]) if "--listen" in sys.argv else 0
    out = Out(root)

    headers_replies(out, "mainnet", 1_016_000)
    headers_replies(out, "mainnet", 2_080_000)
    headers_replies(out, "testnet", 1_376_000)
    headers_replies(out, "testnet", 1_500_000)

    special_blocks(out, "mainnet", 2_089_999)
    tip = rpc("testnet", "getblockcount")
    special_blocks(out, "testnet", tip - 10)

    filters(out, "mainnet", 2_089_001, 1000)
    filters(out, "testnet", tip - 1009, 1000)

    coinbase_clsig(out, "mainnet", 2_089_000)
    coinbase_clsig(out, "testnet", tip - 10)
    rpc_locks(out, "testnet", tip - 2)
    if seconds:
        print("listened:", listen(out, "testnet", seconds), file=sys.stderr)

    with open(os.path.join(root, "manifest.json"), "w") as f:
        json.dump({"captured": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                   "files": out.manifest}, f, indent=1)


if __name__ == "__main__":
    main()
