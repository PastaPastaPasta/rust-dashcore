#!/usr/bin/env python3
"""PF1: profile dashd on RPC-driven workloads (startup, mempool acceptance, getblocktemplate, getblock) and on
regtest blocks full of transactions (connect).

    run_regtest.py --dashd BIN --out OUT --workload W [options]

Workloads (all under `perf record`, instructions:u period + cpu-clock, call graphs by frame pointer):
  startup-regtest   spawn -> RPC ready -> stop, the fixture chain (177 blocks); whole process lifetime recorded
  startup-testnet   the same on a clone of a fully synced testnet datadir (--datadir-src), txindex on
  mempool-accept    sendrawtransaction of the first --txs fixture transactions; only the loop is recorded
                    (--clients N runs them from N threads; the instruction profile uses 1)
  gbt               --mempool-fill txs in the mempool (unrecorded), then --calls x (1 new tx, wait 5.2 s,
                    getblocktemplate); only the getblocktemplate calls are recorded
  getblock          --calls getblock calls at --verbosity over testnet heights (--datadir-src), recorded
  ibd-testnet       IBD of testnet 0..--ibd-height from one local seed, default config, whole lifetime recorded
  prep-big          mine the fixture transactions into full blocks (regtest), stop; writes OUT/datadir
  connect-big       -reindex-chainstate of that datadir, window = the full blocks

Fixture: node-bench's regtest-v1 (30,000 pre-signed 1-in/2-out P2PKH transactions on a 177-block chain).
perf's control FIFOs bound the recorded part of each workload; the output holds run.json (inputs, counts,
provenance) and fold/ (see perf_fold.py).
"""

import argparse
import base64
import hashlib
import http.client
import json
import os
import random
import re
import shutil
import subprocess
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))

import fcntl

PERF_LOCK = os.environ.get("PF1_PERF_LOCK", "/work/scratch/pf1/perf.lock")


def perf_lock():
    """Hold an exclusive lock for the life of the process: concurrent perf sessions exhaust perf_event_mlock_kb."""
    f = open(PERF_LOCK, "w")
    fcntl.flock(f, fcntl.LOCK_EX)
    return f

USER, PASSWORD = "pf1", "pf1"
REGTEST_FLAGS = ["-persistmempool=0", "-checkmempool=0", "-checkblockindex=0", "-maxtipage=2000000000"]


class Rpc:
    def __init__(self, port, wallet=None):
        self.port = port
        self.path = "/" if wallet is None else f"/wallet/{wallet}"
        self.auth = "Basic " + base64.b64encode(f"{USER}:{PASSWORD}".encode()).decode()
        self.local = threading.local()

    def conn(self):
        c = getattr(self.local, "c", None)
        if c is None:
            c = self.local.c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=600)
        return c

    def call(self, method, *params):
        body = json.dumps({"jsonrpc": "1.0", "id": 1, "method": method, "params": list(params)})
        for attempt in range(2):
            try:
                c = self.conn()
                c.request("POST", self.path, body, {"Authorization": self.auth, "Content-Type": "application/json"})
                r = c.getresponse()
                data = json.loads(r.read())
                break
            except (http.client.HTTPException, ConnectionError, OSError):
                self.local.c = None
                if attempt:
                    raise
        if data.get("error"):
            raise RuntimeError(f"{method}: {data['error']}")
        return data["result"]

    def batch(self, calls):
        body = json.dumps([{"jsonrpc": "1.0", "id": i, "method": m, "params": p} for i, (m, p) in enumerate(calls)])
        c = self.conn()
        c.request("POST", self.path, body, {"Authorization": self.auth, "Content-Type": "application/json"})
        res = json.loads(c.getresponse().read())
        for r in res:
            if r.get("error"):
                raise RuntimeError(r["error"])
        return [r["result"] for r in sorted(res, key=lambda r: r["id"])]


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


class Perf:
    """perf record around a child, with enable/disable over control FIFOs (or whole lifetime)."""

    def __init__(self, out, argv, cpus, controlled, period, freq):
        self.out = out
        self.data = os.path.join(out, "perf.data")
        self.controlled = controlled
        cmd = ["taskset", "-c", cpus, "perf", "record", "-o", self.data, "-k", "monotonic", "-g",
               "-e", f"instructions/period={period}/u", "-e", f"cpu-clock/freq={freq}/"]
        if controlled:
            for n in ("ctl", "ack"):
                p = os.path.join(out, n)
                if os.path.exists(p):
                    os.unlink(p)
                os.mkfifo(p)
            cmd += ["-D", "-1", "--control", f"fifo:{out}/ctl,{out}/ack"]
        self.stdout = open(os.path.join(out, "stdout.log"), "w")
        self.proc = subprocess.Popen(cmd + ["--"] + argv, stdout=self.stdout, stderr=subprocess.STDOUT)
        self.ctl = self.ack = None
        self.windows = []  # (t_enable, t_disable) in CLOCK_MONOTONIC seconds
        self.pid = None

    def _open(self):
        if self.ctl is None:
            self.ctl = os.open(os.path.join(self.out, "ctl"), os.O_WRONLY)
            self.ack = open(os.path.join(self.out, "ack"), "rb")

    def send(self, word):
        self._open()
        os.write(self.ctl, word.encode() + b"\n")
        if self.ack.readline().strip(b"\0\n") != b"ack":
            raise RuntimeError("perf did not ack " + word)

    def enable(self):
        self.send("enable")
        self.windows.append([time.clock_gettime(time.CLOCK_MONOTONIC), None])

    def disable(self):
        self.windows[-1][1] = time.clock_gettime(time.CLOCK_MONOTONIC)
        self.send("disable")

    def child_pid(self):
        if self.pid is None:
            out = subprocess.run(["pgrep", "-P", str(self.proc.pid)], capture_output=True, text=True).stdout.split()
            for p in out:
                comm = open(f"/proc/{p}/comm").read().strip()
                if comm.startswith("dashd"):
                    self.pid = int(p)
                    break
                sub = subprocess.run(["pgrep", "-P", p], capture_output=True, text=True).stdout.split()
                for q in sub:
                    if open(f"/proc/{q}/comm").read().strip().startswith("dashd"):
                        self.pid = int(q)
        return self.pid

    def peak_rss_kib(self):
        pid = self.child_pid()
        try:
            for line in open(f"/proc/{pid}/status"):
                if line.startswith("VmHWM:"):
                    return int(line.split()[1])
        except OSError:
            pass
        return None

    def wait(self):
        rc = self.proc.wait()
        for f in (self.ctl,):
            if f is not None:
                try:
                    os.close(f)
                except OSError:
                    pass
        return rc


def perf_throttle_events(perf_data):
    """Count THROTTLE events in perf.data; any means samples were dropped and the instruction total is an undercount."""
    out = subprocess.run(["perf", "report", "-i", perf_data, "--stats"], capture_output=True, text=True).stdout
    m = re.search(r"^\s*THROTTLE events:\s+(\d+)", out, re.M)
    return int(m.group(1)) if m else 0


def fold(out, windows):
    """perf script | perf_fold, restricted to the recorded windows (perf's monotonic clock) when given."""
    results = []
    scr = subprocess.run(["perf", "script", "-i", os.path.join(out, "perf.data"), "-F",
                          "comm,tid,time,event,period,ip,sym,dso", "--no-inline"], capture_output=True, text=True,
                         errors="replace")
    args = [sys.executable, os.path.join(HERE, "perf_fold.py"), "--out", os.path.join(out, "fold")]
    if windows:
        args += ["--window", repr(min(w[0] for w in windows)), repr(max(w[1] for w in windows))]
    r = subprocess.run(args, input=scr.stdout, capture_output=True, text=True)
    return r.stderr.strip().splitlines()


def dashd_argv(a, datadir, port, rpcport, extra, chain="regtest"):
    argv = [a.dashd, f"-datadir={datadir}", "-server=1", "-printtoconsole=0", "-logtimemicros=1",
            f"-rpcuser={USER}", f"-rpcpassword={PASSWORD}", "-rpcbind=127.0.0.1", "-rpcallowip=127.0.0.1",
            f"-rpcport={rpcport}", f"-port={port}", "-rpcthreads=16", "-rpcworkqueue=256", f"-dbcache={a.dbcache}",
            f"-par={a.par}", f"-parbls={a.parbls}", "-upnp=0", "-natpmp=0", "-listenonion=0", "-listen=0",
            "-connect=0", "-dnsseed=0", "-fixedseeds=0", "-disablewallet=1"]
    if chain == "regtest":
        argv += ["-regtest"] + REGTEST_FLAGS
    elif chain == "testnet":
        argv += ["-testnet"]
    return argv + extra


def wait_ready(rpc, proc, timeout=900, t0=None):
    """Seconds from `t0` (default: now) until the RPC server answers getblockcount."""
    t0 = t0 or time.time()
    while time.time() - t0 < timeout:
        if proc.poll() is not None:
            raise RuntimeError("node exited during startup")
        try:
            rpc.call("getblockcount")
            return time.time() - t0
        except Exception:
            time.sleep(0.01)
    raise TimeoutError("RPC never came up")


def wait_idle(perf, quiet_s=3.0, max_s=900, busy_frac=0.02):
    """Block until dashd's own CPU use stays under `busy_frac` of a core for `quiet_s` seconds.

    RPC answers while startup work continues in the background (the one-time v24 evodb repair, cache warm-up), so a
    controlled window opened right after getblockcount would profile that work instead of the workload."""
    pid, hz = perf.child_pid(), os.sysconf("SC_CLK_TCK")

    def cpu():
        f = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
        return (int(f[11]) + int(f[12])) / hz

    t0 = time.monotonic()
    last, last_t, quiet_since = cpu(), time.monotonic(), None
    while time.monotonic() - t0 < max_s:
        time.sleep(1.0)
        now, t = cpu(), time.monotonic()
        busy = (now - last) / (t - last_t) > busy_frac
        last, last_t = now, t
        if busy:
            quiet_since = None
        elif quiet_since is None:
            quiet_since = t
        elif t - quiet_since >= quiet_s:
            return round(t - t0, 1)
    raise TimeoutError("dashd never went idle")


def clone(src, dst, skip=("wallets", "debug.log", "bench-stdout.log")):
    shutil.rmtree(dst, ignore_errors=True)
    subprocess.run(["cp", "-a", "--reflink=always", src, dst], check=True)
    for s in skip:
        shutil.rmtree(os.path.join(dst, s), ignore_errors=True)
        for root, dirs, files in os.walk(dst):
            for d in list(dirs):
                if d == s:
                    shutil.rmtree(os.path.join(root, d), ignore_errors=True)
                    dirs.remove(d)
            for f in files:
                if f == s:
                    os.unlink(os.path.join(root, f))


def load_txs(fixture, n=None, tail=None):
    txs = []
    with open(os.path.join(fixture, "txs.jsonl")) as f:
        for line in f:
            txs.append(json.loads(line)["hex"])
    return txs[:n] if n is not None else txs[-tail:] if tail else txs


def run_node_workload(a, work):
    """Start dashd under perf on a cloned datadir, run `work(rpc, perf, meta)`, stop, fold. Returns meta."""
    os.makedirs(a.out, exist_ok=True)
    chain = "testnet" if a.workload in ("startup-testnet", "getblock") else "regtest"
    datadir = os.path.join(a.out, "datadir")
    src = a.datadir_src
    clone(src, datadir)
    port, rpcport = a.port, a.port + 1
    extra = a.extra
    argv = dashd_argv(a, datadir, port, rpcport, extra, chain)
    whole = a.workload.startswith("startup")
    meta = {"workload": a.workload, "argv": argv, "dashd": a.dashd, "dashd_sha256": sha256(a.dashd),
            "datadir_src": src, "cpus": a.cpus, "par": a.par, "parbls": a.parbls, "dbcache_mib": a.dbcache}
    t_spawn = time.time()
    perf = Perf(a.out, argv, a.cpus, controlled=not whole, period=a.period, freq=a.freq)
    rpc = Rpc(rpcport)
    try:
        ready = wait_ready(rpc, perf.proc, t0=t_spawn)
        meta["ready_s"] = round(ready, 3)
        if not whole:
            meta["idle_wait_s"] = wait_idle(perf)
        work(rpc, perf, meta)
        meta["peak_rss_kib"] = perf.peak_rss_kib()
        rpc.call("stop")
    except BaseException as e:
        meta["error"] = repr(e)
        subprocess.run(["pkill", "-P", str(perf.proc.pid)], check=False)
    rc = perf.wait()
    meta["exit_code"] = rc
    meta["wall_total_s"] = round(time.time() - t_spawn, 3)
    meta["recorded_windows_monotonic"] = perf.windows
    if rc == 0 and "error" not in meta:
        meta["fold_stderr"] = fold(a.out, [w for w in perf.windows if w[1]] if not whole else None)
        meta["perf_throttle_events"] = perf_throttle_events(os.path.join(a.out, "perf.data"))
        if meta["perf_throttle_events"]:
            meta["error"] = "perf throttled: samples dropped, counts are an undercount"
    json.dump(meta, open(os.path.join(a.out, "run.json"), "w"), indent=1)
    if not a.keep_datadir:
        shutil.rmtree(datadir, ignore_errors=True)
    return meta


def wl_startup(a):
    def work(rpc, perf, meta):
        meta["ops"] = 1
        meta["height"] = rpc.call("getblockcount")
    return run_node_workload(a, work)


def wl_mempool(a):
    txs = load_txs(a.fixture, a.txs)

    def work(rpc, perf, meta):
        lat = []
        perf.enable()
        t0 = time.monotonic()
        if a.clients <= 1:
            for t in txs:
                s = time.monotonic()
                rpc.call("sendrawtransaction", t)
                lat.append(time.monotonic() - s)
        else:
            chunks = [txs[i::a.clients] for i in range(a.clients)]
            err = []

            def run(chunk):
                try:
                    for t in chunk:
                        rpc.call("sendrawtransaction", t)
                except Exception as e:  # noqa: BLE001
                    err.append(e)

            ths = [threading.Thread(target=run, args=(c,)) for c in chunks]
            [t.start() for t in ths]
            [t.join() for t in ths]
            if err:
                raise err[0]
        meta["loop_wall_s"] = round(time.monotonic() - t0, 4)
        perf.disable()
        info = rpc.call("getmempoolinfo")
        if info["size"] != len(txs):
            raise RuntimeError(f"mempool holds {info['size']} of {len(txs)} txs")
        meta.update(ops=len(txs), unit="tx", clients=a.clients, mempool=info)
        if lat:
            lat.sort()
            meta["latency_ms"] = {"p50": lat[len(lat) // 2] * 1e3, "p95": lat[int(len(lat) * .95)] * 1e3,
                                  "mean": sum(lat) / len(lat) * 1e3}
    return run_node_workload(a, work)


def wl_gbt(a):
    txs = load_txs(a.fixture)
    fill, extra = txs[:a.mempool_fill], txs[-(a.calls + a.warmup_calls):]

    def work(rpc, perf, meta):
        for i in range(0, len(fill), 500):
            rpc.batch([("sendrawtransaction", [t]) for t in fill[i:i + 500]])
        times = []
        for i, t in enumerate(extra):
            rpc.call("sendrawtransaction", t)
            time.sleep(5.2)  # dashd answers from its template cache unless the mempool changed and >5 s passed
            warm = i < a.warmup_calls  # the first calls pay one-time costs (script cache, allocator growth)
            if not warm:
                perf.enable()
            s = time.monotonic()
            res = rpc.call("getblocktemplate", {})
            if not warm:
                times.append(time.monotonic() - s)
                perf.disable()
        meta.update(ops=len(times), warmup_calls=a.warmup_calls, unit="getblocktemplate", mempool_txs=len(fill) + len(extra),
                    template_txs=len(res["transactions"]), call_ms=[round(x * 1e3, 2) for x in times])
    return run_node_workload(a, work)


def wl_getblock(a):
    rng = random.Random(42)

    def work(rpc, perf, meta):
        tip = rpc.call("getblockcount")
        heights = [rng.randrange(a.h_lo, min(a.h_hi, tip)) for _ in range(a.calls)]
        hashes = rpc.batch([("getblockhash", [h]) for h in heights])
        perf.enable()
        t0 = time.monotonic()
        nbytes = 0
        for h in hashes:
            r = rpc.call("getblock", h, a.verbosity)
            nbytes += len(r) if isinstance(r, str) else len(json.dumps(r))
        meta["loop_wall_s"] = round(time.monotonic() - t0, 4)
        perf.disable()
        meta.update(ops=len(hashes), unit=f"getblock v{a.verbosity}", response_bytes=nbytes, tip=tip,
                    heights=f"random in [{a.h_lo},{a.h_hi}) seed 42")
    return run_node_workload(a, work)


def wl_ibd(a):
    """IBD of testnet blocks 0..--ibd-height from one local seed (a clone of --datadir-src, official dashd),
    default config (txindex on, default assumevalid unless --extra says otherwise), whole process under perf."""
    os.makedirs(a.out, exist_ok=True)
    seed_dir = os.path.join(a.out, "seed")
    clone(a.datadir_src, seed_dir)
    datadir = os.path.join(a.out, "datadir")
    shutil.rmtree(datadir, ignore_errors=True)
    os.makedirs(datadir)
    sport, srpc = a.port, a.port + 1
    nport, nrpc = a.port + 2, a.port + 3
    seed_argv = [a.seed_dashd, "-testnet", f"-datadir={seed_dir}", "-server=1", f"-rpcuser={USER}", f"-rpcpassword={PASSWORD}",
                 f"-rpcport={srpc}", f"-port={sport}", "-listen=1", "-bind=127.0.0.1", "-whitelist=127.0.0.1",
                 "-connect=0", "-dnsseed=0", "-fixedseeds=0", "-disablewallet=1", "-upnp=0", "-natpmp=0",
                 "-listenonion=0", "-printtoconsole=0"]
    seed = subprocess.Popen(["taskset", "-c", a.seed_cpus] + seed_argv, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
    meta = {"workload": "ibd-testnet", "dashd": a.dashd, "dashd_sha256": sha256(a.dashd), "seed_dashd": a.seed_dashd,
            "datadir_src": a.datadir_src, "ibd_height": a.ibd_height, "cpus": a.cpus, "par": a.par, "parbls": a.parbls,
            "dbcache_mib": a.dbcache, "extra": a.extra}
    try:
        wait_ready(Rpc(srpc), seed)
        argv = dashd_argv(a, datadir, nport, nrpc, [f"-connect=127.0.0.1:{sport}", f"-stopatheight={a.ibd_height}",
                                                    "-logtimemicros=1"] + a.extra, "testnet")
        argv = [x for x in argv if x not in ("-connect=0", "-disablewallet=1")] + ["-disablewallet=1"]
        meta["argv"] = argv
        t0 = time.time()
        perf = Perf(a.out, argv, a.cpus, controlled=False, period=a.period, freq=a.freq)
        stop_by = time.time() + a.timeout
        while perf.proc.poll() is None and time.time() < stop_by:
            time.sleep(1)
        if perf.proc.poll() is None:
            subprocess.run(["pkill", "-INT", "-P", str(perf.proc.pid)])
            meta["error"] = "timeout"
        rc = perf.wait()
        meta.update(exit_code=rc, wall_total_s=round(time.time() - t0, 2), ops=a.ibd_height, unit="block")
        tips = {}
        log = os.path.join(datadir, "testnet3", "debug.log")
        import re
        import datetime
        for line in open(log, errors="replace"):
            m = re.match(r"^(\S+) UpdateTip: new best=\S+ height=(\d+) ", line)
            if m:
                tips[int(m.group(2))] = datetime.datetime.fromisoformat(m.group(1).replace("Z", "+00:00")).timestamp()
        if tips:
            lo, hi = min(tips), max(tips)
            meta.update(first_tip=lo, last_tip=hi, tip_span_s=round(tips[hi] - tips[lo], 2),
                        blocks_per_s=round((hi - lo) / max(tips[hi] - tips[lo], 1e-9), 1))
        if rc == 0 and "error" not in meta:
            meta["fold_stderr"] = fold(a.out, None)
            meta["perf_throttle_events"] = perf_throttle_events(os.path.join(a.out, "perf.data"))
            if meta["perf_throttle_events"]:
                meta["error"] = "perf throttled: samples dropped, counts are an undercount"
    finally:
        seed.kill()
        shutil.rmtree(seed_dir, ignore_errors=True)
    json.dump(meta, open(os.path.join(a.out, "run.json"), "w"), indent=1)
    if not a.keep_datadir:
        shutil.rmtree(datadir, ignore_errors=True)
    return meta


def wl_prep_big(a):
    """Start the fixture chain, load every fixture tx, mine it into full blocks, stop. OUT/datadir keeps the result."""
    os.makedirs(a.out, exist_ok=True)
    datadir = os.path.join(a.out, "datadir")
    clone(a.datadir_src, datadir)
    port, rpcport = a.port, a.port + 1
    argv = dashd_argv(a, datadir, port, rpcport, ["-disablewallet=0", "-fallbackfee=0.0001"])
    argv = [x for x in argv if x != "-disablewallet=1"]
    log = open(os.path.join(a.out, "prep.stdout"), "w")
    proc = subprocess.Popen(["taskset", "-c", a.cpus] + argv, stdout=log, stderr=subprocess.STDOUT)
    rpc = Rpc(rpcport)
    try:
        wait_ready(rpc, proc)
        txs = load_txs(a.fixture)
        for i in range(0, len(txs), 500):
            rpc.batch([("sendrawtransaction", [t]) for t in txs[i:i + 500]])
        rpc.call("createwallet", "pf1")
        w = Rpc(rpcport, "pf1")
        addr = w.call("getnewaddress")
        h0 = rpc.call("getblockcount")
        sizes = []
        while rpc.call("getmempoolinfo")["size"] > 0:
            blk = rpc.call("generatetoaddress", 1, addr)[0]
            b = rpc.call("getblock", blk, 1)
            sizes.append((b["height"], len(b["tx"]), b["size"]))
        meta = {"start_height": h0, "blocks": sizes, "fixture_txs": len(txs)}
        rpc.call("stop")
        proc.wait(120)
    finally:
        if proc.poll() is None:
            proc.kill()
    json.dump(meta, open(os.path.join(a.out, "prep.json"), "w"), indent=1)
    print(json.dumps(meta))
    return meta


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dashd", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--workload", required=True,
                    choices=["startup-regtest", "startup-testnet", "mempool-accept", "gbt", "getblock", "prep-big", "ibd-testnet"])
    ap.add_argument("--datadir-src", required=True, help="datadir to clone (regtest fixture chain or a synced testnet datadir)")
    ap.add_argument("--fixture", default="/work/scratch/pf1/nbroot/data/fixtures/regtest-v1")
    ap.add_argument("--txs", type=int, default=5000)
    ap.add_argument("--clients", type=int, default=1)
    ap.add_argument("--mempool-fill", type=int, default=5000)
    ap.add_argument("--calls", type=int, default=5)
    ap.add_argument("--warmup-calls", type=int, default=2, help="unmeasured getblocktemplate calls before the measured ones")
    ap.add_argument("--verbosity", type=int, default=2)
    ap.add_argument("--h-lo", type=int, default=421000)
    ap.add_argument("--h-hi", type=int, default=440000)
    ap.add_argument("--par", type=int, default=1)
    ap.add_argument("--parbls", type=int, default=1)
    ap.add_argument("--dbcache", type=int, default=1024)
    ap.add_argument("--cpus", default="16-27")
    ap.add_argument("--period", type=int, default=6000011,
                    help="instructions:u sample period; must keep the sample rate under perf_event_max_sample_rate")
    ap.add_argument("--freq", type=int, default=997)
    ap.add_argument("--port", type=int, default=48800)
    ap.add_argument("--ibd-height", type=int, default=100000)
    ap.add_argument("--seed-dashd", default="/work/scratch/pf1/nbroot/bin/dashcore-24.0.0-rc.3/bin/dashd")
    ap.add_argument("--seed-cpus", default="28-31")
    ap.add_argument("--timeout", type=int, default=3600)
    ap.add_argument("--extra", nargs="*", default=[])
    ap.add_argument("--keep-datadir", action="store_true")
    a = ap.parse_args()
    lock = perf_lock()  # noqa: F841
    fn = {"startup-regtest": wl_startup, "startup-testnet": wl_startup, "mempool-accept": wl_mempool, "gbt": wl_gbt,
          "getblock": wl_getblock, "prep-big": wl_prep_big, "ibd-testnet": wl_ibd}[a.workload]
    meta = fn(a)
    print(json.dumps({k: v for k, v in meta.items() if k not in ("argv",)}, indent=1))
    return 1 if "error" in meta else 0


if __name__ == "__main__":
    sys.exit(main())
