#!/usr/bin/env python3
"""Captures P2a-core's dashd transcript fixtures from regtest dashd v24.0.0-rc.3.

Each scenario starts a fresh regtest dashd with its mock time frozen at `SIM_START` (P2a §6.1:
fresh datadirs, frozen mocktime), genesis-only unless the scenario mines blocks before any peer
connects (recorded as `chain` lines), scripts one or more V1 peers against it,
and writes what each peer sent and what dashd sent back, segment by segment, to
`dashd-p2p/tests/fixtures/dashd-v24/<scenario>.txt`. `dashd-p2p/tests/dashd_transcripts.rs`
replays the peers' side against the node in `sim` and compares the node's side with dashd's.

Usage:
  P2A_DASHD=/path/to/dashd-24.0.0-rc.3/bin/dashd capture.py OUT_DIR [SCENARIO...]
  capture.py selftest             the V1 reader's negative controls (no dashd)

`P2A_CAPTURE_DIR` (default /work/scratch/p2a-core/capture) holds the datadirs; `P2A_PORT_BASE`
(default 31700) moves the ports. It never touches /work/chains or /work/bench.

Fixture format, one record per line (`#` lines are comments):
  mocktime T                      the node's mock time at start
  chain HEX                       a header the node has before the scenario (in order)
  ibd 0|1                         whether the node is in IBD at start
  connect N TYPE SERVICES         the node opens an outbound TYPE connection to peer N, which
                                  it believes offers SERVICES (addrman's view, echoed in version)
  accept N                        peer N connects to the node (inbound)
  > N CMD HEX                     peer N sends CMD with payload HEX
  > N pong echo                   peer N answers the node's last ping (its nonce)
  time T                          the mock time moves to T (`setmocktime`)
  mine HEX                        the node connects a new tip with header HEX (`generate`)
  < N CMD HEX                     the node sends CMD to peer N
  < N closed                      the node closes the connection to peer N
Each input line (connect, accept, >, time, mine) opens a segment; the `<` lines after it are what
the node sent in that segment, in order per peer.

Reading: every V1 frame dashd sends is checked as Core's peer reads it (net.cpp:731-739: the
network's magic and the length cap; net.cpp:796-807: the checksum and the command's charset and
padding), and a transcript is written only if no incomplete frame is left buffered; anything else
stops the capture (`FramingError`).

Synchronisation: after each input the tool waits until dashd has been quiet for QUIET seconds,
then pings every handshaken peer and waits for the pong. Those pings and pongs are the tool's own
and are not recorded. With the mock time frozen nothing else dashd sends is time-driven; a `time`
record lets the sync tick fire, whose sends (later tasks') the replay drops.
"""

import base64
import contextlib
import hashlib
import json
import os
import pathlib
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import time
import types
import urllib.error
import urllib.request

SIM_START = 1_700_000_000
MAGIC = bytes.fromhex("fcc1b7dc")  # regtest pchMessageStart
# Each chain's pchMessageStart and dashd flag (O13a's testnet and mainnet profiles).
MAGICS = {"regtest": MAGIC, "testnet": bytes.fromhex("cee2caff"), "mainnet": bytes.fromhex("bf0c6bbd")}
CHAIN_FLAG = {"regtest": ("-regtest",), "testnet": ("-testnet",), "mainnet": ()}
QUIET = float(os.environ.get("P2A_QUIET", "2.0"))
ROOT = pathlib.Path(os.environ.get("P2A_CAPTURE_DIR", "/work/scratch/p2a-core/capture"))
PORT_BASE = int(os.environ.get("P2A_PORT_BASE", "31700"))
DASHD = os.environ.get("P2A_DASHD", "")
SYNC_NONCE = 0x7EA5_0000_0000_0000  # + peer: the tool's own sync pings
VERSION_NONCE = 0x5EED_0000_0000_0000  # + peer: the scripted peers' version nonces
UA = b"/p2a-transcripts:1/"  # DEC-83: the tool says who it is

# Services the scripted peers advertise.
FULL = 0x1 | 0x4 | 0x400  # NODE_NETWORK | NODE_BLOOM | NODE_NETWORK_LIMITED
BIT11 = 0x800  # NODE_HEADERS_COMPRESSED


def sha256d(b):
    return hashlib.sha256(hashlib.sha256(b).digest()).digest()


def compact(n):
    if n < 0xFD:
        return bytes([n])
    if n <= 0xFFFF:
        return b"\xfd" + struct.pack("<H", n)
    return b"\xfe" + struct.pack("<I", n)


# Core's MAX_PROTOCOL_MESSAGE_LENGTH (net.h:84): 3 MiB, under MAX_SIZE.
MAX_PROTOCOL_MESSAGE_LENGTH = 3 * 1024 * 1024


class FramingError(RuntimeError):
    """dashd sent bytes a Core peer would refuse or drop: the capture stops."""


def command_valid(raw):
    """Core's `CMessageHeader::IsCommandValid` (protocol.cpp): printable ASCII, then only NULs."""
    name, nul, pad = raw.partition(b"\0")
    return all(0x20 <= c <= 0x7E for c in name) and pad.strip(b"\0") == b""


def take_frames(buf, magic):
    """The whole V1 frames at the front of `buf`, checked as Core's peer reads them, and the
    bytes left (an incomplete frame)."""
    out = []
    while len(buf) >= 24:
        if buf[:4] != magic:
            raise FramingError(f"wrong message start {buf[:4].hex()} (want {magic.hex()})")
        n, = struct.unpack_from("<I", buf, 16)
        if n > MAX_PROTOCOL_MESSAGE_LENGTH:
            raise FramingError(f"a {n}-byte message, over the cap")
        if len(buf) < 24 + n:
            break
        payload = buf[24:24 + n]
        if sha256d(payload)[:4] != buf[20:24]:
            raise FramingError(f"a bad checksum on {buf[4:16]!r}")
        if not command_valid(buf[4:16]):
            raise FramingError(f"an invalid command {buf[4:16]!r}")
        out.append((buf[4:16].rstrip(b"\0").decode(), payload))
        buf = buf[24 + n:]
    return out, buf


def frame(cmd, payload, magic=MAGIC):
    return magic + cmd.encode().ljust(12, b"\0") + struct.pack("<I", len(payload)) + sha256d(payload)[:4] + payload


def version_payload(services, nonce):
    """Core's `PushNodeVersion` layout (start height 0, relay), then Dash's MNAUTH challenge and
    masternode flag."""
    addr = struct.pack("<Q", services) + bytes(18)
    return (struct.pack("<iQq", 70242, services, SIM_START) + addr + addr
            + struct.pack("<Q", nonce) + bytes([len(UA)]) + UA
            + bytes(4) + b"\x01" + bytes(32) + b"\x00")


# ---- headers

def header_fields(raw):
    version, = struct.unpack_from("<i", raw, 0)
    time_, bits, nonce = struct.unpack_from("<III", raw, 68)
    return version, raw[4:36], raw[36:68], time_, bits, nonce


def headers_payload(headers):
    """`headers`: each header followed by a zero transaction count."""
    return compact(len(headers)) + b"".join(h + b"\x00" for h in headers)


# CBlockHeader::Compress's flag bits: the version's index in the recent list (0: present), and
# the fields sent in full.
PREV_HASH, TIME, BITS = 0x08, 0x10, 0x20


def headers2_payload(headers):
    """`headers2` as dashd replies to `getheaders2`: the first header in full and, unlike an
    announcement, kept out of the version list (net_processing.cpp:4851-4855); each later one
    compressed against its predecessor with the 7-entry most-recent version list
    (primitives/block.cpp:37-93)."""
    versions, out, prev = [], [compact(len(headers))], None
    for raw in headers:
        version, prev_hash, merkle, time_, bits, nonce = header_fields(raw)
        flags = 0
        if prev is None:
            flags |= PREV_HASH | TIME | BITS
        else:
            if version in versions:
                i = versions.index(version)
                flags |= i + 1
                versions.insert(0, versions.pop(i))
            else:
                versions.insert(0, version)
                del versions[7:]
            # Core subtracts the uint32 times, so an earlier time wraps and is sent in full.
            diff = (time_ - prev[0]) & 0xFFFF_FFFF
            if diff > 32767:
                flags |= TIME
            if bits != prev[1]:
                flags |= BITS
        body = b"" if flags & 0x07 else struct.pack("<i", version)
        if flags & PREV_HASH:
            body += prev_hash
        body += merkle
        body += struct.pack("<I", time_) if flags & TIME else struct.pack("<h", diff)
        if flags & BITS:
            body += struct.pack("<I", bits)
        out.append(bytes([flags]) + body + struct.pack("<I", nonce))
        prev = (time_, bits)
    return b"".join(out)


# ---- dashd

class Node:
    """A regtest dashd v24 (or `chain`'s) with its own datadir and ports."""

    def __init__(self, name, index, mocktime, extra=(), chain="regtest"):
        if not DASHD:
            sys.exit("set P2A_DASHD to dashd 24.0.0-rc.3")
        self.name = name
        self.dir = ROOT / name
        shutil.rmtree(self.dir, ignore_errors=True)
        self.dir.mkdir(parents=True, mode=0o700)
        self.port = PORT_BASE + 2 * index
        self.rpc_port = PORT_BASE + 2 * index + 1
        self.chain = chain
        # A `-listen=0` node has no listener to bind (Core refuses `-bind` with it).
        listen = () if "-listen=0" in extra else ("-listen=1", f"-bind=127.0.0.1:{self.port}")
        self.args = [DASHD, f"-datadir={self.dir}", *CHAIN_FLAG[chain], "-server", *listen[:1], "-listenonion=0",
                     "-discover=0", "-dnsseed=0", "-fixedseeds=0", "-upnp=0", "-natpmp=0", "-debug=net",
                     "-logips", "-printtoconsole=0", f"-port={self.port}", *listen[1:],
                     f"-rpcport={self.rpc_port}", "-rpcbind=127.0.0.1", "-rpcallowip=127.0.0.1",
                     "-rpcuser=p2a", "-rpcpassword=p2a", f"-mocktime={mocktime}", *extra]
        self.stderr = open(self.dir / "stderr.txt", "wb")
        self.proc = subprocess.Popen(self.args, stdout=subprocess.DEVNULL, stderr=self.stderr)
        end = time.time() + 900
        while True:
            try:
                self.rpc("getblockcount")
                break
            except Exception:
                if self.proc.poll() is not None or time.time() > end:
                    raise RuntimeError(f"{name}: dashd did not start; see {self.dir}/stderr.txt")
                time.sleep(0.5)

    def rpc(self, method, *params):
        req = urllib.request.Request(
            f"http://127.0.0.1:{self.rpc_port}/",
            data=json.dumps({"jsonrpc": "1.0", "id": 0, "method": method, "params": list(params)}).encode(),
            headers={"Authorization": "Basic " + base64.b64encode(b"p2a:p2a").decode()})
        try:
            with urllib.request.urlopen(req, timeout=600) as r:
                reply = json.load(r)
        except urllib.error.HTTPError as e:
            reply = json.load(e)
        if reply.get("error"):
            raise RuntimeError(f"{method}{params}: {reply['error']}")
        return reply["result"]

    def header(self, height):
        return bytes.fromhex(self.rpc("getblockheader", self.rpc("getblockhash", height), False))

    def mine(self, n):
        """Mines `n` blocks; returns their raw headers."""
        hashes = []
        while len(hashes) < n:
            hashes += self.rpc("generatetodescriptor", min(500, n - len(hashes)), "raw(51)")
        return [bytes.fromhex(self.rpc("getblockheader", h, False)) for h in hashes]

    def stop(self):
        try:
            self.rpc("stop")
            self.proc.wait(timeout=600)
        except Exception:
            self.proc.kill()
        self.stderr.close()


# ---- peers and the transcript

class Peer:
    def __init__(self, n, sock, magic=MAGIC):
        self.n = n
        self.sock = sock
        self.magic = magic
        self.buf = b""
        self.closed = False
        self.last_ping = None
        self.got_verack = False
        self.sent_verack = False
        self.sync_pong = struct.pack("<Q", SYNC_NONCE + n)

    @property
    def handshaken(self):
        """Both `verack`s exchanged: dashd processes (and answers) a sync ping."""
        return self.got_verack and self.sent_verack

    def send(self, cmd, payload):
        try:
            self.sock.sendall(frame(cmd, payload, self.magic))
        except (ConnectionResetError, BrokenPipeError):
            pass  # the node closed first (a feeler); the next poll records it

    def poll(self, seconds):
        """Messages that arrive within `seconds`; ("closed", None) when the node closes."""
        out = []
        end = time.time() + seconds
        while not self.closed:
            left = end - time.time()
            if left <= 0:
                break
            self.sock.settimeout(left)
            try:
                chunk = self.sock.recv(1 << 20)
            except socket.timeout:
                break
            except (ConnectionResetError, BrokenPipeError):
                chunk = b""
            if not chunk:
                self.closed = True
                out.append(("closed", None))
                break
            self.buf += chunk
            msgs, self.buf = take_frames(self.buf, self.magic)
            out += msgs
        return out


class Transcript:
    def __init__(self, name, node, notes, ibd):
        """The header comments, then the node's state: its mock time, chain and IBD latch."""
        self.name = name
        self.node = node
        info = node.rpc("getnetworkinfo")
        args = " ".join(a for a in node.args[1:] if not a.startswith(("-datadir", "-rpc")))
        self.lines = [f"# {name}: dashd {info['subversion']} ({info['version']}), {node.chain}, fresh datadir, "
                      f"-mocktime={SIM_START}", f"# args: {args}"]
        self.lines += [f"# {n}" for n in notes]
        self.lines.append(f"mocktime {SIM_START}")
        for h in range(1, node.rpc("getblockcount") + 1):
            self.lines.append(f"chain {node.header(h).hex()}")
        self.lines.append(f"ibd {int(ibd)}")
        self.peers = {}

    def _record(self, peer, msgs):
        for cmd, payload in msgs:
            if cmd == "closed":
                self.lines.append(f"< {peer.n} closed")
                continue
            if cmd == "pong" and payload == peer.sync_pong:
                continue
            if cmd == "ping":
                peer.last_ping = payload
            if cmd == "verack":
                peer.got_verack = True
            self.lines.append(f"< {peer.n} {cmd} {payload.hex()}")

    def settle(self):
        """Waits for quiet, then syncs each handshaken peer with a ping."""
        while True:
            busy = False
            for p in self.peers.values():
                msgs = p.poll(QUIET if not busy else 0.2)
                if msgs:
                    busy = True
                self._record(p, msgs)
            if not busy:
                break
        for p in self.peers.values():
            if p.closed or not p.handshaken:
                continue
            p.send("ping", p.sync_pong)
            end = time.time() + 120
            done = False
            while not done and not p.closed and time.time() < end:
                msgs = p.poll(0.5)
                done = ("pong", p.sync_pong) in msgs
                self._record(p, msgs)
            if not done and not p.closed:
                raise RuntimeError(f"{self.name}: peer {p.n} sync ping not answered")

    def connect(self, n, conn_type):
        """The node dials peer `n` (`addconnection`, or `addnode onetry` for manual)."""
        listener = socket.socket()
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind(("127.0.0.1", 0))
        listener.listen(1)
        addr = "127.0.0.1:%d" % listener.getsockname()[1]
        # addconnection and addnode give the address NODE_NONE (what `version` echoes back).
        self.lines.append(f"connect {n} {conn_type} 0")
        if conn_type == "manual":
            self.node.rpc("addnode", addr, "onetry", False)
        else:
            self.node.rpc("addconnection", addr, conn_type, False)
        listener.settimeout(120)
        sock, _ = listener.accept()
        listener.close()
        self.peers[n] = Peer(n, sock)
        self.settle()

    def accept(self, n):
        """Peer `n` connects to the node."""
        sock = socket.create_connection(("127.0.0.1", self.node.port), timeout=30)
        self.lines.append(f"accept {n}")
        self.peers[n] = Peer(n, sock)
        self.settle()

    def send(self, n, cmd, payload=b""):
        self.lines.append(f"> {n} {cmd} {payload.hex()}")
        self.peers[n].send(cmd, payload)
        if cmd == "verack":
            self.peers[n].sent_verack = True
        self.settle()

    def pong(self, n):
        """Peer `n` answers the node's last ping."""
        p = self.peers[n]
        assert p.last_ping is not None, "no ping to answer"
        self.lines.append(f"> {n} pong echo")
        p.send("pong", p.last_ping)
        p.last_ping = None
        self.settle()

    def set_time(self, t):
        self.lines.append(f"time {t}")
        self.node.rpc("setmocktime", t)
        self.settle()

    def mine(self):
        h = self.node.mine(1)[0]
        self.lines.append(f"mine {h.hex()}")
        self.settle()

    def handshake(self, n, services):
        """`version` (after the node's when outbound), then `verack`."""
        self.send(n, "version", version_payload(services, VERSION_NONCE + n))
        self.send(n, "verack")

    def write(self, out_dir):
        for n, p in self.peers.items():
            if p.buf:
                raise FramingError(f"{self.name}: peer {n}: {len(p.buf)} bytes of an incomplete frame or packet left")
        path = pathlib.Path(out_dir) / f"{self.name}.txt"
        path.write_text("\n".join(self.lines) + "\n")
        for p in self.peers.values():
            p.sock.close()
        print(f"{path}: {sum(1 for l in self.lines if l.startswith('<'))} node messages")


@contextlib.contextmanager
def scenario(out, name, index, notes, premine=0):
    """A fresh node that mines `premine` blocks (leaving IBD) before its transcript starts; the
    transcript is written when the scenario's body ends."""
    node = Node(name, index, SIM_START)
    try:
        if premine:
            node.mine(premine)
        t = Transcript(name, node, notes, ibd=not premine)
        yield t
        t.write(out)
    finally:
        node.stop()


# ---- scenarios

HANDSHAKE_TYPES = ["inbound", "outbound-full-relay", "block-relay-only", "addr-fetch", "feeler", "manual"]


def scenario_handshake(out, conn_type, index):
    """Every connection type up to the first getheaders, then 10 s of silence (P2a §6.1)."""
    with scenario(out, f"handshake-{conn_type}", index, [
            "The window ends 10 s after the last message, with the mock time frozen."]) as t:
        if conn_type == "inbound":
            t.accept(1)
        else:
            t.connect(1, conn_type)
        t.handshake(1, FULL | BIT11)
        time.sleep(10)
        t.settle()


def miner_chain(n):
    """`n` headers mined by a separate regtest dashd whose clock runs 3000 s behind SIM_START,
    so the chain's times stay below the node's clock (block times step by about 1 s per 6
    blocks at a frozen mock time)."""
    cache = ROOT / f"miner-{n}-{hashlib.sha256(pathlib.Path(DASHD).read_bytes()).hexdigest()[:16]}.hex"
    if cache.exists():
        return [bytes.fromhex(l) for l in cache.read_text().split()]
    node = Node("miner", 9, SIM_START - 3000)
    try:
        headers = node.mine(n)
    finally:
        node.stop()
    cache.write_text("\n".join(h.hex() for h in headers))
    return headers


BUILD = {"headers": headers_payload, "headers2": headers2_payload}


def scenario_headers(out, name, index, services, cmd, count, notes):
    """A sync peer answers the node's first getheaders with `count` headers from another chain."""
    chain = miner_chain(8000)
    with scenario(out, name, index, notes + [
            "The headers come from a separate regtest dashd v24 (miner, mock time SIM_START-3000)."]) as t:
        t.connect(1, "outbound-full-relay")
        t.handshake(1, services)
        t.send(1, cmd, BUILD[cmd](chain[:count]))


def scenario_announce(out, index):
    """Block announcements: headers2, headers and inv peers, and a peer that lacks the parent."""
    with scenario(out, "announce", index, [
            "The node mined 3 blocks before the peers connect, so it is out of IBD.",
            "Peers 1-3 announce the node's tip back; peer 4 announces nothing."], premine=3) as t:
        tip = t.node.header(3)
        for n, pref in ((1, "sendheaders2"), (2, "sendheaders"), (3, None), (4, "sendheaders2")):
            t.accept(n)
            t.handshake(n, FULL | BIT11)
            if n != 4:
                t.send(n, "headers", headers_payload([tip]))
            if pref:
                t.send(n, pref)
        t.mine()
        t.mine()


def scenario_eviction(out, index):
    """ConsiderEviction: an outbound peer that never catches up with the node's tip."""
    with scenario(out, "evict-slow-chain", index, [
            "The node mined 2 blocks before the peer connects; the peer answers getheaders2 with none.",
            "The mock time moves in 60 s steps; the peer answers each ping."], premine=2) as t:
        t.connect(1, "outbound-full-relay")
        t.handshake(1, FULL | BIT11)
        t.send(1, "headers2", compact(0))
        p = t.peers[1]
        now = SIM_START
        while not p.closed and now < SIM_START + 30 * 60:
            now += 60
            t.set_time(now)
            if p.last_ping is not None and not p.closed:
                t.pong(1)


def selftest():
    """Negative controls: each framing fault a Core peer refuses or drops must stop a capture."""
    ping = frame("ping", bytes(8))
    assert take_frames(ping + ping[:30], MAGIC) == ([("ping", bytes(8))], ping[:30])

    def flipped(at):
        f = bytearray(ping)
        f[at] ^= 1
        return bytes(f)

    bad = {
        "a wrong magic": flipped(0),
        "another network's magic": frame("ping", bytes(8), bytes.fromhex("cee2caff")),
        "a bad checksum": flipped(20),
        "an unprintable command": ping[:5] + b"\x01" + ping[6:],
        "bytes after the command's padding": ping[:10] + b"x" + ping[11:],
        "a length over the cap": ping[:16] + struct.pack("<I", MAX_PROTOCOL_MESSAGE_LENGTH + 1) + ping[20:],
    }
    for what, data in bad.items():
        try:
            take_frames(data, MAGIC)
        except FramingError:
            continue
        sys.exit(f"selftest: {what} was not refused")

    # A peer's reads go through the same check.
    ours, node = socket.socketpair()
    with ours, node:
        node.sendall(bad["a wrong magic"])
        try:
            Peer(1, ours).poll(5)
        except FramingError:
            pass
        else:
            sys.exit("selftest: a peer recorded a frame with a wrong magic")

    held = types.SimpleNamespace(buf=ping[:30], sock=types.SimpleNamespace(close=lambda: None))
    t = Transcript.__new__(Transcript)
    t.name, t.lines, t.peers = "selftest", [], {1: held}
    try:
        with tempfile.TemporaryDirectory() as d:
            t.write(d)
    except FramingError:
        print(f"selftest: {len(bad) + 3} checks passed")
        return
    sys.exit("selftest: an incomplete trailing frame was written")


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    if sys.argv[1] == "selftest":
        selftest()
        return
    out = sys.argv[1]
    want = set(sys.argv[2:])
    pathlib.Path(out).mkdir(parents=True, exist_ok=True)
    ROOT.mkdir(parents=True, exist_ok=True, mode=0o700)
    jobs = [(f"handshake-{ct}", lambda o, ct=ct, i=i: scenario_handshake(o, ct, i))
            for i, ct in enumerate(HANDSHAKE_TYPES)]
    jobs += [
        ("headers-2000", lambda o: scenario_headers(o, "headers-2000", 6, FULL, "headers", 2000, [
            "A peer without bit 11 sends a full batch of 2000 headers: the node asks for more."])),
        ("headers-2000-bit11", lambda o: scenario_headers(o, "headers-2000-bit11", 6, FULL | BIT11, "headers", 2000, [
            "PQ-24: a bit-11 peer sends 2000 plain headers; its limit is 8000, so no more are asked."])),
        ("headers2-8000", lambda o: scenario_headers(o, "headers2-8000", 6, FULL | BIT11, "headers2", 8000, [
            "A bit-11 peer sends a full headers2 batch of 8000: the node asks for more."])),
        ("announce", lambda o: scenario_announce(o, 7)),
        ("evict-slow-chain", lambda o: scenario_eviction(o, 8)),
    ]
    for name, job in jobs:
        if not want or name in want:
            print(f"== {name}", flush=True)
            job(out)


if __name__ == "__main__":
    main()
