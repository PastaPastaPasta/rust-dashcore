"""Unit tests for the parts of node-bench that need no node: parsing, stats, the gate.

    python3 -m unittest discover -s node-bench/tests
"""

import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from nodebench import compare, impls, logparse, noise, pair, paths, results, stats  # noqa: E402

DEBUG_LOG_BENCH = """\
2026-10-08T03:00:00.000000Z init message: Done loading
2026-10-08T03:00:01.000000Z [bench]     - Sanity checks: 0.01ms [0.00s (0.01ms/blk)]
2026-10-08T03:00:01.000100Z [bench]       - Connect 3 transactions: 0.50ms (0.167ms/tx, 0.250ms/txin) [0.00s (0.50ms/blk)]
2026-10-08T03:00:01.000200Z [bench]     - Verify 2 txins: 0.40ms (0.200ms/txin) [0.00s (0.40ms/blk)]
2026-10-08T03:00:01.000300Z [bench]   - Connect total: 1.00ms [0.00s (1.00ms/blk)]
2026-10-08T03:00:01.000400Z UpdateTip: new best=00aa height=100 version=0x20000000 log2_work=10.0 tx=101 \
date='2026-01-01T00:00:00Z' progress=0.5 cache=0.1MiB(1txo) evodb_cache=0.0MiB
2026-10-08T03:00:01.000500Z [bench] - Connect block: 2.00ms [0.00s (2.00ms/blk)]
2026-10-08T03:00:02.000000Z [bench]     - Verify 4 txins: 0.80ms (0.200ms/txin) [0.00s (0.60ms/blk)]
2026-10-08T03:00:02.000100Z UpdateTip: new best=00bb height=101 version=0x20000000 log2_work=10.1 tx=105 \
date='2026-01-01T00:02:30Z' progress=0.5 cache=0.1MiB(1txo) evodb_cache=0.0MiB
2026-10-08T03:00:02.000200Z [bench] - Connect block: 4.00ms [0.00s (3.00ms/blk)]
2026-10-08T03:00:03.000000Z [bench] CreateNewBlock() packages: 1.50ms (10 packages, 0 updated descendants), \
validity: 0.50ms (total 2.00ms)
"""

DEBUG_LOG_PLAIN = """\
2026-10-08T03:00:01.000000Z UpdateTip: new best=00aa height=1 version=0x20000000 log2_work=1.0 tx=2 date='x' progress=0
2026-10-08T03:00:01.500000Z UpdateTip: new best=00bb height=2 version=0x20000000 log2_work=1.0 tx=3 date='x' progress=0
2026-10-08T03:00:02.500000Z UpdateTip: new best=00cc height=3 version=0x20000000 log2_work=1.0 tx=4 date='x' progress=0
"""


def write(text):
    f = tempfile.NamedTemporaryFile("w", suffix=".log", delete=False)
    f.write(text)
    f.close()
    return f.name


def doc(impl, metrics, params=None):
    env = {"hostname": "h", "cpu_model": "c", "cpu_slice": "numa:0", "slice_cpus": 16, "kernel": "k"}
    r = results.Result("s", {"impl": impl}, env, params or {"reps": 5})
    for name, (samples, better, gate) in metrics.items():
        r.metric(name, samples, "ms", better=better, gate=gate)
    r.doc["status"] = "ok"
    return r.doc


class LogParse(unittest.TestCase):
    def test_bench_blocks(self):
        blocks = list(logparse.iter_dashd_blocks(write(DEBUG_LOG_BENCH)))
        self.assertEqual([b["height"] for b in blocks], [100, 101])
        self.assertEqual([b["connect_ms"] for b in blocks], [2.0, 4.0])
        self.assertEqual([b["verify_txins"] for b in blocks], [2, 4])
        self.assertEqual(blocks[0]["txs"], 3)

    def test_plain_blocks_and_metrics(self):
        path = write(DEBUG_LOG_PLAIN)
        self.assertEqual([b["height"] for b in logparse.iter_dashd_blocks(path)], [1, 2, 3])
        r = results.Result("s", {"impl": "dashd"}, {}, {})
        logparse.record_block_metrics(r, path, "dashd")
        self.assertEqual(r.doc["metrics"]["block_count"]["value"], 3)
        self.assertAlmostEqual(r.doc["metrics"]["block_interval_ms_median"]["value"], 750.0)

    def test_script_verify_throughput(self):
        r = results.Result("s", {"impl": "dashd"}, {}, {})
        logparse.record_block_metrics(r, write(DEBUG_LOG_BENCH), "dashd")
        # 6 txins in 1.2 ms
        self.assertAlmostEqual(r.doc["metrics"]["script_verify_txin_per_s"]["value"], 5000.0)
        self.assertEqual(r.doc["metrics"]["block_connect_ms_median"]["value"], 3.0)

    def test_create_new_block(self):
        cnb = logparse.parse_create_new_block(write(DEBUG_LOG_BENCH))
        self.assertEqual(cnb, [{"packages_ms": 1.5, "packages": 10, "validity_ms": 0.5, "total_ms": 2.0}])


# Startup lines as dashd 24.0.0-rc.3 wrote them (regtest, -txindex -addressindex -spentindex -timestampindex).
DEBUG_LOG_INIT = """\
2026-10-08T07:24:05Z Script verification uses 15 additional threads
2026-10-08T07:24:05Z Cache configuration:
2026-10-08T07:24:05Z * Using 2.0 MiB for block index database
2026-10-08T07:24:05Z * Using 37.2 MiB for transaction index database
2026-10-08T07:24:05Z * Using 32.6 MiB for address index database
2026-10-08T07:24:05Z * Using 3.6 MiB for timestamp index database
2026-10-08T07:24:05Z * Using 28.1 MiB for spent index database
2026-10-08T07:24:05Z * Using 8.0 MiB for chain state database
2026-10-08T07:24:05Z * Using 188.5 MiB for in-memory UTXO set (plus up to 117.3 MiB of unused mempool space)
2026-10-08T07:24:05Z Validating signatures for all blocks.
"""


class TipTime(unittest.TestCase):
    def test_update_tip_is_utc(self):
        ts, h = logparse.update_tip(write(DEBUG_LOG_PLAIN), "dashd", 2)
        self.assertEqual(h, "00bb")
        self.assertAlmostEqual(ts, 1791428401.5)  # 2026-10-08T03:00:01.5Z
        self.assertIsNone(logparse.tip_time(write(DEBUG_LOG_PLAIN), "dashd", 99))

    def test_only_this_invocation_counts(self):
        path = write(DEBUG_LOG_PLAIN)
        # Spawned after these lines were written: a stale log cannot answer (it used to give a negative time).
        self.assertIsNone(logparse.update_tip(path, "dashd", 2, since=1791428402.6))
        self.assertEqual(logparse.update_tip(path, "dashd", 3, since=1791428402.0)[1], "00cc")
        offset = len(DEBUG_LOG_PLAIN.splitlines(True)[0]) + len(DEBUG_LOG_PLAIN.splitlines(True)[1])
        self.assertIsNone(logparse.update_tip(path, "dashd", 2, offset=offset))
        self.assertEqual(logparse.last_tip(path, "dashd")[1:], (3, "00cc"))

    def test_jsonl_window(self):
        import json

        path = write("\n".join(json.dumps({"height": h, "ts": 1000.0 + h, "hash": f"h{h}"}) for h in range(5)))
        self.assertIsNone(logparse.update_tip(path, "jsonl", 1, since=1003.5))
        self.assertEqual(logparse.update_tip(path, "jsonl", 4, since=1003.5), (1004.0, "h4"))


class EffectiveConfig(unittest.TestCase):
    def test_init_lines(self):
        eff = logparse.init_config(write(DEBUG_LOG_INIT))
        self.assertEqual(eff, {"dbcache_mib": 300, "script_threads": 16, "assumevalid": "0"})
        offset = len(DEBUG_LOG_INIT)
        self.assertEqual(logparse.init_config(write(DEBUG_LOG_INIT), offset)["dbcache_mib"], None)

    def test_problems(self):
        cfg = impls.NodeConfig(chain="regtest", datadir="/x", txindex=True, dbcache_mib=300, par=16, assumevalid="0")
        good = {"dbcache_mib": 300, "script_threads": 16, "assumevalid": "0", "indexes": ["txindex"],
                "maxmempool_bytes": 300000000}
        self.assertEqual(impls.config_problems(cfg, good), [])
        self.assertTrue(impls.config_problems(cfg, {**good, "dbcache_mib": 100}))
        self.assertTrue(impls.config_problems(cfg, {**good, "script_threads": 4}))
        self.assertTrue(impls.config_problems(cfg, {**good, "assumevalid": "00ab"}))
        self.assertTrue(impls.config_problems(cfg, {**good, "indexes": []}))  # asked-for index not running
        self.assertTrue(impls.config_problems(cfg, {**good, "indexes": ["txindex", "addressindex"]}))  # extra work
        self.assertTrue(impls.config_problems(cfg, {**good, "dbcache_mib": None}))  # unverifiable fails closed

    def test_index_sync(self):
        from nodebench.rpc import RpcError
        from nodebench.scenarios import common

        class R:
            def __init__(self, info):
                self.info = info

            def getindexinfo(self):
                if self.info is None:
                    raise RpcError("getindexinfo", {"code": -32601, "message": "Method not found"})
                return self.info

        self.assertTrue(common.indexes_synced(R({"txindex": {"synced": True}}), {"txindex"}))
        self.assertFalse(common.indexes_synced(R({"txindex": {"synced": False}}), {"txindex"}))
        self.assertFalse(common.indexes_synced(R({"txindex": {}}), {"txindex"}))  # no "synced": not synced
        with self.assertRaises(RuntimeError):
            common.indexes_synced(R({}), {"txindex"})  # the review's empty-index acceptance
        with self.assertRaises(RuntimeError):
            common.indexes_synced(R(None), {"txindex"})
        self.assertTrue(common.indexes_synced(R({"txindex": {"synced": True}}), {"txindex", "addressindex"}))

    def test_probe_address(self):
        self.assertTrue(impls.probe_address("main").startswith("X"))
        self.assertTrue(impls.probe_address("regtest").startswith("y"))


class CacheDrops(unittest.TestCase):
    def ctx(self, drop=True, pair=None):
        import types

        return types.SimpleNamespace(args=types.SimpleNamespace(drop_caches=drop, pair=pair),
                                     result=results.Result("s", {"impl": "dashd"}, {}, {}))

    def test_failed_drop_fails_closed(self):
        from unittest import mock

        from nodebench.scenarios import common

        ctx = self.ctx()
        with mock.patch.object(common, "_drop_page_cache", return_value=False):
            with self.assertRaises(RuntimeError):
                common.drop_caches(ctx)
        self.assertNotEqual(ctx.result.doc["params"].get("page_cache_dropped"), True)
        self.assertFalse(common.drop_caches(self.ctx(drop=False)))

    def test_concurrent_pair_drops_once_at_the_barrier(self):
        from unittest import mock

        from nodebench.scenarios import common

        with tempfile.TemporaryDirectory() as d, mock.patch.dict(os.environ, {"NODE_BENCH_BARRIER": d,
                                                                              "NODE_BENCH_BARRIER_N": "2"}):
            open(os.path.join(d, "drop.B"), "w").close()  # the other side is already there
            ctx = self.ctx(pair="P:0:A")
            with mock.patch.object(common, "_drop_page_cache", return_value=True) as dropped:
                self.assertTrue(common.drop_caches(ctx))
                self.assertEqual(dropped.call_count, 1)
            self.assertTrue(os.path.exists(os.path.join(d, "DROP.ok")))
            with self.assertRaises(RuntimeError):  # a second drop would hit the other side mid-measurement
                common.drop_caches(ctx)
            self.assertFalse(common.per_start_cache_drop_possible(ctx))


class Layout(unittest.TestCase):
    def test_pair_slices(self):
        pair.validate_layout(True, ("0-11@0", "12-15"), ("16-27@1", "28-31"))
        pair.validate_layout(False, ("0-11", "12-15"), ("0-11", "12-15"))
        for bad in ((True, ("0-11@0", "12-15"), ("12-27@1", "28-31")),   # aux A overlaps node B
                    (True, ("0-11@0", "12-15"), ("16-25@1", "28-31")),   # unequal node slices
                    (False, ("0-11", "12-15"), ("16-27", "28-31")),      # interleave must share slices
                    (False, ("0-11", "8-15"), ("0-11", "8-15")),         # node and aux overlap
                    (True, ("0-11@0", "12-15"), ("16-27@1", "28-999"))):  # CPUs that do not exist
            with self.assertRaises(SystemExit):
                pair.validate_layout(*bad)

    def test_irrelevant_options_refused(self):
        import argparse

        from nodebench import cli
        from nodebench.scenarios import load_all

        reg = load_all()
        ns = argparse.Namespace(**{k: None for k in cli.RUN_PARAMS})
        ns.dbcache = 100
        with self.assertRaises(SystemExit):
            cli.check_params(ns, reg["mempool-accept"])  # fixture scenarios fix dbcache
        cli.check_params(ns, reg["reindex-testnet"])
        ns.dbcache, ns.addressindex = None, 1
        with self.assertRaises(SystemExit):
            cli.check_params(ns, reg["startup-mainnet"])
        ns.addressindex = 0  # an explicit 0 is an option too
        with self.assertRaises(SystemExit):
            cli.check_params(ns, reg["rpc-regtest"])
        ns.addressindex, ns.seeds = None, 2
        with self.assertRaises(SystemExit):
            cli.check_params(ns, reg["ibd-testnet-net"])


class Noise(unittest.TestCase):
    def test_classify(self):
        base = {"n": 3, "foreign_frac_mean": 0.05, "psi_cpu_avg10_mean": 1.0, "slice_foreign_frac_mean": 0.05}
        self.assertEqual(noise.classify(base), "quiet")
        self.assertEqual(noise.classify({**base, "slice_foreign_frac_mean": 0.2}), "moderate")
        self.assertEqual(noise.classify({**base, "slice_foreign_frac_mean": 0.9}), "heavy")
        self.assertEqual(noise.classify({**base, "psi_cpu_avg10_mean": 20.0}), "moderate")
        self.assertEqual(noise.classify({"n": 0}), "unknown")

    @staticmethod
    def samples(n, host=0.0, sl=0.0, psi_io=0.0, t=0.0):
        return [{"t0": t + i, "t1": t + i + 1, "host": host, "slice": sl, "load1": 1.0, "psi_cpu": 0.0,
                 "psi_io": psi_io, "psi_mem": 0.0} for i in range(n)]

    def test_one_saturated_interval_is_not_quiet(self):
        # The review's case: 19 idle intervals + 1 with all 12 slice CPUs foreign-busy averaged to "quiet".
        s = noise.summarize(self.samples(19) + self.samples(1, host=12.0, sl=12.0), slice_ncpu=12)
        self.assertLess(s["slice_foreign_frac_mean"], 0.1)
        self.assertNotEqual(noise.classify(s), "quiet")

    def test_io_pressure_is_not_quiet(self):
        s = noise.summarize(self.samples(20, psi_io=40.0), slice_ncpu=12)
        self.assertEqual(noise.classify(s), "heavy")

    def test_own_pressure_does_not_disqualify_a_measurement(self):
        # PSI is host-wide: during the run it includes our own node and load generators.
        s = noise.summarize(self.samples(20, psi_io=40.0), slice_ncpu=12)
        self.assertEqual(noise.classify(s, use_psi=False), "quiet")

    def test_only_the_measured_window_counts(self):
        m = noise.LoadMonitor(None)
        m.t0 = 0.0
        busy_prep = self.samples(50, host=30.0)          # preparation: busy, but before the start mark
        quiet = self.samples(20, t=100.0)
        m.samples = busy_prep + quiet
        m.marks["start"] = 100.0
        s = m.stop()
        self.assertEqual(s["n"], 20)
        self.assertEqual(noise.classify(s), "quiet")
        m.samples, m.marks = quiet + self.samples(20, host=30.0, t=200.0), {"start": 100.0}
        self.assertEqual(noise.classify(m.stop()), "heavy")  # noise during the measurement is not averaged away


class Track(unittest.TestCase):
    """common.track against a fake node: the loop, the final sample and the CSV must not crash."""

    def test_track_finishes_and_samples(self):
        import subprocess
        import time
        import types

        from nodebench.scenarios import common

        heights = iter(range(0, 1000, 10))

        class FakeRpc:
            def getblockchaininfo(self):
                h = next(heights)
                return {"blocks": h, "headers": 100, "initialblockdownload": h < 100}

            def getblockcount(self):
                return 100

            def close(self):
                pass

        proc = subprocess.Popen(["sleep", "30"])
        try:
            node = types.SimpleNamespace(pid=proc.pid, proc=proc, t_spawn=time.monotonic(),
                                         new_rpc=lambda timeout=0: FakeRpc())
            with tempfile.TemporaryDirectory() as d:
                ctx = types.SimpleNamespace(args=types.SimpleNamespace(interval=0.05), run_dir=d,
                                            log=lambda m: None)
                done, samples, path = common.track(ctx, node, lambda row, rpc: row["blocks"] >= 50)
                self.assertGreater(done, 0)
                self.assertTrue(samples and "rss_mib" in samples[-1])
                with open(path) as f:
                    self.assertGreater(len(f.read().splitlines()), 1)
        finally:
            proc.kill()
            proc.wait()


class Stats(unittest.TestCase):
    def test_describe(self):
        d = stats.describe([1, 2, 3, 4, 100])
        self.assertEqual(d["median"], 3)
        self.assertEqual(d["n"], 5)

    def test_bootstrap_ci_brackets_ratio(self):
        lo, hi = stats.bootstrap_ratio_ci([10, 11, 9, 10, 10, 11], [5, 5.5, 4.5, 5, 5, 5.5])
        self.assertTrue(lo < 2.0 < hi or 1.8 < lo <= 2.0)


def runs(impl, values, better="lower", gate=True, target_2x=True, status="ok"):
    """One result doc per value (a metric 't' per run), as `run` would write them."""
    docs = []
    for i, v in enumerate(values):
        d = doc(impl, {}, {"reps": 5})
        d["run_id"] = f"2026{i:02d}-s-{impl}"
        d["metrics"]["t"] = {"value": v, "unit": "s", "statistic": "median", "better": better, "gate": gate,
                             "target_2x": target_2x, "stats": {"n": 1}, "samples": [v]}
        d["status"] = status
        docs.append(d)
    return docs


class Gate(unittest.TestCase):
    META = {"gate": True, "better": "lower", "target_2x": True}

    def v(self, a, b, required=5, **kw):
        return compare.gate_verdict(a, b, self.META, required, **kw)[3]

    def test_prd_rule(self):
        a = [100, 101, 99, 100, 102, 98]
        self.assertEqual(self.v(a, [x * 1.05 for x in a]), "FAIL")      # 5% slower, every run
        self.assertEqual(self.v(a, [x * 1.005 for x in a]), "PASS")     # significant but < 1% effect
        self.assertEqual(self.v(a, [x * 0.4 for x in a]), "2x")
        self.assertEqual(self.v(a, [130, 70, 120, 80, 110, 90]), "PASS")  # noisy: not shown slower

    def test_insufficient_runs(self):
        self.assertEqual(self.v([1, 1, 1], [2, 2, 2]), "INSUFFICIENT")
        # 3 vs 3 can never reach p < 0.05: a 2x-slower candidate must not PASS.
        self.assertEqual(self.v([1, 1, 1], [2, 2, 2], required=3), "INSUFFICIENT")
        self.assertEqual(self.v([1, 1, 1, 1], [2, 2, 2, 2], required=4), "FAIL")  # 4 vs 4: min p = 1/70

    def test_strict_non_inferiority(self):
        a = [100, 101, 99, 100, 102, 98]
        self.assertEqual(self.v(a, a, strict=True, ci=(0.9, 1.1)), "INCONCLUSIVE")
        self.assertEqual(self.v(a, a, strict=True, ci=(0.99, 1.01)), "PASS")

    def test_higher_is_better(self):
        meta = {"gate": True, "better": "higher", "target_2x": False}
        a = [1000, 1010, 990, 1000, 1005]
        self.assertEqual(compare.gate_verdict(a, [x * 0.9 for x in a], meta, 5)[3], "FAIL")
        self.assertEqual(compare.gate_verdict(a, [x * 3 for x in a], meta, 5)[3], "PASS")  # no 2x target

    def compare_dirs(self, base_docs, cand_docs):
        import json

        with tempfile.TemporaryDirectory() as d:
            for i, dd in enumerate(base_docs + cand_docs):
                with open(os.path.join(d, f"{i:03d}.json"), "w") as f:
                    json.dump(dd, f)
            return compare.compare(d, d)

    def test_compare_uses_run_level_values(self):
        rep = self.compare_dirs(runs("dashd", [10, 10.1, 9.9, 10, 10.2]), runs("rust", [12, 12.1, 11.9, 12, 12.2]))
        v = {r["metric"]: r for r in rep["rows"]}
        self.assertEqual(v["t"]["verdict"], "FAIL")
        self.assertEqual(v["t"]["n"], (5, 5))
        self.assertEqual(v["t"]["test"], "exact")
        self.assertEqual(rep["gate"], "FAIL")

    def test_compare_errored_candidate_blocks(self):
        rep = self.compare_dirs(runs("dashd", [1] * 5), runs("rust", [0.5], status="error"))
        self.assertEqual(rep["rows"][0]["verdict"], "ERROR")
        self.assertEqual(rep["gate"], "INCOMPLETE")

    def test_compare_missing_metric(self):
        cand = runs("rust", [1] * 5)
        for d in cand:
            del d["metrics"]["t"]
        rep = self.compare_dirs(runs("dashd", [1] * 5), cand)
        self.assertEqual(rep["rows"][0]["verdict"], "MISSING")
        self.assertEqual(rep["gate"], "INCOMPLETE")

    def test_compare_is_never_official(self):
        docs = runs("dashd", [10, 10.1, 9.9, 10, 10.2]) + runs("rust", [10, 10.1, 9.9, 10, 10.2])
        for d in docs:  # a scenario with a required inventory, fully observed
            d["scenario"] = "spv-headers-testnet"
            d["metrics"]["header_sync_s"] = d["metrics"].pop("t")
        rep = self.compare_dirs(docs[:5], docs[5:])
        self.assertEqual(rep["gate"], "PROVISIONAL")
        rep = self.compare_dirs(runs("dashd", [10] * 5), runs("rust", [10] * 5))
        self.assertEqual(rep["gate"], "INCOMPLETE")  # no inventory for this scenario: never a pass

    def test_compare_newest_failure_stays_visible(self):
        base = runs("dashd", [1] * 5)
        cand = runs("rust", [1] * 5) + runs("rust", [1], status="error")
        cand[-1]["run_id"], cand[-1]["started_at"] = "2099-s-rust", "2099-01-01T00:00:00Z"
        rep = self.compare_dirs(base, cand)
        self.assertEqual(rep["rows"][0]["verdict"], "ERROR")
        self.assertEqual(rep["gate"], "INCOMPLETE")

    def test_invalid_and_zero_values(self):
        nan = float("nan")
        self.assertEqual(self.v([1.0] * 6, [nan] * 6), "ERROR")  # the review's NaN PASS
        self.assertEqual(self.v([1.0] * 6, [-1.0] * 6), "ERROR")
        self.assertEqual(self.v([0] * 6, [0] * 6), "PASS")  # zero is a value, not absence
        self.assertEqual(self.v([0] * 6, [1] * 6), "FAIL")
        self.assertEqual(self.v([1.0] * 6, []), "MISSING")

    def test_nan_is_not_written(self):
        r = results.Result("s", {"impl": "dashd"}, {}, {})
        with self.assertRaises(ValueError):
            r.metric("t", [1.0, float("nan")], "s")
        r.info("x", float("inf"))
        with tempfile.TemporaryDirectory() as d:
            path = r.finish(d)
            with open(path) as f:
                text = f.read()
        self.assertNotIn("NaN", text)
        self.assertNotIn("Infinity", text)

    def test_param_mismatch_warns(self):
        base = doc("dashd", {}, {"reps": 5})
        cand = doc("rust", {}, {"reps": 3})
        self.assertTrue(any("reps" in w for w in compare.comparable(base, cand)))


class Paired(unittest.TestCase):
    def test_ratio_ci(self):
        g, ci = pair.paired_ratio([0.80, 0.82, 0.79, 0.81, 0.80])
        self.assertAlmostEqual(g, 0.804, places=2)
        self.assertLess(ci[1], 1)
        self.assertIsNone(pair.paired_ratio([1.0, 1.1])[1])

    @staticmethod
    def rep(*rows, official=True):
        return {"rows": [dict(scenario="s", metric=m, verdict=v, speedup=1.0, ci=ci, p=0.5, n=6)
                         for m, v, ci in rows], "warnings": [], "official": official}

    def test_confirmation_clears_only_with_a_tight_ci(self):
        main = self.rep(("t", "FAIL", None), ("u", "FAIL", None), ("w", "FAIL", None))
        conf = self.rep(("t", "PASS", (0.995, 1.01)),   # rules out a >= 1% slowdown: cleared
                        ("u", "PASS", (0.95, 1.05)),    # merely not significant: stands
                        ("w", "FAIL", (0.9, 0.97)))
        pair.settle_confirmation(main, conf, "c")
        v = {r["metric"]: r["verdict"] for r in main["rows"]}
        self.assertEqual((v["t"], v["u"], v["w"]), ("CLEARED", "FAIL", "FAIL"))
        self.assertEqual(main["gate"], "FAIL")
        self.assertIn("u (confirmation)", v)

    def test_confirmation_keeps_new_fails(self):
        # The review's case: t=FAIL,u=PASS then t=PASS,u=FAIL used to end as PASS with both PASS.
        main = self.rep(("t", "FAIL", None), ("u", "PASS", (0.99, 1.01)))
        pair.settle_confirmation(main, self.rep(("t", "PASS", (0.995, 1.01)), ("u", "FAIL", (0.9, 0.97))), "c")
        v = {r["metric"]: r["verdict"] for r in main["rows"]}
        self.assertEqual(v["t"], "CLEARED")
        self.assertEqual(v["u (confirmation)"], "FAIL")
        self.assertEqual(main["gate"], "FAIL")

    def test_confirmation_error_keeps_fail(self):
        crashed = self.rep(("t", "FAIL", None))
        pair.settle_confirmation(crashed, self.rep(("(runs)", "ERROR", None), ("t", "PASS", (0.999, 1.0))), "c")
        self.assertEqual(crashed["rows"][0]["verdict"], "FAIL")  # confirmation errored: FAIL stands
        self.assertEqual(crashed["gate"], "FAIL")

    def test_confirmation_incomplete_blocks(self):
        main = self.rep(("t", "FAIL", None), ("u", "PASS", (0.99, 1.01)))
        pair.settle_confirmation(main, self.rep(("t", "PASS", (0.995, 1.01)), ("u", "MISSING", None)), "c")
        self.assertEqual(main["rows"][0]["verdict"], "FAIL")  # its scenario was not measured cleanly
        self.assertEqual(main["gate"], "FAIL")
        cleared = self.rep(("t", "FAIL", None))
        pair.settle_confirmation(cleared, self.rep(("t", "PASS", (0.995, 1.01)), official=False), "c")
        self.assertEqual(cleared["gate"], "PROVISIONAL")  # confirmation runs not official

    def test_recount_states(self):
        rep = pair.recount({"rows": [{"verdict": "PASS"}, {"verdict": "ERROR"}]})
        self.assertEqual(rep["gate"], "INCOMPLETE")
        self.assertEqual(rep["n_error"], 1)
        self.assertEqual(pair.recount({"rows": [{"verdict": "2x"}], "official": True})["gate"], "PASS")
        self.assertEqual(pair.recount({"rows": [{"verdict": "2x"}]})["gate"], "PROVISIONAL")
        self.assertEqual(pair.recount({"rows": [{"verdict": "MISSING"}, {"verdict": "PASS"}],
                                       "official": True})["gate"], "INCOMPLETE")
        self.assertEqual(pair.recount({"rows": [{"verdict": "MISSING"}, {"verdict": "FAIL"}]})["gate"], "FAIL")
        self.assertEqual(pair.recount({"rows": [], "official": True})["gate"], "INCOMPLETE")


def pair_docs(values_a, values_b, scenario="spv-headers-testnet", official=True, extra=None, params=None):
    """Result docs of one pair on disk layout: {metric: [per-round values]} per side (None = not recorded)."""
    docs = []
    for side, values in (("A", values_a), ("B", values_b)):
        rounds = max(len(v) for v in values.values())
        for r in range(rounds):
            d = doc("dashd" if side == "A" else "rust", {}, {"reps": 5, **(params or {})})
            d["scenario"] = scenario
            d["run_id"] = f"p-{scenario}-r{r}{side}"
            d["pair"] = {"id": "P", "round": str(r), "side": side}
            d["load"] = {"class": "quiet" if official else "heavy", "official": official}
            for name, vals in values.items():
                if vals[r] is not None:
                    d["metrics"][name] = {"value": vals[r], "unit": "s", "statistic": "median", "better": "lower",
                                          "gate": True, "target_2x": False, "stats": {"n": 1}, "samples": [vals[r]]}
            if extra:
                extra(d, side, r)
            docs.append(d)
    return docs


class PairGate(unittest.TestCase):
    """pair.analyze on synthetic result files: the review's fail-open reproductions."""

    def analyze(self, docs, rounds=6, scenario="spv-headers-testnet", **opts):
        import json

        with tempfile.TemporaryDirectory() as d:
            for x in docs:
                with open(os.path.join(d, x["run_id"] + ".json"), "w") as f:
                    json.dump(x, f)
            return pair.analyze(d, "P", opts, expect=([scenario], rounds))

    def test_complete_official_pass(self):
        same = [10, 10.1, 9.9, 10, 10.2, 10]
        rep = self.analyze(pair_docs({"header_sync_s": same}, {"header_sync_s": same}))
        self.assertEqual(rep["gate"], "PASS", rep["rows"])

    def test_missing_on_candidate_blocks(self):
        same = [10, 10.1, 9.9, 10, 10.2, 10]
        rep = self.analyze(pair_docs({"header_sync_s": same, "other_s": same}, {"header_sync_s": same}))
        v = {r["metric"]: r["verdict"] for r in rep["rows"]}
        self.assertEqual(v["other_s"], "MISSING")
        self.assertEqual(rep["gate"], "INCOMPLETE")  # was PASS with n_missing=1

    def test_required_missing_on_both_sides_blocks(self):
        same = [10, 10.1, 9.9, 10, 10.2, 10]
        rep = self.analyze(pair_docs({"other_s": same}, {"other_s": same}))
        v = {r["metric"]: r["verdict"] for r in rep["rows"]}
        self.assertEqual(v["header_sync_s"], "MISSING")
        self.assertEqual(rep["gate"], "INCOMPLETE")

    def test_missing_in_a_later_round_blocks(self):
        same = [10, 10.1, 9.9, 10, 10.2, 10]
        rep = self.analyze(pair_docs({"header_sync_s": same}, {"header_sync_s": same[:5] + [None]}))
        self.assertEqual(rep["rows"][0]["verdict"], "MISSING")
        self.assertIn("r5B", rep["rows"][0]["b"])

    def test_first_round_none_does_not_hide_a_metric(self):
        same = [10, 10.1, 9.9, 10, 10.2, 10]
        rep = self.analyze(pair_docs({"header_sync_s": same, "x": [None] + same[1:]},
                                     {"header_sync_s": same, "x": [None] + same[1:]}))
        self.assertEqual({r["metric"]: r["verdict"] for r in rep["rows"]}["x"], "MISSING")

    def test_provisional_runs_never_pass(self):
        same = [10, 10.1, 9.9, 10, 10.2, 10]
        rep = self.analyze(pair_docs({"header_sync_s": same}, {"header_sync_s": same}, official=False))
        self.assertEqual(rep["gate"], "PROVISIONAL")
        self.assertTrue(rep["provisional_reasons"])

    def test_weakened_gate_options_are_provisional(self):
        same = [10, 10.1, 9.9, 10, 10.2, 10]
        rep = self.analyze(pair_docs({"header_sync_s": same}, {"header_sync_s": same}), effect=0.05)
        self.assertEqual(rep["gate"], "PROVISIONAL")

    def test_nan_round_is_an_error(self):
        same = [10, 10.1, 9.9, 10, 10.2, 10]
        rep = self.analyze(pair_docs({"header_sync_s": same}, {"header_sync_s": same[:5] + [float("nan")]}))
        self.assertEqual(rep["rows"][0]["verdict"], "ERROR")

    def test_parameter_mismatch_between_sides(self):
        same = [10, 10.1, 9.9, 10, 10.2, 10]

        def differ(d, side, r):
            if side == "B":
                d["params"]["dbcache"] = 512

        rep = self.analyze(pair_docs({"header_sync_s": same}, {"header_sync_s": same}, extra=differ))
        self.assertIn("(params)", [r["metric"] for r in rep["rows"] if r["verdict"] == "ERROR"])
        self.assertEqual(rep["gate"], "INCOMPLETE")

    def test_required_metric_recorded_as_info(self):
        same = [10, 10.1, 9.9, 10, 10.2, 10]

        def demote(d, side, r):
            if side == "B":
                d["metrics"]["header_sync_s"]["gate"] = False

        rep = self.analyze(pair_docs({"header_sync_s": same}, {"header_sync_s": same}, extra=demote))
        self.assertEqual(rep["rows"][0]["verdict"], "ERROR")
        self.assertEqual(rep["gate"], "INCOMPLETE")

    def test_narrowed_run_cannot_pass(self):
        # The review's repro: a bench-log/bounded IBD gates nothing on time, B 50% slower, footprint rows pass.
        same = [10, 10.1, 9.9, 10, 10.2, 10]
        slow = [x * 1.5 for x in same]

        def demote(d, side, r):
            d["metrics"]["ibd_time_s"]["gate"] = False
            d["params"]["bench_log"] = 1

        docs = pair_docs({"ibd_time_s": same, "peak_rss_mib": same, "disk_total_gib": same},
                         {"ibd_time_s": slow, "peak_rss_mib": same, "disk_total_gib": same},
                         scenario="ibd-testnet-seed", extra=demote)
        rep = self.analyze(docs, scenario="ibd-testnet-seed")
        self.assertEqual(rep["gate"], "INCOMPLETE")

    def test_informational_scenario_never_passes(self):
        same = [10, 10.1, 9.9, 10, 10.2, 10]
        rep = self.analyze(pair_docs({"x": same}, {"x": same}, scenario="gbt-mainnet"), scenario="gbt-mainnet")
        self.assertIn("(inventory)", [r["metric"] for r in rep["rows"]])
        self.assertEqual(rep["gate"], "INCOMPLETE")

    def test_timing_method_and_chain_must_match(self):
        same = [10, 10.1, 9.9, 10, 10.2, 10]

        def differ(d, side, r):
            d["metrics"]["ibd_time_s"] = dict(d["metrics"]["header_sync_s"],
                                              measured_by="block-log" if side == "A" else "poll")
            d["params"]["seed"] = {"height": 100 if r else 99, "tip": "aa"}

        rep = self.analyze(pair_docs({"header_sync_s": same}, {"header_sync_s": same}, extra=differ))
        errors = {r["metric"] for r in rep["rows"] if r["verdict"] == "ERROR"}
        self.assertTrue({"(timing method)", "(seed chain)"} <= errors, errors)

    def test_no_expectation_is_an_error(self):
        import json

        same = [10, 10.1, 9.9, 10, 10.2, 10]
        with tempfile.TemporaryDirectory() as d:
            for x in pair_docs({"header_sync_s": same}, {"header_sync_s": same}):
                with open(os.path.join(d, x["run_id"] + ".json"), "w") as f:
                    json.dump(x, f)
            rep = pair.analyze(d, "P", {})
        self.assertIn("(expected runs)", [r["metric"] for r in rep["rows"]])
        self.assertEqual(rep["gate"], "INCOMPLETE")

    def test_effective_config_mismatch(self):
        same = [10, 10.1, 9.9, 10, 10.2, 10]

        def eff(d, side, r):
            d["effective_config"] = {"dbcache_mib": 4096, "script_threads": 16 if side == "A" else 32}

        rep = self.analyze(pair_docs({"header_sync_s": same}, {"header_sync_s": same}, extra=eff))
        self.assertIn("(effective config)", [r["metric"] for r in rep["rows"] if r["verdict"] == "ERROR"])
        self.assertEqual(rep["gate"], "INCOMPLETE")

    def test_cold_vs_warm(self):
        same = [10, 10.1, 9.9, 10, 10.2, 10]

        def cache(d, side, r):
            d["params"]["page_cache_dropped"] = side == "A"

        rep = self.analyze(pair_docs({"header_sync_s": same}, {"header_sync_s": same}, extra=cache))
        self.assertIn("(page cache)", [r["metric"] for r in rep["rows"] if r["verdict"] == "ERROR"])

    def test_missing_round_is_an_error(self):
        same = [10, 10.1, 9.9, 10, 10.2, 10]
        rep = self.analyze(pair_docs({"header_sync_s": same}, {"header_sync_s": same}), rounds=8)
        self.assertEqual(rep["rows"][0]["metric"], "(runs)")
        self.assertEqual(rep["gate"], "INCOMPLETE")


class Inventory(unittest.TestCase):
    def test_declared_inventories(self):
        req = compare.required_metrics
        self.assertIn("rpc_quorum_info_ms", req("rpc-mainnet", {"clients": [16]}))
        self.assertNotIn("rpc_quorum_info_ms", req("rpc-regtest", {"clients": [16]}))
        self.assertIn("rpc_gettxout_ops_c16", req("rpc-regtest", {"clients": [16]}))
        self.assertEqual(req("gbt-regtest", {"mempool_sizes": [0, 5]}),
                         {f"gbt_ms_mempool_{n}" for n in (0, 5, 1000, 5000, 20000)})
        self.assertEqual(req("ibd-mainnet-net", {}), set())
        self.assertEqual(req("ibd-testnet-seed", {"bench_log": 1}), req("ibd-testnet-seed", {}))  # canonical
        self.assertIn("disk_indexes_gib", req("ibd-testnet-seed", {"txindex": 0}))  # narrowing cannot drop it
        self.assertEqual(len(req("microbench", {"filter": "ECDSA.*"})), 38)  # a filter cannot narrow it
        self.assertIn("startup_cold_s", req("startup-mainnet", {"drop_caches": True}))
        # Narrowing parameters cannot shrink the canonical inventory.
        self.assertEqual(len(req("microbench", {"filter": "SipHash_32b"})), 38)
        self.assertIn("gbt_ms_mempool_20000", req("gbt-regtest", {"mempool_sizes": [0]}))
        self.assertIn("rpc_gettxout_ops_c64", req("rpc-regtest", {"clients": [16]}))
        self.assertIn("ibd_time_s", req("ibd-testnet-seed", {"bench_log": 1, "stop_height": 10}))


FORKING = """
import subprocess, sys, time
child = subprocess.Popen(["sleep", "120"])
open(sys.argv[1], "w").write(str(child.pid))
time.sleep(120)
"""


class ProcessTree(unittest.TestCase):
    """A forking wrapper (perf/strace-like) must not leave its child behind on any stop path."""

    def alive(self, pid):
        try:
            with open(f"/proc/{pid}/stat") as f:
                return f.read().rsplit(")", 1)[1].split()[0] != "Z"
        except OSError:
            return False

    def child_pid(self, path):
        import time

        for _ in range(200):
            if os.path.exists(path) and os.path.getsize(path):
                with open(path) as f:
                    return int(f.read())
            time.sleep(0.05)
        self.fail("wrapper never started its child")

    def test_reap_group_kills_orphaned_child(self):
        import signal

        with tempfile.TemporaryDirectory() as d:
            pidfile = os.path.join(d, "child")
            p, cg = impls.spawn([sys.executable, "-c", FORKING, pidfile], None)
            child = self.child_pid(pidfile)
            os.kill(p.pid, signal.SIGKILL)  # the wrapper dies, its child does not
            p.wait()
            self.assertTrue(self.alive(child))
            impls.reap_group(p, grace=0)
            self.assertFalse(self.alive(child))
            self.assertEqual(impls.group_members(p.pid), [])

    def test_node_kill_takes_the_whole_tree(self):
        class Forking(impls.Impl):
            name = "fake"

            def argv(self, cfg):
                return [sys.executable, "-c", FORKING, os.path.join(cfg.datadir, "child")]

        with tempfile.TemporaryDirectory() as d:
            node = impls.Node(Forking("python"), impls.NodeConfig(datadir=d))
            node.start()
            child = self.child_pid(os.path.join(d, "child"))
            node.kill()
            self.assertFalse(self.alive(child))
            self.assertNotIn(node.proc, impls.live_groups())

    def test_cgroup_join_prefix_runs_before_exec(self):
        from nodebench import limits

        prefix = limits.Cgroup("x", {}).join_prefix()
        self.assertEqual(prefix[:2], ["sh", "-c"])
        self.assertIn("cgroup.procs", prefix[2])
        self.assertTrue(prefix[2].endswith('exec "$@"'))


class PathBoundary(unittest.TestCase):
    """Writes stay under NODE_BENCH_ROOT and never reach a protected tree; tested on a temporary layout only."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(dir=os.environ.get("NODE_BENCH_TEST_TMP"))
        t = self.tmp.name
        self.root, self.chains = os.path.join(t, "bench"), os.path.join(t, "chains")
        for d in (self.root, self.chains, os.path.join(self.root, "results", "runs", "r1")):
            os.makedirs(d, exist_ok=True)
        self.saved = paths.ROOT, paths.PROTECTED
        paths.ROOT, paths.PROTECTED = self.root, (self.chains,)

    def tearDown(self):
        paths.ROOT, paths.PROTECTED = self.saved
        self.tmp.cleanup()

    def test_boundary(self):
        run = os.path.join(self.root, "results", "runs", "r1")
        self.assertEqual(paths.writable(run), os.path.realpath(run))
        with self.assertRaises(paths.UnsafePath):
            paths.writable(os.path.join(self.chains, "mainnet"))
        with self.assertRaises(paths.UnsafePath):
            paths.writable(os.path.join(self.tmp.name, "elsewhere"))
        link = os.path.join(run, "escape")
        os.symlink(self.chains, link)
        with self.assertRaises(paths.UnsafePath):  # resolved before the check
            paths.writable(os.path.join(link, "datadir"))
        paths.safe_rmtree(link)  # a link is unlinked, never followed
        self.assertTrue(os.path.isdir(self.chains))
        with self.assertRaises(paths.UnsafePath):
            paths.safe_rmtree(os.path.join(self.root, "results"))
        with self.assertRaises(paths.UnsafePath):
            paths.not_protected(os.path.join(self.chains, "x"))

    def test_root_overlapping_protected(self):
        paths.PROTECTED = (os.path.join(self.root, "inner"),)
        with self.assertRaises(paths.UnsafePath):
            paths.check_root()

    def test_clone_refuses_escaping_symlinks_and_empties_conf(self):
        src = os.path.join(self.root, "data", "seed-x")
        os.makedirs(os.path.join(src, "regtest", "blocks"))
        with open(os.path.join(src, "dash.conf"), "w") as f:
            f.write("datadir=/somewhere/else\n")
        for name in ("debug.log", "bench.jsonl"):
            with open(os.path.join(src, "regtest", name), "w") as f:
                f.write("old run\n")
        dst = os.path.join(self.root, "results", "runs", "r1", "datadir")
        impls.clone_datadir(src, dst)
        self.assertEqual(os.path.getsize(os.path.join(dst, "dash.conf")), 0)
        self.assertFalse(os.path.exists(os.path.join(dst, "regtest", "bench.jsonl")))
        self.assertFalse(os.path.exists(os.path.join(dst, "regtest", "debug.log")))
        os.symlink(self.chains, os.path.join(src, "regtest", "chainstate"))
        with self.assertRaises(paths.UnsafePath):
            impls.clone_datadir(src, dst)
        self.assertFalse(os.path.exists(dst))
        with self.assertRaises(paths.UnsafePath):
            impls.clone_datadir(self.chains, dst)


class Retention(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(dir=os.environ.get("NODE_BENCH_TEST_TMP"))
        self.saved = paths.ROOT
        paths.ROOT = self.tmp.name
        self.run_dir = os.path.join(self.tmp.name, "results", "runs", "r1")
        for d in ("datadir/regtest", "seed0-datadir/regtest"):
            os.makedirs(os.path.join(self.run_dir, d))
            with open(os.path.join(self.run_dir, d, "debug.log"), "w") as f:
                f.write("x" * 100)

    def tearDown(self):
        paths.ROOT = self.saved
        self.tmp.cleanup()

    def ctx(self, **kw):
        import types

        args = types.SimpleNamespace(**{"keep_failed": False, "keep_datadir": False, **kw})
        return types.SimpleNamespace(args=args, run_dir=self.run_dir,
                                     result=results.Result("s", {"impl": "dashd"}, {}, {}))

    def test_success_removes_datadirs_keeps_logs(self):
        from nodebench import cli

        cli.retain(self.ctx(), "ok")
        self.assertFalse(os.path.exists(os.path.join(self.run_dir, "datadir")))
        self.assertFalse(os.path.exists(os.path.join(self.run_dir, "seed0-datadir")))
        self.assertEqual(len(os.listdir(os.path.join(self.run_dir, "logs"))), 2)

    def test_failure_retention(self):
        from nodebench import cli

        cli.retain(self.ctx(keep_failed=True), "error")
        self.assertTrue(os.path.exists(os.path.join(self.run_dir, "datadir")))
        cli.retain(self.ctx(), "error")  # default: a failed run's datadirs go too
        self.assertFalse(os.path.exists(os.path.join(self.run_dir, "datadir")))

    def test_kept_datadir_survives(self):
        from nodebench import cli

        ctx = self.ctx(keep_datadir=True)
        ctx.result.artifact("datadir", os.path.join(self.run_dir, "datadir"))
        cli.retain(ctx, "ok")
        self.assertTrue(os.path.exists(os.path.join(self.run_dir, "datadir")))
        self.assertFalse(os.path.exists(os.path.join(self.run_dir, "seed0-datadir")))


class Workloads(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(dir=os.environ.get("NODE_BENCH_TEST_TMP"))
        self.saved = paths.ROOT
        paths.ROOT = self.tmp.name

    def tearDown(self):
        paths.ROOT = self.saved
        self.tmp.cleanup()

    def run_workload(self, fn):
        import argparse
        import contextlib
        import io
        from unittest import mock

        from nodebench import workloads

        args = argparse.Namespace(list=False, name="fake", impl="dashd", dashd_bin="/bin/true", rust_bin=None,
                                  rust_cli=None, wrap="perf stat --", json=os.path.join(self.tmp.name, "m.json"),
                                  keep=False, node_slice=None, aux_slice=None, out=os.path.join(self.tmp.name, "r"),
                                  call="x", ctl_fifo=None, **{k: None for k in workloads.DEFAULTS},
                                  seeds=None, dbcache=None)
        with mock.patch.dict(workloads.WORKLOADS, {"fake": (fn, "")}), contextlib.redirect_stdout(io.StringIO()):
            rc = workloads.cmd_workload(args)
        with open(args.json) as f:
            import json

            return rc, json.load(f)

    def test_failed_wrapper_is_not_success(self):
        def wrapper_died(ctx, a):  # what regtest_run/stop_at raise on a nonzero wrapper exit (perf: 129)
            ctx.manifest["exit_code"] = 129
            raise RuntimeError("the wrapper/node exited with 129 (counters may be missing or partial)")

        rc, manifest = self.run_workload(wrapper_died)
        self.assertEqual(rc, 1)  # was CLI exit 0, status ok
        self.assertEqual(manifest["status"], "error")
        self.assertEqual(manifest["exit_code"], 129)

    def test_markers_drive_the_control_fifo(self):
        import threading

        from nodebench import workloads

        d = self.tmp.name
        ctl, ack = os.path.join(d, "ctl"), os.path.join(d, "ack")
        os.mkfifo(ctl)
        os.mkfifo(ack)
        got = []

        def perf():  # stands in for perf stat --control fifo:ctl,ack
            fd = os.open(ctl, os.O_RDWR)
            with open(ack, "wb", buffering=0) as a:
                with os.fdopen(fd, "rb", buffering=0) as c:
                    for _ in range(2):
                        got.append(c.readline().strip())
                        a.write(b"ack\n")

        t = threading.Thread(target=perf)
        t.start()
        m = workloads.Markers(d, f"{ctl},{ack}")
        with m.work():
            pass
        m.close()
        t.join(10)
        self.assertEqual(got, [b"enable", b"disable"])
        self.assertEqual([e["event"] for e in m.events], ["begin", "end"])

    def test_disable_after_the_wrapper_exited(self):
        import threading

        from nodebench import workloads

        ctl = os.path.join(self.tmp.name, "ctl")
        os.mkfifo(ctl)

        def short_lived_perf():  # reads the enable, then exits like perf at the end of a whole-lifetime run
            with open(ctl, "rb", buffering=0) as c:
                c.readline()

        t = threading.Thread(target=short_lived_perf)
        t.start()
        m = workloads.Markers(self.tmp.name, ctl)
        m.mark("begin")
        t.join(10)
        m.mark("end")  # used to raise BrokenPipeError and hide the workload's real outcome
        m.close()


class MannWhitney(unittest.TestCase):
    def test_exact_values(self):
        self.assertAlmostEqual(stats.mann_whitney_greater([2, 3, 4, 5, 6], [1, 1.5, 1.7, 1.8, 1.9]), 1 / 252)
        self.assertAlmostEqual(stats.mann_whitney_greater([4, 5, 6], [1, 2, 3]), 1 / 20)
        self.assertEqual(stats.mann_whitney_greater([1, 1], [1, 1]), 1.0)

    def test_normal_approximation_agrees(self):
        import random

        r = random.Random(3)
        x = [r.gauss(10.6, 1) for _ in range(12)]
        y = [r.gauss(10, 1) for _ in range(12)]
        exact = stats.mann_whitney_greater(x, y, exact_limit=10**7)
        approx = stats.mann_whitney_greater(x, y, exact_limit=0)
        self.assertAlmostEqual(exact, approx, delta=0.01)


if __name__ == "__main__":
    unittest.main()
