#!/usr/bin/env python3
"""ST1 bake-off runner (docs/node-program/designs/ST1-bakeoff.md §3.4, §5, §7).

Runs every replay in its own cgroup scope (CPUs 24-31 on NUMA node 1, a hard memory cap without
swap, I/O and CPU accounting), in rotated engine order, and stores each run's metrics together
with a noise record of the shared box.

  run.py pilot                      one W-M P300 run per engine
  run.py cells CELL[,CELL...] [--rotations N] [--engines E,E...]
  run.py micro                      D1-D3 on fresh W-M literal-300 databases
  run.py diskfull                   D4
  run.py summary                    medians, spreads and paired ratios per cell
  run.py report                     markdown tables from the summary

Cells: <workload>-<setting>, workload in {wm, wt}, setting in {p300, p4096, l300, l4096, p300u}
(p300u = P300 without the memory cap).
"""

import json
import math
import os
import re
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path("/work/bench-store")
TRACES = Path("/dev/shm/bench-store/traces")
BIN = ROOT / "dashd-store-bakeoff.bin"
RESULTS = ROOT / "results"
DB = ROOT / "db"
ENGINES = ["rocksdb", "leveldb", "mdbx"]
MIB = 1 << 20
ENTRY_COST = 80
# Full-chain UTXO count at mainnet height 2,551,683 (Blockchair API, 2026-10-08), until W-MF exists.
FULL_CHAIN_COINS = 6_422_912
CPUS = "24-31"
HARNESS_OVERHEAD = 64 * MIB
STORED_ENTRY_BYTES = 75


def trace_for(workload):
    net = {"wm": "mainnet", "wt": "testnet"}[workload]
    (t,) = sorted(TRACES.glob(f"{net}-*.trace"))
    return t, json.loads(Path(str(t) + ".json").read_text())


def literal_split(dbcache_mib):
    """A1 §3.4: block cache min(8 %, 64 MiB), 2 x 16 MiB memtables, the rest to coins."""
    block_cache = min(int(0.08 * dbcache_mib * MIB), 64 * MIB)
    engine = block_cache + 32 * MIB
    return dbcache_mib * MIB - engine, block_cache, engine


DBCACHE = {"p300": 300, "p300u": 300, "l300": 300, "p4096": 4096, "l4096": 4096}


def cell_config(cell):
    workload, setting = cell.split("-")
    trace, meta = trace_for(workload)
    peak = meta["peak_live_coins"]
    if setting not in DBCACHE:
        raise SystemExit(f"unknown setting {setting}")
    coins, bc, engine = literal_split(DBCACHE[setting])
    rho300 = (literal_split(300)[0] / ENTRY_COST) / FULL_CHAIN_COINS
    if setting in ("p300", "p300u"):
        coins = round(rho300 * peak * ENTRY_COST / MIB) * MIB
    elif setting == "p4096":
        # Everything fits: no flush before the end of the trace.
        coins = round(1.25 * peak * ENTRY_COST / MIB) * MIB
    max_entries = int(1.25 * peak)
    table = int(subprocess.check_output([BIN, "table-bytes", "--coins-budget", str(coins),
                                         "--max-entries", str(max_entries)]))
    allowance = int(0.5 * peak * STORED_ENTRY_BYTES)
    if workload == "wt":
        # W-T's smaller cap left too little room for dirty and writeback pages on the busy disk
        # (OOM kills with ~110 MiB anonymous memory): a floor for this informational workload.
        allowance = max(allowance, 64 * MIB)
    # Harness memory: the table, the flush sort index (16 B per entry), coins stored out of line
    # and the process itself.
    harness = table + 16 * max_entries + HARNESS_OVERHEAD
    memory_max = None if setting == "p300u" else harness + engine + allowance
    return {
        "cell": cell, "trace": str(trace), "peak_live_coins": peak, "rho300": rho300,
        "rho": (coins / ENTRY_COST) / peak, "coins_budget": coins, "block_cache": bc,
        "engine_budget": engine, "max_entries": max_entries, "table_bytes": table,
        "harness_bytes": harness, "page_cache_allowance": allowance, "memory_max": memory_max,
        "full_chain_coins": FULL_CHAIN_COINS,
    }


# ---- noise record ----

def read(path):
    try:
        return Path(path).read_text()
    except OSError:
        return None


def cpu_times():
    out = {}
    for line in read("/proc/stat").splitlines():
        f = line.split()
        if f[0].startswith("cpu") and f[0] != "cpu":
            v = list(map(int, f[1:]))
            out[int(f[0][3:])] = {"busy": sum(v) - v[3] - v[4], "total": sum(v), "steal": v[7]}
    return out


def disk_stats(dev="sdb"):
    for line in read("/proc/diskstats").splitlines():
        f = line.split()
        if f[2] == dev:
            return {"reads": int(f[3]), "read_sectors": int(f[5]), "writes": int(f[7]),
                    "write_sectors": int(f[9]), "io_ms": int(f[12])}
    return {}


def oracle_cgroups():
    out = {}
    for unit in ["dash-oracle-mainnet", "dash-oracle-testnet"]:
        try:
            cg = subprocess.check_output(["systemctl", "--user", "show", "-p", "ControlGroup",
                                          "--value", unit], text=True).strip()
        except subprocess.CalledProcessError:
            continue
        base = Path("/sys/fs/cgroup") / cg.lstrip("/")
        cpu = dict(l.split() for l in (read(base / "cpu.stat") or "").splitlines())
        io = {}
        for line in (read(base / "io.stat") or "").splitlines():
            for kv in line.split()[1:]:
                k, v = kv.split("=")
                io[k] = io.get(k, 0) + int(v)
        out[unit] = {"usage_usec": int(cpu.get("usage_usec", 0)), "wbytes": io.get("wbytes", 0),
                     "rbytes": io.get("rbytes", 0)}
    return out


def snapshot():
    return {
        "time": time.time(), "loadavg": read("/proc/loadavg").strip(),
        "pressure": {k: read(f"/proc/pressure/{k}") for k in ["cpu", "io", "memory"]},
        "cpus": cpu_times(), "sdb": disk_stats(), "oracles": oracle_cgroups(),
        "top": subprocess.run(["ps", "-eo", "pcpu,rss,comm", "--sort=-pcpu"], capture_output=True,
                              text=True).stdout.splitlines()[1:6],
    }


def noise(before, after):
    dt = after["time"] - before["time"]
    lo, hi = (int(x) for x in CPUS.split("-"))
    busy = total = steal = 0
    for c in range(lo, hi + 1):
        busy += after["cpus"][c]["busy"] - before["cpus"][c]["busy"]
        total += after["cpus"][c]["total"] - before["cpus"][c]["total"]
        steal += after["cpus"][c]["steal"] - before["cpus"][c]["steal"]
    d = {k: after["sdb"].get(k, 0) - before["sdb"].get(k, 0) for k in after["sdb"]}
    oracles = {u: {k: after["oracles"][u][k] - before["oracles"].get(u, {}).get(k, 0)
                   for k in after["oracles"][u]} for u in after["oracles"]}
    return {
        "start": before["time"], "end": after["time"],
        "seconds": dt, "loadavg_before": before["loadavg"], "loadavg_after": after["loadavg"],
        "pressure_before": before["pressure"], "pressure_after": after["pressure"],
        "cpus_24_31_busy_frac": busy / total if total else None,
        "cpus_24_31_steal_frac": steal / total if total else None,
        "sdb_util_frac": d.get("io_ms", 0) / 1000 / dt if dt else None,
        "sdb_read_mib_s": d.get("read_sectors", 0) * 512 / MIB / dt if dt else None,
        "sdb_write_mib_s": d.get("write_sectors", 0) * 512 / MIB / dt if dt else None,
        "oracles": oracles, "top_at_start": before["top"],
    }


# ---- runs ----

def scoped(cmd, memory_max, log):
    props = ["-p", f"AllowedCPUs={CPUS}", "-p", "AllowedMemoryNodes=1", "-p", "IOAccounting=yes",
             "-p", "CPUAccounting=yes", "-p", "MemoryAccounting=yes", "-p", "MemorySwapMax=0"]
    if memory_max:
        props += ["-p", f"MemoryMax={memory_max}"]
    full = ["sudo", "systemd-run", "--scope", "--quiet", f"--uid={os.getuid()}",
            f"--gid={os.getgid()}", *props, "--", *map(str, cmd)]
    with open(log, "w") as f:
        return subprocess.run(full, stdout=f, stderr=subprocess.STDOUT).returncode


def replay_run(cfg, engine, rotation, order, tag="", keep_db=False):
    cell_dir = RESULTS / cfg["cell"]
    cell_dir.mkdir(parents=True, exist_ok=True)
    name = f"{engine}-r{rotation}{tag}"
    db = DB / f"{cfg['cell']}-{name}"
    shutil.rmtree(db, ignore_errors=True)
    out = cell_dir / f"{name}.harness.json"
    block_cache = cfg["block_cache"]
    for part in engine.split("-"):
        if part.startswith("bc") and part[2:].isdigit():
            block_cache = int(part[2:]) * MIB
    if "direct" in engine.split("-"):
        # Direct I/O cannot use the page cache: give its allowance to the block cache.
        block_cache += cfg["page_cache_allowance"]
    cmd = [BIN, "replay", "--engine", engine, "--trace", cfg["trace"], "--db", db,
           "--coins-budget", cfg["coins_budget"], "--max-entries", cfg["max_entries"],
           "--block-cache", block_cache, "--engine-budget", cfg["engine_budget"],
           "--out", out]
    before = snapshot()
    t0 = time.time()
    rc = scoped(cmd, cfg["memory_max"], cell_dir / f"{name}.log")
    wall = time.time() - t0
    after = snapshot()
    result = {"cell": cfg["cell"], "engine": engine, "rotation": rotation, "order": order,
              "returncode": rc, "wall_s": wall, "config": cfg, "noise": noise(before, after),
              "harness": json.loads(out.read_text()) if out.exists() else None}
    for opt in db.glob("OPTIONS-*"):
        shutil.copy(opt, cell_dir / f"{name}.rocksdb-{opt.name}")
    (cell_dir / f"{name}.json").write_text(json.dumps(result, indent=1))
    total = result["harness"]["time_total_s"] if result["harness"] else None
    print(f"{cfg['cell']} {name}: rc={rc} wall={wall:.1f}s time_total_s={total}", flush=True)
    if not keep_db:
        shutil.rmtree(db, ignore_errors=True)
    return db, result


def rotations(n, engines=None):
    engines = engines or ENGINES
    for r in range(n):
        k = r % len(engines)
        yield r, engines[k:] + engines[:k]


def cmd_cells(cells, n, engines=None):
    for cell in cells:
        cfg = cell_config(cell)
        print(json.dumps({k: v for k, v in cfg.items() if k != "trace"}), flush=True)
        for r, order in rotations(n, engines):
            for engine in order:
                done = RESULTS / cell / f"{engine}-r{r}.json"
                if done.exists() and json.loads(done.read_text())["returncode"] == 0:
                    continue  # resume: this run already succeeded
                replay_run(cfg, engine, r, order)


def cmd_pilot():
    cfg = cell_config("wm-p300")
    for engine in ENGINES:
        replay_run(cfg, engine, 0, ENGINES, tag="-pilot")


def cmd_micro(n, engines=None):
    cfg = cell_config("wm-l300")
    for r, order in rotations(n, engines):
        for engine in order:
            db, res = replay_run(cfg, engine, r, order, tag="-micro", keep_db=True)
            largest = res["harness"]["replay"]["largest_flush_bytes"]
            out = RESULTS / "micro" / f"{engine}-r{r}.json"
            out.parent.mkdir(parents=True, exist_ok=True)
            cmd = [BIN, "micro", "--engine", engine, "--db", db, "--block-cache", cfg["block_cache"],
                   "--engine-budget", cfg["engine_budget"], "--largest-flush", largest, "--out", out]
            before = snapshot()
            # D3 holds up to 256 MiB of pending batch: give the run 2 GiB on top of the cell cap.
            rc = scoped(cmd, cfg["memory_max"] + 2048 * MIB, out.with_suffix(".log"))
            after = snapshot()
            meta = {"returncode": rc, "noise": noise(before, after)}
            out.with_suffix(".noise.json").write_text(json.dumps(meta, indent=1))
            print(f"micro {engine} r{r}: rc={rc}", flush=True)
            shutil.rmtree(db, ignore_errors=True)


def cmd_diskfull():
    out_dir = RESULTS / "diskfull"
    out_dir.mkdir(parents=True, exist_ok=True)
    _, bc, engine_budget = literal_split(300)
    for engine in ENGINES:
        img = ROOT / f"diskfull-{engine}.img"
        mnt = Path(f"/mnt/st1-diskfull-{engine}")
        subprocess.run(["truncate", "-s", "512M", img], check=True)
        subprocess.run(["mkfs.xfs", "-q", "-f", img], check=True)
        subprocess.run(["sudo", "mkdir", "-p", mnt], check=True)
        subprocess.run(["sudo", "mount", "-o", "loop", img, mnt], check=True)
        try:
            subprocess.run(["sudo", "chown", f"{os.getuid()}:{os.getgid()}", mnt], check=True)
            ballast = mnt / "ballast"
            # Larger than RocksDB's 64 MiB recovery reserve, so freeing it lets RocksDB resume by
            # itself if the engine does not fail-stop (D4 retries after 12 s; r3 M1).
            subprocess.run(["fallocate", "-l", "128M", ballast], check=True)
            out = out_dir / f"{engine}.json"
            cmd = [BIN, "diskfull", "--engine", engine, "--dir", mnt / "db", "--ballast", ballast,
                   "--block-cache", bc, "--engine-budget", engine_budget, "--out", out]
            rc = scoped(cmd, 2048 * MIB, out.with_suffix(".log"))
            print(f"diskfull {engine}: rc={rc}", flush=True)
        finally:
            subprocess.run(["sudo", "umount", mnt], check=False)
            img.unlink(missing_ok=True)


# ---- summary ----

def sign_test(ratios):
    """Exact two-sided sign test of "the paired ratio is equally likely above and below 1".

    With 2-5 pairs a bootstrap interval of the median is just the range of the pairs (its 2.5 and
    97.5 percentiles are the minimum and maximum), so the summary reports the range and this test
    instead (ST1 review r3 m7). All 5 pairs on one side gives p = 0.0625, the smallest possible at
    n = 5; pairs are also not independent under load drift.
    """
    above = sum(r > 1 for r in ratios)
    below = sum(r < 1 for r in ratios)
    n = above + below
    if n == 0:
        return 1.0
    tail = sum(math.comb(n, i) for i in range(min(above, below) + 1)) / 2 ** n
    return min(1.0, 2 * tail)


METRICS = {
    "time_total_s": lambda h: h["time_total_s"],
    "time_pipelined_est_s": lambda h: h["replay"]["time_pipelined_est_s"] + h["quiesce_s"]
    + h.get("close_s", 0),
    "flush_stall_p99_ms": lambda h: h["replay"]["flush_stall_ms"]["p99"],
    "miss_read_p99_us": lambda h: h["replay"]["miss_read_us"]["p99"],
    "miss_read_p50_us": lambda h: h["replay"]["miss_read_us"]["p50"],
    "db_bytes": lambda h: h["db_bytes"],
    "db_bytes_compacted": lambda h: (h["engine_extra"].get("live_bytes") or h["db_bytes_compacted"])
    if isinstance(h["engine_extra"], dict) else h["db_bytes_compacted"],
    "rss_anon_peak": lambda h: h["memory"]["rss_anon_peak"],
    "engine_anon_peak": lambda h: h["memory"]["rss_anon_peak"] - h["replay"]["harness_table_bytes"],
    "cg_mem_peak": lambda h: h["cgroup_end"]["memory_peak"] or 0,
    "rss_file_peak": lambda h: h["memory"]["rss_file_peak"],
    "cg_file_peak": lambda h: h["memory"]["cg_file_peak"],
    "io_write_bytes": lambda h: h["cgroup_end"]["io_wbytes"] - h["cgroup_start"]["io_wbytes"],
    "cpu_per_event_ns": lambda h: h["cpu_per_event_ns"],
}


def cmd_summary():
    summary = {}
    for cell_dir in sorted(RESULTS.iterdir()):
        if not cell_dir.is_dir() or cell_dir.name in ("micro", "diskfull"):
            continue
        # Only the matrix runs (`<engine>-r<n>.json`), not pilot or micro runs.
        all_runs = [json.loads(p.read_text()) for p in cell_dir.glob("*-r*.json")
                    if re.fullmatch(r"[a-z0-9-]+-r\d+\.json", p.name)]
        runs = [r for r in all_runs if r["harness"] and r["returncode"] == 0]
        failed = {}
        for r in all_runs:
            if not (r["harness"] and r["returncode"] == 0):
                failed[r["engine"]] = failed.get(r["engine"], 0) + 1
        cell = {"failed_runs": failed}
        for engine in sorted({r["engine"] for r in runs}):
            er = [r for r in runs if r["engine"] == engine]
            m = {}
            for name, f in METRICS.items():
                vals = [f(r["harness"]) for r in er]
                m[name] = {"median": statistics.median(vals), "min": min(vals), "max": max(vals)}
            t = m["time_total_s"]
            t["spread"] = t["max"] / t["min"] - 1 if t["min"] else None
            m["n"] = len(er)
            m["busy_frac_median"] = statistics.median(
                r["noise"]["cpus_24_31_busy_frac"] for r in er)
            cell[engine] = m
        # Paired ratios against RocksDB, per rotation.
        by_rot = {}
        for r in runs:
            by_rot.setdefault(r["rotation"], {})[r["engine"]] = r["harness"]
        for engine in sorted({r["engine"] for r in runs} - {"rocksdb"}):
            for name in ["time_total_s", "time_pipelined_est_s", "cpu_per_event_ns"]:
                ratios = [METRICS[name](v[engine]) / METRICS[name](v["rocksdb"])
                          for v in by_rot.values() if engine in v and "rocksdb" in v]
                if ratios:
                    cell.setdefault("paired_vs_rocksdb", {}).setdefault(engine, {})[name] = {
                        "median": statistics.median(ratios), "pairs": len(ratios),
                        "range": [min(ratios), max(ratios)], "sign_p": sign_test(ratios),
                        "ratios": ratios}
        summary[cell_dir.name] = cell
    (RESULTS / "summary.json").write_text(json.dumps(summary, indent=1))
    print(json.dumps(summary, indent=1))


def fmt(v, unit):
    if unit == "MiB":
        return f"{v / MIB:.0f}"
    if unit == "s":
        return f"{v:.1f}"
    return f"{v:.0f}" if v >= 100 else f"{v:.1f}"


REPORT_METRICS = [
    ("time_total_s", "time (s)", "s"),
    ("cpu_per_event_ns", "CPU/event (ns)", ""),
    ("flush_stall_p99_ms", "flush p99 (ms)", ""),
    ("miss_read_p50_us", "miss p50 (µs)", ""),
    ("miss_read_p99_us", "miss p99 (µs)", ""),
    ("db_bytes", "size (MiB)", "MiB"),
    ("db_bytes_compacted", "compacted/live (MiB)", "MiB"),
    ("rss_anon_peak", "RssAnon (MiB)", "MiB"),
    ("rss_file_peak", "RssFile (MiB)", "MiB"),
    ("io_write_bytes", "written (MiB)", "MiB"),
]


def cmd_report():
    """Markdown tables for ST1-bakeoff.md §9 from summary.json, micro/ and diskfull/."""
    summary = json.loads((RESULTS / "summary.json").read_text())
    out = []
    for cell, data in summary.items():
        engines = [e for e in data if e not in ("failed_runs", "paired_vs_rocksdb")]
        if not engines:
            continue  # e.g. wm-l300, which holds only the micro runs' replays
        out.append(f"\n**{cell}** (median [min–max]; n runs; failed: {data['failed_runs'] or 'none'})\n")
        out.append("| metric | " + " | ".join(engines) + " |")
        out.append("|---" * (len(engines) + 1) + "|")
        out.append("| n | " + " | ".join(str(data[e]["n"]) for e in engines) + " |")
        for key, label, unit in REPORT_METRICS:
            cells = []
            for e in engines:
                m = data[e][key]
                cells.append(f"{fmt(m['median'], unit)} [{fmt(m['min'], unit)}–{fmt(m['max'], unit)}]")
            out.append(f"| {label} | " + " | ".join(cells) + " |")
        out.append("| time spread | " + " | ".join(
            f"{data[e]['time_total_s']['spread'] * 100:.0f} %" for e in engines) + " |")
        out.append("| CPUs 24-31 busy | " + " | ".join(
            f"{data[e]['busy_frac_median'] * 100:.0f} %" for e in engines) + " |")
        for e, ratios in data.get("paired_vs_rocksdb", {}).items():
            t = ratios["time_total_s"]
            c = ratios["cpu_per_event_ns"]
            out.append(f"\n- {e} / rocksdb, {t['pairs']} pairs by rotation: time {t['median']:.2f} "
                       f"(range {t['range'][0]:.2f}–{t['range'][1]:.2f}, sign test p = "
                       f"{t['sign_p']:.3f}); CPU per event {c['median']:.2f}")
    print("\n".join(out))


def main():
    args = sys.argv[1:]
    if not args:
        raise SystemExit(__doc__)
    rot = int(args[args.index("--rotations") + 1]) if "--rotations" in args else 3
    if args[0] == "pilot":
        cmd_pilot()
    elif args[0] == "cells":
        engines = args[args.index("--engines") + 1].split(",") if "--engines" in args else None
        cmd_cells(args[1].split(","), rot, engines)
    elif args[0] == "micro":
        engines = args[args.index("--engines") + 1].split(",") if "--engines" in args else None
        cmd_micro(rot, engines)
    elif args[0] == "diskfull":
        cmd_diskfull()
    elif args[0] == "summary":
        cmd_summary()
    elif args[0] == "report":
        cmd_report()
    else:
        raise SystemExit(__doc__)


if __name__ == "__main__":
    main()
