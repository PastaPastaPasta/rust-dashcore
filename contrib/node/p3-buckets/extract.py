"""Extract libstdc++'s `__prime_list` and `__fast_bkt` from an ELF file's bytes.

usage: python3 -I -B contrib/node/p3-buckets/extract.py <elf> [<elf> ...]

For each file: every offset where the 305-entry u64 `__prime_list` (304 primes plus the repeated
sentinel, 0x988 bytes on LP64) starts, its sha256, and every offset of the 14-byte `__fast_bkt`.
Prints the first file's table as JSON on the last line so the caller can diff it.
"""
import hashlib
import json
import struct
import sys

HEAD = struct.pack("<8Q", 2, 3, 5, 7, 11, 13, 17, 19)
FAST = bytes([2, 2, 2, 3, 5, 5, 7, 7, 11, 11, 11, 11, 13, 13])
N = 305


def scan(path):
    data = open(path, "rb").read()
    lists, off = [], data.find(HEAD)
    while off >= 0:
        vals = struct.unpack_from(f"<{N}Q", data, off)
        ok = vals[-1] == vals[-2] == 18446744073709551557 and list(vals[:-1]) == sorted(set(vals[:-1]))
        lists.append((off, ok, vals))
        off = data.find(HEAD, off + 1)
    fasts, off = [], data.find(FAST)
    while off >= 0:
        fasts.append(off)
        off = data.find(FAST, off + 1)
    return hashlib.sha256(data).hexdigest(), lists, fasts


first = None
for path in sys.argv[1:]:
    digest, lists, fasts = scan(path)
    print(f"{path}\n  sha256 {digest}")
    for off, ok, vals in lists:
        raw = struct.pack(f"<{N}Q", *vals)
        print(f"  __prime_list @ {off} valid={ok} sha256(0x988)={hashlib.sha256(raw).hexdigest()}")
        if ok and first is None:
            first = list(vals)
    print(f"  __fast_bkt @ {fasts}")
print(json.dumps(first))
