#!/usr/bin/env python3
"""BL-875 chain driver: every block of the evo-image recording is mined through it.

The recorder (a DashTestFramework subclass on Core v24.0.0's test tree) calls ChainDriver:

    driver = ChainDriver(fw, mninfos, keys_cli)
    driver.enable(at_height)        recorder-only sporks; from here every block settles the DKGs first
    driver.mine_block()             one block on node0, returns its hash, asserts the per-block invariants
    driver.signing_ready(llmq_type) Core can select a quorum of this type at the tip
    driver.capture(cut, T)          keys.json "quorums" for this cut; runs Q1-Q7 at T (raises on a failure)
    driver.recorder_sporks()        for recording.json

Optional, used by the recorder from the first signing-capable height:

    driver.enable_chainlocks()      SPORK_19 on, waits for the first ChainLock; then every block waits for the
                                    tip's ChainLock first, so each CbTx carries the previous block's CL
    driver.hold_ehf(bit, height)    holds the masternodes' own MNHF signal for `bit` out of every block and
                                    mines it in block `height` exactly

The pure helpers (LLMQ schedule, QcTx parsing, BLS12-381 G1, share recovery, Q checks) import nothing from the
test framework, so `python3 -I driver.py --selftest` and the unit tests run without dashd.
"""
import contextlib
import hashlib
import json
import logging
import os
import re
import stat
import struct
import subprocess
import sys
import time
from collections import namedtuple
from dataclasses import dataclass
from functools import lru_cache

# ---------------------------------------------------------------------------------------------------------------
# LLMQ schedule on stock regtest (Core v24.0.0 llmq/params.h)
# ---------------------------------------------------------------------------------------------------------------

DKG_INTERVAL = 24
PHASE_BLOCKS = 2
FINALIZE_STAGE = 10  # phase 6 (Finalize) starts at stage 5 * PHASE_BLOCKS; the final commitment exists from then
IDLE_STAGE = 12      # phase 7 (Idle)


@dataclass(frozen=True)
class LlmqParams:
    llmq_type: int
    name: str
    size: int
    threshold: int
    rotation: bool
    window_start: int
    window_end: int

    @property
    def indexes(self):
        # signingActiveQuorumCount = 2 quorum indexes per cycle when rotating
        return 2 if self.rotation else 1


LLMQ_TEST = 100
LLMQ_TEST_DIP0024 = 103
LLMQ_TEST_PLATFORM = 106

LLMQS = {
    LLMQ_TEST: LlmqParams(LLMQ_TEST, "llmq_test", 3, 2, False, 10, 18),
    LLMQ_TEST_DIP0024: LlmqParams(LLMQ_TEST_DIP0024, "llmq_test_dip0024", 4, 3, True, 12, 20),
    LLMQ_TEST_PLATFORM: LlmqParams(LLMQ_TEST_PLATFORM, "llmq_test_platform", 3, 2, False, 10, 18),
}
# Defined on regtest but never enabled in the recording: LLMQ_TEST_INSTANTSEND (DIP0024 active) and
# LLMQ_TEST_V17 (testdummy FAILED under -vbparams=testdummy:-2:...).
EXCLUDED_TYPES = {102: "llmq_test_v17", 104: "llmq_test_instantsend"}

TX_TYPE_QUORUM_COMMITMENT = 6
TX_TYPE_MNHF_SIGNAL = 7
NULL_PK = "0" * 96
MNHF_PRIORITY_DELTA = 10_000_000  # txmempool.cpp: PrioritiseTransaction(tx_hash, 0.1 * COIN) on entry
EHF_QUIET_POLLS = 4  # consecutive identical quiet snapshots before a block is mined while the EHF hold runs
# Regtest DIP0003EnforcementHeight (chainparams.cpp). CDKGSessionManager::UpdatedBlockTip returns below it
# (llmq/dkgsessionmgr.cpp:46), so no DKG phase runs before tip 500 and the first DKG base is 504.
DIP3_ENFORCEMENT_HEIGHT = 500

# The only RPC errors the driver reads as a state rather than a failure (Core v24.0.0 rpc/protocol.h codes).
RPC_MISC_ERROR = -1
RPC_INTERNAL_ERROR = -32603
NO_QUORUM = (RPC_MISC_ERROR, "no quorums active")  # rpc/quorums.cpp:854, `quorum selectquorum`
NO_CHAINLOCK = (RPC_INTERNAL_ERROR, "Unable to find any ChainLock")  # rpc/blockchain.cpp:276, `getbestchainlock`


def is_rpc_error(exc, expected):
    """True when `exc` is the framework's JSONRPCException carrying exactly Core's (code, message) `expected`."""
    err = getattr(exc, "error", None)
    return isinstance(err, dict) and (err.get("code"), err.get("message")) == expected


@contextlib.contextmanager
def quiet_rpc_log():
    """Keep an RPC exchange that carries key material (shares, sks) out of the framework's RPC trace: the
    "BitcoinRPC" logger (test_framework/authproxy.py) logs every RPC argument and result, and `--tracerpc` sends it to
    stdout (test_framework.py:941-946)."""
    log = logging.getLogger("BitcoinRPC")
    was = log.disabled
    log.disabled = True
    try:
        yield
    finally:
        log.disabled = was


def session_base(height, quorum_index):
    """(quorum base height q, stage) of the (type, quorum_index) session at tip `height`."""
    stage = (height - quorum_index) % DKG_INTERVAL
    return height - stage, stage


def expected_phase(stage):
    """Core's ActiveDKGSessionHandler phase at a stage (phases 1..6; 7 = idle)."""
    return min(stage // PHASE_BLOCKS + 1, 7)


def due_height(params, cycle_base, quorum_index):
    """First block that can carry the session's final commitment.

    The commitment is built when the session enters Finalize, at tip q + 10 (q = cycle_base + quorum_index), so
    the earliest block holding it is q + 11, and it must be inside the mining window [base + start, base + end].
    LLMQ_TEST/PLATFORM: base + 11. DIP0024: base + 12 for both indexes.
    """
    return cycle_base + max(params.window_start, quorum_index + FINALIZE_STAGE + 1)


def first_dkg_base(at_height):
    """First cycle base whose DKG the driver requires: at or after enable and DIP0003 enforcement."""
    lo = max(at_height, DIP3_ENFORCEMENT_HEIGHT)
    return -(-lo // DKG_INTERVAL) * DKG_INTERVAL


def _sha256d(data):
    return hashlib.sha256(hashlib.sha256(data).digest()).digest()


def mnhf_request_id(bit):
    """RPC hex of SerializeHash(std::make_pair(std::string("mnhf"), int64_t{bit})) (evo/mnhftx.cpp:32-35)."""
    ser = bytes([4]) + b"mnhf" + struct.pack("<q", bit)
    return _sha256d(ser)[::-1].hex()


def mnhf_signal_tx(bit, quorum_hash, sig=bytes(96)):
    """MNHFTxPayload::PrepareTx() (evo/mnhftx.cpp:37-45): a version-3 type-7 tx with no inputs or outputs whose
    payload is {nVersion 1, versionBit, quorumHash, sig}. With the default all-zero sig it is the tx whose hash the
    masternodes sign (llmq/ehf_signals.cpp trySignEHFSignal; feature_mnehf.py create_mnehf builds it the same way)."""
    payload = bytes([1, bit]) + bytes.fromhex(quorum_hash)[::-1] + sig
    return (struct.pack("<hh", 3, TX_TYPE_MNHF_SIGNAL) + b"\x00\x00" + struct.pack("<I", 0)
            + bytes([len(payload)]) + payload)


@lru_cache(maxsize=None)
def mnhf_msg_hash(bit, quorum_hash):
    """RPC hex of the msgHash an LLMQ_TEST quorum `quorum_hash` signs for the MNHF signal of `bit`."""
    return _sha256d(mnhf_signal_tx(bit, quorum_hash))[::-1].hex()


def write_private_json(path, obj):
    """Write JSON to `path` as a file that is 0600 from its creation (O_EXCL under a temporary name, then renamed
    over `path`), whatever the umask and whatever mode an older file at `path` had."""
    tmp = f"{path}.tmp{os.getpid()}"
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(fd, "w", encoding="utf8") as f:
            json.dump(obj, f, indent=1, default=str)
        os.replace(tmp, path)
    except BaseException:
        with contextlib.suppress(FileNotFoundError):
            os.unlink(tmp)
        raise


def shown_path(path):
    """`path` for an error message, unless it is 64 hex digits: a seed given where its file's path belongs."""
    return "(value not shown)" if re.fullmatch(r"\s*[0-9a-fA-F]{64}\s*", str(path)) else repr(str(path))


def check_private_file(path, flag="--key-seed-file"):
    """Refuse a secret input file that is not a regular file owned by this user with no group/other bits. Errors name
    `flag` and show the path only through shown_path."""
    try:
        st = os.stat(path)
    except OSError as exc:
        raise ValueError(f"{flag} {shown_path(path)}: cannot stat ({exc.strerror})") from None
    if not stat.S_ISREG(st.st_mode) or st.st_uid != os.getuid() or st.st_mode & 0o077:
        raise ValueError(f"{flag} {shown_path(path)}: must be a regular file owned by uid {os.getuid()} with mode "
                         f"0600 (is {stat.filemode(st.st_mode)}, uid {st.st_uid})")


def withhold_unknown_args(parser):
    """Make argparse's "unrecognized arguments" error name no values: a mistyped flag can carry a key (an old
    `--key-seed <hex>`). The value was on the command line already (ps, shell history), so treat it as exposed."""
    error = parser.error

    def redacted(message):
        if message.startswith("unrecognized arguments:"):
            message = "unrecognized arguments (values withheld; any key given this way is exposed)"
        error(message)
    parser.error = redacted


def probe_id(tag, i):
    return hashlib.sha256(f"bl875/{tag}/{i}".encode()).hexdigest()


# ---------------------------------------------------------------------------------------------------------------
# Block parsing
# ---------------------------------------------------------------------------------------------------------------

def qc_records_from_block(block):
    """QcTx commitments of a `getblock <hash> 2` result."""
    out = []
    for tx in block["tx"]:
        if tx.get("type") != TX_TYPE_QUORUM_COMMITMENT:
            continue
        qc = tx["qcTx"]["commitment"]
        out.append({
            "height": block["height"],
            "payload_height": tx["qcTx"]["height"],
            "type": qc["llmqType"],
            "index": qc["quorumIndex"],
            "quorum_hash": qc["quorumHash"],
            "null": qc["quorumPublicKey"] == NULL_PK or qc["signersCount"] == 0,
            "pk": qc["quorumPublicKey"],
            "txid": tx["txid"],
        })
    return out


def mnhf_records_from_block(block):
    return [{"height": block["height"], "txid": tx["txid"], "bit": tx["mnhfTx"]["signal"]["versionBit"],
             "quorum_hash": tx["mnhfTx"]["signal"]["quorumHash"]}
            for tx in block["tx"] if tx.get("type") == TX_TYPE_MNHF_SIGNAL]


def block_invariants(records, due):
    """Per-block QcTx invariants (§8.2 step 4). `due` = [(type, index, quorum_hash)] that must be non-null here."""
    errs = []
    seen = set()
    for r in records:
        if r["type"] not in LLMQS:
            errs.append(f"QcTx of type {r['type']} at {r['height']}")
        key = (r["type"], r["index"])
        if key in seen:
            errs.append(f"two commitments for {key} at {r['height']}")
        seen.add(key)
        if r["payload_height"] != r["height"]:
            errs.append(f"QcTx payload height {r['payload_height']} in block {r['height']}")
    for t, idx, qh in due:
        hit = [r for r in records if r["type"] == t and r["index"] == idx and r["quorum_hash"] == qh]
        if not hit or hit[0]["null"]:
            errs.append(f"type {t} index {idx} quorum {qh}: no non-null commitment in its due block")
    return errs


# ---------------------------------------------------------------------------------------------------------------
# BLS12-381 G1 (basic/IETF serialization) and threshold recovery, for sk -> pk checks without dashd
# ---------------------------------------------------------------------------------------------------------------

BLS_P = 0x1A0111EA397FE69A4B1BA7B6434BACD764774B84F38512BF6730D2A0F6B0F6241EABFFFEB153FFFFB9FEFFFFFFFFAAAB
BLS_R = 0x73EDA753299D7D483339D80809A1D80553BDA402FFFE5BFEFFFFFFFF00000001
G1_X = 0x17F1D3A73197D7942695638C4FA9AC0FC3688C4F9774B905A14E3A3F171BAC586C55E83FF97A1AEFFB3AF00ADB22C6BB
G1_Y = 0x08B3F481E3AAA0F1A09E30ED741D8AE4FCF5E095D5D00AF600DB18CB2C04B3EDD03CC744A2888AE40CAA232946C5E7E1


def _jac_double(pt):
    x, y, z = pt
    if z == 0 or y == 0:
        return (1, 1, 0)
    p = BLS_P
    a = x * x % p
    b = y * y % p
    c = b * b % p
    d = 2 * ((x + b) ** 2 - a - c) % p
    e = 3 * a % p
    x3 = (e * e - 2 * d) % p
    y3 = (e * (d - x3) - 8 * c) % p
    z3 = 2 * y * z % p
    return (x3, y3, z3)


def _jac_add(p1, p2):
    if p1[2] == 0:
        return p2
    if p2[2] == 0:
        return p1
    p = BLS_P
    x1, y1, z1 = p1
    x2, y2, z2 = p2
    z1z1 = z1 * z1 % p
    z2z2 = z2 * z2 % p
    u1 = x1 * z2z2 % p
    u2 = x2 * z1z1 % p
    s1 = y1 * z2 * z2z2 % p
    s2 = y2 * z1 * z1z1 % p
    if u1 == u2:
        return _jac_double(p1) if s1 == s2 else (1, 1, 0)
    h = (u2 - u1) % p
    i = (2 * h) ** 2 % p
    j = h * i % p
    rr = 2 * (s2 - s1) % p
    v = u1 * i % p
    x3 = (rr * rr - j - 2 * v) % p
    y3 = (rr * (v - x3) - 2 * s1 * j) % p
    z3 = ((z1 + z2) ** 2 - z1z1 - z2z2) * h % p
    return (x3, y3, z3)


def g1_mul(k, pt=(G1_X, G1_Y, 1)):
    acc = (1, 1, 0)
    for bit in bin(k)[2:] if k > 0 else "":
        acc = _jac_double(acc)
        if bit == "1":
            acc = _jac_add(acc, pt)
    return acc


def g1_affine(pt):
    if pt[2] == 0:
        return None
    zi = pow(pt[2], -1, BLS_P)
    zi2 = zi * zi % BLS_P
    return (pt[0] * zi2 % BLS_P, pt[1] * zi2 * zi % BLS_P)


def g1_compress(pt):
    """48-byte compressed G1 (ZCash/IETF form, Core's basic-scheme ToString)."""
    aff = g1_affine(pt)
    if aff is None:
        return (bytes([0xC0]) + bytes(47)).hex()
    x, y = aff
    out = bytearray(x.to_bytes(48, "big"))
    out[0] |= 0x80
    if y > (BLS_P - 1) // 2:
        out[0] |= 0x20
    return out.hex()


def _secret_int(value, what):
    """int of a secret hex string; the error never repeats the value."""
    try:
        return int(value, 16)
    except (TypeError, ValueError):
        raise ValueError(f"{what} is not hex") from None


def bls_pk_from_sk(sk_hex):
    """Basic-scheme public key hex of a 32-byte big-endian secret key hex (Core `bls fromsecret`, legacy=false)."""
    sk = _secret_int(sk_hex, "secret key")
    if not 0 < sk < BLS_R:
        raise ValueError("secret key out of range")
    return g1_compress(g1_mul(sk))


def share_id_scalar(protx_rpc_hex):
    """CBLSId(proTxHash) as the scalar dashbls interpolates with.

    CBLSId keeps the uint256 as stored (the reverse of the RPC hex); Poly::LagrangeInterpolate reads those 32 bytes
    big-endian (bn_read_bin) and reduces mod r (dashbls threshold.cpp).
    """
    return int.from_bytes(bytes.fromhex(protx_rpc_hex)[::-1], "big") % BLS_R


def lagrange_recover(shares):
    """Secret at 0 from [(id_scalar, share_int)], as dashbls Threshold::PrivateKeyRecover."""
    ids = [i for i, _ in shares]
    if len(set(ids)) != len(ids) or 0 in ids:
        raise ValueError("duplicate or zero share id")
    total = 0
    for i, (xi, yi) in enumerate(shares):
        num, den = 1, 1
        for j, xj in enumerate(ids):
            if j != i:
                num = num * xj % BLS_R
                den = den * (xj - xi) % BLS_R
        total = (total + yi * num * pow(den, -1, BLS_R)) % BLS_R
    return total


class QuorumSkError(Exception):
    pass


def recover_quorum_sk(pk, threshold, shares, pk_from_sk=bls_pk_from_sk):
    """§8.4 step 3: recover from the first `threshold` shares by id and from a second, disjoint-as-possible subset;
    require equality and sk·G == pk. shares: [{"id": <proTxHash RPC hex>, "sk": <64 hex>}].

    Refuses what testkit-keys refuses: a threshold below 2, an id that is not 64 hex digits, a share that is not
    64 hex digits in 1..r, and two shares with the same id."""
    if threshold < 2:
        raise QuorumSkError("threshold is below 2")
    if len(shares) < threshold:
        raise QuorumSkError(f"{len(shares)} shares < threshold {threshold}")
    for i, s in enumerate(shares):
        if not (isinstance(s["id"], str) and re.fullmatch(r"[0-9a-fA-F]{64}", s["id"])):
            raise QuorumSkError(f"share {i} id is not 64 hex digits")
        if not (isinstance(s["sk"], str) and re.fullmatch(r"[0-9a-fA-F]{64}", s["sk"])
                and 0 < int(s["sk"], 16) < BLS_R):
            raise QuorumSkError(f"share {i} is not a valid BLS secret key")
    # sorted by the stored id bytes, unreduced, as testkit-keys sorts them, so both pick the same subsets
    ordered = sorted(shares, key=lambda s: bytes.fromhex(s["id"])[::-1])
    if any(bytes.fromhex(a["id"]) == bytes.fromhex(b["id"]) for a, b in zip(ordered, ordered[1:])):
        raise QuorumSkError("two shares have the same id")
    pts = [(share_id_scalar(s["id"]), int(s["sk"], 16)) for s in ordered]
    try:
        first = lagrange_recover(pts[:threshold])
        second = lagrange_recover(pts[-threshold:]) if len(pts) > threshold else first
    except ValueError as exc:
        raise QuorumSkError(str(exc)) from None
    if first != second:
        raise QuorumSkError("share subsets disagree")
    sk_hex = first.to_bytes(32, "big").hex()
    if first == 0 or pk_from_sk(sk_hex) != pk:
        raise QuorumSkError("recovered sk·G != quorum pk")
    return sk_hex


class PyQuorumKeys:
    """In-process twin of `testkit-keys quorum-sk`: the unit tests' keys_cli. Its `pk_from_sk` is also the
    re-derivation behind Q5 for TestkitQuorumKeys, independent of the Rust tool.

    If `core_node` is given, every recovered sk is also checked with Core's own `bls fromsecret`.
    """

    recovered_by = "driver.recover_quorum_sk"

    def __init__(self, core_node=None):
        self.core_node = core_node

    def pk_from_sk(self, sk_hex):
        pk = bls_pk_from_sk(sk_hex)
        if self.core_node is not None:
            with quiet_rpc_log():
                core = self.core_node.bls("fromsecret", sk_hex)
            # Core's reply also holds the sk ("secret"): only the scheme and pk may reach an error
            if core["scheme"] != "basic" or core["public"] != pk:
                raise QuorumSkError(f"Core bls fromsecret disagrees: scheme {core['scheme']} public {core['public']}"
                                    f" vs {pk}")
        return pk

    def quorum_sk(self, pk, threshold, shares):
        return recover_quorum_sk(pk, threshold, shares, self.pk_from_sk)


class TestkitQuorumKeys(PyQuorumKeys):
    """keys_cli backed by `testkit-keys quorum-sk` (§8.5); stdin JSON, stdout {"sk"}, exit 1 on any mismatch.

    The request goes to the tool's stdin, never its argv. The tool reads shares and prints an sk, so neither its stdout
    nor its stderr is repeated in an error."""

    recovered_by = "testkit-keys quorum-sk"

    def __init__(self, binary, core_node=None, timeout=60):
        super().__init__(core_node)
        self.binary = binary
        self.timeout = timeout

    def quorum_sk(self, pk, threshold, shares):
        req = json.dumps({"pk": pk, "threshold": threshold, "shares": shares})
        try:
            res = subprocess.run([self.binary, "quorum-sk"], input=req, capture_output=True, text=True, check=False,
                                 timeout=self.timeout)
        except subprocess.TimeoutExpired:
            raise QuorumSkError(f"testkit-keys quorum-sk timed out after {self.timeout:.0f}s") from None
        if res.returncode != 0:
            raise QuorumSkError(f"testkit-keys quorum-sk exit {res.returncode} (stderr withheld, "
                                f"{len(res.stderr)} chars)")
        try:
            sk = json.loads(res.stdout)["sk"]
        except (ValueError, KeyError, TypeError):
            raise QuorumSkError("testkit-keys quorum-sk printed no {\"sk\": ...} object") from None
        if self.pk_from_sk(sk) != pk:
            raise QuorumSkError("testkit-keys sk·G != quorum pk")
        return sk


# ---------------------------------------------------------------------------------------------------------------
# Q checks (§8.6). Each returns (id, ok, detail). Inputs are plain data so --selftest can feed fixtures.
# ---------------------------------------------------------------------------------------------------------------

def nonnull(records, llmq_type, upto):
    return [r for r in records if r["type"] == llmq_type and not r["null"] and r["height"] <= upto]


def completed_dip0024_cycles(records, T):
    """Cycle bases with non-null commitments for both indexes mined at or below T. Records need base_height."""
    by_base = {}
    for r in nonnull(records, LLMQ_TEST_DIP0024, T):
        by_base.setdefault(r["base_height"] - r["index"], set()).add(r["index"])
    return sorted(b for b, idx in by_base.items() if idx == {0, 1})


SIGN_HEIGHT_OFFSET = 8  # llmq/quorumsman.h


def signing_view_quorums(records, T):
    """Quorums Core selects for signing at tip T: ScanQuorums from T - SIGN_HEIGHT_OFFSET, signingActiveQuorumCount
    (2) per type (llmq/quorumsman.cpp SelectQuorumForSigning). `quorum list` at T scans from T, so a quorum mined in
    (T - 8, T] replaces one that can still sign; this returns the older view."""
    out = set()
    for t, p in LLMQS.items():
        recs = sorted(nonnull(records, t, T - SIGN_HEIGHT_OFFSET), key=lambda r: r["base_height"])
        if p.rotation:
            # the latest quorum of each index
            out |= {(t, [r for r in recs if r["index"] == qi][-1]["quorum_hash"]) for qi in range(p.indexes)
                    if any(r["index"] == qi for r in recs)}
        else:
            out |= {(t, r["quorum_hash"]) for r in recs[-2:]}
    return out


def finalized_unmined(T, records, required):
    """Required sessions that reached Finalize by tip T (their final commitment exists) but are not mined by T."""
    out = []
    for p in LLMQS.values():
        mined = {r["base_height"] for r in nonnull(records, p.llmq_type, T)}
        for qi in range(p.indexes):
            q, stage = session_base(T, qi)
            if stage >= FINALIZE_STAGE and required(p.llmq_type, q - qi) and q not in mined:
                out.append((p.llmq_type, qi, q))
    return out


def check_q1(quorum_list, records, T):
    bad_list = {k: v for k, v in quorum_list.items() if k in EXCLUDED_TYPES.values() and v}
    bad_qc = [r for r in records if r["type"] in EXCLUDED_TYPES and r["height"] <= T]
    ok = not bad_list and not bad_qc
    return ("Q1", ok, f"excluded types in quorum list: {sorted(bad_list)}; excluded QcTx: "
                      f"{[(r['height'], r['type']) for r in bad_qc]}")


def check_q2(records, T):
    n_test = len(nonnull(records, LLMQ_TEST, T))
    n_plat = len(nonnull(records, LLMQ_TEST_PLATFORM, T))
    return ("Q2", n_test >= 2 and n_plat >= 2, f"non-null llmq_test {n_test}, llmq_test_platform {n_plat}")


def due_dip0024_cycles(first, T):
    """DIP0024 cycle bases from `first` whose commitments are due (both indexes at base + 12) by T."""
    due = due_height(LLMQS[LLMQ_TEST_DIP0024], 0, 1)
    return list(range(first, T - due + 1, DKG_INTERVAL))


def check_q3(records, T):
    """≥ 2 completed DIP0024 cycles, and no gap: every cycle after the first completed one whose commitments are due
    by T is complete (non-null for both indexes). Plain data, so it runs on the replay node too."""
    cycles = completed_dip0024_cycles(records, T)
    gaps = [b for b in due_dip0024_cycles(cycles[0], T) if b not in cycles] if cycles else []
    return ("Q3", len(cycles) >= 2 and not gaps, f"completed DIP0024 cycles <= {T}: {cycles}; incomplete cycles after "
                                                 f"the first: {gaps}")


def check_q4(records, T):
    base = T - 18
    aligned = (T + 6) % DKG_INTERVAL
    complete = base in completed_dip0024_cycles(records, T)
    return ("Q4", aligned == 0 and complete, f"T + 6 = {T + 6} (mod 24 = {aligned}); cycle {base} complete <= T: "
                                             f"{complete}")


def check_q5(quorum_set, entries, pk_from_sk, chain_pks=None, recovered_by=None):
    """Every quorum of the set has an sk whose sk·G is its pk (and, on a replay node, the chain's pk). `recovered_by`
    names the keys_cli whose own sk·G == pk check passed for every entry on the recording."""
    by_hash = {(e["type"], e["hash"]): e for e in entries}
    errs = []
    for key in sorted(quorum_set):
        e = by_hash.get(key)
        if e is None or not e.get("sk"):
            errs.append(f"{key}: no sk")
            continue
        try:
            derived = pk_from_sk(e["sk"])
        except (ValueError, QuorumSkError) as exc:
            errs.append(f"{key}: {exc}")
            continue
        if derived != e["pk"]:
            errs.append(f"{key}: sk·G != pk")
        if chain_pks is not None and chain_pks.get(key) != e["pk"]:
            errs.append(f"{key}: keys.json pk != chain pk")
    via = f" (recovered by {recovered_by}, re-derived here)" if recovered_by else ""
    return ("Q5", not errs, "; ".join(errs) or f"{len(quorum_set)} quorums, sk·G == pk for each{via}")


def check_q6(active, quorum_set):
    missing = sorted(set(active) - set(quorum_set))
    return ("Q6", not missing, f"signing-active quorums missing from the set: {missing}" if missing
            else f"{len(set(active))} signing-active quorums all in the set")


def required_from_records(records):
    """`required(type, cycle_base)` from the chain alone, for a node that never ran the driver (the replay node):
    LLMQ_TEST/PLATFORM from their first non-null commitment, DIP0024 from its first completed cycle. A type with no
    such commitment requires every cycle (Q2/Q3 fail on that history anyway)."""
    first = {t: min((r["base_height"] for r in nonnull(records, t, float("inf"))), default=0)
             for t in (LLMQ_TEST, LLMQ_TEST_PLATFORM)}
    cycles = completed_dip0024_cycles(records, float("inf"))
    first[LLMQ_TEST_DIP0024] = cycles[0] if cycles else 0
    return lambda llmq_type, cycle_base: cycle_base >= first[llmq_type]


def check_q7(minable, unmined):
    """No final commitment is waiting at T: neither in node0's pool for block T + 1 (`minable`) nor built by a session
    and not yet mined (`unmined`, which also covers T outside a window). The replay node's pool is empty, so they
    agree."""
    pending = [(c["llmqType"], c["quorumIndex"], c["quorumHash"]) for c in minable if c["quorumPublicKey"] != NULL_PK]
    return ("Q7", not pending and not unmined,
            f"non-null minable commitments at T: {pending}; finalized and unmined (type, index, base): {unmined}")


# ---------------------------------------------------------------------------------------------------------------
# The driver
# ---------------------------------------------------------------------------------------------------------------

def _listed(qlist):
    """{(type, hash)} of a `quorum list` result for the recording's three types."""
    return {(p.llmq_type, qh) for p in LLMQS.values() for qh in qlist.get(p.name, [])}


class DriverError(AssertionError):
    pass


MnhfTx = namedtuple("MnhfTx", "txid bit hex quorum_hash")


class ChainDriver:
    def __init__(self, fw, mninfos, keys_cli, *, settle_timeout=120, probe_timeout=20):
        self.fw = fw
        self.log = fw.log
        self.mninfos = mninfos
        self.keys = keys_cli
        tf = fw.options.timeout_factor
        self.settle_timeout = settle_timeout * tf
        self.probe_timeout = probe_timeout * tf
        self.enabled_at = None
        self.first_base = None
        self.rotation_ready_base = None
        self._sporks = []
        self._hashes = {}
        self.records = {}      # height -> QcTx records (with base_height), from block 1
        self._checked = set()  # heights >= enabled_at that mine_block mined and that passed every per-block check
        self.mnhf = {}         # height -> MNHF records
        self._chainlocks = False
        self._ehf = None
        self._ehf_recsig = {}  # proTxHash -> LLMQ_TEST quorum whose MNHF signal that masternode holds a recsig for
        self._rot_probe = (None, False)
        self._tx_kind = {}
        self.checks = {}

    # -- basics ----------------------------------------------------------------------------------------------
    @property
    def node0(self):
        return self.fw.nodes[0]

    def running_mns(self):
        return [mn for mn in self.mninfos if mn.nodeIdx is not None and mn.nodeIdx < len(self.fw.nodes)
                and self.fw.nodes[mn.nodeIdx].running]

    def _nodes(self):
        return [self.node0] + [mn.get_node(self.fw) for mn in self.running_mns()]

    def block_hash(self, h):
        bh = self._hashes.get(h)
        if bh is None:
            bh = self._hashes[h] = self.node0.getblockhash(h)
        return bh

    def tip(self):
        return self.node0.getblockcount()

    def _wait(self, pred, timeout, what, bump=True):
        deadline = time.time() + timeout
        while True:
            res = pred()
            if res is True:
                return
            if time.time() > deadline:
                raise DriverError(f"timeout ({timeout:.0f}s) waiting for {what} at tip {self.tip()}: {res}")
            if bump:
                self.fw.bump_mocktime(1, nodes=self._nodes())
            time.sleep(0.5)

    # -- interface -------------------------------------------------------------------------------------------
    def enable(self, at_height):
        tip = self.tip()
        if tip + 1 != at_height:
            raise DriverError(f"enable({at_height}) at tip {tip}")
        for name in ("SPORK_17_QUORUM_DKG_ENABLED", "SPORK_21_QUORUM_ALL_CONNECTED"):
            self.node0.sporkupdate(name, 0)
            self._sporks.append({"name": name, "value": 0, "height": at_height})
        self.fw.wait_for_sporks_same()
        self.enabled_at = at_height
        self.first_base = first_dkg_base(at_height)
        self.log.info(f"driver enabled at {at_height}; LLMQ_TEST/PLATFORM required from base {self.first_base}")

    def recorder_sporks(self):
        return [dict(s) for s in self._sporks]

    def _select_quorum(self, llmq_type, request_id):
        """Hash of the quorum node0 selects for signing `request_id`, None when Core has none ("no quorums active").
        Any other error fails the run."""
        try:
            return self.node0.quorum("selectquorum", llmq_type, request_id)["quorumHash"]
        except Exception as exc:
            if is_rpc_error(exc, NO_QUORUM):
                return None
            raise

    def signing_ready(self, llmq_type):
        return self._select_quorum(llmq_type, probe_id("signing-ready", 0)) is not None

    def enable_chainlocks(self):
        if not self.signing_ready(LLMQ_TEST):
            raise DriverError("enable_chainlocks before an LLMQ_TEST quorum can sign")
        self.node0.sporkupdate("SPORK_19_CHAINLOCKS_ENABLED", 0)
        self.fw.wait_for_sporks_same()
        self._wait_chainlock("the first ChainLock")
        self._chainlocks = True
        self.log.info(f"ChainLocks on at tip {self.tip()}")

    def hold_ehf(self, bit, release_height):
        """Call before the first LLMQ_TEST quorum can sign (before enable()); fails if a signal already exists."""
        if self.mnhf or self._mempool_mnhf() or release_height <= self.tip():
            raise DriverError(f"hold_ehf({bit}, {release_height}) at tip {self.tip()}: a signal exists or the release "
                              "height has passed")
        self._ehf = {"bit": bit, "release": release_height, "id": mnhf_request_id(bit), "txid": None, "hex": None,
                     "held_at": None, "others": [],
                     "quiet": {"blocks": 0, "polls": 0, "seconds": 0.0, "max_seconds": 0.0, "max_at": None}}

    def ehf_state(self):
        return dict(self._ehf) if self._ehf else None

    def mine_block(self):
        h = self.tip()
        due = []
        driven = self.enabled_at is not None
        if driven:
            if self._chainlocks:
                self._wait_chainlock(f"the ChainLock of tip {h}")
            due = self._settle(h)
            if self._ehf:
                self._ehf_quiet(h)
        self.fw.bump_mocktime(1, nodes=self._nodes())
        if self._ehf and driven:
            # after the bump, right before generate: a signal that reached node0 since the quiet wait is held too
            self._ehf_hold(h)
        bh = self.fw.generate(self.node0, 1, sync_fun=lambda: self.fw.sync_blocks(self._nodes()))[0]
        self._record_block(bh, due, driven=driven)
        return bh

    # -- per block -------------------------------------------------------------------------------------------
    def _record_block(self, bh, due, driven=False):
        block = self.node0.getblock(bh, 2)
        h = block["height"]
        self._hashes[h] = bh
        recs = qc_records_from_block(block)
        for r in recs:
            r["base_height"] = None if r["null"] else self.node0.getblockheader(r["quorum_hash"])["height"]
        self.records[h] = recs
        errs = block_invariants(recs, due)
        mnhf = mnhf_records_from_block(block)
        if mnhf:
            self.mnhf[h] = mnhf
        if self._ehf:
            for m in mnhf:
                if h != self._ehf["release"] or m["txid"] != self._ehf["txid"]:
                    errs.append(f"MNHF tx {m['txid']} (bit {m['bit']}) mined at {h}")
            if h == self._ehf["release"] and not any(m["txid"] == self._ehf["txid"] for m in mnhf):
                errs.append(f"held MNHF tx {self._ehf['txid']} not in block {h}")
        if errs:
            raise DriverError(f"block {h} {bh}: " + "; ".join(errs))
        if driven:
            self._checked.add(h)
        for r in recs:
            if not r["null"]:
                self.log.info(f"  h={h} commitment type={r['type']} index={r['index']} base={r['base_height']}")

    def all_records(self, upto):
        """QcTx records of blocks 1..upto. Blocks below enable() may be filled in here; every block from enable() on
        must have been mined by mine_block and passed its per-block checks, and must still be node0's block at that
        height. Anything else fails: it is never filled in."""
        if self.enabled_at is not None:
            unchecked = [h for h in range(self.enabled_at, upto + 1) if h not in self._checked]
            if unchecked:
                raise DriverError(f"blocks {unchecked[:10]}{'...' if len(unchecked) > 10 else ''} ({len(unchecked)}) "
                                  "were not mined through the driver's per-block checks")
            replaced = [h for h in range(self.enabled_at, upto + 1) if self.node0.getblockhash(h) != self._hashes[h]]
            if replaced:
                raise DriverError(f"blocks {replaced[:10]} were replaced after the driver checked them")
        missing = [h for h in range(1, upto + 1) if h not in self.records]
        for h in missing:
            self._record_block(self.block_hash(h), due=[])
        return [r for h in range(1, upto + 1) for r in self.records[h]]

    def _session_required(self, llmq_type, cycle_base):
        if self.first_base is None or cycle_base < self.first_base:
            return False
        if llmq_type == LLMQ_TEST_DIP0024:
            return self.rotation_ready_base is not None and cycle_base >= self.rotation_ready_base
        return True

    def _dkg_statuses(self):
        return {mn.proTxHash: mn.get_node(self.fw).quorum("dkgstatus") for mn in self.running_mns()}

    def _member_sessions(self, statuses, params, quorum_index, quorum_hash):
        out = []
        for mn, st in statuses.items():
            for s in st["session"]:
                if (s["llmqType"] == params.name and s.get("quorumIndex", 0) == quorum_index
                        and s["status"]["quorumHash"] == quorum_hash):
                    out.append((mn, st, s["status"]))
        return out

    def _connections(self, st, params, quorum_index, quorum_hash):
        for qc in st.get("quorumConnections", []):
            if (qc["llmqType"] == params.name and qc.get("quorumIndex", 0) == quorum_index
                    and qc.get("quorumHash") == quorum_hash):
                return sum(1 for c in qc.get("quorumConnections", []) if c["connected"])
        return 0

    def _probe_rotation(self, h):
        """Before rotation is ready: does the DIP0024 session starting at tip h run with a full member set?"""
        params = LLMQS[LLMQ_TEST_DIP0024]
        qi = h % DKG_INTERVAL
        if qi not in (0, 1) or self.first_base is None or h - qi < self.first_base:
            return
        qh = self.block_hash(h)
        deadline = time.time() + self.probe_timeout
        n = 0
        while time.time() < deadline:
            statuses = self._dkg_statuses()
            n = len(self._member_sessions(statuses, params, qi, qh))
            if n >= params.size:
                break
            time.sleep(0.5)
        if qi == 0:
            self._rot_probe = (h, n >= params.size)
            self.log.info(f"DIP0024 cycle {h} index 0: {n} members")
        else:
            self.log.info(f"DIP0024 cycle {h - 1} index 1: {n} members")
            if self._rot_probe == (h - 1, True) and n >= params.size:
                self.rotation_ready_base = h - 1
                self.log.info(f"DIP0024 ready from cycle {h - 1}: non-null commitments required from here")

    def _settle(self, h):
        """Wait until every required live session is where the next block expects it; return its due list."""
        if self.rotation_ready_base is None:
            self._probe_rotation(h)
        checks = []  # (params, quorum_index, quorum_hash, stage)
        due = []
        for params in LLMQS.values():
            for qi in range(params.indexes):
                q, stage = session_base(h, qi)
                cycle_base = q - qi
                if not self._session_required(params.llmq_type, cycle_base):
                    continue
                if stage < IDLE_STAGE:
                    checks.append((params, qi, self.block_hash(q), stage))
                if due_height(params, cycle_base, qi) == h + 1:
                    due.append((params.llmq_type, qi, self.block_hash(q)))

        def ready():
            statuses = self._dkg_statuses()
            for params, qi, qh, stage in checks:
                phase = expected_phase(stage)
                members = self._member_sessions(statuses, params, qi, qh)
                at_phase = [m for m in members if m[2]["phase"] == phase]
                if len(at_phase) < params.size:
                    return f"{params.name}[{qi}] {qh[:12]}: {len(at_phase)}/{params.size} at phase {phase}"
                for _, st, ss in at_phase:
                    if ss.get("aborted") or ss.get("badMembers", 0):
                        return f"{params.name}[{qi}]: session aborted or bad members: {ss}"
                    if phase == 1 and self._connections(st, params, qi, qh) < params.size - 1:
                        return f"{params.name}[{qi}]: quorum connections < {params.size - 1}"
                    if phase == 2 and ss["receivedContributions"] < params.size:
                        return f"{params.name}[{qi}]: contributions {ss['receivedContributions']}"
                    if phase >= 3 and ss["receivedComplaints"] != 0:
                        return f"{params.name}[{qi}]: complaints {ss['receivedComplaints']}"
                    if phase >= 4 and ss["receivedJustifications"] != 0:
                        return f"{params.name}[{qi}]: justifications {ss['receivedJustifications']}"
                    if phase == 5 and ss["receivedPrematureCommitments"] < params.size:
                        return f"{params.name}[{qi}]: premature commitments {ss['receivedPrematureCommitments']}"
            if due:
                mine = {(c["llmqType"], c["quorumHash"]) for c in self.node0.quorum("dkgstatus")["minableCommitments"]
                        if c["quorumPublicKey"] != NULL_PK}
                missing = [(t, qi) for t, qi, qh in due if (t, qh) not in mine]
                if missing:
                    return f"node0 has no final commitment for {missing}"
            return True

        if checks or due:
            self._wait(ready, self.settle_timeout, f"DKG settle before block {h + 1}")
        return due

    def _wait_chainlock(self, what):
        def locked():
            tip = self.node0.getbestblockhash()
            try:
                cl = self.node0.getbestchainlock()
            except Exception as exc:
                if is_rpc_error(exc, NO_CHAINLOCK):
                    return "no chainlock yet"
                raise
            return True if cl["blockhash"] == tip else f"best CL at {cl['height']}"
        self._wait(locked, self.settle_timeout, what)

    # -- EHF hold (plan §3.3) --------------------------------------------------------------------------------
    def _mempool_mnhf(self, node=None):
        """[MnhfTx] of the MNHF signal txs in a node's mempool (node0 by default)."""
        node = node or self.node0
        out = []
        for txid in node.getrawmempool():
            kind = self._tx_kind.get(txid)
            if kind is None:
                tx = node.getrawtransaction(txid, True)
                kind = self._tx_kind[txid] = ((tx["mnhfTx"]["signal"]["versionBit"], tx["hex"],
                                               tx["mnhfTx"]["signal"]["quorumHash"])
                                              if tx.get("type") == TX_TYPE_MNHF_SIGNAL else ())
            if kind:
                out.append(MnhfTx(txid, *kind))
        return out

    def _recsig_quorum(self, mn, node, candidates):
        """The LLMQ_TEST quorum whose MNHF signal `mn` holds the recovered sig for, or None if it is none of
        `candidates`. A node keeps one recovered sig per id; `quorum hasrecsig <id> <msgHash>` names it."""
        q = self._ehf_recsig.get(mn.proTxHash)
        if q is None:
            e = self._ehf
            q = next((c for c in candidates
                      if node.quorum("hasrecsig", LLMQ_TEST, e["id"], mnhf_msg_hash(e["bit"], c))), None)
            if q is not None:
                self._ehf_recsig[mn.proTxHash] = q
        return q

    def _ehf_snapshot(self, h):
        """(reason it is not quiet or None, snapshot), read from Core's state at tip h.

        A masternode signs the request id at every tip while it has no recovered sig for it, with the quorum
        SelectQuorumForSigning picks then, and turns a recovered sig into a signal tx in its own mempool at once
        (llmq/ehf_signals.cpp trySignEHFSignal, HandleNewRecoveredSig). Before the release height it is quiet when:
        - every MNHF tx in any masternode's mempool is in node0's;
        - every masternode that holds a recovered sig for the id (`quorum isconflicting <id> 00..00`) holds the one
          of a quorum whose signal tx node0 has or a block holds (`quorum hasrecsig` with that quorum's msgHash), so
          no recovered signal is on its way to node0;
        - fewer than threshold members of the quorum selected now lack a recovered sig, so this tip's shares cannot
          recover a new one by themselves.
        From the release height on no masternode creates a signal (the bit is mined), so only the first holds.
        """
        e = self._ehf
        at_node0 = {f.txid: f.quorum_hash for f in self._mempool_mnhf()}
        for mn in self.running_mns():
            missing = {f.txid for f in self._mempool_mnhf(mn.get_node(self.fw))} - at_node0.keys()
            if missing:
                return f"MNHF signal(s) {sorted(missing)} not at node0 yet", None
        if h >= e["release"]:
            return None, (frozenset(at_node0),)
        signalled = set(at_node0.values()) | {m["quorum_hash"] for ms in self.mnhf.values() for m in ms}
        sel = self._select_quorum(LLMQ_TEST, e["id"])
        members = set()
        candidates = sorted(signalled)
        if sel is not None:
            members = {m["proTxHash"] for m in self.node0.quorum("info", LLMQ_TEST, sel)["members"] if m["valid"]}
            candidates.append(sel)
        candidates += [r["quorum_hash"] for hh in sorted(self.records, reverse=True) for r in self.records[hh]
                       if r["type"] == LLMQ_TEST and not r["null"]]
        candidates = list(dict.fromkeys(candidates))
        holders, lacking = {}, []
        for mn in self.running_mns():
            node = mn.get_node(self.fw)
            if not node.quorum("isconflicting", LLMQ_TEST, e["id"], "00" * 32):
                if mn.proTxHash in members:
                    lacking.append(mn.proTxHash)
                continue
            q = self._recsig_quorum(mn, node, candidates)
            if q is None:
                return f"{mn.proTxHash[:12]} holds a recovered sig for the MNHF id from no known LLMQ_TEST quorum", None
            if q not in signalled:
                return f"the signal of quorum {q[:12]} is recovered on {mn.proTxHash[:12]} but is not at node0", None
            holders[mn.proTxHash] = q
        if len(lacking) >= LLMQS[LLMQ_TEST].threshold:
            # Core signs once per quorum and tip; a round that does not recover is not retried until the next tip
            return f"{len(lacking)} members of {sel[:12]} have no recovered sig for the MNHF id yet", None
        return None, (frozenset(at_node0), frozenset(holders.items()), frozenset(lacking))

    def _ehf_quiet(self, h):
        """Wait for EHF_QUIET_POLLS identical quiet snapshots in a row (bounded by settle_timeout; a timeout raises
        with the last reason). The snapshot is Core's state (_ehf_snapshot), so a recovered signal or a signal tx on
        its way to node0 keeps it from passing. Shares of members that received an older recovered sig after they
        signed are not visible over RPC; the stability window covers them, _ehf_hold re-scans node0 right before
        generate, and _record_block fails the run on any MNHF tx but the released one."""
        run = {"snap": None, "n": 0, "polls": 0}
        start = time.time()

        def quiet():
            run["polls"] += 1
            reason, snap = self._ehf_snapshot(h)
            if reason:
                run["n"] = 0
                return reason
            run["n"] = run["n"] + 1 if snap == run["snap"] else 1
            run["snap"] = snap
            return True if run["n"] >= EHF_QUIET_POLLS else f"quiet {run['n']}/{EHF_QUIET_POLLS}"
        self._wait(quiet, self.settle_timeout, "the masternodes' MNHF signals")
        took = time.time() - start
        q = self._ehf["quiet"]
        q["blocks"] += 1
        q["polls"] += run["polls"]
        q["seconds"] = round(q["seconds"] + took, 1)
        if took > q["max_seconds"]:
            q["max_seconds"], q["max_at"] = round(took, 1), h
        if run["polls"] > EHF_QUIET_POLLS:
            self.log.info(f"MNHF quiet wait before block {h + 1}: {run['polls']} polls, {took:.1f}s")

    def _set_modified_fee(self, txid, target):
        entry = self.node0.getmempoolentry(txid)
        cur = round(float(entry["fees"]["modified"]) * 100_000_000)
        if cur != target:
            self.node0.prioritisetransaction(txid, target - cur)
        got = round(float(self.node0.getmempoolentry(txid)["fees"]["modified"]) * 100_000_000)
        if got != target:
            raise DriverError(f"MNHF {txid}: modified fee {got} != {target}")

    def _ehf_hold(self, h):
        """Hold every MNHF signal in node0's mempool at a -0.1 DASH modified fee except the released one, which gets
        +0.1 only for block `release`."""
        e = self._ehf
        found = self._mempool_mnhf()
        other = [(f.txid, f.bit) for f in found if f.bit != e["bit"]]
        if other:
            raise DriverError(f"MNHF signal for another bit in the mempool: {other}")
        if e["txid"] is None:
            if not found:
                return
            e["txid"], e["hex"] = found[0].txid, found[0].hex
            e["held_at"] = h
            self.log.info(f"MNHF bit {e['bit']} signal {e['txid']} held at tip {h}; release at {e['release']}")
        # Masternodes without the first recovered sig keep signing with whichever LLMQ_TEST quorum is selected, so
        # a later quorum produces further signal txs for the same bit. Every one but the released tx stays held.
        for txid in (f.txid for f in found):
            if txid != e["txid"] and txid not in e["others"]:
                e["others"].append(txid)
                self.log.info(f"MNHF bit {e['bit']}: further signal {txid} held at tip {h}")
        in_pool = {f.txid for f in found}
        for txid in e["others"]:
            if txid in in_pool:
                self._set_modified_fee(txid, -MNHF_PRIORITY_DELTA)
        if h >= e["release"]:
            return
        if e["txid"] not in in_pool:
            self.node0.sendrawtransaction(e["hex"])
        target = MNHF_PRIORITY_DELTA if h + 1 == e["release"] else -MNHF_PRIORITY_DELTA
        self._set_modified_fee(e["txid"], target)

    # -- capture (§8.4) and checks (§8.6) --------------------------------------------------------------------
    def quorum_set(self, T, records):
        """§8.4 step 1: quorum list at T for the three types, both indexes of the last two completed DIP0024 cycles,
        and the quorums Core still selects for signing at T (scanned from T - 8)."""
        qlist = self.node0.quorum("list")
        out = _listed(qlist)
        for b in completed_dip0024_cycles(records, T)[-2:]:
            out |= {(LLMQ_TEST_DIP0024, r["quorum_hash"]) for r in nonnull(records, LLMQ_TEST_DIP0024, T)
                    if r["base_height"] - r["index"] == b}
        return qlist, out | signing_view_quorums(records, T)

    def signing_active(self):
        out = _listed(self.node0.quorum("list"))
        for params in LLMQS.values():
            for i in range(16):
                qh = self._select_quorum(params.llmq_type, probe_id("q6", i))
                if qh is not None:
                    out.add((params.llmq_type, qh))
        return out

    def _capture_quorum(self, llmq_type, qh, active):
        params = LLMQS[llmq_type]
        info = self.node0.quorum("info", llmq_type, qh)
        by_protx = {mn.proTxHash: mn for mn in self.running_mns()}
        shares = []
        for m in info["members"]:
            if not m["valid"]:
                continue
            mn = by_protx.get(m["proTxHash"])
            if mn is None:
                raise DriverError(f"{params.name} {qh}: valid member {m['proTxHash']} is not running")
            with quiet_rpc_log():
                mi = mn.get_node(self.fw).quorum("info", llmq_type, qh, True)
            if "secretKeyShare" not in mi:
                raise DriverError(f"{params.name} {qh}: member {m['proTxHash']} has no secretKeyShare")
            shares.append({"id": m["proTxHash"], "sk": mi["secretKeyShare"]})
        sk = self.keys.quorum_sk(info["quorumPublicKey"], params.threshold, shares)
        return {
            "type": llmq_type, "name": params.name, "hash": qh, "quorum_index": info["quorumIndex"],
            "base_height": info["height"],
            "mined_height": self.node0.getblockheader(info["minedBlock"])["height"],
            "pk": info["quorumPublicKey"], "sk": sk,
            "members": [m["proTxHash"] for m in info["members"]],
            "active_at_T": (llmq_type, qh) in active,
        }

    def run_q_checks(self, cut, T, records, quorum_list, quorum_set, entries, active, minable, chain_pks=None):
        """chain_pks: on a replay node, its `quorum info` pk per (type, hash); None on the recording, where the
        entries' pks were read from node0 itself. Q7's required sessions come from enable()'s state on the recording
        and from the records themselves on a driver that never ran enable() (the replay node)."""
        required = self._session_required if self.first_base is not None else required_from_records(records)
        res = [check_q1(quorum_list, records, T), check_q2(records, T), check_q3(records, T)]
        if cut.startswith("S4"):
            res.append(check_q4(records, T))
        recovered_by = self.keys.recovered_by if chain_pks is None else None
        res += [check_q5(quorum_set, entries, self.keys.pk_from_sk, chain_pks, recovered_by),
                check_q6(active, quorum_set),
                check_q7(minable, finalized_unmined(T, records, required))]
        return res

    def capture(self, cut, T):
        if self.tip() != T:
            raise DriverError(f"capture({cut}, {T}) at tip {self.tip()}")
        if self.enabled_at is None:
            raise DriverError(f"capture({cut}, {T}) before enable()")
        if self.rotation_ready_base is None:
            raise DriverError(f"capture({cut}, {T}): DIP0024 readiness was never detected, so no DIP0024 cycle was "
                              "checked block by block")
        records = self.all_records(T)
        qlist, qset = self.quorum_set(T, records)
        active = self.signing_active()
        entries = [self._capture_quorum(t, qh, active) for t, qh in sorted(qset)]
        minable = self.node0.quorum("dkgstatus")["minableCommitments"]
        res = self.run_q_checks(cut, T, records, qlist, qset, entries, active, minable)
        self.checks[cut] = res
        for cid, ok, detail in res:
            self.log.info(f"{cut} {cid} {'ok' if ok else 'FAIL'}: {detail}")
        if not all(ok for _, ok, _ in res):
            raise DriverError(f"{cut}: Q checks failed: {[c for c in res if not c[1]]}")
        return entries


# ---------------------------------------------------------------------------------------------------------------
# --selftest: each Q check once on an input that must make it fail (§8.6 fail-first), plus its passing twin
# ---------------------------------------------------------------------------------------------------------------

def _fixture_records(last_base):
    """Commitments as mine_block would record them: every type from base 504 (the first DKG base)."""
    recs = []
    for b in range(504, last_base + 1, DKG_INTERVAL):
        for t in (LLMQ_TEST, LLMQ_TEST_PLATFORM):
            recs.append({"height": b + 11, "type": t, "index": 0, "quorum_hash": f"{t}-{b}", "null": False,
                         "base_height": b, "pk": "", "payload_height": b + 11})
        for qi in (0, 1):
            recs.append({"height": b + 12, "type": LLMQ_TEST_DIP0024, "index": qi,
                         "quorum_hash": f"103-{b + qi}", "null": False, "base_height": b + qi, "pk": "",
                         "payload_height": b + 12})
    return recs


def selftest():
    failures = []

    def expect(check, want_ok, label):
        cid, ok, detail = check
        if ok != want_ok:
            failures.append(f"{label}: {cid} returned ok={ok} ({detail})")

    recs = _fixture_records(1272)
    expect(check_q1({"llmq_test": ["a"]}, recs, 1290), True, "Q1 clean")
    bad = recs + [{"height": 516, "type": 102, "index": 0, "quorum_hash": "x", "null": False, "base_height": 504,
                   "pk": "", "payload_height": 500}]
    expect(check_q1({"llmq_test": ["a"]}, bad, 1290), False, "Q1 synthetic type-102 QcTx")
    expect(check_q1({"llmq_test_v17": ["a"]}, recs, 1290), False, "Q1 llmq_test_v17 in quorum list")

    expect(check_q2(_fixture_records(528), 540), True, "Q2 two cycles of each type at 540")
    expect(check_q2(_fixture_records(504), 540), False, "Q2 one cycle of each type")
    expect(check_q2([r for r in recs if not (r["type"] == LLMQ_TEST_PLATFORM and r["base_height"] > 504)], 1290),
           False, "Q2 one llmq_test_platform commitment")

    expect(check_q3(recs, 990), True, "Q3 S5a at 990")
    expect(check_q3(recs, 528), False, "Q3 S5a moved to 528 (one completed cycle)")
    expect(check_q3([r for r in recs if not (r["type"] == LLMQ_TEST_DIP0024 and r["base_height"] in (576, 577))], 990),
           False, "Q3 cycle 576 null between complete cycles")

    expect(check_q4(recs, 1290), True, "Q4 S4 at 1290")
    expect(check_q4(recs, 1291), False, "Q4 S4 moved to 1291")
    expect(check_q4(_fixture_records(1248), 1290), False, "Q4 cycle 1272 not mined")

    sk = (int(hashlib.sha256(b"bl875/selftest/q5").hexdigest(), 16) % BLS_R).to_bytes(32, "big").hex()
    entry = {"type": 100, "hash": "h", "pk": bls_pk_from_sk(sk), "sk": sk}
    expect(check_q5({(100, "h")}, [entry], bls_pk_from_sk), True, "Q5 clean")
    flipped = dict(entry, sk=sk[:-2] + format(int(sk[-2:], 16) ^ 1, "02x"))
    expect(check_q5({(100, "h")}, [flipped], bls_pk_from_sk), False, "Q5 one sk byte changed")
    expect(check_q5({(100, "h"), (106, "g")}, [entry], bls_pk_from_sk), False, "Q5 quorum without sk")
    expect(check_q5({(100, "h")}, [entry], bls_pk_from_sk, {(100, "h"): entry["pk"]}), True, "Q5 chain pk agrees")
    expect(check_q5({(100, "h")}, [entry], bls_pk_from_sk, {(100, "h"): bls_pk_from_sk("01".rjust(64, "0"))}), False,
           "Q5 chain pk differs")

    expect(check_q6({(100, "a")}, {(100, "a"), (103, "b")}), True, "Q6 clean")
    expect(check_q6({(100, "a"), (106, "c")}, {(100, "a")}), False, "Q6 active quorum outside the set")

    expect(check_q7([{"llmqType": 100, "quorumIndex": 0, "quorumHash": "h", "quorumPublicKey": NULL_PK}], []), True,
           "Q7 null only")
    expect(check_q7([{"llmqType": 100, "quorumIndex": 0, "quorumHash": "h", "quorumPublicKey": "a" * 96}], []),
           False, "Q7 pending non-null")
    every = lambda t, b: True  # noqa: E731
    expect(check_q7([], finalized_unmined(1290, recs, every)), True, "Q7 S4 at 1290")
    expect(check_q7([], finalized_unmined(1282, _fixture_records(1248), every)), False,
           "Q7 cut at base + 10: cycle 1272 finalized, not mined")
    replay = _fixture_records(1248)
    expect(check_q7([], finalized_unmined(1282, replay, required_from_records(replay))), False,
           "Q7 replay node (requirements from the records): cycle 1272 finalized, not mined")
    expect(check_q7([], finalized_unmined(1290, recs, required_from_records(recs))), True,
           "Q7 replay node at S4 1290")

    errs = block_invariants([{"height": 467, "payload_height": 467, "type": 100, "index": 0, "quorum_hash": "h",
                              "null": True, "pk": NULL_PK}], [(100, 0, "h")])
    if not errs:
        failures.append("block invariant: a null commitment in its due block passed")

    for f in failures:
        print("SELFTEST FAIL:", f)
    print("selftest:", "FAIL" if failures else "ok")
    return 1 if failures else 0


if __name__ == "__main__":
    if sys.argv[1:] == ["--selftest"]:
        sys.exit(selftest())
    print(__doc__)
    sys.exit(2)
