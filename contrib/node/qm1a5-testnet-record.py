#!/usr/bin/env python3
"""Record the testnet oracle's `quorum listextended H` and `quorum info` for every quorum it
lists (QM1a-5), for `dashd-quorums/tests/testnet_quorums.rs`.

Usage: qm1a5-testnet-record.py HEIGHT OUT_DIR

Reads the shared testnet oracle only through `contrib/oracle-cli testnet`, one call at a time,
with read-only calls: `getblockcount`, `getblockhash`, `getnetworkinfo`, `quorum listextended H`
and `quorum info TYPE HASH false`. `getblockhash H` is read again after the last call, and a
changed hash (a reorg at H) aborts the run. OUT_DIR must not exist; it is created with mode 0700
only after every call succeeds, and holds:

- `listextended-<H>.json`: `quorum listextended H`'s output, byte for byte;
- `info-<type>-<hash>.json`: `quorum info <type> <hash> false`'s output, byte for byte, one per
  hash `listextended` lists (`<type>` is the numeric `Consensus::LLMQType`);
- `provenance.json`: H, `getblockhash H`, the oracle's version and subversion, and the UTC time;
- `SHA256SUMS`: `sha256sum` lines over every other file, in name order.

Core's `quorum listextended H` (v24.0.0 src/rpc/quorums.cpp:109-190) runs
`ScanQuorums(type, block at H, signingActiveQuorumCount)` for each enabled type, so each type's
hash order is `quorum list`'s at H with the default count (:50-107).
"""
import datetime
import hashlib
import json
import os
import subprocess
import sys
from pathlib import Path

ORACLE_CLI = Path(__file__).resolve().parent.parent / "oracle-cli"

# Core: v24.0.0 src/llmq/params.h:20-45 LLMQType, keyed by each type's `name` (:147-492).
LLMQ_TYPES = {
    "llmq_50_60": 1,
    "llmq_400_60": 2,
    "llmq_400_85": 3,
    "llmq_100_67": 4,
    "llmq_60_75": 5,
    "llmq_25_67": 6,
    "llmq_test": 100,
    "llmq_devnet": 101,
    "llmq_test_v17": 102,
    "llmq_test_dip0024": 103,
    "llmq_test_instantsend": 104,
    "llmq_devnet_dip0024": 105,
    "llmq_test_platform": 106,
    "llmq_devnet_platform": 107,
}


def is_hash(s):
    return isinstance(s, str) and len(s) == 64 and all(c in "0123456789abcdef" for c in s)


def listed_quorums(listextended):
    """The (type number, quorum hash) pairs `quorum listextended`'s output lists, in its order.
    Each type's array holds one single-key object per quorum, keyed by the quorum hash."""
    out = []
    for name, entries in json.loads(listextended).items():
        if name not in LLMQ_TYPES:
            raise ValueError(f"unknown LLMQ type name {name!r}")
        if not isinstance(entries, list):
            raise ValueError(f"{name}: not an array")
        for entry in entries:
            if not isinstance(entry, dict) or len(entry) != 1:
                raise ValueError(f"{name}: an entry is not a one-key object")
            (quorum_hash,) = entry
            if not is_hash(quorum_hash):
                raise ValueError(f"{name}: {quorum_hash!r} is no quorum hash")
            out.append((LLMQ_TYPES[name], quorum_hash))
    return out


def sha256sums(files):
    """`sha256sum`'s lines over `files` ({name: bytes}), in name order."""
    return "".join(f"{hashlib.sha256(files[n]).hexdigest()}  {n}\n" for n in sorted(files))


def oracle(*args):
    return subprocess.run(
        [str(ORACLE_CLI), "testnet", *args], check=True, capture_output=True
    ).stdout


def main(argv):
    if len(argv) != 3:
        sys.exit(__doc__.split("\n\n")[1])
    height = int(argv[1])
    out = Path(argv[2])
    if height < 0:
        sys.exit("HEIGHT must not be negative")
    tip = int(oracle("getblockcount"))
    if height > tip:
        sys.exit(f"the oracle's tip is {tip}, below {height}")
    if out.exists():
        sys.exit(f"{out} exists")
    files = {}
    block_hash = oracle("getblockhash", str(height)).decode().strip()
    if not is_hash(block_hash):
        sys.exit(f"getblockhash {height}: {block_hash!r}")
    listextended = oracle("quorum", "listextended", str(height))
    files[f"listextended-{height}.json"] = listextended
    quorums = listed_quorums(listextended)
    for llmq_type, quorum_hash in quorums:
        files[f"info-{llmq_type}-{quorum_hash}.json"] = oracle(
            "quorum", "info", str(llmq_type), quorum_hash, "false"
        )
    network = json.loads(oracle("getnetworkinfo"))
    again = oracle("getblockhash", str(height)).decode().strip()
    if again != block_hash:
        sys.exit(f"block {height} changed from {block_hash} to {again} while recording")
    provenance = {
        "height": height,
        "block_hash": block_hash,
        "version": network["version"],
        "subversion": network["subversion"],
        "recorded_utc": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    }
    files["provenance.json"] = (json.dumps(provenance, indent=2) + "\n").encode()
    os.mkdir(out, 0o700)
    os.chmod(out, 0o700)
    for name, data in files.items():
        (out / name).write_bytes(data)
    (out / "SHA256SUMS").write_text(sha256sums(files))
    print(f"recorded H={height} {block_hash}: {len(quorums)} quorums, {len(files)} files in {out}")


if __name__ == "__main__":
    main(sys.argv)
