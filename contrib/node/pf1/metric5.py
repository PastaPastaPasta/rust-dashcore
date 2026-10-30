#!/usr/bin/env python3
"""PF1 / DEC-108: dashd per-block connect latency (PRD metric #5) from snapshot windows, no perf attached.

    metric5.py --dashd BIN --snaps DIR --out DIR [--windows 421000 925000 ...] [--blocks 2000] [--rounds 4]
               [--cpus 16-27] [--par 11] [--dbcache 263] [--configs av noav]

For every round, window and config: reflink-clone SNAPS/snap-W, run `dashd -debug=bench -stopatheight=W+N`, parse the
per-block bench lines. Two latencies per block, from dashd's own timers:

  connect_tip_ms   the log's top-level `- Connect block: X ms` = all of ConnectTip: block read, ConnectBlock, the
                   coins-view + evodb flush (`Flush`), the IF_NEEDED disk flush (`Writing chainstate`), UpdateTip
                   and post-processing. This is the full latency.
  connect_total_ms the log's `- Connect total: X ms` = ConnectBlock only (validation). It is logged before the
                   coins-view write-back (validation.cpp: `Connect total` precedes `view.Flush()`). This is the
                   narrow latency.

Per cell: p50/p95/p99/max over all blocks of all rounds (first --skip blocks of every run dropped as cold start), the
per-round p99 and max, and the load average around each run. The coins cache is `-dbcache` minus 10 MiB with txindex
off; 263 -> 253.0 MiB against B_DEC43 = 252.75 MiB (DEC-43: dbcache 300, txindex on). The run records the log's
"Using X MiB for in-memory UTXO set" line so the actual budget is on file.
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time

TIP = re.compile(r"UpdateTip: new best=\S+ height=(\d+) ")
CONNECT_BLOCK = re.compile(r"\[bench\] - Connect block: ([\d.]+)ms")
CONNECT_TOTAL = re.compile(r"\[bench\]\s+- Connect total: ([\d.]+)ms")
CACHE = re.compile(r"Using ([\d.]+) MiB for in-memory UTXO set \(plus up to ([\d.]+) MiB")


def pct(v, p):
    v = sorted(v)
    if not v:
        return None
    k = (len(v) - 1) * p / 100
    lo, hi = int(k), min(int(k) + 1, len(v) - 1)
    return v[lo] + (v[hi] - v[lo]) * (k - lo)


def stats(v):
    return {"n": len(v), "p50": pct(v, 50), "p95": pct(v, 95), "p99": pct(v, 99), "max": max(v) if v else None,
            "mean": sum(v) / len(v) if v else None}


def parse(log, start, stop, skip):
    """Per block (height in (start, stop]): [connect_tip_ms, connect_total_ms], in log order."""
    out, total, height = [], None, None
    cache = None
    for line in open(log, errors="replace"):
        m = CACHE.search(line)
        if m:
            cache = (float(m.group(1)), float(m.group(2)))
        m = TIP.search(line)
        if m:
            height = int(m.group(1))
            continue
        m = CONNECT_TOTAL.search(line)
        if m:
            total = float(m.group(1))
            continue
        m = CONNECT_BLOCK.search(line)
        if m and height is not None and start < height <= stop:
            out.append((height, float(m.group(1)), total))
            total = None
    return [r for r in out if r[0] > start + skip], cache


def loadavg():
    return open("/proc/loadavg").read().split()[0]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dashd", required=True)
    ap.add_argument("--snaps", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--windows", type=int, nargs="+", default=[421000, 925000, 1140000, 1340000, 1520000])
    ap.add_argument("--blocks", type=int, default=2000)
    ap.add_argument("--rounds", type=int, default=4)
    ap.add_argument("--skip", type=int, default=10)
    ap.add_argument("--cpus", default="16-27")
    ap.add_argument("--par", type=int, default=11)
    ap.add_argument("--dbcache", type=int, default=263)
    ap.add_argument("--configs", nargs="+", default=["av", "noav"], choices=["av", "noav"])
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    res_path = os.path.join(a.out, "metric5.json")
    res = json.load(open(res_path)) if os.path.exists(res_path) else {"runs": {}}
    res["params"] = {k: v for k, v in vars(a).items()}
    for rnd in range(1, a.rounds + 1):
        for w in a.windows:
            for cfg in a.configs:
                key = f"w{w}-{cfg}-r{rnd}"
                prev = res["runs"].get(key)
                if prev and prev["exit"] == 0 and prev["blocks"] == a.blocks - a.skip:
                    continue  # resume: redo runs that failed or came out short
                data = os.path.join(a.out, "datadir")
                shutil.rmtree(data, ignore_errors=True)
                subprocess.run(["cp", "-a", "--reflink=always", os.path.join(a.snaps, f"snap-{w}"), data], check=True)
                log = os.path.join(data, "testnet3", "debug.log")
                if os.path.exists(log):
                    os.unlink(log)
                stop = w + a.blocks
                argv = [a.dashd, "-testnet", f"-datadir={data}", "-connect=0", "-listen=0", "-dnsseed=0",
                        "-fixedseeds=0", "-disablewallet", "-txindex=0", "-disablegovernance", "-server=0",
                        "-logtimemicros=1", "-printtoconsole=0", f"-dbcache={a.dbcache}", "-maxmempool=5",
                        f"-par={a.par}", f"-parbls={a.par}", "-checkblockindex=0", "-upnp=0", "-natpmp=0",
                        "-listenonion=0", "-debug=bench", f"-stopatheight={stop}"]
                if cfg == "noav":
                    argv.append("-assumevalid=0")
                l0, t0 = loadavg(), time.time()
                rc = subprocess.run(["taskset", "-c", a.cpus] + argv, stdout=subprocess.DEVNULL,
                                    stderr=subprocess.DEVNULL).returncode
                wall = time.time() - t0
                rows, cache = parse(log, w, stop, a.skip) if os.path.exists(log) else ([], None)
                res["runs"][key] = {"exit": rc, "wall_s": round(wall, 1), "load_before": l0, "load_after": loadavg(),
                                    "cache_mib": cache, "blocks": len(rows),
                                    "tip": [r[1] for r in rows], "total": [r[2] for r in rows if r[2] is not None],
                                    "heights": [rows[0][0], rows[-1][0]] if rows else None}
                shutil.rmtree(data, ignore_errors=True)
                json.dump(res, open(res_path, "w"))
                print(key, rc, len(rows), round(wall, 1), flush=True)
    # summary per cell
    cells = {}
    for key, r in res["runs"].items():
        w, cfg, _ = key.split("-")
        c = cells.setdefault(f"{w}-{cfg}", {"tip": [], "total": [], "rounds": []})
        c["tip"] += r["tip"]
        c["total"] += r["total"]
        c["rounds"].append({"key": key, "p99_tip": pct(r["tip"], 99), "max_tip": max(r["tip"]) if r["tip"] else None,
                            "p99_total": pct(r["total"], 99), "load": [r["load_before"], r["load_after"]]})
    res["summary"] = {k: {"connect_tip_ms": stats(v["tip"]), "connect_total_ms": stats(v["total"]),
                          "rounds": v["rounds"]} for k, v in cells.items()}
    json.dump(res, open(res_path, "w"))
    print(json.dumps({k: {"tip": {x: round(y, 2) if isinstance(y, float) else y for x, y in v["connect_tip_ms"].items()},
                          "total": {x: round(y, 2) if isinstance(y, float) else y for x, y in v["connect_total_ms"].items()}}
                      for k, v in res["summary"].items()}, indent=1))


if __name__ == "__main__":
    sys.exit(main())
