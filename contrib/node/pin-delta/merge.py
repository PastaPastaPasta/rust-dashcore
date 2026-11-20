#!/usr/bin/env python3
"""Merge PIN-DELTA auditor packets into one classified index (node-program O17).

Each auditor writes `<packet>.md` with a "Delta rows" table in the format of
AUDIT-BRIEF.md. This script parses every packet, applies the integrator's
decisions file, and prints the consolidated index as markdown tables plus
counts, so the report's index is generated rather than hand-copied.

    python3 -I contrib/node/pin-delta/merge.py --packets /work/scratch/<slug>/pin-delta/out \\
        --decisions designs/O17-pin-delta/decisions.json > index.md

decisions.json:
    {"duplicates": {"SIG-06": "MPIX-16", ...},             # dropped row -> kept row
     "keying":     {"EVO-06": ["profile", "why"], ...},     # final keying override
     "home":       {"MPIX-19": "UngatedRules.time_source", ...},  # home of a profile row
     "titles":     {"MPIX-02": "`MIN_STANDARD_TX_SIZE` 83 → 65", ...},
     "add_consumers": {"CONS-10": "O10", ...},             # e.g. a reclassified row's oracle task
     "deviation":  {"DKG-04": ["H", "O13c", "why no honest peer or client sees it"], ...},
     "duplicate_notes": {"EVO-09": "keying v24-all → profile: ...", ...}}

A dropped duplicate's PRs and consumers are folded into the row it merges
into. With --decisions, every distinct `profile` row needs a home, and every
distinct `<new>-all`/`<old>-all` row needs a deviation entry: the policy that
allows it, the verification task that records it (or "none"), which is also
added to its consumers, and the written unreachability or unobservability
argument. `add_consumers` appends tasks to a kept row's consumers. Without
--decisions the auditor's proposed field is shown. Rows keyed anything but
`profile` have no home. Any malformed row, unknown keying or class, malformed
decision entry, or decision naming an unknown row (or, for `add_consumers`, a
dropped duplicate) is an error (exit 1), never a silent skip.
"""

import argparse
import collections
import json
import os
import re
import sys

ROW_RE = re.compile(r"^\|\s*([A-Z][A-Z0-9]*-\d+)\s*\|")
SPLIT_RE = re.compile(r"(?<!\\)\|")
HEADING_RE = re.compile(r"^(#+)\s")
CLASSES = ("CON", "P2P", "MN", "RPC", "INT")
COLUMNS = 10


def fail(msg):
    sys.exit(f"error: {msg}")


def cells(line):
    return [c.strip() for c in SPLIT_RE.split(line.strip())][1:-1]


def esc(text):
    """Escape the table-cell separators that are not escaped yet."""
    return SPLIT_RE.sub(r"\\|", text)


def short_title(text, limit=110):
    """Bold lead-in if the auditor wrote one, else the first clause."""
    m = re.match(r"\*\*(.+?)\*\*", text)
    t = m.group(1) if m else re.split(r"(?<=[a-z0-9)`])\. |[:;] v\d+| v\d+:|<br>", text, maxsplit=1)[0]
    t = re.sub(r"<br>.*", "", t).strip().rstrip(".:")
    return t if len(t) <= limit else t[: limit - 1].rsplit(" ", 1)[0] + "…"


def merge_lists(a, b):
    """Comma-separated union, keeping a's order. An item whose `#N` references
    all appear already adds nothing."""
    seen = [x.strip() for x in a.split(",") if x.strip()]
    for x in (x.strip() for x in b.split(",")):
        refs = re.findall(r"#\d+", x)
        if not x or x in seen or (refs and all(r in ", ".join(seen) for r in refs)):
            continue
        seen.append(x)
    return ", ".join(seen)


def keying_of(raw, keyings, where):
    word = raw.strip("`* ").lower().split()[0] if raw.strip("`* ") else ""
    word = keyings["aliases"].get(word, word)
    if word not in keyings["all"]:
        fail(f"{where}: unknown keying {raw!r} (expected one of {', '.join(keyings['all'])})")
    return word


def parse_packets(directory, keyings):
    rows = collections.OrderedDict()
    for name in sorted(os.listdir(directory)):
        if not name.endswith(".md"):
            continue
        level = None
        with open(os.path.join(directory, name), encoding="utf-8") as f:
            for n, line in enumerate(f, 1):
                h = HEADING_RE.match(line)
                if h:
                    if re.search(r"delta rows", line, re.I):
                        level = len(h.group(1))
                    elif level is not None and len(h.group(1)) <= level:
                        level = None
                    continue
                if level is None or not line.startswith("|"):
                    continue
                c = cells(line)
                if not c or c[0].lower() == "id" or set("".join(c)) <= set("-: "):
                    continue  # header or separator
                if not ROW_RE.match(line):
                    fail(f"{name}:{n}: table row without a valid id ({c[0]!r})")
                if len(c) != COLUMNS:
                    fail(f"{name}:{n}: row {c[0]} has {len(c)} cells, expected {COLUMNS} (escape | as \\|)")
                rid, diff, cls, keying, both, field, consumers, prs, evidence, a1 = c
                if rid in rows:
                    fail(f"{name}:{n}: row {rid} already defined in {rows[rid]['packet']}.md")
                primary = re.split(r"[\s+(]", cls.strip("`* "), maxsplit=1)[0]
                if primary not in CLASSES:
                    fail(f"{name}:{n}: row {rid} has unknown class {cls!r}")
                rows[rid] = dict(id=rid, packet=name[:-3], diff=diff, cls=cls, primary=primary,
                                 keying=keying_of(keying, keyings, f"{name}:{n}"), keying_raw=keying, both=both,
                                 field=field, consumers=consumers, prs=prs, evidence=evidence, a1=a1)
    if not rows:
        fail(f"no delta rows found in {directory}")
    return rows


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--packets", required=True, help="directory of auditor packet .md files")
    ap.add_argument("--decisions", help="integrator decisions (JSON)")
    ap.add_argument("--json", help="also write the merged rows as JSON here")
    ap.add_argument("--old-name", default="v23", help="label of the old pin in keyings (default v23)")
    ap.add_argument("--new-name", default="v24", help="label of the new pin in keyings (default v24)")
    args = ap.parse_args()

    new_all, old_all = f"{args.new_name}-all", f"{args.old_name}-all"
    keyings = {"all": ("profile", "gated", "params", new_all, old_all, "none"),
               "aliases": {"new-all": new_all, "old-all": old_all}}
    rows = parse_packets(args.packets, keyings)
    dec = {}
    if args.decisions:
        with open(args.decisions, encoding="utf-8") as f:
            dec = json.load(f)
    dups = dec.get("duplicates", {})
    homes = dec.get("home", {})
    titles = dec.get("titles", {})
    deviations = dec.get("deviation", {})
    dup_notes = dec.get("duplicate_notes", {})
    extra = dec.get("add_consumers", {})
    for section, ids in (("duplicates", list(dups) + list(dups.values())), ("keying", dec.get("keying", {})),
                         ("home", homes), ("titles", titles), ("deviation", deviations),
                         ("duplicate_notes", dup_notes), ("add_consumers", extra)):
        unknown = [rid for rid in ids if rid not in rows]
        if unknown:
            fail(f"decisions {section} names unknown rows: {', '.join(unknown)}")
    for dropped, kept in dups.items():
        if kept in dups:
            fail(f"duplicate map chains: {dropped} -> {kept} -> {dups[kept]}")
    for rid, (keying, why) in dec.get("keying", {}).items():
        rows[rid]["keying"] = keying_of(keying, keyings, f"decisions keying {rid}")
        rows[rid]["keying_note"] = why
    for rid, row in rows.items():
        row["title"] = titles.get(rid) or short_title(row["diff"])
        row["home"] = (homes.get(rid) or row["field"]) if row["keying"] == "profile" else "—"
        row["dup_of"] = dups.get(rid)
    for dropped, kept in dups.items():
        rows[kept]["prs"] = merge_lists(rows[kept]["prs"], rows[dropped]["prs"])
        rows[kept]["consumers"] = merge_lists(rows[kept]["consumers"], rows[dropped]["consumers"])
    for rid, tasks in extra.items():
        if rid in dups:
            fail(f"add_consumers names {rid}, which is merged into {dups[rid]}; name the kept row")
        if not isinstance(tasks, str) or not tasks.strip():
            fail(f"add_consumers entry for {rid} must be a non-empty comma-separated string")
        rows[rid]["consumers"] = merge_lists(rows[rid]["consumers"], tasks)
    for rid, entry in deviations.items():
        if not (isinstance(entry, list) and len(entry) == 3
                and all(isinstance(x, str) and x.strip() for x in entry)):
            fail(f"deviation entry for {rid} needs [policy, verification list, argument], all non-empty strings")
        rows[rid]["deviation"] = tuple(entry)
        if entry[1] != "none":
            rows[rid]["consumers"] = merge_lists(rows[rid]["consumers"], entry[1])

    kept = [r for r in rows.values() if not r["dup_of"]]
    homeless = [r["id"] for r in kept if r["keying"] == "profile" and r["id"] not in homes]
    if args.decisions and homeless:
        fail(f"profile rows without a home in the decisions file: {', '.join(homeless)}")
    deviating = [r for r in kept if r["keying"] in (new_all, old_all)]
    unlisted = [r["id"] for r in deviating if "deviation" not in r]
    if args.decisions and unlisted:
        fail(f"{new_all}/{old_all} rows without a deviation entry in the decisions file: {', '.join(unlisted)}")
    stale = [rid for rid in deviations if rows[rid]["keying"] not in (new_all, old_all) or rows[rid]["dup_of"]]
    if stale:
        fail(f"deviation entries for rows that are not distinct {new_all}/{old_all} rows: {', '.join(stale)}")
    counts = collections.Counter((r["primary"], r["keying"]) for r in kept)
    used = [k for k in keyings["all"] if any(k == kk for _, kk in counts)]

    out = sys.stdout
    out.write(f"Rows: {len(rows)} written by auditors, {len(rows) - len(kept)} merged as duplicates, "
              f"{len(kept)} distinct.\n\n")
    out.write("| Class | " + " | ".join(used) + " | total |\n|---|" + "---|" * (len(used) + 1) + "\n")
    for c in CLASSES:
        n = [counts[(c, k)] for k in used]
        if sum(n):
            out.write(f"| {c} | " + " | ".join(str(x) for x in n) + f" | {sum(n)} |\n")
    tot = [sum(counts[(c, k)] for c in CLASSES) for k in used]
    out.write("| **total** | " + " | ".join(f"**{x}**" for x in tot) + f" | **{sum(tot)}** |\n\n")

    by_packet = collections.OrderedDict()
    for r in kept:
        by_packet.setdefault(r["packet"], []).append(r)
    for packet, prow in by_packet.items():
        out.write(f"\n#### {packet}\n\n| id | Behaviour | Class | Keying | Home / field | Consumers | PRs |\n"
                  "|---|---|---|---|---|---|---|\n")
        for r in prow:
            note = f" ({r['keying_note']})" if r.get("keying_note") else ""
            if "deviation" in r:
                note += f" [{r['deviation'][0]} → {r['deviation'][1]}]"
            out.write(f"| {r['id']} | {r['title']} | {r['cls']} | {r['keying']}{note} | {r['home']} | "
                      f"{r['consumers']} | {r['prs']} |\n")
    if deviating and deviations:
        out.write("\n#### Declared deviations\n\n| Row | Behaviour | Policy | Verified by | Why it is unreachable or unobservable |\n"
                  "|---|---|---|---|---|\n")
        for r in sorted(deviating, key=lambda r: (r["deviation"][0], r["id"])):
            policy, task, why = r["deviation"]
            out.write(f"| {r['id']} | {esc(r['title'])} | {policy} | {task} | {esc(why)} |\n")
    if dups:
        out.write("\n#### Merged duplicates\n\n| row | kept as | note |\n|---|---|---|\n")
        for dropped, keep in sorted(dups.items()):
            out.write(f"| {dropped} | {keep} | {dup_notes.get(dropped, '')} |\n")
    if args.json:
        with open(args.json, "w", encoding="utf-8") as f:
            json.dump(list(rows.values()), f, indent=1)


if __name__ == "__main__":
    main()
