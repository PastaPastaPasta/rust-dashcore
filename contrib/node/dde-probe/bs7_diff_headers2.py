#!/usr/bin/env python3
"""diff_headers2: base-sdk's headers2 read against v24's regtest dashd.

Usage: DDE_PAIR=v24 bs7_diff_headers2.py OUT.json SECONDS SEED
Needs the v24 pair running with -debug=net,validation and a chain (bs7_probe.py
leaves them so), and BS7_TOOL naming the built bs7-tool. Cases go to the node
under test only: v24.0.0-rc.3 is the parity reference (DEC-82).

Each case mines 1 to 12 regtest headers (bs7-tool mine) on a random block
Core has, with versions from a pool of nine, and encodes them as a headers2
message with random compression choices: correct or wrong version offsets,
offsets past the list, compressed fields on the first header, explicit or
compressed previous hashes, deltas or full times, unused flag bits; some
cases are then cut, padded, given a wrong count or bit-flipped. Core's
verdict comes from its debug.log and block index:

- throw: "ProcessMessages(headers2, ...): Exception ... caught";
- too_many: "headers message size = N" (+20);
- otherwise Core read the message. base-sdk must have read the same message:
  as many headers as declared, and exactly the bytes an independent walk of
  the wire framing gives. Every header hash Core logs must be one base-sdk
  decoded, with the headers before it in Core's index; a rejection Core logs
  without a hash must fit base-sdk's headers (a continuity break, or more
  than 8 headers whose first parent Core lacks); with no rejection, every
  decoded header must be in Core's index. On every case the connection's
  state must match Core's log.

A thrown message must be refused by base-sdk too: the verdict, not the
error class, which no Core logic observes (DEC-98). A message base-sdk
refuses for a header Core's Compress never builds (BSX D-07) must be one Core
rejects after rebuilding it. At the end, negative controls mutate base-sdk's
side of matched cases (bytes read, headers, a read instead of a refusal,
connection state); each must fail.

base-sdk's verdict is bs7-tool predict (P2pMsg::decode_with_policy at the
common version). A case matches when the verdicts agree and the hash checks
hold. Mismatches are kept whole in OUT.json. It exits non-zero on a
mismatch, a harness error, an uncaught control or a node without cases or
controls.
"""
import json
import os
import random
import re
import struct
import subprocess
import sys
import threading
import time

import bs7_exit
import nodes
from p2p import Peer

TOOL = os.environ["BS7_TOOL"]
VERSIONS = [0x20000000, 0x20000001, 0x20000002, 0x20000004, 0x20000008, 0x20000010, 0x20000020, 4, 5]
HASH = re.compile(r"\b[0-9a-f]{64}\b")

class Tool:
    """One bs7-tool predict process; JSON lines in, JSON lines out."""

    def __init__(self):
        self.p = subprocess.Popen([TOOL, "predict"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        self.lock = threading.Lock()

    def predict(self, cmd, payload, version):
        with self.lock:
            self.p.stdin.write(json.dumps({"id": 0, "cmd": cmd, "payload": payload.hex(), "version": version}) + "\n")
            self.p.stdin.flush()
            return json.loads(self.p.stdout.readline())


def mine(prev_hex, versions, salt):
    r = subprocess.run([TOOL, "mine"], input=json.dumps({"prev": prev_hex, "versions": versions, "salt": salt}),
                       capture_output=True, text=True, check=True)
    return [bytes.fromhex(x) for x in json.loads(r.stdout)]


def compact(n):
    if n < 0xFD:
        return bytes([n])
    if n <= 0xFFFF:
        return b"\xfd" + struct.pack("<H", n)
    return b"\xfe" + struct.pack("<I", n)


class Versions:
    """Core's last_unique_versions, to choose correct offsets."""

    def __init__(self):
        self.items = []

    def find(self, v):
        return self.items.index(v) if v in self.items else None

    def save(self, v):
        self.items.insert(0, v)
        del self.items[7:]

    def mark(self, i):
        self.items.insert(0, self.items.pop(i))


def encode(headers, rng):
    """A headers2 payload for `headers` (80-byte each), with random choices."""
    out = b""
    versions = Versions()
    prev = None
    record_first = rng.random() < 0.5
    for i, hd in enumerate(headers):
        version, prev_hash, root = struct.unpack_from("<i", hd)[0], hd[4:36], hd[36:68]
        time_, bits, nonce = struct.unpack_from("<III", hd, 68)
        flags, fields = 0, b""
        if prev is None:
            # Core sends the first header whole; sometimes compress fields anyway.
            offset = 0
            if rng.random() < 0.1:
                offset = rng.randint(1, 7)
            if offset == 0:
                fields += struct.pack("<i", version)
            if record_first or offset:
                versions.save(version if offset == 0 else 0)
            flags |= offset
            if rng.random() > 0.05:
                flags |= 0x08
                fields += prev_hash
            fields += root
            if rng.random() > 0.05:
                flags |= 0x10
                fields += struct.pack("<I", time_)
            else:
                fields += struct.pack("<h", rng.randint(-5, 5))
            if rng.random() > 0.05:
                flags |= 0x20
                fields += struct.pack("<I", bits)
        else:
            pos = versions.find(version)
            r = rng.random()
            if pos is not None and r < 0.85:
                offset = pos + 1
                versions.mark(pos)
            elif r < 0.93:
                offset = 0
                versions.save(version)
            else:
                offset = rng.randint(1, 7)  # possibly past the list
                if offset <= len(versions.items):
                    versions.mark(offset - 1)
            flags |= offset
            if offset == 0:
                fields += struct.pack("<i", version)
            if rng.random() < 0.15:
                flags |= 0x08
                fields += prev_hash
            fields += root
            delta = (time_ - struct.unpack_from("<I", prev, 68)[0]) & 0xFFFFFFFF
            if delta <= 0x7FFF and rng.random() < 0.85:
                fields += struct.pack("<h", delta)
            else:
                flags |= 0x10
                fields += struct.pack("<I", time_)
            if bits != struct.unpack_from("<I", prev, 72)[0] or rng.random() < 0.15:
                flags |= 0x20
                fields += struct.pack("<I", bits)
        if rng.random() < 0.05:
            flags |= rng.choice([0x40, 0x80, 0xC0])
        out += bytes([flags]) + fields + struct.pack("<I", nonce)
        prev = hd
    return out


def mutate(count, body, rng):
    """The payload, its kind and the bytes base-sdk must read if Core reads it
    (None where the construction does not fix it)."""
    r = rng.random()
    if r < 0.70:
        p = compact(count) + body
        return p, "plain", len(p)
    if r < 0.78:
        return compact(count) + body[:rng.randrange(len(body))], "cut", None
    if r < 0.84:
        p = compact(count) + body
        return p + bytes(rng.randrange(1, 9)), "trailing", len(p)
    if r < 0.88:
        return compact(count + rng.randint(1, 3)) + body, "count_high", None
    if r < 0.91:
        return compact(rng.choice([8000, 8001, 2001])) + body[:50], "count_limit", None
    if r < 0.93:
        return compact(0), "empty", 1
    b = bytearray(body)
    for _ in range(rng.randint(1, 3)):
        b[rng.randrange(len(b))] ^= 1 << rng.randrange(8)
    return compact(count) + bytes(b), "bitflip", None


def walk(payload):
    """Bytes Core's read covers if it reads the whole message: the count, then
    each header's fields as its flags put them on the wire (block.h's
    serializer), written apart from base-sdk. None if the data ends first."""
    first = payload[0]
    pos = {0xFD: 3, 0xFE: 5, 0xFF: 9}.get(first, 1)
    for _ in range(declared_count(payload)):
        if pos >= len(payload):
            return None
        f = payload[pos]
        pos += 1 + (4 if f & 7 == 0 else 0) + (32 if f & 8 else 0) + 32 + (4 if f & 16 else 2) + (4 if f & 32 else 0) + 4
    return pos if pos <= len(payload) else None


def declared_count(payload):
    first = payload[0]
    if first < 0xFD:
        return first
    return struct.unpack_from("<H" if first == 0xFD else "<I", payload, 1)[0]


# Core's exception text -> class, as bs7-tool's class::core.
EXCEPTIONS = [("end of data", "eof"), ("non-canonical ReadCompactSize()", "noncanonical"),
              ("ReadCompactSize(): size too large", "size")]
EXCEPTION = re.compile(r"ProcessMessages\(headers2, \d+ bytes\): Exception '([^']*)'")
MISSING_PREV = re.compile(r"received header ([0-9a-f]{64}): missing prev block ([0-9a-f]{64})")
VALIDATION = ("Consensus::CheckBlockHeader", "Consensus::ContextualCheckBlockHeader", "is marked invalid",
              "prev block not found", "conflicts with chainlock")


def core_class(lines):
    for l in lines:
        m = EXCEPTION.search(l)
        if m:
            return next((c for text, c in EXCEPTIONS if text in m.group(1)), "other")
    return None


def check(case, lines, closed, pred, in_index):
    """Compares one case. `case` has payload, kind and expect; `lines` are
    Core's log lines since the message; `in_index(hash)` asks Core's block
    index. Returns (match, why)."""
    payload = bytes.fromhex(case["payload"])
    disconnected = any("DISCOURAGE THRESHOLD EXCEEDED" in l or "disconnecting peer" in l for l in lines)
    if closed != disconnected:
        return False, "connection closed=%s, Core's log says %s" % (closed, disconnected)
    exc = core_class(lines)
    if exc is not None:
        # The verdict only: no Core logic observes the error class (DEC-98).
        if pred["ok"]:
            return False, "Core threw %s, base-sdk read" % exc
        return True, "throw/" + exc
    if any("headers message size" in l for l in lines):
        return (not pred["ok"] and pred["kind"] == "too_many"), "too_many"
    misbehaving = [l for l in lines if "Misbehaving" in l]
    if not pred["ok"] and pred.get("class") == "strict":
        # A header Core's Compress never builds (BSX D-07): base-sdk refuses
        # the message, Core rebuilds it with zero fields and must reject it.
        rejected = misbehaving or any(MISSING_PREV.search(l) or any(k in l for k in VALIDATION) for l in lines)
        return bool(rejected), "strict/uncompress"
    if not pred["ok"]:
        return False, "Core read, base-sdk failed: %s" % pred.get("error")
    hashes, prevs = pred.get("hashes", []), pred.get("prevs", [])
    if len(hashes) != declared_count(payload):
        return False, "decoded %d headers of %d" % (len(hashes), declared_count(payload))
    want = walk(payload)
    if case["expect"] is not None and want != case["expect"]:
        return False, "harness: the walk gives %s, the construction %d" % (want, case["expect"])
    if pred["consumed"] != want:
        return False, "read %d bytes, Core's read covers %s" % (pred["consumed"], want)
    for l in lines:
        m = MISSING_PREV.search(l)
        if m:
            if (m.group(1), m.group(2)) != (hashes[0], prevs[0]) or in_index(prevs[0]):
                return False, "unconnecting: Core %s/%s, base-sdk %s/%s" % (m.group(1), m.group(2), hashes[0], prevs[0])
            return True, "rejected/unconnecting"
    for l in lines:
        if any(k in l for k in VALIDATION):
            found = HASH.findall(l)
            k = next((i for i, h in enumerate(hashes) if h in found), None)
            if k is None:
                return False, "Core rejected %s, not a header base-sdk decoded" % found
            if not all(in_index(h) for h in hashes[:k]):
                return False, "headers before the rejected one are not in Core's index"
            return True, "rejected/invalid"
    if any("non-continuous headers sequence" in l for l in misbehaving):
        broken = any(prevs[i] != hashes[i - 1] for i in range(1, len(hashes)))
        return broken, "rejected/non-continuous"
    if any("invalid header received" in l for l in misbehaving):
        return (len(hashes) > 8 and not in_index(prevs[0])), "rejected/unconnecting-many"
    if misbehaving:
        return False, "unexplained penalty: %s" % misbehaving
    if not all(in_index(h) for h in hashes):
        return False, "Core logged nothing, yet lacks base-sdk's headers"
    return True, "accepted" if hashes else "empty"


def negative_controls(cases, in_index):
    """Mutates base-sdk's side of matched cases, each mutation only where Core's
    verdict exposes the field: the connection state everywhere; for a read
    message the bytes read and an empty decode, and a changed header where
    Core indexed or logged the headers; for a thrown or strictly refused one a
    successful read. Every control must fail; returns counts by kind."""
    out = {}

    def control(kind, case, closed=None, pred=None):
        ok, _ = check(case, case["lines"], case["closed"] if closed is None else closed,
                      case["pred"] if pred is None else pred, in_index)
        n, failed = out.get(kind, (0, 0))
        out[kind] = (n + 1, failed + (not ok))

    for c in cases:
        pred, why = c["pred"], c["why"]
        control("connection", c, closed=not c["closed"])
        if why.startswith(("throw", "strict")):
            control("read_instead", c, pred={"ok": True, "consumed": 1, "hashes": [], "prevs": []})
        elif pred["ok"] and pred.get("hashes"):
            control("consumed", c, pred=dict(pred, consumed=pred["consumed"] + 1))
            control("empty", c, pred=dict(pred, hashes=[], prevs=[]))
            if why in ("accepted", "rejected/invalid", "rejected/unconnecting"):
                control("header", c, pred=dict(pred, hashes=["00" * 32] + pred["hashes"][1:]))
    return {k: {"controls": n, "failed_as_expected": f} for k, (n, f) in out.items()}


class Node:
    def __init__(self, node, base, seed):
        self.node = node
        self.base = base
        self.rng = random.Random(seed)
        self.peer = None
        self.peer_version = None
        self.index_cache = {}
        # Headers Core has, hex, to build on: start with the active chain's tail.
        self.known = []
        tip = nodes.cli(node, "getbestblockhash")
        for _ in range(20):
            self.known.append(nodes.cli(node, "getblockheader", tip, "false"))
            tip = nodes.cli(node, "getblockheader", tip)["previousblockhash"]

    def connect(self, version=70240):
        """A connection whose `version` message carried `version`; the common
        version is per connection, so another version needs a new one."""
        if self.peer and not self.peer.closed and self.peer_version == version:
            return
        if self.peer:
            self.peer.close()
        p = Peer(self.node)
        p.send("version", struct.pack("<i", version) + self.base[4:])
        p.read(1.0)
        p.send("verack")
        if not p.sync():
            raise RuntimeError("handshake")
        self.peer, self.peer_version = p, version

    def log_since(self, start):
        with open(nodes.log_path(self.node), "rb") as f:
            f.seek(start)
            return f.read().decode(errors="replace").splitlines()

    def in_index(self, h):
        if h not in self.index_cache:
            try:
                nodes.cli(self.node, "getblockheader", h)
                self.index_cache[h] = True
            except RuntimeError:
                self.index_cache[h] = False
        return self.index_cache[h]

    def case(self, tool, n):
        rng = self.rng
        base = rng.choice(self.known[-40:])
        count = rng.randint(1, 12)
        headers = mine(base, [rng.choice(VERSIONS) for _ in range(count)], rng.getrandbits(48))
        payload, kind, expect = mutate(count, encode(headers, rng), rng)
        version = rng.choice([70242, 70242, 70234, 70240]) if kind == "count_limit" else 70242
        self.connect(version)
        common = min(version, 70242)
        start = nodes.log_path(self.node).stat().st_size
        self.peer.send("headers2", payload)
        synced = self.peer.sync(0xC0FFEE + n)
        time.sleep(0.05)
        mine_ = re.compile(r"peer=%d\b" % self.peer.id)
        lines = [l for l in self.log_since(start) if mine_.search(l) or "peer=" not in l]
        pred = tool.predict("headers2", payload, common)
        closed = self.peer.closed or not synced
        res = {"n": n, "kind": kind, "expect": expect, "payload": payload.hex(), "common": common, "pred": pred,
               "closed": closed, "lines": lines}
        # Fresh headers only: a header Core already has would not be checked.
        self.index_cache = {h: v for h, v in self.index_cache.items() if v}
        ok, why = check(res, lines, closed, pred, self.in_index)
        res["match"], res["why"] = ok, why
        if ok and why == "accepted":
            for hd in headers:
                self.known.append(hd.hex())
        if closed:
            self.peer.close()
            self.peer = None
        return res


def run(node, base, seconds, seed, tool, results, controls):
    n = Node(node, base, seed)
    end = time.time() + seconds
    i = 0
    while time.time() < end:
        try:
            results.append(dict(n.case(tool, i), node=node))
        except (RuntimeError, OSError) as e:
            results.append({"node": node, "n": i, "error": str(e)})
            n.peer = None
        i += 1
    controls[node] = negative_controls([r for r in results if r.get("match")][:300], n.in_index)


def report(results, seed, seconds, done, controls=None):
    summary = {}
    for node, rs in results.items():
        rs = list(rs)
        s = summary[node] = {"cases": len(rs), "errors": sum("error" in r for r in rs),
                             "mismatches": sum(r.get("match") is False for r in rs)}
        for r in rs:
            if "why" in r and r.get("match"):
                key = r["why"].split(":")[0]
                s[key] = s.get(key, 0) + 1
    every = [r for rs in results.values() for r in list(rs)]
    return {"seed": seed, "seconds": round(seconds), "done": done, "summary": summary,
            "negative_controls": controls or {},
            "mismatches": [r for r in every if r.get("match") is False],
            "errors": [r for r in every if "error" in r][:20]}


def main():
    out_path, seconds, seed = sys.argv[1], float(sys.argv[2]), int(sys.argv[3])
    base = bytes.fromhex(json.load(open(os.environ.get("BS7_PROBE_JSON", "/work/scratch/bs7/probe.json")))["base_version"])
    tool = Tool()
    results = {node: [] for node in nodes.UNDER_TEST}
    controls = {}
    threads = [threading.Thread(target=run, args=(node, base, seconds, seed + i, tool, results[node], controls))
               for i, node in enumerate(nodes.UNDER_TEST)]
    t0 = time.time()
    for t in threads:
        t.start()
    # A checkpoint every minute, so an interrupted run keeps its results.
    while any(t.is_alive() for t in threads):
        threads[0].join(60)
        json.dump(report(results, seed, time.time() - t0, False), open(out_path, "w"), indent=1)
    for t in threads:
        t.join()
    out = report(results, seed, time.time() - t0, True, controls)
    json.dump(out, open(out_path, "w"), indent=1)
    print(json.dumps({"summary": out["summary"], "negative_controls": controls}, indent=1))
    # Fails on a mismatch, a harness error, a node without cases or controls,
    # or a control the comparison did not catch.
    missed = [k for c in controls.values() for k, v in c.items() if v["failed_as_expected"] != v["controls"]]
    bs7_exit.check(bs7_exit.failures(out["summary"], **{
        "controls not caught": missed, "no controls": [n for n in out["summary"] if not controls.get(n)]}))


if __name__ == "__main__":
    main()
