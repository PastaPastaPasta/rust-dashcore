#!/usr/bin/env python3
"""Tests of check-citations.py (SW-V24 R15): the citation shapes it reads, the changed /
shifted / same judgement of a cited range against the hunks between two pins, and a run on
a throwaway Core repository and tree (report only, nothing edited). Run by selftest-checks.sh."""

import importlib.util
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("check_citations", HERE / "check-citations.py")
cc = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(cc)

GIT_ENV = dict(os.environ, GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_SYSTEM="/dev/null",
               GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@example.invalid",
               GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@example.invalid")


def git(root, *args):
    return subprocess.run(["git", "-C", str(root), "-c", "commit.gpgsign=false", *args], check=True,
                          capture_output=True, text=True, env=GIT_ENV).stdout.strip()


# A pair of files where git's algorithms disagree about line 7: myers deletes lines 6-7 and inserts one
# line further on (the cite is changed), histogram replaces line 6 and leaves 7 alone (the cite is same).
HIST_OLD = "".join(f"{t}\n" for t in "{ } a { } a d { } { b d".split())
HIST_NEW = "".join(f"{t}\n" for t in "{ } a { } { d } { b d".split())


def lines(n, **replace):
    """A file of n lines "L1"... with some lines replaced."""
    return "".join(f"{replace.get(f'L{i}', f'L{i}')}\n" for i in range(1, n + 1))


class Shapes(unittest.TestCase):
    def cites(self, text):
        return [(m.group("path"), m.group("lines")) for m in cc.CITE_RE.finditer(text)]

    def test_shapes_seen_in_the_tree(self):
        for text, want in [
            ("// Core: src/test/evo_sharedmn_tests.cpp:522 (v24.0.0-rc.3)", [("src/test/evo_sharedmn_tests.cpp", "522")]),
            ("(`llmq/snapshot.cpp` rc.3:173-210), and exactly", [("llmq/snapshot.cpp", "173-210")]),
            ("(`httpserver.cpp` v24.0.0-rc.3:91-101, 369). Waits", [("httpserver.cpp", "91-101, 369")]),
            ("`netinfo.cpp:24, 90-113`", [("netinfo.cpp", "24, 90-113")]),
            ("Core: evo/specialtxman.cpp:702-728, :283 HandleQuorumCommitment", [("evo/specialtxman.cpp", "702-728, :283")]),
            ("`ValidateDSTX` (v24.0.0-rc.3 `net_processing.cpp:3637`): what", [("net_processing.cpp", "3637")]),
            ("`net.cpp:90\u2013113` and `net.cpp:90\u2014113`", [("net.cpp", "90\u2013113"), ("net.cpp", "90\u2014113")]),
            ("`netinfo.cpp:24, 90\u2013113`", [("netinfo.cpp", "24, 90\u2013113")]),
            ("see pow.cpp:170-211 and chain.cpp:32-49.", [("pow.cpp", "170-211"), ("chain.cpp", "32-49")]),
        ]:
            self.assertEqual(self.cites(text), want, text)

    def test_things_that_are_not_cites(self):
        for text in ["a Cargo.toml:12 line", "lib.rs:12", "version 24.0.0-rc.3: the tag", "src/foo.cpp has 12 lines",
                     "netinfo.cpp:24 bytes", "x.cpp:1abc"]:
            got = self.cites(text)
            self.assertEqual([c for c in got if c[0] != "netinfo.cpp"], [], text)
        # The line number stops at the word: "24 bytes" is a line 24 cite followed by prose.
        self.assertEqual(self.cites("netinfo.cpp:24 bytes"), [("netinfo.cpp", "24")])

    def test_ranges(self):
        self.assertEqual(cc.parse_ranges("90-113"), [(90, 113)])
        self.assertEqual(cc.parse_ranges("91-101, 369"), [(91, 101), (369, 369)])
        self.assertEqual(cc.parse_ranges("702-728, :283"), [(702, 728), (283, 283)])
        self.assertEqual(cc.parse_ranges("90\u2013113"), [(90, 113)])
        self.assertEqual(cc.parse_ranges("90\u2014113, 5"), [(90, 113), (5, 5)])
        self.assertEqual(cc.parse_ranges("0"), [])
        self.assertEqual(cc.parse_ranges("9-3"), [])


class Pins(unittest.TestCase):
    def test_releases_in_prose(self):
        for text, want in [
            ("Core v24.0.0-rc.3 and v23.1.8", ["v24.0.0-rc.3", "v23.1.8"]),
            ("24.0.0-rc.3 (the tag)", ["24.0.0-rc.3"]),
            ("Dash Core 23.1.8, dashd 22.0.0", ["23.1.8", "22.0.0"]),
            ("rc.3:173", ["rc.3"]),
            ("v0.17.0 era", ["v0.17.0"]),
        ]:
            self.assertEqual(cc.pins(text), want, text)

    def test_numbers_that_are_not_releases(self):
        for text in ["\u00a73.5.1 of the design", "127.0.0.1:9999", "crate 2.10.1", "see 4.2.1.3", "section 1.2.3 and 12.3.4",
                     "x/v1.2.3.4", "ab1.2.3"]:
            self.assertEqual(cc.pins(text), [], text)


class Judge(unittest.TestCase):
    """judge(a, b, hunks): hunks are (old_start, old_len, new_len), -U0 style."""

    def test_overlap_is_changed(self):
        h = [(10, 3, 5)]  # old lines 10-12 replaced
        for a, b in [(10, 10), (12, 12), (8, 10), (12, 20), (1, 99)]:
            self.assertEqual(cc.judge(a, b, h), "changed", (a, b))
        self.assertEqual(cc.judge(13, 15, h), "shifted")
        self.assertEqual(cc.judge(1, 9, h), "same")

    def test_insertions(self):
        h = [(10, 0, 2)]  # two lines inserted after old line 10
        self.assertEqual(cc.judge(10, 10, h), "same")       # the line itself is untouched, before the insertion
        self.assertEqual(cc.judge(11, 11, h), "shifted")    # after it
        self.assertEqual(cc.judge(9, 11, h), "changed")     # the insertion lands inside the range
        self.assertEqual(cc.judge(10, 11, h), "changed")
        self.assertEqual(cc.judge(1, 10, h), "same")

    def test_deletions_and_balanced_edits(self):
        self.assertEqual(cc.judge(20, 20, [(5, 2, 0)]), "shifted")   # two lines gone earlier
        self.assertEqual(cc.judge(20, 20, [(5, 2, 2)]), "same")      # replaced in place: no shift
        self.assertEqual(cc.judge(6, 6, [(5, 2, 0)]), "changed")


class Run(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="test-check-citations.", dir=os.environ.get("TMPDIR"))
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name)
        self.core, self.tree = root / "core", root / "tree"
        for d in (self.core, self.tree):
            d.mkdir()
            git(d, "init", "-q", "-b", "main")
        files = {"src/evo/a.cpp": lines(30), "src/evo/b.cpp": lines(30), "src/rpc/server.cpp": lines(10),
                 "src/coinjoin/server.cpp": lines(10), "src/wallet/gone.h": lines(10), "src/keep.cpp": lines(10),
                 "src/net/util.cpp": lines(5), "src/evo/util.cpp": lines(30), "src/evo/h.cpp": lines(30),
                 "src/evo/bin.cpp": lines(30), "src/evo/hist.cpp": HIST_OLD}
        self.commit(self.core, "rc.3", files)
        git(self.core, "tag", "v1.0.0-rc.3")
        self.commit(self.core, "rc.4", {
            "src/evo/a.cpp": lines(30, L5="five, rewritten"),                  # line 5 changed
            "src/evo/b.cpp": "new1\nnew2\n" + lines(30),                      # two lines inserted at the top
            "src/coinjoin/server.cpp": lines(10, L3="three, rewritten"),
            "src/evo/h.cpp": lines(30, L5="five, rewritten", L8="eight, rewritten"),
            "src/evo/bin.cpp": lines(30, L5="five, rewritten"), "src/evo/hist.cpp": HIST_NEW})  # two close hunks, 6-7 untouched
        (self.core / "src/wallet/gone.h").unlink()
        git(self.core, "add", "-A")
        git(self.core, "commit", "-q", "-m", "rc.4 deletes gone.h")
        git(self.core, "tag", "v1.0.0-rc.4")

        self.src = (
            "//! Core v1.0.0-rc.3.\n"
            "// a.cpp: src/evo/a.cpp:5 is rewritten at rc.4 and src/evo/a.cpp:20 is not\n"       # changed, same
            "// evo/b.cpp:10-12 move down two lines\n"                                           # shifted
            "// keep.cpp:3 and src/keep.cpp:4-6 do not change\n"                                 # same x2
            "// server.cpp:3 names two files, one of which changed\n"                            # ambiguous
            "// src/wallet/gone.h:2 is deleted at rc.4\n"                                        # gone
            "// evo/a.cpp:99 is past the end\n"                                                  # past-end
            "// nothere.cpp:3 is not Core\n"                                                     # unresolved
            "// v0.9.0 had evo/a.cpp:5 as well\n"                                                # other pin
            "// `evo/a.cpp` rc.3:5 carries its label\n"                                          # attached
            "// evo/a.cpp:3\u20137 is an en-dash range holding the rewritten line 5\n"             # changed (not read as line 3)
            "// Core 23.1.8 had evo/a.cpp:5 as well\n"                                           # other pin ("Core" context)
            "// \u00a72.4.5 evo/a.cpp:5 (a section number is not a pin)\n"                       # changed, judged
            "// on 127.0.0.1 with crate 2.10.1 see evo/a.cpp:20\n"                              # same, judged
            "// util.cpp:20 fits only src/evo/util.cpp (src/net/util.cpp has 5 lines)\n"           # same, not past-end
            "// tree_file.cpp:3 is this tree's own\n")
        self.commit(self.tree, "tree", {"src.rs": self.src, "tree_file.cpp": "x\n",
                                        "unrelated.rs": "// evo/a.cpp:5 but no pin is named here\n"})

    @staticmethod
    def commit(repo, msg, files):
        for name, text in files.items():
            p = Path(repo, name)
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(text)
        git(repo, "add", "-A")
        git(repo, "commit", "-q", "-m", msg)

    def report(self, *extra, env=None):
        out = Path(self.tmp.name, "report.md")
        p = subprocess.run([sys.executable, "-I", str(HERE / "check-citations.py"), "--core", str(self.core),
                            "--old", "v1.0.0-rc.3", "--new", "v1.0.0-rc.4", "--tree", str(self.tree),
                            "--out", str(out), *extra], capture_output=True, text=True, env=env or GIT_ENV)
        self.assertEqual(p.stderr, "")
        return p, out.read_text()

    def section(self, text, name):
        body = text.split(f"## {name}", 1)[1].split("\n## ", 1)[0]
        return [l for l in body.splitlines() if l.startswith("| src.rs") or l.startswith("- ")]

    def test_report(self):
        p, text = self.report()
        self.assertEqual(p.returncode, 0)
        changed = self.section(text, "Changed")
        self.assertEqual(len(changed), 6, changed)
        self.assertTrue(any("2.4.5" in l and "| file |" in l for l in changed))
        self.assertTrue(any("`src/evo/a.cpp:5`" in l and "src/evo/a.cpp changed" in l for l in changed))
        amb = [l for l in changed if "AMBIGUOUS" in l]
        self.assertEqual(len(amb), 1)
        self.assertIn("src/coinjoin/server.cpp changed", amb[0])
        self.assertIn("src/rpc/server.cpp same", amb[0])
        self.assertTrue(any("gone.h" in l for l in changed))
        self.assertTrue(any("`evo/a.cpp:3\u20137`" in l and "3-7" in l for l in changed))
        self.assertTrue(any("`evo/a.cpp:5`" in l and "attached" in l for l in changed))
        shifted = self.section(text, "Shifted")
        self.assertEqual(len(shifted), 1)
        self.assertIn("evo/b.cpp:10-12", shifted[0])
        self.assertEqual(len(self.section(text, "Past the end")), 1)
        # Counts: a.cpp:20, keep.cpp:3, src/keep.cpp:4-6 are unchanged.
        self.assertIn("| 5 | same content at the same line numbers |", text)
        self.assertIn("| 1 | cite past the end of the old file", text)  # evo/a.cpp:99 only, not util.cpp:20
        self.assertIn("(of which 1 in a file gone at the new pin,", text)
        self.assertIn("| 1 | no Core file of that name at the old pin |", text)
        self.assertIn("| 2 | name only another pin on their line |", text)
        self.assertIn("| 1 | name a tracked file of this tree |", text)
        # A file that never names the old pin is not judged, and the report counts what it left out.
        self.assertIn("| 1 | Core citations in 1 files that never name `rc.3` (**not judged**", text)
        self.assertEqual(text.count("unrelated.rs"), 1)
        self.assertIn("- unrelated.rs (1)", text.split("## Not judged", 1)[1])

    def test_user_diff_config_does_not_change_the_verdicts(self):
        """diff.noprefix=true drops the a/ b/ prefixes the hunk parser keys on: every cite read "same"."""
        plain = self.report()[1]
        config = dict(GIT_ENV, GIT_CONFIG_COUNT="3", GIT_CONFIG_KEY_0="diff.noprefix", GIT_CONFIG_VALUE_0="true",
                      GIT_CONFIG_KEY_1="diff.mnemonicPrefix", GIT_CONFIG_VALUE_1="true",
                      GIT_CONFIG_KEY_2="diff.context", GIT_CONFIG_VALUE_2="7")
        p, text = self.report(env=config)
        self.assertEqual(text, plain)
        self.assertEqual(len(self.section(text, "Changed")), 6)

    def test_hunk_fusing_and_the_algorithm_do_not_change_the_verdicts(self):
        """diff.interHunkContext fuses hunks 5 lines apart: a cite of the untouched lines 6-7 between two
        rewritten ones would read "changed" instead of "same"."""
        self.commit(self.tree, "between two hunks", {"src.rs": "// Core rc.3: h.cpp:6-7 and h.cpp:5\n"})
        plain = self.report()[1]
        self.assertIn("| 1 | same content at the same line numbers |", plain)
        for key, value in (("diff.interHunkContext", "5"), ("diff.algorithm", "histogram"),
                           ("diff.algorithm", "patience"), ("diff.indentHeuristic", "false")):
            config = dict(GIT_ENV, GIT_CONFIG_COUNT="1", GIT_CONFIG_KEY_0=key, GIT_CONFIG_VALUE_0=value)
            self.assertEqual(self.report(env=config)[1], plain, f"{key}={value}")

    def test_the_algorithm_pin_is_what_decides_the_histogram_case(self):
        self.commit(self.tree, "algorithm case", {"src.rs": "// Core rc.3: hist.cpp:7\n"})
        plain = self.report()[1]
        self.assertEqual(len(self.section(plain, "Changed")), 1, plain)
        histogram = dict(GIT_ENV, GIT_CONFIG_COUNT="1", GIT_CONFIG_KEY_0="diff.algorithm", GIT_CONFIG_VALUE_0="histogram")
        self.assertEqual(self.report(env=histogram)[1], plain)
        # What the fixture is for: histogram alone, without the pin, would call it unchanged.
        raw = subprocess.run(["git", "-C", str(self.core), "-c", "diff.algorithm=histogram", "diff", "-U0",
                              "v1.0.0-rc.3", "v1.0.0-rc.4", "--", "src/evo/hist.cpp"], capture_output=True, text=True,
                             env=GIT_ENV).stdout
        self.assertIn("@@ -6 +6 @@", raw)

    def test_a_file_git_diffs_as_binary_still_has_its_cites_judged(self):
        """.gitattributes `binary` (or a NUL byte) makes git print "Binary files differ" and no hunks:
        every cite in the file read "same", and --strict exited 0."""
        (self.core / ".gitattributes").write_text("src/evo/bin.cpp binary\n")
        self.commit(self.tree, "cite of a binary-attributed file", {"src.rs": "// Core rc.3: bin.cpp:5\n"})
        p, text = self.report("--strict")
        self.assertEqual(p.returncode, 1, text)
        self.assertEqual(len(self.section(text, "Changed")), 1)
        raw = subprocess.run(["git", "-C", str(self.core), "diff", "-U0", "v1.0.0-rc.3", "v1.0.0-rc.4", "--",
                              "src/evo/bin.cpp"], capture_output=True, text=True, env=GIT_ENV).stdout
        self.assertIn("Binary files", raw)  # the attribute does take effect without --text

    def test_a_binary_diff_that_gets_through_stops_the_run(self):
        with mock.patch.object(cc, "git_text", lambda *a: "diff --git a/x b/x\nBinary files a/x and b/x differ"):
            with self.assertRaises(cc.DiffFormatError):
                cc.diff_hunks("unused", "old", "new")

    def test_a_diff_it_cannot_read_stops_the_run(self):
        for lines in (["@@ -1,2 +1,2 @@"], ["diff --git weird header"], ["diff --git a/x b/x", "@@ nonsense"]):
            with mock.patch.object(cc, "git_text", lambda *a, _l=lines: "\n".join(_l)):
                with self.assertRaises(cc.DiffFormatError):
                    cc.diff_hunks("unused", "old", "new")

    def test_the_run_exits_2_on_a_diff_it_cannot_read(self):
        # A wrapper `git` that prints header-less hunks for `diff`, as an unknown diff format would.
        bin_dir = Path(self.tmp.name, "bin")
        bin_dir.mkdir()
        real = subprocess.run(["which", "git"], capture_output=True, text=True, check=True).stdout.strip()
        wrapper = bin_dir / "git"
        wrapper.write_text(f"#!/bin/sh\ncase \" $*\" in *\" diff \"*) echo '@@ -1 +1 @@';; *) exec {real} \"$@\";; esac\n")
        wrapper.chmod(0o755)
        p = subprocess.run([sys.executable, "-I", str(HERE / "check-citations.py"), "--core", str(self.core),
                            "--old", "v1.0.0-rc.3", "--new", "v1.0.0-rc.4", "--tree", str(self.tree)],
                           capture_output=True, text=True, env=dict(GIT_ENV, PATH=f"{bin_dir}:{GIT_ENV['PATH']}"))
        self.assertEqual(p.returncode, 2, p.stdout + p.stderr)
        self.assertIn("hunk header", p.stderr)

    def test_nothing_is_edited(self):
        before = {p: p.read_bytes() for p in self.tree.rglob("*") if p.is_file() and ".git" not in p.parts}
        self.report()
        after = {p: p.read_bytes() for p in self.tree.rglob("*") if p.is_file() and ".git" not in p.parts}
        self.assertEqual(before, after)
        self.assertEqual(git(self.tree, "status", "--porcelain"), "")
        self.assertEqual(git(self.core, "status", "--porcelain"), "")

    def test_strict_exit_status(self):
        self.assertEqual(self.report("--strict")[0].returncode, 1)
        self.assertEqual(self.report()[0].returncode, 0)

    def test_a_clean_tree_is_clean(self):
        self.commit(self.tree, "only unchanged cites", {"src.rs": "// Core rc.3 src/keep.cpp:3 and evo/a.cpp:20\n"})
        p, text = self.report("--strict")
        self.assertEqual(p.returncode, 0)
        self.assertEqual(self.section(text, "Changed"), [])


if __name__ == "__main__":
    unittest.main()
