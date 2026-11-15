#!/usr/bin/env python3
"""Extract the goldens of one recorded FT node (O8 spec §9, §13.1, §13.4).

From a recording made by record-ft.py (<rec>/frozen/node<i>, <rec>/rec/node<i>/), this
restarts a reflinked copy of the frozen node offline with its last start's args plus
  -connect=0 -listen=0 -rpcdoccheck=0 -coinstatsindex=1 -disablewallet -mocktime=<last>
(without the digest flags; <last> is the last mock time the node's debug.log shows, so that
nothing time-dependent moves between extractions) and writes, next to the node's digest:
  golden.jsonl   dash-conformance digest extract --dense, which checks every patch key of each C
                 record against the copy (blocks, rev files, RPCs) before writing it (§13.5)
  blocks.dat     every active-chain block, Core -loadblock framing (magic, LE32 size, bytes)
  dec177.jsonl   canonical_mnlist_check's verdicts on the golden's canonical_mnlist records
  extract.json   what ran, the hashes of the digest and decisions files extracted from and of the
                 golden, the extraction's summary, the BIP9 check of runs with other -vbparams, and
                 whether a second extraction was byte-identical (--twice)
A run whose start had other -vbparams than the last start (feature_mnehf restarts with new ones)
has its BIP9 values checked on one more copy per parameter set, started with that start's args:
digest check-bip9 must check every height the extraction left unchecked, at the same tip.
With --twice, a second extraction that differs is kept in <scratch> and its first differing
line printed.

Usage: extract.py --rec <out>/<slug> --node node0 --bin <dir with dashd> --conformance <dash-conformance>
                  --check <canonical_mnlist_check> --scratch <dir> [--twice]
"""
import argparse
import base64
import json
import re
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

from common import (checked_build_info, free_port, last_start, offline_copy, rig_provenance, run_args, sha256,
                    start_args)

# The args chainparams.cpp reads: the oracle needs them to build the same regtest params.
CHAINPARAMS = {"budgetparams", "dip3params", "fastprune", "highsubsidyblocks", "highsubsidyfactor",
               "llmqchainlocks", "llmqdevnetparams", "llmqinstantsenddip0024", "llmqmnhf", "llmqplatform",
               "llmqtestinstantsenddip0024", "llmqtestinstantsendparams", "llmqtestparams",
               "llmqtestplatformparams", "minimumdifficultyblocks", "powtargetspacing", "testactivationheight",
               "vbparams"}
MOCKTIME = re.compile(rb"\(mocktime: (\d+)\)")


def last_mocktime(debug_log):
    times = MOCKTIME.findall(Path(debug_log).read_bytes())
    return int(times[-1]) if times else None


class Node:
    """A dashd on an offline copy, with a cookie-authenticated JSON-RPC client."""

    def __init__(self, dashd, datadir, extra):
        self.port = free_port()
        self.datadir = datadir
        self.url = f"http://127.0.0.1:{self.port}"
        self.proc = subprocess.Popen(
            # The fixed args come last: the last command-line value of an arg wins.
            [str(dashd), f"-datadir={datadir}", *extra, "-connect=0", "-listen=0", "-rpcdoccheck=0",
             "-coinstatsindex=1", "-disablewallet", f"-rpcport={self.port}", "-printtoconsole=0", "-server=1"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        deadline = time.monotonic() + 600
        while time.monotonic() < deadline and self.proc.poll() is None:
            try:
                self.call("getblockcount")
                return
            except (OSError, RuntimeError):
                time.sleep(1)
        self.kill()
        raise RuntimeError(f"dashd on {datadir} did not start; see its debug.log")

    def cookie(self):
        return self.datadir / "regtest" / ".cookie"

    def call(self, method, *params):
        auth = base64.b64encode(self.cookie().read_bytes().strip()).decode()
        req = urllib.request.Request(self.url, json.dumps({"method": method, "params": params, "id": 0}).encode(),
                                     {"Authorization": f"Basic {auth}", "Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(req, timeout=600) as r:
                reply = json.load(r)
        except urllib.error.HTTPError as e:
            reply = json.load(e)
        if reply.get("error"):
            raise RuntimeError(f"{method}: {reply['error']}")
        return reply["result"]

    def kill(self):
        self.proc.kill()
        self.proc.wait()

    def stop(self):
        """Stops dashd; kills it if the RPC or the shutdown fails. Never raises."""
        try:
            self.call("stop")
            self.proc.wait(timeout=600)
        except Exception as e:
            print(f"dashd on {self.datadir} did not stop cleanly ({e}); killing it", file=sys.stderr)
            self.kill()


def write_blocks(node, path):
    """blocks.dat in -loadblock framing; the magic is the one the node wrote its own block files with."""
    magic = (node.datadir / "regtest" / "blocks" / "blk00000.dat").read_bytes()[:4]
    with open(path, "wb") as f:
        for h in range(node.call("getblockcount") + 1):
            raw = bytes.fromhex(node.call("getblock", node.call("getblockhash", h), 0))
            f.write(magic + len(raw).to_bytes(4, "little") + raw)


def chainparams_args(chainargs):
    """The chainparams args of the last start, command line first, as -key=value strings."""
    return start_chainparams(last_start(chainargs))


def start_chainparams(start):
    """One start's chainparams args, command line first, as -key=value strings."""
    args = []
    for source in ("cmdline", "config"):
        for key, values in sorted(start[source].items()):
            name = key.removeprefix("regtest.")
            if name in CHAINPARAMS:
                args += [f"-{name}={v}" for v in values]
    return args


def other_params_starts(rec):
    """[(run, start)] for the digest runs whose start had other chainparams args than the last
    start (the copy's).

    Each start writes one digest H, so the i-th H is the i-th start. Only command-line -vbparams
    may differ: the extractor then leaves those runs' BIP9 values to check_bip9 (§13.5); anything
    else would move the eras it checks against."""
    starts = json.loads((rec / "chainargs.json").read_text())["starts"]
    runs = [json.loads(line)["run"] for line in (rec / "digest.jsonl").open()
            if line.startswith('{"k":"H"')]
    if len(runs) != len(starts):
        sys.exit(f"{rec}: {len(runs)} digest runs but {len(starts)} starts")
    last = start_chainparams(starts[-1])
    other = []
    for run, start in zip(runs, starts):
        args = start_chainparams(start)
        if args != last:
            moved = {x.split("=", 1)[0] for x in set(args) ^ set(last)}
            if moved != {"-vbparams"} or start["config"] != starts[-1]["config"]:
                sys.exit(f"{rec}: run {run} has other chain params than the last start: {sorted(moved)}")
            other.append((run, start))
    return other


def other_params_runs(rec):
    return [run for run, _ in other_params_starts(rec)]


def check_bip9(a, frozen, datadir, rec, mocktime, summary):
    """Checks the BIP9 values the extraction left unchecked: one copy per parameter set among the
    other-params starts, started with the first such start's args. Exits unless the copies
    checked every one of summary's bip9_unchecked heights, each at summary's tip."""
    groups = {}
    for run, start in other_params_starts(rec):
        groups.setdefault(tuple(start_chainparams(start)), (start, []))[1].append(run)
    checked, report = [], []
    for start, runs in groups.values():
        offline_copy(frozen, datadir)
        # The copy only answers RPCs: no block check at startup
        args = start_args(start) + ([f"-mocktime={mocktime}"] if mocktime else []) + ["-checkblocks=1",
                                                                                      "-checklevel=0"]
        node = Node(a.bin / "dashd", datadir, args)
        try:
            out = subprocess.run([str(a.conformance), "digest", "check-bip9", "--rpc-url", node.url, "--cookie",
                                  str(node.cookie()), "--digest", str(rec / "digest.jsonl"),
                                  *(f"--run={r}" for r in runs)], capture_output=True, text=True)
        finally:
            node.stop()
        if out.returncode:
            sys.exit(f"{rec}: check-bip9 for {runs} failed: {out.stderr.strip()}")
        result = json.loads(out.stdout)
        if result["tip"] != summary["run"]:
            sys.exit(f"{rec}: the check-bip9 copy for {runs} is at {result['tip']}, the extraction at {summary['run']}")
        checked += result["checked"]
        report.append({"runs": runs, "args": args, "tip": result["tip"], "checked": result["checked"]})
    # check-bip9 also checks a block that a run with the copy's params connected too (a reorg
    # across a restart): every tip C of a block holds the same s, so that is not a gap.
    if not set(summary["bip9_unchecked"]) <= set(checked):
        sys.exit(f"{rec}: check-bip9 checked heights {sorted(checked)}, but the extraction left "
                 f"{sorted(set(summary['bip9_unchecked']) - set(checked))} unchecked")
    return {"unchecked": summary["bip9_unchecked"], "groups": report}


def extract(a, frozen, datadir, out_dir, rec, mocktime, summary):
    offline_copy(frozen, datadir)
    extra = run_args(rec / "chainargs.json") + ([f"-mocktime={mocktime}"] if mocktime else [])
    node = Node(a.bin / "dashd", datadir, extra)
    try:
        golden = out_dir / "golden.jsonl"
        with open(out_dir / "extract.log", "w") as log:
            subprocess.run([str(a.conformance), "digest", "extract", "--rpc-url", node.url, "--cookie",
                            str(node.cookie()), "--digest", str(rec / "digest.jsonl"), "--out", str(golden),
                            "--blocks-dir", str(datadir / "regtest" / "blocks"), "--dense", "--summary", str(summary),
                            *(f"--other-params-run={r}" for r in other_params_runs(rec))],
                           check=True, stdout=log, stderr=subprocess.STDOUT)
        write_blocks(node, out_dir / "blocks.dat")
    finally:
        node.stop()
    return golden


def first_difference(a, b):
    for n, (x, y) in enumerate(zip(Path(a).read_bytes().splitlines(), Path(b).read_bytes().splitlines()), 1):
        if x != y:
            return n, x[:300], y[:300]
    return None


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--rec", type=Path, required=True)
    p.add_argument("--node", required=True)
    p.add_argument("--bin", type=Path, required=True)
    p.add_argument("--conformance", type=Path, required=True)
    p.add_argument("--check", type=Path, required=True)
    p.add_argument("--scratch", type=Path, required=True)
    p.add_argument("--twice", action="store_true", help="extract again from a fresh copy and compare")
    a = p.parse_args()

    core = checked_build_info(a.bin)
    frozen = a.rec / "frozen" / a.node
    rec = a.rec / "rec" / a.node
    work = a.scratch / f"{a.rec.name}-{a.node}"
    shutil.rmtree(work, ignore_errors=True)
    (work / "second").mkdir(parents=True)
    mocktime = last_mocktime(frozen / "regtest" / "debug.log")
    golden = extract(a, frozen, work / "datadir", rec, rec, mocktime, work / "summary.json")
    extraction = json.loads((work / "summary.json").read_text())
    bip9 = check_bip9(a, frozen, work / "datadir", rec, mocktime, extraction)
    reproducible, difference = None, None
    if a.twice:
        again = extract(a, frozen, work / "datadir", work / "second", rec, mocktime, work / "second" / "summary.json")
        difference = first_difference(golden, again) or (
            None if (rec / "blocks.dat").read_bytes() == (work / "second" / "blocks.dat").read_bytes()
            else ("blocks.dat",)) or (
            None if json.loads((work / "second" / "summary.json").read_text()) == extraction else ("summary",))
        reproducible = difference is None and golden.read_bytes() == again.read_bytes()
        if not reproducible:
            print(f"{rec}: the second extraction differs (kept in {work / 'second'}): {difference}", file=sys.stderr)

    args = chainparams_args(rec / "chainargs.json")
    golden_lines = golden.read_text().splitlines()
    samples, heights = [], []
    for line in golden_lines:
        r = json.loads(line)
        if r["k"] == "C":
            heights.append(r["h"])
        elif r["k"] == "S" and r["rpc"] == "canonical_mnlist":
            samples.append(json.dumps({"height": r["h"], "chain": "regtest", "chain_args": args,
                                       "hex": r["res"]["hex"], "hash": r["res"]["hash"]}))
    verdicts = subprocess.run([str(a.check)], input="\n".join(samples) + "\n", capture_output=True, text=True,
                              check=True).stdout
    (rec / "dec177.jsonl").write_text(verdicts)
    results = [json.loads(v)["result"] for v in verdicts.splitlines()]

    summary = {
        "core": core, "rig": rig_provenance(),
        "conformance_sha256": sha256(a.conformance), "check_sha256": sha256(a.check),
        "mocktime": mocktime, "chain_args": args, "other_params_runs": other_params_runs(rec),
        "summary": extraction, "bip9": bip9,
        "digest": {"sha256": sha256(rec / "digest.jsonl")},
        "decisions": {"sha256": sha256(rec / "decisions.jsonl")},
        "golden": {"sha256": sha256(golden), "lines": len(golden_lines),
                   "heights": [min(heights), max(heights)] if heights else None},
        "blocks_dat_sha256": sha256(rec / "blocks.dat"),
        "reproducible": reproducible,
        "dec177": {"samples": len(samples), "verdicts": {r: results.count(r) for r in sorted(set(results))},
                   "legs": ["oracle canonical re-encode", "oracle hash = RPC hash", "hash = sha256d(hex)"]},
    }
    (rec / "extract.json").write_text(json.dumps(summary, indent=1, sort_keys=True) + "\n")
    if reproducible is not False:
        shutil.rmtree(work)
    print(json.dumps({"rec": str(rec), "reproducible": reproducible, "dec177": summary["dec177"]["verdicts"]}))
    ok = all(r == "match" for r in results) and reproducible is not False
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
