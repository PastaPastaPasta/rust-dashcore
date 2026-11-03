#!/usr/bin/env python3
# Copyright (c) 2026 The Dash Core developers
# Distributed under the MIT software license, see the accompanying
# file COPYING or http://www.opensource.org/licenses/mit-license.php.
"""base-sdk's core_overlay_ii.json5 (BS9a: DKG and signature share
messages) or core_overlay_iii.json5 (BS9b: CoinJoin and governance messages)
from `bs9-tool cases` output.

Keeps up to two of the shortest cases for each distinct (command, case tag, BLS
scheme, quorum parameters, Core verdict, write-back). The tag (seeds.rs) names
the payload's layout and the field or bound the case varies; a BLS field's case
tag also names the corpus sample, which base-sdk reads as bytes and the
differential checks one by one, so those group by field instead. Fails unless
every tag, both sides of each DKG message's size cap and a refusal of each
bitset are kept (a "padding" class for the signature share bitsets, any
"framing" refusal for the DKG ones, whose padding tags the per-tag check
keeps), then prints a census to stderr. Payloads over 4 KiB are written as the
corpus RLE (hex and {"b", "n"} runs). The BS9b selection also keys by stream
version and checks the sides of its own bounds (`dsa`'s flags by version, the
pool counts, the vote signature sizes, the bloom filter's data, `dsq`'s ready
byte).

usage: make_corpus.py <ii|iii> <cases.json> <judge MANIFEST> <out.json5>
"""

import collections
import json
import sys

HEADER = """\
// Dash Core's reads of the DKG and signature share messages, from a judge
// linked against the v24 pin ({pin}): each payload goes through its
// handler's checks and read (CDataStream at protocol 70242, with
// bls_legacy_scheme set from "legacy"). A DKG message is checked as
// net_dkg.cpp does, against "quorum" (size, minimum size, threshold): the
// length of its type and quorum hash and MaxDKGMessageSize ("oversized"),
// the wire walk ("framing"), then the worker's whole read. "core": "ok" with
// "consumed" (the bytes read), "reenc" where Core's write-back is not the
// bytes read (an eager BLS field re-serialized, or an AUTOBITSET rewritten
// in its shorter form), "hash" (a DKG message's
// inventory hash) and "signHash" (the sign hash Core computes, one per
// object, little-endian, concatenated); or "class", the exception kind.
// Built from rust-dashcore contrib/node/dde-probe/bs9 (seeds.rs cases,
// make_corpus.py selection); judge source and binary hashes: {manifest}.
// Payloads over 4 KiB are lists of hex and {{"b": byte, "n": count}} runs.
"""

HEADER_III = """\
// Dash Core's reads of the CoinJoin and governance messages, from a judge
// linked against the v24 pin ({pin}), mainnet selected:
// each payload is read as its handler reads it (CDataStream at the case's
// "version", the peer's common version; "dss" with
// UnserializeVectorWithMaxSize at the pool's count). "core": "ok" with "consumed" (the bytes read), "reenc"
// where Core's write-back is not the bytes read (a boolean read from a byte
// other than 0 or 1), "hash" (GetHash(): a queue's, a governance object's
// or vote's; a mixed transaction's txid) and "signHash"
// (GetSignatureHash()), little-endian; or "class", the exception kind.
// Built from rust-dashcore contrib/node/dde-probe/bs9 (seeds.rs cases,
// make_corpus.py selection); judge source and binary hashes: {manifest}.
// Payloads over 4 KiB are lists of hex and {{"b": byte, "n": count}} runs.
"""

DKG_AND_SIG_SHARES = ("qcontrib", "qcomplaint", "qjustify", "qpcommit", "qsigsesann",
                      "qsigsinv", "qgetsigs", "qbsigs", "qsigshare")
COINJOIN_AND_GOV = ("dsa", "dsi", "dsf", "dss", "dsc", "dssu", "dstx", "dsq", "govobj",
                    "govobjvote", "govsync")


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


# The first word of a BLS field's case tag (seeds.rs); the rest names the
# bls_wrapper.json5 sample.
BLS_FIELDS = ("vvec", "eph", "pk", "sig", "qsig", "sk")
# Samples kept as their own tag: Core reads the scalar r under the basic
# scheme and zeroes it under the legacy one, which grouping by verdict and
# write-back would keep for one scheme only.
BLS_KEPT = ("sk r",)


def group(tag):
    """The tag a case is selected by: a BLS field's, without its sample."""
    field = tag.split()[0]
    return field if field in BLS_FIELDS and tag not in BLS_KEPT else tag


def check(cases, kept):
    """Fails unless the kept cases cover what the corpus must cover."""
    have = {(r["command"], group(r["tag"]), r["legacy"], r["core"].get("class")) for r in kept}
    missing = sorted({(r["command"], group(r["tag"]), r["legacy"], r["core"].get("class")) for r in cases} - have,
                     key=str)
    verdicts = collections.Counter(
        (r["command"], r["core"].get("class", "ok")) for r in kept)
    # Every DKG command's cap: at it, Core reads or walks the payload; one
    # byte past it, the cap refuses.
    caps = {(r["command"], r["tag"], r["core"].get("class", "ok")) for r in kept if " cap " in r["tag"]}
    for cmd in ("qcontrib", "qcomplaint", "qjustify", "qpcommit"):
        if not any(c == cmd and t.endswith("+1") for c, t, _ in caps):
            missing.append((cmd, "cap"))
    for cmd, tag, verdict in caps:
        if (verdict == "oversized") != tag.endswith("+1"):
            missing.append((cmd, tag, verdict))
    for cmd in ("qcomplaint", "qpcommit", "qsigsinv", "qgetsigs"):
        want = "padding" if cmd.startswith("qs") or cmd == "qgetsigs" else "framing"
        if not verdicts[(cmd, want)]:
            missing.append((cmd, want))
    report(missing + without_ok(verdicts, DKG_AND_SIG_SHARES), verdicts)


# The sides of BS9b's bounds and stages: (command, tag, version) and Core's
# verdict, "ok" or the refusal's class.
BOUNDS_III = {
    # dsa's flags: read at 70241 and later, not before.
    ("dsa", "normal flags Some(3)", 70241): "ok",
    ("dsa", "normal flags None", 70241): "eof",
    ("dsa", "normal flags None", 70240): "ok",
    # The pool: dsi's read throws past it, dss's count gets a reply.
    ("dsi", "ins 180", 70242): "ok",
    ("dsi", "ins 181", 70242): "length_limit",
    ("dsi", "outs 180", 70242): "ok",
    ("dsi", "outs 181", 70242): "length_limit",
    ("dss", "ins 180", 70242): "ok",
    ("dss", "ins 181", 70242): "err_maximum",
    ("dss", "count 181 no ins", 70242): "err_maximum",
    ("dss", "count 180 no ins", 70242): "eof",
    ("govobjvote", "sig 64", 70242): "vote_sig_size",
    ("govobjvote", "sig 65", 70242): "ok",
    ("govobjvote", "sig 96", 70242): "ok",
    ("govobjvote", "sig 97", 70242): "length_limit",
    # govsync: the hash logged apart from the filter, scored.
    ("govsync", "hash short", 70242): "short_hash",
    ("govsync", "hash only", 70242): "filter",
    ("govsync", "data 36000", 70242): "ok",
    ("govsync", "data 36001", 70242): "filter",
    ("govsync", "hash funcs 50", 70242): "ok",
    ("govsync", "hash funcs 51", 70242): "filter",
    ("dsq", "ready 2", 70242): "ok",
}


def check_iii(cases, kept):
    """Fails unless the kept CoinJoin and governance cases cover every tag
    and version with its verdict and both sides of each bound."""
    def keys(rs):
        return {(r["command"], r["tag"], r["version"], r["core"].get("class")) for r in rs}
    missing = sorted(keys(cases) - keys(kept), key=str)
    verdicts = collections.Counter((r["command"], r["core"].get("class", "ok")) for r in kept)
    verdict = {(r["command"], r["tag"], r["version"]): r["core"].get("class", "ok") for r in kept}
    missing += [(k, v) for k, v in BOUNDS_III.items() if verdict.get(k) != v]
    if not any(r["command"] == "dsq" and r["tag"] == "ready 2" and "reenc" in r["core"] for r in kept):
        missing.append(("dsq", "ready 2", "reenc"))
    report(missing + without_ok(verdicts, COINJOIN_AND_GOV), verdicts)


def without_ok(verdicts, commands):
    """The commands with no case Core reads."""
    return [(cmd, "ok") for cmd in commands if not verdicts[(cmd, "ok")]]


def report(missing, verdicts):
    if missing:
        raise SystemExit(f"corpus selection misses {missing}")
    for (cmd, verdict), n in sorted(verdicts.items()):
        print(f"census {cmd} {verdict}: {n}", file=sys.stderr)


def main():
    if len(sys.argv) != 5 or sys.argv[1] not in ("ii", "iii"):
        raise SystemExit(__doc__)
    iii = sys.argv[1] == "iii"
    commands = COINJOIN_AND_GOV if iii else DKG_AND_SIG_SHARES
    with open(sys.argv[2], encoding="utf-8") as f:
        cases = [r for r in json.load(f) if r["command"] in commands]
    if not iii:
        # Every BS9a case is read at the pin's version (core_overlay_ii's
        # format has no version).
        for r in cases:
            assert r.pop("version") == 70242, r["name"]
    with open(sys.argv[3], encoding="utf-8") as f:
        manifest = {}
        for line in f:
            words = line.split()
            if words:
                # "source <sha> judge.cpp" keys by the file.
                manifest[words[-1] if words[0] == "source" else words[0]] = words[1]
    groups = collections.defaultdict(list)
    for r in cases:
        k = r["core"]
        # BS9b tags name no BLS sample ("sig 65" is a size).
        key = (r["command"], r["tag"] if iii else group(r["tag"]), r["legacy"], tuple(r["quorum"]),
               r.get("version"), k["ok"], k.get("class"), "reenc" in k)
        groups[key].append(r)
    kept = []
    for rs in groups.values():
        kept += sorted(rs, key=lambda r: len(r["raw"]))[:2]
    kept.sort(key=lambda r: (r["command"], r["name"]))
    if iii:
        for r in kept:
            # No BLS field or quorum in these messages.
            del r["legacy"], r["quorum"]
        check_iii(cases, kept)
    else:
        check(cases, kept)
    for r in kept:
        r["raw"] = rle(r["raw"])
    pin = f'{manifest["pin"]} {manifest["commit"][:12]}'
    hashes = f'judge.cpp {manifest["judge.cpp"][:16]}, binary {manifest["binary"][:16]}'
    with open(sys.argv[4], "w", encoding="utf-8") as f:
        f.write((HEADER_III if iii else HEADER).format(pin=pin, manifest=hashes))
        f.write(json.dumps({"cases": kept}, indent=1))
        f.write("\n")
    print(f"{len(kept)} cases from {len(cases)}", file=sys.stderr)


if __name__ == "__main__":
    main()
