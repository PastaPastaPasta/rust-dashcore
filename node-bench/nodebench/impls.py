"""Node implementations under test, behind one interface.

Every scenario talks to a node only through `NodeConfig` (what to run) and
`Node` (a running instance: JSON-RPC, pid, datadir, log). Adding the Rust full
node means pointing `--rust-bin` at its daemon; it is expected to accept the
dashd-compatible flag subset documented in README.md ("Plugging in the Rust
node"). Anything it maps differently goes in `RustImpl.translate`.
"""

import atexit
import hashlib
import os
import shutil
import signal
import socket
import subprocess
import sys
import threading
import time
from dataclasses import dataclass, field

from . import limits, paths, pf0
from . import proc as procmod
from .rpc import Rpc, RpcError

RPC_USER = "bench"
RPC_PASSWORD = "bench"

MIN_FREE_GIB = 20  # never start work that would leave less than this free on the bench filesystem
CHAIN_FLAG = {"main": None, "test": "-testnet", "regtest": "-regtest"}
CHAIN_SUBDIR = {"main": "", "test": "testnet3", "regtest": "regtest"}
CHAIN_LABEL = {"main": "mainnet", "test": "testnet", "regtest": "regtest"}


def free_port():
    if pf0.on():
        raise pf0.Pf0Error("PF0 mode pins the daemon's ports (D31): free_port() refused")
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


@dataclass
class NodeConfig:
    """Canonical, implementation-neutral node configuration.

    The same NodeConfig is handed to every implementation; that is what makes
    two runs "comparable configuration".
    """

    chain: str = "regtest"
    datadir: str = ""
    rpcport: int = field(default_factory=free_port)
    p2pport: int = field(default_factory=free_port)
    listen: bool = False
    connect: list = field(default_factory=list)  # [] = normal peer discovery
    txindex: bool = False
    addressindex: bool = False  # dashd: address+spent+timestamp indexes
    blockfilterindex: bool = False
    dbcache_mib: int = 4096
    par: int = 0  # script-check threads, 0 = auto
    assumevalid: str = None  # None = chain default, "0" = verify all
    wallet: bool = False
    debug: list = field(default_factory=list)
    reindex: bool = False
    reindex_chainstate: bool = False
    maxmempool_mib: int = 0  # 0 = default
    extra: list = field(default_factory=list)  # raw extra args, impl-specific
    bare: bool = False  # argv = -datadir, the chain flag and `extra` only (a workload's whole recipe)


class Impl:
    name = "?"

    def __init__(self, daemon, cli=None):
        self.daemon = daemon
        self.cli = cli

    def version(self):
        if not self.daemon:
            return None
        if pf0.on():  # a class-version spawn in its own fresh /pf0/run/version (D46)
            try:
                out = subprocess.run(pf0.stub_argv("version", [f"-datadir={pf0.VERSION_DATADIR}", "-version"]),
                                     stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=300).stdout
            except (OSError, subprocess.TimeoutExpired):
                return "unknown"
            return out.splitlines()[0].strip()[:200] if out else "unknown"
        import tempfile

        try:
            # v24 refuses -version when the default datadir does not exist.
            with tempfile.TemporaryDirectory() as d:
                out = subprocess.run([self.daemon, f"-datadir={d}", "-version"], capture_output=True, text=True,
                                     timeout=60).stdout
            return out.splitlines()[0].strip() if out else "unknown"
        except (OSError, subprocess.TimeoutExpired):
            return "unknown"

    def argv(self, cfg):
        """dashd-compatible command line for `cfg`."""
        if cfg.bare:
            return self.translate([self.daemon, f"-datadir={cfg.datadir}",
                                   *([CHAIN_FLAG[cfg.chain]] if CHAIN_FLAG[cfg.chain] else []), *cfg.extra])
        a = [self.daemon, f"-datadir={cfg.datadir}", "-server=1", "-daemon=0", "-printtoconsole=0",
             "-logtimemicros=1", f"-rpcuser={RPC_USER}", f"-rpcpassword={RPC_PASSWORD}",
             "-rpcbind=127.0.0.1", "-rpcallowip=127.0.0.1", f"-rpcport={cfg.rpcport}",
             "-rpcthreads=16", "-rpcworkqueue=256", f"-dbcache={cfg.dbcache_mib}", f"-par={cfg.par}",
             f"-txindex={int(cfg.txindex)}", "-upnp=0", "-natpmp=0", "-listenonion=0"]
        if CHAIN_FLAG[cfg.chain]:
            a.append(CHAIN_FLAG[cfg.chain])
        if cfg.listen:
            a += ["-listen=1", "-bind=127.0.0.1", f"-port={cfg.p2pport}"]
        else:
            a += ["-listen=0"]
            if cfg.p2pport:
                a.append(f"-port={cfg.p2pport}")
        for peer in cfg.connect:
            a.append(f"-connect={peer}")
        if cfg.connect:
            a += ["-dnsseed=0", "-fixedseeds=0"]
        if cfg.addressindex:
            a += ["-addressindex=1", "-spentindex=1", "-timestampindex=1"]
        if cfg.blockfilterindex:
            a += ["-blockfilterindex=1", "-peerblockfilters=1"]
        if cfg.assumevalid is not None:
            a.append(f"-assumevalid={cfg.assumevalid}")
        a.append("-disablewallet=0" if cfg.wallet else "-disablewallet=1")
        a += [f"-debug={c}" for c in cfg.debug]
        if cfg.reindex:
            a.append("-reindex=1")
        if cfg.reindex_chainstate:
            a.append("-reindex-chainstate=1")
        if cfg.maxmempool_mib:
            a.append(f"-maxmempool={cfg.maxmempool_mib}")
        return self.translate(a + list(cfg.extra))

    def translate(self, argv):
        return argv

    def log_path(self, cfg):
        sub = CHAIN_SUBDIR[cfg.chain]
        return os.path.join(cfg.datadir, sub, "debug.log") if sub else os.path.join(cfg.datadir, "debug.log")

    def chain_dir(self, cfg):
        sub = CHAIN_SUBDIR[cfg.chain]
        return os.path.join(cfg.datadir, sub) if sub else cfg.datadir

    def log_format(self):
        """Name of the per-block log parser in logparse.py ('dashd' or 'jsonl')."""
        return "dashd"

    def effective_config(self, node):
        """The configuration the running node actually applied. Indexes and the mempool cap come over RPC;
        dbcache, script threads and assumevalid from the node's startup log of this invocation, because dashd
        has no RPC for them (a Rust node logs the same dashd-format lines, see README)."""
        from . import logparse

        rpc = node.rpc
        chain = rpc.getblockchaininfo().get("chain")
        out = {"chain": chain, "indexes": sorted(enabled_indexes(rpc, chain))}
        try:
            out["maxmempool_bytes"] = rpc.getmempoolinfo().get("maxmempool")
        except RpcError:
            out["maxmempool_bytes"] = None
        log = self.log_path(node.cfg)
        out.update(logparse.init_config(log, node.log_offset) if os.path.exists(log)
                   else {"dbcache_mib": None, "script_threads": None, "assumevalid": None})
        return out

    def disk_parts(self, cfg):
        """{component: directory} for the per-component disk metrics (PRD metric 11). A Rust node with a different
        layout maps its own directories onto these names here."""
        return {p: os.path.join(self.chain_dir(cfg), p) for p in ("blocks", "chainstate", "indexes", "evodb", "llmq")}

    def describe(self):
        if pf0.on():  # node-bench can't see the exe: the broker records its sha256 (D46)
            return {"impl": self.name, "daemon": self.daemon, "version": self.version(), "sha256": None}
        return {"impl": self.name, "daemon": self.daemon and os.path.realpath(self.daemon),
                "version": self.version(), "sha256": self.daemon and sha256_file(self.daemon)}


class DashdImpl(Impl):
    name = "dashd"


class RustImpl(Impl):
    """The rust-dashcore full node (not built yet).

    Contract (see README): dashd-compatible flags for everything NodeConfig
    emits, the JSON-RPC subset the scenarios call, and either dashd-style
    `UpdateTip:` lines in <datadir>/[chain/]debug.log or a JSONL bench log.
    """

    name = "rust"

    def log_format(self):
        return os.environ.get("NODE_BENCH_RUST_LOG_FORMAT", "jsonl")


INDEX_NAMES = {"basic block filter index": "blockfilterindex"}  # getindexinfo name -> NodeConfig name
B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def probe_address(chain):
    """A valid P2PKH address (all-zero key hash) on `chain`: v23's address RPCs skip the index check for an
    empty address list, so the probe needs a real one."""
    data = bytes([76 if chain == "main" else 140]) + bytes(20)
    data += hashlib.sha256(hashlib.sha256(data).digest()).digest()[:4]
    n, out = int.from_bytes(data, "big"), ""
    while n:
        n, r = divmod(n, 58)
        out = B58[r] + out
    return out


def index_probes(chain):
    """Index RPCs for nodes whose getindexinfo does not list the index (dashd v23); an error saying the index is
    "disabled" / "not enabled" means it is off."""
    return {"addressindex": ("getaddressbalance", [{"addresses": [probe_address(chain)]}]),
            "spentindex": ("getspentinfo", [{"txid": "00" * 32, "index": 0}]),
            "timestampindex": ("getblockhashes", [2, 1])}


def requested_indexes(cfg):
    out = {"txindex"} if cfg.txindex else set()
    if cfg.addressindex:
        out |= {"addressindex", "spentindex", "timestampindex"}
    if cfg.blockfilterindex:
        out.add("blockfilterindex")
    return out


def enabled_indexes(rpc, chain):
    """Index names the node reports as running (getindexinfo, else the index RPCs' errors)."""
    try:
        info = rpc.getindexinfo()
    except RpcError as e:
        if e.code != -32601:
            raise
        info = {}
    out = {INDEX_NAMES.get(k, k) for k in info}
    for name, (method, params) in index_probes(chain).items():
        if name in out:
            continue
        try:
            rpc.call(method, *params)
            out.add(name)
        except RpcError as e:
            msg = (e.message or "").lower()
            if e.code != -32601 and "disabled" not in msg and "not enabled" not in msg:
                out.add(name)  # e.g. -5 "not found": the index answered the query
    return out


def config_problems(cfg, eff):
    """Differences between what was asked for (NodeConfig) and what the node applied."""
    out = []
    if eff.get("dbcache_mib") is None:
        out.append("dbcache not reported (no 'Cache configuration' lines in the log)")
    elif abs(eff["dbcache_mib"] - cfg.dbcache_mib) > 1:
        out.append(f"dbcache {eff['dbcache_mib']} MiB, asked for {cfg.dbcache_mib}")
    if eff.get("script_threads") is None:
        out.append("script threads not reported ('Script verification uses N additional threads')")
    elif cfg.par > 0 and eff["script_threads"] != cfg.par:
        out.append(f"{eff['script_threads']} script threads, asked for -par={cfg.par}")
    if eff.get("assumevalid") is None:
        out.append("assumevalid not reported ('Assuming ancestors of block' / 'Validating signatures')")
    elif cfg.assumevalid is not None and eff["assumevalid"] != cfg.assumevalid:
        out.append(f"assumevalid {eff['assumevalid']}, asked for {cfg.assumevalid}")
    want = requested_indexes(cfg)
    if set(eff.get("indexes", [])) != want:
        out.append(f"indexes {sorted(eff.get('indexes', []))}, asked for {sorted(want)}")
    if cfg.maxmempool_mib and eff.get("maxmempool_bytes") != cfg.maxmempool_mib * 1_000_000:
        out.append(f"maxmempool {eff.get('maxmempool_bytes')} bytes, asked for {cfg.maxmempool_mib} MB")
    return out


def sha256_file(path):
    try:
        h = hashlib.sha256()
        with open(os.path.realpath(path), "rb") as f:
            for chunk in iter(lambda: f.read(1 << 20), b""):
                h.update(chunk)
        return h.hexdigest()
    except OSError:
        return None


class Node:
    """A running node. Use as a context manager so it is always stopped.

    The node runs in its own session/process group together with everything it (or a counter wrapper such as
    `perf stat`) forks; every stop path ends by killing and draining that whole group, and with --limits it is
    inside its cgroup before its first instruction (see spawn). The group leader also gets SIGTERM if the
    harness dies (setpriv --pdeathsig); if the harness is SIGKILLed, a forking wrapper's child can survive
    (nothing in Linux reaps it for us), so prefer non-forking wrappers or clean up by process group.
    """

    def __init__(self, impl, cfg, cpu_slice=None, stdout_path=None, wrap=(), spawn_class="run"):
        """wrap: argv inserted right before the daemon (e.g. `perf stat -x, -o f --`) for counter runs.
        spawn_class: in PF0 mode, the broker class of this start (only `run` is measured)."""
        self.impl, self.cfg, self.cpu_slice, self.wrap = impl, cfg, cpu_slice, list(wrap)
        self.spawn_class = spawn_class
        if pf0.on():
            if self.wrap or cpu_slice:
                raise pf0.Pf0Error("PF0 mode: the broker counts and places the node (no wrap, no slice)")
            # Outside the datadir: the broker makes that fresh right before the spawn (D46).
            stdout_path = stdout_path or os.path.join(pf0.RUN, f"stdout-{spawn_class}.log")
        self.stdout_path = stdout_path or os.path.join(cfg.datadir, "bench-stdout.log")
        self.proc = None
        self.t_spawn = self.spawn_wall = self.t_exit = None
        self.rpc = None
        self.cgroup = None
        self.log_offset = 0
        self._out = None
        self._exited = threading.Event()

    @property
    def pid(self):
        return self.proc.pid

    def argv(self):
        if pf0.on():
            return pf0.stub_argv(self.spawn_class, self.impl.argv(self.cfg)[1:])
        return spawn_prefix(self.cpu_slice) + self.wrap + self.impl.argv(self.cfg)

    def start(self):
        if not self.impl.daemon:
            raise SystemExit(f"--impl {self.impl.name} needs a node binary (--rust-bin / NODE_BENCH_RUST_BIN)")
        if not pf0.on():  # in PF0 mode the broker creates it, fresh (D46)
            os.makedirs(self.cfg.datadir, exist_ok=True)
        log = self.impl.log_path(self.cfg)
        self.log_offset = os.path.getsize(log) if os.path.exists(log) else 0  # this invocation's log starts here
        self._out = open(self.stdout_path, "ab")
        self.t_spawn, self.spawn_wall = time.monotonic(), time.time()
        cmd = pf0.stub_argv(self.spawn_class, self.impl.argv(self.cfg)[1:]) if pf0.on() else (
            self.wrap + self.impl.argv(self.cfg))
        self.proc, self.cgroup = spawn(cmd, self.cpu_slice, stdout=self._out, stderr=subprocess.STDOUT,
                                       stdin=subprocess.DEVNULL)
        try:
            # Exact exit time: a blocking waitpid on its own thread (Popen.wait(timeout) polls at 50 ms).
            threading.Thread(target=self._reap, daemon=True).start()
            self.rpc = self.new_rpc()
        except BaseException:
            self.kill()
            raise
        return self

    def _reap(self):
        self.proc.wait()
        self.t_exit = time.monotonic()
        self._exited.set()

    def new_rpc(self, timeout=600, wallet=None):
        return Rpc(self.cfg.rpcport, RPC_USER, RPC_PASSWORD, timeout=timeout, wallet=wallet)

    def wait_ready(self, timeout=3600):
        """Seconds from spawn until RPC answers outside warmup."""
        self.rpc.wait_ready(timeout, proc=self.proc)
        return time.monotonic() - self.t_spawn

    def wait_exit(self, timeout=None):
        """True once the leader (wrapper or daemon) has exited."""
        return self._exited.wait(timeout)

    def stop(self, timeout=900, rpc=True):
        """Clean `stop` RPC (SIGTERM to the group, then SIGKILL, as fallbacks; SIGTERM first without `rpc`).
        Returns the leader's exit code (the wrapper's, when there is one).

        `t_exit` - the moment before the call is the shutdown time.
        """
        if self.proc is None:
            return None
        if not self._exited.is_set():
            if rpc:
                try:
                    self.rpc.call("stop")
                except Exception:
                    signal_group(self.proc, signal.SIGTERM)
            else:
                signal_group(self.proc, signal.SIGTERM)
            if not self._exited.wait(timeout):
                signal_group(self.proc, signal.SIGTERM)
                if not self._exited.wait(60):
                    signal_group(self.proc, signal.SIGKILL)
                    self._exited.wait()
        self._cleanup()
        return self.proc.returncode

    def kill(self):
        if self.proc and not self._exited.is_set():
            signal_group(self.proc, signal.SIGKILL)
            self._exited.wait(30)
        self._cleanup()

    def _cleanup(self):
        if self.rpc:
            self.rpc.close()
        if self._out:
            self._out.close()
            self._out = None
        try:
            if self.proc is not None:
                reap_group(self.proc)  # anything the wrapper/daemon left behind dies before its datadir goes
        finally:
            if self.cgroup:
                self.cgroup.remove()
                self.cgroup = None

    def __enter__(self):
        return self.start()

    def __exit__(self, *exc):
        self.stop()


_LIVE = set()  # process groups started by spawn() and not yet confirmed empty


def spawn(cmd, cpu_slice, **popen_kw):
    """Start `cmd` as a new session/process-group leader: pinned to `cpu_slice`, dying with the harness, and,
    when the slice has caps (--limits), already inside its cgroup when it execs (so nothing it forks escapes the
    caps: moving a pid after spawn misses children forked in between). Returns (Popen, Cgroup or None)."""
    cg = limits.create_for(cpu_slice)
    try:
        p = subprocess.Popen((cg.join_prefix() if cg else []) + spawn_prefix(cpu_slice) + cmd,
                             start_new_session=True, **popen_kw)
    except BaseException:
        if cg:
            cg.remove()
        raise
    _LIVE.add(p)
    return p, cg


def group_members(pgid):
    """Live (non-zombie) pids of process group `pgid` in session `pgid` (what spawn() created)."""
    out = []
    for pid in os.listdir("/proc"):
        if not pid.isdigit():
            continue
        try:
            with open(f"/proc/{pid}/stat") as f:
                fields = f.read().rsplit(")", 1)[1].split()
        except (OSError, IndexError):
            continue
        if fields[0] != "Z" and int(fields[2]) == pgid and int(fields[3]) == pgid:
            out.append(int(pid))
    return out


def signal_group(p, sig):
    try:
        os.killpg(p.pid, sig)
    except (ProcessLookupError, PermissionError):
        pass


def reap_group(p, grace=10.0, timeout=30.0):
    """Wait `grace` s for the rest of p's group to exit on its own, then SIGKILL it until it is empty."""
    deadline = time.monotonic() + grace
    while group_members(p.pid) and time.monotonic() < deadline:
        time.sleep(0.05)
    deadline = time.monotonic() + timeout
    while (left := group_members(p.pid)):
        if time.monotonic() > deadline:
            raise RuntimeError(f"process group {p.pid} survived SIGKILL: {left}")
        signal_group(p, signal.SIGKILL)
        time.sleep(0.05)
    if p.poll() is None:
        try:
            p.wait(timeout)
        except subprocess.TimeoutExpired:
            raise RuntimeError(f"process {p.pid} did not exit") from None
    _LIVE.discard(p)


def live_groups():
    return [p for p in _LIVE if group_members(p.pid) or p.poll() is None]


def reap_all():
    """atexit / last-resort cleanup: kill every process group this harness started that is still around."""
    for p in list(_LIVE):
        try:
            reap_group(p, grace=0)
        except RuntimeError as e:
            print(f"warning: {e}", file=sys.stderr)


atexit.register(reap_all)


def spawn_prefix(cpu_slice):
    """setpriv (die with the harness) + CPU/NUMA pinning."""
    pre = ["setpriv", "--pdeathsig", "TERM", "--"] if shutil.which("setpriv") else []
    return pre + procmod.pin_prefix(cpu_slice)


def finish_tool(p, cg):
    """Kill whatever is left of a spawned tool's process group (bench_dash, dash-spv-bench) and remove its
    cgroup."""
    try:
        reap_group(p, grace=0)
    finally:
        if cg:
            cg.remove()


def make_impl(name, args):
    if name == "dashd":
        daemon = args.dashd_bin or os.environ.get("NODE_BENCH_DASHD") or default_dashd()
        return DashdImpl(daemon, os.path.join(os.path.dirname(daemon), "dash-cli"))
    if name == "rust":
        # No daemon yet is allowed: scenarios that compare existing Rust pieces
        # (microbench, spv-headers-*) do not start a node.
        daemon = args.rust_bin or os.environ.get("NODE_BENCH_RUST_BIN")
        return RustImpl(daemon, args.rust_cli or os.environ.get("NODE_BENCH_RUST_CLI"))
    raise SystemExit(f"unknown --impl {name!r} (dashd|rust)")


def default_dashd():
    from .paths import pinned_dashd

    path = pinned_dashd()
    if not os.path.exists(path):
        raise SystemExit(f"pinned dashd not found at {path}; run `node-bench/run fetch-dashd` first")
    return path


CLONE_EXCLUDE = ("debug.log", "bench-stdout.log", "bench.jsonl", ".lock", "peers.dat", "banlist.json", "mempool.dat",
                 "wallets", "settings.json")


def clone_datadir(src, dst, exclude=CLONE_EXCLUDE):
    """Copy a datadir snapshot (reflink when the filesystem supports it) into the bench boundary.

    Instrumentation logs are dropped (a stale bench.jsonl would be parsed as this run's), dash.conf is emptied
    (every option comes from the command line), and a symlink pointing outside the clone is refused: writes
    through it would land in the source or elsewhere.
    """
    paths.not_protected(src, "clone source")
    real_dst = paths.writable(dst, "clone destination")
    paths.safe_rmtree(dst, "clone destination")
    parent = os.path.dirname(real_dst)
    if not paths.reflink_supported(parent):  # a full copy: make sure it fits, with room to spare
        paths.check_free(parent, procmod.dir_size_bytes(src) / 2**30 + MIN_FREE_GIB, f"cloning {src}")
    ignore = shutil.ignore_patterns(*exclude)
    subprocess.run(["cp", "-a", "--reflink=auto", src, dst], check=True)
    for root, dirs, files in os.walk(dst):
        for name in ignore(root, dirs + files):
            p = os.path.join(root, name)
            shutil.rmtree(p) if os.path.isdir(p) and not os.path.islink(p) else os.remove(p)
        for name in dirs + files:
            p = os.path.join(root, name)
            if os.path.islink(p) and not paths.under(os.path.realpath(p), real_dst):
                paths.safe_rmtree(dst, "clone destination")
                raise paths.UnsafePath(f"{src} contains a symlink leaving the datadir ({os.path.relpath(p, dst)} -> "
                                       f"{os.path.realpath(p)}): refusing to clone it")
            if name == "dash.conf" and os.path.isfile(p) and os.path.getsize(p):
                open(p, "w").close()
