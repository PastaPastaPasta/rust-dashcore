#!/usr/bin/env python3
"""Derive dash-llmq's LQ4b-1 vectors from the LQ4b C0 captures.

Usage: trim_lq4b1.py <captures/out> <test-vectors/lq4>

Reads only dkg_rc{4,5}_dkgerrors.json5 with its .scenarios.json, dkg_rc{4,5}_singlenode.json5 and
dkg_rc{4,5}_rotation.json5. Every value written is copied from them: each member's proTxHash and
operator secret key, each message's sender and raw bytes in capture order, the final commitment's
raw bytes, a dkgerrors session's scenario label (with its erring member's index), and a
single-member session's base height and V19 state. Fields the tests rebuild from the raw bytes or
the operator keys (vectors, IES blobs, decrypted shares, hashes, verdicts) are dropped. The
effective LLMQ parameters are the runs' (see PROVENANCE.md); the script checks each against the
final commitments' bitset sizes.
"""
import json
import sys

SRC, DST = sys.argv[1], sys.argv[2]
TAGS = ("rc4", "rc5")


def load(name):
    with open(f"{SRC}/{name}") as f:
        text = f.read()
    return json.loads("\n".join(l for l in text.split("\n") if not l.lstrip().startswith("//")))


def compact_size(b, i):
    n = b[i]
    if n < 0xFD:
        return n, i + 1
    width = {0xFD: 2, 0xFE: 4, 0xFF: 8}[n]
    return int.from_bytes(b[i + 1:i + 1 + width], "little"), i + 1 + width


def final_sizes(raw_hex):
    """`CFinalCommitment`: nVersion (2), llmqType (1), quorumHash (32), quorumIndex (2) for
    versions 2 and 4, then signers and validMembers (DYNBITSET each)."""
    b = bytes.fromhex(raw_hex)
    version = int.from_bytes(b[0:2], "little")
    i = 35 + (2 if version in (2, 4) else 0)
    signers, i = compact_size(b, i)
    i += (signers + 7) // 8
    valid, _ = compact_size(b, i)
    return version, b[2], signers, valid


def rows(q, kind):
    return [[m["member"], m["raw"]] for m in q[kind]]


def session(q, params):
    version, llmq_type, signers, valid = final_sizes(q["final"]["raw"])
    assert llmq_type == q["llmq_type"], q["quorum_hash"]
    assert signers == valid == params[str(llmq_type)]["size"], (q["quorum_hash"], signers, valid)
    assert all(q["final"][k] for k in ("vvec_hash_ok", "quorum_pk_ok", "quorum_sig_ok", "members_sig_ok"))
    return {
        "llmq_type": q["llmq_type"],
        "quorum_hash": q["quorum_hash"],
        "members": [[m["pro_tx"], m["operator_sk"]] for m in q["members"]],
        "contributions": rows(q, "contributions"),
        "complaints": rows(q, "complaints"),
        "justifications": rows(q, "justifications"),
        "premature": rows(q, "premature"),
        "final": q["final"]["raw"],
    }


def llmq(size, threshold, min_size=None, bad_votes=None):
    """`-llmqtest*params=S:T` sets size S and minSize = threshold = dkgBadVotesThreshold = T."""
    return {
        "size": size,
        "min_size": threshold if min_size is None else min_size,
        "threshold": threshold,
        "bad_votes": threshold if bad_votes is None else bad_votes,
    }


def dkgerrors():
    params = {"100": llmq(3, 2)}
    out = {"scheme": "basic", "params": params}
    for tag in TAGS:
        d = load(f"dkg_{tag}_dkgerrors.json5")
        scen = load(f"dkg_{tag}_dkgerrors.scenarios.json")
        assert d["scheme"] == "basic"
        by_hash = {(q["llmq_type"], q["quorum_hash"]): q for q in d["quorums"]}
        assert len(by_hash) == len(d["quorums"]) == len(scen["sessions"]) == 8
        sessions = []
        for s in scen["sessions"]:
            q = session(by_hash[(s["llmq_type"], s["quorum_hash"])], params)
            q["scenario"] = s["scenario"]
            pro_txs = [m[0] for m in q["members"]]
            q["erring"] = [pro_txs.index(p) for p in s["erring_pro_tx"]]
            sessions.append(q)
        out[tag] = sessions
    return out


def rotation():
    # `-llmqtest{,instantsend,platform}params=4:4`; llmq_test_dip0024 keeps Core's values
    # (size 4, minSize 4, threshold 3, dkgBadVotesThreshold 2; llmq/params.h:224-248).
    params = {"100": llmq(4, 4), "103": llmq(4, 3, min_size=4, bad_votes=2)}
    out = {"scheme": "basic", "params": params}
    for tag in TAGS:
        d = load(f"dkg_{tag}_rotation.json5")
        assert d["scheme"] == "basic"
        assert len({(q["llmq_type"], q["quorum_hash"]) for q in d["quorums"]}) == len(d["quorums"])
        out[tag] = [session(q, params) for q in d["quorums"]]
    return out


def singlenode():
    # `-llmqtest{,instantsend,platform}params=1:1`.
    params = {t: llmq(1, 1) for t in ("100", "104", "106")}
    out = {"scheme": "basic", "params": params}
    for tag in TAGS:
        d = load(f"dkg_{tag}_singlenode.json5")
        assert d["scheme"] == "basic"
        sessions = []
        for q in d["quorums"]:
            sm = q["single_member"]
            version, llmq_type, signers, valid = final_sizes(q["final"]["raw"])
            assert (llmq_type, signers, valid) == (q["llmq_type"], 1, 1)
            assert version == sm["version"] and sm["quorum_index"] == 0
            assert sm["source"] == "qfcommit and mined"
            [m] = q["members"]
            sessions.append({
                "llmq_type": q["llmq_type"],
                "quorum_hash": q["quorum_hash"],
                "member": [m["pro_tx"], m["operator_sk"]],
                "base_height": sm["base_height"],
                "v19_after_base": sm["v19_active_after_base"],
                "final": q["final"]["raw"],
            })
        out[tag] = sessions
    return out


def dump(name, header, body):
    text = "// " + header + "\n" + json.dumps(body, separators=(",", ":")) + "\n"
    with open(f"{DST}/{name}", "w") as f:
        f.write(text)
    return len(text)


files = [
    ("dkg_lq4b_dkgerrors.json5",
     "LQ4b-1 labelled feature_llmq_dkgerrors transcripts (rc.4, rc.5), derived from the LQ4b C0 captures; see PROVENANCE.md.",
     dkgerrors()),
    ("dkg_lq4b_rotation.json5",
     "LQ4b-1 feature_llmq_rotation transcripts (rc.4, rc.5), derived from the LQ4b C0 captures; see PROVENANCE.md.",
     rotation()),
    ("dkg_lq4b_singlenode.json5",
     "LQ4b-1 feature_llmq_singlenode single-member sessions (rc.4, rc.5), derived from the LQ4b C0 captures; see PROVENANCE.md.",
     singlenode()),
]
for name, header, body in files:
    print(f"{name} {dump(name, header, body)}")
