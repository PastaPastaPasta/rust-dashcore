#!/usr/bin/env python3
"""CH2c's DEC-43 proxy runner (docs/node-program/designs/CH2c-notes.md D6-D8).

Each run is one arm (`core`, or `rdashd` with CH2's tuning: D15's split since DEC-383 A3)
in its own `systemd-run --user --scope` on CPUs 24-31,
under the cell's common cap (anonymous memory and page cache together, no swap).

  proxy.py install                  copy the release binary into the scratch root
  proxy.py overhead CELL[,CELL...]   D7's O (DEC-431): Core's arm 5 times over the cell's whole
                                    trace, uncapped; the median, frozen once written
  proxy.py cells CELL[,CELL...] [--rotations N] [--rotations-for CELL=N[,CELL=N...]]
  proxy.py table                    the paired table and the gates (markdown, and JSON beside it)

Cells: {wm,wt}-{dec43,dec43p}; CELL~oN is CELL with O pinned to N MiB instead (DEC-431: the
old definition's value beside the new one). The scratch root is $PROXY_ROOT (default /work/scratch/ch2c);
the traces are read from $PROXY_TRACES (default /dev/shm/ch2c), copied there beforehand so the
arms' cgroups are not charged for them.
"""

import hashlib
import json
import math
import os
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from run import noise, sign_test, snapshot  # noqa: E402

ROOT = Path(os.environ.get("PROXY_ROOT", "/work/scratch/ch2c"))
TRACES = Path(os.environ.get("PROXY_TRACES", "/dev/shm/ch2c"))
BIN = ROOT / "bin" / "dashd-store-bakeoff"
RESULTS = ROOT / "proxy"
DB = ROOT / "db"
# Core in the middle, so each of our arms runs next to it in every rotation (D8).
ARMS = ["rdashd", "core"]
OURS = ["rdashd"]
MIB = 1 << 20
CPUS = "24-31"
# CacheSizes::calculate(300, txindex): coins 252.75 MiB, txindex 37.25 MiB; E = 74 MiB (CH2 §5.5).
B_LITERAL = int(252.75 * MIB)
TXINDEX = int(37.25 * MIB)
E = 74 * MIB
# ST1's full-chain UTXO count and stored entry size (ST1-bakeoff.md §3.4).
FULL_CHAIN_COINS = 6_422_912
STORED_ENTRY_BYTES = 75
# Core's MAX_BLOCK_COINSDB_USAGE_BYTES (validation.cpp:2956-2964).
MAX_BLOCK_COINSDB_USAGE = 10 * MIB
# D7 (DEC-431): O is the median of this many uncapped runs of Core's arm.
OVERHEAD_RUNS = 5
# D43 (DEC-431): the settings whose coins never reach LARGE on these traces take Core's periodic
# flush at the trace's middle block, in both arms, so each run has a flush before shutdown.
PERIODIC_SETTINGS = {"dec43"}


def large(b):
    """Core's LARGE trigger for a coins share of B: max(9B/10, B - 10 MiB) (DEC-383 A8)."""
    return max(9 * b // 10, b - MAX_BLOCK_COINSDB_USAGE)


def trace_for(workload):
    net = {"wm": "mainnet", "wt": "testnet"}[workload]
    (t,) = sorted(TRACES.glob(f"{net}-*.trace"))
    return t, json.loads(Path(str(t) + ".json").read_text())


def overhead():
    path = RESULTS / "overhead.json"
    if not path.exists():
        raise SystemExit("run `proxy.py overhead` first (D7's O)")
    return json.loads(path.read_text())


def cell_share(cell):
    """The cell's trace, peak live coins, B and periodic flush height (None: no periodic flush)."""
    workload, setting = cell.split("~")[0].split("-")
    trace, meta = trace_for(workload)
    u = meta["peak_live_coins"]
    b = B_LITERAL if setting == "dec43" else B_LITERAL * u // FULL_CHAIN_COINS
    periodic = meta["last_height"] // 2 if setting in PERIODIC_SETTINGS else None
    return trace, u, b, periodic


def cell_config(cell):
    trace, u, b, periodic = cell_share(cell)
    allowance = int(0.5 * u * STORED_ENTRY_BYTES)
    base, _, pinned = cell.partition("~")
    o = int(pinned.removeprefix("o")) * MIB if pinned else overhead().get(base)
    if o is None:
        raise SystemExit(f"run `proxy.py overhead {base}` first (D7's O)")
    return {
        "cell": cell, "trace": str(trace), "peak_live_coins": u, "coins_share": b,
        "periodic_flush_height": periodic, "large": large(b), "overhead": o,
        "allowance": allowance, "memory_max": b + E + TXINDEX + o + allowance,
    }


def provenance():
    """The binary's sha256 and the commit it was installed from: a cell never mixes two."""
    meta = BIN.parent / "installed.json"
    return json.loads(meta.read_text()) if meta.exists() else None


def scoped(cmd, memory_max, log):
    props = ["-p", f"AllowedCPUs={CPUS}", "-p", "MemorySwapMax=0"]
    if memory_max:
        props += ["-p", f"MemoryMax={memory_max}"]
    full = ["systemd-run", "--user", "--scope", "--quiet", *props, "--", *map(str, cmd)]
    with open(log, "w") as f:
        return subprocess.run(full, stdout=f, stderr=subprocess.STDOUT).returncode


def arm_run(cell, cfg, arm, rotation, order, blocks=None):
    cell_dir = RESULTS / cell
    cell_dir.mkdir(parents=True, exist_ok=True)
    name = f"{arm}-r{rotation}"
    db = DB / f"{cell}-{name}"
    shutil.rmtree(db, ignore_errors=True)
    out = cell_dir / f"{name}.harness.json"
    out.unlink(missing_ok=True)
    cmd = [BIN, "proxy", "--arm", arm, "--trace", cfg["trace"], "--db", db,
           "--coins-share", cfg["coins_share"], "--out", out]
    if blocks:
        cmd += ["--blocks", blocks]
    if cfg.get("periodic_flush_height") is not None:
        cmd += ["--periodic-flush-height", cfg["periodic_flush_height"]]
    before = snapshot()
    t0 = time.time()
    rc = scoped(cmd, cfg.get("memory_max"), cell_dir / f"{name}.log")
    wall = time.time() - t0
    after = snapshot()
    result = {"cell": cell, "arm": arm, "rotation": rotation, "order": order, "returncode": rc,
              "wall_s": wall, "config": cfg, "noise": noise(before, after), "provenance": provenance(),
              "harness": json.loads(out.read_text()) if out.exists() else None}
    (cell_dir / f"{name}.json").write_text(json.dumps(result, indent=1))
    total = result["harness"]["time_total_s"] if result["harness"] else None
    print(f"{cell} {name}: rc={rc} wall={wall:.1f}s time_total_s={total}", flush=True)
    shutil.rmtree(db, ignore_errors=True)
    return result


def cmd_install():
    src = Path(__file__).resolve().parent / "target" / "release" / "dashd-store-bakeoff"
    BIN.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(src, BIN)
    head = subprocess.run(["git", "rev-parse", "HEAD"], capture_output=True, text=True,
                          cwd=src.parent, check=True).stdout.strip()
    dirty = subprocess.run(["git", "status", "--porcelain", "--untracked-files=no"],
                           capture_output=True, text=True, cwd=src.parent, check=True).stdout != ""
    meta = {"sha256": hashlib.sha256(BIN.read_bytes()).hexdigest(), "commit": head, "dirty": dirty}
    (BIN.parent / "installed.json").write_text(json.dumps(meta, indent=1))
    print(BIN, json.dumps(meta))


def cmd_overhead(cells):
    """D7 (DEC-383 A8, DEC-431): O = the median, over 5 runs of Core's arm over the cell's whole
    trace at its B, uncapped, of its RssAnon peak less its peak coins usage, rounded up to a MiB.
    Core's arm does not change with ours, and ours sets its own transient, which the cap should
    not follow. Frozen: a cell's O is never measured again; both arms run under it."""
    path = RESULTS / "overhead.json"
    out = json.loads(path.read_text()) if path.exists() else {}
    for cell in cells:
        if cell in out:
            print(f"{cell}: O is frozen at {out[cell] / MIB:.0f} MiB", flush=True)
            continue
        trace, _, b, periodic = cell_share(cell)
        cfg = {"trace": str(trace), "coins_share": b, "periodic_flush_height": periodic}
        inputs = []
        for r in range(OVERHEAD_RUNS):
            run = arm_run(f"overhead-{cell}", cfg, "core", r, ["core"])
            h = run["harness"]
            if run["returncode"] != 0 or h is None:
                raise SystemExit(f"overhead run {cell} r{r} failed")
            inputs.append(h["memory"]["rss_anon_peak"] - h["measured"]["peak_boundary_usage"])
        out[cell] = math.ceil(statistics.median(inputs) / MIB) * MIB
        out[f"{cell}_inputs"] = inputs
        out[f"{cell}_provenance"] = run["provenance"]
        RESULTS.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(out, indent=1))
    print(json.dumps(out, indent=1))


def cmd_cells(cells, n):
    prov = provenance()
    if prov is None or prov["dirty"]:
        raise SystemExit("install a binary from a clean commit first")
    for cell in cells:
        cfg = cell_config(cell)
        print(json.dumps({k: v for k, v in cfg.items() if k != "trace"}), flush=True)
        for r in range(n(cell)):
            # D8: a rotation's arms run back to back, the order alternating per rotation. A
            # rotation is kept whole: a failed run is that arm's loss and is never retried alone,
            # and a rotation started but not finished, or run by another binary, is rerun whole.
            order = ARMS if r % 2 == 0 else ARMS[::-1]
            done = [RESULTS / cell / f"{arm}-r{r}.json" for arm in order]
            if all(d.exists() for d in done):
                provs = {json.dumps(json.loads(d.read_text()).get("provenance")) for d in done}
                if provs == {json.dumps(prov)}:
                    continue
                raise SystemExit(f"{cell} r{r}: results from another binary; move them away first")
            for arm in order:
                arm_run(cell, cfg, arm, r, order)


# ---- the table ----

GATED = {
    "time_total_s": lambda h: h["time_total_s"],
    "v_stall_s": lambda h: h["measured"]["v_stall_s"],
    # DEC-407 Q1: the memory bar is the total resident set a user sees, VmRSS (anon + file + shmem,
    # summed at one read). RssAnon and RssFile are reported beside it.
    "rss_total_peak": lambda h: h["memory"]["rss_total_peak"],
    # DEC-419: what the cache allocated at a block boundary (native Σ less R and the unbacked
    # shard ceilings), against Core's own peak usage, with no floor. Native Σ ≤ B is the hard
    # bound, checked at every boundary (`boundaries_above_b`).
    "peak_coins_allocated": lambda h: h["measured"]["peak_boundary_allocated"],
}
REPORTED = {
    "connect_s": lambda h: h["measured"]["connect_s"],
    # V's whole time at its flush sites; the gated V stall counts only its waits (D8).
    "v_site_s": lambda h: h["measured"].get("v_site_s", h["measured"]["v_stall_s"]),
    "cg_mem_peak": lambda h: (h.get("cgroup_end") or {}).get("memory_peak") or 0,
    "rss_anon_peak": lambda h: h["memory"]["rss_anon_peak"],
    # Native Σ at a block boundary (Core: its usage); gated until DEC-419, against max(Core's,
    # LARGE) (DEC-383 A8).
    "peak_coins_usage": lambda h: h["measured"]["peak_boundary_usage"],
    "rss_file_peak": lambda h: h["memory"]["rss_file_peak"],
    "rss_shmem_peak": lambda h: h["memory"]["rss_shmem_peak"],
    "misses": lambda h: h["measured"]["misses"],
    "db_bytes": lambda h: h["db_bytes"],
}


def runs(cell):
    """Every run, failed ones included: {(arm, rotation): harness JSON, or None if it failed}."""
    out = {}
    for f in sorted((RESULTS / cell).glob("*-r*.json")):
        if f.name.endswith(".harness.json"):
            continue
        r = json.loads(f.read_text())
        ok = r["returncode"] == 0 and r["harness"]
        out[(r["arm"], r["rotation"])] = r["harness"] if ok else None
    return out


def ratio(o, c):
    if c:
        return o / c
    return math.inf if o else 1.0


def summarize(cell, arm, rotations):
    """Our arm against Core over the rotations both ran. A failed run of ours (an OOM kill under
    the cap) loses every gated metric of its pair; a failed Core run voids its pair."""
    rs = runs(cell)
    pairs = sorted(rot for (a, rot) in rs if a == arm and ("core", rot) in rs)
    valid = [p for p in pairs if rs[("core", p)] is not None]
    ok = [p for p in valid if rs[(arm, p)] is not None]
    row = {"cell": cell, "arm": arm, "pairs": len(valid), "ours_failed": len(valid) - len(ok),
           "core_failed": len(pairs) - len(valid), "metrics": {}, "checks": {}}
    for name, f in {**GATED, **REPORTED}.items():
        ours = [f(rs[(arm, p)]) for p in ok]
        core = [f(rs[("core", p)]) for p in ok]
        ratios = [ratio(o, c) for o, c in zip(ours, core)]
        row["metrics"][name] = {
            "gated": name in GATED,
            "ours_median": statistics.median(ours) if ours else None,
            "core_median": statistics.median(core) if core else None,
            "ratio_median": statistics.median(ratios) if ratios else None,
            "ratio_range": [min(ratios), max(ratios)] if ratios else None,
            "ours_le_core": sum(o <= c for o, c in zip(ours, core)),
            "sign_p": sign_test(ratios),
        }
    ours = [rs[(arm, p)] for p in ok]
    row["checks"] = {
        "boundaries_above_b": sum(h["measured"]["boundaries_above_b"] for h in ours),
        "dmax_flushes": sum(h["measured"]["flushes"].get("dmax", 0) for h in ours),
        "flushes_ours": [h["measured"]["flushes"] for h in ours],
        "flushes_core": [rs[("core", p)]["measured"]["flushes"] for p in ok],
    }
    # DEC-431: a gated metric passes when ours is at most Core's on the median and in a majority
    # of the pairs.
    gates = [m["ours_median"] is not None and m["ours_median"] <= m["core_median"]
             and 2 * m["ours_le_core"] > rotations
             for m in row["metrics"].values() if m["gated"]]
    row["pass"] = (len(valid) == rotations and all(gates)
                   and row["checks"]["boundaries_above_b"] == 0
                   and row["checks"]["dmax_flushes"] == 0)
    return row


def fmt(v, name):
    if v is None:
        return "-"
    if name in ("rss_total_peak", "rss_anon_peak", "rss_file_peak", "rss_shmem_peak",
                "peak_coins_usage", "peak_coins_allocated", "cg_mem_peak", "db_bytes"):
        return f"{v / MIB:.1f} MiB"
    if name.endswith("_s"):
        return f"{v:.2f} s"
    return f"{v:g}"


def cmd_table(rotations):
    cells = sorted(d.name for d in RESULTS.iterdir()
                   if d.is_dir() and not d.name.startswith("overhead"))
    rows = [summarize(c, arm, rotations(c)) for c in cells for arm in OURS]
    (RESULTS / "table.json").write_text(json.dumps(rows, indent=1))
    out = ["| cell | arm | metric | ours (median) | Core (median) | ratio median [range] | ours ≤ Core | gated |",
           "|---|---|---|---|---|---|---|---|"]
    for row in rows:
        for name, m in row["metrics"].items():
            rng = m["ratio_range"]
            ratio = f"{m['ratio_median']:.3f} [{rng[0]:.3f}, {rng[1]:.3f}]" if rng else "-"
            out.append(f"| {row['cell']} | {row['arm']} | {name} | {fmt(m['ours_median'], name)} | "
                       f"{fmt(m['core_median'], name)} | {ratio} | "
                       f"{m['ours_le_core']}/{row['pairs']} | {'yes' if m['gated'] else ''} |")
    out.append("")
    for row in rows:
        c = row["checks"]
        out.append(f"- {row['cell']} {row['arm']}: {'PASS' if row['pass'] else 'FAIL'} ({row['pairs']} "
                   f"pairs, ours failed in {row['ours_failed']}, Core in {row['core_failed']}); "
                   f"native Σ > B at {c['boundaries_above_b']} boundaries; peak coins allocated gated "
                   f"against Core's peak; DMAX flushes "
                   f"{c['dmax_flushes']}; flushes ours {c['flushes_ours'][:1]}, Core "
                   f"{c['flushes_core'][:1]}")
    text = "\n".join(out)
    (RESULTS / "table.md").write_text(text + "\n")
    print(text)


def main():
    args = sys.argv[1:]
    if not args:
        raise SystemExit(__doc__)
    default = int(args[args.index("--rotations") + 1]) if "--rotations" in args else 5
    # `--rotations-for cell=n,...`: a cell whose pairs need more rotations to separate (DEC-409).
    per_cell = {}
    if "--rotations-for" in args:
        for item in args[args.index("--rotations-for") + 1].split(","):
            cell, n = item.split("=")
            per_cell[cell] = int(n)
    rot = lambda cell: per_cell.get(cell, default)  # noqa: E731
    if args[0] == "install":
        cmd_install()
    elif args[0] == "overhead":
        cmd_overhead(args[1].split(","))
    elif args[0] == "cells":
        cmd_cells(args[1].split(","), rot)
    elif args[0] == "table":
        cmd_table(rot)
    else:
        raise SystemExit(__doc__)


if __name__ == "__main__":
    main()
