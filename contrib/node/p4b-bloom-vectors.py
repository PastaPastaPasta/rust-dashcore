#!/usr/bin/env python3
"""Extract the transactions and blocks of Dash Core's `src/test/bloom_tests.cpp` (P4b D8).

Usage: p4b-bloom-vectors.py <Core src/test/bloom_tests.cpp> <OUT_DIR>

Writes one hex file per object to OUT_DIR, named after the test case and the object:
`bloom_match-tx.hex`, `bloom_match-spending.hex` (the byte-array transaction, Core's trailing
array terminator dropped as Core's `sizeof(ch) - 1` does), `dip2-proregtx.hex`,
`dip2-proupservtx.hex`, `dip2-proupregtx.hex`, `dip2-prouprevtx.hex`, `merkle_block_<n>.hex`
for the serialized blocks, and `merkle_block_3-merkleblock.hex`, the expected `merkleblock`.
Run it on the v24.0.0-rc.3 file (`git show v24.0.0-rc.3:src/test/bloom_tests.cpp`).
"""

import pathlib
import re
import sys


def cases(src):
    """Yields (name, body) for every BOOST_AUTO_TEST_CASE."""
    parts = re.split(r"BOOST_AUTO_TEST_CASE\((\w+)\)", src)
    for i in range(1, len(parts), 2):
        yield parts[i], parts[i + 1]


def hexes(body):
    return re.findall(r'ParseHex\("([0-9a-f]+)"\)', body)


def main():
    src = pathlib.Path(sys.argv[1]).read_text()
    out = pathlib.Path(sys.argv[2])
    out.mkdir(parents=True, exist_ok=True)
    found = {}
    for name, body in cases(src):
        if name == "bloom_match":
            found["bloom_match-tx"] = hexes(body)[0]
            arr = re.search(r"unsigned char ch\[\] = \{([^}]*)\}", body).group(1)
            data = bytes(int(b, 16) for b in re.findall(r"0x([0-9a-f]{2})", arr))
            found["bloom_match-spending"] = data[:-1].hex()
        elif name == "dip2_bloom_match":
            for tag, h in zip(["proregtx", "proupservtx", "proupregtx", "prouprevtx"], hexes(body)):
                found[f"dip2-{tag}"] = h
        elif name in ("merkle_block_2", "merkle_block_3_and_serialize", "merkle_block_4"):
            n = name.split("_")[2]
            hs = hexes(body)
            found[f"merkle_block_{n}"] = hs[0]
            if name == "merkle_block_3_and_serialize":
                found["merkle_block_3-merkleblock"] = next(h for h in hs if len(h) < len(hs[0]) and h.startswith(hs[0][:160]))
    for key, value in sorted(found.items()):
        (out / f"{key}.hex").write_text(value + "\n")
        print(f"{key}.hex {len(value) // 2} bytes")


if __name__ == "__main__":
    main()
