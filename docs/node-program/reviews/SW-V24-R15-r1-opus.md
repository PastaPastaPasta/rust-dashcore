# SW-V24 R15 r1: Opus review (validator + simplifier)

**Branch:** `node/swv24-r15`, range `e72bbd63a..ef3196dee`. All three commits are signed `G`.
**Spec:** designs/SW-V24-rc4-pin-delta.md row R15 and §5 item 15; BACKLOG BL-402.
**Core:** ~/workspace/dash, tags v24.0.0-rc.3 (0364ed626e) and v24.0.0-rc.4 (0e7382df80).

I reviewed a detached `git worktree add` copy at `/work/scratch/swv24-r15-review`; the probes ran from
`/work/scratch/swv24-r15-dl`. Both are deleted. I did not touch the branch worktree and committed nothing.

| Commit | What |
|---|---|
| d2d6981a2 | `inventory.py` splits backport merges into sub-PR rows (`--no-split` keeps one row per merge) |
| 697e459d0 | `check-citations.py`: Core `file:line` cites that changed or shifted between two pins |
| ef3196dee | BL-402: `setup-dashd.py` checks the archive against the release SHA256SUMS(.asc) before it extracts |

**Verdict: CHANGES REQUIRED.** There are three Majors, and each one has a small fix. Two of them are
false-"same" or false-"covered" paths in the new audit tools, which is the failure those tools exist to
prevent. The BL-402 change is correct as a checksum gate. Its one gap (m1) is older than this change.

## Findings

### Major

**M1. `check-citations.py:47,84-92`: under `diff.noprefix=true` every cite reads "same", and `--strict` exits 0.**
`diff_hunks` finds file paths with `diff --git a/(.+) b/(.+)$`, and `DIFF_OPTS` does not pin the prefixes. With
the user config `diff.noprefix=true`, no header matches. Every hunk then lands under the key `None`, so no
Core file has hunks and `judge()` returns "same" for every cite. I checked this on the real pins by running
`diff_hunks` with `-c diff.noprefix=true` injected: 0 files had hunks (108 without the option), and 559 hunks
went under `None`. The tests set `GIT_CONFIG_GLOBAL=/dev/null`, so they cannot see this. `mnemonicPrefix` is
harmless for commit..commit.
*Fix:* add `--src-prefix=a/`, `--dst-prefix=b/` to `DIFF_OPTS`. Also fail with exit 2 when a `@@` line arrives
while `path is None`, so any future format drift fails loudly instead of reading as "same". Aside, not part of
this range: `inventory.py`'s `reverse_applies` has the same blind spot. It pipes `git diff` into `git apply`
at `-p1`, so under noprefix the "tree" signal is silently never set. The same two options fix it.

**M2. `check-citations.py:51` (`LINES`), `:66-74`: an en- or em-dash range is cut down to its first line, giving false "same" verdicts.**
`net.cpp:90–113` matches as `90` alone, because `(?![\w])` accepts the `–`. Only line 90 is judged. The tree
uses this form a lot: 567 such cites in the files that name rc.3 (all `.md` today). At rc.3→rc.4 I judged
each one both ways (truncated and full range). Two verdicts are wrong:
`docs/node-program/reviews/BSX-r1-sol.md:103` `providertx.cpp:318–324` reads **same** but is **changed**, and
`BSX-r1-sol.md:101` `netinfo.cpp:596–615` reads **shifted** but is **changed**.
*Fix:* accept `[-–—]` in `LINES` (both places) and split on the same class in `parse_ranges`. Add
`"x.cpp:90–113"` to `test_shapes_seen_in_the_tree`.

**M3. `inventory.py:304-311`: in split mode, a merge-resolution edit to a file that a sub-PR touched is in no row, and citing the sub-PR alone passes coverage.**
The residual row is built from the files no sub-commit touched (`if p not in touched`). A conflict
resolution almost always lands in a file a sub-commit touched, because that is where conflicts come from.
So the residual misses the case the comment at `:309` names. I built a probe repo where `#101` sets
`a.cpp` to `a1` and merge `#900` records `a1 plus an unreviewed resolution`, with a report that cites only #101:

    split:      exit 0 (covered)    101 review +1/-1 via 900
    --no-split: exit 1 (uncovered)  900 review +1/-1

The split is now the default, so this is a regression in the "every delta was looked at" guarantee.
rc.4 does not hit it: #7917's first parent is the merge base, and `git show --remerge-diff` of the merge is
empty.
*Fix:* take the residual from what the merge itself did, not from which files were touched. For example, use
the paths of `git show --remerge-diff --format= --numstat <merge>` (git ≥ 2.36), with their numstat
against `parents[0]`. When `merge-base(p1, p2) == p1`, the paths of `git diff --numstat p2 merge` are also
enough. Extend `test_changes_made_by_the_merge_itself_are_kept` with an evil edit to `src/evo/a.cpp`.

### Minor

**m1. `setup-dashd.py:143-156`: extraction is not staged, so a failed or racing extraction leaves a half-filled `dashcore-<v>/` that the next run accepts.**
Verification is correct, and nothing is extracted on any verification failure (the mutants below confirm
this). But `extract()` writes straight into `cache_dir`, and the cache test is only
`os.path.isfile(dashd_bin)` (`:138`). Three cases leave a partial tree with `bin/dashd` in it, and every later
run then uses that tree without checking it:
- ENOSPC partway through,
- a `filter="data"` rejection partway through,
- a second concurrent run.

The archive and list also use fixed names in the shared cache. On agentbox, threads share
~/.rust-dashcore-test (and `func-compat.py` runs this script too). Run B's `open(dest, "wb")` can truncate the
file that run A verified, and it can do so before A extracts. That is a verify-then-use race on one path, and
A's `finally` also deletes B's files. This behaviour predates the change, but the brief asks for "no
half-extracted directory".
*Fix:* download to `tempfile.mkstemp(dir=cache_dir)` and verify that exact file. Extract into
`tempfile.mkdtemp(dir=cache_dir)`, check `bin/dashd` inside it, then `os.rename` the `dashcore-<v>` folder into
place. `shutil.rmtree` the staging folder in the `finally`. This also removes the shared-name race.

**m2. `check-citations.py:55` (`PIN_RE`): any dotted triple counts as "another pin", so in-scope cites are dropped and listed under the wrong reason.**
Section numbers (`§3.5.1`), `127.0.0.1` and crate versions (`2.10.1`) all match `v?\d+\.\d+\.\d+`. A line
that carries one and does not name rc.3 is filed under "name only another pin" and never judged. In the
rc.3-scoped files that is about 197 cites. Judged by hand, they come out as 3 **changed**, 24 **shifted**,
154 same and 8 past-end. Examples: `designs/CH2-coins-flush.md:2500` `validation.cpp:2945-2966` (token
`2.4.5`), and `designs/A1-rev1-dispositions.md:32` `validation.cpp:3048-3095` (tokens `3.5.x`).
*Fix:* only count Core release shapes as pins: a `v` prefix, or `-rc.N`, or `0.1x.y`/`1x.y.z`/`2x.y.z`, with
no `§` or digit before them. Add a test line such as `"§3.5.1: evo/a.cpp:5"`.

**m3. Tests: one assertion is tautological, and three mutants survive.**
- `test-inventory.py:47`: `dict(zip(prs, prs))["7871"] == "7871"` is always true. What it means to check is
  `inv.sub_pr("Merge #7871: backport!: Merge bitcoin/bitcoin#27217: …", "")[0] == "7871"`. Take the subject
  from the recorded row.
- Mutants I ran (all others killed, see below):
  - In `check-citations.py:222`, removing the past-end candidate filter survives. Add an ambiguous name whose
    shorter candidate cannot hold the line.
  - Making `deleted.add` a no-op survives, because a deleted file's hunk already reads "changed". Either
    assert the `gone` label/count, or drop the `gone` special case (see n3).
  - In `inventory.py:181`, `cherry[-1]` → `cherry[0]` survives. Add a commit with two `cherry picked from`
    trailers.

**m4. `check-citations.py:182`: cites in files that never name the old pin are not judged and not counted.**
The scope rule is documented, but the report has no line for what it leaves out. The tree has 5,170
Core-shaped cites in 613 files that never name rc.3. Examples: `dash-p2p/src/netcore.rs` has 60, and
`research/R2-dashcore-inventory.md` has 556. The §5 item 15 row bases its "611 files cite rc.3" figure on
this same kind of scoping. An auditor cannot tell from the report how much was out of scope.
*Fix:* add a count row "Core-shaped cites in files that do not name `<short>` (not judged)", plus the top
files, so the boundary shows.

### Nit

- **n1. `inventory.py:334`.** `via` is inserted before `title`, which moves `title` from column 11 to 12. `commits.tsv` gains two columns. Both modes change format, so `--no-split` keeps the rows but not the old bytes. I found no in-tree or node-program reader (`grep prs.tsv|commits.tsv`: only `inventory.py` and its test). Append `via` after `title`, or note the format change in AUDIT-BRIEF.
- **n2. `inventory.py:269` + `:311`.** The residual row's `sub` and `pids` cover all the sub-commits (`parents[0]..merge`). So `commits.tsv` and the packet list every sub-commit a second time under the umbrella, the summary's "per-merge entries" count goes up, and the residual's `in_old` signal is computed over commits it does not stand for. Give the residual `sub=[]` and skip the patch-id signals.
- **n3. `check-citations.py:88-89,143`.** With `--no-renames -U0`, a deleted file already has a `-1,N +0,0` hunk that reads "changed". `deleted`/"gone" is now only a label. Keep it and test it (m3), or drop it.
- **n4. `check-citations.py:172-176`.** `short` repeats the third element of `names()`. Docstring lines 17 and 33 run to about 140 columns, while the rest of the file wraps at about 100.
- **n5. `check-citations.py:137`.** `length()` counts `\n`, so a cite to the last line of a file with no trailing newline reads past-end.
- **n6. `check-citations.py:194-203`.** A line that names only the new pin is judged as an old-pin cite (`file`). Re-running after the D3 rewrite will list rewritten rc.4 cites as changed or shifted noise. Consider a "new pin" bucket that is not judged.
- **n7. `selftest-checks.sh:2205-2207`.** The three cases sit between the release-guard cases and that group's `release_refs` cases. Move them after the group. Sharding is round-robin by number, so nothing depends on the position.
- **n8. `check-citations.py`.** A missing `--out` directory gives a traceback, not exit 2.

## Checks with no finding

- **setup-dashd fails closed on every verification path.** It stops on:
  - an unreachable list (stops before the archive is fetched),
  - a conflicting duplicate,
  - a missing entry, including a lookalike `.asc` name,
  - a digest mismatch,
  - a tampered archive.

  `sys.exit` inside `try` runs the `finally`, and the test checks that the cache is empty afterwards. An
  identical duplicate entry is accepted, which is correct.
- **Hash comparison.** Hex is checked to be exactly 64 characters, with either case, and lowercased on both
  sides. Whitespace is any run (`split(None, 1)` plus `strip`), and CRLF is handled by `splitlines`. The
  binary `*` marker and clearsign `- ` escaping are both handled. PGP armour and base64 lines can never
  parse as entries.
- **Real release lists.** v23.1.0 (the CI default), rc.3 and rc.4 all publish `SHA256SUMS.asc` with bare
  asset names, so `get_asset_info()`'s names (Linux, Darwin tar.gz and `win64.zip`) all resolve. The checked-in
  `testdata/SHA256SUMS-24.0.0-rc.4.asc` is byte-identical to the live file. Residual note for a future
  signature step: v23.1.0's `.asc` holds two signature blocks one after the other.
- **Env output is byte-identical.** The `print` lines are unchanged, and all new output goes to stderr. Both
  the `export` and the GITHUB_ENV forms are tested.
- **Extraction safety.**
  - The tar side keeps `filter="data"`, which blocks absolute paths, `..`, links that point outside the target,
    device files, and setuid bits.
  - `zipfile` strips `..` and absolute names and creates no symlinks.
  - Extraction now only ever sees a verified archive.
- **check-citations `judge()`:**
  - overlap,
  - a pure insertion with `old_start` meaning "after line K" (inside the range = changed, at `b` = same),
  - deletions,
  - balanced edits (no shift),
  - net-zero shift = same, which is correct because no hunk touches the lines,
  - an omitted `,1` in the hunk header.

  `--no-renames` makes a renamed file "gone", which is the conservative choice.
- **Ambiguous names.** The worst verdict across candidates wins, so an ambiguous cite cannot read "same" while
  one of its candidates changed. Exact path and `src/` path are preferred before suffix matches.
- **Real-tree run** (rc.3→rc.4, 14 s): 9,551 judged cites, of which 361 changed, 2,007 shifted, 7,110 same,
  73 past-end, 316 unresolved and 559 other-pin. No abbreviated ranges (`3637-40`) were dropped.
- **inventory attribution:**
  - On the recorded #7917 commits: 37 distinct sub-PRs and 3 direct commits. Every commit has a cherry source.
  - A bitcoin backport keeps its Dash number.
  - Follow-up mentions and `bitcoin#` merges are not counted as that PR.
  - An ordinary feature merge is not split.
  - Direct commits are covered by their sha.
- **Tests fail with the code reverted.** 19 targeted mutants, all killed except the three in m3:
  - setup-dashd 7/7: `.lower()`, `lstrip("*")`, the dash-escape, the duplicate check, the verify call, the
    `finally` cleanup, the 64-character check.
  - check-citations 6/8.
  - inventory 4/5.
- **Runs.** `test-inventory.py` 9 OK, `test-check-citations.py` 10 OK, `test-setup-dashd.py` 12 OK (Python
  3.14.4, git 2.53.0). These are the commands from selftest cases 331–333 (`--list`), run from the tree root.
  I did not run the whole selftest (it is a gate).
- **Docs.** The CLAUDE.md and AUDIT-BRIEF.md wording matches the behaviour, and the docstrings say plainly
  that there is no signature check.
