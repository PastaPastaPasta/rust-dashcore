#!/usr/bin/env python3
"""Checks a recorded evo-deep-window chain from its block bytes alone (BL-646).

Usage: verify_chain.py OUT_DIR   (OUT_DIR holds chain.txt, fork.txt and summary.json)

Decodes every block of M (chain.txt, blocks 1..N) and S (fork.txt), independently of the RPCs the
recorder asserted with, and fails unless:
- the files hold exactly the heights summary.json names, linked by X11 hash, S off M's block P;
- every block from height 2 (DIP0003) carries a version 2 CbTx naming its own height;
- every cycle from M's first base (and every S cycle) has the same commitment pattern: one null
  commitment per type at base+10..base+offset-1, one non-null per type at base+offset (M 11, S 12),
  each naming its cycle's base block, and none elsewhere;
- no (type, quorum hash) and no (type, mined height) of S is also one of M's: the keys of Core's
  `q_mc` and `q_mcih` index rows, so a reorg from S back to M leaves S's rows erased.
Prints per-file and per-1,000-block counts. Needs `dash_hash` (record.sh runs it in run.sh's venv).
"""
import hashlib
import json
import os
import sys
import subprocess
from collections import Counter

import dash_hash

GENESIS = "000008ca1832a4baf228eb1553c03d3a2c8e02399550dd6ea8d65cec3ef23d2e"
FIRST_BASE = 168
CYCLE = 24
WINDOW_START = 10
M_OFFSET, S_OFFSET = 11, 12
CYCLE_END = 19
TYPES = {100: "llmq_test", 104: "llmq_test_instantsend", 106: "llmq_test_platform"}
CBTX, QCTX = 5, 6
INDEXED = (2, 4)


class Reader:
    def __init__(self, data):
        self.data, self.pos = data, 0

    def take(self, n):
        assert self.pos + n <= len(self.data), "truncated"
        out = self.data[self.pos:self.pos + n]
        self.pos += n
        return out

    def int(self, n, signed=False):
        return int.from_bytes(self.take(n), "little", signed=signed)

    def compact(self):
        first = self.int(1)
        width = {0xfd: 2, 0xfe: 4, 0xff: 8}.get(first)
        return self.int(width) if width else first

    def done(self, what):
        assert self.pos == len(self.data), f"{what}: {len(self.data) - self.pos} bytes left"

    def bits(self):
        size = self.compact()
        raw = self.take((size + 7) // 8)
        return sum(bin(b).count("1") for b in raw)


def display(wire):
    return wire[::-1].hex()


def parse_tx(r):
    n32 = r.int(4)
    version = int.from_bytes(n32.to_bytes(4, "little")[:2], "little", signed=True)
    kind = n32 >> 16 if version >= 3 else 0
    for _ in range(r.compact()):
        r.take(36)
        r.take(r.compact())
        r.take(4)
    for _ in range(r.compact()):
        r.take(8)
        r.take(r.compact())
    r.take(4)
    payload = r.take(r.compact()) if kind else b""
    return kind, payload


def parse_block(raw):
    r = Reader(raw)
    header = r.take(80)
    block = {"hash": display(dash_hash.getPoWHash(header)), "prev": display(header[4:36]),
             "size": len(raw), "cbtx": None, "qcs": []}
    for i in range(r.compact()):
        kind, payload = parse_tx(r)
        p = Reader(payload)
        if kind == CBTX:
            assert i == 0, "CbTx not first"
            block["cbtx"] = (p.int(2), p.int(4, signed=True))
            if block["cbtx"][0] == 2:
                p.take(64)  # merkleRootMNList, merkleRootQuorums
                p.done("CbTx v2")
        elif kind == QCTX:
            p.int(2)
            height = p.int(4)
            version, llmq_type, quorum_hash = p.int(2), p.int(1), display(p.take(32))
            if version in INDEXED:
                p.int(2)
            signers = p.bits()
            p.bits()
            null_key = p.take(48) == bytes(48)
            assert (signers == 0) == null_key, "mixed null commitment"
            p.take(32 + 96 + 96)  # quorumVvecHash, quorumSig, membersSig
            p.done("commitment")
            block["qcs"].append((llmq_type, quorum_hash, signers == 0, height))
    assert r.pos == len(raw), "trailing bytes"
    return block


def chain_bytes(path):
    if os.path.exists(path):
        with open(path, "rb") as f:
            return f.read()
    return subprocess.check_output(["zstd", "-dc", path + ".zst"])


def read_blocks(path, first):
    headers, blocks = [], []
    raw = chain_bytes(path).decode("utf8")
    for line in raw.splitlines():
        line = line.rstrip("\n")
        if line.startswith("#"):
            assert not blocks, "header after blocks"
            headers.append(line)
            continue
        b = parse_block(bytes.fromhex(line))
        b["height"] = first + len(blocks)
        blocks.append(b)
    return headers, blocks


def check(blocks, parent_hash, bases, offset, failures, label):
    """Linkage, CbTx v2 and the commitment pattern; returns {(type, hash)}, {(type, height)}."""
    by_height = {b["height"]: b for b in blocks}
    keys_hash, keys_height = set(), set()
    prev = parent_hash
    for b in blocks:
        h = b["height"]
        if prev is not None and b["prev"] != prev:
            failures.append(f"{label} {h}: prev {b['prev']} != {prev}")
        prev = b["hash"]
        if h >= 2 and b["cbtx"] != (2, h):
            failures.append(f"{label} {h}: CbTx {b['cbtx']}")
        for t, q, null, qh in b["qcs"]:
            if qh != h:
                failures.append(f"{label} {h}: commitment payload height {qh}")
            if t not in TYPES:
                failures.append(f"{label} {h}: commitment type {t}")
            if not null:
                keys_hash.add((t, q))
                keys_height.add((t, h))
    for base in bases:
        q = by_height[base]["hash"]
        for h in range(base, min(base + CYCLE, blocks[-1]["height"] + 1)):
            o = h - base
            want = []
            if WINDOW_START <= o < offset:
                want = [(t, q, True) for t in TYPES]
            elif o == offset:
                want = [(t, q, False) for t in TYPES]
            got = sorted((t, qh, n) for t, qh, n, _ in by_height[h]["qcs"])
            if got != sorted(want):
                failures.append(f"{label} {h} (base {base}+{o}): commitments {got}")
    return keys_hash, keys_height


def windows(blocks, width=1000):
    rows = []
    for start in range(0, len(blocks), width):
        part = blocks[start:start + width]
        qcs = [q for b in part for q in b["qcs"]]
        rows.append("  %6d-%6d  blocks %4d  non-null %3d  null %3d  bytes %8d" % (
            part[0]["height"], part[-1]["height"], len(part), sum(not q[2] for q in qcs),
            sum(q[2] for q in qcs), sum(b["size"] for b in part)))
    return rows


def main():
    out = sys.argv[1]
    with open(os.path.join(out, "summary.json"), encoding="utf8") as f:
        summary = json.load(f)
    m, s = summary["m"], summary["s"]
    failures = []

    m_headers, chain = read_blocks(os.path.join(out, "chain.txt"), 1)
    s_headers, fork = read_blocks(os.path.join(out, "fork.txt"), s["parent"] + 1)
    if len(chain) != m["blocks"]:
        failures.append(f"M has {len(chain)} blocks, summary says {m['blocks']}")
    if len(fork) != s["blocks"]:
        failures.append(f"S has {len(fork)} blocks, summary says {s['blocks']}")
    for name, headers in (("M", m_headers), ("S", s_headers)):
        if len(headers) != 3 or not headers[1].startswith("# dashd -regtest " + " ".join(summary["chain_args"])):
            failures.append(f"{name} headers {headers}")
    if f"fork off {s['parent']} {s['parent_hash']}:" not in s_headers[2]:
        failures.append("S header does not name its parent")

    # The shape is pinned here, not taken on trust from summary.json.
    first_base = m["first_base"]
    if first_base != FIRST_BASE or (m["commit_offset"], s["commit_offset"]) != (M_OFFSET, S_OFFSET):
        failures.append(f"shape: first base {first_base}, offsets {m['commit_offset']}/{s['commit_offset']}")
    if s["fork_base"] != s["parent"] + 1 or s["fork_base"] % CYCLE or s["fork_base"] <= first_base:
        failures.append(f"shape: fork base {s['fork_base']}, parent {s['parent']}")
    if not s["cycles"] or len(s["cycles"]) != (s["blocks"] - 1 - CYCLE_END) // CYCLE + 1:
        failures.append(f"shape: {len(s['cycles'])} S cycles for {s['blocks']} blocks")
    setup_live = [(b["height"], q) for b in chain[:first_base - 1] for q in b["qcs"] if not q[2]]
    if setup_live:
        failures.append(f"non-null commitments before the first base: {setup_live[:5]}")
    m_bases = list(range(first_base, chain[-1]["height"] + 1, CYCLE))
    m_hash, m_height = check(chain, GENESIS, m_bases, m["commit_offset"], failures, "M")
    if chain[s["parent"] - 1]["hash"] != s["parent_hash"]:
        failures.append("M's block P is not S's parent")
    s_bases = [s["fork_base"] + CYCLE * i for i in range(len(s["cycles"]))]
    s_hash, s_height = check(fork, s["parent_hash"], s_bases, s["commit_offset"], failures, "S")
    tail_qcs = [q for b in fork if b["height"] >= s["fork_base"] + CYCLE * len(s_bases) for q in b["qcs"]]
    if tail_qcs:
        failures.append(f"S's plain tail has commitments {tail_qcs}")

    if m_hash & s_hash:
        failures.append(f"S shares (type, quorum hash) with M: {sorted(m_hash & s_hash)}")
    if m_height & s_height:
        failures.append(f"S shares (type, mined height) with M: {sorted(m_height & s_height)}")
    m_qcs = Counter(TYPES[t] for t, _ in m_height)
    s_qcs = Counter(TYPES[t] for t, _ in s_height)
    for c in m["cycles"]:
        if c["base"] <= m["blocks"] and chain[c["base"] - 1]["hash"] != c["quorum_hash"]:
            failures.append(f"M cycle {c['base']}: summary hash {c['quorum_hash']}")
    for c in s["cycles"]:
        if fork[c["base"] - s["parent"] - 1]["hash"] != c["quorum_hash"]:
            failures.append(f"S cycle {c['base']}: summary hash {c['quorum_hash']}")

    def sha(path):
        return hashlib.sha256(chain_bytes(path)).hexdigest()

    print("M: chain.txt blocks %d-%d (%d), sha256 %s" % (chain[0]["height"], chain[-1]["height"], len(chain),
                                                         sha(os.path.join(out, "chain.txt"))))
    print("   cycles from base %d: %d full or partial bases, non-null commitments %s, bytes %d" % (
        first_base, len(m_bases), dict(sorted(m_qcs.items())), sum(b["size"] for b in chain)))
    print("   blocks before the first base: %d, commitments in them %d" % (
        first_base - 1, sum(len(b["qcs"]) for b in chain[:first_base - 1])))
    print("S: fork.txt blocks %d-%d (%d) off %d %s, sha256 %s" % (
        fork[0]["height"], fork[-1]["height"], len(fork), s["parent"], s["parent_hash"],
        sha(os.path.join(out, "fork.txt"))))
    print("   %d cycles from base %d at base+%d, non-null commitments %s, plain tail %d" % (
        len(s_bases), s["fork_base"], s["commit_offset"], dict(sorted(s_qcs.items())), s["plain_tail"]))
    print("   distinct keys from M: (type, quorum hash) %d of %d, (type, mined height) %d of %d" % (
        len(s_hash - m_hash), len(s_hash), len(s_height - m_height), len(s_height)))
    print("M per 1,000 blocks:")
    print("\n".join(windows(chain)))
    if failures:
        print("FAILED: %d" % len(failures))
        print("\n".join(failures[:50]))
        sys.exit(1)
    print("OK")


if __name__ == "__main__":
    main()
