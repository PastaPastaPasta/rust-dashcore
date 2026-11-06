#!/usr/bin/env python3
"""PF0 acceptance runs: the gate on throwaway commits built over a clean base (default: HEAD).

    PERFGATE_ROOT=/work/scratch/<slug>/perfgate python3 contrib/node/perfgate/tests/acceptance.py \\
        [--base REV] [--scenario NAME ...] [--out DIR]

Every scenario is real code built and measured by `run gate`; the commits are made with git plumbing and are
on no branch. First an anchor is recorded at the base (unless one exists), and each scenario's base is passed
as `--integration-ref`, so the full-selection runs are authoritative. The expectations are the review's
(reviews/PF0-r1-opus.md):

    planted       one black_box per SipHash call: FAIL, blocking only siphash-single's instructions (region,
                  and possibly whole-process)
    noop          a doc-comment edit that rebuilds dashcore_hashes and every dependant: PASS
    stale-base    B1: integration has plant P1, the PR (cut before P1) adds plant P2; measured on the merge,
                  so P1+P2 shows and the gate FAILs (--only 'siphash-*')
    bench-input   M2: P1 plus a bench input change (LEN 25 -> 24): FAIL with BENCH-CHANGED, and the head is
                  measured with the base's bench, so P1's regression still shows (--only 'siphash-*')

The runner's checkout must be the base's (M3); it exits 1 when an expectation fails.
"""

import argparse
import json
import os
import subprocess
import sys
import tempfile
import time

TOOL = os.path.dirname(os.path.dirname(os.path.realpath(__file__)))
REPO = subprocess.run(["git", "rev-parse", "--show-toplevel"], cwd=TOOL, capture_output=True, text=True,
                      check=True).stdout.strip()
SIPHASH = "hashes/src/siphash24.rs"
SIPBENCH = "hashes/benches/siphash.rs"
BUILD_RS = "hashes/build.rs"


def git(*args, env=None, inp=None):
    p = subprocess.run(["git", *args], cwd=REPO, capture_output=True, text=True, input=inp,
                       env=None if env is None else {**os.environ, **env})
    if p.returncode:
        raise SystemExit(f"git {' '.join(args)}: {p.stderr}")
    return p.stdout.strip()


def replace_once(text, old, new):
    if text.count(old) != 1:
        raise SystemExit(f"expected exactly one {old!r}")
    return text.replace(old, new)


def commit(parent, edits, msg):
    """A commit over `parent` with `edits` ({path: text -> text}), on no branch."""
    with tempfile.TemporaryDirectory() as d:
        env = {"GIT_INDEX_FILE": os.path.join(d, "index")}
        git("read-tree", parent, env=env)
        for path, edit in edits.items():
            old = subprocess.run(["git", "show", f"{parent}:{path}"], cwd=REPO, capture_output=True, text=True)
            blob = git("hash-object", "-w", "--stdin", inp=edit(old.stdout if old.returncode == 0 else ""))
            git("update-index", "--add", "--cacheinfo", f"100644,{blob},{path}", env=env)
        tree = git("write-tree", env=env)
    return git("commit-tree", tree, "-p", parent, "-m", msg)


def plant_p1(text):
    return replace_once(text, "    v3 ^= b;\n", "    v3 ^= b;\n    v3 = core::hint::black_box(v3); // PF0 plant P1\n")


def plant_p2(text):
    return replace_once(text, "    v2 ^= 0xff;\n", "    v2 ^= core::hint::black_box(0xffu64); // PF0 plant P2\n")


def noop(text):
    return replace_once(text, "//! SipHash 2-4, batched.\n", "//! SipHash 2-4, batched (PF0 no-op edit).\n")


def bench_len(text):
    return replace_once(text, "const LEN: usize = 25;", "const LEN: usize = 24;")


# The PF0 r2 reviewer's exploit: a build script that edits the bench's input after the bench-change check.
BUILD_SCRIPT = """fn main() {
    let p = "benches/siphash.rs";
    let s = std::fs::read_to_string(p).unwrap();
    std::fs::write(p, s.replace("const LEN: usize = 25;", "const LEN: usize = 24;")).unwrap();
}
"""
# The same without failing when the write is refused.
QUIET_BUILD_SCRIPT = BUILD_SCRIPT.replace(".unwrap();\n}", ";\n}").replace("    std::fs::write", "    let _ = std::fs::write")
# The reviewer's state_writer as a build script: append a forged authoritative PASS to the gate's ledger.
STATE_WRITER = """fn main() {{
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open("{ledger}") {{
        let _ = writeln!(f, "{{{{\\"authoritative\\":true,\\"verdict\\":\\"PASS\\",\\"review_probe\\":\\"state-write\\"}}}}");
    }}
}}
"""


def add(text):
    return lambda _old: text


def scenarios(base):
    p1 = commit(base, {SIPHASH: plant_p1}, "PF0 acceptance: plant P1")
    ledger = os.path.join(os.environ.get("PERFGATE_ROOT", "/work/perfgate"), "ledger.jsonl")
    return {
        "planted": (base, p1, [], "FAIL"),
        "noop": (base, commit(base, {SIPHASH: noop}, "PF0 acceptance: no-op"), [], "PASS"),
        "stale-base": (p1, commit(base, {SIPHASH: plant_p2}, "PF0 acceptance: plant P2 (cut before P1)"),
                       ["--only", "siphash-*"], "FAIL"),
        "bench-input": (base, commit(p1, {SIPBENCH: bench_len}, "PF0 acceptance: P1 + bench input change"),
                        ["--only", "siphash-*"], "FAIL"),
        "buildscript": (base, commit(p1, {BUILD_RS: add(BUILD_SCRIPT)}, "PF0 acceptance: P1 + bench-editing build.rs"),
                        ["--only", "siphash-*"], "ERROR"),
        "buildscript-quiet": (base, commit(p1, {BUILD_RS: add(QUIET_BUILD_SCRIPT)},
                                           "PF0 acceptance: P1 + bench-editing build.rs, write errors ignored"),
                              ["--only", "siphash-*"], "FAIL"),
        "state-write": (base, commit(base, {BUILD_RS: add(STATE_WRITER.format(ledger=ledger))},
                                     "PF0 acceptance: build.rs appends a forged PASS to the ledger"),
                        ["--only", "siphash-*"], "FAIL"),
    }


PLANTED = ("siphash-single-16k", "instructions", "REGRESSION")


def check(name, report, verdict):
    """Scenario-specific expectations beyond the verdict; returns problems."""
    blocking = [r for r in report["rows"] if r["status"] in ("REGRESSION", "NOISY-UNRESOLVED", "DRIFT",
                                                             "BENCH-CHANGED", "ERROR")]
    keys = {(r["workload"], r["counter"], r["status"]) for r in blocking}
    problems = []
    whole = {("siphash-single-16k", c, "REGRESSION") for c in ("instructions_total", "instructions_process")}
    if name == "planted" and (PLANTED not in keys or keys - {PLANTED} - whole):
        problems.append(f"blocking rows {sorted(keys)}, expected only siphash-single-16k instructions")
    if name == "stale-base" and ("siphash-single-16k", "instructions", "REGRESSION") not in keys:
        problems.append(f"P1+P2 not caught: {sorted(keys)}")
    if name in ("bench-input", "buildscript", "buildscript-quiet", "state-write"):
        if ("(benches)", "-", "BENCH-CHANGED") not in keys:
            problems.append("no BENCH-CHANGED row")
    if name in ("bench-input", "buildscript-quiet") and PLANTED not in keys:
        problems.append("P1 hidden by the bench edit")
    if name == "state-write":
        with open(os.path.join(os.environ.get("PERFGATE_ROOT", "/work/perfgate"), "ledger.jsonl")) as f:
            if "review_probe" in f.read():
                problems.append("the build script wrote the ledger")
    return problems


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default="HEAD")
    ap.add_argument("--scenario", action="append")
    ap.add_argument("--out", default=os.path.join(os.environ.get("PERFGATE_ROOT", "/work/perfgate"), "acceptance"))
    args = ap.parse_args()
    base = git("rev-parse", "--verify", f"{args.base}^{{commit}}")
    run = [sys.executable, os.path.join(TOOL, "run")]
    root = os.environ.get("PERFGATE_ROOT", "/work/perfgate")
    if not os.path.exists(os.path.join(root, "anchor.json")):
        t0 = time.monotonic()
        subprocess.run([*run, "anchor", base, "--justification", "PF0 acceptance runs"], check=True)
        print(f"anchor at {base[:12]}: {time.monotonic() - t0:.0f} s", flush=True)
    failed = False
    for name, (b, h, extra, want) in scenarios(base).items():
        if args.scenario and name not in args.scenario:
            continue
        out = os.path.join(args.out, name)
        t0 = time.monotonic()
        p = subprocess.run([*run, "gate", "--base", b, "--head", h, "--integration-ref", b, "--out", out, *extra],
                           capture_output=True, text=True)
        wall = time.monotonic() - t0
        os.makedirs(out, exist_ok=True)
        with open(os.path.join(out, "gate.log"), "w") as f:
            f.write(p.stderr + p.stdout)
        try:
            with open(os.path.join(out, "report.json")) as f:
                report = json.load(f)
        except FileNotFoundError:
            report = {"verdict": "ERROR", "rows": []}
        v = report["verdict"]
        problems = ([] if v == want else [f"verdict {v}, expected {want}"]) + check(name, report, v)
        failed |= bool(problems)
        print(f"{name:12} {'ok  ' if not problems else 'FAIL'} verdict {v} (exit {p.returncode}), {wall:.0f} s, "
              f"base {b[:12]} head {h[:12]}{'; ' + '; '.join(problems) if problems else ''}", flush=True)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
