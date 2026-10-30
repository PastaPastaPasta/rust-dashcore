#!/usr/bin/env python3
"""IBD instructions:u of rdashd and dashd at the same -par (CH3a a9, DEC-323).

Builds once, in <work>/source, a regtest chain whose blocks are mostly P2PKH signature checks,
with a source dashd and two wallets: `miner` mines 100 + 2 x FANOUT blocks, then pays FANOUT x 500
outputs of 1 DASH to `spender`; then each of BLOCKS blocks holds TXS `spender` payments of 15 DASH,
each spending INPUTS = 16 of those outputs, so every block checks TXS x 16 signatures. Each payment
names its inputs (`createrawtransaction`, then `signrawtransactionwithwallet`): the wallet's coin
selection over 40,000 outputs takes seconds a payment.

Then, with the source serving that chain on localhost, for each binary and each -par, a node on a
new datadir syncs it over P2P (`-connect` to the source only) and stops at its tip
(`-stopatheight`), under `perf stat -e instructions:u,task-clock`, REPS times; and REPS times
with `-stopatheight=1` (the start, one block and the stop: the baseline). Both binaries get the same
arguments, each at its defaults otherwise (regtest's consistency checks included):
`-regtest -listen=0 -dnsseed=0 -fixedseeds=0 -server=0 -blocksonly=1 -parbls=1 -par=<n>`.
`-blocksonly=1` is rdashd's mode until P5 (DEC-322); `-parbls=1` leaves dashd no idle BLS check
threads, which rdashd does not have yet (LQ3). A run must exit 0 with its tip at the height asked
and log its script-check thread count once, and every run at one -par, of either binary, must log
the same count. -stopatheight starts the shutdown, and blocks that arrive before ActivateBestChain
sees it still connect (validation.cpp:3830-3841), so a baseline may end a few blocks on, short of
the first block with a transaction; each run records its tip.

Then once more for each binary and -par, the split: the same sync under a whole-process
`perf stat -e instructions:u`, and, once the node has block 1 and its script-check threads have
their names, `perf stat --per-thread --no-inherit` on each of its threads. That perf attaches
while the node is stopped (SIGSTOP) with its events disabled, and the node continues once perf
acks `enable`; the node must not have started the first payment block then, so the per-thread
counts hold every payment's checks. A thread that starts after the attach is not counted; the
split names those it sees (it polls every 10 ms).

Usage:
  ch3a-par-ibd.py --dashd <dashd> --rdashd <rdashd> --work <dir> [--pars 1,16] [--reps 3]
                  [--fanout 80] [--blocks 40] [--txs 50] [--port 23500]

Writes <work>/results.json (each run's counters, the threads each binary logged, the splits, the
chain) and prints two tables; the IBD figure is the median run less the median baseline.
"""

import argparse
import base64
import hashlib
import json
import os
import re
import select
import shutil
import signal
import statistics
import subprocess
import sys
import time
import urllib.error
import urllib.request

PAY = 15  # DASH per payment
INPUTS = 16  # 1-DASH inputs per payment: PAY, the change and the fee

COMMON = ["-regtest", "-listen=0", "-dnsseed=0", "-fixedseeds=0", "-server=0", "-blocksonly=1",
          "-parbls=1", "-printtoconsole=0"]


def parse_args():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--dashd", required=True)
    p.add_argument("--rdashd", required=True)
    p.add_argument("--work", required=True)
    p.add_argument("--pars", default="1,16")
    p.add_argument("--reps", type=int, default=3)
    p.add_argument("--fanout", type=int, default=80)
    p.add_argument("--blocks", type=int, default=40)
    p.add_argument("--txs", type=int, default=50)
    p.add_argument("--port", type=int, default=23500)
    return p.parse_args()


class Rpc:
    def __init__(self, datadir, port, wallet=None):
        self.url = f"http://127.0.0.1:{port}/" + (f"wallet/{wallet}" if wallet else "")
        self.cookie = os.path.join(datadir, "regtest", ".cookie")

    def __call__(self, method, *params):
        auth = base64.b64encode(open(self.cookie, "rb").read().strip()).decode()
        body = json.dumps({"jsonrpc": "1.0", "id": 0, "method": method, "params": list(params)}).encode()
        req = urllib.request.Request(self.url, body, {"Authorization": f"Basic {auth}"})
        try:
            with urllib.request.urlopen(req, timeout=600) as r:
                out = json.load(r)
        except urllib.error.HTTPError as e:
            out = json.load(e)
        if out.get("error"):
            raise RuntimeError(f"{method}: {out['error']}")
        return out["result"]


class Source:
    """The source dashd, serving <work>/source on `port`; RPC on `port` + 1."""

    def __init__(self, a):
        self.datadir = os.path.join(a.work, "source")
        self.port = a.port
        os.makedirs(self.datadir, mode=0o700, exist_ok=True)
        self.proc = subprocess.Popen(
            [a.dashd, f"-datadir={self.datadir}", "-regtest", "-listen=1", f"-bind=127.0.0.1:{self.port}",
             f"-port={self.port}", f"-rpcport={self.port + 1}", "-rpcbind=127.0.0.1", "-rpcallowip=127.0.0.1",
             "-server=1", "-dnsseed=0", "-fixedseeds=0", "-discover=0", "-listenonion=0", "-upnp=0", "-natpmp=0",
             "-fallbackfee=0.0001", "-printtoconsole=0", "-whitelist=noban@127.0.0.1"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.rpc = Rpc(self.datadir, self.port + 1)
        for _ in range(600):
            if self.proc.poll() is not None:
                sys.exit(f"source: dashd exited with {self.proc.returncode}")
            try:
                self.rpc("getblockcount")
                return
            except Exception:
                time.sleep(0.5)
        self.stop()
        sys.exit("source: dashd did not start")

    def stop(self):
        try:
            self.rpc("stop")
        except Exception:
            self.proc.terminate()
        self.proc.wait(timeout=300)


def build(a, src):
    """The chain, unless <work>/chain.json says the source already holds it."""
    meta_path = os.path.join(a.work, "chain.json")
    if os.path.exists(meta_path):
        return json.load(open(meta_path))
    rpc = src.rpc
    for w in ("miner", "spender"):
        rpc("createwallet", w)
    miner, spender = Rpc(src.datadir, src.port + 1, "miner"), Rpc(src.datadir, src.port + 1, "spender")
    to = miner("getnewaddress")
    miner("generatetoaddress", 100 + 2 * a.fanout, to)
    for i in range(a.fanout):
        dests = {spender("getnewaddress"): 1 for _ in range(500)}
        miner("sendmany", "", dests)
        if i % 20 == 19:
            miner("generatetoaddress", 1, to)
    miner("generatetoaddress", 1, to)
    coins = [{"txid": u["txid"], "vout": u["vout"]} for u in spender("listunspent", 1) if u["amount"] == 1]
    if len(coins) < a.blocks * a.txs * INPUTS:
        sys.exit(f"build: {len(coins)} 1-DASH outputs for {a.blocks * a.txs * INPUTS} inputs")
    inputs = 0
    for _ in range(a.blocks):
        for _ in range(a.txs):
            spent, coins = coins[:INPUTS], coins[INPUTS:]
            outputs = {to: PAY, spender("getnewaddress"): round(INPUTS - PAY - 0.001, 8)}
            signed = spender("signrawtransactionwithwallet", spender("createrawtransaction", spent, outputs))
            assert signed["complete"], signed
            spender("sendrawtransaction", signed["hex"])
            inputs += INPUTS
        miner("generatetoaddress", 1, to)
        print(f"block {rpc('getblockcount')}: {inputs} inputs so far", flush=True)
    meta = {"height": rpc("getblockcount"), "tip": rpc("getbestblockhash"), "payment_inputs": inputs,
            "fanout": a.fanout, "blocks": a.blocks, "txs": a.txs}
    json.dump(meta, open(meta_path, "w"), indent=1)
    return meta


def perf_counters(path):
    """The counters of a `perf stat -x ,` file, by event."""
    out = {}
    for line in open(path):
        f = line.strip().split(",")
        try:
            out[f[2]] = float(f[0])
        except (IndexError, ValueError):
            continue
    return out


def sync(binary, par, stop_at, max_tip, src, run_dir):
    """One sync into a new datadir under perf, which must end at a tip from `stop_at` to `max_tip`:
    the counters, the exit code and the log's facts."""
    datadir = os.path.join(run_dir, "data")
    os.makedirs(datadir, mode=0o700)
    perf_out = os.path.join(run_dir, "perf.csv")
    cmd = ["perf", "stat", "-x", ",", "-e", "instructions:u,task-clock", "-o", perf_out, "--",
           binary, f"-datadir={datadir}", f"-connect=127.0.0.1:{src.port}", f"-par={par}",
           f"-stopatheight={stop_at}", *COMMON]
    start = time.monotonic()
    proc = subprocess.run(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, timeout=3600)
    wall = time.monotonic() - start
    counters = perf_counters(perf_out)
    log_path = os.path.join(datadir, "regtest", "debug.log")
    threads = [int(t) for t in re.findall(r"Script verification uses (\d+) additional threads",
                                          open(log_path, errors="replace").read())]
    run = {"exit": proc.returncode, "wall_s": round(wall, 3), "instructions_u": int(counters["instructions:u"]),
           "task_clock_ms": round(counters["task-clock"], 1), "tip": tip_of(log_path),
           "script_threads": threads, "stderr": proc.stderr.decode()[-500:]}
    if run["exit"] != 0 or not stop_at <= run["tip"] <= max_tip or len(threads) != 1:
        sys.exit(f"{binary} -par={par} -stopatheight={stop_at}: {run}")
    shutil.rmtree(datadir)
    return run


def tasks(pid):
    """Each thread of `pid` that still runs: its tid, its name and its state (none once it exits)."""
    out = {}
    try:
        tids = os.listdir(f"/proc/{pid}/task")
    except OSError:
        return out
    for tid in tids:
        try:
            name = open(f"/proc/{pid}/task/{tid}/comm").read().rstrip("\n")
            state = open(f"/proc/{pid}/task/{tid}/stat").read().rsplit(")", 1)[1].split()[0]
        except OSError:
            continue
        out[int(tid)] = (name, state)
    return out


def tip_of(log_path):
    """The highest `UpdateTip` in a debug.log (0 before the first, or without the file)."""
    try:
        log = open(log_path, errors="replace").read()
    except OSError:
        return 0
    return max((int(h) for h in re.findall(r"UpdateTip: new best=\w+ height=(\d+)", log)), default=0)


def split(binary, par, threads, src, run_dir, meta):
    """One sync with the whole process and each of its threads counted (the module's doc): every
    thread's instructions:u, and the run's. On any error the node and both perfs are killed."""
    datadir = os.path.join(run_dir, "data")
    os.makedirs(datadir, mode=0o700)
    log_path = os.path.join(datadir, "regtest", "debug.log")
    whole_out, threads_out = os.path.join(run_dir, "whole.csv"), os.path.join(run_dir, "threads.csv")
    whole = subprocess.Popen(
        ["perf", "stat", "-x", ",", "-e", "instructions:u", "-o", whole_out, "--", binary, f"-datadir={datadir}",
         f"-connect=127.0.0.1:{src.port}", f"-par={par}", f"-stopatheight={meta['height']}", *COMMON],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    pid = per_thread = None
    deadline = time.monotonic() + 120

    def wait_until(what, ready):
        while not ready():
            if whole.poll() is not None or time.monotonic() > deadline:
                sys.exit(f"{binary} -par={par} split: {what}")
            time.sleep(0.01)

    try:
        children = f"/proc/{whole.pid}/task/{whole.pid}/children"
        wait_until("no node started", lambda: open(children).read().split())
        pid = int(open(children).read().split()[0])
        wait_until(f"no block 1 and {threads} named script-check threads",
                   lambda: tip_of(log_path) >= 1
                   and sum("scriptch" in n for n, _ in tasks(pid).values()) >= threads)
        os.kill(pid, signal.SIGSTOP)
        wait_until("not stopped", lambda: all(state in "tT" for _, state in tasks(pid).values()))
        # Block N's UpdateTip precedes block N + 1's checks.
        attached_at, first_payment = tip_of(log_path), meta["height"] - meta["blocks"] + 1
        if attached_at + 1 >= first_payment:
            sys.exit(f"{binary} -par={par} split: at height {attached_at} on attaching, "
                     f"past {first_payment - 2}")
        attached = tasks(pid)
        ctl, ack = os.path.join(run_dir, "ctl"), os.path.join(run_dir, "ack")
        os.mkfifo(ctl, 0o600)
        os.mkfifo(ack, 0o600)
        with open(os.path.join(run_dir, "perf-threads.stderr"), "w") as err:
            per_thread = subprocess.Popen(
                ["perf", "stat", "--per-thread", "--no-inherit", "-x", ",", "-e", "instructions:u", "-D", "-1",
                 f"--control=fifo:{ctl},{ack}", "-p", str(pid), "-o", threads_out],
                stdout=subprocess.DEVNULL, stderr=err)
        ctl_fd, ack_fd = os.open(ctl, os.O_RDWR), os.open(ack, os.O_RDWR)
        os.write(ctl_fd, b"enable\n")
        # perf writes sizeof("ack\n"), its NUL included.
        if not select.select([ack_fd], [], [], 60)[0] or os.read(ack_fd, 16).rstrip(b"\0") != b"ack\n":
            sys.exit(f"{binary} -par={par} split: perf did not ack enable")
        os.close(ctl_fd)
        os.close(ack_fd)
        os.kill(pid, signal.SIGCONT)
        later = {}
        while whole.poll() is None:
            later.update((tid, name) for tid, (name, _) in tasks(pid).items() if tid not in attached)
            time.sleep(0.01)
        try:
            per_thread.wait(timeout=60)
        except subprocess.TimeoutExpired:
            per_thread.send_signal(signal.SIGINT)
            per_thread.wait(timeout=60)
    except BaseException:
        if pid is not None:
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        for proc in (per_thread, whole):
            if proc is not None and proc.poll() is None:
                proc.kill()
                proc.wait()
        raise
    counts = {}
    for line in open(threads_out):
        f = line.strip().split(",")
        if len(f) > 3 and f[3] == "instructions:u" and f[1].isdigit():
            counts[int(f[0].rsplit("-", 1)[1])] = int(f[1])
    total = int(perf_counters(whole_out)["instructions:u"])
    tip = tip_of(log_path)
    if whole.returncode != 0 or tip != meta["height"]:
        sys.exit(f"{binary} -par={par} split: exit {whole.returncode}, tip {tip}")
    per = [{"tid": tid, "name": name, "instructions_u": counts.get(tid, 0)} for tid, (name, _) in attached.items()]
    workers = [t["instructions_u"] for t in per if "scriptch" in t["name"]]
    shutil.rmtree(datadir)
    return {"attached_at": attached_at, "total_instructions_u": total, "threads": per,
            "started_after_attach": [{"tid": tid, "name": name} for tid, name in later.items()],
            "counted_instructions_u": sum(t["instructions_u"] for t in per),
            "scriptch_instructions_u": sum(workers), "scriptch_min": min(workers, default=0),
            "scriptch_max": max(workers, default=0)}


def sha256(path):
    with open(path, "rb") as f:
        return hashlib.sha256(f.read()).hexdigest()


def main():
    a = parse_args()
    os.makedirs(a.work, mode=0o700, exist_ok=True)
    src = Source(a)
    results = {"dashd": a.dashd, "dashd_sha256": sha256(a.dashd), "rdashd": a.rdashd,
               "rdashd_sha256": sha256(a.rdashd), "args": COMMON, "reps": a.reps, "runs": {}, "splits": {}}
    try:
        meta = build(a, src)
        results["chain"] = meta
        # build's first block with a transaction: after 100 + 2 x FANOUT coinbase-only blocks.
        first_tx = 100 + 2 * meta["fanout"] + 1
        results["date"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        results["load_start"] = open("/proc/loadavg").read().split()[:3]
        for par in [int(p) for p in a.pars.split(",")]:
            logged = set()
            for name, binary in (("dashd", a.dashd), ("rdashd", a.rdashd)):
                key = f"{name} -par={par}"
                runs = []
                for i, stop_at in enumerate([1] * a.reps + [meta["height"]] * a.reps):
                    run_dir = os.path.join(a.work, "runs", f"{name}-par{par}-{i}")
                    shutil.rmtree(run_dir, ignore_errors=True)
                    os.makedirs(run_dir, mode=0o700)
                    max_tip = stop_at if stop_at == meta["height"] else first_tx - 1
                    runs.append(sync(binary, par, stop_at, max_tip, src, run_dir))
                    logged.add(tuple(runs[-1]["script_threads"]))
                    print(f"{key} stop {stop_at}: {runs[-1]['instructions_u']:,} instructions:u, "
                          f"{runs[-1]['wall_s']} s", flush=True)
                base, full = runs[:a.reps], runs[a.reps:]
                median = statistics.median(r["instructions_u"] for r in full)
                base_median = statistics.median(r["instructions_u"] for r in base)
                results["runs"][key] = {"baselines": base, "full": full, "median_instructions_u": median,
                                        "median_baseline_instructions_u": base_median,
                                        "ibd_instructions_u": median - base_median}
            if len(logged) != 1:
                sys.exit(f"-par={par}: the runs logged different script-check threads: {sorted(logged)}")
            (threads,) = logged.pop()
            for name, binary in (("dashd", a.dashd), ("rdashd", a.rdashd)):
                run_dir = os.path.join(a.work, "runs", f"{name}-par{par}-split")
                shutil.rmtree(run_dir, ignore_errors=True)
                os.makedirs(run_dir, mode=0o700)
                results["splits"][f"{name} -par={par}"] = split(binary, par, threads, src, run_dir, meta)
        results["load_end"] = open("/proc/loadavg").read().split()[:3]
    finally:
        src.stop()
    json.dump(results, open(os.path.join(a.work, "results.json"), "w"), indent=1)
    print(f"\nchain: {meta['height']} blocks, {meta['payment_inputs']} payment inputs")
    print(f"{'run':<18} {'IBD instructions:u':>20} {'median total':>16} {'baseline':>14} {'threads':>8}")
    for key, r in results["runs"].items():
        print(f"{key:<18} {r['ibd_instructions_u']:>20,.0f} {r['median_instructions_u']:>16,.0f} "
              f"{r['median_baseline_instructions_u']:>14,.0f} {str(r['full'][0]['script_threads']):>8}")
    print(f"\n{'split':<18} {'run':>16} {'counted':>16} {'top other thread':>32} {'scriptch':>16} "
          f"{'min':>14} {'max':>14} {'from':>5}")
    for key, r in results["splits"].items():
        other = max((t for t in r["threads"] if "scriptch" not in t["name"]), key=lambda t: t["instructions_u"])
        print(f"{key:<18} {r['total_instructions_u']:>16,} {r['counted_instructions_u']:>16,} "
              f"{other['name'] + ' ' + format(other['instructions_u'], ','):>32} "
              f"{r['scriptch_instructions_u']:>16,} {r['scriptch_min']:>14,} {r['scriptch_max']:>14,} "
              f"{r['attached_at']:>5}")
        if r["started_after_attach"]:
            print(f"  started after the attach: {r['started_after_attach']}")


if __name__ == "__main__":
    main()
