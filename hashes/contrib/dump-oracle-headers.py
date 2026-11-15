#!/usr/bin/env python3
"""Dump a header chain from a reference dashd oracle, for hashes/tests/x11_chain.rs.

    python3 -I hashes/contrib/dump-oracle-headers.py <mainnet|testnet> <outdir> [--end HEIGHT]

Read-only RPCs (`getblockcount`, `getblockhash`, `getblockheaders`) against the oracle of
docs/node-program/ORACLE.md; it never touches the datadir. Writes to <outdir>:

    headers.bin  80 bytes per height, from 0, as the node serializes them
    hashes.bin   32 bytes per height, `getblockhash` in the node's byte order (display reversed)
    tip.txt      "<height> <hash>" of the last entry, re-read from the oracle at the end

The oracle's hashes are Dash Core's X11 hashes of those headers, so a test that hashes
headers.bin must reproduce hashes.bin. The walk stops early (exit 1) if the oracle's chain moved
under it.
"""
import argparse
import base64
import json
import os
import sys
import urllib.request

NETS = {
    "mainnet": ("http://127.0.0.1:27998", "/work/chains/mainnet/rpc.cookie"),
    "testnet": ("http://127.0.0.1:37998", "/work/chains/testnet/rpc.cookie"),
}
CHUNK = 2000  # getblockheaders' maximum


def rpc(net, calls):
    """One JSON-RPC batch; returns the results in call order."""
    url, cookie = NETS[net]
    with open(cookie) as f:  # the cookie changes on every oracle restart
        auth = base64.b64encode(f.read().strip().encode()).decode()
    body = json.dumps(
        [{"jsonrpc": "1.0", "id": i, "method": m, "params": p} for i, (m, p) in enumerate(calls)]
    ).encode()
    req = urllib.request.Request(
        url, body, {"Authorization": "Basic " + auth, "Content-Type": "application/json"}
    )
    with urllib.request.urlopen(req, timeout=600) as resp:
        replies = sorted(json.load(resp), key=lambda r: r["id"])
    for r in replies:
        if r.get("error"):
            sys.exit(f"rpc error: {r['error']}")
    return [r["result"] for r in replies]


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("net", choices=NETS)
    ap.add_argument("outdir")
    ap.add_argument("--end", type=int, help="last height (default: the oracle's block count)")
    args = ap.parse_args()

    (count,) = rpc(args.net, [("getblockcount", [])])
    end = min(args.end, count) if args.end is not None else count
    os.makedirs(args.outdir, exist_ok=True)
    last = None
    with open(os.path.join(args.outdir, "headers.bin"), "wb") as hdrs, open(
        os.path.join(args.outdir, "hashes.bin"), "wb"
    ) as hashes:
        for start in range(0, end + 1, CHUNK):
            n = min(CHUNK, end + 1 - start)
            names = rpc(args.net, [("getblockhash", [h]) for h in range(start, start + n)])
            (headers,) = rpc(args.net, [("getblockheaders", [names[0], n, False])])
            if len(headers) != n:
                sys.exit(f"{start}: asked for {n} headers, got {len(headers)}")
            for name, header in zip(names, headers):
                raw = bytes.fromhex(header)
                if len(raw) != 80:
                    sys.exit(f"{start}: a header of {len(raw)} bytes")
                hdrs.write(raw)
                hashes.write(bytes.fromhex(name)[::-1])
            last = names[-1]
            if start % (100 * CHUNK) == 0:
                print(f"{start + n - 1}/{end}", flush=True)
    (again,) = rpc(args.net, [("getblockhash", [end])])
    if again != last:
        sys.exit(f"the oracle's chain moved: height {end} was {last}, is {again}")
    with open(os.path.join(args.outdir, "tip.txt"), "w") as f:
        f.write(f"{end} {last}\n")
    print(f"done: heights 0..={end}, tip {last}")


if __name__ == "__main__":
    main()
