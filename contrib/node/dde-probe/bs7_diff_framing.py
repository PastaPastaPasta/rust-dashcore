#!/usr/bin/env python3
"""diff_v1_framing: base-sdk's V1 header and payload checks against v24's dashd.

Usage: DDE_PAIR=v24 bs7_diff_framing.py OUT.json CASES SEED
Needs the v24 pair running (-debug=net) and BS7_TOOL naming the built bs7-tool.
Cases go to the node under test only: v24.0.0-rc.3 is the parity reference
(DEC-82).

Each case sends one V1 message after a handshake: the regtest magic or a
wrong one; a `ping` (with a fresh nonce), a printable unknown command or
command bytes Core refuses (below 0x20, 0x7f, above 0x7f, or non-NUL after a
NUL); the right checksum or a wrong one; and the payload's length or one
above 3 MiB (then no payload follows). Core's verdict and its reason: disconnect for
a wrong magic or a length above 3 MiB (the connection closes after that
"Header error"), drop for a wrong checksum or an invalid command (that
"Header error", the ping unanswered, the peer kept) or deliver (the ping's
pong, or "Unknown command"). base-sdk's is bs7-tool frame, mapped to the
same reasons; the two must be equal. OUT.json keeps every
mismatch whole. It exits non-zero on a mismatch, a harness error or a node
without cases.
"""
import json
import os
import random
import struct
import subprocess
import sys
import threading
import time

import bs7_exit
import nodes
from p2p import MAGIC, UNKNOWN, Peer, sha256d

TOOL = os.environ["BS7_TOOL"]
MIB3 = 3 * 1024 * 1024


def command(rng):
    r = rng.random()
    if r < 0.5:
        return b"ping", "ping"
    if r < 0.72:
        # "bs7" keeps it clear of every command Core knows.
        name = "bs7" + "".join(rng.choice("abcdefghijklmnopqrstuvwxyz !~_-") for _ in range(rng.randint(0, 9)))
        return name.encode(), "unknown"
    if r < 0.75:
        return b"", "unknown"
    raw = bytearray(rng.choice([b"ping", b"bs7x", b"inv"]).ljust(12, b"\0"))
    kind = rng.choice(["low", "del", "high", "after_nul"])
    if kind == "after_nul":
        raw[rng.randint(5, 11)] = rng.randint(1, 255)
    else:
        raw[rng.randint(0, 2)] = {"low": rng.randint(1, 0x1F), "del": 0x7F, "high": rng.randint(0x80, 0xFF)}[kind]
    return bytes(raw), "invalid"


def frame(rng, nonce):
    magic = MAGIC if rng.random() < 0.85 else bytes(rng.getrandbits(8) for _ in range(4))
    cmd, kind = command(rng)
    payload = struct.pack("<Q", nonce) if kind != "unknown" else bytes(rng.getrandbits(8) for _ in range(rng.randint(0, 16)))
    length = len(payload)
    if rng.random() < 0.08:
        length = rng.choice([MIB3 + 1, MIB3 + 2, 1 << 31, 0xFFFFFFFF, 0x02000001])
        payload = b""
    checksum = sha256d(payload)[:4] if rng.random() < 0.8 else bytes(rng.getrandbits(8) for _ in range(4))
    header = magic + cmd.ljust(12, b"\0")[:12] + struct.pack("<I", length) + checksum
    return header + payload, kind


class Tool:
    def __init__(self):
        self.p = subprocess.Popen([TOOL, "frame"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        self.lock = threading.Lock()

    def verdict(self, data):
        with self.lock:
            self.p.stdin.write(json.dumps({"id": 0, "magic": MAGIC.hex(), "frame": data.hex()}) + "\n")
            self.p.stdin.flush()
            return json.loads(self.p.stdout.readline())["verdict"]


# Core's "Header error" reasons, and base-sdk's V1FrameError texts, by class.
CORE_REASONS = {"Wrong MessageStart": "disconnect: magic", "Size too large": "disconnect: length",
                "Wrong checksum": "drop: checksum", "Invalid message type": "drop: command"}
OUR_REASONS = {"wrong message start": "disconnect: magic", "message too large": "disconnect: length",
               "wrong checksum": "drop: checksum", "invalid message type": "drop: command"}


def core_verdict(p, nonce, kind):
    """Core's verdict with its reason. A disconnect must say why, and a
    dropped ping must stay unanswered."""
    log = p.log()
    reasons = [c for l in log for k, c in CORE_REASONS.items() if "Header error: " + k in l]
    pong = ("pong", struct.pack("<Q", nonce)) in p.received
    if p.closed:
        return reasons[0] if len(reasons) == 1 and reasons[0].startswith("disconnect") else "disconnect: ?"
    if reasons:
        # The peer is still connected: only a drop fits, with its ping unanswered.
        return reasons[0] if len(reasons) == 1 and reasons[0].startswith("drop") and not pong else "drop: ?"
    if kind == "ping" and pong:
        return "deliver"
    if kind != "ping" and any(UNKNOWN.search(l) for l in log):
        return "deliver"
    return "silent"


def our_verdict(text):
    if text == "deliver":
        return text
    return next((c for k, c in OUR_REASONS.items() if k in text), "?: " + text)


def run(node, base, cases, seed, tool, out):
    rng = random.Random(seed)
    for n in range(cases):
        nonce = (seed << 32) | n
        data, kind = frame(rng, nonce)
        try:
            p = Peer(node)
            p.send("version", base)
            p.read(1.0)
            p.send("verack")
            if not p.sync():
                raise RuntimeError("handshake")
            p.sock.sendall(data)
            p.sync(0x5EED0000 + n)
            time.sleep(0.05)
            p.read(0.1)
            core = core_verdict(p, nonce, kind)
            ours = our_verdict(tool.verdict(data))
            match = ours == core
            res = {"n": n, "kind": kind, "core": core, "base_sdk": ours, "match": match}
            if not match:
                res.update(frame=data[:64].hex(), log=p.log())
            out.append(res)
            p.close()
        except (RuntimeError, OSError) as e:
            out.append({"n": n, "error": str(e)})


def main():
    out_path, cases, seed = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
    base = bytes.fromhex(json.load(open(os.environ.get("BS7_PROBE_JSON", "/work/scratch/bs7/probe.json")))["base_version"])
    tool = Tool()
    results = {node: [] for node in nodes.UNDER_TEST}
    threads = [threading.Thread(target=run, args=(node, base, cases, seed + i, tool, results[node]))
               for i, node in enumerate(nodes.UNDER_TEST)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    summary = {}
    for node, rs in results.items():
        s = summary[node] = {"cases": len(rs), "errors": sum("error" in r for r in rs),
                             "mismatches": sum(r.get("match") is False for r in rs)}
        for r in rs:
            if "core" in r:
                key = "%s/%s" % (r["kind"], r["core"])
                s[key] = s.get(key, 0) + 1
    out = {"seed": seed, "summary": summary,
           "mismatches": [r for rs in results.values() for r in rs if r.get("match") is False],
           "errors": [r for rs in results.values() for r in rs if "error" in r][:20]}
    json.dump(out, open(out_path, "w"), indent=1)
    print(json.dumps(summary, indent=1))
    bs7_exit.check(bs7_exit.failures(summary))


if __name__ == "__main__":
    main()
