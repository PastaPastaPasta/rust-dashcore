#!/usr/bin/env python3
"""O13a's fingerprint lab v0: handshake transcripts per profile (docs/node-program/designs/O13a-notes.md).

The format, the node setup, the peers and the synchronisation are P2a-core's
(`contrib/node/p2a-transcripts/capture.py`, whose docstring defines them), with P2b's all-peer
quiet wait (`contrib/node/p2b-transcripts/capture.py`). This tool adds the profiles (dashd
arguments and chains), V2 (BIP324) peers, raw-byte inputs for the V1 framing cases, and a live
mode that runs one script against the rdashd binary and dashd with identical arguments.

Records added to the format (O13a D6):
  connect N TYPE SERVICES v2   the node opens a V2 connection (Core's `use_v2transport`)
  accept N v2                  peer N connects to the node and speaks V2 (it initiates)
  accept N refused             the node has no listener: the TCP connection was refused
  > N raw HEX                  peer N writes HEX to the socket as it is (no V1 framing)
  < N CMD HEX FORM             on a V2 connection, FORM is `short` (a short ID) or `long`

V2 uses Core's own BIP324 code: the functional test framework's `v2_p2p.py` at the tag
v24.0.0-rc.3 (`FP_CORE_TEST_FRAMEWORK` names its `test/functional` directory, e.g. from
`git -C ~/workspace/dash archive v24.0.0-rc.3 test/functional/test_framework | tar -x`).

Usage:
  P2A_DASHD=/path/to/dashd capture.py fixtures OUT_DIR [SCENARIO...]
  capture.py live --target BINARY --name dashd|rdashd OUT_DIR [SCENARIO...]

`fixtures` writes dashd's transcript per profile scenario (O13a's sim leg replays them against
rdashd's processor in `dash-conformance`'s `fingerprint` test). `live` dials nothing itself:
the node gets `-connect=127.0.0.1:PORT` to the tool's listener, so the same script runs against
either binary; `dash-conformance fingerprint DASHD NODE` compares two live transcripts.
`P2A_CAPTURE_DIR` defaults to /work/scratch/o13a/capture and `P2A_PORT_BASE` to 32000 (below
32768, outside the ephemeral range). It never touches /work/chains or /work/bench, and every
socket is on loopback.
"""

import importlib.util
import os
import pathlib
import socket
import struct
import sys
import time

os.environ.setdefault("P2A_CAPTURE_DIR", "/work/scratch/o13a/capture")
os.environ.setdefault("P2A_PORT_BASE", "32000")

_spec = importlib.util.spec_from_file_location(
    "p2b_capture", pathlib.Path(__file__).resolve().parent.parent / "p2b-transcripts" / "capture.py")
p2b = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(p2b)
p2a = p2b.p2a  # with P2b's all-peer `settle`

sys.path.insert(0, os.environ.get("FP_CORE_TEST_FRAMEWORK", "/work/scratch/o13a/core-tf/test/functional"))
# `messages.py` imports Core's X11 extension at load; no handshake hashes a block, so a stub that
# refuses any use stands in for it (as `dde-probe/bs8/gen_vectors.py` does).
if "dash_hash" not in sys.modules:
    import types
    _stub = types.ModuleType("dash_hash")

    def _no_x11(*_):
        raise RuntimeError("X11 is not needed by the fingerprint lab")

    _stub.getPoWHash = _no_x11
    sys.modules["dash_hash"] = _stub
from test_framework.v2_p2p import EncryptedP2PState, MSGTYPE_TO_SHORTID, SHORTID  # noqa: E402

SIM_START, FULL, BIT11, UA = p2a.SIM_START, p2a.FULL, p2a.BIT11, p2a.UA
BIT12 = 0x1000  # NODE_P2P_V2
V2_NET = {"regtest": "regtest", "testnet": "testnet3", "mainnet": "mainnet"}  # MAGIC_BYTES keys
MAX_PROTOCOL_MESSAGE_LENGTH = p2a.MAX_PROTOCOL_MESSAGE_LENGTH


class Msg(tuple):
    """A (cmd, payload) pair that also carries its V2 type form; equal to the plain pair, so
    P2a's and P2b's `("pong", nonce) in msgs` checks still work."""
    form = None


class V2Peer(p2a.Peer):
    """A scripted peer that speaks BIP324 through Core's `EncryptedP2PState`."""

    def __init__(self, n, sock, chain, initiating):
        super().__init__(n, sock, p2a.MAGICS[chain])
        self.state = EncryptedP2PState(initiating=initiating, net=V2_NET[chain])

    def _recv(self, timeout=120):
        self.sock.settimeout(timeout)
        try:
            chunk = self.sock.recv(1 << 20)
        except (ConnectionResetError, BrokenPipeError):
            chunk = b""
        if not chunk:
            self.closed = True
        return chunk

    def handshake(self):
        """BIP324 up to the peer's verified version packet; bytes after it stay in `buf`. A node
        that drops the connection (V2 off: it reads the key as V1 bytes) leaves the peer closed,
        with nothing kept."""
        import io
        buf = b""
        if self.state.initiating:
            self.sock.sendall(self.state.initiate_v2_handshake())
        else:
            while True:
                chunk = self._recv()
                if self.closed:
                    return
                buf += chunk
                used, reply = self.state.respond_v2_handshake(io.BytesIO(buf))
                buf = buf[used:]
                if reply == -1:
                    raise RuntimeError(f"peer {self.n}: the node spoke V1 to a V2 peer")
                if reply:
                    self.sock.sendall(reply)
                    break
        while True:
            used, reply = self.state.complete_handshake(io.BytesIO(buf))
            if reply:
                buf = buf[used:]
                self.sock.sendall(reply)
                break
            chunk = self._recv()
            if self.closed:
                return
            buf += chunk
        while True:
            used, ok = self.state.authenticate_handshake(buf)
            if not ok:
                raise RuntimeError(f"peer {self.n}: the node's version packet did not verify")
            buf = buf[used:]
            if self.state.tried_v2_handshake:
                self.buf = buf
                return
            chunk = self._recv()
            if self.closed:
                return
            buf += chunk

    def send(self, cmd, payload):
        name = cmd.encode()
        contents = (bytes([MSGTYPE_TO_SHORTID[name]]) if name in MSGTYPE_TO_SHORTID
                    else b"\0" + name.ljust(12, b"\0")) + payload
        try:
            self.sock.sendall(self.state.v2_enc_packet(contents))
        except (ConnectionResetError, BrokenPipeError):
            pass

    def poll(self, seconds):
        """Packets already in `buf` (the handshake's leftovers) count before any new bytes."""
        out = self._decode()
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
            out += self._decode()
        return out

    def _decode(self):
        out = []
        while True:
            used, contents = self.state.v2_receive_packet(self.buf)
            if used == 0:
                break
            if used == -1:
                raise RuntimeError(f"peer {self.n}: a packet did not verify")
            self.buf = self.buf[used:]
            if contents is None:
                raise RuntimeError(f"peer {self.n}: the node sent a decoy packet")
            if contents[0]:
                m = Msg((SHORTID[contents[0]].decode(), contents[1:]))
                m.form = "short"
            else:
                name = contents[1:13].rstrip(b"\0")
                if b"\0" in name:
                    raise RuntimeError(f"peer {self.n}: a long type with bytes after its padding")
                m = Msg((name.decode(), contents[13:]))
                m.form = "long"
            out.append(m)
        return out


class Transcript(p2a.Transcript):
    """P2a's transcript with V2 connections, refused connections and raw inputs."""

    def _record(self, peer, msgs):
        for m in msgs:
            before = len(self.lines)
            super()._record(peer, [m])
            if getattr(m, "form", None) and len(self.lines) > before:
                self.lines[-1] += " " + m.form

    def _peer(self, n, sock, v2, initiating):
        chain = self.node.chain
        if v2:
            p = V2Peer(n, sock, chain, initiating)
            p.handshake()
            if p.closed:
                self.lines.append(f"< {n} closed")
            # Packets that came with the version packet are already off the socket, so `settle`'s
            # select would not see them: record them in this segment.
            self._record(p, p._decode())
            return p
        return p2a.Peer(n, sock, p2a.MAGICS[chain])

    def connect_v(self, n, conn_type, v2):
        """The node dials peer `n`: `addconnection` on regtest, else `addnode onetry` (a manual
        connection); both take Core's `v2transport` argument."""
        listener = socket.socket()
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind(("127.0.0.1", 0))
        listener.listen(1)
        addr = "127.0.0.1:%d" % listener.getsockname()[1]
        self.lines.append(f"connect {n} {conn_type} 0" + (" v2" if v2 else ""))
        if conn_type == "manual":
            self.node.rpc("addnode", addr, "onetry", v2)
        else:
            self.node.rpc("addconnection", addr, conn_type, v2)
        listener.settimeout(120)
        sock, _ = listener.accept()
        listener.close()
        self.peers[n] = self._peer(n, sock, v2, initiating=False)
        self.settle()

    def accept_v(self, n, v2):
        """Peer `n` connects to the node; `refused` when the node has no listener."""
        try:
            sock = socket.create_connection(("127.0.0.1", self.node.port), timeout=30)
        except ConnectionRefusedError:
            self.lines.append(f"accept {n} refused")
            return
        self.lines.append(f"accept {n}" + (" v2" if v2 else ""))
        self.peers[n] = self._peer(n, sock, v2, initiating=True)
        self.settle()

    def send_raw(self, n, data):
        self.lines.append(f"> {n} raw {data.hex()}")
        try:
            self.peers[n].sock.sendall(data)
        except (ConnectionResetError, BrokenPipeError):
            pass
        self.settle()

    def handshake_v(self, n, services):
        """`version` then `verack`, unless the node already closed the connection."""
        if not self.peers[n].closed:
            self.handshake(n, services)


# ---- the sim leg's fixtures

# (profile, chain, dashd arguments). The base arguments are P2a's (`p2a.Node`); testnet and
# mainnet add `-connect=0`, so the node opens no connection of its own (O13a D1).
PROFILES = {
    "default": ("regtest", ()),
    "blocksonly": ("regtest", ("-blocksonly",)),
    "listen0": ("regtest", ("-listen=0",)),
    "v2off": ("regtest", ("-v2transport=0",)),
    "whitelist": ("regtest", ("-whitelist=127.0.0.1",)),
    "testnet": ("testnet", ("-connect=0",)),
    "mainnet": ("mainnet", ("-connect=0",)),
}


def node_for(name, index, profile):
    chain, extra = PROFILES[profile]
    return p2a.Node(name, index, SIM_START, extra, chain)


def handshake(t, direction, v2, services=None):
    """One peer up to the first getheaders, then 10 s of silence (P2a §6.1)."""
    if services is None:
        services = FULL | BIT11 | (BIT12 if v2 else 0)
    if direction == "in":
        t.accept_v(1, v2)
        if 1 not in t.peers:
            return
    else:
        conn_type = "outbound-full-relay" if t.node.chain == "regtest" else "manual"
        t.connect_v(1, conn_type, v2)
    t.handshake_v(1, services)
    time.sleep(10)
    t.settle()


def framing(t):
    """Behaviour 10 on an inbound V1 connection: a bad checksum and an invalid command are
    dropped and the connection stays (the sync ping proves it); a header over the 3 MiB cap
    disconnects. A second peer sends a frame with another network's magic after its handshake."""
    t.accept_v(1, False)
    t.handshake_v(1, FULL | BIT11)
    ping = struct.pack("<Q", 0x0123_4567_89AB_CDEF)
    good = p2a.frame("ping", ping)
    t.send_raw(1, good[:20] + bytes(4) + ping)  # checksum zeroed
    t.send_raw(1, p2a.MAGIC + b"pi\x01ng".ljust(12, b"\0") + good[16:])  # unprintable command
    # A whole header (zero checksum): a short one would be completed by the sync ping's bytes.
    t.send_raw(1, p2a.MAGIC + b"ping".ljust(12, b"\0") + struct.pack("<I", MAX_PROTOCOL_MESSAGE_LENGTH + 1) + bytes(4))
    t.accept_v(2, False)
    t.handshake_v(2, FULL | BIT11)
    t.send_raw(2, p2a.frame("ping", ping, p2a.MAGICS["mainnet"]))


def rekey_pings(t):
    """BIP324's rekey (every 224 packets each way, `REKEY_INTERVAL`): 230 pings to peer 1, each
    answered with a pong."""
    quiet, p2a.QUIET = p2a.QUIET, 0.3
    try:
        for i in range(230):
            t.send(1, "ping", struct.pack("<Q", 0x5100_0000 + i))
    finally:
        p2a.QUIET = quiet


def rekey(t):
    """`rekey_pings` on an outbound V2 connection."""
    t.connect_v(1, "outbound-full-relay", True)
    t.handshake_v(1, FULL | BIT11 | BIT12)
    rekey_pings(t)


def fixture_jobs():
    """(name, index, profile, body) per fixture; the index sets the ports."""
    jobs = []
    cases = {
        "default": [("out", False), ("out", True), ("in", False), ("in", True)],
        "blocksonly": [("out", False), ("in", False)],
        "listen0": [("out", False), ("in", False)],
        "v2off": [("out", False), ("in", False), ("in", True)],
        "whitelist": [("in", False), ("in", True)],
        "testnet": [("out", False), ("out", True), ("in", False), ("in", True)],
        "mainnet": [("out", False), ("out", True), ("in", False), ("in", True)],
    }
    for profile, legs in cases.items():
        for direction, v2 in legs:
            name = f"fp-{profile}-{direction}-{'v2' if v2 else 'v1'}"
            jobs.append((name, profile, lambda t, d=direction, v=v2: handshake(t, d, v)))
    jobs.append(("fp-default-framing", "default", framing))
    jobs.append(("fp-default-rekey", "default", rekey))
    return [(name, i, profile, body) for i, (name, profile, body) in enumerate(jobs)]


def capture_fixtures(out, want):
    for name, index, profile, body in fixture_jobs():
        if want and name not in want:
            continue
        print(f"== {name}", flush=True)
        node = node_for(name, index, profile)
        try:
            t = Transcript(name, node, [f"O13a profile `{profile}`."], ibd=True)
            body(t)
            t.write(out)
        finally:
            node.stop()


# ---- the live leg

class LiveNode(p2a.Node):
    """rdashd or dashd dialling only the tool's listener (`-connect`), with the arguments both
    binaries take; readiness is `getblockhash 0`, which rdashd has."""

    def __init__(self, binary, name, index, chain, connect, extra=()):
        self.name = name
        self.chain = chain
        self.dir = p2a.ROOT / name
        p2a.shutil.rmtree(self.dir, ignore_errors=True)
        self.dir.mkdir(parents=True, mode=0o700)
        self.port = None
        self.rpc_port = p2a.PORT_BASE + 2 * index + 1
        self.args = [binary, f"-datadir={self.dir}", *p2a.CHAIN_FLAG[chain], "-server", "-listen=0",
                     "-listenonion=0", "-discover=0", "-dnsseed=0", "-fixedseeds=0", "-upnp=0", "-natpmp=0",
                     "-debug=net", "-logips", "-printtoconsole=0", f"-connect={connect}",
                     f"-rpcport={self.rpc_port}", "-rpcbind=127.0.0.1", "-rpcallowip=127.0.0.1",
                     "-rpcuser=p2a", "-rpcpassword=p2a", f"-mocktime={SIM_START}", *extra]
        self.stderr = open(self.dir / "stderr.txt", "wb")
        self.proc = p2a.subprocess.Popen(self.args, stdout=p2a.subprocess.DEVNULL, stderr=self.stderr)
        end = time.time() + 900
        while True:
            try:
                self.rpc("getblockhash", 0)
                break
            except Exception:
                if self.proc.poll() is not None or time.time() > end:
                    raise RuntimeError(f"{name}: the node did not start; see {self.dir}/stderr.txt")
                time.sleep(0.5)


class LiveTranscript(Transcript):
    def __init__(self, name, node, target, notes):
        self.name = name
        self.node = node
        args = " ".join(a for a in node.args[1:] if not a.startswith(("-datadir", "-rpc", "-connect")))
        self.lines = [f"# {name}: {target}, {node.chain}, fresh datadir, -mocktime={SIM_START}",
                      f"# args: {args}"] + [f"# {n}" for n in notes]
        self.lines += [f"mocktime {SIM_START}", "ibd 1"]
        self.peers = {}

    def dialled(self, n, listener, v2):
        """The node's `-connect` connection to the tool's listener (a manual connection)."""
        self.lines.append(f"connect {n} manual 0" + (" v2" if v2 else ""))
        listener.settimeout(300)
        sock, _ = listener.accept()
        listener.close()
        self.peers[n] = self._peer(n, sock, v2, initiating=False)
        self.settle()


LIVE = {f"live-{chain}-{v}": (chain, v == "v2", False) for chain in ("regtest", "testnet", "mainnet")
        for v in ("v1", "v2")}
LIVE["live-regtest-rekey"] = ("regtest", True, True)


def capture_live(binary, target, out, want):
    for index, (name, (chain, v2, do_rekey)) in enumerate(LIVE.items()):
        if want and name not in want:
            continue
        print(f"== {name} ({target})", flush=True)
        listener = socket.socket()
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind(("127.0.0.1", p2a.PORT_BASE + 100 + 2 * index))
        listener.listen(1)
        connect = "127.0.0.1:%d" % listener.getsockname()[1]
        node = LiveNode(binary, f"{target}-{name}", 50 + index, chain, connect,
                        () if v2 else ("-v2transport=0",))
        try:
            t = LiveTranscript(name, node, target, ["O13a live leg: the node dials the tool (-connect)."])
            t.dialled(1, listener, v2)
            t.handshake_v(1, FULL | BIT11 | (BIT12 if v2 else 0))
            if do_rekey:
                rekey_pings(t)
            else:
                time.sleep(10)
                t.settle()
            t.write(out)
        finally:
            node.stop()


def main():
    if len(sys.argv) < 3 or sys.argv[1] not in ("fixtures", "live"):
        sys.exit(__doc__)
    if sys.argv[1] == "fixtures":
        out, want = sys.argv[2], set(sys.argv[3:])
        pathlib.Path(out).mkdir(parents=True, exist_ok=True)
        p2a.ROOT.mkdir(parents=True, exist_ok=True, mode=0o700)
        capture_fixtures(out, want)
        return
    import argparse
    ap = argparse.ArgumentParser()
    ap.add_argument("mode")
    ap.add_argument("--target", required=True, help="the node binary")
    ap.add_argument("--name", required=True, choices=("dashd", "rdashd"))
    ap.add_argument("out")
    ap.add_argument("scenarios", nargs="*")
    a = ap.parse_args()
    pathlib.Path(a.out).mkdir(parents=True, exist_ok=True)
    p2a.ROOT.mkdir(parents=True, exist_ok=True, mode=0o700)
    capture_live(a.target, a.name, a.out, set(a.scenarios))


if __name__ == "__main__":
    main()
