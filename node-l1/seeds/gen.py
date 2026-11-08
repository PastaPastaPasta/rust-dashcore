#!/usr/bin/env python3
"""Writes the public seeds of the diff_payload_<type> targets (BS6b; A1 §1.4.1 "seeded per field").

    python3 -I node-l1/seeds/gen.py --regtest dash/tests/data/core_v24_regtest.json \
        --signed <base-sdk>/pkgs/primitives/corpus/sighash.json5 \
        --extra node-l1/seeds/core_cbtx_v1.json \
        --bls <base-sdk>/pkgs/pkc/corpus/bls_wrapper.json5 --out node-l1/seeds

A seed is a flags byte (bit 0: Core's BLS flag is legacy) and a payload. Every payload is one
Dash Core built: the extra payloads of `core_v24_regtest.json`, of the signed transactions in
base-sdk's sign-hash corpus and of `core_cbtx_v1.json`. For each supported payload type and
version (the commitment version for a quorum commitment) the first payload is a seed under both
flags. Then each BLS field of it takes one sample of base-sdk's `bls_wrapper.json5` (Core-probed
G1 keys or G2 signatures) per class label and pair of outcomes under the legacy and basic flags,
under both flags where Core reads the field under the global flag (CbTx and asset-unlock
signatures) and under the basic flag elsewhere. Nothing here judges a seed: the oracle does when
the target runs. File names are the SHA-1 of the contents, as libFuzzer names corpus files.

The run fails when a supported type and version has no payload, when a BLS field cannot be
located, or when a (label, outcome pair) of the source corpus is left without a seed in some
field. `COVERAGE.json` records the samples and each field's base seed and seeds, which
`node-l1/tests/l1_payload.rs` checks byte for byte.
"""
import argparse
import hashlib
import json
import os
import re
import shutil

PK, SIG = 48, 96
GLOBAL_FIELD_TYPES = {5, 9}
# The supported (type, version) matrix; for a quorum commitment, (payload version, commitment
# version).
SUPPORTED = sorted(
    [(t, (v,)) for t in (1, 2, 3, 4) for v in (1, 2, 3)]
    + [(5, (v,)) for v in (1, 2, 3, 4)]
    + [(6, (1, v)) for v in (1, 2, 3, 4)]
    + [(7, (1,)), (8, (1,)), (8, (2,)), (9, (1,)), (9, (2,)), (10, (1,)), (11, (1,)), (12, (1,))]
)


def load_json(path):
    with open(path) as f:
        return json.load(f)


def load_json5(path):
    with open(path) as f:
        text = f.read()
    text = re.sub(r"^\s*//.*$", "", text, flags=re.MULTILINE)
    return json.loads(re.sub(r",(\s*[}\]])", r"\1", text))


def compact(b, i):
    n = b[i]
    if n < 0xFD:
        return n, i + 1
    width = {0xFD: 2, 0xFE: 4, 0xFF: 8}[n]
    return int.from_bytes(b[i + 1:i + 1 + width], "little"), i + 1 + width


def extra_payload(raw):
    """The type and extra payload of a version-3 special transaction."""
    b = bytes.fromhex(raw)
    i = 4
    n, i = compact(b, i)
    for _ in range(n):
        i += 36
        size, i = compact(b, i)
        i += size + 4
    n, i = compact(b, i)
    for _ in range(n):
        i += 8
        size, i = compact(b, i)
        i += size
    i += 4
    size, i = compact(b, i)
    return int.from_bytes(b[2:4], "little"), b[i:i + size]


def shape(tx_type, payload):
    """The version that selects the payload's layout and BLS schemes."""
    version = int.from_bytes(payload[:2], "little") if tx_type not in (7, 8, 9) else payload[0]
    if tx_type == 6:
        return version, int.from_bytes(payload[6:8], "little")
    return (version,)


def fields(tx_type, payload, operator_key):
    """(name, offset, length) of each BLS field; fails when an expected field is not found."""
    n = len(payload)
    version = shape(tx_type, payload)[0]
    if tx_type in (1, 3, 12):
        offset = {3: 36, 12: 34}.get(tx_type)
        if offset is None:
            offset = payload.find(bytes.fromhex(operator_key)) if operator_key else -1
        if offset < 0:
            raise SystemExit(f"type {tx_type} v{version}: pubKeyOperator not found")
        return [("pubKeyOperator", offset, PK)]
    if tx_type in (2, 4, 7, 9):
        return [("sig", n - SIG, SIG)]
    if tx_type == 5 and version >= 3:
        return [("bestCLSignature", n - 8 - (32 if version >= 4 else 0) - SIG, SIG)]
    if tx_type == 6:
        if n < PK + 2 * SIG:
            raise SystemExit(f"type 6 v{version}: too short for its BLS fields")
        return [("quorumPublicKey", n - 272, PK), ("quorumSig", n - 192, SIG),
                ("membersSig", n - 96, SIG)]
    return []


def samples(bls, kind):
    """One sample per class label and (legacy, basic) outcome pair, in corpus order. An outcome
    is its kind: T (throws), I (the invalid object) or V (a valid object)."""
    out = {}
    for entry in bls[kind]["eager"]:
        out.setdefault((entry["class"], entry["L"][0], entry["B"][0]), bytes.fromhex(entry["in"]))
    return out


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--regtest", required=True)
    p.add_argument("--signed", required=True)
    p.add_argument("--extra", required=True)
    p.add_argument("--bls", required=True)
    p.add_argument("--out", required=True)
    args = p.parse_args()

    bases = {}  # (type, shape) -> (payload, operator key hex)
    for tx in load_json(args.regtest)["transactions"]:
        tx_type, core = tx["shape"][0], tx["core"]
        payload = bytes.fromhex(core["extraPayload"])
        key = next((v.get("pubKeyOperator") for v in core.values() if isinstance(v, dict)), None)
        bases.setdefault((tx_type, shape(tx_type, payload)), (payload, key))
    for entry in load_json5(args.signed)["sighash"].values():
        tx_type, payload = extra_payload(entry["raw"])
        bases.setdefault((tx_type, shape(tx_type, payload)), (payload, None))
    for entry in load_json(args.extra)["payloads"]:
        payload = bytes.fromhex(entry["extraPayload"])
        bases.setdefault((entry["tx_type"], shape(entry["tx_type"], payload)), (payload, None))
    missing = [b for b in SUPPORTED if b not in bases]
    if missing:
        raise SystemExit(f"no Core payload for {missing}")

    bls = load_json5(args.bls)
    pool = {PK: samples(bls, "pk"), SIG: samples(bls, "sig")}
    seeds, coverage = {}, []
    for tx_type, version in SUPPORTED:
        payload, key = bases[(tx_type, version)]
        out = seeds.setdefault(tx_type, set())
        out.update(bytes([flag]) + payload for flag in (0, 1))
        flags = (0, 1) if tx_type in GLOBAL_FIELD_TYPES else (0,)
        for name, offset, length in fields(tx_type, payload, key):
            for flag in flags:
                cells = []
                for (label, legacy, basic), sample in sorted(pool[length].items()):
                    data = bytes([flag]) + payload[:offset] + sample + payload[offset + length:]
                    out.add(data)
                    cells.append({"label": label, "legacy": legacy, "basic": basic,
                                  "seed": hashlib.sha1(data).hexdigest()})
                # Floor: the field holds every (label, outcome pair) of the source corpus.
                kind = "pk" if length == PK else "sig"
                want = {(e["class"], e["L"][0], e["B"][0]) for e in bls[kind]["eager"]}
                if {(c["label"], c["legacy"], c["basic"]) for c in cells} != want:
                    raise SystemExit(f"type {tx_type} {name}: outcome pairs without a seed")
                base = hashlib.sha1(bytes([flag]) + payload).hexdigest()
                coverage.append({"type": tx_type, "version": list(version), "field": name,
                                 "flag": "legacy" if flag else "basic", "base": base, "seeds": cells})

    for tx_type, files in sorted(seeds.items()):
        d = os.path.join(args.out, f"diff_payload_{tx_type}")
        shutil.rmtree(d, ignore_errors=True)
        os.makedirs(d)
        for data in files:
            with open(os.path.join(d, hashlib.sha1(data).hexdigest()), "wb") as f:
                f.write(data)
        print(f"diff_payload_{tx_type}: {len(files)} seeds")
    source = {kind: sorted([*k, v.hex()] for k, v in pool[width].items())
              for kind, width in (("pk", PK), ("sig", SIG))}
    with open(os.path.join(args.out, "COVERAGE.json"), "w") as f:
        json.dump({"supported": [[t, list(v)] for t, v in SUPPORTED], "source": source,
                   "fields": coverage}, f, indent=1, sort_keys=True)
        f.write("\n")


if __name__ == "__main__":
    main()
