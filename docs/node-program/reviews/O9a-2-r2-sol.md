# O9a-2 independent Sol review — round 2

Reviewed the narrow fix range `7a9b1a125..ada497e4f` at exact head `ada497e4f` in a detached worktree. No implementer worktree, GitHub, full gate or live oracle was used.

## Findings

### Low

1. **D21's git provenance applies to the checked-in fixture, not every CLI input.** `conformance/src/main.rs:647-659` accepts any canonicalized node directory; `conformance/src/digest/replay.rs:148-160` takes the file hashes and summary from that directory's `extract.json`. In an untracked copy of `feature_dip3_deterministicmns/node0`, I shortened `blocks.dat` and the golden to genesis, then rewrote their hashes and `summary.tip=0`, `summary.blocks=1` in that copy's `extract.json`. `digest replay` exited **0** and wrote a genesis-only digest. This is the residual D21 explicitly declares, and does not undermine the all-node test over a clean git-tracked vector set. Its wording should say that verification proves consistency with the *supplied* `extract.json`, and that git provenance is an external assumption. The claim that replay parses `extract.json` once is also inaccurate: `Node::verify` parses it at `conformance/src/digest/store.rs:274`, then replay parses it again at `replay.rs:149`. This is a documentation and trust-boundary nit, not an O9a-2 gate failure for the trusted corpus.

No medium or high finding in the fix range.

## Round-1 closure

| Finding | Status and evidence |
|---|---|
| F1 — unchecked stored files / vacuous shortened pair | **Closed for the git-tracked corpus.** `replay.rs:148` calls `Node::verify` before output creation; `:153-162` obtains the summary height and hashed golden's last hash; `:289-294` checks V against both. The all-node test at `conformance/tests/digest_replay.rs:110-140,189-213` derives its height and block count from the summary and tip hash from `blocks.dat`. My copied shortened pair exited 2, left no output, and reported summary tip 245 versus V height 0. The forged-extract residual is above. |
| F2 — empty input / repeated genesis | **Closed.** `replay.rs:108-118,161-162,241-245` requires a nonempty file with genesis only at index 0 before V starts. My empty and appended-genesis copies exited 2, left no output, and named the shape error. The committed test also covers missing genesis and genesis at index 5. |
| F3 — fail-stop test gap | **Closed.** `replay.rs:301-355` has a `cfg(test)` hook during `run` and asserts the first latched reason and output removal. Reapplying round-1 M4 (`latch.reason()` replaced with `None`) made that test fail. The release binary has no hook symbol or planted test string. |
| F4 — ignored unlink error | **Closed.** `replay.rs:170-176` reports the replay error and unlink error together, while treating an already absent file as removed. Both focused cleanup tests passed. My relative-path invalid-block replay exited 2 and removed its output. |

## Independent mutations

Each mutation was applied alone to `conformance/src/digest/replay.rs` from an exact-head saved copy. After each run I restored with `cp -p`, ran `touch`, and verified `cmp -s` equal. All four were viable and killed; the final tracked tree was clean.

| Mutation | Focused test | Result |
|---|---|---|
| M4 (round 1): replace the replay's latch reason with `None` | `a_latch_tripped_during_the_replay_fails_it_with_the_first_reason` | Killed: 1 failed |
| N1: remove `Node::verify` | `a_file_extract_json_did_not_hash_is_refused` | Killed: 1 failed; unverified edit reached block parsing instead of the expected hash error |
| N2: allow a second genesis in `check_shape` | `blocks_dat_must_hold_genesis_first_and_only_there` | Killed: 1 failed |
| N3: omit the summary-tip-height check | `a_shortened_blocks_dat_and_golden_pair_fails` | Killed: 1 failed |

## Validation and limits

- `cargo test -p dash-conformance --test digest_replay`: 13 passed, including the 77/81-node replay check (four refused for multiple parameter sets).
- `cargo test -p dash-conformance --lib digest::replay::tests`: 4 passed, including both cleanup cases and the latch hook.
- `cargo test -p dash-statedigest`: 109 passed. `cargo test -p dashd-chainstate --features statedigest --lib digest_tap`: 1 passed.
- `cargo clippy -p dash-conformance -p dash-statedigest -p dashd-chainstate --all-targets --features dashd-chainstate/statedigest -- -D warnings`, `cargo fmt --all -- --check`, and `git diff --check`: exit 0.
- `cargo build -p dash-conformance --release`: exit 0 without test cfg. `nm -C` and `strings` found no `before_block`, `BEFORE_BLOCK`, `replay_with`, or planted latch-test string in the release binary. That binary replayed the valid node at exit 0. The only new test hook is behind `#[cfg(test)]` at the call and module; no production branch depends on it.
- Independent copied-vector probes: valid replay 0; empty input 2; repeated genesis 2; shortened pair with original summary 2; relative-path invalid block 2 with no output; forged extract/shortened pair 0. Comparing a full valid replay to its golden exited 3 (incomplete coverage under §10.3 and the exit-3 erratum); comparing a genesis-only replay to the full golden exited 1 (diverged). Existing tests also check malformed CLI error 2 and a planted key mismatch 1.
- D19 correctly describes linear-only replay, fail-stop priority, tip checks and cleanup. D22 correctly describes all genesis cases. D21 correctly discloses the unanchored `extract.json` residual, subject to the provenance wording above. The checks still exercise V with `NullConnector`; CH3a's real `ConnectBlock`, full gate, fuzzing, and a live Core oracle were not rerun.

**Verdict:** APPROVE-WITH-NITS
Sol gate satisfied: yes
