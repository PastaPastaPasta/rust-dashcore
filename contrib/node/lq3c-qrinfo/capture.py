#!/usr/bin/env python3
"""Builds the LQ3c qrinfo fixture from a copy of feature_llmq_rotation's node0 datadir.

Usage:
  capture.py build STATE.json CLI...      branches the copy (dashd running)
  capture.py capture STATE.json P2P_PORT DEBUG_LOG EVODB.txt REFUSALS.jsonl CLI... > FIXTURE.json
                                          after a clean stop, evodump and a restart

`build` reads the chain over RPC and branches it. c is the last DIP0024 cycle base, m the height
both of its commitments were mined at, C = 24 (llmq_test_dip0024's dkgInterval), T the tip.
SPORK_19 is off for the whole fork phase (feature_llmq_rotation.py v24:238-248 turns it off around
its own invalidations): a reconnected block would otherwise adopt its CbTx ChainLock and the next
branch would be `bad-chainlock`. Each branch is `invalidateblock(F+1)`, `generatetodescriptor(k)`,
`reconsiderblock(F+1)`; its tip must then be a `valid-fork` and the active tip T again:
  - A: F = c-4, up to m: the answered stale S at c+2, the stale S at m (at the mining height, so
    Core reads cycle c's rows, whose bases are on the active chain), a stale request base;
  - B: F = c-12, up to c+2: the request block's cycle work block c-8 is above the fork;
  - L: F = T-2, more than a cycle longer than the active chain, then invalidated at its first
    block, so its tip's first target cycle is above T (Core's `Cannot find block`).
If T < m+1 the active chain is extended first, staying below c+24. STATE.json records c, m, T, the
active chain and the branches.

`capture` records the block index as a tree (every `getchaintips` branch: hash, height, prev, data
on disk, coinbase and txids of every block), the active deployments' heights, the evodb rows LQ3c
reads (`dmn_*`, `q_mc`, `q_mcih`, `q_mcihi`, `llmq_S`; EVODB.txt is evodump's output on the
stopped copy) and the node's reply to each request at each band (getqrinfo.py). Refusals go to
REFUSALS.jsonl, each on its own connection at 70242 and 70235. Every case states the outcome it
expects per band (answered, or the refused log line's exact error text); a mismatch fails the
capture.
"""

import json
import os
import random
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
BANDS = [70221, 70225, 70227, 70228, 70229, 70230, 70235, 70236, 70242]
REFUSAL_BANDS = [70242, 70235]
C = 24
LLMQ = "llmq_test_dip0024"
SPORK_OFF = 4070908800
MAGIC = "fcc1b7dc"
EVODB_PREFIXES = (b"q_mc", b"q_mcih", b"q_mcihi", b"llmq_S")


def cli(args, *rpc):
    return subprocess.run(args + [str(a) for a in rpc], check=True, capture_output=True, text=True).stdout.strip()


def rpc_json(args, *rpc):
    return json.loads(cli(args, *rpc))


def check(cond, what):
    if not cond:
        sys.exit(f"capture check failed: {what}")


def last_cycle(args):
    """c and m: the newest DIP0024 cycle base and the height its commitments were mined at."""
    quorums = [q for entry in rpc_json(args, "quorum", "listextended")[LLMQ] for q in entry.values()]
    c = max(q["creationHeight"] - q["quorumIndex"] for q in quorums)
    mined = {
        q["quorumIndex"]: rpc_json(args, "getblockheader", q["minedBlockHash"])["height"]
        for q in quorums
        if q["creationHeight"] - q["quorumIndex"] == c
    }
    check(sorted(mined) == [0, 1], f"both indices of cycle {c}: {mined}")
    check(mined[0] == mined[1], f"one mining height for cycle {c}: {mined}")
    return c, mined[0]


def chaintip_status(args, tip):
    for t in rpc_json(args, "getchaintips"):
        if t["hash"] == tip:
            return t["status"]
    return None


def branch(args, desc, active, f, k, invalidate=False):
    """k blocks on top of active[f]; the active chain is restored. Returns the branch's hashes."""
    cli(args, "invalidateblock", active[f + 1])
    check(cli(args, "getbestblockhash") == active[f], f"tip after invalidating {f + 1}")
    blocks = rpc_json(args, "generatetodescriptor", k, desc)
    check(len(blocks) == k and cli(args, "getbestblockhash") == blocks[-1], f"branch at {f} mined")
    cli(args, "reconsiderblock", active[f + 1])
    if invalidate:
        # longer than the active chain: it stays active until its first block is invalid
        cli(args, "invalidateblock", blocks[0])
    check(cli(args, "getbestblockhash") == active[-1], f"tip back to T after the branch at {f}")
    want = "invalid" if invalidate else "valid-fork"
    check(chaintip_status(args, blocks[-1]) == want, f"branch at {f}: tip status {want}")
    return blocks


def build(state_path, args):
    c, m = last_cycle(args)
    desc = rpc_json(args, "getdescriptorinfo", "raw(51)")["descriptor"]
    tip = int(cli(args, "getblockcount"))
    check(c + 2 <= tip < c + C, f"tip {tip} in cycle {c}")
    if tip < m + 1:
        cli(args, "generatetodescriptor", m + 1 - tip, desc)
        tip = m + 1
    check(tip < c + C, "the active chain stays below c+24")
    active = [cli(args, "getblockhash", h) for h in range(tip + 1)]
    cli(args, "sporkupdate", "SPORK_19_CHAINLOCKS_ENABLED", SPORK_OFF)
    branches = {
        "A": {"fork": c - 4, "blocks": branch(args, desc, active, c - 4, m - (c - 4))},
        "B": {"fork": c - 12, "blocks": branch(args, desc, active, c - 12, (c + 2) - (c - 12))},
    }
    top = (tip // C + 2) * C + 2  # the first target cycle cs(S) - C is above T
    branches["L"] = {"fork": tip - 2, "blocks": branch(args, desc, active, tip - 2, top - (tip - 2), invalidate=True)}
    cli(args, "sporkupdate", "SPORK_19_CHAINLOCKS_ENABLED", 0)
    state = {"c": c, "m": m, "tip": tip, "active": active, "branches": branches}
    with open(state_path, "w") as f:
        json.dump(state, f, indent=1)


def block_tree(args, active):
    """Every block of every `getchaintips` branch: [hash, height, prev, have_data, tip status, coinbase, txids]."""
    on_active = set(active)
    rows = [[h, n, active[n - 1] if n else None, True, "active"] for n, h in enumerate(active)]
    seen = set(on_active)
    for tip in rpc_json(args, "getchaintips"):
        if tip["status"] == "active":
            continue
        walk, h = [], tip["hash"]
        while h not in seen:
            header = rpc_json(args, "getblockheader", h)
            walk.append([h, header["height"], header["previousblockhash"], None, tip["status"]])
            seen.add(h)
            h = header["previousblockhash"]
        rows.extend(reversed(walk))
    for row in rows:
        try:
            block = rpc_json(args, "getblock", row[0], 2)
            row[3] = True
            row += [block["tx"][0]["hex"], [tx["txid"] for tx in block["tx"]]]
        except subprocess.CalledProcessError:
            check(row[0] not in on_active, f"active block {row[0]} has its data")
            row[3] = False
            row += [None, []]
    return rows


def evodb_rows(path):
    rows = []
    for line in open(path):
        key, value = line.split()
        raw = bytes.fromhex(key)
        prefix = raw[1 : 1 + raw[0]]
        if prefix.startswith(b"dmn_") or prefix in EVODB_PREFIXES:
            rows.append([key, value])
    return rows


def refused_text(entry):
    """The `error=` part of the refusal's Misbehaving line, and its score change."""
    for line in entry["log"]:
        if "Misbehaving" in line and "getquorumrotationinfo failed" in line:
            score = line.split("(", 1)[1].split(")", 1)[0]
            return line.split(" error=", 1)[1], score
    return None, None


def base_sets(h, tip, s):
    """The request base sets for a request block at height s, by name."""
    cs = s - s % C
    return {
        "none": [],
        "one": [h(cs - 2 * C - 5)],
        "three-unsorted": [h(cs - 2 * C - 4), h(cs - 4 * C - 5), h(cs - C - 4)],
        "duplicate-pair": [h(cs - 2 * C - 4), h(cs - 2 * C - 4)],
        "work-block": [h(cs - C - 8)],
        "above-work": [h(min(tip, cs - 5))],
    }


def requests(state):
    """The answered matrix (shared connection per band) and the refusals (fresh connections)."""
    c, m, tip, active = state["c"], state["m"], state["tip"], state["active"]
    br = state["branches"]

    def h(n):
        return active[n]

    def stale(name, height):
        b = br[name]
        return b["blocks"][height - b["fork"] - 1]

    def matrix(label, block, s, sets):
        return [
            {
                "case": f"{label}/bases={b}",
                "bases": base_sets(h, tip, s)[b],
                "block": block,
                "extra": extra,
                "proto": proto,
            }
            for b in sets
            for extra in (False, True)
            for proto in BANDS
        ]

    every = list(base_sets(h, tip, tip))
    answered = []
    for label, s in (("S=tip", tip), ("S=cycle-base", c), ("S=before-mining", c + 2), ("S=mining", m)):
        answered += matrix(label, h(s), s, every)
    # byte-identical to S=before-mining's replies with the same bases
    answered += matrix("stale-S-answered", stale("A", c + 2), c + 2, ("none", "one"))

    unknown = "%064x" % random.Random("lq3c-unknown").getrandbits(256)
    s_long = br["L"]["blocks"][-1]
    long_top = br["L"]["fork"] + len(br["L"]["blocks"])
    work_long = stale("L", long_top - long_top % C - 8)
    s_work, work_b, base_b = stale("B", c + 2), stale("B", c - 8), h(c - 9)

    def nis(base, block):
        return f"block {base} and {block} are not in the same chain"

    # (case, bases, block, {band: Core's error text}). The H diff's base on the efficient path is the
    # newest work block inserted before it (c-32 for a request at c+2); on the legacy path the newest
    # request base, genesis without one.
    refusals = [
        ("unknown-base", [unknown], h(tip), {"*": f"block {unknown} not found"}),
        ("unknown-S", [], unknown, {"*": "block not found"}),
        ("low-S-cycle-base", [], h(10), {"*": "Cannot find work block"}),
        ("low-S-first-target", [], h(30), {"*": "Cannot find work block"}),
        ("no-snapshot", [], h(100), {"*": "Cannot find quorum snapshot"}),
        ("long-branch-S", [], s_long, {70242: "Cannot find block", 70235: nis(h(0), work_long)}),
        ("stale-S-above-mining", [], stale("A", m), {"*": ""}),
        ("stale-S-work-above-fork", [], s_work, {70242: nis(h(c - 32), work_b), 70235: nis(h(0), work_b)}),
        ("stale-base", [stale("A", c + 2)], h(tip), {"*": f"block {stale('A', c + 2)} is not in the active chain"}),
        ("stale-target-picks-base-above-fork", [base_b], s_work, {"*": nis(base_b, work_b)}),
    ]
    out = []
    for case, bases, block, expect in refusals:
        for proto in REFUSAL_BANDS:
            want = expect.get(proto, expect.get("*"))
            request = {"case": case, "bases": bases, "block": block, "extra": False, "proto": proto}
            out.append({**request, "fresh": True, "expect": want})
    return answered, out


def run_getqrinfo(port, log, reqs, path):
    with open(path, "w") as f:
        for r in reqs:
            f.write(json.dumps(r) + "\n")
    out = subprocess.run(
        ["python3", "-I", f"{HERE}/getqrinfo.py", f"127.0.0.1:{port}", MAGIC, log, path],
        check=True, capture_output=True, text=True, timeout=7200,
    ).stdout
    return [json.loads(line) for line in out.splitlines() if line.strip()]


def capture(state_path, port, log, evodb, refusals_path, args):
    state = json.load(open(state_path))
    check(cli(args, "getbestblockhash") == state["active"][-1], "tip is T")
    answered, refusals = requests(state)
    work = os.path.dirname(os.path.abspath(state_path))

    got = run_getqrinfo(port, log, answered, f"{work}/answered.req.jsonl")
    check(len(got) == len(answered) and all(r["answered"] for r in got), "every matrix request answered")
    by_key = {(r["case"], r["extra"], r["proto"]): r["payload"] for r in got}
    for (case, extra, proto), payload in by_key.items():
        if case.startswith("stale-S-answered/"):
            sibling = case.replace("stale-S-answered/", "S=before-mining/")
            check(payload == by_key[(sibling, extra, proto)], f"{case} {proto}: equals its active sibling's reply")

    outcomes = run_getqrinfo(port, log, refusals, f"{work}/refusals.req.jsonl")
    with open(refusals_path, "w") as f:
        for r in outcomes:
            text, score = refused_text(r)
            check(not r["answered"] and text is not None, f"{r['case']} {r['proto']}: a scored refusal")
            check(text == r["expect"], f"{r['case']} {r['proto']}: {text!r} vs {r['expect']!r}")
            check(score.endswith("-> 1"), f"{r['case']} {r['proto']}: score {score}")
            r.pop("fresh")
            f.write(json.dumps(r, separators=(",", ":")) + "\n")

    deployments = rpc_json(args, "getdeploymentinfo")["deployments"]
    json.dump(
        {
            "provenance": (
                "Dash Core v24.0.0 functional test feature_llmq_rotation node0, branched over RPC; evodb rows via "
                "LevelDB's C API on the stopped copy; replies to P2P getqrinfo per protocol version from v24.0.0 "
                "dashd on that datadir (contrib/node/lq3c-qrinfo)"
            ),
            "llmq_type": 103,
            "dkg_interval": C,
            "c": state["c"],
            "m": state["m"],
            "tip": state["tip"],
            "forks": {name: b["fork"] for name, b in state["branches"].items()},
            "deployments": {name: d["height"] for name, d in deployments.items() if d.get("active")},
            "blocks": block_tree(args, state["active"]),
            "evodb": evodb_rows(evodb),
            "replies": [[r["case"], r["bases"], r["block"], r["extra"], r["proto"], r["payload"]] for r in got],
        },
        sys.stdout,
        separators=(",", ":"),
    )


def main():
    op = sys.argv[1] if len(sys.argv) > 1 else ""
    if op == "build":
        build(sys.argv[2], sys.argv[3:])
    elif op == "capture":
        state, port, log, evodb, refusals = sys.argv[2:7]
        capture(state, port, log, evodb, refusals, sys.argv[7:])
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
