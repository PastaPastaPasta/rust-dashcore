#!/usr/bin/env python3
"""The StateDigest flags-off overhead gate (O8 spec §11.3, §13.2).

Each run reflinks the workload (a frozen FT node datadir), removes its debug.log, and runs
  valgrind --tool=callgrind --toggle-collect=<each of TOGGLED> --collect-atstart=yes \
      dashd <mode args> -stopafterblockimport=1 ... <chain args> -par=1 -parbls=1
where the mode args are, with --mode
  reindex   -reindex-chainstate: every block connected once (ConnectBlock);
  verifydb  -checklevel=4 -checkblocks=<tip>: startup's VerifyDB disconnects every block and
            connects it again (DisconnectBlock, then ConnectBlock), the reorg-shaped workload,
and takes callgrind's total: the exact count of user-space instructions executed outside the
toggled functions. Those vary from run to run on their own: SeedStrengthen (random.cpp) hashes
SHA-512 for a fixed wall-clock time and alone varies a whole-process instructions:u count
threefold on a loaded box, and ECC_InitSanityCheck signs with a random key, grinding a random
number of nonces for a low R. -par=1 and -parbls=1 keep every signature check on the import
thread: with worker threads, how the checks split between threads changes the count. What is
left repeats to about 0.015 %. Stock and patched alternate. Every run must reindex to the
workload's tip (a run with the wrong chain args stops early and still exits 0) or, with
verifydb, verify every block at level 4 (too small a -dbcache skips level 3 and up), and each dashd
must have the toggled functions as symbols of their own (an inlined copy would escape the
toggle). The gate: the largest patched count is at most 0.1 % above the smallest stock count,
which also bounds the medians' difference.

Usage: overhead.py --workload <frozen node dir> --chainargs <chainargs.json> --stock <bin dir>
                   --patched <bin dir> --scratch <dir> [--runs 3] [--mode reindex|verifydb]
"""
import argparse
import json
import re
import shutil
import statistics
import subprocess
import sys
from pathlib import Path

from common import checked_build_info, free_port, offline_copy, run_args, run_group

TIP = re.compile(rb"UpdateTip: new best=\S+ height=(\d+)")
TOGGLED = ("SeedStrengthen", "ECC_InitSanityCheck")
# Appended after the workload's args, so they win.
SERIAL = ["-par=1", "-parbls=1"]


def last_tip(debug_log):
    tips = TIP.findall(debug_log.read_bytes())
    return int(tips[-1]) if tips else None


def check_symbols(dashd):
    """Each toggle needs its function as a symbol of its own; an inlined copy would escape it."""
    symbols = subprocess.run(["nm", "-C", str(dashd)], check=True, capture_output=True, text=True).stdout
    for name in TOGGLED:
        if not re.search(rf"^\S+ [tT] {name}\(", symbols, re.MULTILINE):
            sys.exit(f"{dashd} has no {name} function for --toggle-collect")


def mode_args(mode, tip):
    if mode == "reindex":
        return ["-reindex-chainstate"]
    return ["-checklevel=4", f"-checkblocks={tip}"]


def check_run(mode, debug_log, dashd, tip):
    """That the run did the whole workload."""
    if mode == "reindex":
        reached = last_tip(debug_log)
        if reached != tip:
            sys.exit(f"{dashd} reindexed to {reached}, not to the workload's tip {tip}")
        return
    log = debug_log.read_bytes()
    for want in (f"Verifying last {tip} blocks at level 4", f"No coin database inconsistencies in last {tip} blocks"):
        if want.encode() not in log:
            sys.exit(f"{dashd}: {debug_log} lacks {want!r}")
    if b"Skipped verification of level >=3" in log:
        sys.exit(f"{dashd} skipped VerifyDB's level 3 and up")


def measure(dashd, workload, scratch, extra, tip, mode):
    datadir = scratch / "run"
    offline_copy(workload, datadir)
    # The copy's debug.log already ends at the tip: only this run's lines may count.
    (datadir / "regtest" / "debug.log").unlink()
    out = scratch / "callgrind.out"
    # --toggle-collect implies --collect-atstart=no: the explicit yes must come after it.
    cmd = ["valgrind", "--tool=callgrind", f"--callgrind-out-file={out}",
           *[f"--toggle-collect={name}*" for name in TOGGLED], "--collect-atstart=yes",
           str(dashd), f"-datadir={datadir}", *mode_args(mode, tip), "-txindex=0",
           "-stopafterblockimport=1", "-connect=0", "-listen=0", f"-rpcport={free_port()}", "-disablewallet",
           "-printtoconsole=0", *extra, *SERIAL]
    # A timeout, not a hang: stock rc.3 can deadlock in shutdown when init fails during the import.
    run_group(cmd, timeout=4 * 3600, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    totals = re.search(r"^totals: (\d+)$", out.read_text(errors="replace"), re.MULTILINE)
    if not totals:
        sys.exit(f"no totals line in {out}")
    check_run(mode, datadir / "regtest" / "debug.log", dashd, tip)
    out.unlink()
    return int(totals.group(1))


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--workload", type=Path, required=True)
    p.add_argument("--chainargs", type=Path, required=True)
    p.add_argument("--stock", type=Path, required=True, help="bin dir with dashd and build-info.txt")
    p.add_argument("--patched", type=Path, required=True, help="bin dir with dashd and build-info.txt")
    p.add_argument("--scratch", type=Path, required=True)
    p.add_argument("--runs", type=int, default=3)
    p.add_argument("--limit", type=float, default=0.1, help="gate, percent")
    p.add_argument("--mode", choices=("reindex", "verifydb"), default="reindex")
    a = p.parse_args()

    a.scratch.mkdir(parents=True, exist_ok=True)
    extra = run_args(a.chainargs)
    tip = last_tip(a.workload / "regtest" / "debug.log")
    builds = {"stock": a.stock, "patched": a.patched}
    # checked_build_info has verified every binary's hash against build-info.txt.
    info = {name: checked_build_info(d) for name, d in builds.items()}
    for bin_dir in builds.values():
        check_symbols(bin_dir / "dashd")
    counts = {name: [] for name in builds}
    for i in range(a.runs):
        for name, bin_dir in builds.items():
            counts[name].append(measure(bin_dir / "dashd", a.workload, a.scratch, extra, tip, a.mode))
            print(f"run {i + 1} {name}: {counts[name][-1]}", flush=True)
    shutil.rmtree(a.scratch / "run", ignore_errors=True)

    def percent(patched, stock):
        return round(100 * (patched - stock) / stock, 4)

    median_delta = percent(statistics.median(counts["patched"]), statistics.median(counts["stock"]))
    worst_delta = percent(max(counts["patched"]), min(counts["stock"]))
    passed = worst_delta <= a.limit
    print(json.dumps({"workload": str(a.workload), "mode": a.mode, "tip": tip,
                      "args": mode_args(a.mode, tip) + extra + SERIAL, "toggled": TOGGLED,
                      "builds": info, "counts": counts,
                      "spread_percent": {k: percent(max(v), min(v)) for k, v in counts.items()},
                      "median_delta_percent": median_delta, "worst_delta_percent": worst_delta,
                      "limit_percent": a.limit, "pass": passed}, indent=1))
    sys.exit(0 if passed else 1)


if __name__ == "__main__":
    main()
