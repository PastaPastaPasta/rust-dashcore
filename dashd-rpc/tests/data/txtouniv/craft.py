#!/usr/bin/env python3
"""RP1a-2c crafted fixtures: payload shapes no chain carries, made by editing one field of a
transaction extract.py read from a chain. decoderawtransaction needs no validity, so dashd prints
them as it would print the same bytes in a block.

Usage: craft.py RAW.jsonl OUT.jsonl
Each output line: {"net", "source", "type", "bucket", "hex"}, as extract.py writes; `bucket`
starts with `crafted/` and `source` names the edit and the row it was made from.
"""
import hashlib
import json
import sys

from extract import parse_tx, varint


def compact(n):
    if n < 0xFD:
        return bytes([n])
    if n <= 0xFFFF:
        return b"\xfd" + n.to_bytes(2, "little")
    return b"\xfe" + n.to_bytes(4, "little")


def with_payload(raw, payload, new):
    """`raw` with its trailing payload replaced by `new` (the payload is the last field)."""
    head = raw[:len(raw) - len(payload)]
    size = len(compact(len(payload)))
    return head[:len(head) - size] + compact(len(new)) + new


def https_list_end(p, i):
    """For an ExtNetInfo at `i`: the offset of the PLATFORM_HTTPS list's count and of its end."""
    assert p[i] == 1
    i += 1
    purposes, i = varint(p, i)
    for _ in range(purposes):
        purpose = p[i]
        count_at = i + 1
        entries, i = varint(p, count_at)
        for _ in range(entries):
            kind = p[i]
            i += 1
            if kind == 1:
                n, i = varint(p, i + 1)
                i += n + 2
            elif kind == 2:
                n, i = varint(p, i)
                i += n + 2
        if purpose == 2:
            return count_at, i
    raise ValueError("no platform_https list")


def invalid_entry(p):
    """A ProUpServTx v3 with a third PLATFORM_HTTPS entry of type 0xff (Core's Invalid)."""
    count_at, end = https_list_end(p, 2 + 2 + 32)
    assert p[count_at] < 0xFC
    return p[:count_at] + bytes([p[count_at] + 1]) + p[count_at + 1:end] + b"\xff" + p[end:]


def invalid_first_entry(p):
    """A ProUpServTx v3 whose first PLATFORM_HTTPS entry is of type 0xff: under `service`, Core's
    `GetPlatformPort` reads that entry and prints `platformHTTPPort` 0."""
    count_at, _ = https_list_end(p, 2 + 2 + 32)
    assert p[count_at] < 0xFC
    return p[:count_at] + bytes([p[count_at] + 1]) + b"\xff" + p[count_at + 1:]


def share_reward(script):
    """A ProUpShareTx whose reward script is `script`."""
    def edit(p):
        at = 2 + 32 + 2
        n, end = varint(p, at)
        assert n > 0
        return p[:at] + compact(len(script)) + script + p[end + n:]
    return edit


def empty_share_reward(p):
    """A ProUpShareTx whose reward script is empty: Core prints no `rewardAddress`."""
    return share_reward(b"")(p)


def nonstandard_share_reward(p):
    """A ProUpShareTx whose reward script is `OP_TRUE`, non-empty but without a destination: Core
    prints no `rewardAddress`."""
    return share_reward(b"\x51")(p)


def operator_reward(n):
    """A ProRegTx v2 (legacy network info) with nOperatorReward = `n`."""
    def edit(p):
        assert int.from_bytes(p[:2], "little") == 2
        at = 2 + 2 + 2 + 36 + 18 + 20 + 48 + 20
        return p[:at] + n.to_bytes(2, "little") + p[at + 2:]
    edit.__name__ = f"operator_reward_{n}"
    return edit


EDITS = (
    ("crafted/t2/pv3/mn1/invalid", "regtest", "t2/pv3/mn1/domain", invalid_entry),
    ("crafted/t11/pv1/noreward", "regtest", "t11/pv1", empty_share_reward),
    # 7 / 100 prints 0.07000000000000001 (`%.16g`); 57 / 100 prints 0.57, where 57 * 0.01 would
    # print 0.5700000000000001, so the second pins the division as well as the formatter.
    ("crafted/t1/pv2/mn0/opreward7", "regtest", "t1/pv2/mn0", operator_reward(7)),
    ("crafted/t2/pv3/mn1/invalidfirst", "regtest", "t2/pv3/mn1/domain", invalid_first_entry),
    ("crafted/t11/pv1/nonstdreward", "regtest", "t11/pv1", nonstandard_share_reward),
    ("crafted/t1/pv2/mn0/opreward57", "regtest", "t1/pv2/mn0", operator_reward(57)),
)


def main():
    rows = [json.loads(line) for line in open(sys.argv[1])]
    with open(sys.argv[2], "w") as out:
        for name, net, base, edit in EDITS:
            row = min((r for r in rows if r["net"] == net and r["bucket"] == base),
                      key=lambda r: (len(r["hex"]), r["hex"]))
            raw = bytes.fromhex(row["hex"])
            tx, _, txtype, _, payload, end = parse_tx(raw, 0)
            assert end == len(raw)
            new = with_payload(raw, payload, edit(payload))
            source = f"{edit.__name__} of {base} {tx_id(raw)} in {row['source']}"
            out.write(json.dumps({"net": net, "source": source,
                                  "type": txtype, "bucket": name, "hex": new.hex()}) + "\n")


def tx_id(raw):
    return hashlib.sha256(hashlib.sha256(raw).digest()).digest()[::-1].hex()


if __name__ == "__main__":
    main()
