#!/usr/bin/env python3
"""Record the stale branches a reference node knows against H-M or H-T, for SB4's A-4 (README.md).

Read-only RPC (getchaintips, getblockheader, getnetworkinfo) against the shared v24 reference node
of <net> under /work/chains (docs/node-program/ORACLE.md), one call at a time. The dump (<net>.bin,
heights 0..=D) is the node's best header chain as it was; the node has moved on since, so every
tip is mapped onto the dump's heights:

  - a tip at or below D that is a dump header (compared as raw bytes) is on the dump chain;
  - a tip at or below D that is not is a stale branch: its headers are walked back
    (getblockheader <hash> false, hashPrevBlock) until one is a dump header, the fork;
  - a tip above D is walked back to height D. If that ancestor is the dump's tip, the tip
    stands for the dump's tip (truncated at D). Otherwise its branch is stale below D and is
    recorded as a stale branch ending at that ancestor.

The output, tips-<net>.json, holds the dump tip's hash and chain work, each stale branch (its
tip, fork height, chain work and headers, parent first), and provenance. Tips on the dump chain
add nothing to it and are left out, as is the node's state past the dump. Dash Core judges
everything in it; the A-4 tests derive their expected tips from it (§10.2's projection).

Usage: fetch_tips.py <mainnet|testnet> [<dir with <net>.bin>]    (default: this directory)
"""
import base64
import datetime
import hashlib
import http.client
import json
import os
import sys
import time

net = sys.argv[1]
assert net in ("mainnet", "testnet"), net
here = os.path.dirname(os.path.abspath(__file__))
src = sys.argv[2] if len(sys.argv) > 2 else here
port = {"mainnet": 27998, "testnet": 37998}[net]


class Rpc:
    def __init__(self):
        self.conn = None

    def call(self, method, *params):
        body = json.dumps({"jsonrpc": "1.0", "id": 0, "method": method, "params": list(params)})
        for _ in range(300):
            try:
                if self.conn is None:
                    cookie = open(f"/work/chains/{net}/rpc.cookie").read().strip()
                    self.auth = "Basic " + base64.b64encode(cookie.encode()).decode()
                    self.conn = http.client.HTTPConnection("127.0.0.1", port, timeout=120)
                self.conn.request("POST", "/", body, {"Authorization": self.auth})
                res = json.loads(self.conn.getresponse().read())
                break
            except (OSError, http.client.HTTPException, ValueError):
                self.conn = None  # the node restarted: reconnect with a fresh cookie
                time.sleep(2)
        else:
            raise RuntimeError("oracle unreachable")
        if res["error"]:
            raise RuntimeError(f"{method}: {res['error']}")
        return res["result"]


rpc = Rpc()
dump_path = os.path.join(src, f"{net}.bin")
dump = open(dump_path, "rb").read()
assert len(dump) % 80 == 0
D = len(dump) // 80 - 1


def dump_raw(h):
    return dump[80 * h : 80 * h + 80]


def raw_of(hash_):
    return bytes.fromhex(rpc.call("getblockheader", hash_, False))


def on_dump(hash_, height):
    return height <= D and raw_of(hash_) == dump_raw(height)


def walk_down(hash_, height, to):
    """(hash, raw headers above `to`, parent first) of `hash_`'s ancestor at height `to`."""
    raws = []
    while height > to:
        raw = raw_of(hash_)
        raws.append(raw)
        hash_ = raw[4:36][::-1].hex()
        height -= 1
    return hash_, raws[::-1]


def stale_branch(hash_, height):
    """The fork height on the dump chain and the stale headers above it, parent first."""
    raws = []
    while not on_dump(hash_, height):
        raw = raw_of(hash_)
        raws.append(raw)
        hash_ = raw[4:36][::-1].hex()
        height -= 1
    return height, raws[::-1]


tips = rpc.call("getchaintips")
dump_tip_hash = None
stale = {}
for t in tips:
    h, hash_ = t["height"], t["hash"]
    if h <= D:
        anchor_hash, anchor_height = hash_, h
    else:
        anchor_hash, _ = walk_down(hash_, h, D)
        anchor_height = D
    if on_dump(anchor_hash, anchor_height):
        if anchor_height == D:
            dump_tip_hash = anchor_hash
    elif anchor_hash not in stale:
        fork, raws = stale_branch(anchor_hash, anchor_height)
        stale[anchor_hash] = {
            "hash": anchor_hash,
            "height": anchor_height,
            "fork_height": fork,
            "chainwork": rpc.call("getblockheader", anchor_hash)["chainwork"],
            "headers": [r.hex() for r in raws],
        }

if dump_tip_hash is None:
    raise SystemExit("no getchaintips tip descends from the dump's tip")
tip_info = rpc.call("getblockheader", dump_tip_hash)
assert tip_info["height"] == D

out = {
    "provenance": {
        "oracle": f"dashd {rpc.call('getnetworkinfo')['subversion']} at /work/chains/{net}",
        "rpc": "getchaintips, getblockheader, getnetworkinfo (read-only)",
        "script": "dash-chain/tests/data/headers/fetch_tips.py",
        "dump": f"{net}.bin sha256 {hashlib.sha256(dump).hexdigest()}",
        "recorded": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    },
    "network": net,
    "dump_height": D,
    "dump_tip": {"hash": dump_tip_hash, "chainwork": tip_info["chainwork"]},
    "stale": sorted(stale.values(), key=lambda b: (b["height"], b["hash"])),
}
path = os.path.join(here, f"tips-{net}.json")
with open(path, "w") as f:
    json.dump(out, f, indent=1)
    f.write("\n")
print(f"{path}: {len(stale)} stale branches at or below dump height {D}")
