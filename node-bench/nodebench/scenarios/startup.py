"""Startup-to-ready, shutdown and idle memory."""

import dataclasses
import time

from .. import fixture
from .. import proc as procmod
from ..impls import Node, NodeConfig, free_port
from . import common, scenario

METRICS = {
    "startup_s": "Spawn until RPC answers getblockcount outside warmup (-28), polled every 5 ms. Warm page "
                 "cache: `warmup` untimed starts, then `reps` timed starts. Statistic: median.",
    "startup_cold_s": "As startup_s with the page cache dropped before every start (only with --drop-caches).",
    "shutdown_s": "`stop` RPC until the process has exited (blocking waitpid). Statistic: median.",
    "idle_rss_mib": "VmRSS 5 s after ready, no peers, no load. Statistic: median.",
}
REQUIRED = {"startup_s", "shutdown_s", "idle_rss_mib"}


def startup_loop(ctx, make_node, cold=False):
    a, r = ctx.args, ctx.result
    ready, stops, rss = [], [], []
    for i in range(a.warmup + a.reps):
        timed = i >= a.warmup
        if cold:
            common.drop_caches(ctx)
        with make_node() as node:
            t = node.wait_ready()
            if i == 0:
                common.verify_effective(ctx, node)
            time.sleep(5)
            mem = procmod.read_proc(node.pid)["rss_mib"]
            height = node.rpc.getblockcount()
            t0 = time.monotonic()
            node.stop()
            t_stop = node.t_exit - t0
        ctx.log(f"  start {i}{'' if timed else ' (warmup)'}: ready {t:.3f}s, stop {t_stop:.3f}s, "
                f"rss {mem:.0f}MiB, height {height}")
        if timed:
            ready.append(t)
            stops.append(t_stop)
            rss.append(mem)
    if cold:
        r.metric("startup_cold_s", ready, "s", definition=METRICS["startup_cold_s"])
        return
    r.metric("startup_s", ready, "s", definition=METRICS["startup_s"])
    r.metric("shutdown_s", stops, "s", definition=METRICS["shutdown_s"])
    r.metric("idle_rss_mib", rss, "MiB", definition=METRICS["idle_rss_mib"], target_2x=False)


@scenario("startup-regtest", "resources", "Startup-to-ready / shutdown / idle RSS on the regtest fixture chain",
          metrics=METRICS, defaults={"reps": 10, "warmup": 1}, required=REQUIRED)
def startup_regtest(ctx):
    datadir, meta = fixture.sync_from_seed(ctx)
    ctx.result.info("chain_height", meta["height"], "blocks")
    common.barrier(ctx, "start")
    startup_loop(ctx, lambda: fixture.isolated_node(ctx, datadir))


@scenario("startup-mainnet", "resources",
          "Startup-to-ready / shutdown / idle RSS on a fully synced mainnet datadir (warm; cold with --drop-caches)",
          metrics=METRICS, defaults={"reps": 5, "warmup": 1, "dbcache": 4096, "txindex": 1},
          required=lambda p: REQUIRED | ({"startup_cold_s"} if p.get("drop_caches") else set()),
          accepts=("source_datadir", "drop_caches"))
def startup_mainnet(ctx):
    a = ctx.args
    src = a.source_datadir or common.seed_datadir("main")
    ctx.result.doc["params"]["source_datadir"] = src
    with common.cloned(ctx, src) as datadir:
        cfg = NodeConfig(chain="main", datadir=datadir, connect=["0"], txindex=bool(a.txindex),
                         dbcache_mib=a.dbcache)

        def make():
            return Node(ctx.impl, dataclasses.replace(cfg, rpcport=free_port(), p2pport=free_port()),
                        cpu_slice=ctx.node_slice)

        common.barrier(ctx, "start")
        startup_loop(ctx, make)
        if a.drop_caches and not common.per_start_cache_drop_possible(ctx):
            raise RuntimeError("startup_cold_s needs a cache drop before every start, which a concurrent pair "
                               "cannot do: use --mode interleave")
        if a.drop_caches:
            startup_loop(ctx, make, cold=True)
