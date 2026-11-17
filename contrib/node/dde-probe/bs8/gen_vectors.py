#!/usr/bin/env python3
# Copyright (c) 2026 The Dash Core developers
# Distributed under the MIT software license, see the accompanying
# file COPYING or http://www.opensource.org/licenses/mit-license.php.
"""BS8 vectors from Dash Core's functional test framework.

Encodes the overlay messages with the serializers of the pinned tree's
test/functional/test_framework/messages.py and prints one line per vector,
"name cmd stream_version hex [max_a max_b]", for `bs8-tool vectors`. messages.py is an
approximate model of Core's wire forms; the judge decides what Core reads.

usage: gen_vectors.py <dash source tree> <bls_wrapper.json5>
"""

import json
import os
import sys
import types


def bls_samples(path):
    """A basic-scheme valid key and signature from pkc's wrapper corpus."""
    with open(path, encoding="utf-8") as f:
        text = "\n".join(l for l in f if not l.lstrip().startswith("//"))
    corpus = json.loads(text)

    def valid(kind):
        for e in corpus[kind]["eager"]:
            if e["B"].startswith("V"):
                return bytes.fromhex(e["in"])
        raise SystemExit(f"no valid {kind} sample")

    return valid("pk"), valid("sig")


def main():
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    sys.path.insert(0, os.path.join(sys.argv[1], "test", "functional"))
    # messages.py imports the X11 module for block hashes, which none of
    # these serializers use.
    stub = types.ModuleType("dash_hash")

    def no_x11(_data):
        raise RuntimeError("X11 is not needed for the overlay messages")

    stub.getPoWHash = no_x11
    sys.modules["dash_hash"] = stub
    from test_framework import messages as m  # pylint: disable=import-outside-toplevel

    pk, sig = bls_samples(sys.argv[2])
    out = []

    def add(name, cmd, obj, version=70242, raw=None, limits=""):
        data = raw if raw is not None else obj.serialize()
        out.append(f"{name} {cmd} {version} {data.hex()}{limits}")

    for name, s in (("zero", b"\0" * 96), ("valid", sig)):
        add(f"clsig_{name}", "clsig", m.msg_clsig(height=2_000_000, blockHash=0x1234, sig=s))
        add(
            f"isdlock_{name}",
            "isdlock",
            m.msg_isdlock(1, [m.COutPoint(0xAB, 1), m.COutPoint(0xCD, 0)], 0xEF, 0x99, s),
        )
        add(f"platformban_{name}", "platformban", m.msg_platformban(0x11, 2_000_000, 0x22, s))
        rec = m.CRecoveredSig()
        rec.llmqType, rec.quorumHash, rec.id, rec.msgHash, rec.sig = 100, 0x33, 0x44, 0x55, s
        add(f"qsigrec_{name}", "qsigrec", rec)

    for version in (1, 2, 3, 4):
        qc = m.CFinalCommitment()
        qc.nVersion, qc.llmqType, qc.quorumHash, qc.quorumIndex = version, 103, 0x66, 1
        qc.signers = [True, False, True, True] * 3
        qc.validMembers = [True] * 12
        qc.quorumPublicKey, qc.quorumVvecHash, qc.quorumSig, qc.membersSig = pk, 0x77, sig, sig
        add(f"qfcommit_v{version}", "qfcommit", qc)

    for n in (0, 1, 3):
        add(f"getqrinfo_{n}", "getqrinfo", m.msg_getqrinfo(list(range(1, n + 1)), 0xAA, n % 2 == 1))

    add("qgetdata", "qgetdata", m.msg_qgetdata(0xBB, 100, 3, 0xCC))
    for error in (0, 3):
        q = m.msg_qdata()
        q.quorum_type, q.quorum_hash, q.data_mask, q.protx_hash, q.error = 100, 0xBB, 3, 0xCC, error
        key = m.CBLSPublicKey()
        key.data = pk
        enc = m.CBLSIESEncryptedSecretKey()
        enc.ephemeral_pubKey = pk
        q.quorum_vvec, q.enc_contributions = [key, key], [enc]
        # The quorum's threshold and valid member count.
        add(f"qdata_error{error}", "qdata", q, limits=" 2 1")

    for version, mn_type in ((1, 0), (2, 0), (2, 1)):
        e = m.CSimplifiedMNListEntry()
        e.nVersion, e.proRegTxHash, e.confirmedHash = version, 0xD1, 0xD2
        e.service = m.CService()
        e.service.ip, e.service.port = "::ffff:192.0.2.1", 9999
        e.pubKeyOperator, e.keyIDVoting, e.isValid = pk, b"\x13" * 20, True
        e.type, e.platformHTTPPort, e.platformNodeID = mn_type, 26657, b"\x14" * 20
        add(f"smlentry_v{version}_t{mn_type}", "smlentry", e)
        if version == 1:
            add("smlentry_unversioned", "smlentry", e, 70227, e.serialize(with_version=False))

    print("# Encoded by Dash Core's test_framework/messages.py at the v24 pin")
    print("# (gen_vectors.py); name cmd stream_version hex [max_a max_b].")
    print("\n".join(out))


if __name__ == "__main__":
    main()
