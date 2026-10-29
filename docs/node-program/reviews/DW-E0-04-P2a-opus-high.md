# DW E0-04.P2a — Opus high review (independent second reviewer)

No blocker or major finding. The money paths (charge → admit → settle → refund/release), the J discipline, the barrier and gate ordering, and close and removal ordering hold against the APPROVED design (DEC-73) as amended by DEC-134/138/154/160/163. Three findings are minor. One of them was reproduced with a scratch probe: row-less fence state keyed by artifact alone. It cannot be reached from any P2a call site, but it becomes reachable once a second producer admits the same bytes for another wallet. The rest are nits.

## Scope and method

- **Reviewed:** `6421be2fd00654a18940313a675612bab741210b` exactly (`dw/e0-04-p2a-leases`), as `git diff main...6421be2`, in a detached worktree at `/work/scratch/e0-04-p2a-opus/wt` with its own target. The branch was not edited, committed, pushed or commented on. The one scratch probe (below) was appended to the detached worktree's `tests.rs`, run, and reverted. The worktree is clean at 6421be2.
- **Read:**
  - REVIEW-COMMON;
  - the design `docs/design/E0-04-grants-leases.md` (§3–§8, §12 [A+B], §13 P2a row, §16), plus DEC-134, DEC-138, DEC-154, DEC-160 and DEC-163;
  - Sol r1–r5;
  - memory notes `dw-e0-04-design` and `dw-e0-04-p2a-impl`.
- **Code read in full:**
  - `platform/lease/{mod,budget,table,lock,session,fence}.rs`;
  - the P2a diffs of `send/mod.rs`, `session.rs`, `keys.rs`, `wallets.rs`, `multiwallet.rs`, `platform/flows.rs`, dw-ffi and dw-vault (`check_passphrase_change` / `apply_passphrase_change`);
  - `dw-appdb/src/dispatch.rs` (schema, `APPEND_MARKER`, `APPEND_RESOLUTION`, `COMPACTABLE`, `standing`, `register`, `resolve_sent`, `erase_wallet`, `sweep`);
  - the pinned library's `classify_spv_send_error` and `SpvBroadcaster` at bc41f1b.
- **Runs.** All used `nice -n 10`, `CARGO_BUILD_JOBS=8`, `RUSTC_WRAPPER=`, umask 022 and TMPDIR on `/work/scratch`.
  - `cargo test -p dw-engine --lib platform::lease`: 83/83 (plus the probe) green, 246 s.
  - `cargo test -p dw-engine --lib send::`: 34/34 green.
  - `cargo test -p dw-appdb --lib`: 42/42 green.
  - `DW_LEASE_STRESS_SEEDS=20` stress: `the_stress_upholds_the_invariants`, `the_stress_catches_every_mutation` and `the_checker_flags_each_invariant` all green (469 s).
  - Design checks: `e0_04_design_model.py` ok; `e0_04_dispatch_model.py` ok; `e0_04_mutations.py` and `e0_04_split_model.py` against the dispatch model show a clean baseline, and every mutation is flagged.
- PLAN §7 handling: codenames only. Nothing in this change touches a Dash Core consensus or P2P surface.

## Findings (most severe first)

### O-1 — minor (latent money safety): row-less fence state is keyed by artifact, not (wallet, artifact)

**Location:**
- the maps: `fence.rs` `rowless`, `spend` and `sent`;
- `decide`'s lookup filtered by wallet: `fence.rs:1831-1836`;
- the unconditional inserts: `fence.rs:1939` (Unleased), `:2000` (stepped leased First) and `:2020` (leased First);
- the tombstone overwrite in `refuse_rowless`: `fence.rs:2038-2047`;
- `finish`'s wallet-blind lookup: `fence.rs:2094`;
- the cross-wallet overwrite and refund in `bind_spend` / `unbind_spend`: `fence.rs:1359-1397`.

`decide` reads the row-less entry only when it belongs to the requesting wallet. For another wallet's live `Admitted` entry it sees `None`, so `joins = false`, and it takes a fresh First that **replaces** the other wallet's entry. That entry may have `running > 0` and carry a charge. `finish` then looks the entry up by artifact alone, so whichever attempt finishes against the replaced entry settles on state that isn't its own. `bind_spend` / `unbind_spend` have the same shape: a second wallet's prepare of the same txid replaces the first binding, and an unbind by txid refunds whichever lease is bound.

**Reproduced** (scratch probe, appendix A; passes at 6421be2):

1. W2 prepares and admits artifact X (First `p2`, Spend 500 moved to the entry). `p2` is in the library.
2. W prepares the same bytes and admits X. The verdict is First `p1`, not a join; W2's entry is replaced.
3. `p1.finish(NotSent)` returns **`DefinitelyUnsent`** while `p2` is still in flight.
4. `p2.finish(NotSent)` returns `MaybeOut`.
5. W's Spend is refunded to 0. W2's 500 is never refunded.

**Failure scenario.** W's caller gets `Phase::Released` (`send/mod.rs:1275`). It unbinds, drops the pending spend and calls `abandon_transaction`, then tells the user "not sent", while the identical bytes may already be on the wire through W2's attempt. A user who pays again with fresh inputs pays twice, which breaks I-rule "NotSent only when nothing can be on the wire". The other effects are conservative: W2's charge leaks, and W2's attempt reports MaybeSent.

**Reachability.**
- **Not reachable at 6421be2.** The only production `admit` is the leased `TxDraft` send (`send/mod.rs:1228`). Its inputs belong to one wallet, and `WalletId` is the root-xpub hash, so two loaded wallets cannot sign the same txid.
- **Becomes reachable** once the design's other producers route through `admit`. Examples: `UnleasedKind::External` (a PSBT whose inputs span two loaded wallets), `Rebroadcast` of a transaction seen by both wallets, and P2b/P4 multi-wallet hand-offs. DEC-138 already keys registered entries by (wallet, artifact). The row-less side should follow before any of those land.

**Suggested fix (either):**
- (a) Key `rowless`, `spend` and the `sent` lookups by `(wallet, artifact)`, as `entries` is. `dispatch_status` and `note_seen` already filter by wallet.
- (b) Keep artifact keying, but in `decide` treat another wallet's `Admitted` / `Resolving` entry as `Deferred`, and its `Revoked` / `NotSent` as `None`. Make `bind_spend` refuse, rather than overwrite, a binding of another lease, and make `unbind_spend` take the lease.

Add the appendix probe, inverted, as a regression.

### O-2 — minor (H12 gap): `note_seen` and `note_executed` have no production caller

**Location:** `fence.rs:2266` (`note_executed`) and `fence.rs:2327` (`note_seen`). The only references outside tests are their definitions and the doc comment at `send/mod.rs:1302` ("a verdict later than the deadline arrives as `note_seen` instead").

§4.6 lists "the wallet sees the transaction" as a source of `DispatchResolved(Sent)`. H12 says every provisional outcome emits `DispatchResolved` when it settles. P2a wires neither the wallet's transaction-seen path nor the confirmation path to `note_seen`.

**Scenario.**
1. A leased payment's First is cut at its permit deadline (`hand_off` → `Cut`) and reports `BroadcastUnknown` / MaybeSent.
2. The transaction is in fact relayed and confirms.
3. `dispatch_status` keeps returning `MaybeSent`. No `DispatchResolved(Sent)` is ever emitted, and the lock report's outcome for that flow stays `MaybeSent` for the life of the session.

This fails safe (no refund, inputs stay pinned), so it isn't a money bug, but the UI's "may have been sent" never clears. Nothing in the Sol rounds flagged it, because every test calls `note_seen` directly.

**Suggested fix.** Call `note_seen(wallet, txid)` from the wallet's transaction-seen or confirmation handling (or the pump's history refresh for a txid with a row-less entry). Alternatively, record this in the P2a/P2b split as explicitly deferred, and fix the doc comment at `send/mod.rs:1302` to say so. Add one session-level test: a cut First followed by a seen transaction emits `DispatchResolved(Sent)`.

### O-3 — minor: revoked leases and their owners are never collected until `end_flow` or close

**Location:**
- `table.rs:169-176`: `idle()` requires `state.live()`;
- `table.rs:563-566`: `reap`'s `retain` keeps `ended_at == None`;
- `flows.rs:190-197`: `begin_flow` purges only `Ended` owners, though its comment says owners "that ended otherwise (reaper, lock) go now".

A lock leaves every lease `Revoked`, not `Ended`. The reaper never ends a non-live lease, and `retain` keeps any entry with `ended_at == None`. `begin_flow` keeps a `Revoked` owner, so `flow_leases` holds it and the lease never reaches `Ended` either.

**Scenario.** A host that doesn't call `end_flow` after a lock-cancelled flow — a sheet dismissed by the lock, a crashed web view — leaks one table entry (budgets, history) and one owner per flow per lock, until session close. Lock reports and `leases()` keep listing those flows.

**Suggested fix.** Treat `Revoked` like `Ended` for the owner purge (`matches!(v.state, Ended | Revoked { .. })`), and let `reap` end a `Revoked` entry with no permits and no tasks after `idle`. At minimum, correct the comment.

### O-4 — nit: `fenced` releases on every `Refused`, ignoring `cleanup`

**Location:** `send/mod.rs:1261-1263`.

L6 says only the cleanup winner releases. `Refused{cleanup:false}` arises from a `Revoked` tombstone (a repeat of a refused or settled artifact) or a `Reg::Revoked`. Today every such case concerns bytes that were provably never sent and were already released once. The second release just calls `abandon_transaction` on an absent transaction, so it's harmless. With O-1's overwrite it isn't harmless. This is defence in depth: map `cleanup:false` to `Phase::Unknown` with no release, or to a no-op.

### O-5 — nit: `close_journal` doesn't wait for in-flight `register` writes

**Location:** `fence.rs:1131-1145`. `register`'s write isn't counted in `fence.writes`.

A register of a flow being aborted at close can commit after `close_journal` returns and the backend is dropped. The result is an `Unsent` row with a dead origin, which the next load refuses and cleans up (§6.5), so this is benign. Count register writes in `writes` (or wait on `registering`) for symmetry with r2 L1.

### O-6 — nit: a stale-slot `apply_passphrase_change` is reported as `WrongPassphrase` after leases were revoked

**Location:** `session.rs:263-299`; `dw-vault/src/vault.rs:923-935`.

Two concurrent `change_passphrase` calls both pass `check` under the old passphrase, then both freeze. The second `apply` sees the slot changed and returns `WrongPassphrase`, and every lease has already been revoked twice. That ordering is DEC-134(1)'s accepted trade, and the attempt isn't counted. Still, the user is told the wrong cause. Map the slot-identity mismatch to a distinct error (`vault.changed_concurrently`), or serialize the check-and-apply under the op gate.

### O-7 — nit: `gate_panicked()` claims `Locked` / `encrypted`

**Location:** `lock.rs:349-360`.

When the gate panics, the vault's state is unknown. Its comment says so ("the vault may hold its key"), but the returned `VaultStatus` claims Locked. Report `this.vault.status()` read after the panic, or a state the UI renders as "unknown".

### O-8 — nit: `register_task` keeps a task registered after `abort_tasks` already ran

**Location:** `table.rs:929-937`.

`Some(e)` pushes even when `closed` is set. A flow task registered after close's `abort_tasks` (a late spawn racing close) isn't aborted. Close's gate stops it from doing harm, but abort it when `i.closed`.

### O-9 — nit: a spurious `NeedsGrant` when `begin` races an unlock

**Location:** `table.rs:981-1007`.

A `begin` whose issue straddles `vault_op`'s unlock sees `epoch_before != epoch_after` and inserts the lease `NeedsGrant`. The user is asked for a grant they just gave. This is fail-safe; mention it in §4.3 or retry the issue once.

## Focus 1 — money safety end to end

- **No double pay.**
  - A First is granted only under a live lease with a Spend binding for that txid (or credits granted), in the same J step as the charge.
  - Every copy of a marked artifact waits for its markers (H10).
  - Registered artifacts never settle `DefinitelyUnsent` (L13, `finish`'s `a.registered` branch).
  - The only gap is O-1, which is latent.
- **NotSent only when nothing can be on the wire.** Settlement requires all of the following:
  - this attempt reported `NotSent`;
  - `running == 0`;
  - `!possibly_out`, or H16's `slot_taken` for transitions;
  - durably, the `NotSent` row committed (r1 F2 and DEC-154(3); a failed write counts as committed).

  `attempt_outcome` maps only definite pre-wire failures to `NotSent`: `NoPeers`, `BroadcastRejected`, `PreparedTxSpent`, `Cancelled`, `NetworkNotOpen` and `WalletNotFound`. The pinned library classifies only `NetworkError::NotConnected` as `Rejected` (it never reached a peer); everything else is MaybeSent. `hand_off`'s `NotEntered` is checked before the first poll, and `Cut` is MaybeSent (L5).
- **Resend only on evidence.** A Resend needs a `Dispatching` / `PreFence` row, a joined `Admitted` entry, a standing marker, or a `Sent` row, and DEC-163's bundle makes Sent evidence atomic with its markers. The `Ambiguous` → `Record` write precedes the Resend.
- **Refunds only when provably unsent.**
  - Funding is refunded on a failed or abandoned register and on `Reg::Unsent` with a dead origin (`revoke_unsent`).
  - Spend is refunded on `unbind_spend` before admission, on `refuse_rowless`, and on a `DefinitelyUnsent` settlement.
  - Credits are refunded on a `DefinitelyUnsent` settlement.

  Each refund is a `take()` or a map removal, so it happens at most once (confirmed by `spend_charges_bind_to_the_txid_and_refund_on_a_definite_not_sent`). A MaybeOut keeps the charge.
- **Grant accounting.**
  - Rebind sets each purpose's `ceiling = min(available, fresh)`, and older generations' refunds never lift it (`a_rebind_caps_each_purpose_and_old_refunds_never_lift_it`).
  - A zero charge still needs the purpose (r1 F8).
  - `grant_request` constants: CONTACT_REQUEST 150 M, NAME 300 M, CONTESTED_NAME_FUND 2·10¹⁰ credits = 0.2 DASH (matches Platform's contested-name fee), DOCUMENT_10K 300 M, KEY_UPDATE 100 M.
  - `contested()` normalizes o→0 and i/l→1 over 3–19 characters of `[a-z01-]`, matching DPNS.

## Focus 2 — lock and lease semantics

- **Barrier.** `freeze` revokes, snapshots the in-scope permits, bumps `lock_gen` or `wallet_gen`, and sets gates/drains in one J step before the first await (H4). `begin` re-checks the barrier and generations at insert, after its `spawn_blocking` issue, so a lease issued across a freeze is refused (`begin_lease_paused_between_redemption_and_insert_is_refused`).
- **Joined locks.** Each request runs its own gate (H14), and the drains share the snapshot subset. `vault_op` waits for `barrier.gates == 0` before an unlock (`joined_locks_run_their_own_gates_and_an_unlock_waits_for_them`).
- **Own-key lease.** The key timer parks at `key_until`, and `proof_wait_started` / `chainlock_fallback` move it, gated by the origin lease.
- **Epoch change.** `vault_op` compares epochs, then calls `epoch_changed` → NeedsGrant, then `ensure_background`; `observe_epoch` is monotonic. Rebind is allowed only from NeedsGrant or Parked, with a fresh-epoch check.
- **Background lease.** It is dropped in the freeze and re-created only with an unchanged generation and epoch (`create_background`'s insert check).
- **Close ordering.** `close_leases` (freeze Close → drain → finish → `abort_tasks`) runs before `gate.close()`, and `close_journal` runs last. `closed` refuses new leases (`nothing_is_created_after_close`).
- **Cancellation safety.**
  - `lock()` runs its coordinator as a spawned task; `lock_sync` spawns its drain.
  - `Freeze::drop` releases gates and drains.
  - `checked_revoking_vault_op` spawns the gate, drain and `ensure_background`.
  - `register` writes on a spawned task, with refund on abandon.
  - `admit` uses PendingFirst: a dropped admit discards its permit and releases a step-scoped entry (`a_dropped_admit_or_register_keeps_the_fence_consistent`).
  - `remove_wallet` and unload run `on_runtime`.

  Every async entry point is safe against its caller being dropped.

## Focus 3 — concurrency

- **J is a leaf.** `with_j` takes no other lock inside, and `Effects` (drops, events, notify) run after J is released. Vault calls run on the blocking pool outside J, and journal writes run on spawned tasks. Lock order is session op gate → (vault gate on the blocking pool) → J, and `flow_leases`' mutex is never held while taking J (`begin_flow` drops `Ended` owners outside it).
- **No lost wake-ups.** `wait_until` and `drained` call `notified.enable()` before checking.
- **Drop and panic paths.** `Freeze::drop` releases the barrier, the `LeaseOwner` drop ends the lease, and a panicking resolution is retried (`r1_f2_a_panicking_resolution_is_retried`). A panicking gate releases its gate count via `gate_done` in `finish` and drop (see O-7 for its status).

## Focus 4 — the three "never ruled" questions

DEC-134 (2026-10-09) ruled all three, so the prompt's premise is out of date. I checked the implementation against each ruling and recommend keeping all three.

1. **A wrong old passphrase revokes leases?** Ruled no: the check runs before the freeze. It is implemented in `session.rs:278-279` (`check` on the blocking pool, `?`, then `freeze`) and tested by `a_wrong_old_passphrase_revokes_no_lease_and_a_correct_one_revokes_every_lease`.
   - **Recommendation:** keep it. Add O-6's distinct error for the stale-slot race, the one path where a revocation precedes a refusal.
2. **An orphan `Unsent` row from a `register` stalled across removal?** Ruled no orphan: removal's erase waits for in-flight registers of the wallet, and the wallet barrier refuses new ones. It is implemented in `erase_wallet_rows` (waits on `registering`) with the barrier check in `register`, and tested by `the_removal_erase_waits_for_an_in_flight_register` and `r1_f4_duplicate_registers_cannot_outlive_a_removal`.
   - **Recommendation:** keep it. The residual close-time variant (O-5) is benign, because the next load refuses a dead-origin `Unsent` row.
3. **A seed left behind when `wipe_wallet_secret` fails?** Ruled to fail closed, vault first: it is implemented in `wallets.rs:239-242`, before `manager.remove_wallet` and the row deletes.
   - **Consequences (documented in the method's doc comment):** a failed wipe still consumes the grant and revokes the wallet's leases, and a failure after the wipe leaves a listed wallet without a seed; removing it again finishes the job.
   - **Recommendation:** keep it. The only gap is that no engine-level test injects a wipe failure (dw-vault tests cover the vault side). A session test where the wipe fails and the wallet stays listed and usable would pin the ordering.

## Focus 5 — test quality

The §12 [A+B] rows are covered by `tests.rs` (62 tests, 36 of them review regressions) and `session_tests.rs` (15, with a real vault and real SPV dispatch for L5). The stress checker covers I1–I6, I10, I11 and I16, plus L5 and DEC-134. All 15 mutations are caught. The design models' baselines are clean, and they flag every mutation. This is strong work.

Three realistic bug classes would get past the suite:

1. **Cross-wallet artifact identity (O-1).** No test, stress schedule or model admits the same artifact under two wallets. The stress generator draws artifacts per wallet. Appendix A is the minimal regression. Adding "two wallets, shared artifact" to the stress generator would also cover `bind_spend` / `unbind_spend`.
2. **The consumer side of settlement.** The stress checker models the table and a fake library, but not the send path's release (`Phase::Released` → `unbind_spend` + pending-spend removal + `abandon_transaction`) or its pin. Two mutations in `fenced` would therefore pass the stress: releasing on `MaybeOut`, or releasing on `Refused{cleanup:false}` (O-4). Only the r1_f3 session tests constrain the mapping, and only for the paths they drive. A small table-driven test of `fenced`'s `(Verdict, Settlement) → Phase` mapping, with a seam `broadcaster`, would close this.
3. **Budget ceilings under concurrency.** The checker has no invariant that `spent ≤ ceiling` per generation across interleaved charge, refund and rebind. The unit tests cover it sequentially only. Adding it to the checker is cheap, because charges are already logged as `Grant` events.

## Appendix A — probe for O-1 (scratch only; append to `lease/tests.rs`)

```rust
#[tokio::test(start_paused = true)]
async fn opus_probe_cross_wallet_rowless_overwrite() {
    let (t, _rec) = table();
    with_journal(&t);
    let l1 = begin(&t, W, 0, 0, 1_000).await.unwrap();
    let l2 = begin(&t, W2, 0, 0, 1_000).await.unwrap();
    // W2 admits the bytes first; its attempt is still running.
    draft(&l2, art(5), 500);
    let p2 = first(t.admit(core(&l2, art(5))).await);
    // W admits the identical bytes: the entry is filtered by wallet, so it
    // is a fresh First that replaces W2's Admitted entry.
    draft(&l1, art(5), 400);
    let p1 = first(t.admit(core(&l1, art(5))).await);
    let s1 = p1.finish(Outcome::NotSent);
    let s2 = p2.finish(Outcome::NotSent);
    // Observed at 6421be2: s1 = DefinitelyUnsent (W2's attempt still in
    // flight), s2 = MaybeOut, Spend l1 = (1000, 0), l2 = (1000, 500).
    assert_eq!(s1, Settlement::DefinitelyUnsent);
    assert_eq!(s2, Settlement::MaybeOut);
}
```

The fix's regression inverts the first assertion: `s1` must be `MaybeOut` (or the second admit must be `Deferred` or a join), and W2's charge must be refunded exactly once when both attempts end `NotSent`.

**Verdict:** APPROVE-WITH-NITS. There is no blocker or major. Fix O-1 (row-less keying) before any non-`TxDraft` or multi-wallet producer calls `admit`, ideally in this branch since the change is local to `fence.rs`. Wire or explicitly defer O-2. Fix O-3's owner purge. O-4 to O-9 are nits.
