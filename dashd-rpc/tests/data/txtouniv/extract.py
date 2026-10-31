#!/usr/bin/env python3
"""RP1a-2c fixtures, step 1: special transactions (and a few plain spends) read from Dash Core's
own blk files, bucketed so every payload shape appears a few times.

Usage: extract.py OUT.jsonl NET:BLKFILE [NET:BLKFILE ...]
Each output line: {"net", "source", "type", "bucket", "hex"}.
Read-only: the blk files are opened for reading and never locked or written.
"""
import json
import sys

PER_BUCKET = 3
PLAIN_PER_FILE = 4
# Payload versions are u16 for these types, u8 for the rest.
U16_VERSION = {1, 2, 3, 4, 5, 6, 10, 11, 12}


def varint(b, i):
    n = b[i]
    if n < 0xFD:
        return n, i + 1
    if n == 0xFD:
        return int.from_bytes(b[i + 1:i + 3], "little"), i + 3
    if n == 0xFE:
        return int.from_bytes(b[i + 1:i + 5], "little"), i + 5
    return int.from_bytes(b[i + 1:i + 9], "little"), i + 9


def parse_tx(b, i):
    start = i
    ver32 = int.from_bytes(b[i:i + 4], "little")
    i += 4
    version, txtype = ver32 & 0xFFFF, ver32 >> 16
    nin, i = varint(b, i)
    for _ in range(nin):
        i += 36
        n, i = varint(b, i)
        i += n + 4
    nout, i = varint(b, i)
    for _ in range(nout):
        i += 8
        n, i = varint(b, i)
        i += n
    i += 4
    payload = b""
    if version >= 3 and txtype != 0:
        n, i = varint(b, i)
        payload = b[i:i + n]
        i += n
    return b[start:i], version, txtype, nin, payload, i


def script(b, i):
    n, i = varint(b, i)
    return b[i:i + n], i + n


def net_info_kinds(b, i, extended):
    """The entry types of a ProTx's network info, and the offset after it."""
    if not extended:
        return set(), i + 18  # MnNetInfo: one legacy CService
    version = b[i]
    i += 1
    if version != 1:
        return set(), i  # an unknown version stops after its version byte
    kinds = set()
    purposes, i = varint(b, i)
    for _ in range(purposes):
        i += 1
        entries, i = varint(b, i)
        for _ in range(entries):
            kind = b[i]
            i += 1
            kinds.add(kind)
            if kind == 1:  # CService in ADDRv2: network id, address, big-endian port
                n, i = varint(b, i + 1)
                i += n + 2
            elif kind == 2:  # DomainPort: string, big-endian port
                n, i = varint(b, i)
                i += n + 2
    return kinds, i


def shape(txtype, pv, payload):
    """The payload dimensions the fixture keeps one row of each: a domain or an invalid network
    entry, a collateral share whose reward script differs from its refund script, and a
    ProUpShareTx whose reward script is empty (it prints no rewardAddress)."""
    if txtype == 11:
        reward, _ = script(payload, 2 + 32 + 2)
        return "" if reward else "/noreward"
    if txtype not in (1, 2) or pv < 3:
        return ""
    i = 2 + 2 + (2 + 36 if txtype == 1 else 32)
    kinds, i = net_info_kinds(payload, i, True)
    key = "/domain" if 2 in kinds else ""
    key += "/invalid" if kinds - {1, 2} else ""
    if txtype == 1:
        i += 20 + 48 + 20 + 2
        payouts = payload[i]
        i += 1
        for _ in range(payouts):
            _, i = script(payload, i)
            i += 2
        shares = payload[i]
        i += 1
        for _ in range(shares):
            refund, i = script(payload, i + 8)
            reward, i = script(payload, i)
            i += 20
            if reward and reward != refund:
                key += "/ownreward"
                break
    return key


def bucket(txtype, version, nin, payload):
    if txtype == 0:
        return f"plain/v{version}/in{min(nin, 3)}"
    if txtype in U16_VERSION:
        key = f"t{txtype}/pv{int.from_bytes(payload[:2], 'little')}"
        # A ProRegTx always has its type after the version; a ProUpServTx from version 2 on.
        if (txtype == 1 or txtype == 2 and payload[:2] != b"\x01\x00") and len(payload) >= 4:
            key += f"/mn{int.from_bytes(payload[2:4], 'little')}"
        # A commitment's own version (after the payload's version and height), and whether it is
        # null: a null commitment's members signature, its last 96 bytes, is all zeros.
        if txtype == 6 and len(payload) >= 104:
            null = "null" if payload[-96:] == bytes(96) else "full"
            key += f"/qv{int.from_bytes(payload[6:8], 'little')}/{null}"
        try:
            key += shape(txtype, int.from_bytes(payload[:2], "little"), payload)
        except IndexError:
            key += "/short"
        return key
    return f"t{txtype}/pv{payload[0] if payload else -1}"


def main():
    out_path, sources = sys.argv[1], sys.argv[2:]
    counts = {}
    with open(out_path, "w") as out:
        for spec in sources:
            net, path = spec.split(":", 1)
            data = open(path, "rb").read()
            plain = 0
            pos = 0
            while pos + 8 <= len(data):
                size = int.from_bytes(data[pos + 4:pos + 8], "little")
                if size == 0:
                    break
                block = data[pos + 8:pos + 8 + size]
                pos += 8 + size
                if len(block) < size:
                    break  # a block still being written
                i = 80
                ntx, i = varint(block, i)
                parsed = []
                try:
                    for t in range(ntx):
                        raw, version, txtype, nin, payload, i = parse_tx(block, i)
                        parsed.append((t, raw, version, txtype, nin, payload))
                except (IndexError, ValueError):
                    print("unparsed block in", path, file=sys.stderr)
                    continue
                for t, raw, version, txtype, nin, payload in parsed:
                    if t == 0 and txtype == 0:
                        continue
                    key = bucket(txtype, version, nin, payload)
                    if txtype == 0:
                        if plain >= PLAIN_PER_FILE:
                            continue
                        plain += 1
                    if counts.get((net, key), 0) >= PER_BUCKET:
                        continue
                    counts[(net, key)] = counts.get((net, key), 0) + 1
                    out.write(json.dumps({"net": net, "source": path, "type": txtype,
                                          "bucket": key, "hex": raw.hex()}) + "\n")
    for (net, key), n in sorted(counts.items()):
        print(net, key, n)


if __name__ == "__main__":
    main()
