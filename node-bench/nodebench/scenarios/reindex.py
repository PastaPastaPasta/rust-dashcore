"""Reindex, reindex-chainstate (pure local validation) and wallet rescan."""

import functools
import os
import time

from .. import paths
from ..impls import CHAIN_LABEL, Node, NodeConfig, requested_indexes
from . import common, scenario

METRICS = {
    "reindex_time_s": "Spawn with -reindex (rebuild block index + chainstate from the local block files) or "
                      "-reindex-chainstate (chainstate only) on a reflink clone of a synced datadir, no peers, "
                      "until block <source height> is the tip (per-block log timestamp) and every enabled index "
                      "reports synced. One run per invocation. Gated unless --bench-log 1.",
    "cpu_s / peak_rss_mib / disk_total_gib": "Gated footprint, as for IBD.",
    "blocks_per_s / block_connect_ms_* / script_verify_txin_per_s": "Information. Per-block numbers need "
                                                                    "--bench-log 1 (dashd -debug=bench).",
}

RESCAN_METRICS = {
    "rescan_time_s": "`rescanblockchain` from genesis to tip on an isolated synced node, in a blank watch-only "
                     "descriptor wallet that watches the coinbase payee of every 20000th block (deterministic, "
                     "real history). One untimed warmup rescan, then `reps` timed; statistic: median.",
    "rescan_blocks_per_s": "Tip height / rescan_time_s (information).",
}


def run_reindex(ctx, chain, mode, assumevalid):
    a, r = ctx.args, ctx.result
    src = paths.not_protected(a.source_datadir or common.seed_datadir(chain), "--source-datadir")
    meta = common.datadir_meta(src)
    target = meta["height"]
    gated = not a.bench_log
    if a.bench_log:
        r.note("bench logging on: timings are information only (logging cost differs between implementations)")
    r.doc["params"]["source"] = {"datadir": src, **meta}
    with common.cloned(ctx, src, what="a synced datadir (--source-datadir)") as datadir:
        cfg = NodeConfig(chain=chain, datadir=datadir, connect=["0"], txindex=bool(a.txindex),
                         addressindex=bool(a.addressindex), dbcache_mib=a.dbcache, par=a.par,
                         assumevalid=assumevalid, debug=["bench"] if a.bench_log else [],
                         reindex=mode == "full", reindex_chainstate=mode == "chainstate")
        r.doc["params"]["node_config"] = cfg.__dict__
        common.barrier(ctx, "start")
        common.drop_caches(ctx)
        node = Node(ctx.impl, cfg, cpu_slice=ctx.node_slice, stdout_path=os.path.join(ctx.run_dir, "stdout.log"))
        ctx.log("argv: " + " ".join(node.argv()))
        need = requested_indexes(cfg)
        with node:
            node.wait_ready()
            common.verify_effective(ctx, node)

            def is_done(row, rpc):
                return (row["blocks"] >= target - 500 and common.call_retry(rpc.getblockcount) >= target
                        and common.indexes_synced(rpc, need))

            polled, samples, samples_path = common.track(ctx, node, is_done, label="reindex")
            tip = common.call_retry(lambda: node.rpc.getblockhash(target))
        if tip != meta["tip"]:
            raise RuntimeError(f"block {target} is {tip} after the reindex, the source's is {meta['tip']}")
        done, how = common.secondary(ctx, "per-block done time", common.precise_done, ctx, cfg, node, target,
                                     polled) or (polled, "poll")
        # Index sync can finish after the last block; the run is only done when both are.
        if need and polled > done:
            done, how = polled, "poll (index sync finished after the last block)"
        ctx.log(f"reindex ({mode}) done in {done:.0f}s")
        r.metric("reindex_time_s", [done], "s", definition=METRICS["reindex_time_s"], gate=gated,
                 target_2x=gated, extra={"measured_by": how, "polled_s": polled})
        r.metric("blocks_per_s", [target / done], "blocks/s", better="higher", gate=False)
        r.info("height", target, "blocks")
        r.artifact("samples_csv", samples_path)
        r.checkpoint()
        common.secondary(ctx, "resource metrics", common.record_resources, ctx, cfg, samples, target, gated)
        common.secondary(ctx, "per-block log", common.record_block_log, ctx, cfg)
        if a.keep_datadir:
            common.keep_or_remove(ctx, datadir, {"chain": chain, "height": target, "tip": tip,
                                                 "txindex": cfg.txindex, "addressindex": cfg.addressindex})


VARIANTS = {
    # name: (chain, mode, assumevalid)
    "reindex-mainnet": ("main", "full", None),
    "reindex-chainstate-mainnet": ("main", "chainstate", None),
    "reindex-chainstate-mainnet-noav": ("main", "chainstate", "0"),
    "reindex-testnet": ("test", "full", None),
    "reindex-chainstate-testnet-noav": ("test", "chainstate", "0"),
}

for _name, (_chain, _mode, _av) in VARIANTS.items():
    scenario(
        _name, "sync",
        f"{CHAIN_LABEL[_chain].capitalize()} {'-reindex' if _mode == 'full' else '-reindex-chainstate'} from "
        f"local block files, no network{', assumevalid=0' if _av == '0' else ''}",
        metrics=METRICS, long=True,
        defaults={"txindex": 1, "addressindex": 0, "dbcache": 4096, "par": 0, "interval": 5.0, "bench_log": 0},
        required=lambda p: set() if p.get("bench_log") else {"reindex_time_s"} | common.footprint_required(p),
        accepts=("source_datadir", "keep_datadir", "block_stats_from", "drop_caches"),
    )(functools.partial(run_reindex, chain=_chain, mode=_mode, assumevalid=_av))


@scenario("rescan-mainnet", "wallet", "Wallet rescanblockchain over the whole mainnet chain (watch-only)",
          metrics=RESCAN_METRICS, defaults={"reps": 2, "dbcache": 4096, "txindex": 1}, long=True,
          required={"rescan_time_s"}, accepts=("source_datadir",))
def rescan_mainnet(ctx):
    a, r = ctx.args, ctx.result
    src = a.source_datadir or common.seed_datadir("main")
    times = []
    with common.cloned(ctx, src) as datadir:
        cfg = NodeConfig(chain="main", datadir=datadir, connect=["0"], txindex=bool(a.txindex),
                         dbcache_mib=a.dbcache, wallet=True)
        with Node(ctx.impl, cfg, cpu_slice=ctx.node_slice) as node:
            node.wait_ready(3600)
            common.verify_effective(ctx, node)
            tip = node.rpc.getblockcount()
            hashes = node.rpc.batch([("getblockhash", [h]) for h in range(20000, tip, 20000)])
            blocks = node.rpc.batch([("getblock", [h, 2]) for h in hashes])
            addrs = set()
            for b in blocks:
                spk = b["tx"][0]["vout"][0]["scriptPubKey"]
                addr = (spk.get("addresses") or [spk.get("address")])[0]
                if addr:
                    addrs.add(addr)
            descs = [{"desc": node.rpc.getdescriptorinfo(f"addr({x})")["descriptor"], "timestamp": "now"}
                     for x in sorted(addrs)]
            r.info("watched_addresses", len(descs), "addresses")
            common.barrier(ctx, "start")
            for i in range(a.reps + 1):  # rescan 0 warms the cache and is not timed
                name = f"rescan{i}"
                node.rpc.createwallet(name, True, True, "", False, True, False)
                w = node.new_rpc(timeout=36000, wallet=name)
                res = w.importdescriptors(descs)
                if not all(x["success"] for x in res):
                    raise RuntimeError(f"importdescriptors failed: {res[:3]}")
                t = time.monotonic()
                w.rescanblockchain(0)
                elapsed = time.monotonic() - t
                ctx.log(f"  rescan {i}{' (warmup)' if i == 0 else ''}: {elapsed:.1f}s")
                if i:
                    times.append(elapsed)
                bal = w.getbalances()
                r.info(f"rescan{i}_watchonly_trusted", bal.get("watchonly", bal.get("mine", {})).get("trusted"))
                w.close()
                node.rpc.unloadwallet(name)
    r.metric("rescan_time_s", times, "s", definition=RESCAN_METRICS["rescan_time_s"])
    r.metric("rescan_blocks_per_s", [tip / t for t in times], "blocks/s", better="higher", gate=False,
             definition=RESCAN_METRICS["rescan_blocks_per_s"])
