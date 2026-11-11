"""Tests for PF0-N (BL-128): what the code under test can reach of the counters' outputs.

    python3 -m unittest discover -s contrib/node/perfgate/tests
"""

import contextlib
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.realpath(__file__))))

from perfgate import gate, measure, sandbox, spec  # noqa: E402

# A bench that reopens every inherited descriptor but stdout and stderr (the gate's pipes) through /proc/self/fd
# and appends a forged perf line, padded so that it outlives the real line perf writes at its own offset on exit
# (F0), then reports like the shim.
FORGER = """#!/usr/bin/python3
import json, os
for fd in os.listdir("/proc/self/fd"):
    if int(fd) not in (1, 2):
        try:
            with open("/proc/self/fd/" + fd, "a") as f:
                f.write("0" * 4096 + "\\n7,,instructions:u,1,100.00,,\\n")
        except OSError:
            pass
res = dict.fromkeys(%r, 0)
res.update(time_running=1, time_enabled=1)
with open(os.environ["PERFGATE_OUT"], "w") as f:
    json.dump(res, f)
""" % (measure.COUNTER_KEYS,)


def perf_counts():
    if not shutil.which("perf"):
        return False
    return subprocess.run(["perf", "stat", "-e", "instructions:u", "true"], capture_output=True).returncode == 0


@unittest.skipUnless(sandbox.available() and perf_counts() and shutil.which("cc"), "needs bwrap, perf and cc")
class CounterOutputTest(unittest.TestCase):
    """F0: `perf stat -o FILE` leaves FILE open in the process it starts; through /proc/self/fd the code under
    test could rewrite the authoritative `instructions_process`, so nothing but stdio may pass the counter."""

    def run_forger(self, stdin_closed=False):
        with tempfile.TemporaryDirectory(dir=os.environ.get("PERFGATE_TEST_DIR", os.path.expanduser("~"))) as root, \
                mock.patch.dict(os.environ, {"PERFGATE_ROOT": root}):
            exe = os.path.join(root, "forger")
            with open(exe, "w") as f:
                f.write(FORGER)
            os.chmod(exe, 0o555)
            if not stdin_closed:
                wl = spec.Workload(name="forger", kind="bench", package="p", target="t", id="forge/id")
                return measure.run_bench(exe, root, wl, 1, "counters", measure.cpu_slots(1)[0])
            # A gate started with fd 0 closed: perf's -o file would open as fd 0.
            code = ("import sys; sys.path.insert(0, %r); from perfgate import measure, spec; "
                    "wl = spec.Workload(name='forger', kind='bench', package='p', target='t', id='forge/id'); "
                    "print(measure.run_bench(%r, %r, wl, 1, 'counters', measure.cpu_slots(1)[0])"
                    "['instructions_process'])") % (os.path.dirname(os.path.dirname(os.path.realpath(__file__))),
                                                     exe, root)
            p = subprocess.run([sys.executable, "-c", code], capture_output=True, text=True,
                               preexec_fn=lambda: os.close(0))
            self.assertEqual(p.returncode, 0, p.stderr)
            return {"instructions_process": int(p.stdout.split()[-1])}

    def test_the_bench_cannot_rewrite_the_process_count(self):
        # A Python process runs tens of millions of instructions; the forged line says 7.
        self.assertGreater(self.run_forger()["instructions_process"], 1_000_000)

    def test_a_gate_with_stdin_closed_is_not_forged_either(self):
        self.assertGreater(self.run_forger(stdin_closed=True)["instructions_process"], 1_000_000)

    def test_without_the_launcher_the_forgery_lands(self):
        # The control: the same run with the counter's descriptors inherited reads the forged count, so the test
        # above fails if the launcher stops closing them.
        with mock.patch.object(sandbox, "counted", lambda counter, argv: [*counter, "--", *argv]):
            self.assertEqual(self.run_forger()["instructions_process"], 7)

    def test_launcher_passes_only_stdio(self):
        with tempfile.TemporaryDirectory(dir=os.environ.get("PERFGATE_TEST_DIR", os.path.expanduser("~"))) as root, \
                mock.patch.dict(os.environ, {"PERFGATE_ROOT": root}):
            out = os.path.join(root, "perf.csv")
            # The descriptors still open once listdir's own is closed again.
            probe = "import os; print(*sorted(f for f in os.listdir('/proc/self/fd') if os.path.exists('/proc/self/fd/' + f)))"
            argv = sandbox.counted(["perf", "stat", "-x,", "-e", "instructions:u", "-o", out],
                                   sandbox.wrap(["python3", "-c", probe], env={"PATH": "/usr/bin"}))
            p = subprocess.run(argv, capture_output=True, text=True, stdin=subprocess.DEVNULL)
            self.assertEqual(p.returncode, 0, p.stderr)
            self.assertEqual(p.stdout.split(), ["0", "1", "2"])
            # perf still wrote its count.
            with open(out) as f:
                self.assertIn("instructions:u", f.read())

    def test_launcher_empties_a_closed_stdin(self):
        # Without the gate's own stdin=DEVNULL: the launcher alone keeps perf's file (opened as fd 0) out.
        with tempfile.TemporaryDirectory(dir=os.environ.get("PERFGATE_TEST_DIR", os.path.expanduser("~"))) as root, \
                mock.patch.dict(os.environ, {"PERFGATE_ROOT": root}):
            out = os.path.join(root, "perf.csv")
            probe = "import os; print(os.readlink('/proc/self/fd/0'))"
            argv = sandbox.counted(["perf", "stat", "-x,", "-e", "instructions:u", "-o", out],
                                   sandbox.wrap(["python3", "-c", probe], env={"PATH": "/usr/bin"}))
            p = subprocess.run(argv, capture_output=True, text=True, preexec_fn=lambda: os.close(0))
            self.assertEqual(p.returncode, 0, p.stderr)
            self.assertEqual(p.stdout.strip(), "/dev/null")

    def test_both_counters_go_through_the_launcher(self):
        with tempfile.TemporaryDirectory(dir=os.environ.get("PERFGATE_TEST_DIR", os.path.expanduser("~"))) as root, \
                mock.patch.dict(os.environ, {"PERFGATE_ROOT": root}):
            wl = spec.Workload(name="w", kind="bench", package="p", target="t", id="i")
            for mode, counter in (("counters", "perf"), ("syscalls", "strace")):
                done = subprocess.CompletedProcess([], 1, "", "stop here")
                with mock.patch.object(measure.subprocess, "run", return_value=done) as run, \
                        self.assertRaises(measure.RunError):
                    measure.run_bench("/bin/true", root, wl, 1, mode, 0)
                argv = run.call_args.args[0]
                i = argv.index(sandbox.launcher())
                self.assertEqual((argv[0], argv[i - 1], argv[i + 1:i + 4]), (counter, "--", ["--exec", "--", "bwrap"]))
                self.assertEqual(run.call_args.kwargs["stdin"], subprocess.DEVNULL)

    def test_concurrent_first_compiles_agree(self):
        with tempfile.TemporaryDirectory(dir=os.environ.get("PERFGATE_TEST_DIR", os.path.expanduser("~"))) as root, \
                mock.patch.dict(os.environ, {"PERFGATE_ROOT": root}):
            got, errors = [], []

            def one():
                try:
                    got.append(sandbox.launcher())
                except Exception as e:  # noqa: BLE001 - any failure is the finding
                    errors.append(e)

            threads = [threading.Thread(target=one) for _ in range(8)]
            for t in threads:
                t.start()
            for t in threads:
                t.join()
            self.assertEqual(errors, [])
            self.assertEqual(len(set(got)), 1)
            self.assertEqual(os.listdir(os.path.dirname(got[0])), [os.path.basename(got[0])])  # no temporaries

    def test_launcher_refuses_a_bad_command_line(self):
        with tempfile.TemporaryDirectory(dir=os.environ.get("PERFGATE_TEST_DIR", os.path.expanduser("~"))) as root, \
                mock.patch.dict(os.environ, {"PERFGATE_ROOT": root}):
            for argv in ([], ["true"], ["--exec", "true"], ["--exec", "--"]):
                p = subprocess.run([sandbox.launcher(), *argv], capture_output=True, text=True)
                self.assertEqual(p.returncode, 125, argv)


class CounterFingerprintTest(unittest.TestCase):
    """The counting path moves `instructions_process` (the launcher, bwrap, taskset and setarch run under perf)
    and the strace counts, but is not part of the shim, so it has its own fingerprint field, compared for the
    cache and the anchor alike."""

    def setUp(self):
        measure.counter_hash.cache_clear()
        self.addCleanup(measure.counter_hash.cache_clear)
        self.base = measure.counter_hash()

    def changed(self, *patchers):
        """counter_hash with `patchers` applied, re-reading the sources as an import would."""
        with contextlib.ExitStack() as stack:
            for p in patchers:
                stack.enter_context(p)
            stack.enter_context(mock.patch.object(measure, "_COUNTING_SOURCES", measure._counting_sources()))
            measure.counter_hash.cache_clear()
            return measure.counter_hash()

    def test_it_is_stable(self):
        self.assertEqual(self.changed(), self.base)

    def test_changing_the_launcher_flips_it(self):
        with tempfile.TemporaryDirectory(dir=os.environ.get("PERFGATE_TEST_DIR")) as d:
            src = os.path.join(d, "launch.c")
            with open(sandbox.LAUNCH_SRC) as f, open(src, "w") as g:
                g.write(f.read().replace("close_range(3, ~0U, 0)", "close_range(4, ~0U, 0)"))
            self.assertNotEqual(self.changed(mock.patch.object(sandbox, "LAUNCH_SRC", src)), self.base)

    def test_changing_a_counting_function_flips_it(self):
        binds = sandbox.readonly_binds()

        def stub(*a, **kw):  # another source; as readonly_binds, the same number of binds
            return binds

        for module, name in ((measure, "bench_argv"), (measure.node, "parse_perf_csv"), (measure, "strace_region"), (measure, "split_sched"),
                             (sandbox, "wrap"), (sandbox, "counted"), (sandbox, "readonly_binds")):
            with self.subTest(name):
                self.assertNotEqual(self.changed(mock.patch.object(module, name, stub)), self.base)

    def test_changing_a_constant_flips_it(self):
        for name, value in (("PERF_COUNTER", (*measure.PERF_COUNTER[:-1], "instructions")),
                            ("STRACE_COUNTER", ("strace", "-f")),
                            ("FS_SYSCALLS", measure.FS_SYSCALLS | {"statx2"}),
                            ("SCHED_SYSCALLS", measure.SCHED_SYSCALLS - {"futex"}),
                            ("MARKER", measure.re.compile(measure.MARKER.pattern + "x")),
                            ("SYSCALL_LINE", measure.re.compile(measure.SYSCALL_LINE.pattern + "x")),
                            ("INTROSPECTION", measure.re.compile(measure.INTROSPECTION.pattern + "x"))):
            with self.subTest(name):
                self.assertNotEqual(self.changed(mock.patch.object(measure, name, value)), self.base)
        self.assertNotEqual(self.changed(mock.patch.object(sandbox, "CC_FLAGS", ("-O0",))), self.base)

    def test_one_more_sandbox_bind_flips_it(self):
        # Each bind costs bwrap ~355k instructions; the sources (read at import) stay as they were.
        real = sandbox.readonly_binds()
        with mock.patch.object(sandbox, "readonly_binds", lambda: [*real, "/x"]):
            measure.counter_hash.cache_clear()
            self.assertNotEqual(measure.counter_hash(), self.base)

    def test_a_tool_upgrade_flips_it(self):
        real = subprocess.run
        for tool in ("perf", "strace", "bwrap", "taskset"):
            def run(cmd, *a, tool=tool, **kw):
                if tuple(cmd[:1]) == (tool,) and len(cmd) == 2:
                    return subprocess.CompletedProcess(cmd, 0, f"{tool} version 99\n", "")
                return real(cmd, *a, **kw)

            with self.subTest(tool):
                self.assertNotEqual(self.changed(mock.patch.object(measure.subprocess, "run", run)), self.base)

    def test_a_missing_or_silent_tool_is_none(self):
        self.assertIsNone(measure._version("perfgate-no-such-tool", "--version"))
        self.assertIsNone(measure._version("true"))
        if shutil.which("perf"):
            self.assertTrue(measure._version("perf", "--version"))

    def test_the_bench_runs_use_the_hashed_command_lines(self):
        seen, real = [], subprocess.run

        def run(argv, *a, **kw):
            if "/launch" not in argv:  # the sandbox's own setup (git and the like)
                return real(argv, *a, **kw)
            seen.append(argv)
            raise subprocess.TimeoutExpired(argv, 0)

        wl = spec.Workload(name="w", kind="bench", package="p", target="t", id="w/id")
        with tempfile.TemporaryDirectory(dir=os.environ.get("PERFGATE_TEST_DIR")) as root, \
                mock.patch.dict(os.environ, {"PERFGATE_ROOT": root}), \
                mock.patch.object(sandbox, "launcher", lambda: "/launch"), \
                mock.patch.object(measure.subprocess, "run", run):
            for mode in ("counters", "syscalls"):
                with self.assertRaises(measure.RunError):
                    measure.run_bench("/exe", root, wl, 1, mode, 0)
        perf, strace = seen
        self.assertEqual(tuple(perf[:len(measure.PERF_COUNTER)]), measure.PERF_COUNTER)
        self.assertEqual(tuple(strace[:len(measure.STRACE_COUNTER)]), measure.STRACE_COUNTER)

    def test_another_counting_path_is_incomparable(self):
        fp = {k: "same" for k in set(gate.ANCHOR_COMPARABLE) | set(gate.COMPARABLE)}
        other = dict(fp, counter="other")
        anchor = {"rev": "a" * 40, "fingerprint": fp, "workloads": {}}
        rows = gate.drift_rows([], {"meta": {"fingerprint": other}, "workloads": {}}, anchor)
        self.assertEqual([r.status for r in rows], ["ERROR"])
        self.assertIn("counter differs", rows[0].note)
        rows = gate.compare({"meta": {"fingerprint": fp}, "workloads": {}},
                            {"meta": {"fingerprint": other}, "workloads": {}})
        self.assertEqual([(r.workload, r.status) for r in rows], [("(sides)", "ERROR")])
        self.assertIn("counter differs", rows[0].note)

    def test_the_measurement_fingerprint_carries_it(self):
        with tempfile.TemporaryDirectory(dir=os.environ.get("PERFGATE_TEST_DIR")) as wt:
            open(os.path.join(wt, "Cargo.lock"), "w").close()
            with mock.patch.object(measure.build, "toolchain", lambda wt: {}), \
                    mock.patch.object(sandbox, "label", lambda: None):
                self.assertEqual(measure.fingerprint(wt)["counter"], self.base)


@unittest.skipUnless(shutil.which("cc"), "needs cc")
class CompiledNameTest(unittest.TestCase):
    """A helper is rebuilt when anything it is built with changes, not only its source: otherwise a flag or
    compiler change flips the fingerprint while the old binary keeps running until `clean`."""

    def test_flags_and_compiler_select_the_binary(self):
        with tempfile.TemporaryDirectory(dir=os.environ.get("PERFGATE_TEST_DIR")) as root:
            src = os.path.join(root, "t.c")
            with open(src, "w") as f:
                f.write("int main(void) { return 0; }\n")

            def build(flags=(), cc_flags=sandbox.CC_FLAGS, cc_version="cc 1\n"):
                real = subprocess.run

                def run(cmd, *a, **kw):
                    if cmd == ["cc", "--version"]:
                        return subprocess.CompletedProcess(cmd, 0, cc_version, "")
                    return real(cmd, *a, **kw)

                with mock.patch.dict(sandbox._COMPILED, clear=True), \
                        mock.patch.object(sandbox, "CC_FLAGS", cc_flags), \
                        mock.patch.object(sandbox.subprocess, "run", run):
                    return sandbox._compiled(src, "t", flags, (), root)

            base = build()
            self.assertEqual(build(), base)
            self.assertEqual(len({base, build(("-g",)), build(cc_flags=("-O1",)), build(cc_version="cc 2\n")}), 4)


class HostFingerprintTest(unittest.TestCase):
    """PF0-N 1c (D38, D68): the online CPU count and the sandbox's security label are host facts that move the
    counts without touching a source, so both are compared for the cache and the anchor."""

    def test_both_fields_are_compared(self):
        for field in ("ncpu", "sandbox_label"):
            self.assertIn(field, gate.COMPARABLE)
            self.assertIn(field, gate.ANCHOR_COMPARABLE)

    def test_a_cpu_resize_makes_the_anchor_incomparable(self):
        fp = {k: "same" for k in set(gate.ANCHOR_COMPARABLE) | set(gate.COMPARABLE)}
        with tempfile.TemporaryDirectory(dir=os.environ.get("PERFGATE_TEST_DIR")) as wt:
            open(os.path.join(wt, "Cargo.lock"), "w").close()
            with mock.patch.object(measure.build, "toolchain", lambda wt: {}), \
                    mock.patch.object(sandbox, "label", lambda: "same"):
                with mock.patch.object(measure.os, "sysconf", lambda name: 32):
                    now = measure.fingerprint(wt)
                with mock.patch.object(measure.os, "sysconf", lambda name: 24):
                    resized = measure.fingerprint(wt)
        self.assertEqual(now["ncpu"], 32)
        self.assertNotEqual(now, resized)
        anchor = {"rev": "a" * 40, "fingerprint": dict(fp, ncpu=now["ncpu"]), "workloads": {}}
        rows = gate.drift_rows([], {"meta": {"fingerprint": dict(fp, ncpu=resized["ncpu"])}, "workloads": {}},
                               anchor)
        self.assertEqual([r.status for r in rows], ["ERROR"])
        self.assertIn("ncpu differs", rows[0].note)

    def test_another_sandbox_label_is_incomparable(self):
        fp = {k: "same" for k in set(gate.ANCHOR_COMPARABLE) | set(gate.COMPARABLE)}
        rows = gate.compare({"meta": {"fingerprint": fp}, "workloads": {}},
                            {"meta": {"fingerprint": dict(fp, sandbox_label="unconfined")}, "workloads": {}})
        self.assertEqual([(r.workload, r.status) for r in rows], [("(sides)", "ERROR")])
        self.assertIn("sandbox_label differs", rows[0].note)

    def test_host_meta_records_both(self):
        with mock.patch.object(sandbox, "label", lambda: "bwrap//&unpriv_bwrap (enforce)"):
            meta = measure.host_meta()
        self.assertEqual(meta["ncpu"], os.sysconf("SC_NPROCESSORS_ONLN"))
        self.assertEqual(meta["sandbox_label"], "bwrap//&unpriv_bwrap (enforce)")

    @unittest.skipUnless(sandbox.available(), "needs bwrap")
    def test_the_label_is_read_inside_the_sandbox(self):
        with tempfile.TemporaryDirectory(dir=os.environ.get("PERFGATE_TEST_DIR", os.path.expanduser("~"))) as root, \
                mock.patch.dict(os.environ, {"PERFGATE_ROOT": root}):
            sandbox.label.cache_clear()
            self.addCleanup(sandbox.label.cache_clear)
            got = sandbox.label()
        attr = "/proc/self/attr/current"
        if os.path.exists("/sys/kernel/security/apparmor") and os.path.exists(attr):
            self.assertTrue(got)
            with open(attr) as f:
                outside = f.read().strip()
            # The exec of bwrap moves into its own profile where the policy has one; never "less" than outside.
            self.assertIsInstance(got, str)
            if "bwrap" in got:
                self.assertNotEqual(got, outside)
