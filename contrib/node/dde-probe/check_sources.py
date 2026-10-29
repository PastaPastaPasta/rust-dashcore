#!/usr/bin/env python3
"""Pins the DDE-SWEEP §4 claims no live probe can reach to Core's source at both tags.

Usage: check_sources.py [DASH_REPO [SWEEP_MD]]
  (defaults ~/workspace/dash and this repository's docs/node-program/DDE-SWEEP.md)
Reads the files with `git show <tag>:<path>`, so the checkout itself is not
touched. Each claim is a list of snippets that must appear, in order, in one
file at one tag, or must not appear. It fails naming the first claim that no
longer holds, so a re-pin shows which sweep rows to re-derive.

It also checks the sweep's own text for the DKG messages: the "Handler reads"
cell of the qcontrib/qcomplaint/qjustify/qpcommit row must list each message
body field by field exactly as Core reads it at both tags, and state the
premature-commitment size those fields give for a 3-member quorum.

The DKG intake checks need a masternode in an active DKG phase and an
MNAuth-verified peer, which the regtest probes cannot set up; the transport
and map claims back the decode rules the live probes cover only in part.
"""
import math
import os
import re
import subprocess
import sys

V23, V24 = "v23.1.8", "v24.0.0-rc.3"

# (tag, path, snippets in order, or None for "the file must not exist", label)
PRESENT = [
    # DKG intake at v23: an empty payload +100 before the prefix read, then
    # the size cap, then a typed decode of a copy whose shape rules are
    # equalities, scored +100; no unrequested check and no whole-payload rule.
    (V23, "src/llmq/dkgsessionmgr.cpp", [
        "if (vRecv.empty()) {\n        return MisbehavingError{100};",
        "vRecv >> llmqType;\n    vRecv >> quorumHash;",
        "MaxDKGMessageSize(msg_type, llmq_params)) {\n        return MisbehavingError{100, \"oversized DKG message\"};",
        "if (!CheckDKGMessageStructure(msg_type, vRecv, llmq_params)) {\n"
        "        return MisbehavingError{100, \"malformed DKG message\"};",
    ], "v23 DKG intake order"),
    (V23, "src/llmq/dkgsessionmgr.cpp", [
        "qc.vvec->size() == threshold",
        "qc.contributions->blobs.size() >= min_size",
        "qc.contributions->blobs.size() <= size",
        "return qc.badMembers.size() == size && qc.complainForMembers.size() == size;",
        "return qj.contributions.size() <= size;",
        "return qc.validMembers.size() == size;",
    ], "v23 DKG intake shapes"),
    # DKG intake at v24: under 33 bytes +100, the size cap, a framing walk
    # that bounds bitsets by the quorum size and needs the whole payload,
    # then the unrequested +10; the size equalities wait for PreVerify.
    (V24, "src/llmq/net_dkg.cpp", [
        "ignoring %s in observer mode",
        "if (vRecv.size() < sizeof(uint8_t) + sizeof(uint256)) {\n"
        "        m_peer_manager->PeerMisbehaving(pfrom.GetId(), 100, \"malformed DKG message\");",
        "PeerMisbehaving(pfrom.GetId(), 100, \"oversized DKG message\");",
        "if (!CheckDKGMessageWireStructure(msg_type, vRecv, llmq_params)) {\n"
        "        m_peer_manager->PeerMisbehaving(pfrom.GetId(), 100, \"malformed DKG message\");",
        "UNREQUESTED_OBJECT_MISBEHAVIOR_SCORE, \"unrequested DKG message\"",
    ], "v24 DKG intake order"),
    (V24, "src/llmq/net_dkg.cpp", [
        "if (bit_count > max_size) {\n        throw std::ios_base::failure(\"dynamic bitset exceeds quorum size\");",
        "if (ReadCompactSize(ds) != threshold) { // vvec",
        "if (blob_count < min_size || blob_count > size) {",
        "return ds.empty();",
        "if (bad_members != complain_for_members) {",
        "return ds.empty();",
        "if (contribution_count > size) {",
        "return ds.empty();",
        "ReadBoundedDynBitset(ds, size); // validMembers",
        "return ds.empty();",
    ], "v24 DKG wire structure"),
    # The v24 walk skips the same fields the serializers read: the IES
    # ephemeral key and IV seed before the blobs, and two signatures after a
    # premature commitment's public key and vvec hash.
    (V24, "src/llmq/net_dkg.cpp", [
        "ds.ignore(CBLSPublicKey::SerSize + 32); // IES ephemeralPubKey + ivSeed",  # spellchecker:disable
        "ReadBoundedDynBitset(ds, size); // validMembers",
        "ds.ignore(CBLSPublicKey::SerSize + 32 + 2 * CBLSSignature::SerSize);",  # spellchecker:disable
    ], "v24 DKG walk: IES fields, two premature-commitment signatures"),
    (V24, "src/llmq/net_dkg.cpp", [
        "ds >> *msg;",
        "if (!ds.empty()) {",
        "if (!CheckDKGMessageStructure(*msg, params)) {",
    ], "v24 DKG worker decode"),
    (V24, "src/llmq/dkgsession.cpp", [
        "if (qc.badMembers.size() != static_cast<size_t>(params.size)) {",
        "if (qc.complainForMembers.size() != static_cast<size_t>(params.size)) {",
        "if (qc.validMembers.size() != static_cast<size_t>(params.size)) {",
    ], "v24 DKG size equalities in PreVerify"),
    (V23, "src/llmq/net_dkg.cpp", None, "no NetDKG at v23"),
    # Transport command checks: V1 accepts 0x20-0x7e, V2 long form 0x20-0x7f.
    *[(tag, "src/protocol.cpp", [
        "if (*p1 == 0) {\n            // Must be all zeros after the first zero",
        "} else if (*p1 < ' ' || *p1 > 0x7E) {",
    ], tag + " V1 IsCommandValid") for tag in (V23, V24)],
    *[(tag, "src/net.cpp", [
        "if (contents[msg_type_len] < ' ' || contents[msg_type_len] > 0x7F) {",
        "if (contents[msg_type_len] != 0) return {};",
    ], tag + " V2 GetMessageType") for tag in (V23, V24)],
    # ExtNetInfo is a std::map: a repeated purpose is read and dropped.
    *[(tag, "src/evo/netinfo.h", ["std::map<NetInfoPurpose, NetInfoList> m_data{};", "s >> m_data;"],
       tag + " ExtNetInfo map") for tag in (V23, V24)],
    *[(tag, "src/serialize.h", ["void UnserializeMap(Stream& is, Map& m)", "mi = m.insert(mi, item);"],
       tag + " map insert keeps the first") for tag in (V23, V24)],
]

ABSENT = [
    (V23, "src/llmq/dkgsessionmgr.cpp", "unrequested", "no unrequested DKG check at v23"),
]


# The sweep row whose "Handler reads" cell lists the DKG message bodies.
DKG_ROW = "| `qcontrib`, `qcomplaint`, `qjustify`, `qpcommit` |"
DKG_CLASSES = {
    "qcontrib": "CDKGContribution",
    "qcomplaint": "CDKGComplaint",
    "qjustify": "CDKGJustification",
    "qpcommit": "CDKGPrematureCommitment",
}
# Serialized sizes of the fixed-width member types, in bytes.
TYPE_SIZE = {"Consensus::LLMQType": 1, "uint256": 32, "CBLSPublicKey": 48, "CBLSSignature": 96}
SIZE_RULE_MEMBERS = 3
# The sweep's statement of that size: a full bitset; v24's walk accepts a shorter one.
SIZE_PHRASE = r"%d-member quorum with a full %d-bit `validMembers` is therefore (\d+) bytes" % (SIZE_RULE_MEMBERS,
                                                                                            SIZE_RULE_MEMBERS)
NUMBER_WORDS = {"one": 1, "two": 2, "three": 3, "four": 4}


def block(text, start):
    """The text from `start` to the end of its brace-balanced body."""
    i = text.index("{", start)
    depth = 0
    for j in range(i, len(text)):
        depth += {"{": 1, "}": -1}.get(text[j], 0)
        if depth == 0:
            return text[start:j + 1]
    raise ValueError("unbalanced braces")


def parenthesized(text, start):
    """The contents of the parentheses opening at or after `start`."""
    i = text.index("(", start)
    depth = 0
    for j in range(i, len(text)):
        depth += {"(": 1, ")": -1}.get(text[j], 0)
        if depth == 0:
            return text[i + 1:j]
    raise ValueError("unbalanced parentheses")


def split_top(args):
    parts, depth, cur = [], 0, ""
    for c in args:
        depth += {"(": 1, ")": -1, "<": 1, ">": -1, "{": 1, "}": -1, "[": 1, "]": -1}.get(c, 0)
        if c == "," and depth == 0:
            parts.append(cur.strip())
            cur = ""
        else:
            cur += c
    return parts + [cur.strip()] if cur.strip() else parts


def class_body(text, name):
    m = re.search(r"\n(class|struct) %s\b[^;{]*\{" % name, text)
    return block(text, m.start() + 1) if m else None


def members(body):
    """Data members declared at the class's own level: name -> type."""
    out = {}
    for line in body.splitlines():
        code = line.split("//")[0].rstrip()
        if not re.match(r"^ {4}\S", code) or "(" in code or not code.endswith(";"):
            continue
        decl = re.sub(r"\{[^}]*\}", "", code).strip().rstrip(";").strip()
        if " " in decl and not decl.startswith(("return", "using", "friend")):
            typ, name = decl.rsplit(" ", 1)
            out[name] = typ.strip()
    return out


def readwrite(body, name):
    """The fields a SERIALIZE_METHODS(name, obj) block reads, `obj.` dropped.

    The block must be one unconditional READWRITE, so a read that depends on
    the stream version, or a second READWRITE, cannot slip past the guard.
    """
    at = body.find("SERIALIZE_METHODS(%s, obj)" % name)
    if at < 0:
        return None
    methods = block(body, at)
    args = parenthesized(methods, methods.index("READWRITE("))
    rest = methods[methods.index("{"):].replace("READWRITE(%s)" % args, "", 1)
    if re.sub(r"[{};\s]", "", rest):
        raise ValueError("%s's SERIALIZE_METHODS is not one unconditional READWRITE" % name)
    return [re.sub(r"\bobj\.", "", f) for f in split_top(args)]


def field_type(field, mem):
    """The declared type of a listed field, by its member name."""
    return mem.get(re.match(r"(?:DYNBITSET\()?(\w+)", field).group(1))


def core_fields(dkg_h, ies_h, cls):
    """Core's read order for one DKG message, nested types expanded."""
    body = class_body(dkg_h, cls)
    if body is None:
        raise ValueError("no class %s in dkgmessages.h" % cls)
    mem = members(body)
    fields = readwrite(body, cls)
    if fields is None:
        # CDKGContribution reads into temporaries, then moves them in.
        unserialize_body = block(body, body.index("inline void Unserialize(Stream& s)"))
        reads = [r.strip() for r in re.findall(r"s >> ([^;]+);", unserialize_body)]
        if "if (" in unserialize_body or not all(re.fullmatch(r"\w+", r) for r in reads):
            raise ValueError("%s::Unserialize reads %s, not plain fields in order" % (cls, reads))
        moved = dict((tmp, field) for field, tmp in
                     re.findall(r"(\w+) = std::make_shared<[^;]*\(std::move\((\w+)\)\);", unserialize_body))
        fields = [moved.get(f, f) for f in reads]
    out = []
    for f in fields:
        typ = mem.get(f, "")
        nested = re.match(r"std::vector<(\w+)>$", typ)
        if "CBLSIESMultiRecipientObjects" in typ:
            objs = class_body(ies_h, "CBLSIESMultiRecipientObjects")
            head = re.search(r"class CBLSIESMultiRecipientObjects\b[^{]*\{", ies_h)
            if (objs is None or head is None or "public CBLSIESMultiRecipientBlobs" not in head.group(0)
                    or re.search(r"SERIALIZE_METHODS|\b(Un)?[Ss]erialize\w*\s*\(", objs)):
                raise ValueError("CBLSIESMultiRecipientObjects no longer reads exactly its Blobs base")
            blobs = readwrite(class_body(ies_h, "CBLSIESMultiRecipientBlobs"), "CBLSIESMultiRecipientBlobs")
            out.append("%s{%s}" % (f, ", ".join(blobs)))
        elif nested and readwrite(body, nested.group(1)):
            out.append("%s[%s]" % (f, ", ".join(readwrite(body, nested.group(1)))))
        else:
            out.append(f)
    return out, mem


def wire_size(fields, mem, n):
    """Bytes the fields take for an n-member quorum, None if one is variable."""
    total = 0
    for f in fields:
        bitset = re.match(r"DYNBITSET\((\w+)\)$", f)
        if bitset and mem.get(bitset.group(1)) == "std::vector<bool>":
            total += (1 if n < 0xfd else 3) + math.ceil(n / 8)
        elif mem.get(f) in TYPE_SIZE:
            total += TYPE_SIZE[mem[f]]
        else:
            return None
    return total


def check_sweep_dkg(repo, sweep_path):
    """The sweep's DKG read shapes against Core at both tags; returns failures."""
    try:
        return sweep_dkg_failures(repo, sweep_path)
    except (OSError, ValueError, AttributeError, TypeError, IndexError) as e:
        print("FAIL  sweep DKG read shapes: %s: %s" % (type(e).__name__, e))
        return 1


def need(repo, tag, path):
    text = show(repo, tag, path)
    if text is None:
        raise ValueError("%s not found at %s in %s" % (path, tag, repo))
    return text


def sweep_dkg_failures(repo, sweep_path):
    failures = 0
    lines = [l for l in open(sweep_path, encoding="utf-8").read().splitlines() if l.startswith(DKG_ROW)]
    if len(lines) != 1:
        print("FAIL  sweep DKG row: %d rows start with %s" % (len(lines), DKG_ROW))
        return 1
    cell = lines[0].strip().strip("|").split("|")[2]
    shapes = {}
    for tag in (V23, V24):
        dkg_h, ies_h = need(repo, tag, "src/llmq/dkgmessages.h"), need(repo, tag, "src/bls/bls_ies.h")
        for cmd, cls in DKG_CLASSES.items():
            shapes.setdefault(cmd, {})[tag] = core_fields(dkg_h, ies_h, cls)
    for cmd in DKG_CLASSES:
        typed = {tag: [(f, field_type(f, mem)) for f in fields] for tag, (fields, mem) in shapes[cmd].items()}
        core24 = shapes[cmd][V24][0]
        if typed[V23] != typed[V24]:
            print("FAIL  %s read shape differs between pins: %s / %s" % (cmd, typed[V23], typed[V24]))
            failures += 1
            continue
        spans = re.findall(r"`%s` `([^`]+)`" % cmd, cell)
        stated = split_top(spans[0]) if len(spans) == 1 else None
        ok = stated == core24
        failures += not ok
        print("%s  sweep %s read shape: %s" % ("ok  " if ok else "FAIL", cmd, ", ".join(core24)))
        if not ok:
            print("      the sweep %s" % ("lists: " + ", ".join(stated) if stated else
                                         "has %d `%s` `fields` spans in its Handler reads cell, not one"
                                         % (len(spans), cmd)))
    # Each body ends in one `sig`, and the premature commitment's signature
    # count, as the prose states them.
    pfields, pmem = shapes["qpcommit"][V24]
    sigs = sum(1 for f in pfields if pmem.get(f) == "CBLSSignature")
    ends = all(fields[-1] == "sig" and fields.count("sig") == 1 and mem.get("sig") == "CBLSSignature"
               for fields, mem in (shapes[cmd][V24] for cmd in DKG_CLASSES))
    count = re.findall(r"carries (\w+) signatures in all", cell)
    ok = ends and "each message ends in one `sig`" in cell and len(count) == 1 and NUMBER_WORDS.get(count[0]) == sigs
    failures += not ok
    print("%s  sweep DKG prose: each body ends in one sig, qpcommit carries %d signatures" %
          ("ok  " if ok else "FAIL", sigs))
    if not ok:
        print("      the sweep states %s signatures; Core's bodies %s in one sig" %
              (count or "no count of", "all end" if ends else "do not all end"))
    # The premature commitment's size rule, from Core's fields at both tags and as stated.
    sizes = {tag: wire_size(*shapes["qpcommit"][tag], SIZE_RULE_MEMBERS) for tag in (V23, V24)}
    spans = re.findall(r"`qpcommit` `([^`]+)`", cell)
    stated_size = wire_size(split_top(spans[0]), pmem, SIZE_RULE_MEMBERS) if len(spans) == 1 else None
    stated_n = re.findall(SIZE_PHRASE, cell)
    size = sizes[V24]
    ok = (size is not None and sizes[V23] == size and len(stated_n) == 1 and int(stated_n[0]) == size == stated_size)
    failures += not ok
    print("%s  sweep qpcommit size for a %d-member quorum: %s bytes" % ("ok  " if ok else "FAIL", SIZE_RULE_MEMBERS, size))
    if not ok:
        print("      the sweep states %s; its field list gives %s; Core's fields give %s at %s and %s at %s" %
              ("%s bytes" % stated_n[0] if len(stated_n) == 1 else "%d sizes" % len(stated_n),
               stated_size if spans else "nothing, there is no list", sizes[V23], V23, size, V24))
    return failures


def show(repo, tag, path):
    r = subprocess.run(["git", "-C", repo, "show", "%s:%s" % (tag, path)], capture_output=True, text=True)
    return r.stdout if r.returncode == 0 else None


def main():
    repo = sys.argv[1] if len(sys.argv) > 1 else os.path.expanduser("~/workspace/dash")
    here = os.path.dirname(os.path.abspath(__file__))
    sweep = sys.argv[2] if len(sys.argv) > 2 else os.path.join(here, "..", "..", "..", "docs", "node-program",
                                                                "DDE-SWEEP.md")
    failures = 0
    for tag, path, snippets, label in PRESENT:
        text = show(repo, tag, path)
        if snippets is None:
            ok = text is None
        else:
            pos, ok = 0, text is not None
            for snippet in snippets if ok else []:
                found = text.find(snippet, pos)
                if found < 0:
                    ok = False
                    print("missing at %s %s: %r" % (tag, path, snippet[:70]))
                    break
                pos = found + len(snippet)
        failures += not ok
        print("%s  %s" % ("ok  " if ok else "FAIL", label))
    for tag, path, needle, label in ABSENT:
        ok = needle not in (show(repo, tag, path) or "").lower()
        failures += not ok
        print("%s  %s" % ("ok  " if ok else "FAIL", label))
    failures += check_sweep_dkg(repo, sweep)
    if failures:
        sys.exit("%d source claim(s) no longer hold" % failures)
    print("all source claims hold at both pins")


if __name__ == "__main__":
    main()
