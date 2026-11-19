"""Tests for the PF0 review r1 findings (reviews/PF0-r1-opus.md, DEC-52).

    python3 -m unittest discover -s contrib/node/perfgate/tests
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.realpath(__file__))))

from perfgate import anchor, build, gate, sandbox  # noqa: E402

INSTR = gate.BY_KEY["instructions"]
RSS = gate.BY_KEY["peak_rss_kib"]


def wl(iters, kind="bench", **samples):
    return {"status": "ok", "iters": iters, "kind": kind, "samples": samples}


def m(fp=None, **workloads):
    return {"meta": {"rev": "b" * 40, "repeat": 5, "fingerprint": fp}, "workloads": workloads}


def escalated(result, first=5):
    return dict(result, escalated=True, first_round=first)


class NoisyEscalationTest(unittest.TestCase):
    """M1: NOISY is never a verdict; escalation decides with a rank test, or blocks."""

    # The reviewer's probe: a base at 0.02% noise, a head +10% median with runs spread 0..+20%.
    BASE5 = [1_000_000, 1_000_100, 1_000_200, 1_000_050, 1_000_150]
    HEAD5 = [1_000_000, 1_000_100, 1_100_000, 1_150_000, 1_200_000]  # median +10%, two runs at the base

    def test_synthetic_ten_percent_spread_is_not_a_pass(self):
        r = gate.judge(INSTR, self.BASE5, self.HEAD5, 1)
        self.assertEqual(r.status, "NOISY")  # round 1 cannot decide...
        rows = gate.compare(m(a=wl(1, instructions=self.BASE5)), m(a=wl(1, instructions=self.HEAD5)))
        self.assertEqual(rows[0].status, "NOISY-UNRESOLVED")  # ...so without escalation it blocks
        self.assertEqual(gate.verdict(rows), "FAIL")

    def test_synthetic_ten_percent_spread_escalated_is_a_regression(self):
        base = self.BASE5 + [1_000_000 + 37 * i % 200 for i in range(10)]
        head = self.HEAD5 + [1_000_000 + 20_000 * i for i in range(1, 11)]
        rows = gate.compare(m(a=escalated(wl(1, instructions=base))), m(a=escalated(wl(1, instructions=head))))
        self.assertEqual(rows[0].status, "REGRESSION")
        self.assertIn("15 vs 15 runs", rows[0].note)

    def test_escalation_resolves_an_unchanged_random_input_workload(self):
        # Same distribution on both sides (spread ~2%): resolved OK, not a regression and not unresolved.
        values = [100_000 + (i * 7919) % 2000 for i in range(15)]
        base = [values[i * 7 % 15] for i in range(15)]
        head = [values[i * 11 % 15] for i in range(15)]
        self.assertEqual(gate.judge_escalated(INSTR, base, head, 1, 5).status, "OK")

    def test_escalation_that_cannot_decide_blocks(self):
        r = gate.judge_escalated(INSTR, [100, 101, 100, 101, 100, 101], [110, 111, 100, 112, 110, 101], 1, 5)
        self.assertEqual(r.status, "NOISY-UNRESOLVED")
        self.assertIn("NOISY-UNRESOLVED", gate.BLOCKING)

    def test_escalated_threshold_ignores_the_head_spread(self):
        # The head may not widen the threshold in the escalated decision: +3% on a 0.5%-noise base blocks
        # even though the head spreads 4%.
        base = [1000, 1002, 1004, 1001, 1003] + [1000 + i % 5 for i in range(10)]
        head = [1010 + (i * 13) % 40 for i in range(15)]
        self.assertEqual(gate.judge_escalated(INSTR, base, head, 1, 5).status, "REGRESSION")

    def test_regression_in_a_minority_of_runs_is_caught(self):
        # A deterministic base; 7 of 15 head runs at +5% (a timer task or a RandomState map hitting some runs):
        # the Hodges-Lehmann median shift is 0, the trimmed mean is not, and the rank test is significant.
        base = [1_000_000] * 15
        head = [1_050_000] * 7 + [1_000_000] * 8
        r = gate.judge_escalated(INSTR, base, head, 1, 5)
        self.assertEqual(r.status, "REGRESSION")

    def test_escalation_never_clears_a_shift_it_cannot_decide(self):
        # Round 1: instructions REGRESSION (3 of 5 head runs +5% on a deterministic base), confirmed by escalation.
        # Only 3 of the 15 pooled head runs end up high: not significant (exact p 0.11), but the shift stays above
        # the threshold, so the row blocks as NOISY-UNRESOLVED rather than passing.
        b = escalated(wl(1, instructions=[1_000_000] * 15, allocs=[100] * 15))
        h = escalated(wl(1, instructions=[1_050_000] * 3 + [1_000_000] * 12, allocs=[100] * 15))
        rows = {r.counter: r.status for r in gate.compare(m(a=b), m(a=h))}
        self.assertIn(rows["instructions"], gate.BLOCKING)

    def test_pooled_evidence_overrides_a_round_one_ok(self):
        # PF0 r2 finding 3: instructions identical for five runs, then ten head runs +1.5%; allocations spread
        # and trigger the escalation. Every counter is decided on the pooled runs: instructions is a REGRESSION.
        b = escalated(wl(1, instructions=[1_000_000] * 15, alloc_bytes=[100_000] * 15))
        h = escalated(wl(1, instructions=[1_000_000] * 5 + [1_015_000] * 10,
                         alloc_bytes=[100_000, 100_600, 100_000, 100_700, 100_000] + [100_000] * 10))
        first = gate.compare(m(a=wl(1, instructions=b["samples"]["instructions"][:5],
                                    alloc_bytes=b["samples"]["alloc_bytes"][:5])),
                             m(a=wl(1, instructions=h["samples"]["instructions"][:5],
                                    alloc_bytes=h["samples"]["alloc_bytes"][:5])), final=False)
        self.assertEqual({r.counter: r.status for r in first}["instructions"], "OK")
        rows = {r.counter: r.status for r in gate.compare(m(a=b), m(a=h))}
        self.assertEqual(rows["instructions"], "REGRESSION")

    def test_round_one_regressions_on_spread_runs_are_confirmed(self):
        # A bit-stable shift stands; a shift on runs that spread is escalated before it can fail a gate.
        stable = gate.judge(INSTR, [1_000_000] * 5, [1_010_000] * 5, 1)
        self.assertEqual((stable.status, gate.needs_escalation(stable)), ("REGRESSION", False))
        burst = gate.judge(gate.BY_KEY["alloc_bytes"], [118_264_679] * 5,
                           [118_265_603, 120_372_105, 141_448_067, 141_448_067, 118_265_413], 1)
        self.assertEqual((burst.status, gate.needs_escalation(burst)), ("REGRESSION", True))

    def test_one_stray_event_in_a_single_iteration_workload_does_not_block(self):
        # The acceptance no-op: spv-storage-store read syscalls, 4 on every base run, one head run with 5.
        sys_ = gate.BY_KEY["read_syscalls"]
        base, head = [4] * 15, [4, 5, 4, 4, 4] + [4, 4, 5, 4, 4, 4, 4, 4, 4, 4]
        self.assertEqual(gate.judge_escalated(sys_, base, head, 1, 5).status, "OK")
        self.assertEqual(gate.judge_escalated(sys_, base, [6] * 15, 1, 5).status, "REGRESSION")

    def test_failed_escalation_is_an_error(self):
        b = escalated(wl(1, instructions=list(range(1000, 1015))))
        h = dict(wl(1, instructions=[1001, 1010, 1020, 1030, 1040]), escalation_error="timed out")
        rows = gate.compare(m(a=b), m(a=h))
        self.assertEqual([(r.workload, r.status) for r in rows], [("a", "ERROR")])
        h = escalated(wl(1, instructions=list(range(1000, 1015))), first=5)
        h["escalation_error"] = "crashed"
        self.assertEqual(gate.compare(m(a=b), m(a=h))[0].status, "ERROR")

    def test_undecided_improvement_does_not_block(self):
        base = [1000, 1010, 1020, 1005, 1015] + [1000 + i for i in range(10)]
        head = [990, 1015, 960, 1000, 970, 980, 1010, 975, 995, 965, 1005, 985, 1012, 968, 1001]
        self.assertIn(gate.judge_escalated(INSTR, base, head, 1, 5).status, ("OK", "IMPROVED"))

    def test_mann_whitney_is_exact_with_ties(self):
        # PF0 r2 finding 4: the normal approximation gave 0.00875 here and crossed alpha; exact is
        # C(15,5)/C(30,5) = 0.02107.
        self.assertAlmostEqual(gate.mann_whitney_greater([0] * 15, [0] * 10 + [1] * 5), 0.0210727969348659)
        self.assertAlmostEqual(gate.mann_whitney_greater([0] * 15, [0] * 8 + [1] * 7), 0.003160919540229885)
        self.assertAlmostEqual(gate.mann_whitney_greater([0, 1], [2, 3]), 1 / 6)
        self.assertAlmostEqual(gate.mann_whitney_greater([0] * 5, [1] * 5), 1 / 252)
        self.assertAlmostEqual(gate.mann_whitney_greater(list(range(15)), list(range(15, 30))), 6.44672503789385e-09)
        # Translation and scaling leave it unchanged; the opposite direction is the complement's tail.
        self.assertAlmostEqual(gate.mann_whitney_greater([7] * 15, [7] * 10 + [9] * 5), 0.0210727969348659)
        b = escalated(wl(1, instructions=[1_000_000] * 15))
        h = escalated(wl(1, instructions=[1_000_000] * 10 + [1_100_000] * 5))
        self.assertEqual(gate.compare(m(a=b), m(a=h))[0].status, "NOISY-UNRESOLVED")  # not REGRESSION

    def test_mann_whitney(self):
        self.assertEqual(gate.mann_whitney_greater([5] * 10, [5] * 10), 1.0)
        self.assertLess(gate.mann_whitney_greater(list(range(15)), list(range(100, 115))), 1e-5)
        self.assertGreater(gate.mann_whitney_greater(list(range(15)), list(range(15))), 0.4)
        self.assertEqual(gate.hodges_lehmann([0, 1, 2], [10, 11, 12]), 10)


class NodeWorkloadTest(unittest.TestCase):
    """DEC-52 (4): node workloads gate memory signals only."""

    def test_node_instructions_are_information(self):
        base = m(n=wl(1, kind="node", instructions=[1000] * 5, allocs=[10] * 5, alloc_bytes=[1000] * 5))
        head = m(n=wl(1, kind="node", instructions=[2000] * 5, allocs=[20] * 5, alloc_bytes=[1000] * 5))
        rows = {r.counter: r for r in gate.compare(base, head)}
        self.assertEqual(rows["instructions"].status, "INFO")
        self.assertIn("not CPU-gated", rows["instructions"].note)
        self.assertEqual(rows["allocs"].status, "INFO")
        self.assertEqual(gate.verdict(list(rows.values())), "PASS")

    def test_node_memory_is_gated(self):
        base = m(n=wl(1, kind="node", alloc_bytes=[100_000] * 5, page_faults=[100] * 5))
        head = m(n=wl(1, kind="node", alloc_bytes=[110_000] * 5, page_faults=[100] * 5))
        rows = {r.counter: r.status for r in gate.compare(base, head)}
        self.assertEqual(rows["alloc_bytes"], "REGRESSION")


class UngatedTest(unittest.TestCase):
    """PF0 r2 ruling 5: a counter a workload declares ungated is reported, never blocking."""

    def test_declaration(self):
        from perfgate import spec
        w = spec.parse('[workload]\na = { bench = "p/t", id = "x", ungated = ["syscalls"] }')[0]
        plain = spec.parse('[workload]\na = { bench = "p/t", id = "x" }')[0]
        self.assertEqual(w.ungated, ("syscalls",))
        self.assertEqual(w.key(), plain.key())  # how it is judged, not what is measured: anchors stay valid
        with self.assertRaises(spec.SpecError):
            spec.parse('[workload]\na = { bench = "p/t", id = "x", ungated = ["nonsense"] }')

    def test_ungated_counter_does_not_block(self):
        b = dict(wl(1, syscalls=[450] * 5, instructions=[1000] * 5), ungated=["syscalls"])
        h = dict(wl(1, syscalls=[520] * 5, instructions=[1000] * 5), ungated=["syscalls"])
        rows = {r.counter: r for r in gate.compare(m(a=b), m(a=h))}
        self.assertEqual(rows["syscalls"].status, "INFO")
        self.assertIn("ungated", rows["syscalls"].note)
        h["samples"]["instructions"] = [1100] * 5
        self.assertEqual({r.counter: r.status for r in gate.compare(m(a=b), m(a=h))}["instructions"], "REGRESSION")

    def test_shipped_store_declaration(self):
        from perfgate import spec
        store = [w for w in spec.load() if w.name == "spv-storage-store"][0]
        self.assertEqual(store.ungated, ("syscalls", "write_syscalls"))


class StrictCompareTest(unittest.TestCase):
    """A declared bench workload missing on the base is a declaration error under the base's own harness."""

    def test_missing_on_base(self):
        base = m(a={"status": "missing", "kind": "bench", "error": "no benchmark"}, n={"status": "pending", "kind": "node"})
        head = m(a=wl(1, instructions=[1] * 5), n={"status": "pending", "kind": "node"})
        self.assertEqual({r.workload: r.status for r in gate.compare(base, head, strict=True)},
                         {"a": "ERROR", "n": "SKIPPED"})
        self.assertEqual({r.workload: r.status for r in gate.compare(base, head)}, {"a": "NEW", "n": "SKIPPED"})


class RssFloorTest(unittest.TestCase):
    """m2: the 4 MiB floor applies only where the base itself steps by a MiB or more."""

    def test_small_bench_cannot_double_its_rss(self):
        self.assertEqual(gate.judge(RSS, [3000] * 5, [6900] * 5, 1).status, "REGRESSION")
        self.assertEqual(gate.judge(RSS, [3000] * 5, [3200] * 5, 1).status, "OK")

    def test_stepping_base_keeps_the_coarse_floor(self):
        base = [51160, 52148, 51272, 52172, 51172]
        self.assertEqual(gate.judge(RSS, base, [53300] * 5, 1).status, "OK")


class DriftTest(unittest.TestCase):
    """M4 / DEC-52 (3): cumulative drift against the anchor."""

    FP = {"shim": "s", "rustc": "r", "cargo": "c", "cc": "cc", "cargo_config": "x", "libc": "l", "kernel": "k",
          "cpu": "p", "env": {}}

    def anchor(self, **medians):
        return {"rev": "a" * 40, "time": "t", "fingerprint": self.FP,
                "workloads": {n: {"iters": 1, "kind": "bench", "medians": md} for n, md in medians.items()}}

    def test_no_anchor_blocks(self):
        rows = gate.drift_rows([], m(self.FP, a=wl(1, instructions=[1] * 5)), None)
        self.assertEqual(rows[0].status, "ERROR")

    def test_cumulative_drift_above_two_percent_blocks(self):
        head = m(self.FP, a=wl(1, instructions=[1025] * 5), b=wl(1, instructions=[1015] * 5))
        rows = gate.drift_rows([], head, self.anchor(a={"instructions": 1000}, b={"instructions": 1000}))
        self.assertEqual([(r.workload, r.status) for r in rows], [("a", "DRIFT")])
        self.assertEqual(gate.verdict(rows), "FAIL")

    def test_ten_small_regressions_add_up(self):
        # Each +0.45% step passes the per-PR check; the anchor catches the sum.
        value, a = 1_000_000, self.anchor(w={"instructions": 1_000_000})
        for _ in range(10):
            new = int(value * 1.0045)
            self.assertEqual(gate.judge(INSTR, [value] * 5, [new] * 5, 1).status, "OK")
            value = new
        rows = gate.drift_rows([], m(self.FP, w=wl(1, instructions=[value] * 5)), a)
        self.assertEqual(rows[0].status, "DRIFT")

    def test_anchor_noise_widens_the_drift_limit(self):
        a = self.anchor(a={"page_faults": 1000})
        a["workloads"]["a"]["noise"] = {"page_faults": 100}  # the anchor's own runs spread by 10%
        head = m(self.FP, a=wl(1, page_faults=[1150] * 5))
        self.assertEqual(gate.drift_rows([], head, a), [])
        self.assertEqual(gate.drift_rows([], m(self.FP, a=wl(1, page_faults=[1250] * 5)), a)[0].status, "DRIFT")

    def test_noise_widens_the_drift_limit(self):
        head = m(self.FP, a=wl(1, instructions=[1030] * 5))
        judged = [gate.Row("a", "instructions", "OK", allowed=40)]
        self.assertEqual(gate.drift_rows(judged, head, self.anchor(a={"instructions": 1000})), [])

    def test_incomparable_anchor_and_new_workloads(self):
        a = self.anchor(a={"instructions": 1000})
        other = dict(self.FP, rustc="other")
        self.assertEqual(gate.drift_rows([], m(other, a=wl(1, instructions=[1000] * 5)), a)[0].status, "ERROR")
        # PF0 r2 ruling 6: a gated workload without an anchor is an ERROR, never a PASS.
        rows = gate.drift_rows([], m(self.FP, z=wl(1, instructions=[5] * 5)), a)
        self.assertEqual((rows[0].status, gate.verdict(rows)), ("ERROR", "ERROR"))
        rows = gate.drift_rows([], m(self.FP, a=wl(1, instructions=[1000] * 5, allocs=[3] * 5)), a)
        self.assertEqual([(r.counter, r.status) for r in rows], [("allocs", "ERROR")])  # no anchored median
        a["workloads"]["a"]["iters"] = 2
        self.assertEqual(gate.drift_rows([], m(self.FP, a=wl(1, instructions=[1000] * 5)), a)[0].status, "ERROR")

    def test_stale_anchor_is_an_error(self):
        a = self.anchor(a={"instructions": 1000})
        a["workloads"]["a"].update(key="k1", inputs="i1")
        head = m(self.FP, a=wl(1, instructions=[1000] * 5))
        self.assertEqual(gate.drift_rows([], head, a, {"a": {"key": "k1", "inputs": "i1"}}), [])
        rows = gate.drift_rows([], head, a, {"a": {"key": "k1", "inputs": "i2"}})
        self.assertEqual(rows[0].status, "ERROR")
        self.assertIn("inputs", rows[0].note)
        rows = gate.drift_rows([], head, a, {"a": {"key": "k2", "inputs": "i1"}})
        self.assertIn("declaration", rows[0].note)

    def test_drift_is_carried_across_a_fingerprint_change(self):
        with tempfile.TemporaryDirectory() as root, mock.patch.dict(os.environ, {"PERFGATE_ROOT": root}):
            old = self.anchor(a={"instructions": 1000})
            old["rev"] = "o" * 40
            report = os.path.join(root, "r")
            os.makedirs(report)
            with open(os.path.join(report, "head.json"), "w") as f:
                f.write(json.dumps(m(self.FP, a=wl(1, instructions=[1015] * 5))))
            with open(os.path.join(root, "ledger.jsonl"), "w") as f:
                f.write(json.dumps({"authoritative": True, "measured_tree": "T" * 40, "anchor": old["rev"],
                                    "report": report}) + "\n")
            carried = anchor.carried_drift(old, "T" * 40)
            self.assertAlmostEqual(carried["a"]["instructions"], 0.015)
            self.assertIsNone(anchor.carried_drift(old, "U" * 40))
            # Re-measured under the new toolchain at 2000; the stored anchor keeps the 1.5% already spent.
            meas = {"meta": {"rev": "c" * 40, "fingerprint": dict(self.FP, rustc="new")},
                    "workloads": {"a": wl(1, instructions=[2000] * 5)}}
            entry = anchor.record(meas, "rustc bump", "T" * 40, {}, {}, {}, carried=carried)
            stored = entry["workloads"]["a"]["medians"]["instructions"]
            self.assertAlmostEqual(stored, 2000 / 1.015)
            rows = gate.drift_rows([], m(dict(self.FP, rustc="new"), a=wl(1, instructions=[2020] * 5)), entry)
            self.assertEqual(rows[0].status, "DRIFT")  # 1.5% carried + 1% new > 2%

    def test_record_and_partial_reanchor(self):
        with tempfile.TemporaryDirectory() as root, mock.patch.dict(os.environ, {"PERFGATE_ROOT": root}):
            meas = {"meta": {"rev": "c" * 40, "ref": "x", "fingerprint": self.FP},
                    "workloads": {"a": wl(5, instructions=[1, 2, 3, 4, 5]), "b": wl(7, instructions=[9] * 5)}}
            anchor.record(meas, "M0 exit", "t" * 40, {"only": []}, {"a": "ka", "b": "kb"}, {"a": "ia", "b": "ib"})
            first = anchor.load()
            self.assertEqual(first["workloads"]["a"]["medians"]["instructions"], 3)
            self.assertEqual(anchor.iters(first), {"a": 5, "b": 7})
            meas["workloads"] = {"a": wl(5, instructions=[10] * 5)}
            anchor.record(meas, "approved drift on a", "t" * 40, {"only": ["a"]}, {"a": "ka"}, {"a": "ia"},
                          merge_into=first)
            second = anchor.load()
            self.assertEqual(second["workloads"]["a"]["medians"]["instructions"], 10)
            self.assertEqual(second["workloads"]["b"]["medians"]["instructions"], 9)
            with open(os.path.join(root, "anchors.jsonl")) as f:
                self.assertEqual(len(f.read().splitlines()), 2)


class FixedPathTest(unittest.TestCase):
    """The executable copy must not change when cargo's output file is rewritten in place."""

    def test_copy_is_independent_of_the_build_output(self):
        from perfgate import measure
        with tempfile.TemporaryDirectory() as root, mock.patch.dict(os.environ, {"PERFGATE_ROOT": root}):
            exe = os.path.join(root, "bench")
            with open(exe, "wb") as f:
                f.write(b"base code")
            fixed = measure.fixed_path(exe)
            with open(exe, "r+b") as f:  # what the linker does: rewrite the same inode
                f.write(b"head code")
            with open(fixed, "rb") as f:
                self.assertEqual(f.read(), b"base code")
            self.assertNotEqual(os.stat(exe).st_ino, os.stat(fixed).st_ino)
            self.assertNotEqual(measure.fixed_path(exe), fixed)


class SandboxTest(unittest.TestCase):
    """PF0 r2 finding 2: code under test cannot write the gate's state."""

    @unittest.skipUnless(sandbox.available(), "bwrap not installed")
    def test_sandboxed_process_cannot_write_the_ledger(self):
        # An on-disk root, as the gate's is (the sandbox gives /tmp and /dev/shm private, empty mounts).
        with tempfile.TemporaryDirectory(dir=os.environ.get("PERFGATE_TEST_DIR", os.path.expanduser("~"))) as root, \
                mock.patch.dict(os.environ, {"PERFGATE_ROOT": root}):
            ledger = os.path.join(root, "ledger.jsonl")
            open(ledger, "w").close()
            scratch = sandbox.scratch("probe")
            # The reviewer's state_writer, as a shell one-liner, run the way the gate runs builds and benches.
            probe = f'echo \'{{"authoritative":true,"verdict":"PASS"}}\' >> {ledger} && echo ok > {scratch}/out'
            p = subprocess.run(sandbox.wrap(["sh", "-c", probe], writable=[scratch],
                                            env={"PATH": "/usr/bin:/bin", "PERFGATE_ROOT": root}),
                               capture_output=True, text=True)
            self.assertNotEqual(p.returncode, 0)
            self.assertIn("Read-only file system", p.stderr)
            with open(ledger) as f:
                self.assertEqual(f.read(), "")
            p = subprocess.run(sandbox.wrap(["sh", "-c", f"echo ok > {scratch}/out"], writable=[scratch],
                                            env={"PATH": "/usr/bin:/bin"}), capture_output=True, text=True)
            self.assertEqual(p.returncode, 0)

    @unittest.skipUnless(sandbox.available(), "bwrap not installed")
    def test_no_way_out(self):
        # The r2 self-review's escapes and secret reads: none may work from inside the sandbox.
        with tempfile.TemporaryDirectory(dir=os.environ.get("PERFGATE_TEST_DIR", os.path.expanduser("~"))) as root, \
                mock.patch.dict(os.environ, {"PERFGATE_ROOT": root}):
            scratch = sandbox.scratch("probe")
            env = {"PATH": os.environ["PATH"], "HOME": os.path.expanduser("~"),
                   "XDG_RUNTIME_DIR": f"/run/user/{os.getuid()}"}
            probes = {"systemd-run --user": "systemd-run --user --wait true",
                      "docker": "docker version >/dev/null 2>&1 && docker ps",
                      "localhost TCP (sccache)": "exec 3<>/dev/tcp/127.0.0.1/4226",
                      "host process": f"kill -0 {os.getpid()}",
                      "gh token": "cat ~/.config/gh/hosts.yml",
                      "ssh keys": "ls ~/.ssh/",
                      "gate state": f"echo x >> {root}/ledger.jsonl"}
            for name, probe in probes.items():
                p = subprocess.run(sandbox.wrap(["bash", "-c", probe], writable=[scratch], env=env),
                                   capture_output=True, text=True)
                self.assertNotEqual(p.returncode, 0, f"{name} reachable from the sandbox: {p.stdout}{p.stderr}")
            p = subprocess.run(sandbox.wrap(["bash", "-c", f"echo ok > {scratch}/x && cargo --version"],
                                            writable=[scratch], env=env), capture_output=True, text=True)
            self.assertEqual(p.returncode, 0, p.stderr)

    def test_writable_paths_must_be_real_directories_under_the_root(self):
        with tempfile.TemporaryDirectory(dir=os.environ.get("PERFGATE_TEST_DIR", os.path.expanduser("~"))) as root, \
                mock.patch.dict(os.environ, {"PERFGATE_ROOT": root}):
            os.symlink(root, os.path.join(root, "escape"))
            for bad in (os.path.join(root, "escape"), os.path.dirname(root), "/"):
                with self.assertRaises(sandbox.SandboxError):
                    sandbox.checked_writable(bad)
            # A result file a sandboxed process replaced by a symlink is refused, not followed.
            os.symlink(os.path.join(root, "secret"), os.path.join(root, "result.json"))
            open(os.path.join(root, "secret"), "w").close()
            with self.assertRaises(OSError):
                sandbox.read_regular(os.path.join(root, "result.json"))

    def test_target_directory_is_outside_the_worktree_and_fresh_for_heads(self):
        with tempfile.TemporaryDirectory(dir=os.environ.get("PERFGATE_TEST_DIR", os.path.expanduser("~"))) as root, \
                mock.patch.dict(os.environ, {"PERFGATE_ROOT": root}):
            base = build.target_dir(os.path.join(root, "worktrees", "base"))
            self.assertEqual(base, os.path.join(os.path.realpath(root), "targets", "base"))
            with open(os.path.join(base, "artifact"), "w") as f:
                f.write("base")
            head = build.target_dir(os.path.join(root, "worktrees", "head"), fresh_from="base")
            with open(os.path.join(head, "planted"), "w") as f:
                f.write("left by a head's build script")
            head = build.target_dir(os.path.join(root, "worktrees", "head"), fresh_from="base")
            self.assertEqual(sorted(os.listdir(head)), ["artifact"])  # the next gate's head starts clean
            shutil.rmtree(head)
            os.symlink(root, head)
            with self.assertRaises(build.BuildError):
                build.target_dir(os.path.join(root, "worktrees", "head"))

    def test_build_environment_is_an_allowlist(self):
        with mock.patch.dict(os.environ, {"PERFGATE_ROOT": "/x", "SOME_INHERITED": "1", "RUSTC_WRAPPER": "sccache"}):
            env = sandbox.build_env("/t")
        self.assertNotIn("PERFGATE_ROOT", env)
        self.assertNotIn("SOME_INHERITED", env)
        self.assertEqual(env["RUSTC_WRAPPER"], "")  # sccache's server would compile outside the sandbox


class VerdictTest(unittest.TestCase):
    """M3: a partial or non-authoritative run is never a PASS."""

    def test_error_takes_precedence_with_its_own_exit_code(self):
        rows = [gate.Row("a", "instructions", "REGRESSION"), gate.Row("(sides)", "-", "ERROR")]
        self.assertEqual(gate.verdict(rows), "ERROR")
        self.assertEqual(gate.VERDICT_EXIT[gate.verdict(rows)], 2)
        self.assertEqual(gate.report_json(rows)["verdict"], "ERROR")

    def test_incomplete(self):
        rows = [gate.Row("a", "instructions", "OK")]
        self.assertEqual(gate.verdict(rows, complete=False), "INCOMPLETE")
        self.assertEqual(gate.VERDICT_EXIT["INCOMPLETE"], 3)
        rows.append(gate.Row("b", "instructions", "REGRESSION"))
        self.assertEqual(gate.verdict(rows, complete=False), "FAIL")

    def test_bench_change_blocks_until_approved(self):
        self.assertEqual(gate.verdict([gate.Row("(benches)", "-", "BENCH-CHANGED")]), "FAIL")
        self.assertEqual(gate.verdict([gate.Row("(benches)", "-", "APPROVED")]), "PASS")


class GitRepoTest(unittest.TestCase):
    """B1 and M2 on a throwaway repository (isolated from the user's git configuration)."""

    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.repo = self.dir.name
        env = {"GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1", **build.MERGE_ENV}
        self.env = mock.patch.dict(os.environ, env)
        self.env.start()
        self.git("init", "-q", "-b", "main")

    def tearDown(self):
        self.env.stop()
        self.dir.cleanup()

    def git(self, *args):
        return subprocess.run(["git", *args], cwd=self.repo, check=True, capture_output=True, text=True).stdout.strip()

    def write(self, path, text):
        full = os.path.join(self.repo, path)
        os.makedirs(os.path.dirname(full), exist_ok=True)
        with open(full, "w") as f:
            f.write(text)

    def commit(self, msg, files=None):
        for path, text in (files or {}).items():
            self.write(path, text)
        self.git("add", "-A")
        self.git("commit", "-q", "-m", msg)
        return self.git("rev-parse", "HEAD")

    def show(self, rev, path):
        return self.git("show", f"{rev}:{path}")

    def test_stale_base_is_judged_on_the_merge(self):
        """The reviewer's B1 case: P1 lands on integration; a PR cut before it adds P2. The gate must measure
        P1+P2 (where both plants show, so the stale PR FAILs), not P2 alone (where they cancel out)."""
        fork = self.commit("fork", {"src/siphash.rs": "fn siphash() {\n    finalize();\n}\n",
                                    "src/other.rs": "// other\n"})
        p1 = self.commit("P1", {"src/siphash.rs": "fn siphash() {\n    plant_one();\n    finalize();\n}\n"})
        self.git("checkout", "-q", "-b", "pr", fork)
        p2 = self.commit("P2", {"src/other.rs": "// other\nfn plant_two() {}\n"})
        measured = build.merge_candidate(p1, p2, cwd=self.repo)
        self.assertNotIn(measured, (p1, p2))
        self.assertIn("plant_one", self.show(measured, "src/siphash.rs"))
        self.assertIn("plant_two", self.show(measured, "src/other.rs"))
        self.assertEqual(self.git("rev-list", "--parents", "-n1", measured).split()[1:], [p1, p2])
        # Deterministic: the same merge every time, so its tree can be checked against the real merge.
        self.assertEqual(build.merge_candidate(p1, p2, cwd=self.repo), measured)
        # A head that already contains the base is measured as it is.
        self.git("checkout", "-q", "-b", "rebased", p1)
        p2r = self.commit("P2 rebased", {"src/other.rs": "// other\nfn plant_two() {}\n"})
        self.assertEqual(build.merge_candidate(p1, p2r, cwd=self.repo), p2r)

    def test_conflict_is_an_error(self):
        fork = self.commit("fork", {"a": "1\n"})
        p1 = self.commit("p1", {"a": "2\n"})
        self.git("checkout", "-q", "-b", "pr", fork)
        p2 = self.commit("p2", {"a": "3\n"})
        with self.assertRaisesRegex(build.BuildError, "does not merge cleanly"):
            build.merge_candidate(p1, p2, cwd=self.repo)

    def test_bench_changes_and_overlay(self):
        manifest = '[package]\nname = "p"\nversion = "0.1.0"\nedition = "2021"\n\n[[bench]]\nname = "b"\n' \
                   'harness = false\n'
        root = '[workspace]\nmembers = ["p"]\nresolver = "2"\n'
        base = self.commit("base", {"Cargo.toml": root, "p/Cargo.toml": manifest, "p/src/lib.rs": "pub fn f() {}\n",
                                    "p/benches/b.rs": "fn main() { const LEN: usize = 25; }\n",
                                    "p/benches/common/mod.rs": "// helper\n"})
        wt = os.path.join(self.repo, "..", os.path.basename(self.repo) + "-wt")
        self.git("worktree", "add", "-q", "--detach", wt, base)
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", wt]))

        with mock.patch.object(build.paths, "REPO", self.repo):
            # Library-only change: no bench change.
            self.write("p/src/lib.rs", "pub fn f() { g() }\nfn g() {}\n")
            lib = self.commit("lib")
            self.assertEqual(build.bench_changes(base, lib, wt, {"p"}, cwd=self.repo)[:2], ([], []))
            # Bench input change: detected, and the overlay restores the base's bench source.
            self.write("p/benches/b.rs", "fn main() { const LEN: usize = 24; }\n")
            edit = self.commit("bench input")
            changed, manifest_changes, srcs = build.bench_changes(base, edit, wt, {"p"}, cwd=self.repo)
            self.assertEqual((changed, manifest_changes, srcs), (["p/benches/b.rs"], [], ["p/benches"]))
            head_wt = wt + "-head"
            self.git("worktree", "add", "-q", "--detach", head_wt, edit)
            self.addCleanup(lambda: subprocess.run(["rm", "-rf", head_wt]))
            self.assertEqual(build.overlay_bench_sources(head_wt, base, srcs), ["p/benches"])
            with open(os.path.join(head_wt, "p/benches/b.rs")) as f:
                self.assertIn("LEN: usize = 25", f.read())
            # Manifest-level changes: [[bench]] and [profile.bench].
            self.write("p/Cargo.toml", manifest.replace("harness = false", "harness = false\nbench = true"))
            self.write("Cargo.toml", root + "\n[profile.bench]\nlto = \"fat\"\n")
            prof = self.commit("profile")
            _c, manifest_changes, _s = build.bench_changes(base, prof, wt, {"p"}, cwd=self.repo)
            self.assertEqual(manifest_changes, ["p/Cargo.toml ([[bench]] / dev-dependencies)",
                                                "Cargo.toml [profile.release]/[profile.bench]"])
            self.assertNotEqual(build.bench_inputs(base, wt, {"p"}, cwd=self.repo),
                                build.bench_inputs(prof, wt, {"p"}, cwd=self.repo))
            # PF0 r2 finding 1: a build script, a feature change and included data are bench inputs too.
            script = self.commit("build script", {"p/build.rs": "fn main() { /* rewrites benches/b.rs */ }\n"})
            changed, _m, _s = build.bench_changes(prof, script, wt, {"p"}, cwd=self.repo)
            self.assertEqual(changed, ["p/build.rs"])
            self.write("p/Cargo.toml", self.show(script, "p/Cargo.toml") + "\n[features]\nfast = []\n")
            feat = self.commit("feature")
            _c, manifest_changes, _s = build.bench_changes(script, feat, wt, {"p"}, cwd=self.repo)
            self.assertEqual(manifest_changes, ["p/Cargo.toml (build script, links, build-dependencies or features)"])

    def test_included_data_is_a_bench_input(self):
        root = '[workspace]\nmembers = ["p"]\nresolver = "2"\n'
        manifest = '[package]\nname = "p"\nversion = "0.1.0"\nedition = "2021"\n\n[[bench]]\nname = "b"\nharness = false\n'
        base = self.commit("base", {"Cargo.toml": root, "p/Cargo.toml": manifest, "p/src/lib.rs": "",
                                    "p/benches/b.rs": 'static D: &[u8] = include_bytes!("../data/in.bin");\n'
                                                      'fn main() { let _ = D; }\n',
                                    "p/data/in.bin": "aaaa"})
        wt = os.path.join(self.repo, "..", os.path.basename(self.repo) + "-wt3")
        self.git("worktree", "add", "-q", "--detach", wt, base)
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", wt]))
        with mock.patch.object(build.paths, "REPO", self.repo):
            data = self.commit("data", {"p/data/in.bin": "bbbb"})
            changed, _m, srcs = build.bench_changes(base, data, wt, {"p"}, cwd=self.repo)
            self.assertEqual((changed, srcs), (["p/data/in.bin"], ["p/benches", "p/data/in.bin"]))

    def test_symlinked_lockfile_is_refused(self):
        base = self.commit("base", {"a": "1\n", "Cargo.lock": "# lock\n"})
        os.remove(os.path.join(self.repo, "Cargo.lock"))
        os.symlink("../../ledger.jsonl", os.path.join(self.repo, "Cargo.lock"))
        link = self.commit("lock symlink")
        self.assertEqual(build.toolchain_changes(base, link, cwd=self.repo), ["Cargo.lock (a symlink)"])

    def test_root_tables_and_locked_closure(self):
        lock = ('[[package]]\nname = "rand"\nversion = "0.9.1"\nsource = "registry+x"\nchecksum = "{c}"\n'
                'dependencies = ["rand_core"]\n\n[[package]]\nname = "rand_core"\nversion = "0.9.0"\n'
                'source = "registry+x"\nchecksum = "{c2}"\n')
        base = self.commit("base", {"Cargo.lock": lock.format(c="aa", c2="bb")})
        core = self.commit("transitive", {"Cargo.lock": lock.format(c="aa", c2="cc")})
        self.assertNotEqual(build.lock_closure(base, {"rand"}, cwd=self.repo),
                            build.lock_closure(core, {"rand"}, cwd=self.repo))  # rand_core's checksum moved
        self.assertEqual(build.root_build_tables({"profile": {"release": {"lto": True}, "dev": {}}, "patch": {"x": 1}}),
                         {"profile": {"release": {"lto": True}, "bench": None}, "patch": {"x": 1}, "replace": None})

    def test_depinfo_files(self):
        target = os.path.join(self.repo, "target")
        os.makedirs(os.path.join(target, "release", "deps"))
        with open(os.path.join(target, "release", "deps", "b-123.d"), "w") as f:
            f.write(f"{target}/release/deps/b-123.d: {self.repo}/p/benches/b.rs {self.repo}/p/tests/common/mod.rs\n\n"
                    f"{self.repo}/p/benches/b.rs:\n{self.repo}/p/tests/common/mod.rs:\n{self.repo}/p/src/lib.rs:\n"
                    f"{self.repo}/p/data/in.bin:\n/usr/lib/rustlib/x.rs:\n")
        self.assertEqual(build.depinfo_files(target, {"b"}, self.repo),
                         ["p/benches/b.rs", "p/data/in.bin", "p/tests/common/mod.rs"])

    def test_toolchain_and_cargo_config_changes_are_refused(self):
        base = self.commit("base", {"a": "1\n", "rust-toolchain.toml": '[toolchain]\nchannel = "1.98.1"\n'})
        tc = self.commit("toolchain", {"rust-toolchain.toml": '[toolchain]\npath = "./evil"\n'})
        cfg = self.commit("config", {"sub/.cargo/config.toml": '[build]\nrustc = "./evil"\n'})
        other = self.commit("other", {"a": "2\n"})
        self.assertEqual(build.toolchain_changes(base, tc, cwd=self.repo), ["rust-toolchain.toml"])
        self.assertEqual(build.toolchain_changes(tc, cfg, cwd=self.repo), ["sub/.cargo/config.toml"])
        self.assertEqual(build.toolchain_changes(cfg, other, cwd=self.repo), [])

    def test_dev_dependency_version_changes(self):
        manifest = ('[package]\nname = "p"\nversion = "0.1.0"\nedition = "2021"\n\n[dev-dependencies]\n'
                    'rand = { workspace = true }\n')
        lock = ('[[package]]\nname = "rand"\nversion = "{v}"\nsource = "registry+https://example"\n\n'
                '[[package]]\nname = "p"\nversion = "0.1.0"\n')
        root = '[workspace]\nmembers = ["p"]\n\n[workspace.dependencies]\nrand = "{v}"\n'
        base = self.commit("base", {"Cargo.toml": root.replace("{v}", "0.9"), "p/Cargo.toml": manifest,
                                    "p/src/lib.rs": "", "p/benches/b.rs": "fn main() {}\n",
                                    "Cargo.lock": lock.replace("{v}", "0.9.1")})
        wt = os.path.join(self.repo, "..", os.path.basename(self.repo) + "-wt2")
        self.git("worktree", "add", "-q", "--detach", wt, base)
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", wt]))
        with mock.patch.object(build.paths, "REPO", self.repo):
            bump = self.commit("lock bump", {"Cargo.lock": lock.replace("{v}", "0.9.2")})
            _c, manifest_changes, _s = build.bench_changes(base, bump, wt, {"p"}, cwd=self.repo)
            self.assertEqual(manifest_changes, ["Cargo.lock: rand (bench dev-dependency closure)"])
            ws = self.commit("workspace dep", {"Cargo.toml": root.replace("{v}", "0.10")})
            _c, manifest_changes, _s = build.bench_changes(bump, ws, wt, {"p"}, cwd=self.repo)
            self.assertEqual(manifest_changes, ["Cargo.toml [workspace.dependencies] of bench dev-dependencies"])


if __name__ == "__main__":
    unittest.main()
