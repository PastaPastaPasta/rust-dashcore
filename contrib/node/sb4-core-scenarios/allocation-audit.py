#!/usr/bin/env python3
"""Check that --perturb-heap still reaches the pointers Core orders by address,
and only those (Sol HX1 r3/r4's allocation audit; DEC-175). Rerun it after any
re-pin or harness change to how block index entries are allocated.

Runs the address-sensitive and reproducible scenarios and a reload's ties under
`up`, `down` and two seeds with --audit-index, which prints every block index
entry's address and region offset at each dump and fails the run unless every
entry, and nothing else, was served from the perturbation region (as many live
slots as entries). It fails unless `up` and `down` order every pair of entries
oppositely at every dump, and each seed gives the same offsets in two
processes (R4-3). It records the CBlockIndex and BlockMap node sizes (144 and
192 bytes, alignment 8, at the v24.0 pin), which must fit the region's slot.

usage: allocation-audit.py --bin <dir with sb4_scenarios>
"""

import argparse
import itertools
import json
import os
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
CASES = [
    os.path.join(HERE, "scenarios", "hx1-dump-orders.json"),  # a reload's loaded ties
    *sorted(os.path.join(HERE, d, f) for d in ("address-sensitive", "reproducible")
            for f in os.listdir(os.path.join(HERE, d)) if f.endswith(".json")),
]
MODES = ("up", "down", "17", "29")


def audit(binary, path, mode, tmp_root):
    """{step: {label: region offset}} and the size lines of one --audit-index run."""
    with tempfile.TemporaryDirectory(prefix="sb4-audit-", dir=tmp_root) as tmp:
        proc = subprocess.run([binary, "--perturb-heap", mode, "--audit-index", "run", path],
                              env=dict(os.environ, TMPDIR=tmp), capture_output=True, text=True)
    if proc.returncode != 0:
        sys.exit(f"{path} under {mode}: exit {proc.returncode}\n{proc.stderr[-3000:]}")
    seen, sizes = {}, set()
    for line in proc.stderr.splitlines():
        if line.startswith("HX1_SIZE "):
            f = dict(x.split("=", 1) for x in line.split()[1:])
            if f["entries"] != f["live_slots"]:  # the harness fails the run first
                sys.exit(f"{path} under {mode}: the region served something else: {line}")
            sizes.add(" ".join(f"{k}={f[k]}" for k in ("index", "node", "align", "slot")))
        elif line.startswith("HX1_INDEX "):
            f = dict(x.split("=", 1) for x in line.split()[1:])
            if f["region"] != "1":
                sys.exit(f"{path} under {mode}: an index entry outside the region: {line}")
            seen.setdefault(int(f["step"]), {})[f["label"]] = int(f["offset"])
    if not seen:
        sys.exit(f"{path} under {mode}: no HX1_INDEX lines (built without --audit-index?)")
    return seen, sizes


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True)
    a = ap.parse_args()
    binary = os.path.join(os.path.abspath(a.bin), "sb4_scenarios")
    tmp_root = os.environ.get("TMPDIR")
    summary = {}
    for path in CASES:
        name = os.path.basename(path)[:-5]
        runs = {m: audit(binary, path, m, tmp_root) for m in MODES}
        for m in MODES[2:]:  # R4-3: a seed's layout is a function of the seed alone
            if audit(binary, path, m, tmp_root)[0] != runs[m][0]:
                sys.exit(f"{name}: --perturb-heap {m} places entries differently in another process")
        up, down = runs["up"][0], runs["down"][0]
        if up.keys() != down.keys():
            sys.exit(f"{name}: up and down dumped at different steps")
        pairs = 0
        for step, x in up.items():
            y = down[step]
            if x.keys() != y.keys():
                sys.exit(f"{name} step {step}: up and down hold different entries")
            for p, q in itertools.combinations(sorted(x), 2):
                if (x[p] < x[q]) == (y[p] < y[q]):
                    sys.exit(f"{name} step {step}: {p} and {q} in the same order under up and down")
                pairs += 1
        snapshots = sum(len(v) for seen, _ in runs.values() for v in seen.values())
        sizes = sorted(set().union(*(s for _, s in runs.values())))
        summary[name] = {"index_snapshots": snapshots, "pairs_reversed": pairs, "sizes": sizes}
        print(f"ok   {name}: {snapshots} index snapshots in the region (and nothing else), {pairs} pairs reversed, "
              f"seeds reproduce, {'; '.join(sizes)}")
    print(json.dumps(summary, indent=1))


if __name__ == "__main__":
    main()
