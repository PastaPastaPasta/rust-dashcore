#!/usr/bin/env python3
"""Tests of selftest-rocksdb.py (BL-133): the [profile] tables it gives the throwaway package read back
as the root's, its key moves with what the library's compilation reads and not with anything else,
and a tree without librocksdb-sys gets no library. Builds nothing. Run by selftest-checks.sh."""

import importlib.util
import os
import subprocess
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path
from unittest import mock

_path = Path(__file__).with_name("selftest-rocksdb.py")
_spec = importlib.util.spec_from_file_location("selftest_rocksdb", _path)
sr = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(sr)
ROOT = Path(__file__).resolve().parents[2]


class Profile(unittest.TestCase):
    def test_the_root_profiles_read_back(self):
        profile = tomllib.loads((ROOT / "Cargo.toml").read_text()).get("profile", {})
        self.assertTrue(profile)
        self.assertEqual(tomllib.loads(sr.profile_toml(profile)).get("profile", {}), profile)

    def test_scalars_sub_tables_and_dotted_names(self):
        profile = {"dev": {"opt-level": 1, "package": {"a.b": {"opt-level": 3}, "c": {"debug": False}, "*": {}},
                           "build-override": {"opt-level": "s"}}, "release": {"panic": "unwind"}}
        self.assertEqual(tomllib.loads(sr.profile_toml(profile))["profile"], profile)


class Env(unittest.TestCase):
    def test_compiler_and_flag_variables_are_inputs(self):
        for name in ("CC", "CXX", "AR", "CFLAGS", "CXXFLAGS", "CXX_x86_64_unknown_linux_gnu", "HOST_CXX",
                     "TARGET_CFLAGS", "CXX_x86_64-unknown-linux-gnu", "CXXSTDLIB", "CARGO_BUILD_TARGET",
                     "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS",
                     "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS", "CARGO_PROFILE_DEV_OPT_LEVEL",
                     "ROCKSDB_CXX_STD", "ZSTD_SYS_USE_PKG_CONFIG", "LIBCLANG_PATH"):
            self.assertTrue(sr.ENV.fullmatch(name), name)

    def test_unrelated_variables_are_not(self):
        for name in ("PATH", "HOME", "TMPDIR", "RUSTC_WRAPPER", "CARGO_BUILD_JOBS", "CARGO_INCREMENTAL",
                     "SELFTEST_RUN_NONCE", "CCACHE_DIR", "XDG_CACHE_HOME", "CARGO_TARGET_DIR",
                     "CARGO_TARGET_TMPDIR", "CARGO_BUILD_TARGET_DIR"):
            self.assertFalse(sr.ENV.fullmatch(name), name)


class Key(unittest.TestCase):
    """key_inputs on this tree (cargo tree, offline): the features are the workspace's, and the key
    moves with a compiler flag variable but not with an unrelated one."""

    @classmethod
    def setUpClass(cls):
        cls.ver, cls.features, cls.inputs = sr.key_inputs(ROOT)

    def test_features_and_lock(self):
        self.assertIsNotNone(self.ver)
        self.assertIn("static", self.features)
        self.assertIn("librocksdb-sys", [p["name"] for p in self.inputs["lock"]])

    def test_a_flag_moves_the_key_and_an_unrelated_variable_does_not(self):
        with mock.patch.dict(os.environ, {"SELFTEST_UNRELATED": "1"}):
            self.assertEqual(sr.key_inputs(ROOT)[2], self.inputs)
        with mock.patch.dict(os.environ, {"CXXFLAGS": "-DSELFTEST"}):
            self.assertNotEqual(sr.key_inputs(ROOT)[2], self.inputs)


class NoRocksDb(unittest.TestCase):
    def test_a_tree_without_librocksdb_sys_gets_nothing(self):
        with tempfile.TemporaryDirectory() as d:
            (Path(d) / "Cargo.lock").write_text('version = 4\n\n[[package]]\nname = "a"\nversion = "0.1.0"\n')
            r = subprocess.run([sys.executable, "-I", "-B", str(_path), d], capture_output=True, text=True,
                               env={**os.environ, "NODE_SELFTEST_CACHE": str(Path(d) / "cache")})
            self.assertEqual((r.returncode, r.stdout), (0, ""), r.stderr)
            self.assertFalse((Path(d) / "cache").exists())


if __name__ == "__main__":
    unittest.main()
