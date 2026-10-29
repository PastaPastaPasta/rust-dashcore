"""Tests for the PF0 review r3 findings (reviews/PF0-r3-opus.md, DEC-76).

    python3 -m unittest discover -s contrib/node/perfgate/tests
"""

import os
import shutil
import sys
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.realpath(__file__))))

from perfgate import build, gate, measure, sandbox, spec  # noqa: E402
from test_review_r1 import GitRepoTest  # noqa: E402


class FsSyscallsTest(unittest.TestCase):
    """Approved deviation: the storage benches' filesystem syscalls are gated, introspection excluded."""

    def test_fs_syscalls(self):
        log = [
            '9 access("/perfgate/begin", F_OK) = -1 ENOENT',
            '9 openat(AT_FDCWD, "/sys/devices/system/cpu/online", O_RDONLY|O_CLOEXEC) = 3',
            '9 openat(AT_FDCWD, "/proc/self/cgroup", O_RDONLY|O_CLOEXEC) = 3',
            '9 openat(AT_FDCWD, "/t/.segment_000000.dat.tmp", O_WRONLY|O_CREAT|O_TRUNC|O_CLOEXEC, 0666) = 3',
            '9 statx(AT_FDCWD, "/t/block_headers", AT_STATX_SYNC_AS_STAT, STATX_ALL, {...}) = 0',
            '9 mkdir("/t/block_headers", 0777) = -1 EEXIST (File exists)',
            '10 fsync(3 <unfinished ...>',
            '9 futex(0x7f, FUTEX_WAKE_PRIVATE, 1) = 0',
            '10 <... fsync resumed>) = 0',
            '9 rename("/t/.segment_000000.dat.tmp", "/t/segment_000000.dat") = 0',
            '9 unlinkat(AT_FDCWD, "/t/stale", 0) = 0',
            '9 write(3, "x", 1) = 1',
            '9 access("/perfgate/end", F_OK) = -1 ENOENT',
            '9 openat(AT_FDCWD, "/t/after", O_RDONLY) = 4',
        ]
        counts, names, spans = measure.strace_region(log)
        self.assertEqual(counts, {"syscalls": 9, "sched_syscalls": 1, "fs_syscalls": 6})
        self.assertEqual((names["openat"], spans), (3, 1))

    def test_gated_on_the_store_bench(self):
        self.assertTrue(gate.BY_KEY["fs_syscalls"].gated)
        wls = {w.name: w for w in spec.load()}
        store = wls["spv-storage-store"]
        self.assertTrue(gate.is_gated(gate.BY_KEY["fs_syscalls"], "bench", store.ungated))
        self.assertFalse(gate.is_gated(gate.BY_KEY["syscalls"], "bench", store.ungated))
        self.assertIn("spv-storage-persist", wls)


class ProcMacroBoundaryTest(GitRepoTest):
    """F3: a proc macro in the bench's closure runs at compile time, so its sources are bench inputs."""

    def test_proc_macro_sources_are_bench_inputs(self):
        root = '[workspace]\nmembers = ["p", "gs"]\nresolver = "2"\n'
        manifest = ('[package]\nname = "p"\nversion = "0.1.0"\nedition = "2021"\n\n[dependencies]\n'
                    'gs = { path = "../gs" }\n\n[[bench]]\nname = "b"\nharness = false\n')
        root = root.replace('"gs"]', '"gs", "gs-impl"]')
        # A crate-root lib.rs with a sibling module, and a helper crate the macro alone uses.
        macro = ('[package]\nname = "gs"\nversion = "0.1.0"\nedition = "2021"\n\n[lib]\nproc-macro = true\n'
                 'path = "lib.rs"\n\n[dependencies]\ngs-impl = { path = "../gs-impl" }\n')
        helper = '[package]\nname = "gs-impl"\nversion = "0.1.0"\nedition = "2021"\n'
        base = self.commit("base", {"Cargo.toml": root, "p/Cargo.toml": manifest, "p/src/lib.rs": "",
                                    "p/benches/b.rs": "fn main() {}\n", "gs/Cargo.toml": macro,
                                    "gs/lib.rs": "mod expand;\n", "gs/expand.rs": "// expands\n",
                                    "gs-impl/Cargo.toml": helper, "gs-impl/src/lib.rs": "// helper\n"})
        wt = os.path.join(self.repo, "..", os.path.basename(self.repo) + "-wt-pm")
        self.git("worktree", "add", "-q", "--detach", wt, base)
        self.addCleanup(shutil.rmtree, wt, ignore_errors=True)
        with mock.patch.object(build.paths, "REPO", self.repo):
            build_paths = build.bench_boundary(wt, {"p"})["p"]["build"]
            self.assertTrue({"gs", "gs-impl"} <= set(build_paths), build_paths)
            self.assertNotIn("p", build_paths)  # the bench's own library is code under test, not a bench input
            edit = self.commit("macro writes the bench", {"gs/expand.rs": "// rewrites p/benches/b.rs\n"})
            changed, _m, _s = build.bench_changes(base, edit, wt, {"p"}, cwd=self.repo)
            self.assertEqual(changed, ["gs/expand.rs"])
            helper_edit = self.commit("helper", {"gs-impl/src/lib.rs": "// rewrites p/benches/b.rs\n"})
            changed, _m, _s = build.bench_changes(edit, helper_edit, wt, {"p"}, cwd=self.repo)
            self.assertEqual(changed, ["gs-impl/src/lib.rs"])


class BenchGlueBoundaryTest(GitRepoTest):
    """BL-426 (DEC-514): bench glue in a crate's src/ (`bench.rs`, `bench/`) is a bench input, in the bench's own
    package and in the crates it depends on."""

    def test_bench_glue_edits_are_bench_input_changes(self):
        root = '[workspace]\nmembers = ["p", "q"]\nresolver = "2"\n'
        manifest = ('[package]\nname = "p"\nversion = "0.1.0"\nedition = "2021"\n\n[dependencies]\n'
                    'q = { path = "../q" }\n\n[[bench]]\nname = "b"\nharness = false\n')
        dep = '[package]\nname = "q"\nversion = "0.1.0"\nedition = "2021"\n'
        base = self.commit("base", {"Cargo.toml": root, "p/Cargo.toml": manifest,
                                    "p/src/lib.rs": "pub mod connect;\n", "p/src/connect.rs": "pub mod bench;\n",
                                    "p/src/connect/bench.rs": "// NoEvoSteps\n", "p/src/serial.rs": "// product\n",
                                    "p/benches/b.rs": "fn main() {}\n", "q/Cargo.toml": dep,
                                    "q/src/lib.rs": "pub mod bench;\n", "q/src/bench/mod.rs": "// glue\n"})
        wt = os.path.join(self.repo, "..", os.path.basename(self.repo) + "-wt-glue")
        self.git("worktree", "add", "-q", "--detach", wt, base)
        self.addCleanup(shutil.rmtree, wt, ignore_errors=True)
        with mock.patch.object(build.paths, "REPO", self.repo):
            build_paths = build.bench_boundary(wt, {"p"})["p"]["build"]
            self.assertTrue({"p/src/connect/bench.rs", "q/src/bench"} <= set(build_paths), build_paths)
            self.assertNotIn("p/src/serial.rs", build_paths)  # the product stays code under test
            glue = self.commit("glue", {"p/src/connect/bench.rs": "// NoEvoSteps, changed\n"})
            changed, _m, _s = build.bench_changes(base, glue, wt, {"p"}, cwd=self.repo)
            self.assertEqual(changed, ["p/src/connect/bench.rs"])
            self.assertNotEqual(build.bench_inputs(base, wt, {"p"}, cwd=self.repo),
                                build.bench_inputs(glue, wt, {"p"}, cwd=self.repo))
            dep_glue = self.commit("dep glue", {"q/src/bench/mod.rs": "// glue, changed\n"})
            changed, _m, _s = build.bench_changes(glue, dep_glue, wt, {"p"}, cwd=self.repo)
            self.assertEqual(changed, ["q/src/bench/mod.rs"])
            product = self.commit("product", {"p/src/serial.rs": "// faster\n"})
            changed, _m, _s = build.bench_changes(dep_glue, product, wt, {"p"}, cwd=self.repo)
            self.assertEqual(changed, [])
            # Glue only the head has is found in the head's tree (head_wt), as the gate passes it.
            added = self.commit("new glue", {"q/src/disconnect/bench.rs": "// new glue\n"})
            head_wt = os.path.join(self.repo, "..", os.path.basename(self.repo) + "-wt-glue-head")
            self.git("worktree", "add", "-q", "--detach", head_wt, added)
            self.addCleanup(shutil.rmtree, head_wt, ignore_errors=True)
            changed, _m, _s = build.bench_changes(product, added, wt, {"p"}, cwd=self.repo, head_wt=head_wt)
            self.assertEqual(changed, ["q/src/disconnect/bench.rs"])

    def test_the_tree_s_bench_glue(self):
        """Inventory, not behaviour: the repository's glue today is dashd-chainstate's connect/bench.rs alone. A
        new bench.rs or bench/ in a crate's src/ fails this on purpose, so its bench rows' re-anchor is planned."""
        repo = os.path.realpath(build.paths.REPO)
        glue = set()
        for crate in sorted(os.listdir(repo)):
            if os.path.isfile(os.path.join(repo, crate, "Cargo.toml")):
                glue |= build.bench_glue(repo, os.path.join(repo, crate))
        self.assertEqual(glue, {"dashd-chainstate/src/connect/bench.rs"})


class HomeBindTest(unittest.TestCase):
    """Review r3 finding 3: a bind-back path containing the home directory would undo its tmpfs."""

    def test_home_ancestor_bind_is_refused(self):
        home = os.path.realpath(os.path.expanduser("~"))
        with mock.patch.object(sandbox.paths, "REPO", os.path.dirname(home)):
            with self.assertRaisesRegex(sandbox.SandboxError, "contains the home directory"):
                sandbox.readonly_binds()


if __name__ == "__main__":
    unittest.main()
