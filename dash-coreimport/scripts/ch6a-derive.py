#!/usr/bin/env python3
"""Derive explicitly labelled CH6a parser fixtures from stopped Core captures."""

import importlib.util
import json
from pathlib import Path
import shutil
import struct
import sys

SPEC = importlib.util.spec_from_file_location("capture", Path(__file__).with_name("ch6a-capture.py"))
CORE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CORE)
OBFUSCATE_KEY = b"\x0e\0obfuscate_key"


def varint(value):
    data = bytearray()
    while value >= 128:
        data.append((value & 127) | 128)
        value >>= 7
    data.append(value)
    return data


def sized(value):
    return varint(len(value)) + value


def log_record(data):
    assert len(data) <= 32761
    return struct.pack("<IHB", CORE.crc32c(b"\x01" + data), len(data), 1) + data


def batch(sequence, rows):
    data = bytearray(struct.pack("<QI", sequence, len(rows)))
    for key, value in rows:
        data.append(0 if value is None else 1)
        data.extend(sized(key))
        if value is not None:
            data.extend(sized(value))
    return log_record(data)


def block(rows):
    data = bytearray()
    restarts = []
    for key, value in rows:
        restarts.append(len(data))
        data.extend(varint(0) + varint(len(key)) + varint(len(value)) + key + value)
    restarts = restarts or [0]
    for offset in restarts:
        data.extend(struct.pack("<I", offset))
    data.extend(struct.pack("<I", len(restarts)))
    return data


def table(path, rows):
    rows = sorted(rows, key=lambda row: (row[0], -row[1]))
    entries = [(key + struct.pack("<Q", (seq << 8) | (value is not None)), value or b"")
               for key, seq, value in rows]
    data = bytearray()

    def append(payload):
        handle = varint(len(data)) + varint(len(payload))
        data.extend(payload)
        data.append(0)
        data.extend(struct.pack("<I", CORE.crc32c(payload + b"\0")))
        return handle

    data_handle = append(block(entries))
    meta_handle = append(block([]))
    index_handle = append(block([(entries[-1][0], data_handle)]))
    footer = meta_handle + index_handle
    data.extend(footer + bytes(40 - len(footer)) + struct.pack("<Q", 0xdb4775248b80fb57))
    path.write_bytes(data)
    return len(data), entries[0][0], entries[-1][0]


def audit_expected(dest, root, expected):
    audits = {name: CORE.audit(dest / name) for name in ("chainstate", "blocks/index")}
    for audit in audits.values():
        for item in audit["versions"] + audit["features"]:
            item["file"] = str(Path(item["file"]).relative_to(root))
    expected["audit"] = audits
    (dest / "expected.json").write_text(json.dumps(expected, indent=2, sort_keys=True) + "\n")


def derive(root):
    base = root / "a-wal"
    expected = json.loads((base / "expected.json").read_text())
    raw_best = bytes.fromhex(CORE.latest(expected["audit"]["chainstate"], b"B")["value"])
    raw_key = bytes.fromhex(CORE.latest(expected["audit"]["chainstate"], OBFUSCATE_KEY)["value"])
    assert len(raw_key) == 9 and raw_key[0] == 8 and any(raw_key[1:])

    def copy(name):
        dest = root / name
        shutil.copytree(base, dest)
        return dest

    def describe(dest, detail):
        (dest / "derivation.json").write_text(json.dumps(dict(base="a-wal", detail=detail), indent=2) + "\n")

    dest = copy("h-nonzero-key")
    audit_expected(dest, root, json.loads(json.dumps(expected)))
    describe(dest, "Verbatim fresh-chainstate nonzero key; checked against Core debug log")

    dest = copy("i-multiple-logs")
    (dest / "chainstate/000007.log").write_bytes(batch(100, [(b"B", raw_best)]))
    (dest / "chainstate/000008.log").write_bytes(batch(1, [(b"B", None)]))
    values = json.loads(json.dumps(expected))
    values["refusal"] = "log sequence order"
    values["best_block"] = values["decoded"]["B"] = None
    audit_expected(dest, root, values)
    describe(dest, "Live WAL 7 holds B at seq 100; higher-numbered WAL 8 holds older deletion at seq 1; original WAL remains live")

    for name, old_kind, winning in [("j-l0-older-tombstone", None, raw_best),
                                    ("k-l0-older-value", bytes(32), raw_best),
                                    ("l-lower-level-tombstone", raw_best, None)]:
        dest = copy(name)
        db = dest / "chainstate"
        shutil.rmtree(db)
        db.mkdir()
        rows = [(0, 101, [(b"B", 20, old_kind)]),
                (0, 100, [(b"B", 10, raw_best)]),
                (1, 10, [(OBFUSCATE_KEY, 1, raw_key), (b"B", 30, winning)])]
        manifest = bytearray(b"\x01" + sized(b"leveldb.BytewiseComparator"))
        manifest.extend(b"\x02" + varint(200) + b"\x09\0\x03" + varint(201) + b"\x04\x1e")
        for level, number, values in rows:
            size, smallest, largest = table(db / f"{number:06}.ldb", values)
            manifest.extend(b"\x07" + varint(level) + varint(number) + varint(size) + sized(smallest) + sized(largest))
        (db / "CURRENT").write_text("MANIFEST-000001\n")
        (db / "MANIFEST-000001").write_bytes(log_record(manifest))
        (db / "000200.log").write_bytes(b"")
        values = json.loads(json.dumps(expected))
        values["refusal"] = "source priority contradiction"
        values["best_block"] = values["decoded"]["B"] = None
        audit_expected(dest, root, values)
        describe(dest, "Constructed overlapping L0 files (seq 10,20); lower-priority L1 holds contradictory seq 30; values originate in a-wal")

    malformed = {
        "malformed-R": ("blocks/index", b"R", b"11"),
        "malformed-pruned": ("blocks/index", b"F\x10prunedblockfiles", b"2"),
        "malformed-last-file": ("blocks/index", b"l", b"\0\0\0"),
        "negative-last-file": ("blocks/index", b"l", b"\xff\xff\xff\xff"),
        "malformed-key": ("chainstate", OBFUSCATE_KEY, b"\x08" + raw_key[1:8]),
        "malformed-key-vector": ("chainstate", OBFUSCATE_KEY, b"\xfd\x08\0" + raw_key[1:]),
        "malformed-key-length": ("chainstate", OBFUSCATE_KEY, b"\x07" + raw_key[1:8]),
        "malformed-B": ("chainstate", b"B", raw_best[:-1]),
        "missing-B": ("chainstate", b"B", None),
        "best-not-in-files": ("chainstate", b"B", raw_key[1:] * 4),
    }
    for name, (db, key, value) in malformed.items():
        dest = copy(name)
        (dest / db / "000200.log").write_bytes(batch(100, [(key, value)]))
        describe(dest, dict(database=db, key=key.hex(), value=value.hex() if value is not None else None,
                            sequence=100, file=f"{db}/000200.log", offset=19))

    dest = copy("absent-obfuscation-key")
    plaintext = bytes.fromhex(expected["best_block"])[::-1]
    (dest / "chainstate/000200.log").write_bytes(batch(100, [(OBFUSCATE_KEY, None), (b"B", plaintext)]))
    values = json.loads(json.dumps(expected))
    values["decoded"]["obfuscation_key"] = bytes(8).hex()
    audit_expected(dest, root, values)
    describe(dest, "Tombstone the obfuscation-key record and store plaintext RPC-known B; absent key decodes as eight zero bytes")

    original = (base / "blocks/blk00000.dat").read_bytes()
    second = 8 + struct.unpack_from("<I", original, 4)[0]
    for name in ["torn-blk", "corrupt-blk", "malformed-blk", "bad-merkle-blk", "corrupt-later-blk", "zero-tail-blk"]:
        dest = copy(name)
        path = dest / "blocks/blk00000.dat"
        data = bytearray(original)
        offset = second
        if name == "torn-blk":
            data = data[:-1]
            offset = len(data)
        elif name == "corrupt-blk":
            data[second] ^= 1
        elif name == "malformed-blk":
            data[second + 8 + 80] = 255
            offset = second + 8 + 80
        elif name == "bad-merkle-blk":
            data[second + 8 + 36] ^= 1
            offset = second + 8 + 36
        elif name == "corrupt-later-blk":
            path = dest / "blocks/blk00001.dat"
            data = bytearray(original)
            data[second] ^= 1
        else:
            offset = len(data)
            data.extend(bytes(4096))
        path.write_bytes(data)
        describe(dest, dict(file=str(path.relative_to(dest)), offset=offset, change=name))


if __name__ == "__main__":
    derive(Path(sys.argv[1]))
