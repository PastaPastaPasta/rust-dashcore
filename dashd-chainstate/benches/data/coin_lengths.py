#!/usr/bin/env python3
"""The coin-length histogram of a UTXO churn trace (dash-coreimport's `DSTRACE1` format).

Counts every created coin's serialized length (Core's `Coin` bytes, the coins cache's value) and
prints `length count` lines in length order, after a comment header. `coins_layer_density` reads
the output (CH2 §2.3, the layout gate).

Usage: python3 -I coin_lengths.py <trace> > mainnet-453371-coin-lengths.txt
"""

import collections
import os
import struct
import sys

MAGIC = b"DSTRACE1"
END = 0xFFFFFFFF


def main() -> int:
    path = sys.argv[1]
    counts = collections.Counter()
    with open(path, "rb") as f:
        data = f.read()
    if data[:8] != MAGIC:
        sys.exit("bad magic")
    first, last = struct.unpack_from("<II", data, 12)
    pos = 52
    creates = spends = 0
    while True:
        (height,) = struct.unpack_from("<I", data, pos)
        pos += 4
        if height == END:
            t_spends, t_creates = struct.unpack_from("<QQ", data, pos)
            if (t_spends, t_creates) != (spends, creates):
                sys.exit("trailer totals do not match the events")
            break
        (n,) = struct.unpack_from("<I", data, pos)
        pos += 4
        for _ in range(n):
            tag = data[pos]
            pos += 37
            if tag == 0:
                spends += 1
            elif tag == 1:
                (length,) = struct.unpack_from("<H", data, pos)
                pos += 2 + length
                counts[length] += 1
                creates += 1
            else:
                sys.exit(f"event tag {tag}")
    print(f"# coin-length histogram of {os.path.basename(path)}: heights {first}..{last},")
    print(f"# {creates} created coins ({spends} spends). Columns: serialized length, count.")
    for length in sorted(counts):
        print(length, counts[length])
    return 0


if __name__ == "__main__":
    sys.exit(main())
