# DW-DP1-02-f1 independent Sol review, round 2

Reviewed `56c974d..2e79978d` (f1) and `58b5510..0546a62` (f2) in private detached worktrees. All three reviewed commits verify `G`. The implementer's worktree was not changed.

## Closure

| Item | Result | Evidence |
| --- | --- | --- |
| Round 1 **High**: an old release answer is taken for a new marker after erase/re-add | **Closed** | `fence.rs:3416-3420` captures the marker sequence and `:3465-3469` requires that exact sequence at completion, as well as `call_marked`. The round 1 post-commit-delay scenario, transplanted as a test onto `56c974d`, failed because the old answer returned success; it passed on `2e79978d` and then passed 20/20 repeated runs. The test checks the new marker has no inherited release time or row and later writes its own release row (`mode_b_tests.rs:3124-3178`). |
| Removing `supersedes` | **Safe for this invariant** | The former field's only operative uses were increment on definite supersession, removal on erase, capture at release start, and comparison at completion. Exact `marker_seqs` equality subsumes them; `NotSent` removes the old sequence (`fence.rs:1210-1214`) and erase forgets it (`:1547-1550`). New marker writes replace it with a new `step_log.seq`. |
| Sequence non-reuse | **Confirmed for supported journals** | `STEP_LOG` declares `seq INTEGER PRIMARY KEY AUTOINCREMENT` (`dispatch.rs:118-127`). Both a fresh journal and v1 migration execute that definition (`:884-886`, `:919-922`); `bc40ab6d`, the original `step_log` commit, already had it. V2–v7 migrations do not rebuild `step_log`. Compaction/erase delete rows inside transactions, with no `sqlite_sequence` reset or production `VACUUM` path. A local SQLite probe gave sequences 1, 2, 3 across delete, `VACUUM`, and reopen. `.dwbackup` exports/imports app rows and vault bundles, not `dispatch.sqlite`; Core/seed imports likewise do not replace the journal. |
| f2 Finish window and old flake | **Confirmed** | `enter_funding` sets `in_call` before `funding_permit`; the fake's `permit_hold` sets `hung` before obtaining the permit (`registration_flow.rs:1761`, `:1865-1885`; `registration_tests.rs:640-652`). No marker means the recovered unindexed lock is `Free`; `may_take(Row)` refuses the calling row, `Recovery` may take it, and `next_index` chooses one past the current maximum (`registration_ledger.rs:609-627`, `:690-695`, `:913-927`). Finish adopts the existing lock into a separate recovery row; the call later funds one distinct lock at its own index (`registration_flow.rs:1194-1258`). The old test now waits until the library call, after the marker, so its one-row assertion applies to the intended contested state; it passed 10/10 repeated runs. |
| New pre-marker test | **Effective** | `rules.rs:400-445` asserts `in_call`, no step marker, the calling row untouched by Finish, a separate recovery row at a different index, the old lock held only there, one later funding call, and a distinct new outpoint. Removing `¬calling` or making `next_index` reuse the maximum each fails this test. Adoption of the pre-existing lock is no new funding operation; the later call is the single new funding operation. |

## New findings

| Severity | Location | Probe / impact | Fix |
| --- | --- | --- | --- |
| None in these changes | — | The requested race and release paths behaved as designed. | — |

possible security note: see manager — `rust/crates/dw-engine/src/platform/registration_ledger.rs:927`.

## Independent mutation and probe evidence

Each source mutant was applied singly from a saved copy. After each run, the file was copied back, touched and `cmp`-verified before the next mutation. The restored f2 control passed, and all detached worktrees were clean.

| Probe / mutation | Focused test | Result |
| --- | --- | --- |
| Round 1 post-commit response delay, applied as test-only code to `56c974d` | `a_release_answering_after_erase_never_answers_the_new_marker` | **Exposed old bug**, exit 101: the old marker's answer returned `Ok` (assertion at `mode_b_tests.rs:3166`); same test on fixed head passed, including 20/20 repetitions. |
| Remove `!self.calling(id)` from the `Free` row rule | `finish_before_the_marker_gives_an_unindexed_lock_to_recovery` | **Killed**, exit 101: Finish attached the recovered lock to the calling row (`FundingSent`, old outpoint), instead of leaving it in `KeysPrepared`. |
| Change `next_index` from `max + 1` to `max` | same test | **Killed**, exit 101: recovery was assigned the calling row's index `2`; assertion at `rules.rs:437` failed. |

The implementer's four reported f1 mutants were not repeated; these two f2 mutants are independent additions to that evidence.

## Validation

- Fixed f1 head: `dw-engine` library suite **706 passed, 4 ignored, 0 failed**; `dw-appdb` library suite **72 passed, 0 failed**. The release-focused engine subset passed **25/25**; appdb release tests passed **2/2**. The new erase/re-add regression passed **20/20** repetitions with its fully qualified exact name.
- f2 head: new pre-marker control passed before mutation and again after restoration; revised post-marker test passed **10/10** repetitions. Both independent mutants failed at their intended assertions.
- `cargo fmt --all -- --check` and `git diff --check` passed on both reviewed ranges. Builds/tests used `nice -n 10`, `CARGO_BUILD_JOBS=12`, private TMPDIR and CARGO_TARGET_DIR, and `systemd-run --user` for long runs. All review-only source edits were restored. The implementer's reported 1,283-test gate was not rerun as an identical command; the relevant engine/appdb suites above passed independently.

**Verdict:** APPROVE
Sol gate satisfied: yes
