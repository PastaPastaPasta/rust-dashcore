"""Initial block download: real network and local-seed (controlled) variants."""

import contextlib
import csv
import functools
import os

from .. import logparse, paths
from ..impls import CHAIN_LABEL, Node, NodeConfig, requested_indexes
from ..rpc import RpcError
from . import common, scenario

IBD_METRICS = {
    "ibd_time_s": "Process spawn until block <target> became the tip, from the node's own per-block log of this "
                  "invocation (dashd: UpdateTip timestamp; falls back to RPC polling), or until every enabled "
                  "index reports synced if that is later. target = the seed's tip (local seed: its hash must "
                  "match and the node must leave IBD) or the height at which blocks == headers and "
                  "initialblockdownload=false (network). Empty datadir. One run per invocation. Gated for "
                  "full local-seed runs without bench logging; --stop-height runs stop via -stopatheight and "
                  "are information.",
    "header_sync_s": "Spawn until the header chain is downloaded: headers >= target (local seed), or the end of "
                     "the initial header burst, i.e. headers grow < 1/s over the next 60 s (network). Polled "
                     "every `interval`.",
    "time_to_rpc_ready_s": "Spawn until RPC answers outside warmup.",
    "blocks_per_s": "target / ibd_time_s (information; the gate uses ibd_time_s).",
    "cpu_s / peak_rss_mib / disk_total_gib": "Gated footprint (no 2x target on memory/disk).",
    "steady_rss_mib / cpu_ms_per_block / disk_<part>_gib": "Information.",
    "block_connect_ms_* / script_verify_txin_per_s": "Information from the node's own per-block log.",
    "time_to_height_*": "Seconds to reach selected heights (progress curve, information).",
}

VARIANTS = {
    # name: (chain, source, assumevalid)
    "ibd-mainnet-net": ("main", "net", None),
    "ibd-mainnet-net-noav": ("main", "net", "0"),
    "ibd-testnet-net": ("test", "net", None),
    "ibd-testnet-net-noav": ("test", "net", "0"),
    "ibd-mainnet-seed": ("main", "seed", None),
    "ibd-mainnet-seed-noav": ("main", "seed", "0"),
    "ibd-testnet-seed": ("test", "seed", None),
    "ibd-testnet-seed-noav": ("test", "seed", "0"),
}


def run_ibd(ctx, chain, source, assumevalid):
    a, r = ctx.args, ctx.result
    datadir = paths.writable(a.datadir or os.path.join(ctx.run_dir, "datadir"), "--datadir")
    if os.path.exists(datadir) and os.listdir(datadir):
        raise SystemExit(f"IBD datadir {datadir} is not empty (cold runs only)")
    cfg = NodeConfig(chain=chain, datadir=datadir, txindex=bool(a.txindex), dbcache_mib=a.dbcache, par=a.par,
                     assumevalid=assumevalid, debug=["bench"] if a.bench_log else [])
    need = requested_indexes(cfg)
    bounded = bool(a.stop_height)
    # Bounded runs (--stop-height) end at -stopatheight, so their index state is not verified: information only.
    gated = source == "seed" and not a.bench_log and not bounded
    if a.bench_log:
        r.note("bench logging on: timings are information only (logging cost differs between implementations)")
    if bounded:
        r.note(f"bounded at {a.stop_height} (-stopatheight): information only")
    marks, milestones = {}, sorted(set(a.milestones))
    state = {"headers_at": None, "target": a.stop_height}
    reference = None

    with contextlib.ExitStack() as stack:
        if source == "seed":
            seeds = stack.enter_context(common.seeds(ctx, chain, a.seeds, extra=["-maxuploadtarget=0"]))
            seed_height = seeds[0].rpc.getblockcount()
            if bounded and a.stop_height > seed_height:
                raise SystemExit(f"--stop-height {a.stop_height} is above the seed's tip {seed_height}")
            cfg.connect = [f"127.0.0.1:{x.cfg.p2pport}" for x in seeds]
            # The seeds' frozen tip can be older than the default max tip age; the node must still be able to
            # leave IBD, because initialblockdownload=false is part of the end condition.
            cfg.extra = ["-maxtipage=2000000000"]
            state["target"] = a.stop_height or seed_height
            reference = seeds[0].rpc.getblockhash(state["target"])
            r.doc["params"]["seed"] = {"height": seed_height, "tip": seeds[0].rpc.getbestblockhash(),
                                       "datadir": common.seed_datadir(chain), "count": len(seeds)}
            ctx.log(f"{len(seeds)} seed dashd(s) up on {', '.join(cfg.connect)} at height {seed_height}")
        if bounded:
            cfg.extra = list(cfg.extra) + [f"-stopatheight={state['target']}"]
        r.doc["params"]["node_config"] = cfg.__dict__
        common.barrier(ctx, "start")
        common.drop_caches(ctx)
        node = Node(ctx.impl, cfg, cpu_slice=ctx.node_slice, stdout_path=os.path.join(ctx.run_dir, "stdout.log"))
        ctx.log("argv: " + " ".join(node.argv()))
        stack.enter_context(node)
        ready = node.wait_ready()
        common.verify_effective(ctx, node)
        target = state["target"]

        def on_row(row, t):
            if state["headers_at"] is None and target and row["headers"] >= target:
                state["headers_at"] = t
            while milestones and row["blocks"] >= milestones[0]:
                marks[milestones.pop(0)] = t

        def is_done(row, rpc):
            if bounded:
                return False  # the node stops itself at the target
            if target:
                # Sampler rows lag by up to `interval`; confirm with direct calls once close. Done = the target
                # connected, out of IBD, and every enabled index synced.
                return (row["blocks"] >= target - 500 and common.call_retry(rpc.getblockcount) >= target
                        and not common.call_retry(rpc.getblockchaininfo)["initialblockdownload"]
                        and common.indexes_synced(rpc, need))
            return row["ibd"] == 0 and row["blocks"] == row["headers"] > 0 and common.indexes_synced(rpc, need)

        polled, samples, samples_path = common.track(ctx, node, is_done, on_row, label="IBD", exit_ok=bounded)
        if bounded:
            height, best = end_from_log(ctx, cfg, node, target, reference)
        else:
            bci = common.call_retry(node.rpc.getblockchaininfo)  # height and tip read together
            height, best = bci["blocks"], bci["bestblockhash"]
            if reference and common.call_retry(lambda: node.rpc.getblockhash(target)) != reference:
                raise RuntimeError(f"block {target} differs from the seed's {reference}: not the same chain")
            try:
                r.doc["params"]["mnsync_at_end"] = node.rpc.mnsync("status")
            except RpcError:
                pass
    done_height = target or height
    finalize(ctx, cfg, node, done_height, height, best, polled, ready, state["headers_at"], marks, samples,
             samples_path, gated, need_index=bool(need) and not bounded)
    common.keep_or_remove(ctx, datadir, {"chain": chain, "height": height, "tip": best, "txindex": cfg.txindex})


def end_from_log(ctx, cfg, node, target, reference):
    """A run that stopped itself (-stopatheight): its endpoint from this invocation's per-block log."""
    _, height, best = common.stopped_endpoint(ctx, cfg, node, target, reference)
    ctx.result.info("overshoot_blocks", height - target, "blocks", "Blocks connected past -stopatheight.")
    return height, best


def finalize(ctx, cfg, node, done_height, height, best, polled, ready, headers_at, marks, samples, samples_path,
             gated, need_index=False):
    """Record metrics. Core numbers first (and checkpointed), the rest best-effort."""
    r = ctx.result
    done, how = common.secondary(ctx, "per-block done time", common.precise_done, ctx, cfg, node, done_height,
                                 polled) or (polled, "poll")
    # Index sync can finish after the last block; the run is only done when both are.
    if need_index and polled > done:
        done, how = polled, "poll (index sync finished after the last block)"
    if headers_at is None:
        headers_at = headers_caught_up(samples)
    r.metric("ibd_time_s", [done], "s", definition=IBD_METRICS["ibd_time_s"], target_2x=gated, gate=gated,
             extra={"measured_by": how, "polled_s": polled})
    r.metric("header_sync_s", [headers_at], "s", definition=IBD_METRICS["header_sync_s"], gate=gated)
    r.metric("time_to_rpc_ready_s", [ready], "s", definition=IBD_METRICS["time_to_rpc_ready_s"], gate=False)
    r.metric("blocks_per_s", [done_height / done if done else None], "blocks/s", better="higher", gate=False,
             definition=IBD_METRICS["blocks_per_s"])
    r.info("height", height, "blocks")
    r.info("target_height", done_height, "blocks")
    r.info("tip", best)
    for h, t in sorted(marks.items()):
        r.info(f"time_to_height_{h}", t, "s")
    r.artifact("samples_csv", samples_path)
    r.artifact("debug_log", ctx.impl.log_path(cfg))
    r.checkpoint()
    common.secondary(ctx, "resource metrics", common.record_resources, ctx, cfg, samples, height, gated)
    common.secondary(ctx, "per-block log", common.record_block_log, ctx, cfg)


def headers_caught_up(samples, window=60.0, rate=1.0):
    """Network mode: when the initial header download ended, i.e. the first sample after which headers
    grow by less than `rate`/s over the next `window` seconds (new blocks arrive every ~2.5 min)."""
    rows = [x for x in samples if "headers" in x]
    j = 0
    for i, x in enumerate(rows):
        j = max(j, i)
        while j < len(rows) and rows[j]["t"] < x["t"] + window:
            j += 1
        if j == len(rows):
            return None
        if x["headers"] > 0 and (rows[j]["headers"] - x["headers"]) / (rows[j]["t"] - x["t"]) < rate:
            return x["t"]
    return None


class _Recovered:
    """Stand-in for a Node when rebuilding a result from a run directory."""

    def __init__(self, spawn_wall):
        self.spawn_wall = spawn_wall
        self.log_offset = 0


def recover(ctx, run_dir, chain, source, target=None, bench_log=False):
    """Rebuild an IBD result from samples.csv + the node's log after the harness failed late.

    Refuses unless the run demonstrably reached its target (a sample at the target, or an
    UpdateTip to it in the log): a node that crashed part-way must not become a fast "ok" run.
    """
    rows = []
    with open(os.path.join(run_dir, "samples.csv")) as f:
        for row in csv.DictReader(f):
            rows.append({k: float(v) for k, v in row.items() if v not in ("", None) and k != "probe_error"})
    if not rows:
        raise SystemExit("no samples to recover from")
    spawn_wall = rows[0]["wall"] - rows[0]["t"]
    cfg = NodeConfig(chain=chain, datadir=os.path.join(run_dir, "datadir"))
    if target:
        done_row = next((x for x in rows if x.get("blocks", 0) >= target), None)
        log = common.block_log_path(ctx, cfg)
        if done_row is None and not (log and logparse.tip_time(log, ctx.impl.log_format(), target)):
            raise SystemExit(f"the run never reached its target height {target}: nothing to recover")
    else:
        done_row = next((x for x in rows if x.get("ibd") == 0 and x.get("blocks") == x.get("headers", -1) > 0),
                        None)
        if done_row is None:
            raise SystemExit("the run never reached the network tip: nothing to recover")
    done_row = done_row or rows[-1]
    height = int(done_row["blocks"])
    ctx.result.note(f"recovered from {run_dir} after a harness failure")
    finalize(ctx, cfg, _Recovered(spawn_wall), target or height, int(rows[-1]["blocks"]), None, done_row["t"],
             None, None, {}, rows, os.path.join(run_dir, "samples.csv"), source == "seed" and not bench_log)


IBD_DEFAULTS = {"txindex": 1, "dbcache": 4096, "par": 0, "interval": 5.0, "bench_log": 0, "seeds": 4,
                "milestones": [250000, 500000, 1000000, 1500000, 2000000]}

def ibd_required(source):
    """Full local-seed IBD without bench logging is gated; network, bench-log and bounded runs are information."""
    def required(params):
        if source != "seed" or params.get("bench_log") or params.get("stop_height"):
            return set()
        return {"ibd_time_s", "header_sync_s"} | common.footprint_required(params)
    return required


for _name, (_chain, _src, _av) in VARIANTS.items():
    scenario(
        _name, "sync",
        f"{CHAIN_LABEL[_chain].capitalize()} IBD from "
        f"{'the real P2P network' if _src == 'net' else 'a local dashd seed over localhost'}"
        f"{', assumevalid=0 (verify every script)' if _av == '0' else ', default assumevalid'}",
        metrics=IBD_METRICS, long=True, required=ibd_required(_src),
        defaults=IBD_DEFAULTS if _src == "seed" else {k: v for k, v in IBD_DEFAULTS.items() if k != "seeds"},
        accepts=("stop_height", "datadir", "keep_datadir", "block_stats_from", "drop_caches"),
    )(functools.partial(run_ibd, chain=_chain, source=_src, assumevalid=_av))
