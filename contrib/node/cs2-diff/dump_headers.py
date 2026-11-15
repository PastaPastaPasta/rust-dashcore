#!/usr/bin/env python3
"""Dump every header of a BS2 reference node's best header chain to a file of 80-byte records,
the input of `cs2-diff sweep` and `cs2-diff sample` (README.md).

Read-only RPC; the reference nodes under /work/chains are shared and may be in IBD, and getblockhash,
getblockheaders and P2P getheaders only serve the active chain. So:
  - heights 0..=blocks come from getblockheaders along the active chain;
  - heights above are walked back with getblockheader <hash> false, in parallel segments that
    start at Core's checkpoints above the block tip (and the best header) and must end exactly on
    the next anchor below (checked by hash), the lowest one on the active tip.
Each segment is cached in <out>.seg/, so a rerun after an oracle restart resumes. Every link is
checked: within a getblockheaders chunk against Core's own hashes (its verbose answer), across
chunks with getblockhash, and along a segment by walking hashPrevBlock.

Usage: dump_headers.py <mainnet|testnet> <out.bin> <chainparams.json> [--until H] [--sha256 HEX]
(<chainparams.json> is `cs2-diff params v24.0 main` or `... test`, for Core's checkpoints.)
  --until H     dump heights 0..=H instead of up to the best header, so a recorded dump can be
                rebuilt once the node has moved on (PROVENANCE.md: --until 2551766 for mainnet,
                --until 1568459 for testnet). H must be on the node's current best header chain.
  --sha256 HEX  fail unless the dump's sha256 is HEX (the file is then left as <out>.mismatch).
"""
import argparse
import base64
import hashlib
import http.client
import json
import multiprocessing
import os
import sys
import time

parser = argparse.ArgumentParser()
parser.add_argument("net", choices=["mainnet", "testnet"])
parser.add_argument("out")
parser.add_argument("cp_path")
parser.add_argument("--until", type=int)
parser.add_argument("--sha256")
args = parser.parse_args()
net, out, cp_path = args.net, args.out, args.cp_path
port = {"mainnet": 27998, "testnet": 37998}[net]


class Rpc:
    def __init__(self):
        self.conn = None

    def batch(self, calls):
        body = json.dumps([{"jsonrpc": "1.0", "id": i, "method": m, "params": p} for i, (m, p) in enumerate(calls)])
        for attempt in range(600):
            try:
                if self.conn is None:
                    cookie = open(f"/work/chains/{net}/rpc.cookie").read().strip()
                    self.auth = "Basic " + base64.b64encode(cookie.encode()).decode()
                    self.conn = http.client.HTTPConnection("127.0.0.1", port, timeout=600)
                self.conn.request("POST", "/", body, {"Authorization": self.auth, "Content-Type": "text/plain"})
                res = json.loads(self.conn.getresponse().read())
                break
            except Exception as e:  # node restarting: reconnect with a fresh cookie
                self.conn = None
                time.sleep(2)
        else:
            raise RuntimeError("oracle unreachable")
        res.sort(key=lambda r: r["id"])
        for r in res:
            if r["error"]:
                raise RuntimeError(r["error"])
        return [r["result"] for r in res]

    def call(self, m, *p):
        return self.batch([(m, list(p))])[0]


def check_fields(raw, info, height):
    """`raw` is the header Core's verbose getblockheader(s) describes at `height`."""
    fields = (
        int.from_bytes(raw[0:4], "little", signed=True) == info["version"],
        raw[4:36][::-1].hex() == info.get("previousblockhash", "00" * 32),
        raw[36:68][::-1].hex() == info["merkleroot"],
        int.from_bytes(raw[68:72], "little") == info["time"],
        f"{int.from_bytes(raw[72:76], 'little'):08x}" == info["bits"],
        int.from_bytes(raw[76:80], "little") == info["nonce"],
        info["height"] == height,
    )
    if not all(fields):
        sys.exit(f"the raw header at {height} is not Core's: {fields}")


def walk(seg):
    """Walk back from (top_height, top_hash) to just above (low_height, low_hash)."""
    top_h, top_hash, low_h, low_hash, path = seg
    if os.path.exists(path):
        return path
    rpc = Rpc()
    raws = []
    cur = top_hash
    for _ in range(top_h - low_h):
        raw = bytes.fromhex(rpc.call("getblockheader", cur, False))
        raws.append(raw)
        cur = raw[4:36][::-1].hex()
    assert cur == low_hash, f"segment {top_h}->{low_h} does not land on the anchor"
    raws.reverse()
    with open(path + ".tmp", "wb") as f:
        f.write(b"".join(raws))
    os.rename(path + ".tmp", path)
    print(f"  segment {low_h + 1}..={top_h} done", flush=True)
    return path


def main():
    rpc = Rpc()
    info = rpc.call("getblockchaininfo")
    blocks, headers = info["blocks"], info["headers"]
    # the best header chain's tip: the active tip, or a headers-only tip above it during IBD
    best = [t for t in rpc.call("getchaintips")
            if t["height"] == headers and t["status"] in ("active", "headers-only")]
    assert len(best) == 1, f"not one best tip at the header height: {best}"
    print(f"{net}: blocks {blocks}, headers {headers}, best header {best[0]['hash']}", flush=True)
    until = headers if args.until is None else args.until
    assert 0 <= until <= headers, f"--until {until} is above the best header {headers}"
    blocks = min(blocks, until)

    segs = []
    if blocks < until:
        checkpoints = json.load(open(cp_path))["checkpoints"]
        anchors = sorted((int(h), x) for h, x in checkpoints.items() if blocks < int(h) < headers)
        anchors = [(blocks, rpc.call("getblockhash", blocks))] + anchors + [(headers, best[0]["hash"])]
        segdir = out + ".seg"
        os.makedirs(segdir, exist_ok=True)
        segs = [(hi, hh, lo, lh, f"{segdir}/{lo + 1}-{hi}.bin") for (lo, lh), (hi, hh) in zip(anchors, anchors[1:])]
        # longest first, so the pool stays busy
        with multiprocessing.Pool(8) as pool:
            pool.map(walk, sorted(segs, key=lambda s: s[2] - s[0]), chunksize=1)

    with open(out + ".tmp", "wb") as f:
        chunk = 2000
        for base in range(0, blocks + 1, chunk * 10):
            starts = list(range(base, min(base + chunk * 10, blocks + 1), chunk))
            hashes = rpc.batch([("getblockhash", [s]) for s in starts])
            prevs = rpc.batch([("getblockhash", [s - 1]) for s in starts if s > 0])
            if starts[0] == 0:
                prevs = [None] + prevs
            counts = [min(chunk, blocks + 1 - s) for s in starts]
            chunks = rpc.batch([("getblockheaders", [h, c, False]) for h, c in zip(hashes, counts)])
            verbose = rpc.batch([("getblockheaders", [h, c, True]) for h, c in zip(hashes, counts)])
            for s, prev, hdrs, info in zip(starts, prevs, chunks, verbose):
                assert len(hdrs) == len(info) == min(chunk, blocks + 1 - s), (s, len(hdrs))
                raw = [bytes.fromhex(x) for x in hdrs]
                assert all(len(r) == 80 for r in raw)
                # every link: each raw header is Core's verbose header at its height (so its
                # hash is Core's), and links to the previous chunk or to the header before it
                hashes_before = [prev] + [i["hash"] for i in info[:-1]]
                for i, (r, before) in enumerate(zip(raw, hashes_before)):
                    check_fields(r, info[i], s + i)
                    if before is not None:
                        assert r[4:36][::-1].hex() == before, f"linkage broken at {s + i}"
                f.write(b"".join(raw))
        for s in sorted(segs, key=lambda s: s[2]):
            seg = open(s[4], "rb").read()
            if len(seg) != (s[0] - s[2]) * 80:
                sys.exit(f"{s[4]}: {len(seg)} bytes, not {(s[0] - s[2]) * 80}; delete it and rerun")
            f.write(seg)
        # segments end at the best header; --until may stop below it
        if f.tell() < (until + 1) * 80:
            sys.exit(f"only {f.tell() // 80} headers for heights 0..={until}")
        f.truncate((until + 1) * 80)
    digest = hashlib.sha256(open(out + ".tmp", "rb").read()).hexdigest()
    if args.sha256 is not None and digest != args.sha256.lower():
        os.rename(out + ".tmp", out + ".mismatch")
        sys.exit(f"sha256 {digest}, not {args.sha256}: left as {out}.mismatch")
    os.rename(out + ".tmp", out)
    print(f"done: heights 0..={until} (blocks {blocks}), sha256 {digest}", flush=True)


if __name__ == "__main__":
    main()
