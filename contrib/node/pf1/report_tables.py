#!/usr/bin/env python3
"""PF1: summarise run directories (run_window.py / run_regtest.py) into summary.json and markdown tables.

    report_tables.py --out DIR RUN_DIR [RUN_DIR ...]

For every run: total instructions:u and CPU time in the recorded window, per-block / per-op figures, the
subsystem, thread-group, anchor and inclusive tables, the top leaf symbols, and (windows) the -debug=bench phase
table. summary.json holds the numbers; tables.md the same as markdown, one section per run.
"""

import argparse
import json
import os


def tsv(path, cols):
    rows = []
    if os.path.exists(path):
        for line in open(path):
            parts = line.rstrip("\n").split("\t")
            if len(parts) >= cols:
                rows.append(parts)
    return rows


def table(rows, headers, n=12):
    out = ["| " + " | ".join(headers) + " |", "|" + "|".join("---" for _ in headers) + "|"]
    for r in rows[:n]:
        out.append("| " + " | ".join(r) + " |")
    return "\n".join(out)


def human(x):
    x = float(x)
    for unit, div in (("G", 1e9), ("M", 1e6), ("k", 1e3)):
        if abs(x) >= div:
            return f"{x / div:.2f}{unit}"
    return f"{x:.0f}"


def summarize(run):
    name = os.path.basename(run.rstrip("/"))
    meta = json.load(open(os.path.join(run, "run.json")))
    fold = os.path.join(run, "fold")
    ins = tsv(os.path.join(fold, "instructions_u.total"), 4)
    clk = tsv(os.path.join(fold, "cpu-clock.total"), 4)
    s = {"run": name, "workload": meta.get("workload", "window"), "error": meta.get("error")}
    if ins:
        s["instructions"] = int(ins[0][0])
    ex = tsv(os.path.join(fold, "instructions_u.total_ex_bg"), 1)
    if ex and "window" in meta:  # windows: also the total without the unnamed LevelDB background thread
        s["instructions_ex_bg"] = int(ex[0][0])
    if clk:
        s["cpu_ms"] = int(clk[0][0]) / 1e6
    if "window" in meta:
        w = meta["window"]
        blocks = w["blocks_in_window"]  # last tip - first tip: the blocks connected inside the timed window
        s.update(blocks=blocks, wall_s=w["wall_s"], par=meta["par"], parbls=meta.get("parbls"),
                 assumevalid=meta["assumevalid"], bench=meta["bench_log"], start_height=meta["start_height"],
                 net=meta.get("net", "testnet"))
        if "instructions" in s:
            s["instr_per_block"] = s["instructions"] / blocks
            if "instructions_ex_bg" in s:
                s["instr_per_block_ex_bg"] = s["instructions_ex_bg"] / blocks
        if "cpu_ms" in s:
            s["cpu_ms_per_block"] = s["cpu_ms"] / blocks
            s["parallelism"] = round(s["cpu_ms"] / 1e3 / w["wall_s"], 2)
        s["wall_ms_per_block"] = w["wall_s"] * 1e3 / blocks
        bj = os.path.join(run, "bench.json")
        if os.path.exists(bj):
            b = json.load(open(bj))
            s["txs_per_block"], s["txins_per_block"] = b["txs_per_block"], b["txins_per_block"]
            s["bench_phases"] = {k: v["ms_per_block"] for k, v in b["phases"].items()}
            s["bench_p95"] = {k: v["p95_ms"] for k, v in b["phases"].items()}
    else:
        ops = meta.get("ops")
        s["ops"], s["unit"] = ops, meta.get("unit")
        if ops and "instructions" in s and s["workload"] not in ("startup-regtest", "startup-testnet"):
            s["instr_per_op"] = s["instructions"] / ops
        if ops and "cpu_ms" in s:
            s["cpu_ms_per_op"] = s["cpu_ms"] / ops
        for k in ("ready_s", "wall_total_s", "loop_wall_s", "latency_ms", "call_ms", "peak_rss_kib", "mempool_txs",
                  "template_txs", "response_bytes", "clients"):
            if k in meta:
                s[k] = meta[k]
    s["subsystems"] = {r[0]: float(r[2]) for r in tsv(os.path.join(fold, "instructions_u.subsystems.tsv"), 3)}
    s["subsystems_ex_bg"] = {r[0]: float(r[2]) for r in tsv(os.path.join(fold, "instructions_u.subsystems_ex_bg.tsv"), 3)}
    s["subsystems_cpu"] = {r[0]: float(r[2]) for r in tsv(os.path.join(fold, "cpu-clock.subsystems.tsv"), 3)}
    s["threads"] = {r[0]: float(r[2]) for r in tsv(os.path.join(fold, "instructions_u.threads.tsv"), 3)}
    s["threads_cpu"] = {r[0]: float(r[2]) for r in tsv(os.path.join(fold, "cpu-clock.threads.tsv"), 3)}
    s["anchors"] = {r[0]: float(r[2]) for r in tsv(os.path.join(fold, "instructions_u.anchors.tsv"), 3)}
    s["inclusive"] = {r[0]: float(r[2]) for r in tsv(os.path.join(fold, "instructions_u.inclusive.tsv"), 3)}
    s["inclusive_cpu"] = {r[0]: float(r[2]) for r in tsv(os.path.join(fold, "cpu-clock.inclusive.tsv"), 3)}
    s["top_symbols"] = [(r[0], float(r[2])) for r in tsv(os.path.join(fold, "instructions_u.top.tsv"), 3)[:15]]
    md = [f"### {name}", ""]
    head = []
    if "instr_per_block" in s:
        head.append(f"{s['blocks']} blocks, {human(s['instructions'])} instructions:u = **{human(s['instr_per_block'])}/block**")
        if "instr_per_block_ex_bg" in s:
            head.append(f"{human(s['instr_per_block_ex_bg'])}/block without the unnamed LevelDB background thread")
        if "txs_per_block" in s:
            head.append(f"{s['txs_per_block']} tx/blk, {s['txins_per_block']} txin/blk")
        head.append(f"CPU {s.get('cpu_ms', 0) / 1e3:.1f} s over {s['wall_s']:.1f} s wall (parallelism {s.get('parallelism')})")
    elif "instructions" in s:
        head.append(f"{human(s['instructions'])} instructions:u"
                    + (f", {s['ops']} {s['unit']} = **{human(s['instr_per_op'])}/op**" if "instr_per_op" in s else ""))
    md.append("; ".join(head))
    md.append("")
    md.append(table([[k, f"{v:.2f}%"] for k, v in s["subsystems"].items()], ["subsystem (instructions:u)", "share"]))
    md.append("")
    md.append(table([[k, f"{v:.2f}%", f"{s['inclusive_cpu'][k]:.2f}%" if k in s["inclusive_cpu"] else "n/a"]
                     for k, v in s["inclusive"].items()],
                    ["inclusive (instructions:u)", "share", "CPU-time share"], 14))
    md.append("")
    md.append(table([[k, f"{v:.2f}%"] for k, v in s["threads"].items()], ["thread group", "instr share"], 8))
    md.append("")
    md.append(table([[k[:70], f"{v:.2f}%"] for k, v in s["top_symbols"]], ["top leaf symbols", "share"], 10))
    md.append("")
    return s, "\n".join(md)


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("runs", nargs="+")
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    summary, mds = {}, []
    for r in a.runs:
        try:
            if json.load(open(os.path.join(r, "run.json"))).get("error"):
                print("skip (run recorded an error)", r)
                continue
            s, md = summarize(r)
        except Exception as e:  # noqa: BLE001
            print("skip", r, repr(e))
            continue
        summary[s["run"]] = s
        mds.append(md)
    json.dump(summary, open(os.path.join(a.out, "summary.json"), "w"), indent=1)
    open(os.path.join(a.out, "tables.md"), "w").write("\n".join(mds))
    print(f"{len(summary)} runs")
