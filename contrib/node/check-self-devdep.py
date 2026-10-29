#!/usr/bin/env python3
"""Fail on a self dev-dependency that turns on test features in a crate with benches.

PROCESS.md "benches measure production only": cargo unifies a crate's dev-dependency on itself
into every dev target, so `pkg = { path = ".", features = ["test-util"] }` builds the benches PF0
measures with test hooks on. Found three times on 10-11 (dash-chain, dashd-chainstate, dash-evo).
Features that only enable production code (dashcore's signer, ...) are not test features.
"""
import json
import re
import subprocess
import sys

TESTISH = re.compile(r"test|mock|crash|planted|fault|alloc-tag|fuzz", re.I)
# Known violators, each with the task that removes it; delete the entry when it lands.
ALLOW = {
    ("dashd-chainstate", "alloc-tag"): "CH2b commit 6 (ch2b-crash)",
    ("dashd-chainstate", "planted-bugs"): "CH2b commit 6 (ch2b-crash)",
    ("dash-evo", "test-utils"): "SB7 m1",
    ("key-wallet", "test-utils"): "BL-104",
    ("dash-spv", "test-utils"): "BL-104",
    ("dash-dml", "test-utils"): "BL-105 (EV2a follow-up; additive-only, checked 10-11)",
}

meta = json.loads(subprocess.run(["cargo", "metadata", "--format-version=1", "--no-deps"],
                                 capture_output=True, text=True, check=True).stdout)
bad = 0
for p in meta["packages"]:
    if not any("bench" in t["kind"] for t in p["targets"]):
        continue
    for d in p["dependencies"]:
        if d["name"] != p["name"] or d.get("kind") != "dev":
            continue
        for f in d.get("features") or []:
            if not TESTISH.search(f):
                continue
            if (p["name"], f) in ALLOW:
                print(f"self-devdep: {p['name']} turns on {f!r} in its benches (allowed until {ALLOW[p['name'], f]})")
            else:
                print(f"self-devdep: {p['name']} turns on test feature {f!r} in its benches: drop the self "
                      "dev-dep, use required-features, add a bench canary (PROCESS.md)")
                bad += 1
sys.exit(1 if bad else 0)
