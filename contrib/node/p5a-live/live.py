#!/usr/bin/env python3
"""P5a's live testnet check (P5a D18): spork and clsig intake and relay, rdashd next to dashd v24.

1. Fetch: the harness connects to public testnet peers (DNS seeds) as a client and only reads.
   It asks `getsporks` for the network's sporks, waits for a `clsig` `inv` (one per block) and
   fetches that ChainLock with `getdata`.
2. Replay: each node (rdashd, then dashd v24.0.0-rc.3) starts on `-testnet` in its own datadir.
   It dials two local scripted peers, A and B (`-connect`; rdashd takes no inbound connections
   before P2b), and nothing else, so neither node learns sporks elsewhere. The scenario:
     a. A announces the first spork (`inv`); the node, in IBD, asks A for it; A answers.
     b. A pushes the other sporks unsolicited.
     c. A pushes the ChainLock unsolicited: with SPORK_19 on, Core scores +10 "unrequested clsig".
     d. A announces the ChainLock (`inv`): a node in IBD asks for nothing but sporks.
     e. B asks for the ChainLock and the first spork (`getdata`), then announces that spork.
   Each peer records what the node sends it.
3. Compare: the two nodes' messages to A and B after the handshake, by command and payload, less
   what neither task owns (pings, header requests, the handshake extras). dashd's `getsporks` is
   SP1's (P5a D14), so it is listed apart. Invs are compared by entry within each step: how many
   entries one `SendMessages` pass flushes after a burst is timing, in both nodes ("SAME up to
   inv batching"). The nodes' misbehaviour log lines are compared too. Both nodes run in
   `-blocksonly=1` mode (rdashd's default until P5, DEC-322): each node's `version` must carry
   relay 0 and neither may send `senddsq` to either peer (np:1582, 4224-4232); the run exits 1
   if not.

Both nodes run with `-v2transport=0`: the scripted peers speak V1 only. It never touches the
oracles or /work/chains. Usage:
  live.py --rdashd PATH --dashd PATH --out DIR [--clsig-wait SECONDS] [--fetched FETCHED_JSON]
  live.py --out DIR --rdashd - --dashd - --compare-only   # re-judge a run's saved JSON
"""

import argparse
import hashlib
import json
import os
import random
import re
import shutil
import socket
import struct
import subprocess
import sys
import threading
import time

MAGIC = bytes.fromhex("cee2caff")
TESTNET_PORT = 19999
SEEDS = ["testnet-seed.dashdot.io"]
PROTOCOL = 70240
UA = b"/p5a-live:1/"  # DEC-83: the tool says who it is
MSG_SPORK, MSG_CLSIG = 6, 29
NODE_NETWORK = 1
# What neither SP0 nor P5a owns, or what carries per-run values: dropped before comparing.
IGNORED = {"ping", "pong", "getheaders", "getheaders2", "sendheaders", "sendheaders2", "sendcmpct",
           "senddsq", "sendaddrv2", "verack", "version", "getaddr", "addr", "addrv2", "qsendrecsigs",
           "mempool", "sendtxrcncl"}
# A Dash `version` ends with fRelay, the 32-byte mnauth challenge and fMasternode.
RELAY_FROM_END = 34
# SP1's sync tick (P5a D14): recorded, compared apart.
SP1 = {"getsporks", "govsync", "ssc", "getmnlistd"}


def sha256d(b):
    return hashlib.sha256(hashlib.sha256(b).digest()).digest()


def frame(cmd, payload=b""):
    return (MAGIC + cmd.encode().ljust(12, b"\0") + struct.pack("<I", len(payload))
            + sha256d(payload)[:4] + payload)


def inv_payload(entries):
    return bytes([len(entries)]) + b"".join(struct.pack("<I", t) + h for t, h in entries)


def parse_invs(payload):
    n = payload[0]
    return [(struct.unpack_from("<I", payload, 1 + 36 * i)[0], payload[5 + 36 * i:37 + 36 * i])
            for i in range(n)]


def version_payload(services=NODE_NETWORK):
    addr = struct.pack("<Q", 0) + bytes(18)
    return (struct.pack("<iQq", PROTOCOL, services, int(time.time())) + addr + addr
            + struct.pack("<Q", random.getrandbits(64)) + bytes([len(UA)]) + UA
            + struct.pack("<i", 0) + b"\x01" + os.urandom(32) + b"\x00")


class Conn:
    """One framed P2P connection; `recv` returns `(cmd, payload)` or None at the deadline."""

    def __init__(self, sock):
        self.s, self.buf = sock, b""

    def send(self, cmd, payload=b""):
        self.s.sendall(frame(cmd, payload))

    def recv(self, deadline):
        while True:
            if len(self.buf) >= 24:
                n, = struct.unpack_from("<I", self.buf, 16)
                if len(self.buf) >= 24 + n:
                    cmd = self.buf[4:16].rstrip(b"\0").decode()
                    payload, self.buf = self.buf[24:24 + n], self.buf[24 + n:]
                    return cmd, payload
            left = deadline - time.time()
            if left <= 0:
                return None
            self.s.settimeout(left)
            try:
                chunk = self.s.recv(1 << 20)
            except socket.timeout:
                return None
            if not chunk:
                raise ConnectionError("closed")
            self.buf += chunk


def fetch(clsig_wait):
    """Sporks and one ChainLock from public testnet peers, by reads only."""
    hosts = set()
    for seed in SEEDS:
        hosts |= {a[4][0] for a in socket.getaddrinfo(seed, TESTNET_PORT, socket.AF_INET)}
    hosts = sorted(hosts)
    random.shuffle(hosts)
    for host in hosts:
        try:
            c = Conn(socket.create_connection((host, TESTNET_PORT), timeout=10))
        except OSError as e:
            print(f"fetch {host}: {e}", file=sys.stderr)
            continue
        sporks, clsig, asked = [], None, False
        end = time.time() + clsig_wait
        c.send("version", version_payload(0))
        try:
            while time.time() < end and clsig is None:
                m = c.recv(end)
                if m is None:
                    break
                cmd, payload = m
                if cmd == "version":
                    c.send("verack")
                elif cmd == "verack" and not asked:
                    c.send("getsporks")
                    asked = True
                elif cmd == "ping":
                    c.send("pong", payload)
                elif cmd == "spork":
                    sporks.append(payload)
                elif cmd == "inv":
                    cl = [(t, h) for t, h in parse_invs(payload) if t == MSG_CLSIG]
                    if cl:
                        c.send("getdata", inv_payload(cl[:1]))
                elif cmd == "clsig":
                    clsig = payload
        except (OSError, ConnectionError) as e:
            print(f"fetch {host}: {e}", file=sys.stderr)
        c.s.close()
        print(f"fetch {host}: {len(sporks)} sporks, clsig {'yes' if clsig else 'no'}",
              file=sys.stderr)
        if sporks and clsig:
            return host, sporks, clsig
    sys.exit("no public testnet peer gave both the sporks and a ChainLock")


class ScriptedPeer(threading.Thread):
    """A local peer the node dials: answers the handshake and pings, records everything."""

    def __init__(self, name):
        super().__init__(daemon=True)
        self.name, self.log, self.conn = name, [], None
        self.ready = threading.Event()
        self.lsock = socket.socket()
        self.lsock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.lsock.bind(("127.0.0.1", 0))
        self.lsock.listen(1)
        self.port = self.lsock.getsockname()[1]
        self.t0 = None
        self.lock = threading.Lock()

    def run(self):
        s, _ = self.lsock.accept()
        self.conn = Conn(s)
        while True:
            try:
                m = self.conn.recv(time.time() + 3600)
            except (OSError, ConnectionError):
                with self.lock:
                    self.log.append((time.time(), "closed", ""))
                return
            if m is None:
                continue
            cmd, payload = m
            with self.lock:
                self.log.append((time.time(), cmd, payload.hex()))
            if cmd == "version":
                self.send("version", version_payload())
                self.send("verack")
            elif cmd == "verack":
                self.ready.set()
            elif cmd == "ping":
                self.send("pong", payload)

    def send(self, cmd, payload=b""):
        with self.lock:
            self.log.append((time.time(), f"> {cmd}", payload.hex()))
        self.conn.send(cmd, payload)

    def mark(self, step):
        with self.lock:
            self.log.append((time.time(), f"# {step}", ""))


def free_port():
    """A loopback port nothing listens on now (bind only, then release)."""
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def start_node(kind, binary, datadir, peers, rpcport):
    os.makedirs(datadir, mode=0o700)
    args = [binary, "-testnet", f"-datadir={datadir}", "-dnsseed=0", "-fixedseeds=0",
            "-debug=net", "-debug=spork", "-debug=chainlocks", "-printtoconsole=0",
            # The scripted peers speak V1 only.
            "-v2transport=0",
            f"-rpcport={rpcport}", "-rpcbind=127.0.0.1", "-rpcallowip=127.0.0.1",
            "-rpcuser=p5a", "-rpcpassword=p5a"]
    args += [f"-connect=127.0.0.1:{p.port}" for p in peers]
    if kind == "dashd":
        # rdashd's defaults: no inbound, and `-blocksonly=1` until P5 (DEC-322, P5a D23).
        args += ["-listen=0", "-server=1", "-blocksonly=1"]
    return subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def scenario(a, b, sporks, clsig, settle):
    """Steps a-e of the module docs; `settle` seconds after each."""
    first = sporks[0]
    h = sha256d(first)

    def mark(step):
        a.mark(step)
        b.mark(step)

    mark("a: inv first spork")
    a.send("inv", inv_payload([(MSG_SPORK, h)]))
    end = time.time() + 30
    while time.time() < end and not any(c == "getdata" for _, c, _ in a.log):
        time.sleep(0.2)
    a.send("spork", first)
    time.sleep(settle)
    mark("b: push the other sporks")
    for s in sporks[1:]:
        a.send("spork", s)
    time.sleep(settle)
    mark("c: push the clsig")
    a.send("clsig", clsig)
    time.sleep(settle)
    mark("d: inv the clsig")
    a.send("inv", inv_payload([(MSG_CLSIG, sha256d(clsig))]))
    time.sleep(settle)
    mark("e: getdata clsig + first spork, then inv the spork")
    b.send("getdata", inv_payload([(MSG_CLSIG, sha256d(clsig)), (MSG_SPORK, h)]))
    time.sleep(settle)
    b.send("inv", inv_payload([(MSG_SPORK, h)]))
    time.sleep(settle)


def received_rows(log, after_ready):
    """What the node sent a peer, per its `log`, once the handshake was done: (step, cmd, payload)."""
    out, step = [], "-"
    for t, cmd, payload in log:
        if cmd.startswith("# "):
            step = cmd[2:3]
            continue
        if cmd.startswith("> ") or t < after_ready:
            continue
        out.append((step, cmd, payload))
    return out


def log_lines(path):
    pat = re.compile(r"Misbehaving|unrequested|SPORK -- |CLSIG|PushInv|Requesting|timeout of inflight")
    with open(path, errors="replace") as f:
        return [l.split(" ", 1)[1].rstrip() for l in f if pat.search(l)]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--rdashd", required=True)
    ap.add_argument("--dashd", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--clsig-wait", type=int, default=600)
    ap.add_argument("--settle", type=float, default=3.0)
    ap.add_argument("--fetched", help="replay a previous run's fetched.json instead of fetching")
    ap.add_argument("--compare-only", action="store_true",
                    help="re-judge the rdashd.json and dashd.json already in --out")
    o = ap.parse_args()
    if o.compare_only:
        results = {}
        for kind in ("rdashd", "dashd"):
            with open(os.path.join(o.out, f"{kind}.json")) as f:
                results[kind] = json.load(f)
            # Runs before "ready" was saved: the scenario's first mark came `--settle` after it.
            marks = [t for t, c, _ in results[kind]["A"] if c.startswith("# ")]
            results[kind].setdefault("ready", marks[0] - o.settle)
        compare(o.out, results)
        return
    if os.path.exists(o.out):
        shutil.rmtree(o.out)
    os.makedirs(o.out, mode=0o700)

    if o.fetched:
        with open(o.fetched) as f:
            got = json.load(f)
        host, sporks, clsig = got["peer"], [bytes.fromhex(s) for s in got["sporks"]], bytes.fromhex(got["clsig"])
    else:
        host, sporks, clsig = fetch(o.clsig_wait)
    with open(os.path.join(o.out, "fetched.json"), "w") as f:
        json.dump({"peer": host, "sporks": [s.hex() for s in sporks], "clsig": clsig.hex()}, f,
                  indent=1)

    results = {}
    for kind, binary in [("rdashd", o.rdashd), ("dashd", o.dashd)]:
        a, b = ScriptedPeer("A"), ScriptedPeer("B")
        a.start()
        b.start()
        datadir = os.path.join(o.out, kind)
        node = start_node(kind, binary, datadir, [a, b], free_port())
        try:
            for p in (a, b):
                if not p.ready.wait(120):
                    sys.exit(f"{kind}: peer {p.name} saw no handshake")
            time.sleep(o.settle)
            ready = time.time()
            scenario(a, b, sporks, clsig, o.settle)
        finally:
            node.terminate()
            try:
                node.wait(60)
            except subprocess.TimeoutExpired:
                node.kill()
        results[kind] = {
            "ready": ready,
            "A": a.log,
            "B": b.log,
            "log": log_lines(os.path.join(datadir, "testnet3", "debug.log")),
        }
        with open(os.path.join(o.out, f"{kind}.json"), "w") as f:
            json.dump(results[kind], f, indent=1)
    compare(o.out, results)


def by_entry(rows):
    """`rows` with each inv split into its entries, sorted within each step: when a burst of
    sporks is processed, how many entries one SendMessages pass flushes is timing, in both nodes."""
    out = []
    for s, c, p in rows:
        if c == "inv":
            out += [(s, "inv", (bytes([1]) + t.to_bytes(4, "little") + h).hex())
                    for t, h in parse_invs(bytes.fromhex(p))]
        else:
            out.append((s, c, p))
    return sorted(out, key=lambda r: (r[0], r[1] != "inv", r[2] if r[1] == "inv" else ""))


def compare(out, results):
    report = []
    for peer in ("A", "B"):
        rows = {k: received_rows(results[k][peer], results[k]["ready"]) for k in results}
        keep = lambda r: [(s, c, p) for s, c, p in r if c not in IGNORED | SP1]
        sp1 = lambda r: sorted({c for _, c, _ in r if c in SP1})
        rd, d = keep(rows["rdashd"]), keep(rows["dashd"])
        verdict = ("SAME" if rd == d else
                   "SAME up to inv batching" if by_entry(rd) == by_entry(d) else "DIFFERENT")
        report.append(f"peer {peer}: {verdict}")
        for name, r in (("rdashd", rd), ("dashd", d)):
            report.append(f"  {name}:")
            report += [f"    {s} {c} {p[:80]}" for s, c, p in by_entry(r)]
        report.append(f"  SP1 (D14): rdashd {sp1(rows['rdashd'])} dashd {sp1(rows['dashd'])}")
    failed = False
    for kind in results:
        for peer in ("A", "B"):
            sent = [(c, p) for _, c, p in results[kind][peer] if not c.startswith(("> ", "# "))]
            relay = [bytes.fromhex(p)[-RELAY_FROM_END] for c, p in sent if c == "version"]
            senddsq = sum(c == "senddsq" for c, _ in sent)
            ok = relay == [0] and senddsq == 0
            failed |= not ok
            report.append(f"blocks-only {kind} -> {peer}: version relay {relay}, senddsq x{senddsq}: "
                          f"{'PASS' if ok else 'FAIL'}")
    for kind in ("rdashd", "dashd"):
        report.append(f"{kind} log:")
        report += [f"  {l}" for l in results[kind]["log"] if "Misbehaving" in l or "unrequested" in l]
    text = "\n".join(report) + "\n"
    with open(os.path.join(out, "summary.txt"), "w") as f:
        f.write(text)
    print(text)
    if failed:
        sys.exit(1)


if __name__ == "__main__":
    main()
