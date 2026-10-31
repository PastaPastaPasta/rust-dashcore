# DW E0-04.P2b — narrow Sol review, round 5

Exact head: `52215e39c02c6cf78c6ed6adb9ba6b33aa95bfcf`, branch `dw/e0-04-p2b-mode-b`, one commit after r4's `5a1e3abbddcc46c37b21ac38c65e6b230549ef01`. Detached review worktree: `/work/scratch/p2b-r5-sol/wt`. Scope: **R4-01 only**, including the signing/restart premise, the changed AT0 expectation, and DP1-02's dependency on the funding status and signed-lock accessor. Reviewed the actual four-file diff and the relevant unchanged producer, journal, signer and gate paths. No source-worktree edits, commits, pushes or GitHub actions. This report is uncommitted.

**R4-01 is closed at this head.** The legacy recovery-only route to NotSent is removed, including its recorded-but-empty-conflict equivalent. No remaining unsafe NotSent path was found within the documented, verified one-lock-per-call pin contract. The original signed withheld-lock counterexample now passes, as do the independent crash/restart and forgotten-record probes.

## Findings

No blocker, major or other required change in this scope. Approval status: **APPROVED**.

The §2a.6 one-lock-per-call restriction remains an explicit assumption, rather than a transaction-count restriction enforced by the permit type. I verified the current pinned producer and DP1-02 caller honor it. This approval does not generalize to a caller that builds multiple independent locks with the same permit's signer.

## 1. Remaining routes to NotSent

There are two production routes, and neither grants a retry while a signed funding may still confirm:

- **A ChainLocked self-spend with coverage.** `rust/crates/dw-engine/src/platform/lease/mode_b.rs:296` rejects Legacy (`conflicts = None`) and recorded empty conflicts. For nonempty conflicts it checks the exact lock txid and an actual input outpoint of every signed lock still known, including an undurable process-local lock. `:615` supplies this predicate as the sole self-spend authority. There is no recovery disjunct, field or trait method left. Proved rows and a step's Sent still win (`:497`); unknown rows remain MaybeSent. A partial record covering one of two known locks cannot resolve the step.
- **A definitely-unsent call resolution.** `mode_b.rs:521` still accepts a durable `CallStepStatus::NotSent`, without a self-spend. This is appropriate: the call never started, or failed with no released or in-flight signature and no earlier marker (`lease/call.rs:733`). `SignatureCount::close` closes the signer before reading both in-flight and released counts (`call.rs:299`). `lease/fence.rs:3002` writes a NotSent resolution only when no call remains and none is possibly out. A failed resolution write retains the marker; only a successful durable write supersedes it. There is consequently no signed lock requiring conflict coverage on this route.

A record naming a lock the restarted process has forgotten is considered separately in item 2. The nonempty record is evidence preserved by the authenticated writer, not evidence inferred from chain absence.

**Repair now fails closed without a signed lock.** `mode_b.rs:854` unconditionally refuses NoKnownLock. A trusted matching Built/Broadcast row, a ChainLocked legacy self-spend, and a complete scan alone cannot bypass that check. Removing the fallback for empty recorded rows is justified: at the pinned key-wallet revision, `key-wallet/src/utxo.rs:78` makes an unlocked non-coinbase output spendable without requiring confirmation. A peer can hold both an unconfirmed parent and its signed child lock; no complete scan of the current chain proves that child impossible.

## 2. Item 6: signing, crash, restart and a second call

**The argument is sound at the current pin, per standing funding marker.** It does not mean a step can never have multiple zero-signature attempts over its lifetime.

1. **Before admission / marker durability.** `call_permit_step_fenced` performs the funding gate and marks the step in the same J step (`lease/call.rs:486`, `:518`). The gate checks the standing marker independently of the derived status (`mode_b.rs:701`). Even a marker whose write is still running prevents another funding permit. The first permit is handed back only after its marker write succeeds and the lease/deadline checks pass (`call.rs:544`). Refusal or cancellation before handoff signs nothing.
2. **After signing, before any signed_lock write.** The FULL marker already exists. A crash loses the call's counters and perhaps its wallet row, but does not turn the marker into a definite resolution. On load it is restored as durable (`lease/fence.rs:1743`); with no rows/proof it reads MaybeSent. The signed-lock list may be empty, but the marker still blocks another funding call, and Repair refuses NoKnownLock. The independent probe snapshots the journal before calling finish/drop or the signed-lock tap and verifies all these assertions on a fresh table.
3. **A signed_lock write fails, or the call is cancelled/returns an error.** The tap retains the authenticated lock in memory even if its write fails (`fence.rs:3220`). A released signature, or one in progress when the permit ends, produces MaybeSent and leaves the marker standing (`call.rs:733`). Dropping the permit runs the same end path. A retained cloned signer cannot start another signature after the count closes. A vault refusal that never produced a signature can support a definite unsent resolution; it cannot hide a released signature.
4. **Restart without the signed_lock record.** No new funding call is admitted, regardless of row discovery or scan completeness. A Resumable permit under the same step cannot bypass the gate by dropping the funding key: `call_key_differs` rejects it before admission (`fence.rs:1026`). The independent second-call probe verifies both refusals.
5. **A fresh conflict record survives while its lock record does not.** Repair can write this row while the authenticated lock is still known locally. `record_self_spend` checks that no call is running, the marker stands, a signed lock is known, and its supplied actual transaction inputs cover every known signed lock (`fence.rs:3134`; `mode_b.rs:797`). The row records the lock txid and the input it takes. After restart the signed-lock list can be empty, but no intervening second signer was admitted. The surviving nonempty record still names the one possible signed lock. Once its self-spend ChainLocks, accepting it is safe. The independent forgotten-record probe verifies MaybeSent before the ChainLock, NotSent afterwards, and refusal of a second funding permit in both states.
6. **After Repair yields NotSent.** Repair writes no durable NotSent call resolution; its original marker still stands. The same step therefore cannot open another funding permit even when its derived status is NotSent. The coverage record cannot become stale over a new signing call of that step. A true zero-signature NotSent resolution does supersede its marker and associated records, so an admitted later attempt does not inherit stale self-spends (`fence.rs:1035`; `dw-appdb/src/dispatch.rs:632`).

The remaining producer premise is verified against platform pin `ebe37f8a679b1a552483dc7ff12c306291e9a32b` and its patched rust-dashcore pin `d3d50520a26f465a43851362fdcb12e0776ade14`: `create_funded_asset_lock_proof_pooled` invokes the broadcast builder once, then waits for that outpoint's proof (`rs-platform-wallet/src/wallet/asset_lock/build.rs:853`). The broadcast builder invokes the transaction builder once (`:962`), which calls `build_asset_lock_with_signer` once (`:278`); key-wallet makes one signed transaction (`key-wallet/src/wallet/managed_wallet_info/asset_lock_builder.rs:548`, `:417`). Errors return; there is no rebuild/retry loop under the same signer. Resuming a tracked lock rebroadcasts its existing transaction rather than constructing another lock. DP1-02's production `fund` calls this entry point once with the permit's counting signer (`registration_flow.rs:2976`).

The documentation at `docs/design/E0-04-grants-leases.md:712` correctly retains the limitation: a future multi-build call with partial lock-record persistence needs a stronger rule. At this pin, a crash before the record and a subsequent call do not falsify the one-signer argument.

## 3. Fresh Repair, legacy rows and the liveness residual

The actual Repair producer obtains the input outpoints from the prepared signed bytes, validates their txid against the prepared summary, records them, and broadcasts that same prepared transaction (`lease/repair.rs:183`–`:218`). Recording occurs before broadcast. A legacy row is not promoted by re-recording its txid: the journal preserves the earlier row and provenance (`dw-appdb/src/dispatch.rs:909`), and the in-memory path preserves an existing row too. A fresh, distinct self-spend receives its own provenance-1 row beside the legacy one.

The committed real-schema-4 migration/restart test at `mode_b_tests.rs:2827` passed: the ChainLocked legacy row leaves MaybeSent, fresh Repair remains MaybeSent before its ChainLock, the new row yields NotSent afterwards and across restart, and both the old Legacy and new Conflicts row survive. The production Repair-session tests also passed.

If the legacy spend already consumed all usable inputs of the signed lock, the fresh draft cannot provide the required conflict: Repair refuses LockInputsSpent (`repair.rs:151`), or the earlier no-spendable-coins check if there are no remaining coins at all. The legacy row still cannot resolve the step. The resulting indefinite MaybeSent is the documented liveness cost and does not authorize a second payment.

## 4. Item 7: AT0's Sent → NotSent

**Safe in the test's setup.** `mode_b_tests.rs:1222` now gives AT0 an authenticated own lock, txid 6, and records a self-spend taking that lock's input. The later reconstructed index-0 row has txid 1. It is not AT0's signed lock; a match by index does not establish ownership.

While that unrelated row has a proof, AT0 temporarily reads Sent because positive evidence wins. That Sent is not made durable: the row is untrusted and not an own signed lock (`mode_b.rs:645`). When the source supplies a proof-less Broadcast row and AT0's covering self-spend is ChainLocked, AT0 may read NotSent. Its unique possible signed funding is dead; the unrelated row's continued ability to confirm cannot pay AT0's funding a second time. The test asserts both the statuses and the exact event sequence `[Sent, NotSent]`, and passed. A durable Sent or a proof of AT0's own lock would continue to win; this change does not revoke such evidence.

## 5. DP1-02 compatibility

Inspected DP1-02 at `6bcaa2f8eaec676587ea899bb89953aa036e1711`. Its merge base with this P2b head is exactly `5a1e3ab`.

Removing recovery_complete breaks no DP1-02 production interface or dependency found here. DP1-02 has no reference to that method/field outside its inherited P2b lease implementation/tests, and supplies no recovery override. Its only code change to `mode_b.rs` removes the unused-code expectation from `signed_lock_txids`; this accessor's signature, contents, persistence and behavior are unchanged by 52215e3 (`mode_b.rs:1128`).

DP1-02 reads those exact txids to reserve a draft for its own lock. It releases that reservation only when P2b reports NotSent (`registration_flow.rs:932`, `:938`). Removing the legacy/empty-record route strengthens the guarantee it relies on. A failed-write/restart empty list still does not authorize another payment: the P2b funding permit refuses the standing marker, and DP1-02's rejected-call settlement refuses to return to KeysPrepared when P2b reads Sent, WillBeSent or MaybeSent (`registration_flow.rs:2272`).

This is a source-level compatibility check, not a claim that the complete combined DP1-02 branch was rebuilt or rebased. DP1-02's current 5a1e3ab-based lease source still contains the old fallback; it must incorporate this P2b fix to inherit the new guarantee. No DP1-02 or P2b branch was changed during review.

## Validation and limits

- Exact-head `cargo test --locked -p dw-engine -p dw-appdb --lib --no-run`: **exit 0**. A review-owned target was seeded with copied dependency artifacts, then Cargo rebuilt and checked this head. Logs: `/work/scratch/p2b-r5-sol/build.log` and `build.jsonl`.
- Unmodified exact-head suites: dispatch **25/0**, Mode B **64/0**, call permits **22/0**, Mode B sessions **6/0**, Repair sessions **4/0**, unrecorded-self-spend release **1/0**. **122 passed / 0 failed**. Logs and commands: `baseline-results.json` and individual suite logs.
- Replayed both tests from `/work/scratch/p2b-r4-sol/r4-probes.rs` in a copy of the current engine crate. The original invariant and its genuinely signed type-8 lock A and disjoint signed self-spend S are retained. Mechanical changes: unique test names, the same r4 crypto helper, and replacing the deleted recovery-completeness helper with another refresh of the already established empty snapshot. The original withheld-lock counterexample now reads **Some(MaybeSent)** and passes; the retained-Built rejection replay also passes.
- Three independent boundary probes passed: crash after signing but before the signed-lock tap refuses a second funding signer and disguised Resumable call; a persisted conflict record safely covers a forgotten undurable lock and does not admit another signer; and a self-spend missing a second known lock is refused before recording. Together with the two r4 replays: **5 passed / 0 failed**. Source: `r4-probes-adapted.rs`, `r5-boundary-probes.rs`; result: `probes-results.json`, `original-r4-replay-and-boundaries.log`.
- Probe compilation uses this head's exact Cargo-selected rustc command/dependency artifacts. Only review tests in the separate copy were added; production logic was unchanged. An initial copy omitted the CSV test fixture and did not compile; copying the unchanged fixture tree corrected that review harness setup, after which compilation and all five scoped probes passed. A first broad test-name filter also ran two unrelated existing name tests; the final five-test run uses the full Mode B module filter.
- `cargo fmt --all -- --check`, `git diff --check 5a1e3ab 52215e3`, and `docs/design/checks/e0_04_dispatch_model.py`: **passed**. The model is supplementary and is not claimed to model the withheld-lock counterexample.

No live peer-held transaction, mining/ChainLock experiment, full workspace/DP1-02 integration test, CI run, or macOS/Swift execution is claimed. The counterexample uses real signatures and SQLite migration/load paths with fake tracked/Core evidence; the additional crash probes load durable-only fake-journal snapshots through the production LeaseTable. The scope is the no-double-pay implication of R4-01, under the existing trusted journal/ChainLock and pinned caller contracts.

All review builds and tests use nice -n 10, CARGO_BUILD_JOBS=12, umask 022 and disk TMPDIR `/work/scratch/p2b-r5-sol/tmp` at 0700. The private target and probe executable were deleted after validation; logs, probe sources and the clean detached worktree are retained. No process-pattern kills, source edits, commits or pushes.

**Verdict:** APPROVED — R4-01 closed at 52215e3.

Sol gate satisfied: yes
