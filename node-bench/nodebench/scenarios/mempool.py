"""Mempool acceptance and block-template construction (regtest fixture and live mainnet)."""

import os
import statistics
import time

from .. import fixture, load, logparse
from .. import proc as procmod
from ..impls import Node, NodeConfig
from ..rpc import RpcError
from . import common, scenario

LAT_TXS = 2000  # sequential latency sample per repetition
GBT_RESERVE = 200  # fixture txs kept back to perturb the mempool between GBT samples

MEMPOOL_METRICS = {
    "tx_accept_latency_ms": f"sendrawtransaction round trip, one client, back to back, {LAT_TXS} independent "
                            "fixture txs per repetition into an initially empty mempool. Sample = the median of "
                            "one repetition; statistic: median over repetitions.",
    "tx_accept_latency_p99_ms": "p99 over all calls of all repetitions (information).",
    "tx_accept_tps": "Accepted txs/s with `clients` concurrent RPC clients (separate processes on the aux slice) "
                     "submitting `txs` distinct fixture txs as fast as possible; wall time from first send to "
                     "last response. One value per repetition (fresh node, empty mempool). Statistic: median.",
    "rpc_ceiling_ops": "Same clients calling getbestblockhash for 5 s: the load generator's ceiling. If "
                       "tx_accept_tps approaches it the measurement is client-bound.",
    "mempool_rss_mib": "Node VmRSS with the full mempool loaded.",
}

GBT_METRICS = {
    "gbt_ms_mempool_<N>": "getblocktemplate round trip with N fixture txs in the mempool. Before each sample one "
                          "more tx is submitted and we wait 5.2 s, so dashd's template cache (reused unless the "
                          "mempool changed AND >5 s passed) cannot serve it: every sample is a fresh "
                          "construction. Statistic: median over `reps` samples.",
    "cnb_total_ms_mempool_<N>": "dashd only: CreateNewBlock() total from -debug=bench (server side).",
    "gbt_txs_mempool_<N>": "Transactions in the template (sanity: the template is actually filled).",
}


def submit_all(ctx, node, txs, clients):
    calls = [("sendrawtransaction", [t["hex"]]) for t in txs]
    return load.run_parallel(node, calls, clients[0], load.aux_cpus(ctx.aux_slice))


@scenario("mempool-accept", "mempool", "Transaction acceptance latency and throughput (regtest load generator)",
          metrics=MEMPOOL_METRICS, defaults={"reps": 5, "txs": 20000, "clients": [8]},
          required={"tx_accept_latency_ms", "tx_accept_tps", "mempool_rss_mib"})
def mempool_accept(ctx):
    a, r = ctx.args, ctx.result
    datadir, meta = fixture.sync_from_seed(ctx)
    txs = fixture.load_txs()
    need = LAT_TXS + a.txs
    if need > len(txs):
        raise SystemExit(f"fixture has {len(txs)} txs, need {need}")
    lat, rep_lat, tps, ceil, rss = [], [], [], [], []
    common.barrier(ctx, "start")
    for rep in range(a.reps):
        with fixture.isolated_node(ctx, datadir) as node:
            node.wait_ready(600)
            if rep == 0:
                common.verify_effective(ctx, node)
            seq = load.run_sequential(node.rpc, [("sendrawtransaction", [t["hex"]]) for t in txs[:LAT_TXS]])
            lat += seq
            rep_lat.append(statistics.median(seq) * 1000)
            out = submit_all(ctx, node, txs[LAT_TXS:need], a.clients)
            if out["errors"]:
                raise RuntimeError(f"{out['errors']} submissions failed: {out['first_error']}")
            size = node.rpc.getmempoolinfo()["size"]
            if size != need:
                raise RuntimeError(f"mempool has {size} txs, expected {need}")
            tps.append(out["ops_per_s"])
            c = load.run_parallel(node, [("getbestblockhash", [])], a.clients[0], load.aux_cpus(ctx.aux_slice),
                                  duration=5)
            ceil.append(c["ops_per_s"])
            rss.append(procmod.read_proc(node.pid)["rss_mib"])
            ctx.log(f"  rep {rep}: {out['ops_per_s']:.0f} tx/s, latency median {rep_lat[-1]:.3f} ms, "
                    f"client ceiling {c['ops_per_s']:.0f} ops/s")
    r.metric("tx_accept_latency_ms", rep_lat, "ms", definition=MEMPOOL_METRICS["tx_accept_latency_ms"])
    r.metric("tx_accept_latency_p99_ms", [x * 1000 for x in lat], "ms", statistic="p99", gate=False,
             definition=MEMPOOL_METRICS["tx_accept_latency_p99_ms"], max_samples=2000)
    r.metric("tx_accept_tps", tps, "tx/s", better="higher", definition=MEMPOOL_METRICS["tx_accept_tps"])
    r.metric("rpc_ceiling_ops", ceil, "ops/s", better="higher", gate=False,
             definition=MEMPOOL_METRICS["rpc_ceiling_ops"])
    r.metric("mempool_rss_mib", rss, "MiB", definition=MEMPOOL_METRICS["mempool_rss_mib"], target_2x=False)
    r.info("fixture_tip", meta["tip"])
    if tps and ceil and statistics.median(tps) > 0.7 * statistics.median(ceil):
        r.note("tx_accept_tps is within 30% of the client ceiling: the load generator may be the bottleneck")


@scenario("gbt-regtest", "mining", "getblocktemplate construction time vs mempool size (regtest fixture)",
          metrics=GBT_METRICS, defaults={"reps": 10, "clients": [8], "mempool_sizes": [0, 1000, 5000, 20000]},
          required=lambda p: {f"gbt_ms_mempool_{n}" for n in p.get("mempool_sizes") or []})
def gbt_regtest(ctx):
    a, r = ctx.args, ctx.result
    datadir, _ = fixture.sync_from_seed(ctx)
    txs = fixture.load_txs()
    reserve = txs[-GBT_RESERVE:]
    common.barrier(ctx, "start")
    for size in a.mempool_sizes:
        if size + a.reps > len(txs) - GBT_RESERVE:
            raise SystemExit(f"mempool size {size} exceeds the fixture")
        with fixture.isolated_node(ctx, datadir, debug=["bench"]) as node:
            node.wait_ready(600)
            common.verify_effective(ctx, node)
            if size:
                out = submit_all(ctx, node, txs[:size], a.clients)
                if out["errors"]:
                    raise RuntimeError(f"fill failed: {out['first_error']}")
            log = ctx.impl.log_path(node.cfg)
            offset = os.path.getsize(log) if os.path.exists(log) else 0
            samples, ntx = [], []
            node.rpc.getblocktemplate()  # first template after startup; untimed
            for i in range(a.reps):
                node.rpc.sendrawtransaction(reserve[i]["hex"])
                time.sleep(5.2)
                t = time.monotonic()
                tmpl = node.rpc.getblocktemplate()
                samples.append((time.monotonic() - t) * 1000)
                ntx.append(len(tmpl["transactions"]))
            mempool = node.rpc.getmempoolinfo()
        r.metric(f"gbt_ms_mempool_{size}", samples, "ms", definition=GBT_METRICS["gbt_ms_mempool_<N>"])
        r.info(f"gbt_txs_mempool_{size}", statistics.median_high(ntx), "txs", GBT_METRICS["gbt_txs_mempool_<N>"])
        r.info(f"mempool_bytes_{size}", mempool["bytes"], "B")
        if ctx.impl.log_format() == "dashd" and os.path.exists(log):
            cnb = logparse.parse_create_new_block(log, offset)[-a.reps:]
            if cnb:
                r.metric(f"cnb_total_ms_mempool_{size}", [c["total_ms"] for c in cnb], "ms", gate=False,
                         definition=GBT_METRICS["cnb_total_ms_mempool_<N>"])
        ctx.log(f"  mempool {size}: gbt median {statistics.median(samples):.2f} ms, "
                f"{statistics.median_high(ntx)} txs in template")


GBT_MAINNET_METRICS = {
    "gbt_ms_mainnet": "getblocktemplate round trip on a node synced to the live mainnet tip with its real "
                      "mempool. Before each sample one mempool tx is re-prioritised by ±1 duff "
                      "(prioritisetransaction bumps the mempool's update counter) and we wait 5.2 s, so every "
                      "sample is a fresh construction. Statistic: median. Information only: the live mempool differs "
                      "between runs.",
    "cnb_total_ms_mainnet": "dashd only: CreateNewBlock() total from -debug=bench.",
}


@scenario("gbt-mainnet", "mining", "getblocktemplate on a synced mainnet node with the live network mempool",
          metrics=GBT_MAINNET_METRICS, defaults={"reps": 20, "dbcache": 4096, "txindex": 1}, long=True,
          accepts=("source_datadir",))
def gbt_mainnet(ctx):
    a, r = ctx.args, ctx.result
    src = a.source_datadir or common.seed_datadir("main")
    samples, sizes = [], []
    with common.cloned(ctx, src) as datadir:
        cfg = NodeConfig(chain="main", datadir=datadir, txindex=bool(a.txindex), dbcache_mib=a.dbcache,
                         debug=["bench"])
        log = ctx.impl.log_path(cfg)
        with Node(ctx.impl, cfg, cpu_slice=ctx.node_slice) as node:
            node.wait_ready(3600)
            common.verify_effective(ctx, node)
            deadline = time.monotonic() + 3 * 3600
            while True:  # catch up with the live tip and finish masternode/governance sync
                bci = node.rpc.getblockchaininfo()
                synced = not bci["initialblockdownload"] and bci["blocks"] == bci["headers"]
                try:
                    synced = synced and node.rpc.mnsync("status").get("IsSynced", True)
                except RpcError:
                    pass
                if synced:
                    break
                if time.monotonic() > deadline:
                    raise TimeoutError("node did not reach the live tip / finish mnsync")
                time.sleep(10)
            ctx.log(f"  synced at {bci['blocks']}; sampling")
            offset = os.path.getsize(log) if os.path.exists(log) else 0
            sign = 1
            for _ in range(a.reps):
                pool = node.rpc.getrawmempool()
                if pool:
                    node.rpc.prioritisetransaction(pool[0], sign)
                    sign = -sign
                time.sleep(5.2)
                t = time.monotonic()
                tmpl = node.rpc.getblocktemplate()
                samples.append((time.monotonic() - t) * 1000)
                sizes.append(len(tmpl["transactions"]))
            r.info("chain_height", node.rpc.getblockcount(), "blocks")
        if ctx.impl.log_format() == "dashd" and os.path.exists(log):
            cnb = logparse.parse_create_new_block(log, offset)
            if cnb:
                r.metric("cnb_total_ms_mainnet", [c["total_ms"] for c in cnb], "ms", gate=False,
                         definition=GBT_MAINNET_METRICS["cnb_total_ms_mainnet"])
    # Not gated: each run sees a different live mempool, so the two sides never build the same template.
    r.metric("gbt_ms_mainnet", samples, "ms", gate=False, definition=GBT_MAINNET_METRICS["gbt_ms_mainnet"])
    r.info("gbt_txs_mainnet_median", statistics.median_high(sizes), "txs")
    if max(sizes, default=0) == 0:
        r.note("mainnet mempool was empty for every sample: the template measured only the coinbase")
