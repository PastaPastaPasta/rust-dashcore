#!/usr/bin/env python3
"""Collect 10,000 outbound BIP35 inv intervals at one-second mock-time resolution.

Usage: timing.py OUT_JSON [PEERS=20] [SAMPLES_PER_PEER=500]
Each peer always has a pending mempool request. A ping/pong after each mock-time
step drains that pass's SendMessages. A prior barrier proves each mempool
request was processed before the timer advances. The first interval is
excluded as left-censored. The Rust companion uses the same one-second ticks.
"""
import hashlib
import importlib.util
import json
import pathlib
import select
import socket
import struct
import sys

spec = importlib.util.spec_from_file_location("g4_capture", pathlib.Path(__file__).with_name("capture.py"))
cap = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cap)
p2a = cap.p2a


def barrier(peers, nonce, callback, drain=True):
    payload = struct.pack("<Q", nonce)
    for peer in peers:
        peer.send("ping", payload)
    pending = {peer.n for peer in peers}
    while pending:
        ready, _, _ = select.select([peer.sock for peer in peers], [], [], 120)
        if not ready:
            raise RuntimeError("timing ping barrier timed out")
        for peer in peers:
            if peer.sock in ready:
                for cmd, body in peer.poll(0.0001):
                    if cmd == "pong" and body == payload:
                        pending.discard(peer.n)
                    elif cmd == "ping":
                        peer.send("pong", body)
                    elif cmd == "closed":
                        raise RuntimeError("timing peer disconnected")
                    callback(peer.n, cmd, body)

    if not drain:
        return
    # PONG precedes SendMessages in Core's pass. Drain that pass's INV before
    # advancing mock time, including TCP's delayed acknowledgement window.
    while True:
        ready, _, _ = select.select([peer.sock for peer in peers], [], [], 0.05)
        if not ready:
            break
        for peer in peers:
            if peer.sock in ready:
                for cmd, body in peer.poll(0.0001):
                    if cmd == "ping":
                        peer.send("pong", body)
                    elif cmd == "closed":
                        raise RuntimeError("timing peer disconnected")
                    callback(peer.n, cmd, body)


def run(out, count, samples):
    if cap.p3.dashd_sha256() != cap.DASHD_SHA256:
        sys.exit("requires the pinned Dash Core v24.0.0 final binary")
    node = p2a.Node("tx-timing", 5, cap.cap.SIM_START,
                    ("-acceptnonstdtxn=1", "-maxconnections=100"))
    peers = []
    try:
        node.mine(110)
        block = node.rpc("getblock", node.rpc("getblockhash", 1), 2)
        coinbase = block["tx"][0]
        raw = (struct.pack("<iB", 1, 1) + bytes.fromhex(coinbase["txid"])[::-1]
               + struct.pack("<IBIBqB", 0, 0, 0xffffffff, 1, coinbase["vout"][0]["valueSat"] - 10000, 25)
               + b"\x76\xa9\x14" + bytes([1]) * 20 + b"\x88\xac" + bytes(4))
        node.rpc("sendrawtransaction", raw.hex(), 0)
        for n in range(count):
            listener = socket.socket()
            listener.bind(("127.0.0.1", 0))
            listener.listen(1)
            node.rpc("addnode", f"127.0.0.1:{listener.getsockname()[1]}", "onetry", False)
            listener.settimeout(120)
            sock, _ = listener.accept()
            listener.close()
            peer = p2a.Peer(n, sock)
            peer.send("version", p2a.version_payload(cap.cap.FULL | cap.cap.BIT11, p2a.VERSION_NONCE + n))
            peer.send("verack", b"")
            peers.append(peer)
        barrier(peers, 1, lambda *_: None)
        intervals = [[] for _ in peers]
        last = [None for _ in peers]
        tick = 0
        late_invs = 0
        def received(n, cmd, at_tick):
            if cmd == "inv" and len(intervals[n]) < samples:
                if last[n] is not None:
                    intervals[n].append(at_tick - last[n])
                last[n] = at_tick
        while any(len(values) < samples for values in intervals):
            tick += 1
            for peer in peers:
                if len(intervals[peer.n]) < samples:
                    peer.send("mempool", b"")
            def before_tick(n, cmd, _):
                nonlocal late_invs
                if cmd == "inv":
                    late_invs += 1
                received(n, cmd, tick - 1)
            barrier(peers, tick * 2 + 1, before_tick, drain=False)
            node.rpc("setmocktime", cap.cap.SIM_START + tick)
            barrier(peers, tick * 2 + 2, lambda n, cmd, _: received(n, cmd, tick))
            if tick % 250 == 0:
                print(tick, sum(map(len, intervals)), flush=True)
        result = {"dashd_sha256": cap.DASHD_SHA256, "resolution_s": 1,
                  "peers": count, "samples_per_peer": samples,
                  "intervals_s": [value for values in intervals for value in values],
                  "ticks": tick, "late_invs_before_next_tick": late_invs, "transaction": raw.hex()}
        pathlib.Path(out).write_text(json.dumps(result, indent=2) + "\n")
        print("samples", len(result["intervals_s"]), "sha256", hashlib.sha256(pathlib.Path(out).read_bytes()).hexdigest())
    finally:
        for peer in peers:
            peer.sock.close()
        node.stop()


if __name__ == "__main__":
    run(sys.argv[1], int(sys.argv[2]) if len(sys.argv) > 2 else 20,
        int(sys.argv[3]) if len(sys.argv) > 3 else 500)
