"""Unit tests for the parsers, the workload spec and the threshold policy.

    python3 -m unittest discover -s contrib/node/perfgate/tests
"""

import os
import sys
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.realpath(__file__))))

from perfgate import build, cli, gate, measure, node, spec  # noqa: E402

INSTR = gate.BY_KEY["instructions"]
ALLOCS = gate.BY_KEY["allocs"]


def wl_result(iters, **samples):
    return {"status": "ok", "iters": iters, "samples": samples}


def m(**workloads):
    return {"meta": {"rev": "x", "repeat": 5}, "workloads": workloads}


class SpecTest(unittest.TestCase):
    def test_one_line_declarations(self):
        wls = spec.parse("""
[workload]
a = { bench = "dashcore_hashes/hashes", id = "sha256/64k" }
b = { bench = "dash-spv/storage", id = "storage/disk/store", iters = 3, syscalls = false }
c = { node = "rpc", bin = "rdashd", args = ["--call", "getblock_v2"] }
""")
        self.assertEqual([w.name for w in wls], ["a", "b", "c"])
        self.assertEqual((wls[0].package, wls[0].target, wls[0].id, wls[0].iters), ("dashcore_hashes", "hashes",
                                                                                    "sha256/64k", None))
        self.assertEqual((wls[1].iters, wls[1].syscalls), (3, False))
        self.assertEqual((wls[2].kind, wls[2].args), ("node", ("--call", "getblock_v2")))
        self.assertNotEqual(wls[0].key(), wls[1].key())

    def test_rejects_bad_declarations(self):
        for text in ('[workload]\na = { bench = "nopackage", id = "x" }',
                     '[workload]\n"../x" = { bench = "p/t", id = "x" }',
                     '[workload]\na = { bench = "p/t" }',
                     '[workload]\na = { bench = "p/t", id = "x", iters = 0 }',
                     '[workload]\na = { bench = "p/t", id = "x", typo = 1 }',
                     '[workload]\na = { node = "startup" }',
                     '[workload]\na = { what = 1 }',
                     '[workloads]\na = { bench = "p/t", id = "x" }'):
            with self.assertRaises(spec.SpecError, msg=text):
                spec.parse(text)

    def test_select(self):
        wls = spec.parse('[workload]\nhashes-a = { bench = "p/t", id = "x" }\n'
                         'tx-a = { bench = "p/t", id = "y" }\nnode-a = { node = "startup", bin = "rdashd" }')
        self.assertEqual([w.name for w in spec.select(wls, ["hashes-*", "tx-*"])], ["hashes-a", "tx-a"])
        self.assertEqual([w.name for w in spec.select(wls, None, {"node"})], ["node-a"])

    def test_shipped_file_parses(self):
        wls = spec.load()
        self.assertTrue(any(w.kind == "bench" for w in wls))
        self.assertEqual(len({w.name for w in wls}), len(wls))


class PolicyTest(unittest.TestCase):
    def test_planted_one_percent_regression_blocks(self):
        base = [1_000_000_000 + d for d in (0, 3, 5, 2, 1)]
        head = [int(x * 1.01) for x in base]
        r = gate.judge(INSTR, base, head, 100)
        self.assertEqual(r.status, "REGRESSION")
        self.assertAlmostEqual(r.delta, 0.01, places=4)

    def test_below_half_percent_passes(self):
        base = [1_000_000] * 5
        self.assertEqual(gate.judge(INSTR, base, [1_004_000] * 5, 1).status, "OK")
        self.assertEqual(gate.judge(INSTR, base, [1_006_000] * 5, 1).status, "REGRESSION")

    def test_noise_floor_raises_the_threshold(self):
        base = [100, 110, 120, 105, 115]  # trimmed (100 dropped): range 15, allowed 2 x 15 over median 110
        self.assertEqual(gate.judge(INSTR, base, [122, 123, 124, 125, 126], 1).status, "OK")
        self.assertEqual(gate.judge(INSTR, base, [145, 146, 147, 148, 149], 1).status, "REGRESSION")

    def test_random_input_workload_shift_is_not_blocking(self):
        # The planted-regression run's spv-storage-revindex alloc_bytes (unseeded inputs, unchanged code).
        base = [5_078_073, 5_079_953, 5_035_274, 5_130_683, 5_123_192]
        head = [5_252_033, 5_180_545, 5_189_568, 5_070_606, 5_198_931]
        self.assertNotEqual(gate.judge(gate.BY_KEY["alloc_bytes"], base, head, 500).status, "REGRESSION")
        # read_bytes from the same run: +2.3% against trimmed ranges of 1.1% (base) and 1.7% (head).
        base = [4143444, 4146068, 4084788, 4189252, 4172884]
        head = [4312610, 4242132, 4241540, 4136644, 4243796]
        self.assertNotEqual(gate.judge(gate.BY_KEY["read_bytes"], base, head, 500).status, "REGRESSION")

    def test_noisy_base_needs_full_separation(self):
        base = [1000, 1010, 1020, 1005, 1015]
        head = [1000, 1010, 1100, 1100, 1100]  # median +8%, but two head runs overlap the base
        self.assertEqual(gate.judge(INSTR, base, head, 1).status, "NOISY")

    def test_noisy_head_cannot_hide_a_regression(self):
        base = [1_000_000] * 5  # deterministic
        # +4% with a 1.5% spread: blocks although the head runs do not separate cleanly.
        r = gate.judge(INSTR, base, [1_000_000, 1_032_000, 1_040_000, 1_047_000, 1_040_000], 1)
        self.assertEqual(r.status, "REGRESSION")
        # +0.95% with a 0.49% spread: the head's spread does not widen a deterministic base's 0.5%.
        r = gate.judge(INSTR, base, [1_007_000, 1_009_500, 1_011_900, 1_009_000, 1_010_000], 1)
        self.assertEqual(r.status, "REGRESSION")
        # Spread alone, median unchanged: NOISY in round 1, so the gate escalates it (never a verdict).
        self.assertEqual(gate.judge(INSTR, base, [1_000_000, 1_000_000, 1_000_000, 1_030_000, 970_000], 1).status,
                         "NOISY")

    def test_wall_time_coupled_head_spread_is_not_blocking(self):
        # spv-storage-get peak_rss_kib from the planted-regression run (storage code unchanged): the base is
        # noisy, the head spread 12%.
        base = [51176, 51284, 51188, 52328, 51152]
        head = [56360, 51164, 52288, 55404, 49120]
        self.assertNotEqual(gate.judge(gate.BY_KEY["peak_rss_kib"], base, head, 10).status, "REGRESSION")

    def test_peak_rss_steps_are_not_regressions(self):
        # spv-storage-revindex in the no-op gate: identical code, the head's runs all in the 52.1 MiB mode.
        rss = gate.BY_KEY["peak_rss_kib"]
        base, head = [51160, 51148, 52148, 51272, 51172], [52148, 52144, 52124, 52112, 52148]
        self.assertEqual(gate.judge(rss, base, head, 500).status, "OK")
        self.assertEqual(gate.judge(rss, base, [x + 6000 for x in head], 500).status, "REGRESSION")

    def test_base_error_is_never_a_silent_pass(self):
        rows = gate.compare(m(a={"status": "error", "error": "multiplexed"}), m(a=wl_result(1, instructions=[1] * 5)))
        self.assertEqual([(r.workload, r.status) for r in rows], [("a", "ERROR")])

    def test_one_stray_run_neither_hides_nor_blocks(self):
        # A single high base run must not lift the threshold to 3% and hide a 1% regression...
        base = [1_000_000, 1_000_001, 1_030_000, 999_999, 1_000_002]
        self.assertEqual(gate.judge(INSTR, base, [1_010_000 + d for d in range(5)], 1).status, "REGRESSION")
        # ...and a single stray head run must not block an unchanged PR.
        self.assertEqual(gate.judge(INSTR, [1000] * 5, [1000, 1000, 1000, 1000, 1100], 1).status, "OK")

    def test_trim(self):
        self.assertEqual(gate.trim([5, 1, 1, 1, 1]), [1, 1, 1, 1])
        self.assertEqual(gate.trim([1, 2, 3]), [1, 2, 3])

    def test_stable_results_only_are_reused(self):
        self.assertTrue(gate.stable(wl_result(1, instructions=[1_000_000, 1_000_001] * 2 + [1_000_000],
                                              allocs=[0] * 5, sched_syscalls=[1, 9, 3, 4, 5])))
        self.assertFalse(gate.stable(wl_result(1, instructions=[1000] * 5, syscalls=[10, 10, 10, 10, 12])))
        self.assertFalse(gate.stable({"status": "error"}))

    def test_incomparable_sides_fail(self):
        fp = {"harness": "h", "lock": "a", "rustc": "r", "libc": "glibc 2.42", "kernel": "k", "cpu": "c", "env": {}}
        base = m(a=wl_result(1, instructions=[1] * 5))
        head = m(a=wl_result(1, instructions=[1] * 5))
        base["meta"]["fingerprint"], head["meta"]["fingerprint"] = fp, dict(fp, lock="b")
        self.assertEqual(gate.verdict(gate.compare(base, head)), "PASS")  # a PR may change dependencies
        head["meta"]["fingerprint"] = dict(fp, harness="other")
        rows = gate.compare(base, head)
        self.assertEqual((rows[0].workload, rows[0].status), ("(sides)", "ERROR"))

    def test_counter_missing_on_head_is_an_error(self):
        rows = gate.compare(m(a=wl_result(1, instructions=[1] * 5, allocs=[3] * 5)),
                            m(a=wl_result(1, instructions=[1] * 5)))
        self.assertIn(("allocs", "ERROR"), [(r.counter, r.status) for r in rows])

    def test_improvement(self):
        self.assertEqual(gate.judge(INSTR, [1000] * 5, [900] * 5, 1).status, "IMPROVED")

    def test_zero_baseline_uses_the_absolute_floor(self):
        # 0 -> 1 allocation over 1000 iterations is under the 0.01/iter floor; 0 -> 1 per iteration is not.
        self.assertEqual(gate.judge(ALLOCS, [0] * 5, [1] * 5, 1000).status, "OK")
        r = gate.judge(ALLOCS, [0] * 5, [1000] * 5, 1000)
        self.assertEqual(r.status, "REGRESSION")
        self.assertIsNone(r.delta)

    def test_ungated_counter_is_info(self):
        self.assertEqual(gate.judge(gate.BY_KEY["sched_syscalls"], [1] * 5, [100] * 5, 1).status, "INFO")

    def test_compare_and_verdict(self):
        base = m(a=wl_result(10, instructions=[100] * 5), b=wl_result(10, instructions=[100] * 5),
                 gone=wl_result(10, instructions=[100] * 5), pend={"status": "pending"})
        head = m(a=wl_result(10, instructions=[100] * 5), b=wl_result(10, instructions=[110] * 5),
                 new=wl_result(10, instructions=[1] * 5), pend={"status": "pending"},
                 gone={"status": "missing", "error": "removed"})
        rows = {(r.workload, r.counter): r.status for r in gate.compare(base, head)}
        self.assertEqual(rows[("a", "instructions")], "OK")
        self.assertEqual(rows[("b", "instructions")], "REGRESSION")
        self.assertEqual(rows[("gone", "-")], "ERROR")
        self.assertEqual(rows[("new", "-")], "NEW")
        self.assertEqual(rows[("pend", "-")], "SKIPPED")
        self.assertEqual(gate.verdict(gate.compare(base, head)), "ERROR")  # "gone" could not be judged
        self.assertEqual(gate.verdict([r for r in gate.compare(base, head) if r.workload != "gone"]), "FAIL")
        self.assertEqual(gate.verdict(gate.compare(base, base)), "PASS")

    def test_iteration_mismatch_is_an_error(self):
        rows = gate.compare(m(a=wl_result(10, instructions=[1] * 5)), m(a=wl_result(20, instructions=[2] * 5)))
        self.assertEqual(rows[0].status, "ERROR")

    def test_render(self):
        base = m(a=wl_result(10, instructions=[100] * 5, allocs=[0] * 5))
        head = m(a=wl_result(10, instructions=[110] * 5, allocs=[0] * 5))
        md = gate.render(gate.compare(base, head), base, head)
        self.assertIn("FAIL", md)
        self.assertIn("| a | instructions | REGRESSION |", md)
        self.assertIn("| a | 10 |", gate.noise_table(base))


class RegressionApprovalTest(unittest.TestCase):
    """BL-406: an attributed regression is APPROVED within its ruled bound and stays a REGRESSION above it."""

    @staticmethod
    def genesis(head=11_399, status="REGRESSION"):
        # T34's node-startup-genesis page_faults (DEC-478): 11.281k -> 11.399k, +1.04%.
        base = 11_281
        return gate.Row("node-startup-genesis", "page_faults", status, base, head, (head - base) / base)

    def test_parse(self):
        a = gate.parse_regression_approval("node-startup-genesis:page_faults:+1.5%:DEC-464")
        self.assertEqual((a.workload, a.counter, a.relative, a.ref), ("node-startup-genesis", "page_faults", True,
                                                                     "DEC-464"))
        self.assertAlmostEqual(a.bound, 0.015)
        a = gate.parse_regression_approval("node-startup-genesis:page_faults:+150:DEC-464")
        self.assertEqual((a.bound, a.relative), (150.0, False))
        for bad in ("node-startup-genesis:page_faults:1.5%:DEC-464",  # no sign: a bound is an increase
                    "node-startup-genesis:page_faults:+1.5%",  # no DEC
                    "node-startup-genesis:page_faults:+1.5%:because",
                    "node-startup-genesis:pagefaults:+1.5%:DEC-464",  # unknown counter
                    "node-startup-genesis:page_faults:+0%:DEC-464",
                    "node-startup-genesis:page_faults:-1%:DEC-464"):
            with self.assertRaises(ValueError, msg=bad):
                gate.parse_regression_approval(bad)

    def test_within_bound_is_approved_and_an_obligation(self):
        rows = [self.genesis()]
        notes = gate.approve_regressions(rows, [gate.parse_regression_approval(
            "node-startup-genesis:page_faults:+1.5%:DEC-464")])
        self.assertEqual((rows[0].status, notes), ("APPROVED", []))
        self.assertIn("DEC-464", rows[0].note)
        self.assertEqual(gate.verdict(rows), "PASS")
        self.assertEqual(gate.regression_obligations(rows),
                         [{"row": "node-startup-genesis", "reason": "approved regression (page_faults)"}])
        rows = [self.genesis()]  # the absolute form: +118 faults against a bound of 122
        gate.approve_regressions(rows, [gate.parse_regression_approval("node-startup-genesis:page_faults:+122:DEC-464")])
        self.assertEqual(rows[0].status, "APPROVED")

    def test_above_bound_still_fails(self):
        for text in ("node-startup-genesis:page_faults:+1%:DEC-464", "node-startup-genesis:page_faults:+100:DEC-464"):
            rows = [self.genesis()]
            gate.approve_regressions(rows, [gate.parse_regression_approval(text)])
            self.assertEqual(rows[0].status, "REGRESSION", text)
            self.assertIn("above the approved bound", rows[0].note)
            self.assertEqual(gate.verdict(rows), "FAIL")
            self.assertEqual(gate.regression_obligations(rows), [])

    def test_only_the_named_row_and_counter_and_only_regressions(self):
        a = [gate.parse_regression_approval("node-startup-genesis:page_faults:+5%:DEC-464")]
        other_counter = gate.Row("node-startup-genesis", "peak_rss_kib", "REGRESSION", 100, 101, 0.01)
        other_row = gate.Row("spv-storage-get", "page_faults", "REGRESSION", 100, 101, 0.01)
        noisy = self.genesis(status="NOISY-UNRESOLVED")
        rows = [other_counter, other_row, noisy]
        notes = gate.approve_regressions(rows, a)
        self.assertEqual([r.status for r in rows], ["REGRESSION", "REGRESSION", "NOISY-UNRESOLVED"])
        self.assertEqual(notes, ["approval node-startup-genesis:page_faults:+5%:DEC-464 matched no REGRESSION row "
                                 "(the row is NOISY-UNRESOLVED)"])

    def test_a_bound_equal_to_the_shift_is_within_it(self):
        # the shift is rebuilt from delta * base; at base 1133, +3 came out a hair above 3 (Opus review)
        for base in range(1000, 1400):
            for d in (1, 3, 17, 118):
                rows = [gate.Row("w", "page_faults", "REGRESSION", base, base + d, d / base)]
                gate.approve_regressions(rows, [gate.parse_regression_approval(f"w:page_faults:+{d}:DEC-1")])
                self.assertEqual(rows[0].status, "APPROVED", (base, d))

    def test_the_unused_note_names_the_row_status(self):
        notes = gate.approve_regressions([self.genesis(status="DRIFT")], [gate.parse_regression_approval(
            "node-startup-genesis:page_faults:+5%:DEC-464")])
        self.assertEqual(notes, ["approval node-startup-genesis:page_faults:+5%:DEC-464 matched no REGRESSION row "
                                 "(the row is DRIFT)"])

    def test_cli_usage_errors(self):
        wls = [spec.Workload("node-startup-genesis", "node"), spec.Workload("b", "bench", package="p")]
        ok = cli.regression_approvals(mock.Mock(approve_regression=["node-startup-genesis:page_faults:+1%:DEC-1",
                                                                    "b:instructions:+2%:DEC-1"]), wls)
        self.assertEqual([(a.workload, a.counter) for a in ok], [("node-startup-genesis", "page_faults"),
                                                                 ("b", "instructions")])
        self.assertEqual(cli.regression_approvals(mock.Mock(approve_regression=None), wls), [])
        for bad in (["x:page_faults:+1%:DEC-1"],  # not selected
                    ["node-startup-genesis:instructions:+1%:DEC-1"],  # node workloads are not CPU-gated
                    ["b:sched_syscalls:+1%:DEC-1"],  # never gated
                    ["node-startup-genesis:page_faults:+1%:DEC-1", "node-startup-genesis:page_faults:+2%:DEC-2"],
                    ["node-startup-genesis:page_faults:1%:DEC-1"]):
            with self.assertRaises(SystemExit, msg=bad):
                cli.regression_approvals(mock.Mock(approve_regression=bad), wls)

    def test_a_zero_base_never_passes_a_relative_bound(self):
        rows = [gate.Row("w", "allocs", "REGRESSION", 0, 3, None)]
        gate.approve_regressions(rows, [gate.parse_regression_approval("w:allocs:+50%:DEC-1")])
        self.assertEqual(rows[0].status, "REGRESSION")
        rows = [gate.Row("w", "allocs", "REGRESSION", 0, 3, None)]
        gate.approve_regressions(rows, [gate.parse_regression_approval("w:allocs:+3:DEC-1")])
        self.assertEqual(rows[0].status, "APPROVED")


class ParserTest(unittest.TestCase):
    def test_strace_region(self):
        log = """\
100 openat(AT_FDCWD, "/etc/ld.so.cache", O_RDONLY|O_CLOEXEC) = 3
100 access("/perfgate/begin", F_OK)  = -1 ENOENT (No such file or directory)
100 read(3, "x", 1)               = 1
101 futex(0x7f, FUTEX_WAIT_PRIVATE, 0, NULL <unfinished ...>
100 write(1, "y", 1 <unfinished ...>
101 <... futex resumed>)           = 0
100 <... write resumed>)           = 1
100 --- SIGCHLD {si_signo=SIGCHLD} ---
100 access("/perfgate/end", F_OK <unfinished ...>
101 read(3, "z", 1)               = 1
100 <... access resumed>)          = -1 ENOENT (No such file or directory)
100 access("/perfgate/begin", F_OK)  = -1 ENOENT (No such file or directory)
100 epoll_wait(4, [], 1024, 0)    = 0
100 access("/perfgate/end", F_OK)    = -1 ENOENT (No such file or directory)
100 close(3)                       = 0
100 +++ exited with 0 +++
"""
        counts, names, spans = measure.strace_region(log.splitlines())
        self.assertEqual((counts, spans), ({"syscalls": 2, "sched_syscalls": 2, "fs_syscalls": 0}, 2))
        self.assertEqual(names, {"read": 1, "futex": 1, "write": 1, "epoll_wait": 1})

    def test_strace_region_aarch64_marker(self):
        log = ['7 faccessat(AT_FDCWD, "/perfgate/begin", F_OK) = -1 ENOENT', "7 read(3, \"x\", 1) = 1",
               '7 faccessat(AT_FDCWD, "/perfgate/end", F_OK) = -1 ENOENT']
        counts, _names, spans = measure.strace_region(log)
        self.assertEqual((counts["syscalls"], spans), (1, 1))

    def test_nice_iters(self):
        self.assertEqual([measure.nice_iters(n) for n in (0.3, 1, 3, 7, 19, 350, 9999)], [1, 1, 2, 5, 10, 200, 5000])
        self.assertEqual(measure.nice_iters(1e12), measure.MAX_ITERS)

    def test_perf_csv(self):
        text = "# started on Thu Oct  8\n\n373011046,,instructions:u,84258720,100.00,,\n1078,,page-faults:u,84258720,100.00,,\n"
        self.assertEqual(node.parse_perf_csv(text), {"instructions": 373011046, "page-faults": 1078})
        with self.assertRaises(ValueError):
            node.parse_perf_csv("<not counted>,,instructions:u,0,0.00,,\n")
        with self.assertRaises(ValueError):
            node.parse_perf_csv("100,,instructions:u,10,50.00,,\n")

    def test_alloc_records(self):
        from perfgate import nodegate

        recs = [{"pid": 1, "allocs": 10, "alloc_bytes": 80, "peak_rss_kib": 500},
                {"pid": 2, "allocs": 5, "alloc_bytes": 40, "peak_rss_kib": 900}]
        self.assertEqual(nodegate.alloc_samples(recs), {"allocs": 15, "alloc_bytes": 120, "read_bytes": 0,
                                                        "write_bytes": 0, "read_syscalls": 0, "write_syscalls": 0,
                                                        "peak_rss_kib": 900})

class BuildCommandTest(unittest.TestCase):
    """Every build runs against the Cargo.lock that lock_dependencies resolved with the shim patched in, so
    every build must carry the same patches: `--locked` refuses a lockfile that disagrees with them (the
    train 10 PF0 failed on the rdashd build)."""

    SHIM = ["--config", 'patch.crates-io.criterion.path="/shim/v0.8"']

    def build_cmds(self, run):
        cmds = []
        tmap = {("dashcore", "bench", "hashes"): "/x/dashcore", ("rdashd", "bin", "rdashd"): "/x/rdashd"}
        with mock.patch.object(build, "metadata", return_value={}), \
                mock.patch.object(build, "targets", return_value=tmap), \
                mock.patch.object(build, "shim_config", return_value=self.SHIM), \
                mock.patch.object(build, "cargo_json", side_effect=lambda cmd, wt, log: cmds.append(cmd) or []):
            run()
        return cmds

    def assert_shim(self, cmds):
        self.assertTrue(cmds)
        for cmd in cmds:
            i = cmd.index(self.SHIM[0])
            self.assertEqual(cmd[i:i + len(self.SHIM)], self.SHIM, cmd)

    def test_bins_build_with_the_shim_config(self):
        self.assert_shim(self.build_cmds(lambda: build.build_bins("/wt", {"rdashd"}, os.devnull)))

    def test_benches_build_with_the_shim_config(self):
        self.assert_shim(self.build_cmds(lambda: build.build_benches("/wt", {("dashcore", "hashes")}, os.devnull)))


if __name__ == "__main__":
    unittest.main()
