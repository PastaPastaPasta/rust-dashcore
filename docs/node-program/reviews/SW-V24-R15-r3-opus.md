# SW-V24 R15 r3: Opus narrow re-check of the round-2 fixes

**Branch:** `node/swv24-r15`, fix range `b03161563..86adda6d9` (84fd17640, 574227847, 86adda6d9). All three commits are signed `G`.
**Round 2:** reviews/SW-V24-R15-r2-opus.md.
**Core:** ~/workspace/dash, tags v23.0.0 (b380d4837c), v23.1.0 (0b7f5bec8d), v24.0.0-rc.3 (0364ed626e) and v24.0.0-rc.4 (0e7382df80).

I reviewed a detached `git worktree add` copy at `/work/scratch/r15-opus-r3`. Outputs, probes and mutants went to
`/work/scratch/r15-opus-r3.{out,probe,mut,core,core2}`, where `.core`/`.core2` are `--shared --no-checkout` clones
of Core used for repo-local config probes. All of them are deleted. I did not touch the T3 worktree or
~/.rust-dashcore-test, and I committed nothing. Python 3.14.4, git 2.53.0.

**Verdict: ACCEPT WITH MINOR FIXES.** There is no Blocker and no Major.
- **Fixed:** R1 (verified on real history, including two-level nesting), R2, R4, n1 and n2. Every targeted fix has a test that fails with the fix reverted.
- **R3:** fixed for every key round 2 named. The audit found two more git-behaviour leaks:
  - S1: `log.showSignature` crashes inventory with the "coverage gaps" exit code.
  - S2: a file that git diffs as binary reads "same" in the checker.

  Both are one-line fixes. S2 is the tool's worst failure mode: a silent false "same", exit 0 under `--strict`. It is latent, because no cited-extension file in rc.3→rc.4 or v23.1.0→rc.3 diffs as binary.
- **S3:** the stale sweep matches a broader name shape than the script creates. It also deletes user data if `CACHE_DIR` points at a shared directory.

## Findings

### Minor

**S1. `inventory.py:153,188,195,202,224,264,266,308,367-368`: `log.showSignature=true` crashes inventory with exit 1, the "coverage gaps" code.**
With that key set, every `git log`/`git show` of a signed commit writes the `gpg: Signature made …` block to
**stdout**, before the `--format` output. Core's first-parent commits are signed (`%G?` = `Y`). This is what happens
end to end, with repo-local `log.showSignature true` in a `--shared` clone, on rc.3→rc.4:

```
File "inventory.py", line 203, in first_parent_commits
    sha, parents, title = line.split("\t", 2)
ValueError: not enough values to unpack (expected 3, got 1)
```

- **Exit code.** The run exits 1, the code the docstring reserves for coverage gaps. `main()` (`:471`) maps only `CalledProcessError` to 2.
- **Other parsers.** `parse_numstat` (via the remerge-diff `show` at `:188`) and `backport_commits` (`:224`) would also break, or read gpg lines as data.
- **check-citations.** Unaffected: it runs no `log`, and `git show <rev>:<path>` of a blob prints no signature.
- **History.** This predates r3. It is in R3's scope, though: the comment at `:134` promises output that does not depend on config.

*Fix:*
- Add `--no-show-signature` to every `log` and `show` call. A `LOG_OPTS` constant next to `DIFF_OPTS` would keep this in one place.
- In `main()`, also map unexpected exceptions (`ValueError` at least) to exit 2, so that a parser surprise can never read as "gaps".
- *Test:* sign one fixture commit with an SSH key (`gpg.format=ssh`, `user.signingkey` set to a throwaway key from `ssh-keygen`, all repo-local), or mock `git_text`. Assert the tables are unchanged under `log.showSignature=true`.

**S2. `check-citations.py:51-52,97-117`: a cited file that git diffs as binary is judged "same" (false negative, exit 0 even with `--strict`).**
For such a file, `git diff -U0` prints the `diff --git` header and `Binary files a/x and b/x differ`, with no `@@`
hunk. `diff_hunks` records the path with an empty hunk list, so `judge()` returns "same" for every cite in it. A
deleted binary also prints no `+++ /dev/null`, so `deleted` misses it.

The trigger is whatever makes git treat a text file as binary:
- the Core checkout's `.gitattributes` (Core ships one, under `src/dashbls/depends/mimalloc/`, with `*.sln binary`);
- `$GIT_DIR/info/attributes` or `core.attributesFile`;
- a `diff.<driver>.binary=true` driver;
- a NUL byte in the first 8 KB.

Probe: `src/foo.cpp`, line 5 rewritten between `v1.0.0-rc.3` and `rc.4`, and a tree that cites `foo.cpp:5`:

| setting | verdict | `--strict` exit |
|---|---|---|
| none | changed | 1 |
| `*.cpp -diff` in `info/attributes` | **same** | **0** |
| `*.cpp diff=foo` + `diff.foo.binary=true` | **same** | **0** |

On real Core, `git diff --numstat` reports 0 binary-diffed files with a cited extension (`CORE_PATH`) in rc.3→rc.4
and in v23.1.0→rc.3, so no real report is wrong today.

*Fix:* add `--text` to `DIFF_OPTS`; then every file gets line hunks and deletions print `+++ /dev/null`. As a belt
and braces measure, raise `DiffFormatError` on a `Binary files … differ` line.

*Test:* write `*.cpp -diff` into the test Core repo's `.git/info/attributes` and assert the cite still reads changed.
Inventory's numstat shows such files as `-`→0 counts, which is cosmetic (the tier and coverage are unaffected).
`--text` there too would make the counts independent of the checkout. The rc.3→rc.4 `.sln` counts already differ
between a checkout and a `--no-checkout` clone.

**S3. `setup-dashd.py:139,142-156`: the sweep removes any entry named `.dashcore-*` or `.SHA256SUMS-*` that is older than an hour, not just the shapes this script creates.**
Within `cache_dir` it is safe:
- `lstat` plus the `islink` check unlink a symlink and never follow it;
- `rmtree` runs only on real directories;
- nothing outside `cache_dir` is touched.

`CACHE_DIR` is free-form, though (docstring `:22`). With `CACHE_DIR=$HOME`, or a shared scratch directory, a
user's `~/.dashcore-testnet/` or `~/.dashcore-backup/` (a plausible name for a Dash Core datadir copy) is
`rmtree`'d after an hour. Before r3 the script never deleted anything it had not created, except a `dashcore-<v>`
directory with no `bin/dashd` in it. CI uses `${{ github.workspace }}/.rust-dashcore-test`, and the default is a
dedicated directory, so this needs an unusual setup. The loss would be irreversible, and the fix is cheap.

*Fix:* match exactly what `mkstemp`/`mkdtemp` produce. For example:
- `^\.dashcore-.+\.[a-z0-9_]{8}(\.tar\.gz|\.zip)?$`
- `^\.SHA256SUMS-.+\.[a-z0-9_]{8}$`

(tempfile's names are 8 characters drawn from `[a-z0-9_]`.) Cleaner still: create all temp entries under a
dedicated `cache_dir/.setup-dashd-tmp/` and sweep only there. A rename within one filesystem stays atomic.
*Test:* add `.dashcore-backup/` (old mtime) to the `keep` list of
`test_leftovers_of_a_killed_run_are_swept_but_a_live_run_is_not`.

### Nit

- **n1. `inventory.py:188`: `merge.conflictStyle` changes the residual rows' counts, and with them the primary area and packet.**
  The remerge-diff's automatic merge honours the user's conflict style. For #7044's inner merge 491db4a481c:

  | style | src/ total | `deterministicmns.cpp` |
  |---|---|---|
  | default | −27 | −4 |
  | `zdiff3` (end to end) | −43 | (not measured) |
  | `diff3` | (not measured) | **−343** |

  Under `diff3`, evo outweighs consensus, so the row's primary area moves from consensus to evo and it changes packet. Coverage is unaffected: the row stays `review` and must be cited.
  *Fix:* `git -c merge.conflictStyle=merge show --remerge-diff …`.

  The same class, cosmetic only:
  - `core.abbrev=12` changes every `%h` (`:308`, `:367-368`). That is 1,186 changed lines in `commits.tsv` on v23.0.0→v23.1.0. Use `%H` and slice, or `--abbrev=10`.
  - `:266` `--name-only` follows `diff.renames` (the `old_side_commits` column).
  - `merge.renames=false`, `merge.directoryRenames=false`, `diff.renames=copies` and `log.follow=true` changed nothing observable.
- **n2. Tests: two pins survive their revert.**
  - `check-citations.py:52` with `--diff-algorithm=myers` removed passes all 16 tests. The histogram leg of `test_hunk_fusing_and_the_algorithm_do_not_change_the_verdicts` (`test-check-citations.py:217`) does not separate the algorithms on its file. Here is a ready fixture where it matters, in the **false-"same"** direction:
    - old `{ } a { } a d { } { b d`
    - new `{ } a { } { d } { b d` (one line each)
    - cite `h.cpp:7`
    - myers gives `-6,2 +5,0` / `-8,0 +7`, which reads **changed**; histogram gives `-6 +6` / `-8 +7,0`, which reads **same**.

    With the pin removed and repo-local `diff.algorithm=histogram`, the verdict flips changed→same. The shipped code reads changed under both.
  - In inventory, removing `--diff-algorithm=myers` or `--inter-hunk-context=0`, or using `DIFF_OPTS` instead of `PATCH_OPTS` in `patch_ids`, also survives. B0/B1 give the same counts under every algorithm. Patch-ids are computed the same way on both sides, so `-U` and fusing cancel out. These pins are defensive and harmless. Only the algorithm pin could ever show, and then only in counts.
- **n3. `inventory.py:355` with `:414`: an untitled inner merge under an umbrella that has no PR number is labelled with the outer merge's sha, but coverage counts the inner sha.**
  The packet line and the summary's "not cited" list print `#<outer sha10>`. A report that cites that label stays uncovered. The result is conservative but confusing. Core's backport merges always have a number, so I found no real case.
  *Fix:* `add_row(side, mpr, mpr if mpr != "-" else msha[:10], …)`.
- **n4. `setup-dashd.py:135-137,181`: the staging directory is created before both downloads and is not written until extraction.**
  The comment says "a live run keeps writing to its files, and an hour is far longer than its download timeouts". But `timeout=300` is per read, not total: a 200 MB archive at under ~55 KB/s takes over an hour. So a concurrent run can sweep a live run's still-empty staging directory.
  I checked that this is benign: `tarfile.extractall(filter="data")` recreates a missing destination, which I verified. The only real hazard is a sweep that starts in the microseconds before the first member lands and then races the extraction. That needs both a download over an hour and that timing, and even then `bin/dashd` ends up complete or absent, never half-written. Some sibling files might be missing.
  *Fix:* call `mkdtemp` after `verify_sha256`, and correct the comment.
- **n5. Inner merges already in the old pin get residual rows too.** On v23.0.0→v23.1.0, #7044's range holds #6948, which *is* v23.0.0, plus #6932, #7026 and #7033. Their remerge-diffs are empty, so they add no rows today. A non-empty one would add a row for a change the old pin already has. That is conservative noise, not a gap. A one-line docstring note, or `^old` in `merges_in`, would settle it.

## Checks

### 1. R1 on real history

- **v23.0.0→v23.1.0, split mode (the default): exit 0.** #7044 still splits into 44 rows, and gains a new residual row:
  `new 7044 491db4a481 review consensus consensus,evo,wallet 0 27 4 - Merge branch 'master' into merge_master_23.0.2 (changes made by the merge itself)`.
  The consensus packet lists exactly `src/chainparams.cpp +0/-14`, `src/evo/deterministicmns.cpp +0/-4`,
  `src/evo/specialtxman.cpp +0/-5` and `src/wallet/interfaces.cpp +0/-4`. That matches `git show --remerge-diff --numstat 491db4a481c`.
  `files.tsv` now names `7044` on all four.
- **Coverage.** A report citing every row label except #7044 (every sub-PR plus every direct sha) exits **1**, with
  exactly one uncovered row, `#7044 [consensus] … (changes made by the merge itself)`. Adding `#7044` gives exit **0**.
- **Diff against r2 (b03161563) on the same range.**
  - `prs.tsv`: only the new #7044 row (review 180→181, consensus packet 6→7).
  - `commits.tsv`: identical.
  - `files.tsv`: `7044` added where the inner merge touched the file, and duplicate labels removed (`6920,6920`→`6920`). The duplicates were r2 noise. The dedup is cosmetic: with it reverted, no targeted test fails.
- **rc.3→rc.4: exit 0, 40 rows, 0 residual rows.** `prs.tsv`, `files.tsv`, `commits.tsv`, `summary.md` and `packets/` are byte-identical to r2's.
- **Fixture faithfulness.** Real #7044: P2 = 491db4a481c, whose P1 09d533a2833 is on develop (an ancestor of
  #7044's P1) and whose P2 cdc5a63e52e is master's #7033. The fixture's #900: P2 = the inner merge of `rel`
  (cherry-pick #101) and `side`.
  - The parent layout differs: real is develop-vs-master, the fixture is rel-vs-side.
  - The property under test is the same: the resolution exists only in the inner merge's remerge-diff, and the outer merge's remerge-diff is empty. It runs the same code path (`merges_in` → `merge_own_numstat`).
  - The fixture adds the untouched-file subcase (`u.cpp`).

  I judge it faithful, and the real-history run above backs it.
- **Nesting deeper than one level.** My probe had `side2` merged into `side` with an edit to `y.cpp` (untitled), `side` merged into `rel` (clean, untitled), and `rel` merged as #900.
  The two-deep merge gets its own `900` residual row. Citing #101 and the two direct shas gives exit 1, uncovered
  `#900 … Merge branch 'side2' into side`. `git log --merges P1..M` lists every depth, so this follows by construction.
- **An inner merge with no `Merge #N` title** falls back to the umbrella number. #7044's own inner merge is that case. A titled inner merge takes its own number (`test_an_inner_merge_with_its_own_pr_number_is_that_prs_row`).

### 2. R2

- **Octopus.**
  - A first-parent octopus gives one row with `via -`, no split and exit 0 (`test_an_octopus_merge_is_one_row_and_does_not_crash`).
  - An octopus inside the range disables the split: one row, exit 0.
  - `--merges` includes octopus merges, so the `len(p) != 2` guard sees every depth.
- **A non-merge commit titled `Merge #700`** is one row, and `files.tsv` reads `700` with no duplicate.
- **Exit codes.** `CalledProcessError` → 2, gaps → 1, clean → 0. The one remaining way to get a stray 1 is an uncaught parser exception (S1).

### 3. R3: the git calls

- **check-citations.** It has one diff call (`:103`, `DIFF_OPTS` + `-U0`).
  - The remaining calls are `ls-tree --name-only`, `show <rev>:<path>` (a blob, which gets no textconv unless `--textconv` is given), `ls-files -z` and `rev-parse`. None depends on the keys I tested, apart from attributes (S2).
  - `diff.interHunkContext`, `diff.algorithm` and `diff.indentHeuristic` are now covered.
  - I built the histogram-vs-myers case the implementer could not (n2). The shipped code is correct on it.
- **inventory.**
  - The diff calls are covered: `numstat` and `merge_own_numstat` take `DIFF_OPTS`; `patch_ids` and `reverse_applies` take `PATCH_OPTS` (adds `-U3`).
  - `diff.context=0`, `diff.algorithm=histogram` and `diff.interHunkContext=5` are now in the config test, which compares all three tables byte for byte.
  - The leftovers, all outside the diff options: `log.showSignature` (S1), `merge.conflictStyle` (n1), `core.abbrev` (n1) and attributes (S2, counts only).

### 4. R4 and the nits

- **What the sweep removes.** It touches only the names starting `.dashcore-` or `.SHA256SUMS-`. That covers all three temp shapes, because the archive is `.dashcore-<v>-<arch>….XXXXXXXX.tar.gz`.
  - The regtest data archives (`regtest-*.tar.gz`) and other hidden files are kept. The test covers that.
  - Prefix breadth: S3.
- **A concurrent live run.**
  - The archive is written continuously while it downloads.
  - SHA256SUMS is read immediately after its download, and `finally` tolerates its removal.
  - The staging directory can be swept while idle, which is benign (n4).

  A live run's verified archive is never deleted mid-extraction: extraction takes seconds after a write.
- **Outside the cache directory.** Nothing is removed outside it. Symlinks are unlinked, not followed, and `rmtree` refuses symlinks even in a race.
- **n1 (staged-binary check).** `test_an_archive_without_dashd_installs_nothing` fails with the staged check removed: `os.rename` is mocked to raise `AssertionError`, which escapes `except OSError` and fails `assertRaises(SystemExit)`.
- **n2.** The transitional hazard is documented in the docstring (`:12-14`).

### 5. Tests against reverted fixes (16 targeted mutants)

Mutants ran on copies of each script and its test file. In those copies `test_the_recorded_merge` and
`test_the_recorded_rc4_list` fail by construction, because their `testdata/` was not copied; an unmutated control
copy fails the same way. I discounted those two.

| mutant | result (targeted tests) |
|---|---|
| cc: drop `--inter-hunk-context=0` | killed: `test_hunk_fusing_and_the_algorithm_do_not_change_the_verdicts` |
| cc: drop `--diff-algorithm=myers` | **survived** (n2) |
| inv: outer merge's remerge only (r2 behaviour) | killed: `test_a_merge_inside_the_range_keeps_its_own_edits`, `test_an_inner_merge_with_its_own_pr_number_is_that_prs_row` |
| inv: drop the `mg[0] != sha` filter (outer merge counted twice) | killed: `test_changes_made_by_the_merge_itself_are_kept`, `test_a_merge_edit_in_a_file_a_sub_pr_touched_is_kept` |
| inv: inner merge always under the umbrella | killed: `test_an_inner_merge_with_its_own_pr_number_is_that_prs_row` |
| inv: drop `len(parents) != 2` | killed: `test_a_plain_commit_titled_merge_is_one_row` |
| inv: drop the inner-octopus check | killed: `test_a_range_holding_an_octopus_merge_is_not_split` |
| inv: `reverse_applies` without `-U3` | killed: `test_user_diff_config_does_not_change_the_results` |
| inv: `patch_ids` without `-U3`; drop algorithm; drop inter-hunk; drop the `file_prs` dedup | survived (harmless, n2) |
| sd: no sweep call | killed: `test_leftovers_of_a_killed_run_are_swept_but_a_live_run_is_not` |
| sd: no age check | killed: same test (`live` entries removed) |
| sd: sweep any hidden name | killed: same test (`.hidden-other` removed) |
| sd: no staged `bin/dashd` check | killed: `test_an_archive_without_dashd_installs_nothing` |

No test is tautological. Every assertion I reverted against compares real tool output with a concrete expected value.

## Runs

| run | result |
|---|---|
| `test-inventory.py` | 17 OK (42 s) |
| `test-check-citations.py` | 16 OK |
| `test-setup-dashd.py` | 19 OK |
| inventory v23.0.0→v23.1.0 (split) | exit 0; #7044 inner-merge row as above |
| … with `--report` citing the sub-PRs only / plus #7044 | exit 1 / exit 0 |
| inventory rc.3→rc.4 | exit 0, 40 rows, identical to r2 |
| `log.showSignature=true` (repo-local), rc.3→rc.4 | traceback, exit 1 (S1) |
| `merge.conflictStyle=zdiff3` / `log.follow`+`core.abbrev=12`+`diff.renames=copies`, v23 range | counts, abbreviations only (n1) |

Per the brief, I ran no selftest shards. The implementer ran 331–333 green.
