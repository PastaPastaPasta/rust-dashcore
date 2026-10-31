#!/usr/bin/env python3
"""Special-payload cases checked with decoderawtransaction on both regtest dashds.

decoderawtransaction prints the payload object (proRegTx, cbTx, ...) only when
Core's GetTxPayload succeeds, so its presence is Core's payload decode verdict.

Usage: probe_payload.py BASE_SDK_ROOT OUT.json
"""
import json
import re
import sys

import nodes

KEYS = ["proRegTx", "proUpServTx", "proUpRegTx", "proUpRevTx", "cbTx", "qcTx", "mnhfTx", "assetLockTx", "assetUnlockTx"]


def corpus_raw(root, name, txid):
    text = open("%s/pkgs/primitives/corpus/%s.json5" % (root, name)).read()
    rest = text[text.index('"%s"' % txid):]
    return bytes.fromhex(re.search(r'"raw":\s*"([0-9a-f]+)"', rest).group(1))


def read_cs(b, i):
    c = b[i]
    if c < 0xfd:
        return c, i + 1
    w = {0xfd: 2, 0xfe: 4, 0xff: 8}[c]
    return int.from_bytes(b[i + 1:i + 1 + w], "little"), i + 1 + w


def write_cs(n):
    if n < 0xfd:
        return bytes([n])
    for marker, w in ((0xfd, 2), (0xfe, 4), (0xff, 8)):
        if n < 1 << (8 * w):
            return bytes([marker]) + n.to_bytes(w, "little")


def split_payload(tx):
    """(everything before the payload length, payload) of a special tx."""
    n, i = read_cs(tx, 4)
    for _ in range(n):
        l, i = read_cs(tx, i + 36)
        i += l + 4
    n, i = read_cs(tx, i)
    for _ in range(n):
        l, i = read_cs(tx, i + 8)
        i += l
    i += 4
    plen, j = read_cs(tx, i)
    assert j + plen == len(tx)
    return tx[:i], tx[j:]


def with_payload(tx, payload):
    return split_payload(tx)[0] + write_cs(len(payload)) + payload


def service(ip, port):
    """A NetInfo service entry: type 1, BIP155 IPv4, big-endian port."""
    return bytes([1, 1, 4]) + bytes(ip) + port.to_bytes(2, "big")


def proupserv_v3(proreg, groups):
    """A ProUpServTx v3 (regular masternode) whose NetInfo has `groups`, in the
    order given, built on the proreg transaction's inputs and outputs."""
    netinfo = bytes([1, len(groups)]) + b"".join(bytes([p, len(e)]) + b"".join(e) for p, e in groups)
    # An all-zero signature decodes (as an invalid one); 0xc0 + zeros throws.
    payload = b"\x03\x00" + b"\x00\x00" + bytes([0x11]) * 32 + netinfo + b"\x00" + bytes(32) + bytes(96)
    prefix = split_payload(proreg)[0]
    return ((2 << 16) | 3).to_bytes(4, "little") + prefix[4:] + write_cs(len(payload)) + payload


def verdict(tx):
    out = {}
    for node in nodes.NODES:
        d = nodes.cli(node, "decoderawtransaction", tx.hex())
        out[node] = {k: d[k] for k in KEYS if k in d} or None
    return out


def check(res):
    """The repeated-purpose case: both releases keep the first group of each
    purpose, in purpose order."""
    case = res["proupserv_v3_repeated_purpose"]["core"]
    for node, got in case.items():
        assert got["proUpServTx"]["addresses"] == {"core_p2p": ["5.6.7.8:2"], "platform_https": ["1.2.3.4:1"]}, (node, got)


def main():
    root = sys.argv[1]
    proreg = corpus_raw(root, "proregtx", "a9e25c9aa56874c5703fc78ce8541364560d5b21eb12812cf452a27776b1f840")
    cbtx = corpus_raw(root, "cbtx", "ac9fc5d0c7229a1c9fb834d8c6f6be200ae5b55d1b731c38afd83adc0c54aa31")
    _, ppl = split_payload(proreg)
    _, cpl = split_payload(cbtx)
    assert cpl[70] == 1  # CbTx v3: bestCLHeightDiff follows version, height and two roots
    cases = {
        "proregtx_as_mined": proreg,
        "proregtx_trailing_1": with_payload(proreg, ppl + b"\x00"),
        "cbtx_as_mined": cbtx,
        "cbtx_trailing_1": with_payload(cbtx, cpl + b"\x00"),
        "cbtx_cl_height_diff_max_size": with_payload(cbtx, cpl[:70] + bytes.fromhex("fe00000002") + cpl[71:]),
        "cbtx_cl_height_diff_above_max_size": with_payload(cbtx, cpl[:70] + bytes.fromhex("fe01000002") + cpl[71:]),
        # Purpose 2, then 0, then 2 again: Core's map keeps the first group of
        # each purpose and lists them in purpose order.
        "proupserv_v3_repeated_purpose": proupserv_v3(proreg, [
            (2, [service([1, 2, 3, 4], 1)]),
            (0, [service([5, 6, 7, 8], 2)]),
            (2, [service([9, 10, 11, 12], 3)]),
        ]),
    }
    res = {k: {"raw": v.hex(), "core": verdict(v)} for k, v in cases.items()}
    json.dump(res, open(sys.argv[2], "w"), indent=1)
    check(res)


if __name__ == "__main__":
    main()
