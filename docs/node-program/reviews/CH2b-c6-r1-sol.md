# CH2b commit 6 (CRASH) and DEC-202 — Sol state review

Review target: `339044e71..b33ab7afd` (`node/ch2b-crash`), 2026-10-12. I reviewed a private detached worktree at `b33ab7afd`; I did not edit the implementer's worktree or branch. All 17 range commits report `G` signatures. `339044e71` and integration head `2863ea0f7` have identical trees. Normative inputs were CH2 final rev, A1 §2.10.1 and §3.5, ST3a's model rules, DEC-165/185/195/196, the prior b1 reviews, and the supplied Opus review files. DEC-195 concerns a different program and did not change this review's bar. I did not inspect the private Core-side material for DEC-196.

## New finding

| Severity | Location | Finding, probe, and fix |
|---|---|---|
| **Medium — model false negative** | `dashd-chainstate/src/crash/chain.rs:976–987`, `:1045`, `:1472–1516` | A warranted `INVALID_TIP` refusal runs R2 and R1/R5 store checks, then returns before `ChainSut::check` calls `effects()`. Thus R13 is unjudged on this permitted abort. I injected a Durable deletion of covered `llmq_mined/i:5` after recovery, before activation, in the private worktree. With `effects=5; c5;f;i`, the identical deletion yielded `R13: effect i:5 ... is lost`. With `effects=5; c5;f;x3;i`, the loaded failed tip warranted refusal and the verdict was `[]`, even though the deletion was confirmed. Both runs used a process kill after the forward run. The existing production recovery does not delete this effect; this is a missing model check, not a demonstrated production loss. **Fix:** include `sut.effects(&world.trace())` in the refusal's `world.checking` block and turn its violation into the same unsanctioned abort as R1/R2/R5. Add a planted loss on this refusal path, with a normal-restart control, so the witness cannot pass vacuously. |

The gap matters to DEC-202: a covered effect must remain present even when a restart correctly refuses to activate a failed coins tip. ST3a R13 has no exemption for that refusal. This is the same abort-path shape as Opus r4 M3, whose index/store checks were added; R13 was omitted.

## Closure of the supplied Opus rounds

| Earlier finding | Status at `b33ab7afd` |
|---|---|
| r4 M1: block-file read fault gave false FA7 | **Closed.** `Stores::read_failed` trips the shared latch for fatal I/O; undo and block-read fault tests pass. Disabling the latch again made `block_file_read_failures_stop_without_findings` fail with R8. |
| r4 M2: FA7 began at `K`'s write, vacuous X21 repro | **Closed.** It now begins at the connect that reads the corrupt DB coin; `X21.repro` includes `c10;f;K;c1`. The checked-in planted repro test also requires reproduction to fail without `planted=`. Moving FA7's start back to the write caused false R8 on `c10;K;f;c1`. |
| r4 M3: warranted refusal hid R1/R2/R5 damage | **Closed for those rules.** The refusal runs `store_check` and `check_stores` in checking mode; T31 under refusal reports an unsanctioned R0. Disabling this check killed that test. **R13 remains open** as the finding above. |
| r4 L1: weak witness expectations | **Closed.** T21 includes R0; T27 R7a; T29/X14 R12; T31 R12. The witnessed searches and all checked-in planted repros pass. |
| r4 L2: X14 witness mechanism | **Accepted residual.** S1's effect at 25 is after flush 1; X14's covered-sequence error skips flush 2's barrier and yields R12 plus D1f. An effect written inside `F d` instead triggers D1f without a bug because `SinceFlushStart` includes V's above-tip append. This scope over-approximation is documented in D-c6-7. |
| r4 L3/L4: read-message ambiguity and FA7 strength | **Closed to the model's declared scope.** Site-specific message tests cover block, undo, and link-count reads; FA7 rejects either `Connected` value and ties the fatal refusal to the op that met the read. The r5 follow-up avoids treating tolerated allocation faults as fatal. |
| r4 follow-up: `-reindex` loses invalidation marks; feature scope; blind smoke limits | **Closed/documented.** The refusal advises repeating `invalidateblock`; a literal regression pins it. `Cargo.toml` has no self dev-dependency enabling test features, normal-edge scope test passes, and benches reject `crash`/`planted-bugs`. D-c6-10 lists the blind smoke's omitted operations and their other suites. |
| Supplied earlier CP6.1 review: failed-tip continuation, wipe/reference disagreement, file-message paths, early crash roll | **Closed.** V and `ChainRef` choose a fresh branch after failure and agree on wipe activation; message tests cover rollover/read sites; an early `-dbcrashratio` roll is a precondition with its op number. Corresponding CRASH tests pass. |

## DEC-202 and production code

`1159af730` changes only the no-marker/no-tip consistency arm (`recovery/replay.rs:382–407`): `evo_tip` remains corruption, and `nonempty_wiped_cf` still rejects records in `coins`, `mn_diff`, `mn_snapshot`, `credit_pool`, and `mnhf`. It exempts exactly `llmq_mined`, `llmq_mined_h`, and `llmq_snapshot` (`store/meta.rs:44`). The tip-present and marker paths retain their prior checks and do not wipe effects. The `without_a_tip_effect_records_stay` unit test checks all three effect CFs and rejects coins/evo state. Restoring the old empty-store rule killed that test. The branch regression passes; suppressing the restart's base snapshot rewrite killed it with R12/R3. In the model, a no-tip restart activates from genesis: the base connect rewrites the presumed snapshot before the reader's connect, while the independent effect is not read by V. The same ordering is exercised when the restart chooses a different branch. The future full evo reader is outside this b1 model, so this result is a model-level claim.

The flush machine's default path still syncs files before B1, runs the presumed-effect WAL barrier before B1 when pending, writes B1 Durable, barriers coin sub-batches before Bk, and finalizes the marker Durably. The added branches in `flush/machine.rs`, `recovery/replay.rs`, `store/guarded.rs`, and `coins/cache.rs` are controlled by the `planted-bugs` macro; without that feature it is constant false. `recovery/mod.rs` retains `sync_wal` at open unless X6 is explicitly planted. I found no change to production ordering or a test hook reachable through the default or bench feature set. `planted-bugs` is rejected in optimized builds; the benches reject `crash` and `planted-bugs`; the feature-scope test passes. `alloc-tag` and `model` remain explicit test features with no self dev-dependency enabling them for benches.

## Residual dispositions and model limits

- **D1f over-approximation: acceptable as a declared model limitation.** I reran exhaustive `c22;f;F3;c5;f`: `effects=23` had 119 images/1,260 branches, `presumed=23:31` had 120 images/1,262 branches; neither truncated, and each found only D1f. The extractor's `SinceFlushStart` treats the above-tip effect appended by V during the flush as a dependency of that flush. No state violation appeared, and DEC-202 keeps a survived effect when there is no tip. X14 remaining on S1 is a deliberate witness of its covered-sequence bug, not evidence from this over-approximated window.
- **DEC-196 x5/x4 from Durable 0: acceptable.** Exhaustive `c5;f;x5;i` and `c5;f;x4;i` each produced six images/13 branches, no truncation or findings, and one `INVALID_TIP` refusal on the end image. The loaded failed tip strictly outworks the valid candidate. Equal-work and less-work direct regressions reorg; changing V's strict comparison to include equality killed the equal-work test. The model's `may_refuse` accepts the DEC-196 row's equal-work alternative, while V's direct test pins its chosen reorg behavior.
- The reference builds its own chain, validity and coins projection; the extractor reads durable records and generated block bytes, not V's cache or recovered projection. The b1 model covers R0–R8, R12 and R13 for its subscriber-free, no-prune SUT. R9–R11 need their later components. `checked_in_planted_repros_replay` checks all 27 planted chainstate repros against unplanted execution; the present finding shows that this does not certify an invariant on a sanctioned abort unless that check is explicitly run there.

## Mutation and validation record

Each mutation was applied alone in the detached worktree. I copied the saved original back and touched it before the next run; the final worktree diff is empty.

| Mutation | Probe result |
|---|---|
| Reject effect CFs without a tip (old DEC-202 rule) | **Killed:** `without_a_tip_effect_records_stay` failed on `llmq_mined`. |
| Skip the presumed snapshot rewrite on restart | **Killed:** abandoned-branch regression failed with R12/R3 evo mismatch. |
| Refuse a failed tip at equal work | **Killed:** equal-work direct regression observed an unexpected refusal. |
| Omit latch trip on fatal block-file read | **Killed:** block-file read regression reported R8. |
| Start FA7 at `K`'s write | **Killed:** `fa7_starts_at_the_read` reported false R8. |
| Skip refusal's recovered-store checks | **Killed:** T31 refusal witness no longer found its required R0. |
| Remove the repeat-`invalidateblock` advice | **Killed:** exact refusal-text regression failed. |
| Delete a barrier-covered independent effect before startup activation | **Survived only on warranted refusal:** `[]`; the ordinary startup control reported R13. This is the new finding. |

Passed at the pinned head: `cargo test -p dashd-chainstate --lib` (177); `cargo test -p dashd-chainstate --features crash --test crash` (29); `cargo test -p dashd-chainstate --features alloc-tag,crash,planted-bugs --test crash_planted` (34, one ignored repro generator); `cargo test -p dashd-store --features faulty` (all executed unit/integration/doc suites); `cargo test -p dashd-chainstate --test test_feature_scope` (1); `cargo fmt --all -- --check`; and `cargo clippy -p dashd-chainstate --all-targets -- -D warnings` with default, `crash`, and `alloc-tag,crash,planted-bugs` feature sets. All completed successfully. I did not run the full workspace or a physical power-cut test; the review and mutation evidence concern this range's b1 SUT and the ST3a storage model.

**Verdict:** CHANGES-REQUIRED
Sol gate satisfied: no
