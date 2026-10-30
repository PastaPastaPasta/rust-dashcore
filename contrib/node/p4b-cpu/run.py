#!/usr/bin/env python3
"""P4b's 125-peer CPU bar, the counter half (P4b notes, "The 125-peer CPU bar"; D14).

Starts regtest dashd v24.0.0-rc.3 (sha256-checked, `P2A_DASHD`) with `-maxconnections=136`, so
125 inbound peers fit with no eviction, and no outbound peers (`-connect=0 -listen=1`). The node
mines BLOCKS blocks at the frozen mock time, so it is out of IBD. Then 125 scripted peers connect
from 127.0.0.1 and complete the handshake (the first barrier). `perf stat -e
instructions:u,task-clock -p PID` attaches to the daemon, and every peer then sends one fixed
workload:

  getheaders from genesis; getdata MSG_BLOCK of the last 100 blocks in batches of 16;
  getblocks from genesis and from block 500 (500 invs each); filterload holding the last 50
  coinbase txids, then one getdata of 50 MSG_FILTERED_BLOCK; getblocktxn of index 0 of the
  last 10 blocks, twice; 10 pings

It ends with a ping of its own nonce and reads replies until that pong arrives. A peer's
messages are processed in order, so the pong means the peer was fully served. When every peer
is done (the second barrier), perf stops. The workload is a fixed message count, not a
duration, so `instructions:u` compares two daemons on identical work. task-clock is recorded
but not judged, because of box load.

The bar is instructions:u(rdashd) <= instructions:u(dashd). rdashd's row (BL-327) runs the same
workload: rdashd has no mining RPC, so a dashd mines the same chain at the same mock time, and
rdashd, started with dashd's arguments (`-maxconnections=136 -connect=0`, listening on
`-bind`), takes its blocks by `submitblock` before any peer connects. (Syncing over P2P and
restarting would lose the blocks: a clean rdashd restart comes back at its last flush, BL-271.)
Its mock time is dashd's, so it is out of IBD too.
Peers dial from 127.0.0.1, not from distinct /16s: with 125 free inbound slots nothing is
evicted, so netgroups do not matter here.

`--window SECONDS` (BL-327) is the timed half, rate-driven: peer n starts a workload every
`P4B_CPU_PERIOD` seconds (default 25, so 5 workloads a second from 125 peers), offset by
n/125 of a period, until SECONDS have passed; a workload still running at its next start delays
that start (counted as `late`). perf covers the window; the result adds each workload's latency
(its first message to its final pong) in milliseconds, their percentiles, and the box's load
average at the start and the end. The comparison is CPU-seconds (task-clock) and the latency
distributions (a KS test belongs to O13b).

Equal work is checked, not assumed (BL-327 D18): each run records the chain's tip, a digest of
every peer's requests, and per command the reply count, the payload bytes and a canonical
payload digest (SHA-256 over the sorted SHA-256s of the payloads, so the order in which peers
are served does not matter). The node's own `ping`s carry random nonces and are counted but
not digested. `--compare A.json B.json` exits non-zero unless the two results' first runs agree
on all of these.

Usage:
  run.py [--rdashd BIN] [--window SECONDS] OUT_JSON [RUNS]
  run.py --compare A_JSON B_JSON

`P2A_CAPTURE_DIR` defaults to /work/scratch/p4b/cpu, `P2A_PORT_BASE` to 21800 (below Linux's
ephemeral range, which a client socket elsewhere on the box can take a port from),
`P4B_CPU_BLOCKS` to 1000 and `P4B_CPU_PEERS` to 125. It never touches /work/chains or
/work/bench.
"""

import asyncio
import collections
import hashlib
import importlib.util
import json
import os
import pathlib
import shutil
import signal
import statistics
import struct
import subprocess
import sys
import time

os.environ.setdefault("P2A_CAPTURE_DIR", "/work/scratch/p4b/cpu")
os.environ.setdefault("P2A_PORT_BASE", "21800")

_spec = importlib.util.spec_from_file_location(
    "p4b_capture", pathlib.Path(__file__).resolve().parent.parent / "p4b-transcripts" / "capture.py")
cap = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(cap)
p2a, p3 = cap.p2a, cap.p3

BLOCKS = int(os.environ.get("P4B_CPU_BLOCKS", "1000"))
PEERS = int(os.environ.get("P4B_CPU_PEERS", "125"))
PERIOD = float(os.environ.get("P4B_CPU_PERIOD", "25"))
DONE_NONCE = 0xD0E5_0000_0000_0000  # + peer: the end-of-workload ping


def workload(chain, n):
    """Peer `n`'s messages after the handshake, in order."""
    h, tip = chain.hashes, len(chain.hashes) - 1
    msgs = [("getheaders", cap.getblocks([h[0]]))]  # the same locator layout
    last = [h[i] for i in range(tip - 99, tip + 1)]
    msgs += [("getdata", cap.getdata([(cap.MSG_BLOCK, x) for x in last[i:i + 16]]))
             for i in range(0, 100, 16)]
    msgs += [("getblocks", cap.getblocks([h[0]])), ("getblocks", cap.getblocks([h[500]]))]
    msgs.append(("filterload", cap.filterload(1000, 10, chain.coinbases)))
    msgs.append(("getdata", cap.getdata([(cap.MSG_FILTERED_BLOCK, h[i])
                                         for i in range(tip - 49, tip + 1)])))
    msgs += [("getblocktxn", cap.getblocktxn(h[i], [0]))
             for _ in range(2) for i in range(tip - 9, tip + 1)]
    msgs += [("ping", struct.pack("<Q", (n << 8) + k)) for k in range(10)]
    msgs.append(("ping", struct.pack("<Q", DONE_NONCE + n)))
    return msgs


async def read_msg(reader):
    head = await reader.readexactly(24)
    size, = struct.unpack_from("<I", head, 16)
    return head[4:16].rstrip(b"\0").decode(), await reader.readexactly(size)


class Replies:
    """One peer's replies after the handshake, by command: count, payload bytes and the
    payloads' SHA-256s (none for the node's `ping`s, whose nonces are random)."""

    def __init__(self):
        self.count = collections.Counter()
        self.bytes = collections.Counter()
        self.digests = collections.defaultdict(list)

    def add(self, cmd, payload):
        self.count[cmd] += 1
        self.bytes[cmd] += len(payload)
        if cmd != "ping":
            self.digests[cmd].append(hashlib.sha256(payload).digest())


def reply_summary(replies):
    """Every peer's replies together, independent of the order peers were served in."""
    count, size, digests = collections.Counter(), collections.Counter(), collections.defaultdict(list)
    for r in replies:
        count.update(r.count)
        size.update(r.bytes)
        for cmd, ds in r.digests.items():
            digests[cmd] += ds
    return {"replies": sum(count.values()), "replies_by_command": dict(sorted(count.items())),
            "reply_bytes_by_command": dict(sorted(size.items())),
            "reply_digest_by_command": {cmd: hashlib.sha256(b"".join(sorted(ds))).hexdigest()
                                        for cmd, ds in sorted(digests.items())}}


def request_digest(workloads):
    """SHA-256 over every peer's workload, in peer order: the requests both daemons answer."""
    h = hashlib.sha256()
    for msgs in workloads:
        for cmd, payload in msgs:
            h.update(p2a.frame(cmd, payload))
    return h.hexdigest()


async def peer(n, port, msgs, connected, go, stats, until=None):
    """Peer `n`: the handshake, then one workload, or with `until` one every PERIOD seconds
    (offset n/PEERS of a period) until `until`. `stats[n]` is (latencies in ms, late,
    `Replies`)."""
    reader, writer = await asyncio.open_connection("127.0.0.1", port)
    writer.write(p2a.frame("version", p2a.version_payload(p2a.FULL, p2a.VERSION_NONCE + n)))
    got_version = got_verack = False
    while not (got_version and got_verack):
        cmd, payload = await read_msg(reader)
        if cmd == "version":
            got_version = True
            writer.write(p2a.frame("verack", b""))
        got_verack |= cmd == "verack"
    await writer.drain()
    connected.release()
    await go.wait()
    done = struct.pack("<Q", DONE_NONCE + n)
    late = 0
    latencies = []
    replies = Replies()
    next_start = time.time() + PERIOD * n / PEERS
    while True:
        if until is not None:
            if next_start >= until:
                break
            wait = next_start - time.time()
            if wait > 0:
                await asyncio.sleep(wait)
            else:
                late += 1
            next_start += PERIOD
        sent = time.monotonic()
        for cmd, payload in msgs:
            writer.write(p2a.frame(cmd, payload))
        await writer.drain()
        while True:
            cmd, payload = await read_msg(reader)
            replies.add(cmd, payload)
            if cmd == "ping":
                writer.write(p2a.frame("pong", payload))
            elif cmd == "pong" and payload == done:
                break
        latencies.append((time.monotonic() - sent) * 1000)
        if until is None:
            break
    stats[n] = (latencies, late, replies)
    writer.close()


def process_cpu_s(pid):
    """The process's user and system CPU time, every thread's (`/proc/PID/stat` utime + stime):
    a check on perf's thread coverage."""
    fields = pathlib.Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    return (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")


async def drive(node, chain, window=None):
    connected = asyncio.Semaphore(0)
    go = asyncio.Event()
    stats = {}
    # Peers start their schedule at `go`, a second after perf attaches.
    until = None if window is None else time.time() + 1.0 + window
    workloads = [workload(chain, n) for n in range(1, PEERS + 1)]
    tasks = [asyncio.create_task(peer(n, node.port, msgs, connected, go, stats, until))
             for n, msgs in enumerate(workloads, 1)]
    for _ in range(PEERS):
        await asyncio.wait_for(connected.acquire(), 600)
    out = node.dir / "perf.csv"
    perf = subprocess.Popen(["perf", "stat", "-x,", "-e", "instructions:u,task-clock",
                             "-p", str(node.proc.pid), "-o", str(out)])
    await asyncio.sleep(1.0)  # perf attached
    start, load_start = time.time(), os.getloadavg()
    cpu_start = process_cpu_s(node.proc.pid)
    go.set()
    await asyncio.wait_for(asyncio.gather(*tasks), 3600 + (window or 0))
    elapsed = time.time() - start
    cpu_s = process_cpu_s(node.proc.pid) - cpu_start
    perf.send_signal(signal.SIGINT)
    perf.wait(timeout=60)
    counters = {}
    for line in out.read_text().splitlines():
        f = line.split(",")
        if len(f) > 2 and f[2] in ("instructions:u", "task-clock"):
            counters[f[2]] = float(f[0])
    result = {"instructions_u": counters["instructions:u"], "task_clock_ms": counters["task-clock"],
              "elapsed_s": round(elapsed, 3), "process_cpu_s": round(cpu_s, 2),
              "tip": chain.hashes[-1][::-1].hex(), "height": len(chain.hashes) - 1,
              "request_digest": request_digest(workloads),
              **reply_summary(r for *_, r in stats.values())}
    if window is not None:
        lat = sorted(x for ls, _, _ in stats.values() for x in ls)
        pct = {f"p{q}": round(lat[min(len(lat) - 1, len(lat) * q // 100)], 3)
               for q in (50, 90, 99)}
        result.update(window_s=window, period_s=PERIOD, workloads=len(lat),
                      late=sum(k for _, k, _ in stats.values()), latency_ms=pct | {"max": round(lat[-1], 3)},
                      latencies_ms=[round(x, 3) for x in lat],
                      load_start=load_start, load_end=os.getloadavg())
    return result


# rdashd refuses to connect a block under DIP0003, V20, MN_RR, V24 or EVO_SHARES until CH7a
# (dashd-chainstate `connect/guard.rs`), and regtest activates DIP0003 at 432; both daemons'
# chain keeps them off (BL-327 D9). P4b's first dashd baseline ran on regtest's defaults.
NO_EVO_ARGS = ["-dip3params=100000:100000", "-testactivationheight=v20@100000",
               "-testactivationheight=mn_rr@100000", "-vbparams=v24:-2:0", "-vbparams=evo_shares:-2:0"]
SERVE_ARGS = ["-maxconnections=136", "-connect=0", "-debug=0", *NO_EVO_ARGS]


def mined_chain(node):
    node.mine(BLOCKS)
    chain = cap.Chain(node)
    chain.coinbases = [chain.coinbase_txid(i) for i in range(BLOCKS - 49, BLOCKS + 1)]
    return chain


def run_dashd(index, window):
    node = p2a.Node(f"cpu-dashd-{index}", index, p2a.SIM_START, SERVE_ARGS)
    try:
        return asyncio.run(drive(node, mined_chain(node), window))
    finally:
        node.stop()


class Rdashd(p2a.Node):
    """rdashd on a fresh datadir with the arguments p2a gives dashd (`p2a.Node`), which rdashd
    takes since BL-327, and dashd's default `-blocksonly=0`: rdashd's is 1 until P5 (DEC-322),
    which would leave out its `senddsq` and turn its `version`'s relay off (BL-327 D27). RPC and
    stop are p2a's. rdashd has no `getblockcount` yet (RP1a), so
    readiness reads `getblockhash`."""

    def __init__(self, binary, name, index, extra):
        self.name = name
        self.dir = p2a.ROOT / name
        shutil.rmtree(self.dir, ignore_errors=True)
        self.dir.mkdir(parents=True, mode=0o700)
        self.port = p2a.PORT_BASE + 2 * index
        self.rpc_port = p2a.PORT_BASE + 2 * index + 1
        self.args = [binary, f"-datadir={self.dir}", "-regtest", "-server", "-listen=1",
                     "-listenonion=0", "-discover=0", "-dnsseed=0", "-fixedseeds=0", "-upnp=0",
                     "-natpmp=0", "-blocksonly=0", "-debug=net", "-logips", "-printtoconsole=0",
                     f"-port={self.port}", f"-bind=127.0.0.1:{self.port}", f"-rpcport={self.rpc_port}",
                     "-rpcbind=127.0.0.1", "-rpcallowip=127.0.0.1", "-rpcuser=p2a",
                     "-rpcpassword=p2a", f"-mocktime={p2a.SIM_START}", *extra]
        self.stderr = open(self.dir / "stderr.txt", "ab")
        self.proc = subprocess.Popen(self.args, stdout=subprocess.DEVNULL, stderr=self.stderr)
        end = time.time() + 900
        while True:
            try:
                self.rpc("getblockhash", 0)
                return
            except Exception:
                if self.proc.poll() is not None or time.time() > end:
                    raise RuntimeError(f"{name}: rdashd did not start; see {self.dir}/stderr.txt")
                time.sleep(0.5)


def run_rdashd(binary, index, window):
    """A dashd mines the chain and stops; rdashd, with dashd's serving arguments, takes the
    blocks by `submitblock` and serves the workload."""
    src = p2a.Node(f"cpu-src-{index}", index, p2a.SIM_START, ["-connect=0", "-debug=0", *NO_EVO_ARGS])
    try:
        chain = mined_chain(src)
        blocks = [src.rpc("getblock", h[::-1].hex(), 0) for h in chain.hashes[1:]]
    finally:
        src.stop()
    node = Rdashd(binary, f"cpu-rdashd-{index}", index + 1, SERVE_ARGS)
    try:
        for raw in blocks:
            if (verdict := node.rpc("submitblock", raw)) is not None:
                raise RuntimeError(f"{node.name}: submitblock {verdict}; see {node.dir}")
        assert node.rpc("getblockhash", BLOCKS) == chain.hashes[BLOCKS][::-1].hex(), "another tip"
        return asyncio.run(drive(node, chain, window))
    finally:
        node.stop()


EQUAL_WORK = ("tip", "height", "request_digest", "replies_by_command", "reply_bytes_by_command",
              "reply_digest_by_command")


def compare(a_path, b_path):
    """Whether two results' first runs did the same work; prints each field that differs."""
    a, b = (json.loads(pathlib.Path(x).read_text())["runs"][0] for x in (a_path, b_path))
    differ = [k for k in EQUAL_WORK if a[k] != b[k]]
    for k in differ:
        print(f"{k}:\n  {a_path}: {a[k]}\n  {b_path}: {b[k]}")
    print("same work" if not differ else f"{len(differ)} field(s) differ")
    return not differ


def main():
    argv = sys.argv[1:]
    if argv[:1] == ["--compare"] and len(argv) == 3:
        sys.exit(0 if compare(argv[1], argv[2]) else 1)
    opts = {}
    while argv and argv[0] in ("--rdashd", "--window"):
        opts[argv[0]] = argv[1]
        argv = argv[2:]
    if not argv:
        sys.exit(__doc__)
    if p3.dashd_sha256() != p3.DASHD_SHA256:
        sys.exit(f"dashd is not v24.0.0-rc.3 ({p3.DASHD_SHA256})")
    window = float(opts["--window"]) if "--window" in opts else None
    rdashd = opts.get("--rdashd")
    count = int(argv[1]) if len(argv) > 1 else 1
    if rdashd:
        run = lambda i: run_rdashd(rdashd, 2 * i, window)
        daemon = {"daemon": "rdashd", "binary": rdashd, "sha256": hashlib.sha256(pathlib.Path(rdashd).read_bytes()).hexdigest()}
    else:
        run = lambda i: run_dashd(i, window)
        daemon = {"daemon": "dashd v24.0.0-rc.3", "sha256": p3.DASHD_SHA256}
    runs = []
    for i in range(count):
        runs.append(run(i))
        print(json.dumps(runs[-1]), flush=True)
    result = {**daemon, "peers": PEERS, "blocks": BLOCKS, "runs": runs,
              "median_instructions_u": statistics.median(r["instructions_u"] for r in runs)}
    pathlib.Path(argv[0]).write_text(json.dumps(result, indent=1) + "\n")


if __name__ == "__main__":
    main()
