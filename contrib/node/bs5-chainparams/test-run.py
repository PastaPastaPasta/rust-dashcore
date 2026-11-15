#!/usr/bin/env python3
"""Tests of run.py's oracle comparison: the oracle prints minBlocksToKeep in its params
(ABI 35), the dump keeps it in "ext", and a case matches only when the rest of the params
are equal and the oracle's value is the dump's ext value. Run by selftest-checks.sh."""

import importlib.util
import unittest
from pathlib import Path

_spec = importlib.util.spec_from_file_location("bs5_run", Path(__file__).with_name("run.py"))
run = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(run)

PARAMS = {"networkID": "main", "pruneAfterHeight": 100000, "consensus": {"nPowTargetSpacing": 150}}


def oracle(keep=2304, **changes):
    params = {"core_version": "v24.0.0-rc.5", **PARAMS, **changes}
    if keep is not None:
        params["minBlocksToKeep"] = keep
    return {"pin": "v24.0", "name": "main/none", "params": params}


def dump(keep=2304):
    return {"params": dict(PARAMS), "ext": {"minBlocksToKeep": keep, "rpcPort": 9998}}


def matches(o, rec):
    return run.oracle_view(o) == run.dump_view(rec)


class OracleComparison(unittest.TestCase):
    def test_equal_params_and_ext_value_match(self):
        self.assertTrue(matches(oracle(), dump()))

    def test_the_value_is_compared_with_ext(self):
        self.assertFalse(matches(oracle(keep=288), dump(keep=2304)))
        self.assertFalse(matches(oracle(keep=2304), dump(keep=288)))

    def test_an_oracle_without_the_key_does_not_match(self):
        self.assertFalse(matches(oracle(keep=None), dump()))

    def test_other_params_still_compare_exactly(self):
        self.assertFalse(matches(oracle(pruneAfterHeight=1), dump()))
        self.assertFalse(matches(oracle(extra=1), dump()))

    def test_errors_compare_as_errors(self):
        err = {"pin": "v24.0", "name": "error/x", "error": "bad"}
        self.assertTrue(matches(err, {"error": "bad"}))
        self.assertFalse(matches(err, {"error": "other"}))
        self.assertFalse(matches(err, dump()))


if __name__ == "__main__":
    unittest.main()
