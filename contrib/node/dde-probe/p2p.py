"""A minimal V1 P2P client for sending raw payloads to a regtest dashd."""
import hashlib
import re
import socket
import struct
import time

import nodes

MAGIC = bytes.fromhex("fcc1b7dc")  # regtest pchMessageStart
PEER = re.compile(r"\bpeer=(\d+)\b")


def sha256d(b):
    return hashlib.sha256(hashlib.sha256(b).digest()).digest()


# 12-byte command strings around Core's transport checks, by name. V1 accepts
# 0x20-0x7e up to the first NUL and NULs after it (IsCommandValid); a V2
# long-form type also accepts 0x7f (GetMessageType).
COMMANDS = {
    "after_nul": b"ping\x00\x01",
    "byte_7f": b"pi\x7fng",
    "byte_1f": b"pi\x1fng",
    "byte_80": b"pi\x80ng",
    "space": b"pi ng",
    "empty": b"",
}
UNKNOWN = re.compile(r'Unknown command "(.*)" from peer=')


def frame(cmd, payload):
    """A V1 message; `cmd` is a str or the raw command bytes."""
    raw = cmd if isinstance(cmd, bytes) else cmd.encode()
    return MAGIC + raw.ljust(12, b"\0") + struct.pack("<I", len(payload)) + sha256d(payload)[:4] + payload


def peer_lines(lines, peer_id):
    """The lines naming exactly `peer_id`."""
    return [l for l in lines if any(int(m) == peer_id for m in PEER.findall(l))]


class Peer:
    """One inbound connection; records every message the node sends back."""

    def __init__(self, node):
        self.node = node
        self.log_start = nodes.log_path(node).stat().st_size
        self.sock = socket.create_connection(("127.0.0.1", nodes.p2p_port(node)), timeout=5)
        self.local = "127.0.0.1:%d" % self.sock.getsockname()[1]
        self.buf = b""
        self.received = []
        self.closed = False
        self.id = self._peer_id()

    def _peer_id(self):
        """The node's id for this connection, read before anything is sent."""
        for _ in range(50):
            p = self.peerinfo()
            if p:
                return p["id"]
            time.sleep(0.1)
        raise RuntimeError("connection not in getpeerinfo")

    def send(self, cmd, payload=b""):
        self.sock.sendall(frame(cmd, payload))

    def read(self, seconds):
        end = time.time() + seconds
        while time.time() < end and not self.closed:
            self.sock.settimeout(max(0.05, end - time.time()))
            try:
                chunk = self.sock.recv(65536)
            except socket.timeout:
                break
            except ConnectionResetError:
                chunk = b""
            if not chunk:
                self.closed = True
                break
            self.buf += chunk
            while len(self.buf) >= 24:
                n, = struct.unpack_from("<I", self.buf, 16)
                if len(self.buf) < 24 + n:
                    break
                self.received.append((self.buf[4:16].rstrip(b"\0").decode(), self.buf[24:24 + n]))
                self.buf = self.buf[24 + n:]
        return [c for c, _ in self.received]

    def sync(self, nonce=0x5EED):
        """Pings and waits for the pong: Core has then processed every earlier message."""
        self.send("ping", struct.pack("<Q", nonce))
        end = time.time() + 5
        while time.time() < end and not self.closed:
            self.read(0.2)
            if ("pong", struct.pack("<Q", nonce)) in self.received:
                return True
        return False

    def peerinfo(self):
        """This connection's full getpeerinfo entry, or None once it is gone."""
        for p in nodes.cli(self.node, "getpeerinfo"):
            if p["addr"] == self.local:
                return p
        return None

    def info(self):
        p = self.peerinfo()
        return p and {k: p.get(k) for k in ("version", "subver", "startingheight", "relaytxes")}

    def log(self):
        """debug.log lines about this connection since it opened.

        A line naming no peer (Core's "Exception ... caught") belongs to the
        peer named last, whose message ProcessMessages was handling.
        """
        with open(nodes.log_path(self.node), "rb") as f:
            f.seek(self.log_start)
            lines = f.read().decode(errors="replace").splitlines()
        out, current = [], None
        for line in lines:
            m = PEER.search(line)
            if m:
                current = int(m.group(1))
            if current == self.id:
                out.append(line.split(" ", 1)[-1])
        return out

    def close(self):
        self.sock.close()


def read_capture(path):
    """Messages from a -capturemessages file: (time, command, payload)."""
    data, i, out = open(path, "rb").read(), 0, []
    while i < len(data):
        t, = struct.unpack_from("<Q", data, i)
        cmd = data[i + 8:i + 20].rstrip(b"\0").decode()
        n, = struct.unpack_from("<I", data, i + 20)
        out.append((t, cmd, data[i + 24:i + 24 + n]))
        i += 24 + n
    return out
