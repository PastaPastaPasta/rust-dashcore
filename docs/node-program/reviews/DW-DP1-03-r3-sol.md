# DW DP1-03 — narrow confirmation review, round 3

Reviewed `dashwallet-desktop` `dw/dp1-03-names` at `8ff269c0ffa47a7d6c154b48b960c1011d3b702d`, based on desktop main `de1c373`, with commits `9f75040`, `7b443a3`, `cf4730d`, and `8ff269c`. Read `REVIEW-COMMON.md` and both prior Sol reports, and applied the code-review-validator workflow. Used the manager's ruling in this request: Platform is authoritative, pending labels are intents only, kinds are never persisted, and definitive answers clear the intent. The implementer's worktree remained read-only. All experiments used a detached worktree under `/work/scratch/dp1-03-review-r3/`; no commits, pushes, or GitHub comments.

## Findings

### R3-1 — major: an absent choices cache treats unknown pending state as no pending state

**File:line:** `rust/crates/dw-engine/src/platform/recovery.rs:186`, `:303`, `:425`, `:445`, and `:467`; compare the DB-backed `name_prefs` at `:534`.

**Failure scenario:** An interrupted contested registration leaves `alice` in the library's `dpns_names` and `pending_name = alice` in SQLite, without an owned marketplace row or established local contest membership. With a correctly loaded cache, both APIs hide it. If the first choices load fails, or the cache was forgotten and has not been successfully reloaded, `choices_of` returns `IdentityChoices::default()`. `summaries` therefore receives an empty pending list, treats that provisional library entry as owned, and returns `names = ["alice"]`, `main_name = Some("alice")`.

This is the first self-reported low, and **it is a real display-correctness defect**. `main_name()` reading the DB does not fix `identities()`: after the database is available again, the former returns `None` and explicit selection is refused, but neither DB read repairs the summary cache. The wrong ownership display persists until a successful `load_identity_choices`, rather than being limited to the failed SQL call. A failed refresh with a complete existing cache preserves that cache correctly; the defect is the missing/incomplete-cache case.

**Suggested fix:** Distinguish unloaded/failed choices from a successfully loaded empty set. Until the pending state is known, preserve a complete verified read model, return an appropriate unavailable result, or conservatively require positive ownership rows for ambiguous library labels. Retry/refill choices asynchronously; do not add SQLite reads to the sync facade. Ensure a later single-preference write cannot promote a partial cache to a supposedly complete one. Add a failed-initial-load test asserting that a provisional pending label stays out of both summary fields and remains unpickable after DB recovery.

### R3-2 — major: an old wallet snapshot with newly cleared prefs resurrects a refused name

**File:line:** `rust/crates/dw-engine/src/platform/recovery.rs:408`, `:415`, `:420`, and `:425`; `rust/crates/dw-engine/src/platform/names.rs:1243`, `:1247`, and `:1265` (also settlement at `:1198` and `:1228`).

**Failure scenario:** Call `identities()` while `alice` is a pending provisional library label. The returned summary correctly hides it, but `snapshots` stores the raw `OwnedIdentity` containing `alice`. A subsequent registration retry receives definitive `Locked`, removes the library copy, and clears the pending preference through the shared writer. The registration path does not replace that raw recovery snapshot. When another ordinary wallet-manager writer holds the lock, `identities()` falls back to the old raw snapshot and combines it with the new empty pending list. It returns `names = ["alice"]`, `main_name = Some("alice")`, although the refusal already completed and `main_name()` returns `None`.

This needs no SQL failure, removal, or concurrent registration at the moment of the summary read. It is not merely serving a previous legitimate display: the previously returned summary hid `alice`; the combination creates a false ownership answer that no completed state supported. Once a current manager read succeeds, the display corrects itself. The same missing coherence can expose a just-settled contender whose old snapshot has no open-contest entry.

**Suggested fix:** Publish wallet name evidence and the preference/pending inputs as one coherent cached summary view, or version and validate their combination. Name settlement/refusal must update or invalidate the fallback view before a cleared intent can be applied to an older provisional snapshot. Merely sharing the pure filter or locking preference writers does not establish this cross-cache consistency. Add a regression that primes the pending snapshot, completes a locked/taken refusal, holds the manager write lock, and requires that both summary fields still hide the disproven label.

## R2 and R5 disposition

- **R2-1 closed for the async display/selection paths:** `main_name` and `set_main_name` take the identity state lock and use `evident_names`; no pending label without positive ownership evidence is displayed or accepted. A registration holding the separate serial mutex no longer changes that behavior. The summary-specific gaps are R3-1/R3-2 above.
- **R2-2 closed under the manager's revised rule:** the kind is derived from current Platform-backed state, never stored. `conclude_others` resolves earlier intents before classifying the next label; the cut-short contest test produces the temporary `alice2` name. Unknown, truncated, or unplaced contest evidence holds the next plain registration back without spending its grant. Keeping the old persisted kind, requested as one option in r2, is intentionally superseded by the ruling.
- **R2-3 closed in registration/refusal handling:** incomplete contender evidence retains the intent and does not become an offline `Registered` result. Definitive locked, taken, and sufficiently old closed-contest refusals remove the provisional library copy as well as the intent; repeated retries query Platform again and submit nothing. A just-closed contest retains its intent. The summary fallback can still resurrect a disproven copy as described in R3-2.
- **R5 structural composition confirmed:** `recovery::main_name` is removed; both callers use `resolve_main_name`. Both use `evident_names` with the same ownership-row extraction; summary `names` additionally removes open-contest labels via the same `owned_labels` rule used in resolution/selection. All production name-preference writes flow through `NetworkSession::set_name_pref` (including the `set_main_name` wrapper), with DP1-05's `choice_writer` spanning persistence and cache mutation. No duplicate direct name-preference SQL writer remains. These shared helpers are correct given complete, consistent inputs; R3-1/R3-2 show that the synchronous path does not always supply those inputs.
- The `IdentitySummary.names` behavior change is accurately documented in the contract row at `docs/contracts/m4-dashpay-engine.md:122`.
- `rust/crates/dw-appdb/src/lib.rs` is byte-identical to `de1c373`: both blobs are `9c59bbcae9898a897597440a226bcbdec944ee43`.
- R2-4's estimated-budget wording is corrected in the contract. The accepted E0-04/DP1-06 fee accounting and dispatch prerequisites remain deferred; this round does not reopen them.

## Classification of the second self-reported low

**Minor, inherited lifecycle hygiene; not an independent blocker for this round.** `Recovery::forget` removes the choices entry without taking `choice_writer`, while `update_choices` uses `entry(...).or_default()`. An already-admitted writer can therefore recreate a cache entry after forgetting the wallet. The same shape exists in DP1-05 at `de1c373`; R5 extends its values rather than introducing the pattern. References: `recovery.rs:191`, `:254`, and `:524`.

For an actually removed/unloaded wallet, `identity_summaries` checks `require_wallet` before using that cache; cache recreation alone does not expose an identity for a nonexistent wallet or authorize a name write. A normal successful load replaces the entry from SQLite. Track a follow-up to serialize forgetting with writers or reject updates from a retired wallet generation. This classification concerns the reported cache-entry recreation, not a claim that every wallet-removal/write race has been exhaustively audited; a subsequent failed reload must also obey the fail-closed rule in R3-1.

## Validation

On the unchanged reviewed head, under `rust/`:

- `cargo fmt --check`: exit 0.
- `cargo clippy --workspace --all-targets -- -D warnings`: exit 0.
- `cargo test --workspace --no-fail-fast`: exit 0; **644 passed, 0 failed, 4 ignored**, including doc tests. The ignored tests are three public-testnet tests and the opt-in OS-keyring test. This independently confirms the claimed 644-pass gate.
- The checkout was clean before the gate and again after clippy, before adding any probe.

The original review-only r2 probe sources were removed after that review. The surviving historical `/work/scratch/dp1-03-r2/base-run.log` records three failing reproduction equivalents at the old head; their committed equivalents are present at the reviewed head and all passed in the unchanged-head run:

- `a_pending_label_is_not_owned_until_platform_says_so` (R2-1);
- `a_cut_short_contest_is_concluded_before_the_next_name` (R2-2);
- `refusals_never_turn_a_pending_label_into_a_name` (R2-3).

The additional `an_unplaced_contest_holds_back_the_next_plain_name`, `a_contest_just_closed_keeps_the_intent`, `identities_show_only_names_platform_shows_owned`, and `identities_show_the_main_name_main_name_does` tests also passed.

After the unchanged-head gate, appended five independent review-only probes in the scratch checkout and ran `cargo test --workspace --no-fail-fast review_r3_ -- --nocapture`: exit 0; **5 passed, 0 failed**. Only those five tests executed; other workspace tests were filtered out. Three assert observed erroneous behavior, so their passing results confirm defects rather than approval:

1. `review_r3_missing_choice_cache_displays_unconfirmed_name`: forget the cache with the persisted pending intent intact; the summary shows `alice`, while `main_name()` returns `None` and selecting it is refused. A successful choices load restores the filtered summary.
2. `review_r3_failed_choice_load_does_not_get_repaired_by_main_name`: inject a real choices SELECT failure by temporarily renaming `dp_prefs`, retaining its rows. Restore the table, then read `main_name()` successfully. It returns `None`, but summaries still show `alice` until a successful choices load. This confirms R3-1 through the load-error path, not just direct cache eviction.
3. `review_r3_old_snapshot_after_refusal_displays_disproven_name`: prime the pending raw snapshot, complete a `Locked` refusal, and verify that the current wallet list and DB pending row are empty. With the manager write lock held, the summary nevertheless shows `alice`; releasing that lock and taking a current summary corrects it. This confirms R3-2.
4. `review_r3_main_name_agrees_through_settlement_sync_and_reopen`: both APIs agree through a cut-short contest's conclusion, temporary-name registration, explicit pick and clear, partial sync, won-contest settlement, complete sync, and session reopening.
5. `review_r3_pending_name_stays_hidden_during_registration`: while holding the serial registration mutex, the pending label remains absent from both display paths and selection is refused.

All commands used `umask 022`, `TMPDIR=/dev/shm/dp1-03-review-r3-tmp` (mode **0755**, as expressly requested), `CARGO_BUILD_JOBS=12`, and the scratch checkout's own Cargo target directory. The initial dependency build was stopped before tests to enable the shared sccache after a bounded smoke check; the complete successful run is the gate reported above. A package-only probe build was stopped before execution because it changed feature selection; the final probe command retained the workspace graph. A scratch-probe compilation error using the private `data_dir` field was corrected to its public accessor before the successful probe run. None of these setup corrections changed the implementation or the unchanged-head gate.

No shared cache service or other agent process was stopped. The detached worktree, probe sources, build output, logs, and this review's `/dev/shm` directory were removed after validation. The requested report is the only retained review artifact and is uncommitted.

## Open questions / limits

No public-network registration was attempted. This was a narrow r2/R5 confirmation, not a new full registration or E0-04 authorization audit. The wallet-removal cache race was classified from the exact DP1-05 and R5 code; no teardown stress test was added for that inherited pattern.

## Verdict

**CHANGES-REQUIRED.** The three requested r2 registration fixes are closed under the manager's ruling, and R5 shares the intended rule/filter/writer. Fix R3-1 and R3-2 before declaring the two name display APIs consistent: a cache miss must not assert ownership, and a cleared intent must not make an older provisional snapshot look owned.
