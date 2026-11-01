#!/usr/bin/env python3
"""Derive dash-llmq's LQ4a-2 vectors from the DEC-638 captures.

Usage: trim.py <captures/out> <captures/run-sp> <test-vectors/lq4>

Every value written is copied from the captures except each `premature` entry's bitset, which is
the `validMembers` parsed out of the captured premature commitment's raw bytes, and a fold's
`sent`, the `badCount` and `complaintCount` of the "sending complaint" line its node logged for
that session (null without one): a complaint Core sent is not always in the transcript. Fields the
tests rebuild from `raw` (vvec, ies, sign hashes, verdicts) are dropped.
"""
import collections
import hashlib
import json
import re
import sys

SRC, RUNS, DST = sys.argv[1], sys.argv[2], sys.argv[3]


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


def valid_members(raw_hex):
    """`CDKGPrematureCommitment::validMembers`: type (1), quorumHash (32), proTxHash (32), then
    DYNBITSET (compact size, ceil(n/8) bytes, LSB first)."""
    b = bytes.fromhex(raw_hex)
    n, i = compact_size(b, 65)
    bits = b[i:i + (n + 7) // 8]
    return "".join("1" if bits[k // 8] >> (k % 8) & 1 else "0" for k in range(n))


def bits(flags):
    return "".join("1" if f else "0" for f in flags)


def session(q, keep_shares):
    out = {
        "llmq_type": q["llmq_type"],
        "quorum_hash": q["quorum_hash"],
        "members": [[m["pro_tx"], m["operator_sk"]] for m in q["members"]],
        "contributions": [],
        "complaints": [[c["member"], c["raw"]] for c in q["complaints"]],
        "justifications": [[j["member"], j["raw"]] for j in q["justifications"]],
        "premature": [[p["member"], valid_members(p["raw"])] for p in q["premature"]],
        "final_valid": bits(q["final"]["valid"]),
    }
    for c in q["contributions"]:
        row = [c["member"], c["raw"]]
        if keep_shares:
            row.append([s["share"] for s in c["shares"]])
        out["contributions"].append(row)
    return out


def dump(name, header, body):
    text = "// " + header + "\n" + json.dumps(body, separators=(",", ":")) + "\n"
    with open(f"{DST}/{name}", "w") as f:
        f.write(text)
    return hashlib.sha256(text.encode()).hexdigest(), len(text)


# 12/7: the three labelled sessions, in the scenarios file's order.
scen = load("dkg_12_7.scenarios.json")
d127 = load("dkg_12_7.json5")
assert d127["scheme"] == "basic"
by_hash = {(q["llmq_type"], q["quorum_hash"]): q for q in d127["quorums"]}
mninfo = scen["mninfo"]
s127 = []
for s in scen["sessions"]:
    q = session(by_hash[(s["llmq_type"], s["quorum_hash"])], keep_shares=True)
    q["scenario"] = s["scenario"]
    q["mn"] = [mninfo.index(m[0]) for m in q["members"]]
    s127.append(q)

# simplepose: the fold calls by session and complainer.


def faults(call, pro_tx):
    """conn.rs's ConnSnapshot::faults over a logged fold; checked against the logged
    badConnection below."""
    info = None
    for p in call["peers"]:
        if p["pro_tx"] == "00" * 32 or p["pro_tx"] != pro_tx:
            continue
        if info is None:
            info = {"version": p["version"], "mn": False, "relay": False, "filters": False}
        if p["mn_connection"]:
            info["mn"] = True
            info["relay"] |= p["relay"]
            info["filters"] |= p["services"] & (1 << 6) != 0
    got = []
    if info is None:
        if call["all_connected"]:
            got.append("not_connected")
    elif info["version"] < call["min_proto"]:
        got.append("min_proto")
    elif info["mn"] and not info["relay"]:
        got.append("no_relay")
    elif info["mn"] and not info["filters"]:
        got.append("no_filters")
    return got


SENT = re.compile(r"\[QuorumDKG\(type=(\S+), qIndex=(\d+), h=(\d+), member=\d+\)\] \[llmq-dkg\]\s+"
                  r"sending complaint\. badCount=(\d+), complaintCount=(\d+)$")


def sent_complaints(tag, node):
    """`SendComplaint`'s log line per (type name, qIndex, height) in `node`'s debug.log."""
    out = {}
    with open(f"{RUNS}/ft-{tag}/{node}/regtest/debug.log") as f:
        for line in f:
            m = SENT.search(line.rstrip("\n"))
            if m:
                key = (m[1], int(m[2]), int(m[3]))
                assert key not in out, key
                out[key] = [int(m[4]), int(m[5])]
    return out


def simplepose(tag, pick):
    d = load(f"dkg_simplepose_{tag}.json5")
    calls = load(f"folds_simplepose_{tag}.json")["calls"]
    assert d["scheme"] == "basic"
    logs = {node: sent_complaints(tag, node) for node in {c["node"] for c in calls}}
    folds = collections.defaultdict(dict)
    for c in calls:
        key = (c["llmq_type"], c["base"])
        assert c["me"] not in folds[key], "one fold per (type, base, me)"
        folds[key][c["me"]] = c
    seen, kept, cover = set(), [], collections.Counter()
    for q in d["quorums"]:
        key = (q["llmq_type"], q["quorum_hash"])
        if key in seen:
            continue
        seen.add(key)
        runs = folds.get(key, {})
        kinds = set()
        for call in runs.values():
            for m in call.get("members", []):
                f = faults(call, m["pro_tx"])
                f += [k for k, v in (("outbound", m["outbound_failed"]), ("banned", m["platform_banned"])) if v]
                assert bool(f) == m["bad_connection"], (key, m)
                kinds.update(f)
            kinds.add("all_connected" if call["all_connected"] else "not_all_connected")
        if len(q["contributions"]) < len(q["members"]):
            kinds.add("missing_contribution")
        kinds.add(f"type{q['llmq_type']}")
        kinds.add("complaints" if q["complaints"] else "no_complaints")
        if not pick(kinds, cover):
            continue
        cover.update(kinds)
        s = session(q, keep_shares=False)
        s["folds"] = []
        for i, (pro_tx, _) in enumerate(s["members"]):
            call = runs.get(pro_tx)
            if call is None:
                continue
            sent = logs[call["node"]].get((call["llmq_type_name"], call["quorum_index"], call["height"]))
            for c in q["complaints"]:
                if c["member"] == i:
                    assert sent == [sum(c["bad"]), sum(c["complain_for"])], (key, i, sent)
            s["folds"].append({
                "me": i,
                "sent": sent,
                "pose": call["pose"],
                "all_connected": call["all_connected"],
                "peers": [[p["pro_tx"], p["version"], p["mn_connection"], p["relay"], p["services"]]
                          for p in call.get("peers", [])],
                "members": [[m["pro_tx"], m["outbound_failed"], m["platform_banned"], m["bad_connection"]]
                            for m in call.get("members", [])],
            })
            assert call["min_proto"] == 70242
        kept.append(s)
    return kept, cover


sp23, cover23 = simplepose("spork23", lambda kinds, cover: any(cover[k] == 0 for k in kinds))
sp_no, cover_no = simplepose("nospork23", lambda kinds, cover: any(cover[k] == 0 for k in kinds))

files = [
    ("dkg_12_7.json5", "LQ4a-2 12/7 transcript, derived from the DEC-638 captures; see PROVENANCE.md.",
     {"scheme": "basic", "llmq_test": {"size": 12, "min_size": 7, "threshold": 7, "bad_votes": 7},
      "sessions": s127}),
    ("dkg_simplepose.json5", "LQ4a-2 simplepose transcripts and folds, derived from the DEC-638 captures; see PROVENANCE.md.",
     {"scheme": "basic", "llmq_test": {"size": 5, "min_size": 3, "threshold": 3, "bad_votes": 3},
      "spork23": sp23, "nospork23": sp_no}),
]
for name, header, body in files:
    sha, size = dump(name, header, body)
    print(f"{name} {size} {sha}")
print("spork23", len(sp23), dict(cover23))
print("nospork23", len(sp_no), dict(cover_no))
