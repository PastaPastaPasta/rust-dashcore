# DW E0-06 — independent code review, round 2

Reviewed `dw/e0-06-changeset-tap` at **8b459a01f379bd0b1417041d71844f408772c6e3**, against desktop `main` **de1c3737a2fff22a48c4c7185c3f20de977d17cb** (DP1-05 merged). The five commits are 211ae4a, 1bcddad, f5d4270, 115334a and 8b459a0; the base-to-head diff is **19 files, +2682/−24**. Read r1, DEC-110/114, the changes, relevant pinned upstream persistence/discovery/marketplace paths and the recovery scheduler. DEC-114 supersedes r1's demand for unknown-age deferral/reconciliation: no such machinery is requested here.

**Verdict: REQUIRES CHANGES.** **M1 is not closed end to end:** the initial store refusal works, but upstream retains the refused identity in memory and a later successful snapshot can persist it without provenance. **M2's authoritative-age defect is closed; its phase coverage remains partially open. m1 is closed.** The phase race is low/cosmetic in effect; the trust-provenance finding is separate and is not covered by DEC-114's cosmetic residual. Workspace tests reproduce **633 passed / 0 failed**.

## Findings

### R2-1 — major: a refused flag write can become an unflagged successful store through the live cache

**Locations:** `rust/crates/dw-engine/src/store.rs:104`; `rust/crates/dw-engine/src/platform/journal.rs:552`; `rust/crates/dw-engine/src/platform/recovery.rs:677`. Pinned upstream `platform@bc41f1bc233dec4607d387101c1d9c2f111019b2`, `packages/rs-platform-wallet/src/wallet/identity/state/manager/lifecycle.rs:62`–86 and `:122`–138; `.../managed_identity/identity_ops.rs:124`–133.

The new direct-store busy test is correct: `flag_unverified` fails, `WalletStore::store` returns `Transient`, and SQLite sees no changeset. But returning the error does not prevent the library from accepting the fetched entity into its live state. `IdentityManager::add_identity` inserts the managed identity and side index **before** persistence, logs a store error and returns `Ok(())`. Its out-of-wallet identity insertion has the same behavior. The ordinary profile and name setters also update memory and log persistence errors. The tap records no failed-provenance state beyond returning the error.

The reviewer probe `review_probe_a_refused_flag_write_leaves_discovery_visible_without_provenance` demonstrates this through the real session and public discovery entry point, using the repository's `MockPlatform` discovery seam. The wallet-manager insertion, session store, facade reads and later profile setter are real; the SDK scan/key-derivation/network call is replaced, not exercised live:

1. Set trust fallback on; start explicit discovery and hold its response after the host's prelude has finished.
2. Hold app.sqlite with a second connection's `BEGIN IMMEDIATE`; release the response. The real upstream identity insertion calls the session store, whose mandatory flag transaction fails at the busy timeout.
3. Discovery nevertheless returns **success, 1 identity**, and `DashPay::identities()` exposes it. SQLite has **0 identities**, and app.sqlite has **0 trust flags**. Recovery's “stored” count reads the manager's live identities, not the accepted database rows.
4. Release the writer and turn fallback off. Call the real managed-identity `set_dashpay_profile(None, persister)` setter. Its whole-identity snapshot now succeeds: SQLite has **1 identity**, while trust flags remain **0**. No verified re-fetch occurred between these steps.

Thus the failure is not limited to volatile display data: fallback-fetched data can reach disk later after the provenance attempt was forgotten. The tap cannot distinguish that snapshot from verified data once fallback is off. The planned E0-10b/DP3-04 gate cannot safely infer verification from the absence of a row. No actual payment was attempted, and this review does not claim that the not-yet-implemented money gate has already been bypassed; this is a demonstrated defect in the provenance it is supposed to consume.

**Fix:** retain a general fail-closed state when any mandatory provenance write fails. The simplest conservative option is a **Live/session quarantine latch**, consulted both before subsequent Platform stores (including after fallback turns off) and before data can be used as verified or admitted by the money gate. Keep returning the store error before `inner.store`; do not clear quarantine merely because a later unrelated flag write succeeds or fallback ends. Clear it only after discarding/rebuilding the unsafe live cache from safe persisted state, or an equivalently proven verified rebuild. A close/reopen recovery avoids an entity-by-entity reconciliation ledger. Do not synchronously re-enter/close the manager from `store`, whose caller can hold its write lock.

An alternative is to make all relevant upstream mutators persist first and propagate refusal, but fixing only owned identity insertion leaves the watched-identity and setter paths. One general quarantine is preferable to another sequence of caller-specific repairs. This provenance requirement is independent of notification ages and does not request reconciliation for DEC-114's declared residual. Extend the busy regression through a real producer and the fallback-off/later-snapshot transition; a safe implementation must refuse/quarantine/discard the unsafe state or durably flag it before that final acceptance.

### R2-2 — medium, required: the owed-phase mark has no owner and can be discharged by the wrong bring-up

**Locations:** `rust/crates/dw-engine/src/platform/journal.rs:367`, `:395`, `:450`, `:458`; `rust/crates/dw-engine/src/platform/bringup.rs:321`, `:449`, `:475`; `rust/crates/dw-engine/src/platform/recovery.rs:684`.

`recovering[wallet]` is a pair of booleans, not the ownership of particular queued work. Every admitted bring-up creates a `BringUpPhase`, including one admitted before a recovery existed. Dropping it unconditionally clears the wallet's current bring-up bit. A discovery can meanwhile set that same bit for the *later* bring-up its Readmit signal queues. The earlier run can therefore consume a later run's protection.

The scheduler permits this order:

1. The initial bring-up is running, held at its Platform await.
2. Explicit discovery stores an identity and queues its bring-up and names pass, setting both owed bits.
3. The original bring-up completes. Its drop clears the owed bring-up bit, even though the Readmit bring-up has not run.
4. The supervisor starts its loops and runs the due names pass **before** processing the queued Readmit. Completion clears the remaining names bit and removes the recovery mark.
5. The Readmit bring-up sees an identity on file, so the `identity.is_none() && !created_here` predicate supplies no local catch-up guard. Its unknown-age event is stored unread.

The reviewer probe `review_probe_running_bring_up_consumes_a_later_discoverys_mark` exercises the real session, supervisor, discovery entry point, wallet-manager mutation and session persister, substituting only Platform calls with deterministic held futures. Its queued bring-up stores an unknown-age owned name; the journal row is unread. This is a scheduler integration reproduction, not merely calls to `begin_recovery`/`end_phase`. It does not claim a live DAPI reproduction.

**Classification of the self-reported race:** low/cosmetic in observed effect. Authoritatively old events still become read, journal deduplication still applies, and trust flags/money movement do not consult read state. Nevertheless, this event is written *inside a required recovery phase*. The declared residual permits unknown-age events first surfacing *outside* phases; it does not waive DEC-114's separate requirement that each recovery phase hold `catch_up(id)`.

**Design judgment:** the mark is another source of the DEC-110/114 “one more path” pattern. Protection depends on duplicating scheduler state in the tap, coordinating `names_due` and `recovering`, detecting rediscovery, finishing arbitrary bring-ups, and cleaning up on lifecycle exits. The order test proves two boolean transitions in isolation; it cannot prove that the transitions belong to the work they discharge. Fixing this race with epoch counters or phase tokens would add machinery to solve a problem that DEC-114 already bounds cosmetically.

**Simplest single-source alternative:** keep the tap's existing reference-counted `CatchUp` RAII mechanism as the sole source of active silence. Acquire `self.catch_up(id)` at the entry of **every admitted `bring_up_wallet` run**, through its exit, rather than conditioning it on identity absence. Keep the guards already added to `discover_identities_of` and `names_pass`. Every bring-up can replay recovered state, including a queued discovery bring-up with an identity already on file; a universal entry guard also covers retries and does not need to know who queued it.

Remove `recovering`, `Owed`, `Phase`, `BringUpPhase`, their begin/end wrappers and lifecycle hooks, and the `names_due_of` completion cross-check added solely for this mechanism. Retain DP1-05's existing names scheduling/cancellation and discovery teardown barriers. Between actual phases, use the authoritative-time boundary exactly as DEC-114 specifies; an unknown-age event then falls within the declared residual. Do not require gap-wide silence, a persisted recovery ledger or notification reconciliation. Update the contract to describe active phase guards rather than a promise to carry owed bits across tasks. Add the overlapping discovery/bring-up regression, asserting **read**, to the normal suite.

## Disposition of r1

| Finding | Round 2 disposition |
|---|---|
| M1 — lost mandatory trust flags after a successful wallet store | **Initial refusal fixed; provenance still open (R2-1).** `WalletStore::store` propagates the flag error before `inner.store`, and `flag_unverified` commits its rows in a separate transaction. Busy/locked/full/I/O/out-of-memory map to `Transient`; other failures map to `Fatal`. The supplied direct-store busy test passes, but a real producer swallows that refusal, retains the identity, and a later snapshot persists it unflagged after fallback ends. |
| M2 — non-authoritative ages and incomplete recovery coverage | **Age handling closed; phase coverage partially open (R2-2).** Payment time comes from `get_core_tx_record` after acceptance, and `context.block_info()` returns none for both mempool and InstantSend-only records. Name age comes from the durable marketplace row's transfer time, else creation time; acquired/fetch time and host history are not ages. Request/relationship ages use the accepted request `$createdAt` values. Discovery and names now hold local guards, and ordinary owed-phase tests pass, but the queued bring-up relies on the unsound mark. |
| m1 — identity/key tombstones not flagged | **Closed.** Identity `removed` IDs and both key upsert/removal owners enter the identity flag set. The test includes both tombstone forms. Contract §5 explicitly exempts removed requests and name-state rows, which carry no request `$createdAt` or label payload to flag. |
| m2 — independent removal fence | **Still open, low, with r1's reachability qualification.** No generation/admission check was added to journal recording or pending Platform delivery. `forget` clears boundary/recovery caches, not pending signals. The current synchronous journal-producing manager-lock invariant still narrows the row-recreation case; I did not reproduce a production journal store escaping it. This round adds no evidence warranting a severity increase or new reconciliation architecture. |

The recovery lifecycle hooks are present: removal/unload call `tap.forget`, rollback calls `end_recovery`, and dropping Live drops the tap. That confirms the claimed hooks, not sound task ownership. The helper test for finishing phases in either order is valid for its narrower assertion. The new session tests for restore names and discovery handoff cover a sequential completion; they do not force discovery to overlap an already-running bring-up. At the pin, marketplace rows copy document creation/transfer times directly; the contact-request decoder copies document `$createdAt`, which is required by both DashPay contract versions. Thus the accepted request timestamps are source times, not a fetch-time fallback.

**Synchronous/best-effort limitation remains:** journal failure is logged after the persister has accepted; signals are best-effort display refreshes. Trust flags are now separate and mandatory. SQLite calls, including the new durable age reads, remain synchronous in `store`; appdb retains its five-second busy timeout. Thus r1's runtime-worker/manager-lock latency objection has not been implemented away, and a blocked store is not merely a journal append. I found no new reverse lock cycle and do not claim that the flag fix also fixes latency. This scoped round does not request an offloaded writer or notification reconciliation subsystem.

## Open questions and limits

The smallest universal-entry-guard alternative intentionally treats ordinary warm-start bring-ups as guarded replay too: genuinely new arrivals during that pass would also be read. DEC-114 settles recovery silence; it does not expressly decide all warm-start notification policy. If ordinary warm bring-ups must remain live, carry the recovery mode on the existing scheduler job/readmission and acquire `CatchUp` for that job, including its rescheduling; let the scheduler own pending recovery work rather than maintaining a second obligation ledger inside the tap. This is a policy constraint on the simplification, not a reason to add reconciliation or patch the boolean ledger with more finish paths. In either case the queued recovery bring-up itself must own a guard. No trust decision depends on read state. The temporary probes use the repository's offline Platform seam; live Platform timing and macOS UI/FFI regeneration were not tested. The existing ignored network tests remain opt-in.

## Validation

All commands used the detached worktree's own `rust/target`, Rust **1.98.1**, `CARGO_BUILD_JOBS=12`, `TMPDIR=/dev/shm/e0-06-r2` (0755), and `umask 022`. The implementer worktree was read-only. Before adding probes, `git diff --quiet` confirmed exact head 8b459a0.

- `cargo fmt --all -- --check`: **PASS**, exit 0.
- `cargo clippy --workspace --all-targets -- -D warnings`: **PASS**, exit 0.
- `cargo test --workspace --no-fail-fast`: **PASS**, exit 0, **633 passed / 0 failed / 3 ignored** on the first full run. The ignored cases are the two public-testnet fixtures and the OS-keyring opt-in test. No transient failure needed a rerun. The fresh build took 56m 01s on the heavily loaded shared VM; Clippy's reported 72m 22s included waiting for that build lock.
- With temporary tests appended only in the detached tree, `cargo test --workspace review_probe -- --nocapture --test-threads=1`: **PASS**, exit 0, **3 reviewer probes passed** (asserting observed behavior, not approving it), in 6.77s after compilation:
  - `review_probe_session_uses_durable_authoritative_ages`: prior committed Core/name rows, with no host history, produce read events; the mempool payment and unknown-age name remain unread. This verifies the production `Times` implementation through the actual session persister, beyond the supplied fake-times classifier fixtures.
  - `review_probe_a_refused_flag_write_leaves_discovery_visible_without_provenance`: discovery reports success with a live identity but zero durable identities/flags; after fallback-off and a real profile setter, one durable identity exists with zero flags. Confirms R2-1.
  - `review_probe_running_bring_up_consumes_a_later_discoverys_mark`: a queued recovery bring-up writes `username_registered`, `unguarded@900`, **unread** after the earlier run and names pass consume the two bits. Confirms R2-2.
- Guard control: with only an added entry `self.catch_up(id)` guard in `bring_up_wallet` and the probe's expected read bit changed to true, `cargo test --workspace review_probe_running_bring_up -- --nocapture --test-threads=1` **PASS**, exit 0. The same schedule writes `unguarded@900` **read**. This tests the guard mechanism; it does not implement the full tracker removal or settle warm-start policy. All control/probe changes were temporary.

## Cleanup

The three touched scratch source files were restored to exact head 8b459a0 and verified clean before removing the detached worktree. Its build artifacts, reviewer tests, control patch and scratch logs were removed; the test TMPDIR was removed after it was empty. The implementer branch/worktree was not edited. This report is the only requested output and is **uncommitted**. No commits, pushes or GitHub comments.
