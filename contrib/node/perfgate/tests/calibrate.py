#!/usr/bin/env python3
"""False-FAIL calibration of the gate procedure on no-op runs (PF0 r2 ruling 5).

    python3 contrib/node/perfgate/tests/calibrate.py SESSION... [--trials 20000] [--seed 1] [--json OUT]

Each SESSION is a gate report directory (base.json and head.json, taken interleaved in one session) or a
measurement JSON from `run measure`. Under a no-op both sides are one population, so a session's runs of a
workload, both sides together, form one pool of per-run counter vectors (the counters of one run stay
together; pools under MIN_POOL runs are refused).

One trial is one simulated no-op gate, the way `run gate` decides it:
1. pick a session (a two-level bootstrap: between-session differences such as load stay in);
2. per workload, draw 5 base and 5 head runs from it and judge them with gate.compare (round 1);
3. escalate the workloads gate.needs_escalation selects: 10 more runs per side, judged again by gate.compare;
4. add gate.drift_rows against an anchor simulated from an independently drawn session (median and noise
   floor of 15 runs per counter, as `run anchor` records them);
5. the trial is a false FAIL when any row blocks.

The result is conditional on the sessions: runs are treated as exchangeable within a session, and a load or
host effect no session contained cannot show. It covers the workloads present in the sessions; bit-stable
workloads (most of them) cannot fail a no-op and are reported as such.
"""

import argparse
import json
import math
import os
import random
import statistics
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.realpath(__file__))))

from perfgate import anchor as anchor_mod  # noqa: E402
from perfgate import gate, spec  # noqa: E402

ROUND, EXTRA, MIN_POOL, ANCHOR_RUNS = 5, 10, 10, 15  # ANCHOR_RUNS: cli.ANCHOR_REPEAT
FP = {k: "same" for k in gate.ANCHOR_COMPARABLE}


def runs_of(result):
    """The per-run counter vectors of one workload result (counter and syscall passes are separate processes,
    paired by index)."""
    s = result["samples"]
    n = min(len(v) for v in s.values() if v)
    return [{k: v[i] for k, v in s.items() if v} for i in range(n)]


def load_sessions(paths_):
    sessions, meta = [], {}
    for d in paths_:
        files = [os.path.join(d, f) for f in ("base.json", "head.json")] if os.path.isdir(d) else [d]
        pools = {}
        for f in files:
            with open(f) as fh:
                m = json.load(fh)
            for name, w in m["workloads"].items():
                if w.get("status") == "ok":
                    pools.setdefault(name, []).extend(runs_of(w))
                    meta[name] = (w["iters"], w.get("kind", "bench"))
        small = {n: len(p) for n, p in pools.items() if len(p) < MIN_POOL}
        if small:
            raise SystemExit(f"{d}: pools under {MIN_POOL} runs: {small}")
        sessions.append((d, pools))
    return sessions, meta


UNGATED = {w.name: list(w.ungated) for w in spec.load()}  # judged with today's declarations


def side(runs, kind, iters, escalated=False, name=None):
    out = {"status": "ok", "iters": iters, "kind": kind, "samples": {k: [r[k] for r in runs] for k in runs[0]},
           "ungated": UNGATED.get(name, [])}
    if escalated:
        out.update(escalated=True, first_round=ROUND)
    return out


def measurement(draws, meta, idx, n, escalate=()):
    return {"meta": {"fingerprint": FP}, "workloads": {
        w: side(d[idx] if w in escalate else d[idx][:n], meta[w][1], meta[w][0], w in escalate, w)
        for w, d in draws.items()}}


def trial(sessions, meta, rng):
    """One simulated no-op gate: (blocking rows, escalated workloads, session index)."""
    si = rng.randrange(len(sessions))
    pools = sessions[si][1]
    draws = {w: ([rng.choice(p) for _ in range(ROUND + EXTRA)], [rng.choice(p) for _ in range(ROUND + EXTRA)])
             for w, p in pools.items()}
    first = gate.compare(measurement(draws, meta, 0, ROUND), measurement(draws, meta, 1, ROUND), final=False)
    escalate = {r.workload for r in first if gate.needs_escalation(r)}
    base_m, head_m = (measurement(draws, meta, i, ROUND, escalate) for i in (0, 1))
    rows = gate.compare(base_m, head_m)
    anchor_pools = sessions[rng.randrange(len(sessions))][1]
    anchor = {"rev": "a" * 40, "fingerprint": FP, "workloads": {}}
    for w in pools:
        sample = [rng.choice(anchor_pools.get(w) or pools[w]) for _ in range(ANCHOR_RUNS)]
        anchor["workloads"][w] = {"iters": meta[w][0],
                                  **{f: g({"samples": {k: [r[k] for r in sample] for k in sample[0]}})
                                     for f, g in (("medians", anchor_mod.medians), ("noise", anchor_mod.noise))}}
    rows += gate.drift_rows(rows, head_m, anchor)
    return [(r.workload, r.counter, r.status) for r in rows if r.status in gate.BLOCKING], escalate, si


def wilson(k, n, z=1.96):
    if n == 0:
        return 0.0, 1.0
    p = k / n
    c = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / (1 + z * z / n)
    centre = (p + z * z / (2 * n)) / (1 + z * z / n)
    return max(0.0, centre - c), min(1.0, centre + c)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("sessions", nargs="+")
    ap.add_argument("--trials", type=int, default=20000)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--json")
    args = ap.parse_args()
    sessions, meta = load_sessions(args.sessions)
    rng = random.Random(args.seed)
    fails, causes, escalations = 0, {}, 0
    per_session = [[0, 0] for _ in sessions]
    for _ in range(args.trials):
        blocking, escalated, si = trial(sessions, meta, rng)
        escalations += bool(escalated)
        per_session[si][1] += 1
        if blocking:
            fails += 1
            per_session[si][0] += 1
            for b in set(blocking):
                causes[b] = causes.get(b, 0) + 1
    lo, hi = wilson(fails, args.trials)
    workloads = sorted({w for _d, p in sessions for w in p})
    print(f"{len(sessions)} sessions; workloads: {', '.join(workloads)}; pool sizes "
          f"{min(len(p) for _d, s in sessions for p in s.values())}-"
          f"{max(len(p) for _d, s in sessions for p in s.values())} runs")
    print(f"false FAIL: {fails}/{args.trials} = {fails / args.trials:.4%} (95% Wilson {lo:.4%}-{hi:.4%}); "
          f"trials with an escalation: {escalations / args.trials:.2%}")
    rates = [f / n for f, n in per_session if n]
    print(f"per-session false-FAIL rates: min {min(rates):.3%}, median {statistics.median(rates):.3%}, "
          f"max {max(rates):.3%}")
    for (w, c, s), n in sorted(causes.items(), key=lambda kv: -kv[1])[:15]:
        print(f"  {n:6} {w} {c} {s}")
    if args.json:
        with open(args.json, "w") as f:
            json.dump({"trials": args.trials, "seed": args.seed, "fails": fails, "wilson95": [lo, hi],
                       "escalation_rate": escalations / args.trials, "sessions": [d for d, _p in sessions],
                       "per_session": per_session, "workloads": workloads,
                       "causes": {f"{w}:{c}:{s}": n for (w, c, s), n in causes.items()}}, f, indent=1)
    return 0


if __name__ == "__main__":
    sys.exit(main())
