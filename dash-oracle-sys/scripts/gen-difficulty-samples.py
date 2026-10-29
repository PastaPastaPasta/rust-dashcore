#!/usr/bin/env python3
"""Capture dashd's `difficulty` for the headers in the mainnet/testnet smoke samples.

getblockheader reports GetDifficulty(), rpc/blockchain.cpp's own copy of the
ConvertBitsToDouble() formula, printed by UniValue with 16 significant digits. For
each distinct nBits in the samples' header ranges the first header carrying it is
looked up on the BS2 reference node (read-only RPC).

Usage: gen-difficulty-samples.py --out dash-oracle-sys/tests/data/difficulty.json
"""

import argparse
import base64
import json
import os
import urllib.request

PORTS = {"mainnet": 27998, "testnet": 37998}
HERE = os.path.dirname(os.path.abspath(__file__))


def rpc(net, method, *params):
    with open(f"/work/chains/{net}/rpc.cookie") as f:
        auth = base64.b64encode(f.read().strip().encode()).decode()
    body = json.dumps({"jsonrpc": "1.0", "id": 0, "method": method, "params": list(params)}).encode()
    req = urllib.request.Request(f"http://127.0.0.1:{PORTS[net]}/", body,
                                 {"Authorization": "Basic " + auth, "Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=600) as resp:
        # keep the number exactly as dashd printed it
        reply = json.load(resp, parse_float=str)
    if reply["error"] is not None:
        raise RuntimeError(reply["error"])
    return reply["result"]


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--out", required=True)
    args = p.parse_args()
    out = {"provenance": {}, "samples": []}
    for net in ("mainnet", "testnet"):
        out["provenance"][net] = rpc(net, "getnetworkinfo")["subversion"]
        samples = json.load(open(os.path.join(HERE, "..", "tests", "data", f"{net}.json")))
        seen = set()
        for r in samples["header_ranges"]:
            raw = bytes.fromhex(r["hex"])
            for i in range(len(raw) // 80):
                bits = int.from_bytes(raw[80 * i + 72:80 * i + 76], "little")
                if bits in seen:
                    continue
                seen.add(bits)
                h = rpc(net, "getblockheader", rpc(net, "getblockhash", r["first_height"] + i))
                assert int(h["bits"], 16) == bits
                # as printed; integral values come back as JSON integers
                out["samples"].append({"network": net, "height": h["height"], "bits": bits, "difficulty": str(h["difficulty"])})
    with open(args.out, "w") as f:  # one sample per line
        f.write(json.dumps({"provenance": out["provenance"]})[:-1] + ',\n"samples": [\n')
        f.write(",\n".join(json.dumps(s) for s in out["samples"]) + "\n]}\n")
    print(f"{len(out['samples'])} samples")


if __name__ == "__main__":
    main()
