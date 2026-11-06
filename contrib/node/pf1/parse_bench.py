#!/usr/bin/env python3
"""PF1: parse dashd `-debug=bench` lines of a run_window.py run into per-block phase costs.

    parse_bench.py RUN_DIR [RUN_DIR ...]      (reads RUN_DIR/debug.log and RUN_DIR/run.json, writes RUN_DIR/bench.json)

Blocks start+2..last are used (the ones connected between the two timed UpdateTip lines); each block's lines are keyed by its own UpdateTip height. The `[total s (ms/blk)]`
suffixes of dashd's lines are process-cumulative and ignored; the first `<x>ms` after the label is the cost of
that one block. Output per phase: occurrences, total ms, ms per block of the window, and the p50/p95 of the
per-occurrence values. Also window totals: blocks, transactions, tx inputs, and the 'Connect total' distribution.
"""

import json
import os
import re
import statistics
import sys

LINE = re.compile(r"^(?P<ts>\S+) \[bench\]\s+(?:- )?(?P<label>.*?): (?P<ms>[\d.]+)ms\b(?P<rest>.*)$")
TIP = re.compile(r"^(\S+) UpdateTip: new best=\S+ height=(\d+) .* tx=(\d+) ")
CONNECT = re.compile(r"^Connect (\d+) transactions$")
VERIFY = re.compile(r"^Verify (\d+) txins$")


def pct(v, p):
    v = sorted(v)
    return v[min(len(v) - 1, int(round(p / 100 * (len(v) - 1))))] if v else None


def parse(run):
    meta = json.load(open(os.path.join(run, "run.json")))
    w = meta["window"]
    lo, hi = w["first_tip_height"], w["last_tip_height"]
    # A block's lines straddle its UpdateTip line: load/connect/flush phases come before it, `Connect postprocess`
    # and the top-level `Connect block` after it. `Connect block` closes the block, so lines are collected until it
    # appears and attributed to the height of the UpdateTip seen meanwhile. The window is the blocks lo+1..hi, the
    # ones whose connect work happens between the two timed UpdateTip lines.
    vals, txs, txins, blocks = {}, 0, 0, 0
    cur, height = [], None
    for line in open(os.path.join(run, "debug.log"), errors="replace"):
        m = TIP.match(line)
        if m:
            height = int(m.group(2))
            continue
        m = LINE.match(line)
        if not m:
            continue
        label, ms = m["label"], float(m["ms"])
        c = CONNECT.match(label)
        if c:
            cur.append(("Connect transactions", ms, int(c.group(1)), 0))
            continue
        v = VERIFY.match(label)
        if v:
            cur.append(("Verify txins", ms, 0, int(v.group(1))))
            continue
        cur.append((label, ms, 0, 0))
        if label == "Connect block":  # closes the block
            if height is not None and lo < height <= hi:
                blocks += 1
                for lab, x, t, i in cur:
                    vals.setdefault(lab, []).append(x)
                    txs += t
                    txins += i
            cur = []
    phases = {}
    for k, v in vals.items():
        phases[k] = {"n": len(v), "total_ms": round(sum(v), 3), "ms_per_block": round(sum(v) / blocks, 4),
                     "p50_ms": pct(v, 50), "p95_ms": pct(v, 95), "max_ms": max(v)}
    out = {"blocks": blocks, "window_blocks_expected": w["blocks_in_window"], "txs_excl_nothing": txs, "txins": txins,
           "txs_per_block": round(txs / blocks, 2), "txins_per_block": round(txins / blocks, 2), "phases": phases}
    json.dump(out, open(os.path.join(run, "bench.json"), "w"), indent=1)
    return out


if __name__ == "__main__":
    for r in sys.argv[1:]:
        o = parse(r)
        ct = o["phases"].get("Connect total", {})
        print(f"{os.path.basename(r.rstrip("/"))}: {o['blocks']} blocks, {o['txs_per_block']} tx/blk, {o['txins_per_block']} txin/blk, "
              f"Connect total {ct.get('ms_per_block')} ms/blk (p50 {ct.get('p50_ms')}, p95 {ct.get('p95_ms')})")
