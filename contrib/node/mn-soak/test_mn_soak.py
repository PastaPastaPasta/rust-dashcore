#!/usr/bin/env python3
"""Unit tests for mn-soak.py's summary and feature_mn_soak.py's pure helpers.

Run with `mn-soak.py selftest` or `python3 -m unittest test_mn_soak` from this directory.
"""

import argparse
import hashlib
import importlib.util
import json
import sys
import tempfile
import types
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent


def load_module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def stub_test_framework():
    """feature_mn_soak.py imports Core's test_framework; its two helpers used here are
    serialization only, so stand them in for the import."""
    messages = types.ModuleType("test_framework.messages")
    messages.hash256 = lambda b: hashlib.sha256(hashlib.sha256(b).digest()).digest()
    messages.ser_string = lambda b: bytes([len(b)]) + b
    framework = types.ModuleType("test_framework.test_framework")
    framework.DashTestFramework = object
    package = types.ModuleType("test_framework")
    sys.modules.update({"test_framework": package, "test_framework.messages": messages,
                        "test_framework.test_framework": framework})


soak = load_module("mn_soak", HERE / "mn-soak.py")
stub_test_framework()
feature = load_module("feature_mn_soak", HERE / "feature_mn_soak.py")
sys.modules["feature_mn_soak"] = feature
nonrec = load_module("feature_mn_nonrecovery", HERE / "feature_mn_nonrecovery.py")


def sha256d(b):
    return hashlib.sha256(hashlib.sha256(b).digest()).digest()

RNODE = "3"
# rdashd's VmRSS (kB) per cycle in run-1 (2026-11-22, 34 cycles, rotation warm-up 3 cycles).
RUN1_RSS = [61872, 55300, 63580, 64624, 65752, 66408, 67004, 67536, 67948, 68380, 68812, 69376, 69752, 70616,
            71196, 71580, 71956, 72328, 72696, 73084, 73440, 73920, 74336, 74588, 74960, 75240, 75880, 76192,
            76476, 76628, 75436, 73300, 71800, 72000]
# rdashd's VmRSS (kB) per cycle after warm-up in run-2 (2026-11-22, 6 h, 196 cycles after 3 warm-up).
RUN2_RSS = [
            68840, 69448, 70016, 70660, 71212, 71644, 72040, 72440, 72892, 73684, 74008, 74320, 73340, 73604,
            74316, 74780, 69608, 68984, 69412, 69768, 70100, 70512, 70876, 71208, 71560, 71888, 72204, 72552,
            72836, 73132, 74328, 74592, 75352, 75652, 76036, 76312, 76592, 76860, 77104, 77436, 77808, 78904,
            78652, 78964, 79276, 79596, 79836, 80060, 80328, 80592, 80824, 81076, 81312, 81580, 81836, 82088,
            82332, 82588, 82836, 83068, 83296, 83372, 83504, 82896, 82472, 80968, 81216, 79100, 79376, 79596,
            79816, 80232, 80452, 80692, 80884, 81724, 81940, 82164, 83748, 84100, 84316, 84524, 84656, 84848,
            85012, 82904, 81964, 79180, 79336, 79472, 78760, 77908, 78256, 78548, 78884, 79212, 79348, 78940,
            78728, 79212, 79512, 80792, 81016, 81244, 81492, 81548, 81800, 82040, 82140, 82036, 82304, 82500,
            82760, 83008, 83220, 83440, 83640, 84396, 83948, 84224, 84416, 84600, 83908, 82980, 80896, 81760,
            81984, 82680, 82000, 83352, 82084, 83160, 83456, 83692, 84032, 84112, 85540, 85048, 85168, 85376,
            84976, 85644, 85560, 86280, 86624, 86920, 87376, 86972, 87060, 87844, 87440, 88160, 87156, 88292,
            88636, 89112, 89608, 89792, 90032, 89928, 90412, 90728, 90156, 92532, 92380, 92132, 89976, 89480,
            88728, 89944, 90084, 90380, 90648, 90748, 90792, 91024, 89936, 89092, 90020, 90024, 89988, 90576,
            90852, 91024, 89976, 90180, 90368, 90552, 90736, 89700, 90388, 90332, 90792, 90708, 91076, 91244,
]


def session(**kw):
    s = {"type": 100, "index": 0, "base": 24, "selected": False, "enabled": True, "empty": False, "phases": {},
         "miner_wait": {"wait_s": 0.1, "timed_out": False, "core_has": True},
         "commitment": {"null": False, "validMembers": "07", "signers": "07"}}
    s.update(kw)
    return s


def cycle(**kw):
    c = {"event": "cycle", "base": 24, "wall_s": 200.0, "sessions": [session()],
         "pose": {"penalty": 0, "ban_height": -1}, "chainlock": {"agree": True, "tip_locked_s": 0.5},
         "mempool_sync": {"timed_out": False}, "islock_wait": {"timed_out": False},
         "active_members": {"100": [1, 2, 5], "103": [1, 2, 3, 4]}, "signs": [],
         "cl_recsigs": {"heights": 24, "held": {"0": 0, "1": 24, "2": 24, "3": 0, "4": 0, "5": 24}},
         "rdashd_proc": {"rss_kb": 60000, "fds": 86}, "rdashd_log": []}
    c.update(kw)
    return c


def sign(t=100, dependent=False, member=True, asked=None, recovered=None, members=(1, 3, 5), **kw):
    s = {"type": t, "dependent": dependent, "id": "ab" * 32, "quorum": "cd" * 32, "quorum_members": list(members),
         "rdashd_member": member, "asked": asked if asked is not None else {"1": True, "3": True, "5": True},
         "recovered_after_s": recovered if recovered is not None else {"1": 0.3, "3": 0.3, "5": 0.4},
         "distinct_sigs": 1}
    s.update(kw)
    return s


def full_run(n=16, rss=None, **kw):
    """n healthy cycles covering every tracked type, rotation formed in the first."""
    rss = rss or [60000] * n
    events = []
    for i in range(n):
        sessions = [session(type=t, base=24 * (i + 7), selected=t != 106, rdashd_valid=t != 106, rdashd_signer=t != 106)
                    for t in (100, 102, 103, 106)]
        signs = [sign(t) for t in (100, 102, 103)] + [sign(t, dependent=True, asked={"3": True, "1": True}) for t in (100, 102, 103)]
        events.append(cycle(base=24 * (i + 7), sessions=sessions, signs=signs,
                            active_members={"100": [1, 3, 5], "102": [1, 3, 5], "103": [1, 2, 3, 4]},
                            cl_recsigs={"heights": 24, "held": {"0": 0, "1": 24, "3": 24, "5": 24}},
                            rdashd_proc={"rss_kb": rss[i], "fds": 92}, **kw))
    return events


class Helpers(unittest.TestCase):
    def test_bit_reads_cores_bitset_order(self):
        self.assertEqual([feature.bit("05", i) for i in range(8)], [True, False, True] + [False] * 5)
        self.assertTrue(feature.bit("0001", 8))
        self.assertFalse(feature.bit("01", 8))

    def test_expected_phase_steps_every_two_blocks_up_to_finalize(self):
        self.assertEqual([feature.expected_phase(24, t) for t in range(24, 37)],
                         [1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 6])

    def test_clsig_request_id(self):
        # Held as a ChainLock recovered sig by every llmq_test member in dev run cycles.
        self.assertEqual(feature.clsig_request_id(200),
                         "cf2479969e06eefbfe3f3999f1e52261807e63c371b04d001e5455b789823751")

    def test_session_connections_reads_one_sessions_entry(self):
        status = {"quorumConnections": [
            {"llmqType": "llmq_test", "quorumHash": "aa", "quorumConnections": [{"proTxHash": "x", "connected": True}]},
            {"llmqType": "llmq_test_dip0024", "quorumHash": "aa", "quorumConnections": [
                {"proTxHash": "x", "connected": True}, {"proTxHash": "y", "connected": False},
                {"proTxHash": "z", "connected": True}]},
            {"llmqType": "llmq_test_dip0024", "quorumHash": "bb"}]}
        conns = feature.session_connections(status, "llmq_test_dip0024", "aa")
        self.assertEqual(feature.connected(conns), 2)
        self.assertIsNone(feature.session_connections(status, "llmq_test_dip0024", "bb"))
        self.assertIsNone(feature.session_connections({"error": "x"}, "llmq_test", "aa"))
        self.assertEqual(feature.connected(None), 0)

    def test_tracked_types_and_thresholds(self):
        self.assertEqual(feature.TYPES, soak.TYPE_NAMES)
        self.assertNotIn(104, feature.TYPES)
        self.assertEqual(feature.THRESHOLD, {100: 2, 102: 2, 103: 3})

    def test_log_lines_group_without_hashes_numbers_or_timestamps(self):
        lines = ["2026-11-22T11:00:00Z (mocktime: 1) [x:warning] block=" + "ab" * 32 + " height 12",
                 "2026-11-22T11:00:01Z (mocktime: 2) [x:warning] block=" + "cd" * 32 + " height 13"]
        groups = soak.group_log_lines(lines)
        self.assertEqual(len(groups), 1)
        self.assertEqual(groups[0][1], 2)


class LogPattern(unittest.TestCase):
    PREFIX = "2026-11-22T11:00:00.000000Z (mocktime: 1417713351) [   dashd-v] [dashd-llmq/src/x.rs:1] [dashd_llmq::x] "

    def test_both_scripts_use_one_pattern(self):
        self.assertEqual(feature.LOG_PROBLEM.pattern, soak.LOG_PROBLEM.pattern)

    def test_rdashd_prefix_forms_match(self):
        for level in ("[warning] ", "[error] ", "[llmq:warning] ", "[llmq:error] ", "[mnpayments:error] "):
            self.assertTrue(soak.LOG_PROBLEM.search(self.PREFIX + level + "dkg session failed"), level)

    def test_core_text_forms_match(self):
        for text in ("ERROR: AcceptBlock: bad", "Warning: unknown new rules", "Error: x", "warning: y", "error: z"):
            self.assertTrue(soak.LOG_PROBLEM.search(self.PREFIX + text), text)

    def test_debug_and_info_lines_do_not_match(self):
        for text in ("[mnpayments] IsSuperblockValidationRequired -- WARNING! Node is not fully synced, checked superblock",
                     "[llmq] CDKGSession::ReceiveMessage -- errors=0 warnings=0",
                     "[instantsend] ProcessPendingRetryLockTxs -- retried 2 TXs."):
            self.assertIsNone(soak.LOG_PROBLEM.search(self.PREFIX + text), text)

    def test_panics_match(self):
        self.assertTrue(feature.PANIC.search("thread 'llmq-100-0' panicked at dashd-llmq/src/x.rs:9:5:"))


class Flags(unittest.TestCase):
    def test_a_healthy_cycle_is_not_flagged(self):
        self.assertEqual(soak.cycle_flags(cycle(), RNODE), [])

    def test_selected_without_a_good_commitment(self):
        self.assertIn("selected, no final commitment", soak.session_flags(session(selected=True, commitment=None)))
        self.assertIn("selected, no final commitment",
                      soak.session_flags(session(selected=True, commitment={"null": True})))
        self.assertIn("selected, not valid in the final commitment",
                      soak.session_flags(session(selected=True, rdashd_valid=False, rdashd_signer=False)))
        self.assertIn("selected, not a signer of the final commitment",
                      soak.session_flags(session(selected=True, rdashd_valid=True, rdashd_signer=False)))
        self.assertEqual(soak.session_flags(session(selected=True, rdashd_valid=True, rdashd_signer=True)), [])

    def test_a_null_commitment_flags_any_type_rdashd_is_not_in(self):
        for t in (100, 102, 106):
            self.assertEqual(soak.session_flags(session(type=t, commitment={"null": True}), warmup=True),
                             ["no non-null final commitment"])
        self.assertEqual(soak.session_flags(session(type=103, commitment=None)), ["no non-null final commitment"])

    def test_a_type_not_enabled_is_not_flagged(self):
        self.assertEqual(soak.session_flags(session(type=102, enabled=False, empty=True, commitment=None)), [])

    def test_bad_complaints_and_late_phases(self):
        flags = soak.session_flags(session(selected=True, rdashd_valid=True, rdashd_signer=True, rdashd_bad_by=2,
                                           rdashd_complained_by=1, phases={2: {"timed_out": True}, 3: {"timed_out": False}}))
        self.assertEqual(flags, ["marked bad by 2 Core member(s)", "complained about by 1 Core member(s)",
                                 "phase wait timed out: 2"])

    def test_the_warm_up_exempts_only_rotation_and_islocks(self):
        empty103 = session(type=103, empty=True, commitment=None)
        empty100 = session(type=100, empty=True, commitment=None)
        self.assertEqual(soak.cycle_flags(cycle(sessions=[empty103], islock_wait={"timed_out": True}), RNODE, True), [])
        self.assertEqual(soak.cycle_flags(cycle(sessions=[empty100]), RNODE, True),
                         ["llmq_test#0: no DKG session on any Core member", "llmq_test#0: no non-null final commitment"])
        self.assertEqual(soak.cycle_flags(cycle(sessions=[empty103], islock_wait={"timed_out": True}), RNODE, False),
                         ["llmq_test_dip0024#0: no DKG session on any Core member",
                          "llmq_test_dip0024#0: no non-null final commitment", "InstantSend lock missing on a node"])

    def test_miner_wait_pose_chainlock_mempool(self):
        flags = soak.cycle_flags(cycle(sessions=[session(miner_wait={"timed_out": True})],
                                       pose={"penalty": 66, "ban_height": 300},
                                       chainlock={"agree": False, "tip_locked_s": None},
                                       mempool_sync={"timed_out": True}), RNODE)
        self.assertEqual(flags, ["llmq_test#0: miner lacked a commitment a Core member held", "PoSe penalty 66",
                                 "PoSe banned at 300", "ChainLock disagreement or tip not locked",
                                 "mempool sync timed out"])

    def test_a_locked_tip_that_stalls_is_flagged(self):
        self.assertEqual(soak.cycle_flags(cycle(chainlock={"agree": True, "tip_locked_s": None}), RNODE),
                         ["ChainLock disagreement or tip not locked"])

    def test_rdashd_log_lines_flag_their_cycle(self):
        lines = ["t (mocktime: 1) [llmq:warning] late share", "stderr: thread 'x' panicked at a.rs:1"]
        self.assertEqual(soak.cycle_flags(cycle(rdashd_log=lines), RNODE), ["2 rdashd warning/error/panic line(s)"])

    def test_allowlisted_log_groups_do_not_flag(self):
        saved = soak.BENIGN_LOG
        try:
            soak.BENIGN_LOG = [(r"late share", "test entry")]
            self.assertEqual(soak.cycle_flags(cycle(rdashd_log=["t [llmq:warning] late share"]), RNODE), [])
        finally:
            soak.BENIGN_LOG = saved


class Signing(unittest.TestCase):
    def test_rdashds_class_is_members_or_non_members(self):
        self.assertEqual(soak.same_class(cycle(), 100, RNODE), (False, ["0", "4"]))
        self.assertEqual(soak.same_class(cycle(), 103, RNODE), (True, ["1", "2", "4"]))
        self.assertEqual(soak.same_class(cycle(active_members={"100": "error"}), 100, RNODE), (None, []))
        asked = {"type": 100, "asked": {k: False for k in "12345"}, "recovered_after_s": {}}
        self.assertEqual(soak.same_class(cycle(signs=[asked]), 100, RNODE), (False, ["4"]))

    def test_a_healthy_request_pair_is_not_flagged(self):
        self.assertEqual(soak.sign_flags(cycle(signs=[sign(), sign(dependent=True, asked={"3": True, "1": True})]), RNODE), [])

    def test_quorum_sign_on_rdashd_must_be_true_for_its_quorum(self):
        for answer in (False, "JSONRPCException('x')"):
            flags = soak.sign_flags(cycle(signs=[sign(asked={"1": True, "3": answer, "5": True})]), RNODE)
            self.assertEqual(flags, [f"type 100: quorum sign on rdashd returned {answer!r} for a quorum it is in"])
        self.assertEqual(soak.sign_flags(cycle(signs=[sign(member=False, asked={"1": True, "3": False})]), RNODE), [])

    def test_an_rdashd_dependent_request_must_recover(self):
        dep = sign(t=103, dependent=True, asked={"3": True, "1": True, "2": True}, recovered={})
        self.assertEqual(soak.sign_flags(cycle(signs=[dep]), RNODE),
                         ["type 103: rdashd-dependent request recovered on no node"])
        dep["recovered_after_s"] = {"1": "JSONRPCException('x')"}
        self.assertEqual(len(soak.sign_flags(cycle(signs=[dep]), RNODE)), 1)
        skipped = {"type": 103, "dependent": True, "skipped": "rdashd in no selected quorum"}
        self.assertEqual(soak.sign_flags(cycle(signs=[skipped]), RNODE), [])

    def test_a_member_rdashd_missing_a_recsig_is_flagged(self):
        flags = soak.sign_flags(cycle(signs=[sign(recovered={"1": 0.3, "5": 0.3})]), RNODE)
        self.assertEqual(flags, ["type 100: rdashd, in the signing quorum, lacks a recovered sig every Core member holds"])

    def test_a_non_member_rdashd_without_relayed_recsigs_is_core_behaviour(self):
        s = sign(member=False, members=(1, 2, 5), asked={"1": True, "2": True, "3": False, "5": True},
                 recovered={"1": 0.3, "2": 0.3, "4": 0.5, "5": 0.4})
        held = {"0": 0, "1": 24, "2": 24, "3": 0, "4": 24, "5": 24}
        self.assertEqual(soak.sign_flags(cycle(signs=[s], cl_recsigs={"heights": 24, "held": held}), RNODE), [])

    def test_pre_r2_records_use_the_cycles_active_quorums(self):
        old = {"type": 103, "distinct_sigs": 1, "asked": {"1": True}, "recovered_after_s": {"1": 0.4, "2": 0.4, "4": 0.5}}
        self.assertEqual(soak.sign_flags(cycle(signs=[old]), RNODE),
                         ["type 103: rdashd, in the signing quorum, lacks a recovered sig every Core member holds"])

    def test_differing_sigs_and_fewer_chainlock_recsigs_than_core_peers(self):
        s = sign(distinct_sigs=2)
        held = {"0": 0, "1": 24, "2": 24, "3": 20, "4": 0, "5": 24}
        flags = soak.sign_flags(cycle(signs=[s], cl_recsigs={"heights": 24, "held": held},
                                      active_members={"100": [1, 2, 3, 5]}), RNODE)
        self.assertEqual(flags, ["type 100: recovered signatures differ between nodes",
                                 "ChainLock recsigs on rdashd 20 < every Core member's (24)"])


class Trends(unittest.TestCase):
    def test_run1s_rss_after_warm_up_flags(self):
        t = soak.trend(RUN1_RSS[3:])
        self.assertTrue(soak.rss_grows(t), t)
        self.assertIn("rdashd RSS grows", " ".join(soak.run_flags(full_run(34, RUN1_RSS), 1, True, RNODE)))

    def test_run2s_slow_6h_rss_growth_flags(self):
        t = soak.trend(RUN2_RSS)
        self.assertEqual((t["n"], t["first_third"], t["middle_third"], t["last_third"]), (196, 75352, 81870.0, 89936))
        self.assertLess(t["slope"], 250)  # the slope alone reads this run as flat
        self.assertTrue(soak.rss_grows(t), t)

    def test_noise_and_a_step_do_not_flag(self):
        flat = [70000 + (5000 if i % 2 else -5000) for i in range(42)]  # equal third medians
        self.assertFalse(soak.rss_grows(soak.trend(flat)))
        self.assertFalse(soak.rss_grows(soak.trend([70000] * 10 + [80000] * 10 + [70000] * 10)))
        self.assertFalse(soak.rss_grows(soak.trend([70000] * 10 + [90000] * 20)))
        self.assertFalse(soak.fds_grow(soak.trend([86, 86] + [92] * 30)))
        self.assertFalse(soak.fds_grow(soak.trend([92] * 10 + [97] * 20)))

    def test_small_ordered_rss_rise_does_not_flag(self):
        self.assertFalse(soak.rss_grows(soak.trend([70000] * 10 + [71000] * 10 + [72000] * 10)))

    def test_noisy_fd_growth_flags(self):
        self.assertTrue(soak.fds_grow(soak.trend([90, 92, 91, 93, 92, 94, 93, 95, 94, 96, 95, 97])))

    def test_too_few_samples_is_not_judged_silently(self):
        self.assertFalse(soak.rss_grows(soak.trend(list(range(60000, 70000, 1000)))))
        self.assertIn("RSS/fd trend judged on 9 samples (< 12)", soak.run_flags(full_run(10), 1, True, RNODE))


class Warmup(unittest.TestCase):
    def test_warm_up_ends_with_the_first_rotation_quorum(self):
        cycles = [cycle(sessions=[session(type=103, commitment=None)]) for _ in range(2)] + full_run(3)
        self.assertEqual(soak.warmup_length(cycles), (3, True))

    def test_warm_up_is_bounded(self):
        cycles = [cycle(sessions=[session(type=103, empty=True, commitment=None)]) for _ in range(6)]
        self.assertEqual(soak.warmup_length(cycles), (4, False))
        self.assertIn("rotation quorum not formed after 4 cycles", soak.run_flags(cycles, 4, False, RNODE))
        self.assertTrue(soak.cycle_flags(cycles[4], RNODE, False))

    def test_metrics_on_zero_samples_are_flagged(self):
        self.assertEqual([f for f in soak.run_flags(full_run(), 1, True, RNODE) if "0 samples" in f], [])
        cycles = [cycle(sessions=[session(type=103, commitment=None)]) for _ in range(3)]
        flags = soak.run_flags(cycles, 3, False, RNODE)
        for metric in ("InstantSend after warm-up", "rdashd RSS and fds after warm-up", "llmq_test_v17 DKG sessions",
                       "llmq_test_dip0024 sessions with rdashd selected", "llmq_test_v17 rdashd-dependent signing",
                       "llmq_test control signing with rdashd in the quorum"):
            self.assertIn(f"measured on 0 samples: {metric}", flags)


class Summary(unittest.TestCase):
    def test_summary_tables_flags_and_trends(self):
        meta = {"rdashd_node": 3, "rdashd_protx": "d6" * 32, "regular": 4, "evo": 3}
        events = full_run(16, [60000 + 1000 * i for i in range(16)])
        events[3]["pose"] = {"penalty": 10, "ban_height": -1}
        events[4]["rdashd_log"] = ["t (mocktime: 1) [llmq:warning] late share 7"]
        events.append({"event": "done", "cycles": 16, "height": 600, "rdashd_log": []})
        text = soak.summarize(meta, events)
        self.assertIn("| llmq_test_v17#0 | 16 | 0 | 16 | 16 | 16 | 16 | 0 |", text)
        self.assertIn("Rotation warm-up: 1 cycle(s)", text)
        self.assertIn("2 of 16 cycles flagged; 1 run-level flag(s).", text)
        self.assertIn("- base 240: PoSe penalty 10", text)
        self.assertIn("- base 264: 1 rdashd warning/error/panic line(s)", text)
        self.assertIn("OLS slope +1000.0 kB/cycle", text)
        self.assertIn("- rdashd RSS grows after warm-up: third medians 63000 < 68000 < 73000 kB (+10000 kB, +16%)", text)
        self.assertIn("- rdashd-dependent recoveries: 48/48", text)
        self.assertIn("- llmq_test_dip0024 rdashd-dependent recoveries: 16/16 (0 skipped", text)
        self.assertIn('"event": "done"', text)

    def test_rescan_places_lines_by_height(self):
        with tempfile.TemporaryDirectory() as d:
            node = Path(d) / "nodes/node3"
            (node / "regtest").mkdir(parents=True)
            (node / "stderr").mkdir()
            (node / "regtest/debug.log").write_text("\n".join([
                "t [warning] during setup",
                "t UpdateTip: new best=aa height=48 version=1",
                "t [llmq:error] in the first cycle",
                "t [llmq] a debug line",
                "t UpdateTip: new best=bb height=71 version=1",
                "t [error] still the first cycle",
                "t UpdateTip: new best=cc height=72 version=1",
                "t Warning: in the second cycle",
                "t UpdateTip: new best=dd height=96 version=1",
                "t [warning] after the last cycle"]) + "\n")
            (node / "stderr/tmp1").write_text("thread 'main' panicked at src/main.rs:1:1:\nnote: backtrace\n")
            cycles = [cycle(base=48, rdashd_log=["stale"]), cycle(base=72)]
            outside, panics = soak.rescan_logs(d, {"rdashd_node": 3}, cycles)
            self.assertEqual([len(c["rdashd_log"]) for c in cycles], [2, 1])
            self.assertEqual(len(outside), 2)
            self.assertEqual(panics, ["stderr: thread 'main' panicked at src/main.rs:1:1:"])
            (Path(d) / "cycles.jsonl").write_text("".join(json.dumps(c) + "\n" for c in cycles))
            (Path(d) / "meta.json").write_text(json.dumps({"rdashd_node": 3}))
            text = soak.write_summary(d, "summary-r2.md", rescan=True).read_text()
            self.assertIn("(rescan)", text)
            self.assertIn("2 rdashd warning/error line(s) outside the cycles", text)
            self.assertIn("1 rdashd stderr panic line(s)", text)

    def test_write_summary_reads_the_run_dir(self):
        with tempfile.TemporaryDirectory() as d:
            (Path(d) / "cycles.jsonl").write_text(json.dumps(cycle()) + "\n")
            path = soak.write_summary(d)
            self.assertIn("cycles: 1", path.read_text())

    def test_no_cycle(self):
        self.assertIn("No cycle completed.", soak.summarize({}, [{"event": "rdashd-exited", "status": 1}]))


class NonRecovery(unittest.TestCase):
    MEMBERS = [hashlib.sha256(bytes([i])).hexdigest() for i in range(4)]
    RID = "0123456789abcdef" * 4

    def test_recovery_order_sorts_by_serialized_hash_bytes(self):
        rid = bytes.fromhex(self.RID)[::-1]
        keys = {m: sha256d(bytes.fromhex(m)[::-1] + rid) for m in self.MEMBERS}
        self.assertEqual(nonrec.recovery_order(self.MEMBERS, self.RID), sorted(self.MEMBERS, key=keys.get))
        self.assertEqual(nonrec.recovery_order(list(reversed(self.MEMBERS)), self.RID),
                         nonrec.recovery_order(self.MEMBERS, self.RID))

    def test_recovery_order_reads_ids_in_display_order(self):
        rids = ["%02x" % i + self.RID[2:] for i in range(16)]
        orders = {tuple(nonrec.recovery_order(self.MEMBERS, rid)) for rid in rids}
        self.assertGreater(len(orders), 1)

        def unreversed(rid):
            return sorted(self.MEMBERS, key=lambda m: sha256d(bytes.fromhex(m) + bytes.fromhex(rid)))
        self.assertTrue(any(nonrec.recovery_order(self.MEMBERS, rid) != unreversed(rid) for rid in rids))

    def test_sign_hash_serializes_type_quorum_id_msg(self):
        q, rid, msg = "aa" * 31 + "01", self.RID, "bb" * 31 + "02"
        data = bytes([103]) + bytes.fromhex(q)[::-1] + bytes.fromhex(rid)[::-1] + bytes.fromhex(msg)[::-1]
        self.assertEqual(nonrec.sign_hash(103, q, rid, msg), sha256d(data)[::-1].hex())

    def test_the_first_recovery_members_are_controls(self):
        self.assertEqual([nonrec.classify(p, 3) for p in range(4)], ["control"] * 3 + ["nonrec"])

    def test_core_log_lines_parse(self):
        sh, to, other = "ab" * 32, "cd" * 32, "ef" * 32
        prefix = "2026-11-22T00:00:00Z [llmq-sigs] CSigSharesManager::CollectSigSharesToSendConcentrated -- "
        lines = [prefix + f"signHash={other}, sending to {other}, attempt=1",
                 prefix + f"signHash={sh}, sending to {to}, attempt=1",
                 prefix + f"signHash={sh}, sending to {other}, attempt=2"]
        self.assertEqual(nonrec.first_send(lines, sh), to)
        self.assertIsNone(nonrec.first_send(lines[2:], sh))
        skipped = [prefix + f"signHash={sh}, skipped 1 unreachable recovery members"] + lines
        self.assertIsNone(nonrec.first_send(skipped, sh))
        share = (f"CSigSharesManager::ProcessMessageSigShare -- signHash={sh}, id={other}, msgHash={to}, "
                 "member=3, node=7")
        self.assertEqual(nonrec.shares_from([share, share.replace("node=7", "node=9")], sh, 3), [7, 9])
        self.assertEqual(nonrec.shares_from([share], sh, 2), [])
        rec = f"CSigSharesManager::TryRecoverSig -- recovered signature. id={other}, msgHash={to}, time=3"
        self.assertTrue(nonrec.recovered_here([rec], other, to))
        self.assertFalse(nonrec.recovered_here([rec], to, other))

    def rows(self):
        check = [{"node": 1, "logged": "x", "expected": "x", "ok": True}]
        held = {"3": 0.4, "1": 0.5, "2": 0.5, "4": 0.6}
        base = {"core_recovery_nodes": [1, 2, 4], "distinct_sigs": 1}
        rows = [dict(base, kind="nonrec", id=f"n{i}", quorum="q%d" % (i % 2), recovered_after_s=held, order_check=check)
                for i in range(10)]
        rows.append(dict(base, kind="control", id="c", quorum="q0", core_recovery_nodes=[1, 2],
                         recovered_after_s=held, order_check=[]))
        rows.append(dict(base, kind="fail-closed", id="f", quorum="q1", recovered_after_s={}, distinct_sigs=0,
                         order_check=check))
        return rows

    def test_verdict(self):
        self.assertEqual(nonrec.verdict(self.rows(), 10, 2), ("PASS", []))
        rows = self.rows()
        rows[3]["recovered_after_s"] = {}
        self.assertEqual(nonrec.verdict(rows, 10, 2),
                         ("FAIL", ["non-recovery n3 did not recover on a Core recovery member"]))
        rows = self.rows()
        rows[-1]["recovered_after_s"] = {"3": 1.0}
        self.assertEqual(nonrec.verdict(rows, 10, 2), ("FAIL", ["fail-closed f recovered"]))
        rows = self.rows()
        rows[0]["distinct_sigs"] = 2
        self.assertEqual(nonrec.verdict(rows, 10, 2), ("FAIL", ["nonrec n0: recovered sigs differ between nodes"]))

    def test_rdashds_own_recsig_does_not_count_as_recovered(self):
        rows = self.rows()
        rows[0]["recovered_after_s"] = {"3": 0.4}
        rows[-2]["recovered_after_s"] = {"3": 0.4, "4": 0.5}
        self.assertEqual(nonrec.verdict(rows, 10, 2),
                         ("FAIL", ["non-recovery n0 did not recover on a Core recovery member",
                                   "control c did not recover on a Core recovery member"]))
        rows = self.rows()
        rows[0]["order_check"] = [{"node": 4, "logged": "y", "expected": "x", "ok": False}]
        self.assertEqual(nonrec.verdict(rows, 10, 2)[1], ["order check: node 4 sent y first, computed x"])
        self.assertEqual(nonrec.verdict(self.rows(), 11, 2)[0], "FAIL")
        self.assertEqual(nonrec.verdict(self.rows(), 10, 3)[0], "FAIL")
        rows = [r for r in self.rows() if r["kind"] != "control"]
        self.assertEqual(nonrec.verdict(rows, 10, 2)[1], ["no control request"])
        rows = [dict(r, order_check=[]) for r in self.rows()]
        self.assertEqual(nonrec.verdict(rows, 10, 2)[0], "FAIL")

    def test_verdict_can_require_rdashds_own_send_line(self):
        rows = self.rows()
        for r in rows:
            r["rdashd_send"] = {"logged": "x", "expected": "x", "ok": True}
        self.assertEqual(nonrec.verdict(rows, 10, 2, rdashd_send=True), ("PASS", []))
        rows[2]["rdashd_send"] = {"logged": None, "expected": "x", "ok": False}
        self.assertEqual(nonrec.verdict(rows, 10, 2, rdashd_send=True)[1],
                         ["non-recovery n2: rdashd's first send None, expected x"])
        self.assertEqual(nonrec.verdict(rows, 10, 2)[0], "PASS")

    def test_rdashds_prefixed_send_line_parses(self):
        sh, to = "ab" * 32, "cd" * 32
        line = (f"2026-11-23T00:00:00Z [llmq-sigs] CSigSharesManager::CollectSigSharesToSendConcentrated -- "
                f"signHash={sh}, sending to {to}, attempt=1")
        self.assertEqual(nonrec.first_send([line], sh), to)


class Run(unittest.TestCase):
    def test_run_refuses_a_used_out_dir_and_missing_binaries(self):
        with tempfile.TemporaryDirectory() as d:
            (Path(d) / "used").write_text("")
            args = argparse.Namespace(out=d, rdashd="/nonexistent", rdash_cli="/nonexistent")
            self.assertEqual(soak.run(args), 2)
            (Path(d) / "used").unlink()
            self.assertEqual(soak.run(args), 2)


if __name__ == "__main__":
    unittest.main()
