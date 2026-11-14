#!/usr/bin/env python3
"""Transcribe the mnemonic vectors of Dash Core's `wallet_upgradetohd.py`.

    upgradetohd-vectors.py <wallet_upgradetohd.py> <out.json>

Reads the literals of `test/functional/wallet_upgradetohd.py` (lines 234-238 and 258-267 at
v24.0.0-rc.3): a phrase with and without a passphrase and the address each one owns (the test
pays it before `upgradetohd` and asserts the balance after), and the same phrase with a
NUL-terminated passphrase, which owns neither (balance 0). Every value comes from the file; a
pattern that does not match exactly one line is an error, so a changed test cannot be
transcribed silently. Called by `regen-core-vectors.sh functional`.
"""

import ast
import json
import re
import sys


def main(source_path, out_path):
    with open(source_path, encoding="utf-8") as f:
        lines = f.read().split("\n")

    def one(pattern):
        hits = [(i + 1, m) for i, line in enumerate(lines) for m in [re.search(pattern, line)] if m]
        if len(hits) != 1:
            sys.exit("pattern %r matched %d lines, expected 1" % (pattern, len(hits)))
        return hits[0]

    phrase_line, m = one(r'^\s*custom_mnemonic = ("[^"]*")$')
    phrase = ast.literal_eval(m.group(1))
    records = []
    for name, wallet in (("custom_address_1", "w11"), ("custom_address_2", "w12")):
        addr_line, m = one(r'^\s*%s = ("[^"]*")$' % name)
        address = ast.literal_eval(m.group(1))
        _, m = one(r"^\s*node\.sendtoaddress\(%s, (\d+)\)$" % name)
        paid = int(m.group(1))
        up_line, m = one(r'^\s*(?:ret = )?%s\.upgradetohd\(custom_mnemonic(?:, ("[^"]*"))?\)$' % wallet)
        passphrase = ast.literal_eval(m.group(1)) if m.group(1) else ""
        _, m = one(r"^\s*assert_equal\((\d+), %s\.getbalance\(\)\)$" % wallet)
        if int(m.group(1)) != paid:
            sys.exit("%s: balance %s, but %s is paid %d" % (wallet, m.group(1), name, paid))
        records.append({"lines": [phrase_line, addr_line, up_line], "mnemonic": phrase,
                        "passphrase": passphrase, "owns": address, "balance": paid})

    up_line, m = one(r'^\s*ret = w_null\.upgradetohd\(custom_mnemonic, ("[^"]*")\)$')
    passphrase = ast.literal_eval(m.group(1))
    _, m = one(r"^\s*assert_equal\((\d+), w_null\.getbalance\(\)\)$")
    records.append({"lines": [phrase_line, up_line], "mnemonic": phrase, "passphrase": passphrase,
                    "owns_none_of": [r["owns"] for r in records], "balance": int(m.group(1))})

    with open(out_path, "w", encoding="utf-8") as out:
        out.write('{\n"source": "test/functional/wallet_upgradetohd.py",\n"records": [\n')
        out.write(",\n".join(json.dumps(r, separators=(",", ":")) for r in records))
        out.write("\n]\n}\n")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    main(sys.argv[1], sys.argv[2])
