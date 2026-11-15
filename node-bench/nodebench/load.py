"""RPC load generation: sequential latency sampling and multi-process throughput.

Workers are separate processes (no GIL contention) pinned to the aux CPU slice,
each with one keep-alive connection. Clocks are CLOCK_MONOTONIC, which is
system-wide on Linux, so start/end times from different workers are comparable.
"""

import multiprocessing as mp
import os
import queue
import time

from .proc import cpu_set, slice_cpus
from .rpc import Rpc, RpcError


def _worker(port, user, password, calls, start_at, deadline, cpus, q):
    lat, errors, first_err = [], 0, None
    t_first = t_last = time.monotonic()
    try:
        if cpus:
            os.sched_setaffinity(0, cpu_set(cpus))
        rpc = Rpc(port, user, password, timeout=600)
        rpc.call("getblockcount")  # connect before the start line
        while time.monotonic() < start_at:
            time.sleep(0.0005)
        t_first = time.monotonic()
        i, n = 0, len(calls)
        while (i < n) if deadline is None else (time.monotonic() < deadline):
            method, params = calls[i % n]
            t = time.monotonic()
            try:
                rpc.call(method, *params)
                lat.append(time.monotonic() - t)
            except RpcError as e:
                errors += 1
                first_err = first_err or str(e)[:300]
            i += 1
        t_last = time.monotonic()
    except Exception as e:  # always report, or the parent waits forever
        errors += 1
        first_err = first_err or f"worker crashed: {e!r}"[:300]
        t_last = time.monotonic()
    q.put({"lat": lat, "errors": errors, "first_error": first_err, "t_first": t_first, "t_last": t_last})


def run_parallel(node, calls, clients, cpu_list=None, duration=None):
    """Run `calls` split round-robin across `clients` processes.

    duration=None: every call once (e.g. distinct transactions).
    duration=s:    each worker loops over its share until the deadline.
    Returns ops/s, latencies (s) and error counts.
    """
    from .impls import RPC_PASSWORD, RPC_USER

    ctx = mp.get_context("spawn")
    q = ctx.Queue()
    if duration:
        # Every worker loops over the whole set, starting at a different offset.
        shares = [calls[i % len(calls):] + calls[:i % len(calls)] for i in range(clients)]
    else:
        shares = [calls[i::clients] for i in range(clients)]
    start_at = time.monotonic() + 2.0 + 0.05 * clients
    deadline = start_at + duration if duration else None
    procs = [ctx.Process(target=_worker, args=(node.cfg.rpcport, RPC_USER, RPC_PASSWORD, s, start_at, deadline,
                                               cpu_list, q)) for s in shares if s]
    budget = (duration or 0) + 3600
    outs = []
    try:
        for p in procs:
            p.start()
        for _ in procs:
            try:
                outs.append(q.get(timeout=budget))
            except queue.Empty:
                raise RuntimeError(f"load workers did not report within {budget:.0f}s "
                                   f"(exit codes {[p.exitcode for p in procs]})") from None
    finally:
        # Interrupted (SIGTERM -> Terminated) or failed: no worker outlives this call.
        incomplete = len(outs) < len(procs)
        for p in procs:
            if p.pid is None:
                continue
            if incomplete:
                p.kill()
            p.join(30)
            if p.is_alive():
                p.kill()
                p.join()
    lat = [x for o in outs for x in o["lat"]]
    t0 = min(o["t_first"] for o in outs)
    t1 = max(o["t_last"] for o in outs)
    return {
        "ok": len(lat), "errors": sum(o["errors"] for o in outs),
        "first_error": next((o["first_error"] for o in outs if o["first_error"]), None),
        "wall_s": t1 - t0, "ops_per_s": len(lat) / (t1 - t0) if t1 > t0 else None, "lat": lat,
    }


def run_sequential(rpc, calls):
    """One client, back-to-back calls; returns per-call latency in seconds."""
    lat = []
    for method, params in calls:
        t = time.monotonic()
        rpc.call(method, *params)
        lat.append(time.monotonic() - t)
    return lat


def aux_cpus(slice_spec):
    """taskset-style cpu list for an aux slice ('numa:1' -> '16-31')."""
    return slice_cpus(slice_spec)
