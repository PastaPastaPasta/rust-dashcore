#!/usr/bin/env python3
"""Core's own getblocktxn reader: its verdict, and the bytes read on success.

Usage: bs7_getblocktxn.py DASH_REPO OUT_DIR > core_getblocktxn.json5
Extracts src/ of v24.0.0-rc.3, the only parity reference (DEC-82), with
`git archive` into OUT_DIR, compiles bs7-getblocktxn/getblocktxn.cpp against
it (Core's blockencodings.h, streams.h and serialize.h, nothing
re-implemented), and runs a fixed case set: edge cases of the index vector,
every prefix of each, and seeded random suffixes. Prints base-sdk's
`core_getblocktxn.json5`. Where a failed read stops and its exception are
not kept: no Core logic observes them (DEC-98).
"""
import json
import os
import random
import subprocess
import sys

TAG = "v24.0.0-rc.3"
HERE = os.path.dirname(os.path.abspath(__file__))
HASH = "00" * 32
SUFFIXES = [
    "00", "0300", "030000", "03000000", "0100", "01fdff00", "01fdfc00", "01fdffff", "02fdffff00",
    "0200fdfeff", "0200fdffff", "01fe00000100", "01ffffffffffffffffff", "fe00000002", "fe01000002",
    "fd0100", "fdfd00", "02fe000000000000", "0400010203", "05000000",
]


def build(repo, out_dir):
    src = os.path.join(out_dir, TAG)
    if not os.path.isdir(os.path.join(src, "src")):
        os.makedirs(src, exist_ok=True)
        archive = subprocess.run(["git", "-C", repo, "archive", TAG, "src"], capture_output=True, check=True).stdout
        subprocess.run(["tar", "-x", "-C", src], input=archive, check=True)
    s = os.path.join(src, "src")
    exe = os.path.join(out_dir, "gbt-" + TAG)
    subprocess.run(["g++", "-std=c++20", "-O1", "-I" + s, "-I" + s + "/univalue/include",
                    os.path.join(HERE, "bs7-getblocktxn", "getblocktxn.cpp"), s + "/util/strencodings.cpp",
                    s + "/uint256.cpp", s + "/util/string.cpp", s + "/support/cleanse.cpp", "-o", exe], check=True)
    return exe


def cases():
    rng = random.Random(7)
    out = []
    for suffix in SUFFIXES:
        full = HASH + suffix
        out += [full[:n] for n in range(0, len(full) + 1, 2)]
    alphabet = ["00", "01", "02", "03", "7f", "80", "fc", "fd", "fe", "ff"]
    for _ in range(400):
        out.append(HASH + "".join(rng.choice(alphabet) for _ in range(rng.randint(0, 12))))
    return list(dict.fromkeys(out))


def verdict(result):
    """Core's verdict on one payload: read, with the bytes read, or reject."""
    if result["error"]:
        return {"payload": result["payload"], "core": "reject"}
    return {"payload": result["payload"], "core": "read", "consumed": result["consumed"]}


def main():
    repo, out_dir = sys.argv[1], sys.argv[2]
    payloads = cases()
    exe = build(repo, out_dir)
    r = subprocess.run([exe], input="\n".join(payloads) + "\n", capture_output=True, text=True, check=True)
    results = [verdict(json.loads(l)) for l in r.stdout.splitlines()]
    if [c["payload"] for c in results] != payloads:
        sys.exit("FAIL: the reader skipped a case")
    print("// Dash Core's verdict on getblocktxn payloads: its own read")
    print("// (BlockTransactionsRequest from blockencodings.h into a CDataStream, as")
    print("// net_processing reads it), compiled from v24.0.0-rc.3. \"core\": \"read\"")
    print("// with the bytes it read, or \"reject\" when the read threw.")
    print("{")
    print('  "getblocktxn": [')
    for c in results:
        print("    %s," % json.dumps(c))
    print("  ],")
    print("}")


if __name__ == "__main__":
    main()
