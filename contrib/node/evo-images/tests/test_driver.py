"""Unit tests of driver.py's pure helpers. Run: python3 -I -m unittest discover -s contrib/node/evo-images/tests"""
import hashlib
import json
import logging
import os
import random
import stat
import struct
import sys
import tempfile
import unittest
from types import SimpleNamespace

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, ".."))

import driver as d  # noqa: E402

# test-vectors/evo-driver/PROVENANCE.md: a copy of Core v24.0.0's dashbls threshold example and a regtest MNHF tx
VECTORS = os.path.join(HERE, "..", "..", "..", "..", "test-vectors", "evo-driver", "core-v24.json")
VECTORS_SHA256 = "df56e9f6aa20aede8f6e638ee1f3939c5d2e4f360fa7bf1ca3029ba2167565dd"


def _vectors():
    with open(VECTORS, "rb") as f:
        raw = f.read()
    assert hashlib.sha256(raw).hexdigest() == VECTORS_SHA256, "test-vectors/evo-driver/core-v24.json changed"
    return json.loads(raw)


def _sha256d(b):
    return hashlib.sha256(hashlib.sha256(b).digest()).digest()


class Schedule(unittest.TestCase):
    def test_due_heights(self):
        self.assertEqual(d.due_height(d.LLMQS[100], 456, 0), 467)
        self.assertEqual(d.due_height(d.LLMQS[106], 456, 0), 467)
        self.assertEqual(d.due_height(d.LLMQS[103], 1272, 0), 1284)
        self.assertEqual(d.due_height(d.LLMQS[103], 1272, 1), 1284)
        for p in d.LLMQS.values():
            for qi in range(p.indexes):
                due = d.due_height(p, 480, qi)
                self.assertTrue(480 + p.window_start <= due <= 480 + p.window_end)
                # the session is in Finalize (phase 6) at the tip before its due block
                q, stage = d.session_base(due - 1, qi)
                self.assertEqual(q, 480 + qi)
                self.assertGreaterEqual(d.expected_phase(stage), 6)

    def test_phases(self):
        self.assertEqual([d.expected_phase(s) for s in range(14)], [1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7])
        self.assertEqual(d.session_base(457, 1), (457, 0))
        self.assertEqual(d.session_base(457, 0), (456, 1))
        self.assertEqual(d.session_base(479, 0), (456, 23))

    def test_first_base(self):
        self.assertEqual(d.first_dkg_base(441), 504)
        self.assertEqual(d.first_dkg_base(505), 528)
        self.assertEqual(d.first_dkg_base(504), 504)

    def test_s4_alignment(self):
        # §8.3: T = 1290 = base 1296 - 6; cycle 1272 mined at 1284 <= T
        self.assertEqual((1290 + 6) % 24, 0)
        self.assertLessEqual(d.due_height(d.LLMQS[103], 1290 - 18, 1), 1290)

    def test_mnhf_request_id(self):
        # feature_mnehf.py: hash256(ser_string(b"mnhf") + struct.pack("<Q", bit))[::-1]
        buf = bytes([4]) + b"mnhf" + struct.pack("<Q", 13)
        want = _sha256d(buf)[::-1].hex()
        self.assertEqual(d.mnhf_request_id(13), want)
        self.assertNotEqual(d.mnhf_request_id(13), d.mnhf_request_id(14))


def _block(height, txs):
    return {"height": height, "tx": txs}


def _qctx(height, t, qi, qh, pk="ab" * 48, signers=3):
    return {"txid": f"tx-{t}-{qi}-{height}", "type": 6,
            "qcTx": {"height": height, "commitment": {"llmqType": t, "quorumIndex": qi, "quorumHash": qh,
                                                      "quorumPublicKey": pk, "signersCount": signers}}}


class Blocks(unittest.TestCase):
    def test_parse_and_invariants(self):
        blk = _block(467, [{"txid": "cb", "type": 5}, _qctx(467, 100, 0, "h100"), _qctx(467, 106, 0, "h106")])
        recs = d.qc_records_from_block(blk)
        self.assertEqual([(r["type"], r["null"]) for r in recs], [(100, False), (106, False)])
        self.assertEqual(d.block_invariants(recs, [(100, 0, "h100"), (106, 0, "h106")]), [])

    def test_null_in_due_block_fails(self):
        recs = d.qc_records_from_block(_block(467, [_qctx(467, 100, 0, "h", pk=d.NULL_PK, signers=0)]))
        self.assertTrue(recs[0]["null"])
        self.assertTrue(d.block_invariants(recs, [(100, 0, "h")]))

    def test_excluded_type_and_duplicates_fail(self):
        recs = d.qc_records_from_block(_block(500, [_qctx(500, 102, 0, "a"), _qctx(500, 103, 0, "b"),
                                                    _qctx(500, 103, 0, "c")]))
        errs = d.block_invariants(recs, [])
        self.assertTrue(any("type 102" in e for e in errs))
        self.assertTrue(any("two commitments" in e for e in errs))

    def test_payload_height(self):
        blk = _block(467, [_qctx(466, 100, 0, "h")])
        self.assertTrue(d.block_invariants(d.qc_records_from_block(blk), []))

    def test_mnhf(self):
        blk = _block(650, [{"txid": "m", "type": 7, "mnhfTx": {"signal": {"versionBit": 13, "quorumHash": "q"}}}])
        self.assertEqual(d.mnhf_records_from_block(blk), [{"height": 650, "txid": "m", "bit": 13, "quorum_hash": "q"}])


class Bls(unittest.TestCase):
    def test_generator(self):
        self.assertEqual((d.G1_Y ** 2 - d.G1_X ** 3 - 4) % d.BLS_P, 0)
        self.assertEqual(d.g1_mul(d.BLS_R)[2], 0)
        # the standard compressed G1 generator
        self.assertTrue(d.g1_compress(d.g1_mul(1)).startswith("97f1d3a73197d794"))

    def test_linear(self):
        a, b = 0x1234567, 0xABCDEF
        lhs = d.g1_compress(d.g1_mul(a + b))
        rhs = d.g1_compress(d._jac_add(d.g1_mul(a), d.g1_mul(b)))
        self.assertEqual(lhs, rhs)
        self.assertEqual(d.g1_compress(d._jac_add(d.g1_mul(a), d.g1_mul(a))), d.g1_compress(d.g1_mul(2 * a)))

    def test_pk_range(self):
        with self.assertRaises(ValueError):
            d.bls_pk_from_sk("00" * 32)
        with self.assertRaises(ValueError):
            d.bls_pk_from_sk(d.BLS_R.to_bytes(32, "big").hex())


def _split(secret, threshold, protx_ids, rng):
    """Shamir split as dashbls Threshold::PrivateKeyShare: f(id) with id = share_id_scalar(proTxHash)."""
    coeffs = [secret] + [rng.randrange(1, d.BLS_R) for _ in range(threshold - 1)]
    out = []
    for pid in protx_ids:
        x = d.share_id_scalar(pid)
        y = sum(c * pow(x, k, d.BLS_R) for k, c in enumerate(coeffs)) % d.BLS_R
        out.append({"id": pid, "sk": y.to_bytes(32, "big").hex()})
    return out


class Recover(unittest.TestCase):
    def setUp(self):
        self.rng = random.Random(875)
        self.ids = [hashlib.sha256(b"protx%d" % i).hexdigest() for i in range(4)]
        self.secret = self.rng.randrange(1, d.BLS_R)
        self.pk = d.bls_pk_from_sk(self.secret.to_bytes(32, "big").hex())

    def test_every_subset(self):
        shares = _split(self.secret, 3, self.ids, self.rng)
        for drop in range(4):
            sub = [s for i, s in enumerate(shares) if i != drop]
            self.assertEqual(int(d.recover_quorum_sk(self.pk, 3, sub), 16), self.secret)
        self.assertEqual(int(d.recover_quorum_sk(self.pk, 3, shares), 16), self.secret)

    def test_mixed_quorums_fail(self):
        a = _split(self.secret, 3, self.ids, self.rng)
        b = _split(self.secret + 1, 3, self.ids, self.rng)
        with self.assertRaises(d.QuorumSkError):
            d.recover_quorum_sk(self.pk, 3, a[:2] + b[2:])

    def test_too_few(self):
        with self.assertRaises(d.QuorumSkError):
            d.recover_quorum_sk(self.pk, 3, _split(self.secret, 3, self.ids, self.rng)[:2])


def _basic_from_legacy(pk_hex):
    """dashbls G1Element::SerializeToArray (elements.cpp:178-198): legacy form flags the sign of y with 0x80; the
    basic form sets 0x80 (compressed) and flags the sign with 0x20."""
    b = bytearray.fromhex(pk_hex)
    sign = b[0] & 0x80
    b[0] = (b[0] & 0x1F) | 0x80 | (0x20 if sign else 0)
    return b.hex()


class CoreThresholdVector(unittest.TestCase):
    """Core v24.0.0 src/dashbls/src/test.cpp:1253-1322 (TEST_CASE "Schemes", SECTION "Legacy scheme"): ten secret
    key shares, their ids in RPC order (Core's test reverses each before use) and the threshold public key. Copied
    verbatim into test-vectors/evo-driver/core-v24.json. Pins the share-id byte order and the G1 sign flag."""

    THRESHOLD = 6  # Core's example is a degree-5 split: any 6 of its 10 shares recover the same key

    def setUp(self):
        v = _vectors()["dashbls_legacy_threshold"]
        self.shares = [{"id": i, "sk": sk} for i, sk in zip(v["ids"], v["secret_key_shares"])]
        self.pk = _basic_from_legacy(v["threshold_public_key_legacy"])

    def test_vector_has_sign_flag(self):
        # y of Core's threshold key is the larger root, so the basic encoding carries 0x20
        self.assertEqual(int(self.pk[:2], 16) & 0xE0, 0xA0)

    def test_recovers_core_threshold_pk(self):
        sk = d.recover_quorum_sk(self.pk, self.THRESHOLD, self.shares)
        self.assertEqual(d.bls_pk_from_sk(sk), self.pk)
        for lo in range(len(self.shares) - self.THRESHOLD + 1):
            sub = self.shares[lo:lo + self.THRESHOLD]
            self.assertEqual(d.recover_quorum_sk(self.pk, self.THRESHOLD, sub), sk)

    def test_rpc_order_ids_fail(self):
        # ids read without the reversal Core's CBLSId storage implies: a different key, so sk·G != pk
        wrong = [{"id": bytes.fromhex(s["id"])[::-1].hex(), "sk": s["sk"]} for s in self.shares]
        with self.assertRaises(d.QuorumSkError):
            d.recover_quorum_sk(self.pk, self.THRESHOLD, wrong)

    def test_legacy_encoding_is_not_basic(self):
        legacy = _vectors()["dashbls_legacy_threshold"]["threshold_public_key_legacy"]
        with self.assertRaises(d.QuorumSkError):
            d.recover_quorum_sk(legacy, self.THRESHOLD, self.shares)


class CoreBasicKeygenVector(unittest.TestCase):
    """Core v24.0.0 src/dashbls/src/test.cpp:336-365 ("Chia test vectors 1 (Basic)"): sk1 and pk1 of KeyGen(32 x 0x00).
    Its y is the smaller root and odd: the 0x20 flag follows y > (p-1)/2, not y's parity."""

    def test_pk_from_core_sk(self):
        v = _vectors()["dashbls_basic_keygen"]
        self.assertEqual(int(v["pk"][:2], 16) & 0xE0, 0x80)
        self.assertEqual(d.bls_pk_from_sk(v["sk"]), v["pk"])


class MnhfVector(unittest.TestCase):
    """A Core v24.0.0 regtest bit-13 MNHF signal tx (test-vectors/evo-driver/PROVENANCE.md)."""

    def setUp(self):
        self.v = _vectors()["mnhf_signal_tx"]
        self.raw = bytes.fromhex(self.v["hex"])

    def test_signal_tx_rebuilds_core_tx(self):
        tx = d.mnhf_signal_tx(self.v["bit"], self.v["quorum_hash"], self.raw[-96:])
        self.assertEqual(tx.hex(), self.v["hex"])
        self.assertEqual(_sha256d(tx)[::-1].hex(), self.v["txid"])

    def test_msg_hash_is_the_zero_sig_tx(self):
        want = _sha256d(self.raw[:-96] + bytes(96))[::-1].hex()
        self.assertEqual(d.mnhf_msg_hash(self.v["bit"], self.v["quorum_hash"]), want)
        self.assertNotEqual(d.mnhf_msg_hash(self.v["bit"], "00" * 32), want)
        self.assertNotEqual(d.mnhf_msg_hash(self.v["bit"] + 1, self.v["quorum_hash"]), want)


class Checks(unittest.TestCase):
    def test_selftest_passes(self):
        self.assertEqual(d.selftest(), 0)

    def test_signing_view_lags_by_offset(self):
        recs = d._fixture_records(672)
        view = d.signing_view_quorums(recs, 690)
        # cycle 672 is mined at 683/684 > 690 - 8, so the 648 and 624 quorums are the ones Core selects at 690
        self.assertEqual({qh for t, qh in view if t == 100}, {"100-624", "100-648"})
        self.assertEqual({qh for t, qh in view if t == 103}, {"103-648", "103-649"})
        # DIP0024: the latest quorum of each index, even when one index's latest cycle is older
        lopsided = [r for r in recs if not (r["type"] == 103 and r["base_height"] == 649)]
        self.assertEqual({qh for t, qh in d.signing_view_quorums(lopsided, 690) if t == 103}, {"103-648", "103-625"})
        self.assertEqual({qh for t, qh in d.signing_view_quorums(recs, 692) if t == 100}, {"100-648", "100-672"})

    def test_finalized_unmined(self):
        every = lambda t, b: True  # noqa: E731
        recs = d._fixture_records(1248)
        # base 1272: LLMQ_TEST/PLATFORM finalize at tip 1282 and are due 1283; DIP0024 index 0 finalizes at 1282 too
        self.assertEqual(sorted(d.finalized_unmined(1282, recs, every)),
                         [(100, 0, 1272), (103, 0, 1272), (106, 0, 1272)])
        self.assertEqual(sorted(d.finalized_unmined(1283, recs, every)),
                         [(100, 0, 1272), (103, 0, 1272), (103, 1, 1273), (106, 0, 1272)])
        self.assertEqual(d.finalized_unmined(1281, recs, every), [])
        self.assertEqual(d.finalized_unmined(1290, d._fixture_records(1272), every), [])
        self.assertEqual(d.finalized_unmined(1283, recs, lambda t, b: False), [])

    def test_q2_fails_on_too_few(self):
        # selftest covers one cycle of each type and a single llmq_test_platform commitment
        recs = [r for r in d._fixture_records(1272) if not (r["type"] == d.LLMQ_TEST and r["base_height"] > 504)]
        self.assertFalse(d.check_q2(recs, 1290)[1])

    def test_q3_fails_on_a_gap(self):
        recs = d._fixture_records(1272)
        self.assertTrue(d.check_q3(recs, 990)[1])
        gap = [r for r in recs if not (r["type"] == d.LLMQ_TEST_DIP0024 and r["base_height"] in (576, 577))]
        ok, detail = d.check_q3(gap, 990)[1:]
        self.assertFalse(ok)
        self.assertIn("[576]", detail)
        # a null-only cycle is the same gap: only non-null commitments count
        nulls = [dict(r, null=True) if r["type"] == d.LLMQ_TEST_DIP0024 and r["base_height"] == 577 else r
                 for r in recs]
        self.assertFalse(d.check_q3(nulls, 990)[1])
        # warm-up before the first complete cycle is not a gap
        late = [r for r in recs if not (r["type"] == d.LLMQ_TEST_DIP0024 and r["base_height"] in (504, 505))]
        self.assertTrue(d.check_q3(late, 990)[1])
        # a cycle not yet due at T is not a gap
        self.assertTrue(d.check_q3(recs, 1283)[1])

    def test_q7_on_a_replay_driver_is_not_vacuous(self):
        drv = _driver(_Node())
        self.assertIsNone(drv.first_base)
        recs = d._fixture_records(1248)
        res = dict((c, ok) for c, ok, _ in drv.run_q_checks("S5", 1282, recs, {}, set(), [], set(), []))
        self.assertFalse(res["Q7"])
        res = dict((c, ok) for c, ok, _ in drv.run_q_checks("S4", 1290, d._fixture_records(1272), {}, set(), [], set(),
                                                             []))
        self.assertTrue(res["Q7"])

    def test_completed_cycles_need_both_indexes(self):
        recs = [{"height": 492, "type": 103, "index": 0, "null": False, "base_height": 480, "quorum_hash": "a"}]
        self.assertEqual(d.completed_dip0024_cycles(recs, 600), [])
        recs.append({"height": 492, "type": 103, "index": 1, "null": False, "base_height": 481, "quorum_hash": "b"})
        self.assertEqual(d.completed_dip0024_cycles(recs, 600), [480])
        self.assertEqual(d.completed_dip0024_cycles(recs, 491), [])


class _RpcError(Exception):
    """The shape of the framework's JSONRPCException: .error = {"code", "message"}."""

    def __init__(self, code, message):
        super().__init__(message)
        self.error = {"code": code, "message": message}


class _Node:
    """A node stub: `quorum` answers from `answers` (a dict or a callable) or raises `exc`."""

    def __init__(self, exc=None, answers=None, mempool=(), running=True):
        self.exc, self.answers, self.mempool, self.running = exc, answers or {}, list(mempool), running
        self.calls = []
        self.txs = {}
        self.blocks = {}
        self.hashes = {}
        self.count = 0

    def quorum(self, *args):
        self.calls.append(args)
        if callable(self.answers):
            return self.answers(*args)
        if args[0] in self.answers:
            return self.answers[args[0]]
        if self.exc:
            raise self.exc
        return {"quorumHash": "q"}

    def getrawmempool(self):
        return list(self.mempool)

    def getrawtransaction(self, txid, verbose):
        return self.txs[txid]

    def getblockcount(self):
        return self.count

    def getblockhash(self, h):
        return self.hashes[h]

    def getblock(self, bh, verbosity):
        return self.blocks[bh]


def _driver(node0, nodes=(), mninfos=()):
    fw = SimpleNamespace(log=logging.getLogger("test_driver"), options=SimpleNamespace(timeout_factor=1),
                         nodes=[node0, *nodes])
    return d.ChainDriver(fw, list(mninfos), d.PyQuorumKeys())


class RpcErrors(unittest.TestCase):
    def test_no_quorum_is_a_state(self):
        drv = _driver(_Node(exc=_RpcError(d.RPC_MISC_ERROR, "no quorums active"), answers={"list": {}}))
        self.assertFalse(drv.signing_ready(d.LLMQ_TEST))
        self.assertEqual(drv.signing_active(), set())

    def test_other_errors_propagate(self):
        for exc in (_RpcError(d.RPC_MISC_ERROR, "Work queue depth exceeded"),
                    _RpcError(-8, "no quorums active"),  # RPC_INVALID_PARAMETER: right text, wrong code
                    ConnectionResetError("node gone")):
            drv = _driver(_Node(exc=exc, answers={"list": {}}))
            with self.assertRaises(type(exc)):
                drv.signing_ready(d.LLMQ_TEST)
            with self.assertRaises(type(exc)):
                drv.signing_active()
            drv._ehf = {"bit": 13, "release": 650, "id": d.mnhf_request_id(13)}
            with self.assertRaises(type(exc)):
                drv._ehf_snapshot(600)

    def test_chainlock_error_codes(self):
        self.assertTrue(d.is_rpc_error(_RpcError(-32603, "Unable to find any ChainLock"), d.NO_CHAINLOCK))
        self.assertFalse(d.is_rpc_error(_RpcError(-1, "Unable to find any ChainLock"), d.NO_CHAINLOCK))
        self.assertFalse(d.is_rpc_error(RuntimeError("Unable to find any ChainLock"), d.NO_CHAINLOCK))


class _Mn:
    def __init__(self, protx, idx, node):
        self.proTxHash, self.nodeIdx, self._node = protx, idx, node

    def get_node(self, fw):
        return self._node


class EhfQuiet(unittest.TestCase):
    """_ehf_snapshot reads Core's state: three masternodes, all members of the selected LLMQ_TEST quorum "sel"."""
    BIT = 13
    ID = d.mnhf_request_id(13)

    Q = {"q1": "11" * 32, "sel": "22" * 32, "qx": "33" * 32}

    def _setup(self, holds, mempools=None, invalid=()):
        """holds[i] = label of the quorum whose recovered sig MN i holds, or None. node0 holds signal t1 of q1."""
        holds = [self.Q[q] if q else None for q in holds]
        node0 = _Node(answers=lambda *a: {"selectquorum": {"quorumHash": self.Q["sel"]},
                                          "info": {"members": [{"proTxHash": f"p{i}", "valid": i not in invalid}
                                                               for i in range(3)]}}[a[0]],
                      mempool=("t1",))
        node0.txs = {t: {"type": 7, "hex": "00", "mnhfTx": {"signal": {"versionBit": self.BIT, "quorumHash": q}}}
                     for t, q in (("t1", self.Q["q1"]), ("t2", self.Q["sel"]))}
        mns, nodes = [], []
        for i, q in enumerate(holds):
            def answers(*a, q=q):
                if a[0] == "isconflicting":
                    return q is not None
                if a[0] == "hasrecsig":
                    return q is not None and a[3] == d.mnhf_msg_hash(self.BIT, q)
                raise AssertionError(a)
            n = _Node(answers=answers, mempool=(mempools or {}).get(i, ()))
            n.txs = node0.txs
            nodes.append(n)
            mns.append(_Mn(f"p{i}", i + 1, n))
        drv = _driver(node0, nodes, mns)
        drv._ehf = {"bit": self.BIT, "release": 650, "id": self.ID}
        return drv

    def test_quiet(self):
        reason, snap = self._setup(["q1", "q1", None])._ehf_snapshot(600)
        self.assertIsNone(reason)
        self.assertIsNotNone(snap)

    def test_recovered_signal_not_at_node0(self):
        # MN 1 holds the recovered sig of the selected quorum, whose signal tx node0 does not have yet
        reason, _ = self._setup(["q1", "sel", None])._ehf_snapshot(600)
        self.assertIn("not at node0", reason)

    def test_recovered_sig_of_unknown_quorum(self):
        reason, _ = self._setup(["q1", "qx", None])._ehf_snapshot(600)
        self.assertIn("no known", reason)

    def test_threshold_members_lacking(self):
        reason, _ = self._setup(["q1", None, None])._ehf_snapshot(600)
        self.assertIn("have no recovered sig", reason)

    def test_signal_in_a_masternode_mempool(self):
        reason, _ = self._setup(["q1", "q1", None], mempools={2: ("t2",)})._ehf_snapshot(600)
        self.assertIn("not at node0", reason)

    def test_mined_signal_counts_as_signalled(self):
        drv = self._setup(["q1", "sel", None])
        drv.mnhf = {612: [{"height": 612, "txid": "t2", "bit": self.BIT, "quorum_hash": self.Q["sel"]}]}
        self.assertIsNone(drv._ehf_snapshot(620)[0])

    def test_invalid_members_do_not_count_as_lacking(self):
        self.assertIsNotNone(self._setup(["q1", None, None])._ehf_snapshot(600)[0])
        self.assertIsNone(self._setup(["q1", None, None], invalid=(2,))._ehf_snapshot(600)[0])

    def test_after_release_only_propagation_counts(self):
        reason, _ = self._setup(["q1", "sel", None])._ehf_snapshot(650)
        self.assertIsNone(reason)


class MineBlockOrder(unittest.TestCase):
    """mine_block: settle, MNHF quiet wait, mocktime bump, MNHF hold re-scan, generate, per-block checks."""

    def _driver(self, enabled, ehf):
        calls = []
        node0 = _Node()
        node0.count = 600
        drv = _driver(node0)
        drv.fw.bump_mocktime = lambda *a, **k: calls.append("bump")
        drv.fw.generate = lambda *a, **k: calls.append("generate") or ["bh"]
        drv.fw.sync_blocks = lambda *a, **k: None
        drv._settle = lambda h: calls.append("settle") or []
        drv._ehf_quiet = lambda h: calls.append("quiet")
        drv._ehf_hold = lambda h: calls.append("hold")
        drv._record_block = lambda bh, due, driven=False: calls.append(f"record driven={driven}")
        drv.enabled_at = 441 if enabled else None
        drv._ehf = {"bit": 13} if ehf else None
        return drv, calls

    def test_order_with_the_hold(self):
        drv, calls = self._driver(True, True)
        drv.mine_block()
        self.assertEqual(calls, ["settle", "quiet", "bump", "hold", "generate", "record driven=True"])

    def test_order_without_the_hold(self):
        drv, calls = self._driver(True, False)
        drv.mine_block()
        self.assertEqual(calls, ["settle", "bump", "generate", "record driven=True"])

    def test_before_enable(self):
        drv, calls = self._driver(False, True)
        drv.mine_block()
        self.assertEqual(calls, ["bump", "generate", "record driven=False"])


class QuietWait(unittest.TestCase):
    def _driver(self, snapshots):
        drv = _driver(_Node())
        drv.fw.bump_mocktime = lambda *a, **k: None
        drv._ehf = {"bit": 13, "quiet": {"blocks": 0, "polls": 0, "seconds": 0.0, "max_seconds": 0.0, "max_at": None}}
        it = iter(snapshots)
        drv._ehf_snapshot = lambda h: next(it)
        return drv

    def test_needs_identical_quiet_polls_and_reports(self):
        snaps = [(None, "a"), ("not quiet", None)] + [(None, "b")] * d.EHF_QUIET_POLLS
        drv = self._driver(snaps)
        drv._ehf_quiet(600)
        q = drv._ehf["quiet"]
        self.assertEqual((q["blocks"], q["polls"]), (1, len(snaps)))

    def test_bounded(self):
        drv = self._driver([("3 members lack it", None)] * 10)
        drv.settle_timeout = 0.1
        with self.assertRaisesRegex(d.DriverError, "3 members lack it"):
            drv._ehf_quiet(600)


def _null_block(h):
    qc = {"llmqType": 100, "quorumIndex": 0, "quorumHash": f"h{h}", "quorumPublicKey": d.NULL_PK, "signersCount": 0}
    return {"height": h, "tx": [{"txid": f"qc{h}", "type": 6, "qcTx": {"height": h, "commitment": qc}}]}


class Capture(unittest.TestCase):
    def _driver(self, upto, enabled_at=441):
        node0 = _Node()
        for h in range(1, upto + 1):
            node0.hashes[h] = f"b{h}"
            node0.blocks[f"b{h}"] = {"height": h, "tx": []}
        drv = _driver(node0)
        drv.enabled_at = enabled_at
        for h in range(1, upto + 1):
            drv._record_block(f"b{h}", [], driven=h >= enabled_at)
        return drv, node0

    def test_all_checked(self):
        drv, _ = self._driver(445)
        self.assertEqual(drv.all_records(445), [])

    def test_block_mined_outside_the_driver_fails(self):
        drv, node0 = self._driver(445)
        node0.hashes[446] = "b446"
        node0.blocks["b446"] = {"height": 446, "tx": []}
        with self.assertRaisesRegex(d.DriverError, r"\[446\].*not mined through"):
            drv.all_records(446)

    def test_block_that_failed_its_check_fails(self):
        drv, node0 = self._driver(445)
        node0.hashes[446] = "b446"
        node0.blocks["b446"] = _null_block(446)
        with self.assertRaises(d.DriverError):
            drv._record_block("b446", [(100, 0, "h446")], driven=True)
        self.assertIn(446, drv.records)
        with self.assertRaisesRegex(d.DriverError, "not mined through"):
            drv.all_records(446)

    def test_replaced_block_fails(self):
        drv, node0 = self._driver(445)
        node0.hashes[443] = "other"
        with self.assertRaisesRegex(d.DriverError, "replaced"):
            drv.all_records(445)

    def test_blocks_before_enable_are_filled_in(self):
        drv, node0 = self._driver(445)
        del drv.records[10]
        drv.all_records(445)
        self.assertIn(10, drv.records)

    def test_capture_without_dip0024_readiness_fails(self):
        drv, node0 = self._driver(445)
        node0.count = 445
        drv.first_base = 504
        with self.assertRaisesRegex(d.DriverError, "readiness was never detected"):
            drv.capture("S5", 445)


SECRET = "0b" * 32


class KeyMaterial(unittest.TestCase):
    def test_core_disagreement_hides_the_secret(self):
        core = _Node()
        core.bls = lambda *a: {"scheme": "basic", "public": "ab" * 48, "secret": SECRET}
        with self.assertRaises(d.QuorumSkError) as cm:
            d.PyQuorumKeys(core).pk_from_sk(SECRET)
        self.assertNotIn(SECRET, str(cm.exception))

    def test_rpc_log_is_off_during_secret_calls(self):
        seen = []
        core = _Node()

        def bls(*a):
            seen.append(logging.getLogger("BitcoinRPC").disabled)
            return {"scheme": "basic", "public": d.bls_pk_from_sk(SECRET), "secret": SECRET}
        core.bls = bls
        d.PyQuorumKeys(core).pk_from_sk(SECRET)
        self.assertEqual(seen, [True])
        self.assertFalse(logging.getLogger("BitcoinRPC").disabled)

    def test_parse_errors_hide_the_value(self):
        bad = "zz" + SECRET[2:]
        with self.assertRaises(ValueError) as cm:
            d.bls_pk_from_sk(bad)
        self.assertNotIn(bad, str(cm.exception))
        with self.assertRaises(d.QuorumSkError) as cm:
            d.recover_quorum_sk("00", 2, [{"id": "01" * 32, "sk": bad}, {"id": "02" * 32, "sk": SECRET}])
        self.assertNotIn(bad, str(cm.exception))

    def _tool(self, tmp, body):
        path = os.path.join(tmp, "tool")
        with open(path, "w") as f:
            f.write("#!/bin/sh\n" + body + "\n")
        os.chmod(path, 0o700)
        return path

    def test_key_tool_timeout(self):
        with tempfile.TemporaryDirectory() as tmp:
            keys = d.TestkitQuorumKeys(self._tool(tmp, "sleep 30"), timeout=0.5)
            with self.assertRaisesRegex(d.QuorumSkError, "timed out"):
                keys.quorum_sk("ab" * 48, 1, [{"id": "01" * 32, "sk": SECRET}])

    def test_key_tool_output_is_withheld(self):
        with tempfile.TemporaryDirectory() as tmp:
            keys = d.TestkitQuorumKeys(self._tool(tmp, f"cat >&2; echo {SECRET} >&2; exit 1"), timeout=10)
            with self.assertRaises(d.QuorumSkError) as cm:
                keys.quorum_sk("ab" * 48, 1, [{"id": "01" * 32, "sk": SECRET}])
            self.assertNotIn(SECRET, str(cm.exception))
            keys = d.TestkitQuorumKeys(self._tool(tmp, f"echo not-json-{SECRET}"), timeout=10)
            with self.assertRaises(d.QuorumSkError) as cm:
                keys.quorum_sk("ab" * 48, 1, [{"id": "01" * 32, "sk": SECRET}])
            self.assertNotIn(SECRET, str(cm.exception))


class UnknownArgs(unittest.TestCase):
    def _error(self, withhold):
        import argparse
        import contextlib
        import io
        parser = argparse.ArgumentParser(allow_abbrev=False)
        parser.add_argument("--key-seed-file")
        if withhold:
            d.withhold_unknown_args(parser)
        err = io.StringIO()
        with contextlib.redirect_stderr(err), self.assertRaises(SystemExit):
            parser.parse_args(["--key-seed", SECRET])
        return err.getvalue()

    def test_unknown_values_are_withheld(self):
        self.assertIn(SECRET, self._error(withhold=False))
        text = self._error(withhold=True)
        self.assertNotIn(SECRET, text)
        self.assertIn("unrecognized arguments (values withheld", text)


def _testkit_keys():
    """TESTKIT_KEYS, else the workspace's default debug build, else None."""
    if os.environ.get("TESTKIT_KEYS"):
        return os.environ["TESTKIT_KEYS"]
    default = os.path.join(os.path.dirname(os.path.abspath(__file__)), *[os.pardir] * 4, "target", "debug",
                           "testkit-keys")
    return os.path.normpath(default) if os.access(default, os.X_OK) else None


@unittest.skipUnless(_testkit_keys(), "no testkit-keys: set TESTKIT_KEYS or build it into <repo>/target/debug")
class TestkitQuorumSkBinary(unittest.TestCase):
    """TestkitQuorumKeys over the real `testkit-keys quorum-sk` (dash-testkit), on Core's threshold vector."""

    def setUp(self):
        v = _vectors()["dashbls_legacy_threshold"]
        self.shares = [{"id": i, "sk": sk} for i, sk in zip(v["ids"], v["secret_key_shares"])]
        self.pk = _basic_from_legacy(v["threshold_public_key_legacy"])
        self.keys = d.TestkitQuorumKeys(_testkit_keys(), timeout=60)

    def test_recovers_core_threshold_pk(self):
        sk = self.keys.quorum_sk(self.pk, CoreThresholdVector.THRESHOLD, self.shares)
        self.assertEqual(sk, d.recover_quorum_sk(self.pk, CoreThresholdVector.THRESHOLD, self.shares))
        self.assertEqual(d.bls_pk_from_sk(sk), self.pk)
        entry = {"type": 100, "hash": "h", "pk": self.pk, "sk": sk}
        _, ok, detail = d.check_q5({(100, "h")}, [entry], d.bls_pk_from_sk, recovered_by=self.keys.recovered_by)
        self.assertTrue(ok, detail)
        self.assertIn("recovered by testkit-keys quorum-sk", detail)

    def test_stored_order_ids_and_mixed_quorums_fail(self):
        stored = [{"id": bytes.fromhex(s["id"])[::-1].hex(), "sk": s["sk"]} for s in self.shares]
        rng = random.Random(85)
        ids = [hashlib.sha256(b"protx%d" % i).hexdigest() for i in range(4)]
        secret = rng.randrange(1, d.BLS_R)
        a = _split(secret, 3, ids, rng)
        b = _split(secret + 1, 3, ids, rng)
        pk = d.bls_pk_from_sk(secret.to_bytes(32, "big").hex())
        self.assertEqual(int(self.keys.quorum_sk(pk, 3, a), 16), secret)
        for pk_, threshold, shares in [(self.pk, CoreThresholdVector.THRESHOLD, stored),
                                       *((pk, 3, a[:j] + [b[j]] + a[j + 1:]) for j in range(4))]:
            with self.assertRaises(d.QuorumSkError) as cm:
                self.keys.quorum_sk(pk_, threshold, shares)
            self.assertIn("exit 1", str(cm.exception))
            for s in shares:
                self.assertNotIn(s["sk"], str(cm.exception))

    def test_twin_picks_the_same_subsets(self):
        # 2 of 5, so the middle share is in neither subset; one foreign share in each position
        rng = random.Random(8585)
        ids = [hashlib.sha256(b"twin%d" % i).hexdigest() for i in range(5)]
        secret = rng.randrange(1, d.BLS_R)
        pk = d.bls_pk_from_sk(secret.to_bytes(32, "big").hex())
        a, b = _split(secret, 2, ids, rng), _split(secret + 1, 2, ids, rng)
        for j in range(5):
            shares = a[:j] + [b[j]] + a[j + 1:]
            try:
                want = d.recover_quorum_sk(pk, 2, shares)
            except d.QuorumSkError:
                want = None
            try:
                got = self.keys.quorum_sk(pk, 2, shares)
            except d.QuorumSkError:
                got = None
            self.assertEqual(got, want, j)


class TwinRefusals(unittest.TestCase):
    """recover_quorum_sk refuses what `testkit-keys quorum-sk` refuses, before any recovery."""

    def setUp(self):
        rng = random.Random(851)
        self.ids = [hashlib.sha256(b"refuse%d" % i).hexdigest() for i in range(3)]
        secret = rng.randrange(1, d.BLS_R)
        self.pk = d.bls_pk_from_sk(secret.to_bytes(32, "big").hex())
        self.shares = _split(secret, 2, self.ids, rng)
        self.assertEqual(int(d.recover_quorum_sk(self.pk, 2, self.shares), 16), secret)

    def _refused(self, threshold, shares, reason):
        with self.assertRaisesRegex(d.QuorumSkError, reason):
            d.recover_quorum_sk(self.pk, threshold, shares)

    def test_threshold_below_two(self):
        self._refused(1, self.shares, "threshold is below 2")
        self._refused(0, self.shares, "threshold is below 2")

    def test_ids_not_64_hex(self):
        for bad in [self.ids[1][:62], self.ids[1] + "00", "00" + self.ids[1], self.ids[1][:63] + "g", None]:
            shares = [dict(s) for s in self.shares]
            shares[1]["id"] = bad
            self._refused(2, shares, "share 1 id is not 64 hex digits")

    def test_share_not_below_r(self):
        for value in [0, d.BLS_R, d.BLS_R + 1, 2**256 - 1]:
            shares = [dict(s) for s in self.shares]
            shares[2]["sk"] = value.to_bytes(32, "big").hex()
            self._refused(2, shares, "share 2 is not a valid BLS secret key")
        shares = [dict(s) for s in self.shares]
        shares[2]["sk"] += "00"
        self._refused(2, shares, "share 2 is not a valid BLS secret key")
        shares[2]["sk"] = (d.BLS_R - 1).to_bytes(32, "big").hex()
        with self.assertRaisesRegex(d.QuorumSkError, "subsets disagree|sk·G"):
            d.recover_quorum_sk(self.pk, 2, shares)

    def _outside_subsets(self):
        """2-of-5 shares sorted by stored id bytes, and the index of the middle one, which is in neither subset."""
        rng = random.Random(852)
        ids = [hashlib.sha256(b"outside%d" % i).hexdigest() for i in range(5)]
        secret = rng.randrange(1, d.BLS_R)
        pk = d.bls_pk_from_sk(secret.to_bytes(32, "big").hex())
        shares = sorted(_split(secret, 2, ids, rng), key=lambda s: bytes.fromhex(s["id"])[::-1])
        self.assertEqual(int(d.recover_quorum_sk(pk, 2, shares), 16), secret)
        return pk, shares

    def test_zero_share_outside_the_subsets(self):
        pk, shares = self._outside_subsets()
        shares[2] = {**shares[2], "sk": "00" * 32}
        with self.assertRaisesRegex(d.QuorumSkError, "share 2 is not a valid BLS secret key"):
            d.recover_quorum_sk(pk, 2, shares)

    def test_duplicate_id_outside_the_subsets(self):
        pk, shares = self._outside_subsets()
        for dup in [dict(shares[1]), {**shares[1], "id": shares[1]["id"].upper()}]:
            with self.assertRaisesRegex(d.QuorumSkError, "two shares have the same id"):
                d.recover_quorum_sk(pk, 2, [*shares[:2], dup, *shares[2:]])

    @unittest.skipUnless(_testkit_keys(), "no testkit-keys binary")
    def test_the_tool_refuses_the_same(self):
        keys = d.TestkitQuorumKeys(_testkit_keys(), timeout=60)
        cases = [(1, self.shares), (2, [*self.shares[:2], {**self.shares[2], "sk": d.BLS_R.to_bytes(32, "big").hex()}])]
        pk, outside = self._outside_subsets()
        for shares in [[*outside[:2], {**outside[2], "sk": "00" * 32}, *outside[3:]],
                       [*outside[:2], dict(outside[1]), *outside[2:]]]:
            with self.assertRaisesRegex(d.QuorumSkError, "exit 1"):
                keys.quorum_sk(pk, 2, shares)
        for threshold, shares in cases:
            with self.assertRaisesRegex(d.QuorumSkError, "exit 1"):
                keys.quorum_sk(self.pk, threshold, shares)
        with self.assertRaisesRegex(d.QuorumSkError, "exit 2"):
            keys.quorum_sk(self.pk, 2, [*self.shares[:2], {**self.shares[2], "id": self.ids[2][:62]}])


class PrivateFiles(unittest.TestCase):
    def test_written_0600_from_creation(self):
        old = os.umask(0o002)
        try:
            with tempfile.TemporaryDirectory() as tmp:
                path = os.path.join(tmp, "S4-quorums.json")
                with open(path, "w") as f:
                    f.write("{}")
                os.chmod(path, 0o664)
                d.write_private_json(path, {"sk": SECRET})
                self.assertEqual(stat.S_IMODE(os.stat(path).st_mode), 0o600)
                with open(path) as f:
                    self.assertEqual(json.load(f), {"sk": SECRET})
                self.assertEqual(os.listdir(tmp), ["S4-quorums.json"])
        finally:
            os.umask(old)

    def test_seed_file_must_be_private(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "seed")
            with open(path, "w") as f:
                f.write("00" * 32)
            os.chmod(path, 0o640)
            with self.assertRaises(ValueError):
                d.check_private_file(path)
            os.chmod(path, 0o600)
            d.check_private_file(path)
            with self.assertRaises(ValueError):
                d.check_private_file(tmp)

    def test_seed_given_as_the_path_is_not_shown(self):
        with tempfile.TemporaryDirectory() as tmp:
            for value in [SECRET, SECRET.upper(), f" {SECRET}\n"]:
                with self.assertRaises(ValueError) as cm:
                    d.check_private_file(value)
                text = str(cm.exception)
                self.assertNotIn(SECRET.lower(), text.lower())
                self.assertIn("--key-seed-file (value not shown): cannot stat", text)
            path = os.path.join(tmp, SECRET)
            with open(path, "w") as f:
                f.write("00" * 32)
            os.chmod(path, 0o644)
            with self.assertRaises(ValueError) as cm:
                d.check_private_file(path)
            self.assertIn(SECRET, str(cm.exception))
            cwd = os.getcwd()
            os.chdir(tmp)
            try:
                with self.assertRaises(ValueError) as cm:
                    d.check_private_file(SECRET)
            finally:
                os.chdir(cwd)
            self.assertNotIn(SECRET, str(cm.exception))
            self.assertIn("--key-seed-file (value not shown): must be a regular file", str(cm.exception))
            missing = os.path.join(tmp, "no-such-seed")
            with self.assertRaises(ValueError) as cm:
                d.check_private_file(missing)
            self.assertIn(repr(missing), str(cm.exception))


if __name__ == "__main__":
    unittest.main()
