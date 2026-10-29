# DW E0-04.P2a — Sol review, round 1

**Verdict: REQUIRES CHANGES. Sol gate satisfied: no.**

The advertised 658/0 gate, design model and three mutation checks reproduce. Additional seeds also pass. Independent witnesses nevertheless reproduce failures in write-ahead exclusion, definite-unsent retry authority, the actual leased-send deadline, DEC-134's removal synchronization, exact-cap preparation and lease ownership. The stress checker accepts traces that violate obligations it is supposed to help validate. These require another Sol round before the Opus-high gate.

## Reviewed scope and authority

- Date: 2026-10-09. Reviewer: Codex, gpt-6.1-sol, independent first review.
- Desktop worktree: `/work/home/agent/workspace/dw-wt/e0-04-p2a-leases`.
- Branch: `dw/e0-04-p2a-leases`; reviewed HEAD `270665a463b76bf7ab31fda5bfdcb931166d76c6`.
- Main/merge base: `de1c3737a2fff22a48c4c7185c3f20de977d17cb`. Reviewed the entire merge-base-to-HEAD diff: four commits, 38 files, 7,813 insertions and 182 deletions, including tests, FFI and the DEC-134 follow-up.
- Binding design: desktop `docs/design/E0-04-grants-leases.md`, approved at `723d9d3`, with the §15 edits at `f048a65`; particularly §12 [A+B], §13 P2a, H10/H14 and the row-less tombstone rules.
- Binding decisions: DEC-57, DEC-67, DEC-73 and DEC-134 in integration `docs/node-program/DECISIONS.md`.

Source references below are relative to the desktop worktree. Line numbers refer to the reviewed HEAD, without temporary review tests. No implementation fix was made. Temporary test-only includes were restored byte-for-byte; the desktop worktree is clean and HEAD unchanged. This report is deliberately uncommitted.

## Findings, by severity

### F1 — High: a differently scoped copy transports before a resumable step's marker is durable

**Location:** `rust/crates/dw-engine/src/platform/lease/fence.rs:1164`, `:1178`, `:1283`; existing tests at `lease/tests.rs:1393` and `:1469`.

The step-scoped First installs a globally visible `RowlessState::Admitted` placeholder before its marker write completes. A repeat defers only if the **incoming request's** step finds `Mark::Committing`. The identical artifact under no step, or a different step, receives `Resend` immediately. Thus the placeholder confers transport authority before the write-ahead obligation is satisfied. The existing tests intentionally expect this admission; they do not transport the copy before observing the write result.

Concrete interleaving, reproduced with a controllable backend wrapping real SQLite:

1. A live lease admits artifact h in `registration/d/identity`; block its `insert_step` before the SQLite write.
2. Admit the same h under the same lease with no step. It returns `Resend`.
3. Record the copy's transport and finish it `MaybeSent` while the marker writer remains blocked.
4. Release the writer with an injected pre-write failure. The original First returns `Deferred`; SQLite contains **zero** step markers.

Observed: `copy sent before marker: true; First: Deferred; durable markers: 0; I1-I6: Ok(())`.

After a crash, all knowledge of that actual handoff disappears. The step has no marker to make a subsequent refusal say `step_possibly_dispatched`, although h may execute. A flow can consequently classify its resumed step as canceled and construct a different payment/transition. This violates §5.4's artifact-wide exclusion, §6.4, H10 and I11. Repeating the same bytes is normally safe; allowing the only durable evidence of their possible execution to be absent is not.

**Required fix:** associate pending/ambiguous durability with the artifact, independently of the new request's scope. Every copy must defer until the required marker succeeds; after an ambiguous write, every transport path must first repair that obligation. Preserve one charge and aggregate attempt settlement. Replace the tests that bless early `Resend` with transport-before-marker and crash/fault regression tests.

Witness: `review_repro_copy_sends_before_step_marker_and_checker_accepts`.

### F2 — High: a durable marker revives a refunded, definitely-unsent transition after Lock

**Location:** `rust/crates/dw-engine/src/platform/lease/fence.rs:1182`, `:1200`, `:1394`; §5.5's state-transition tombstone rule.

`RowlessState::NotSent` falls through to the durable-marker lookup. That lookup replaces the tombstone with an admitted entry and returns an uncharged `Resend`, before checking the lease. The marker therefore overrides stronger, positive evidence that the previous attempt never sent.

Reproduced sequence:

1. Admit a step-scoped transition with a live 100-credit lease, charging 10 credits; its marker becomes durable.
2. Finish its sole attempt `Outcome::NotSent`. Settlement is `DefinitelyUnsent`, status is `NotSent`, and the 10 credits are refunded.
3. Lock and revoke the lease.
4. Admit the identical request again. It returns `Resend`; finishing it `Sent` changes status to `Sent` despite the dead lease and refunded charge.

Observed: `NotSent + refunded + revoked step was resent: true; new status: Some(Sent)`.

The user is allowed to retry a different artifact on the earlier `NotSent`. The old artifact can now also execute, making two logical payments possible, even though no ledger executes the same txid/transition twice. This is also spending outside the current budget accounting. The binding design explicitly requires an identical transition after definite rejection to be a **fresh First under a live lease**, replacing the tombstone and charging normally.

**Required fix:** honor a process-local `NotSent` tombstone before marker-based recovery. A retry needs live authority and a fresh charge. Keep the conservative marker behavior for a genuinely unknown prior-process attempt; test the distinction among unknown recovery, definite rejection, nonce-slot resolution and post-lock repeat.

Witness: `review_repro_settled_step_resends_after_refund_and_lock`.

### F3 — High: `fenced()` lets a leased First enter the actual SPV dispatch after its deadline

**Location:** `rust/crates/dw-engine/src/send/mod.rs:1228`, `:1277`, `:1298`.

The First arm awaits `dispatch()` without consulting `permit.deadline()` or bounding pre-enqueue work. Its comment says L5/L10 clamp the drain rather than this call, but L5 requires transport only before the deadline and under `timeout_at`, and §5.1/L10 require the Core enqueue/acceptance split. A call beginning at the engine wrapper is not evidence that transport has begun: the pinned library first awaits the wallet-manager read guard and then further SPV/configuration/network guards.

Pinned path checked: platform `bc41f1b`, `packages/rs-platform-wallet/src/wallet/core/broadcast.rs:129` (`wallet_manager.read().await` at 135, broadcaster at 158), `src/spv/runtime.rs:323`; rust-dashcore `40268cc`, `dash-spv/src/client/transactions.rs:32` and `:68`. The latter starts its acceptance deadline only after submission and does not know the lease deadline.

Reproduced through a real session and the actual initialized SPV client:

1. Prepare a leased payment. Initialize SPV, with no network run loop/peers.
2. Hold the wallet-manager write guard (`wallet.state_mut()`), start broadcast, and wait until the lease has an in-flight permit.
3. Lock. It returns after approximately 10.1 seconds, reporting `MaybeSent`, while broadcast still waits for the manager guard.
4. Release the guard after the deadline. The actual SPV dispatch now runs and returns `BroadcastRejected { reason: "SPV broadcast not sent: no connected peers" }`.

This is a demonstrated late entry to the real dispatch path, **not** a demonstrated on-wire send: the witness deliberately has zero peers. With peers, the same unchecked path proceeds to enqueue after the deadline. Late bytes from a transport already started before the deadline are allowed by the design; starting this transport after it is not. The known SPV gap hides this bug.

**Required fix:** enforce the permit deadline across every pre-enqueue wait and at the actual enqueue boundary; separate acceptance waiting. Any timeout/cancellation with uncertain submission must retain the reservation/pending-spend protection and report `MaybeSent`, never infer `NotSent`. Add real or injectable-broadcaster tests for First→Sent, MaybeOut, a stalled manager/network guard and deadline expiry. Deferring platform wiring to P2b/P4 does not validate the engine-owned leased `TxDraft` path introduced in P2a.

Witness: `review_repro_fenced_send_continues_beyond_permit_deadline`.

### F4 — Medium: duplicate concurrent `register` calls defeat DEC-134's erase barrier

**Location:** `rust/crates/dw-engine/src/platform/lease/fence.rs:796`, `:817`, `:1523`.

`registering` tracks one boolean per `(wallet, txid)`, not all in-flight writers. While no completed entry exists, two same-origin calls can both charge and spawn writes. The first completion removes the only marker. Removal can then erase the wallet while the second writer is still running.

Reproduced with two separately blocked calls around actual SQLite:

1. Start two registrations of the same artifact and lease; hold both before insertion.
2. Complete only the first.
3. Freeze the wallet for removal and drain. `erase_wallet_rows` completes within 100 ms although the second writer is still blocked; the first row and memory entry are erased.
4. Release the second writer. It inserts a new `Unsent` SQLite row, restores an in-memory entry and returns `Ok(())` after the erase.

Observed: `erase returned with second writer blocked: true; second: Ok(()); rows after removal: 1`.

This directly contradicts DEC-134(2) and the last commit's claim. The existing removal test covers only one writer; the duplicate-registration test checks only the final budget. The orphan's old lease is dead, so this witness alone does not establish unauthorized transport; its persistence/removal guarantee is still broken.

**Required fix:** single-flight same-artifact registration, or track every outstanding writer with an owner/count and shared abandonment state. Do not erase until all writers have resolved under J. Cover duplicate same-origin and competing-origin writes, abandon, panic and removal together.

Witness: `review_repro_duplicate_register_survives_removal`.

### F5 — Medium: a payment exactly equal to its Spend cap cannot prepare

**Location:** `rust/crates/dw-engine/src/send/mod.rs:898`, `:905`; `rust/crates/dw-engine/src/platform/lease/table.rs:790`.

`prepare_leased` charges the debit before requesting its signer. `spend_signer` then requires `spend.available() > 0`. An allowed debit equal to the cap leaves zero remaining, so the signer rejects the very transaction already charged. Concurrent prepares can produce the same problem when their valid charges exhaust the cap before either asks for a signer.

Repro: prepare and abandon a reference transaction, measure `total_debit = 1,000,226`, issue an AcceptAndPay lease with exactly that Spend cap, and prepare the same draft. Observed: `debit 1000226, exact lease cap 1000226, prepare: Err(Send(VaultLocked))`. The RAII charge is refunded, but the valid payment fails and the error misstates the cause. This conflicts with caps being inclusive and the advertised one-prompt AcceptAndPay flow.

**Required fix:** obtain authorization, charge and the appropriate signer atomically, or authorize signing against the existing charge's lease/generation rather than the remaining unallocated balance. Preserve revocation checks. Test exact caps and concurrent budget exhaustion.

Witness: `review_repro_spend_exactly_at_cap_must_prepare`.

### F6 — Medium: abandoning the last owning lease handle does not end authority

**Location:** `rust/crates/dw-engine/src/platform/lease/table.rs:687`, `:169`; design §4.1 and §4.7.

The binding design says a lease is ended by `end()` **or dropping the last owning Arc**. The implementation explicitly makes `Lease` a clonable lookup handle with no Drop semantics and leaves the table entry authoritative. Saving its ID/request, dropping the sole returned handle, then looking it up still yields `Active` and admits a new First. The independent witness reproduced both observations.

For vault-key leases the idle reaper eventually limits an abandoned entry to ten minutes, instead of ending it immediately. Own-key expiry parks and erases its key, but own-key entries are excluded from idle reaping; their dispatch-live metadata can remain until an explicit end/lock/close. Thus key erasure and lease end are different guarantees here. This is an authority-lifetime deviation, not merely a change of handle representation.

**Required fix:** preserve a distinct owning RAII flow lease and borrowed lookup handles. The string-ID facade needs an explicit session-held owner across calls until `end_flow`/reaping; dropping a temporary lookup must not end it. Alternatively, this lifetime change requires an explicit approved design amendment before closure; none of DEC-57/67/73/134 grants it.

Witness: `review_repro_last_lease_drop_must_end_authority`.

### F7 — Medium: the stress checker is too weak to support the claimed authority gate

**Location:** `rust/crates/dw-engine/src/platform/lease/stress_tests.rs:93`, `:117`, `:161`, `:183`, `:358`.

More seeds exercise the current scenarios; they cannot repair missing scenarios or missing oracle information. Three concrete invalid traces are accepted:

- F1's step-scoped transport before durability is accepted as `Ok(())`: the durability check is restricted to the caller-supplied **registered** set, excluding resumable row-less steps.
- `Begin; Grant(deadline=now+H); Freeze(in_flight=[grant]); LockReturned(now)` is accepted although the permit is neither finished nor expired. The checker checks the return's upper bound, not the design's drain-completeness obligation.
- `Grant(guard); Finish(guard); Transport(guard)` is accepted. Finish neither removes the attempt nor records its outcome.

The latter two were exercised by `review_checker_accepts_early_lock_return` and `review_checker_accepts_transport_after_finished_guard`, each printing `Ok(())`. These passing probes demonstrate blind spots, not correct production behavior.

The log has no definite-resolution/status, refund or budget evidence to catch F2. The driver never creates step-scoped transitions, concurrent duplicate registration/removal, or real SPV handoffs. Its purported unlock arm just calls `wait_gates()`, so it does not exercise actual vault unlocks or new epochs. Its cleanup check sees a verdict flag rather than completion of owner-guarded cleanup/pin release. RNG draws are shared among tasks, so a seed describes randomized inputs rather than an independently replayable schedule. Its I1–I6 labels are also not a one-to-one mapping of design §9.1's I1–I6.

**Required fix:** log and check artifact-wide durability obligations, active attempt lifetime/outcome, actual drain completion, definite resolutions and budget generations/refunds. Generate real gate/epoch changes, scoped/unscoped copies, registration/removal races and transport stalls. Keep the existing mutations, but add mutations and bad-trace self-tests for the missing obligations. The passing model is a design oracle, not a refinement proof that this implementation matches it.

### F8 — Low: zero cost bypasses the absence of a Credits purpose at the admission boundary

**Location:** `rust/crates/dw-engine/src/platform/lease/fence.rs:1244`, `:1255`.

Transition authority checks lease liveness, then whether `credits.charge(credits)` succeeds; they do not independently check that Credits was granted. A Crypto-only lease with zero caps admits `Transition { credits: 0 }` as First because charging zero to a zero ceiling succeeds. The independent table witness reproduces this. The background lease itself uses a separate representation; this witness is a normal lease containing only Crypto.

§3.1 says a zero cap grants nothing of that kind, and §4.2 says Crypto never hands off. Current honest transition quotes include a positive fee bound and the corresponding identity signer is absent, so I have **not** demonstrated a real zero-fee spend or an externally exploitable grant bypass. This is a missing independent purpose check at the fence's public admission boundary.

**Suggested fix:** require a granted Credits purpose independently of the numerical charge, and apply the same principle to registration/Funding. Keep zero cost distinct from absent authority.

Witness: `review_repro_zero_credit_purpose_admits`.

## Journal, lock/epoch and DEC-134 conclusions

The SQLite module uses one mutex-protected connection, WAL, `synchronous=FULL`, secure deletion and a transactional schema gate. The monotone registered-artifact path and ambiguous-write retry are sound in the cases exercised. A newer schema/unavailable journal fails closed for leased Firsts. The journal is separate from exported app rows.

Independent SIGKILL subprocess tests killed the writer after registration, after the Dispatching commit and after the step-marker commit, then reopened SQLite. They recovered respectively `Unsent`, `Dispatching`, and one step marker. Combined with the branch's next-process/fault tests, a normal crash **after** a successful marker and **before** transport retains conservative recovery evidence. An identical artifact's Resend is intentional; a different signature for the step under dead authority must report possibly sent. F1 breaks this evidence guarantee by permitting transport **before** the marker. F2 incorrectly applies conservative recovery when this process already has definite-unsent evidence.

These tmpfs SIGKILL checks prove process-crash behavior at committed boundaries. They do not simulate physical power loss, fsync hardware failure or lost `wallet.sqlite` WAL tails; the latter recovery/library wiring belongs to the later Mode A work. Apple fullfsync behavior and Apple/Swift execution were not tested on this Linux host.

I found no additional demonstrated barrier or joined-gate escape in the reviewed coordinator. Freeze/revocation and permit snapshot happen under J; every lock request runs its own gate, and begin/rebind/background installation use generation/barrier checks. I additionally exercised a real encrypted vault: K1 starts and completes its gate while a permit holds the drain; abort K1's caller; unlock and issue a grant; run synchronous K2 during K1's drain. K2 locks again and invalidates the pre-K2 grant; finishing the held permit lets the remaining operation complete. This test passes. It validates the coordinator independently of the stress driver's fake unlock. F3 still leaves the actual engine-owned handoff outside its intended deadline discipline.

DEC-134 assessment:

1. **Old-passphrase verification before freeze: satisfied for verification failures.** Check executes first on the blocking pool under the vault writer lock. A checked change binds to the vault ID and wrapped slot, so a concurrent successful change invalidates the stale check instead of letting it overwrite the winner. The wrong-passphrase session test and stale-slot vault test pass. A correctly checked change can later fail after another change replaces the slot, after its own freeze; that fail-closed concurrency case should not be described as every possible error revoking nothing.
2. **Erase waits for all in-flight registers: not satisfied.** F4 is a concrete counterexample despite holding the per-wallet barrier through removal.
3. **Failed secret wipe leaves the wallet listed and usable: satisfied in the examined ordering and regression test.** The wipe now precedes manager removal and wallet/app/journal deletion, propagates errors and passes `remove_wallet_fails_closed_when_the_seed_cannot_be_deleted`. Revocation and consuming the Wipe grant are documented. Errors after a successful wipe are a separate partial-removal case, explicitly documented in the updated contract; they do not establish failure of the failed-wipe guarantee.

## Disposition of deviations and phase boundaries

| Item | Judgment against the binding design |
| --- | --- |
| Table-owned handles; explicit end plus idle reaping | **Not accepted as equivalent:** F6 changes authority lifetime. Preserve ownership semantics or obtain an explicit design amendment. |
| One grant per kind | Conservative restriction in `session.rs:58`; rejects some more general grant sets, without widening authority. Record the narrowed API rather than claiming arbitrary grant composition. The intended PlatformOp+Spend AcceptAndPay set remains representable. |
| Unscoped First deferred; known copy allowed Resend; Notice | Verdict semantics conform to §5.4/Q12. They do not justify F1's nondurable copy. The required debug assertion is replaced by tracing plus Notice; this is a diagnostic omission to document/fix, not an independent spending exploit. |
| Step First deferred on failed/late write; charge retained | Conservative and consistent with an ambiguous/durable marker eventually allowing recovery. It does not authorize a copy before durability, or override a refunded NotSent tombstone (F1/F2). |
| Revoking session methods and generic epoch observation | Appropriate H9/§8.6 implementation. Passphrase precheck/apply split is required by DEC-134; stale checked-slot refusal is appropriate. |
| Eager plus on-demand background Crypto lease | Appropriate practical implementation of §4.5/H8, subject to the same barrier/generation checks; it does not grant a submission purpose. State/epoch tests pass. |
| Holding a removal/Close Wallet barrier through the whole operation | Correct and necessary to avoid reinsertion before the wallet is gone. The writer bookkeeping inside that barrier is still defective (F4). |
| `WalletSecretNotDeleted` retained in frozen FFI but no longer emitted on wipe failure | Consistent with DEC-134's error propagation and the updated m1 contract. |
| New events/notices kept engine-side; Cancelled mapped to existing PreparedTxSpent in dw-ffi | Conforms to §13/§16 and the Swift-shell freeze. No requirement to expose the new enum variants in this phase. |
| Plain M1 grant sends remain unleased | Explicit P5 boundary. Do not reject P2a for missing P5; the leased AcceptAndPay/TxDraft path already added here must itself be correct (F3/F5). |
| Mode B call permits/signature counting/status derivation; platform hook/SPV split; seeding/restoration/catch-up | P2b/P4 work is legitimately deferred. P2a supplies the engine primitives, not completed registration/top-up integration. This deferral does not make the current unchecked `fenced()` First conformant. |
| UI retry/rebind prompts, Lock copy, 90-second accept through actual facade flow, Repair/testnet registration | Later consumers/stack work; table-level stand-ins are useful but cannot establish all §12 [A+B] acceptance rows literally complete. Report the P2a subset and remaining end-to-end acceptance explicitly. |

## Independent validation and reproducibility

All Cargo commands ran from the desktop `rust/` workspace with `umask 022`, `TMPDIR=/dev/shm/e0-04-p2a-r1` (mode 0700), `RUSTC_WRAPPER=` and `CARGO_BUILD_JOBS=12`. A dedicated target directory, `rust/target-review-sol-r1`, isolated the review build. After testing it was moved to `/work/scratch/dw-e0-04-p2a-r1-sol-target` to leave the reviewed tree clean.

| Check | Result |
| --- | --- |
| `cargo test --workspace --locked` on unchanged HEAD | **658 passed, 0 failed, 3 ignored**, exit 0. |
| `python3 -I docs/design/checks/e0_04_design_model.py` | All checks pass, final `ok`, exit 0. |
| `cargo fmt --all --check` | Pass. |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | Pass. |
| `git diff --check main...HEAD` | Pass. |
| Existing stress seeds 1–3 plus independent seeds 4–32 | Pass, 300 driver cycles per seed, 16 flows and four resume tasks per run. |
| CheckThenAct / RevokeInDrain / TransportFirst mutations | Caught by harness I1 / I1 / I4 respectively. |
| Seven new `review_repro_*` witnesses | **All seven fail their required-behavior assertion**, reproducing F1–F6/F8; zero compilation failures. |
| Two new checker-blind-spot probes | Pass by confirming acceptance of invalid traces (F7). |
| Real joined lock/unlock/K2 test | Pass. |
| Actual SQLite SIGKILL harness at three commit boundaries | Pass. |

Expanded stress command: `cargo test -p dw-engine --lib platform::lease::stress_tests --locked -- --nocapture` (six tests, exit 0). Witness command: `cargo test -p dw-engine --lib review_repro --locked -- --nocapture --test-threads=1` (seven tests, expected exit 101). Positive follow-ups: `cargo test -p dw-engine --lib review_real --locked -- --nocapture` and `cargo test -p dw-appdb review_crash --locked -- --nocapture`.

Witness source and logs remain on this host in `/dev/shm/e0-04-p2a-r1`: `repro-table.rs`, `repro-session.rs`, `extra-stress.rs`, `repro-appdb.rs`; `gate.log`, `model.log`, `fmt.log`, `clippy.log`, `repros.log`, `stress-extra.log`, `joined-real.log`, `crash-real.log`. The external sources were temporarily included in the existing corresponding test modules (`repro-appdb.rs` inside the dispatch tests module); no production logic or dependency was changed. These host-local files are transient; the interleavings and observed values above are the durable review evidence.

**Closure required:** fix F1–F5, resolve F6's ownership contract, strengthen the gate for F7, and cover the leased real-transport outcomes/deadline path before another Sol review. F8 is a bounded hardening finding. Passing the current numerical gate is insufficient to approve spending authority. **Sol gate satisfied: no.**
