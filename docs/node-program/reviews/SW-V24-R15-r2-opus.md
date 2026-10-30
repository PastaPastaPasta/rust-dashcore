# SW-V24 R15 r2: Opus re-check of the round-1 fixes

**Branch:** `node/swv24-r15`, fix range `ef3196dee..b03161563`. All six commits are signed `G`.
**Round 1:** reviews/SW-V24-R15-r1-opus.md.
**Core:** ~/workspace/dash. Tags v24.0.0-rc.3 (0364ed626e) and v24.0.0-rc.4 (0e7382df80). I also used
v24.0.0-rc.2, v23.0.0, v23.1.0 and v23.1.2 to probe the fixes for regressions.

I reviewed a detached `git worktree add` copy at `/work/scratch/swv24-r15-recheck`. The mutants and probes ran
in `/work/scratch/swv24-r15-recheck.mut`. Both are deleted. I did not touch the T3 worktree and committed nothing.
The config probes used repo-local config in a `--shared` scratch clone of Core. (The git guard blocks `GIT_CONFIG_*`
in my own commands. The unit tests still set them.)

**Verdict: CHANGES REQUIRED.** M1, M2 and all four minors are fixed, and every new test fails with its fix
reverted. M3 is fixed for the case round 1 described: a conflict resolution in the two-parent backport merge itself.
It is **not** fixed when the resolution sits in a merge *inside* the backport range, and real Core history has
exactly that case (Major R1 below). The octopus case crashes (R2).

## Findings

### Major

**R1. `inventory.py:316-327`: in split mode, an edit made by a merge *inside* the split range is in no row, so citing only the sub-PRs passes coverage.**
`backport_commits` lists `--no-merges` commits only (`:200-212`). `merge_own_numstat` (`:179`) takes the
remerge-diff of the outer merge. An inner merge's resolution is part of the outer merge's second parent, so the
outer merge's automatic merge already contains it. The resolution is therefore in no sub-commit row and not in the
residual.

*Real case: v23.0.0→v23.1.0.*
- `Merge #7044: chore: merge master 23.0.2 back into develop` contains `491db4a481c Merge branch 'master' into merge_master_23.0.2`.
- That merge's `--remerge-diff` has conflict resolutions in 12 files. Four of them are runtime files: `src/chainparams.cpp`, `src/evo/deterministicmns.cpp`, `src/evo/specialtxman.cpp` and `src/wallet/interfaces.cpp`. For example, `deterministicmns.cpp:1854` keeps `current_list = next_list;` over the release branch's `std::move`.
- With `--no-split`, #7044 is a review row with 4 net files, so a report has to cite it.
- In split mode (the default), #7044 has no row at all. It shows only as `via` on 44 sub-rows.

*Synthetic case.* A `nested` probe repo has an inner `Merge branch side` that adds `EVIL` to a file no sub-commit touched:

| version | exit | outcome |
|---|---|---|
| r2 split | 0 | covered; `files.tsv` gives `src/evo/u.cpp=log:<inner sha>` |
| r1 split | 1 | uncovered: #900 residual |
| `--no-split` | 1 | uncovered |

So r2 also regresses r1 on the untouched-file subcase. The touched-file subcase (the real #7044) was missed by r1
as well. I missed it in round 1, so this is not new code, but the M3 fix leaves it open.

"Merge master back into develop" merges follow every Core release, so the next pin delta past v24.0.0 is likely to
include one.

*Fix:* for a split merge, take the residual from the remerge-diff of every merge in `parents[0]..sha`, not just the
outer one (`git rev-list --merges parents[0]..sha`). Give each inner merge its own residual row, under its
`Merge #N` number when it has one and under the umbrella PR otherwise. Alternatively, do not split a merge whose
range contains merges. Add the `nested` shape to `backport_repo` (an inner merge with an edit to a file a sub-PR
touched) and assert exit 1 when the report cites only the sub-PRs.

### Minor

**R2. `inventory.py:179-184`, `:163`: an octopus merge crashes split mode, and a non-merge `Merge #N` commit gains a duplicate residual row.**

- **Octopus.** `git show --remerge-diff` prints `diff: warning: Skipping remerge-diff for octopus merges.` on stdout. `parse_numstat` then fails at `:163` with `ValueError: not enough values to unpack`. The result is a traceback with **exit 1**, the code that means "coverage gaps" (docstring `:59`). This happens for an evil octopus and for a clean one. r1 handled the clean octopus (exit 0) and caught the evil one (exit 1, residual). `--no-split` handles both. Core history has no octopus merges in the ranges I checked (rc.2→rc.4, v22.0.0→v22.1.0, v23.0.0→v23.1.2).
- **Non-merge `Merge #N` commit.** On the first-parent line, `backport_commits(parents[0], sha)` splits a non-merge commit with a `Merge #N:` subject into one sub-row. `merge_own_numstat` then shows that commit's ordinary diff and adds a second residual row with the same PR. The probe `ours` shows `700 review 700 …` plus `700 review - … (changes made by the merge itself)`, and `files.tsv` reads `700,700`. Coverage is unaffected, because the PR number is the same. The real ranges I checked have none of these commits.

*Fix:* split only when `len(parents) == 2`, and give anything else one row, as `--no-split` does. If octopus merges should ever split, refuse them with exit 2 instead of crashing. Add tests for the octopus and non-merge shapes.

**R3. `check-citations.py:50`, `inventory.py:134,257`: some diff config keys still change the results.**
The comment on `DIFF_OPTS` says "Diff output must not depend on the user's git config". The keys round 1 named
(noprefix, mnemonicPrefix) no longer change anything, and `diff.context` does not affect check-citations
(`-U0` is explicit). On the real pins (rc.3→rc.4) against this tree, these keys still change the output:

| config | tool | effect |
|---|---|---|
| `diff.interHunkContext=5` | check-citations | changed 368→373, ambiguous 12→14, shifted 2033→2028 |
| `diff.algorithm=histogram` | check-citations | changed 368→367 |
| `diff.algorithm=histogram` | inventory | the add/del columns of `prs.tsv` change (#7537, #7909, …) |
| `diff.context=0` | inventory | `reverse_applies` makes a zero-context patch, `git apply --check -R` refuses it, and the "tree" signal disappears |

The "same" count does not change in any check-citations case, because the knobs only move cites between changed
and shifted. So there is no false "same". The inventory `diff.context=0` case was confirmed end to end: a 20-line
file with a commit pair that undoes itself gives #104 `tree`, then `no`. `diff.context=1` and `7` are fine.
`diff.indentHeuristic=false` changes nothing.

*Fix:* add `--diff-algorithm=myers` and `--inter-hunk-context=0` to both `DIFF_OPTS` lists, and `-U3` to the
`reverse_applies` diff. Extend the two config tests with `diff.algorithm=histogram`, `diff.interHunkContext=5` and
(for inventory) `diff.context=0` on a multi-line file.

**R4. `setup-dashd.py:146-175`: a killed run leaves its temp files in the cache forever.**
The unique names fix the race, but on SIGKILL or SIGTERM the `finally` does not run. (Python has no SIGTERM handler,
and test-runner and CI timeouts send SIGTERM or SIGKILL.) The run then leaves `.SHA256SUMS-<v>.XXXX`,
`.<asset>.XXXX.tar.gz` and the `.dashcore-<v>.XXXX/` staging tree, about 200 MB for a real release. Nothing removes
them. I confirmed this with real processes: after SIGKILL, a successful rerun leaves all three hidden entries next
to the new `dashcore-<v>`. Before the fix, the fixed names were overwritten by the next run, so this is a small
disk-hygiene regression. SIGINT is cleaned correctly. A killed run never leaves a trusted tree (see below).

*Fix:* at the start of `setup_dashd`, remove `.dashcore-<v>.*`, `.<asset>.*` and `.SHA256SUMS-<v>.*` entries
older than about an hour. (Use age, not mere presence, so a live concurrent run's files survive.) Or install a
SIGTERM handler that calls `sys.exit`.

### Nit

- **n1. `setup-dashd.py:160-162`.** The staged `bin/dashd` check survives mutation, because no test feeds an archive without `bin/dashd`. Without the check, the tree is renamed into place and the final check at `:178` exits. The next run then deletes that tree (`:163-164`), so it is never trusted. A test with "an archive with no dashd installs nothing" would pin the check.
- **n2. `setup-dashd.py:163-164`.** These hazards are transitional, not new. A partial tree left by the *old* script that already holds `bin/dashd` is still trusted, because the cache test is unchanged. A pre-fix `setup-dashd.py` in another worktree, extracting straight into the shared cache, can also race the stale-tree `rmtree`. Once every worktree has the fix, both are gone. Worth one line in the commit note or docstring.

## Per-finding status

Each "revert" below is a targeted mutant of the fix, run against the test file, 21 in all.

| r1 item | fixed? | new test fails with fix reverted? | regression? |
|---|---|---|---|
| **M1** prefixes, check-citations | yes | yes: `test_user_diff_config_does_not_change_the_verdicts` | none. On real pins, `noprefix`+`mnemonicPrefix`+`context=7`, `context=0` and `context=1` reports are byte-identical to plain (see R3 for other keys) |
| M1 header-less hunk / unreadable header → exit 2 | yes | yes: both raises reverted one at a time; `test_a_diff_it_cannot_read_stops_the_run`, `test_the_run_exits_2_on_a_diff_it_cannot_read` | none. The `git` wrapper test shows exit 2 with "hunk header" on stderr |
| M1 prefixes, inventory (`reverse_applies`) | yes | yes: `test_user_diff_config_does_not_hide_the_tree_signal` | `diff.context=0` still breaks it (R3) |
| **M2** en- and em-dash ranges | yes | yes: `LINES` reverted and `parse_ranges` reverted, each fails 3–5 tests | none. On real pins, `BSX-r1-sol.md:101 netinfo.cpp:596–615` and `:103 providertx.cpp:318–324,391–392` now read **changed** |
| **M3** residual from the merge's own edits | **partly** | yes: the r1 "untouched files" residual put back fails `test_a_merge_edit_in_a_file_a_sub_pr_touched_is_kept`. With `residual=False`, the commits.tsv assertion fails | Conflict probe (resolution in a file a sub-PR touched): r1 exit 0, r2 and `--no-split` exit 1. Correct. Empty merge (commits that cancel): no residual, exit 0. Correct. `-s ours` merge: residual `no-net-delta` #900 must be cited. That is conservative, and acceptable. Octopus: crash (R2). Inner merge: missed (R1). On real ranges rc.3→rc.4, rc.2→rc.3 and v23.1.0→v23.1.2, `prs.tsv`, `files.tsv` and `commits.tsv` are byte-identical to r1 |
| **m1** setup-dashd staging | yes | yes: 6 of 7 mutants killed (extract into the cache, fixed archive name, no staging `rmtree`, no stale-tree `rmtree`, always re-raise on rename, zip suffix). The staged-bin check survives (n1) | see the setup-dashd section below; R4 |
| **m2** "another pin" = Core releases only | yes | yes: the old `PIN_RE` reverted, and the `Core`/`dashd` lead-in dropped | none found (below) |
| **m3** tests: tautology plus 3 mutants | yes | yes: the past-end filter, `deleted.add` and `cherry[-1]` mutants are now killed. The #7871 assertion takes the recorded subject and its trailer | none |
| **m4** count the cites that are not judged | yes | yes: with `unscoped[rel] += 1` removed, `test_report` fails | none. On real pins: "4465 Core citations in 529 files that never name `rc.3` (**not judged**)", plus a top-15 section |
| n2 (residual lists no sub-commits) | yes | yes: with `residual=False`, the commits.tsv assertion fails | none |

### setup-dashd

**Concurrency.** I ran two real processes against one cache with a 1.5 s slow extraction. Both returned the same
`dashcore-23.1.0/bin/dashd`. The loser logged "installed by another run", the winner's files stayed, and the cache
held only `dashcore-23.1.0`.

**Killed runs.** After a SIGKILL or SIGTERM during extraction, with `bin/dashd` already written to staging, there
was no `dashcore-<v>` directory, and the next run installed cleanly. The leftover temp files are covered in R4.

**Archive handling.**
- `tar` extraction still uses `filter="data"` (`:129`). `extract()` is unchanged.
- The zip path keeps its `.zip` suffix, and a test covers it.

**Env-var output.** It is unchanged. `main()` and its three `print` lines are not in the diff, and `setup_dashd`
returns the same `dashd_bin` path on every success path, including the race path. The existing tests check both
the `export` and GITHUB_ENV forms.

### "another pin" against the real tree

**Cites the new rule moved.** I classified every cite line in the 1,099 files that name rc.3 under both rules:
- 223 cites move from "other" to judged, and none move the other way.
- 170 of the dropped tokens are section numbers, crate versions or IP addresses (`127.0.0`, `2.10.1`, `3.5.5`, …).
- The 19 Core-shaped dropped tokens are user-agent strings, IP addresses or library versions, not pins:
  - user-agent strings (`/Dash Core:23.1.8/`, UA `23.1.8`→`24.0.0`),
  - IP addresses (`13.9.0.1`, `11.0.0.x`, `10.0.0.x`),
  - library versions (RocksDB `11.8.1`, `13.3.0`/`15.2.0`).

**Tokens the new rule still reads as pins.** Every one is a Core release: `rc.N`, `v23.1.8`, `v24.0.0-rc.N`,
`v24.1.0`, `v23.1.0`, `19.2.0`, and the rest. The one exception is the test fixture's `v0.9.0`.

## Runs

| run | result |
|---|---|
| `test-inventory.py` | 12 OK |
| `test-check-citations.py` | 15 OK |
| `test-setup-dashd.py` | 17 OK |
| selftest `--shard 331/1000` | PASS (13 ok, 0 fail, 1 skip; 42 min) |
| selftest `--shard 332/1000` | PASS (13 ok, 0 fail, 1 skip; 31 min) |
| selftest `--shard 333/1000` | PASS (13 ok, 0 fail, 1 skip; 28 min) |

Each shard also runs the clean-tree baseline cases, which is where most of the time goes (box load was 50–140). Python 3.14.4, git 2.53.0. No full selftest and no gates.
