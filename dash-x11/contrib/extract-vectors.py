#!/usr/bin/env python3
"""Writes tests/data/known-hashes.json: Dash block hashes Core asserts or checkpoints, with
the headers Core's nodes hold for them.

Usage: contrib/extract-vectors.py <dash-core-git-dir> <commit> > tests/data/known-hashes.json

- genesis: the `assert(consensus.hashGenesisBlock == uint256S(...))` hashes of mainnet, testnet
  and regtest in Core's src/chainparams.cpp at <commit>. Mainnet's and testnet's headers come
  from the reference oracles (`contrib/oracle-cli <net> getblockheader <hash> false`). Regtest's
  genesis has mainnet's coinbase, so its header is mainnet's with the nTime, nNonce and nBits of
  regtest's `CreateGenesisBlock` call at <commit>.
- checkpoints: every `{height, uint256S(hash)}` of mainnet's and testnet's `checkpointData`,
  with the header from the oracle.

The script checks each oracle header's prev-hash and height against the oracle's answers, so
nothing it writes comes from anywhere but Core. Run it on agentbox (the oracles live there).
"""

import json
import re
import struct
import subprocess
import sys
from pathlib import Path

ORACLE_CLI = Path(__file__).resolve().parents[2] / "contrib" / "oracle-cli"


def git_show(repo, commit, path):
    return subprocess.run(
        ["git", "-C", repo, "show", f"{commit}:{path}"], check=True, capture_output=True, text=True
    ).stdout


def oracle(net, *args):
    out = subprocess.run([str(ORACLE_CLI), net, *args], check=True, capture_output=True, text=True)
    return out.stdout.strip()


def sections(chainparams):
    """The source text of each network's params class, by Core's network name."""
    names = {"CMainParams": "main", "CTestNetParams": "test", "CRegTestParams": "regtest"}
    out = {}
    for cls, net in names.items():
        start = chainparams.index(f"class {cls} ")
        end = chainparams.find("\nclass ", start + 1)
        out[net] = chainparams[start : end if end != -1 else len(chainparams)]
    return out


def main():
    repo, commit = sys.argv[1], sys.argv[2]
    full = subprocess.run(
        ["git", "-C", repo, "rev-parse", f"{commit}^{{commit}}"],
        check=True, capture_output=True, text=True,
    ).stdout.strip()
    params = sections(git_show(repo, full, "src/chainparams.cpp"))
    oracle_net = {"main": "mainnet", "test": "testnet"}

    genesis = []
    for net, text in params.items():
        want = re.search(r"assert\(consensus\.hashGenesisBlock == uint256S\(\"0x([0-9a-f]{64})\"\)\)", text)
        want = want.group(1)
        if net in oracle_net:
            header = oracle(oracle_net[net], "getblockheader", want, "false")
        else:
            args = re.search(r"genesis = CreateGenesisBlock\((\d+)U?L?, (\d+)U?L?, (0x[0-9a-f]+), 1, 50 \* COIN\);", text)
            n_time, n_nonce, n_bits = int(args.group(1)), int(args.group(2)), int(args.group(3), 16)
            main = next(g for g in genesis if g["net"] == "main")["header"]
            header = main[:136] + struct.pack("<III", n_time, n_bits, n_nonce).hex()
        genesis.append({"net": net, "hash": want, "header": header})

    checkpoints = []
    for net in ("main", "test"):
        block = re.search(r"checkpointData = \{\s*\{(.*?)\n\s*\}\s*\};", params[net], re.S).group(1)
        for height, want in re.findall(r"\{\s*(\d+),\s*uint256S\(\"0x([0-9a-f]{64})\"\)\s*\}", block):
            info = json.loads(oracle(oracle_net[net], "getblockheader", want, "true"))
            if info["height"] != int(height):
                raise SystemExit(f"{net} checkpoint {height}: oracle has it at {info['height']}")
            header = oracle(oracle_net[net], "getblockheader", want, "false")
            prev = bytes.fromhex(header[8:72])[::-1].hex()
            if int(height) > 0 and prev != info["previousblockhash"]:
                raise SystemExit(f"{net} checkpoint {height}: header and prev-hash disagree")
            checkpoints.append({"net": net, "height": int(height), "hash": want, "header": header})

    json.dump(
        {"core_commit": full, "genesis": genesis, "checkpoints": checkpoints},
        sys.stdout, indent=1,
    )
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
