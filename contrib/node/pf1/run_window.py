#!/usr/bin/env python3
"""PF1: profile dashd connecting a fixed block window, from a reflink snapshot of its datadir.

    run_window.py --dashd BIN --snap SNAPDIR --start W --blocks N --out OUTDIR
                  [--par 1] [--assumevalid default|0] [--dbcache 1024] [--bench 1] [--cpus 16-27]
                  [--mode record|stat] [--period 1000003] [--freq 997]

SNAPDIR is a datadir whose chain tip is block W (see walk-snapshots.sh). The run clones it, starts dashd with
`-connect=0 -stopatheight=W+N` and measures from the log line of block W+1 to the one of block W+N (so the timed blocks are W+2..W+N), so node
startup, the first block's cold effects and shutdown are excluded. `--mode record` runs under perf record
(instructions:u with a fixed period, plus cpu-clock, call graphs by frame pointer, perf's monotonic clock);
`--mode stat` runs under perf stat and reports whole-process counters (startup and shutdown included).

Outputs in OUTDIR: run.json (command line, counters, window, provenance), debug.log (the window), perf.data and
the folded/subsystem tables (record mode).
"""

import argparse
import datetime
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))

import fcntl

PERF_LOCK = os.environ.get("PF1_PERF_LOCK", "/work/scratch/pf1/perf.lock")


def perf_lock():
    """Hold an exclusive lock for the life of the process: concurrent perf sessions exhaust perf_event_mlock_kb."""
    f = open(PERF_LOCK, "w")
    fcntl.flock(f, fcntl.LOCK_EX)
    return f

UPDATE_TIP = re.compile(r"^(\S+) UpdateTip: new best=\S+ height=(\d+) ")


def iso_to_epoch(s):
    return datetime.datetime.fromisoformat(s.replace("Z", "+00:00")).timestamp()


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def perf_throttle_events(perf_data):
    """Count THROTTLE events in perf.data; any means samples were dropped and the instruction total is an undercount."""
    out = subprocess.run(["perf", "report", "-i", perf_data, "--stats"], capture_output=True, text=True).stdout
    m = re.search(r"^\s*THROTTLE events:\s+(\d+)", out, re.M)
    return int(m.group(1)) if m else 0


def find_pid(parent):
    try:
        out = subprocess.run(["pgrep", "-P", str(parent), "-x", "dashd|dashd-v24.0.0-rc.3-sym"], capture_output=True,
                             text=True).stdout.split()
        if out:
            return int(out[0])
        for pid in subprocess.run(["pgrep", "-P", str(parent)], capture_output=True, text=True).stdout.split():
            return find_pid(int(pid)) or int(pid)
    except Exception:
        pass
    return None


def watch_rss(parent, state, stop):
    while not stop.is_set():
        pid = find_pid(parent)
        if pid:
            try:
                for line in open(f"/proc/{pid}/status"):
                    if line.startswith("VmHWM:"):
                        state["peak_rss_kib"] = max(state.get("peak_rss_kib", 0), int(line.split()[1]))
            except OSError:
                pass
        stop.wait(0.5)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dashd", required=True)
    ap.add_argument("--snap", required=True)
    ap.add_argument("--start", type=int, required=True)
    ap.add_argument("--blocks", type=int, required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--par", type=int, default=1)
    ap.add_argument("--parbls", type=int, default=1)
    ap.add_argument("--assumevalid", default="default")
    ap.add_argument("--dbcache", type=int, default=1024)
    ap.add_argument("--bench", type=int, default=1)
    ap.add_argument("--cpus", default="16-27")
    ap.add_argument("--mode", choices=["record", "stat"], default="record")
    ap.add_argument("--period", type=int, default=6000011,
                    help="instructions:u sample period; keep the sample rate under kernel.perf_event_max_sample_rate "
                         "(2000 Hz on agentbox) or perf throttles and drops samples")
    ap.add_argument("--freq", type=int, default=997)
    ap.add_argument("--callgraph", choices=["fp", "dwarf"], default="fp",
                    help="dwarf: unwind with DWARF CFI from 16 KiB stack copies (resolves BLS/GMP stacks that frame "
                         "pointers lose); use with a larger --period")
    ap.add_argument("--net", choices=["testnet", "regtest"], default="testnet")
    ap.add_argument("--reindex-chainstate", action="store_true",
                    help="rebuild the chainstate from SNAPDIR's block files (use with --start below the interesting blocks)")
    ap.add_argument("--extra", nargs="*", default=[])
    ap.add_argument("--keep-datadir", action="store_true")
    a = ap.parse_args()
    lock = perf_lock()  # noqa: F841

    os.makedirs(a.out, exist_ok=True)
    data = os.path.join(a.out, "datadir")
    shutil.rmtree(data, ignore_errors=True)
    subprocess.run(["cp", "-a", "--reflink=always", a.snap, data], check=True)
    a.chain = "testnet3" if a.net == "testnet" else "regtest"
    log = os.path.join(data, a.chain, "debug.log")
    if os.path.exists(log):
        os.unlink(log)
    stop = a.start + a.blocks
    argv = [a.dashd, f"-{a.net}", f"-datadir={data}", "-connect=0", "-listen=0", "-dnsseed=0", "-fixedseeds=0",
            "-disablewallet", "-txindex=0", "-disablegovernance", "-server=0", "-logtimemicros=1",
            "-printtoconsole=0", f"-dbcache={a.dbcache}", f"-par={a.par}", f"-parbls={a.parbls}", "-checkblockindex=0", "-upnp=0",
            "-natpmp=0", "-listenonion=0", f"-stopatheight={stop}"]
    if a.net == "regtest":
        argv += ["-persistmempool=0", "-checkmempool=0", "-maxtipage=2000000000"]
    if a.reindex_chainstate:
        argv.append("-reindex-chainstate")
    if a.assumevalid != "default":
        argv.append(f"-assumevalid={a.assumevalid}")
    if a.bench:
        argv.append("-debug=bench")
    argv += a.extra
    perf_data = os.path.join(a.out, "perf.data")
    if a.mode == "record":
        cg = ["-g"] if a.callgraph == "fp" else ["--call-graph", "dwarf,16384"]
        wrap = ["perf", "record", "-o", perf_data, "-k", "monotonic", *cg, "-e", f"instructions/period={a.period}/u",
                "-e", f"cpu-clock/freq={a.freq}/", "--"]
    else:
        stat_csv = os.path.join(a.out, "stat.csv")
        wrap = ["perf", "stat", "-x,", "-o", stat_csv, "-e", "instructions:u,cycles:u,task-clock,page-faults,"
                "context-switches", "--"]
    cmd = ["taskset", "-c", a.cpus] + wrap + argv
    offset = time.clock_gettime(time.CLOCK_REALTIME) - time.clock_gettime(time.CLOCK_MONOTONIC)
    t_spawn = time.time()
    state = {}
    with open(os.path.join(a.out, "stdout.log"), "w") as so:
        proc = subprocess.Popen(cmd, stdout=so, stderr=subprocess.STDOUT)
    ev = threading.Event()
    th = threading.Thread(target=watch_rss, args=(proc.pid, state, ev), daemon=True)
    th.start()
    rc = proc.wait()
    ev.set()
    t_exit = time.time()

    t_first = t_last = None
    first_h = last_h = None
    ticks = {}
    if os.path.exists(log):
        with open(log, errors="replace") as f:
            for line in f:
                m = UPDATE_TIP.match(line)
                if m:
                    ticks[int(m.group(2))] = iso_to_epoch(m.group(1))
    if a.start + 1 in ticks and stop in ticks:
        t_first, t_last = ticks[a.start + 1], ticks[stop]
        first_h, last_h = a.start + 1, stop
    meta = {"cmd": cmd, "exit_code": rc, "dashd": a.dashd, "dashd_sha256": sha256(a.dashd), "snapshot": a.snap, "net": a.net, "reindex_chainstate": a.reindex_chainstate,
            "start_height": a.start, "stop_height": stop, "par": a.par, "parbls": a.parbls, "assumevalid": a.assumevalid,
            "dbcache_mib": a.dbcache, "bench_log": bool(a.bench), "cpus": a.cpus, "mode": a.mode, "callgraph": a.callgraph,
            "wall_total_s": round(t_exit - t_spawn, 3), "peak_rss_kib": state.get("peak_rss_kib"),
            "clock_offset_realtime_minus_monotonic": offset}
    if t_first is not None:
        meta["window"] = {"first_tip_height": first_h, "last_tip_height": last_h, "blocks_in_window": last_h - first_h,
                          "wall_s": round(t_last - t_first, 4), "t_first_realtime": t_first, "t_last_realtime": t_last}
    if rc != 0 or t_first is None:
        meta["error"] = "dashd/perf exited nonzero or the window ticks are missing"
    # The whole debug.log is kept (the per-block parsers select their own window).
    if os.path.exists(log):
        with open(log, errors="replace") as f, open(os.path.join(a.out, "debug.log"), "w") as o:
            for line in f:
                o.write(line)
    if a.mode == "stat" and os.path.exists(os.path.join(a.out, "stat.csv")):
        counters = {}
        for line in open(os.path.join(a.out, "stat.csv")):
            parts = line.strip().split(",")
            if len(parts) >= 3 and parts[0].replace(".", "").isdigit():
                counters[parts[2]] = float(parts[0])
        meta["counters"] = counters
    if a.mode == "record" and t_first is not None and rc == 0:
        t0, t1 = t_first - offset, t_last - offset
        # DWARF unwinding stops yielding frames on a whole-file pass after the startup samples (perf script quirk);
        # restricting the pass to the window with --time avoids it and is faster.
        tsel = ["--time", f"{t0:.9f},{t1:.9f}"] if a.callgraph == "dwarf" else []
        scr = subprocess.Popen(["perf", "script", "-i", perf_data, "-F", "comm,tid,time,event,period,ip,sym,dso",
                                "--no-inline", *tsel], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
                               errors="replace")
        fold = subprocess.run([sys.executable, os.path.join(HERE, "perf_fold.py"), "--out", os.path.join(a.out, "fold"),
                               "--window", repr(t0), repr(t1)], stdin=scr.stdout, capture_output=True, text=True)
        scr.wait()
        meta["fold_stderr"] = fold.stderr.strip().splitlines()
        meta["perf_throttle_events"] = perf_throttle_events(perf_data)
        if meta["perf_throttle_events"]:
            meta["error"] = "perf throttled: samples dropped, counts are an undercount"
    json.dump(meta, open(os.path.join(a.out, "run.json"), "w"), indent=1)
    if not a.keep_datadir:
        shutil.rmtree(data, ignore_errors=True)
    print(json.dumps({k: meta[k] for k in meta if k not in ("cmd", "fold_stderr")}, indent=1))
    if "fold_stderr" in meta:
        print("\n".join(meta["fold_stderr"]))
    return 1 if "error" in meta else 0


if __name__ == "__main__":
    sys.exit(main())
