#!/usr/bin/env python3
"""Tests of ev1-census.py's pair-table validation (EV1 §6.2, D19): the checked-in
ev1-pairs.toml passes, and each way a table can go wrong fails with its message.
Run by selftest-checks.sh."""

import importlib.util
import os
import tempfile
import unittest
from pathlib import Path

_spec = importlib.util.spec_from_file_location("ev1_census", Path(__file__).with_name("ev1-census.py"))
census = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(census)

CENSUS = {"bad-a": True, "bad-b": True, "bad-c": True, "bad-other": False}

# A small table in the file's shape: steps x:1-x:4, x:4 unreachable, one infeasible
# pair and one equal-string pair.
GOOD = '''[[table]]
key = "x"
section = "3.3"
function = "CheckX"
steps = [
  "x:1 bad-a",
  "x:2 bad-b",
  "x:3 bad-b",
  "x:4 bad-c",
]
loops = [
  "x:2",
]

[[unreachable]]
steps = [
  "x:4",
]
why = "the reader never yields it"

[[infeasible]]
pairs = [
  "x:1 x:2",
]
why = "x:1 needs version 1 and x:2 version 3"

[[equal-string]]
pairs = [
  "x:2 x:3",
]
why = "the same reason"
'''


def errors(text, root=census.ROOT):
    return census.check_pairs(text, CENSUS, root=root)[0]


class Pairs(unittest.TestCase):
    def fails(self, text, message, root=census.ROOT):
        got = errors(text, root)
        self.assertTrue(any(message in e for e in got), f"no {message!r} in {got}")

    def test_the_checked_in_table_is_valid(self):
        with open(census.PAIRS, encoding="utf-8") as f:
            got, report = census.check_pairs(f.read(), census.census_reasons())
        self.assertEqual(got, [])
        self.assertTrue(report[-1].startswith("census: "), report)

    def test_a_good_table_passes(self):
        got, report = census.check_pairs(GOOD, CENSUS, root=census.ROOT)
        self.assertEqual(got, [])
        self.assertIn("1 to reach,  1 infeasible, 0 world,  1 equal-string", report[0])
        world = GOOD.replace("[[infeasible]]", "[[world]]")
        got, report = census.check_pairs(world, CENSUS, root=census.ROOT)
        self.assertEqual(got, [])
        self.assertIn("1 to reach,  0 infeasible, 1 world,  1 equal-string", report[0])

    def test_an_exclusion_naming_a_missing_step(self):
        self.fails(GOOD.replace('  "x:4",\n]', '  "x:9",\n]'), "x:9 is not a step of any table")
        self.fails(GOOD.replace('"x:1 x:2"', '"x:1 x:9"'), "x:9 is not a step of any table")

    def test_an_exclusion_without_a_reason(self):
        self.fails(GOOD.replace('why = "the reader never yields it"', 'why = ""'), "no reason")
        self.fails(GOOD.replace('why = "x:1 needs version 1 and x:2 version 3"\n', ""), "no reason")

    def test_an_equal_string_pair_whose_reasons_differ(self):
        self.fails(GOOD.replace('"x:2 x:3"', '"x:1 x:3"'), "the reasons differ")

    def test_an_infeasible_pair_with_equal_reasons(self):
        bad = GOOD.replace('"x:1 x:2"', '"x:2 x:3"').replace('"x:2 x:3",\n]\nwhy = "the same', '"x:1 x:3",\n]\nwhy = "the same')
        self.fails(bad, "the reasons are equal")
        self.fails(bad.replace("[[infeasible]]", "[[world]]"), "[[world]] x:2 x:3: the reasons are equal")

    def test_a_pair_of_an_excluded_step(self):
        self.fails(GOOD.replace('"x:1 x:2"', '"x:1 x:4"'), "x:4 is unreachable")

    def test_a_step_outside_the_census(self):
        self.fails(GOOD.replace('"x:1 bad-a"', '"x:1 bad-z"'), "bad-z is not a census reason")

    def test_a_step_of_another_table(self):
        self.fails(GOOD.replace('"x:1 bad-a"', '"y:1 bad-a"'), "step y:1 is not labelled x:")

    def test_an_elsewhere_test_that_does_not_exist(self):
        with tempfile.TemporaryDirectory() as root:
            Path(root, "t.rs").write_text("#[test]\nfn covered() {}\n")
            elsewhere = '[[elsewhere]]\ntests = [\n  "t.rs::{}",\n]'
            self.fails(GOOD.replace("[[unreachable]]", elsewhere.format("missed")), "test t.rs::missed not found", root)
            self.fails(GOOD.replace("[[unreachable]]", elsewhere.format("cover")), "test t.rs::cover not found", root)
            self.assertEqual(errors(GOOD.replace("[[unreachable]]", elsewhere.format("covered")), root), [])
            self.fails(GOOD.replace("[[unreachable]]", "[[elsewhere]]"), "names no test", root)

    def test_a_driver_that_does_not_exist(self):
        with tempfile.TemporaryDirectory() as root:
            Path(root, "t.rs").write_text("#[test]\nfn drives() {}\n")
            driven = GOOD.replace('function = "CheckX"\n', 'function = "CheckX"\ndriver = "t.rs::{}"\n')
            self.fails(driven.format("renamed"), "table x: driver t.rs::renamed not found", root)
            self.fails(driven.format("drive"), "table x: driver t.rs::drive not found", root)
            self.fails(driven.replace("t.rs::", "moved.rs::").format("drives"), "driver moved.rs::drives not found", root)
            self.assertEqual(errors(driven.format("drives"), root), [])

    def test_a_malformed_table(self):
        self.fails(GOOD + '\n[[tables]]\nkey = "y"\n', "unknown entry kind [[tables]]")
        for field in ('key = "x"\n', 'section = "3.3"\n', 'function = "CheckX"\n'):
            self.fails(GOOD.replace(field, ""), "needs key, section, function and steps")
        self.fails(GOOD.replace('  "x:1 bad-a",\n  "x:2 bad-b",\n  "x:3 bad-b",\n  "x:4 bad-c",\n', ""),
                   "needs key, section, function and steps")
        table = GOOD.split("\n\n")[0]
        self.fails(GOOD + "\n" + table.replace('"x:', '"x:1') + "\n", "table x: declared twice")

    def test_a_bad_loop_step(self):
        self.fails(GOOD.replace('loops = [\n  "x:2"', 'loops = [\n  "x:9"'), "loop step x:9 is not a step of the table")
        self.fails(GOOD.replace('loops = [\n  "x:2"', 'loops = [\n  "x:4"'), "loop step x:4 is unreachable")

    def test_a_bad_step(self):
        self.fails(GOOD.replace('"x:3 bad-b"', '"x:2 bad-b"'), "step x:2 declared twice")
        self.fails(GOOD.replace('"x:3 bad-b"', '"x:3"'), "step x:3 names no reason")
        self.fails(GOOD.replace('"x:3 bad-b"', '""'), "an empty step")

    def test_a_bad_exclusion(self):
        self.fails(GOOD.replace('steps = [\n  "x:4",\n]\n', ""), "names no step")
        self.fails(GOOD.replace('pairs = [\n  "x:1 x:2",\n]\n', ""), "names no pair")
        twice = '[[elsewhere]]\nsteps = [\n  "x:4",\n]\ntests = [\n  "t.rs::covered",\n]\nwhy = "w"\n\n'
        self.fails(GOOD.replace("[[infeasible]]", twice + "[[infeasible]]"), "x:4 is already unreachable")

    def test_a_bad_pair(self):
        self.fails(GOOD.replace('"x:1 x:2"', '"x:1 x:1"'), "is not two steps")
        self.fails(GOOD.replace('"x:1 x:2"', '"x:1"'), "is not two steps")
        self.fails(GOOD.replace('"x:1 x:2"', '"x:1 x:2 x:3"'), "is not two steps")
        self.fails(GOOD.replace('"x:1 x:2"', '"x:1 x:2",\n  "x:2 x:1"'), "already infeasible")
        self.fails(GOOD.replace('"x:2 x:3"', '"x:3 x:2",\n  "x:2 x:3"'), "already equal-string")
        other = '[[table]]\nkey = "y"\nsection = "3.4"\nfunction = "CheckY"\nsteps = [\n  "y:1 bad-c",\n]\n\n'
        self.fails(other + GOOD.replace('"x:1 x:2"', '"x:1 y:1"'), "the steps are in different tables")

    def test_text_that_is_not_toml(self):
        self.fails(GOOD.replace('key = "x"', 'key = "x'), "not TOML")

    def test_a_blank_census_line(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root, "census.tsv")
            path.write_text("# header\na\tev1\tf\tbad-a\tliteral\t\n\nb\tother\tg\tbad-b\tliteral\t\n")
            self.assertEqual(census.census_reasons(path), {"bad-a": True, "bad-b": False})

    def test_the_restricted_shape(self):
        self.fails(GOOD.replace('steps = [\n  "x:4",\n]', 'steps = ["x:4"]'), "outside the restricted shape")
        self.fails(GOOD.replace('key = "x"', "key = 'x'"), "outside the restricted shape")


if __name__ == "__main__":
    os.chdir(census.ROOT)
    unittest.main()
