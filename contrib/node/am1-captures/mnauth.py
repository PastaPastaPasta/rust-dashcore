#!/usr/bin/env python3
"""Record Core MNAUTH wire bytes (evo/mnauth.cpp:19-49 at v24.0.0).

The target is an already-running active masternode started from the v0.0.4
regtest-mn ``network.json`` datadir.  The scripted peer uses one fixed
challenge for both directions.  Inbound mode dials the target; outbound mode
listens and asks the target's RPC ``addnode`` to dial it.
"""

import argparse
import base64
import hashlib
import json
from pathlib import Path
import socket
import shutil
import struct
import subprocess
import time
import urllib.error
import urllib.request


MAGIC = bytes.fromhex("fcc1b7dc")  # regtest pchMessageStart
SERVICES = 1 | (1 << 2)
USER_AGENT = b"/mnauth-capture:0.1/"
MOCKTIME = 1700000063


def sha256d(payload):
    return hashlib.sha256(hashlib.sha256(payload).digest()).digest()


def frame(command, payload):
    return MAGIC + command.encode().ljust(12, b"\0") + struct.pack("<I", len(payload)) + sha256d(payload)[:4] + payload


def version_payload(version, challenge):
    address = struct.pack("<Q", SERVICES) + bytes(18)
    return (struct.pack("<iQq", version, SERVICES, int(time.time())) + address + address
            + struct.pack("<Q", 0xA2_0000_0000_0001) + bytes([len(USER_AGENT)]) + USER_AGENT
            + bytes(4) + b"\x01" + challenge + b"\x00")


def next_message(sock, data):
    sock.settimeout(30)
    while len(data) < 24:
        chunk = sock.recv(65536)
        if not chunk:
            raise RuntimeError("peer closed before the expected message")
        data += chunk
    size = struct.unpack_from("<I", data, 16)[0]
    while len(data) < 24 + size:
        chunk = sock.recv(65536)
        if not chunk:
            raise RuntimeError("peer closed before the expected message")
        data += chunk
    if data[:4] != MAGIC:
        raise RuntimeError("wrong network magic")
    payload = data[24:24 + size]
    if sha256d(payload)[:4] != data[20:24]:
        raise RuntimeError("bad message checksum")
    command = data[4:16].rstrip(b"\0").decode("ascii")
    return command, payload, data[24 + size:]


def messages(sock, initial=b""):
    data = initial
    while True:
        command, payload, data = next_message(sock, data)
        yield command, payload


def rpc(url, user, password, method, params):
    request = urllib.request.Request(
        url,
        data=json.dumps({"jsonrpc": "1.0", "id": 1, "method": method, "params": params}).encode(),
        headers={"Authorization": "Basic " + base64.b64encode(f"{user}:{password}".encode()).decode()},
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        result = json.load(response)
    if result["error"]:
        raise RuntimeError(result["error"])
    return result["result"]


def port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


class Target:
    """A Core masternode copied from the fixture, matching state.py's startup."""

    def __init__(self, binary, source, work, metadata, mn, pushversion):
        self.work = work
        shutil.copytree(source, work)
        work.chmod(0o700)
        (work / "dash.conf").write_text("")
        for name in ["peers.dat", "anchors.dat", "debug.log", "mempool.dat"]:
            (work / "regtest" / name).unlink(missing_ok=True)
        self.p2p = 19950
        self.rpcport = port()
        self.rpc_url = f"http://127.0.0.1:{self.rpcport}/"
        self.args = [str(binary), f"-datadir={work}", "-regtest", "-server", "-listen=1",
                     f"-port={self.p2p}", f"-bind=127.0.0.1:{self.p2p}",
                     f"-externalip=127.0.0.1:{self.p2p}", f"-rpcport={self.rpcport}",
                     "-rpcuser=am1", "-rpcpassword=am1", "-rpcbind=127.0.0.1",
                     "-rpcallowip=127.0.0.1", "-connect=0", "-listenonion=0", "-discover=0",
                     "-dnsseed=0", "-fixedseeds=0", "-upnp=0", "-natpmp=0", "-par=1",
                     "-parbls=1", "-fallbackfee=0.00001", f"-mocktime={MOCKTIME}",
                     "-debug=net", "-printtoconsole=0", "-v2transport=0", "-logtimestamps=0",
                     "-logthreadnames=0", *metadata["dashd_extra_args"],
                     f"-masternodeblsprivkey={mn['bls_private_key']}"]
        if pushversion is not None:
            self.args.append(f"-pushversion={pushversion}")
        self.stderr = open(work / "stderr.txt", "ab")
        self.proc = subprocess.Popen(self.args, stdout=self.stderr, stderr=self.stderr)
        try:
            wait(lambda: self.rpc("getblockchaininfo"), "target startup")
            for _ in range(2):
                self.rpc("mnsync", "next")
            tip = self.rpc("getblock", self.rpc("getbestblockhash"))
            self.rpc("setmocktime", tip["time"])
            time.sleep(2)
        except BaseException:
            self.stop()
            raise

    def rpc(self, method, *params):
        return rpc(self.rpc_url, "am1", "am1", method, params)

    def stop(self):
        if self.proc.poll() is None:
            try:
                self.rpc("stop")
                self.proc.wait(timeout=60)
            except (OSError, RuntimeError, subprocess.TimeoutExpired):
                pass
            if self.proc.poll() is None:
                self.proc.kill()
                self.proc.wait()
        self.stderr.close()


def wait(check, label, timeout=180):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        try:
            result = check()
            if result is not None:
                return result
        except (OSError, RuntimeError, urllib.error.URLError):
            pass
        time.sleep(0.1)
    raise RuntimeError(f"timeout: {label}")


def inbound(host, port, version, challenge):
    sock = socket.create_connection((host, port), timeout=30)
    sock.sendall(frame("version", version_payload(version, challenge)))
    sock.sendall(frame("verack", b""))
    return sock


def outbound(host, port, rpc_url, rpc_user, rpc_password, version, challenge):
    listener = socket.socket()
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind((host, port))
    listener.listen(1)
    rpc(rpc_url, rpc_user, rpc_password, "addnode", [f"{host}:{port}", "onetry"])
    listener.settimeout(60)
    sock, _ = listener.accept()
    listener.close()
    data = b""
    while True:
        command, _, data = next_message(sock, data)
        if command == "version":
            sock.sendall(frame("version", version_payload(version, challenge)))
            sock.sendall(frame("verack", b""))
            return sock, data
    raise RuntimeError("target closed before version")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--network", type=Path, required=True)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--challenge", default="11" * 32)
    parser.add_argument("--peer-version", type=int, default=70242)
    parser.add_argument("--mn-index", type=int, default=0)
    parser.add_argument("--pushversion", type=int,
                        help="the target's -pushversion value, if set")
    parser.add_argument("--process-scheme", choices=("basic", "legacy"), default="basic")
    parser.add_argument("--direction", choices=("inbound", "outbound"), required=True)
    parser.add_argument("--rpc-url")
    parser.add_argument("--rpc-user", default="am1")
    parser.add_argument("--rpc-password", default="am1")
    args = parser.parse_args()
    challenge = bytes.fromhex(args.challenge)
    if len(challenge) != 32:
        parser.error("--challenge must be 32 bytes")
    network = args.network / "network.json"
    if not network.exists():
        parser.error(f"missing network.json: {network}")
    metadata = json.loads(network.read_text())
    try:
        mn = metadata["masternodes"][args.mn_index]
    except (KeyError, IndexError):
        parser.error(f"network has no masternodes[{args.mn_index}]")
    if len(mn["bls_private_key"]) != 64:
        parser.error("network operator key is not 32 bytes")
    if args.work.exists():
        parser.error(f"work directory already exists: {args.work}")
    args.work.mkdir(mode=0o700, parents=True)
    target = None
    sock = None
    try:
        target = Target(args.binary, args.network / mn["datadir"], args.work / "mn", metadata, mn,
                        args.pushversion)
        if args.direction == "inbound":
            sock, initial = inbound(args.host, target.p2p, args.peer_version, challenge), b""
        else:
            if not args.rpc_url:
                args.rpc_url = target.rpc_url
            sock, initial = outbound(args.host, args.port, args.rpc_url, args.rpc_user,
                                     args.rpc_password, args.peer_version, challenge)
        payload = None
        for command, candidate in messages(sock, initial):
            if command == "mnauth":
                if len(candidate) != 128:
                    raise RuntimeError(f"unexpected mnauth payload length: {len(candidate)}")
                payload = candidate
                break
        if payload is None:
            raise RuntimeError("peer closed before sending mnauth")
        args.out.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        args.out.write_text(json.dumps({
            "core_lines": "evo/mnauth.cpp:19-49; bls/bls.h:177-180",
            "direction": args.direction,
            "challenge": args.challenge,
            "peer_version": args.peer_version,
            "pushversion": args.pushversion,
            "version": args.pushversion if args.pushversion is not None else 70242,
            "pro_tx_hash": mn["pro_tx_hash"],
            "public_key": mn["bls_public_key"],
            "process_scheme": args.process_scheme,
            "signature_scheme": "basic",
            "signature_serialization_scheme": args.process_scheme,
            "network_sha256": hashlib.sha256(network.read_bytes()).hexdigest(),
            "payload": payload.hex(),
        }, indent=2) + "\n")
    finally:
        if sock is not None:
            sock.close()
        if target is not None:
            target.stop()


if __name__ == "__main__":
    main()
