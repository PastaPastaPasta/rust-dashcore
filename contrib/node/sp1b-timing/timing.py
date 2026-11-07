#!/usr/bin/env python3
"""SP1b: the masternode-sync tick's timers on the wire, dashd v24.0.0-rc.4 against rdashd
(PRD NFR-FP-03, behaviour 8; SP1-notes D18-D22).

Each session starts a fresh regtest node with its own datadir, real time (no mock time), Core's
`-disablegovernance=1` (rdashd's default until GV1b, DEC-510) and `-blocksonly=0`, and scripts V1
peers against it. A sample is an interval between two moments: an RPC this tool made
returning, a message or a close arriving on one of its sockets, or (the `_tick` measures) the
time on the node's own Core log line for the tick that decided a disconnect. Each session gives
at most one sample of each measure, so the samples of a measure are independent (D19).

Measures (D18), each gated (SP1's tick: the verdict) or reported (another task's path):
  first6      gated: `submitblock` of a fresh height-1 block -> the node's `mempool` to its
              outbound peer, with four peers (the BLOCKCHAIN asset's 6 s timeout,
              `snap.Nodes().size() > 3`)
  reset_tick  gated: `mnsync reset` after FINISHED -> the node's `Sync Tick -- disconnecting from
              recently synced peer` line for its outbound peer, which has `full-sync` (the next
              passing tick: the 6 s gate's phase)
  gap_tick    gated: that line -> the same line for the peer reconnected at once (one tick
              period)
  first30     gated: as first6 with the outbound peer alone (the 30 s timeout)
  reset, gap  reported: reset_tick and gap_tick as the peer sees them, at the socket's close.
              They add the P2P layer's disconnect path (Core's linger and socket loop,
              net.cpp:2115-2146; P2a's sweep), not the tick's (D20)
  submit6, submit30
              reported: the `submitblock` call's duration in each session kind, the node's own
              processing inside the first6 and first30 references (D21)
  govsync     blocked: the gap between successive `govsync`s (one peer per tick): BL-450, not
              sampled until GV1b lets rdashd run governance (DEC-562); `MEASURES` holds its slot

Sessions `f6` give first6, the reset and gap measures; sessions `s30` give first30. Before `submitblock` and
before `mnsync reset` the session waits a uniform 0-6 s from its own seeded generator, so the
reference moment's phase against the node's 1 s scheduler and 6 s gate is uniform whatever the
node's start-up time (D19).

Usage:
  timing.py run OUT_DIR [--samples N] [--jobs P] [--kinds f6,s30] [--seed S] [--max-load L]
  timing.py ks OUT_DIR [--samples N] [--resolution SECONDS]
  timing.py selftest

The nightly job is `nightly.sh` beside this file: a cores cap and nice 10 around `run` and `ks`.

`run` appends each session's samples to OUT_DIR/samples.tsv and its failures to errors.tsv,
and schedules sessions until each node has N good sessions of each kind (default 10000); it
resumes from what samples.tsv holds. It alternates the nodes and kinds so that both nodes run
under the same load. Each sample row also carries its session's dispatch time and the 1-minute
load then; with --max-load (default SP1B_MAX_LOAD) no session starts while the load is at or
above it, and each pause is logged (D22). `ks` writes OUT_DIR/ks.tsv, the two-sample Kolmogorov-Smirnov table for
every measure, and exits 1 unless every gated measure has N samples per node and p > 0.05 (at
10 ms resolution, permutation p; D21) and no (node, kind) failed more than 1% of its sessions.

env:
  SP1B_DASHD      dashd v24.0.0-rc.4 (default: setup-dashd's cache in the home directory)
  SP1B_RDASHD     rdashd, a release build (required for `run`)
  SP1B_MAX_LOAD   `run`'s --max-load default (unset: no load ceiling)
  SP1B_PORT_BASE  the first port (default 23000); job slot k uses base+3k .. base+3k+2, all
                  below the ephemeral range
  TMPDIR          the sessions' datadirs, each removed when its session ends well
"""

import argparse
import base64
import calendar
import collections
import importlib.util
import itertools
import json
import math
import os
import pathlib
import queue
import random
import resource
import select
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request

_p2a = pathlib.Path(__file__).resolve().parent.parent / "p2a-transcripts" / "capture.py"
_spec = importlib.util.spec_from_file_location("p2a_capture", _p2a)
p2a = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(p2a)

CORE_VERSION = "v24.0.0-rc.4"
# SP1B_DIAG=1: the nodes log mnsync and net with microsecond times, every datadir is kept, and
# diag.tsv gets each session's events at the wall clock, to line them up with the logs.
DIAG = os.environ.get("SP1B_DIAG") == "1"
PORT_BASE = int(os.environ.get("SP1B_PORT_BASE", "23000"))
MAX_JOBS = (32768 - PORT_BASE) // 3
# A session's longest wait for one event: the 30 s timeout plus two tick periods and the
# reference wait, with room for a loaded box.
EVENT_TIMEOUT = 90.0
# A block older than this is refreshed: well inside the 6 h `nMaxTipAge` that keeps a node
# with it as its tip out of IBD.
BLOCK_MAX_AGE = 1800

# measure: (session kind, "gate" | "report" | "blocked", note). The order is the KS table's.
CLOSE_PATH = "the P2P disconnect path, not the tick (D20; P2a's)"
MEASURES = {
    "first6": ("f6", "gate", ""),
    "reset_tick": ("f6", "gate", ""),
    "gap_tick": ("f6", "gate", ""),
    "first30": ("s30", "gate", ""),
    "reset": ("f6", "report", CLOSE_PATH),
    "gap": ("f6", "report", CLOSE_PATH),
    "submit6": ("f6", "report", "submitblock's duration: the node's processing in first6 (D21)"),
    "submit30": ("s30", "report", "submitblock's duration: the node's processing in first30 (D21)"),
    # BL-450: rdashd refuses -disablegovernance=0 until GV1b. Sample it from a session kind
    # with governance on (`govsync` to one more peer each tick, node/sync_manager.cpp:262-271)
    # once both nodes run it, and add the kind to KINDS.
    "govsync": ("gov", "blocked", "BL-450 (GV1b): rdashd runs governance off until GV1b (DEC-510)"),
}
# Core's line for each peer the tick disconnects (node/sync_manager.cpp:198-201); both nodes
# write it unconditionally.
DISCONNECT_LINE = "Sync Tick -- disconnecting from recently synced peer="
KINDS = ("f6", "s30")
NODES = ("dashd", "rdashd")

# Services the scripted peers advertise (NODE_NETWORK | NODE_BLOOM | NODE_NETWORK_LIMITED).
SERVICES = p2a.FULL
UA = b"/sp1b-timing:1/"  # DEC-83: the tool says who it is


def default_dashd():
    version = CORE_VERSION.lstrip("v")
    return str(pathlib.Path.home() / f".rust-dashcore-test/dashcore-{version}/bin/dashd")


# ---- the KS test

def ks_d(a, b):
    """The two-sample Kolmogorov-Smirnov statistic: the largest gap between the two empirical
    CDFs, taken after every tied value. `a` and `b` are sorted."""
    n, m = len(a), len(b)
    i = j = 0
    d = 0.0
    while i < n and j < m:
        x = min(a[i], b[j])
        while i < n and a[i] == x:
            i += 1
        while j < m and b[j] == x:
            j += 1
        d = max(d, abs(i / n - j / m))
    return d


def ks_2samp(a, b):
    """D and its asymptotic p-value, with Stephens' small-sample correction (Numerical Recipes'
    `kstwo`/`probks`). For continuous samples; on tied ones the p-value is conservative."""
    a, b = sorted(a), sorted(b)
    d = ks_d(a, b)
    en = math.sqrt(len(a) * len(b) / (len(a) + len(b)))
    return d, kolmogorov_q((en + 0.12 + 0.11 / en) * d)


def ks_permutation(a, b, rounds, rng):
    """D and its permutation p-value, (1 + #{D* >= D}) / (1 + rounds) over `rounds` relabellings
    of the pooled samples: exact in distribution for tied samples too (D21)."""
    d = ks_d(sorted(a), sorted(b))
    pooled = list(a) + list(b)
    hits = 0
    for _ in range(rounds):
        rng.shuffle(pooled)
        if ks_d(sorted(pooled[:len(a)]), sorted(pooled[len(a):])) >= d - 1e-12:
            hits += 1
    return d, (1 + hits) / (1 + rounds)


def kolmogorov_q(lam):
    """Q_KS(lambda) = 2 sum_{k>=1} (-1)^(k-1) exp(-2 k^2 lambda^2). Below 0.2 it is 1 to double
    precision, and the series converges too slowly there to sum."""
    if lam < 0.2:
        return 1.0
    total, sign = 0.0, 1.0
    for k in range(1, 101):
        term = sign * math.exp(-2.0 * k * k * lam * lam)
        total += term
        if abs(term) < 1e-12 * max(total, 1e-300):
            break
        sign = -sign
    return min(max(2.0 * total, 0.0), 1.0)


def quantiles(xs, qs=(0.01, 0.25, 0.5, 0.75, 0.99)):
    s = sorted(xs)
    return [s[min(len(s) - 1, int(q * len(s)))] for q in qs]


def fmt(xs):
    """The quantiles of `xs` in seconds, joined by slashes, for the KS table."""
    return "/".join(f"{q:.3f}" for q in quantiles(xs))


# ---- a node

class RpcError(RuntimeError):
    pass


def log_time(line):
    """A `-logtimemicros` log line's time, `2026-10-27T16:33:44.620792Z`, in Unix seconds."""
    stamp = line.split(" ", 1)[0]
    secs = calendar.timegm(time.strptime(stamp[:19], "%Y-%m-%dT%H:%M:%S"))
    return secs + int(stamp[20:26]) / 1e6


class Node:
    """A fresh regtest node, dashd or rdashd, on job slot `slot`'s ports."""

    def __init__(self, kind, binary, slot, root):
        self.kind = kind
        self.dir = pathlib.Path(tempfile.mkdtemp(prefix=f"{kind}-", dir=root))
        os.chmod(self.dir, 0o700)
        self.port = PORT_BASE + 3 * slot
        self.rpc_port = self.port + 1
        # Each node's own password: an earlier node still on the slot's ports cannot answer.
        self.password = os.urandom(8).hex()
        args = [binary, "-regtest", f"-datadir={self.dir}", "-listen=1",
                f"-bind=127.0.0.1:{self.port}", f"-port={self.port}", "-listenonion=0",
                "-discover=0", "-dnsseed=0", "-fixedseeds=0", "-printtoconsole=0",
                "-v2transport=0", "-blocksonly=0", "-disablegovernance=1", "-logtimemicros",
                f"-rpcport={self.rpc_port}", "-rpcbind=127.0.0.1", "-rpcallowip=127.0.0.1",
                "-rpcuser=sp1b", f"-rpcpassword={self.password}"]
        if kind == "dashd":
            # rdashd has no wallet; dashd's would only slow its start.
            args.append("-disablewallet")
        if DIAG:
            args += ["-debug=mnsync", "-debug=net"]
        self.stderr = open(self.dir / "stderr.txt", "wb")
        self.proc = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=self.stderr)
        end = time.monotonic() + 120
        while True:
            try:
                self.rpc("getblockcount")
                break
            except (OSError, RpcError):
                if self.proc.poll() is not None or time.monotonic() > end:
                    self.proc.kill()
                    self.proc.wait()
                    self.stderr.close()
                    raise RuntimeError(f"{kind} did not start (see {self.dir}/stderr.txt)")
                time.sleep(0.1)

    def rpc(self, method, *params):
        req = urllib.request.Request(
            f"http://127.0.0.1:{self.rpc_port}/",
            data=json.dumps({"jsonrpc": "1.0", "id": 0, "method": method,
                             "params": list(params)}).encode(),
            headers={"Authorization": "Basic "
                     + base64.b64encode(f"sp1b:{self.password}".encode()).decode()})
        try:
            with urllib.request.urlopen(req, timeout=60) as r:
                reply = json.load(r)
        except urllib.error.HTTPError as e:
            try:
                reply = json.load(e)
            except ValueError:
                raise RpcError(f"{method}: HTTP {e.code}") from None
        if reply.get("error"):
            raise RpcError(f"{method}{params}: {reply['error']}")
        return reply["result"]

    def tick_disconnects(self, want, timeout=10.0):
        """The times (wall clock, from `-logtimemicros`) of the ticks that disconnected a
        recently synced peer: Core logs the line once per peer, so lines within a second are one
        tick. Waits until the log holds `want` of them."""
        log = self.dir / "regtest" / "debug.log"
        end = time.monotonic() + timeout
        while True:
            ticks = []
            for line in open(log, errors="replace"):
                if DISCONNECT_LINE in line:
                    t = log_time(line)
                    if not ticks or t - ticks[-1] > 1.0:
                        ticks.append(t)
            if len(ticks) >= want or time.monotonic() > end:
                return ticks
            time.sleep(0.05)

    def stop(self, keep):
        try:
            self.rpc("stop")
            self.proc.wait(timeout=60)
        except (OSError, RpcError, subprocess.TimeoutExpired):
            self.proc.kill()
            self.proc.wait()
        self.stderr.close()
        if not keep:
            shutil.rmtree(self.dir, ignore_errors=True)


class Miner:
    """A regtest dashd at real time that mines the sessions' height-1 block: mined, read, then
    invalidated so that the next one is at height 1 again (a fresh time, so a node with it as
    its tip is out of IBD)."""

    def __init__(self, dashd, slot, root):
        self.node = Node("dashd", dashd, slot, root)
        self.lock = threading.Lock()
        self.hex = None
        self.mined = 0.0

    def block(self):
        with self.lock:
            if self.hex is None or time.time() - self.mined > BLOCK_MAX_AGE:
                h = self.node.rpc("generatetodescriptor", 1, "raw(51)")[0]
                self.hex = self.node.rpc("getblock", h, 0)
                self.node.rpc("invalidateblock", h)
                self.mined = time.time()
            return self.hex

    def stop(self):
        self.node.stop(keep=False)


# ---- the scripted peers

class Conn:
    """One V1 connection to the node: what arrived, when, and the close."""

    def __init__(self, sock, outbound):
        self.sock = sock
        sock.setblocking(False)
        self.outbound = outbound  # the node dialled it
        self.buf = b""
        self.msgs = []  # (monotonic time, command, payload)
        self.closed_at = None
        self.ping_nonce = struct.pack("<Q", random.getrandbits(64))

    def send(self, cmd, payload=b""):
        self.sock.settimeout(30)
        try:
            self.sock.sendall(p2a.frame(cmd, payload))
        finally:
            self.sock.setblocking(False)

    def read(self):
        now = time.monotonic()
        try:
            data = self.sock.recv(1 << 16)
        except BlockingIOError:
            return
        except OSError:
            data = b""
        if not data:
            self.closed_at = now
            return
        frames, self.buf = p2a.take_frames(self.buf + data, p2a.MAGIC)
        for cmd, payload in frames:
            self.msgs.append((now, cmd, payload))
            if cmd == "ping":
                self.send("pong", payload)

    def first(self, cmd, after=0):
        return next((t for t, c, p in self.msgs[after:] if c == cmd), None)

    def close(self):
        self.sock.close()


def version_payload():
    """Core's `PushNodeVersion` layout at the real time (start height 0, relay), then Dash's
    MNAUTH challenge and masternode flag."""
    addr = struct.pack("<Q", SERVICES) + bytes(18)
    return (struct.pack("<iQq", 70242, SERVICES, int(time.time())) + addr + addr
            + struct.pack("<Q", random.getrandbits(64)) + bytes([len(UA)]) + UA
            + bytes(4) + b"\x01" + bytes(32) + b"\x00")


class Session:
    def __init__(self, node, listener):
        self.node = node
        self.listener = listener
        self.conns = []
        self.diag = None

    def pump(self, until, timeout=EVENT_TIMEOUT, what="an event"):
        """Reads every connection until `until()` holds; raises after `timeout` seconds."""
        end = time.monotonic() + timeout
        while not until():
            left = end - time.monotonic()
            if left <= 0:
                raise TimeoutError(f"no {what} in {timeout:.0f} s")
            live = [c for c in self.conns if c.closed_at is None]
            ready, _, _ = select.select([c.sock for c in live], [], [], min(left, 0.05))
            for c in live:
                if c.sock in ready:
                    c.read()

    def handshake(self, c):
        """V1 handshake, then a ping the node answers: it has processed our `verack`, so the
        peer is fully connected to its tick (`FullyConnectedOnly`)."""
        if c.outbound:
            self.pump(lambda: c.first("version") is not None, 30, "version")
            c.send("version", version_payload())
        else:
            c.send("version", version_payload())
            self.pump(lambda: c.first("version") is not None, 30, "version")
        self.pump(lambda: c.first("verack") is not None, 30, "verack")
        c.send("verack")
        c.send("ping", c.ping_nonce)
        self.pump(lambda: any(cmd == "pong" and p == c.ping_nonce for _, cmd, p in c.msgs),
                  30, "pong")

    def dial_out(self):
        """The node dials this session's listener (`addnode onetry`: both nodes have it)."""
        self.node.rpc("addnode", f"127.0.0.1:{self.listener.getsockname()[1]}", "onetry")
        self.listener.settimeout(30)
        sock, _ = self.listener.accept()
        c = Conn(sock, outbound=True)
        self.conns.append(c)
        self.handshake(c)
        return c

    def dial_in(self):
        sock = socket.create_connection(("127.0.0.1", self.node.port), timeout=30)
        c = Conn(sock, outbound=False)
        self.conns.append(c)
        self.handshake(c)
        return c

    def idle(self, seconds):
        """Waits `seconds`, reading the connections (pings are answered)."""
        end = time.monotonic() + seconds
        self.pump(lambda: time.monotonic() >= end, seconds + 1.0)

    def wait_uniform(self, rng):
        """A uniform 0-6 s wait (D19)."""
        self.idle(rng.uniform(0.0, 6.0))

    def close(self):
        for c in self.conns:
            c.close()


def submit(s, block):
    """`submitblock`, timed: the reply's moment (the reference) and the call's duration. The
    bump happens inside the call, so its duration bounds how much of a node's own processing
    the `first` measures carry."""
    t0 = time.monotonic()
    s.node.rpc("submitblock", block)
    t_ref = time.monotonic()
    return t_ref, t_ref - t0


def session_f6(s, rng, block):
    out = s.dial_out()
    for _ in range(3):
        s.dial_in()
    s.wait_uniform(rng)
    t_ref, submit6 = submit(s, block)
    s.pump(lambda: out.first("mempool") is not None, what="mempool")
    first6 = out.first("mempool") - t_ref
    end = time.monotonic() + EVENT_TIMEOUT
    while not s.node.rpc("mnsync", "status")["IsSynced"]:
        if time.monotonic() > end:
            raise TimeoutError("no FINISHED")
        s.idle(0.2)
    s.wait_uniform(rng)
    # The reference is the call, not its return: the reset applies inside it, and a tick can
    # run before the reply comes back. No tick before the reset disconnects anyone (a FINISHED
    # sync returns before the peer loop, node/sync_manager.cpp:165-168).
    t_reset, wall_reset = time.monotonic(), time.time()
    s.node.rpc("mnsync", "reset")
    s.pump(lambda: out.closed_at is not None, what="full-sync close")
    again = s.dial_out()
    s.pump(lambda: again.closed_at is not None, what="second full-sync close")
    ticks = s.node.tick_disconnects(2)
    if len(ticks) != 2 or ticks[0] < wall_reset:
        raise RuntimeError(f"want two disconnecting ticks after the reset, the log has {ticks}")
    if DIAG:
        wall = wall_reset - t_reset
        s.diag = {"datadir": str(s.node.dir), "reset": wall_reset, "ticks": ticks,
                  "close1": out.closed_at + wall, "close2": again.closed_at + wall}
    return {"first6": first6, "reset_tick": ticks[0] - wall_reset, "gap_tick": ticks[1] - ticks[0],
            "reset": out.closed_at - t_reset, "gap": again.closed_at - out.closed_at,
            "submit6": submit6}


def session_s30(s, rng, block):
    out = s.dial_out()
    s.wait_uniform(rng)
    t_ref, submit30 = submit(s, block)
    s.pump(lambda: out.first("mempool") is not None, what="mempool")
    return {"first30": out.first("mempool") - t_ref, "submit30": submit30}


SESSIONS = {"f6": session_f6, "s30": session_s30}


# ---- the run

class Ledger:
    """samples.tsv and errors.tsv, appended under a lock; the good-session counts."""

    def __init__(self, out):
        self.out = out
        self.lock = threading.Lock()
        self.done = collections.Counter()
        self.failed = collections.Counter()
        self.next_id = 0
        for name in ("samples.tsv", "errors.tsv"):
            drop_partial_line(out / name)
        samples = read_samples(out / "samples.tsv")
        for row in samples:
            self.next_id = max(self.next_id, int(row[2]) + 1)
        self.done.update(good_sessions(samples))
        for row in read_rows(out / "errors.tsv", 4):
            self.next_id = max(self.next_id, int(row[2]) + 1)
            self.failed[row[0], row[1]] += 1

    def record(self, node, kind, sid, samples, error, dispatched):
        """`dispatched`: the session's start, (unix time, the 1-minute load then)."""
        with self.lock:
            if error is None:
                t, load = dispatched
                with open(self.out / "samples.tsv", "a") as f:
                    for name, v in samples.items():
                        f.write(f"{node}\t{kind}\t{sid}\t{name}\t{v:.6f}\t{t:.3f}\t{load:.2f}\n")
                self.done[node, kind] += 1
            else:
                with open(self.out / "errors.tsv", "a") as f:
                    f.write(f"{node}\t{kind}\t{sid}\t{error}\n")
                self.failed[node, kind] += 1


def drop_partial_line(path):
    """Cut a last line a killed run left without its newline, so appended rows start clean."""
    if not path.exists():
        return
    data = path.read_bytes()
    if data and not data.endswith(b"\n"):
        with open(path, "r+b") as f:
            f.truncate(data.rfind(b"\n") + 1)


def read_rows(path, fields):
    """The file's rows with `fields` fields; a row cut short by a killed run is skipped."""
    if not path.exists():
        return []
    rows = (line.rstrip("\n").split("\t", fields - 1) for line in open(path) if line.endswith("\n"))
    return [r for r in rows if len(r) == fields]


# samples.tsv's columns: node, kind, session id, measure, value (s), the session's dispatch time
# (unix s) and the 1-minute load then. Rows written before the last two existed (SP1b r1) read
# with "-" in them.
SAMPLE_FIELDS = 7


def read_samples(path):
    """samples.tsv's rows, each with SAMPLE_FIELDS fields."""
    rows = []
    if not path.exists():
        return rows
    for line in open(path):
        if not line.endswith("\n"):
            continue  # cut short by a killed run
        r = line.rstrip("\n").split("\t")
        if len(r) == 5:
            r += ["-", "-"]
        if len(r) == SAMPLE_FIELDS:
            rows.append(r)
    return rows


def good_sessions(rows):
    """The good sessions per (node, kind) among samples.tsv's `rows`: a session is one (node,
    kind, id), whatever number of measures it gave."""
    return collections.Counter((node, kind) for node, kind, _ in {tuple(r[:3]) for r in rows})


def failure_cap(samples):
    """Failed sessions a (node, kind) may have: `run` stops scheduling it past this."""
    return 20 + samples // 50


# `ks` fails a (node, kind) whose failed sessions are more than this share of its sessions: a
# failure that tracks the timing would bias the samples it leaves (D19).
MAX_FAILURE_SHARE = 0.01


def run(args):
    out = pathlib.Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    dashd = os.environ.get("SP1B_DASHD") or default_dashd()
    rdashd = os.environ.get("SP1B_RDASHD")
    if not rdashd:
        sys.exit("set SP1B_RDASHD to a release rdashd")
    kinds = args.kinds.split(",")
    if args.jobs + 1 > MAX_JOBS:
        sys.exit(f"--jobs {args.jobs}: at most {MAX_JOBS - 1} below port 32768")
    ledger = Ledger(out)
    inflight = collections.Counter()

    def short():
        """The (node, kind) pairs still to schedule: below the target, failures within the cap."""
        return [p for p in itertools.product(NODES, kinds)
                if ledger.done[p] + inflight[p] < args.samples
                and ledger.failed[p] <= failure_cap(args.samples)]

    if not short():
        # A resumed run that is complete (or whose short pairs are past the failure cap): `ks`
        # judges what samples.tsv holds.
        if not args.quiet:
            print(f"nothing to run: done {dict(ledger.done)} failed {dict(ledger.failed)}",
                  flush=True)
        return 0
    root = pathlib.Path(tempfile.mkdtemp(prefix="sp1b-", dir=os.environ.get("TMPDIR")))
    os.chmod(root, 0o700)
    # dashd -version rewrites its datadir's settings.json, so it gets a private one (DEC-569).
    probe = root / "version"
    probe.mkdir(mode=0o700)
    version = subprocess.run([dashd, f"-datadir={probe}", "-version"], capture_output=True,
                             text=True).stdout
    first = (version.splitlines() or [""])[0]
    if CORE_VERSION not in first:
        shutil.rmtree(root, ignore_errors=True)
        sys.exit(f"{dashd} is not dashd {CORE_VERSION}: {first!r}")
    shutil.rmtree(probe)
    binaries = {"dashd": dashd, "rdashd": rdashd}
    # Slot 0 is the miner's; slots 1..jobs the sessions'.
    miner = Miner(dashd, 0, root)
    slots = queue.Queue()
    for k in range(1, args.jobs + 1):
        slots.put(k)
    pick_lock = threading.Lock()
    kept = [0]
    t0 = time.monotonic()
    pause = threading.Lock()  # held by the one worker that waits out a load above args.max_load
    pauses = [0]

    def wait_load():
        """Return once the 1-minute load is below --max-load; one worker waits, the others queue
        behind it, so no session starts meanwhile. Each pause is logged."""
        if args.max_load is None:
            return
        with pause:
            load = os.getloadavg()[0]
            if load < args.max_load or not short():
                return
            pauses[0] += 1
            began = time.monotonic()
            print(f"[{began - t0:7.0f} s] pause: load {load:.1f} >= {args.max_load:g}", flush=True)
            while os.getloadavg()[0] >= args.max_load and short():
                time.sleep(10)
            print(f"[{time.monotonic() - t0:7.0f} s] resume: load {os.getloadavg()[0]:.1f} after "
                  f"{time.monotonic() - began:.0f} s", flush=True)

    def pick():
        """The (node, kind) with the fewest good and running sessions, or None when all have
        enough: the nodes and kinds stay interleaved through the run. A pair whose failures
        pass the cap is not scheduled again, so `ks` reports it short."""
        with pick_lock:
            want = [(ledger.done[p] + inflight[p], p) for p in short()]
            if not want:
                return None
            _, p = min(want)
            inflight[p] += 1
            sid = ledger.next_id
            ledger.next_id += 1
            return p, sid, (time.time(), os.getloadavg()[0])

    def worker():
        while True:
            wait_load()
            job = pick()
            if job is None:
                return
            (node_kind, kind), sid, dispatched = job
            slot = slots.get()
            rng = random.Random(f"{args.seed}:{sid}")
            listener = socket.socket()
            listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            node = None
            error = None
            samples = None
            try:
                listener.bind(("127.0.0.1", PORT_BASE + 3 * slot + 2))
                listener.listen(4)
                block = miner.block()
                node = Node(node_kind, binaries[node_kind], slot, root)
                s = Session(node, listener)
                try:
                    samples = SESSIONS[kind](s, rng, block)
                finally:
                    s.close()
                if s.diag:
                    with ledger.lock, open(out / "diag.tsv", "a") as f:
                        f.write(f"{node_kind}\t{sid}\t{json.dumps(s.diag)}\n")
            except Exception as e:  # a session's failure is recorded, and the run goes on
                error = f"{type(e).__name__}: {e}".replace("\t", " ").replace("\n", " ")
            finally:
                listener.close()
                if node is not None:
                    keep = DIAG or (error is not None and kept[0] < 20)
                    kept[0] += keep
                    node.stop(keep)
                # Counted as done before it stops counting as running, so `pick` never schedules
                # a pair past its target.
                ledger.record(node_kind, kind, sid, samples, error, dispatched)
                with pick_lock:
                    inflight[node_kind, kind] -= 1
                slots.put(slot)

    threads = [threading.Thread(target=worker, daemon=True) for _ in range(args.jobs)]
    for t in threads:
        t.start()
    last = time.monotonic()
    loads = [os.getloadavg()[0]]  # the 1-minute load, each minute: the run's conditions
    while any(t.is_alive() for t in threads):
        time.sleep(1)
        if time.monotonic() - last > 60:
            last = time.monotonic()
            loads.append(os.getloadavg()[0])
            print(f"[{time.monotonic() - t0:7.0f} s] load {loads[-1]:.0f} done {dict(ledger.done)} "
                  f"failed {dict(ledger.failed)}", flush=True)
    miner.stop()
    if kept[0] == 0:
        shutil.rmtree(root, ignore_errors=True)
    wall = time.monotonic() - t0
    me, kids = resource.getrusage(resource.RUSAGE_SELF), resource.getrusage(resource.RUSAGE_CHILDREN)
    cpu = me.ru_utime + me.ru_stime + kids.ru_utime + kids.ru_stime
    summary = {
        "dashd": version.splitlines()[0], "rdashd": rdashd, "seed": args.seed,
        "jobs": args.jobs, "samples": args.samples, "kinds": kinds,
        "wall_s": round(wall, 1), "cpu_h": round(cpu / 3600, 3),
        "affinity": len(os.sched_getaffinity(0)), "nice": os.nice(0),
        "load1": {"min": min(loads), "mean": round(sum(loads) / len(loads), 1), "max": max(loads)},
        "max_load": args.max_load, "pauses": pauses[0],
        "done": {f"{n}/{k}": v for (n, k), v in ledger.done.items()},
        "failed": {f"{n}/{k}": v for (n, k), v in ledger.failed.items()},
        "kept_datadirs": str(root) if kept[0] else None,
    }
    with open(out / "run-summary.json", "a") as f:
        f.write(json.dumps(summary) + "\n")
    print(json.dumps(summary, indent=1))


def ks(args):
    out = pathlib.Path(args.out)
    samples = read_samples(out / "samples.tsv")
    values = collections.defaultdict(list)
    for node, _kind, _sid, name, v, _t, _load in samples:
        values[node, name].append(float(v))
    failed = collections.Counter((r[0], r[1]) for r in read_rows(out / "errors.tsv", 4))
    res = args.resolution
    rng = random.Random(f"ks:{args.seed}")
    rows = [f"# D and p: KS at {res * 1000:g} ms resolution, p by {args.rounds} permutations "
            "(the verdict, D21); D_raw and p_raw: the samples' own resolution, asymptotic p "
            "(reported only); quantiles in seconds",
            "measure\tn_dashd\tn_rdashd\tD\tp\tD_raw\tp_raw\t"
            "dashd p1/p25/p50/p75/p99\trdashd p1/p25/p50/p75/p99\tverdict"]
    ok = True
    for name, (_kind, status, note) in MEASURES.items():
        if status == "blocked":
            rows.append(f"{name}\t-\t-\t-\t-\t-\t-\t-\t-\tNOT SAMPLED: {note}")
            continue
        a, b = values["dashd", name], values["rdashd", name]
        if not a or not b:
            verdict = "FAIL: no samples" if status == "gate" else f"no samples (reported: {note})"
            rows.append(f"{name}\t{len(a)}\t{len(b)}\t-\t-\t-\t-\t-\t-\t{verdict}")
            ok &= status != "gate"
            continue
        d_raw, p_raw = ks_2samp(a, b)
        d, p = ks_permutation([round(x / res) for x in a], [round(x / res) for x in b],
                              args.rounds, rng)
        if len(a) < args.samples or len(b) < args.samples:
            verdict = "FAIL: too few"
        else:
            verdict = "PASS" if p > 0.05 else "FAIL"
        if status == "gate":
            ok &= verdict == "PASS"
        else:
            verdict = f"{verdict} (reported: {note})"
        rows.append(f"{name}\t{len(a)}\t{len(b)}\t{d:.4f}\t{p:.4g}\t{d_raw:.4f}\t{p_raw:.4g}\t"
                    f"{fmt(a)}\t{fmt(b)}\t{verdict}")
    done = good_sessions(samples)
    for node, kind in sorted(set(failed)):
        share = failed[node, kind] / (failed[node, kind] + done[node, kind])
        verdict = "FAIL" if share > MAX_FAILURE_SHARE else "ok"
        ok &= verdict == "ok"
        rows.append(f"# failed sessions {node}/{kind}: {failed[node, kind]} ({share:.2%}, "
                    f"at most {MAX_FAILURE_SHARE:.0%}): {verdict}")
    text = "\n".join(rows) + "\n"
    (out / "ks.tsv").write_text(text)
    print(text, end="")
    return 0 if ok else 1


def selftest(_args):
    """The KS code's own checks: one sample against itself, two draws of one distribution, a
    shift, and a known statistic."""
    rng = random.Random(1)
    a = [rng.uniform(0, 6) for _ in range(10000)]
    b = [rng.uniform(0, 6) for _ in range(10000)]
    assert ks_2samp(a, a) == (0.0, 1.0)
    d, p = ks_2samp(a, b)
    assert p > 0.05, (d, p)
    d, p = ks_2samp(a, [x + 0.2 for x in b])
    assert p < 1e-6, (d, p)
    assert ks_2samp([1, 2, 3], [4, 5, 6])[0] == 1.0
    assert abs(ks_2samp([1, 2, 2, 3], [2, 3])[0] - 0.25) < 1e-12
    # Q_KS at the 5% critical lambda (1.358), and 1 where the series cannot be summed.
    assert abs(kolmogorov_q(1.358) - 0.05) < 1e-3
    assert kolmogorov_q(0.005) == 1.0 and kolmogorov_q(0.3) > 0.9999
    # `ks_d` is `dash-p2p-transport/tests/dashd_interop.rs`'s `ks_two` (DEC-562): the largest
    # CDF difference over every point of an integer domain, there 0 to 4095.
    for seed in range(5):
        r = random.Random(seed)
        xs, ys = [r.randrange(64) for _ in range(300)], [r.randrange(64) for _ in range(200)]
        cdf = lambda s, x: sum(v <= x for v in s) / len(s)
        brute = max(abs(cdf(xs, x) - cdf(ys, x)) for x in range(64))
        assert abs(ks_d(sorted(xs), sorted(ys)) - brute) < 1e-12, seed
    # The permutation p on tied samples: one 3-point distribution twice, and a 2% shift of mass
    # between two of its points (what the asymptotic p, conservative on ties, can miss).
    pick3 = lambda r: r.choices([600, 601, 602], weights=[70, 25, 5], k=10000)
    a3, b3 = pick3(random.Random(2)), pick3(random.Random(3))
    assert ks_permutation(a3, b3, 200, random.Random(4))[1] > 0.05
    c3 = random.Random(5).choices([600, 601, 602], weights=[68, 27, 5], k=10000)
    assert ks_permutation(a3, c3, 200, random.Random(6))[1] < 0.05
    resume_selftest()
    print("selftest ok")
    return 0


def resume_selftest():
    """`run` into a complete OUT_DIR returns at once, starting no node (a dashd or rdashd that
    cannot exist would fail it), and `ks` reads its rows, old 5-column ones among them."""
    with tempfile.TemporaryDirectory() as d:
        out = pathlib.Path(d)
        with open(out / "samples.tsv", "w") as f:
            for sid, (node, kind) in enumerate(itertools.product(NODES, KINDS)):
                for name, (k, _status, _note) in MEASURES.items():
                    if k == kind:
                        tail = "" if node == "dashd" else "\t1700000000.000\t3.25"
                        f.write(f"{node}\t{kind}\t{sid}\t{name}\t6.000000{tail}\n")
            f.write("dashd\tf6\t9\tfirst6\t6.0")  # a row a killed run cut short
        rows = read_samples(out / "samples.tsv")
        assert all(len(r) == SAMPLE_FIELDS for r in rows) and rows[0][5:] == ["-", "-"], rows[0]
        assert {r[5] for r in rows if r[0] == "rdashd"} == {"1700000000.000"}
        ns = argparse.Namespace(out=str(out), samples=1, jobs=1, kinds=",".join(KINDS),
                                seed="selftest", max_load=None, quiet=True)
        saved = {k: os.environ.get(k) for k in ("SP1B_DASHD", "SP1B_RDASHD")}
        os.environ["SP1B_DASHD"] = os.environ["SP1B_RDASHD"] = str(out / "no-such-node")
        try:
            assert run(ns) == 0
        finally:
            for k, v in saved.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = v
        assert not (out / "run-summary.json").exists()
        # The cut-short row is gone, so a resumed run's first row starts its own line.
        text = (out / "samples.tsv").read_text()
        assert text.endswith("\n") and "\t9\t" not in text, text[-80:]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run")
    r.add_argument("out")
    r.add_argument("--samples", type=int, default=10000)
    r.add_argument("--jobs", type=int, default=64)
    r.add_argument("--kinds", default=",".join(KINDS))
    r.add_argument("--seed", default="sp1b")
    r.add_argument("--max-load", type=float, default=os.environ.get("SP1B_MAX_LOAD") or None,
                   help="start no session while the 1-minute load is at or above this")
    r.set_defaults(quiet=False)
    k = sub.add_parser("ks")
    k.add_argument("out")
    k.add_argument("--samples", type=int, default=10000)
    k.add_argument("--resolution", type=float, default=0.01,
                   help="seconds; samples are rounded to it for the verdict (D21)")
    k.add_argument("--rounds", type=int, default=1000, help="the verdict's permutations")
    k.add_argument("--seed", default="sp1b")
    sub.add_parser("selftest")
    args = ap.parse_args()
    if args.cmd == "run":
        run(args)
        return 0
    return {"ks": ks, "selftest": selftest}[args.cmd](args)


if __name__ == "__main__":
    sys.exit(main())
