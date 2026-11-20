#!/usr/bin/env python3
"""Tests of check-products.py's rustc argv tokenizer and allow-list (DEC-188
am. 1 (3)): every spelling rustc accepts of each allow-listed option reads as
the same (option, value) and gets the same verdict, and options off the list
fail closed. Run by selftest-checks.sh."""

import importlib.util
import os
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

_spec = importlib.util.spec_from_file_location("check_products", Path(__file__).with_name("check-products.py"))
cp = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(cp)

# A value each allow-listed option's rule takes ("-L", "--extern" and
# "--target" are given theirs in setUp).
SAMPLES = {"-l": "static=z", "--cfg": "selftest", "--check-cfg": "cfg(selftest)", "--crate-type": "rlib",
           "--crate-name": "selftest", "--edition": "2024", "--emit": "link", "--error-format": "json",
           "--json": "artifacts", "--cap-lints": "allow", "--out-dir": "out", "-W": "unused", "-A": "unused",
           "-D": "unused", "-C": "opt-level=3"}


def spellings(option, value):
    """Every spelling rustc's getopts accepts of `option value`."""
    short = [c for c, (name, takes) in cp.SHORT.items() if name == option and takes]
    long = [n for n, (name, takes) in cp.LONG.items() if name == option and takes]
    return ([[f"-{c}{value}"] for c in short] + [[f"-{c}", value] for c in short] +
            [[f"--{n}={value}"] for n in long] + [[f"--{n}", value] for n in long])


class Table(unittest.TestCase):
    def test_the_option_table_is_rustc_s(self):
        """SHORT and LONG name rustc's options with their arity, as `rustc
        --help -v` lists them (-Z aside, which it does not list)."""
        listed = {}
        for line in subprocess.run(["rustc", "--help", "-v"], stdout=subprocess.PIPE, check=True,
                                   text=True).stdout.splitlines():
            words = line.split()
            if not words or not re.fullmatch(r"--?[A-Za-z][\w-]*,?", words[0]):
                continue
            names = [words[0].rstrip(",")] + (words[1:2] if words[0].endswith(",") else [])
            after = words[len(names)] if len(words) > len(names) else ""
            if after == "help":
                continue  # `-C help`, `-W help`
            takes = after[:1] in ("<", "[")
            for name in names:
                listed[name] = takes
        table = {f"-{c}": takes for c, (_, takes) in cp.SHORT.items() if c != "Z"}
        table |= {f"--{n}": takes for n, (_, takes) in cp.LONG.items()}
        self.assertEqual(table, listed)


class Tokens(unittest.TestCase):
    def setUp(self):
        self.dir = Path(self.enterContext(tempfile.TemporaryDirectory(prefix="check-products-test.")))
        deps = self.dir / "deps"
        deps.mkdir()
        self.owned, self.foreign = deps / "libowned-1.rlib", deps / "libforeign-1.rlib"
        self.owned.touch()
        self.foreign.touch()
        self.context = {"triple": "aarch64-linux-android", "deps": {deps.resolve()}, "sysroot": {"proc_macro"},
                        "owner": {cp.inode(self.owned): "path+file:///w/owned#0.1.0"}}
        self.samples = SAMPLES | {"-L": f"dependency={deps}", "--extern": f"owned={self.owned}",
                                  "--target": "aarch64-linux-android"}

    def verdicts(self, option, value):
        """The verdict on each spelling of `option value`, which must read as it."""
        out = []
        for args in spellings(option, value):
            pairs, bad = cp.tokens(args)
            self.assertIsNone(bad, args)
            self.assertEqual([(o, v) for o, v, _ in pairs], [(option, value)], args)
            out.append(cp.check(option, value, self.dir, self.context))
        self.assertGreaterEqual(len(out), 2, option)
        return out

    def test_every_allow_listed_option_in_every_spelling(self):
        self.assertEqual(set(self.samples), set(cp.ALLOWED) - {""})
        for option, value in self.samples.items():
            with self.subTest(option=option):
                self.assertEqual(self.verdicts(option, value), [None] * len(spellings(option, value)))

    def test_every_allow_listed_codegen_key_in_every_spelling(self):
        for key in cp.CODEGEN:
            for value in (f"{key}=x", f"{key.replace('-', '_')}=x"):
                with self.subTest(value=value):
                    self.assertEqual(set(self.verdicts("-C", value)), {None})

    def test_refusals_hold_in_every_spelling(self):
        for option, value in (("--extern", f"foreign={self.foreign}"), ("--extern", "foreign"),
                              ("--extern", f"priv:owned={self.owned}"), ("--extern", "proc_macro=x"),
                              ("-L", f"all={self.dir}"), ("-L", str(self.dir)), ("-L", f"crate={self.dir}/deps"),
                              ("-L", f"dependency={self.dir}"), ("-C", "passes=x"), ("-C", "relocation-model=pic"),
                              ("-C", "link-args=-lz"), ("-C", "panic=abort"), ("-F", "unused"),
                              ("--diagnostic-width", "80"),
                              ("--emit", "link=/elsewhere"), ("--emit", "dep-info,link=/elsewhere"),
                              ("--target", "x86_64-unknown-linux-gnu")):
            with self.subTest(option=option, value=value):
                self.assertNotIn(None, self.verdicts(option, value))

    def test_allowed_natives_and_sysroot(self):
        for option, value in (("-L", f"native={self.dir}"), ("-L", f"framework={self.dir}"), ("--extern", "proc_macro"),
                              ("-C", "link-arg=-fuse-ld=mold")):
            with self.subTest(option=option, value=value):
                self.assertEqual(set(self.verdicts(option, value)), {None})

    def test_the_input_file_is_accepted(self):
        """The source input is allow-listed as the option "" (Sol r9 nit 1)."""
        pairs, bad = cp.tokens(["src/main.rs", "-", "--cfg", "x"])
        self.assertIsNone(bad)
        self.assertEqual([(o, v) for o, v, _ in pairs], [("", "src/main.rs"), ("", "-"), ("--cfg", "x")])
        self.assertEqual(cp.refusals({"cwd": str(self.dir), "args": ["src/main.rs"]}, "the product unit",
                                     self.context), [])

    def test_a_sysroot_name_shadowed_in_deps(self):
        (self.dir / "deps" / "libproc_macro-1.rlib").touch()
        self.assertIsNotNone(cp.check("--extern", "proc_macro", self.dir, self.context))

    def test_grouped_short_options(self):
        pairs, bad = cp.tokens(["-gLnative=x", "-OW", "unused", "main.rs"])
        self.assertIsNone(bad)
        self.assertEqual([(o, v) for o, v, _ in pairs],
                         [("-g", None), ("-L", "native=x"), ("-O", None), ("-W", "unused"), ("", "main.rs")])

    def test_options_off_the_list_fail_closed(self):
        for args in (["--sysroot", "/s"], ["--sysroot=/s"], ["-Zthreads=1"], ["-Z", "threads=1"], ["@args"],
                     ["-L", "@args"], ["--"], ["--remap-path-prefix=a=b"], ["--test"], ["-g"], ["-O"],
                     ["--print", "cfg"], ["--bogus"], ["-q"], ["--test=x"], ["--cfg"], ["-L"]):
            with self.subTest(args=args):
                reasons = cp.refusals({"cwd": str(self.dir), "args": args}, "the planted unit", self.context)
                self.assertTrue(reasons, args)
                self.assertTrue(all(r.startswith("the planted unit gives rustc ") for r in reasons), reasons)

    def test_the_refusal_names_option_and_source(self):
        reasons = cp.refusals({"cwd": str(self.dir), "args": [f"--extern=hidden={self.foreign}"]},
                              "the product unit", self.context)
        self.assertEqual(reasons, [f"the product unit gives rustc `--extern=hidden={self.foreign}`: a file that is "
                                   f"no Cargo artifact of a package in the unit graph"])
        reasons = cp.refusals({"cwd": str(self.dir), "args": ["--sysroot=/s"]}, "the product unit", self.context)
        self.assertEqual(reasons, ["the product unit gives rustc `--sysroot=/s`: --sysroot is not an allow-listed "
                                   "option"])


if __name__ == "__main__":
    os.chdir(Path(__file__).resolve().parents[2])
    unittest.main()
