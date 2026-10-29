# DW DP2-01 — Sol r2

Reviewed **0bd536d605aa249883b888e0a9cdbd155d80c5d1**, range **cb6c6d01d2a82f393f56bee5182d8f187c96dbdb..0bd536d605aa249883b888e0a9cdbd155d80c5d1**, in the detached worktree `/work/scratch/dp2-01-r2-sol/wt`. This is the narrow follow-up to `DW-DP2-01-r1-sol.md`: closure mechanisms, shutdown races, invalidation coverage and test strength. E0-10b/DEC-183 adaptation is excluded.

The reviewed source is unmodified. No commits, pushes, GitHub actions, funding or private-directory reads were performed. Independent probes are generated outside the checkout under `/work/scratch/dp2-01-r2-sol/`; this report is left uncommitted.

## R1 closure status

### R1-01 — Closed for the reported queue-only defect

`platform/journal.rs:287` now classifies additions and removals of all three setup kinds by `owner_identity_id`. At the exact platform pin there are four kinds; excluding `ContactInfoDecrypt` includes RegisterReceiving, RegisterExternal and AutoAccept. The owner's `Contacts` change enters a `BTreeSet`, including deduplication against contact/account changes already classified in the same changeset. Queue rows create no journal event. `store.rs:101` still records the classification only after the real persister accepts the changeset.

The tap regression covers owner deduplication, separate dequeue ownership, journal silence and decrypt-only `None`. The session regression goes through `WalletStore` and verifies count **1 → 2 → 2 → 1**, one Contacts signal for each standalone write, an unchanged journal, and Bob remaining in Requests. The middle removal deliberately leaves Carol pending because her account remains missing. The new DASHPAY §3.5 row describes the behavior.

The broader invalidation audit found a separate Core-history carrier gap, R2-01 below. It does not reopen the fixed crypto-queue mechanism.

### R1-02 — Closed

`platform/contacts_model.rs:557` emits `Identities` when a completed refresh has no owned identities. The cold-wallet test at `platform/contacts_tests.rs:1124` holds the real manager's write lock, observes `platform.unavailable`, releases the writer, waits for the Identities signal and then gets live **0**. Unlike a fixture preloaded with identities, this setup cannot receive a pre-existing identity-added signal. Snapshot publication occurs before the completion mark. m4 §2.4 now specifies the empty refresh and the pending count's Contacts/Identities triggers. These are wording changes, with no public surface change.

### R1-03 — Closed

`platform/contacts_model.rs:473` and `:485` use the **same mutex** for checking admission, spawning and recording the handle, and setting `closed`/taking all handles. A competing spawn either registers before close and is included in its drain, or sees `closed` and drops its future. There is no window for a new untracked task between the flag and the abort. Independently, both public sync read paths hold an operation guard; `session.rs:639` drains those guards before closing contacts tasks at `:641`.

Close aborts and awaits every retained task before stopping the pump, shutting down the manager or emitting SessionClosed. A task can finish while close is in progress; it cannot remain alive after close returns. The weak-manager check is correctly removed: a reference count was not a lifecycle signal.

`Owed` is constructed **outside** the async block (`contact_names.rs:214`), so rejecting a spawn or cancelling a never-polled future also drops the claims. Its destructor lands all remaining IDs as failed, clears `in_flight` and drains waiters without signaling or discarding a previous label. Once a fetch answers, popping its ID and landing the result contain no await, so ordinary Tokio cancellation cannot interrupt between those steps.

An aborted refresh can leave its old runtime's `refreshing` membership set. This does not strand a reopened session: `Engine::close_network` removes the old session under the sessions lock, and a subsequent open constructs a new `PlatformRuntime`/default `ContactsRuntime` (`engine.rs:124`, `:153`; `session.rs:413`; `platform/runtime.rs:263`). The old facade is refused by its closed operation gate. These caches and claims are memory-only; cancellation writes no persistent wallet state.

The real-session test holds a fetch, closes the network, releases it and verifies no answer, claim, label or contact signal. Additional isolated probes verify cancellation before first poll, rejection after close, cleanup of multiple queued IDs, preservation of an existing label, and fresh claims in a newly constructed runtime. The latter is a runtime-level check, not an end-to-end reopen test.

### R1-04 — Closed

The Username arm at `platform/contacts_model.rs:360` compares only known-first, folded usernames, then the common id tie-break at `:368`. The new test at `platform/contacts_tests.rs:648` assigns aliases in the opposite order to two unknown usernames' Base58 IDs. It deterministically distinguishes the old comparator. Equal known usernames also take the same common tie-break.

## New findings

### R2-01 — Minor: a Core-only block confirmation changes contact activity without an advertised contact invalidation

**Location:** `rust/crates/dw-engine/src/platform/contacts_model.rs:307` and `:591`; `rust/crates/dw-engine/src/platform/journal.rs:179`; `rust/crates/dw-engine/src/events.rs:553` and `:694`; `rust/crates/dw-engine/src/platform/contacts.rs:154`; `docs/contracts/m4-dashpay-engine.md:231`.

**Scenario:** A sent DashPay payment becomes Confirmed through InstantSend while still unmined. The contact view re-queries on that Payments signal; its last activity still uses the request time because the Core record has no block timestamp. Later the same transaction is mined. Its Core history record gains a block time, changing `last_activity_at` and potentially LastActivity ordering. The payment is already Confirmed, so the library emits no changed payment row. This confirmation can be a Core-only changeset, which the tap does not classify. The host receives **HistoryChanged**, but contacts' documented triggers are only Contacts, Identities and Payments. A host following that contract can retain stale activity and ordering until an unrelated event or manual re-query.

**Evidence:** At platform pin `ebe37f8`, `changeset/core_bridge.rs:1433` explicitly maps `Confirmed + Final` to `None`; `:1559` returns an empty overlay when no verdict changes, and the adapter's store at `:834` then carries the Core records without a payment overlay. The received-payment hook does not repair a sent payment (`wallet/identity/network/payment_handler.rs:307`). Desktop's `BlockProcessed` handler updates history and marks HistoryChanged; its history flush does not mark any Platform change. Contact summaries read authoritative times directly from that history, independently of payment-status changes.

An isolated probe compiles the unchanged exact-head `classify`, `Entry::summary`, `WalletHistory::upsert` and pinned status-transition function against the real pinned changeset/transaction types. Updating the same transaction from unmined to a block at time 100 produces:

```text
contact last_activity_at: Some(2) -> Some(100)
Confirmed + Final: no payment overlay
core-only classify: None
```

Source and evidence: `/work/scratch/dp2-01-r2-sol/source-probe.rs`, `make-source-probe.py`, `source-probe.log`. This demonstrates the component mechanism; it does not inject a live SPV event through a real session. The HistoryChanged delivery and lack of a compensating Platform signal are established by the event path above. This gap predates the r2 commit and is reported under the requested broader invalidation audit.

**Fix:** The smallest correction is to add **HistoryChanged for this wallet** to the contacts/contact activity re-query contract and facade documentation, then cover an already-Confirmed sent payment acquiring a block time. Alternatively, emit an owner-scoped Contacts/Payments invalidation when a known DashPay payment's Core time changes, using the existing coalescing pump and adding no journal news. Preserve Core-only silence for unrelated transactions.

No blocker or major findings. No new shutdown or claim-corruption finding.

## Bulk signals and test strength

The classifier deduplicates within a changeset, and `EventPump::mark_platform` deduplicates across writes by wallet and PlatformChange (`pump.rs:62`, `:122`). Platform flushes have a 250 ms minimum spacing and preserve the trailing edge (`:231`). Thus a bulk sync does not produce a host signal per crypto row: each owner appears once per flush, with at most four Platform flushes per second. A long sync can produce successive refreshes; this is bounded invalidation, not an unbounded loop. Live contact reads write nothing and emit no completion signal; contended reads permit only one outstanding refresh per wallet.

The independent burst probe feeds all three setup kinds in 200 additions, a second owner's clear, and 100 repeated classifications into the unchanged real pump. It observes exactly the two owners in one pending set. The pump's paused-clock tests additionally cover throttle spacing and the trailing edge.

The new branch tests are meaningful regressions, with these limits:

- The queue session test manually changes memory and stores the corresponding deltas. Its auto-accept expiry step faithfully models the library's standalone dequeue, but does **not** execute the real expiry parser/drain. That is sufficient to exercise this host integration; it is not a new test of the pinned library's proof-expiry logic.
- The removal step checks the signal and queue change while the count remains 2; it does not alone demonstrate a Ready transition. Existing channel-state tests cover missing/current accounts and both build kinds. The additional classifier probe explicitly covers RegisterExternal.
- The close test's 200 ms post-release sleep is not its sole evidence: the immediate no-in-flight assertion distinguishes the old surviving future, and the implementation awaits cancellation. Independent probes cover never-polled and rejected futures, which the session test does not.
- The LastActivity tests use preloaded joins; they do not cover a new Core block time arriving without a payment-status change. That is the missing sequence in R2-01.
- The supplied mutation claims were not rerun against modified repository source. The reviewed checkout remains unchanged.

## Validation

All review builds/tests were invoked with `nice -n 10`, `CARGO_BUILD_JOBS=8`, `umask 022`, and on-disk, mode-0700 `TMPDIR=/work/scratch/dp2-01-r2-sol/tmp`. Test roots use `dw_testutil::private_tempdir()` (0700). The review used its own `/work/scratch/dp2-01-r2-sol/wt/rust/target`, initially seeded with an independent reflink copy of existing artifacts; no writable target was shared.

- Exact-head `cargo fmt --check`: pass. `git diff --check cb6c6d0..0bd536d`: pass.
- Fresh `cargo test --locked -p dw-engine contacts --lib`: **33 passed, 0 failed, 1 ignored**. This rebuilt the engine test binary from the detached head. `contacts-test.log`.
- `cargo test --locked -p dw-engine --lib platform::contact_names`: **3 passed**. `names-test.log`.
- `cargo test --locked -p dw-engine --lib platform::journal_tests`: **21 passed**. `journal-test.log`.
- `cargo test --locked -p dw-engine --lib pump::tests`: **4 passed**; `gate::tests`: **3 passed**. `pump-test.log`, `gate-test.log`.
- `cargo test --locked -p dw-engine --test m4_dashpay_contract`: **9 passed**. `m4-test.log`. No blessing was run.
- `cargo test --locked -p dw-engine --test e0_05_bringup a_restored_wallet_gets_the_full_budget -- --exact --nocapture --test-threads=1`: **1 passed**. SPV started **20.024676292 s** after the call, within the 21 s ceiling. `e0-05-alone.log`.
- `cargo clippy --locked -p dw-engine --all-targets -- -D warnings`: **pass**. `clippy.log`.
- The separately compiled source-extraction harness: **8 passed**, covering the Core carrier gap, unpolled/rejected/aborted name batches, all setup kinds, and the unchanged pump's burst/coalescing tests. `source-probe.log`. The Core test deliberately demonstrates the current missing invalidation; its pass is not a claim that the contract is satisfied.
- While the fresh build was running, the copied binary named by the supplied gate log (`dw_engine-63e1374aee5aedc6`) also ran the contacts filter serially: **33 passed, 1 ignored**. `cached-contacts-test.log`. This artifact result is additional evidence, distinct from the fresh build above.

Logs, the standalone probe binary/source, and the clean detached checkout remain under `/work/scratch/dp2-01-r2-sol/`. The private build target and temporary test data were removed after validation. No full workspace gate or live testnet fixture was rerun.

The supplied gate log independently totals **890 passed, 1 failed, 5 ignored**. Its failure is the stated E0-05 budget assertion: **21.406376065 s** against **21 s**. That log attests a failed aggregate run, not a green full gate, and it does not itself embed the reviewed hash. The implementer's two isolated passes are supplied evidence; the review's isolated result is recorded above. The budget issue is outside this r2 diff and is not the reason for this verdict.

All four original items are closed by mechanism. R2-01 needs the existing HistoryChanged event connected to the advertised contact-refresh contract, or a targeted compensating invalidation and regression coverage.

**Verdict:** CHANGES-REQUIRED
Sol gate satisfied: no
