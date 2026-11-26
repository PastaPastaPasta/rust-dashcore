#!/usr/bin/env python3
"""Prints the known-answer vector of dash-consensus's `lock_inputs` test (`connect.rs`).

Core v24.0.0's `GetLockInputs` (instantsend/lock.cpp:46-57), written out here from Core's
serialization alone, not from the Rust code under test:

- an asset unlock (`IsAssetUnlockPayload`, primitives/transaction.h:390-394: version >= 3,
  type 9, a payload of exactly ASSET_UNLOCK_PAYLOAD_SIZE = 1 + 8 + 4 + 4 + 32 + 96 bytes) has
  one lock input, `(SerializeHash(make_pair(ASSET_UNLOCK_REQUESTID_PREFIX, index)), 0)`:
  `SerializeHash` is SHA256d of the serialized pair, a `string_view` serializes as a compact size
  then its bytes (serialize.h:1057-1063), and the index is `ReadLE64(payload + 1)`
  (transaction.h:405-410), serialized as 8 bytes little-endian;
- any other transaction has its inputs' prevouts, in input order.

Each transaction is Core's wire form (`CTransaction::SerializationOp`): nVersion and nType packed
into one little-endian int32 (`PackVersionAndType`), the inputs, the outputs, nLockTime, and for a
special transaction (version >= 3, type != 0) the payload as a compact-size vector. Hashes are
printed in internal (wire) byte order.

Usage: lock-inputs-vectors.py
"""

import hashlib
import struct


def compact_size(n):
    if n < 0xFD:
        return bytes([n])
    if n <= 0xFFFF:
        return b"\xfd" + struct.pack("<H", n)
    if n <= 0xFFFFFFFF:
        return b"\xfe" + struct.pack("<I", n)
    return b"\xff" + struct.pack("<Q", n)


def sha256d(data):
    return hashlib.sha256(hashlib.sha256(data).digest()).digest()


def tx_bytes(version, tx_type, prevouts, payload):
    out = struct.pack("<i", (version & 0xFFFF) | (tx_type << 16))
    out += compact_size(len(prevouts))
    for txid, n in prevouts:
        out += txid + struct.pack("<I", n) + compact_size(0) + struct.pack("<I", 0xFFFFFFFF)
    script = bytes([0x51])  # OP_TRUE
    out += compact_size(1) + struct.pack("<q", 100_000) + compact_size(len(script)) + script
    out += struct.pack("<I", 0)
    if version >= 3 and tx_type != 0:
        out += compact_size(len(payload)) + payload
    return out


def main():
    # An ordinary version-2 spend of three prevouts, one repeated.
    a = bytes(range(0x00, 0x20))
    b = bytes(range(0xE0, 0x100))
    ordinary = [(a, 1), (b, 0), (a, 1)]
    print("ordinary tx:", tx_bytes(2, 0, ordinary, b"").hex())
    for txid, n in ordinary:
        print("  lock input:", txid.hex(), n)

    # A version-3 asset unlock without inputs: payload version 1, index 0x0102030405060708, fee
    # 1000, requested height 500, quorum hash 0x11.., quorum signature 0x22...
    index = 0x0102030405060708
    payload = (
        bytes([1])
        + struct.pack("<Q", index)
        + struct.pack("<I", 1000)
        + struct.pack("<I", 500)
        + b"\x11" * 32
        + b"\x22" * 96
    )
    assert len(payload) == 145
    print("asset unlock tx:", tx_bytes(3, 9, [], payload).hex())
    prefix = b"plwdtx"
    request_id = sha256d(compact_size(len(prefix)) + prefix + struct.pack("<Q", index))
    print("  lock input:", request_id.hex(), 0)


if __name__ == "__main__":
    main()
