#!/usr/bin/env python3
"""Compare `dml_window`'s per-block digests with an O8 replica's `digest.jsonl` (EV2b D43).

`dml_window` writes `<height> <CanonicalMNListHash>` per block; the replica's StateDigest log has
one `{"k": "C", "h": height, "s": {"mnl": hash, ...}}` record per connected block, both hashes in
display order. A height connected more than once (a reorg, a restart replaying blocks) keeps its
last record, the block the replica's chain holds now, which is the one the window was fetched
from. Every digest of ours must have a replica record and equal it. Run it only on a window that
printed WINDOW OK: one that stopped on a difference leaves a partial digest file.

Usage: dml-digest-compare.py DIGEST_JSONL OUR_DIGESTS
"""
import json
import sys


def main():
    jsonl, ours_path = sys.argv[1:3]
    ours = {}
    with open(ours_path) as f:
        for line in f:
            height, digest = line.split()
            ours[int(height)] = digest
    if not ours:
        sys.exit("no digests of ours")
    replica = {}
    with open(jsonl) as f:
        for line in f:
            record = json.loads(line)
            if record.get("k") == "C" and record["h"] in ours:
                replica[record["h"]] = record["s"]["mnl"]
    equal = 0
    for height in sorted(ours):
        theirs = replica.get(height)
        if theirs != ours[height]:
            print(f"DIGEST DIFF at {height}: ours {ours[height]}, replica {theirs} ({equal} equal before)")
            sys.exit(1)
        equal += 1
    print(f"DIGESTS OK {equal} blocks {min(ours)}..{max(ours)}")


if __name__ == "__main__":
    main()
