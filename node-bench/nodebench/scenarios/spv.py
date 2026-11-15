"""Header sync from a local seed: dashd vs the existing Rust piece, dash-spv.

Until the Rust full node exists, `--impl rust` here runs dash-spv-bench (the
dash-spv SPV client) against the same localhost seed, so the one sync stage
both implement — downloading and validating the header chain — is compared
like for like. dash-spv's later stages (filter headers, filters, masternode
lists) are recorded as Rust-only information.
"""

import fcntl
import functools
import os
import shutil
import subprocess
import time
from datetime import datetime, timezone

from .. import paths
from ..impls import CHAIN_LABEL, Node, NodeConfig, clone_datadir, finish_tool, make_impl, sha256_file, spawn
from ..proc import read_proc
from . import common, scenario

METRICS = {
    "header_sync_s": "Process spawn until the full header chain of the local seed is synced and validated "
                     "(dashd: getblockchaininfo.headers == seed height, polled every 0.1 s; dash-spv: spawn to "
                     "its first log line plus its BlockHeaderSyncComplete time). Empty storage, seed pinned to "
                     "the aux slice. Statistic: median over `reps`.",
    "headers_per_s": "seed height / header_sync_s (information).",
    "peak_rss_mib": "Information only: dashd is read when headers finish, dash-spv after its whole sync.",
    "spv_filter_headers_s / spv_filters_s / spv_total_s": "dash-spv only: later sync stages (information).",
}


def ensure_filter_index(ctx, chain):
    """Build the BIP157 filter index once (hours on mainnet) on a clone, then copy it into the master seed."""
    master = paths.writable(common.seed_datadir(chain), "seed (filter index)")
    marker = os.path.join(master, "bench-filterindex.done")
    with open(os.path.join(master, ".bench-filterindex.lock"), "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if os.path.exists(marker):
            return
        ctx.log(f"building the block filter index for {master} (one-time)")
        work = os.path.join(ctx.run_dir, "filterindex-datadir")
        clone_datadir(master, work)
        cfg = NodeConfig(chain=chain, datadir=work, connect=["0"], txindex=True, blockfilterindex=True,
                         dbcache_mib=4096)
        dashd = make_impl("dashd", ctx.args)
        with Node(dashd, cfg, cpu_slice=ctx.aux_slice,
                  stdout_path=os.path.join(ctx.run_dir, "filterindex-stdout.log")) as node:
            node.wait_ready(3600)
            while not node.rpc.getindexinfo().get("basic block filter index", {}).get("synced"):
                time.sleep(30)
        rel = os.path.relpath(os.path.join(dashd.chain_dir(cfg), "indexes", "blockfilter"), work)
        shutil.copytree(os.path.join(work, rel), os.path.join(master, rel), dirs_exist_ok=True)
        paths.safe_rmtree(work, "filter index work datadir")
        open(marker, "w").close()


def dashd_header_sync(ctx, chain, seed_addr, target):
    cfg = NodeConfig(chain=chain, datadir=os.path.join(ctx.run_dir, "dashd-datadir"), connect=seed_addr.split(","),
                     txindex=False, dbcache_mib=ctx.args.dbcache)
    try:
        with Node(ctx.impl, cfg, cpu_slice=ctx.node_slice) as node:
            node.wait_ready()
            while node.rpc.getblockchaininfo()["headers"] < target:
                time.sleep(0.1)
            t = time.monotonic() - node.t_spawn
            rss = read_proc(node.pid)["hwm_mib"]
    finally:
        paths.safe_rmtree(cfg.datadir, "datadir")
    return {"header_sync_s": t, "peak_rss_mib": rss}


def spv_header_sync(ctx, chain, seed_addr, binary):
    storage = os.path.join(ctx.run_dir, "spv-storage")
    env = dict(os.environ, BENCH_MODE=CHAIN_LABEL[chain], BENCH_PEERS=seed_addr, BENCH_STORAGE_DIR=storage,
               BENCH_WALLET_FILE="")
    try:
        with open(os.path.join(ctx.run_dir, "spv-stdout.log"), "ab") as out:
            spawn_wall = time.time()
            p, cg = spawn([binary], ctx.node_slice, env=env, stdout=out, stderr=subprocess.STDOUT,
                               stdin=subprocess.DEVNULL)
            try:
                p.wait(timeout=3600)
            finally:
                finish_tool(p, cg)
        summary = {}
        with open(os.path.join(storage, "summary.txt")) as f:
            for line in f:
                k, _, v = line.partition(":")
                summary[k.strip()] = v.strip()
        first_log = first_log_time(os.path.join(storage, "run.log"))
    finally:
        paths.safe_rmtree(storage, "dash-spv storage")

    def secs(k):
        return int(summary[k]) / 1000 if summary.get(k, "").isdigit() else None

    if secs("block_headers_ms") is None:
        raise RuntimeError(f"dash-spv did not finish header sync: {summary}")
    # dash-spv's clock starts after its own setup; add the time from spawn to its first log line.
    # (Setup after that line — storage, wallets — is still uncounted; it is milliseconds without wallets.)
    if first_log is None:
        raise RuntimeError("no timestamp in dash-spv's run.log: cannot time it from spawn")
    offset = first_log - spawn_wall
    return {"header_sync_s": offset + secs("block_headers_ms"), "startup_offset_s": offset,
            "peak_rss_mib": float(summary.get("peak_rss_mib") or 0) or None,
            "spv_filter_headers_s": secs("filter_headers_ms"), "spv_filters_s": secs("filters_ms"),
            "spv_total_s": secs("total_ms")}


def first_log_time(path):
    """Timestamp of the first tracing line (RFC 3339, UTC) in dash-spv's run.log."""
    try:
        with open(path) as f:
            for line in f:
                tok = line.split(" ", 1)[0].rstrip("Z")
                try:
                    return datetime.fromisoformat(tok).replace(tzinfo=timezone.utc).timestamp()
                except ValueError:
                    continue
    except OSError:
        pass
    return None


def run_spv(ctx, chain):
    a, r = ctx.args, ctx.result
    meta = common.datadir_meta(common.seed_datadir(chain))
    binary = None
    if a.impl == "rust":
        binary = a.rust_bench_bin or os.path.join(paths.REPO_ROOT, "target", "release", "dash-spv-bench")
        if not os.path.exists(binary):
            raise SystemExit(f"{binary} missing: cargo build --release -p dash-spv-bench")
        rev = subprocess.run(["git", "-C", paths.REPO_ROOT, "rev-parse", "HEAD"], capture_output=True,
                             text=True).stdout.strip()
        r.doc["impl"] = {"impl": "rust", "daemon": binary, "sha256": sha256_file(binary),
                         "version": f"dash-spv-bench @ {rev}"}
    ensure_filter_index(ctx, chain)
    runs = []
    with common.seeds(ctx, chain, a.seeds, blockfilterindex=True) as seeds:
        seed_addr = ",".join(f"127.0.0.1:{x.cfg.p2pport}" for x in seeds)
        target = seeds[0].rpc.getblockcount()
        deadline = time.monotonic() + 600
        while not all(x.rpc.getindexinfo().get("basic block filter index", {}).get("synced") for x in seeds):
            if time.monotonic() > deadline:  # the index is already built; this only waits for it to load
                raise TimeoutError("seed's block filter index did not load")
            time.sleep(1)
        r.doc["params"]["seed"] = {"height": target, "tip": seeds[0].rpc.getbestblockhash(), "count": len(seeds),
                                   **meta}
        common.barrier(ctx, "start")
        for i in range(a.reps):
            common.drop_caches(ctx)
            out = (spv_header_sync(ctx, chain, seed_addr, binary) if binary
                   else dashd_header_sync(ctx, chain, seed_addr, target))
            ctx.log(f"  rep {i}: {out}")
            runs.append(out)
    r.metric("header_sync_s", [x["header_sync_s"] for x in runs], "s", definition=METRICS["header_sync_s"])
    r.metric("headers_per_s", [target / x["header_sync_s"] for x in runs], "headers/s", better="higher",
             gate=False, definition=METRICS["headers_per_s"])
    r.metric("peak_rss_mib", [x["peak_rss_mib"] for x in runs], "MiB", gate=False,
             definition=METRICS["peak_rss_mib"])
    for k in ("spv_filter_headers_s", "spv_filters_s", "spv_total_s", "startup_offset_s"):
        if any(x.get(k) for x in runs):
            r.metric(k, [x.get(k) for x in runs], "s", gate=False,
                     definition=METRICS["spv_filter_headers_s / spv_filters_s / spv_total_s"])


for _chain in ("main", "test"):
    scenario(f"spv-headers-{CHAIN_LABEL[_chain]}", "sync",
             f"Header-chain sync from a local {CHAIN_LABEL[_chain]} seed: dashd vs dash-spv (--impl rust)",
             metrics=METRICS, defaults={"reps": 5, "dbcache": 1024, "seeds": 4}, required={"header_sync_s"},
             accepts=("rust_bench_bin", "drop_caches"),
             )(functools.partial(run_spv, chain=_chain))
