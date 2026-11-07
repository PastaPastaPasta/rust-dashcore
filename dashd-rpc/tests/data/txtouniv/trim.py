#!/usr/bin/env python3
"""RP1a-2c fixtures, step 3: per network, bucket and `service`, the two shortest transactions;
`service` rows only for ProRegTx and ProUpServTx; no mainnet row whose output depends on Core's
BLS flag (CbTx version 3 and above, MnHF signals, asset unlocks), since the offline mainnet dashd
printed those under the legacy flag (see decode.py). A crafted row (craft.py) is ruled by the
bucket after its `crafted/` prefix.

Usage: trim.py DECODED.jsonl OUT.jsonl
"""
import collections
import json
import sys

FIELDS = ("net", "service", "bucket", "source", "dashd", "hex", "reply")


def flag_dependent(bucket):
    kind, _, rest = bucket.removeprefix("crafted/").partition("/")
    version = int(rest.split("/")[0][2:]) if rest.startswith("pv") else 0
    return kind == "t5" and version >= 3 or kind in ("t7", "t9")


def main():
    rows = [json.loads(line) for line in open(sys.argv[1])]
    groups = collections.defaultdict(list)
    for r in rows:
        kind = r["bucket"].removeprefix("crafted/").split("/")[0]
        if r["service"] and kind not in ("t1", "t2"):
            continue
        if r["net"] == "main" and flag_dependent(r["bucket"]):
            continue
        groups[(r["net"], r["bucket"], r["service"])].append(r)
    kept = 0
    with open(sys.argv[2], "w") as out:
        for _, group in sorted(groups.items()):
            group.sort(key=lambda r: len(r["hex"]))
            for r in group[:2]:
                out.write(json.dumps({k: r[k] for k in FIELDS}) + "\n")
                kept += 1
    print(kept)


if __name__ == "__main__":
    main()
