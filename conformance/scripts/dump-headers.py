#!/usr/bin/env python3
"""Dump a scratch dashd's best header chain, heights 0..=headers, to a file of 80-byte headers
(CH3a a7-3, D162: the preload of `digest l2 --headers`).

- Heights 0..=blocks come along the active chain: getblockheaders <getblockhash(s)> 2000 false.
  Each chunk's first prev_hash must be getblockhash(s - 1).
- Heights above are walked back from the best header with getblockheader <hash> false
  (getblockheaders serves the active chain only), down to blocks + 1, whose prev_hash must be
  the active tip's hash.
- The best header is the one chain tip at getblockchaininfo's `headers` height.
Within-chunk links are checked again when `digest l2` opens the file (X11, from genesis).

Run it against a dashd on a reflink copy of the datadir, never the datadir itself, with the network
off and RPC on a free local port, e.g. `dashd -datadir=<copy> -connect=0 -listen=0 -dnsseed=0
-fixedseeds=0 -networkactive=0 -server=1 -rpcbind=127.0.0.1 -rpcallowip=127.0.0.1 -rpcport=<port>
-disablewallet`; then stop dashd and delete the copy.

Usage: python3 -I conformance/scripts/dump-headers.py <cookie> <port> <out.bin>; writes
<out.bin>.json beside it.
"""
import base64
import hashlib
import http.client
import json
import sys
import time

cookie_path, port, out = sys.argv[1], int(sys.argv[2]), sys.argv[3]


class Rpc:
    def __init__(self):
        self.conn = None

    def call(self, method, *params):
        body = json.dumps({"jsonrpc": "1.0", "id": 0, "method": method, "params": list(params)})
        if self.conn is None:
            cookie = open(cookie_path).read().strip()
            self.auth = "Basic " + base64.b64encode(cookie.encode()).decode()
            self.conn = http.client.HTTPConnection("127.0.0.1", port, timeout=600)
        self.conn.request("POST", "/", body, {"Authorization": self.auth, "Content-Type": "text/plain"})
        res = json.loads(self.conn.getresponse().read())
        if res.get("error"):
            raise RuntimeError(f"{method} {params}: {res['error']}")
        return res["result"]


def prev_of(raw):
    return raw[4:36][::-1].hex()


rpc = Rpc()
started = time.time()
# Wait out init (the cookie appears, then RPC answers -28 until warmup ends).
for _ in range(3600):
    try:
        info = rpc.call("getblockchaininfo")
        break
    except (OSError, RuntimeError, http.client.HTTPException) as e:
        rpc.conn = None
        time.sleep(1)
else:
    sys.exit("dashd's RPC never answered")
blocks, headers, active = info["blocks"], info["headers"], info["bestblockhash"]
tips = [t for t in rpc.call("getchaintips") if t["height"] == headers]
if len(tips) != 1:
    sys.exit(f"{len(tips)} chain tips at the best header height {headers}: {tips}")
best = tips[0]
print(f"active tip {blocks} {active}; best header {headers} {best['hash']} ({best['status']})", flush=True)

chain = []
for s in range(0, blocks + 1, 2000):
    want = min(2000, blocks + 1 - s)
    hexes = rpc.call("getblockheaders", rpc.call("getblockhash", s), want, False)
    if len(hexes) != want:
        sys.exit(f"getblockheaders at {s}: {len(hexes)} headers, not {want}")
    raws = [bytes.fromhex(h) for h in hexes]
    if any(len(r) != 80 for r in raws):
        sys.exit(f"getblockheaders at {s}: a header that is not 80 bytes")
    if s > 0 and prev_of(raws[0]) != rpc.call("getblockhash", s - 1):
        sys.exit(f"height {s}'s prev_hash is not getblockhash({s - 1})")
    chain.extend(raws)
    if s % 200000 == 0:
        print(f"active {s} at {time.time() - started:.0f} s", flush=True)

walked = []
h = best["hash"]
for i in range(headers - blocks):
    raw = bytes.fromhex(rpc.call("getblockheader", h, False))
    if len(raw) != 80:
        sys.exit(f"getblockheader {h}: not 80 bytes")
    walked.append(raw)
    h = prev_of(raw)
    if i % 200000 == 0:
        print(f"walked {i} at {time.time() - started:.0f} s", flush=True)
if h != active:
    sys.exit(f"the walk from the best header ends on {h}, not the active tip {active}")
chain.extend(reversed(walked))
assert len(chain) == headers + 1

data = b"".join(chain)
with open(out, "xb") as f:
    f.write(data)
summary = {
    "file": out,
    "headers": len(chain),
    "tip": {"h": headers, "b": best["hash"], "status": best["status"]},
    "active_tip": {"h": blocks, "b": active},
    "bytes": len(data),
    "sha256": hashlib.sha256(data).hexdigest(),
    "wall_s": round(time.time() - started, 1),
    "dashd": rpc.call("getnetworkinfo")["subversion"],
}
with open(out + ".json", "x") as f:
    json.dump(summary, f, indent=1)
print(json.dumps(summary), flush=True)
