#!/usr/bin/env python3
# Copyright (c) 2026 The Dash Core developers
# Distributed under the MIT software license, see the accompanying
# file COPYING or http://www.opensource.org/licenses/mit-license.php.
"""base-sdk's core_overlay.json5 from `bs8-tool cases` output.

Keeps up to three of the shortest cases for each distinct (command, stream
version, case tag, BLS scheme, Core verdict, write-back, strict flag, qdata
limits). The tag (seeds.rs) names the payload's layout and the field the case
varies, so each commitment version, SML entry version and type, and BLS field
position keeps its representatives. Fails unless the required layouts and
protocol bands are kept, then prints a census to stderr. Payloads over 4 KiB
are written as the corpus RLE (hex and {"b", "n"} runs).

usage: make_corpus.py <cases.json> <judge MANIFEST> <out.json5>
"""

import collections
import json
import sys

# The protocol bands of seeds.rs (BANDS), each with an SML entry, a diff and
# rotation info.
BANDS = (70221, 70225, 70227, 70228, 70229, 70230, 70242)
CURRENT = 70242

HEADER = """\
// Dash Core's reads of the overlay messages, from a judge linked against the
// v24 pin ({pin}): each payload is read with Core's serializer and its
// handler's checks (CDataStream at the stream "version", with
// bls_legacy_scheme set from "legacy"), and written back with Core's
// serializer. "core": "ok" with
// "consumed" (the bytes read), "reenc" where Core's write-back is not the
// bytes read (an eager BLS field re-serialized, a cbTx written as Core writes
// it), "errorCode" and "vvecSize" for qdata, "hash" for the objects Core
// hashes (the hash it keys them by, little-endian); or "class", the
// exception kind.
// "max": for qdata, the threshold and the valid member count the data is read
// against. "strict": "flag" marks a flag byte Core reads as `true` that
// base-sdk refuses (0 or 1 only). Built from rust-dashcore
// contrib/node/dde-probe/bs8 (seeds.rs cases, make_corpus.py selection);
// judge source and binary hashes: {manifest}.
// Payloads over 4 KiB are lists of hex and {{"b": byte, "n": count}} runs.
"""


def rle(hexstr):
    if len(hexstr) <= 8192:
        return hexstr
    data = bytes.fromhex(hexstr)
    out, i, lit = [], 0, bytearray()
    while i < len(data):
        j = i
        while j < len(data) and data[j] == data[i]:
            j += 1
        if j - i >= 64:
            if lit:
                out.append(lit.hex())
                lit = bytearray()
            out.append({"b": f"{data[i]:02x}", "n": j - i})
        else:
            lit += data[i:j]
        i = j
    if lit:
        out.append(lit.hex())
    return out


def layout(r):
    """The payload layout a tag names: its first word for a commitment or an
    SML entry (the entry version), else the tag."""
    tag = r.get("tag", "")
    if r["command"] in ("qfcommit", "smlentry") and tag.startswith("v"):
        return tag.split()[0]
    return tag


def check(cases, kept):
    """Fails unless the kept cases cover what the corpus must cover."""
    have = {(r["command"], r["version"], r.get("tag", "")) for r in kept}
    missing = []
    # Every tag of every command, at every stream version it was seeded at.
    for r in cases:
        key = (r["command"], r["version"], r.get("tag", ""))
        if key not in have:
            missing.append(key)
    commitments = {layout(r) for r in kept if r["command"] == "qfcommit"}
    for v in ("v1", "v2", "v3", "v4"):
        for field in ("quorum_pk", "quorum_sig", "members_sig"):
            if ("qfcommit", CURRENT, f"{v} {field}") not in have:
                missing.append(("qfcommit", CURRENT, f"{v} {field}"))
    if not {"v1", "v2", "v3", "v4"} <= commitments:
        missing.append(("qfcommit", "versions", sorted(commitments)))
    for evo in ("regular", "evo"):
        if ("smlentry", CURRENT, f"v3 {evo}") not in have:
            missing.append(("smlentry", CURRENT, f"v3 {evo}"))
    for cmd in ("smlentry", "mnlistdiff", "qrinfo"):
        bands = {r["version"] for r in kept if r["command"] == cmd}
        for band in BANDS:
            if band not in bands:
                missing.append((cmd, band, "band"))
    if missing:
        raise SystemExit(f"corpus selection misses {sorted(set(map(str, missing)))}")
    census = collections.Counter(
        (r["command"], r["version"], layout(r)) for r in kept
        if r["command"] in ("qfcommit", "smlentry", "mnlistdiff", "qrinfo"))
    for (cmd, version, lay), n in sorted(census.items()):
        print(f"census {cmd} {version} {lay or '-'}: {n}", file=sys.stderr)


def main():
    if len(sys.argv) != 4:
        raise SystemExit(__doc__)
    with open(sys.argv[1], encoding="utf-8") as f:
        cases = json.load(f)
    with open(sys.argv[2], encoding="utf-8") as f:
        manifest = {}
        for line in f:
            words = line.split()
            if words:
                # "source <sha> judge.cpp" keys by the file.
                manifest[words[-1] if words[0] == "source" else words[0]] = words[1]
    groups = collections.defaultdict(list)
    for r in cases:
        k = r["core"]
        key = (r["command"], r["version"], r.get("tag", ""), r["legacy"],
               k["ok"], k.get("class"), "reenc" in k, r.get("strict"),
               tuple(r["max"]))
        groups[key].append(r)
    kept = []
    for rs in groups.values():
        kept += sorted(rs, key=lambda r: len(r["raw"]))[:3]
    kept.sort(key=lambda r: (r["command"], r["name"]))
    check(cases, kept)
    for r in kept:
        r["raw"] = rle(r["raw"])
    pin = f'{manifest["pin"]} {manifest["commit"][:12]}'
    hashes = f'judge.cpp {manifest["judge.cpp"][:16]}, binary {manifest["binary"][:16]}'
    with open(sys.argv[3], "w", encoding="utf-8") as f:
        f.write(HEADER.format(pin=pin, manifest=hashes))
        f.write(json.dumps({"cases": kept}, indent=1))
        f.write("\n")
    print(f"{len(kept)} cases from {len(cases)}", file=sys.stderr)


if __name__ == "__main__":
    main()
