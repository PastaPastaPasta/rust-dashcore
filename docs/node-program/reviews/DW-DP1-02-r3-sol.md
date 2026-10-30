# DP1-02 Phase A — Sol narrow round 3

Reviewed `dw/dp1-02-registration` at **`008513fd8b00defea11dad678b9e6c3fe6bf497e`**, on `af4e47e`, against REVIEW-COMMON, Sol r1/r2, DEC-171/173, and m4 Contract-Version 10. **R2-01 through R2-04 are closed by their mechanisms. One new major in the reservation lifecycle remains: the fourth reported residual can block its own cleanup.** Closed r1/r2 findings are not reopened.

No reviewed-branch edits, commits, pushes, GitHub comments, testnet transactions, or private-path reads. PLAN §7 handling stayed at codenames only. The report is uncommitted.

## R3-01 — major: stale registration reservations prevent the next funding from reaching cleanup

**Locations:** `rust/crates/dw-engine/src/platform/registration_flow.rs:2402`, `registration_flow.rs:2433`, `registration_flow.rs:2476`, `registration_flow.rs:1897`; `rust/crates/dw-engine/src/platform/registration.rs:522`.

The fourth reported residual says a lock rejected after its call keeps its inputs reserved until the next funding. Cleanup actually runs only inside `PwLib::fenced` (before its coin check, and after that call itself returns `NotSent`). `PwLib::spendable` does not perform it. Both start and the driver's funding step reject any protected source coin **before** calling `RegistrationLib::fund`, so the stale reservation prevents reaching the helper that would remove it.

**Concrete scenario:** the Built tap transfers a funding's input into `PendingSpends`; the funding call ends while the lock is still tracked; the library later removes the lock, leaving its input unspent. The real spendable view continues to return `Spendable { duffs: 0, inputs: 1, protected: 1 }`. On an otherwise eligible start, the real facade returns `Err(CoinsLocked)` before inserting a row or invoking funding. The driver's same early check has the same barrier. Normal coin snapshots also keep that unspent input reserved, so it remains unavailable to sends and CoinJoin funding. Waiting or repeating the gate does not free it; wallet unload/session close clears the session reservation. The promised next-funding recovery is unreachable when the stale reservation is present.

**Suggested fix:** reconcile this registration-owned reservation set against the library's tracked set under the shared coin fence before the policy snapshots used by pre-funding gates, or process the library's removal/completed rejection at its lifecycle boundary. Keep reservations for every still-tracked lock, and preserve other operations' reservations. Add a regression through the ordinary gate; a direct call to `fenced` bypasses the failing precheck and is insufficient. This is a bounded Phase A fix and does not require P2b's TxDraft migration or dispatch markers.

**Evidence:** `review_sol_r3_untracked_reservation_does_not_block_next_funding_gate` reaches the intended assertion using the real wallet coin, Built tap, host reservation, spendable implementation, and start facade. The final strengthened probe first asserts one entry in the real wallet’s tracked-lock set, removes that entry after the fenced call returns, then asserts the set is empty. It fails only at the expected `CoinsLocked` assertion when run alone; no setup assertion fails. [Final isolated output](/work/scratch/dp1-02-r3-sol/reservation-residual-alone.log). This is an offline lifecycle-state proof of the implementer's stated residual, not a test of a network's rejection response.

## R2 closure and fresh bypass attempts

### R2-01 — closed: retired acquisition and generation loss

Acquisition's in-memory admission and SQL generation increment run in one actor job (`registration_flow.rs:553`). `forget_wallet` queues its clear behind already-sent jobs (`:647`), and reopening uses the same wallet queue. Jobs are synchronous and run to completion even if their async receiver is dropped; no runtime actor job returns an awaited write outside that queue. Driver writes classify a different generation as `Written::Lost`, leading to `Step::Stop` (`:1588`); the funding entry rechecks the stored generation and row (`:1814`).

The existing `a_retired_claim_cannot_invalidate_the_reopened_owner` and `a_run_that_lost_its_generation_stops_before_funding` regressions pass. **Fresh attempt:** pause the actual acquisition job after admission, before SQL; cancel its result receiver; enqueue unload and reopen; release it. The reopened claim owns generation 2, SQL remains at 2, and its driver completes exactly one funding/identity creation. A separate actor request raises a Draft row's generation without changing its in-memory holder: its stale driver stops at the next transition, with no funding and no loop. Probes: `review_sol_r3_cancelled_acquire_cannot_overtake_reopen`, `review_sol_r3_lost_generation_stops_at_next_transition`.

### R2-02 — closed: unreadable-row ambiguity parks

`may_be_unreadables` excludes potentially owned locks from `stranded` imports (`registration_flow.rs:1035`), and lost-index parked locks are included in `still_pending`. `funding_status` retains positive/possible evidence for unknown rows (`:1142`). The parking predicate treats an unparseable outpoint conservatively (`:1129`); unknown rows are left unchanged.

The old r2 unknown-funding/index-7 plus reconstructed index-0 repro now leaves one unchanged row, creates/consumes no identity, reports `still_pending = 1`, and sets `funds_committed = true`. **Fresh attempts:** use a newer profile encoding, an unknown phase, an unparseable stored outpoint, and an absent persisted index, each with a lost-index recovered lock. All four remain parked, refuse discard, and preserve possible funding. The existing no-row-index/known-lock-index control still imports the positively indexed lock. Probes: `review_sol_r2_unknown_row_keeps_ambiguous_reconstructed_lock`, `review_sol_r3_unreadable_profile_phase_and_outpoint_all_park`.

### R2-03 — closed: the tap cannot write an obsolete association

`lock_tracked` dispatches a blocking actor job whose `note_lock_tracked` selection and `attach_lock` update run together (`registration_flow.rs:1230`). Every update requires a concrete persisted owner; the appdb `owner=None` API is gone. Claims and index reassignment queue behind a paused tap job; phase compare-and-set rejects the obsolete reassignment once the tap advances the row. There is no remaining production registration update path using an ownerless write.

The existing `tap_evidence_cannot_land_inside_an_index_reassignment` repro passes. **Fresh attempt:** the old r2 ownership inconsistency, with an older unreadable row U at index 7, a runnable row R also at 7, and tracked L7. The idle tap refuses to donate U's evidence to R. R then assigns index 8 and completes using its own distinct lock; U remains protected as funded. Probe: `review_sol_r3_tap_respects_unreadable_index_owner`.

### R2-04 — closed: coin policy remains fenced through selection/signing

`PwLib::fund` wraps the library call in `fenced`, which rechecks the real policy snapshot while holding the per-wallet fence. Manual-lock writes and snapshot dust-lock writes use that fence. The Built tap reserves transaction inputs before dropping it (`registration_flow.rs:717`); `reserve_spends` takes the same fence and rechecks host reservations before adding another (`coins.rs:411`). The changed CoinJoin `commit_funding_tx` awaits that reservation before signing and releases its allocated addresses on refusal (`coinjoin.rs:1533`).

The existing coin-lock wait and Built-input exclusion regressions pass. **Fresh attempts:** queue CoinJoin's reservation after the funding check, before the offline builder. It waits; after Built tracking, it sees the just-reserved input and refuses. The reverse order—an existing host reservation before funding—refuses the signing callback entirely. A call with no tap notification keeps its fence until return and then lets the waiting manual lock complete. Probes: `review_sol_r3_reservation_waits_then_rechecks_built_inputs`, `review_sol_r3_prior_reservation_refuses_signing`, `review_sol_r3_unknown_funding_call_retains_fence_until_return`.

The remaining direct CoinJoin reservations concern CoinJoin-account collateral, mixing and sweep inputs; registration's pooled funding sources are BIP44/BIP32/DashPay receiving, so those sites do not bypass this source-coin fence. The send/mod.rs TxDraft path is the explicitly scoped P2b residual. The exact old `review_sol_r2_coin_locked_after_snapshot_is_not_signed` repro is adapted to use `PwLib::fenced` around the same offline builder. A real manual lock completes after the driver’s saved spendable snapshot; the new fenced recheck refuses it with `registration.coins_locked`, and the builder signs no input. [Final closure probes](/work/scratch/dp1-02-r3-sol/closure-probes-final.log).

## The four reported residuals

1. **TxDraft reservations outside the fence — P2b-owned.** Explicitly assigned there by m4. Registration still checks completed manual/dust locks under its own fence. A preselected ordinary send's overlap with a registration lock remains the stated Phase A limitation; this review does not certify the send path's broader coin-control migration.
2. **Tap persistence failure — acceptable for Phase A, with a precise boundary.** If library persistence fails before the tap sees Built, `FenceHeld` retains the fence until the call ends/cancels. If only appdb association fails after `release_built`, the fence has already become an input reservation, so selected inputs stay protected. No double-funding permission follows from either error. Durable dispatch markers remain P2b-owned.
3. **Database failure in acquire drops the pre-empted run — acceptable.** The failure propagates, the temporary claim is released, and the durable row remains resumable. An actual SQL abort trigger on owner updates reproduces failed takeover; removing the trigger admits another claim and completes the same row. Probe: `review_sol_r3_failed_acquire_keeps_row_resumable`.
4. **Late rejection retains inputs until next funding — required now.** R3-01 demonstrates that the retained reservation prevents reaching the next-funding cleanup. This differs from temporary conservative retention while the library still tracks a possibly sent lock.

## Validation

- Existing focused registration baseline: **46 passed, 0 failed**, then the same **46/0** from the independently compiled archive, with review tests excluded. Logs: [existing baseline](/work/scratch/dp1-02-r3-sol/registration-baseline.log), [archived baseline](/work/scratch/dp1-02-r3-sol/archived-registration-baseline.log).
- Final independent closure/fresh-bypass probes: **11 passed, 0 failed**, including the adapted old snapshot-to-signing race: [final closure probes](/work/scratch/dp1-02-r3-sol/closure-probes-final.log). The strengthened R3-01 probe was run alone and reproduced **1 expected assertion failure**, after successfully verifying real tracked-state insertion/removal: [final regression output](/work/scratch/dp1-02-r3-sol/reservation-residual-alone.log). The initial 10-pass/1-fail run and its isolated reproduction are also retained in scratch.
- Appdb registration checks **8/0**, m4 contract checks **9/0**: [appdb](/work/scratch/dp1-02-r3-sol/appdb-tests.log), [m4](/work/scratch/dp1-02-r3-sol/m4-contract-tests.log). The implementer's supplied 904/0 whole gate was not rerun in full.
- Tests/builds use nice 10, `CARGO_BUILD_JOBS=8`, `RUSTC_WRAPPER=`, and mode-0700 `/work/scratch/dp1-02-r3-sol/tmp`. No `/dev/shm` or `/tmp` test/build outputs. Scratch source is `git archive 008513f`, with review-only tests and one test-only acquisition pause. The engine test binary was compiled using the captured cargo rustc invocation, current dependency artifacts as read-only inputs, and scratch compiler output/incremental state. No shared `CARGO_TARGET_DIR` was introduced.
- [Review-only probe patch](/work/scratch/dp1-02-r3-sol/review-probes.patch) applies cleanly to the reviewed head. The final strengthened probe build succeeded without diagnostics. Reviewed worktree remains clean; branch diff whitespace checks pass. Scratch temporary test data and incremental build cache were removed after validation; the source archive, final test binary, compiler recipe, patch and logs remain.

**Verdict:** CHANGES-REQUIRED

Sol gate satisfied: no
