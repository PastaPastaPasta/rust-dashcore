#!/usr/bin/env python3
"""Capture the mainnet and testnet LLMQ-context samples (tests/data/llmq-samples.json.gz;
node-program O2a-LQ1): non-rotated quorums whose members a context computes from the list
and coinbase a node would read, and whose mined commitment then has to verify with those
members through Core (oracle_verify_commitment with signatures).

Reads the shared reference dashds over RPC with pure reads only, one request at a time:
getblockhash, getblockheader, getblock (verbosity 2) and `protx listdiff` (A1/ORACLE.md;
`quorum *`, `protx diff` and rotationinfo are not pure on these nodes and are not used).
Mainnet heights stay at or below MAX_MAINNET_HEIGHT.

A sample is a group of quorums whose members come from one list: the list after their
base block before V20, or after the work block (base - 8) once V20 is active after the base
block (dml-listdiff-check.py's member_list_height). For each it records:
- the headers from the list block to the last commitment's mined block;
- that list (`protx listdiff 1 <h>`, re-encoded the way evodb stores it by
  dml-listdiff-check.py's encode_list) and the block's coinbase transaction (the CbTx
  chainlock of the V20 modifier);
- each quorum's non-null commitment (getblock 2 over its base's following blocks) and the
  height that mined it.
Nothing the RPC prints about members is copied: the test computes them with Core.

Usage: gen-llmq-samples.py --out tests/data/llmq-samples.json.gz
"""

import argparse
import gzip
import importlib.util
import json
from pathlib import Path

spec = importlib.util.spec_from_file_location("llmqregtest", Path(__file__).with_name("gen-llmq-regtest.py"))
regtest = importlib.util.module_from_spec(spec)
spec.loader.exec_module(regtest)
dml = regtest.dml

MAX_MAINNET_HEIGHT = 2_090_000
# How far after its base block a commitment is looked for (above every mining window end).
MINING_SEARCH = 60

TESTNET = {
    # From Core's chainparams (oracle_chainparams_json at the pin).
    "chain": "test",
    "rpc_port": 37998,
    "cookie": "/work/chains/testnet/rpc.cookie",
    "dip3_height": 7000,
    "v19_height": 850100,
    "v20_height": 905100,
    "min_confirmations": 1,
    "pubkey_prefix": 140,
    "script_prefix": 19,
}

# (network, LLMQ types, base height): one sample per entry.
MAINNET_SAMPLES = [
    # DIP0024 activation: before V19 (legacy BLS, all masternodes for the platform type) and
    # V20 (the base block's list, the block-hash modifier).
    ("mainnet", (1, 2, 4), 1_737_792),
    # V19 activation: the basic scheme and the evonode-only platform quorum.
    ("mainnet", (1, 2, 4), 1_899_072),
    # V20 activation: the work block's list, still the block-hash modifier (the work block is
    # below V20).
    ("mainnet", (1, 2, 3, 4), 1_987_776),
    # The first cycle whose work block is past V20: the CbTx chainlock modifier.
    ("mainnet", (1, 4), 1_987_800),
]
# A testnet window of at least 200 blocks after V20: every non-null commitment of these types.
TESTNET_WINDOW = ((1, 2, 3, 6), 1_100_016, 1_100_016 + 240)
TESTNET_INTERVALS = {1: 24, 2: 288, 3: 576, 6: 24}


def base_heights(types, intervals, first, end):
    return sorted({(t, h) for t in types for h in range(first, end) if h % intervals[t] == 0})


def find_commitment(chain, llmq_type, base_height):
    """The non-null commitment of the quorum at base_height and the height that mined it."""
    base_hash = chain.block_hash(base_height)
    for h in range(base_height + 1, base_height + MINING_SEARCH):
        block = chain.rpc.call("getblock", chain.block_hash(h), 2)
        for qc, commitment in regtest.mined_commitments(block):
            if qc["llmqType"] == llmq_type and qc["quorumHash"] == base_hash:
                return {"commitment": commitment.hex(), "mined_height": h}
    return None


def sample(chain, network, quorums):
    """One sample: quorums [(type, base height)] whose members read the same list."""
    list_heights = {dml.member_list_height(chain, base) for _, base in quorums}
    assert len(list_heights) == 1, f"quorums of different lists: {quorums}"
    list_height = list_heights.pop()
    found = []
    for llmq_type, base in quorums:
        qc = find_commitment(chain, llmq_type, base)
        if qc is None:
            print(f"{network}: no non-null commitment of type {llmq_type} at {base}", flush=True)
            continue
        found.append({"type": llmq_type, "base_height": base, **qc})
    assert found, f"no commitment in {quorums}"
    last = max(q["mined_height"] for q in found)
    if network == "mainnet":
        assert last <= MAX_MAINNET_HEIGHT
    block = chain.rpc.call("getblock", chain.block_hash(list_height), 2)
    return {
        "network": network,
        "first_height": list_height,
        "headers": chain.headers_hex(list_height, last + 1),
        "list_height": list_height,
        "list": dml.encode_list(chain.list_json(list_height), list_height, chain).hex(),
        "coinbase": block["tx"][0]["hex"],
        "quorums": found,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True, type=Path)
    args = ap.parse_args()
    samples = []
    mainnet = dml.NETWORKS["mainnet"]
    chain = dml.Chain(dml.Rpc(mainnet["rpc_port"], mainnet["cookie"]), mainnet)
    for network, types, base in MAINNET_SAMPLES:
        samples.append(sample(chain, network, [(t, base) for t in types]))
        print(f"{network} {base}: {len(samples[-1]['quorums'])} quorums", flush=True)

    chain = dml.Chain(dml.Rpc(TESTNET["rpc_port"], TESTNET["cookie"]), TESTNET)
    types, first, end = TESTNET_WINDOW
    by_list = {}
    for llmq_type, base in base_heights(types, TESTNET_INTERVALS, first, end):
        by_list.setdefault(dml.member_list_height(chain, base), []).append((llmq_type, base))
    for quorums in by_list.values():
        samples.append(sample(chain, "testnet", quorums))
        print(f"testnet {quorums[0][1]}: {len(samples[-1]['quorums'])} quorums", flush=True)

    with gzip.GzipFile(args.out, "wb", mtime=0) as f:
        f.write((json.dumps({"samples": samples}, indent=1, sort_keys=True) + "\n").encode())


if __name__ == "__main__":
    main()
