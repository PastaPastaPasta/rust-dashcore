"""Regressions for store-vectors.sh and record-ft.py's manifest (O8 review "O8 Sol r1", H3).

Each test builds a small FT output directory in a temp dir (under TMPDIR) from the two
mining_basic nodes already stored in test-vectors, adds the manifest.json and the digest and
decisions hashes the rig writes, and runs the script on it against a throwaway ft-list.

Run: python3 -I -m unittest discover -s contrib/node/statedigest/tests
The statedigest-check example must be built (cargo build -p dash-statedigest --example
statedigest-check), or STATEDIGEST_CHECK must name it.
"""
import hashlib
import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
RIG = HERE.parent
ROOT = RIG.parents[2]
CHECK = Path(os.environ.get("STATEDIGEST_CHECK", ROOT / "target/debug/examples/statedigest-check"))
STORED = ROOT / "test-vectors/statedigest/regtest-v24/mining_basic"
NODES = ["node0", "node1"]
TEST = "mining_basic.py"


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def h_lines(digest):
    """The digest's H records only: as many runs as starts, no records."""
    return b"".join(l for l in Path(digest).read_bytes().splitlines(keepends=True) if l.startswith(b'{"k":"H"'))


def snapshot(tree):
    """Every path under tree with its bytes' hash, for comparing a tree before and after."""
    tree = Path(tree)
    return {str(p.relative_to(tree)): sha256(p) if p.is_file() else None for p in sorted(tree.rglob("*"))}


class Fixture(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="store-vectors-test."))
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.ft = self.tmp / "ft"
        self.top = self.ft / "mining_basic"
        for node in NODES:
            rec = self.top / "rec" / node
            rec.mkdir(parents=True)
            for name in ("digest", "decisions", "golden"):
                subprocess.run(["zstd", "-dc", "-q", str(STORED / node / f"{name}.jsonl.zst"), "-o",
                                str(rec / f"{name}.jsonl")], check=True)
            for name in ("chainargs.json", "provenance.json", "extract.json", "dec177.jsonl"):
                shutil.copy(STORED / node / name, rec / name)
            if (STORED / node / "blocks.dat.zst").exists():
                subprocess.run(["zstd", "-dc", "-q", str(STORED / node / "blocks.dat.zst"), "-o",
                                str(rec / "blocks.dat")], check=True)
            else:  # stored before blocks.dat was: any bytes, hashed below
                (rec / "blocks.dat").write_bytes(b"blocks of " + node.encode())
            self.rewrite_extract(rec)
        self.write_manifest()
        self.list = self.tmp / "ft-list.txt"
        self.list.write_text(TEST + "\n")
        self.dest = self.tmp / "vectors"
        # An earlier store, which a failing run must leave alone.
        (self.dest / "old").mkdir(parents=True)
        (self.dest / "old" / "marker").write_text("previous\n")

    def rec(self, node):
        return self.top / "rec" / node

    def rewrite_extract(self, rec, **changes):
        """extract.json as the rig writes it now: the stored one plus the digest and decisions hashes,
        the extraction's summary and the (empty) BIP9 check."""
        e = json.loads((rec / "extract.json").read_text())
        for name in ("digest", "decisions"):
            e[name] = {"sha256": sha256(rec / f"{name}.jsonl")}
        e["blocks_dat_sha256"] = sha256(rec / "blocks.dat")
        e.setdefault("other_params_runs", [])
        e.setdefault("summary", {"tip": e["golden"]["heights"][1], "bip9_unchecked": []})
        e.setdefault("bip9", {"unchecked": [], "groups": []})
        e.update(changes)
        (rec / "extract.json").write_text(json.dumps(e, indent=1, sort_keys=True) + "\n")

    def write_manifest(self, **changes):
        m = {"test": TEST, "test_args": [], "nodes": NODES, "recorded": NODES, "unstarted": [],
             "source": "run directory"}
        m.update(changes)
        (self.top / "manifest.json").write_text(json.dumps(m, indent=1, sort_keys=True) + "\n")

    def store(self, ft_list=None):
        env = dict(os.environ, STATEDIGEST_FT_LIST=str(ft_list or self.list))
        return subprocess.run(["bash", str(RIG / "store-vectors.sh"), str(self.ft), str(CHECK), str(self.dest)],
                              capture_output=True, text=True, env=env)

    def refused(self, why):
        """The store fails with `why` in its message and leaves the destination as it was."""
        before = snapshot(self.dest)
        r = self.store()
        self.assertNotEqual(r.returncode, 0, r.stdout)
        self.assertIn(why, r.stderr)
        self.assertEqual(snapshot(self.dest), before)
        self.assertEqual([p.name for p in self.tmp.iterdir() if p.name.startswith(".store.")], [])


class StoreVectors(Fixture):
    def test_a_clean_fixture_is_stored_in_place_of_the_old_tree(self):
        r = self.store()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("stored 2 nodes", r.stdout)
        self.assertFalse((self.dest / "old").exists())
        self.assertEqual(sorted(p.name for p in self.dest.iterdir()), ["mining_basic"])
        self.assertEqual(sorted(p.name for p in (self.dest / "mining_basic").iterdir()),
                         ["manifest.json", "node0", "node1"])
        self.assertEqual(sorted(p.name for p in (self.dest / "mining_basic/node0").iterdir()),
                         ["blocks.dat.zst", "chainargs.json", "dec177.jsonl", "decisions.jsonl.zst",
                          "digest.jsonl.zst", "extract.json", "golden.jsonl.zst", "provenance.json"])
        stored = subprocess.run(["zstd", "-dc", str(self.dest / "mining_basic/node0/blocks.dat.zst")],
                                capture_output=True, check=True).stdout
        self.assertEqual(stored, (self.rec("node0") / "blocks.dat").read_bytes())
        self.assertEqual(sha256(self.dest / "mining_basic/manifest.json"), sha256(self.top / "manifest.json"))
        self.assertEqual([p.name for p in self.tmp.iterdir() if p.name.startswith(".store.")], [])

    def test_a_first_store_creates_the_destination(self):
        shutil.rmtree(self.dest)
        r = self.store()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue((self.dest / "mining_basic/node1/golden.jsonl.zst").is_file())

    def test_a_missing_node_directory_is_refused(self):
        shutil.rmtree(self.rec("node1"))
        self.refused("manifest.json records ['node0', 'node1'] but rec/ has ['node0']")

    def test_an_unlisted_node_directory_is_refused(self):
        self.write_manifest(nodes=["node0"], recorded=["node0"])
        self.refused("but rec/ has ['node0', 'node1']")

    def test_a_manifest_whose_recorded_is_not_nodes_minus_unstarted_is_refused(self):
        self.write_manifest(nodes=NODES + ["node2"])
        self.refused("recorded is not nodes minus unstarted")

    def test_an_h_only_digest_beside_the_full_golden_is_refused(self):
        digest = self.rec("node0") / "digest.jsonl"
        digest.write_bytes(h_lines(digest))
        self.refused("a digest file other than the one extracted")

    def test_an_h_only_digest_with_its_hash_fixed_up_is_refused_by_the_checker(self):
        rec = self.rec("node0")
        digest = rec / "digest.jsonl"
        digest.write_bytes(h_lines(digest))
        self.rewrite_extract(rec)
        self.refused("statedigest-check")

    def test_a_digest_that_differs_from_the_hash_in_extract_json_is_refused(self):
        rec = self.rec("node1")
        self.rewrite_extract(rec, digest={"sha256": "0" * 64})
        self.refused("a digest file other than the one extracted")

    def test_a_decisions_file_that_differs_from_the_hash_is_refused(self):
        self.rewrite_extract(self.rec("node1"), decisions={"sha256": "0" * 64})
        self.refused("a decisions file other than the one extracted")

    def test_an_extract_json_without_the_hashes_is_refused(self):
        rec = self.rec("node0")
        e = json.loads((rec / "extract.json").read_text())
        del e["digest"]
        (rec / "extract.json").write_text(json.dumps(e))
        self.refused("lacks 'digest'")

    def other_params(self, rec, unchecked, groups):
        """Gives the first start of rec other -vbparams, so its digest's first run is an
        other-params run, and writes an extract.json whose BIP9 check has `groups`."""
        c = json.loads((rec / "chainargs.json").read_text())
        c["starts"][0]["cmdline"]["vbparams"] = ["testdummy:0:1:0:4:4:4:5:1"]
        (rec / "chainargs.json").write_text(json.dumps(c))
        run = json.loads((rec / "digest.jsonl").open().readline())["run"]
        e = json.loads((rec / "extract.json").read_text())
        e["summary"].update(bip9_unchecked=unchecked, run="tiphash")
        groups = [dict({"runs": [run], "args": [], "tip": "tiphash"}, **g) for g in groups]
        self.rewrite_extract(rec, summary=e["summary"], bip9={"unchecked": unchecked, "groups": groups},
                             other_params_runs=[run])

    def test_bip9_heights_checked_on_a_copy_are_stored(self):
        self.other_params(self.rec("node0"), [3, 4], [{"checked": [2, 3, 4]}])
        r = self.store()
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_bip9_heights_left_unchecked_are_refused(self):
        self.other_params(self.rec("node0"), [3, 4], [{"checked": [3]}])
        self.refused("BIP9 heights left unchecked")

    def test_a_bip9_group_at_another_tip_is_refused(self):
        self.other_params(self.rec("node0"), [3, 4], [{"checked": [3, 4], "tip": "other"}])
        self.refused("BIP9 heights left unchecked")

    def test_bip9_groups_that_miss_an_other_params_run_are_refused(self):
        self.other_params(self.rec("node0"), [3, 4], [{"checked": [3, 4], "runs": []}])
        self.refused("BIP9 heights left unchecked")

    def test_other_params_runs_must_follow_from_chainargs_and_the_digest(self):
        rec = self.rec("node1")
        self.rewrite_extract(rec, other_params_runs=["made-up"])
        self.refused("other-params runs that chainargs.json and the digest do not give")

    def test_a_summary_for_another_tip_is_refused(self):
        rec = self.rec("node1")
        e = json.loads((rec / "extract.json").read_text())
        self.rewrite_extract(rec, summary=dict(e["summary"], tip=e["summary"]["tip"] - 1))
        self.refused("a summary for another tip than the golden's")

    def test_a_blocks_dat_other_than_the_one_extracted_is_refused(self):
        blocks = self.rec("node1") / "blocks.dat"
        blocks.write_bytes(blocks.read_bytes()[:-1])
        self.refused("a blocks.dat other than the one extracted")

    def test_a_missing_blocks_dat_is_refused(self):
        (self.rec("node0") / "blocks.dat").unlink()
        self.refused("blocks.dat")

    def test_a_golden_other_than_the_one_extracted_is_refused(self):
        golden = self.rec("node0") / "golden.jsonl"
        golden.write_bytes(golden.read_bytes() + golden.read_bytes().splitlines(keepends=True)[-1])
        self.refused("a golden file other than the one extracted")

    def test_provenance_counts_that_do_not_match_the_digest_are_refused(self):
        rec = self.rec("node0")
        p = json.loads((rec / "provenance.json").read_text())
        p["records"]["digest"]["R"] += 1
        (rec / "provenance.json").write_text(json.dumps(p))
        self.refused("digest holds")

    def test_a_missing_test_is_refused(self):
        self.list.write_text("mining_basic.py\nrpc_invalidateblock.py\n")
        self.refused("the recording of a listed test is missing")

    def test_a_manifest_for_another_test_is_refused(self):
        self.write_manifest(test="feature_block.py")
        self.refused("manifest.json is for feature_block.py")

    def test_a_test_without_a_manifest_is_refused(self):
        (self.top / "manifest.json").unlink()
        self.refused("manifest.json: missing")

    def test_an_unsorted_list_is_refused(self):
        self.list.write_text("rpc_invalidateblock.py\nmining_basic.py\n")
        self.refused("sorted")

    def test_other_entries_of_the_ft_output_are_ignored(self):
        (self.ft / "mining_basic.record.log").write_text("log\n")
        (self.ft / "STATUS").write_text("ok\n")
        (self.ft / "feature_block").mkdir()
        self.assertEqual(self.store().returncode, 0)


class Manifest(Fixture):
    def backfill(self, top):
        return subprocess.run(["python3", str(RIG / "record-ft.py"), "--backfill-manifest", str(top)],
                              capture_output=True, text=True)

    def test_backfill_writes_the_manifest_from_frozen_and_unstarted_nodes(self):
        (self.top / "manifest.json").unlink()
        for node in NODES:
            (self.top / "frozen" / node).mkdir(parents=True)
        # A node the test created and never started: only provenance.json knows it.
        for node in NODES:
            p = json.loads((self.rec(node) / "provenance.json").read_text())
            p["unstarted_nodes"] = ["node2"]
            (self.rec(node) / "provenance.json").write_text(json.dumps(p))
        r = self.backfill(self.top)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(json.loads((self.top / "manifest.json").read_text()),
                         {"test": TEST, "test_args": [], "nodes": ["node0", "node1", "node2"],
                          "recorded": NODES, "unstarted": ["node2"],
                          "source": "backfill: frozen/ and unstarted_nodes"})
        self.assertTrue((self.top / "manifest.json").read_text().endswith("}\n"))
        again = self.backfill(self.top)
        self.assertNotEqual(again.returncode, 0)
        self.assertIn("exists", again.stderr)

    def test_backfill_refuses_a_recording_whose_frozen_nodes_differ_from_rec(self):
        (self.top / "manifest.json").unlink()
        (self.top / "frozen" / "node0").mkdir(parents=True)
        r = self.backfill(self.top)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("frozen/", r.stderr)
        self.assertFalse((self.top / "manifest.json").exists())


if __name__ == "__main__":
    unittest.main()
