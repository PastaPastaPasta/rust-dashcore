#!/usr/bin/env python3
"""Offline tests of setup-dashd.py's release checksum check (BL-402): the dashd archive is
verified against the release's SHA256SUMS before it is extracted, and a mismatch, a missing
entry, a conflicting list or an unreachable list stops the script with nothing extracted. A
fake tarball stands in for the release; `download` is replaced, so there is no network, and
every cache directory is a fresh temporary one (never ~/.rust-dashcore-test). Download's retries and
its fallback to the API asset endpoint (DEC-1392) run against local HTTP servers on 127.0.0.1. Run by
contrib/node/selftest-checks.sh."""

import contextlib
import hashlib
import http.server
import importlib.util
import io
import json
import os
import shutil
import socket
import tarfile
import time
import tempfile
import threading
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
                    raise SystemExit(f"Failed to download {url} after 6 attempts")
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


class Server:
    """An HTTP server on 127.0.0.1 in a thread. `routes` maps a path to a list of replies, each
    (status, headers, body), the last repeating; every request's path and headers are recorded."""

    def __init__(self, test, routes):
        self.routes = {path: list(replies) for path, replies in routes.items()}
        self.seen = []
        server = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                server.seen.append((self.path, dict(self.headers)))
                replies = server.routes.get(self.path, [(404, {}, b"not found")])
                status, headers, body = replies.pop(0) if len(replies) > 1 else replies[0]
                self.send_response(status)
                for name, value in headers.items():
                    self.send_header(name, value)
                if "Content-Length" not in headers:
                    self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *args):
                pass

        self.httpd = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self.httpd.server_address[1]}"
        thread = threading.Thread(target=self.httpd.serve_forever, args=(0.05,), daemon=True)
        thread.start()
        test.addCleanup(thread.join)
        test.addCleanup(self.httpd.server_close)
        test.addCleanup(self.httpd.shutdown)

    def paths(self):
        return [path for path, _ in self.seen]

    def authorizations(self):
        return [h.get("Authorization") for _, h in self.seen]


class Download(unittest.TestCase):
    """The download path, the API's asset endpoint (which redirects to a storage host), and the retries."""

    DL = "/dashpay/dash/releases/download/v9.9.9/dashcore.tar.gz"
    TAG = "/repos/dashpay/dash/releases/tags/v9.9.9"
    ASSET = "/repos/dashpay/dash/releases/assets/77"
    PAYLOAD = b"the release asset" * 1000
    TOKEN = "ghs_test-token-0123456789"

    def setUp(self):
        tmp = tempfile.TemporaryDirectory(prefix="test-setup-dashd.", dir=os.environ.get("TMPDIR"))
        self.addCleanup(tmp.cleanup)
        self.dest = Path(tmp.name, "out")
        self.storage = Server(self, {"/blob": [(200, {}, self.PAYLOAD)]})
        self.logs, self.sleeps = [], []
        for patch in (mock.patch.object(sd, "log", self.logs.append),
                      mock.patch.object(sd.time, "sleep", self.sleeps.append),
                      mock.patch.dict(os.environ, {"GH_TOKEN": self.TOKEN})):
            patch.start()
            self.addCleanup(patch.stop)
        os.environ.pop("GITHUB_TOKEN", None)

    def serve(self, dl, tag=None, asset=None):
        """The github.com and api.github.com stand-ins; by default the API serves the asset."""
        release = json.dumps({"assets": [{"id": 76, "name": "other.zip"}, {"id": 77, "name": "dashcore.tar.gz"}]}).encode()
        self.gh = Server(self, {self.DL: dl})
        self.api = Server(self, {self.TAG: tag or [(200, {}, release)],
                                 self.ASSET: asset or [(302, {"Location": f"{self.storage.url}/blob"}, b"")]})
        for patch in (mock.patch.object(sd, "GITHUB", self.gh.url), mock.patch.object(sd, "GITHUB_API", self.api.url)):
            patch.start()
            self.addCleanup(patch.stop)

    def download(self, url=None):
        sd.download(url or self.gh.url + self.DL, str(self.dest), timeout=10)

    def test_the_download_path_is_tried_first(self):
        self.serve([(200, {}, self.PAYLOAD)])
        self.download()
        self.assertEqual(self.dest.read_bytes(), self.PAYLOAD)
        self.assertEqual((self.gh.paths(), self.api.paths(), self.sleeps), ([self.DL], [], []))

    def test_a_5xx_falls_back_to_the_api_asset(self):
        self.serve([(504, {}, b"gateway timeout")])
        self.download()
        self.assertEqual(self.dest.read_bytes(), self.PAYLOAD)
        self.assertEqual(self.api.paths(), [self.TAG, self.ASSET])
        self.assertEqual(self.storage.paths(), ["/blob"])
        self.assertEqual(self.sleeps, [10])
        _, asset_headers = self.api.seen[1]
        self.assertEqual(asset_headers.get("Accept"), "application/octet-stream")

    def test_the_token_goes_only_to_the_api(self):
        self.serve([(502, {}, b"")])
        self.download()
        self.assertEqual(self.api.authorizations(), [f"Bearer {self.TOKEN}"] * 2)
        # urllib copies ordinary headers onto a redirect: the storage host must not get the token.
        self.assertEqual(self.storage.authorizations(), [None])
        self.assertEqual(self.gh.authorizations(), [None])
        self.assertFalse([line for line in self.logs if self.TOKEN in line], "the token was logged")

    def test_which_token_the_api_gets(self):
        # A token http.client would refuse (non-latin-1, a space, DEL) is neither sent nor logged; a
        # trailing line ending is trimmed; GITHUB_TOKEN stands in for an unset GH_TOKEN.
        cases = [({}, None),
                 ({"GITHUB_TOKEN": "other-token"}, "Bearer other-token"),
                 ({"GH_TOKEN": self.TOKEN + "\r\n"}, f"Bearer {self.TOKEN}"),
                 ({"GH_TOKEN": "ghs_bad\u20actoken"}, None),
                 ({"GH_TOKEN": "ghs_bad token"}, None),
                 ({"GH_TOKEN": "ghs_bad\x7ftoken"}, None)]
        for env, expected in cases:
            with self.subTest(env=env):
                for name in ("GH_TOKEN", "GITHUB_TOKEN"):
                    os.environ.pop(name, None)
                os.environ.update(env)
                self.logs.clear()
                self.storage.seen.clear()
                self.serve([(503, {}, b"")])
                self.download()
                self.assertEqual(self.dest.read_bytes(), self.PAYLOAD)
                self.assertEqual(self.api.authorizations(), [expected] * 2)
                self.assertEqual(self.storage.authorizations(), [None])
                self.assertFalse([line for line in self.logs if "ghs_bad" in line], "the token was logged")

    def test_the_paths_alternate_and_the_gaps_grow(self):
        self.serve([(504, {}, b"")], asset=[(502, {}, b""), (502, {}, b""),
                                            (302, {"Location": f"{self.storage.url}/blob"}, b"")])
        self.download()
        self.assertEqual(self.gh.paths(), [self.DL] * 3)
        self.assertEqual(self.api.paths(), [self.TAG] + [self.ASSET] * 3)  # the asset id is looked up once
        self.assertEqual(self.sleeps, [10, 20, 40, 60, 60])
        self.assertEqual(self.dest.read_bytes(), self.PAYLOAD)

    def test_every_attempt_failing_exits_after_six(self):
        self.serve([(504, {}, b"")], tag=[(502, {}, b"")])
        with self.assertRaises(SystemExit) as e:
            self.download()
        self.assertIn("after 6 attempts", str(e.exception))
        self.assertEqual((len(self.gh.seen), len(self.api.seen)), (3, 3))
        self.assertEqual(self.sleeps, [10, 20, 40, 60, 60])

    def test_another_4xx_is_final(self):
        for status in (404, 401, 403, 410):
            with self.subTest(status=status):
                self.sleeps.clear()
                self.serve([(status, {}, b"")])
                with self.assertRaises(SystemExit) as e:
                    self.download()
                self.assertIn(f"HTTP Error {status}", str(e.exception))
                self.assertEqual((len(self.gh.seen), self.api.paths(), self.sleeps), (1, [], []))

    def test_a_final_api_answer_leaves_the_download_path_the_remaining_attempts(self):
        # A 403 without rate-limit headers (GitHub's secondary limit can look like that), a stale token's
        # 401, a missing asset: the API is done, but the download path's 5xx may still clear.
        for name, tag, asset in (("403", [(403, {}, b"")], None),
                                 ("401", [(401, {}, b"")], None),
                                 ("no asset", [(200, {}, b'{"assets": [{"id": 1, "name": "dashcore.tar.gz.asc"}]}')], None),
                                 ("id not a number", [(200, {}, b'{"assets": [{"id": true, "name": "dashcore.tar.gz"}]}')], None),
                                 ("asset 404", None, [(404, {}, b"")])):
            with self.subTest(name):
                self.sleeps.clear()
                self.serve([(504, {}, b""), (504, {}, b""), (200, {}, self.PAYLOAD)], tag=tag, asset=asset)
                self.download()
                self.assertEqual(self.dest.read_bytes(), self.PAYLOAD)
                self.assertEqual(len(self.gh.seen), 3)
                self.assertEqual(self.api.paths()[0], self.TAG)
                self.assertLessEqual(len(self.api.seen), 2)
                self.assertEqual(self.sleeps, [10, 20, 40])
                self.assertTrue([line for line in self.logs if line.startswith("API fallback off")])

    def test_with_the_api_off_the_download_path_still_gets_six_attempts(self):
        self.serve([(504, {}, b"")], tag=[(404, {}, b"")])
        with self.assertRaises(SystemExit) as e:
            self.download()
        self.assertIn("after 6 attempts", str(e.exception))
        self.assertEqual((len(self.gh.seen), self.api.paths()), (5, [self.TAG]))
        self.assertEqual(self.sleeps, [10, 20, 40, 60, 60])

    def test_an_unreadable_release_json_is_retried(self):
        for garbled in (b"<html>not json", b"[]", b'{"assets": "none"}', b"\xff\xfe", b"[" * 100000 + b"]" * 100000):
            with self.subTest(garbled=garbled):
                self.sleeps.clear()
                release = json.dumps({"assets": [{"id": 77, "name": "dashcore.tar.gz"}]}).encode()
                self.serve([(504, {}, b"")], tag=[(200, {}, garbled), (200, {}, release)])
                self.download()
                self.assertEqual(self.dest.read_bytes(), self.PAYLOAD)
                self.assertEqual(self.api.paths(), [self.TAG, self.TAG, self.ASSET])
                self.assertEqual(self.sleeps, [10, 20, 40])

    def test_a_reply_cut_short_is_retried(self):
        self.serve([(200, {"Content-Length": str(len(self.PAYLOAD) * 3)}, self.PAYLOAD)])
        self.download()
        self.assertEqual(self.dest.read_bytes(), self.PAYLOAD)
        self.assertEqual(self.sleeps, [10])
        self.assertEqual(self.api.paths(), [self.TAG, self.ASSET])
        self.assertTrue([line for line in self.logs if "cut short" in line])

    def test_rate_limits_are_retried_and_retry_after_is_honoured_up_to_60_s(self):
        cases = [((429, {"Retry-After": "30"}), [30]),
                 ((429, {}), [10]),
                 ((403, {"Retry-After": "5"}), [10]),
                 ((403, {"X-RateLimit-Remaining": "0"}), [10]),
                 ((503, {"Retry-After": "600"}), [60]),
                 ((503, {"Retry-After": "Wed, 21 Oct 2015 07:28:00 GMT"}), [10])]
        for (status, headers), sleeps in cases:
            with self.subTest(status=status, headers=headers):
                self.sleeps.clear()
                self.serve([(status, headers, b"")])
                self.download()
                self.assertEqual(self.sleeps, sleeps)
                self.assertEqual(self.dest.read_bytes(), self.PAYLOAD)

    def test_a_refused_connection_is_retried(self):
        self.serve([(200, {}, self.PAYLOAD)])
        with socket.socket() as s:
            s.bind(("127.0.0.1", 0))
            closed = f"http://127.0.0.1:{s.getsockname()[1]}"
        with mock.patch.object(sd, "GITHUB", closed):
            self.download(closed + self.DL)
        self.assertEqual(self.dest.read_bytes(), self.PAYLOAD)
        self.assertEqual(self.sleeps, [10])
        self.assertEqual(self.api.paths(), [self.TAG, self.ASSET])

    def test_a_timeout_is_retried(self):
        self.serve([(200, {}, self.PAYLOAD)])
        with socket.socket() as s:  # accepts (backlog) but never answers
            s.bind(("127.0.0.1", 0))
            s.listen(1)
            silent = f"http://127.0.0.1:{s.getsockname()[1]}"
            with mock.patch.object(sd, "GITHUB", silent):
                sd.download(silent + self.DL, str(self.dest), timeout=2)
        self.assertEqual(self.dest.read_bytes(), self.PAYLOAD)
        self.assertEqual(self.sleeps, [10])

    def test_a_url_that_is_no_github_release_retries_itself(self):
        # Another host, even with a release-shaped path, never sends the request (or the token) to the API.
        for path in ("/file", self.DL):
            with self.subTest(path=path):
                self.sleeps.clear()
                other = Server(self, {path: [(500, {}, b""), (200, {}, self.PAYLOAD)]})
                self.serve([(200, {}, b"")])
                self.download(other.url + path)
                self.assertEqual(other.paths(), [path] * 2)
                self.assertEqual((self.gh.seen, self.api.seen), ([], []))
                self.assertEqual(self.dest.read_bytes(), self.PAYLOAD)
                self.assertEqual(self.sleeps, [10])

    def test_a_retried_download_replaces_a_partial_file(self):
        self.dest.write_bytes(b"x" * (len(self.PAYLOAD) * 2))
        self.serve([(504, {}, b"")])
        self.download()
        self.assertEqual(self.dest.read_bytes(), self.PAYLOAD)


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
