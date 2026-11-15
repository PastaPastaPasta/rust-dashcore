#!/usr/bin/env python3
"""Tests of inventory.py's backport-merge split (SW-V24 R15): a first-parent commit that
carries cherry-picked develop merges becomes one row per upstream PR. The attribution
rule runs on the recorded commits of Dash Core's rc.3 -> rc.4 merge
(testdata/rc4-backport-commits.tsv, no network); the end-to-end runs use small
throwaway repositories. Run by selftest-checks.sh."""

import contextlib
import csv
import io
import importlib.util
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("inventory", HERE / "inventory.py")
inv = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(inv)

GIT_ENV = dict(os.environ, GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_SYSTEM="/dev/null",
               GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@example.invalid",
               GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@example.invalid")


def recorded():
    with open(HERE / "testdata" / "rc4-backport-commits.tsv", encoding="utf-8") as f:
        return [line.rstrip("\n").split("\t") for line in f]


class Attribution(unittest.TestCase):
    """sub_pr() on the 40 commits of the real rc.4 merge (#7917)."""

    def test_the_recorded_merge(self):
        rows = recorded()
        self.assertEqual(len(rows), 40)
        got = [inv.sub_pr(subject, trailer) for _, subject, trailer in rows]
        direct = sorted(rows[i][0][:11] for i, (pr, _) in enumerate(got) if pr == "-")
        self.assertEqual(direct, ["6c02b2ba87c", "79480ff2feb", "a56ed47c98a"])
        prs = [pr for pr, _ in got if pr != "-"]
        self.assertEqual(len(prs), 37)
        self.assertEqual(len(set(prs)), 37)
        for pr in ("7888", "7537", "7909", "7871", "7817", "7836", "7814"):
            self.assertIn(pr, prs)
        # A backport of a bitcoin PR is still its Dash PR, not the bitcoin number.
        subject, trailer = next((s, t) for _, s, t in rows if s.startswith("Merge #7871:"))
        self.assertIn("bitcoin/bitcoin#27217", subject)
        self.assertEqual(inv.sub_pr(subject, trailer)[0], "7871")
        self.assertEqual([i for i, (_, s) in enumerate(got) if s == "-"], [])

    def test_the_cherry_pick_source_is_the_trailer_s(self):
        pr, source = inv.sub_pr("Merge #7888: fix", "(cherry picked from commit 0123456789abcdef)")
        self.assertEqual((pr, source), ("7888", "0123456789abcdef"))
        self.assertEqual(inv.sub_pr("ci: x", "(partially cherry picked from commit abcdef1)"), ("-", "abcdef1"))
        # A commit that was itself cherry-picked before keeps both lines; the last one is the immediate source.
        both = "(cherry picked from commit 1111111)\n\n(cherry picked from commit 2222222)"
        self.assertEqual(inv.sub_pr("Merge #7001: x", both), ("7001", "2222222"))

    def test_other_title_shapes(self):
        self.assertEqual(inv.sub_pr("Merge dashpay/dash#7001: x", "")[0], "7001")
        self.assertEqual(inv.sub_pr("fix: y (#7002)", "")[0], "7002")
        # A follow-up that merely mentions a PR, and a bitcoin merge, are not that PR.
        self.assertEqual(inv.sub_pr("test: follow-up for #7821: z", "")[0], "-")
        self.assertEqual(inv.sub_pr("Merge bitcoin/bitcoin#27217: wallet", "")[0], "-")
        self.assertEqual(inv.sub_pr("Merge bitcoin#27217: wallet", "")[0], "-")


class Repo:
    """A throwaway Core-like repository."""

    def __init__(self, root):
        self.root = str(root)
        os.makedirs(self.root)
        self.git("init", "-q", "-b", "main")

    def git(self, *args, check=True):
        return subprocess.run(["git", "-C", self.root, "-c", "commit.gpgsign=false", *args], check=check,
                              capture_output=True, text=True, env=GIT_ENV).stdout.strip()

    def write(self, path, text):
        p = Path(self.root, path)
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text)

    def commit(self, subject, **files):
        """Commit `files` ("src__evo__a_cpp" is src/evo/a.cpp)."""
        for name, text in files.items():
            self.write(name.replace("__", "/").replace("_cpp", ".cpp").replace("_py", ".py"), text)
        self.git("add", "-A")
        self.git("commit", "-q", "-m", subject)
        return self.git("rev-parse", "HEAD")


# A 20-line file: the diffs of a one-line edit have context, so diff.context / algorithm settings show.
B0 = "".join(f"b{i}\n" for i in range(20))
B1 = B0.replace("b10\n", "b10 changed\n").replace("b12\n", "b12 changed\n")


def nested_repo(root, outer="Merge #900: chore: merge side back", signing_key=None):
    """The shape of Core's #7044 ("merge master 23.0.2 back into develop"): the backport merge #900 holds
    cherry-picked "Merge #N" commits and an inner merge of another branch, whose conflict resolution in a.cpp
    (a file the sub-PR #101 touched) and an edit in u.cpp (no sub-commit touches it) are in no commit."""
    r = Repo(root)
    r.commit("base", src__evo__a_cpp="".join(f"a{i}\n" for i in range(10)), src__evo__u_cpp="u0\n")
    r.git("tag", "old")
    r.git("checkout", "-q", "-b", "side")
    side = r.commit("fix: on the side branch", src__evo__a_cpp="side\n" + "".join(f"a{i}\n" for i in range(1, 10)))
    r.git("checkout", "-q", "-b", "rel", "old")
    sub = r.commit("Merge #101: fix: evo thing\n\n(cherry picked from commit aaaaaaa1)",
                   src__evo__a_cpp="rel\n" + "".join(f"a{i}\n" for i in range(1, 10)))
    r.git("merge", "--no-commit", "side", check=False)
    r.write("src/evo/a.cpp", "resolved\n" + "".join(f"a{i}\n" for i in range(1, 10)))
    r.write("src/evo/u.cpp", "u0 plus an unreviewed edit in the inner merge\n")
    r.git("add", "-A")
    r.git("commit", "-q", "-m", "Merge branch 'side' into rel")
    inner = r.git("rev-parse", "HEAD")
    r.git("checkout", "-q", "main")
    if signing_key:  # sign the first-parent merge, as Core's merges are
        r.git("-c", "gpg.format=ssh", "-c", f"user.signingkey={signing_key}", "merge", "-q", "--no-ff", "-S", "-m",
              outer, "rel")
    else:
        r.git("merge", "-q", "--no-ff", "-m", outer, "rel")
    r.git("tag", "new")
    return r, sub, side, inner


def backport_repo(root, evil=None):
    """main at "old"; a release branch of cherry-picked develop merges, a direct
    commit and a commit pair that cancels out, merged back as #900 and tagged "new"."""
    r = Repo(root)
    r.commit("base", src__evo__a_cpp="a0\n", src__evo__b_cpp=B0, src__wallet__w_cpp="w0\n",
             test__functional__t_py="t0\n")
    r.git("tag", "old")
    r.git("checkout", "-q", "-b", "v24.0.x")
    r.commit("Merge #101: fix: evo thing\n\nPull request description:\n\n(cherry picked from commit aaaaaaa1)",
             src__evo__a_cpp="a1\n")
    r.commit("Merge #102: test: only a test\n\n(cherry picked from commit aaaaaaa2)", test__functional__t_py="t1\n")
    direct = r.commit("fix(wallet): a direct commit\n\n(cherry picked from commit aaaaaaa3)", src__wallet__w_cpp="w1\n")
    r.commit("Merge #103: fix: touches b\n\n(cherry picked from commit aaaaaaa4)", src__evo__b_cpp=B1)
    r.commit("Merge #104: fix: puts b back\n\n(cherry picked from commit aaaaaaa5)", src__evo__b_cpp=B0)
    r.git("checkout", "-q", "main")
    r.git("merge", "-q", "--no-ff", "--no-commit", "v24.0.x")
    if evil == "new-file":
        r.write("src/evo/c.cpp", "resolved differently\n")
        r.git("add", "-A")
    elif evil == "touched":  # an unreviewed edit in a file the sub-PR #101 also changed
        r.write("src/evo/a.cpp", "a1 plus an unreviewed resolution\n")
        r.git("add", "-A")
    r.git("commit", "-q", "-m", "Merge #900: backport: v24.0.x backports")
    r.git("tag", "new")
    return r, direct


def run_inventory(repo, out, *extra, env=None):
    p = subprocess.run([sys.executable, "-I", str(HERE / "inventory.py"), "--core", repo.root, "--old", "old",
                        "--new", "new", "--out", str(out), *extra], capture_output=True, text=True, env=env or GIT_ENV)
    return p.returncode, p.stdout, p.stderr


def table(path):
    with open(path, encoding="utf-8", newline="") as f:
        return list(csv.DictReader(f, delimiter="\t"))


class Split(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="test-inventory.", dir=os.environ.get("TMPDIR"))
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)

    def go(self, repo, *extra, env=None):
        out = self.root / "out"
        rc, stdout, stderr = run_inventory(repo, out, *extra, env=env)
        self.assertEqual(stderr, "")
        return rc, out, stdout

    def test_each_pr_is_a_row(self):
        repo, direct = backport_repo(self.root / "core")
        rc, out, stdout = self.go(repo)
        self.assertEqual(rc, 0)
        rows = {r["pr"] if r["pr"] != "-" else "direct": r for r in table(out / "prs.tsv")}
        self.assertEqual(sorted(rows), ["101", "102", "103", "104", "direct"])
        self.assertTrue(all(r["via"] == "900" for r in rows.values()))
        self.assertEqual(rows["direct"]["sha"], direct[:10])
        self.assertEqual({k: r["tier"] for k, r in rows.items()},
                         {"101": "review", "102": "skip", "direct": "review", "103": "no-net-delta",
                          "104": "no-net-delta"})
        self.assertEqual(rows["101"]["primary"], "evo")
        self.assertEqual(rows["direct"]["primary"], "wallet")
        self.assertIn("backport merge #900 (new) split into 5 rows, 1 direct", stdout)
        # The umbrella PR is not a row; the sub-PRs own the files.
        files = {r["path"]: r["prs"] for r in table(out / "files.tsv")}
        self.assertEqual(files, {"src/evo/a.cpp": "101", "src/wallet/w.cpp": direct[:10],
                                 "test/functional/t.py": "102"})
        commits = {r["pr"]: r for r in table(out / "commits.tsv")}
        self.assertEqual(commits["101"]["cherry_picked_from"], "aaaaaaa1")
        self.assertEqual(commits["101"]["via"], "900")

    def tables(self, repo, settings):
        env = dict(GIT_ENV, GIT_CONFIG_COUNT=str(len(settings)))
        for i, (k, v) in enumerate(settings):
            env[f"GIT_CONFIG_KEY_{i}"], env[f"GIT_CONFIG_VALUE_{i}"] = k, v
        _, out, _ = self.go(repo, env=env)
        got = {t: (out / t).read_text() for t in ("prs.tsv", "files.tsv", "commits.tsv", "summary.md")}
        got.update({f"packets/{p.name}": p.read_text() for p in sorted((out / "packets").iterdir())})
        return got

    def test_user_git_config_does_not_change_the_results(self):
        """diff.noprefix broke the `git apply -R` pipe, diff.context=0 makes git refuse a zero-context patch,
        diff.algorithm / diff.interHunkContext move the add/del counts, merge.conflictStyle=diff3 grows the
        remerge-diff of a conflict (and with it a row's area and packet) and core.abbrev changes every %h:
        none may change the tables, on a clean backport and on one with a conflicted inner merge."""
        settings = [("diff.noprefix", "true"), ("diff.mnemonicPrefix", "true"), ("diff.context", "0"),
                    ("diff.algorithm", "histogram"), ("diff.interHunkContext", "5"), ("merge.conflictStyle", "diff3"),
                    ("core.abbrev", "12"), ("log.showSignature", "true")]
        for shape, make in (("backport", lambda r: backport_repo(r)[0]), ("nested", lambda r: nested_repo(r)[0])):
            with self.subTest(shape):
                repo = make(self.root / shape)
                plain = self.tables(repo, [])
                if shape == "backport":
                    in_old = {l.split("\t")[1]: l.split("\t")[4] for l in plain["prs.tsv"].splitlines()[1:]}
                    self.assertEqual(in_old["104"], "tree")
                for k, v in settings:
                    self.assertEqual(self.tables(repo, [(k, v)]), plain, f"{k}={v}")
                self.assertEqual(self.tables(repo, settings), plain, "all together")

    def test_signed_commits_do_not_break_the_parsers(self):
        """With log.showSignature=true every log/show of a signed commit (Core's merges are) puts the gpg output
        on stdout, ahead of the --format lines."""
        if not shutil.which("ssh-keygen"):
            self.skipTest("no ssh-keygen")
        key = self.root / "key"
        subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(key)], check=True)
        repo, sub, side, inner = nested_repo(self.root / "core", signing_key=str(key) + ".pub")
        plain = self.tables(repo, [])
        repo.git("config", "log.showSignature", "true")
        shown = subprocess.run(["git", "-C", repo.root, "log", "-1", "--format=%H", "new"], capture_output=True,
                               text=True, env=GIT_ENV).stdout.strip().splitlines()
        self.assertGreater(len(shown), 1, "the fixture's signature is not shown, so this tests nothing")
        self.assertEqual(self.tables(repo, []), plain)

    def test_an_unexpected_failure_is_exit_2_never_exit_1(self):
        """Exit 1 means "coverage gaps"."""
        with mock.patch.object(inv, "run", side_effect=ValueError("not enough values to unpack")):
            with mock.patch.object(sys, "argv", ["inventory.py", "--core", ".", "--old", "a", "--new", "b",
                                                 "--out", "unused"]):
                with contextlib.redirect_stderr(io.StringIO()) as err:
                    self.assertEqual(inv.main(), 2)
        self.assertIn("ValueError", err.getvalue())

    def test_an_untitled_inner_merge_under_a_prless_backport_is_labelled_by_its_own_sha(self):
        repo, sub, side, inner = nested_repo(self.root / "core", outer="chore: backports with no PR number")
        report = self.root / "report.md"
        report.write_text(f"#101 {side[:11]}\n")
        rc, out, stdout = self.go(repo, "--report", str(report))
        self.assertEqual(rc, 1)
        # The label the report has to cite is the one coverage counts: the inner merge's sha.
        self.assertIn(f"- #{inner[:10]} ", stdout)
        report.write_text(f"#101 {side[:11]} {inner[:11]}\n")
        rc, out, stdout = self.go(repo, "--report", str(report))
        self.assertEqual(rc, 0, stdout)

    def test_an_inner_merge_already_in_the_old_pin_gets_no_residual_row(self):
        r = Repo(self.root / "core")
        r.commit("base", src__evo__a_cpp="a0\n", src__evo__u_cpp="u0\n", src__evo__w_cpp="w0\n")
        r.git("checkout", "-q", "-b", "rel")
        r.commit("Merge #101: fix: evo thing\n\n(cherry picked from commit aaaaaaa1)", src__evo__a_cpp="a1\n")
        r.git("checkout", "-q", "-b", "side", "main")
        r.commit("fix: side", src__evo__a_cpp="a2\n")
        r.git("checkout", "-q", "rel")
        r.git("merge", "--no-commit", "side", check=False)
        r.write("src/evo/a.cpp", "a3 resolved\n")
        r.write("src/evo/u.cpp", "u0 plus an edit in the inner merge\n")
        r.git("add", "-A")
        r.git("commit", "-q", "-m", "Merge branch 'side' into rel")
        r.git("tag", "old")       # the old pin already holds the inner merge
        r.git("checkout", "-q", "main")
        r.commit("chore: main work", src__evo__w_cpp="w1\n")
        r.git("merge", "-q", "--no-ff", "-m", "Merge #900: backport", "rel")
        r.git("tag", "new")
        _, out, _ = self.go(r)
        self.assertEqual([x["title"] for x in table(out / "prs.tsv") if "merge itself" in x["title"]], [])

    def test_packets_list_the_sub_prs_only(self):
        repo, _ = backport_repo(self.root / "core")
        _, out, _ = self.go(repo)
        evo = (out / "packets" / "evo.prs.txt").read_text()
        self.assertIn("#101 ", evo)
        self.assertIn("via #900", evo)
        self.assertNotIn("\n#900 ", "\n" + evo)
        self.assertNotIn("#102 ", evo + (out / "packets" / "wallet.prs.txt").read_text())

    def test_every_reviewable_sub_pr_needs_a_citation(self):
        repo, direct = backport_repo(self.root / "core")
        report = self.root / "report.md"
        # The umbrella PR alone no longer covers anything.
        report.write_text("Backports in #900. Looked at #101.\n")
        rc, out, stdout = self.go(repo, "--report", str(report))
        self.assertEqual(rc, 1)
        self.assertIn("Reviewable PRs not cited by the report: 3", stdout)
        self.assertIn(f"- #{direct[:10]} ", stdout)
        report.write_text(f"#101, #103, #104 and the direct commit {direct[:11]}.\n")
        rc, out, stdout = self.go(repo, "--report", str(report))
        self.assertEqual(rc, 0, stdout)
        cov = {r["pr"] if r["pr"] != "-" else "direct": r["citations"] for r in table(out / "coverage.tsv")}
        self.assertEqual(cov["101"], "1")
        self.assertEqual(cov["102"], "0")

    def test_no_split_keeps_the_merge_whole(self):
        repo, _ = backport_repo(self.root / "core")
        rc, out, _ = self.go(repo, "--no-split")
        rows = table(out / "prs.tsv")
        self.assertEqual([(r["pr"], r["via"], r["tier"]) for r in rows], [("900", "-", "review")])

    def test_changes_made_by_the_merge_itself_are_kept(self):
        repo, direct = backport_repo(self.root / "core", evil="new-file")
        rc, out, _ = self.go(repo)
        rows = table(out / "prs.tsv")
        residual = [r for r in rows if r["pr"] == "900"]
        self.assertEqual(len(residual), 1)
        self.assertEqual(residual[0]["tier"], "review")
        self.assertIn("changes made by the merge itself", residual[0]["title"])
        self.assertEqual(residual[0]["via"], "-")
        files = {r["path"]: r["prs"] for r in table(out / "files.tsv")}
        self.assertEqual(files["src/evo/c.cpp"], "900")
        # The residual stands for the merge's edit only: it lists none of the sub-commits again.
        self.assertEqual(sorted(r["pr"] for r in table(out / "commits.tsv")),
                         sorted(["101", "102", "103", "104", direct[:10]]))
        # The residual row is reviewable, so a report has to cite the umbrella PR now.
        report = self.root / "report.md"
        report.write_text(f"#101 #103 #104 and {direct[:11]}\n")
        self.assertEqual(self.go(repo, "--report", str(report))[0], 1)
        report.write_text(f"#101 #103 #104 #900 and {direct[:11]}\n")
        self.assertEqual(self.go(repo, "--report", str(report))[0], 0)

    def test_a_merge_edit_in_a_file_a_sub_pr_touched_is_kept(self):
        """Conflicts are resolved in files the picked commits touched; the sub-PR rows must not cover them."""
        repo, direct = backport_repo(self.root / "core", evil="touched")
        rc, out, _ = self.go(repo)
        rows = table(out / "prs.tsv")
        residual = [r for r in rows if r["pr"] == "900"]
        self.assertEqual(len(residual), 1, rows)
        self.assertEqual((residual[0]["tier"], residual[0]["via"]), ("review", "-"))
        files = {r["path"]: r["prs"] for r in table(out / "files.tsv")}
        self.assertEqual(files["src/evo/a.cpp"], "101,900")
        # Citing only the sub-PR is not enough, as with --no-split.
        report = self.root / "report.md"
        report.write_text(f"#101 #103 #104 and {direct[:11]}\n")
        rc, _, stdout = self.go(repo, "--report", str(report))
        self.assertEqual(rc, 1)
        self.assertIn("- #900 ", stdout)
        self.assertEqual(self.go(repo, "--no-split", "--report", str(report))[0], 1)
        report.write_text(f"#101 #103 #104 #900 and {direct[:11]}\n")
        self.assertEqual(self.go(repo, "--report", str(report))[0], 0)

    def test_a_merge_that_is_just_its_parents_has_no_residual(self):
        repo, _ = backport_repo(self.root / "core")
        _, out, _ = self.go(repo)
        self.assertEqual([r["pr"] for r in table(out / "prs.tsv") if r["pr"] == "900"], [])

    def test_a_merge_inside_the_range_keeps_its_own_edits(self):
        """#7044's shape: an inner merge's resolution is in the outer merge's second parent, so neither the
        sub-PR rows nor the outer merge's remerge-diff hold it."""
        repo, sub, side, inner = nested_repo(self.root / "core")
        rc, out, stdout = self.go(repo)
        rows = table(out / "prs.tsv")
        residual = [r for r in rows if "changes made by the merge itself" in r["title"]]
        self.assertEqual([(r["pr"], r["sha"], r["tier"]) for r in residual], [("900", inner[:10], "review")])
        files = {r["path"]: r["prs"] for r in table(out / "files.tsv")}
        self.assertEqual(files["src/evo/u.cpp"], "900")        # no sub-commit touched it
        self.assertIn("900", files["src/evo/a.cpp"].split(","))
        report = self.root / "report.md"
        report.write_text(f"#101 and {side[:11]}\n")
        rc, _, stdout = self.go(repo, "--report", str(report))
        self.assertEqual(rc, 1, stdout)
        self.assertIn("- #900 ", stdout)
        self.assertEqual(self.go(repo, "--no-split", "--report", str(report))[0], 1)
        report.write_text(f"#101 {side[:11]} #900\n")
        self.assertEqual(self.go(repo, "--report", str(report))[0], 0)

    def test_an_inner_merge_with_its_own_pr_number_is_that_prs_row(self):
        repo, _ = backport_repo(self.root / "core")
        repo.git("checkout", "-q", "v24.0.x")
        repo.git("checkout", "-q", "-b", "other", "old")
        repo.commit("Merge #778: fix: other\n\n(cherry picked from commit bbbbbbb1)", src__evo__c_cpp="c\n")
        repo.git("checkout", "-q", "v24.0.x")
        repo.git("merge", "--no-ff", "--no-commit", "other")
        repo.write("src/evo/c.cpp", "c plus an edit\n")
        repo.git("add", "-A")
        repo.git("commit", "-q", "-m", "Merge #777: an inner merge")
        repo.git("checkout", "-q", "main")
        repo.git("merge", "-q", "--no-ff", "-m", "Merge #901: second backport", "v24.0.x")
        repo.git("tag", "-f", "new")
        _, out, _ = self.go(repo)
        residual = {r["pr"]: r["via"] for r in table(out / "prs.tsv") if "merge itself" in r["title"]}
        self.assertEqual(residual, {"777": "-"})

    def test_an_octopus_merge_is_one_row_and_does_not_crash(self):
        r = Repo(self.root / "core")
        r.commit("base", src__evo__a_cpp="a0\n", src__evo__b_cpp="b0\n")
        r.git("tag", "old")
        for name, path in (("b1", "a"), ("b2", "b")):
            r.git("checkout", "-q", "-b", name, "old")
            r.commit(f"Merge #{101 if name == 'b1' else 102}: fix {path}", **{f"src__evo__{path}_cpp": "changed\n"})
        r.git("checkout", "-q", "main")
        r.git("merge", "-q", "--no-ff", "-m", "Merge #901: octopus", "b1", "b2")
        r.git("tag", "new")
        rc, out, stdout = self.go(r)
        self.assertEqual(rc, 0, stdout)
        self.assertEqual([(x["pr"], x["via"]) for x in table(out / "prs.tsv")], [("901", "-")])
        self.assertNotIn("split into", stdout)

    def test_a_range_holding_an_octopus_merge_is_not_split(self):
        r = Repo(self.root / "core")
        r.commit("base", src__evo__a_cpp="a0\n", src__evo__b_cpp="b0\n")
        r.git("tag", "old")
        r.git("checkout", "-q", "-b", "rel")
        r.commit("Merge #100: fix", src__evo__a_cpp="a1\n")
        for name in ("b1", "b2"):
            r.git("checkout", "-q", "-b", name, "old")
            r.commit(f"Merge #{101 if name == 'b1' else 102}: fix", **{f"src__evo__{'a' if name == 'b1' else 'b'}2_cpp": "x\n"})
        r.git("checkout", "-q", "rel")
        r.git("merge", "-q", "--no-ff", "-m", "octopus inside", "b1", "b2")
        r.git("checkout", "-q", "main")
        r.git("merge", "-q", "--no-ff", "-m", "Merge #902: holds an octopus", "rel")
        r.git("tag", "new")
        rc, out, stdout = self.go(r)
        self.assertEqual(rc, 0, stdout)
        self.assertEqual([(x["pr"], x["via"]) for x in table(out / "prs.tsv")], [("902", "-")])

    def test_a_plain_commit_titled_merge_is_one_row(self):
        r = Repo(self.root / "core")
        r.commit("base", src__evo__a_cpp="a0\n")
        r.git("tag", "old")
        r.commit("Merge #700: a squash commit that only looks like a merge", src__evo__a_cpp="a1\n")
        r.git("tag", "new")
        rc, out, stdout = self.go(r)
        self.assertEqual([(x["pr"], x["via"], x["title"]) for x in table(out / "prs.tsv")],
                         [("700", "-", "Merge #700: a squash commit that only looks like a merge")])
        self.assertEqual({x["path"]: x["prs"] for x in table(out / "files.tsv")}, {"src/evo/a.cpp": "700"})

    def test_an_ordinary_feature_merge_is_not_split(self):
        r = Repo(self.root / "core")
        r.commit("base", src__evo__a_cpp="a0\n")
        r.git("tag", "old")
        r.git("checkout", "-q", "-b", "feature")
        r.commit("fix: step one\n\n(cherry picked from commit bbbbbbb1)", src__evo__a_cpp="a1\n")
        r.commit("Merge bitcoin/bitcoin#27217: wallet: a bitcoin backport", src__evo__a_cpp="a2\n")
        r.git("checkout", "-q", "main")
        r.git("merge", "-q", "--no-ff", "-m", "Merge #500: fix: a feature", "feature")
        r.git("tag", "new")
        rc, out, _ = self.go(r)
        self.assertEqual([(x["pr"], x["via"]) for x in table(out / "prs.tsv")], [("500", "-")])


if __name__ == "__main__":
    unittest.main()
