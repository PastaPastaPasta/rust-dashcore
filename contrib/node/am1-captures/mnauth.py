#!/usr/bin/env python3
"""Record Core MNAUTH vectors at v24.0.0.

``--mode out`` (the default) records the MNAUTH wire bytes a masternode sends
(evo/mnauth.cpp:19-49).  The target is an active masternode started from the
v0.0.4 regtest-mn ``network.json`` datadir.  The scripted peer uses one fixed
challenge for both directions.  Inbound mode dials the target; outbound mode
listens and asks the target's RPC ``addnode`` to dial it.

``--mode in`` records MNAUTH intake (evo/mnauth.cpp:51-177) on a receiving
masternode: two Core masternode legs and scripted peers; see the README.
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

    def __init__(self, binary, source, work, metadata, mn, pushversion, p2p=19950, extra=(),
                 sync=True):
        self.work = work
        shutil.copytree(source, work)
        work.chmod(0o700)
        (work / "dash.conf").write_text("")
        for name in ["peers.dat", "anchors.dat", "debug.log", "mempool.dat"]:
            (work / "regtest" / name).unlink(missing_ok=True)
        self.p2p = p2p
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
                     f"-masternodeblsprivkey={mn['bls_private_key']}", *extra]
        if pushversion is not None:
            self.args.append(f"-pushversion={pushversion}")
        self.stderr = open(work / "stderr.txt", "ab")
        self.proc = subprocess.Popen(self.args, stdout=self.stderr, stderr=self.stderr)
        try:
            wait(lambda: self.rpc("getblockchaininfo"), "target startup")
            for _ in range(2 if sync else 0):
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


# MNAUTH intake (--mode in): evo/mnauth.cpp:51-177 at v24.0.0.

INTAKE_LINES = ("CMNAuth::ProcessMessage", "Misbehaving:", "ProcessMessages(mnauth")
NODE_NETWORK = 1


def read_capture(path):
    """Parse one -capturemessages file (net.cpp:5048-5077 at v24.0.0)."""
    out = []
    if not path.exists():
        return out
    data = path.read_bytes()
    at = 0
    while at < len(data):
        when = struct.unpack_from("<q", data, at)[0]
        command = data[at + 8:at + 20].rstrip(b"\0").decode("ascii")
        size = struct.unpack_from("<I", data, at + 20)[0]
        out.append((when, command, data[at + 24:at + 24 + size]))
        at += 24 + size
    return out


def version_challenge(payload):
    """The MNAUTH challenge in a Core v24 ``version`` payload (net_processing.cpp:1583-1586)."""
    at = 4 + 8 + 8 + 26 + 26 + 8
    size = payload[at]
    at += 1 + size + 4 + 1
    return payload[at:at + 32]


class Log:
    """The target's debug.log from a byte offset."""

    def __init__(self, path):
        self.path = path
        self.at = 0

    def mark(self):
        self.at = self.path.stat().st_size if self.path.exists() else 0

    def since(self):
        with open(self.path, "rb") as f:
            f.seek(self.at)
            text = f.read().decode("utf-8", "replace")
        # Each line starts with its category, "[net] " or "[net|netconn] ".
        return [line for line in text.splitlines()
                if line.partition("] ")[2].startswith(INTAKE_LINES)]


def peers(target):
    return [{
        "id": p["id"],
        "addr": p["addr"],
        "inbound": p["inbound"],
        "connection_type": p["connection_type"],
        "verified_proregtx_hash": p.get("verified_proregtx_hash"),
        "verified_pubkey_hash": p.get("verified_pubkey_hash"),
        "banscore": p.get("banscore"),
    } for p in target.rpc("getpeerinfo")]


def operator(target, pro_tx_hash):
    """The stored operator key as protx info prints it, in the entry's own scheme
    (evo/dmnstate.cpp ToJson; bls/bls.h:495-513 at v24.0.0)."""
    state = target.rpc("protx", "info", pro_tx_hash)["state"]
    version = state.get("version")
    return {
        "pro_tx_hash": pro_tx_hash,
        "stored": state["pubKeyOperator"],
        "state_version": version,
        "stored_scheme": "legacy" if version == 1 else "basic",
    }


def process_scheme(target):
    forks = target.rpc("getblockchaininfo").get("softforks", {})
    v19 = forks.get("v19", {})
    return "basic" if v19.get("active", True) else "legacy"


def settle(target, check, label, timeout=30):
    return wait(lambda: check(peers(target)) or None, label, timeout)


def capture_dir(target, addr):
    return target.work / "regtest" / "message_capture" / addr.replace(":", "_")


def leg_record(name, target, log, before, peer, operator_info, me, process):
    """One MNAUTH from a Core masternode, as the receiver captured it."""
    base = capture_dir(target, peer["addr"])
    sent = read_capture(base / "msgs_sent.dat")
    recv = read_capture(base / "msgs_recv.dat")
    version = next(p for _, c, p in recv if c == "version")
    challenge = version_challenge(next(p for _, c, p in sent if c == "version"))
    mnauth_at, mnauth = next((t, p) for t, c, p in recv if c == "mnauth")
    after = {p["id"]: p for p in peers(target)}
    return {
        "name": name,
        "kind": "core-masternode",
        "synced": True,
        "process_scheme": process,
        "direction": "inbound" if peer["inbound"] else "outbound",
        "peer_id": peer["id"],
        "sent_challenge": challenge.hex(),
        "peer_version": struct.unpack_from("<i", version, 0)[0],
        "peer_services": struct.unpack_from("<Q", version, 4)[0],
        "operator": operator_info,
        "me": me,
        "pre_peers": before,
        "payload": mnauth.hex(),
        "observed": {
            "peer": after.get(peer["id"]),
            "pre_peers": [after.get(p["id"]) for p in before],
            "qsendrecsigs": [p.hex() for t, c, p in sent if c == "qsendrecsigs" and t >= mnauth_at],
            "log": log.since(),
        },
    }


def scripted_case(target, log, name, payload, services, synced, operator_info, me, process,
                  listen=None):
    """A scripted peer completes the handshake and sends ``payload``. It dials the target, or
    with ``listen`` the target dials it (an unsynced masternode refuses inbound peers)."""
    before = peers(target)
    log.mark()
    if listen is None:
        sock = socket.create_connection(("127.0.0.1", target.p2p), timeout=30)
        local = "127.0.0.1:%d" % sock.getsockname()[1]
    else:
        listener = socket.socket()
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind(("127.0.0.1", listen))
        listener.listen(1)
        target.rpc("addnode", f"127.0.0.1:{listen}", "onetry")
        listener.settimeout(60)
        sock, _ = listener.accept()
        listener.close()
        local = f"127.0.0.1:{listen}"
    try:
        address = struct.pack("<Q", services) + bytes(18)
        version = (struct.pack("<iQq", 70242, services, int(time.time())) + address + address
                   + struct.pack("<Q", 0xA3_0000_0000_0001) + bytes([len(USER_AGENT)]) + USER_AGENT
                   + bytes(4) + b"\x01" + bytes(32) + b"\x00")
        sock.sendall(frame("version", version))
        sock.sendall(frame("verack", b""))
        challenge = None
        got_verack = False
        data = b""
        while challenge is None or not got_verack:
            command, body, data = next_message(sock, data)
            if command == "version":
                challenge = version_challenge(body)
            elif command == "verack":
                got_verack = True
        peer = settle(target, lambda ps: next((p for p in ps if p["addr"] == local), None),
                      f"{name}: peer listed")
        time.sleep(1)
        sock.sendall(frame("mnauth", payload))
        time.sleep(2)
        after = {p["addr"]: p for p in peers(target)}
        return {
            "name": name,
            "kind": "scripted",
            "synced": synced,
            "process_scheme": process,
            "direction": "inbound" if listen is None else "outbound",
            "peer_id": peer["id"],
            "sent_challenge": challenge.hex(),
            "peer_version": 70242,
            "peer_services": services,
            "operator": operator_info,
            "me": me,
            "pre_peers": before,
            "payload": payload.hex(),
            "observed": {
                "peer": after.get(local),
                "log": log.since(),
            },
        }
    finally:
        sock.close()
        time.sleep(0.5)


def intake(args, metadata):
    """Core-to-Core legs and scripted cases against one receiving masternode."""
    receiver_mn = metadata["masternodes"][args.receiver_index]
    sender_mn = metadata["masternodes"][args.sender_index]
    capture = ["-capturemessages", "-deprecatedrpc=banscore"]
    records = []
    receiver = sender = unsynced = None
    try:
        receiver = Target(args.binary, args.network / receiver_mn["datadir"], args.work / "receiver",
                          metadata, receiver_mn, None, extra=capture)
        state = receiver.rpc("protx", "info", sender_mn["pro_tx_hash"])["state"]
        service = state["addresses"]["core_p2p"][0]
        sender = Target(args.binary, args.network / sender_mn["datadir"], args.work / "sender",
                        metadata, sender_mn, None, p2p=int(service.rsplit(":", 1)[1]))
        log = Log(receiver.work / "regtest" / "debug.log")
        me = receiver.rpc("masternode", "status")["proTxHash"]
        process = process_scheme(receiver)
        op = operator(receiver, sender_mn["pro_tx_hash"])
        synced = receiver.rpc("mnsync", "status")["IsBlockchainSynced"]
        if not synced:
            raise RuntimeError("receiver is not blockchain-synced")

        # Leg 1: the sender dials the receiver.
        before = peers(receiver)
        log.mark()
        sender.rpc("addnode", f"127.0.0.1:{receiver.p2p}", "onetry")
        leg = settle(receiver, lambda ps: next(
            (p for p in ps if p["inbound"] and p["verified_proregtx_hash"] == sender_mn["pro_tx_hash"]),
            None), "leg 1 verified")
        time.sleep(2)
        records.append(leg_record("core-inbound", receiver, log, before, leg, op, me, process))

        # Leg 2: the receiver dials the sender, which authenticates again (dedup walk).
        before = peers(receiver)
        log.mark()
        receiver.rpc("addnode", f"127.0.0.1:{sender.p2p}", "onetry")
        known = {p["id"] for p in before}

        def second(ps):
            new = [p for p in ps if p["id"] not in known and not p["inbound"]]
            return new[0] if new else None

        try:
            leg = settle(receiver, second, "leg 2 listed")
        except RuntimeError:
            leg = None
        time.sleep(3)
        if leg is not None and capture_dir(receiver, leg["addr"]).exists():
            records.append(leg_record("core-outbound-dedup", receiver, log, before, leg, op, me,
                                      process))
        else:
            records.append({"name": "core-outbound-dedup", "missing": "leg not captured",
                            "log": log.since()})

        sig = bytes.fromhex(records[0]["payload"])[32:]
        pro = bytes.fromhex(sender_mn["pro_tx_hash"])[::-1]
        full = NODE_NETWORK | (1 << 2)
        cases = [
            ("null-signature", pro + bytes(96), full),
            ("unknown-masternode", bytes([0x42]) * 32 + sig, full),
            ("wrong-challenge", pro + sig, full),
            ("null-protx", bytes(32) + sig, full),
            ("no-bloom-service", pro + sig, NODE_NETWORK),
            ("malleable-signature", pro + b"\xff" * 96, full),
            ("truncated", pro[:3], full),
        ]
        for name, payload, services in cases:
            records.append(scripted_case(receiver, log, name, payload, services, True, op, me,
                                         process))
        receiver.stop()
        receiver = None

        # A receiver that is not blockchain-synced reads nothing (evo/mnauth.cpp:57-60).
        unsynced = Target(args.binary, args.network / receiver_mn["datadir"],
                          args.work / "unsynced", metadata, receiver_mn, None, extra=capture,
                          sync=False)
        if unsynced.rpc("mnsync", "status")["IsBlockchainSynced"]:
            records.append({"name": "unsynced-truncated", "missing": "target synced at startup"})
        else:
            log = Log(unsynced.work / "regtest" / "debug.log")
            records.append(scripted_case(unsynced, log, "unsynced-truncated", pro[:3], full, False,
                                         op, me, process, listen=port()))
    finally:
        for t in (receiver, sender, unsynced):
            if t is not None:
                t.stop()
    return {
        "core_commit": "938fba502824b9d850b7c710601456dc3d71603e",
        "version": "v24.0.0",
        "core_lines": "evo/mnauth.cpp:51-177; net.cpp:4514-4539, :5048-5077",
        "network_sha256": hashlib.sha256((args.network / "network.json").read_bytes()).hexdigest(),
        "receiver": receiver_mn["pro_tx_hash"],
        "sender": sender_mn["pro_tx_hash"],
        "records": records,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--network", type=Path, required=True)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--mode", choices=("out", "in"), default="out")
    parser.add_argument("--port", type=int)
    parser.add_argument("--receiver-index", type=int, default=0)
    parser.add_argument("--sender-index", type=int, default=1)
    parser.add_argument("--challenge", default="11" * 32)
    parser.add_argument("--peer-version", type=int, default=70242)
    parser.add_argument("--mn-index", type=int, default=0)
    parser.add_argument("--pushversion", type=int,
                        help="the target's -pushversion value, if set")
    parser.add_argument("--process-scheme", choices=("basic", "legacy"), default="basic")
    parser.add_argument("--direction", choices=("inbound", "outbound"))
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
    if args.mode == "in":
        if args.work.exists():
            parser.error(f"work directory already exists: {args.work}")
        args.work.mkdir(mode=0o700, parents=True)
        result = intake(args, metadata)
        args.out.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        args.out.write_text(json.dumps(result, indent=2) + "\n")
        return
    if args.direction is None or args.port is None:
        parser.error("--mode out needs --direction and --port")
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
