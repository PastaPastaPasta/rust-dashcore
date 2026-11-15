#!/usr/bin/env python3
"""Generate the ReleaseProfile spec's per-field and per-task tables (node-program O17).

Reads the rows `merge.py --json` writes and the task table of PLAN.md, and
prints the two generated tables of `O17-release-profile.md`: §6 (one row per
profile field: rows, Core call sites at both pins, the tasks that read it and
the other tasks' roles) and §10 (one row per task).

    python3 -I -B contrib/node/pin-delta/merge.py ... --json rows.json > index.md
    python3 -I -B contrib/node/pin-delta/spec_tables.py --rows rows.json \\
        --plan node-program/PLAN.md --fields fields.md --tasks tasks.md

Roles: R reads the field (its crate can depend on the field's home crate);
P provides both variants or receives the chosen value as a parameter (a pure
crate, or one below the home); T consumes a generated per-release table
(AR1/AR2); V verifies (O*, PL1, RL*). HOME_CRATE and READERS follow the
crate graph of A1 §2.2; update them when a group or a crate edge changes.
"""

import argparse
import collections
import json
import re
import sys

HOME_CRATE = {
    "UngatedRules": "dash-consensus", "ChainstateRules": "dashd-chainstate", "Identity": "dashd-p2p",
    "P2pRules": "dashd-p2p", "PolicyRules": "dashd-mempool", "MiningRules": "dashd-mining",
    "SporkRules": "dashd-masternode", "SyncRules": "dashd-masternode", "LlmqRules": "dashd-quorums",
    "LockRules": "dashd-locks", "GovernanceRules": "dashd-governance", "CoinJoinRules": "dashd-coinjoin",
    "IndexRules": "dashd-indexes", "WalletRules": "dashd-wallet", "InitRules": "dashd-node",
    "RpcRules": "dashd-rpc", "CliRules": "rdash-cli",
}
GROUP_ORDER = list(HOME_CRATE)
READERS = {  # crates that can depend on each home crate (prefix match against a task's Crates cell)
    "dash-consensus": ["dash-consensus", "dash-evo", "dashd-"],
    "dashd-chainstate": ["dashd-chainstate", "dashd-mempool", "dashd-mining", "dashd-p2p", "dashd-masternode",
                         "dashd-quorums", "dashd-locks", "dashd-governance", "dashd-coinjoin", "dashd-indexes",
                         "dashd-wallet", "dashd-rpc", "dashd-node"],
    "dashd-mempool": ["dashd-mempool", "dashd-mining", "dashd-p2p", "dashd-locks", "dashd-coinjoin", "dashd-wallet",
                      "dashd-quorums", "dashd-masternode", "dashd-governance", "dashd-rpc", "dashd-node"],
    "dashd-p2p": ["dashd-p2p", "dashd-masternode", "dashd-quorums", "dashd-locks", "dashd-coinjoin",
                  "dashd-governance", "dashd-rpc", "dashd-node"],
    "dashd-mining": ["dashd-mining", "dashd-rpc", "dashd-node"],
    "dashd-masternode": ["dashd-masternode", "dashd-rpc", "dashd-node"],
    "dashd-quorums": ["dashd-quorums", "dashd-locks", "dashd-rpc", "dashd-node"],
    "dashd-locks": ["dashd-locks", "dashd-rpc", "dashd-node"],
    "dashd-governance": ["dashd-governance", "dashd-mining", "dashd-rpc", "dashd-node"],
    "dashd-coinjoin": ["dashd-coinjoin", "dashd-rpc", "dashd-node"],
    "dashd-indexes": ["dashd-indexes", "dashd-governance", "dashd-rpc", "dashd-node"],
    "dashd-wallet": ["dashd-wallet", "rdash-wallet", "dashd-rpc", "dashd-node"],
    "dashd-node": ["dashd-node", "rdashd"],
    "dashd-rpc": ["dashd-rpc", "dashd-node"],
    "rdash-cli": ["rdash-cli"],
}
PLAN_ROW_RE = re.compile(r"^\| ([A-Z]{1,3}[0-9]+[a-z]?(?:-t)?) \|")
SPLIT_RE = re.compile(r"(?<!\\)\|")
GENERATED_HOMES = ("RpcSurface", "ArgsTable", "dashd-wallet")  # checked against AR1/AR2's tables instead


def fail(msg):
    sys.exit(f"error: {msg}")


def plan_crates(path):
    """Task id -> its Crates cell, from every task table of PLAN.md (column 4)."""
    crates = {}
    with open(path, encoding="utf-8") as f:
        for line in f:
            m = PLAN_ROW_RE.match(line)
            if m:
                cells = SPLIT_RE.split(line)
                if len(cells) > 4:
                    crates.setdefault(m.group(1), cells[4].strip())
    if not crates:
        fail(f"no task rows found in {path}")
    return crates


def role(task, group, crates):
    if re.match(r"^(O\d|PL1|RL\d)", task):
        return "V"
    if task in ("AR1", "AR2") or (task == "AR3" and group != "CliRules"):
        return "T"
    home = HOME_CRATE[group]
    return "R" if any(r in crates.get(task, "") for r in READERS[home]) else "P"


def split_evidence(ev):
    """(v23 call sites, v24 call sites) from an auditor's evidence cell."""
    ev = re.sub(r"\s+", " ", ev.replace("<br>", " "))
    m = re.search(r"v24(?:\.0\.0-rc\.3)?\s*:", ev)
    if not m:
        return ev[:120], ""
    old = re.sub(r"^\s*v23(?:\.1\.8)?\s*:\s*", "", ev[:m.start()]).strip(" /;—-")
    new = ev[m.end():].strip(" /;—-")

    def cut(s, n=115):
        s = s.replace("src/", "")
        if len(s) <= n:
            return s
        s = s[:n]
        i = max(s.rfind(";"), s.rfind(", "))
        return (s[:i] if i > 40 else s) + " …"
    return cut(old), cut(new)


def union_evidence(rows):
    """Call sites of every row of a field, first row first, without repeats."""
    if len(rows) == 1:
        return split_evidence(rows[0]["evidence"])
    parts = [split_evidence(r["evidence"]) for r in rows]
    out = []
    for k in (0, 1):
        seen = []
        for p in parts:
            x = p[k].rstrip(" ….")
            if x and x not in seen:
                seen.append(x)
        joined = "; ".join(seen)
        if len(joined) > 260:
            joined = joined[:joined.rfind("; ", 0, 260)] + " …"
        out.append(joined)
    return out[0], out[1]


def task_key(t):
    m = re.match(r"([A-Z]+)(\d+)?(.*)", t)
    return (m.group(1), int(m.group(2) or 0), m.group(3))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--rows", required=True, help="merged rows from merge.py --json")
    ap.add_argument("--plan", required=True, help="node-program PLAN.md")
    ap.add_argument("--fields", required=True, help="write the per-field table (spec §6) here")
    ap.add_argument("--tasks", required=True, help="write the per-task table (spec §10) here")
    args = ap.parse_args()

    with open(args.rows, encoding="utf-8") as f:
        rows = json.load(f)
    crates = plan_crates(args.plan)

    fields = collections.OrderedDict()
    for r in rows:
        if r["dup_of"] or r["keying"] != "profile":
            continue
        for home in r["home"].split(" + "):
            home = re.sub(r"\s*\(.*$", "", home).strip()
            if home.startswith(GENERATED_HOMES):
                continue
            if home.split(".")[0] not in HOME_CRATE:
                fail(f"{r['id']}: home {home!r} names no known group (add it to HOME_CRATE)")
            fields.setdefault(home, []).append(r)

    by_task = collections.defaultdict(lambda: collections.defaultdict(list))
    field_lines = []
    for field in sorted(fields, key=lambda h: (GROUP_ORDER.index(h.split(".")[0]), h)):
        group = field.split(".")[0]
        frows = fields[field]
        tasks = []
        for r in frows:
            for c in re.split(r",\s*", re.sub(r"\(.*?\)", "", r["consumers"])):
                c = c.strip()
                if c and c not in tasks:
                    tasks.append(c)
        roles = {t: role(t, group, crates) for t in tasks}
        for t, ro in roles.items():
            by_task[t][ro].append(field)
        old, new = union_evidence(frows)
        readers = ", ".join(t for t in tasks if roles[t] == "R") or "—"
        others = ", ".join(f"{t} ({roles[t]})" for t in tasks if roles[t] != "R") or "—"
        field_lines.append(f"| `{field}` | {', '.join(r['id'] for r in frows)} | {old} | {new} | {readers} | {others} |")

    def fmt(xs):
        return ", ".join(f"`{x}`" for x in xs) or "—"

    task_lines = [f"| {t} | {fmt(by_task[t]['R'])} | {fmt(by_task[t]['P'])} | {fmt(by_task[t]['T'] + by_task[t]['V'])} |"
                  for t in sorted(by_task, key=task_key)]
    unknown = sorted(t for t in by_task if t not in crates and role(t, "UngatedRules", crates) in ("R", "P"))
    if unknown:
        print(f"warning: consumers with no PLAN row (role guessed as P): {', '.join(unknown)}", file=sys.stderr)

    with open(args.fields, "w", encoding="utf-8") as f:
        f.write("\n".join(field_lines) + "\n")
    with open(args.tasks, "w", encoding="utf-8") as f:
        f.write("\n".join(task_lines) + "\n")
    print(f"{len(field_lines)} fields; {len(task_lines)} tasks")


if __name__ == "__main__":
    main()
