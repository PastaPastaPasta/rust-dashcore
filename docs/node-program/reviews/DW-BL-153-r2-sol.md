# BL-153 independent review, round 2

Reviewed `3448eaeb..0315d269` in the `dw/bl-153-dashpay-status` worktree. The branch was not edited.

## Finding

1. **Minor — expiry tasks outlive removed wallets and closed sessions** (`rust/crates/dw-engine/src/platform/banner.rs:246`, `rust/crates/dw-engine/src/platform/runtime.rs:129`, `rust/crates/dw-engine/src/platform/runtime.rs:587`). The first status read for a valid proof inserts `(wallet, until)` and spawns a task holding a strong `Arc<SessionHub>` until the deadline, which may be nearly seven days away. The task and set entry are never removed or aborted by `PlatformRuntime::forget`, `NetworkSession::close`, or `Engine::shutdown`. Removing a wallet leaves a later, irrelevant `Platform{Startup}` mark; closing and reopening a network repeatedly retains every old hub (including its history and trust state) until its timer fires, and each long-lived session accumulates entries for renewed proofs. Track an abortable timer per wallet/proof, cancel it on wallet removal and session close, and remove its deduplication entry when it fires. Cover removal and close with a timer test. Scheduling only when the derived answer actually relies on the proof would also avoid unnecessary tasks for watch-only, registering, or already-`NoIdentity` wallets.

## Round-1 fixes and checks

- **D19 fixed.** `walletCreated`, `walletRemoved`, and `walletLoadChanged` now invalidate the named wallet's status even with SPV stopped. The `events.test.ts` seed-attach case would fail if the lifecycle invalidation were reverted; it also checks another wallet remains valid.
- **D20 fixed.** A `Contested` row contributes only while the same label appears in that identity's live `contested_dpns_names`. `record_contest` adds the label before `start_watching`, so the pre-watch interval is covered. `end_watch` removes it; a later contest for the same identity and label would correctly make the status pending again. The parked-flow test checks the pre-watch, watched, and lost states and would fail on the old row-only rule. The rule test checks the ended-list case.
- **D21 fixes the silent expiry for a restored wallet whose status relies on the proof.** Its test requires one `Startup` mark after two status reads and then `StartupIncomplete`; reverting the signal or its once-per-proof guard fails those assertions. The timer lifecycle issue above remains.
- **D22's documentation is accurate.** The remaining marker-read mutant changes only the SQLite read-fault path during a snapshot read. Recording that limit without adding a fault seam is acceptable for this narrow fix.
- **Merge and contract:** `git show --remerge-diff 3448eaeb` shows only conflict markers removed in `apps/desktop/src/fixture/scenarios.ts`; the BL-153 contest scenario and U-05 scenarios are both present. m4 remains version 23, and `DashPay.status` remains in `IMPLEMENTED`. Among generated desktop TypeScript bindings, the only difference from main `cb0735c4` is the `startup` domain in `PlatformChangeName.ts`.

The owner reports gate EXIT 0 on `0315d269` (Rust 1892 passed, Vitest 623, Playwright 656 passed), Swift T0 568 passed, and testnet GL PASS. I checked `/work/scratch/dw-bl153/gl-out/result.json`: it records chip `D@dwrecovera2`. I did not rerun builds or tests; the finding follows from the timer ownership and close paths above.

VERDICT: CHANGES-REQUESTED
