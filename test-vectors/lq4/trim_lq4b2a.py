#!/usr/bin/env python3
"""Derive dash-llmq's LQ4b-2a clone vector from the LQ4b C0b capture.

Usage: trim_lq4b2a.py <captures/v24> <test-vectors/lq4>

Reads only dkg_v24_simplepose.json5 and dkg_v24_simplepose.scenarios.json, whose one label is
the contribution-clone session; only that session is kept. Every value written is copied from
them: each member's proTxHash and operator secret key, each message's sender and raw bytes in
capture order, the final commitment's raw bytes, and the session's scenario with its cloners'
indexes. Fields the tests rebuild from the raw bytes or the operator keys (vectors, IES blobs,
decrypted shares, hashes, verdicts) are dropped. The effective LLMQ parameters are the run's
(see PROVENANCE.md); the script checks them against the final commitment's bitset sizes.
"""
import json
import sys

SRC, DST = sys.argv[1], sys.argv[2]


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


def clone():
    # `set_dash_llmq_test_params(5, 3)`: `-llmqtest{,instantsend,platform}params=5:3`, size 5 and
    # minSize = threshold = dkgBadVotesThreshold = 3.
    params = {"100": {"size": 5, "min_size": 3, "threshold": 3, "bad_votes": 3}}
    d = load("dkg_v24_simplepose.json5")
    scen = load("dkg_v24_simplepose.scenarios.json")
    assert d["scheme"] == "basic"
    assert [s["scenario"] for s in scen["sessions"]] == ["contribution-clone"]
    sessions = []
    for s in scen["sessions"]:
        key = (s["llmq_type"], s["quorum_hash"])
        [q] = [q for q in d["quorums"] if (q["llmq_type"], q["quorum_hash"]) == key]
        version, llmq_type, signers, valid = final_sizes(q["final"]["raw"])
        assert (version, llmq_type) == (3, q["llmq_type"]), q["quorum_hash"]
        assert signers == valid == params["100"]["size"], (q["quorum_hash"], signers, valid)
        assert all(q["final"][k] for k in ("vvec_hash_ok", "quorum_pk_ok", "quorum_sig_ok", "members_sig_ok"))
        pro_txs = [m["pro_tx"] for m in q["members"]]
        erring = [pro_txs.index(p) for p in s["erring_pro_tx"]]
        assert erring == s["erring"], (erring, s["erring"])
        sessions.append({
            "llmq_type": q["llmq_type"],
            "quorum_hash": q["quorum_hash"],
            "members": [[m["pro_tx"], m["operator_sk"]] for m in q["members"]],
            "contributions": rows(q, "contributions"),
            "complaints": rows(q, "complaints"),
            "justifications": rows(q, "justifications"),
            "premature": rows(q, "premature"),
            "final": q["final"]["raw"],
            "scenario": s["scenario"],
            "erring": erring,
        })
    return {"scheme": "basic", "params": params, "v24": sessions}


def dump(name, header, body):
    text = "// " + header + "\n" + json.dumps(body, separators=(",", ":")) + "\n"
    with open(f"{DST}/{name}", "w") as f:
        f.write(text)
    return len(text)


name = "dkg_lq4b_clone.json5"
header = ("LQ4b-2a labelled feature_llmq_simplepose contribution-clone transcript (v24.0.0), derived "
          "from the LQ4b C0b capture; see PROVENANCE.md.")
print(f"{name} {dump(name, header, clone())}")
