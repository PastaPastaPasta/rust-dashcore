**Verdict: CHANGES-REQUIRED.**

# DP1-02 Phase A — Sol r1

Reviewed `dw/dp1-02-registration` at `02ae788292d7a8c90cca85cf3d1fbef4645b4aca`, against desktop main `d50de65`, DASHPAY registration/restore rules, E0-04 grants/leases/dispatch rules and §2a.5, DEC-154/160/163, and m4 Contract-Version 9. P2b permits, durable step markers, testnet execution and step-2 retry fencing are outside this review's gate.

## R1-01 — blocker: recovered registration indexes are synthetic; restore can fund twice

**Locations:** `rust/crates/dw-engine/src/platform/registration_flow.rs:1102`, `registration_flow.rs:565`, `registration.rs:850`, `registration.rs:867`.

The pinned platform-wallet does not recover a registration lock's destination identity index from chain. `rs-platform-wallet/src/wallet/asset_lock/sync/reconstruction.rs:68` (pin `bc41f1bc233dec4607d387101c1d9c2f111019b2`) maps `AccountType::IdentityRegistration` to index **0**, and its `registration_lock_reconstructs_from_chainlocked_record` regression explicitly builds at index 4 and expects reconstructed index 0. `PwLib::tracked_locks` passes that value through as an authoritative identity index.

**Reproduction:** Keep an older backup with a `KeysPrepared` CoreBalance row at index 7, without its outpoint. The source machine funds that row; a crash before the new automatic backup completes leaves the older backup available. Restore it, finish SPV and discovery, and reconstruct the original unconsumed lock. Discovery finds earlier identities but no identity at 7; the recovered lock reports index 0. `unheld_lock(7)` misses it, the funding gate reads `None`, and resume builds a second asset lock at 7. This is a supported behind-reality backup, not a lost/unavailable chain lock awaiting P2b.

The repair variant is also unsafe: with an earlier identity at index 0, "Finish transfers" imports a recovered registration lock with `identity_index = 0` and attempts IdentityCreate using index 0 again. The repair probe demonstrates duplicate index assignments. Its index assignment bypasses `next_index` and the index lock.

**Suggested fix:** Treat the reconstructed registration index as unknown, reconcile recovered locks with persisted outpoints/rows before permitting fresh funding, and park ambiguous mappings. Allocate a genuinely unused identity index under the shared index guard when repair creates a new identity without an existing row mapping.

**Probes:** `review_sol_reconstructed_nonzero_index_does_not_fund_twice`; `review_sol_finish_does_not_reuse_synthetic_recovery_index_zero`.

## R1-02 — major: overlapping discard guards release each other's exclusion

**Locations:** `rust/crates/dw-engine/src/platform/registration_flow.rs:384`, `registration_flow.rs:470`; `registration.rs:810`, `registration.rs:823`.

`begin_stop` checks `in_call` but accepts another decision while `stopping` is already true. Both guards own the same boolean, and either guard's drop clears it.

**Interleaving:** Discards A and B both acquire the decision guard for an unfunded row. B reads an unfunded snapshot and pauses before deletion. A ends early, for example when one of its fallible appdb reads returns an error. A's drop clears `stopping` while B still owns its guard. The driver now enters funding and may track/sign/broadcast a lock. B then deletes using its earlier snapshot and aborts the driver: no final `in_call` or funding check protects the delete. The promised exclusion and the running-call refusal no longer hold.

**Suggested fix:** Make the decision ownership exclusive (serialize/reject another `begin_stop`) or use an ownership/count mechanism that stays set until every deciding guard leaves. The guard probe keeps B live, drops A, and demonstrates that `enter_call` succeeds.

**Probe:** `review_sol_two_discard_guards_keep_calls_excluded`.

## R1-03 — major: the listed KeysPrepared/outpoint residual strands a funded lock

**Locations:** `rust/crates/dw-engine/src/platform/registration_flow.rs:1103`, `registration_flow.rs:1116`, `registration_flow.rs:650`; `registration.rs:887`.

**Reproduction:** Import a known CoreBalance `KeysPrepared` row with its canonical outpoint and a tracked ChainLocked lock at the same index. Resume excludes its own lock from `unheld_lock`, does not regard it as `held_elsewhere`, then fails `platform.broadcast_unknown` because `funding_status` correctly sees Sent. Another resume clears the failure and repeats the same path. "Finish transfers" finds the existing holder and leaves it unchanged, then skips both its unfunded phase and its non-retryable failure. Discard is rightly refused. Every exposed recovery path in this build leaves the unconsumed lock stuck, even though its proof is present. This is the imported state explicitly listed among the implementer's residuals.

**Suggested fix:** Reconcile a row's explicit tracked outpoint first and advance it to the funded/proof phase regardless of a stale unfunded phase. Repair must normalize and resume that existing holder. This residual is worse than cosmetic looping: the build has no working recovery path for the funded row.

**Probe:** `review_sol_finish_recovers_keys_prepared_row_with_own_outpoint`.

## R1-04 — major: an unknown funding encoding erases positive funding evidence

**Locations:** `rust/crates/dw-engine/src/platform/registration_flow.rs:598`; `registration.rs:817`.

**Reproduction:** Restore a future-version CoreBalance row (`{"v":2,"kind":"core_balance","lock_duffs":3000000}`) with a canonical outpoint, phase FundingSent and a matching tracked ChainLocked lock. The sweep fails it with `registration.unsupported`, but `funding_status` returns `None` before checking either its outpoint or the tracked lock, because decoding is not known CoreBalance. Status reports `funds_committed = false` and discard succeeds, deleting a funded flow. An older build cannot infer "none of the user's funds" from an encoding it does not understand.

**Suggested fix:** Preserve positive/possible funding evidence independently of whether the funding encoding is runnable. Unknown rows with an outpoint or matching lock must fail closed for discard and subsequent funding; only unsupported rows with nothing committed should free the slot.

**Probe:** `review_sol_unknown_funding_with_tracked_lock_still_refuses_discard`.

## R1-05 — major: index-only identity adoption precedes the other flow's ownership check

**Locations:** `rust/crates/dw-engine/src/platform/registration_flow.rs:1089`, `registration_flow.rs:1108`.

**Reproduction:** Extend the existing other-holder restore test so the holder has completed IdentityCreate: its lock is Consumed, its row is Done, and its identity is persisted at index 0. Resume a behind-reality KeysPrepared row also carrying index 0. `fund` adopts the completed holder's identity before consulting `held_elsewhere`, registers the restored row's requested label/profile on that identity, and finishes with both rows at index 0. The same ownership situation before IdentityCreate correctly moves the restored row to a fresh index; completing the holder changes the result to reuse/adoption.

**Suggested fix:** Resolve explicit outpoints and other-row ownership before adopting an identity by index. Where an outpoint is known, accept only the identity derived from that outpoint, as `create_identity` already does.

**Probe:** `review_sol_completed_holder_does_not_steal_restored_flow_identity`.

## R1-06 — major: unsupported initial profiles are rejected only after money is committed

**Locations:** `rust/crates/dw-engine/src/platform/registration.rs:567`; `registration_flow.rs:884`, `registration_flow.rs:1422`.

**Reproduction:** Import a Draft CoreBalance row with `initial_profile = {"v":2,"display_name":"Alice"}`. The sweep checks phase and funding only, so status leaves it live and it blocks another start. Resume then funds 3,000,000 duffs, registers the identity and its name, and only at the profile step fails `registration.unsupported`, not retryable. The row now has committed funds and continues blocking registrations. Contract-Version 9 specifically says an unknown `initial_profile` fails at once and an unsupported unfunded row frees the slot.

**Suggested fix:** Decode/check the profile in both the early sweep and the driver's pre-step validation, before any funding or other write. Preserve the funded-state protection for unsupported rows that were already funded.

**Probe:** `review_sol_newer_profile_is_refused_before_funding`.

## R1-07 — major: real funding ignores the app's coin locks and reservations

**Locations:** `rust/crates/dw-engine/src/platform/registration_flow.rs:1614`; `rust/crates/dw-engine/src/coins.rs:170`, `coins.rs:264`.

`PwLib::spendable` calls `read_coins`, which initializes every `WalletCoin` with `user_locked = false` and `reserved = false`. Only `NetworkSession::coin_snapshot` overlays the appdb user/dust locks and the session's spend reservations. Calling `auto_selectable` on the raw snapshot therefore does not exclude those coins from the balance, contrary to Contract-Version 9. The library funding call also receives no corresponding input exclusions.

**Reproduction:** Credit the real fixture's BIP44 account with one confirmed 1-DASH coin and call the real `lock_outpoints` API for it. The normal coin snapshot has zero automatically spendable duffs; the new real `PwLib::spendable` reports 100,000,000 duffs and one input. An offline call to the same asset-lock builder used by the funding pipeline signs a lock selecting that manually locked outpoint. No fake `Spendable` model is involved and nothing is broadcast. A normal funding call can consequently spend the user's locked coin; host reservations are omitted by the same path.

**Suggested fix:** Use the app's policy-aware coin snapshot for the available balance and carry its input exclusions into the library's selection. If the pinned funding API cannot honor an exclusion, refuse that build rather than treating the protected inputs as spendable. Counting all source coins for the worst-case fee does not require counting them as available funding.

**Probe:** `review_sol_real_spendable_excludes_a_manually_locked_coin`.

## R1-08 — major: retired task cleanup clears a reopened row's new driver

**Locations:** `rust/crates/dw-engine/src/platform/registration_flow.rs:451`, `registration_flow.rs:431`; `rust/crates/dw-engine/src/platform/runtime.rs:426`. Related caller: `multiwallet.rs:285`.

`forget_wallet` removes flow entries and requests task abortion, without joining the retired tasks. `Claim::drop` later clears the entry by wallet/row key only; it ignores its run identity. A new claim for the same persistent row after wallet reload starts its generation at 1 again, so comparing the existing resettable counter alone would not solve the problem.

**Interleaving:** An unfunded row's driver is paused, then the user closes its wallet. Reopen and resume that same stored row before the old task handles its abort. The new driver claims the row. When the retired task is finally polled and drops its claim, it clears the new driver's `running`, `in_call`, task and lease state. Another resume can now claim the same row and run concurrently, allowing two funding calls after both read an unfunded row; or discard can overlook a call that the new driver already marked in flight. The old cleanup does not cancel the new driver, so the cleared state is a false view of a still-running task.

**Suggested fix:** Use a unique run identity that survives forgetting/recreating the map entry, and require every retired guard's cleanup to match that identity; alternatively join retired tasks before admitting a replacement. Apply the same ownership rule to call/stop cleanup, which also updates by key alone.

**Probe:** `review_sol_retired_claim_cannot_clear_reopened_flow` directly executes claim → forget-wallet → claim → old-claim drop, and checks that a third claim is refused.

## Validation and residual assessment

- Target remained clean; no edits, commits, pushes or GitHub comments. No testnet transactions.
- All work and TMPDIR are under `/work/scratch/dp1-02-r1-sol/`; compilation/tests use nice 10 and `CARGO_BUILD_JOBS=6`.
- Fresh baseline registration run: **33 passed, 0 failed**. Appdb registration checks: **7 passed, 0 failed**. m4 contract checks: **9 passed, 0 failed**. The first baseline attempt hit the scratch parent's mode-0775 protection; after chmod 0700 the same tests passed.
- Isolated probes were added only to a `git archive 02ae788` copy. **Nine expected assertion failures reproduce the eight findings**, with two probes for R1-01. All nine reached their intended assertions; none failed in setup. The restored-row probes use the existing fake library, seeded with metadata verified against the pinned reconstruction code; R1-07 uses the real wallet, signer and offline builder; R1-02/R1-08 exercise the real flow guards. [Final probe output](/work/scratch/dp1-02-r1-sol/probes-r1c.log).
- The isolated unit-test binary was compiled from the archived sources using the captured target rustc invocation and its dependency artifacts as read-only inputs, with compiler output and incremental state in scratch. The supplied **884/0 workspace gate** was not rerun in full. [Baseline registration output](/work/scratch/dp1-02-r1-sol/registration-baseline-rerun.log), [appdb output](/work/scratch/dp1-02-r1-sol/appdb-tests.log), [m4 contract output](/work/scratch/dp1-02-r1-sol/m4-contract-tests.log).
- [Portable test-only patch](/work/scratch/dp1-02-r1-sol/review-probes.patch): `git apply --check` passes against 02ae788. To reproduce, archive that commit into a separate on-disk source directory, apply the patch there, create a mode-0700 TMPDIR under scratch, and run `env TMPDIR=<scratch-tmp> CARGO_BUILD_JOBS=6 nice -n 10 cargo test -p dw-engine --lib review_sol -- --test-threads=2 --nocapture` from its `rust/` directory. Temporary test/build data was removed after validation; the report, patch and logs are retained.
- The normal funding-failure classification follows the requested rule and the pinned broadcaster contract: tracked wins; actual definite rejection/pre-build failures can return to unfunded; other failures remain possibly sent. I found no separate error-variant misclassification in that path.
- The per-lock cap pending P2b is an explicitly scoped Phase A limitation. Recomputing the fee bound can force another authorization but the cap checks refuse overspending. The concurrent-resume grant waste and status flicker remain usability residuals. The KeysPrepared/outpoint residual is **R1-03**, a recovery failure requiring a fix.

Sol gate: NOT SATISFIED.
