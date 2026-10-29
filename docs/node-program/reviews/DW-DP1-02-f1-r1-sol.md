# DW-DP1-02-f1 independent Sol review, round 1

Reviewed `58b5510..56c974d` in a detached review worktree. All three commits (`bb49280`, `35ca942`, `56c974d`) verify `G`. The unmodified worktree was clean before review and after every review-only edit was restored.

## Residual 2 closure

**Partly closed; one blocking path remains.** The appdb write now names the marker `seq` returned by `insert_funding_step` and checks that the exact marker still stands in an IMMEDIATE transaction. A release delayed until after `NotSent` and a new marker returns `None`; neither the new marker nor its DEC-197 time is polluted on disk. The new appdb and engine regressions pass, including reload, and a source mutant that substitutes the current marker fails the appdb regression.

A release that has *committed* under the old marker but whose result returns after wallet erasure/re-add can still be attributed to the new marker **in memory**. `forget_erased` clears the supersede counter as well as the old marker sequence. The new marker starts with the same counter value as the old call captured. The completion check compares only that counter and whether *a* marker stands, then inserts the old release time into `released`. The new call's `record_release` sees `released` and returns without writing its own `funding_release` row. Thus the release durability gate can approve a new signature without the new marker's release row, and the in-process DEC-197 time may start from the old call. Reloading would remove the false time, but cannot repair a signature already released under this state.

## Findings

| Severity | Location | Probe and impact | Fix |
| --- | --- | --- | --- |
| **High — blocking** | `rust/crates/dw-engine/src/platform/lease/fence.rs:1549-1552`, `:3467-3485` | Review-only fake journal held the response **after** the old release had been stored. The old call ended `Cancelled`; `NotSent` superseded it; `erase_wallet_rows` compacted the old marker and release; the wallet was opened again; a new marker was written; then the old response returned. On `56c974d`, the probe failed with `old=Ok(()), memory=Some(1791793691), disk=[]`. A subsequent release call for the new marker returned `Ok` while `disk=[]` remained. The sequence is eligible for a real wallet removal: the permit is finished before erasure, and erasure does not wait for unrelated journal writes. | At release completion, require the current `marker_seqs[(wallet, step)]` to equal the `marker` captured at start, in addition to the existing checks. Add a regression holding the backend result after commit through erasure and re-add. A review-only candidate adding this comparison made the same probe pass; it was restored and not committed. |

## Path and invariant checks

- **Cancel → new marker / `NotSent`:** the appdb transaction serializes release and resolution and compares the exact sequence. If release wins, the later `NotSent` removes it from standing rows. If resolution wins, the old release returns `None`. The existing new tests cover the latter order. The finding is the later *response* after an earlier commit plus forgetting the generation.
- **Forget:** appdb compaction removes the old side row with the old marker. In memory, forgetting also resets `supersedes` and `marker_seqs`; the former can collide with a new marker's generation. This is the open path.
- **Restart / marker write with no returned sequence:** loaded markers do not populate `marker_seqs`. The funding gate refuses a funding call over any standing marker (`mode_b.rs:842-855`), and `record_release` refuses a call with no known sequence. A marker write that committed but did not return cannot hand out a call permit (`call.rs:623-643`). A committed release row loads with its own marker and stored `at`; an unrecorded one supplies no release time. These paths fail toward a held/unknown commitment, not an early DEC-197 override.
- **Two calls:** the second call sees the first call's in-memory `Committing` or durable marker inside the J step and the funding gate refuses it. After a definite `NotSent`, a later call writes a different marker sequence. The known exception requires erasure/re-add to reset the in-memory generation while an old release result remains in flight.
- **P2b / DEC-191:** the diff changes no schema or migration. Mode B step-ledger and call-permit wiring remain; the marker transaction returns its sequence only after commit, and the release SQL keeps the first stored `at`. The `debug_assert` in `call_step_mark` holds for production funding calls because `funding_gate` refuses a standing marker before taking a permit. The blocking finding breaks the release-row durability invariant only on the forget path.
- **`35ca942` test race:** `registration_flow.rs:1761` sets `in_call` before `funding_permit` writes its marker. The fake library sets `hung` inside `fund`, after a permit exists; waiting for `lib.hung` therefore establishes the intended ordering. The revised test passed. The separately assigned flaky test investigation was outside this review.

## Independent mutation and probe evidence

Each source mutant was applied alone to a saved file. After its focused test, the saved file was copied back, touched, and `cmp`-verified before the next mutant. All unmodified focused controls passed after restoration.

| Mutation | Focused test | Result |
| --- | --- | --- |
| appdb substitutes the latest standing marker for the caller's sequence | `a_late_release_never_answers_the_next_marker` | **Killed**, exit 101: got `Some(4)` instead of `None` |
| appdb returns the new call's `now` instead of the stored row's `at` | `a_release_answers_its_marker_once` | **Killed**, exit 101: got `Some(4)` instead of first time `Some(3)` |
| engine omits `marker_seqs` insertion after the durable marker write | `r6_01_a_release_ends_with_its_marker` | **Killed**, exit 101: release refused because its sequence is unknown |
| review-only post-commit return delay through erase/re-add | `review_erase_can_relabel_old_release_in_memory` | **Exposed finding**, exit 101 on reviewed head; **passed** with sequence equality added to completion guard |

## Validation

- `cargo test -p dw-appdb -p dw-engine --lib`: exit 0; appdb **72 passed**, engine **705 passed, 4 ignored**. Both new late-release regressions and the revised funding-call ordering test passed. The long stress mutation test passed. The first review-only probe command used `--exact` with a short name and filtered out all tests; it was rerun without that flag, with one test executed.
- `cargo clippy --workspace --all-targets -- -D warnings`: exit 0. `cargo fmt --all -- --check`: exit 0. `git diff --check 58b5510..56c974d`: exit 0.
- Review-only probe, candidate fix and mutants were removed. Original source copies were `cmp`-verified after restoration; the detached worktree was clean. Builds used `nice -n 10`, `CARGO_BUILD_JOBS=12`, a private target and mode-0700 TMPDIR, with long runs in `systemd-run --user`. No implementer worktree, commit, push or GitHub item was changed.

**Verdict:** CHANGES-REQUIRED
Sol gate satisfied: no
