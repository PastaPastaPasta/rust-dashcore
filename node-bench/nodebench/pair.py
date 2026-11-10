"""Paired comparisons: the only kind of comparison the merge gate trusts.

On a shared host, absolute numbers drift with whatever else is running, so A
(dashd) and B (rust) are always measured under the same conditions and judged
by their ratio:

interleave  rounds run A,B,B,A,A,B,... back to back on the same CPU slice
            (ABBA order cancels linear drift); each run is a fresh process.
concurrent  each round runs A and B at the same time on disjoint, equally
            sized cpusets (default node 0-11@0 + aux 12-15 vs node 16-27@1 +
            aux 28-31: each side has its own seeds/load-generator CPUs),
            swapping them every round so NUMA/core asymmetry cancels; both
            sides wait at a barrier before measuring; optional identical
            cgroup caps. Both sides share the pair's process mark, so neither
            counts the other as foreign load.

Per metric and round r, speedup_r = A_r/B_r (lower-is-better) or B_r/A_r. The
headline is the geometric mean of speedup_r with a t-distribution 95% CI on
log(speedup_r) (>= 3 rounds). The verdict is compare.gate_verdict on the
rounds' values: the NFR-PERF-02(g) Mann-Whitney rule. A FAIL is re-run in an
independent pair; it is cleared only if that re-run's CI excludes a slowdown of
the effect size, and every row of the re-run (including new FAILs) is reported
(settle_confirmation; --no-confirm skips the re-run).
Running with --a dashd --b dashd measures the noise floor (A/A): its CI should
contain 1, and its width is the smallest regression the setup can detect.
"""

import json
import math
import os
import shutil
import subprocess
import sys
import time

from . import compare, noise, paths, results, stats

RUN = os.path.join(paths.HARNESS_DIR, "run")

# Two-sided 97.5% t quantiles by degrees of freedom.
T975 = {1: 12.71, 2: 4.30, 3: 3.18, 4: 2.78, 5: 2.57, 6: 2.45, 7: 2.36, 8: 2.31, 9: 2.26, 10: 2.23, 12: 2.18,
        15: 2.13, 20: 2.09, 30: 2.04}

# `run` parameters forwarded verbatim to every side.
FORWARD = ("reps", "warmup", "stop_height", "seeds", "txindex", "addressindex", "dbcache", "par", "interval",
           "bench_log", "milestones", "block_stats_from", "source_datadir", "chain", "txs", "clients", "duration",
           "mempool_sizes", "filter", "dashd_bench_bin", "rust_bench_bin", "limits", "reservation", "min_free_gib")


def t975(df):
    return T975.get(df) or T975[max(k for k in T975 if k <= df)]


def forward_args(args, drop_caches):
    out = ["--drop-caches"] if drop_caches else []
    if getattr(args, "quiet_window", None) is not None:
        # `pair` checked the quiet window before the round; the side records it rather than waiting again.
        out += ["--pair-quiet-window", str(args.quiet_window)]
    for k in FORWARD:
        v = getattr(args, k, None)
        if v is None:
            continue
        if isinstance(v, list):
            v = ",".join(str(x) for x in v)
        out += [f"--{k.replace('_', '-')}", str(v)]
    for k in ("dashd_bin", "rust_bin", "rust_cli", "tag"):
        if getattr(args, k, None):
            out += [f"--{k.replace('_', '-')}", getattr(args, k)]
    return out


def side_cmd(args, scenario, impl, node_slice, aux, pair_tag, drop_caches):
    # setpriv: a side dies with `pair`, and its own handlers then stop its nodes.
    pre = ["setpriv", "--pdeathsig", "TERM", "--"] if shutil.which("setpriv") else []
    cmd = pre + [sys.executable, RUN, "run", scenario, "--impl", impl, "--node-slice", node_slice or "",
                 "--aux-slice", aux or "", "--out", args.out, "--pair", pair_tag] + forward_args(args, drop_caches)
    # --a-bin/--b-bin: this side's daemon (e.g. the previous integration head vs the PR).
    side_bin = getattr(args, f"{pair_tag.rsplit(':', 1)[1].lower()}_bin", None)
    if side_bin:
        cmd += [f"--{impl}-bin", side_bin]  # argparse: the last occurrence wins
    return cmd


def _terminate(signum, _frame):
    raise SystemExit(128 + signum)  # unwinds run_rounds' finally: the sides get SIGTERM


def cmd_pair(args):
    from .cli import install_handlers, selected_scenarios

    install_handlers(_terminate)
    paths.writable(args.out, "--out")
    if args.analyze:
        expect = (selected_scenarios(args), args.rounds) if args.scenarios else None
        return report(args, args.analyze, analyze(args.out, args.analyze, gate_opts(args), expect=expect))
    scenarios = selected_scenarios(args)  # options a scenario does not use fail here, not in every side
    if args.rounds % 2:
        raise SystemExit("--rounds must be even: ABBA order and slice swaps only balance over pairs of rounds")
    if (args.a_bin or args.b_bin) and {"microbench", "spv-headers-mainnet", "spv-headers-testnet"} & set(scenarios):
        raise SystemExit("--a-bin/--b-bin select node daemons; microbench and spv-headers-* run bench binaries "
                         "(--dashd-bench-bin/--rust-bench-bin, same on both sides): pair them separately")
    concurrent = args.mode == "concurrent"
    if concurrent:
        a_side = (args.slice_a or "0-11@0", args.aux_a or "12-15")
        b_side = (args.slice_b or "16-27@1", args.aux_b or "28-31")
    else:
        a_side = (args.slice_a or "numa:0", args.aux_a or "numa:1")
        b_side = (args.slice_b or a_side[0], args.aux_b or a_side[1])
    validate_layout(concurrent, a_side, b_side)
    pair_id = run_rounds(args, scenarios, concurrent, a_side, b_side)
    if pair_id is None:
        return 1
    rep = analyze(args.out, pair_id, gate_opts(args))
    failing = sorted({r["scenario"] for r in rep["rows"] if r["verdict"] == "FAIL"})
    if failing and not args.no_confirm:
        # Many gated metrics => some false FAILs by chance. A FAIL is cleared only if an independent re-run
        # shows the metric is not slower (settle_confirmation); everything the re-run finds is reported.
        print(f"confirming FAIL in {', '.join(failing)} with an independent re-run", flush=True)
        confirm_id = run_rounds(args, failing, concurrent, a_side, b_side, suffix="-confirm")
        if confirm_id is None:
            rep["warnings"].append("FAIL confirmation was not run (host never got quiet): FAILs stand")
        else:
            rep2 = analyze(args.out, confirm_id, gate_opts(args))
            settle_confirmation(rep, rep2, confirm_id, effect=args.effect)
    return report(args, pair_id, rep)


def validate_layout(concurrent, a_side, b_side):
    """Equal, non-overlapping CPU budgets. interleave: both sides on the same node and aux slices; concurrent:
    four pairwise disjoint slices, node slices and aux slices of equal size."""
    from .proc import slice_set

    sets = {name: slice_set(spec) for name, spec in (("node A", a_side[0]), ("aux A", a_side[1]),
                                                     ("node B", b_side[0]), ("aux B", b_side[1]))}
    if any(v is None for v in sets.values()):
        raise SystemExit("pair needs explicit node and aux slices on both sides")
    if not concurrent:
        if a_side != b_side:
            raise SystemExit("--mode interleave runs both sides on the same slices: drop --slice-b/--aux-b")
        if sets["node A"] & sets["aux A"]:
            raise SystemExit(f"node slice {a_side[0]} and aux slice {a_side[1]} overlap")
        return
    names = list(sets)
    for i, x in enumerate(names):
        for y in names[i + 1:]:
            if sets[x] & sets[y]:
                raise SystemExit(f"concurrent pair: {x} and {y} overlap ({sorted(sets[x] & sets[y])})")
    for kind in ("node", "aux"):
        if len(sets[f"{kind} A"]) != len(sets[f"{kind} B"]):
            raise SystemExit(f"concurrent pair: unequal {kind} slices ({len(sets[f'{kind} A'])} vs "
                             f"{len(sets[f'{kind} B'])} CPUs)")


def settle_confirmation(rep, rep2, confirm_id, effect=compare.EFFECT):
    """Merge an independent confirmation pair into the report without losing power or evidence.

    A FAIL is CLEARED only when the confirmation measured that metric cleanly (its scenario has no ERROR row),
    its own verdict passes, and its paired 95% CI excludes a slowdown of `effect` or more (lower bound >=
    1/(1+effect)). Mere non-significance never clears a FAIL: for a true slowdown >= effect the chance of a wrong
    clearance is at most the CI's one-sided 2.5%, so the confirmation keeps the first run's detection power.
    Every confirmation row is appended as well, so a FAIL (or ERROR, MISSING, ...) that only the confirmation
    found still decides the gate.
    """
    errored = {r["scenario"] for r in rep2["rows"] if r["verdict"] in ("ERROR", "MISSING")}
    conf = {(r["scenario"], r["metric"]): r for r in rep2["rows"]}
    floor = 1 / (1 + effect)
    for r in rep["rows"]:
        if r["verdict"] != "FAIL":
            continue
        c = conf.get((r["scenario"], r["metric"]))
        if c is None or r["scenario"] in errored:
            r["note"] = f"FAIL stands: not measured cleanly in confirmation {confirm_id}"
            continue
        ci = c.get("ci")
        r["confirmation"] = {k: c.get(k) for k in ("verdict", "speedup", "ci", "p", "n")}
        if c["verdict"] in compare.PASSING and ci and ci[0] >= floor:
            r["verdict"] = "CLEARED"
            r["note"] = (f"FAIL cleared by {confirm_id}: speedup {c['speedup']:.3f}, 95% CI "
                         f"[{ci[0]:.3f}, {ci[1]:.3f}] excludes a slowdown >= {effect:.0%}")
        else:
            shown = f"[{ci[0]:.3f}, {ci[1]:.3f}]" if ci else "none"
            r["note"] = (f"FAIL stands: confirmation {confirm_id} is {c['verdict']}, CI {shown} does not exclude a "
                         f"slowdown >= {effect:.0%}")
    rep["rows"] += [dict(r, metric=f"{r['metric']} (confirmation)") for r in rep2["rows"]]
    rep["warnings"] += [f"confirmation: {w}" for w in rep2.get("warnings", [])]
    rep["confirm_pair_id"] = confirm_id
    # Official only if the confirmation's runs are as well.
    rep["official"] = rep.get("official", False) and rep2.get("official", False)
    rep["provisional_reasons"] = rep.get("provisional_reasons", []) + [
        f"confirmation: {x}" for x in rep2.get("provisional_reasons", [])]
    recount(rep)


def run_rounds(args, scenarios, concurrent, a_side, b_side, suffix=""):
    """Run every scenario for args.rounds rounds; returns the pair id (None if the host never got quiet)."""
    pair_id = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime()) + f"-{args.a}-vs-{args.b}{suffix}"
    noise.ensure_mark(pair_id)  # inherited by both sides and everything they start
    log_dir = os.path.join(args.out, "pairs", pair_id)
    os.makedirs(log_dir, exist_ok=True)
    log = lambda m: print(f"[{time.strftime('%H:%M:%S')}] {m}", flush=True)  # noqa: E731
    log(f"pair {pair_id}: {args.mode}, {args.rounds} rounds, A={args.a}@{a_side} B={args.b}@{b_side}")
    save_expect(args.out, pair_id, scenarios, args.rounds, gate_opts(args))
    for scenario in scenarios:
        for r in range(args.rounds):
            if args.quiet_window is not None and not all(
                    noise.wait_for_quiet(log, args.quiet_window, node_slice=sl, timeout=args.quiet_timeout)
                    for sl in {a_side[0], b_side[0]}):
                log("host never got quiet; stopping")
                return None
            sides = [("A", args.a, a_side), ("B", args.b, b_side)]
            if r % 2:
                # interleave: B goes first (ABBA); concurrent: swap CPU slices.
                sides = sides[::-1] if not concurrent else [("A", args.a, b_side), ("B", args.b, a_side)]
            env = dict(os.environ)
            if concurrent:
                # Barriers keep both sides at the same measurement boundary; with --drop-caches side A drops the
                # page cache there, once, and side B waits for it (common.drop_caches).
                barrier = os.path.join(log_dir, f"barrier-{scenario}-r{r}")
                os.makedirs(barrier, exist_ok=True)
                env.update(NODE_BENCH_BARRIER=barrier, NODE_BENCH_BARRIER_N="2")
            procs = []
            try:
                for side, impl, (node_slice, aux) in sides:
                    cmd = side_cmd(args, scenario, impl, node_slice, aux, f"{pair_id}:{r}:{side}", args.drop_caches)
                    out = open(os.path.join(log_dir, f"{scenario}.r{r}.{side}.log"), "w")
                    log(f"  {scenario} round {r} side {side} ({impl} on {node_slice}, aux {aux})")
                    p = subprocess.Popen(cmd, stdout=out, stderr=subprocess.STDOUT, env=env)
                    procs.append((side, p, out))
                    if not concurrent:
                        p.wait()
                pending = list(procs)
                while pending:  # poll both sides: the first nonzero exit releases the other's barrier
                    for item in list(pending):
                        side, p, _ = item
                        if p.poll() is None:
                            continue
                        pending.remove(item)
                        if p.returncode:
                            log(f"  {scenario} round {r} side {side} exited with {p.returncode}")
                            if concurrent:
                                open(os.path.join(barrier, f"ABORT.{side}"), "w").close()
                    time.sleep(0.5)
            finally:
                for _, p, out in procs:
                    if p.poll() is None:
                        p.terminate()  # the side's handler unwinds its nodes and seeds
                        try:
                            p.wait(900)
                        except subprocess.TimeoutExpired:
                            p.kill()
                            p.wait()
                    out.close()
    return pair_id


def gate_opts(args):
    return {"effect": args.effect, "alpha": args.alpha, "strict": args.strict, "margin": args.margin}


def report(args, pair_id, rep):
    log_dir = os.path.join(args.out, "pairs", pair_id)
    os.makedirs(log_dir, exist_ok=True)
    md = to_markdown(rep)
    results.write_text(os.path.join(log_dir, "report.md"), md)
    results.write_json(os.path.join(log_dir, "report.json"), rep)
    # One line per comparison: the per-PR history the program tracks.
    rev = paths.harness_commit()
    with open(os.path.join(args.out, "ledger.jsonl"), "a") as f:
        f.write(json.dumps({"pair_id": pair_id, "tag": args.tag, "gate": rep["gate"], "official": rep["official"],
                            "profile": rep["profile"], "gate_opts": rep.get("gate_opts"), "n_gated": rep["n_gated"],
                            **{k: rep[k] for k in ("n_fail", "n_missing", "n_error", "n_insufficient",
                                                   "n_inconclusive", "n_cleared", "n_2x")},
                            "harness_commit": rev, "load_classes": rep["load_classes"],
                            "report": os.path.join(log_dir, "report.md")}) + "\n")
    print(md)
    return 0 if rep["gate"] == "PASS" else 1


def collect(out_dir, pair_id):
    """{scenario: {round: {side: doc}}} for one pair id."""
    got = {}
    for doc in results.iter_docs(out_dir):
        pr = doc.get("pair")
        if not pr or pr.get("id") != pair_id:
            continue
        got.setdefault(doc["scenario"], {}).setdefault(int(pr["round"]), {})[pr["side"]] = doc
    return got


def paired_ratio(speedups):
    """Geometric mean of per-round ratios with a 95% t-interval (None below 3 rounds, or when some round's
    ratio is undefined: the interval must use the same rounds as the test)."""
    s = list(speedups)
    if not s or not all(isinstance(x, (int, float)) and math.isfinite(x) and x > 0 for x in s):
        return None, None
    logs = [math.log(x) for x in s]
    mu = sum(logs) / len(logs)
    ci = None
    if len(s) >= 3:
        sd = math.sqrt(sum((x - mu) ** 2 for x in logs) / (len(logs) - 1))
        h = t975(len(logs) - 1) * sd / math.sqrt(len(logs))
        ci = (math.exp(mu - h), math.exp(mu + h))
    return math.exp(mu), ci


def expect_path(out_dir, pair_id):
    return os.path.join(out_dir, "pairs", pair_id, "expect.json")


def save_expect(out_dir, pair_id, scenarios, rounds, opts):
    """What a pair set out to run, written before the first round: a re-analysis then still notices a scenario or
    round that left no result at all."""
    results.write_json(expect_path(out_dir, pair_id), {"scenarios": list(scenarios), "rounds": rounds,
                                                       "gate": opts, "profile": compare.PROFILE})


def load_expect(out_dir, pair_id):
    try:
        with open(expect_path(out_dir, pair_id)) as f:
            e = json.load(f)
        return e["scenarios"], e["rounds"]
    except (OSError, ValueError, KeyError):
        return None


def consistency_rows(sc, docs):
    """Both sides of every round must have run the same scenario definition: same parameters (paths, binaries
    and slices aside), the same cold/warm page-cache state and the same effective node configuration."""
    rows = []
    variants = {json.dumps(compare.comparable_params(x), sort_keys=True, default=str) for x in docs}
    if len(variants) > 1:
        rows.append(compare.problem_row(sc, "(params)", "ERROR", f"{len(variants)} different parameter sets"))
    for key, fields in compare.CHAIN_PARAMS.items():
        chains = {json.dumps([(x["params"].get(key) or {}).get(f) for f in fields]) for x in docs}
        if len(chains) > 1:
            rows.append(compare.problem_row(sc, f"({key} chain)", "ERROR", "runs used different chains: "
                                            + " vs ".join(sorted(chains))))
    how = {x["metrics"].get(m, {}).get("measured_by", "").split(" ")[0] for x in docs
           for m in ("ibd_time_s", "reindex_time_s") if m in x["metrics"]}
    if len(how) > 1:
        rows.append(compare.problem_row(sc, "(timing method)", "ERROR", "runs were timed differently: "
                                        + ", ".join(sorted(how))))
    caches = {json.dumps(x["params"].get("page_cache_dropped"), default=str) for x in docs}
    if len(caches) > 1:
        rows.append(compare.problem_row(sc, "(page cache)", "ERROR", f"cold/warm state differs: {sorted(caches)}"))
    # What the nodes actually applied (dbcache, threads, assumevalid, indexes, mempool cap; common.verify_effective).
    effective = {json.dumps(x.get("effective_config"), sort_keys=True) for x in docs}
    if len(effective) > 1:
        detail = "the nodes applied different configurations: " + " vs ".join(sorted(effective))
        rows.append(compare.problem_row(sc, "(effective config)", "ERROR", detail))
    return rows


def metric_rows(sc, complete, docs, required, opts):
    """One row per metric. Gated metrics (required, or reported as gated by any run) need a valid value on both
    sides of every complete round; the test, the medians and the CI all use exactly those rounds."""
    n_runs = compare.required_runs(sc)
    rows = []
    for name in compare.metric_names(docs, required):
        meta = compare.metric_meta(docs, name)
        gated = compare.is_gated(docs, name, required)
        if compare.required_as_info(docs, name, required):
            rows.append(compare.problem_row(sc, name, "ERROR", "required metric recorded as information"))
            continue
        a_vals, b_vals, sp, missing, invalid = [], [], [], [], []
        for r, d in sorted(complete.items()):
            ma, mb = d["A"]["metrics"].get(name), d["B"]["metrics"].get(name)
            for side, m in (("A", ma), ("B", mb)):
                if m is None or m.get("value") is None:
                    missing.append(f"r{r}{side}")
                elif not compare.valid_value(m["value"]):
                    invalid.append(f"r{r}{side}={m['value']!r}")
            if ma is None or mb is None or not all(compare.valid_value(m.get("value")) for m in (ma, mb)):
                continue
            a_vals.append(ma["value"])
            b_vals.append(mb["value"])
            sp.append(compare.ratio(ma["value"], mb["value"], meta["better"]))
        if not gated:
            if a_vals and not missing and not invalid:
                g, ci = paired_ratio(sp)
                rows.append(compare.verdict_row(sc, name, meta, a_vals, b_vals, "info", g, ci, None, len(sp)))
            continue
        if missing:
            rows.append(compare.problem_row(sc, name, "MISSING", "absent in " + ", ".join(missing),
                                            meta.get("unit", "")))
            continue
        if invalid:
            rows.append(compare.problem_row(sc, name, "ERROR", "invalid values: " + ", ".join(invalid),
                                            meta.get("unit", "")))
            continue
        g, ci = paired_ratio(sp)
        _, p, slower_by, v = compare.gate_verdict(a_vals, b_vals, {**meta, "gate": True}, n_runs, ci=ci,
                                                  speedup=g, **opts)
        rows.append(compare.verdict_row(sc, name, meta, a_vals, b_vals, v, g, ci, p, len(sp), slower_by=slower_by,
                                        per_round=sp, a_p95=stats.percentile(sorted(a_vals), 95),
                                        b_p95=stats.percentile(sorted(b_vals), 95)))
    return rows


def analyze(out_dir, pair_id, opts=None, expect=None):
    """expect: (scenarios, rounds) that were run (default: the pair's expect.json); a scenario/round with no result
    at all is an ERROR. opts: effect / alpha / strict / margin for compare.gate_verdict."""
    opts = opts or {}
    got = collect(out_dir, pair_id)
    rows, warnings, load_classes, official = [], [], [], []
    saved = load_expect(out_dir, pair_id)
    if saved and expect:  # an explicit list on re-analysis adds to what the pair set out to run
        expect = (sorted(set(saved[0]) | set(expect[0])), saved[1])
    expect = expect or saved
    if expect:
        for sc in expect[0]:
            for rnd in range(expect[1]):
                got.setdefault(sc, {}).setdefault(rnd, {})
    else:
        rows.append(compare.problem_row("*", "(expected runs)", "ERROR", "no expect.json and no scenarios given: "
                                        "a scenario that left no result at all cannot be detected"))
    for sc, rounds in sorted(got.items()):
        bad = [(r, side, d[side].get("status") if side in d else "missing")
               for r, d in rounds.items() for side in ("A", "B")
               if side not in d or d[side].get("status") != "ok"]
        if bad:
            # A side that errored, was interrupted or never wrote a result blocks the gate.
            rows.append(compare.problem_row(sc, "(runs)", "ERROR", "; ".join(f"r{r}{side}={st}" for r, side, st in bad),
                                            n=len(rounds)))
        complete = {r: d for r, d in rounds.items()
                    if all(side in d and d[side].get("status") == "ok" for side in ("A", "B"))}
        if not complete:
            continue
        docs = [d[s] for d in complete.values() for s in ("A", "B")]
        load_classes += [(x.get("load") or {}).get("class", "unknown") for x in docs]
        official += [bool((x.get("load") or {}).get("official")) for x in docs]
        rows += consistency_rows(sc, docs)
        required = compare.required_metrics(sc, docs[0]["params"])
        rows += compare.inventory_row(sc, required)
        rows += metric_rows(sc, complete, docs, required, opts)
    reasons = []
    if not official or not all(official):
        reasons.append(f"{len(official) - sum(official)}/{len(official)} runs are not official (quiet window, "
                       "reserved, quiet throughout the measurement)")
    if not compare.opts_compliant(opts):
        reasons.append(f"gate options weaker than the PRD: {opts}")
    rep = {"pair_id": pair_id, "profile": compare.PROFILE, "gate_opts": opts, "rows": rows, "warnings": warnings,
           "load_classes": load_classes, "official": not reasons, "provisional_reasons": reasons}
    heavy = sum(c == "heavy" for c in load_classes)
    if heavy:
        warnings.append(f"{heavy}/{len(load_classes)} runs were under heavy foreign load: ratios still pair "
                        "the noise, but widen the CIs")
    return recount(rep)


def recount(rep):
    rep.update(compare.summarize_rows(rep["rows"], official=rep.get("official", False)))
    return rep


def to_markdown(rep):
    lines = [f"# node-bench paired comparison `{rep['pair_id']}` — gate: **{rep['gate']}**", "",
             compare.counts_line(rep), "",
             "speedup = geometric mean of the per-round paired ratios (> 1: B better), 95% t-interval over rounds. "
             "FAIL = B's median worse by ≥ 1% with one-sided Mann-Whitney p < 0.05 over the rounds' values "
             "(NFR-PERF-02(g)).", ""] + compare.TABLE_HEAD
    lines += [compare.fmt_row(r) for r in rep["rows"]] + compare.notes_lines(rep["rows"])
    if rep.get("provisional_reasons"):
        lines += ["", "Not official (never merge evidence):", ""] + [f"- {x}" for x in rep["provisional_reasons"]]
    if rep["warnings"]:
        lines += ["", "Warnings:", ""] + [f"- {w}" for w in rep["warnings"]]
    return "\n".join(lines) + "\n"
