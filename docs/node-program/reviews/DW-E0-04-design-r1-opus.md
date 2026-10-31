**Verdict: APPROVE-WITH-CHANGES**

# DW-E0-04 design review r1 (Opus): product, codebase fit, implementability

- **Reviewed:** `dw/e0-04-design` at 6b4cdf9: `docs/design/E0-04-grants-leases.md` and `docs/design/checks/e0_04_design_model.py`.
- **Read against:**
  - DASHPAY.md §2.3, §2.6, §3.2–§3.7 and §4–§5;
  - UX-SPEC.md §4.1, §4.4, §4.7 and §4.13;
  - ROADMAP.md: the E0-04, E0-05, E0-13 and DP1-02 rows, and the DP2/DP3 rows;
  - m1-engine §2.2;
  - the m4 contract on `dw/e0-08-dashpay-facade` at **cf9ba8e** (the r1 fix commit);
  - merged E0-03 code: `dw-vault`, `dw-engine/src/{keys,wallets,session,send,history_ops}.rs`;
  - DEC-57 and DEC-65;
  - the pinned platform source (`bc41f1bc23`, extracted to `/work/scratch/dw-e004-review-opus/pin/`) and rust-dashcore `40268cc0`.
- **Focus:** UX, fit with E0-08 and platform-wallet, and implementability. Concurrency and security belong to the parallel GPT review.

## Summary of the check

- **The concurrency core holds.** I found no problem with the journal, the single compare-and-set under J, revocation at the freeze, host-enforced deadlines, `lock_gen`, the spawned cleanup or the seeding. Most facts F1–F15 check out against the pin (the list is below).
- **The model.** `python3 -I docs/design/checks/e0_04_design_model.py` exits 0 in 23 s, with 48 PASS and 0 FAIL. Its 7 scenarios, 23 mutations, 5 rejected timing rules and 3 lock-order checks match §10. The output is in `/work/scratch/dw-e004-review-opus/model-run.txt`.
- **Where the problems are.** The majors sit where the design meets the product:
  - what the user is told after a lock, and what they can then do;
  - the facade contract;
  - one cleanup obligation that the model's single "reserved" bit cannot see;
  - the case where pasta declines Q1.

**Pin facts verified.**

| Fact | Status | Detail |
|---|---|---|
| F1 | holds | The broadcast sites are all at the cited lines. |
| F2 | holds | `psbt.rs:464`, `runtime.rs:323-338`. |
| F3 | holds | `transactions.rs:32-50`. |
| F4 | holds | `build.rs:1098/1110/1142`; `manager.rs:220-236` logs the error and carries on. |
| F5 | holds | `session.rs:288`. |
| F6 | holds | No dw code calls `resume_asset_lock`. `create_funded_asset_lock_proof` does return `FinalityTimeout` after 300 s (`proof.rs:1008ff` → `build.rs:872`); only the resolver falls back to a ChainLock. |
| F9 | holds | `broadcast.rs:125-170, 280-356`. IdentityCreate also reaches `StateTransition::broadcast` (`put_identity.rs:309, 343`). |
| F14 | holds | `load.rs:605`, `signed_payment_registry.rs:410-423`. |
| F7 | partly checked | — |
| F12 | not checked | — |

**Q1 matrix.**

| Capability | Without Q1 | With Q1 |
|---|---|---|
| P1 (caps, `IdentityScan`, `authorize_set`, `KeyHold`, `epoch()`) | yes | yes |
| Leases, async lock with a bounded drain, `LockReport`, background lease | yes | yes |
| Contact payments and M1 sends (`TxDraft`, which the engine calls itself): Cancelled before the call, MaybeSent at H | yes (coarse permit, §11) | yes (exact) |
| Library STs (contact request, accept, profile, DPNS, top-up, registration step 2): Lock cancels whatever is not yet signed | yes (vault epoch) | yes |
| The same STs: Cancelled after signing but before hand-off | **no**: reported MaybeSent | yes |
| Asset locks: journal-decided resume and cleanup of never-sent rows | **no**: pin behaviour, `Built` rows are resent | yes |
| Load catch-up (H6 part 2) | yes: dw can call the public `resume_asset_lock` | yes, decided by the journal |
| Re-reserving a possibly-sent row's inputs (L12) | **no**: pin gap remains | yes |
| Exact key window (`proof_wait_started`) | no: the key is held for the whole `create_funded_asset_lock_proof` call, bounded by the 300 s window plus acceptance | yes |
| SPV split (`stop()` not blocked by a pending acceptance wait) | no | yes |

## Findings

### 1. MAJOR: a declined Q1 has no defined end state, so E0-04 and DP1-02 cannot close

- **Where:**
  - §0 item 11 and §11, "Interim" (lines 1119-1125);
  - Q1 (line 1287);
  - §13, P3 and P4;
  - ROADMAP E0-04 "Depends", which needs the dispatch fence in the pin.
- **Problem.**
  - The design says library flows keep the interim wording, and that "E0-04 is not done for them until P4". P4 needs P3, and P3 needs B5.
  - If pasta declines Q1, or decides late, E0-04 has no acceptance it can ever meet.
  - DP1-02 builds against P2 and the fake library, but its own acceptance cannot close either: the kill matrix, "the funding cap holds" and a testnet registration all need real wiring.
  - DEC-65 says the design proceeds "either way", yet the doc only specifies the "yes" branch.
- **A degraded design exists, and it meets DEC-57.** DEC-57 asks for a reviewed design of the dispatch rule; B5's own consequence column already describes the weaker promise.
  1. **Coarse permits around every library write call.** The engine wraps each call in a permit taken under J, with the deadline grant + H. Calls in scope: `create_funded_asset_lock_proof`, `register_identity_with_funding` with `FromExistingAssetLock`, the document and contact calls, top-up and withdraw.
  2. **Signatures are already safe.** A lock ends the vault epoch (E0-03), so nothing inside a call can be signed after `lock()` returns.
  3. **Outcomes per call:**
     - Sent when the call returned Ok before the drain ended;
     - Cancelled when it failed with the vault's `Locked` from a signer and the call hands off a single artifact. That covers most DashPay writes once registration is split in two. DPNS registration hands off two (preorder, then domain), so a failure on the second is MaybeSent;
     - MaybeSent otherwise.
  4. **No journal.** The library re-dispatches with pin semantics.
  5. **Keep the Q1-independent parts.** The H6 catch-up uses the public `resume_asset_lock`. TxDraft coin control should exclude the outpoints of tracked `Built`/`Broadcast` asset-lock rows, which mitigates the missing L12 for engine-built sends.
- **Fix.**
  - Add a "Q1 declined" section with this design, its theorem in weakened form, its UI copy ("Locking stops new signatures; a send already signed may still go out"), and an acceptance list that E0-04 and DP1-02 can close against.
  - Sequence P2 so that the journal and the `EngineFence` registered-artifact path come after the B5 answer. Without Q1 nothing ever calls `register`.

### 2. MAJOR: outcomes the journal can still resend are reported as plain MaybeSent, and the UI can be led to pay twice

- **Where:**
  - §8.4, the four outcomes ("Committed, not yet sent … reported as MaybeSent, and a later Resend may send it");
  - §6.4;
  - Q13 ("will be sent when the network is back");
  - m4 §4 at cf9ba8e:1066 ("offers a retry only once a re-query shows nothing was sent");
  - m4 §2.2, `discard_registration` ("before `FundingSent` only");
  - `RegistrationStatus.funds_committed` ("true from `FundingSent` on").
- **Failure scenario 1: top-up.**
  1. A top-up's asset lock reaches `Ambiguous`/`Deferred`, or a peer is lost after `Dispatching` (L13).
  2. `top_up` returns `platform.broadcast_unknown`.
  3. The user re-queries: nothing is on chain or on Platform, so m4 lets the UI offer a retry.
  4. The retry builds a second lock.
  5. The first entry is at 1, or is retried to 1 by a later resume, the catch-up or `finish_asset_locks`, and is resent. Two locks fund two top-ups.
- **Failure scenario 2: registration.**
  1. A registration whose lock ends MaybeSent before the row reached `FundingSent` stays discardable.
  2. "Discard" and "Register again" build a second lock while the first may be on the wire.
- **The reverse case.** A MaybeSent whose entry was Ambiguous with nothing on disk is refused and cleaned up at the next load. Nothing tells the row, so a `FundingSent` row can wait forever on a lock that no longer exists.
- **Fix.**
  - Split the outcome:
    - `platform.broadcast_unknown` for "may already be out";
    - a new `platform.broadcast_pending` (or a field on the outcome) for "committed, the engine will resend it; never retry".
  - Map Q13's copy to the second.
  - Any registered artifact that is not definitely not-sent sets `funds_committed` and moves the row to `FundingSent`.
  - `discard_registration` and every retry offer consult the journal: they are allowed only when the lock's entry is `Revoked` or gone and the row has been cleaned up.
  - Add an engine event, `DispatchResolved{wallet, txid, Sent|NotSent}`, emitted when a provisional outcome resolves (a later Resend is accepted, or a reload refuses and cleans up). DP1-02 then moves the row back to retryable, and the payment and top-up UIs clear their "may have been sent" state.
  - Add a test for each scenario above.

### 3. MAJOR: "Lock to cancel" promises a cancel that cannot happen after funding, and there is no Lock control while the vault is locked

- **Where:**
  - §4.6 line 386 ("own-key lease in `Active` or `AwaitingProof`: 'Registration in progress — Lock to cancel'");
  - DASHPAY §2.6 line 454;
  - UX-SPEC §4.1, status bar and toolbar: `lock.fill` green means locked, red unlocked, orange mixing-only;
  - m4 `holds_key` doc.
- **Problem (a): the copy is wrong after funding.**
  - `AwaitingProof` starts after the funding hand-off, so the asset lock is already on the wire.
  - Pressing Lock there drops the key and parks the flow ("Unlock to finish"). It does not cancel the registration or return the funds; they stay in the asset lock until the identity is created or `finish_asset_locks` runs.
  - The copy promises a refund the product cannot give. Mobile wallets offer no cancel at all once a registration has paid; iOS shows a progress banner.
- **Problem (b): there is no affordance.**
  - An own-key lease exists exactly when the vault is `Locked` (or mixing-only). The status bar then shows green "Locked", and dash-qt's "Lock wallet" action is disabled while locked.
  - So the user has no visible Lock control, and the indicator claims no key is in memory while a lease holds one.
- **Fix.**
  - Copy keyed on `funds_committed`:
    - before it, "Lock to cancel";
    - after it, "Funds locked — finishing. Lock to stop; you'll finish after you unlock".
  - UX-SPEC gets a fourth lock state, "Locked, key held by a flow" (for example orange `lock.fill` with a badge and a tooltip naming the flow and the seconds left from `LeaseView.key_expires_in_secs`).
  - The toolbar and menu Lock action stays enabled while any own-key lease exists.
  - m1 states that `Vault.lock()` on a `Locked` vault still revokes leases.
  - Add these rows to §15 for UX-SPEC.

### 4. MAJOR: E0-08's m4 contract cannot carry this design as written

- **Where:** m4 §1 "Grants", §2 and §4, `RegistrationQuote`/`TopUpQuote` (`registration.rs:63-72`, `credits.rs:25-30` at cf9ba8e), and `RegistrationWait` (`registration.rs:119-128`); design §3.3, §4.1, §4.2 and §15.
- **Exact changes, for the E0-08 fix round or a Contract-Version 2 owned by E0-04:**
  1. **"Accept and pay" spans two calls, but no API carries one lease across them.**
     - `begin_lease` takes grant ids and returns an engine `Arc<Lease>`. The facade's `accept_request(identity, from, grant)` and DP3-01's `TxDraft.prepare(grant)` each receive one grant id.
     - With two separate grants, the Spend grant's 120 s TTL can run out during the accept's Platform round trip: one SDK wait is 30 s × 3 (F9). That breaks DASHPAY §2.3's one-prompt rule.
     - Add `NetworkSession.begin_flow(wallet, flow, grants: Vec<String>) -> String`, which returns a lease id. That id is accepted wherever a `grant: String` is (m4 §1 already allows this), and `end_flow(id)` releases it.
     - Add an idle reaper: a vault-key lease ends after N minutes with no call, and an own-key lease's key already expires at `key_until`. Without it, a host that abandons the sheet leaks lease entries.
  2. **Quotes must tell the host what to authorize.**
     - Registration needs `max_credits > 0`, or there is no `PlatformIdentity` signer for IdentityCreate (§3.4). Yet `RegistrationQuote` reports only duffs.
     - The engine charges `explicit + fee_bound(st)` from constants the host cannot see (§4.2), so a host that sizes `max_credits` from `cost_table()` gets `platform.grant_exceeded` on every write.
     - Add a `grant: GrantRequest{max_duffs, max_credits}` to `RegistrationQuote`, `TopUpQuote` and `WithdrawQuote`, and a quote, or `grant_request(action)`, for `send_request`, `accept_request`, `register_name`, `update_profile`, `set_private_details` and `enable_dashpay_keys`.
     - State that the engine charges exactly what it quoted.
  3. **`RegistrationWait` needs values for E0-04's states.**
     - `NeedsGrant` (after an unlock, a scope change or a passphrase change) and a mid-flow budget shortfall happen while the vault may be unlocked. "Unlock to finish" is then wrong; add `Authorize` ("Confirm to finish").
     - `Parked{ProofWaiting}` is `ChainLock`, and the resume needs a grant once the proof arrives. Either add `needs_grant: bool`, or state that `resume_registration` accepts a grant early only when the vault is `Unlocked`, so the lease is vault-key and no key is held during the ChainLock wait.
  4. **Error mapping.**
     - `lease.locked` (§4.1) → `platform.cancelled`.
     - `LeaseError::Parked` / `NeedsGrant` → the new `platform.needs_grant{purpose}`.
     - A budget refusal → `platform.grant_exceeded{purpose, needed, remaining}` (today the code has no parameters).
     - An unavailable journal (§6.4) → `platform.storage`, with the notice.
  5. **`holds_key` and `funds_committed`.** Define `holds_key` = `LeaseView.own_key && state ∈ {Active, AwaitingProof}`, and `funds_committed` per finding 2.
  6. **Notices.** `DispatchRecordMissing`, `UnscopedDispatch` and `DispatchJournalUnavailable` join m4 §6's `NoticeCode` list with an owner.
  7. **§15.** It lists DASHPAY, ROADMAP, m1 and B5, but not m4, DP1-02's row (finding 10) or UX-SPEC. Add them.

### 5. MAJOR: the Refused and abandon cleanup leaves the build's in-broadcast pin "pending", so the inputs stay unspendable for the session

- **Where:** §5.5 "Cleanup" (lines 569-579), L6, L14, §12 "A dropped build". The pin's behaviour: `build.rs:376`, where the pin is taken at build time; `generation.rs:634-656`, where drop settles `Pending`; `generation.rs:537-576`, where a pending fence clears only on an observed spend.
- **Problem.**
  - The build's `InBroadcastPin` is held from the build through the broadcast. Its `Drop` settles as `PendingSpend` unless `settle_released[_on_drop]` was called, and a pending fence is released only by an observed spend, with no timer.
  - The design's cleanup untracks the row, releases the reservation with the build's token and queues the removal. It never settles the pin, and the drop-guard path (L14) drops the pin with the future.
  - The DASHPAY draft did list "releases its inputs and in-broadcast fence"; the design lost it.
- **Failure scenario.**
  1. Lock lands before `admit`.
  2. The flow is Cancelled, and the cleanup runs as specified.
  3. The pin settles Pending, so the inputs can never be selected again in this session.
  4. "Register again" fails with insufficient funds until a restart.
- **Why the tests miss it.** The model folds the reservation and the pin into one `reserved` bit, so it cannot see this, and §12's "inputs released once" checks only the reservation.
- **Fix.**
  - L6 and L14 must settle the pin released: move it into the drop guard and the cleanup task, with `settle_released_on_drop()` set when `abandon` or `admit` returns `cleanup: true`.
  - For `Deferred`, `Committed` and L13, keep it pending, as the pin does.
  - Barrier 1, "a dropped build" and dispatch-record case 1 must assert that a new build can select the inputs, not just that the reservation was released.

### 6. MINOR: the PR's shape: a decorating `FencedBroadcaster` cannot hold the permit for step 2 only, and rs-sdk cannot see a platform-wallet scope

- **Where:** §5.1 (lines 415-425), §5.2 (line 441), §5.3, §11.
- **Problems.**
  - The production broadcaster is a concrete per-wallet `Arc<SpvBroadcaster>`, monomorphized into `CoreWallet`, `IdentityWallet` and `AssetLockManager` (`platform_wallet.rs:312-325, 663-700`).
  - A wrapper around `broadcast(&tx)` would hold the permit across the whole 65 s acceptance wait. The permit-for-step-2 split has to live inside `SpvBroadcaster`: a fence-aware broadcaster, not a decorator.
  - `tracked_row` and `site` are not visible through `broadcast(&tx)`, so every F1 site must pass them.
  - `DispatchScope` is said to be platform-wallet's, but the rs-sdk hook must read it, and rs-sdk cannot depend on platform-wallet. A state transition also has no wallet id at `StateTransition::broadcast`, so the scope must carry the wallet.
  - `DapiBroadcaster` is not "one request": `sdk.execute(…, RequestSettings::default())` retries across nodes (`broadcaster.rs:131-145`).
- **Fix.**
  - Define the trait change: a fenced `SpvBroadcaster` with `admit` called between subscribe and enqueue, and a `DispatchContext{wallet, tracked_row, site}` argument or task-local.
  - Put `DispatchScope{wallet, origin}` in rs-sdk's module.
  - Clamp the DAPI retries the way rs-sdk's are clamped.
  - Re-size P3 as L; it touches every F1 site, `SpvRuntime`, rs-sdk's retry loop and the asset-lock build and recovery arms.

### 7. MINOR: the phase plan understates P2 and collides with the Swift freeze

- **Where:** §13, P2 (S/M) and P4's "FFI and integration"; §12 ("`Vault.lock()` is async in dw-ffi and both UI shells"); DASHPAY R8 and §2.1 ("MacUI and CrossUI are frozen"); ROADMAP E0-13 (keeping "the frozen Swift bindings unchanged").
- **Problems.**
  - P2 holds the lease table, the background lease, events, async lock and close, seven revoking session methods, the `vault_op` epoch check, the FFI, the journal with seeding, `EngineFence`, the fake-library harness and the stress checker. That is L.
  - Making dw-ffi's `Vault.lock()` async changes the frozen Swift shells.
- **Fix.**
  - Keep the FFI `lock()` synchronous. It already does all the work that matters synchronously: the freeze and the vault gate run before the first await (§8.1).
  - Report the drain through `LockProgress` and `LockReport` events.
  - The chosen stack binds an async variant in E0-13.
  - Re-size P2 as L and the whole of E0-04 to L–XL in ROADMAP, and say that DP1-02's closure waits for P4, or for the Q1-declined acceptance of finding 1.

### 8. MINOR: two stale texts contradict §4.4 and §15 does not fix them

- **Where:**
  - DASHPAY §2.6 line 450: "Parking needs no aborted future. The engine builds the lock with the library's `build_asset_lock_transaction` … lets platform-wallet track it". This sits above the "Commit points" range that §15 replaces.
  - ROADMAP DP1-02: "the persisted state machine (`build_asset_lock_transaction`, then tracked lock, then `FromExistingAssetLock`)".
- **Problem.** §4.4 says correctly that this path never leaves the build-only function: it returns the lock unsent, untracked and with no token. A DP1-02 implementer following the ROADMAP row would build the flow E0-04 forbids.
- **Fix.** Add both to §15. They become the two-call split: `create_funded_asset_lock_proof`, then `FromExistingAssetLock`.

### 9. MINOR: registration's step 2 still has hidden ChainLock fallbacks

- **Where:** §4.4 and Q19. In the pin, `registration.rs:162-200`: `FromExistingAssetLock` → `resume_asset_lock(Some(300 s))` → `IsTimeout` → `upgrade_to_chain_lock_proof(None)`. At `:230-245`, an IS-proof rejection at submit falls back to an unbounded ChainLock wait.
- **What happens.**
  - The key is still bounded, because the `KeyHold` timer drops it at `key_until`.
  - The library then retries the submit with dead signers and returns a signer `Locked` error after the ChainLock.
  - If DP1-02 maps that to `Failed`, the user sees a failed registration with funds committed.
- **Fix.**
  - Say this in §4.4.
  - Require that step 2 run only once the row carries a proof.
  - Map a signer `Locked` out of step 2 under an expired or revoked lease to `Parked` ("Unlock to finish").
  - Add a fake-library test: an IS rejection that waits past `key_until`.

### 10. MINOR: dispatch.sqlite's lifecycle has gaps

- **Where:** §6.2 (lines 680-684) and §6.5 (lines 747-770).
- **Wallet removal.**
  - "A wallet removed and imported again finds its old entries, and the library's rows for it come back only from wallet.sqlite" is inaccurate.
  - dw's `remove_wallet` calls `persister.delete_wallet` (`wallets.rs:223-232`), which cascade-deletes and secure-erases the wallet's rows (`PWS persister.rs:740-790`). The entries are useful only if PWS's auto-backup is restored.
  - Meanwhile the journal keeps the wallet id and every asset-lock txid forever (no GC, Q10) after a secure wipe.
  - Say that, and decide whether `remove_wallet` with a `Wipe` grant deletes the wallet's entries once it has no tracked row left.
- **Seeding is coupled to PWS's schema.** It depends on the `asset_locks` table, the `status` TEXT labels (`asset_locks.rs:199-233`, V004) and the outpoint encoding. Check the PWS schema version first; if it is unknown, treat the journal as unavailable (refuse leased Firsts) rather than seed wrong.
- **Downgrade.** An older build that opens a newer journal schema must refuse the fence, not fail the CHECK on an unknown `state`.
- **Network switch and close.** The journal is per network and closed with the session. That is implied but never stated; say it next to `close` (§8.5).
- **Wording.** "The read-only connection dw already uses for history": `history_ops.rs:86` opens an ad-hoc read-only connection per call. Do the same.

### 11. MINOR: Ambiguous, L13 and Deferred rows are not driven again until the next launch

- **Where:** §6.4, Q13, H6.
- **Problem.**
  - Within one process, an `Ambiguous` entry is retried only "after a later write succeeds", which needs another `admit`. An L13 row is resent "later".
  - Only DP1-02's own flow or the next load's catch-up ever calls `admit` again. A top-up or a `finish_asset_locks` lock sits as "may have been sent" with its inputs held for the session, so "will be sent when the network is back" is untrue within the session.
- **Fix.** Run H6 part 2 (the unscoped resume pass) again on each SPV peers 0 → >0 transition and after a journal write recovers, and test it.

### 12. MINOR: L2 is broader than its reason, and would make the PR weaken a pin invariant

- **Where:** L2 (line 611) and F14.
- **Problem.**
  - `payment_guard` is a per-generation lifecycle read gate. The library holds it across the network send on purpose, so that wallet teardown is linearized against the payment (`signed_payment_registry.rs:410-423`, `load.rs:605`).
  - Those sites are row-less, and a row-less `admit` does no I/O and takes only J, a leaf.
  - Forbidding `payment_guard` across `admit` buys nothing and pushes the PR to drop that linearization.
- **Fix.** Narrow L2: no wallet-manager guard and no `build_persist_serial` across `register` or a registered-artifact `admit`, the calls that do journal I/O. `payment_guard` is explicitly allowed.

### 13. MINOR: contact payments are leased from DP3-01, but their Cancelled outcome and release are scheduled only for P5

- **Where:** §4.2 ("`Spend` … for `TxDraft` (DP3-01)"), §5.2, P5 (`send.cancelled_by_lock`), and dw `send/mod.rs:1048-1110`.
- **Problem.**
  - A fence refusal inside the library's broadcaster surfaces today as `TransactionBroadcast` → `SendFailure::BroadcastRejected` or `NoPeers`.
  - So a contact payment that Lock cancelled would read "rejected by the network". Its DIP-15 address reuse and its reservation release are also left unspecified.
- **Fix.**
  - Move the code (as `send.cancelled`) and the m1 and m1-swift rows into DP3-01's scope.
  - Map L6's `DispatchRefused` to it.
  - On a refusal, release the draft's reservation through the finalized handle's owner-guarded path, and keep the reserved address on the draft (DASHPAY §2.3 step 5).
  - For "Accept and pay": an accept that was Sent, then a payment Cancelled by Lock, shows "Accepted — payment cancelled. Pay now?".

### 14. MINOR: UX diverges from mobile conventions in four places

- **Unlock mid-flow costs a second prompt** (Q5 and `rebind`).
  - With "require authentication for every payment" on, which is the default, a user who unlocks during their own registration moves the lease to `NeedsGrant` and is asked again for a flow they have already approved.
  - Rebinding a `NeedsGrant` lease on `Unlocked(Full)` from the vault key, within its remaining budgets, grants no new authority. Do that, or record why not.
  - Also say whether `rebind` keeps the old caps or takes the new grant's.
- **Auto lock.** UX-SPEC §4.13 has "Auto lock 5 minutes". The design never mentions it.
  - Say that it is `lock_vault`, whether a visible progress screen counts as activity, and what the copy is ("Locked automatically — unlock to finish").
- **The drain as a modal.** "Locking — finishing a send already handed off" reads like a blocking sheet, while mobile locks at once.
  - The vault is locked in step 2, before the drain, so show the lock screen immediately and the drain as a non-blocking status.
- **Quick unlock is refused for `PlatformOp`** (§3.1, and the all-or-nothing rule of `authorize_set`).
  - On a Mac with Touch ID, every DashPay write, and "Accept and pay", needs the typed passphrase. iOS uses biometrics for these.
  - Either allow `QuickUnlock` for `PlatformOp` under a credit/duff limit like the spend limit, or record the divergence in DASHPAY §2.6.

### 15. MINOR: rows with no journal entry leave the user stuck, with no way out

- **Where:** §5.4 `none, tracked_row` and Q16.
- **Problem.** The row is never sent and never cleaned up. It shows as an unfinished asset lock ("Funds locked — finishing", `finish_asset_locks`) forever, and every resume is `Deferred`.
- **Fix.** Under Tools ▸ Repair, offer "Send it" (the user commits it as a First under a fresh lease) or "Discard" (the cleanup, after checking the inputs are unspent and the transaction is not on chain).

### 16. MINOR: a refusal in rs-sdk skips the nonce refresh

- **Where:** §5.1 and §11 commit (1).
- **Problem.** The identity nonce is fetched and bumped before signing. `StateTransition::broadcast` refreshes it only on the failure arm after `broadcast_with_retries` (`broadcast.rs:161-168`). A refusal returned before that arm leaves a gap in the cached nonce.
- **Fix.** The refusal path calls `sdk.refresh_identity_nonce(owner)`, or the hook sits inside the retry closure so the existing arm runs. Add it to the rs-sdk L-tests.

## Nits

1. "Five" against "four" rejected rules: §0's table and §10.4 say five; §A.1 m-2 (line 1357) says four. The model runs five.
2. The model's label "commit in progress elsewhere: Deferred after H" (`e0_04_design_model.py:269`) contradicts §5.4's "no waiting". The semantics match; fix the label.
3. §4.3's `Revoked{…}` causes omit `WalletClosed`, which §8.6 uses.
4. In §5.7, H10 comes before H9.
5. §4.7 `funding_signer() -> PlatformFunding{remaining}` cannot follow "remaining" once the tokens are dropped (§4.1). Say that the signer's cap is fixed when it is issued, and that the remaining budget is checked at `register`.
6. §4.6 copy: "sent before the lock" and "may have been sent" need a third line for finding 2's "will be sent".

## Summary

- **The verdict is APPROVE-WITH-CHANGES.** The core holds:
  - the host-owned journal, the one-step compare-and-set, revocation at the call, host-enforced deadlines and `lock_gen`;
  - the model, which passes and matches the text;
  - the pin facts, which mostly hold up.
- **What is missing is at the edges, where users and other tasks meet it:**
  - a defined end state if pasta declines Q1 (1);
  - outcome semantics that never invite a second payment (2);
  - honest "Lock to cancel" copy and a Lock control while the vault is locked (3);
  - the m4 contract surface for leases, quotes and waits (4);
  - the in-broadcast pin in the cleanup (5).
- **None of these needs re-architecting.** Each is a doc change plus tests. Findings 2 and 5 also add obligations to DP1-02 and to the PR.
- **Scratch:** `/work/scratch/dw-e004-review-opus/`.
