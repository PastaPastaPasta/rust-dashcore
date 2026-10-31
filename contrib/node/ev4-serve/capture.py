#!/usr/bin/env python3
"""Builds one EV4-serve fixture from a Core functional-test regtest datadir (EV4-serve notes D9).

Usage: capture.py NAME P2P_PORT CLI... > NAME.json

Inputs, all produced by Dash Core v24.0.0-rc.3:
- NAME.evodb.txt: every evodb record (`key_hex value_hex`), dumped with LevelDB's C API from a
  pristine copy of the datadir (evodump);
- the same chain's dashd (a second copy), read over RPC (getblockhash, getblock 0/1) through the
  CLI given after the port;
- that dashd's `mnlistdiff` replies to P2P `getmnlistd` for the chosen (base, block) pairs in each
  serialization band (getmnlistd.py).

Output JSON: the chain's block hashes by height, every block's coinbase transaction, the target
blocks' txids, the `dmn_*` and `q_mc` evodb records, and the replies.
"""

import json
import random
import subprocess
import sys

BANDS = [70221, 70225, 70227, 70228, 70229, 70230, 70242]
TOOLS = "/work/scratch/ev4s/tools"
# Pairs a chain adds to the common set (Opus-h review, DEC-331): rpc_netinfo's 204 -> 260 deletes
# two masternodes whose CHAMP order is not their proTxHash order.
EXTRA_PAIRS = {"rpc_netinfo": [(204, 260)]}


def cli(args, *rpc):
    return subprocess.run(args + list(rpc), check=True, capture_output=True, text=True).stdout.strip()


def main():
    name, port, cli_args = sys.argv[1], sys.argv[2], sys.argv[3:]
    tip = int(cli(cli_args, "getblockcount"))
    hashes = [cli(cli_args, "getblockhash", str(h)) for h in range(tip + 1)]
    coinbases = []
    for h in hashes:
        raw = bytes.fromhex(cli(cli_args, "getblock", h, "0"))
        coinbases.append(coinbase_hex(raw))

    rng = random.Random(f"ev4s-{name}")
    pairs = {(0, tip), (0, tip // 2), (2, tip), (tip // 4, tip), (tip // 2, tip), (tip - 1, tip), (tip, tip)}
    for _ in range(10):
        h = rng.randrange(3, tip + 1)
        pairs.add((h - 1, h))
    for _ in range(4):
        a = rng.randrange(0, tip)
        pairs.add((a, rng.randrange(a, tip + 1)))
    # Block 1 is below DIP3 on every chain: Core's default list, whose hash is null
    # (deterministicmns.cpp v24:828-832, smldiff.cpp v24:125).
    pairs.add((0, 1))
    pairs.update(EXTRA_PAIRS.get(name, []))
    requests = []
    for base, block in sorted(pairs):
        for proto in BANDS:
            requests.append({"base": hashes[base], "block": hashes[block], "proto": proto})
    # A null base asks from genesis and is echoed as null (smldiff.cpp v24:162-169, 211-214).
    for target in (tip, 1):
        for proto in BANDS:
            requests.append({"base": "00" * 32, "block": hashes[target], "proto": proto})
    req_path = f"/work/scratch/ev4s/{name}.req.jsonl"
    with open(req_path, "w") as f:
        for r in requests:
            f.write(json.dumps(r) + "\n")
    out = subprocess.run(
        ["python3", "-I", f"{TOOLS}/getmnlistd.py", f"127.0.0.1:{port}", "fcc1b7dc", req_path],
        check=True, capture_output=True, text=True, timeout=600,
    ).stdout
    replies = [json.loads(line) for line in out.splitlines() if line.strip()]

    targets = sorted({r["block"] for r in replies})
    txids = {t: json.loads(cli(cli_args, "getblock", t, "1"))["tx"] for t in targets}

    records = []
    for line in open(f"/work/scratch/ev4s/{name}-node0.evodb.txt"):
        key, value = line.split()
        prefix = bytes.fromhex(key[2 : 2 + 2 * int(key[:2], 16)])
        if prefix.startswith(b"dmn_") or prefix == b"q_mc":
            records.append([key, value])

    json.dump(
        {
            "provenance": (
                f"Dash Core v24.0.0-rc.3 functional test {name} node0 (BS2, /work/conformance/regtest-v24): "
                "evodb records via LevelDB's C API on a pristine copy; blocks via getblock; replies to P2P "
                "getmnlistd per protocol version from rc.3 dashd on that datadir "
                "(contrib/node/ev4-serve, EV4-serve notes D9)"
            ),
            "hashes": hashes,
            "coinbases": coinbases,
            "txids": txids,
            "evodb": records,
            "replies": [[r["base"], r["block"], r["proto"], r["payload"]] for r in replies],
        },
        sys.stdout,
        separators=(",", ":"),
    )


def read_compact(b, i):
    n = b[i]
    if n < 0xFD:
        return n, i + 1
    width = {0xFD: 2, 0xFE: 4, 0xFF: 8}[n]
    return int.from_bytes(b[i + 1 : i + 1 + width], "little"), i + 1 + width


def coinbase_hex(block):
    """The first transaction of a serialized block (80-byte header, tx count, coinbase)."""
    _, i = read_compact(block, 80)
    start = i
    version = int.from_bytes(block[i : i + 4], "little")
    i += 4
    n_in, i = read_compact(block, i)
    for _ in range(n_in):
        i += 36
        n, i = read_compact(block, i)
        i += n + 4
    n_out, i = read_compact(block, i)
    for _ in range(n_out):
        i += 8
        n, i = read_compact(block, i)
        i += n
    i += 4
    if (version >> 16) != 0 and (version & 0xFFFF) >= 3:
        n, i = read_compact(block, i)
        i += n
    return block[start:i].hex()


if __name__ == "__main__":
    main()
