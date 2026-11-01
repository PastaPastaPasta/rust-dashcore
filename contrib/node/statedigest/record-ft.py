#!/usr/bin/env python3
"""Record one Core functional test with the StateDigest-patched dashd (O8 spec §13.1).

Runs the test through dashd-wrapper, then for every node of the test:
  <out>/<slug>/frozen/node<i>/   the stopped node's datadir, moved out of the test tmpdir
  <out>/<slug>/rec/node<i>/      digest.jsonl, decisions.jsonl, chainargs.json, provenance.json
and, once, <out>/<slug>/manifest.json: the test, its args and every node<i> directory the test
created in its run directory ("nodes", the test's node count), split into "recorded" and
"unstarted". store-vectors.sh requires the recordings it stores to match the manifest, so a
node directory that goes missing later is noticed.

chainargs.json lists every process start of the node with its command-line and
config-file args as debug.log printed them ("Command-line arg:", "Config file arg:"),
without -mocktime; sensitive values are "<redacted>" and negated args false. extract.py
restarts a frozen copy with the last start's args. Nodes the test never started (no
regtest/debug.log) are listed in each provenance.json and not recorded; a node that started
but wrote no digest stops the recording.

Usage: record-ft.py --build <core build tree> --bin <dir with dashd, build-info.txt>
                    --out <dir> [--python <python with dash_hash>] [--timeout-factor N] <test>.py [test args...]
       record-ft.py --backfill-manifest <out>/<slug>
The second form writes manifest.json for a recording made before the manifest existed. Every
node directory of the run was then either moved to frozen/ or listed as unstarted in
provenance.json, so those two sets are the run directory's listing.
"""
import argparse
import datetime
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

from common import HERE, REDACTED, checked_build_info, rig_provenance, sha256

# Core's logArgsPrefix: a string value is quoted, a sensitive one is ****, a negated one is false.
ARG_LINE = re.compile(r"\[logArgsPrefix\] (Command-line|Config file) arg: (?:\[(\w+)\] )?([\w-]+)="
                      r"(?:\"(.*)\"|(\*\*\*\*)|(false))$")
START_LINE = "[LogPackageVersion] Dash Core version "


def now():
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def chain_args(debug_log):
    starts = []
    with open(debug_log, encoding="utf-8", errors="replace") as f:
        for line in f:
            line = line.rstrip("\n")
            if START_LINE in line:
                starts.append({"version": line.split(START_LINE, 1)[1], "cmdline": {}, "config": {}})
                continue
            m = ARG_LINE.search(line)
            if not m or not starts:
                continue
            source, section, key, value, sensitive, negated = m.groups()
            if key == "mocktime":
                continue
            value = REDACTED if sensitive else False if negated else value
            if section:
                key = f"{section}.{key}"
            starts[-1]["cmdline" if source == "Command-line" else "config"].setdefault(key, []).append(value)
    return {"network": "regtest", "starts": starts}


def record_counts(path):
    counts, heights = {}, []
    with open(path, encoding="ascii") as f:
        for line in f:
            r = json.loads(line)
            counts[r["k"]] = counts.get(r["k"], 0) + 1
            if r["k"] == "C" and r["org"] == "tip":
                heights.append(r["h"])
    return counts, [min(heights), max(heights)] if heights else None


def node_index(name):
    return int(name[4:])


def write_manifest(top, test, test_args, nodes, recorded, unstarted, source):
    manifest = {"test": test, "test_args": test_args, "nodes": sorted(nodes, key=node_index),
                "recorded": sorted(recorded, key=node_index), "unstarted": sorted(unstarted, key=node_index),
                "source": source}
    (top / "manifest.json").write_text(json.dumps(manifest, indent=1, sort_keys=True) + "\n")


def backfill_manifest(top):
    """manifest.json for a recording made before record-ft.py wrote one (see the usage)."""
    if (top / "manifest.json").exists():
        sys.exit(f"{top}/manifest.json exists")
    recorded = {d.name for d in (top / "rec").iterdir() if re.fullmatch(r"node\d+", d.name)}
    frozen = {d.name for d in (top / "frozen").iterdir() if re.fullmatch(r"node\d+", d.name)}
    if not recorded or recorded != frozen:
        sys.exit(f"{top}: rec/ has {sorted(recorded)} but frozen/ has {sorted(frozen)}")
    provenances = [json.loads((top / "rec" / n / "provenance.json").read_text()) for n in sorted(recorded)]
    first = provenances[0]
    for pv in provenances:
        if [pv["test"], pv["test_args"], pv["unstarted_nodes"]] != [first["test"], first["test_args"],
                                                                     first["unstarted_nodes"]]:
            sys.exit(f"{top}: the nodes' provenance.json files disagree on the test or its unstarted nodes")
    unstarted = set(first["unstarted_nodes"])
    if unstarted & recorded:
        sys.exit(f"{top}: {sorted(unstarted & recorded)} are both recorded and unstarted")
    write_manifest(top, first["test"], first["test_args"], recorded | unstarted, recorded, unstarted,
                   "backfill: frozen/ and unstarted_nodes")
    print(f"{top}: wrote manifest.json")


def main():
    if len(sys.argv) == 3 and sys.argv[1] == "--backfill-manifest":
        return backfill_manifest(Path(sys.argv[2]))
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--build", type=Path, required=True)
    p.add_argument("--bin", type=Path, required=True)
    p.add_argument("--out", type=Path, required=True)
    p.add_argument("--python", default=sys.executable)
    p.add_argument("--timeout-factor", default="4")
    p.add_argument("test")
    p.add_argument("test_args", nargs=argparse.REMAINDER)
    a = p.parse_args()

    slug = "".join([a.test.removesuffix(".py"), *(f"-{x.lstrip('-')}" for x in a.test_args)])
    top = a.out / slug
    run_dir = top / "run"
    if top.exists():
        sys.exit(f"{top} exists; remove it to record again")
    top.mkdir(parents=True)
    core = checked_build_info(a.bin)
    dashd = (a.bin / "dashd").resolve()
    env = dict(os.environ, DASHD=str(HERE / "dashd-wrapper"), STATEDIGEST_DASHD=str(dashd),
               DASHCLI=str((a.bin / "dash-cli").resolve()))
    cmd = [a.python, str(a.build / "test/functional" / a.test), "--nocleanup", f"--tmpdir={run_dir}",
           f"--timeout-factor={a.timeout_factor}", *a.test_args]
    started = now()
    with open(top / "test.log", "wb") as log:
        rc = subprocess.run(cmd, env=env, stdout=log, stderr=subprocess.STDOUT).returncode
    stopped = now()
    if rc != 0:
        print(f"{slug}: test failed (exit {rc}); see {top}/test.log", file=sys.stderr)
        sys.exit(1)

    seed = re.search(r"PRNG seed is: (\d+)", (run_dir / "test_framework.log").read_text())
    shutil.copy(run_dir / "test_framework.log", top / "test_framework.log")
    rig = rig_provenance()
    test_script = a.build / "test/functional" / a.test
    created = sorted((d for d in run_dir.iterdir() if re.fullmatch(r"node\d+", d.name)),
                     key=lambda d: node_index(d.name))
    # A node is unstarted only if dashd never ran on it (no debug.log); one that ran without
    # writing a digest is a recording failure, not a node to leave out
    unstarted = [d.name for d in created if not (d / "regtest" / "debug.log").exists()]
    for d in created:
        if d.name not in unstarted and not (d / "regtest" / "digest.jsonl").exists():
            sys.exit(f"{d.name} ran (it has a debug.log) but wrote no digest.jsonl")
    nodes = [d for d in created if d.name not in unstarted]
    write_manifest(top, a.test, a.test_args, [d.name for d in created], [d.name for d in nodes], unstarted,
                   "run directory")
    for node in nodes:
        net = node / "regtest"
        rec = top / "rec" / node.name
        rec.mkdir(parents=True)
        for name in ("digest.jsonl", "decisions.jsonl"):
            shutil.copy(net / name, rec / name)
        (rec / "chainargs.json").write_text(json.dumps(chain_args(net / "debug.log"), indent=1) + "\n")
        digest_counts, heights = record_counts(rec / "digest.jsonl")
        decision_counts, _ = record_counts(rec / "decisions.jsonl")
        provenance = {
            "test": a.test, "test_args": a.test_args, "prng_seed": seed and int(seed.group(1)),
            "timeout_factor": a.timeout_factor, "node": node.name, "network": "regtest",
            "started": started, "stopped": stopped,
            "core": core, "build_tree": str(a.build.resolve()), "test_sha256": sha256(test_script),
            "rig": rig, "unstarted_nodes": unstarted,
            "records": {"digest": digest_counts, "decisions": decision_counts}, "tip_heights": heights,
        }
        (rec / "provenance.json").write_text(json.dumps(provenance, indent=1, sort_keys=True) + "\n")
        (top / "frozen").mkdir(exist_ok=True)
        node.rename(top / "frozen" / node.name)
    print(f"{slug}: recorded {len(nodes)} node(s) in {top}")


if __name__ == "__main__":
    main()
