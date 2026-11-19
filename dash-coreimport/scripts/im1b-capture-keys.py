#!/usr/bin/env python3
"""IM1b fixture capture: `onion_v3_private_key` and `i2p_private_key` written by a REAL Dash Core
v24.0.0-rc.3 regtest `dashd`, talking to two small MOCK servers (no `tor` / `i2pd` is needed).

Not part of Core. Run:
    python3 im1b-capture-keys.py --dashd <bindir>/dashd --outdir <out> [--workdir <dir>]

What the mocks provide and what dashd writes
  * Tor control mock (Core `torcontrol.cpp`): speaks PROTOCOLINFO / AUTHENTICATE (HASHEDPASSWORD,
    chosen because `-torpassword` is the simplest path) / GETINFO net/listeners/socks / ADD_ONION.
    For `ADD_ONION NEW:ED25519-V3` it generates a REAL v3 key (random seed -> Tor expanded secret
    key = clamp(SHA512(seed)[:32]) || SHA512(seed)[32:], public key, onion address) and replies
    `ServiceID=` + `PrivateKey=ED25519-V3:<b64 of 64 bytes>`. For `ADD_ONION ED25519-V3:<key>` it
    derives the public key from the supplied expanded key (pure-python Ed25519 scalar mult, no seed
    needed) and replies only `ServiceID=` as real Tor does. dashd writes the file.
  * SAM v3.1 mock (Core `i2p.cpp`): HELLO, DEST GENERATE SIGNATURE_TYPE=7 (a structurally real I2P
    Ed25519 private destination, 679 bytes), SESSION CREATE, STREAM ACCEPT (OK, then idle),
    NAMING LOOKUP / STREAM CONNECT (generic failures). dashd writes the file.
  * Two dashd runs on one datadir: run 1 with neither file (generate path), run 2 with both
    (cached path: Core reads the files and sends them back). Run 2 must round-trip byte-exactly.

NOTE: Core only creates the PERSISTENT SAM session (the one that uses `i2p_private_key`) when
`-i2pacceptincoming=1` AND `-listen=1` (`-listen=0` forces `-i2pacceptincoming=0`; net.cpp
`CConnman::Start`). With `-i2pacceptincoming=0` Core only uses TRANSIENT sessions and never writes
the file. So this script runs dashd with `-listen=1 -i2pacceptincoming=1`.

Key material is never printed; only sizes, short prefixes and sha256 go to stdout / the notes.
"""
import argparse
import base64
import hashlib
import json
import os
import re
import secrets
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat

TOR_PASSWORD = "im1b-throwaway-pw"
RPC_USER = "im1b"
RPC_PASS = "im1b-throwaway-rpc"
REGTEST_DEFAULT_PORT = 19899


# --------------------------------------------------------------------------------------------
# Ed25519 helpers (pure python scalar mult: derive the public key from a Tor expanded secret key)
# --------------------------------------------------------------------------------------------
_P = 2**255 - 19
_D = (-121665 * pow(121666, _P - 2, _P)) % _P
_L = 2**252 + 27742317777372353535851937790883648493
_I = pow(2, (_P - 1) // 4, _P)


def _inv(x):
    return pow(x, _P - 2, _P)


def _xrecover(y):
    xx = (y * y - 1) * _inv(_D * y * y + 1)
    x = pow(xx, (_P + 3) // 8, _P)
    if (x * x - xx) % _P != 0:
        x = (x * _I) % _P
    if x % 2 != 0:
        x = _P - x
    return x


_BY = 4 * _inv(5) % _P
_B = (_xrecover(_BY), _BY)


def _add(p, q):
    x1, y1 = p
    x2, y2 = q
    t = _D * x1 * x2 * y1 * y2
    x3 = (x1 * y2 + x2 * y1) * _inv(1 + t)
    y3 = (y1 * y2 + x1 * x2) * _inv(1 - t)
    return (x3 % _P, y3 % _P)


def _scalarmult(p, e):
    q = (0, 1)
    while e:
        if e & 1:
            q = _add(q, p)
        p = _add(p, p)
        e >>= 1
    return q


def _encode_point(p):
    x, y = p
    return (y | ((x & 1) << 255)).to_bytes(32, "little")


def pubkey_from_expanded(expanded64):
    """Public key from a Tor expanded secret key (first 32 bytes = clamped scalar, little endian)."""
    a = int.from_bytes(expanded64[:32], "little")
    return _encode_point(_scalarmult(_B, a))


def ed25519_pub_from_seed(seed):
    return Ed25519PrivateKey.from_private_bytes(seed).public_key().public_bytes(
        Encoding.Raw, PublicFormat.Raw)


def tor_expanded_key(seed):
    h = hashlib.sha512(seed).digest()
    a = bytearray(h[:32])
    a[0] &= 248
    a[31] &= 127
    a[31] |= 64
    return bytes(a) + h[32:]


def onion_address(pub):
    checksum = hashlib.sha3_256(b".onion checksum" + pub + b"\x03").digest()[:2]
    return base64.b32encode(pub + checksum + b"\x03").decode().lower().rstrip("=")


# --------------------------------------------------------------------------------------------
# I2P helpers
# --------------------------------------------------------------------------------------------
def i2p_b64(raw):
    return base64.b64encode(raw).decode().replace("+", "-").replace("/", "~")


def i2p_b64_decode(s):
    return base64.b64decode(s.replace("-", "+").replace("~", "/"), validate=True)


def i2p_b32(dest):
    return base64.b32encode(hashlib.sha256(dest).digest()).decode().lower().rstrip("=") + ".b32.i2p"


def make_i2p_private_destination():
    """Return (priv_blob, dest_blob, ed25519_pub). 679 bytes of private destination."""
    seed = secrets.token_bytes(32)
    pub = ed25519_pub_from_seed(seed)
    enc_pub = secrets.token_bytes(256)  # ElGamal-slot public key (crypto type 0): opaque to Core
    sign_slot = secrets.token_bytes(96) + pub  # 128 bytes: random padding, key right-justified
    cert = bytes([0x05]) + (4).to_bytes(2, "big") + (7).to_bytes(2, "big") + (0).to_bytes(2, "big")
    dest = enc_pub + sign_slot + cert
    assert len(dest) == 391
    enc_priv = secrets.token_bytes(256)
    priv = dest + enc_priv + seed
    assert len(priv) == 679
    return priv, dest, pub


# --------------------------------------------------------------------------------------------
# Mock servers
# --------------------------------------------------------------------------------------------
def redact(line):
    line = re.sub(r"(ED25519-V3:)\S+", lambda m: "%s<redacted %d chars>" % (m.group(1), len(m.group(0)) - 11), line)
    line = re.sub(r"(PRIV=|PUB=|DESTINATION=)(\S{41,})",
                  lambda m: "%s<redacted %d chars>" % (m.group(1), len(m.group(2))), line)
    line = line.replace('"' + TOR_PASSWORD + '"', '"<redacted>"')
    return line


class BaseMock:
    def __init__(self, name):
        self.name = name
        self.phase = "setup"
        self.events = []  # (phase, direction, text) -- already redacted
        self.lock = threading.Lock()
        self.conns = []
        self.stop = False
        self.srv = socket.socket()
        self.srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.srv.bind(("127.0.0.1", 0))
        self.srv.listen(8)
        self.port = self.srv.getsockname()[1]
        self.thread = threading.Thread(target=self._accept_loop, daemon=True)

    def start(self):
        self.thread.start()

    def log(self, direction, text):
        with self.lock:
            self.events.append((self.phase, direction, redact(text)))

    def phase_events(self, phase):
        with self.lock:
            return [e for e in self.events if e[0] == phase]

    def _accept_loop(self):
        while not self.stop:
            try:
                c, _ = self.srv.accept()
            except OSError:
                return
            with self.lock:
                self.conns.append(c)
            threading.Thread(target=self.handle, args=(c,), daemon=True).start()

    def close(self):
        self.stop = True
        try:
            self.srv.close()
        except OSError:
            pass
        with self.lock:
            conns = list(self.conns)
        for c in conns:
            try:
                c.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            try:
                c.close()
            except OSError:
                pass


class LineReader:
    def __init__(self, conn, terminator):
        self.conn = conn
        self.term = terminator
        self.buf = b""

    def readline(self):
        while self.term not in self.buf:
            try:
                d = self.conn.recv(4096)
            except OSError:
                return None
            if not d:
                return None
            self.buf += d
        line, self.buf = self.buf.split(self.term, 1)
        return line.decode("utf-8", "replace")


class TorMock(BaseMock):
    """Tor control port speaking just what torcontrol.cpp uses."""

    def __init__(self):
        super().__init__("tor")
        self.issued = None  # (expanded_key_b64_with_type_prefix, pub, address, seed-derived pub)
        self.add_onion_cmds = {}  # phase -> redacted ADD_ONION line
        self.add_onion_checks = {}  # phase -> dict of facts

    def handle(self, c):
        rd = LineReader(c, b"\r\n")
        while True:
            line = rd.readline()
            if line is None:
                return
            self.log("<-", line)
            reply = self.respond(line)
            self.log("->", reply.replace("\r\n", " | "))
            try:
                c.sendall(reply.encode())
            except OSError:
                return

    def respond(self, line):
        if line.startswith("PROTOCOLINFO"):
            return ('250-PROTOCOLINFO 1\r\n250-AUTH METHODS=HASHEDPASSWORD\r\n'
                    '250-VERSION Tor="0.4.8.10"\r\n250 OK\r\n')
        if line.startswith("AUTHENTICATE"):
            if line == 'AUTHENTICATE "%s"' % TOR_PASSWORD:
                return "250 OK\r\n"
            return "515 Authentication failed: bad password\r\n"
        if line.startswith("GETINFO net/listeners/socks"):
            return '250-net/listeners/socks="127.0.0.1:9050"\r\n250 OK\r\n'
        if line.startswith("ADD_ONION "):
            return self.add_onion(line)
        return "510 Unrecognized command\r\n"

    def add_onion(self, line):
        m = re.fullmatch(r"ADD_ONION (\S+) Port=(\d+),(\S+)", line)
        facts = {"phase": self.phase}
        if not m:
            facts["error"] = "unparsable ADD_ONION"
            self.add_onion_checks[self.phase] = facts
            return "512 Syntax error in ADD_ONION\r\n"
        keyspec, vport, target = m.groups()
        facts["virtual_port"] = int(vport)
        facts["target"] = target
        self.add_onion_cmds[self.phase] = redact(line)
        if keyspec == "NEW:ED25519-V3":
            facts["kind"] = "NEW"
            seed = secrets.token_bytes(32)
            expanded = tor_expanded_key(seed)
            pub = pubkey_from_expanded(expanded)
            facts["pub_matches_cryptography"] = (pub == ed25519_pub_from_seed(seed))
            addr = onion_address(pub)
            keyval = "ED25519-V3:" + base64.b64encode(expanded).decode()
            self.issued = {"keyval": keyval, "pub": pub, "address": addr}
            self.add_onion_checks[self.phase] = facts
            return "250-ServiceID=%s\r\n250-PrivateKey=%s\r\n250 OK\r\n" % (addr, keyval)
        if keyspec.startswith("ED25519-V3:"):
            facts["kind"] = "CACHED"
            expanded = base64.b64decode(keyspec[len("ED25519-V3:"):], validate=True)
            facts["key_blob_len"] = len(expanded)
            pub = pubkey_from_expanded(expanded)
            addr = onion_address(pub)
            facts["key_verbatim_vs_run1"] = bool(self.issued) and keyspec == self.issued["keyval"]
            facts["address_vs_run1"] = bool(self.issued) and addr == self.issued["address"]
            self.add_onion_checks[self.phase] = facts
            return "250-ServiceID=%s\r\n250 OK\r\n" % addr
        facts["error"] = "unknown key spec"
        self.add_onion_checks[self.phase] = facts
        return "513 Invalid argument\r\n"


class SamMock(BaseMock):
    """SAM v3.1 bridge speaking just what i2p.cpp uses."""

    def __init__(self):
        super().__init__("sam")
        self.generated = None  # dict(priv, dest, pub)
        self.session_create = {}  # phase -> facts (no key material)
        self.dest_generate_count = 0

    def send(self, c, text):
        self.log("->", text.rstrip("\n"))
        c.sendall(text.encode())

    def handle(self, c):
        rd = LineReader(c, b"\n")
        while True:
            line = rd.readline()
            if line is None:
                return
            self.log("<-", line)
            try:
                if not self.respond(c, line):
                    # STREAM ACCEPT is OK: the socket idles until the peer closes it
                    while rd.readline() is not None:
                        pass
                    return
            except OSError:
                return

    def respond(self, c, line):
        if line.startswith("HELLO VERSION"):
            self.send(c, "HELLO REPLY RESULT=OK VERSION=3.1\n")
        elif line.startswith("DEST GENERATE"):
            self.dest_generate_count += 1
            priv, dest, pub = make_i2p_private_destination()
            self.generated = {"priv": priv, "dest": dest, "pub": pub}
            self.send(c, "DEST REPLY PUB=%s PRIV=%s\n" % (i2p_b64(dest), i2p_b64(priv)))
        elif line.startswith("SESSION CREATE"):
            m = re.search(r" DESTINATION=(\S+)", line)
            facts = {"phase": self.phase, "style_stream": "STYLE=STREAM" in line}
            if not m or m.group(1) == "TRANSIENT":
                self.send(c, 'SESSION STATUS RESULT=I2P_ERROR MESSAGE="mock: transient unsupported"\n')
                facts["error"] = "no persistent destination"
            else:
                priv = i2p_b64_decode(m.group(1))
                facts["priv_len"] = len(priv)
                facts["cert_len_field"] = int.from_bytes(priv[385:387], "big")
                facts["verbatim_vs_generated"] = bool(self.generated) and priv == self.generated["priv"]
                self.send(c, "SESSION STATUS RESULT=OK DESTINATION=%s\n" % m.group(1))
            self.session_create[self.phase] = facts
        elif line.startswith("STREAM ACCEPT"):
            self.send(c, "STREAM STATUS RESULT=OK\n")
            return False
        elif line.startswith("NAMING LOOKUP"):
            name = line.split("NAME=", 1)[-1]
            self.send(c, "NAMING REPLY RESULT=KEY_NOT_FOUND NAME=%s\n" % name)
        elif line.startswith("STREAM CONNECT"):
            self.send(c, "STREAM STATUS RESULT=CANT_REACH_PEER\n")
        else:
            self.send(c, 'REPLY RESULT=I2P_ERROR MESSAGE="mock: unsupported"\n')
        return True


# --------------------------------------------------------------------------------------------
# dashd driver
# --------------------------------------------------------------------------------------------
def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    p = s.getsockname()[1]
    s.close()
    return p


def sha256_file(path):
    return hashlib.sha256(open(path, "rb").read()).hexdigest()


def file_mode(path):
    return "%o" % (os.stat(path).st_mode & 0o777)


class Dashd:
    def __init__(self, dashd, datadir, p2p, onion_target, rpc, tor, sam):
        self.dashd = dashd
        self.cli = os.path.join(os.path.dirname(dashd), "dash-cli")
        self.datadir = datadir
        self.rpc = rpc
        self.args = [
            dashd, "-regtest", "-datadir=" + datadir, "-printtoconsole=0",
            "-listen=1", "-bind=127.0.0.1:%d" % p2p, "-bind=127.0.0.1:%d=onion" % onion_target,
            "-listenonion=1", "-torcontrol=127.0.0.1:%d" % tor, "-torpassword=" + TOR_PASSWORD,
            "-i2psam=127.0.0.1:%d" % sam, "-i2pacceptincoming=1",
            "-debug=tor", "-debug=i2p",
            "-connect=0", "-dnsseed=0", "-discover=0", "-disablewallet",
            "-rpcport=%d" % rpc, "-rpcuser=" + RPC_USER, "-rpcpassword=" + RPC_PASS,
            "-rpcbind=127.0.0.1", "-rpcallowip=127.0.0.1",
        ]
        self.proc = None
        self.log_path = os.path.join(datadir, "regtest", "debug.log")

    def log_size(self):
        return os.path.getsize(self.log_path) if os.path.exists(self.log_path) else 0

    def start(self):
        self.proc = subprocess.Popen(self.args, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)

    def log_since(self, offset):
        if not os.path.exists(self.log_path):
            return ""
        with open(self.log_path, "rb") as f:
            f.seek(offset)
            return f.read().decode("utf-8", "replace")

    def wait_for_patterns(self, offset, patterns, timeout):
        deadline = time.time() + timeout
        while time.time() < deadline:
            text = self.log_since(offset)
            if all(re.search(p, text) for p in patterns):
                return True
            if self.proc.poll() is not None:
                err = self.proc.stderr.read().decode("utf-8", "replace")[-500:] if self.proc.stderr else ""
                raise RuntimeError("dashd exited early rc=%s stderr tail: %s" % (self.proc.returncode, err))
            time.sleep(0.5)
        return False

    def stop(self, timeout=180):
        if self.proc is None:
            return
        deadline = time.time() + timeout
        sent = False
        while time.time() < deadline and self.proc.poll() is None:
            if not sent:
                r = subprocess.run(
                    [self.cli, "-regtest", "-datadir=" + self.datadir, "-rpcport=%d" % self.rpc,
                     "-rpcuser=" + RPC_USER, "-rpcpassword=" + RPC_PASS, "stop"],
                    capture_output=True, text=True)
                sent = r.returncode == 0
            time.sleep(1)
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(30)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()
        if self.proc.stderr:
            self.proc.stderr.close()

    def kill(self):
        if self.proc and self.proc.poll() is None:
            self.proc.kill()
            self.proc.wait()


EXCERPT_RE = re.compile(
    r"\btor\b|\bTor\b|\bi2p\b|\bSAM\b|onion|torcontrol|i2paccept|ADD_ONION|AUTHENTICATE", re.I)


def excerpts(text, datadir):
    out = []
    for ln in text.splitlines():
        if EXCERPT_RE.search(ln):
            out.append(ln.replace(datadir, "<datadir>"))
    return out


def run(args):
    dashd = os.path.abspath(args.dashd)
    outdir = os.path.abspath(args.outdir)
    os.makedirs(outdir, exist_ok=True)
    base_tmp = os.environ.get("TMPDIR", "/work/tmp")
    workdir = args.workdir or tempfile.mkdtemp(prefix="im1b-capkeys-", dir=base_tmp)
    os.makedirs(workdir, exist_ok=True)
    datadir = os.path.join(workdir, "datadir")
    os.makedirs(datadir, mode=0o700, exist_ok=True)
    regdir = os.path.join(datadir, "regtest")

    tor = TorMock()
    sam = SamMock()
    tor.start()
    sam.start()
    p2p, onion_target, rpc = free_port(), free_port(), free_port()
    node = Dashd(dashd, datadir, p2p, onion_target, rpc, tor.port, sam.port)
    print("ports: p2p=%d onion-target=%d rpc=%d tor-control=%d sam=%d" % (
        p2p, onion_target, rpc, tor.port, sam.port))

    onion_file = os.path.join(regdir, "onion_v3_private_key")
    i2p_file = os.path.join(regdir, "i2p_private_key")
    result = {"problems": []}
    log_runs = {}
    try:
        for phase in ("run1-generate", "run2-cached"):
            tor.phase = sam.phase = phase
            if phase == "run1-generate":
                assert not os.path.exists(onion_file) and not os.path.exists(i2p_file)
            else:
                assert os.path.exists(onion_file) and os.path.exists(i2p_file)
                result["run1_onion_sha256"] = sha256_file(onion_file)
                result["run1_i2p_sha256"] = sha256_file(i2p_file)
            offset = node.log_size()
            print("[%s] starting dashd" % phase)
            node.start()
            ok = node.wait_for_patterns(
                offset,
                [r"Got tor service ID \w+, advertising service",
                 r"Persistent SAM session \w+ created, my address="],
                timeout=args.timeout)
            if not ok:
                result["problems"].append("%s: timed out waiting for Core's tor/i2p log lines" % phase)
            time.sleep(2)  # let the STREAM ACCEPT exchange happen
            node.stop()
            log_runs[phase] = node.log_since(offset)
            print("[%s] dashd stopped rc=%s" % (phase, node.proc.returncode))
            if node.proc.returncode not in (0, None):
                result["problems"].append("%s: dashd rc=%s" % (phase, node.proc.returncode))
        # ---- collect ----
        for p in (onion_file, i2p_file):
            if not os.path.exists(p):
                raise RuntimeError("missing %s" % p)
        onion_bytes = open(onion_file, "rb").read()
        i2p_bytes = open(i2p_file, "rb").read()
        result["onion_unchanged_after_run2"] = sha256_file(onion_file) == result["run1_onion_sha256"]
        result["i2p_unchanged_after_run2"] = sha256_file(i2p_file) == result["run1_i2p_sha256"]

        # Tor facts
        issued = tor.issued
        onion_ok_content = onion_bytes.decode() == issued["keyval"]
        logged_onion = None
        advertised = None
        m = re.search(r"Got tor service ID (\w+), advertising service (\S+)", log_runs["run1-generate"])
        if m:
            logged_onion, advertised = m.group(1) + ".onion", m.group(2)
        m2 = re.search(r"Got tor service ID (\w+), advertising service (\S+)", log_runs["run2-cached"])
        logged_onion_run2 = m2.group(1) + ".onion" if m2 else None
        expanded = base64.b64decode(onion_bytes.decode()[len("ED25519-V3:"):], validate=True)
        onion = {
            "file": "onion_v3_private_key",
            "size": len(onion_bytes),
            "sha256": hashlib.sha256(onion_bytes).hexdigest(),
            "mode_octal": file_mode(onion_file),
            "content_format": "ASCII 'ED25519-V3:' + standard base64 (with '=' padding) of the 64-byte "
                              "Tor expanded secret key; no trailing newline",
            "key_blob_len": len(expanded),
            "b64_len": len(onion_bytes) - len("ED25519-V3:"),
            "no_trailing_newline": not onion_bytes.endswith(b"\n"),
            "public_key_hex": issued["pub"].hex(),
            "public_key_rederived_from_file": pubkey_from_expanded(expanded).hex(),
            "onion_address_computed": issued["address"] + ".onion",
            "onion_address_core_logged": logged_onion,
            "onion_address_core_logged_run2": logged_onion_run2,
            "advertised_service_core_logged": advertised,
            "address_match": logged_onion == issued["address"] + ".onion",
            "file_equals_mock_PrivateKey_verbatim": onion_ok_content,
            "add_onion_run1": tor.add_onion_cmds.get("run1-generate"),
            "add_onion_run2": tor.add_onion_cmds.get("run2-cached"),
            "mock_checks": tor.add_onion_checks,
        }
        if not onion["address_match"]:
            result["problems"].append("onion address mismatch")
        if not onion_ok_content:
            result["problems"].append("onion file != mock PrivateKey value")
        c2 = tor.add_onion_checks.get("run2-cached", {})
        if not (c2.get("key_verbatim_vs_run1") and c2.get("address_vs_run1")):
            result["problems"].append("run2 ADD_ONION did not round-trip the key")
        if logged_onion_run2 != logged_onion:
            result["problems"].append("run2 onion address differs")

        # I2P facts
        gen = sam.generated
        dest = i2p_bytes[:391]
        cert_len = int.from_bytes(i2p_bytes[385:387], "big")
        m3 = re.search(r"Persistent SAM session \w+ created, my address=(\S+)", log_runs["run1-generate"])
        m4 = re.search(r"Persistent SAM session \w+ created, my address=(\S+)", log_runs["run2-cached"])
        logged_b32 = m3.group(1) if m3 else None
        logged_b32_run2 = m4.group(1) if m4 else None
        computed_b32 = i2p_b32(i2p_bytes[:387 + cert_len])
        i2p = {
            "file": "i2p_private_key",
            "size": len(i2p_bytes),
            "sha256": hashlib.sha256(i2p_bytes).hexdigest(),
            "mode_octal": file_mode(i2p_file),
            "content_format": "raw binary private destination: 391-byte Destination (256 enc pubkey slot "
                              "+ 128 signing slot [96 padding + 32-byte Ed25519 pubkey] + key cert "
                              "05 0004 0007 0000) + 256-byte encryption private key + 32-byte Ed25519 seed",
            "destination_len": 387 + cert_len,
            "cert_len_field_offset_385": cert_len,
            "cert_bytes_hex": i2p_bytes[384:391].hex(),
            "sig_type": int.from_bytes(i2p_bytes[387:389], "big"),
            "crypto_type": int.from_bytes(i2p_bytes[389:391], "big"),
            "signing_public_key_hex": dest[256 + 96:256 + 128].hex(),
            "signing_public_key_rederived_from_seed": ed25519_pub_from_seed(i2p_bytes[-32:]).hex(),
            "file_equals_mock_PRIV_verbatim": bool(gen) and i2p_bytes == gen["priv"],
            "b32_address_computed": computed_b32,
            "b32_address_core_logged_with_port": logged_b32,
            "b32_address_core_logged": logged_b32.rsplit(":", 1)[0] if logged_b32 else None,
            "b32_address_core_logged_run2": logged_b32_run2.rsplit(":", 1)[0] if logged_b32_run2 else None,
            "address_match": bool(logged_b32) and logged_b32.rsplit(":", 1)[0] == computed_b32,
            "dest_generate_calls_total": sam.dest_generate_count,
            "session_create": sam.session_create,
        }
        if not i2p["address_match"]:
            result["problems"].append("i2p address mismatch")
        if not i2p["file_equals_mock_PRIV_verbatim"]:
            result["problems"].append("i2p file != mock PRIV")
        if sam.dest_generate_count != 1:
            result["problems"].append("DEST GENERATE called %d times (expected 1)" % sam.dest_generate_count)
        s2 = sam.session_create.get("run2-cached", {})
        if not s2.get("verbatim_vs_generated"):
            result["problems"].append("run2 SESSION CREATE did not round-trip the destination")
        if i2p["b32_address_core_logged_run2"] != i2p["b32_address_core_logged"]:
            result["problems"].append("run2 i2p address differs")
        if i2p["signing_public_key_hex"] != i2p["signing_public_key_rederived_from_seed"]:
            result["problems"].append("i2p pubkey not consistent with seed")
        if not (result["onion_unchanged_after_run2"] and result["i2p_unchanged_after_run2"]):
            result["problems"].append("a key file changed in run 2")

        # ---- write outputs ----
        shutil.copyfile(onion_file, os.path.join(outdir, "onion_v3_private_key"))
        shutil.copyfile(i2p_file, os.path.join(outdir, "i2p_private_key"))
        expected = {
            "core": {"tag": "v24.0.0-rc.3", "dashd_sha256": sha256_file(dashd), "network": "regtest",
                     "default_port": REGTEST_DEFAULT_PORT},
            "dashd_args": [a.replace(datadir, "<datadir>") for a in node.args[1:]
                           if not a.startswith(("-rpcpassword", "-torpassword"))],
            "onion_v3_private_key": onion,
            "i2p_private_key": i2p,
            "run2": {"onion_unchanged": result["onion_unchanged_after_run2"],
                     "i2p_unchanged": result["i2p_unchanged_after_run2"]},
            "problems": result["problems"],
        }
        with open(os.path.join(outdir, "expected.json"), "w") as f:
            json.dump(expected, f, indent=2, sort_keys=True)
            f.write("\n")

        secrets_to_hide = [onion_bytes.decode(), onion_bytes.decode().split(":", 1)[1],
                           i2p_b64(i2p_bytes), base64.b64encode(i2p_bytes).decode(),
                           i2p_b64(dest), i2p_bytes.hex()]
        with open(os.path.join(outdir, "core-excerpts.log"), "w") as f:
            for phase in ("run1-generate", "run2-cached"):
                f.write("==== %s (debug.log lines matching tor/i2p/SAM/onion) ====\n" % phase)
                for ln in excerpts(log_runs[phase], datadir):
                    f.write(ln + "\n")
        with open(os.path.join(outdir, "mock-transcripts.log"), "w") as f:
            for mock in (tor, sam):
                f.write("#### %s mock (key material redacted) ####\n" % mock.name)
                for phase, direction, text in mock.events:
                    f.write("%s %s %s\n" % (phase, direction, text))
        for name in ("core-excerpts.log", "mock-transcripts.log", "expected.json"):
            body = open(os.path.join(outdir, name)).read()
            for s in secrets_to_hide:
                if s and s in body:
                    result["problems"].append("SECRET LEAK in %s" % name)

        print("onion: size=%d mode=%s sha256=%s prefix=%s" % (
            onion["size"], onion["mode_octal"], onion["sha256"][:16], onion_bytes[:11].decode()))
        print("onion address match=%s" % onion["address_match"])
        print("i2p: size=%d mode=%s sha256=%s" % (i2p["size"], i2p["mode_octal"], i2p["sha256"][:16]))
        print("i2p address match=%s" % i2p["address_match"])
        print("problems: %s" % (result["problems"] or "none"))
        # re-write expected.json problems in case a leak was found after writing
        expected["problems"] = result["problems"]
        with open(os.path.join(outdir, "expected.json"), "w") as f:
            json.dump(expected, f, indent=2, sort_keys=True)
            f.write("\n")
        return 0 if not result["problems"] else 1
    finally:
        node.kill()
        tor.close()
        sam.close()
        if not args.keep_workdir:
            shutil.rmtree(workdir, ignore_errors=True)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--dashd", required=True)
    ap.add_argument("--outdir", required=True)
    ap.add_argument("--workdir", help="fresh datadir parent (default: mkdtemp under $TMPDIR)")
    ap.add_argument("--keep-workdir", action="store_true")
    ap.add_argument("--timeout", type=int, default=240, help="seconds to wait for Core's log lines per run")
    sys.exit(run(ap.parse_args()))


if __name__ == "__main__":
    main()
