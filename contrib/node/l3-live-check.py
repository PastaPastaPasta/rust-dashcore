#!/usr/bin/env python3
"""Checks l3_live's two reports (OD-L3-18's live witnesses; BL-672 N3), failing closed.

usage: l3-live-check.py --tag TAG [--head SHA] MNHF_REPORT DIP3_REPORT

Each report must be a JSON object whose dashd tag is TAG and, with --head, whose rdc_head is
SHA. The MNHF report's legs must be exactly mnhf-dup and mnhf-signal, both done. Run by
l3-tier1.sh (at the checkout's head) and the node-l1 nightly (at head.txt's head when it built
the binaries). Exits 1 naming every problem, 2 on bad usage.
"""

import argparse
import json
import sys


def problems(mnhf_path, dip3_path, tag, head):
    found = []
    for path in (mnhf_path, dip3_path):
        try:
            with open(path) as f:
                r = json.load(f)
        except (OSError, ValueError) as e:
            found.append(f"{path}: {e}")
            continue
        if not isinstance(r, dict):
            found.append(f"{path}: not an object")
            continue
        if head is not None and r.get("rdc_head") != head:
            found.append(f"{path}: rdc_head {r.get('rdc_head')!r}, not {head}")
        dashd = r.get("dashd")
        if not isinstance(dashd, dict) or dashd.get("tag") != tag:
            found.append(f"{path}: dashd {dashd!r}, not {tag}")
        if path == mnhf_path:
            legs = r.get("legs")
            if not isinstance(legs, list) or not all(isinstance(leg, dict) for leg in legs):
                found.append(f"{path}: legs {legs!r}, not a list of objects")
                continue
            names = sorted(str(leg.get("name")) for leg in legs)
            if names != ["mnhf-dup", "mnhf-signal"]:
                found.append(f"{path}: legs {names}, not mnhf-dup and mnhf-signal")
            for leg in legs:
                if leg.get("status") != "done":
                    found.append(f"{path}: leg {leg.get('name')!r}: {leg.get('status')!r}")
    return found


def main():
    p = argparse.ArgumentParser(description="Checks l3_live's MNHF and DIP3-parent reports.")
    p.add_argument("--tag", required=True, help="the dashd tag both reports must carry")
    p.add_argument("--head", help="the rdc head both reports must carry")
    p.add_argument("mnhf")
    p.add_argument("dip3")
    a = p.parse_args()
    found = problems(a.mnhf, a.dip3, a.tag, a.head)
    if found:
        print("l3-live-check:", *found, sep="\n  ", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
