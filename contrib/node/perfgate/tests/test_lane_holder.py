"""The lane holder record (DEC-707 c, BL-513): paths.locked() names its holder in .lock.holder while it holds the
lock, removes only its own record, and a holder killed before its cleanup leaves a record lane_holder() calls stale.

    python3 -m unittest discover -s contrib/node/perfgate/tests -p test_lane_holder.py
"""

import fcntl
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

HERE = os.path.dirname(os.path.realpath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

from perfgate import paths  # noqa: E402

# A child that takes the lock, says so, then waits to be killed.
HOLD = f"""
import sys, time
sys.path.insert(0, {os.path.dirname(HERE)!r})
from perfgate import paths
with paths.locked():
    print("held", flush=True)
    time.sleep(600)
"""


def dead_pid():
    p = subprocess.Popen(["true"])
    p.wait()
    return p.pid


class LaneHolder(unittest.TestCase):
    def setUp(self):
        self.root = tempfile.mkdtemp(prefix="holder-", dir=os.environ.get("PERFGATE_TEST_DIR"))
        os.chmod(self.root, 0o700)
        self.addCleanup(shutil.rmtree, self.root, True)
        env = mock.patch.dict(os.environ, {"PERFGATE_ROOT": self.root})
        env.start()
        self.addCleanup(env.stop)
        self.record = os.path.join(self.root, ".lock.holder")

    def lock_is_free(self):
        with open(os.path.join(self.root, ".lock"), "w") as f:
            try:
                fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                return False
            fcntl.flock(f, fcntl.LOCK_UN)
            return True

    def test_record_names_the_holder_while_held(self):
        self.assertIsNone(paths.lane_holder())
        with paths.locked():
            with open(self.record) as f:
                rec = json.load(f)
            self.assertEqual(rec["pid"], os.getpid())
            self.assertEqual(rec["argv"], sys.argv)
            self.assertRegex(rec["start"], r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$")
            self.assertTrue(paths.lane_holder()["alive"])
            self.assertFalse(os.path.exists(self.record + ".tmp"))

    def test_release_removes_the_record(self):
        with paths.locked():
            pass
        self.assertFalse(os.path.exists(self.record))
        self.assertIsNone(paths.lane_holder())

    def test_release_on_error_removes_the_record_and_frees_the_lock(self):
        with self.assertRaises(RuntimeError):
            with paths.locked():
                raise RuntimeError
        self.assertFalse(os.path.exists(self.record))
        self.assertTrue(self.lock_is_free())

    def test_release_keeps_a_later_holders_record(self):
        other = {"pid": dead_pid(), "argv": ["other"], "start": "2026-10-29T00:00:00Z"}
        with paths.locked():
            with open(self.record, "w") as f:
                json.dump(other, f)
        with open(self.record) as f:
            self.assertEqual(json.load(f), other)

    def test_a_failed_record_write_still_runs_and_releases(self):
        ran = []
        with mock.patch.object(paths.os, "replace", side_effect=OSError(28, "No space left on device")):
            with paths.locked():
                ran.append(self.lock_is_free())
        self.assertEqual(ran, [False])
        self.assertTrue(self.lock_is_free())
        self.assertFalse(os.path.exists(self.record + ".tmp"))
        self.assertIsNone(paths.lane_holder())

    def test_an_unreadable_own_proc_entry_still_runs(self):
        ran = []
        with mock.patch.object(paths, "_starttime", side_effect=PermissionError(13, "hidepid")):
            with paths.locked():
                ran.append(True)
        self.assertEqual(ran, [True])
        self.assertTrue(self.lock_is_free())
        self.assertIsNone(paths.lane_holder())

    def write(self, rec):
        with open(self.record, "w") as f:
            json.dump(rec, f)

    def test_non_object_records(self):
        for rec in ([1], 5, "x", None):
            with paths.locked():  # a release that finds one leaves it, and doesn't raise
                self.write(rec)
            self.assertIsNone(paths.lane_holder())
            self.assertTrue(os.path.exists(self.record))

    def test_pids_that_name_no_single_process(self):
        me = paths._starttime(os.getpid())
        for pid in (0, -1, True, "1", "self", None):
            self.write({"pid": pid, "starttime": me})
            self.assertFalse(paths.lane_holder()["alive"], pid)

    def test_reused_pid_is_stale(self):
        self.write({"pid": os.getpid(), "starttime": paths._starttime(os.getpid()) - 1})
        self.assertFalse(paths.lane_holder()["alive"])

    def test_dead_pid_is_stale(self):
        with open(self.record, "w") as f:
            json.dump({"pid": dead_pid(), "argv": ["x"], "start": "2026-10-29T00:00:00Z"}, f)
        self.assertFalse(paths.lane_holder()["alive"])

    def test_malformed_record(self):
        with open(self.record, "w") as f:
            f.write("{not json")
        self.assertIsNone(paths.lane_holder())
        with open(self.record, "w") as f:
            json.dump({"argv": ["no pid"]}, f)
        self.assertFalse(paths.lane_holder()["alive"])

    def test_second_process_blocks_and_a_killed_holder_leaves_a_stale_record(self):
        child = subprocess.Popen([sys.executable, "-c", HOLD], stdout=subprocess.PIPE, text=True)
        self.addCleanup(child.stdout.close)
        self.addCleanup(child.wait)
        self.addCleanup(child.kill)
        self.assertEqual(child.stdout.readline().strip(), "held")
        rec = paths.lane_holder()
        self.assertEqual(rec["pid"], child.pid)
        self.assertTrue(rec["alive"])
        self.assertFalse(self.lock_is_free())  # the record is informational; the flock is what excludes
        child.send_signal(signal.SIGKILL)
        child.wait()
        self.assertTrue(self.lock_is_free())
        rec = paths.lane_holder()
        self.assertEqual(rec["pid"], child.pid)
        self.assertFalse(rec["alive"])
        with paths.locked():  # the next holder replaces the stale record, then removes its own
            self.assertEqual(paths.lane_holder()["pid"], os.getpid())
        self.assertIsNone(paths.lane_holder())


if __name__ == "__main__":
    unittest.main()
