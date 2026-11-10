#!/usr/bin/env python3
"""Checks a recorded V19-window chain from its block bytes alone (BL-648, BL-839).

Usage: verify_chain.py OUT_DIR   (chain.txt, summary.json; R1 also fork.txt, R2 also clsigs.jsonl,
                                  R3 also serv-refused.txt and rev-refused.txt)
       verify_chain.py --probe OUT_DIR   (probe.json's verdicts, against the expected ones; see check_probe)

Decodes every block, independently of the RPCs the recorder asserted with, and fails unless:
- the files hold the heights summary.json names, linked by X11 hash (R1's S off M's block P);
- every block from height 2 (DIP0003) carries a CbTx naming its own height, version 2 below V20 and
  3 from it (R1 defers V20, R2 has it at 100);
- every commitment is of a recorded type and names its cycle's base: null ones only inside a mining
  window (base+10..base+18), version 1 below V19 and 3 from it (by the mining block, Core's
  `blockprocessor.cpp:899`); non-null ones only at base+11, version 1 when V19 is not active after the
  base and 3 when it is (`commitment.cpp:130-137`), one per type per cycle from the first base:
  llmq_test and llmq_test_instantsend always, llmq_test_platform while V19 is not active after the
  base (regular masternodes; EvoNodes only from V19, `llmq/utils.cpp:294-295`, and there are none);
- no ProTx below V19 has a version above 1 (`specialtxman.cpp:1172-1176`); R1's ProTxs sit at the
  heights and versions summary.json names;
- R2: every CbTx from V20 to V19 carries a null ChainLock (`node/miner.cpp:117-124`), V19 + 1's the
  ChainLock on V19 at difference 0, every recorded ChainLock's wire payload names its height and
  block hash on M, and switch-refused.txt is the V19 block with only its CbTx ChainLock changed;
- R3: no non-null commitment (no DKG); the V19 block holds one ProUpServTx v1 and the next one
  ProUpRevTx v1, as summary.json names them; each refused file is its block with only that ProTx
  changed, and in it only the payload's signature (the last 96 bytes) and the scriptSigs; the
  header differs only in its merkle root and nonce.
Exits 1 on any failure. Needs `dash_hash` (record.sh runs it in run.sh's venv).
"""
import hashlib
import json
import os
import struct
import subprocess
import sys
from collections import Counter

import dash_hash

GENESIS = "000008ca1832a4baf228eb1553c03d3a2c8e02399550dd6ea8d65cec3ef23d2e"
CYCLE = 24
WINDOW_START, WINDOW_END = 10, 18
COMMIT_OFFSET = 11
CBTX, QCTX = 5, 6
PROTX = {1: "ProRegTx", 2: "ProUpServTx", 3: "ProUpRegTx", 4: "ProUpRevTx"}
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
    """(kind, payload, raw, masked): `masked` is the raw transaction with every scriptSig emptied and a
    ProTx payload's signature (its last 96 bytes) zeroed, the parts a re-signing changes."""
    start, cuts = r.pos, []
    n32 = r.int(4)
    version = int.from_bytes(n32.to_bytes(4, "little")[:2], "little", signed=True)
    kind = n32 >> 16 if version >= 3 else 0
    for _ in range(r.compact()):
        r.take(36)
        at = r.pos
        r.take(r.compact())
        cuts.append((at, r.pos, b"\x00"))
        r.take(4)
    for _ in range(r.compact()):
        r.take(8)
        r.take(r.compact())
    r.take(4)
    payload = r.take(r.compact()) if kind else b""
    if kind in PROTX and len(payload) >= 96:
        cuts.append((r.pos - 96, r.pos, bytes(96)))
    masked, at = b"", start
    for a, b, replacement in cuts:
        masked += r.data[at:a] + replacement
        at = b
    masked += r.data[at:r.pos]
    return kind, payload, r.data[start:r.pos], masked


def parse_block(raw):
    r = Reader(raw)
    header = r.take(80)
    block = {"hash": display(dash_hash.getPoWHash(header)), "prev": display(header[4:36]),
             "size": len(raw), "cbtx": None, "qcs": [], "protx": [], "payloads": [], "header": header, "txs": []}
    for i in range(r.compact()):
        kind, payload, tx_raw, tx_masked = parse_tx(r)
        block["txs"].append((tx_raw, tx_masked))
        p = Reader(payload)
        if kind == CBTX:
            assert i == 0, "CbTx not first"
            version, height = p.int(2), p.int(4, signed=True)
            p.take(64)  # merkleRootMNList, merkleRootQuorums
            cl = None
            if version >= 3:
                cl = (p.compact(), p.take(96).hex())
                p.int(8)  # creditPoolBalance
            p.done(f"CbTx v{version}")
            block["cbtx"] = (version, height, cl)
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
            block["qcs"].append({"type": llmq_type, "quorum": quorum_hash, "null": signers == 0,
                                 "height": height, "version": version})
        elif kind in PROTX:
            block["protx"].append((PROTX[kind], p.int(2)))
            block["payloads"].append(payload)
    assert r.pos == len(raw), "trailing bytes"
    return block


def chain_bytes(path):
    if os.path.exists(path):
        with open(path, "rb") as f:
            return f.read()
    return subprocess.check_output(["zstd", "-dc", path + ".zst"])


def read_blocks(path, first):
    headers, blocks = [], []
    for line in chain_bytes(path).decode("utf8").splitlines():
        if line.startswith("#"):
            assert not blocks, "header after blocks"
            headers.append(line)
            continue
        b = parse_block(bytes.fromhex(line))
        b["height"] = first + len(blocks)
        blocks.append(b)
    return headers, blocks


def check(blocks, parent_hash, s, hash_at, failures, label):
    """Linkage, CbTx, commitments and ProTx versions of `blocks`; `hash_at(h)` is the hash at h on
    the branch (for a commitment's base). Returns {base: {type: (height, version)}} of non-null ones."""
    v19, v20 = s["v19_height"], s["v20_height"]
    types = {int(t): n for n, t in {**s["types"], **s["platform"]}.items()}
    mined = {}
    prev = parent_hash
    for b in blocks:
        h = b["height"]
        if b["prev"] != prev:
            failures.append(f"{label} {h}: prev {b['prev']} != {prev}")
        prev = b["hash"]
        want_cbtx = 3 if h >= v20 else 2
        if h >= 2 and (b["cbtx"] is None or b["cbtx"][:2] != (want_cbtx, h)):
            failures.append(f"{label} {h}: CbTx {b['cbtx'] and b['cbtx'][:2]}, want ({want_cbtx}, {h})")
        base = h - h % CYCLE
        for q in b["qcs"]:
            if q["height"] != h:
                failures.append(f"{label} {h}: commitment payload height {q['height']}")
            if q["type"] not in types:
                failures.append(f"{label} {h}: commitment type {q['type']}")
            if q["quorum"] != (hash_at(base) if base else GENESIS):
                failures.append(f"{label} {h}: commitment names {q['quorum']}, not base {base}")
            if q["null"]:
                if not WINDOW_START <= h % CYCLE <= WINDOW_END:
                    failures.append(f"{label} {h}: null commitment outside a window")
                if q["version"] != (3 if h >= v19 else 1):
                    failures.append(f"{label} {h}: null commitment version {q['version']}")
            else:
                if q["type"] not in cycle_types(s, base) or h % CYCLE != COMMIT_OFFSET:
                    failures.append(f"{label} {h}: commitment of {q['type']} at offset {h % CYCLE}")
                if q["version"] != (3 if base + 1 >= v19 else 1):
                    failures.append(f"{label} {h}: commitment version {q['version']} for base {base}")
                if q["type"] in mined.setdefault(base, {}):
                    failures.append(f"{label} {h}: second commitment of {q['type']} for base {base}")
                mined[base][q["type"]] = (h, q["version"])
        for kind, version in b["protx"]:
            if h < v19 and version > 1:
                failures.append(f"{label} {h}: {kind} v{version} below V19")
    return mined


def cycle_types(s, base):
    """The types with a non-null commitment in the cycle at `base`: llmq_test_platform's only while
    V19 is not active after the base (EvoNodes only from then, `llmq/utils.cpp:294-295`)."""
    types = {int(t) for t in s["types"].values()}
    return types | ({int(t) for t in s["platform"].values()} if base + 1 < s["v19_height"] else set())


def check_cycles(mined, first_base, last, s, failures):
    for base in range(first_base, last - COMMIT_OFFSET + 1, CYCLE):
        got, want = set(mined.get(base, {})), cycle_types(s, base)
        if got != want:
            failures.append(f"cycle {base}: non-null commitments of {sorted(got)}, want {sorted(want)}")


def protx_at(blocks, h):
    return blocks[h - blocks[0]["height"]]["protx"]


def check_r1(out, s, chain, failures):
    sm, ss = s["m"], s["s"]
    p = ss["parent"]
    s_headers, fork = read_blocks(os.path.join(out, "fork.txt"), p + 1)
    if len(fork) != ss["blocks"]:
        failures.append(f"S has {len(fork)} blocks, summary says {ss['blocks']}")
    if not s_headers[2].startswith(f"# fork off {p} {ss['parent_hash']}:"):
        failures.append("S header does not name its parent")
    if chain[p - 1]["hash"] != ss["parent_hash"] or chain[p]["hash"] != ss["invalidated"]:
        failures.append("M's P or P + 1 is not the summary's")
    by_s = {b["height"]: b["hash"] for b in fork}
    s_mined = check(fork, ss["parent_hash"], s, lambda h: by_s.get(h) or chain[h - 1]["hash"], failures, "S")
    if s_mined or any(not q["null"] for b in fork for q in b["qcs"]) or any(b["protx"] for b in fork):
        failures.append("S carries a non-null commitment or a ProTx")
    if fork[-1]["hash"] != ss["tip_hash"]:
        failures.append("S's tip is not the summary's")
    if len(chain) <= sm["tip_at_fork"] or ss["blocks"] >= sm["tip_at_fork"] - p:
        failures.append("M does not outgrow S")
    for name, e in s["events"].items():
        kind = {"upserv_a": "ProUpServTx", "reg_e": "ProRegTx", "upreg_b": "ProUpRegTx", "revoke_c": "ProUpRevTx",
                "upserv_b": "ProUpServTx"}[name]
        if protx_at(chain, e["height"]) != [(kind, e["version"])]:
            failures.append(f"{name}: block {e['height']} holds {protx_at(chain, e['height'])}")
    want = {"upserv_a": (210, 1), "reg_e": (s["v19_height"], 2), "upreg_b": (213, 2), "revoke_c": (214, 2),
            "upserv_b": (215, 2)}
    got = {k: (e["height"], e["version"]) for k, e in s["events"].items()}
    if got != want:
        failures.append(f"events {got}")
    print("S: fork.txt blocks %d-%d (%d) off %d %s, sha256 %s" % (
        fork[0]["height"], fork[-1]["height"], len(fork), p, ss["parent_hash"], sha(os.path.join(out, "fork.txt"))))
    print("   null commitments %d; N1 (N's ProRegTx at S's B) on dashd: %s" % (
        sum(len(b["qcs"]) for b in fork), s["n1_dashd"]))
    print("ProTxs on M: %s" % ", ".join("%d %s v%d" % (b["height"], k, v) for b in chain for k, v in b["protx"]
                                       if b["height"] > s["setup_tip"]))


def check_r2(out, s, chain, failures):
    n2 = s["v19_height"]
    by_h = {b["height"]: b for b in chain}
    null = (0, "00" * 96)
    for h in range(s["v20_height"], n2 + 1):
        if by_h[h]["cbtx"][2] != null:
            failures.append(f"{h}: CbTx ChainLock {by_h[h]['cbtx'][2]}, Core's miner keeps it null below V19's end")
    after = by_h[n2 + 1]["cbtx"][2]
    if after is None or after[0] != 0 or after[1] == null[1]:
        failures.append(f"{n2 + 1}: CbTx ChainLock {after}, want the ChainLock on {n2} at difference 0")
    check_refused(out, s, by_h, failures)
    locks = []
    with open(os.path.join(out, "clsigs.jsonl"), encoding="utf8") as f:
        locks = [json.loads(line) for line in f]
    for lock in locks:
        raw = bytes.fromhex(lock["wire"])
        h, block_hash = struct.unpack("<i", raw[:4])[0], display(raw[4:36])
        if len(raw) != 132 or (h, block_hash) != (lock["height"], lock["block_hash"]) or by_h[h]["hash"] != block_hash:
            failures.append(f"ChainLock {lock['height']}: wire names {(h, block_hash)}")
        for c in lock["in_cbtx"]:
            cl = by_h[c]["cbtx"][2]
            if cl is None or c - 1 - cl[0] != h:
                failures.append(f"ChainLock {h}: CbTx {c} carries {cl}")
            if lock["arrival_tip"] > c - 1:
                failures.append(f"ChainLock {h}: arrives at {lock['arrival_tip']}, in CbTx {c}")
    lock_n2 = [lk for lk in locks if lk["height"] == n2]
    if len(lock_n2) != 1 or n2 + 1 not in lock_n2[0]["in_cbtx"]:
        failures.append(f"the ChainLock on {n2}: {lock_n2}")
    diffs = Counter()
    for b in chain:
        cl = b["cbtx"] and b["cbtx"][2]
        if cl is not None:
            diffs["null" if cl[1] == "00" * 96 else ("0" if cl[0] == 0 else ">0")] += 1
    first = min((b["height"] for b in chain if b["cbtx"] and b["cbtx"][2] and b["cbtx"][2][1] != null[1]),
                default=None)
    print("ChainLocks: %d recorded (%s), first CbTx ChainLock at %s, CbTx v3 differences %s" % (
        len(locks), dict(Counter(lk["wire_source"] for lk in locks)), first, dict(diffs)))
    print("switch: CbTx ChainLocks null from %d to %d; %d carries the ChainLock on %d at difference %s" % (
        s["v20_height"], n2, n2 + 1, n2, after and after[0]))


def check_refused(out, s, by_h, failures):
    """switch-refused.txt: chain.txt's V19 block with its CbTx carrying the legacy ChainLock on
    V19 - 2 at difference 1. Everything but the CbTx's ChainLock and the header's merkle root and
    nonce matches the recorded block."""
    r, n2 = s["refused"], s["v19_height"]
    headers, [b] = read_blocks(os.path.join(out, "switch-refused.txt"), n2)
    mined = by_h[n2]
    want = (mined["cbtx"][0], n2, (1, r["lock_sig"]))
    if (b["hash"], b["prev"], b["cbtx"]) != (r["hash"], by_h[n2 - 1]["hash"], want):
        failures.append(f"switch-refused.txt: {b['hash']} on {b['prev']} CbTx {b['cbtx']}")
    if r["as_mined"]["hash"] != mined["hash"] or r["lock"] != [n2 - 2, by_h[n2 - 2]["hash"]]:
        failures.append(f"switch-refused.txt summary {r}")
    if (b["qcs"], b["protx"], b["size"]) != (mined["qcs"], mined["protx"], mined["size"]) or len(headers) != 3:
        failures.append("switch-refused.txt differs beyond the CbTx")
    print("refused: %s on %d, CbTx ChainLock on %d at difference 1, %d bytes" % (b["hash"], n2 - 1, n2 - 2, b["size"]))


def check_r3(out, s, chain, failures):
    n = s["v19_height"]
    by_h = {b["height"]: b for b in chain}
    kinds = {"upserv_a": "ProUpServTx", "revoke_c": "ProUpRevTx"}
    want = {"upserv_a": (n, 1), "revoke_c": (n + 1, 1)}
    got = {k: (e["height"], e["version"]) for k, e in s["events"].items()}
    if got != want:
        failures.append(f"events {got}")
    for name, e in s["events"].items():
        if by_h[e["height"]]["protx"] != [(kinds[name], 1)]:
            failures.append(f"{name}: block {e['height']} holds {by_h[e['height']]['protx']}")
    others = [(b["height"], k) for b in chain for k in b["protx"] if b["height"] > s["setup_tip"]
              and b["height"] not in (n, n + 1)]
    if others:
        failures.append(f"ProTxs after setup beside R3's: {others}")
    for name, r in s["refused"].items():
        headers, [b] = read_blocks(os.path.join(out, name), r["height"])
        mined = by_h[r["height"]]
        if len(headers) != 3:
            failures.append(f"{name}: {len(headers)} header lines")
        if (b["hash"], b["prev"], b["cbtx"], b["qcs"], b["protx"]) != (
                r["hash"], by_h[r["height"] - 1]["hash"], mined["cbtx"], mined["qcs"], mined["protx"]):
            failures.append(f"{name}: {b['hash']} on {b['prev']} differs beyond its ProTx")
        # The header but its merkle root (36..68) and nonce (76..80).
        if b["header"][:36] + b["header"][68:76] != mined["header"][:36] + mined["header"][68:76]:
            failures.append(f"{name}: its header differs beyond the merkle root and nonce")
        changed = [i for i, (ours, theirs) in enumerate(zip(b["txs"], mined["txs"])) if ours[0] != theirs[0]]
        if len(b["txs"]) != len(mined["txs"]) or changed != [r["index"]]:
            failures.append(f"{name}: transactions {changed} changed, want only {r['index']}")
        elif b["txs"][r["index"]][1] != mined["txs"][r["index"]][1]:
            failures.append(f"{name}: its ProTx differs beyond its signatures")
        if len(b["payloads"]) != 1 or len(mined["payloads"]) != 1 or b["payloads"][0][-96:] == mined["payloads"][0][-96:]:
            failures.append(f"{name}: not one ProTx with another payload signature")
        print("refused: %s %s on %d, %s v1 signed under legacy, %d bytes" % (
            name, b["hash"], r["height"] - 1, b["protx"][0][0], b["size"]))
    print("ProTxs above V19 at %d: %s" % (n, ", ".join("%d %s v%d" % (b["height"], k, v) for b in chain
                                                      for k, v in b["protx"] if b["height"] >= n)))


def sha(path):
    return hashlib.sha256(chain_bytes(path)).hexdigest()


def main():
    if sys.argv[1] == "--probe":
        return check_probe(sys.argv[2])
    out = sys.argv[1]
    with open(os.path.join(out, "summary.json"), encoding="utf8") as f:
        s = json.load(f)
    failures = []
    headers, chain = read_blocks(os.path.join(out, "chain.txt"), 1)
    if len(chain) != s["tip"] or chain[-1]["hash"] != s["tip_hash"]:
        failures.append(f"chain.txt has {len(chain)} blocks, summary says {s['tip']}")
    if len(headers) != 3 or headers[1] != "# dashd -regtest " + " ".join(s["chain_args"]):
        failures.append(f"headers {headers}")
    mined = check(chain, GENESIS, s, lambda h: chain[h - 1]["hash"], failures, "M")
    if s["cycles"]:
        first_base = s["cycles"][0]["base"]
        check_cycles(mined, first_base, len(chain), s, failures)
        if any(b < first_base for b in mined):
            failures.append(f"non-null commitments before the first base: {sorted(b for b in mined if b < first_base)}")
    elif mined:
        failures.append(f"non-null commitments without a DKG: {sorted(mined)}")
    for c in s["cycles"]:
        versions = {v for _, v in mined[c["base"]].values()}
        if chain[c["base"] - 1]["hash"] != c["quorum_hash"] or versions != {c["version"]}:
            failures.append(f"cycle {c['base']}: summary {c}")
    print("M: chain.txt blocks 1-%d, sha256 %s, bytes %d; V19 at %d, V20 at %d" % (
        len(chain), sha(os.path.join(out, "chain.txt")), sum(b["size"] for b in chain), s["v19_height"],
        s["v20_height"]))
    print("   cycles %s; commitment versions by base %s" % (
        [c["base"] for c in s["cycles"]], {b: sorted({v for _, v in m.values()}) for b, m in sorted(mined.items())}))
    nulls = Counter((q["version"], b["height"] >= s["v19_height"]) for b in chain for q in b["qcs"] if q["null"])
    print("   null commitments (version, at or above V19): %s" % dict(sorted(nulls.items())))
    {"reorg": check_r1, "v20-first": check_r2, "v1-above": check_r3}[s["mode"]](out, s, chain, failures)
    finish(failures)


def finish(failures):
    if failures:
        print("FAILED: %d" % len(failures))
        print("\n".join(failures[:50]))
        sys.exit(1)
    print("OK")


# The scheme lines of R1's probe windows (BL-648 phase 1 §1.2; validation.cpp:150-167,
# evo/specialtxman.cpp:1043-1047, :1065-1073, node/chainstate.cpp:429-434 at v24.0.0).
PS0 = "CSpecialTxProcessor::ProcessSpecialTxsInBlock -- bls_legacy_scheme=0"
SW1 = "CSpecialTxProcessor::UndoSpecialTxsInBlock -- bls_legacy_scheme=1"
EN1 = "ScopedBLSLegacyScheme: entered bls_legacy_scheme=1"
RV0 = "ScopedBLSLegacyScheme: reverted bls_legacy_scheme=0"
ST0 = "AppInitMain: bls_legacy_scheme=0"
BAD_SIG = "bad-protx-sig"


def expect(failures, what, got, want):
    if got != want:
        failures.append(f"{what}: {got!r}, want {want!r}")


def check_probe(out):
    """probe.json's verdicts against the ones the recording's mode expects; exits 1 on any other."""
    with open(os.path.join(out, "probe.json"), encoding="utf8") as f:
        p = json.load(f)
    with open(os.path.join(out, "summary.json"), encoding="utf8") as f:
        s = json.load(f)
    failures = []
    print("probe %s, %s" % (p["mode"], p["dashd_version"]))
    expect(failures, "probe mode", p["mode"], "probe-" + s["mode"])
    {"reorg": probe_r1, "v20-first": probe_r2, "v1-above": probe_r3}[s["mode"]](out, p, s, failures)
    finish(failures)


def probe_r1(out, p, s, failures):
    """Each step's window is §1.2's: PS0 at M's B; EN1 at its disconnect; PS0 at S's B; on the restart
    ST0, then VerifyDB's level-3 walk over B (SW1, RV0); EN1 then PS0 on the reconsider; none after.
    No block is refused."""
    for w in p["windows"]:
        print("  %-26s tip %3d  %s%s" % (w["step"], w["tip"], w["lines"] or "-",
                                         " refused %s" % w["verdicts"] if w["verdicts"] else ""))
    fork, m_at_fork, tip = s["s"]["parent"], s["m"]["tip_at_fork"], s["tip"]
    s_tip = fork + s["s"]["blocks"]
    want = [
        ("start", 0, []),
        ("connect M 1-%d" % m_at_fork, m_at_fork, [PS0]),
        ("invalidateblock %d" % (fork + 1), fork, [EN1]),
        ("connect S %d-%d" % (fork + 1, s_tip), s_tip, [PS0]),
        ("restart on S's %d" % s_tip, s_tip, [ST0, "Verifying last 6 blocks at level 3", SW1, RV0]),
        ("reconsiderblock %d" % (fork + 1), m_at_fork, [EN1, PS0]),
        ("connect M %d-%d" % (m_at_fork + 1, tip), tip, []),
    ]
    got = [(w["step"], w["tip"], w["lines"]) for w in p["windows"]]
    expect(failures, "window steps", [g[0] for g in got], [w[0] for w in want])
    for g, w in zip(got, want):
        expect(failures, f"window {w[0]!r} (tip, lines)", g[1:], w[1:])
    expect(failures, "refusals", [w["verdicts"] for w in p["windows"] if w["verdicts"]], [])
    expect(failures, "tip", p["tip"], tip)


def probe_r2(out, p, s, failures):
    """(a) and (b) take the whole chain; every recorded ChainLock is taken (no error) and (b)'s best is
    the tip's; switch-refused.txt at N2 - 1 is bad-cbtx-cldiff on both, with no best ChainLock on (a)
    and the one on N2 - 2 on (b)."""
    a, b = p["a"], p["b"]
    print("  (a) no ChainLock: tip %d, refusal %s" % (a["tip"], a["refusal"] or "none"))
    refused = [v for v in b["submitchainlock"] if "error" in v]
    print("  (b) recorded ChainLocks at their arrival tips: tip %d, refusal %s; submitchainlock %d, errors %d %s" % (
        b["tip"], b["refusal"] or "none", len(b["submitchainlock"]), len(refused), refused[:5]))
    print("  (b) best ChainLock at the end: %s" % (b["best_chainlock"] and b["best_chainlock"]["height"]))
    for k, side in (("a", a), ("b", b)):
        print("  (%s) switch-refused.txt: %s" % (k, side["switch_refused"]))
    n2, tip = s["v19_height"], s["tip"]
    with open(os.path.join(out, "clsigs.jsonl"), encoding="utf8") as f:
        locks = sum(1 for line in f if line.strip())
    for k, side in (("a", a), ("b", b)):
        expect(failures, f"({k}) refusal", side["refusal"], {})
        expect(failures, f"({k}) tip", side["tip"], tip)
        best = n2 - 2 if k == "b" else None
        expect(failures, f"({k}) switch-refused.txt", side["switch_refused"],
               {"tip": n2 - 1, "verdict": "bad-cbtx-cldiff", "best_chainlock": best, "tip_after": n2 - 1})
    expect(failures, "(a) best ChainLock", a["best_chainlock"], None)
    expect(failures, "(b) submitchainlock calls", len(b["submitchainlock"]), locks)
    expect(failures, "(b) submitchainlock errors", refused, [])
    expect(failures, "(b) best ChainLock", b["best_chainlock"] and b["best_chainlock"]["height"], tip)


def probe_r3(out, p, s, failures):
    """Below the switch (tip N - 2) the legacy-signed v1 ProTxs enter the mempool and the basic-signed
    ones are bad-protx-sig; from B on (and at N, C's) the other way round. serv-refused.txt at B and
    rev-refused.txt at N are bad-protx-sig, and the chain is taken whole. protx info at each state
    height is the recorder's."""
    n, tip = s["v19_height"], s["tip"]
    for h, v in sorted(p["mempool"].items(), key=lambda x: int(x[0])):
        print("  mempool at %s: %s" % (h, v))
    for name, v in sorted(p["blocks"].items()):
        print("  %s: %s" % (name, v))
    print("  tip %d, refusal %s, states at %s" % (p["tip"], p["refusal"] or "none", sorted(p["states"], key=int)))
    below = {"upserv_a/legacy": "allowed", "revoke_c/legacy": "allowed",
             "upserv_a/basic": BAD_SIG, "revoke_c/basic": BAD_SIG}
    above = {"upserv_a/legacy": BAD_SIG, "revoke_c/legacy": BAD_SIG,
             "upserv_a/basic": "allowed", "revoke_c/basic": "allowed"}
    above_c = {k: v for k, v in above.items() if k.startswith("revoke_c/")}
    expect(failures, "mempool", p["mempool"], {str(n - 2): below, str(n - 1): above, str(n): above_c})
    expect(failures, "the recorder's mempool", s["mempool"], {str(n - 2): below, str(n - 1): above})
    expect(failures, "refused blocks", p["blocks"], {
        "serv-refused.txt": {"tip": n - 1, "verdict": BAD_SIG, "tip_after": n - 1},
        "rev-refused.txt": {"tip": n, "verdict": BAD_SIG, "tip_after": n},
    })
    expect(failures, "refusal", p["refusal"], {})
    expect(failures, "tip", p["tip"], tip)
    expect(failures, "states", p["states"], s["state_versions"])


if __name__ == "__main__":
    main()
