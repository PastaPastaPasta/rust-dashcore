"""The "slower" rule (PRD NFR-PERF-02) and the unpaired comparison.

A metric's per-run values (one headline value per run) are compared between
the baseline A (dashd, or the previous integration head) and the candidate B:

    speedup = A/B for lower-is-better metrics, B/A for higher-is-better,
              so > 1 always means B is better

Row verdicts:

    FAIL          B is slower: B's median is worse than A's by >= 1% (--effect)
                  AND a one-sided Mann-Whitney test gives p < 0.05 (--alpha)
                  [NFR-PERF-02(g)]
    PASS          not slower
    2x            PASS with speedup >= 2 on a metric where the 2x target applies
    CLEARED       a FAIL that an independent confirmation showed is not a slowdown
                  of >= --effect (pair.py); not blocking
    INSUFFICIENT  fewer runs than NFR-PERF-02(f) requires (5; 4 for the long
                  sync scenarios, DEC-23), or so few that p < 0.05 is unreachable
    INCONCLUSIVE  only with --strict: not shown slower, but the 95% CI of the
                  speedup reaches below 1 - margin (non-inferiority not shown)
    ERROR         a run errored, was interrupted, produced no result, or holds an
                  invalid value (non-finite, negative); or the sides ran with
                  different parameters / effective configuration
    MISSING       a gated metric is absent on a side in some round: either it is
                  in the scenario's required inventory (`required_metrics`) or
                  the other side reports it as gated

Overall gate (most severe first):

    FAIL          some row is FAIL
    INCOMPLETE    some row is ERROR, MISSING, INSUFFICIENT or INCONCLUSIVE, or no
                  gated row exists: the tracked contract was not fully observed
    PROVISIONAL   every required metric observed and not slower, but the evidence
                  is not official (not all runs quiet, in a reserved window, with
                  the PRD's effect/alpha); never merge evidence
    PASS          all of the above hold

The inventories are versioned (PROFILE): a comparison only covers the metrics
its scenarios declare, and a partial set can never PASS.

The merge gate is `pair` (pair.py), which produces the runs under identical
conditions. `compare` lines up results recorded separately (successful,
non-paired runs per scenario from the same binary and parameters as the newest
one); it is never official (at best PROVISIONAL), because separate runs on a
shared host do not see the same background load.
"""

import math
import os
import statistics

from . import results, stats

IGNORED_PARAMS = {"impl", "dashd_bin", "rust_bin", "rust_cli", "dashd_bench_bin", "rust_bench_bin", "bench_bin",
                  "out", "tag", "pair", "node_config", "mnsync_at_end", "datadir", "keep_datadir", "source_datadir",
                  "seed", "source", "quiet_window", "quiet_timeout", "page_cache_dropped", "page_cache_drops",
                  "node_slice", "aux_slice", "reservation", "keep_failed", "min_free_gib"}
# Compared field by field (the rest of these dicts are paths that always differ).
CHAIN_PARAMS = {"seed": ("height", "tip"), "source": ("height", "tip")}

# Version of the gate rules and the scenarios' required-metric inventories (scenarios/*.py `required=`).
PROFILE = "nfr-perf-02/b1-r2"
INCOMPLETE = ("ERROR", "MISSING", "INSUFFICIENT", "INCONCLUSIVE")
BLOCKING = ("FAIL",) + INCOMPLETE
PASSING = ("PASS", "2x", "CLEARED")
GATED = BLOCKING + PASSING
EFFECT, ALPHA, MARGIN = 0.01, 0.05, 0.02


def required_runs(scenario):
    """NFR-PERF-02(f) as amended by DEC-23: >= 5 runs per metric, >= 4 for the long sync scenarios
    (with 3 vs 3 the minimum Mann-Whitney p is 0.05, so "slower" could never be shown)."""
    from .scenarios import load_all

    sc = load_all().get(scenario)
    return 4 if sc is not None and sc.long else 5


def required_metrics(scenario, params):
    """The gated metrics a comparison of `scenario` run with `params` must observe on both sides.

    Declared by the scenario (never inferred from what a run happened to record): the inventory of its canonical
    parameters (its defaults) plus whatever `params` add. Parameters that narrow the work (a --filter, fewer
    mempool sizes or client counts, --stop-height, --bench-log, txindex=0) therefore cannot shrink the contract:
    the metrics they drop show up as MISSING, or as "required metric recorded as information". An empty
    inventory means an informational scenario; the gate adds an INCOMPLETE "(inventory)" row for it.
    """
    from .scenarios import load_all

    sc = load_all().get(scenario)
    if sc is None or sc.required is None:
        return set()
    if not callable(sc.required):
        return set(sc.required)
    return set(sc.required(dict(sc.defaults))) | set(sc.required(params))


def inventory_row(sc, required):
    """INCOMPLETE marker for a scenario without a required inventory: its rows alone can never make a PASS."""
    if required:
        return []
    return [problem_row(sc, "(inventory)", "MISSING", "informational scenario: no required metrics, never a pass")]


def valid_value(x):
    """A usable measurement: a finite, non-negative real number (0 is a value, not absence)."""
    return isinstance(x, (int, float)) and not isinstance(x, bool) and math.isfinite(x) and x >= 0


def ratio(a, b, better):
    """Speedup of B over A for one pair of values (> 1: B better); None when undefined."""
    num, den = (a, b) if better == "lower" else (b, a)
    if den > 0:
        return num / den
    return 1.0 if num == den else None


def worsening(ma, mb, lower):
    """How much worse B's median is than A's, relative to A (0 when not worse)."""
    if lower:
        return (mb - ma) / ma if ma > 0 else (math.inf if mb > 0 else 0.0)
    return (ma - mb) / ma if ma > 0 else 0.0


def gate_verdict(a, b, meta, required, *, effect=EFFECT, alpha=ALPHA, strict=False, margin=MARGIN, ci=None,
                 speedup=None):
    """Verdict for one metric from per-run values a (baseline) and b (candidate).

    Returns (speedup, p_slower, slower_by, verdict). `speedup` defaults to the ratio of medians;
    `ci` (of the speedup) is only used by --strict.
    """
    if meta.get("better") == "none":
        return None, None, None, "info"
    gated = meta.get("gate", True)
    if not a or not b:
        return None, None, None, "MISSING" if gated else "info"
    if not all(valid_value(x) for x in list(a) + list(b)):
        return None, None, None, "ERROR" if gated else "info"
    ma, mb = statistics.median(a), statistics.median(b)
    lower = meta["better"] == "lower"
    if speedup is None:
        speedup = ratio(ma, mb, meta["better"])
    slower_by = worsening(ma, mb, lower)
    p = stats.mann_whitney_greater(b, a) if lower else stats.mann_whitney_greater(a, b)
    if not gated:
        return speedup, p, slower_by, "info"
    # Too few runs, or so few that Mann-Whitney cannot reach p < alpha at all (3 vs 3: min p = 0.05).
    if len(a) < required or len(b) < required or 1 / math.comb(len(a) + len(b), len(a)) >= alpha:
        return speedup, p, slower_by, "INSUFFICIENT"
    if slower_by >= effect and p is not None and p < alpha:
        return speedup, p, slower_by, "FAIL"
    if strict and (ci is None or ci[0] < 1 - margin):
        return speedup, p, slower_by, "INCONCLUSIVE"
    return speedup, p, slower_by, ("2x" if meta.get("target_2x") and speedup and speedup >= 2 else "PASS")


def opts_compliant(opts):
    """Gate options at least as strict as the PRD (overrides that weaken the rule make a result provisional)."""
    return opts.get("effect", EFFECT) <= EFFECT and opts.get("alpha", ALPHA) >= ALPHA


def summarize_rows(rows, official=False):
    gated = [r for r in rows if r["verdict"] in GATED]
    if any(r["verdict"] == "FAIL" for r in rows):
        gate = "FAIL"
    elif not gated or any(r["verdict"] in INCOMPLETE for r in rows):
        gate = "INCOMPLETE"
    else:
        gate = "PASS" if official else "PROVISIONAL"
    out = {"gate": gate, "official": bool(official), "n_gated": len(gated),
           "n_2x_applicable": sum(1 for r in gated if r.get("target_2x"))}
    for k in ("FAIL", "INCONCLUSIVE", "INSUFFICIENT", "ERROR", "2x", "MISSING", "CLEARED"):
        out[f"n_{k.lower()}"] = sum(r["verdict"] == k for r in rows)
    return out


def comparable_params(doc):
    return {k: v for k, v in doc["params"].items() if k not in IGNORED_PARAMS}


def runs_by_scenario(path, impl=None):
    """{scenario: [docs]}: successful, non-paired runs, newest last, from the newest successful run's binary and
    with its parameters. When the newest attempt failed, it is kept last so the failure stays visible."""
    by = {}
    for doc in results.iter_docs(path):
        if doc.get("schema") != results.SCHEMA or doc.get("pair") or doc.get("status") is None:
            continue
        if impl and doc["impl"]["impl"] != impl:
            continue
        by.setdefault(doc["scenario"], []).append(doc)
    out = {}
    for sc, docs in by.items():
        docs.sort(key=lambda d: d.get("started_at") or "")
        ok = [d for d in docs if d["status"] == "ok"]
        if not ok:
            out[sc] = [docs[-1]]  # keep the failure visible
            continue
        ref, sha = comparable_params(ok[-1]), ok[-1]["impl"].get("sha256")
        same = [d for d in ok if comparable_params(d) == ref and d["impl"].get("sha256") == sha]
        out[sc] = same + ([docs[-1]] if docs[-1]["status"] != "ok" else [])
    return out


def metric_names(docs, required):
    """Every comparable metric of a scenario: the required inventory plus whatever any run recorded."""
    names = set(required)
    for d in docs:
        names |= {n for n, m in d["metrics"].items() if m.get("better") != "none"}
    return sorted(names)


def metric_meta(docs, name):
    for d in docs:
        if name in d["metrics"]:
            return d["metrics"][name]
    return {"gate": True, "better": "lower", "unit": "", "target_2x": False, "value": None}


def is_gated(docs, name, required):
    return name in required or any(d["metrics"].get(name, {}).get("gate") for d in docs)


def required_as_info(docs, name, required):
    """A required metric some run recorded as information: the run's definition does not match the profile."""
    return name in required and any(name in x["metrics"] and not x["metrics"][name].get("gate") for x in docs)


def problem_row(sc, metric, verdict, detail, unit="", n=None):
    return {"scenario": sc, "metric": metric, "verdict": verdict, "unit": unit, "a": None, "b": detail,
            "speedup": None, "ci": None, "p": None, "n": n}


def verdict_row(sc, name, meta, a, b, verdict, speedup, ci, p, n, **extra):
    """A compared metric: medians of both sides, the speedup and its CI, the test and its method."""
    return {"scenario": sc, "metric": name, "verdict": verdict, "unit": meta.get("unit", ""),
            "a": statistics.median(a), "b": statistics.median(b), "speedup": speedup, "ci": ci, "p": p,
            "test": stats.mw_method(len(a), len(b)) if p is not None else None, "n": n,
            "target_2x": meta.get("target_2x"), **extra}


def comparable(base, cand):
    """Warnings when the two sides were not taken under the same conditions."""
    warn = []
    eb, ec = base["env"], cand["env"]
    for k in ("hostname", "cpu_model", "cpu_slice", "slice_cpus", "kernel"):
        if eb.get(k) != ec.get(k):
            warn.append(f"{k} differs: {eb.get(k)} vs {ec.get(k)}")
    pb, pc = comparable_params(base), comparable_params(cand)
    for k in sorted(set(pb) | set(pc)):
        if pb.get(k) != pc.get(k):
            warn.append(f"param {k} differs: {pb.get(k)} vs {pc.get(k)}")
    for k, fields in CHAIN_PARAMS.items():
        vb, vc = base["params"].get(k) or {}, cand["params"].get(k) or {}
        for f in fields:
            if vb.get(f) != vc.get(f):
                warn.append(f"{k} {f} differs: {vb.get(f)} vs {vc.get(f)} (not the same chain)")
    if base["params"].get("page_cache_dropped") != cand["params"].get("page_cache_dropped"):
        warn.append("one side dropped the page cache and the other did not (cold vs warm)")
    return warn


def compare(base_path, cand_path, *, effect=EFFECT, alpha=ALPHA, strict=False, margin=MARGIN):
    base = runs_by_scenario(base_path, "dashd" if os.path.isdir(base_path) else None)
    cand = runs_by_scenario(cand_path, "rust" if os.path.isdir(cand_path) else None)
    rows, warnings = [], {}
    for sc in sorted(set(base) & set(cand)):
        b_runs, c_runs = base[sc], cand[sc]
        warnings[sc] = comparable(b_runs[-1], c_runs[-1])
        for doc in b_runs[-1:] + c_runs[-1:]:
            klass = (doc.get("load") or {}).get("class", "unknown (no load record)")
            if klass != "quiet":
                warnings[sc].append(f"{doc['impl']['impl']} runs were taken on a {klass} host: provisional")
        if c_runs[-1]["status"] != "ok" or b_runs[-1]["status"] != "ok":
            rows.append(problem_row(sc, "(run)", "ERROR", f"newest dashd {b_runs[-1]['status']}, "
                                                          f"rust {c_runs[-1]['status']}"))
            continue
        required = required_metrics(sc, b_runs[-1]["params"])
        rows += inventory_row(sc, required)
        n_runs = required_runs(sc)
        docs = b_runs + c_runs
        for name in metric_names(docs, required):
            meta, unit = metric_meta(docs, name), metric_meta(docs, name).get("unit", "")
            gated = is_gated(docs, name, required)
            a = [d["metrics"].get(name, {}).get("value") for d in b_runs]
            b = [d["metrics"].get(name, {}).get("value") for d in c_runs]
            bad = [x for x in a + b if x is not None and not valid_value(x)]
            if required_as_info(docs, name, required):
                rows.append(problem_row(sc, name, "ERROR", "required metric recorded as information", unit))
            elif any(x is None for x in a + b):
                if gated:
                    rows.append(problem_row(sc, name, "MISSING", f"absent in {a.count(None)}/{len(a)} baseline "
                                            f"and {b.count(None)}/{len(b)} candidate runs", unit, (len(a), len(b))))
            elif bad:
                if gated:
                    rows.append(problem_row(sc, name, "ERROR", f"invalid values: {bad}", unit))
            else:
                ci = None
                if len(a) >= 3 and len(b) >= 3 and all(x > 0 for x in a + b):
                    lower = meta["better"] == "lower"
                    ci = stats.bootstrap_ratio_ci(a, b) if lower else stats.bootstrap_ratio_ci(b, a)
                speedup, p, _, v = gate_verdict(a, b, {**meta, "gate": gated}, n_runs, effect=effect, alpha=alpha,
                                                strict=strict, margin=margin, ci=ci)
                rows.append(verdict_row(sc, name, meta, a, b, v, speedup, ci, p, (len(a), len(b))))
    return {"profile": PROFILE, "baseline": {s: [d["run_id"] for d in v] for s, v in base.items() if s in cand},
            "candidate": {s: [d["run_id"] for d in v] for s, v in cand.items() if s in base},
            "only_baseline": sorted(set(base) - set(cand)), "rows": rows, "warnings": warnings,
            **summarize_rows(rows, official=False)}


def fmt_row(r):
    ci = f"{r['ci'][0]:.2f}–{r['ci'][1]:.2f}" if r.get("ci") else "—"
    sp = f"{r['speedup']:.2f}×" if r.get("speedup") else "—"
    p = f"{r['p']:.3f}" if r.get("p") is not None else "—"
    n = r.get("n") or ""
    n = f"{n[0]}/{n[1]}" if isinstance(n, (tuple, list)) else str(n)
    return (f"| {r['scenario']} | {r['metric']} | {results.fmt(r['a'], r['unit'])} | "
            f"{results.fmt(r['b'], r['unit'])} | {sp} | {ci} | {p} | {n} | {r['verdict']} |")


def notes_lines(rows):
    """Row notes (cleared FAILs, confirmation results), which do not fit the table."""
    out = [f"- {r['scenario']} / {r['metric']}: {r['note']}" for r in rows if r.get("note")]
    return ["", "Row notes:", ""] + out if out else []


TABLE_HEAD = ["| scenario | metric | A median | B median | speedup | 95% CI | p (B slower) | runs A/B | verdict |",
              "|---|---|---|---|---|---|---|---|---|"]


def counts_line(rep):
    return (f"{rep['n_gated']} gated rows: {rep['n_fail']} FAIL, {rep['n_missing']} MISSING, "
            f"{rep['n_error']} ERROR, {rep['n_insufficient']} INSUFFICIENT, {rep['n_inconclusive']} INCONCLUSIVE, "
            f"{rep.get('n_cleared', 0)} CLEARED; 2x target met on {rep['n_2x']}/{rep['n_2x_applicable']}. "
            f"Profile `{rep.get('profile', PROFILE)}`.")


def to_markdown(rep):
    lines = [f"# node-bench comparison (unpaired, indicative) — verdict: **{rep['gate']}**", "",
             counts_line(rep), "",
             "A = dashd, B = rust. speedup > 1 means B is better. FAIL = B's median worse by ≥ 1% with "
             "one-sided Mann-Whitney p < 0.05 (NFR-PERF-02(g)). Unpaired comparisons are never official.",
             ""] + TABLE_HEAD
    lines += [fmt_row(r) for r in rep["rows"]] + notes_lines(rep["rows"])
    warn = [(s, w) for s, ws in rep["warnings"].items() for w in ws]
    if warn:
        lines += ["", "Comparability warnings:", ""] + [f"- {s}: {w}" for s, w in warn]
    if rep["only_baseline"]:
        lines += ["", "No rust result yet for: " + ", ".join(rep["only_baseline"])]
    return "\n".join(lines) + "\n"


def cmd_compare(args):
    rep = compare(args.baseline, args.candidate, effect=args.effect, alpha=args.alpha, strict=args.strict,
                  margin=args.margin)
    md = to_markdown(rep)
    print(md)
    if args.md:
        results.write_text(args.md, md)
    if args.json:
        results.write_json(args.json, rep)
    # Indicative only: exit 0 when nothing blocks (PROVISIONAL is the best an unpaired comparison can get).
    return 0 if rep["gate"] in ("PASS", "PROVISIONAL") else 1


def cmd_summary(args):
    by = runs_by_scenario(args.dir, args.impl)
    lines = []
    for sc, docs in sorted(by.items()):
        doc = docs[-1]
        imp = doc["impl"]
        lines += [f"## {sc} — {imp['impl']} {imp.get('version', '')}", "",
                  f"run `{doc['run_id']}` ({len(docs)} comparable run(s)); {results.load_line(doc)[2:]}",
                  "", "| metric | value | statistic | n | cv | gate |", "|---|---|---|---|---|---|"]
        for name, m in doc["metrics"].items():
            cv = m["stats"].get("cv")
            lines.append(f"| {name} | {results.fmt(m['value'], m['unit'])} | {m['statistic']} | "
                         f"{m['stats'].get('n', 0)} | {'—' if cv is None else f'{cv * 100:.1f}%'} | "
                         f"{'gate' + (' 2x' if m.get('target_2x') else '') if m.get('gate') else 'info'} |")
        lines.append("")
    md = "\n".join(lines)
    if args.md:
        with open(args.md, "w") as f:
            f.write(md)
    else:
        print(md)
    return 0
