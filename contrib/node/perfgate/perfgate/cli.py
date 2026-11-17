"""Command line: gate, anchor, warm, measure, compare, list, clean, wrap."""

import argparse
import contextlib
import hashlib
import json
import os
import shutil
import subprocess
import sys
import time
import types

from . import anchor, build, gate, invocation, measure, paths, sandbox, spec
from . import inputs as node_inputs


def log(msg):
    print(msg, file=sys.stderr, flush=True)


ANCHOR_REPEAT = 15  # an anchor's medians and noise floors are reused by every later gate: measure them well


def add_selection(p, gate_cmd=False, repeat=5):
    p.add_argument("--workloads", help=f"workload file (default {os.path.relpath(spec.DEFAULT_FILE)}; any other "
                   "file makes a gate non-authoritative)")
    p.add_argument("--only", action="append", help="workload name glob (repeatable; a gate on a subset is "
                   "INCOMPLETE, never PASS)")
    p.add_argument("--repeat", type=int, default=repeat, help=f"runs per workload and side (default {repeat})")
    p.add_argument("--jobs", type=int, default=4, help="bench processes at a time (counts do not depend on it)")
    p.add_argument("--no-cache", action="store_true", help="re-measure even when a cached result exists")
    p.add_argument("--dev-harness", action="store_true", help="allow a running harness that is not the base's "
                   "(results are non-authoritative)" if gate_cmd else argparse.SUPPRESS)


def selected(args, kinds=None):
    if args.repeat < (5 if args.cmd in ("gate", "anchor") else 1):
        raise SystemExit("--repeat must be at least 5 for a gate or anchor (A1 §3.11; trimming needs 5)")
    wls = spec.select(spec.load(args.workloads), args.only, kinds)
    if not wls:
        raise SystemExit("no workloads selected")
    return wls


def selection_info(args, wls):
    """What a run measured, for the ledger and the anchor (M3)."""
    path = os.path.realpath(args.workloads or spec.DEFAULT_FILE)
    with open(path, "rb") as f:
        digest = hashlib.sha256(f.read()).hexdigest()[:16]
    declared = len(spec.load(None))
    return {"only": args.only or [], "workloads_file": path, "workloads_sha256": digest,
            "default_file": path == os.path.realpath(spec.DEFAULT_FILE), "selected": len(wls),
            "declared": declared, "repeat": args.repeat}


def write_text(path, text):
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    with open(path, "w") as f:
        f.write(text)


def write_json(path, obj):
    write_text(path, json.dumps(obj, indent=1))


def cmd_list(args):
    for w in spec.load(args.workloads):
        pin = f" iters={w.iters}" if w.iters else ""
        print(f"{w.name:32} {w.kind:6} {w.describe()}{pin}{'' if w.syscalls else ' (no syscall pass)'}")
    return 0


def cmd_measure(args):
    with paths.locked():  # --dashd too: its node units share the root's prefix, which a gate's start GCs (D54)
        if args.dashd:
            m = measure.measure_dashd(args.dashd, selected(args, {"node"}), args.repeat, log)
        else:
            m = measure.measure_rev(args.label, args.rev, selected(args), args.repeat, args.jobs,
                                    use_cache=not args.no_cache, iters_from=anchor.iters(anchor.load()), log=log)
    print(measure.summary(m))
    table = gate.noise_table(m)
    print("\nNoise floors (range / median over the runs, the farthest run dropped):\n\n" + table)
    if args.json:
        write_json(args.json, m)
    if args.md:
        write_text(args.md, table)
    return 0


def cmd_warm(args):
    """Build and measure the integration head in the gate's base slot, so the next gate finds its target/ and
    base cache hot (run it in the background after every merge, m6)."""
    args.label, args.rev, args.dashd, args.json, args.md = "base", args.base, None, None, None
    return cmd_measure(args)


def cmd_compare(args):
    with open(args.base) as f:
        base_m = json.load(f)
    with open(args.head) as f:
        head_m = json.load(f)
    rows = gate.compare(base_m, head_m)
    md = gate.render(rows, base_m, head_m, all_rows=args.all)
    print(md)
    if args.md:
        write_text(args.md, md)
    if args.json:
        write_json(args.json, gate.report_json(rows))
    return gate.VERDICT_EXIT[gate.verdict(rows)]


def check_harness(args, rev):
    """M3: the running harness must be the committed harness of `rev`. Returns the problems tolerated under
    --dev-harness."""
    problems = build.harness_problems(rev)
    if problems and not args.dev_harness:
        raise build.BuildError("; ".join(problems) + ". Run the gate from a clean checkout of the base (the "
                               "integration worktree), or pass --dev-harness for a non-authoritative run")
    return problems


class GateError(build.BuildError):
    """A gate that could not measure, with the rows it had already found (e.g. BENCH-CHANGED)."""

    rows = ()


def workload_inputs(side, wls):
    """{workload: {"key", "inputs"}} at a side's revision, for the anchor; a node row also has its §8
    "components" and "tools" (PF0-N D48)."""
    benches = [w for w in wls if w.kind == "bench"]
    inputs = build.bench_inputs(side.sha, side.wt, {w.package for w in benches})
    out = {w.name: {"key": w.key(), "inputs": inputs.get(w.package)} for w in benches}
    for w in wls:
        if w.kind == "node":
            out[w.name] = {"key": w.key(), **node_inputs.node_inputs(w, side.sha)}
    return out


def run_gate(args, wls, base_sha, candidate, anchored, strict):
    """Measure base and the merge candidate with their runs interleaved, escalate NOISY workloads, and judge.
    Returns (base measurement, head measurement, rows, notes, the §7 ledger fields)."""
    benches = [w for w in wls if w.kind == "bench"]
    nodes = [w for w in wls if w.kind == "node"]
    pinned = anchor.iters(anchored)
    reuse_base = gate.stable if not args.no_cache else (lambda _r: False)
    base = measure.Side("base", base_sha, log=log)
    measure.prepare_benches(base, benches, args.repeat, pinned, reuse_base, log)
    head = measure.Side("head", candidate, lock_from=os.path.join(base.wt, "Cargo.lock"), log=log)
    head.cache_results = False

    rows, notes, sides, own = [], [], [base, head], None
    depinfo = build.depinfo_files(build.target_dir(base.wt), {w.target for w in benches}, base.wt)
    changed, manifest, srcs = build.bench_changes(base.sha, head.sha, base.wt, {w.package for w in benches},
                                                  head_wt=head.wt, depinfo=depinfo)
    if changed or manifest:
        what = ", ".join(changed + manifest)
        if args.approve_bench_change:
            rows.append(gate.Row("(benches)", "-", "APPROVED", note=f"bench inputs changed ({what}). Approval: "
                                 f"{args.approve_bench_change}"))
        else:
            rows.append(gate.Row("(benches)", "-", "BENCH-CHANGED", note=f"bench inputs changed ({what}): needs the "
                                 "manager's recorded approval (--approve-bench-change TEXT)"))
    # The gated head always runs the base's bench sources when they build against it (M2); with an approval
    # the head's own benches are measured as well, for information and the re-anchor.
    overlaid = build.overlay_bench_sources(head.wt, base.sha, srcs) if changed else []
    try:
        measure.prepare_benches(head, benches, args.repeat, base.iters(), lambda _r: False, log)
        if overlaid:
            notes.append(f"the head was measured with the base's bench sources ({', '.join(overlaid)})")
    except build.BuildError as e:
        if not overlaid or not args.approve_bench_change:
            err = GateError(f"{e}\nThe head does not build" + (" with the base's bench sources" if overlaid else "")
                            + ": review it, and a bench change with --approve-bench-change TEXT")
            err.rows = rows + [gate.Row("(build)", "-", "ERROR", note="the head does not build in the sandbox")]
            raise err from None
        log("the base's bench sources do not build against the head; gating the head's own benches (approved)")
        head = measure.Side("head", candidate, lock_from=os.path.join(base.wt, "Cargo.lock"), log=log)
        head.cache_results = False
        measure.prepare_benches(head, benches, args.repeat, base.iters(), lambda _r: False, log)
        notes.append("the base's bench sources do not build against the head: gated with the head's own benches")
        overlaid = []
        sides = [base, head]
    if overlaid and args.approve_bench_change:
        own = measure.Side("headown", candidate, lock_from=os.path.join(base.wt, "Cargo.lock"), log=log)
        own.cache_results = False
        measure.prepare_benches(own, benches, args.repeat, base.iters(), lambda _r: False, log)
        sides.append(own)
    measure.run_plans(sides, args.repeat, args.jobs, log)
    # Node rows run on both sides, PR code included, through the PF0 chain (PF0-N): the node is jailed, counted
    # from outside and never sees the gate's state.
    with measure.node_session([base, head], nodes, args.repeat + args.escalate, log) or contextlib.nullcontext() \
            as session:
        measure.measure_nodes(session, [base, head], nodes, args.repeat, log)
        first = gate.compare(base.measurement(wls, args.repeat), head.measurement(wls, args.repeat), final=False)
        noisy = sorted({r.workload for r in first if gate.needs_escalation(r)})
        if noisy and args.escalate:
            log(f"escalating {len(noisy)} NOISY workloads: {args.escalate} more runs per side ({', '.join(noisy)})")
            measure.escalate([base, head], [w for w in wls if w.name in noisy], args.escalate, args.jobs, log,
                             session)
    base_m, head_m = base.measurement(wls, args.repeat), head.measurement(wls, args.repeat)
    judged = gate.compare(base_m, head_m, strict=strict, source_approval=args.approve_counter_source)
    current = workload_inputs(base, wls)
    refused, ledger = gate.refusal_rows(base_m, head_m, anchored, args.approve_new_from_refused)
    rows = judged + rows + refused + gate.drift_rows(judged, head_m, anchored, current, base_m,
                                                     args.approve_counter_source)
    notes += gate.approve_regressions(rows, args.regression_approvals)
    # an approved counter-source change (m1) or an approved regression (BL-406) leaves the row to re-anchor
    ledger["anchor_obligations"] += gate.source_obligations(rows) + gate.regression_obligations(rows)
    if own:
        for r in gate.compare(base_m, own.measurement(wls, args.repeat), final=False):
            if r.status not in ("OK", "INFO"):
                rows.append(gate.Row(f"{r.workload} (head's own benches)", r.counter, "INFO", r.base, r.head, r.delta,
                                     r.noise_base, r.noise_head, r.allowed, r.iters, f"would be {r.status}"))
    return base_m, head_m, rows, notes, ledger


def record(entry):
    line = json.dumps(entry, sort_keys=True)
    with open(os.path.join(paths.root(), "ledger.jsonl"), "a") as f:
        f.write(line + "\n")
    return line


def regression_approvals(args, wls):
    """The parsed --approve-regression approvals (BL-406); each must name a selected workload, at most once per
    workload and counter. A malformed one is a usage error (exit 2) before anything is measured."""
    out, seen = [], set()
    for text in args.approve_regression or ():
        try:
            a = gate.parse_regression_approval(text)
        except ValueError as e:
            sys.exit(f"--approve-regression: {e}")
        w = next((w for w in wls if w.name == a.workload), None)
        if w is None:
            sys.exit(f"--approve-regression: no selected workload {a.workload!r}: {text!r}")
        if not gate.is_gated(gate.BY_KEY[a.counter], w.kind, w.ungated):
            sys.exit(f"--approve-regression: {a.counter} does not gate {w.kind} workload {w.name} (never a "
                     f"REGRESSION): {text!r}")
        if (a.workload, a.counter) in seen:
            sys.exit(f"--approve-regression: {a.workload}:{a.counter} approved twice")
        seen.add((a.workload, a.counter))
        out.append(a)
    return out


def cmd_gate(args):
    wls = selected(args)
    sel = selection_info(args, wls)
    args.regression_approvals = regression_approvals(args, wls)
    t0 = time.monotonic()
    stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    entry = {"time": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "base_ref": args.base,
             "head_ref": args.head, "selection": sel, "harness": build.harness_hash(),
             "harness_tree": build.tool_tree("HEAD"), "escalate": args.escalate,
             "bench_change_approval": args.approve_bench_change,
             "new_from_refused_approval": args.approve_new_from_refused,
             "counter_source_approval": args.approve_counter_source,
             "regression_approvals": [a.text for a in args.regression_approvals]}
    try:
        with paths.locked():
            base_sha, head_sha = build.resolve(args.base), build.resolve(args.head)
            entry.update(base=base_sha, head=head_sha)
            dev = check_harness(args, base_sha)
            integration = build.resolve(args.integration_ref)
            entry.update(integration_ref=args.integration_ref, integration=integration)
            candidate = build.merge_candidate(base_sha, head_sha)
            tooling = build.toolchain_changes(base_sha, candidate)
            if tooling:
                raise build.BuildError(f"the head changes files the gate will not run cargo over "
                                       f"({', '.join(tooling)}): toolchain and cargo configuration changes are "
                                       "gated separately with the manager")
            entry.update(measured=candidate, measured_tree=build.git("rev-parse", f"{candidate}^{{tree}}"))
            anchored = anchor.load()
            entry["anchor"] = (anchored or {}).get("rev")
            base_m, head_m, rows, notes, ledger = run_gate(args, wls, base_sha, candidate, anchored,
                                                           strict=not dev)
            entry.update(ledger)
    except Exception as e:  # noqa: BLE001 - whatever stopped the gate, it is an ERROR with a ledger line
        if not isinstance(e, (build.BuildError, sandbox.SandboxError, OSError, invocation.ChainError)):
            e = build.BuildError(f"unexpected {type(e).__name__}: {e}")
        out = args.out or os.path.join(paths.sub("reports"), f"{stamp}-error")
        rows = list(getattr(e, "rows", ()))
        lines = [gate._line(r) for r in rows]
        write_text(os.path.join(out, "report.md"), "# PF0 counter gate: ERROR\n\n" +
                   ("\n".join([*gate.ROW_HEADER, *lines]) + "\n\n" if rows else "") + f"```\n{e}\n```\n")
        write_json(os.path.join(out, "report.json"), {"verdict": "ERROR", "rows": [r.__dict__ for r in rows],
                                                      "error": str(e)})
        entry.update(verdict="ERROR", authoritative=False, report=out,
                     flagged=[f"{r.workload}:{r.counter}:{r.status}" for r in rows] + [str(e)[:500]])
        log(f"gate ERROR: {e}\nledger: {record(entry)}")
        return gate.VERDICT_EXIT["ERROR"]

    reasons = [f"partial selection: {sel['selected']} of {sel['declared']} workloads"] if (
        sel["selected"] < sel["declared"] or not sel["default_file"]) else []
    if not sel["default_file"]:
        reasons.append(f"workloads file {sel['workloads_file']}")
    reasons += [f"dev harness: {p}" for p in dev]
    if build.node_bench_override():
        reasons.append(build.node_bench_override())
    if base_sha != integration:
        reasons.append(f"the base {base_sha[:12]} is not {args.integration_ref} ({integration[:12]})")
    if args.integration_ref != "node/integration":
        notes.append(f"integration ref overridden: {args.integration_ref}")
    complete = not reasons
    if candidate != head_sha:
        notes.insert(0, f"measured the merge of the head into the base ({candidate[:12]}, tree "
                        f"{entry['measured_tree'][:12]}); the merged tree must match it")
    notes += [f"non-authoritative ({r})" for r in reasons]
    v = gate.verdict(rows, complete)
    md = gate.render(rows, base_m, head_m, all_rows=args.all, notes=notes, complete=complete)
    md += f"\n_{len(wls)} workloads, {time.monotonic() - t0:.0f} s wall._\n"
    out = args.out or os.path.join(paths.sub("reports"), f"{stamp}-{head_sha[:12]}-vs-{base_sha[:12]}")
    write_text(os.path.join(out, "report.md"), md)
    write_json(os.path.join(out, "base.json"), base_m)
    write_json(os.path.join(out, "head.json"), head_m)
    write_json(os.path.join(out, "report.json"), gate.report_json(rows, complete))
    entry.update(verdict=v, authoritative=complete, report=out, wall_s=round(time.monotonic() - t0),
                 flagged=[f"{r.workload}:{r.counter}:{r.status}:{gate.pct(r.delta)}" for r in gate.flagged(rows)])
    print(md)
    log(f"report: {out}/report.md\nledger: {record(entry)}")
    return gate.VERDICT_EXIT[v]


def drift_text(d):
    return ", ".join(f"{k} {'rose from 0' if v is None else gate.pct(v)}" for k, v in sorted(d.items()))


def cmd_anchor(args):
    if not args.justification.strip():
        raise SystemExit("--justification is required: say why the counts at this revision are the new reference")
    wls = selected(args)
    sel = selection_info(args, wls)
    partial = sel["selected"] < sel["declared"] or not sel["default_file"]
    with paths.locked():
        sha = build.resolve(args.rev)
        if check_harness(args, sha) or build.node_bench_override():
            raise SystemExit("an anchor cannot be recorded with a dev harness or an overridden node-bench")
        old = anchor.load()
        if args.reset_drift and not old:
            raise SystemExit("--reset-drift only applies across a fingerprint change, and there is no anchor yet")
        m = measure.measure_rev("base", sha, wls, args.repeat, args.jobs, use_cache=not args.no_cache,
                                iters_from=anchor.iters(old), log=log)
        benches = [w for w in wls if w.kind == "bench"]
        bad = [w.name for w in benches if m["workloads"][w.name]["status"] != "ok"]
        if bad and not partial:
            raise SystemExit(f"a full anchor needs every bench workload measured; not ok: {', '.join(bad)}")
        # a node row that errored would be left out (or keep its old record) without a word: refuse instead
        bad = [f"{w.name} ({m['workloads'][w.name].get('error', '')[:200]})" for w in wls if w.kind == "node"
               and m["workloads"][w.name]["status"] not in ("ok", "pending", *gate.UNMEASURED)]
        if bad:
            raise SystemExit(f"node rows did not measure, so they cannot be anchored: {'; '.join(bad)}. Fix them, "
                             "or leave them out with --only")
        tree = build.git("rev-parse", f"{sha}^{{tree}}")
        incomparable = old and gate.fingerprint_diffs(old["fingerprint"], m["meta"]["fingerprint"],
                                                      gate.ANCHOR_COMPARABLE)
        if partial and (not old or incomparable):
            raise SystemExit("a partial re-anchor (--only or another workloads file) needs a comparable existing "
                             "anchor: anchor every workload")
        if args.reset_drift and not incomparable:
            raise SystemExit("--reset-drift only applies across a fingerprint change, and the anchor's fingerprint "
                             "is comparable: re-anchor without it (a plain re-anchor forgives the drift, logged in "
                             "anchors.jsonl)")
        carried = forgiven = None
        reset = None
        if incomparable and args.reset_drift:  # even when a gate of this tree could carry it (BL-313)
            log(f"  --reset-drift: the drift of anchor {old['rev'][:12]} is not carried; drift starts from zero")
            reset = {n: d for n, d in ((n, {k: v for k, v in d.items() if v is None or abs(v) > 1e-4})
                                       for n, d in (anchor.carried_drift(old, tree) or {}).items()) if d}
        elif incomparable:
            carried = anchor.carried_drift(old, tree)
            if carried is None:
                raise SystemExit(f"the fingerprint changed ({'; '.join(incomparable)}) and no authoritative gate "
                                 f"measured tree {tree[:12]} against anchor {old['rev'][:12]}, so the drift so far "
                                 "cannot be carried over. Gate that tree first, or pass --reset-drift and say why "
                                 "in the justification")
        elif old:
            forgiven = anchor.forgiven_drift(old, m)
        cur = workload_inputs(types.SimpleNamespace(sha=sha, wt=os.path.join(paths.root(), "worktrees", "base")),
                              wls)
        entry = anchor.record(m, args.justification, tree, sel, {w.name: w.key() for w in wls},
                              {n: {k: v for k, v in i.items() if k != "key"} for n, i in cur.items()},
                              old if partial else None, carried, forgiven, reset)
    print(measure.summary(m))
    for name, d in sorted((forgiven or {}).items()):
        log(f"  forgiven drift {name}: {drift_text(d)}")
    for name, d in sorted((reset or {}).items()):
        log(f"  discarded drift (--reset-drift) {name}: {drift_text(d)}")
    if carried:
        log(f"  carried the drift of {len(carried)} workloads across the fingerprint change")
    for name, w in sorted(entry["workloads"].items()):
        if w.get("uncarried") and name in m["workloads"]:
            log(f"  not carried (no ratio, so the measured median is anchored) {name}: {drift_text(w['uncarried'])}")
    log(f"anchored {len(entry['workloads'])} workloads at {sha[:12]}: {anchor.path()}")
    return 0


def cmd_clean(args):
    """Remove the gate's worktrees and scratch (and with --cache, the measurement cache) under the root."""
    root = paths.root()

    def du():
        p = subprocess.run(["du", "-sh", root], capture_output=True, text=True)
        return p.stdout.split()[0] if p.returncode == 0 else "?"

    before = du()
    with paths.locked():
        wtdir = os.path.join(root, "worktrees")
        for name in sorted(os.listdir(wtdir)) if os.path.isdir(wtdir) else []:
            build.git("worktree", "remove", "--force", os.path.join(wtdir, name), check=False)
            shutil.rmtree(os.path.join(wtdir, name), ignore_errors=True)
        build.git("worktree", "prune", check=False)
        for d in ("tmp", "exe", "shim", "home", "lib") + (("cache",) if args.cache else ()):
            shutil.rmtree(os.path.join(root, d), ignore_errors=True)
    log(f"{root}: {before} -> {du()}")
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(prog="perfgate", description="PF0 deterministic-counter performance gate")
    sub = ap.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("gate", help="measure the merge of head into base; exit 0 PASS, 1 FAIL, 2 ERROR, "
                       "3 INCOMPLETE")
    p.add_argument("--base", default="node/integration", help="base revision (default node/integration)")
    p.add_argument("--head", default="HEAD", help="head revision (default HEAD)")
    p.add_argument("--out", help="report directory (default $PERFGATE_ROOT/reports/<time>-<head>-vs-<base>)")
    p.add_argument("--all", action="store_true", help="list every counter in the report, not only flagged ones")
    p.add_argument("--escalate", type=int, default=10, help="extra runs per side for NOISY workloads (default 10)")
    p.add_argument("--approve-bench-change", metavar="TEXT", help="the manager's recorded approval of changed "
                   "bench inputs (the head's own benches are then measured too, for information)")
    p.add_argument("--approve-new-from-refused", metavar="TEXT", help="the manager's recorded approval of a node "
                   "row's first live measurement (NEW-FROM-REFUSED, PF0-N §7)")
    p.add_argument("--approve-counter-source", metavar="TEXT", help="the manager's recorded approval of a node row "
                   "whose counter source changed (a gated counter under 1%% of the base's or the anchor's, or "
                   "allocs 0 on one side only); the row must be re-anchored")
    p.add_argument("--approve-regression", metavar="WORKLOAD:COUNTER:+BOUND[%]:DEC-n", action="append",
                   help="the manager's ruling on an attributed regression (BL-406): that workload's counter is "
                   "APPROVED while its shift is at most BOUND (relative with %%, else absolute per run), and stays a "
                   "REGRESSION above it; the row must be re-anchored at the merge. Repeat for more rows")
    p.add_argument("--integration-ref", default="node/integration", help="the ref an authoritative gate's base "
                   "must be (default node/integration; recorded in the ledger)")
    add_selection(p, gate_cmd=True)

    p = sub.add_parser("anchor", help="record the drift anchor at a revision (DEC-52 3)")
    p.add_argument("rev")
    p.add_argument("--justification", required=True, help="why these counts are the new reference")
    p.add_argument("--reset-drift", action="store_true", help="after a fingerprint change: start drift from zero "
                   "instead of carrying it, even when a gate could carry it (say why in the justification); "
                   "refused without a fingerprint change")
    add_selection(p, repeat=ANCHOR_REPEAT)

    p = sub.add_parser("warm", help="build and measure the base slot ahead of the next gate")
    p.add_argument("--base", default="node/integration")
    add_selection(p)

    p = sub.add_parser("measure", help="measure one revision (or dashd) and print its noise floors")
    p.add_argument("rev", nargs="?", default="HEAD")
    p.add_argument("--label", default="measure", help="worktree slot to build in (default measure; `base` "
                   "seeds the gate's base cache)")
    p.add_argument("--dashd", metavar="BIN", help="measure the node workloads with this dashd instead")
    p.add_argument("--json", help="write the measurement here")
    p.add_argument("--md", help="write the noise-floor table here")
    add_selection(p)

    p = sub.add_parser("compare", help="re-judge two measurement files (NOISY counts as unresolved)")
    p.add_argument("base")
    p.add_argument("head")
    p.add_argument("--md")
    p.add_argument("--json")
    p.add_argument("--all", action="store_true")

    p = sub.add_parser("list", help="the declared workloads")
    p.add_argument("--workloads")

    p = sub.add_parser("clean", help="remove worktrees and scratch under the root (--cache: the cache too)")
    p.add_argument("--cache", action="store_true")

    args = ap.parse_args(argv)
    try:
        return {"gate": cmd_gate, "anchor": cmd_anchor, "warm": cmd_warm, "measure": cmd_measure,
                "compare": cmd_compare, "list": cmd_list, "clean": cmd_clean}[args.cmd](args)
    except SystemExit as e:  # usage errors carry a message: exit 2 (ERROR), never 1 (FAIL)
        if isinstance(e.code, str):
            print(f"perfgate: {e.code}", file=sys.stderr)
            return gate.VERDICT_EXIT["ERROR"]
        raise
    except (spec.SpecError, build.BuildError, sandbox.SandboxError, OSError, ValueError, KeyError) as e:
        print(f"perfgate: {e}", file=sys.stderr)
        return gate.VERDICT_EXIT["ERROR"]
