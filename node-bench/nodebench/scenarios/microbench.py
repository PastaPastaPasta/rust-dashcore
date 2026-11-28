"""Microbenchmarks: dashd's bench_dash, and Rust equivalents under the same names.

Both sides emit nanobench-style JSON ({"results": [{"name", "unit",
"median(elapsed)"}]}, seconds per unit). bench_dash does this natively with
-output-json; a Rust bench binary plugged in with --bench-bin must print the
same shape (--rust-bench-bin) for the benchmarks it implements, using bench_dash's names so the
comparison lines up (see README, "Microbenchmarks").
"""

import json
import os
import re
import subprocess

from .. import paths
from ..impls import finish_tool, sha256_file, spawn
from . import common, scenario

# Benchmarks that measure work a full node does on its hot paths.
CORE_FILTER = ("ECDSAVerify|ECDSAVerify_LargeBlock|ECDSASign|CCheckQueueSpeedPrevectorJob|DeserializeBlockTest|"
               "DeserializeAndCheckBlockTest|CCoinsCaching|VerifyNestedIfScript|MerkleRoot|"
               "BLS_Verify_Normal|BLS_Verify_Batched|BLS_Verify_LargeBlock1000|BLS_Verify_LargeAggregatedBlock1000|"
               "BLS_Sign_Normal|BLS_PubKeyAggregate_Normal|BLSDKG_.*_100|Pow_X11_0080b|Pow_X11_1M|"
               "DSHA256_0080b_single|DSHA256_1M|SHA256_STANDARD|SHA256D64_1024_STANDARD|SipHash_32b|"
               "AssembleBlock|ComplexMemPool|MempoolCheck|MempoolEviction|RpcMempool|BlockToJsonVerbose|"
               "LoadExternalBlockFile|GCSFilterConstruct|GCSFilterMatch|MuHash|CoinSelection|AddrManSelect")

# The exact benchmarks CORE_FILTER selects in bench_dash v23.1.8: the required inventory. A custom --filter
# cannot narrow it (compare.required_metrics always includes the default parameters' inventory); names outside
# this list are compared but not required.
NAMES = ("AddrManSelect", "AssembleBlock", "BLSDKG_BuildQuorumVerificationVectors_simple_100",
         "BLSDKG_GenerateContributions_simple_100", "BLSDKG_VerifyContributionShares_aggregated_100",
         "BLSDKG_VerifyContributionShares_simple_100", "BLS_PubKeyAggregate_Normal", "BLS_Sign_Normal",
         "BLS_Verify_Batched", "BLS_Verify_LargeAggregatedBlock1000", "BLS_Verify_LargeBlock1000", "BLS_Verify_Normal",
         "BlockToJsonVerbose", "CCheckQueueSpeedPrevectorJob", "CCoinsCaching", "CoinSelection", "ComplexMemPool",
         "DSHA256_0080b_single", "DSHA256_1M", "DeserializeAndCheckBlockTest", "DeserializeBlockTest", "ECDSASign",
         "ECDSAVerify_LargeBlock", "ECDSAVerify", "GCSFilterConstruct", "GCSFilterMatch", "LoadExternalBlockFile",
         "MempoolCheck", "MempoolEviction", "MerkleRoot", "MuHash", "Pow_X11_0080b", "Pow_X11_1M", "RpcMempool",
         "SHA256D64_1024_STANDARD", "SHA256_STANDARD", "SipHash_32b", "VerifyNestedIfScript")


def required(params):
    pattern = re.compile(f"^({params.get('filter') or CORE_FILTER})$")
    return {f"micro_{n}_ns" for n in NAMES if pattern.match(n)}


METRICS = {
    "micro_<name>_ns": "bench_dash (nanobench) median time per unit (op, byte, ...) in ns (median(elapsed) / "
                       "batch), `-min-time` per "
                       "benchmark, pinned to the node slice. The whole binary is run `reps` times as separate "
                       "processes; the sample per rep is nanobench's median; statistic: median over reps.",
}


def default_bench_bin():
    return paths.path("bin", f"bench_dash-v{paths.DASHD_VERSION}")


@scenario("microbench", "validation", "bench_dash microbenchmarks (script/sig, BLS, X11, block, mempool)",
          metrics=METRICS, defaults={"reps": 5, "filter": CORE_FILTER, "duration": 0.5}, required=required,
          accepts=("dashd_bench_bin", "rust_bench_bin"))
def microbench(ctx):
    a, r = ctx.args, ctx.result
    if a.impl == "rust":
        binary = a.rust_bench_bin
        if not binary:
            raise SystemExit("--impl rust microbench needs --rust-bench-bin: a binary printing nanobench-style "
                             "JSON with bench_dash names (none exists yet)")
    else:
        binary = a.dashd_bench_bin or default_bench_bin()
    if not os.path.exists(binary):
        raise SystemExit(f"{binary} missing (dashd: node-bench/scripts/build-dashd-bench.sh)")
    r.doc["params"]["bench_bin"] = {"path": binary, "sha256": sha256_file(binary)}
    per, units = {}, {}
    common.barrier(ctx, "start")
    for rep in range(a.reps):
        out = os.path.join(ctx.run_dir, f"bench-{rep}.json")
        cmd = [binary, f"-filter=^({a.filter})$", f"-min-time={int(a.duration * 1000)}", f"-output-json={out}"]
        p, cg = spawn(cmd, ctx.node_slice, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            p.wait(timeout=7200)
        finally:
            finish_tool(p, cg)
        if p.returncode:
            raise RuntimeError(f"{binary} exited with {p.returncode}")
        with open(out) as f:
            for res in json.load(f)["results"]:
                # median(elapsed) is per iteration; an iteration is `batch` units (bytes, txs, ...).
                name = res["name"].split(" ")[0]
                per.setdefault(name, []).append(res["median(elapsed)"] / (res.get("batch") or 1) * 1e9)
                units[name] = res.get("unit", "op")
        ctx.log(f"  rep {rep}: {len(per)} benchmarks")
    for name, xs in sorted(per.items()):
        r.metric(f"micro_{name}_ns", xs, f"ns/{units[name]}", definition=METRICS["micro_<name>_ns"])
