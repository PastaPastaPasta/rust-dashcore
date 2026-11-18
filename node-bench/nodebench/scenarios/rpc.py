"""JSON-RPC latency and throughput for the calls wallets, explorers and pools use."""

import random
import statistics

from .. import fixture, load, paths
from ..impls import Node, NodeConfig
from ..rpc import RpcError
from . import common, scenario

METRICS = {
    "rpc_<call>_ms": "Round-trip latency, one client on a keep-alive connection, `reps` sequential calls cycling "
                     "through a fixed deterministic parameter set (seed 42) after one untimed warmup pass over "
                     "it. Statistic: median.",
    "rpc_<call>_p99_ms": "p99 of the same samples (recorded, not gated).",
    "rpc_<call>_ops_c<N>": "Calls/s with N concurrent clients (processes on the aux slice, keep-alive) for "
                           "`duration` s over the same parameter set; N from --clients (default 16,64 per PRD "
                           "NFR #13; c1 is the latency metric). Statistic: the single measured value.",
}

# Every label build_workload produces; all are required on both sides (quorum_info only where the chain has
# quorums: the regtest fixture has no masternodes).
LABELS = ("getblockchaininfo", "getbestblockhash", "getblockcount", "getblockhash", "getblockheader", "getblock_v0",
          "getblock_v1", "getblock_v2", "getrawtransaction_v0", "getrawtransaction_v1", "gettxout", "getmempoolinfo",
          "getnetworkinfo", "getaddressbalance", "getaddressutxos", "getaddresstxids", "masternode_count",
          "masternodelist", "protx_list", "quorum_list")


def required(chain):
    def inventory(params):
        labels = LABELS + (("quorum_info",) if chain == "main" else ())
        out = {f"rpc_{label}_ms" for label in labels}
        return out | {f"rpc_{label}_ops_c{n}" for label in THROUGHPUT for n in params.get("clients") or []}
    return inventory


# Calls that also get a throughput measurement.
THROUGHPUT = ("getblockchaininfo", "getbestblockhash", "getblock_v1", "getrawtransaction_v1", "gettxout",
              "getaddressbalance")


# Each build_workload label's call, as (method, *subcommand): what PF0's preflight probes for the rpc row (D113).
CALL_METHODS = {
    **{x: (x,) for x in ("getblockchaininfo", "getbestblockhash", "getblockcount", "getblockhash", "getblockheader",
                         "gettxout", "getmempoolinfo", "getnetworkinfo", "getaddressbalance", "getaddressutxos",
                         "getaddresstxids", "masternodelist")},
    **{f"getblock_v{v}": ("getblock",) for v in range(3)},
    **{f"getrawtransaction_v{v}": ("getrawtransaction",) for v in range(2)},
    "masternode_count": ("masternode", "count"),
    "protx_list": ("protx", "list"),
    "quorum_list": ("quorum", "list"),
    "quorum_info": ("quorum", "info"),
}


def build_workload(rpc, heights, rng, addressindex):
    """{label: [(method, params)]} sampled from the node's own chain."""
    hashes = rpc.batch([("getblockhash", [h]) for h in heights])
    blocks = rpc.batch([("getblock", [h, 2]) for h in hashes])
    txids, outpoints, addrs = [], [], []
    for b in blocks:
        for tx in b["tx"][1:] or b["tx"]:
            txids.append(tx["txid"])
            for o in tx["vout"]:
                spk = o.get("scriptPubKey", {})
                if spk.get("addresses") or spk.get("address"):
                    addrs.append((spk.get("addresses") or [spk.get("address")])[0])
                outpoints.append((tx["txid"], o["n"]))
    pick = lambda xs, n: rng.sample(xs, min(n, len(xs))) if xs else []  # noqa: E731
    w = {
        "getblockchaininfo": [("getblockchaininfo", [])],
        "getbestblockhash": [("getbestblockhash", [])],
        "getblockcount": [("getblockcount", [])],
        "getblockhash": [("getblockhash", [h]) for h in heights],
        "getblockheader": [("getblockheader", [h, True]) for h in hashes],
        "getblock_v0": [("getblock", [h, 0]) for h in hashes],
        "getblock_v1": [("getblock", [h, 1]) for h in hashes],
        "getblock_v2": [("getblock", [h, 2]) for h in hashes],
        "getrawtransaction_v0": [("getrawtransaction", [t, False]) for t in pick(txids, 500)],
        "getrawtransaction_v1": [("getrawtransaction", [t, True]) for t in pick(txids, 500)],
        "gettxout": [("gettxout", [t, n]) for t, n in pick(outpoints, 500)],
        "getmempoolinfo": [("getmempoolinfo", [])],
        "getnetworkinfo": [("getnetworkinfo", [])],
    }
    if addressindex:
        sample = pick(sorted(set(addrs)), 200)
        w["getaddressbalance"] = [("getaddressbalance", [{"addresses": [a]}]) for a in sample]
        w["getaddressutxos"] = [("getaddressutxos", [{"addresses": [a]}]) for a in sample]
        w["getaddresstxids"] = [("getaddresstxids", [{"addresses": [a]}]) for a in sample]
    # Masternode / quorum state (deterministic MN list and LLMQs from evodb).
    w["masternode_count"] = [("masternode", ["count"])]
    w["masternodelist"] = [("masternodelist", ["json"])]
    w["protx_list"] = [("protx", ["list", "valid", False])]
    w["quorum_list"] = [("quorum", ["list"])]
    try:
        quorums = [(LLMQ_TYPES.get(name), h) for name, hs in rpc.quorum("list").items() for h in hs[:2]]
        info = [("quorum", ["info", t, h]) for t, h in quorums if t is not None][:6]
        if info:
            w["quorum_info"] = info
    except RpcError as e:
        if e.code != -32601:
            raise
        # Unsupported: quorum_list reports it; where quorum_info is required it is then MISSING (blocks).
    return w


# `quorum list` reports names, `quorum info` takes the numeric type (src/llmq/params.h).
LLMQ_TYPES = {"llmq_50_60": 1, "llmq_400_60": 2, "llmq_400_85": 3, "llmq_100_67": 4, "llmq_60_75": 5,
              "llmq_25_67": 6, "llmq_test": 100, "llmq_devnet": 101, "llmq_test_v17": 102,
              "llmq_test_dip0024": 103, "llmq_test_instantsend": 104, "llmq_devnet_dip0024": 105,
              "llmq_test_platform": 106, "llmq_devnet_platform": 107}


def measure(ctx, node, workload, reps, clients, duration):
    r = ctx.result
    unsupported, failed = [], []
    common.barrier(ctx, "start")
    for label, calls in workload.items():
        try:
            load.run_sequential(node.rpc, calls[:reps])  # warmup + support probe
        except RpcError as e:
            if e.code == -32601:
                unsupported.append(label)
            else:
                failed.append(f"{label}: {e}")
            continue
        seq = [calls[i % len(calls)] for i in range(reps)]
        lat = [x * 1000 for x in load.run_sequential(node.rpc, seq)]
        r.metric(f"rpc_{label}_ms", lat, "ms", definition=METRICS["rpc_<call>_ms"], max_samples=1000)
        r.metric(f"rpc_{label}_p99_ms", lat, "ms", statistic="p99", gate=False,
                 definition=METRICS["rpc_<call>_p99_ms"], max_samples=1000)
        line = f"  {label:24} median {statistics.median_high(lat):8.3f} ms"
        if label in THROUGHPUT:
            for n in clients:
                out = load.run_parallel(node, calls, n, load.aux_cpus(ctx.aux_slice), duration=duration)
                if out["errors"]:
                    failed.append(f"{label} (c{n}): {out['errors']} errors, e.g. {out['first_error']}")
                    continue
                r.metric(f"rpc_{label}_ops_c{n}", [out["ops_per_s"]], "ops/s", better="higher",
                         definition=METRICS["rpc_<call>_ops_c<N>"])
                line += f", {out['ops_per_s']:.0f} ops/s @c{n}"
        ctx.log(line)
    if unsupported:
        r.note("unsupported RPCs (method not found): " + ", ".join(unsupported))
        r.info("unsupported_rpcs", ",".join(unsupported))
    if failed:
        # Errors are not "missing features": they fail the scenario, so the gate blocks.
        raise RuntimeError("RPC calls failed: " + "; ".join(failed))


@scenario("rpc-regtest", "rpc", "RPC latency/throughput on the regtest fixture (txindex + addressindex)",
          metrics=METRICS, defaults={"reps": 500, "clients": [16, 64], "duration": 5.0}, required=required("regtest"))
def rpc_regtest(ctx):
    a = ctx.args
    datadir, meta = fixture.sync_from_seed(ctx, cfg_overrides={"addressindex": True})
    with fixture.isolated_node(ctx, datadir, addressindex=True) as node:
        node.wait_ready(600)
        common.verify_effective(ctx, node)
        rng = random.Random(42)
        heights = list(range(1, meta["height"] + 1))
        workload = build_workload(node.rpc, heights, rng, addressindex=True)
        measure(ctx, node, workload, a.reps, a.clients, a.duration)


@scenario("rpc-mainnet", "rpc", "RPC latency/throughput on a synced mainnet datadir with txindex + addressindex",
          metrics=METRICS, defaults={"reps": 300, "clients": [16, 64], "duration": 5.0, "dbcache": 4096},
          required=required("main"), accepts=("source_datadir",))
def rpc_mainnet(ctx):
    a = ctx.args
    src = a.source_datadir or paths.data_dir("indexed-main")
    ctx.result.doc["params"]["source_datadir"] = src
    with common.cloned(ctx, src, what="a mainnet datadir with txindex+addressindex (build it: `run "
                                      "reindex-mainnet --addressindex 1 --keep-datadir <path>`)") as datadir:
        cfg = NodeConfig(chain="main", datadir=datadir, connect=["0"], txindex=True, addressindex=True,
                         dbcache_mib=a.dbcache)
        with Node(ctx.impl, cfg, cpu_slice=ctx.node_slice) as node:
            node.wait_ready(3600)
            common.verify_effective(ctx, node)
            tip = node.rpc.getblockcount()
            rng = random.Random(42)
            # Half uniform over history, half from the last 10k blocks (what clients mostly ask for).
            heights = sorted(rng.sample(range(1, tip), 100) + rng.sample(range(tip - 10000, tip), 100))
            workload = build_workload(node.rpc, heights, rng, addressindex=True)
            ctx.result.info("chain_height", tip, "blocks")
            measure(ctx, node, workload, a.reps, a.clients, a.duration)
