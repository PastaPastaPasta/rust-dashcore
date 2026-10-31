#!/usr/bin/env python3
"""Captures P4a's inbound-policy dashd transcript fixtures from regtest dashd v24.0.0-rc.3.

The node setup, the scripted peers, the synchronisation and the base format are P2a-core's
(`contrib/node/p2a-transcripts/capture.py`, whose docstring defines them) with P2b's all-peer
quiet wait (`contrib/node/p2b-transcripts/capture.py`). This tool adds P4a's scenarios
(`docs/node-program/designs/P4a-notes.md` D5, D10-D13): `-whitebind`/`-whitelist` permissions,
manual bans and discouragement (and the ban-expiry boundary, BL-214), eviction across several
netgroups, the verified-masternode eviction exemption, the 8000-header edge of `getheaders2`,
`-capturemessages`, and an unsolicited `qfcommit` (D8). Each writes
`dashd-p2p/tests/fixtures/dashd-v24/<scenario>.txt`.

Usage:
  P2A_DASHD=/path/to/dashd-24.0.0-rc.3/bin/dashd capture.py OUT_DIR [SCENARIO...]

`P2A_CAPTURE_DIR` defaults to /work/scratch/p4a/capture here; `P2A_PORT_BASE` (default 33900)
moves the ports. It never touches /work/chains or /work/bench.

Network namespace (D11): the tool re-executes itself under `unshare -rn` (a private user and
network namespace), brings `lo` up and adds each peer's source address to `lo` as a /32. The
addresses are public IPv4 (11.1.0.1, 12.7.0.1, ...), so dashd sees them routable, not local, and
each /16 is its own netgroup. dashd, its RPC and every peer stay inside the namespace; the host's
network is untouched.

Records added to P2a's format (one per line):
  accept N from ADDR [perm FLAGS]
                                  peer N connects to the node from source ADDR (IPv4), local
                                  port 40000+N; the replay uses ADDR:40000+N. `perm FLAGS` is
                                  the comma list (Core's names, `NetPermissions::ToStrings`
                                  order) of the `-whitebind` listener the peer dialled; absent
                                  for the plain `-bind` port. Plain `accept N` (P2a) is a peer
                                  from 127.0.0.1 with an ephemeral port on the plain port.
                                  `-whitelist` permissions come from the address and the
                                  fixture's `# args:` line, not from a record.
  setban TARGET add BANTIME ABSOLUTE
                                  input: `setban TARGET add BANTIME ABSOLUTE` through RPC
                                  exactly (BANTIME seconds, 0 = the default `-bantime`;
                                  ABSOLUTE 0|1). TARGET is an address or a subnet (with `/`).
  setban TARGET remove            input: `setban TARGET remove`.
  clearbanned                     input: `clearbanned`.
  masternode N PROREGTXHASH       input: inbound leg N is a second dashd that dialled the node
                                  (`addnode ... onetry`), completed the handshake and sent
                                  MNAUTH, recorded once the node's `getpeerinfo` shows
                                  `verified_proregtx_hash` = PROREGTXHASH for it. The `<` lines
                                  of its segment are what the node sent to the scripted peers
                                  meanwhile (an eviction its connect caused); the node's sends
                                  to N are not recorded (the tool cannot see them).
  banlist HEX                     check, not an input: the exact bytes of dashd's
                                  `<datadir>/regtest/banlist.json` at that point, after the
                                  preceding segment settled (Core saves on every `setban`,
                                  unban and `clearbanned`). `banlist -` if the file is absent.
  capture N recv|sent TIMEUS CMD HEX
                                  at the end of the scenario: dashd's `-capturemessages` files
                                  for scripted peer N, record by record in file order, the
                                  `msgs_recv.dat` records first, then `msgs_sent.dat`; TIMEUS
                                  is the record's time (mock time, microseconds), CMD the
                                  command without its NUL padding, HEX the payload. Each
                                  per-peer directory (`IP_port`) maps to N by the peer's local
                                  port. The files are rendered whole, so they include the
                                  tool's sync pings (nonce 0x7EA5000000000000 + N) and dashd's
                                  pongs to them, which the `>`/`<` lines leave out.

Unrecorded sync pings: the tool's own pings (P2a) are harmless except in `inbound-evict-mn`, where
dashd is a masternode and a peer's first message after `verack` ends its first-second eviction
protection; there the first ping after each peer's `verack` is recorded as an ordinary
`> N ping HEX` / `< N pong HEX` pair (nonce 0xF1A5000000000000 + N), see `settle_parallel`.
"""

import importlib.util
import os
import pathlib
import select
import shutil
import socket
import struct
import subprocess
import sys
import time
import types

os.environ.setdefault("P2A_CAPTURE_DIR", "/work/scratch/p4a/capture")
os.environ.setdefault("P2A_PORT_BASE", "33900")

_spec = importlib.util.spec_from_file_location(
    "p2b_capture", pathlib.Path(__file__).resolve().parent.parent / "p2b-transcripts" / "capture.py")
p2b = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(p2b)
p2a = p2b.p2a

SIM_START, FULL, BIT11 = p2a.SIM_START, p2a.FULL, p2a.BIT11
handshake, locator_payload, hash_of = p2b.handshake, p2b.locator_payload, p2b.hash_of
GENESIS_REGTEST = p2b.GENESIS_REGTEST
PEER_PORT = 40000  # + N: a scripted peer's local port when it dials from a source address
WHITE_PORT = p2a.PORT_BASE + 60  # + k: the -whitebind listeners

_added = {"127.0.0.1"}


def source(addr):
    """Puts `addr` on `lo` (inside the namespace) once."""
    if addr not in _added:
        subprocess.run(["ip", "addr", "add", f"{addr}/32", "dev", "lo"], check=True)
        _added.add(addr)


def accept_from(t, n, addr, port=None, perm=None):
    """Peer `n` connects to the node's `port` (default: the plain `-bind` port) from `addr`,
    local port 40000+n."""
    source(addr)
    sock = socket.socket()
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    sock.bind((addr, PEER_PORT + n))
    sock.settimeout(30)
    sock.connect(("127.0.0.1", port or t.node.port))
    t.lines.append(f"accept {n} from {addr}" + (f" perm {perm}" if perm else ""))
    t.peers[n] = p2a.Peer(n, sock)
    t.settle()


def setban(t, target, command, bantime=0, absolute=False):
    if command == "add":
        t.lines.append(f"setban {target} add {bantime} {int(absolute)}")
        t.node.rpc("setban", target, "add", bantime, absolute)
    else:
        t.lines.append(f"setban {target} remove")
        t.node.rpc("setban", target, "remove")
    t.settle()


def clearbanned(t):
    t.lines.append("clearbanned")
    t.node.rpc("clearbanned")
    t.settle()


def banlist(t):
    path = t.node.dir / "regtest" / "banlist.json"
    t.lines.append(f"banlist {path.read_bytes().hex()}" if path.exists() else "banlist -")


def mnlistdiff(t, n):
    """An unsolicited `mnlistdiff`: Core scores +100 before reading the payload (np:5654-5658)."""
    t.send(n, "mnlistdiff")


FIRST_PING_NONCE = 0xF1A5_0000_0000_0000  # + peer: the recorded first ping after verack


def settle_parallel(self):
    """P2b's settle, with the sync pings sent to every handshaken peer at once and their pongs
    awaited together (P2a/P2b ping one peer at a time, and `Peer.poll` waits out its whole
    window, so a settle there costs 0.5 s per peer: hours at 114 peers). The condition is the
    same: dashd answered a ping from each peer after QUIET seconds of silence.

    A peer's first ping after its `verack` is recorded (`> N ping`, nonce FIRST_PING_NONCE + N,
    and dashd's `< N pong`): it is the peer's first message after the handshake, which ends a
    masternode's first-second eviction protection for it (np:4368-4371,
    `nTimeFirstMessageReceived`), so it is an input the replay must deliver. Later sync pings
    change nothing and stay unrecorded."""
    while True:
        live = [p for p in self.peers.values() if not p.closed]
        ready, _, _ = select.select([p.sock for p in live], [], [], p2a.QUIET) if live else ([], [], [])
        if not ready:
            break
        for p in live:
            if p.sock in ready:
                self._record(p, p.poll(0.2))
    pending = {}
    for p in self.peers.values():
        if not p.closed and p.handshaken:
            nonce = p.sync_pong
            if not getattr(p, "first_ping_sent", False):
                p.first_ping_sent = True
                nonce = struct.pack("<Q", FIRST_PING_NONCE + p.n)
                self.lines.append(f"> {p.n} ping {nonce.hex()}")
            p.send("ping", nonce)
            pending[p.sock] = (p, ("pong", nonce))
    end = time.time() + 300
    while pending:
        if time.time() > end:
            raise RuntimeError(f"{self.name}: sync pings not answered by {sorted(p.n for p, _ in pending.values())}")
        ready, _, _ = select.select(list(pending), [], [], 1.0)
        for sock in ready:
            p, pong = pending[sock]
            msgs = p.poll(0.05)
            self._record(p, msgs)
            if p.closed or pong in msgs:
                del pending[sock]


scenario = p2b.scenario


def send_all(t, n, cmds):
    """Peer `n` sends each command (empty payload) until the node closes it."""
    for cmd in cmds:
        if t.peers[n].closed:
            return
        t.send(n, cmd)


# ---- D5: permissions and bans

def scenario_permissions(out, index):
    """`-whitebind` listeners with `noban`, `mempool` and both, beside the plain port, with
    `-peerbloomfilters=0`: an unsolicited `mnlistdiff` (+100) and a `mempool` without
    NODE_BLOOM offered."""
    perms = ["noban", "mempool", "noban,mempool"]
    port = {p: WHITE_PORT + k for k, p in enumerate(perms)}
    extra = ["-peerbloomfilters=0"] + [f"-whitebind={p}@127.0.0.1:{port[p]}" for p in perms]
    with scenario(out, "inbound-permissions", index, [
            "-peerbloomfilters=0 (no NODE_BLOOM) with a plain -bind port and -whitebind listeners",
            f"noban (:{port['noban']}), mempool (:{port['mempool']}) and noban,mempool "
            f"(:{port['noban,mempool']}). Every peer dials from its own 11.1.0.x address.",
            "Each peer completes the handshake, then sends mempool and/or an unsolicited mnlistdiff",
            "(empty payload). The node's mempool is empty, so a processed mempool sends nothing."],
            extra) as t:
        def peer(n, addr, perm, cmds):
            accept_from(t, n, addr, port[perm] if perm else None, perm)
            handshake(t, n, FULL)
            send_all(t, n, cmds)
        peer(1, "11.1.0.1", None, ["mempool", "mnlistdiff"])           # mempool: disconnected
        peer(2, "11.1.0.2", None, ["mnlistdiff", "mempool"])           # +100: discouraged
        peer(3, "11.1.0.3", "noban", ["mnlistdiff", "mempool"])        # neither punished
        peer(4, "11.1.0.4", "mempool", ["mempool", "mnlistdiff"])      # served, then +100
        peer(5, "11.1.0.5", "noban,mempool", ["mempool", "mnlistdiff"])  # neither punished
        peer(6, "11.1.0.2", None, ["mempool"])                         # discouraged, accepted
        peer(7, "11.1.0.4", "noban", ["mnlistdiff", "mempool"])        # discouraged, noban


def scenario_whitelist(out, index):
    """`-whitelist`: a subnet with the implicit flags (`-whitelistrelay=1`), one address with
    an explicit `mempool@`, and peers outside both, doing D5's misbehaviour."""
    extra = ["-peerbloomfilters=0", "-whitelist=11.2.0.0/16", "-whitelistrelay=1",
             "-whitelist=mempool@11.4.0.1"]
    with scenario(out, "inbound-whitelist", index, [
            "-peerbloomfilters=0, -whitelist=11.2.0.0/16 (implicit: noban, mempool, relay via",
            "-whitelistrelay=1), -whitelist=mempool@11.4.0.1 (explicit: mempool only). Peers dial",
            "the plain port from inside and outside the subnets, then send mempool and/or an",
            "unsolicited mnlistdiff (empty payload)."], extra) as t:
        def peer(n, addr, cmds):
            accept_from(t, n, addr)
            handshake(t, n, FULL)
            send_all(t, n, cmds)
        peer(1, "11.2.0.1", ["mempool", "mnlistdiff"])    # inside: served, not punished
        peer(2, "11.2.0.2", ["mnlistdiff", "mempool"])    # inside: not punished, served
        peer(3, "11.3.0.1", ["mempool", "mnlistdiff"])    # outside: disconnected
        peer(4, "11.3.0.2", ["mnlistdiff", "mempool"])    # outside: discouraged
        peer(5, "11.4.0.1", ["mempool", "mnlistdiff"])    # explicit mempool: served, then +100


def scenario_bans(out, index):
    """`setban` (address, subnet, relative, absolute, remove), `clearbanned`, and a discouraged
    peer at a nearly full inbound table (4 slots)."""
    with scenario(out, "inbound-bans", index, [
            "-maxconnections=15: 4 inbound slots. Peers dial the plain port from public addresses",
            "(a private network namespace). The bans are setban/clearbanned RPCs; banlist lines are",
            "dashd's banlist.json at that point."], ["-maxconnections=15"]) as t:
        def peer(n, addr):
            accept_from(t, n, addr)
            if not t.peers[n].closed:
                handshake(t, n, FULL)
        banlist(t)                                    # the empty list dashd writes at start
        peer(1, "11.1.0.1")
        peer(2, "11.1.0.1")                           # a second peer from the same address
        setban(t, "11.1.0.1", "add")                  # default -bantime: both disconnected
        banlist(t)
        peer(3, "11.1.0.1")                           # dropped (banned)
        peer(4, "12.7.0.1")
        peer(5, "12.7.0.2")
        setban(t, "12.7.0.0/16", "add", 3600)         # the subnet: both disconnected
        banlist(t)
        peer(6, "12.7.9.9")                           # dropped (banned by the subnet)
        setban(t, "11.1.0.1", "remove")
        banlist(t)
        peer(7, "11.1.0.1")                           # accepted again
        setban(t, "13.9.0.1", "add", 60)              # relative, 60 s
        banlist(t)
        peer(8, "13.9.0.1")                           # dropped (banned)
        t.set_time(SIM_START + 61)
        peer(9, "13.9.0.1")                           # the ban expired: accepted
        setban(t, "14.3.0.1", "add", SIM_START + 61 + 7200, True)  # absolute; sweeps 13.9.0.1
        banlist(t)
        peer(10, "14.3.0.1")                          # dropped (banned)
        peer(11, "15.5.0.1")
        mnlistdiff(t, 11)                             # +100: discouraged and disconnected
        peer(12, "15.5.0.1")                          # 2 inbound: accepted (2 + 1 < 4)
        peer(13, "15.5.0.1")                          # 3 inbound: dropped (discouraged)
        peer(14, "16.1.0.1")                          # 4 inbound: the table is full
        peer(15, "17.1.0.1")                          # full, every candidate protected: dropped
        mnlistdiff(t, 14)                             # frees a slot (16.1.0.1 discouraged)
        clearbanned(t)
        banlist(t)
        peer(16, "12.7.0.1")                          # the subnet ban is gone: accepted
        banlist(t)


# ---- D11: eviction across netgroups

def scenario_evict_groups(out, index, design):
    """Fills the inbound table from four /16 netgroups, then connects more peers so dashd
    evicts. `design` is (fill, newcomers), each a list of (group, absorbed) in connect order.

    Core's netgroup protection (`EraseLastKElements(.., CompareNetGroupKeyed, 4)`) protects the
    4 candidates with the largest keyed netgroups, and the keys are random per node, so which 4
    depends on the node. The design makes every eviction independent of that: the union over
    all 24 orders of the four keys (and over every eviction) of the 4 protected candidates is
    the "absorbed" set, whose peers do not relay transactions and answer the node's first ping
    at once (ping time 0 under the frozen clock); the other peers relay and never answer. So
    whatever 4 the keys protect, the 8 lowest-ping protection and the block-relay-only
    protection (non-relaying peers, at most 8) take the rest of the absorbed set, and the later
    protections (transaction time, block time, uptime) and the largest-netgroup pick see only
    the relaying peers, whose connect times are distinct (the mock time moves 1 s before every
    connect). `contrib/node/p4a-transcripts/evict_model.py` checks a design against
    `node/eviction.cpp`. The keys aside, ties (equal netgroups, equal ping times) still fall to
    libstdc++'s unstable `std::sort` over the candidates in `m_nodes` order (connect order, an
    evicted peer removed in place), as in every eviction fixture: the replay must keep that
    order (`dash_p2p::stdsort` ports GCC 13.3's sort)."""
    fill, newcomers = design
    groups = ["11.1", "12.2", "13.3", "14.4"]
    slots = len(fill)
    sizes = [sum(1 for g, _ in fill if g == k) for k in range(4)]
    with scenario(out, "inbound-evict-groups", index, [
            f"-maxconnections={slots + 11}: {slots} inbound slots, filled from four /16 netgroups",
            "(" + ", ".join(f"{groups[k]}.0.0/16: {sizes[k]}" for k in range(4)) + "), one address per peer;",
            f"then {len(newcomers)} more peers connect, each making dashd evict one. The mock time moves",
            "1 s before every connect. Peers with relay=0 in their version answer the node's ping at",
            "once; peers with relay=1 never do. Every eviction is the same whatever the order of",
            "dashd's keyed netgroups (the tool's design rule, checked against node/eviction.cpp)."],
            [f"-maxconnections={slots + 11}"]) as t:
        now = SIM_START
        count = [0, 0, 0, 0]
        for n, (g, absorbed) in enumerate(fill + newcomers, 1):
            now += 1
            t.set_time(now)
            count[g] += 1
            accept_from(t, n, f"{groups[g]}.0.{count[g]}")
            if t.peers[n].closed:
                continue
            handshake(t, n, FULL, relay=not absorbed)
            if absorbed:
                if t.peers[n].last_ping is None:
                    raise RuntimeError(f"peer {n}: no ping to answer, the design needs ping time 0")
                t.pong(n)


# ---- D12: the verified-masternode exemption

def start_node(name, index, extra=(), fresh=True):
    """P2a's `Node`, optionally keeping an existing datadir."""
    if fresh:
        return p2a.Node(name, index, SIM_START, extra)
    node = p2a.Node.__new__(p2a.Node)
    node.name, node.dir = name, p2a.ROOT / name
    node.port, node.rpc_port = p2a.PORT_BASE + 2 * index, p2a.PORT_BASE + 2 * index + 1
    # The argument list exactly as P2a's Node builds it.
    node.args = [p2a.DASHD, f"-datadir={node.dir}", "-regtest", "-server", "-listen=1", "-listenonion=0",
                 "-discover=0", "-dnsseed=0", "-fixedseeds=0", "-upnp=0", "-natpmp=0", "-debug=net",
                 "-logips", "-printtoconsole=0", f"-port={node.port}", f"-bind=127.0.0.1:{node.port}",
                 f"-rpcport={node.rpc_port}", "-rpcbind=127.0.0.1", "-rpcallowip=127.0.0.1",
                 "-rpcuser=p2a", "-rpcpassword=p2a", f"-mocktime={SIM_START}", *extra]
    node.stderr = open(node.dir / "stderr.txt", "ab")
    node.proc = subprocess.Popen(node.args, stdout=subprocess.DEVNULL, stderr=node.stderr)
    end = time.time() + 900
    while True:
        try:
            node.rpc("getblockcount")
            return node
        except Exception:
            if node.proc.poll() is not None or time.time() > end:
                raise RuntimeError(f"{name}: dashd did not start; see {node.dir}/stderr.txt")
            time.sleep(0.5)


def mn_setup(a_name, b_name, a_index, b_index):
    """Mines A's chain past DIP3 enforcement, registers A and B as masternodes on it (ProRegTx
    through `protx register_fund`, keys from `bls generate`), and copies the chain to B.
    Returns {name: (protx, bls secret)}; both nodes are stopped."""
    a = start_node(a_name, a_index)
    keys = {}
    try:
        a.rpc("createwallet", "w")
        addr = a.rpc("getnewaddress")
        a.rpc("generatetoaddress", 510, addr)  # past DIP3 enforcement (500): owner != voting
        for name, index in ((a_name, a_index), (b_name, b_index)):
            bls = a.rpc("bls", "generate")
            port = p2a.PORT_BASE + 2 * index
            protx = a.rpc("protx", "register_fund", a.rpc("getnewaddress"), f"127.0.0.1:{port}",
                          a.rpc("getnewaddress"), bls["public"], a.rpc("getnewaddress"), 0,
                          a.rpc("getnewaddress"), addr)
            keys[name] = (protx, bls["secret"])
        a.rpc("generatetoaddress", 10, addr)
        listed = set(a.rpc("protx", "list", "valid"))
        assert {p for p, _ in keys.values()} <= listed, f"protx list: {listed}"
    finally:
        a.stop()
    b_dir = p2a.ROOT / b_name
    shutil.rmtree(b_dir, ignore_errors=True)
    b_dir.mkdir(parents=True, mode=0o700)
    state = ("peers.dat", "anchors.dat", "banlist.json", "mempool.dat")
    skip = {"wallets", "debug.log", "settings.json", ".lock", ".cookie", *state}
    src = a.dir / "regtest"
    dst = b_dir / "regtest"
    dst.mkdir(mode=0o700)
    for entry in src.iterdir():
        if entry.name in skip:
            continue
        (shutil.copytree if entry.is_dir() else shutil.copy2)(entry, dst / entry.name)
    # A starts the scenario with no peers: drop what the setup left behind.
    for f in state:
        (src / f).unlink(missing_ok=True)
    return keys


def mn_ready(node):
    """Finishes the masternode sync (`mnsync next`, as Dash's functional tests do under a
    frozen mock time) and waits for the active masternode to be READY."""
    end = time.time() + 600
    while not node.rpc("mnsync", "status")["IsSynced"]:
        node.rpc("mnsync", "next")
        if time.time() > end:
            raise RuntimeError(f"{node.name}: mnsync did not finish")
    while node.rpc("masternode", "status").get("state") != "READY":
        if time.time() > end:
            raise RuntimeError(f"{node.name}: masternode status {node.rpc('masternode', 'status')}")
        time.sleep(1)


def scenario_evict_mn(out, a_index, b_index):
    """Node A (under test, an active masternode) with its 114 inbound slots full of scripted
    peers; masternode B dials it and authenticates; more scripted peers connect."""
    a_name, b_name = "inbound-evict-mn", "inbound-evict-mn-b"
    keys = mn_setup(a_name, b_name, a_index, b_index)
    a_protx, a_secret = keys[a_name]
    b_protx, b_secret = keys[b_name]
    a = start_node(a_name, a_index, [f"-masternodeblsprivkey={a_secret}", "-maxconnections=125"], fresh=False)
    b = None
    try:
        b = start_node(b_name, b_index, [f"-masternodeblsprivkey={b_secret}"], fresh=False)
        mn_ready(a)
        mn_ready(b)
        slots = 125 - 11
        t = p2a.Transcript(a_name, a, [
            "Node A is an active masternode (-masternodeblsprivkey; Core requires",
            "-maxconnections >= 125, so 114 inbound slots). A and B were registered on A's chain",
            "(protx register_fund; the chain was copied to B), both restarted with their operator",
            f"keys; A's proRegTx is {a_protx}, B's {b_protx}.",
            "Scripted peers dial from 11.1.0.1 (one netgroup). The mock time moves 1 s before",
            "every tenth peer. B dials A by `addnode <A> onetry` (from 127.0.0.1) once A's table",
            "is full; the masternode record is written when A's getpeerinfo shows B verified.",
            "The tool's sync pings go to all peers at once here (settle_parallel), and each peer's",
            "first ping after its verack is recorded (> N ping f1a5..., nonce 0xF1A5000000000000+N):",
            "as the first message after the handshake it ends the peer's first-second protection.",
            "-masternodeblsprivkey implies -disablewallet=1, -peerblockfilters=1 and",
            "-blockfilterindex=basic (init.cpp parameter interaction), so A offers NODE_COMPACT_FILTERS."],
            ibd=False)
        # The operator key is a throwaway regtest key, but a key all the same: keep it out.
        t.lines[1] = t.lines[1].replace(a_secret, "<operator-key>")
        t.settle = types.MethodType(settle_parallel, t)
        now = SIM_START

        def peer(n, hs=True, tick=True):
            nonlocal now
            if tick and n % 10 == 0:
                now += 1
                t.set_time(now)
            accept_from(t, n, "11.1.0.1")
            if hs and not t.peers[n].closed:
                handshake(t, n, FULL)
                p = t.peers[n]
                if n % 2 and p.last_ping is not None and not p.closed:
                    t.pong(n)
        for n in range(1, slots + 1):
            peer(n)
        n_b = slots + 1
        def b_legs():
            """A's inbound legs from B (127.0.0.1; the scripted peers are at 11.1.0.1)."""
            return [p for p in a.rpc("getpeerinfo") if p.get("inbound") and p["addr"].startswith("127.0.0.1:")]

        def b_verified(legs):
            return len(legs) == 1 and legs[0].get("verified_proregtx_hash") == b_protx
        assert not b_legs(), f"inbound legs from 127.0.0.1 before B dials: {b_legs()}"
        b.rpc("addnode", f"127.0.0.1:{a.port}", "onetry")
        end = time.time() + 600
        while True:
            legs = b_legs()
            if b_verified(legs):
                break
            if len(legs) > 1 or time.time() > end:
                raise RuntimeError(f"B not verified at A over exactly one leg: {legs}")
            time.sleep(1)
        t.lines.append(f"masternode {n_b} {b_protx}")
        t.settle()
        peer(n_b + 1)    # B does not count: accepted without an eviction
        peer(n_b + 2)    # full again: evicts a scripted peer, never B
        peer(n_b + 3)
        # The first-second protection (masternode mode only): a peer whose first message is not
        # yet received (or is MNAUTH) is protected while the mock time has not moved a second
        # since it connected.
        now += 1
        t.set_time(now)
        peer(n_b + 4, hs=False, tick=False)   # silent; its connect evicts one
        peer(n_b + 5, hs=False, tick=False)   # silent; evicts another, not n_b+4 (protected)
        handshake(t, n_b + 4, FULL)           # its first ping after verack (not MNAUTH) ends the protection
        peer(n_b + 6, hs=False, tick=False)   # evicts n_b+4; n_b+5 is still protected
        now += 1
        t.set_time(now)
        peer(n_b + 7, tick=False)             # a second later the silent peers are not protected
        legs = b_legs()
        assert b_verified(legs), f"B's leg: {legs}"
        t.write(out)
    finally:
        if b:
            b.stop()
        a.stop()


# ---- D13: the 8000-header edge

def scenario_serve_8000(out, index):
    with scenario(out, "inbound-serve-8000", index, [
            "The node mined 8001 blocks before the peers connect (out of IBD).",
            "Peer 1 has bit 11 at 70242; peer 2 lacks it, at 70234."], premine=8001) as t:
        t.accept(1)
        handshake(t, 1, FULL | BIT11)
        t.accept(2)
        handshake(t, 2, FULL, protocol=70234)
        t.send(1, "getheaders2", locator_payload([GENESIS_REGTEST]))      # 8000 headers2
        t.send(1, "getheaders2", locator_payload([hash_of(t, 8000)]))     # the last one
        t.send(2, "getheaders", locator_payload([GENESIS_REGTEST]))       # 2000 headers


# ---- D10: -capturemessages

def capture_records(t):
    """dashd's message_capture files for each scripted peer, as `capture` lines."""
    base = t.node.dir / "regtest" / "message_capture"
    by_name = {"%s_%d" % p.sock.getsockname(): n for n, p in t.peers.items()}
    dirs = {}
    for d in sorted(base.iterdir()):
        if d.name not in by_name:
            raise RuntimeError(f"unexpected capture directory {d.name}")
        dirs[by_name[d.name]] = d
    for n in sorted(dirs):
        for kind in ("recv", "sent"):
            f = dirs[n] / f"msgs_{kind}.dat"
            data = f.read_bytes() if f.exists() else b""
            off = 0
            while off < len(data):
                assert off + 24 <= len(data), f"truncated capture record in {dirs[n]}"
                ts, = struct.unpack_from("<q", data, off)
                cmd = data[off + 8:off + 20].rstrip(b"\0").decode()
                size, = struct.unpack_from("<I", data, off + 20)
                payload = data[off + 24:off + 24 + size]
                assert len(payload) == size, f"truncated capture record in {dirs[n]}"
                t.lines.append(f"capture {n} {kind} {ts} {cmd} {payload.hex()}")
                off += 24 + size


def scenario_capture(out, index):
    with scenario(out, "inbound-capture", index, [
            "-capturemessages. Two peers from 11.1.0.1 and 11.1.0.2 (local ports 40001, 40002):",
            "handshake, the mock time moves 5 s, a ping, getheaders/getheaders2 from genesis, then",
            "peer 2 sends an unsolicited mnlistdiff (+100: discouraged, disconnected). The capture",
            "lines are dashd's message_capture files, including the tool's sync pings and dashd's",
            "pongs."],
            ["-capturemessages"]) as t:
        accept_from(t, 1, "11.1.0.1")
        handshake(t, 1, FULL | BIT11)
        accept_from(t, 2, "11.1.0.2")
        handshake(t, 2, FULL, protocol=70234)
        t.set_time(SIM_START + 5)  # the capture records carry the node's mock time
        t.send(1, "ping", struct.pack("<Q", 0x1234_5678_9ABC_DEF0))
        if t.peers[1].last_ping is not None:
            t.pong(1)
        t.send(2, "getheaders", locator_payload([GENESIS_REGTEST], protocol=70234))
        t.send(1, "getheaders2", locator_payload([GENESIS_REGTEST]))
        mnlistdiff(t, 2)
        # Every capture record is written before the message is processed or queued, so the
        # settled state is complete.
        capture_records(t)


# ---- D8: P2P-12 at M2, an unsolicited `qfcommit`

# `qfcommit_100` and `qfcommit_10` from dash-oracle-sys/tests/data/bs8-core-overlay.json5: a
# version-4 commitment that decodes, and a version-1 one in the v4 layout that does not.
QFCOMMIT_OK = bytes.fromhex(
    "040001313131313131313131313131313131313131313131313131313131313131313103000a01010a0303000000"
    "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000032"
    "32323232323232323232323232323232323232323232323232323232323232000000000000000000000000000000"
    "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"
    "00000000000000000000000000000000000000000000000000000000000000000000009272721c3207e8506b818d"
    "aa024af2efb89a99fcff75df945ab36255b0f87bb4f7cf8362c7d20201cebb1b6786289a87181d2d6e8bded7c065"
    "a413f6c0f8100cb40e34380f1756ce99b7884da8abebfd2e0d026c4fe8c91aa3964a6537e6d5bf")
QFCOMMIT_BAD = bytes.fromhex(
    "01000131313131313131313131313131313131313131313131313131313131313131310a01010a03030000000000"
    "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000323232"
    "32323232323232323232323232323232323232323232323232323232320000000000000000000000000000000000"
    "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"
    "000000000000000000000000000000000000000000000000000000000000000000b77ade2e475e137b2b237f727a"
    "95fb4a27ce084c5a5dc5df3398c972a630f727c84a521e3bb19becb4fbe7c6a77c79c90cb0e212c0086b3c929114"
    "8d76e22c0ec857d9f256c9bbb87d9034e5b2266319ca20324ab0d0390e637e7c57bffa3f37")


def scenario_qfcommit(out, index):
    """Unsolicited `qfcommit` (P4a D8): +10 `unrequested quorum commitment` per decodable one
    (llmq/blockprocessor.cpp), nothing for one whose read throws first."""
    with scenario(out, "inbound-qfcommit", index, [
            "-debug=llmq (logging only). Three peers from 11.1.0.1-3 complete the handshake on a",
            "genesis-only node (IBD). Peer 1 sends a decodable qfcommit (bs8 corpus qfcommit_100,",
            "version 4) ten times, peer 2 nine times; peer 3 sends one whose read throws",
            "(qfcommit_10, version 1 in the v4 layout) ten times."], ["-debug=llmq"]) as t:
        for n in (1, 2, 3):
            accept_from(t, n, f"11.1.0.{n}")
            handshake(t, n, FULL)
        for n, payload, times in ((1, QFCOMMIT_OK, 10), (2, QFCOMMIT_OK, 9), (3, QFCOMMIT_BAD, 10)):
            for _ in range(times):
                if t.peers[n].closed:
                    break
                t.send(n, "qfcommit", payload)


def scenario_ban_expiry(out, index):
    """The ban-expiry boundary (Opus-h P4a r1 item 1, BL-214): `IsBanned` holds a ban while
    `now < nBanUntil` and `SweepBanned` keeps the entry while `now <= nBanUntil`
    (banman.cpp:98-124, 201), so at `now == nBanUntil` the address connects but the next save
    still lists the entry; one second later the save sweeps it."""
    with scenario(out, "inbound-ban-expiry", index, [
            "setban 13.9.0.1 add 60 at the start (ban until +60). Peers from 13.9.0.1 connect at +59",
            "(dropped) and +60 (accepted); setban of 13.9.0.2 at +60 and of 13.9.0.3 at +61 make",
            "dashd save banlist.json, the first still listing 13.9.0.1, the second without it."]) as t:
        banlist(t)
        setban(t, "13.9.0.1", "add", 60)
        banlist(t)
        t.set_time(SIM_START + 59)
        accept_from(t, 1, "13.9.0.1")                 # dropped: 59 < 60
        t.set_time(SIM_START + 60)
        accept_from(t, 2, "13.9.0.1")                 # accepted: 60 < 60 is false
        if not t.peers[2].closed:
            handshake(t, 2, FULL)
        setban(t, "13.9.0.2", "add", 3600)            # the save sweeps nothing: 60 > 60 is false
        banlist(t)
        t.set_time(SIM_START + 61)
        setban(t, "13.9.0.3", "add", 3600)            # the save sweeps 13.9.0.1: 61 > 60
        banlist(t)


# The inbound-evict-groups design (`evict_model.py` checks it): the netgroup (0-3) of each peer in
# connect order, the first 30 filling the table (groups of 1, 4, 7 and 18 peers), and the
# absorbed peers (non-relaying, ping answered at once).
EVICT_FILL = [3, 2, 3, 3, 1, 3, 3, 3, 3, 3, 1, 3, 2, 3, 3, 2, 2, 2, 2, 3, 0, 3, 3, 1, 3, 3, 1, 2, 3, 3]
EVICT_NEW = [1, 3, 1, 1, 3]
EVICT_ABSORBED = {1, 2, 5, 7, 8, 9, 10, 11, 12, 13, 14, 15, 17, 18, 21, 24, 27, 31, 33, 34}


def evict_design():
    peers = [(g, n in EVICT_ABSORBED) for n, g in enumerate(EVICT_FILL + EVICT_NEW, 1)]
    return peers[:len(EVICT_FILL)], peers[len(EVICT_FILL):]


def in_namespace():
    """Re-executes under `unshare -rn` unless already inside; brings `lo` up."""
    if os.environ.get("P4A_NETNS") != "1":
        env = dict(os.environ, P4A_NETNS="1")
        os.execvpe("unshare", ["unshare", "-rn", sys.executable, os.path.abspath(__file__), *sys.argv[1:]], env)
    subprocess.run(["ip", "link", "set", "lo", "up"], check=True)


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    in_namespace()
    os.umask(0o077)
    out = sys.argv[1]
    want = set(sys.argv[2:])
    pathlib.Path(out).mkdir(parents=True, exist_ok=True)
    p2a.ROOT.mkdir(parents=True, exist_ok=True, mode=0o700)
    jobs = [
        ("inbound-permissions", lambda o: scenario_permissions(o, 0)),
        ("inbound-whitelist", lambda o: scenario_whitelist(o, 1)),
        ("inbound-bans", lambda o: scenario_bans(o, 2)),
        ("inbound-evict-groups", lambda o: scenario_evict_groups(o, 3, evict_design())),
        ("inbound-evict-mn", lambda o: scenario_evict_mn(o, 4, 5)),
        ("inbound-serve-8000", lambda o: scenario_serve_8000(o, 6)),
        ("inbound-capture", lambda o: scenario_capture(o, 7)),
        ("inbound-qfcommit", lambda o: scenario_qfcommit(o, 8)),
        ("inbound-ban-expiry", lambda o: scenario_ban_expiry(o, 9)),
    ]
    for name, job in jobs:
        if not want or name in want:
            print(f"== {name}", flush=True)
            job(out)


if __name__ == "__main__":
    main()
