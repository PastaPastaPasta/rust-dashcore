"""node-bench command line: `run <scenario> --impl dashd|rust`, `compare`, `list`, ..."""

import argparse
import copy
import glob
import json
import os
import shutil
import signal
import sys
import time
import traceback

from . import envinfo, impls, limits, noise, paths, results
from .impls import make_impl
from .proc import cpu_set, pin_method, slice_cpus, slice_set
from .scenarios import load_all

REPO_ROOT = paths.REPO_ROOT


class Terminated(BaseException):
    """SIGTERM/SIGHUP: unwind through every `with Node(...)` so nothing is left running."""


def _terminate(signum, _frame):
    raise Terminated(signal.Signals(signum).name)


def install_handlers(handler):
    """SIGTERM always unwinds; SIGHUP too, unless it was ignored on purpose (nohup)."""
    signal.signal(signal.SIGTERM, handler)
    if signal.getsignal(signal.SIGHUP) is not signal.SIG_IGN:
        signal.signal(signal.SIGHUP, handler)


class Context:
    def __init__(self, args, impl, result, run_dir):
        self.args, self.impl, self.result, self.run_dir = args, impl, result, run_dir
        self.node_slice = args.node_slice or None
        self.aux_slice = args.aux_slice or None
        self._log = open(os.path.join(run_dir, "harness.log"), "a")
        self.monitor = None

    def mark(self, name):
        if self.monitor:
            self.monitor.mark(name)

    def close(self):
        self._log.close()

    def log(self, msg):
        line = f"[{time.strftime('%H:%M:%S')}] {msg}"
        print(line, flush=True)
        self._log.write(line + "\n")
        self._log.flush()


def add_run_params(p):
    g = p.add_argument_group("scenario parameters (defaults per scenario; see `list -v`)")
    g.add_argument("--reps", type=int, help="repetitions (micro-metrics)")
    g.add_argument("--warmup", type=int, help="untimed warmup iterations")
    g.add_argument("--datadir", help="node datadir (IBD: must be empty)")
    g.add_argument("--keep-datadir", nargs="?", const=True, help="keep the datadir (optionally move it here)")
    g.add_argument("--stop-height", type=int, help="IBD: stop at this height (bounded run)")
    g.add_argument("--seeds", type=int, help="local seed nodes serving the chain (seeded IBD, spv-headers)")
    g.add_argument("--txindex", type=int, choices=(0, 1))
    g.add_argument("--addressindex", type=int, choices=(0, 1), help="address/spent/timestamp indexes")
    g.add_argument("--dbcache", type=int, help="MiB")
    g.add_argument("--par", type=int, help="script verification threads (0=auto)")
    g.add_argument("--interval", type=float, help="resource sampling interval, seconds")
    g.add_argument("--bench-log", type=int, choices=(0, 1), help="enable per-block bench logging (-debug=bench)")
    g.add_argument("--milestones", type=lambda s: [int(x) for x in s.split(",")], help="heights to timestamp")
    g.add_argument("--block-stats-from", type=int, help="ignore blocks below this height in per-block stats")
    g.add_argument("--source-datadir", help="reindex/rescan/rpc: chain snapshot to clone")
    g.add_argument("--chain", choices=("main", "test", "regtest"))
    g.add_argument("--txs", type=int, help="mempool/gbt: transactions to submit")
    g.add_argument("--clients", type=lambda s: [int(x) for x in s.split(",")],
                   help="concurrent RPC clients, comma list (rpc-*: one throughput run per value)")
    g.add_argument("--duration", type=float, help="seconds per throughput measurement")
    g.add_argument("--mempool-sizes", type=lambda s: [int(x) for x in s.split(",")], help="gbt: mempool sizes")
    g.add_argument("--filter", help="microbench: bench_dash -filter regex")
    g.add_argument("--dashd-bench-bin", help="microbench: bench_dash binary (default: built from the pinned tag)")
    g.add_argument("--rust-bench-bin", help="Rust bench binary for --impl rust (microbench: nanobench-style "
                                            "JSON; spv-headers: dash-spv-bench)")
    g.add_argument("--drop-caches", action="store_true",
                   help="drop the page cache before cold measurements (host-wide: slows everyone else)")
    g.add_argument("--keep-failed", action="store_true", help="keep run datadirs after a failed or interrupted run")
    g.add_argument("--min-free-gib", type=float,
                   help=f"refuse to start with less free space (default {LONG_FREE_GIB} for long scenarios, else 20)")


def add_noise_params(p):
    g = p.add_argument_group("noise control")
    g.add_argument("--quiet-window", nargs="?", type=float, const=noise.QUIET_SLICE, default=None,
                   metavar="MAXFRAC", help="wait before each scenario until foreign CPU load on the node slice is "
                   f"<= MAXFRAC (default {noise.QUIET_SLICE}); required for official baselines")
    g.add_argument("--quiet-timeout", type=float, default=6 * 3600, help="give up waiting after this many s")
    g.add_argument("--reservation", help="the manager-reserved quiet window this runs in (DEC-23(b)); required, "
                                         "with --quiet-window and a quiet measurement, for an official result")
    g.add_argument("--pair-quiet-window", type=float, help=argparse.SUPPRESS)  # set by `pair`: it checked quiet
    g.add_argument("--limits", help="cgroup caps for the node slice, e.g. cpu=12,mem=24G,io=100")


def add_gate_params(p):
    g = p.add_argument_group("gate (PRD NFR-PERF-02)")
    g.add_argument("--effect", type=float, default=0.01, help="minimum slowdown that counts (default 0.01)")
    g.add_argument("--alpha", type=float, default=0.05, help="Mann-Whitney significance level (default 0.05)")
    g.add_argument("--strict", action="store_true",
                   help="also block when non-inferiority is not shown (speedup CI below 1 - margin)")
    g.add_argument("--margin", type=float, default=0.02, help="non-inferiority margin for --strict")


def build_parser():
    ap = argparse.ArgumentParser(prog="node-bench/run", description=__doc__)
    sub = ap.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("run", help="run scenario(s) against one implementation")
    p.add_argument("scenarios", nargs="+", help="scenario names, or 'quick' / 'all'")
    p.add_argument("--impl", default="dashd", choices=("dashd", "rust"))
    p.add_argument("--dashd-bin", help="dashd binary (default: pinned release under NODE_BENCH_ROOT)")
    p.add_argument("--rust-bin", help="Rust node daemon (dashd-compatible flags)")
    p.add_argument("--rust-cli", help="Rust node CLI (dash-cli compatible), optional")
    p.add_argument("--node-slice", default="numa:0", help="CPU slice for the node under test ('' = unpinned)")
    p.add_argument("--aux-slice", default="numa:1", help="CPU slice for seeds and load generators")
    p.add_argument("--out", default=paths.results_dir(), help="results directory")
    p.add_argument("--tag", help="label stored with the result (e.g. PR number)")
    add_noise_params(p)
    p.add_argument("--pair", help=argparse.SUPPRESS)  # set by `pair`: "<pair id>:<round>:<side>"
    add_run_params(p)

    p = sub.add_parser("pair", help="paired dashd-vs-rust comparison (interleaved or concurrent rounds)")
    p.add_argument("scenarios", nargs="*")
    p.add_argument("--a", default="dashd", choices=("dashd", "rust"), help="baseline side (default dashd)")
    p.add_argument("--b", default="rust", choices=("dashd", "rust"),
                   help="candidate side (default rust; dashd/dashd gives the A/A noise floor)")
    p.add_argument("--a-bin", help="daemon for side A only (e.g. --a rust --a-bin <previous integration head>)")
    p.add_argument("--b-bin", help="daemon for side B only")
    p.add_argument("--mode", default="interleave", choices=("interleave", "concurrent"))
    p.add_argument("--rounds", type=int, default=6, help="even, so ABBA order / slice swaps balance (default 6)")
    p.add_argument("--slice-a", default=None, help="node slice for side A (concurrent default 0-11@0)")
    p.add_argument("--slice-b", default=None, help="node slice for side B (concurrent default 16-27@1)")
    p.add_argument("--aux-a", default=None, help="side A seeds/load generators (concurrent default 12-15)")
    p.add_argument("--aux-b", default=None, help="side B seeds/load generators (concurrent default 28-31)")
    p.add_argument("--dashd-bin")
    p.add_argument("--rust-bin")
    p.add_argument("--rust-cli")
    p.add_argument("--out", default=paths.results_dir())
    p.add_argument("--tag")
    add_gate_params(p)
    p.add_argument("--analyze", help="only (re)analyze an existing pair id")
    p.add_argument("--no-confirm", action="store_true", help="do not re-run scenarios with a FAIL to confirm it")
    add_noise_params(p)
    add_run_params(p)

    p = sub.add_parser("list", help="list scenarios")
    p.add_argument("-v", "--verbose", action="store_true")

    sub.add_parser("env", help="print the environment capture")

    p = sub.add_parser("compare", help="compare rust vs dashd results")
    p.add_argument("baseline", help="dashd result JSON (or a directory: latest per scenario)")
    p.add_argument("candidate", help="rust result JSON (or directory)")
    p.add_argument("--md", help="write the Markdown report here")
    p.add_argument("--json", help="write the machine-readable verdicts here")
    add_gate_params(p)

    p = sub.add_parser("summary", help="Markdown table of the latest result per scenario/impl")
    p.add_argument("dir", nargs="?", default=paths.results_dir())
    p.add_argument("--impl", default=None)
    p.add_argument("--md", help="write here instead of stdout")

    p = sub.add_parser("workload", help="one fixed-input workload, optionally under a counter wrapper (PF0)")
    p.add_argument("name", nargs="?")
    p.add_argument("--list", action="store_true")
    p.add_argument("--impl", default="dashd", choices=("dashd", "rust"))
    p.add_argument("--dashd-bin")
    p.add_argument("--rust-bin")
    p.add_argument("--rust-cli")
    p.add_argument("--wrap", help='command prefix for the node, e.g. "perf stat -x, -o /tmp/c.csv --"')
    p.add_argument("--json", help="write the manifest here (also printed)")
    p.add_argument("--keep", action="store_true", help="keep the run datadir")
    p.add_argument("--node-slice", default="numa:0")
    p.add_argument("--aux-slice", default="numa:1")
    p.add_argument("--out", default=paths.results_dir())
    p.add_argument("--txs", type=int)
    p.add_argument("--calls", type=int)
    p.add_argument("--call", default="getblock_v2", help="rpc workload: label (getblock_v2, gettxout, ...)")
    p.add_argument("--height", type=int, help="reindex/ibd workloads: stop height")
    p.add_argument("--gbt-mempool", type=int)
    p.add_argument("--gbt-calls", type=int)
    p.add_argument("--seeds", type=int)
    p.add_argument("--dbcache", type=int)
    p.add_argument("--par", type=int, help="script verification threads (default 1: deterministic)")
    p.add_argument("--timeout", type=float, help="seconds before a stop-at-height workload is killed (default 6 h)")
    p.add_argument("--ctl-fifo", help="perf control FIFO(s) CTL[,ACK]: 'enable'/'disable' at the work boundaries "
                                      "(use with perf stat -D -1 --control fifo:CTL[,ACK])")
    p.add_argument("--pf0", action="store_true", help="perfgate's PF0 mode: the daemon is the broker stub, the "
                                                      "paths and ports are pinned (see nodebench/pf0.py)")
    p.add_argument("--preflight", action="store_true", help="PF0 mode: only the refusal preflight of the row's "
                                                            "measured argv (manifest status ok/refused/error)")

    p = sub.add_parser("recover", help="rebuild an IBD result from a run directory after a harness failure")
    p.add_argument("run_dir")
    p.add_argument("--keep-datadir", help="move the recovered datadir here (e.g. a seed location)")
    p.add_argument("--out", default=paths.results_dir())

    p = sub.add_parser("fetch-dashd", help="download + verify the pinned dashd release")
    p.add_argument("--version", default=paths.DASHD_VERSION)
    return ap


QUICK = ["startup-regtest", "rpc-regtest", "mempool-accept", "gbt-regtest"]


def expand(names, registry):
    out = []
    for n in names:
        if n == "quick":
            out += QUICK
        elif n == "all":
            out += [s for s, sc in registry.items() if not sc.long and not sc.pending]
        elif n in registry:
            out.append(n)
        else:
            raise SystemExit(f"unknown scenario {n!r}; see `run list`")
    return out


LONG_FREE_GIB = 200  # free space a long (mainnet-scale) scenario needs before it starts
EVIDENCE_TAIL = 64 << 20  # bytes of each node log kept from a removed datadir


def retain(ctx, status, long_run=False):
    """Retention: datadirs left in the run directory are removed once the scenario is over; the tail of their
    node logs stays as evidence (run_dir/logs). Kept: the datadir --keep-datadir asked for, and after a failure
    every datadir of a long scenario, or of any scenario with --keep-failed or --keep-datadir (so `recover` can
    rebuild the result)."""
    a = ctx.args
    if status != "ok" and (a.keep_failed or a.keep_datadir or long_run):
        ctx.result.note(f"run datadirs kept after {status} in {ctx.run_dir}")
        return
    kept = ctx.result.doc["artifacts"].get("datadir")
    for d in glob.glob(os.path.join(ctx.run_dir, "*datadir")) + glob.glob(os.path.join(ctx.run_dir, "spv-storage")):
        if kept and paths.canon(d) == paths.canon(kept):
            continue
        for log in glob.glob(os.path.join(d, "**", "debug.log"), recursive=True) + glob.glob(
                os.path.join(d, "**", "bench.jsonl"), recursive=True):
            dst = os.path.join(ctx.run_dir, "logs", os.path.relpath(log, ctx.run_dir).replace(os.sep, "__"))
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            with open(log, "rb") as src, open(dst, "wb") as out:
                src.seek(max(0, os.path.getsize(log) - EVIDENCE_TAIL))
                shutil.copyfileobj(src, out)
        paths.safe_rmtree(d, "run datadir")


RUN_PARAMS = ("reps", "warmup", "datadir", "keep_datadir", "stop_height", "seeds", "txindex", "addressindex",
              "dbcache", "par", "interval", "bench_log", "milestones", "block_stats_from", "source_datadir", "chain",
              "txs", "clients", "duration", "mempool_sizes", "filter", "dashd_bench_bin", "rust_bench_bin",
              "drop_caches")


def check_params(args, sc):
    """Refuse scenario parameters the scenario does not use: a result must never describe a setting that did not
    take effect."""
    given = {k for k in RUN_PARAMS if getattr(args, k, None) is not None and getattr(args, k) is not False}
    ignored = given - set(sc.defaults) - set(sc.accepts)
    if ignored:
        raise SystemExit(f"{sc.name} does not use {', '.join('--' + k.replace('_', '-') for k in sorted(ignored))}")


def selected_scenarios(args):
    """The expanded scenario names, after refusing options any of them does not use."""
    registry = load_all()
    names = expand(args.scenarios, registry)
    for name in names:
        check_params(args, registry[name])
    return names


def cmd_run(args):
    paths.writable(args.out, "--out")
    registry = load_all()
    names = selected_scenarios(args)
    if args.node_slice and args.aux_slice:
        node_cpus, aux_cpus = slice_set(args.node_slice), slice_set(args.aux_slice)
        if node_cpus & aux_cpus:
            raise SystemExit(f"--node-slice {args.node_slice} and --aux-slice {args.aux_slice} overlap")
    impl = make_impl(args.impl, args)
    noise.ensure_mark()
    install_handlers(_terminate)
    # The harness itself (RPC clients, samplers) stays off the node's CPUs.
    if args.aux_slice:
        os.sched_setaffinity(0, cpu_set(slice_cpus(args.aux_slice)))
    if args.limits:
        limits.SLICE_LIMITS[args.node_slice or None] = limits.parse(args.limits)
        if "io" in limits.SLICE_LIMITS[args.node_slice or None] and not limits.io_weight_effective():
            print("warning: io.weight has no effect without the BFQ I/O scheduler", file=sys.stderr)
    pair = dict(zip(("id", "round", "side"), args.pair.rsplit(":", 2), strict=True)) if args.pair else None
    rc = 0
    for name in names:
        sc = registry[name]
        if sc.pending:
            print(f"== {name}: PENDING — {sc.pending}")
            continue
        a = copy.copy(args)
        for k, v in sc.defaults.items():
            if getattr(a, k, None) is None:
                setattr(a, k, v)
        for k in ("keep_datadir", "block_stats_from"):
            if getattr(a, k, None) is None:
                setattr(a, k, 0 if k == "block_stats_from" else False)
        env = envinfo.capture(paths.ROOT, REPO_ROOT, a.node_slice,
                              extra_repos=[os.path.expanduser("~/workspace/dash")])
        env["pinning"] = {"node": pin_method(a.node_slice), "aux": pin_method(a.aux_slice)}
        params = {k: v for k, v in vars(a).items() if k not in ("cmd", "scenarios", "func")}
        res = results.Result(name, impl.describe(), env, params,
                             f"-r{pair['round']}{pair['side']}" if pair else "")
        res.out_dir = a.out
        # Paired runs live under their pair id: it also marks every process of the pair in /proc.
        run_dir = os.path.join(a.out, "runs", *([pair["id"]] if pair else []), res.run_id)
        os.makedirs(run_dir)  # fails loudly rather than letting two runs share a datadir
        ctx = Context(a, impl, res, run_dir)
        ctx.log(f"== {name} [{impl.name}] -> {run_dir}")
        if pair:
            res.doc["pair"] = pair
        if args.tag:
            res.doc["tag"] = args.tag
        status = "ok"
        if args.quiet_window is not None and not noise.wait_for_quiet(
                ctx.log, args.quiet_window, node_slice=a.node_slice, timeout=args.quiet_timeout):
            res.note(f"host never got quiet within {args.quiet_timeout:.0f}s; not run")
            res.finish(a.out, "refused-noisy")
            ctx.log(f"== {name}: refused-noisy")
            ctx.close()
            rc = 1
            continue
        monitor = noise.LoadMonitor(a.node_slice)
        ctx.monitor = monitor
        monitor.start()
        load_start = None
        try:
            load_start = noise.snapshot()
            paths.check_free(a.out, a.min_free_gib or (LONG_FREE_GIB if sc.long else impls.MIN_FREE_GIB), name)
            sc.run(ctx)
        except (KeyboardInterrupt, Terminated) as e:
            status = "interrupted"
            rc = 130
            res.note(f"interrupted: {e!r}")
        except BaseException as e:  # record the failure, keep going with the next scenario
            status = "error"
            rc = 1
            res.note("error: " + "".join(traceback.format_exception_only(type(e), e)).strip())
            ctx.log(traceback.format_exc())
        if status != "ok" and pair:
            from .scenarios.common import abort_barrier

            abort_barrier(pair["side"])  # release the other side of a concurrent pair
        summary = monitor.stop()
        leaked = [p.pid for p in impls.live_groups()]
        if leaked:
            impls.reap_all()
            res.note(f"process groups still running after the scenario were killed: {leaked}")
            status = "error" if status == "ok" else status
            rc = rc or 1
        if limits.LEFTOVERS:
            res.note("cgroups that could not be removed: " + "; ".join(limits.LEFTOVERS))
            limits.LEFTOVERS.clear()
        quiet_window = args.quiet_window if args.quiet_window is not None else args.pair_quiet_window
        klass = noise.classify(summary, max_slice=quiet_window or noise.QUIET_SLICE, use_psi=False)
        try:
            load_end = noise.snapshot()
        except Exception as e:
            load_end = {"error": repr(e)}
        res.doc["load"] = {"start": load_start, "end": load_end, "during": summary, "class": klass,
                           "quiet_window": quiet_window, "reservation": args.reservation,
                           "official": quiet_window is not None and klass == "quiet" and bool(args.reservation)}
        try:
            retain(ctx, status, long_run=sc.long)
        except Exception as e:  # cleanup must never cost the result
            res.note(f"retention cleanup failed: {e!r}")
        path = res.finish(a.out, status)
        ctx.log(f"== {name}: {status} (host {klass}); result {path}")
        ctx.close()
        print(results.markdown(res.doc))
        if status == "interrupted":
            break
    return rc


def cmd_recover(args):
    """Turn an errored IBD run (harness failed after the node finished) into a result."""
    from .logparse import update_tip
    from .scenarios import common, ibd

    run_dir = paths.writable(args.run_dir.rstrip("/"), "run directory")
    paths.writable(args.out, "--out")
    run_id = os.path.basename(run_dir)
    old = results.load(os.path.join(args.out, f"{run_id}.json"))
    if old["scenario"] not in ibd.VARIANTS:
        raise SystemExit("recover handles IBD runs only")
    if old["status"] not in ("error", "partial", "interrupted"):
        raise SystemExit(f"{run_id} has status {old['status']}: nothing to recover")
    chain, source, _ = ibd.VARIANTS[old["scenario"]]
    params = old["params"]
    target = params.get("stop_height") or (params.get("seed") or {}).get("height")
    impl = make_impl(old["impl"]["impl"], argparse.Namespace(dashd_bin=old["impl"].get("daemon"),
                                                             rust_bin=old["impl"].get("daemon"), rust_cli=None))
    res = results.Result(old["scenario"], old["impl"], old["env"], params, "-recovered")
    res.out_dir = args.out
    res.doc["load"] = old.get("load")
    res.doc["recovered_from"] = old["run_id"]
    ns = argparse.Namespace(**{**params, "keep_datadir": args.keep_datadir})
    ctx = Context(ns, impl, res, run_dir)
    ibd.recover(ctx, run_dir, chain, source, target, bench_log=bool(params.get("bench_log")))
    height = int(res.doc["metrics"]["height"]["value"])
    cfg_log = impl.log_path(ibd.NodeConfig(chain=chain, datadir=os.path.join(run_dir, "datadir")))
    _, tip = update_tip(cfg_log, impl.log_format(), height) or (None, None)
    res.doc["metrics"]["tip"]["value"] = tip
    if args.keep_datadir:
        common.keep_or_remove(ctx, os.path.join(run_dir, "datadir"),
                              {"chain": chain, "height": height, "tip": tip, "txindex": bool(params.get("txindex"))})
    path = res.finish(args.out, "ok")
    print(results.markdown(res.doc))
    print(f"recovered result: {path}")
    return 0


def cmd_list(args):
    for name, sc in load_all().items():
        flag = " [PENDING]" if sc.pending else " [long]" if sc.long else ""
        print(f"{name:28} {sc.group:10} {sc.summary}{flag}")
        if args.verbose:
            if sc.pending:
                print(f"    pending: {sc.pending}")
            for m, d in sc.metrics.items():
                print(f"    - {m}: {d}")
            if sc.defaults:
                print(f"    defaults: {sc.defaults}")
    return 0


def cmd_env(_args):
    print(json.dumps(envinfo.capture(paths.ROOT, REPO_ROOT, "numa:0"), indent=1))
    return 0


def main(argv=None):
    try:
        return _main(argv)
    except paths.UnsafePath as e:
        raise SystemExit(f"node-bench: {e}") from None


def _main(argv=None):
    args = build_parser().parse_args(argv)
    paths.check_root()
    if args.cmd == "run":
        return cmd_run(args)
    if args.cmd == "list":
        return cmd_list(args)
    if args.cmd == "env":
        return cmd_env(args)
    if args.cmd == "compare":
        from .compare import cmd_compare

        return cmd_compare(args)
    if args.cmd == "pair":
        from .pair import cmd_pair

        return cmd_pair(args)
    if args.cmd == "summary":
        from .compare import cmd_summary

        return cmd_summary(args)
    if args.cmd == "recover":
        return cmd_recover(args)
    if args.cmd == "workload":
        from .workloads import cmd_workload

        noise.ensure_mark()
        install_handlers(_terminate)
        return cmd_workload(args)
    if args.cmd == "fetch-dashd":
        from .fetch import fetch_dashd

        return fetch_dashd(args.version)
    return 2


if __name__ == "__main__":
    sys.exit(main())
