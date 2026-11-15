"""Node rows on PR heads (PF0-N plan rev4.1 §7, D20-D22, D39, D41-D43, D62, D69, D91): the REFUSED table, the
NEW-FROM-REFUSED predicate, the measure wiring, and one end-to-end gate pass of a row through the chain.

    python3 -m unittest discover -s contrib/node/perfgate/tests -p test_noderows.py
"""

import json
import os
import shutil
import sys
import tempfile
import types
import unittest
from unittest import mock

HERE = os.path.dirname(os.path.realpath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
sys.path.insert(0, HERE)

import test_chain  # noqa: E402

from perfgate import anchor, build, gate, measure, nodegate, spec  # noqa: E402

N = "node-rpc"
ARGV = ["/pf0/bin/node-launch", "-datadir=/pf0/run/node", "-regtest", "-txindex", "-addressindex=1", "-server",
        "-rpcworkqueue=64"]
GATED = {"page_faults": [1000] * 5, "alloc_bytes": [5000] * 5, "peak_rss_kib": [800] * 5}


def rec(*opts, key="k1", argv="s1", attempts=None, accepted="ready"):
    """A preflight record refusing `opts` ("-opt" with owner "OWN", or (option, owner))."""
    on = [{"option": o, "owner": "OWN"} if isinstance(o, str) else {"option": o[0], "owner": o[1]} for o in opts]
    return {"status": "refused" if on else "ok", "argv_sha256": argv, "refused_on": on, "key": key,
            "attempts": attempts or len(on) + 1, "accepted_by": accepted, "version": 1}


def refused(*opts, **kw):
    return {"status": "refused", "kind": "node", "preflight": rec(*opts, **kw), "argv": ARGV}


def ok(**kw):
    return {"status": "ok", "kind": "node", "iters": 1, "samples": {k: list(v) for k, v in GATED.items()},
            "preflight": rec(**kw), "argv": ARGV}


def preflight_only(**kw):
    return {"status": "preflight-only", "kind": "node", "preflight": rec(**kw), "argv": ARGV}


def pending():
    return {"status": "pending", "kind": "node", "error": "no `rdashd` binary target in this revision"}


FP = {"rustc": "r"}


def m(w):
    return {"meta": {"rev": "b" * 40, "fingerprint": FP}, "workloads": {N: w}}


def anchored(w):
    return {"rev": "a" * 40, "fingerprint": FP, "workloads": {N: w}}


def anchor_refused(*opts, **kw):
    return anchored({"status": "refused", "kind": "node", "key": kw.get("key", "k1"), "preflight": rec(*opts, **kw)})


def anchor_preflight_only(**kw):
    return anchored({"status": "preflight-only", "kind": "node", "key": "k1", "preflight": rec(**kw)})


def anchor_ok(**kw):
    return anchored({"iters": 1, "kind": "node", "key": "k1", "medians": {k: v[0] for k, v in GATED.items()},
                     "preflight": rec(**kw)})


class RefusalTableTest(unittest.TestCase):
    """§7's gate table, row by row."""

    def gate(self, base, head, anc, approval=None):
        rows, led = gate.refusal_rows(m(base), m(head), anc, approval)
        judged = gate.compare(m(base), m(head))
        drift = gate.drift_rows(judged, m(head), anc, None, m(base))
        return judged + rows + drift, led

    def one(self, base, head, anc, approval=None):
        rows, led = self.gate(base, head, anc, approval)
        self.assertEqual(len({(r.status, r.note) for r in rows}), 1, rows)
        return rows[0], led

    def test_equal_options_are_refused_and_pass(self):
        r, led = self.one(refused("-txindex"), refused("-txindex"), anchor_refused("-txindex"))
        self.assertEqual((r.status, r.note), ("REFUSED", "refused on -txindex (OWN)"))
        self.assertEqual(gate.verdict([r]), "PASS")
        self.assertEqual(led, {"new_from_refused": [], "refused_narrowed": [], "anchor_obligations": [], "unanchored_new": []})

    def test_head_adds_a_refused_option(self):
        r, _ = self.one(refused("-txindex"), refused("-txindex", "-server"), anchor_refused("-txindex"))
        self.assertEqual((r.status, r.note), ("REFUSAL-REGRESSION", "the head refuses -server, which the base accepted"))
        self.assertEqual(gate.verdict([r]), "FAIL")

    def test_ok_to_refused(self):
        r, _ = self.one(ok(), refused("-server", key="k1"), anchor_ok())
        self.assertEqual(r.status, "REFUSAL-REGRESSION")
        self.assertIn("-server (OWN)", r.note)

    def test_head_drops_one_then_the_next_gate_needs_the_anchor(self):
        r, led = self.one(refused("-txindex", "-server"), refused("-server"), anchor_refused("-txindex", "-server"))
        self.assertEqual((r.status, gate.verdict([r])), ("REFUSED", "PASS"))
        self.assertIn("implemented: -txindex", r.note)
        self.assertEqual(led["refused_narrowed"], [{"row": N, "key": "k1", "argv_sha256": "s1",
                                                    "consumed": [{"option": "-txindex", "owner": "OWN"}]}])
        self.assertEqual(led["anchor_obligations"], [{"row": N, "reason": "refusals narrowed"}])
        # merged, not re-anchored: the base now refuses less than the anchor
        r, _ = self.one(refused("-server"), refused("-server"), anchor_refused("-txindex", "-server"))
        self.assertEqual(r.status, "ERROR")
        self.assertIn("anchor obligation: `run anchor <tip> --only node-rpc`", r.note)

    def test_owner_only_change_then_the_next_gate(self):
        r, led = self.one(refused(("-txindex", "A1")), refused(("-txindex", "B2")), anchor_refused(("-txindex", "A1")))
        self.assertEqual((r.status, r.note), ("REFUSED", "refusals changed (order or owners): was -txindex (A1), now -txindex (B2)"))
        self.assertEqual(led["anchor_obligations"], [{"row": N, "reason": "owners changed"}])
        r, _ = self.one(refused(("-txindex", "B2")), refused(("-txindex", "B2")), anchor_refused(("-txindex", "A1")))
        self.assertEqual(r.status, "ERROR")
        self.assertIn("anchor obligation: refusals changed (order or owners) since the anchor: was -txindex (A1)", r.note)

    def test_no_binary_and_refused(self):
        r, led = self.one(pending(), refused("-txindex"), anchored({}))
        self.assertEqual((r.status, gate.verdict([r])), ("REFUSED", "PASS"))
        self.assertEqual(led["anchor_obligations"], [{"row": N, "reason": "new binary, refused"}])
        r, _ = self.one(refused("-txindex"), pending(), anchor_refused("-txindex"))
        self.assertEqual((r.status, r.note), ("ERROR", "binary target removed (refused on the base)"))

    def test_refused_base_and_head_error(self):
        head = {"status": "error", "kind": "node", "error": "preflight: boom"}
        r, _ = self.one(refused("-txindex"), head, anchor_refused("-txindex"))
        self.assertEqual((r.status, r.note), ("ERROR", "error on head: preflight: boom"))

    def test_base_against_the_anchor(self):
        for anc, want in ((anchored({}), "not in the anchor"),
                          (anchored({"status": "pending"}), "not in the anchor"),
                          (anchor_ok(), "anchor inconsistent: base refuses -txindex the anchor accepted"),
                          (anchor_refused("-txindex", key="k0"), "stale anchor (declaration changed"),
                          (anchor_refused("-txindex", argv="s0"), "stale anchor (argv changed")):
            with self.subTest(want=want):
                r, _ = self.one(refused("-txindex"), refused("-txindex"), anc)
                self.assertEqual(r.status, "ERROR")
                self.assertIn(want, r.note)

    def test_new_from_refused_needs_approval(self):
        base, head, anc = refused("-txindex"), ok(attempts=1), anchor_refused("-txindex")
        rows, led = self.gate(base, head, anc)
        self.assertEqual(sorted((r.counter, r.status, r.head) for r in rows),
                         [("alloc_bytes", "NEW-FROM-REFUSED", 5000), ("page_faults", "NEW-FROM-REFUSED", 1000),
                          ("peak_rss_kib", "NEW-FROM-REFUSED", 800)])  # and no drift ERROR
        self.assertEqual(gate.verdict(rows), "FAIL")
        rows, led = self.gate(base, head, anc, approval="RP0 implemented -txindex")
        self.assertEqual({r.status for r in rows}, {"APPROVED"})
        self.assertEqual(gate.verdict(rows), "PASS")
        self.assertEqual(led["new_from_refused"], [{
            "row": N, "key": "k1", "argv_sha256": "s1", "consumed": [{"option": "-txindex", "owner": "OWN"}],
            "medians": {"page_faults": 1000, "alloc_bytes": 5000, "peak_rss_kib": 800},
            "approval": "RP0 implemented -txindex"}])
        # merged, not re-anchored: the base measures it against the REFUSED record
        rows, _ = self.gate(ok(), ok(), anc)
        self.assertEqual([(r.status, "anchor obligation" in r.note) for r in rows if r.status != "OK"],
                         [("ERROR", True)])

    def test_a_missing_rpc_method_is_a_refusal(self):
        """D113: node-bench refuses a row on an RPC method the node lacks (`rpc:<method>`, owner PENDING). It is
        compared like an option, but it is no argv token, so a head implementing it is still a first live
        measurement."""
        meth = ("rpc:getblockcount", "PENDING")
        r, _ = self.one(refused(meth), refused(meth), anchor_refused(meth))
        self.assertEqual((r.status, r.note), ("REFUSED", "refused on rpc:getblockcount (PENDING)"))
        r, _ = self.one(preflight_only(), refused(meth), anchor_preflight_only())
        self.assertEqual((r.status, gate.verdict([r])), ("REFUSAL-REGRESSION", "FAIL"))
        self.assertIn("the head refuses rpc:getblockcount", r.note)
        r, led = self.one(refused("-txindex", meth), refused("-txindex"), anchor_refused("-txindex", meth))
        self.assertEqual((r.status, led["anchor_obligations"]), ("REFUSED", [{"row": N, "reason": "refusals narrowed"}]))
        self.assertIn("implemented: rpc:getblockcount", r.note)
        rows, _ = self.gate(refused(meth), ok(attempts=1), anchor_refused(meth), approval="RP2 added getblockcount")
        self.assertEqual({r.status for r in rows}, {"APPROVED"}, rows)

    def test_a_settled_start_probed_no_methods(self):
        """D116: a "settled" start never answered RPC, so its record says nothing of the row's methods: against it,
        `rpc:` entries are no regression, narrowing or anchor inconsistency, only a changed record to re-anchor
        (an obligation, or D69's ERROR when it is the base's record against the anchor's)."""
        meth = ("rpc:getblocktemplate", "PENDING")
        settled = {"accepted": "settled"}
        r, led = self.one(refused("-txindex", **settled), refused("-txindex", meth, accepted="answered"),
                          anchor_refused("-txindex", **settled))
        self.assertEqual((r.status, led["anchor_obligations"]), ("REFUSED", [{"row": N, "reason": "owners changed"}]))
        r, led = self.one(refused("-txindex", meth, accepted="answered"), refused("-txindex", **settled),
                          anchor_refused("-txindex", meth, accepted="answered"))
        self.assertEqual((r.status, led["refused_narrowed"]), ("REFUSED", []))
        r, _ = self.one(refused("-txindex", meth, accepted="answered"), refused("-txindex", meth, accepted="answered"),
                        anchor_refused("-txindex", **settled))
        self.assertEqual(r.status, "ERROR")  # the anchor's record differs: re-anchor it (D69), not a regression
        self.assertTrue(r.note.startswith("anchor obligation: refusals changed (order or owners)"), r)
        # both answered: a method the base had is still a regression
        r, _ = self.one(refused("-txindex", accepted="answered"), refused("-txindex", meth, accepted="answered"),
                        anchor_refused("-txindex", accepted="answered"))
        self.assertEqual(r.status, "REFUSAL-REGRESSION")

    def test_new_from_refused_predicate(self):
        """D20, D22: deletion, replacement, reordering, a different option, an owner change, a retry."""
        two = ("-txindex", "-server")
        first = "base refused, head measured, but not a first live measurement: "
        cases = {
            "base argv changed": (refused(*two, argv="s2"), ok(argv="s2", attempts=1), anchor_refused(*two),
                                  "stale anchor (argv changed"),
            "head argv changed": (refused(*two), ok(argv="s2", attempts=1), anchor_refused(*two),
                                  first + "argv changed"),
            "declaration changed": (refused(*two), ok(key="k2", attempts=1), anchor_refused(*two),
                                    first + "declaration changed"),
            "reordered": (refused("-server", "-txindex"), ok(attempts=1), anchor_refused(*two),
                          "anchor obligation: refusals changed (order or owners)"),
            "owner": (refused(("-txindex", "B2"), "-server"), ok(attempts=1),
                      anchor_refused(("-txindex", "A1"), "-server"), "anchor obligation: refusals changed"),
            "not in argv": (refused("-dbcache"), ok(attempts=1), anchor_refused("-dbcache"),
                            first + "refused option not in the argv: -dbcache"),
            "retried": (refused(*two), ok(attempts=2, accepted="settled"), anchor_refused(*two),
                        first + "the head's preflight did not accept the argv as is (2 attempts, settled)"),
        }
        for what, (base, head, anc, want) in cases.items():
            with self.subTest(what):
                rows, led = self.gate(base, head, anc)
                self.assertEqual({r.status for r in rows}, {"ERROR"}, rows)
                self.assertTrue(any(r.note.startswith(want) for r in rows), (want, rows))
                self.assertEqual(led["new_from_refused"], [])
        _, led = self.gate(refused(*two), ok(attempts=1), anchor_refused(*two))
        self.assertEqual(len(led["new_from_refused"]), 1)
        # the predicate's own owner check, which _anchor_problem shadows in a gate
        b, a = refused(("-txindex", "B2")), anchor_refused(("-txindex", "A1"))["workloads"][N]
        self.assertIn("refusals changed (order or owners) since the anchor: was -txindex (A1)",
                      gate._new_from_refused_problems(b, ok(attempts=1), a))

    def test_new_from_refused_without_gated_samples_still_needs_approval(self):
        head = dict(ok(attempts=1), samples={})
        r, led = self.one(refused("-txindex"), head, anchor_refused("-txindex"))
        self.assertEqual((r.status, r.counter, gate.verdict([r])), ("NEW-FROM-REFUSED", "-", "FAIL"))
        self.assertEqual(len(led["new_from_refused"]), 1)

    def test_preflight_only(self):
        """D92: an accepted argv with no measured run is a refusal of no options."""
        r, led = self.one(refused("-txindex"), preflight_only(), anchor_refused("-txindex"))
        self.assertEqual((r.status, r.note, gate.verdict([r])),
                         ("PREFLIGHT-ONLY", "implemented: -txindex; still refused: none", "PASS"))
        self.assertEqual(led["anchor_obligations"], [{"row": N, "reason": "refusals narrowed"}])
        r, _ = self.one(preflight_only(), preflight_only(), anchor_refused("-txindex"))  # merged, not re-anchored
        self.assertEqual(r.status, "ERROR")
        self.assertIn("anchor obligation: `run anchor <tip> --only node-rpc`", r.note)
        r, led = self.one(preflight_only(), preflight_only(), anchor_preflight_only())
        self.assertEqual((r.status, gate.verdict([r])), ("PREFLIGHT-ONLY", "PASS"))
        self.assertEqual(led["anchor_obligations"], [])
        r, _ = self.one(preflight_only(), refused("-server"), anchor_preflight_only())
        self.assertEqual((r.status, r.note), ("REFUSAL-REGRESSION", "the head refuses -server, which the base accepted"))
        r, led = self.one(pending(), preflight_only(), anchored({}))
        self.assertEqual((r.status, gate.verdict([r])), ("PREFLIGHT-ONLY", "PASS"))
        self.assertEqual(led["anchor_obligations"], [{"row": N, "reason": "new binary, preflight-only"}])
        r, _ = self.one(ok(), preflight_only(), anchor_ok())  # measured on the base: node-bench's flag is static
        self.assertEqual((r.status, r.note), ("ERROR", "ok on base, preflight-only on head"))
        r, _ = self.one(preflight_only(), ok(attempts=1), anchor_preflight_only())
        self.assertEqual((r.status, r.note), ("ERROR", "ok on head (preflight-only on the base)"))
        # node-bench gained a measured run, merged, not re-anchored
        rows, _ = self.gate(ok(), ok(), anchor_preflight_only())
        self.assertEqual([(r.status, "PREFLIGHT-ONLY record" in r.note) for r in rows if r.status != "OK"],
                         [("ERROR", True)])
        r, _ = self.one(preflight_only(), preflight_only(), anchor_ok())
        self.assertEqual((r.status, r.note), ("ERROR", "anchor inconsistent: the anchor measured node-rpc, the base does "
                                                       "not"))

    def test_unanchored_new(self):
        """D22: a row the head measures first, with no anchor record, is listed for the manager."""
        _, led = gate.refusal_rows(m(pending()), m(ok()), anchored({}))
        self.assertEqual(led["unanchored_new"], [N])
        _, led = gate.refusal_rows(m(pending()), m(ok()), anchor_ok())
        self.assertEqual(led["unanchored_new"], [])

    def test_ok_rows_are_compares(self):
        """ok on both sides is compare()'s and drift_rows' alone."""
        rows, led = gate.refusal_rows(m(ok()), m(ok()), anchor_ok())
        self.assertEqual((rows, led["anchor_obligations"]), ([], []))


def measured(**samples):
    """An ok node side whose samples are GATED's, with `samples` replacing or adding counters."""
    r = ok()
    r["samples"].update({k: list(v) for k, v in samples.items()})
    return r


class NodeCountsTest(unittest.TestCase):
    """The counts of a measured node row: Opus final m1 (a counter whose source changed is an ERROR, never
    IMPROVED), plan §10's INFO memory flags (M2), and Session.run's guards (N1, N3)."""

    def test_a_counter_under_1pct_of_the_base_is_an_error_not_improved(self):
        rows = gate.compare(m(ok()), m(measured(alloc_bytes=[40] * 5)))
        row = next(r for r in rows if r.counter == "alloc_bytes")
        self.assertEqual(row.status, "ERROR")
        self.assertTrue(row.note.startswith("counter source changed: the head's median is under 1% of the base's"),
                        row.note)
        self.assertIn("--approve-counter-source", row.note)
        self.assertEqual(gate.verdict(rows), "ERROR")
        rows = gate.compare(m(ok()), m(measured(alloc_bytes=[40] * 5)), source_approval="mimalloc, reviewed")
        row = next(r for r in rows if r.counter == "alloc_bytes")
        self.assertEqual(row.status, "APPROVED", row.note)
        self.assertIn("approved: mimalloc, reviewed", row.note)
        self.assertEqual(gate.verdict(rows), "PASS")
        # a large real drop stays a measurement
        rows = gate.compare(m(ok()), m(measured(alloc_bytes=[60] * 5)))
        self.assertEqual(next(r for r in rows if r.counter == "alloc_bytes").status, "IMPROVED")

    def test_allocs_0_on_one_side_only_is_an_error(self):
        for base, head, side in (([7] * 5, [0] * 5, "head"), ([0] * 5, [7] * 5, "base")):
            with self.subTest(side=side):
                rows = gate.compare(m(measured(allocs=base)), m(measured(allocs=head)))
                bad = {r.counter: r for r in rows if r.status == "ERROR"}
                self.assertEqual(sorted(bad), ["alloc_bytes", "allocs"], rows)
                self.assertIn(f"allocs is 0 on the {side} only", bad["allocs"].note)
        rows = gate.compare(m(measured(allocs=[0] * 5)), m(measured(allocs=[0] * 5)))
        self.assertEqual([r for r in rows if r.status == "ERROR"], [])

    def test_a_head_under_1pct_of_the_anchor_needs_a_reanchor(self):
        """After an approved change merged, base and head agree, but the anchor still holds the old source."""
        low = measured(alloc_bytes=[40] * 5)
        rows = gate.drift_rows(gate.compare(m(low), m(low)), m(low), anchor_ok(), None, m(low))
        self.assertEqual([(r.counter, r.status, r.note.split(":")[0]) for r in rows],
                         [("alloc_bytes", "ERROR", "counter source changed")])
        self.assertIn(f"re-anchor {N}", rows[0].note)
        rows = gate.drift_rows([], m(low), anchor_ok(), None, m(low), "approved in the PR's gate")
        self.assertEqual([r.status for r in rows], ["APPROVED"])
        self.assertEqual(gate.drift_rows([], m(ok()), anchor_ok(), None, m(ok())), [])

    def test_compare_and_drift_share_the_source_rule(self):
        """D111 (f2 review M-a, M-c): the anchor counting no allocations and the head counting some is the same
        source change as on the base, so its approval clears drift too (no alloc_bytes DRIFT from a 0 median);
        `allocs` alone falling 99.5% is a measurement in both places."""
        anc = anchor_ok()
        anc["workloads"][N]["medians"].update(allocs=0, alloc_bytes=0)
        head = measured(allocs=[7] * 5)
        rows = gate.drift_rows([], m(head), anc, None, m(head))
        self.assertEqual(sorted((r.counter, r.status) for r in rows), [("alloc_bytes", "ERROR"), ("allocs", "ERROR")])
        self.assertIn(f"allocs is 0 on the anchor {'a' * 12} only", rows[0].note)
        rows = gate.drift_rows([], m(head), anc, None, m(head), "jemalloc dropped, reviewed")
        self.assertEqual({r.status for r in rows}, {"APPROVED"})
        self.assertEqual(gate.verdict(rows), "PASS")
        anc = anchor_ok()
        anc["workloads"][N]["medians"]["allocs"] = 1000
        head = measured(allocs=[5] * 5)
        self.assertEqual(gate.drift_rows([], m(head), anc, None, m(head)), [])
        self.assertEqual([r for r in gate.compare(m(measured(allocs=[1000] * 5)), m(head)) if r.status == "ERROR"], [])

    def test_an_approved_source_change_still_drift_checks_the_other_counters(self):
        """D111 (f2 review M-b): only the changed counters leave the drift loop."""
        head = measured(alloc_bytes=[40] * 5, page_faults=[1300] * 5)
        rows = gate.drift_rows([], m(head), anchor_ok(), None, m(head), "mimalloc, reviewed")
        self.assertEqual(sorted((r.counter, r.status) for r in rows), [("alloc_bytes", "APPROVED"),
                                                                       ("page_faults", "DRIFT")])
        self.assertEqual(gate.verdict(rows), "FAIL")

    def test_an_ungated_alloc_bytes_is_not_a_source_change(self):
        """D111 (f2 review L-3): allocs 0 on one side marks alloc_bytes only where it is gated."""
        head = measured(allocs=[0] * 5)
        head["ungated"] = ["alloc_bytes"]
        base = measured(allocs=[7] * 5)
        base["ungated"] = ["alloc_bytes"]
        bad = sorted(r.counter for r in gate.compare(m(base), m(head)) if r.status == "ERROR")
        self.assertEqual(bad, ["allocs"])

    def test_the_ledger_names_the_approved_counters(self):
        """D111 (f2 review L-1): an approved source change leaves one anchor obligation per row, naming its
        counters; an unapproved one leaves none (it blocks)."""
        rows = gate.compare(m(measured(allocs=[7] * 5)), m(measured(allocs=[0] * 5)), source_approval="why")
        self.assertEqual(gate.source_obligations(rows),
                         [{"row": N, "reason": "counter source changed (alloc_bytes, allocs)"}])
        self.assertEqual(gate.source_obligations(gate.compare(m(ok()), m(measured(allocs=[0] * 5)))), [])

    def test_memory_flags(self):
        """Plan §10: VmHWM against the trusted wait4 maxrss (4 MiB) and memory.peak (64 MiB or 50%)."""
        vm = [18000] * 5
        self.assertEqual(gate.memory_flags(measured(peak_rss_kib=vm, maxrss_kib=[20000] * 5,
                                                    memory_peak_kib=[80000] * 5)), [])
        flags = gate.memory_flags(measured(peak_rss_kib=vm, maxrss_kib=[18000, 18000, 23000, 18000, 18000],
                                           memory_peak_kib=[18000 + 65537] * 5))
        self.assertEqual(flags, ["rss-mismatch in 1/5 runs (VmHWM 18000 KiB, maxrss 23000 KiB)",
                                 "tree-memory in 5/5 runs (VmHWM 18000 KiB, memory_peak 83537 KiB)"])
        big = [400000] * 5  # 50% of VmHWM, not 64 MiB, bounds a large node
        self.assertEqual(gate.memory_flags(measured(peak_rss_kib=big, maxrss_kib=big, memory_peak_kib=[590000] * 5)),
                         [])
        self.assertEqual(gate.memory_flags(measured(peak_rss_kib=vm, maxrss_kib=[30000] * 4)), [], "unpaired runs")
        rows = gate.compare(m(ok()), m(measured(maxrss_kib=[800] * 5, memory_peak_kib=[200000] * 5)))
        row = next(r for r in rows if r.counter == "peak_rss_kib")
        self.assertEqual((row.status, row.flags), ("OK", ("head: tree-memory in 5/5 runs (VmHWM 800 KiB, "
                                                          "memory_peak 200000 KiB)",)))
        self.assertIn("plan §10 (INFO): head: tree-memory", row.note)
        self.assertEqual(gate.verdict(rows), "PASS")
        md = gate.render(rows, m(ok()), m(ok()))
        self.assertIn("## Node memory flags (INFO, plan §10)\n\n- node-rpc: head: tree-memory in 5/5 runs", md)
        base = ok()
        del base["samples"]["peak_rss_kib"]  # no peak_rss_kib row to carry the flags: an INFO row of its own
        rows = gate.compare(m(base), m(measured(maxrss_kib=[800] * 5, memory_peak_kib=[200000] * 5)))
        row = next(r for r in rows if r.flags)
        self.assertEqual((row.counter, row.status), ("-", "INFO"))

    def session_run(self, spawn, perf="1000,,instructions:u,100,100.00\n50,,page-faults,100,100.00\n",
                    records=({"pid": 1, "allocs": 3, "alloc_bytes": 100, "peak_rss_kib": 18000},)):
        """Session.run over a fake invocation result whose res holds one measured run spawn."""
        res = tempfile.mkdtemp(prefix="res-", dir=os.environ.get("PERFGATE_TEST_DIR"))
        self.addCleanup(shutil.rmtree, res, True)
        files = {"manifest.json": {"status": "ok", "argv_sha256": "s1"},
                 "spawn-1.result.json": {"spawn": 1, "class": "run", "measured": True, "status": {"exit": 0},
                                         "alloc_errors": [], **spawn},
                 "spawn-1.alloc.json": list(records)}
        for name, doc in files.items():
            with open(os.path.join(res, name), "w") as f:
                json.dump(doc, f)
        with open(os.path.join(res, "spawn-1.perf"), "w") as f:
            f.write(perf)

        def load(name):
            try:
                with open(os.path.join(res, name)) as f:
                    return json.load(f)
            except OSError:
                return None

        s = nodegate.Session.__new__(nodegate.Session)
        s.invoke = lambda *_a: types.SimpleNamespace(res=res, errors=lambda: [], json=load)
        return s.run("head-rdashd", spec.Workload(name=N, kind="node", node="rpc", bin="rdashd"), "s1")

    def test_session_run_reads_the_trusted_memory(self):
        out = self.session_run({"rusage": {"maxrss_kib": 19000}, "memory_peak": 40 << 20})
        self.assertEqual({k: out[k] for k in ("instructions", "page_faults", "peak_rss_kib", "maxrss_kib",
                                              "memory_peak_kib")},
                         {"instructions": 1000, "page_faults": 50, "peak_rss_kib": 18000, "maxrss_kib": 19000,
                          "memory_peak_kib": 40960})
        self.assertNotIn("maxrss_kib", self.session_run({}), "an unrecorded value is left out, not an error")

    def test_session_run_refuses_rejected_alloc_records(self):
        """N1: the broker rejected the records (here: none from the jail's pid 1); the shell's tiny one is left."""
        with self.assertRaisesRegex(nodegate.RunError, "the allocation counter: .*no alloc record from the jail"):
            self.session_run({"alloc_errors": ["no alloc record from the jail's pid 1"]},
                             records=({"pid": 7, "allocs": 1, "alloc_bytes": 10, "peak_rss_kib": 900},))

    def test_session_run_names_a_missing_perf_event(self):
        """N3: a perf CSV without an event is the row's RunError, not a KeyError."""
        with self.assertRaisesRegex(nodegate.RunError, f"^{N}: perf did not report page-faults$"):
            self.session_run({}, perf="1000,,instructions:u,100,100.00\n")


class LateRefusalGateTest(unittest.TestCase):
    """D99, end to end over node-bench's real preflight (pf0.preflight and run_attempt against its fake stub): a head
    that answers RPC and then refuses an option the base accepted is a REFUSAL-REGRESSION, never PREFLIGHT-ONLY."""

    def test_ready_then_refusal_does_not_pass(self):
        import socket

        nb = os.path.join(HERE, "..", "..", "..", "..", "node-bench")
        sys.path.insert(0, nb)
        self.addCleanup(sys.path.remove, nb)
        from nodebench import pf0

        d = tempfile.mkdtemp(dir=os.environ.get("PERFGATE_TEST_DIR"))
        self.addCleanup(shutil.rmtree, d, True)
        with socket.socket() as sk:
            sk.bind(("127.0.0.1", 0))
            port = sk.getsockname()[1]
        datadir = os.path.join(d, "node")
        for patch in (mock.patch.object(pf0, "STUB", os.path.join(nb, "tests", "fake_stub.py")),
                      mock.patch.object(pf0, "RUN", d), mock.patch.object(pf0, "NODE_RPC", port),
                      mock.patch.object(pf0, "NODE_DATADIR", datadir), mock.patch.object(pf0, "STOP_WAIT", 3),
                      mock.patch.dict(os.environ, {"PF0_TEST_PORT": str(port)})):
            patch.start()
            self.addCleanup(patch.stop)
        argv = ["/pf0/bin/node-launch", f"-datadir={datadir}", "-mode=late-server", "-txindex", "-server"]
        rec_ = pf0.preflight(argv, lambda cur, i: pf0.run_attempt(cur, i, ready="rpc", deadline=30))
        w = spec.Workload(name=N, kind="node", node="rpc", bin="rdashd")
        s = nodegate.Session.__new__(nodegate.Session)
        s.log = lambda _m: None
        man = {"status": rec_["status"], "preflight": rec_, "preflight_argv": argv, "measured": False}
        s.invoke = mock.Mock(return_value=mock.Mock(json=lambda _n: man, errors=lambda: [], outer={}))
        s.run = mock.Mock(side_effect=AssertionError("no measured run"))
        head = dict(s.measure(["head-rdashd"], w, 5)["head-rdashd"], kind="node")
        self.assertEqual((head["status"], [x["option"] for x in head["preflight"]["refused_on"]]),
                         ("refused", ["-server", "-txindex"]))
        base = refused(("-txindex", "XX1"), key=w.key(), argv=rec_["argv_sha256"])
        anc = anchor_refused(("-txindex", "XX1"), key=w.key(), argv=rec_["argv_sha256"])
        rows, _ = gate.refusal_rows(m(base), m(head), anc, None)
        judged = gate.compare(m(base), m(head))
        rows = judged + rows + gate.drift_rows(judged, m(head), anc, None, m(base))
        self.assertIn(("REFUSAL-REGRESSION", "the head refuses -server, which the base accepted"),
                      [(r.status, r.note) for r in rows])
        self.assertNotEqual(gate.verdict(rows), "PASS")


class WiringTest(unittest.TestCase):
    def test_spec_has_no_node_strace_pass(self):
        with tempfile.NamedTemporaryFile("w", suffix=".toml", dir=os.environ.get("PERFGATE_TEST_DIR")) as f:
            f.write('[workload]\nn = { node = "startup-genesis", bin = "rdashd" }\n')
            f.flush()
            self.assertFalse(spec.load(f.name)[0].syscalls)
            f.seek(0)
            f.write('[workload]\nn = { node = "startup-genesis", bin = "rdashd", syscalls = true }\n')
            f.flush()
            with self.assertRaisesRegex(spec.SpecError, "no strace pass"):
                spec.load(f.name)

    def test_pr_heads_are_measured(self):
        """DEC-76(b) is lifted: both sides' binaries are built (in the build sandbox) and measured on the chain."""
        w = spec.Workload(name=N, kind="node", node="rpc", bin="rdashd")
        sides = [types.SimpleNamespace(label=lb, wt=f"/wt/{lb}", bins={}, results={}, build_log=lambda: os.devnull)
                 for lb in ("base", "head")]
        built = {}

        def bins(wt, names, log):
            built[wt] = names
            return {"rdashd": f"{wt}/target/release/rdashd"} if wt.endswith("head") else {}

        session = mock.Mock()
        session.measure.return_value = {"head-rdashd": {"status": "ok", "samples": {}}}
        with mock.patch.object(build, "metadata", return_value={}), \
                mock.patch.object(build, "targets", return_value=[("p", "bin", "rdashd")]), \
                mock.patch.object(build, "build_bins", side_effect=bins), \
                mock.patch.object(nodegate, "Session", return_value=session) as sess:
            s = measure.node_session(sides, [w], 15, lambda _m: None)
            measure.measure_nodes(s, sides, [w], 5, lambda _m: None)
        self.assertEqual(built, {"/wt/base": {"rdashd"}, "/wt/head": {"rdashd"}})
        self.assertEqual(sess.call_args.args[0], {"head-rdashd": "/wt/head/target/release/rdashd"})
        session.measure.assert_called_once_with(["head-rdashd"], w, 5)
        self.assertEqual(sides[0].results[N]["status"], "pending")
        self.assertEqual(sides[1].results[N], {"status": "ok", "samples": {}, "kind": "node"})

    def test_an_accepted_row_without_a_measured_run_is_preflight_only(self):
        """D92: node-bench's `measured` flag decides whether an ok preflight is followed by measured runs."""
        w = spec.Workload(name=N, kind="node", node="rpc", bin="rdashd")

        def session(man):
            s = nodegate.Session.__new__(nodegate.Session)
            s.log = lambda _m: None
            s.invoke = mock.Mock(return_value=mock.Mock(json=lambda _n: man, errors=lambda: [], outer={}))
            s.run = mock.Mock(side_effect=AssertionError("no measured run"))
            return s

        man = {"status": "ok", "preflight": rec(), "preflight_argv": ARGV, "measured": False}
        out = session(man).measure(["head-rdashd"], w, 5)["head-rdashd"]
        self.assertEqual((out["status"], out["preflight"]["key"]), ("preflight-only", w.key()))
        out = session(dict(man, status="refused", preflight=rec("-x"))).measure(["head-rdashd"], w, 5)["head-rdashd"]
        self.assertEqual(out["status"], "refused")
        del man["measured"]
        out = session(man).measure(["head-rdashd"], w, 5)["head-rdashd"]
        self.assertEqual(out["status"], "error")
        self.assertIn("does not say whether the row has a measured run", out["error"])
        # a measured row whose run starts another argv than the preflight accepted (node-bench's two builders
        # disagree) is an error, not a measurement
        s = session(dict(man, measured=True, argv_sha256="s0"))
        s.run = nodegate.Session.run.__get__(s)
        out = s.measure(["head-rdashd"], w, 1)["head-rdashd"]
        self.assertEqual(out["status"], "error")
        self.assertIn("the measured run's argv (s0) is not the one its preflight accepted (s1)", out["error"])
        root = tempfile.mkdtemp(dir=os.environ.get("PERFGATE_TEST_DIR"))
        self.addCleanup(shutil.rmtree, root, True)
        with mock.patch.dict(os.environ, {"PERFGATE_ROOT": root}):
            entry = anchor.record({"meta": {"rev": "c" * 40, "fingerprint": {}}, "workloads": {N: preflight_only()}},
                                  "PF0-N", "t" * 40, {}, {N: "k1"}, {N: "i"})
        self.assertEqual(entry["workloads"][N]["status"], "preflight-only")

    def test_anchor_refuses_a_node_row_that_errored(self):
        """`run anchor` names a node row that did not measure instead of silently leaving it out."""
        from perfgate import cli

        root = tempfile.mkdtemp(dir=os.environ.get("PERFGATE_TEST_DIR"))
        self.addCleanup(shutil.rmtree, root, True)
        args = types.SimpleNamespace(cmd="anchor", justification="PF0-N", repeat=5, workloads=None, only=["node-*"],
                                     rev="tip", dev_harness=False, no_cache=False, jobs=1, reset_drift=False)
        wls = spec.select(spec.load(None), ["node-*"])
        status = {w.name: refused("-txindex") for w in wls} | {"node-startup-genesis": {
            "status": "error", "kind": "node", "error": "preflight: boom"}}
        meas = {"meta": {"rev": "c" * 40, "fingerprint": FP}, "workloads": status}
        with mock.patch.dict(os.environ, {"PERFGATE_ROOT": root}), \
                mock.patch.object(build, "resolve", return_value="c" * 40), \
                mock.patch.object(cli, "check_harness", return_value=[]), \
                mock.patch.object(build, "node_bench_override", return_value=None), \
                mock.patch.object(measure, "measure_rev", return_value=meas), \
                self.assertRaisesRegex(SystemExit, r"node rows did not measure, so they cannot be anchored: "
                                                   r"node-startup-genesis \(preflight: boom\)"):
            cli.cmd_anchor(args)

    def test_report_cells_hold_node_text(self):
        line = gate._line(gate.Row(N, "-", "ERROR", note="error on head: x\n| PASS |\x1b[2J"))
        self.assertEqual(line.count("\n"), 0)
        self.assertEqual(line, f"| {N} | - | ERROR | | | | | error on head: x\\n\\| PASS \\|\\x1b[2J |")

    def test_a_root_too_long_for_the_broker_socket(self):
        parent = tempfile.mkdtemp(dir=os.environ.get("PERFGATE_TEST_DIR"))
        self.addCleanup(shutil.rmtree, parent, True)
        rest = len(os.path.join(parent, "", "node", f"g-{os.getpid()}", "inv", "head-rdashd-99", "mnt", "ctl", "broker"))
        # too long in characters; and short enough in characters but not in bytes (the kernel counts bytes)
        for name in ("r" * 90, "é" * (nodegate.SUN_PATH_MAX - rest)):
            root = os.path.join(parent, name)
            os.makedirs(root, 0o700)
            w = spec.Workload(name="node-startup-genesis", kind="node", node="startup-genesis", bin="rdashd")
            with self.subTest(len(name)), mock.patch.dict(os.environ, {"PERFGATE_ROOT": root}), \
                    mock.patch.object(nodegate.invocation, "self_probe", side_effect=AssertionError("probed")), \
                    self.assertRaisesRegex(nodegate.invocation.ChainError, "too long for the broker's socket path"):
                nodegate.Session({"head-rdashd": "/x"}, [w], 5, lambda _m: None, test=True)
            self.assertEqual(os.listdir(os.path.join(root, "node")), [])

    def test_node_escalation_runs_against_the_preflight(self):
        """Escalation hands Session.repeat the sides' real results: their preflight's argv hash checks every run."""
        w = spec.Workload(name=N, kind="node", node="startup-genesis", bin="rdashd")
        sides = [types.SimpleNamespace(label=lb, ready={}, results={N: ok()}) for lb in ("base", "head")]
        s = nodegate.Session.__new__(nodegate.Session)
        s.log = lambda _m: None
        s.run = mock.Mock(return_value={"page_faults": 7})
        with mock.patch.object(measure, "run_plans"):
            measure.escalate(sides, [w], 2, 1, lambda _m: None, s)
        self.assertEqual({c.args[2] for c in s.run.call_args_list}, {"s1"})
        for side in sides:
            self.assertEqual(side.results[N]["samples"]["page_faults"], [1000] * 5 + [7, 7])
            self.assertEqual((side.results[N]["escalated"], side.results[N]["first_round"]), (True, 5))

    def test_a_refused_anchor_record_is_usable(self):
        """Gates, measures and re-anchors read a REFUSED record (no iters, no medians) without failing, so the
        `run anchor --only` that discharges a NEW-FROM-REFUSED obligation works."""
        old = anchor_refused("-x")
        self.assertEqual(anchor.iters(old), {})
        self.assertEqual(anchor.forgiven_drift(old, m(ok())), {})
        root = tempfile.mkdtemp(dir=os.environ.get("PERFGATE_TEST_DIR"))
        self.addCleanup(shutil.rmtree, root, True)
        with open(os.path.join(root, "head.json"), "w") as f:
            json.dump(m(ok()), f)
        with open(os.path.join(root, "ledger.jsonl"), "w") as f:
            f.write(json.dumps({"authoritative": True, "measured_tree": "t", "anchor": old["rev"], "report": root})
                    + "\n")
        with mock.patch.dict(os.environ, {"PERFGATE_ROOT": root}):
            self.assertEqual(anchor.carried_drift(old, "t"), {})

    def test_anchor_keeps_an_ok_rows_preflight(self):
        root = tempfile.mkdtemp(dir=os.environ.get("PERFGATE_TEST_DIR"))
        self.addCleanup(shutil.rmtree, root, True)
        meas = {"meta": {"rev": "c" * 40, "fingerprint": {}}, "workloads": {N: ok()}}
        with mock.patch.dict(os.environ, {"PERFGATE_ROOT": root}):
            entry = anchor.record(meas, "PF0-N", "t" * 40, {}, {N: "k1"}, {N: "i"})
        self.assertEqual(entry["workloads"][N]["preflight"],
                         {k: v for k, v in rec().items() if k in anchor.PREFLIGHT_FIELDS})


@unittest.skipIf(test_chain.SKIP, test_chain.SKIP or "")
class ChainRowTest(unittest.TestCase):
    """One node row end to end: a base node refusing an option and a head that accepts the argv, through the real
    node-bench startup-genesis in the view, the broker and the jail."""

    def setUp(self):
        self.root = tempfile.mkdtemp(prefix="noderow-", dir=os.environ.get("PERFGATE_TEST_DIR"))
        os.chmod(self.root, 0o700)
        self.addCleanup(shutil.rmtree, self.root, True)
        env = mock.patch.dict(os.environ, {"PERFGATE_ROOT": self.root})
        env.start()
        self.addCleanup(env.stop)

    def fake(self, name, refuse, child_mib=0):
        with open(test_chain.FAKE_NODE) as f:
            text = f.read()
        self.assertEqual(text.count("\nREFUSE = {}\nCHILD_MIB = 0\n"), 1)
        path = os.path.join(self.root, name)
        with open(path, "w") as f:
            f.write(text.replace("\nREFUSE = {}\nCHILD_MIB = 0\n", f"\nREFUSE = {refuse!r}\nCHILD_MIB = {child_mib}\n"))
        os.chmod(path, 0o755)
        return path

    def test_refused_base_and_measured_head(self):
        w = spec.Workload(name="node-startup-genesis", kind="node", node="startup-genesis", bin="rdashd")
        exes = {"base-rdashd": self.fake("base", {"-fixedseeds": "P2b"}), "head-rdashd": self.fake("head", {})}
        with nodegate.Session(exes, [w], 2, lambda _m: None, test=True) as s:
            out = s.measure(["base-rdashd", "head-rdashd"], w, 2)
        base, head = out["base-rdashd"], out["head-rdashd"]
        self.assertEqual(base["status"], "refused", base)
        self.assertEqual(base["preflight"]["refused_on"], [{"option": "-fixedseeds", "owner": "P2b"}])
        self.assertEqual((base["preflight"]["attempts"], base["preflight"]["key"]), (2, w.key()))
        self.assertIn("-fixedseeds=0", base["argv"])
        self.assertEqual(head["status"], "ok", head)
        self.assertEqual((head["preflight"]["attempts"], head["preflight"]["accepted_by"]), (1, "ready"))
        self.assertEqual(head["preflight"]["argv_sha256"], base["preflight"]["argv_sha256"])
        for k in ("page_faults", "alloc_bytes", "peak_rss_kib", "instructions"):
            self.assertEqual(len(head["samples"][k]), 2, k)
            self.assertTrue(all(v > 0 for v in head["samples"][k]), (k, head["samples"][k]))
        self.assertEqual(os.listdir(os.path.join(self.root, "node")), [])  # the gate dir is gone

    def test_a_forked_child_raises_tree_memory_not_the_gated_value(self):
        """Plan §10 (Opus final M2): a node whose 100 MiB child the preload does not see keeps peak_rss_kib small;
        the trusted spawn-k memory.peak sees it, so the side carries the INFO `tree-memory` flag (and nothing ERRORs
        or gates on it). The plain fake is the control: no tree-memory. Its rss-mismatch, if any, is perf's own RSS:
        wait4's maxrss is the largest of perf, bwrap and the node, and this fake is smaller than perf (D107)."""
        w = spec.Workload(name="node-startup-genesis", kind="node", node="startup-genesis", bin="rdashd")
        exes = {"base-rdashd": self.fake("base", {}), "head-rdashd": self.fake("head", {}, child_mib=100)}
        with nodegate.Session(exes, [w], 2, lambda _m: None, test=True) as s:
            out = s.measure(["base-rdashd", "head-rdashd"], w, 2)
        base, head = out["base-rdashd"], out["head-rdashd"]
        self.assertEqual((base["status"], head["status"]), ("ok", "ok"), (base, head))
        self.assertTrue(all(v < 64 << 10 for v in head["samples"]["peak_rss_kib"]), head["samples"])
        self.assertTrue(all(v > 100 << 10 for v in head["samples"]["memory_peak_kib"]), head["samples"])
        flags = gate.memory_flags(head)
        self.assertTrue(any(f.startswith("tree-memory in 2/2 runs") for f in flags), (flags, head["samples"]))
        self.assertTrue(any(f.startswith("rss-mismatch in 2/2 runs") for f in flags), (flags, head["samples"]))
        self.assertFalse([f for f in gate.memory_flags(base) if f.startswith("tree-memory")], base["samples"])
        self.assertTrue(all(m < 64 << 10 for m in base["samples"]["maxrss_kib"]), base["samples"])


if __name__ == "__main__":
    unittest.main()
