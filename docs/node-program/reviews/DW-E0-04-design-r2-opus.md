**Verdict: APPROVE-WITH-CHANGES**

# DW-E0-04 design review r2 (Opus): focused closure check

- **Reviewed:** `dw/e0-04-design` at **d2a21c6**: `docs/design/E0-04-grants-leases.md` (rev1), and the rev1 edits to DASHPAY §2.6, ROADMAP DP1-02 and UX-SPEC §4.1/§4.13.
- **Against:**
  - my r1 (`DW-E0-04-design-r1-opus.md`) and GPT's r1;
  - DEC-65 and DEC-67 (Q21 accepted; Q22 rejected; Mode B accepted with its weaker promise in user-facing terms);
  - the E0-08 contract on `dw/e0-08-dashpay-facade` at **c3c9bbd** (`docs/contracts/m4-dashpay-engine.md`, Contract-Version 2, and `rust/crates/dw-engine/src/platform/{flows,errors,registration}.rs`);
  - pin facts I re-checked: `list_tracked_locks` exists (`PW/wallet/asset_lock/manager.rs:263-271`); the build takes its in-broadcast pin only after signing (`build.rs:278` → `:376`); registration's step 2 re-signs inside one call (`registration.rs:215-259`, `submit_with_cl_height_retry` and the IS→CL retry); dw-ffi's `Vault.lock()` is a sync UniFFI function that calls `session.lock_vault()` inline on the caller's thread (`dw-ffi/src/api/vault.rs:513-515`, `dw-engine/src/keys.rs:143-155`).
- **Model:** `python3 -I docs/design/checks/e0_04_design_model.py` exits 0 with **67 PASS, 0 FAIL**, including 29 mutations caught and 12 Part 1 scenarios. Output: `/work/scratch/dw-e004-r2-opus/model-run.txt`. GPT's rev0 traces (`rowless-rev0`, `snapshot-outcome`, `no-step-marker`, `no-payload`, `forget-rowless-history`, Part 2 `gate_window`/`rebind_cap`, Part 4) each fail under rev0 and pass under rev1, as the doc claims.

## Closure of r1

| r1 # | Severity | Status | Note |
|---|---|---|---|
| 1 Q1 declined has no end state | major | **partial** | Mode B (§2a.3) exists, with its own acceptance list (§2a.4) and phase (P2b). But its "no double pay" half has no data source; see finding 2. |
| 2 Outcomes invite a second payment | major | **partial** | Mode A is closed (`WillBeSent`, H11, H12, §8.4 from history). The contract reopens it through "absent ⇒ may retry" (finding 1a), and Mode B never defines it (finding 2). |
| 3 "Lock to cancel" after funding; no Lock control | major | **closed** | The copy is keyed on `funds_committed`, there is a "key held" state with an enabled Lock action, and `Vault.lock()` revokes leases on a locked vault (§3.6, §4.6, UX-SPEC). UX minors are in finding 5. |
| 4 m4 contract cannot carry the design | major | **partial** | §16 is written, and c3c9bbd implements most of it, but the two disagree in about a dozen places (finding 1). |
| 5 Cleanup leaves the in-broadcast pin pending | major | **closed** | §5.5, L6, L14 and I6. The model splits `reserved` from `pinned` (the `pin-left-pending` mutation is caught), and the tests assert that the inputs can be selected again (§12 "A dropped build"). |
| 6 PR shape | minor | closed | Fence-aware `SpvBroadcaster`, `DispatchContext`, `DispatchScope` in rs-sdk, DAPI clamp, P3 sized L. |
| 7 P2 sizing; async FFI lock vs the Swift freeze | minor | **partial** | P2a/P2b split and sync FFI lock are done. P4 is still sized M, and P2a/P5 still touch the frozen bindings (finding 6). |
| 8 Stale build-only API text | minor | closed | DASHPAY §2.6 and ROADMAP DP1-02 are fixed. |
| 9 Step 2's hidden ChainLock fallback | minor | closed | L17 in Mode A; in Mode B the residual is documented and a signer `Locked` maps to Parked. |
| 10 dispatch.sqlite lifecycle | minor | closed | `list_tracked_locks` exists at the pin, so seeding through the library is implementable. |
| 11 Provisional rows not re-driven in-session | minor | closed | H6 part 2 re-runs on peers 0 → >0 and after the journal recovers. |
| 12 L2 too broad | minor | closed in L2 | One stale test line remains (finding 7c). |
| 13 Contact-payment cancel code | minor | closed | `send.cancelled` (§4.6, §16.11). P5 and Q2 still say `send.cancelled_by_lock` (finding 7d). |
| 14 Four mobile conventions | minor | **partial** | Rebind contradicts DEC-67 (finding 3). `QuickUnlock` has a stale line, and auto lock has a gap (finding 5). |
| 15 No way out for rows with no entry | minor | closed | Tools ▸ Repair "Send it" / "Discard". |
| 16 Nonce refresh on refusal | minor | closed | Hook inside the retry closure (L19). |
| nits 1–6 | nit | closed | The contract's `RevokeCause` still lacks nit 3's `WalletClosed` (finding 1). |

## Findings

### 1. MAJOR: E0-04 §16 and the c3c9bbd contract disagree, and one disagreement re-allows a second payment

E0-04 §16 (`E0-04-grants-leases.md:1958-2001`, with §4.6 `:637-702` and §4.7 `:704-732`) compared with m4 at c3c9bbd (`m4-dashpay-engine.md`, `flows.rs`, `errors.rs`).

**a. Safety: "no entry" means "retry allowed" in the contract.**
- The contract's rules:
  - m4 `:1169-1171`: "`resume_registration`, `discard_registration`, `finish_asset_locks` and every retry the host offers act only when the artifact's journal entry is `NotSent` … **or absent**";
  - `:127`: "anything but `NotSent` **or gone**";
  - `:128`: "revoked **or gone**";
  - `dispatch_status` returns `None` "with no entry" (`:227`, `flows.rs:139`).
- E0-04 says the opposite: H11 (`:1040`) allows a retry only on `DefinitelyUnsent`, and §4.6 has an explicit `Unknown`.
- **Failure 1 (Mode A).** A row-less state transition has no journal entry by design (§5.5, "not persisted"). After a restart, the in-process set is gone.
  1. A `withdraw` returns `broadcast_unknown{st_hash}`.
  2. The app restarts.
  3. `dispatch_status` returns `None`, so the host's retry rule permits a retry.
  4. The retry signs a new withdrawal under nonce n+1. Both can execute, and the user withdraws twice.
- **Failure 2 (Mode B).** No asset lock ever has a journal entry. So "Register again" and a top-up retry after `broadcast_unknown` always see "absent" and build a second lock. That is exactly r1 finding 2.
- The contract also contradicts itself: `:1162` (retry only on `NotSent`) against `:1171` (`NotSent` or absent).
- **`finish_asset_locks` is gated backwards** (`:1170`). It resumes committed locks, which by definition are never `NotSent`. E0-04 does not gate it.

**b. Names and shapes. Each needs one answer.**

| Item | E0-04 rev1 | c3c9bbd | Suggested resolution |
|---|---|---|---|
| WillBeSent code | `platform.broadcast_pending` (§16.4 `:1985`, disposition `:71`) | `platform.will_be_sent{artifact}` | Take the contract's name. |
| `broadcast_unknown` parameter | none stated | `{artifact}` | E0-04 adopts it. |
| `dispatch_status` receiver and signature | `NetworkSession::dispatch_status(wallet, artifact) -> DispatchStatus` | `DashPay::dispatch_status(artifact) -> Result<Option<DispatchState>>` | Either receiver works; pick one. |
| Status values | `DefinitelyUnsent \| Committed{will_resend} \| Sent \| Unknown` | `WillBeSent \| MaybeSent \| Sent \| NotSent`, `None` = no entry | Use the contract's four values plus an explicit `Unknown` (never `None` ⇒ retry). |
| `DispatchResolved` | `EngineEvent::DispatchResolved{network, wallet, artifact, outcome}` | `{network, resolved: DispatchResolved{wallet_id, artifact, resolution}}` | Align the field names. |
| `DispatchResolved` triggers | five (`:663-665`), including "an `Ambiguous` write retried to durable", which settles nothing | two (resend accepted; reload refused) | Drop the Ambiguous trigger. Add the row-less settlement and Mode B's sources (finding 2). |
| `grant_request` | `grant_request(action: WriteAction) -> GrantRequest` | `DashPay::grant_request(identity, action: GrantAction) -> Result<GrantRequest>` | The contract's. |
| Charging | "exactly what it quoted" (§16.2) | "no more than it quoted; a stale quote is `grant_exceeded`, quote again" | The contract's. It fits Mode B's later correction to the actual debit (§4.2 `:489-490`). |
| Proof-wait grant | `RegistrationStatus.needs_grant: bool` while `Parked{ProofWaiting}` (§16.3) | no field; the row moves to `Unlock`/`Authorize` when the proof arrives (`:1235`) | The contract's (one of my r1 options). Drop `needs_grant` from §16.3. |
| `key_until` passing | lease → `Parked{ProofWaiting}` → `LeaseError::Parked` → `needs_grant` (§4.4 `:611`, §16.4) | `platform.lease_expired` ("its own key passed `key_until`", `:1180`) | The two codes conflict. Expiry of the key is a park, not an end; keep `lease_expired` for `end_flow` and the reaper only. |
| `lease_revoked{cause}`, `lease_expired` | absent from E0-04 | new codes | E0-04 adopts them. It must say when a call on a revoked lease is `lease_revoked` and when it is `cancelled`; for an uncommitted write both mean E0-04's `Cancelled`. |
| `RevokeCause` | `Lock \| Close \| PassphraseChange \| WalletRemoved \| WalletClosed` (§4.3 `:538`) | lacks `WalletClosed` (`flows.rs:55`) | Add it. |
| Journal unavailable | `platform.storage` (§6.4 `:1163`, §16.4) | common `storage` | Write `storage`. |
| Post-funding copy | "Lock stops it here; you'll finish after you unlock" (`:684`, `:2000`, DASHPAY `:456`) | "Lock to stop; you'll finish after you unlock" (`:1239`) | Pick one (see finding 5). |
| `funds_committed` | "any registered artifact (or, in Mode B, its funding call)" (§16.5) | "its journal entry left `Unsent`" (`:1240`), which never happens in Mode B | Contract gets the Mode B clause. |
| `LeaseView` / `leases()` | the UX "key held" tooltip needs `key_expires_in_secs` and the flow's name (UX-SPEC `:377-378`) | in no contract | Name the owning contract (m1 §2.1 or m4 §2.11). |
| Repair "Unrecorded asset locks" (§6.5 `:1205-1210`) | two actions | no facade call | Add one, or mark it as engine-internal until DP1-02. |
| Idle reaper | vault-key leases only, 10 min, "no call and no permit" | "ends a lease after a few minutes" | The reaper must skip a lease with a running `flow_task`. A vault-key registration in a 1 h ChainLock wait makes no call and holds no permit; it must not be reaped. |
| `FlowKind::Discovery` | no such lease (`IdentityScan` is a grant, not a purpose, §3.2/§4.2) | listed | Drop it or define it. |

**Fix.**
- Reconcile §16 with c3c9bbd under the contract's version rule (E0-04 owns the next bump).
- Make "no entry" non-retryable everywhere: `None`/`Unknown` ⇒ show "may have been sent", never offer a retry.
- Ungate `finish_asset_locks`.
- Add a test: a restart after `broadcast_unknown` on a withdrawal offers no retry.

### 2. MAJOR: Mode B cannot close its "no double pay" items as written, because nothing defines where `dispatch_status`, `funds_committed`, `WillBeSent` or `DispatchResolved` come from

- **Where:**
  - §2a.1 (`:264-267`) lists these as shared by both modes, and §2a.4 Mode B item 7 (`:334`) requires them;
  - every definition reads the journal's registered entries: §4.6 `:655-659`, §8.4 `:1404-1409` ("a registered artifact at `Dispatching`, `PreFence` or `Ambiguous`"), H11 and §16.5/§16.6;
  - in Mode B the journal holds only step markers (§0 item 2, `:97`; §6.2 `:1101`, "Mode A").
- **Failure.** A top-up in Mode B:
  1. `create_funded_asset_lock_proof` signs, tracks `Built` and is cut off by a peer loss, so the call returns an error.
  2. The engine reports `broadcast_unknown`.
  3. `dispatch_status(txid)` has nothing to read.
  4. Under finding 1a's "absent" rule, the UI offers a retry. Meanwhile H6 part 2 resends the first lock at the next peers 0 → >0 (pin semantics, `:297-298`).
  5. Two locks fund two top-ups.
- **The engine cannot always name the artifact.** If the call is dropped or MaybeSent, the engine never saw the txid. Nothing says how it finds the row (by funding type, identity index and account, or by diffing `list_tracked_locks`).
- **The fix is Mode B-only and small.** The pin has what it needs: `list_tracked_locks` (`manager.rs:263-271`), with each row's status. Specify a Mode B source of truth:
  - a tracked `Built` row ⇒ `WillBeSent`. The catch-up and the deferred resume resend it.
  - `Broadcast` ⇒ `MaybeSent`, or `WillBeSent` if you prefer to promise the resend.
  - `InstantSendLocked`, `ChainLocked` or `Consumed` ⇒ `Sent`.
  - No row, and the call failed with a pre-sign signer `Locked` or a pre-track error ⇒ `NotSent`.
  - No row otherwise ⇒ `MaybeSent`. Never "none".
  - The pin's `Rejected` arm untracking and releasing ⇒ `DispatchResolved(NotSent)`. A row reaching a proof ⇒ `DispatchResolved(Sent)`.
  - The engine identifies the row by the call's funding key (funding type plus identity index). It records that key before the call, under the call permit, as it already does for step markers.
- **Row-less state transitions across a restart (both modes).** Say how a `broadcast_unknown` ST ever resolves. The in-process set dies with the process, and H12 has no source for it. One option: a re-query of the identity nonce. If the ST's nonce was consumed by this hash, it is `Sent`; if it was consumed by another transition, `NotSent`. Otherwise it stays "may have been sent" and has no retry.
- **Tests.** Add the trace above to Mode B's acceptance item 7. The model does not cover Mode B call permits (§10.1, `:1569`), so this needs an engine-level test against the fake library.

### 3. MAJOR: rebind still skips the prompt when "require authentication for every payment" is on; DEC-67 rejected Q22

- **Where:**
  - §4.3 `:558-563` ("so 'require authentication for every payment' does not prompt again");
  - §4.8 `:740` ("A second prompt after an unlock is gone");
  - Q5 `:1908`, Q22 `:1925` (still "Yes"), Opus disposition 14 (`:83`), §16.3.
- **Failure.** An implementer builds the silent rebind that the manager rejected. The contract (c3c9bbd `:1229`, "Confirm to finish … the flow's grant died with an unlock") already assumes that a prompt follows.
- **Fix.**
  - Rebind is silent only when the setting is off and the vault is `Unlocked(Full)`, within the remaining caps (`Credential::None`).
  - With the setting on, the lease goes to `NeedsGrant` → `RegistrationWait::Authorize`, or the sheet's prompt.
  - Mark Q22 rejected (DEC-67), and add both cases to §12's rebind tests.
  - Also apply Q21: drop §3.1's stale line `:361` ("The quick-unlock spend limit does not apply. `QuickUnlock` keeps issuing `Spend` and `SignMessage` only"), which contradicts §3.7.

### 4. MINOR: Mode B's remaining closability gaps

**a. Step markers have no read rule in Mode B.**
- §7.6's "Reading" (`:1286-1290`) is written as fence verdicts (`Resend`, `Refused{step_possibly_dispatched}`), and Mode B has no fence. Mode B keys markers by "call id" (`:1115`), and a later process has a new call id.
- State the rule. At the call-permit J step of a resumable step:
  - if any marker exists for `(wallet, step_id)` and the lease is not live, the outcome is `MaybeSent`, not `Cancelled`;
  - with a live lease, the call proceeds, and the identity re-query settles it.
- Also note that the "J step" there does a `FULL` write. Use the `Committing` pattern, as in Mode A, so H1 holds.

**b. Mode B's `Cancelled` rule gives the wrong reason for step 2.**
- §2a.3 (`:289-291`) says step 2 "signs its only artifact before handing anything off". At the pin, step 2 re-signs inside one call: `submit_with_cl_height_retry` (`orchestration.rs:353`) and the IS→CL retry (`registration.rs:240-259`).
- The conclusion still holds: every re-sign follows a definite Platform rejection of the previous transition, and a `wait_for_response` timeout returns its own error, not a signer `Locked`.
- State that reason, add a fake-library test (a CL-height rejection, then a signer `Locked` on the re-sign), and keep the Parked mapping for the flow (§4.4 `:595`).
- Say also that the one-call path (`register_identity_with_funding`) is either forbidden in Mode B or always `MaybeSent`.

**c. Mode B lets library builds pick possibly-sent inputs.**
- The residual (`:309-310`) mitigates only engine-built `TxDraft`s. After a restart, a second `create_funded_asset_lock_proof` can select the inputs of a `Built` row that was possibly sent.
- No funds are lost: one of the two locks confirms and the other row is stuck. Still, the cheap fix is to start no library funding build while a tracked `Built` or `Broadcast` row of the wallet has no proof and H6 part 2 has not yet run on it.

**d. Mode A's acceptance list says "all of Mode B's list"** (`:343`). That includes Mode B's call-permit items 5 and 9, which P4 replaces "site by site" (`:1893-1894`). Say which Mode B items Mode A supersedes.

### 5. MINOR: UX: "key held", the post-funding copy and the mobile conventions

**a. Mode B copy precedence is undefined.**
- In Mode B, `funds_committed` is true from the funding call's start (§16.5). So during `create_funded_asset_lock_proof`, both §4.6 lines apply:
  - "Locking stops new signatures…" (`:682-683`);
  - "Funds locked — finishing…" (`:684`).
- Rule:
  - running call and no tracked row yet: the Mode B line;
  - tracked row present: "Funds locked — finishing";
  - between calls, before funding: "Lock to cancel".
- **Use DEC-67's user-facing wording**: "Lock stops new signatures; a transaction already signed may still be sent". Today's "a send already signed may still go out" (`:166`, `:304`, `:683`) uses "send" as a noun. That honours DEC-67 and is honest about Mode B.

**b. The "key held" state (UX-SPEC `:377-378`) has four gaps.**
- An own-key lease also exists on `UnlockedMixingOnly` (§4.4 `:567`). Mixing-only is already orange (`:106`), so define the combined state: the mixing-only icon plus the badge.
- Cross's short-text form is missing. Plain "Locked" would be dishonest; suggest "Locked · key held".
- The tooltip is registration-only ("Registration of @name"). Top-up needs its own name.
- The tooltip always says "Lock to stop". It should say "Lock to cancel" while `!funds_committed`, matching §4.6.

**c. Post-funding copy.**
- "Lock stops it here" (E0-04, DASHPAY `:456`) and "Lock to stop" (m4 `:1239`) differ; pick one. I prefer the contract's.
- "Funds locked" next to a "Lock" control invites a misreading ("my funds are frozen"). Consider "Funds committed — finishing". This is iOS-derived F2 wording, so it is a nit if pasta wants iOS parity.
- The `holds_key` line names only "Registration".

**d. Auto lock (§4.8).**
- Say that the auto-lock timer is armed only while the vault holds its key (`Unlocked` or mixing-only). Otherwise an inactivity timer firing on an already-locked vault would revoke every own-key registration, since `lock_vault` revokes even on `Locked` (§3.6). About 5 minutes of watching the progress screen, which is not activity (Q23), is about the IS window.
- Key the auto-lock copy on the outcome as well: "Locked automatically — cancelled" before funding, and "… unlock to finish" after.

**e. QuickUnlock limits (§3.7 `:428`).**
- "Applies the limits to the set's sums" lets "Accept and pay" reach almost twice the limit in value: duffs ≤ L, and also credits ≤ L × 1000.
- DEC-67 says "capped at the spend limit". Make it one combined value: `max_duffs + ceil(max_credits / 1000) ≤ spend_limit_duffs`.

**f. Two items that are fine as written.**
- The drain is non-modal (§4.6 `:687-690`), and the sync FFI lock returns after the gate.
- The `Vault.lock()` claim at `:1340` ("which the FFI already runs on a blocking thread") is not accurate: the sync UniFFI `lock` runs on the caller's thread (`vault.rs:513`). That is harmless, because today's lock already does the gate inline. Say that the FFI path runs `vault.lock()` inline and marks the barrier's gate as done, instead of block-waiting on the coordinator's `spawn_blocking` from a thread that may belong to the runtime.

### 6. MINOR: phase plan sizing and the Swift-shell freeze

- **"Common first" is right and closable.**
  - P2a (L) carries everything [A+B].
  - P2b (M) is Mode B wiring, and E0-04 can close at P2b.
  - P4 replaces call permits site by site.
  - Plus finding 2's Mode B source of truth (P2b), which stays M.
- **P2a is a heavy L.** It holds r1's P2 plus the barrier, history, step markers, attempt tracking and tombstone, `dispatch_status` and §16. Watch it, or split out the journal and the step markers. Not blocking.
- **P4 is understated at M.** It carries:
  - installing the fence;
  - registered entries, seeding and payload restore;
  - pending-spend fences;
  - the surfaced fallback wiring;
  - PSBT;
  - a new power-loss harness, and the kill matrix and stress at hand-off granularity.

  That is L.
- **The freeze, in P2a.** P2a's "the synchronous FFI lock and its events" adds `EngineEvent` variants (`LockProgress`, `LeaseChanged`). dw-ffi maps `EngineEvent` one to one, so this changes the frozen Swift bindings. The contract defers `DispatchResolved`'s variant to E0-13 for exactly this reason (m4 `:1310`).
  - Say that P2a adds the variants engine-side and that dw-ffi does not forward them until E0-13.
  - The FFI `lock()` keeps returning `VaultStatus` only.
- **The freeze, in P5.** P5 says "m1 contract and **both shells** updated" (`:1889`), which collides with DASHPAY R8. Either make P5 depend on E0-13 (bind it in the chosen stack only), or have dw-ffi map the new send code onto an existing one for the frozen shells.

### 7. MINOR/NIT: stale or inconsistent lines

- **a.** `funds_committed` is defined three ways:
  - `:648`, "some artifact of this flow", under which a contact request's MaybeSent ST would read "Funds locked";
  - `:677`, "any registered artifact";
  - `:1987`, registered artifact or Mode B funding call.

  Use the last one.
- **b.** `WillBeSent` covers `Ambiguous` (`:672`, `:1406`). Its copy, "when the network is back", is wrong for a disk error, and an Ambiguous entry at 0 is refused at the next load (§6.4 `:1158-1159`), so it may end NotSent. Use cause-neutral copy ("Will be sent"); `DispatchResolved(NotSent)` already covers the reversal.
- **c.** §12 "Library obligations" (`:1845`) still asserts that no `payment_guard` is held at `admit`, which contradicts L2's explicit allowance (`:1005`).
- **d.** `send.cancelled_by_lock` in P5 (`:1889`) and Q2 (`:1905`) against `send.cancelled` everywhere else.
- **e.** Repair "Discard" (`:1209-1210`) checks that the transaction is "not … in the mempool". An SPV client cannot see the whole mempool, and a withheld lock (GPT 5) can appear later. Name DP1-05's `RecoveredFromChain` as the backstop for that case.
- **f.** `DispatchScope{wallet, …}` lives in rs-sdk, which cannot use platform-wallet's `WalletId`. Use `[u8; 32]`.

## Summary

- **The verdict is APPROVE-WITH-CHANGES.**
  - rev1 closes my r1 majors 3 and 5 outright.
  - It closes GPT's seven majors in a model that passes 67/67, with each rev0 trace caught.
  - It gives Q1-declined a real design in Mode B.
- **Three majors remain, all doc and contract work:**
  1. reconcile §16 with c3c9bbd. Above all, "no entry" must never mean "retry allowed": as written it re-allows a double withdrawal after a restart, and a double top-up in Mode B;
  2. give Mode B a source of truth for `dispatch_status`, `funds_committed` and `DispatchResolved`, from `list_tracked_locks` and the call's outcome. Without it, Mode B's acceptance item 7 cannot pass;
  3. apply DEC-67's rejection of Q22 (and the Q21 cleanup of §3.1).
- **Mode B is otherwise closable.** Its promise is honest in the UI once the copy precedence and DEC-67's wording are applied (5a).
- **The phase order is right.** P4 should be L, and P2a's event variants and P5's shell rows must respect the freeze.
- **No re-architecture is needed.** None of the findings touches the journal, the CAS under J, the barrier or the model.
- **Scratch:** `/work/scratch/dw-e004-r2-opus/`.
