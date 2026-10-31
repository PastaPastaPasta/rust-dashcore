#!/usr/bin/env python3
"""Offline tests of setup-dashd.py's release checksum check (BL-402): the dashd archive is
verified against the release's SHA256SUMS before it is extracted, and a mismatch, a missing
entry, a conflicting list or an unreachable list stops the script with nothing extracted. A
fake tarball stands in for the release; `download` is replaced, so there is no network, and
every cache directory is a fresh temporary one (never ~/.rust-dashcore-test). Run by
contrib/node/selftest-checks.sh."""

import contextlib
import hashlib
import importlib.util
import io
import os
import shutil
import tarfile
import time
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("setup_dashd", HERE / "setup-dashd.py")
sd = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(sd)

VERSION = "9.9.9"


def fake_release(tmp, payload=b"#!/bin/sh\necho dashd\n", escape=False, zipped=False):
    """The bytes of a tarball (or zip) holding dashcore-9.9.9/bin/dashd. With `escape`, a member
    that `filter="data"` refuses follows it, so extraction fails after dashd is already written."""
    work = tempfile.mkdtemp(prefix="release.", dir=tmp)
    src = Path(work, f"dashcore-{VERSION}", "bin")
    src.mkdir(parents=True)
    (src / ("dashd.exe" if zipped else "dashd")).write_bytes(payload)
    out = Path(work, "built.zip" if zipped else "built.tar.gz")
    if zipped:
        with zipfile.ZipFile(out, "w") as zf:
            zf.write(src / "dashd.exe", f"dashcore-{VERSION}/bin/dashd.exe")
    else:
        with tarfile.open(out, "w:gz") as tf:
            tf.add(Path(work, f"dashcore-{VERSION}"), arcname=f"dashcore-{VERSION}")
            if escape:
                member = tarfile.TarInfo("../escape")
                member.size = 3
                tf.addfile(member, io.BytesIO(b"bad"))
    return out.read_bytes()


class Parse(unittest.TestCase):
    def test_the_recorded_rc4_list(self):
        sums = sd.parse_sha256sums((HERE / "testdata" / "SHA256SUMS-24.0.0-rc.4.asc").read_text())
        self.assertEqual(len(sums), 10)
        self.assertEqual(sums["dashcore-24.0.0-rc.4-x86_64-linux-gnu.tar.gz"],
                         "dd91084338e2415f4c4b8307e62668530c1da0bb8875f07b5662e797945d7b4c")
        self.assertEqual(sums["dashcore-24.0.0-rc.4-win64.zip"],
                         "164739e5db13a378339bd000d355af2310ea4bfef68fadf9e8c8642442ed2c13")
        self.assertTrue(all(len(d) == 64 for d in sums.values()))
        self.assertNotIn("Hash:", " ".join(sums))

    def test_shapes(self):
        d = "ab" * 32
        self.assertEqual(sd.parse_sha256sums(f"{d}  a.tar.gz\n{d.upper()} *b.zip\n- {d}  c.tgz\nnot a line\n{d[:-1]}  short\n"),
                         {"a.tar.gz": d, "b.zip": d, "c.tgz": d})
        self.assertEqual(sd.parse_sha256sums(""), {})

    def test_two_digests_for_one_name_are_refused(self):
        with self.assertRaises(SystemExit) as e:
            sd.parse_sha256sums(f"{'a' * 64}  x.tar.gz\n{'b' * 64}  x.tar.gz\n")
        self.assertIn("two different digests", str(e.exception))


class Setup(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="test-setup-dashd.", dir=os.environ.get("TMPDIR"))
        self.addCleanup(self.tmp.cleanup)
        self.cache = Path(self.tmp.name, "cache")
        self.cache.mkdir()
        self.archive = fake_release(self.tmp.name)
        for patch in (mock.patch.object(sd, "DASHVERSION", VERSION), mock.patch.object(sd, "log", lambda m: None)):
            patch.start()
            self.addCleanup(patch.stop)
        self.asset = sd.get_asset_info()
        self.urls = []
        self.dests = []

    def digest(self, data=None):
        return hashlib.sha256(self.archive if data is None else data).hexdigest()

    def fake_download(self, sums_text, archive=None, fail_sums=False):
        def download(url, dest, **_):
            self.urls.append(url.rsplit("/", 1)[-1])
            self.dests.append(dest)
            if url.endswith("/SHA256SUMS.asc"):
                if fail_sums:
                    raise SystemExit(f"Failed to download {url} after 3 attempts")
                Path(dest).write_text(sums_text)
            else:
                Path(dest).write_bytes(self.archive if archive is None else archive)
        return mock.patch.object(sd, "download", download)

    def sums(self, **entries):
        return ("-----BEGIN PGP SIGNED MESSAGE-----\nHash: SHA256\n\n"
                + "".join(f"{d}  {name}\n" for name, d in entries.items())
                + "-----BEGIN PGP SIGNATURE-----\n\nxxxx\n-----END PGP SIGNATURE-----\n")

    def run_setup(self, sums_text, **kw):
        with self.fake_download(sums_text, **kw):
            return sd.setup_dashd(str(self.cache))

    def assertNothingLeft(self):
        self.assertEqual(sorted(p.name for p in self.cache.iterdir()), [], "extracted files or leftovers in the cache")

    def test_a_matching_archive_is_extracted(self):
        dashd = self.run_setup(self.sums(**{self.asset: self.digest(), "other.tar.gz": "0" * 64}))
        self.assertEqual(dashd, str(self.cache / f"dashcore-{VERSION}" / "bin" / "dashd"))
        self.assertTrue(os.path.isfile(dashd))
        # The list is fetched before the archive, and neither is left behind.
        self.assertEqual(self.urls, ["SHA256SUMS.asc", self.asset])
        self.assertEqual(sorted(p.name for p in self.cache.iterdir()), [f"dashcore-{VERSION}"])

    def test_a_mismatch_extracts_nothing(self):
        with self.assertRaises(SystemExit) as e:
            self.run_setup(self.sums(**{self.asset: self.digest(b"another archive")}))
        self.assertIn("SHA256 mismatch", str(e.exception))
        self.assertIn(self.digest(), str(e.exception))
        self.assertNothingLeft()

    def test_a_tampered_archive_is_refused(self):
        listed = self.sums(**{self.asset: self.digest()})
        with self.assertRaises(SystemExit) as e:
            self.run_setup(listed, archive=self.archive[:-1] + bytes([self.archive[-1] ^ 1]))
        self.assertIn("SHA256 mismatch", str(e.exception))
        self.assertNothingLeft()

    def test_a_missing_entry_extracts_nothing(self):
        for text in (self.sums(**{"dashcore-0.0.0-other.tar.gz": self.digest()}),
                     self.sums(**{self.asset + ".asc": self.digest()}),  # a lookalike name is not the entry
                     "-----BEGIN PGP SIGNED MESSAGE-----\n\n-----END PGP SIGNATURE-----\n", ""):
            self.urls.clear()
            with self.assertRaises(SystemExit) as e:
                self.run_setup(text)
            self.assertIn("no entry", str(e.exception))
            self.assertNothingLeft()

    def test_an_unreachable_list_stops_before_the_archive(self):
        with self.assertRaises(SystemExit):
            self.run_setup("", fail_sums=True)
        self.assertEqual(self.urls, ["SHA256SUMS.asc"])
        self.assertNothingLeft()

    def test_a_conflicting_list_is_refused(self):
        text = f"{self.digest()}  {self.asset}\n{'0' * 64}  {self.asset}\n"
        with self.assertRaises(SystemExit):
            self.run_setup(text)
        self.assertNothingLeft()

    def test_downloads_go_to_unique_names(self):
        """A fixed cache/<asset> name lets a concurrent run truncate the file this run verified."""
        listed = self.sums(**{self.asset: self.digest()})
        self.run_setup(listed)
        first = list(self.dests)
        self.assertNotIn(str(self.cache / self.asset), first)
        shutil.rmtree(self.cache / f"dashcore-{VERSION}")
        self.dests.clear()
        self.run_setup(listed)
        self.assertEqual(len(set(first) | set(self.dests)), 4)

    def test_a_failed_extraction_leaves_nothing_a_later_run_would_trust(self):
        bad = fake_release(self.tmp.name, escape=True)
        listed = self.sums(**{self.asset: self.digest(bad)})
        with self.assertRaises(Exception):
            self.run_setup(listed, archive=bad)
        # dashd was written before the refused member; it must not be in the cache.
        self.assertNothingLeft()
        self.assertFalse((self.cache.parent / "escape").exists())
        # And the next run installs from scratch.
        dashd = self.run_setup(self.sums(**{self.asset: self.digest()}))
        self.assertTrue(os.path.isfile(dashd))

    def test_a_directory_left_by_an_old_failed_run_is_replaced(self):
        stale = self.cache / f"dashcore-{VERSION}" / "bin"
        stale.mkdir(parents=True)
        (stale / "half-written").write_text("x")
        dashd = self.run_setup(self.sums(**{self.asset: self.digest()}))
        self.assertTrue(os.path.isfile(dashd))
        self.assertFalse((stale / "half-written").exists())

    def test_a_run_that_lost_the_race_keeps_the_winners_install(self):
        winner = self.cache / f"dashcore-{VERSION}" / "bin"

        def racing_extract(archive_path, dest):
            real_extract(archive_path, dest)
            winner.mkdir(parents=True)  # another run finishes its rename first
            (winner / "dashd").write_text("winner")

        real_extract = sd.extract
        with mock.patch.object(sd, "extract", racing_extract):
            dashd = self.run_setup(self.sums(**{self.asset: self.digest()}))
        self.assertEqual(Path(dashd).read_text(), "winner")
        self.assertEqual(sorted(p.name for p in self.cache.iterdir()), [f"dashcore-{VERSION}"])

    def test_a_zip_archive_keeps_its_extension(self):
        """extract() tells zip from tar by the file name, so the temporary name must end in .zip."""
        zipped = fake_release(self.tmp.name, zipped=True)
        asset = f"dashcore-{VERSION}-win64.zip"
        with mock.patch.object(sd, "get_asset_info", lambda: asset), mock.patch.object(sd.platform, "system", lambda: "Windows"):
            dashd = self.run_setup(f"{hashlib.sha256(zipped).hexdigest()}  {asset}\n", archive=zipped)
        self.assertTrue(dashd.endswith("dashd.exe") and os.path.isfile(dashd))

    def test_an_archive_without_dashd_installs_nothing(self):
        """The staged check, not only the final one: without it the tree would be renamed into place first."""
        src = Path(self.tmp.name, "nodashd", f"dashcore-{VERSION}", "share")
        src.mkdir(parents=True)
        (src / "readme").write_text("no binary here")
        out = Path(self.tmp.name, "nodashd.tar.gz")
        with tarfile.open(out, "w:gz") as tf:
            tf.add(src.parent, arcname=f"dashcore-{VERSION}")
        bad = out.read_bytes()
        with mock.patch.object(sd.os, "rename", side_effect=AssertionError("renamed an unchecked tree")):
            with self.assertRaises(SystemExit) as e:
                self.run_setup(self.sums(**{self.asset: self.digest(bad)}), archive=bad)
        self.assertIn("Expected binary not found", str(e.exception))
        self.assertNothingLeft()

    def test_leftovers_of_a_killed_run_are_swept_but_a_live_run_is_not(self):
        old = time.time() - 2 * sd.STALE_TEMP_SECONDS
        stale = [self.cache / f".dashcore-{VERSION}.x1y2z3_4", self.cache / f".{self.asset}.ab12cd34.tar.gz",
                 self.cache / f".SHA256SUMS-{VERSION}.q9w8e7r6", self.cache / ".dashcore-0.0.1.old_tmp1"]
        (stale[0] / f"dashcore-{VERSION}" / "bin").mkdir(parents=True)
        (stale[0] / f"dashcore-{VERSION}" / "bin" / "dashd").write_text("half")
        stale[3].mkdir()
        for p in stale[1:3]:
            p.write_text("partial")
        live = [self.cache / f".dashcore-{VERSION}.live1234", self.cache / f".SHA256SUMS-{VERSION}.live5678"]
        live[0].mkdir()
        live[1].write_text("being written")
        # CACHE_DIR may be a shared directory ($HOME): someone's own ".dashcore-*" entries survive, however old.
        keep = [self.cache / "regtest-blockchain-v0.0.4", self.cache / "notes.txt", self.cache / ".hidden-other",
                self.cache / ".dashcore-backup", self.cache / ".dashcore-testnet", self.cache / ".dashcore-24.0.0-rc.4.bak",
                self.cache / ".SHA256SUMS-keep", self.cache / ".dashcore-1.2.3.tooshrt"]
        keep[0].mkdir()
        keep[1].write_text("x")
        keep[2].write_text("x")
        keep[3].mkdir()
        (keep[3] / "wallets").mkdir()
        (keep[3] / "wallets" / "wallet.dat").write_text("irreplaceable")
        keep[4].mkdir()
        for p in keep[5:]:
            p.write_text("x")
        for p in stale + keep:
            os.utime(p, (old, old))
        dashd = self.run_setup(self.sums(**{self.asset: self.digest()}))
        self.assertTrue(os.path.isfile(dashd))
        self.assertEqual([p.exists() for p in stale], [False] * 4)
        self.assertEqual([p.exists() for p in live + keep], [True] * (len(live) + len(keep)))
        self.assertEqual((keep[3] / "wallets" / "wallet.dat").read_text(), "irreplaceable")

    def test_the_staging_directory_exists_only_after_the_archive_is_verified(self):
        """A staging directory made before a long download sits idle and looks stale to a concurrent sweep."""
        made = []
        real_mkdtemp = sd.tempfile.mkdtemp

        def mkdtemp(*a, **k):
            made.append(list(self.urls))
            return real_mkdtemp(*a, **k)

        with mock.patch.object(sd.tempfile, "mkdtemp", mkdtemp):
            self.run_setup(self.sums(**{self.asset: self.digest()}))
        self.assertEqual(made, [["SHA256SUMS.asc", self.asset]])

    def test_a_cached_dashd_is_not_downloaded_again(self):
        self.run_setup(self.sums(**{self.asset: self.digest()}))
        self.urls.clear()
        with self.fake_download(""):
            sd.setup_dashd(str(self.cache))
        self.assertEqual(self.urls, [])


class Main(unittest.TestCase):
    """The environment-variable output is the one the CI jobs and the CLAUDE.md recipe eval."""

    def run_main(self, github_actions):
        with tempfile.TemporaryDirectory(prefix="test-setup-dashd.", dir=os.environ.get("TMPDIR")) as tmp:
            archive = fake_release(tmp)
            cache = Path(tmp, "cache")
            variants = {v: str(cache / "regtest-blockchain-v0.0.4" / v) for v in sd.VARIANTS}
            out = io.StringIO()
            with mock.patch.dict(os.environ, {"CACHE_DIR": str(cache), "GITHUB_ACTIONS": str(github_actions).lower()}), \
                    mock.patch.object(sd, "DASHVERSION", VERSION), mock.patch.object(sd, "log", lambda m: None), \
                    mock.patch.object(sd, "setup_test_data", lambda c, v, m: variants[v]):
                listed = f"{hashlib.sha256(archive).hexdigest()}  {sd.get_asset_info()}\n"

                def download(url, dest, **_):
                    Path(dest).write_bytes(listed.encode() if url.endswith(".asc") else archive)

                with mock.patch.object(sd, "download", download), contextlib.redirect_stdout(out):
                    sd.main()
            return out.getvalue().splitlines(), str(cache)

    def test_output_is_unchanged(self):
        lines, cache = self.run_main(github_actions=False)
        self.assertEqual(lines, [f"export DASHD_PATH={cache}/dashcore-{VERSION}/bin/dashd",
                                 f"export DASHD_TEST_DATA={cache}/regtest-blockchain-v0.0.4",
                                 f"export DASHD_MN_DATADIR={cache}/regtest-blockchain-v0.0.4/regtest-mn"])

    def test_github_env_form_has_no_export(self):
        lines, _ = self.run_main(github_actions=True)
        self.assertEqual([l.split("=")[0] for l in lines], ["DASHD_PATH", "DASHD_TEST_DATA", "DASHD_MN_DATADIR"])


if __name__ == "__main__":
    unittest.main()
