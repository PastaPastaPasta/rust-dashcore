#!/usr/bin/env python3
"""The churn profile of a UTXO churn trace's last blocks (dash-coreimport's `DSTRACE1` format).

`coins_flush`'s `replay/*` workloads replay a deterministic synthetic churn with this shape (CH2 §5.7,
CH2b b1): PF0 benches read only tracked inputs, and a trace is ~1 GB. For the last `window` blocks it
prints, after a comment header:

- `window <first> <last>`: the window's heights;
- `live <coins> <bytes>`: the live set when the window starts;
- `live_len <length> <count>`: that live set's serialized coin lengths;
- `start_age <bucket> <count>`: that live set's ages in blocks at the window's start, in log2 buckets
  (bucket `b` holds ages in `[2^b - 1, 2^(b+1) - 1)`, so bucket 0 is age 0);
- `spend_age <bucket> <count>`: the age of each coin the window spends, when it is spent;
- `len <length> <count>`: the serialized lengths of the coins the window creates;
- `block <creates> <spends> <bucket>:<count>...`: one line per block of the window, in order, with
  the ages of the coins it spends.

Usage: python3 -I churn_profile.py <trace> <window> > <net>-<height>-churn.txt
"""

import collections
import os
import struct
import sys

MAGIC = b"DSTRACE1"
END = 0xFFFFFFFF


def bucket(age: int) -> int:
    return (age + 1).bit_length() - 1


def main() -> int:
    path, window = sys.argv[1], int(sys.argv[2])
    with open(path, "rb") as f:
        data = f.read()
    if data[:8] != MAGIC:
        sys.exit("bad magic")
    first, last = struct.unpack_from("<II", data, 12)
    start = last - window + 1
    live = {}  # outpoint -> (height, length)
    start_age = collections.Counter()
    live_len = collections.Counter()
    spend_age = collections.Counter()
    lengths = collections.Counter()
    blocks = []
    live_at_start = None
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
        if height == start:
            live_at_start = (len(live), sum(n for _, n in live.values()))
            for h, length in live.values():
                start_age[bucket(start - h)] += 1
                live_len[length] += 1
        (n,) = struct.unpack_from("<I", data, pos)
        pos += 4
        in_window = height >= start
        c = s = 0
        ages = collections.Counter()
        for _ in range(n):
            tag = data[pos]
            key = data[pos + 1 : pos + 37]
            pos += 37
            if tag == 0:
                h, _ = live.pop(key)
                spends += 1
                if in_window:
                    spend_age[bucket(height - h)] += 1
                    ages[bucket(height - h)] += 1
                    s += 1
            elif tag == 1:
                (length,) = struct.unpack_from("<H", data, pos)
                pos += 2 + length
                live[key] = (height, length)
                creates += 1
                if in_window:
                    lengths[length] += 1
                    c += 1
            else:
                sys.exit(f"event tag {tag}")
        if in_window:
            blocks.append((c, s, ages))
    if live_at_start is None or len(blocks) != window:
        sys.exit("the window is not inside the trace")
    name = os.path.basename(path)
    total_c = sum(c for c, _, _ in blocks)
    total_s = sum(s for _, s, _ in blocks)
    print(f"# churn profile of {name}: the window {start}..{last} of heights {first}..{last},")
    print(f"# {total_c} creates and {total_s} spends in {window} blocks. See churn_profile.py.")
    print("window", start, last)
    print("live", *live_at_start)
    for length in sorted(live_len):
        print("live_len", length, live_len[length])
    for b in sorted(start_age):
        print("start_age", b, start_age[b])
    for b in sorted(spend_age):
        print("spend_age", b, spend_age[b])
    for length in sorted(lengths):
        print("len", length, lengths[length])
    for c, s, ages in blocks:
        print("block", c, s, *(f"{b}:{ages[b]}" for b in sorted(ages)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
