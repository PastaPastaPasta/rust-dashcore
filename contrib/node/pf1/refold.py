#!/usr/bin/env python3
"""PF1: re-run the perf_fold step for run directories made by run_window.py (after changing the classifier)."""
import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
for run in sys.argv[1:]:
    meta = json.load(open(os.path.join(run, "run.json")))
    w = meta.get("window")
    if not os.path.exists(os.path.join(run, "perf.data")):
        print("skip", run)
        continue
    if w:
        off = meta["clock_offset_realtime_minus_monotonic"]
        window = [repr(w["t_first_realtime"] - off), repr(w["t_last_realtime"] - off)]
    else:  # run_regtest.py: perf's monotonic clock windows recorded around the measured work (none: whole lifetime)
        ws = [x for x in meta.get("recorded_windows_monotonic", []) if x[1]]
        window = [repr(min(x[0] for x in ws)), repr(max(x[1] for x in ws))] if ws else []
    scr = subprocess.Popen(["perf", "script", "-i", os.path.join(run, "perf.data"), "-F",
                            "comm,tid,time,event,period,ip,sym,dso", "--no-inline",
                            *(["--time", f"{float(window[0]):.9f},{float(window[1]):.9f}"]
                              if window and meta.get("callgraph") == "dwarf" else [])], stdout=subprocess.PIPE,
                           stderr=subprocess.DEVNULL, text=True, errors="replace")
    fold = subprocess.run([sys.executable, os.path.join(HERE, "perf_fold.py"), "--out", os.path.join(run, "fold"),
                           *(["--window"] + window if window else [])],
                          stdin=scr.stdout, capture_output=True, text=True)
    scr.wait()
    meta["fold_stderr"] = fold.stderr.strip().splitlines()
    json.dump(meta, open(os.path.join(run, "run.json"), "w"), indent=1)
    print(run, *meta["fold_stderr"])
