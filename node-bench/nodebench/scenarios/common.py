"""Building blocks shared by the scenarios: seeds, datadir clones, progress tracking, footprint."""

import contextlib
import glob
import json
import os
import shutil
import subprocess
import time

from .. import logparse, paths
from .. import proc as procmod
from ..impls import CHAIN_LABEL, CLONE_EXCLUDE, INDEX_NAMES, Node, NodeConfig, clone_datadir, config_problems, make_impl
from ..rpc import RpcError

FOOTPRINT = ("cpu_s", "peak_rss_mib", "disk_total_gib")


def footprint_required(params):
    """Gated footprint metrics of a sync run (IBD, reindex): CPU, peak RSS, total and per-component disk."""
    parts = ["blocks", "chainstate", "evodb", "llmq"] + (
        ["indexes"] if params.get("txindex") or params.get("addressindex") else [])
    return set(FOOTPRINT) | {f"disk_{p}_gib" for p in parts}
SEED_FLAGS = ["-whitelist=download,noban@127.0.0.1", "-maxtipage=2000000000"]


def seed_datadir(chain):
    return paths.data_dir(f"seed-{chain}")


def _drop_page_cache():
    return subprocess.run(["sudo", "-n", "sh", "-c", "sync && echo 3 > /proc/sys/vm/drop_caches"],
                          stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0


def concurrent_pair(ctx):
    return bool(os.environ.get("NODE_BENCH_BARRIER")) and bool(getattr(ctx.args, "pair", None))


def drop_caches(ctx):
    """Cold page cache, only with --drop-caches, right before a cold measurement (after all preparation).

    It is host-wide (it also slows every other process on a shared box). In a concurrent pair both sides meet
    at a barrier, side A drops once and side B waits for it, so the drop sits at the common measurement
    boundary; a second drop in the same run would hit the other side mid-measurement and is refused. A drop
    that fails raises: a "cold" result must never be silently warm. Each drop is recorded.
    """
    params = ctx.result.doc["params"]
    drops = params.setdefault("page_cache_drops", [])
    if not getattr(ctx.args, "drop_caches", False):
        params["page_cache_dropped"] = False
        return False
    if concurrent_pair(ctx):
        if drops:
            raise RuntimeError("a concurrent pair drops the page cache once, at the measurement start; repeated "
                               "cold measurements need --mode interleave")
        d, side = os.environ["NODE_BENCH_BARRIER"], ctx.args.pair.rsplit(":", 1)[1]
        wait_barrier(ctx, "drop", 4 * 3600)
        if side == "A":
            ok = _drop_page_cache()
            open(os.path.join(d, f"DROP.{'ok' if ok else 'failed'}"), "w").close()
        else:
            while not glob.glob(os.path.join(d, "DROP.*")):
                if glob.glob(os.path.join(d, "ABORT.*")):
                    raise RuntimeError("barrier drop: the other side failed")
                time.sleep(0.05)
            ok = os.path.exists(os.path.join(d, "DROP.ok"))
    else:
        ok = _drop_page_cache()
    drops.append({"t": time.time(), "ok": ok})
    if not ok:
        raise RuntimeError("--drop-caches: dropping the page cache failed (needs passwordless sudo)")
    params["page_cache_dropped"] = True
    return True


def per_start_cache_drop_possible(ctx):
    """False inside a concurrent pair: a per-start drop would also hit the other side mid-measurement."""
    return not concurrent_pair(ctx)


def verify_effective(ctx, node):
    """Check that the node under test applied the requested configuration (impls.config_problems) and record it
    as the result's `effective_config`; `pair` requires it to be identical on both sides. Checked on the first
    start of each configuration in a run; a later start must report the same."""
    eff = ctx.impl.effective_config(node)
    problems = config_problems(node.cfg, eff)
    first = ctx.result.doc.setdefault("effective_config", eff)
    if first != eff:
        problems.append(f"effective configuration changed between starts: {first} vs {eff}")
    if problems:
        raise RuntimeError("node did not apply the requested configuration: " + "; ".join(problems))
    return eff


def indexes_synced(rpc, names):
    """True once every requested index (NodeConfig names) reports synced=true in getindexinfo. Indexes a node
    builds inside block connection and does not list (dashd v23: address/spent/timestamp) are complete with
    the tip; txindex and the filter index must be listed."""
    if not names:
        return True
    try:
        info = {INDEX_NAMES.get(k, k): v for k, v in rpc.getindexinfo().items()}
    except RpcError as e:
        if e.code == -32601:
            raise RuntimeError("indexes are enabled but the node has no getindexinfo: cannot tell when they "
                               "are built, so the time would be wrong") from None
        raise
    for n in names:
        if n in info:
            if info[n].get("synced") is not True:
                return False
        elif n in ("txindex", "blockfilterindex"):
            raise RuntimeError(f"{n} is enabled but getindexinfo does not report it")
    return True


def barrier(ctx, name, timeout=4 * 3600):
    """A measurement boundary: concurrent pairs wait here until the other side reaches the same point, then the
    load monitor marks it ("start" = the measured work begins; noise is classified from there on).

    A side that fails writes ABORT.<side> (see abort_barrier); the other side then stops
    waiting at once instead of holding its node and seed for the whole timeout.
    """
    wait_barrier(ctx, name, timeout)
    if getattr(ctx, "mark", None):
        ctx.mark(name)


def wait_barrier(ctx, name, timeout):
    d, n = os.environ.get("NODE_BENCH_BARRIER"), int(os.environ.get("NODE_BENCH_BARRIER_N", "0"))
    if not d or n < 2 or not getattr(ctx.args, "pair", None):
        return
    side = ctx.args.pair.rsplit(":", 1)[1]
    open(os.path.join(d, f"{name}.{side}"), "w").close()
    deadline = time.monotonic() + timeout
    while len(glob.glob(os.path.join(d, f"{name}.*"))) < n:
        aborted = glob.glob(os.path.join(d, "ABORT.*"))
        if aborted:
            raise RuntimeError(f"barrier {name}: the other side failed ({os.path.basename(aborted[0])})")
        if time.monotonic() > deadline:
            raise TimeoutError(f"barrier {name}: the other side never arrived")
        time.sleep(0.05)


def abort_barrier(side):
    d = os.environ.get("NODE_BENCH_BARRIER")
    if d and os.path.isdir(d):
        open(os.path.join(d, f"ABORT.{side}"), "w").close()


@contextlib.contextmanager
def cloned(ctx, src, name="datadir", what="a synced datadir"):
    """A reflink clone of `src` in the run directory, removed afterwards."""
    if not os.path.isdir(src):
        raise SystemExit(f"need {what} at {src}")
    dst = os.path.join(ctx.run_dir, name)
    ctx.log(f"cloning {src} -> {dst} (reflink)")
    # The seed masters carry a block filter index for spv-headers (ensure_filter_index); a node under test that
    # does not use it must not carry (and be charged on disk for) it.
    clone_datadir(src, dst, exclude=CLONE_EXCLUDE + ("blockfilter",))
    try:
        yield dst
    finally:
        if ctx.result.doc["artifacts"].get("datadir") != dst:  # kept in place by keep_or_remove
            paths.safe_rmtree(dst, "clone")


@contextlib.contextmanager
def seed(ctx, chain, src=None, *, copy=None, dbcache=1024, extra=(), name="seed", **cfg):
    """The pinned dashd serving a frozen chain on localhost from the aux slice.

    Always runs on a copy (reflink clone by default), so the master is never
    written and concurrent pairs can each have one.
    """
    src = src or seed_datadir(chain)
    if not os.path.isdir(src):
        raise SystemExit(f"no seed chain at {src}: run `run ibd-{CHAIN_LABEL[chain]}-net "
                         f"--keep-datadir {src}` first")
    dst = os.path.join(ctx.run_dir, f"{name}-datadir")
    (copy or clone_datadir)(src, dst)
    scfg = NodeConfig(chain=chain, datadir=dst, listen=True, connect=["0"], txindex=True, dbcache_mib=dbcache,
                      extra=SEED_FLAGS + list(extra), **cfg)
    node = Node(make_impl("dashd", ctx.args), scfg, cpu_slice=ctx.aux_slice,
                stdout_path=os.path.join(ctx.run_dir, f"{name}-stdout.log"))
    try:
        node.start()
        node.wait_ready(3600)
        yield node
    finally:
        node.stop()
        paths.safe_rmtree(dst, "seed clone")


@contextlib.contextmanager
def seeds(ctx, chain, n, **kw):
    """`n` seeds serving the same frozen chain (each its own reflink clone).

    One localhost peer is latency-bound: dashd keeps at most 16 blocks in flight per peer, so a single
    seed caps IBD far below what the node could do. Several seeds give the parallelism a real network
    has, identically for both implementations.
    """
    with contextlib.ExitStack() as stack:
        nodes = [stack.enter_context(seed(ctx, chain, name=f"seed{i}", **kw)) for i in range(n)]
        heights = {s.rpc.getbestblockhash() for s in nodes}
        if len(heights) != 1:
            raise RuntimeError(f"seeds disagree on the tip: {heights}")
        yield nodes


def call_retry(fn, tries=5):
    """RPC calls in long poll loops: a node busy flushing can time out once; that is not a failure."""
    for i in range(tries):
        try:
            return fn()
        except (RpcError, OSError, TimeoutError):
            if i == tries - 1:
                raise
            time.sleep(2 * (i + 1))


def track(ctx, node, is_done, on_row=None, label="sync", exit_ok=False):
    """Sample the node until is_done(row, rpc) is true; returns (seconds since spawn, samples, sample_path).

    exit_ok: the node stops itself at the end of the work (-stopatheight); a clean exit (code 0) ends tracking
    and the returned time is its exit time.

    The sampler thread and this loop each have their own RPC connection (Rpc is
    not thread-safe). Rows have the sampler's columns plus whatever probe adds.
    """
    a = ctx.args
    probe_rpc = node.new_rpc(timeout=120)
    loop_rpc = node.new_rpc(timeout=300)
    path = os.path.join(ctx.run_dir, "samples.csv")
    writer = procmod.CsvSink(path)

    def probe():
        bci = probe_rpc.getblockchaininfo()
        return {"blocks": bci["blocks"], "headers": bci["headers"], "ibd": int(bci["initialblockdownload"])}

    sampler = procmod.Sampler(node.pid, interval=a.interval, probe=probe, sink=writer.write, t0=node.t_spawn)
    sampler.start()
    last_log = 0
    try:
        while True:
            time.sleep(1)
            if node.proc.poll() is not None:
                if exit_ok and node.proc.returncode == 0:
                    node.wait_exit()
                    done = node.t_exit - node.t_spawn
                    break
                raise RuntimeError(f"node exited with {node.proc.returncode} during {label}")
            row = sampler.last
            if not row or "blocks" not in row:
                continue
            t = time.monotonic() - node.t_spawn
            if on_row:
                on_row(row, t)
            if t - last_log > 300:
                last_log = t
                ctx.log(f"t={t:.0f}s height={row['blocks']} headers={row['headers']} rss={row['rss_mib']:.0f}MiB "
                        f"load1={row['load1']}")
            if is_done(row, loop_rpc):
                done = time.monotonic() - node.t_spawn
                break
    finally:
        samples = sampler.stop()
        writer.close()
        probe_rpc.close()
    sampler.probe = sampler.sink = None  # the RPC and the CSV are closed by now
    last = sampler.sample()  # resource counters at completion, after the thread has stopped
    if last:
        samples.append(last)
    loop_rpc.close()
    return done, samples, path


def log_window(ctx, node):
    """(since, offset) bounding the per-block log to this node invocation (see logparse.iter_tips)."""
    return node.spawn_wall, (node.log_offset if ctx.impl.log_format() == "dashd" else 0)


def stopped_endpoint(ctx, cfg, node, target, reference=None):
    """A node that stopped itself (-stopatheight): (hash at `target`, last height, last hash) from this
    invocation's per-block log; raises unless it connected `target` (with hash `reference`, when known)."""
    path = block_log_path(ctx, cfg)
    if not path:
        raise RuntimeError("no per-block log: cannot verify where the node stopped")
    window = log_window(ctx, node)
    at = logparse.update_tip(path, ctx.impl.log_format(), target, *window)
    if at is None:
        raise RuntimeError(f"the node exited without connecting block {target}")
    if reference and at[1] != reference:
        raise RuntimeError(f"block {target} is {at[1]}, expected {reference}: not the same chain")
    _, height, best = logparse.last_tip(path, ctx.impl.log_format(), *window)
    return at[1], height, best


def precise_done(ctx, cfg, node, height, fallback):
    """Seconds from spawn until block `height` became the tip, from the node's own per-block log of this
    invocation (an entry from an earlier run, or one outside spawn..now, is ignored)."""
    path = block_log_path(ctx, cfg)
    if path and height is not None:
        ts = logparse.tip_time(path, ctx.impl.log_format(), height, *log_window(ctx, node))
        if ts is not None:
            return ts - node.spawn_wall, "block-log"
    return fallback, "poll"


def block_log_path(ctx, cfg):
    path = (os.path.join(cfg.datadir, "bench.jsonl") if ctx.impl.log_format() == "jsonl"
            else ctx.impl.log_path(cfg))
    return path if os.path.exists(path) else None


def record_resources(ctx, cfg, samples, blocks, gate_timing):
    """Footprint metrics common to IBD and reindex: CPU time, peak RSS, total and per-component disk are gated;
    steady RSS (not the PRD's settled tip RSS, see README "PRD coverage") and CPU per block are information."""
    r = ctx.result
    summ = procmod.summarize_samples(samples)
    cpu = summ["cpu_user_s"] + summ["cpu_sys_s"]
    r.metric("cpu_s", [cpu], "s", definition="user+sys CPU seconds of the node process for the whole run.",
             gate=gate_timing)
    r.metric("peak_rss_mib", [summ["peak_rss_mib"]], "MiB", target_2x=False,
             definition="Kernel VmHWM of the node process at the end of the run.")
    r.metric("steady_rss_mib", [summ["steady_rss_mib"]], "MiB", gate=False,
             definition="Median VmRSS over the last 50% of samples.")
    r.metric("cpu_ms_per_block", [1000 * cpu / blocks if blocks else None], "ms", gate=False,
             definition="cpu_s / blocks connected.")
    r.info("read_gib", summ["read_bytes"] / 2**30, "GiB")
    r.info("write_gib", summ["write_bytes"] / 2**30, "GiB")
    chain_dir = ctx.impl.chain_dir(cfg)
    for part, p in ctx.impl.disk_parts(cfg).items():
        if os.path.isdir(p):
            # Gated (never larger), no 2x target: components can trade against each other inside the total.
            r.metric(f"disk_{part}_gib", [procmod.dir_size_bytes(p) / 2**30], "GiB", target_2x=False,
                     definition="Allocated bytes of this datadir component at the end of the run.")
    # Everything in the chain directory except logs (debug.log, bench.jsonl, ...): implementation-neutral.
    logs = sum(procmod.dir_size_bytes(p) if os.path.isdir(p) else os.lstat(p).st_blocks * 512
               for p in glob.glob(os.path.join(chain_dir, "*.log")) + glob.glob(os.path.join(chain_dir, "*.jsonl")))
    full = procmod.dir_size_bytes(chain_dir) - logs
    if full <= 0:
        raise RuntimeError(f"empty chain directory {chain_dir}")
    r.metric("disk_total_gib", [full / 2**30], "GiB", target_2x=False,
             definition="Allocated bytes of the chain directory at the end of the run, excluding top-level *.log "
                        "and *.jsonl files (instrumentation).")


def record_block_log(ctx, cfg):
    """Per-block latency and script-check throughput from the node's own log (information, not gated:
    each implementation instruments itself)."""
    path = block_log_path(ctx, cfg)
    if not path:
        ctx.result.note("no per-block log")
        return
    logparse.record_block_metrics(ctx.result, path, ctx.impl.log_format(), min_height=ctx.args.block_stats_from)


def secondary(ctx, what, fn, *a, **kw):
    """Post-processing that must not turn a finished multi-hour run into an error."""
    try:
        return fn(*a, **kw)
    except Exception as e:
        ctx.result.note(f"{what} failed: {e!r}")
        ctx.log(f"{what} failed: {e!r}")
        return None


def datadir_meta(datadir):
    """{'height', 'tip', ...} of a kept datadir (bench-meta.json, else the IBD result that produced it)."""
    p = os.path.join(datadir, "bench-meta.json")
    if os.path.exists(p):
        with open(p) as f:
            meta = json.load(f)
        meta["height"] = int(meta["height"])
        return meta
    for res in sorted(glob.glob(os.path.join(paths.results_dir(), "*.json")), reverse=True):
        with open(res) as f:
            doc = json.load(f)
        if doc.get("artifacts", {}).get("datadir") == datadir and doc.get("status") == "ok":
            m = doc["metrics"]
            return {"height": int(m["height"]["value"]), "tip": m["tip"]["value"]}
    raise SystemExit(f"no bench-meta.json for {datadir}; write {{\"height\": H, \"tip\": hash}} there")


def keep_or_remove(ctx, datadir, meta):
    """--keep-datadir [PATH]: keep (or move) the datadir and stamp it with bench-meta.json."""
    keep = ctx.args.keep_datadir
    if not keep:
        paths.safe_rmtree(datadir, "datadir")
        return
    dst = datadir if keep is True else keep
    paths.writable(dst, "--keep-datadir")
    if dst != datadir:
        if os.path.exists(dst):
            ctx.result.note(f"--keep-datadir {dst} exists; datadir left at {datadir}")
            dst = datadir
        else:
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            shutil.move(datadir, dst)
    with open(os.path.join(dst, "bench-meta.json"), "w") as f:
        json.dump(meta, f)
    ctx.result.artifact("datadir", dst)
