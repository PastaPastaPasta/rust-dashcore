"""BL-313: re-anchoring across a fingerprint change when a counter fell to 0, and --reset-drift with a gate.

    python3 -m unittest discover -s contrib/node/perfgate/tests
"""

import json
import os
import shutil
import sys
import tempfile
import types
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.realpath(__file__))))

from perfgate import anchor, build, cli, measure  # noqa: E402

FP = {"shim": "s", "rustc": "r", "cargo": "c", "cc": "cc", "cargo_config": "x", "libc": "l", "kernel": "k",
      "cpu": "old cpu", "env": {}}
NEW_FP = dict(FP, cpu="new cpu")
TREE = "t" * 40


def wl(**samples):
    return {"status": "ok", "iters": 1, "kind": "bench", "samples": samples}


def meas(fp, **workloads):
    return {"meta": {"rev": "c" * 40, "fingerprint": fp}, "workloads": workloads}


class Bl313Test(unittest.TestCase):
    def setUp(self):
        self.root = tempfile.mkdtemp(dir=os.environ.get("PERFGATE_TEST_DIR"))
        self.addCleanup(shutil.rmtree, self.root, True)
        env = mock.patch.dict(os.environ, {"PERFGATE_ROOT": self.root})
        env.start()
        self.addCleanup(env.stop)
        # The old anchor, and the authoritative gate of TREE under the old fingerprint: page_faults fell 1 -> 0
        # (spv-storage-persist in T23), fs_syscalls rose from 0, instructions rose 1%, allocs halved.
        self.old = {"rev": "a" * 40, "time": "t", "fingerprint": FP,
                    "workloads": {"a": {"iters": 1, "kind": "bench",
                                        "medians": {"instructions": 1000, "page_faults": 1, "allocs": 1000,
                                                    "fs_syscalls": 0}}}}
        with open(anchor.path(), "w") as f:
            json.dump(self.old, f)
        report = os.path.join(self.root, "r")
        os.makedirs(report)
        with open(os.path.join(report, "head.json"), "w") as f:
            json.dump(meas(FP, a=wl(instructions=[1010] * 5, page_faults=[0] * 5, allocs=[500] * 5,
                                    fs_syscalls=[4] * 5)), f)
        with open(os.path.join(self.root, "ledger.jsonl"), "w") as f:
            f.write(json.dumps({"authoritative": True, "measured_tree": TREE, "anchor": self.old["rev"],
                                "report": report}) + "\n")

    def last_logged(self):
        with open(os.path.join(self.root, "anchors.jsonl")) as f:
            return json.loads(f.readlines()[-1])

    def test_a_counter_without_a_ratio_is_recorded_uncarried(self):
        carried = anchor.carried_drift(self.old, TREE)
        self.assertEqual((carried["a"]["page_faults"], carried["a"]["fs_syscalls"]), (-1, None))
        entry = anchor.record(meas(NEW_FP, a=wl(instructions=[2020] * 5, page_faults=[3] * 5, allocs=[300] * 5,
                                                fs_syscalls=[6] * 5)), "cpu change", TREE, {}, {}, {},
                              carried=carried)
        a = entry["workloads"]["a"]
        # the measured medians, not a division by zero (fell to 0) or a silent reset (rose from 0)
        self.assertEqual((a["medians"]["page_faults"], a["medians"]["fs_syscalls"]), (3, 6))
        self.assertAlmostEqual(a["medians"]["instructions"], 2000)  # the other counters are still carried,
        self.assertAlmostEqual(a["medians"]["allocs"], 600)  # an improvement short of -100% too
        self.assertEqual(a["uncarried"], {"page_faults": -1, "fs_syscalls": None})
        self.assertEqual(set(a["carried"]), {"instructions", "allocs"})
        logged = self.last_logged()
        self.assertEqual(logged["uncarried"], {"a": {"page_faults": -1, "fs_syscalls": None}})
        self.assertEqual(set(logged["carried"]["a"]), {"instructions", "allocs"})
        self.assertEqual(logged["reset"], {})

    def anchor_cmd(self, reset_drift, fp=NEW_FP):
        self.logs = []
        w = types.SimpleNamespace(name="a", kind="bench", key=lambda: "ka")
        args = types.SimpleNamespace(cmd="anchor", justification="cpu change", rev="c" * 40, dev_harness=False,
                                     no_cache=False, jobs=1, repeat=5, reset_drift=reset_drift)
        m = meas(fp, a=wl(instructions=[2020] * 5, page_faults=[3] * 5, allocs=[300] * 5, fs_syscalls=[6] * 5))
        self.measure_rev = mock.Mock(return_value=m)
        with mock.patch.object(cli, "selected", return_value=[w]), \
                mock.patch.object(cli, "selection_info",
                                  return_value={"selected": 1, "declared": 1, "default_file": True}), \
                mock.patch.object(build, "resolve", return_value="c" * 40), \
                mock.patch.object(build, "git", return_value=TREE), \
                mock.patch.object(cli, "check_harness", return_value=[]), \
                mock.patch.object(build, "node_bench_override", return_value=None), \
                mock.patch.object(cli, "workload_inputs", return_value={"a": {"key": "ka", "inputs": "i"}}), \
                mock.patch.object(measure, "summary", return_value=""), \
                mock.patch.object(cli, "log", self.logs.append), \
                mock.patch.object(measure, "measure_rev", self.measure_rev):
            cli.cmd_anchor(args)
        return anchor.load()["workloads"]["a"]

    def test_the_gate_carries_drift_without_reset_drift(self):
        a = self.anchor_cmd(reset_drift=False)
        self.assertAlmostEqual(a["medians"]["instructions"], 2000)
        self.assertAlmostEqual(a["medians"]["allocs"], 600)
        self.assertEqual((a["medians"]["page_faults"], a["medians"]["fs_syscalls"]), (3, 6))
        self.assertIn("  not carried (no ratio, so the measured median is anchored) a: fs_syscalls rose from 0, "
                      "page_faults -100.000%", self.logs)

    def test_reset_drift_applies_even_when_a_gate_of_the_tree_exists(self):
        a = self.anchor_cmd(reset_drift=True)
        self.assertEqual(a["medians"], {"instructions": 2020, "page_faults": 3, "allocs": 300, "fs_syscalls": 6})
        self.assertEqual((a["carried"], "uncarried" in a), ({}, False))
        logged = self.last_logged()
        self.assertEqual((logged["carried"], logged["uncarried"]), ({}, {}))
        # the drift the gate would have carried is discarded on the record, not silently (N1)
        self.assertEqual(set(logged["reset"]["a"]), {"instructions", "page_faults", "allocs", "fs_syscalls"})
        self.assertAlmostEqual(logged["reset"]["a"]["instructions"], 0.01)
        self.assertIsNone(logged["reset"]["a"]["fs_syscalls"])
        self.assertIn("  discarded drift (--reset-drift) a: allocs -50.000%, fs_syscalls rose from 0, "
                      "instructions +1.000%, page_faults -100.000%", self.logs)

    def test_reset_drift_is_refused_without_a_fingerprint_change(self):
        with self.assertRaisesRegex(SystemExit, "--reset-drift only applies across a fingerprint change, and the "
                                                "anchor's fingerprint is comparable"):
            self.anchor_cmd(reset_drift=True, fp=FP)
        self.assertEqual(anchor.load()["rev"], self.old["rev"])  # nothing recorded

    def test_reset_drift_without_an_anchor_is_refused_before_measuring(self):
        os.remove(anchor.path())
        with self.assertRaisesRegex(SystemExit, "there is no anchor yet"):
            self.anchor_cmd(reset_drift=True)
        self.measure_rev.assert_not_called()


if __name__ == "__main__":
    unittest.main()
