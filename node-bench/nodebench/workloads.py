"""Fixed-input workloads for counter runs (task PF0).

    node-bench/run workload <name> [--impl dashd|rust] [--wrap "perf stat -x, -o out.csv --"] [--json m.json]
                                   [--ctl-fifo CTL[,ACK]]

Each workload prepares its inputs without the wrapper (cached per binary, then
reflink cloned per run), then runs the node under test *inside* `--wrap` for
one fixed piece of work, and returns when the wrapper has exited (so it has
written its counters). Inputs are byte-identical across runs and
implementations: the regtest fixture's chain and pre-signed transactions,
deterministic RPC parameter sets, and frozen testnet blocks.

Measurement boundaries. Every workload emits markers (markers.jsonl in the run
directory and `markers` in the manifest; CLOCK_MONOTONIC and wall time):
spawn, ready, begin/end around each piece of fixed work, stop, exit. With
--ctl-fifo the harness also writes `enable` at each begin and `disable` at each
end to a `perf stat --control fifo:CTL[,ACK] -D -1` control FIFO, so the
counters cover only the fixed work, not startup, setup or shutdown. The
whole-lifetime workloads (startup, reindex-chainstate-testnet,
ibd-testnet-seed) enable at spawn and disable at exit.

Failure is loud: a nonzero exit of the wrapper or the node, a timeout, or a
workload that did not reach its exact endpoint (target height and hash) makes
the command exit nonzero with status "error" in the manifest. The counter
methodology itself (startup subtraction, noise floors, previous-head
comparison) belongs to PF0; see README "Counter workloads (PF0)" for the known
limits of these workloads.
"""

import contextlib
import errno
import fcntl
import glob
import hashlib
import json
import os
import random
import shlex
import time
from dataclasses import dataclass

from . import fixture, logparse, paths, pf0, results
from .impls import Node, NodeConfig, clone_datadir, sha256_file
from .proc import cpu_set, slice_cpus
from .scenarios import common
from .scenarios.rpc import CALL_METHODS, build_workload

DEFAULTS = {"txs": 2000, "calls": 200, "height": 300000, "gbt_mempool": 5000, "gbt_calls": 3, "par": 1,
            "timeout": 6 * 3600}
KEEP_CACHES = 3  # prepared datadirs kept per implementation (newest first)


class Markers:
    """Measurement-boundary events, mirrored to an optional perf control FIFO."""

    def __init__(self, run_dir, ctl=None):
        self.events = []
        self.path = os.path.join(run_dir, "markers.jsonl")
        self.ctl = self.ack = None
        self.ctl_path, _, self.ack_path = (ctl or "").partition(",")

    def mark(self, event, **kw):
        e = {"event": event, "mono": time.monotonic(), "wall": time.time(), **kw}
        self.events.append(e)
        with open(self.path, "a") as f:
            f.write(json.dumps(e) + "\n")
        command = {"begin": b"enable\n", "end": b"disable\n"}.get(event)
        if command and self.ctl_path:
            self._send(command)

    def _send(self, command):
        if self.ctl is None:
            # The FIFO has a reader only once the wrapper (perf) runs: open lazily, and never block forever.
            deadline = time.monotonic() + 60
            while self.ctl is None:
                try:
                    self.ctl = os.open(self.ctl_path, os.O_WRONLY | os.O_NONBLOCK)
                except OSError as e:
                    if e.errno != errno.ENXIO or time.monotonic() > deadline:
                        raise RuntimeError(f"perf control FIFO {self.ctl_path}: no reader ({e})") from None
                    time.sleep(0.01)
            self.ack = open(self.ack_path, "rb") if self.ack_path else None
        try:
            os.write(self.ctl, command)
        except BrokenPipeError:
            if command == b"disable\n":
                return  # the wrapper already exited (whole-lifetime workloads): its counters stopped with it
            raise
        if self.ack is not None and self.ack.readline().strip(b"\0\n") != b"ack":
            raise RuntimeError("perf control FIFO did not acknowledge " + command.decode().strip())

    @contextlib.contextmanager
    def work(self, **kw):
        self.mark("begin", **kw)
        try:
            yield
        finally:
            self.mark("end", **kw)

    def close(self):
        if self.ctl is not None:
            os.close(self.ctl)
        if self.ack is not None:
            self.ack.close()


def prepared_regtest(ctx, addressindex=False):
    """Fixture datadir synced by this implementation, cached per (impl binary, fixture, indexes).

    Built under a lock into a temporary directory and published by rename, so concurrent first runs never see
    (or delete) a half-built cache; older caches of the same implementation are pruned.
    """
    fixture.ensure(ctx)
    key = f"{ctx.impl.name}-{(sha256_file(ctx.impl.daemon) or 'x')[:12]}-{fixture.FIXTURE}" + (
        "-addr" if addressindex else "")
    d = paths.writable(paths.data_dir("workloads", key), "workload cache")
    os.makedirs(os.path.dirname(d), exist_ok=True)
    with open(d + ".lock", "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if not os.path.exists(os.path.join(d, ".ready")):
            tmp = f"{d}.building-{os.getpid()}"
            paths.safe_rmtree(tmp, "workload cache")
            paths.safe_rmtree(d, "workload cache")
            fixture.sync_from_seed(ctx, cfg_overrides={"addressindex": addressindex}, datadir=tmp)
            open(os.path.join(tmp, ".ready"), "w").close()
            os.rename(tmp, d)
        os.utime(d)
    prune_caches(ctx.impl.name, keep={d})
    run = os.path.join(ctx.run_dir, "datadir")
    clone_datadir(d, run)
    return run


def prune_caches(impl_name, keep):
    caches = sorted((p for p in glob.glob(paths.data_dir("workloads", f"{impl_name}-*"))
                     if os.path.exists(os.path.join(p, ".ready"))), key=os.path.getmtime, reverse=True)
    for p in caches[KEEP_CACHES:]:
        if p not in keep:
            with open(p + ".lock", "w") as lock:
                try:
                    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)  # in use by a concurrent run: leave it
                except BlockingIOError:
                    continue
                paths.safe_rmtree(p, "workload cache")


def file_digest(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def fixture_inputs(**kw):
    meta = fixture.load_meta()
    return {"fixture": fixture.FIXTURE, "fixture_tip": meta["tip"],
            "txs_sha256": file_digest(os.path.join(fixture.fixture_dir(), "txs.jsonl")), **kw}


def fixture_config(a, datadir, addressindex=False):
    """The fixture rows' node config: one builder for the measured run and its PF0 preflight (§7: the exact
    measured argv; PF0 pins the ports, D31)."""
    ports = {"rpcport": pf0.NODE_RPC, "p2pport": pf0.NODE_P2P} if pf0.on() else {}
    return fixture.regtest_config(datadir, connect=["0"], addressindex=addressindex, par=a.par, **ports)


def regtest_run(ctx, a, work, addressindex=False, lifetime=False):
    """Start the wrapped node on its prepared fixture datadir, run `work(node)` (which marks its own boundaries,
    unless `lifetime`: then the boundary is spawn..exit), stop it, and require a clean exit of the wrapper and
    the node."""
    cfg = fixture_config(a, prepared_regtest(ctx, addressindex), addressindex)
    node = Node(ctx.impl, cfg, cpu_slice=ctx.node_slice, wrap=ctx.wrap,
                stdout_path=os.path.join(ctx.run_dir, "stdout.log"))
    record_node(ctx, node)
    ctx.markers.mark("spawn")
    node.start()
    try:
        if lifetime:
            ctx.markers.mark("begin")
        node.wait_ready(600)
        ctx.markers.mark("ready")
        ctx.manifest["effective_config"] = ctx.impl.effective_config(node)
        out = work(node)
        ctx.markers.mark("stop")
    except BaseException:
        node.kill()
        raise
    code = node.stop()
    if lifetime:
        ctx.markers.mark("end")
    ctx.markers.mark("exit", code=code)
    ctx.manifest["exit_code"] = code
    if code != 0:
        raise RuntimeError(f"the wrapper/node exited with {code} (counters may be missing or partial)")
    return out


def record_node(ctx, node):
    ctx.manifest["argv"] = node.argv()
    ctx.manifest["node_config"] = dict(node.cfg.__dict__)


def w_startup(ctx, a):
    def work(node):
        return None  # the fixed work is the start itself (spawn..ready) and the stop (stop..exit)

    regtest_run(ctx, a, work, lifetime=True)  # with --ctl-fifo the counters run from spawn to exit
    return {"ops": 1, "unit": "start+stop", "inputs": fixture_inputs()}


GENESIS_MOCKTIME = 1700000000  # at or after regtest genesis (a negative -mocktime is an InitError)


def genesis_config(a, datadir=pf0.NODE_DATADIR):
    """ND1's startup recipe (§12): regtest from genesis on an empty datadir, no network and no RPC; the
    `-startupnotify` command touches <datadir>/ready, the readiness signal. `-par=16` starts 15 script-check
    threads on any host, the most a node starts (CH3a a9, DEC-450 (5))."""
    return NodeConfig(chain="regtest", datadir=datadir, rpcport=0, p2pport=0, bare=True,
                      extra=["-listen=0", "-dnsseed=0", "-connect=0", "-fixedseeds=0", "-printtoconsole=0",
                             "-par=16", f"-mocktime={GENESIS_MOCKTIME}", f"-startupnotify=touch {datadir}/ready"])


def wait_ready_file(node, path, timeout):
    """Seconds from spawn until `path` exists (lstat: a PR-written datadir is never followed or opened)."""
    end = time.monotonic() + timeout
    while True:
        if node.proc.poll() is not None:
            raise RuntimeError(f"node exited with code {node.proc.returncode} before {os.path.basename(path)}")
        try:
            os.lstat(path)
            return time.monotonic() - node.t_spawn
        except FileNotFoundError:
            pass
        if time.monotonic() > end:
            raise TimeoutError(f"no {path} after {timeout}s")
        time.sleep(0.005)


def w_startup_genesis(ctx, a):
    """Start from genesis on an empty datadir, ready (the startupnotify file), SIGTERM, exit 0: the whole process
    lifetime. Nothing of the node's datadir is read but the ready file's existence."""
    cfg = genesis_config(a, pf0.NODE_DATADIR if pf0.on() else os.path.join(ctx.run_dir, "datadir"))
    if pf0.on():  # perfgate checks the run's argv is the one the preflight accepted (§7)
        ctx.manifest["argv_sha256"] = pf0.argv_sha256(ctx.impl.argv(cfg))
    node = Node(ctx.impl, cfg, cpu_slice=ctx.node_slice, wrap=ctx.wrap,
                stdout_path=os.path.join(ctx.run_dir, "stdout.log"))
    record_node(ctx, node)
    ctx.markers.mark("spawn")
    node.start()
    ctx.markers.mark("begin")
    try:
        ctx.manifest["ready_s"] = round(wait_ready_file(node, os.path.join(cfg.datadir, "ready"), 600), 3)
        ctx.markers.mark("ready")
        ctx.markers.mark("stop")
    except BaseException:
        node.kill()
        raise
    code = node.stop(timeout=120, rpc=False)
    ctx.markers.mark("end")
    ctx.markers.mark("exit", code=code)
    ctx.manifest["exit_code"] = code
    if code != 0:
        raise RuntimeError(f"the node exited with {code} after SIGTERM")
    return {"ops": 1, "unit": "start+stop", "inputs": {"chain": "regtest", "datadir": "empty (genesis)",
                                                        "mocktime": GENESIS_MOCKTIME}}


def w_mempool_accept(ctx, a):
    def work(node):
        txs = fixture.load_txs()[:a.txs]
        with ctx.markers.work():
            for t in txs:
                node.rpc.sendrawtransaction(t["hex"])
        if node.rpc.getmempoolinfo()["size"] != len(txs):
            raise RuntimeError("not every fixture transaction was accepted")
        return len(txs)

    n = regtest_run(ctx, a, work)
    return {"ops": n, "unit": "tx", "inputs": fixture_inputs(txs=f"first {n}")}


def w_gbt(ctx, a):
    def work(node):
        txs = fixture.load_txs()
        fill, extra = txs[:a.gbt_mempool], txs[-a.gbt_calls:]
        node.rpc.batch([("sendrawtransaction", [t["hex"]]) for t in fill])  # setup, outside the boundary
        for i, t in enumerate(extra):  # one new tx + > 5 s idle: every template is built from scratch
            node.rpc.sendrawtransaction(t["hex"])
            time.sleep(5.2)
            with ctx.markers.work(call=i):
                node.rpc.getblocktemplate()
        return len(fill) + len(extra)

    size = regtest_run(ctx, a, work)
    return {"ops": a.gbt_calls, "unit": "getblocktemplate",
            "inputs": fixture_inputs(mempool_txs=size, fill=f"first {a.gbt_mempool}", fresh=f"last {a.gbt_calls}")}


def w_rpc(ctx, a):
    def work(node):
        meta = fixture.load_meta()
        workload = build_workload(node.rpc, list(range(1, meta["height"] + 1)), random.Random(42), True)
        if a.call not in workload:
            raise SystemExit(f"unknown --call {a.call!r}; one of {', '.join(workload)}")
        calls = [workload[a.call][i % len(workload[a.call])] for i in range(a.calls)]
        digest = hashlib.sha256(json.dumps(calls).encode()).hexdigest()
        with ctx.markers.work():
            for m, p in calls:
                node.rpc.call(m, *p)
        return digest

    digest = regtest_run(ctx, a, work, addressindex=True)
    return {"ops": a.calls, "unit": f"rpc {a.call}",
            "inputs": fixture_inputs(param_seed=42, call=a.call, calls_sha256=digest)}


def stop_at(ctx, cfg, height, timeout, reference=None):
    """Run the wrapped node until it stops itself at -stopatheight; verify it connected exactly block `height`
    (hash = `reference` when known) in this invocation. Counters run from spawn to exit."""
    cfg.extra = list(cfg.extra) + [f"-stopatheight={height}"]
    node = Node(ctx.impl, cfg, cpu_slice=ctx.node_slice, wrap=ctx.wrap,
                stdout_path=os.path.join(ctx.run_dir, "stdout.log"))
    record_node(ctx, node)
    ctx.markers.mark("spawn")
    node.start()
    ctx.markers.mark("begin")
    try:
        if not node.wait_exit(timeout):
            raise TimeoutError(f"the node did not stop at height {height} within {timeout:.0f}s")
    finally:
        ctx.markers.mark("end")
        node.kill()
    code = node.proc.returncode
    ctx.markers.mark("exit", code=code)
    ctx.manifest["exit_code"] = code
    if code != 0:
        raise RuntimeError(f"the wrapper/node exited with {code} (counters may be missing or partial)")
    block, last_height, _ = common.stopped_endpoint(ctx, cfg, node, height, reference)
    ctx.manifest["endpoint"] = {"height": height, "hash": block, "reference": reference,
                                "overshoot_blocks": last_height - height}
    if os.path.exists(ctx.impl.log_path(cfg)):
        ctx.manifest["effective_config"] = logparse.init_config(ctx.impl.log_path(cfg), node.log_offset)
    return code


def master_hash(chain, height):
    """Hash of block `height` from the seed master's own debug.log (its original IBD), if still there."""
    log = os.path.join(common.seed_datadir(chain), "testnet3" if chain == "test" else "", "debug.log")
    got = logparse.update_tip(log, "dashd", height) if os.path.exists(log) else None
    return got[1] if got else None


def w_reindex_chainstate_testnet(ctx, a):
    src = common.seed_datadir("test")
    reference = master_hash("test", a.height)
    if reference is None:
        ctx.manifest.setdefault("notes", []).append("reference hash unavailable: endpoint checked by height only")
    with common.cloned(ctx, src) as datadir:
        cfg = NodeConfig(chain="test", datadir=datadir, connect=["0"], txindex=False, dbcache_mib=a.dbcache or 1024,
                         par=a.par, reindex_chainstate=True)
        stop_at(ctx, cfg, a.height, a.timeout, reference)
    return {"ops": a.height, "unit": "block",
            "inputs": {"chain": "testnet", "blocks": f"0..{a.height}", "source": common.datadir_meta(src)}}


def w_ibd_testnet_seed(ctx, a):
    seeds = a.seeds or 4
    with common.seeds(ctx, "test", seeds) as nodes:
        reference = nodes[0].rpc.getblockhash(a.height)
        cfg = NodeConfig(chain="test", datadir=os.path.join(ctx.run_dir, "datadir"), txindex=False,
                         dbcache_mib=a.dbcache or 1024, par=a.par,
                         connect=[f"127.0.0.1:{x.cfg.p2pport}" for x in nodes])
        stop_at(ctx, cfg, a.height, a.timeout, reference)
    return {"ops": a.height, "unit": "block",
            "inputs": {"chain": "testnet", "blocks": f"0..{a.height}", "seeds": seeds,
                       "source": common.datadir_meta(common.seed_datadir("test"))}}


WORKLOADS = {
    "startup": (w_startup, "start on the fixture chain, RPC ready, stop (the whole process lifetime)"),
    "startup-genesis": (w_startup_genesis, "start from genesis on an empty datadir, startupnotify ready, SIGTERM "
                                           "(the whole process lifetime; no RPC)"),
    "mempool-accept": (w_mempool_accept, "sendrawtransaction of the first --txs fixture txs, one client"),
    "gbt": (w_gbt, "fill the mempool with --gbt-mempool txs, then --gbt-calls fresh getblocktemplates"),
    "rpc": (w_rpc, "--calls sequential calls of one --call label (rpc-regtest's parameter sets)"),
    "reindex-chainstate-testnet": (w_reindex_chainstate_testnet,
                                   "-reindex-chainstate of testnet blocks 0..--height, no peers"),
    "ibd-testnet-seed": (w_ibd_testnet_seed, "IBD of testnet 0..--height from local seeds (--seeds 1 for a fixed "
                                             "peer order)"),
}


@dataclass(frozen=True)
class Pf0Workload:
    """A workload's PF0 path (§7, §12, D89): `config(args)` builds the measured run's NodeConfig (the preflight
    runs exactly its argv), `ready` is how a start shows it accepted the argv ("file": <datadir>/ready from
    -startupnotify; "rpc": RPC answers), and `measured` whether PF0 can measure it (the fixture rows need the PREP
    flow, which no phase-1 row has: preflight only, so they show REFUSED with their owners, D86). `methods(args)`
    names the RPC calls the workload makes, as (method, *subcommand); one the node lacks is a refusal of the row
    (D113)."""

    config: object
    ready: str
    measured: bool
    methods: object = lambda a: ()


def rpc_row_calls(a):
    """The rpc row's calls: build_workload's, then those of its `--call` label."""
    if a.call not in CALL_METHODS:
        raise ValueError(f"unknown --call {a.call!r}; one of {', '.join(CALL_METHODS)}")
    return (("getblockhash",), ("getblock",), CALL_METHODS[a.call])


# What each fixture row calls over RPC beyond the getblockcount ready signal, as (method, *subcommand):
# regtest_run's effective_config (getblockchaininfo), then the row's own work.
FIXTURE_METHODS = {
    "startup": lambda a: (("getblockchaininfo",),),
    "mempool-accept": lambda a: (("getblockchaininfo",), ("sendrawtransaction",), ("getmempoolinfo",)),
    "gbt": lambda a: (("getblockchaininfo",), ("sendrawtransaction",), ("getblocktemplate",)),
    "rpc": lambda a: (("getblockchaininfo",), *rpc_row_calls(a)),
}

PF0_WORKLOADS = {
    "startup-genesis": Pf0Workload(genesis_config, "file", True),
    **{name: Pf0Workload(lambda a, x=x: fixture_config(a, pf0.NODE_DATADIR, x), "rpc", False, FIXTURE_METHODS[name])
       for name, x in (("startup", False), ("mempool-accept", False), ("gbt", False), ("rpc", True))},
}


def cmd_preflight(args, impl):
    """`workload NAME --pf0 --preflight`: the preflight batch over the row's exact measured argv (§7); the manifest
    carries its record and `measured`, whether the row has a PF0 measured run (perfgate runs none otherwise). Exit 0
    for ok and refused, 1 when the preflight is neither."""
    w = PF0_WORKLOADS[args.name]
    argv = impl.argv(w.config(args))
    manifest = {"workload": args.name, "pf0": True, "preflight_argv": argv, "measured": w.measured}
    try:
        methods = tuple(dict.fromkeys(w.methods(args)))
        rec = pf0.preflight(argv, lambda cur, i: pf0.run_attempt(cur, i, ready=w.ready, methods=methods))
        manifest.update(status=rec["status"], preflight=rec)
        rc = 0
    except Exception as e:  # noqa: BLE001 — every failure is a recorded ERROR (D87)
        manifest.update(status="error", error=pf0.printable(e) if isinstance(e, pf0.PreflightError) else repr(e))
        rc = 1
    results.write_json(args.json, manifest)
    print(json.dumps(manifest, indent=1))
    return rc


def cmd_workload(args):
    from .cli import Context
    from .impls import make_impl

    if args.list or not args.name:
        for name, (_, desc) in WORKLOADS.items():
            print(f"{name:28} {desc}")
        return 0
    if args.name not in WORKLOADS:
        raise SystemExit(f"unknown workload {args.name!r}; see --list")
    for k, v in DEFAULTS.items():
        if getattr(args, k, None) is None:
            setattr(args, k, v)
    if getattr(args, "pf0", False):
        pf0.check_args(args)
        if args.name not in PF0_WORKLOADS:
            raise SystemExit(f"workload {args.name!r} has no PF0 path (PF0 workloads: {', '.join(PF0_WORKLOADS)})")
        if not (PF0_WORKLOADS[args.name].measured or getattr(args, "preflight", False)):
            raise SystemExit(f"workload {args.name!r} has only a PF0 preflight (its fixture needs the PREP flow)")
        pf0.enable()
        paths.ROOT = args.out = pf0.RUN  # node-bench writes only RUN; the manifest goes where perfgate copies it
        args.json = pf0.MANIFEST
    elif getattr(args, "preflight", False):
        raise SystemExit("--preflight is a PF0 mode step (--pf0)")
    if args.aux_slice:  # the harness and its RPC client stay off the node's CPUs
        os.sched_setaffinity(0, cpu_set(slice_cpus(args.aux_slice)))
    impl = make_impl(args.impl, args)
    if getattr(args, "preflight", False):
        return cmd_preflight(args, impl)
    run_id = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime()) + f"-wl-{args.name}-{impl.name}"
    run_dir = os.path.join(paths.writable(args.out, "--out"), "workloads", run_id)
    if args.json:
        paths.not_protected(args.json, "--json")
    os.makedirs(run_dir)
    ctx = Context(args, impl, results.Result(args.name, {"impl": impl.name}, {}, {}), run_dir)
    ctx.wrap = shlex.split(args.wrap or "")
    ctx.manifest = {"workload": args.name, "impl": impl.describe(), "wrap": ctx.wrap, "run_dir": run_dir,
                    "harness_commit": None if pf0.on() else paths.harness_commit(),
                    "params": {k: getattr(args, k) for k in DEFAULTS}, "pf0": pf0.on(), "status": "running"}
    t0 = time.monotonic()
    fn, _ = WORKLOADS[args.name]
    rc = 0
    ctx.markers = Markers(run_dir, args.ctl_fifo)
    try:
        ctx.manifest.update(fn(ctx, args))
        ctx.manifest["status"] = "ok"
    except BaseException as e:
        ctx.manifest.update(status="error", error=repr(e))
        rc = 130 if isinstance(e, KeyboardInterrupt) else 1
    finally:
        ctx.manifest["markers"] = ctx.markers.events
        ctx.markers.close()
        ctx.manifest["elapsed_s"] = round(time.monotonic() - t0, 3)
        if args.json:
            results.write_json(args.json, ctx.manifest)
        results.write_json(os.path.join(run_dir, "manifest.json"), ctx.manifest)
        print(json.dumps(ctx.manifest, indent=1, default=str))
        if not args.keep:
            paths.safe_rmtree(os.path.join(run_dir, "datadir"), "datadir")
        ctx.close()
    if rc:
        print(f"workload {args.name} FAILED: {ctx.manifest.get('error')}", flush=True)
    return rc
