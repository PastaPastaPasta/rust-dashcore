#!/usr/bin/env python3
"""Tests of qm1a5-testnet-record.py's parsing of `quorum listextended` and its SHA256SUMS lines.
They read no oracle."""

import hashlib
import importlib.util
import json
import unittest
from pathlib import Path

_spec = importlib.util.spec_from_file_location(
    "qm1a5_record", Path(__file__).with_name("qm1a5-testnet-record.py")
)
rec = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(rec)

H1 = "00" * 31 + "01"
H2 = "00" * 31 + "02"
H3 = "00" * 31 + "03"


def entry(h):
    return {h: {"creationHeight": 1, "minedBlockHash": H1, "numValidMembers": 1, "healthRatio": "1.00"}}


class ListedQuorums(unittest.TestCase):
    def test_types_and_order_kept(self):
        text = json.dumps(
            {"llmq_50_60": [entry(H2), entry(H1)], "llmq_400_60": [], "llmq_60_75": [entry(H3)]}
        )
        self.assertEqual(rec.listed_quorums(text), [(1, H2), (1, H1), (5, H3)])

    def test_empty_types(self):
        self.assertEqual(rec.listed_quorums('{"llmq_400_85": []}'), [])

    def test_unknown_type_refused(self):
        with self.assertRaisesRegex(ValueError, "unknown LLMQ type"):
            rec.listed_quorums(json.dumps({"llmq_7_7": [entry(H1)]}))

    def test_two_key_entry_refused(self):
        bad = {**entry(H1), **entry(H2)}
        with self.assertRaisesRegex(ValueError, "one-key"):
            rec.listed_quorums(json.dumps({"llmq_50_60": [bad]}))

    def test_non_hash_key_refused(self):
        with self.assertRaisesRegex(ValueError, "no quorum hash"):
            rec.listed_quorums(json.dumps({"llmq_50_60": [entry("AB" * 32)]}))

    def test_non_array_refused(self):
        with self.assertRaisesRegex(ValueError, "not an array"):
            rec.listed_quorums(json.dumps({"llmq_50_60": entry(H1)}))


class Sums(unittest.TestCase):
    def test_sha256sum_format_in_name_order(self):
        got = rec.sha256sums({"b": b"x", "a": b""})
        want = (
            f"{hashlib.sha256(b'').hexdigest()}  a\n"
            f"{hashlib.sha256(b'x').hexdigest()}  b\n"
        )
        self.assertEqual(got, want)


if __name__ == "__main__":
    unittest.main()
