# DW-DP1-02 Phase A — Opus-high review

- **Repo:** dashwallet-desktop, branch `dw/dp1-02-registration`, head **7e331dd** (rebased on P2b 9c9ed46).
- **Reviewed in:** detached worktree /work/scratch/dp1-02-opus/wt. The branch itself was not touched.
- **Spec:**
  - docs/design (DP1-02 rows, §2a.5, §12);
  - docs/contracts/m4-dashpay-engine.md: the Contract-Version 9 "Done (DP1-02)" list (the file now reads Contract-Version 10);
  - DEC-171, DEC-179 and DEC-183.
- **Prior rounds:** r3, r4 and r5 (Sol). r5 approved, and its N1 is addressed by 7e331dd. I don't reopen any closed item without new evidence. MJ-1 and MN-1 are new: probes reproduce them, and no earlier round raised them.
- **Review-only patch:** /work/scratch/dp1-02-opus/review-probes.patch. It holds the probes, the mutant switches and two Fake knobs; none of it is for the branch.

## Findings

### Major

#### MJ-1 A name step that fails for good after the identity exists wedges registration for the wallet permanently

**Location:**
- registration_flow.rs:2142: `names()` maps `Taken | Locked | ContestOpen | Invalid | …` to `Step::Fail(code, false)`.
- registration_flow.rs:1102-1138: `funding_status` reads the consumed lock as `Sent`.
- registration_flow.rs:1156: `funds_committed`.
- registration.rs:423: `in_progress` counts a failed row while `retryable || funds_committed`.
- registration.rs:784: `discard_refused` refuses committed funds.
- registration.rs:705/727: `resume` and `clear_failure` refuse a row that failed for good.

**Scenario:**
1. The user starts registering `alice2`; the label was free at `plan`/`start`.
2. The flow funds and waits for its proof: up to 300 s for InstantSend, then a ChainLock with no limit, and possibly a park at `Unlock`.
3. During that wait someone else registers `alice2`.
4. `IdentityCreate` succeeds and the lock is `Consumed`. `register_name` then returns `NameError::Taken`, and the row is written `Failed{name.taken, retryable: false}` at `name_requested`.

From then on:
- `funding_status` reads `Sent` (consumed lock), so `funds_committed` is true, so `in_progress` is true for ever.
- Every `start_registration` for the wallet is `registration.in_progress`.
- `discard_registration` is refused ("the registration's funds are committed").
- `resume_registration` is refused ("failed for good (name.taken)").
- "Finish transfers" skips the row: it has an identity and isn't `KeysPrepared`.

There is no exit short of editing the database. The code comment at :2140 says "the identity exists, and the user picks another name with `register_name`". `register_name` is indeed usable, but registration stays blocked anyway, because nothing ever ends the row.

The same wedge follows any `Fail(_, false)` after `IdentityRegistered`:
- `name.contest_open`, `name.locked` and `name.invalid`;
- a non-retryable Platform error from `failed()` in the name or profile step;
- `internal()` at :2110/:2155.

**Evidence:** the probe `review_opus_name_taken_after_identity_does_not_wedge_new_registrations` (a Fake knob `name_error = Some(Taken)`) fails:
```
after name.taken: phase=name_requested error=Some("name.taken") retryable=false identity=Some("44KC…") funds_committed=true
discard=Err(Platform(InvalidArgument { detail: "the registration's funds are committed" }))
resume=Err(Platform(InvalidArgument { detail: "registration 1 failed for good (name.taken)" }))
start=Err(InProgress)
```
No existing test drives a non-Platform `NameError`. The contract text ("a failed row counts while it is retryable or its funds are committed") reads literally to this outcome, so this is a contract gap as much as a code one.

**Fix:** once the row's identity exists (`row.identity.is_some()`, and the wallet has it), its funds have become that identity, so it has nothing left to commit. Either fix works; I prefer (b):
- (a) `funds_committed` (or just `in_progress` and `discard_refused`) treats a row that failed for good and has an identity as settled: it doesn't count and it may be discarded. Discarding loses nothing, because the identity lives in the library and `register_name` names it.
- (b) A name failure that isn't Platform/retryable ends the row as `Done` without a name, carrying a `name_failure` code for the UI. The identity is registered; only the name isn't.

Add a test per code (`Taken`, `ContestOpen`, `Locked`, a non-retryable Platform error in the profile step). Each asserts that a later `start` works, or that discard succeeds and then `start` works. Update the contract's "In progress" bullet to match.

### Minor

#### MN-1 L7 is not harmless at identity index 0: an unrecorded outpoint plus a non-Ok call wedges the row, and the lock goes to a second row

**Location:**
- registration_flow.rs:1899-1935: `fund`'s post-call attribution through `ev.lock_of(current)`.
- registration_flow.rs:184: `known_index`, where `Broadcast` at index 0 reads as unknown.
- registration_flow.rs:1963: `maybe_sent`.
- journal.rs:488-505: the tap only logs a failed record (Opus r1 L7).
- store.rs: `inner.store()?` runs before `tap.record`, so a failed library store skips the tap entirely.

**Scenario:**
1. A first identity (index 0) funds.
2. The library tracks the lock `Built` and stores the changeset, but the outpoint never reaches the row. Either the dw appdb write in the tap failed (L7, logged), or the library's own `persister.store` failed (logged by the library), so the tap never ran.
3. The broadcast goes out and the call ends non-Ok: `IsTimeout`, or `TransactionBroadcastUnconfirmed`, with the lock still tracked as `Broadcast` at index 0.
4. Post-call, `lock_of` finds no outpoint on the row. `known_index(Broadcast, 0)` is `None`, so the row's own live lock is not attributed, and the match takes `(Err(e), None) → maybe_sent`.
5. The row gets `platform.broadcast_unknown`, not retryable, with no outpoint.

After that:
- `funding_status` reads `MaybeSent` for ever.
- `start` is refused as in progress, and so are `discard` and `resume`.
- The tap later passes index `None`, so it never attaches the lock.
- "Finish transfers" skips the row: `awaiting_a_lock` wants `error.is_none() || retryable`. It gives the lock to a new unnamed row at a fresh index.

The funds do become an identity, so nothing is lost or paid twice. But the original row wedges registration exactly as in MJ-1. This contradicts the contract sentence "then the tap hands the lock to it (the row resumes at `FundingSent`, no second row)". At index 0 the lock is only ever re-tracked with index 0 (reconstruction), so that path can't happen there. The existing `an_unbacked_funding_is_maybe_sent_until_its_lock_is_tracked` (registration_tests.rs:1132) avoids the case by re-tracking at index 1.

**Evidence:** the probe `review_opus_untapped_index_zero_lock_is_attributed_after_the_call` (`skip_tap`, IsTimeout, index 0) fails:
```
index 0 after the call: phase=keys_prepared error=Some("platform.broadcast_unknown") retryable=false outpoint=None index=Some(0)
after Finish: draft=1 label="alice2" phase=keys_prepared error=Some("platform.broadcast_unknown") outpoint=None index=Some(0)
after Finish: draft=2 label="" phase=proof_waiting error=None outpoint=Some("0101…:0") index=Some(1)
finish=1/0/1 status.funds_committed=true start=Err(InProgress) discard=Err(… funds are committed)
```
The control `review_opus_untapped_index_one_lock_is_attributed_control` passes: at index 1 the same failure ends at `ProofWaiting` with the lock.

The trigger needs a storage failure, so this is rare. The outcome, though, is permanent and hits the most common index, which is why it is minor and not a nit.

**Fix:** either works.
- Snapshot the tracked registration-lock outpoints before the call (in the actor job that takes the fence). After the call, attribute a lock that is new since the snapshot and that no row holds to the calling row, at any index. Only this call could have built it, because the fence and `in_call` exclude any other funding call for the wallet.
- Let `awaiting_a_lock` (Finish) and the tap's unknown-index branch also accept a `broadcast_unknown` row with no outpoint, as the oldest candidate whose amount the lock covers. The design already treats `broadcast_unknown` as "until the lock is tracked".

Add the index-0 probe as a test.

#### MN-2 The automatic backup is not re-armed after `unfund`

**Location:** registration_flow.rs:2251 `backup_once` (`backed_up` is set once per `(wallet, draft)`), and :1941 `unfund`.

**Scenario:**
1. The first funding attempt is tracked `Built`. The tap writes its outpoint, `on_funded` fires, and `backup_once` schedules the automatic backup.
2. The broadcast is definitely rejected (`TransactionBroadcast`), so `unfund` returns the row to `KeysPrepared`, retryable, with the outpoint cleared.
3. The retry funds a new lock, and the tap records the new outpoint. `on_funded` finds `backed_up == true`, so no backup is scheduled for the live lock in this process.

The newest backup now holds either the rejected lock's outpoint or a `KeysPrepared` row, not the live funding (Restore rule 4: a backup after funding carries the outpoint). If the dead outpoint is restored, the row reads `MaybeSent` and waits for ever, and the live lock (reconstructed at index 0) goes to an unnamed row through Finish, as in MN-1.

**Fix:** clear `backed_up` in `unfund` (in the same job that writes it), or key the once-flag by outpoint instead of draft. Add a test that runs reject, retry and tap, and asserts that two backups were scheduled.

#### MN-3 The profile step is not idempotent against Platform

**Location:**
- registration_flow.rs:2556 `PwLib::create_profile`: it checks only the library's local `managed_identity(..).dashpay().profile`.
- Pin profile.rs `create_profile_with_external_signer`: it calls `put_document` unconditionally, then `set_dashpay_profile`.

**Scenario:** the put reaches Platform, but the call times out, or the app is killed between `put_document` and `set_dashpay_profile`. Resume runs the step again. The local profile is still empty, so it puts again, and Platform rejects the duplicate under the profile contract's unique `$ownerId` index.

That's a retryable failure that repeats until the library's own-profile sweep (profile.rs:80-115) refreshes the local profile. The rejected state transition may also cost credits.

The kill matrix doesn't catch this, because the Fake's `create_profile` is idempotent (a `BTreeSet`). The design says "Every step is idempotent against platform-wallet's own tracking", and the library tracks the profile only after the put returns.

**Fix:** before putting, fetch the identity's profile document from Platform (as the name step already does for DPNS state) and treat an existing one as done. Make the Fake's `create_profile` non-idempotent (a second put fails), so the kill-at-profile row of the kill matrix covers it.

#### MN-4 12 of 25 targeted mutants survive, several of them on rules the contract states

**Location:** registration_tests.rs / registration_flow.rs flows_tests.

**Evidence:** each survivor was run against the 59 registration tests and against 109 more lib tests (journal, recovery, runtime, session, coins, backup, names), and passed all of them. Mutants that break a rule the contract states, and that no test catches:
- **B:** the tap ignores its unreadable-row guard, so it hands a lock to a readable row while an unreadable row may own it. This breaks DEC-173 (2) on the tap side; the Finish side is covered (Q killed).
- **I:** a write that went `Stale` because the generation was taken reads as `Moved`, so the run re-reads and goes on. This breaks "A run whose generation was taken stops at its next write". Only the funding-call gate is tested (V killed).
- **N:** Finish's waiting-row candidates include a row whose funding call runs. The contract says "not one whose funding call runs".
- **O:** a NotSent-class error beats a tracked lock and unfunds the row. This breaks H12's first rule: "a tracked row … means committed". It is unreachable at the pin but is the rule's whole point.
- **W:** the tap ignores which row's call runs and takes the first row without an error. This breaks "The tap's lock goes to the row whose funding call runs". Every tap test has a single candidate row.
- **G:** `index_owner` drops "another row at the index that is … older".
- **H:** `create_identity` adopts any identity at the index. That breaks "An identity with no lock to prove it is never adopted" after funding.
- **R:** `unheld` counts a lock an unreadable row may own.
- **F:** discard's in-job re-judgment is skipped. That's the DEC-173 race guard; the delete's CAS on phase and outpoint partly backs it up.

Equivalent or defensive, needing no test:
- **X:** the driver's `funding_status` gate, unreachable after `Free` (N-3).
- **E:** `enter_funding`'s outpoint check, which only matters if a tap or Finish write lands between the step's read and the job.
- **U:** `funds_committed` with `in_call`. Discard is still refused by `flows.take`; only `statuses().funds_committed` during a call changes.

**Scenario:** a later refactor that drops any of B, I, N, O, W, G, H or R passes CI. B, N and W can attach a lock to the wrong row. That is still recoverable, but it's the wrong-row class DEC-171 and DEC-173 exist to prevent.

**Fix:** one test per rule:
- B and W: two rows, a tap with the in-call row second, and an unreadable row alongside;
- I: take the generation between a run's two writes, and assert the second doesn't apply and the run stops;
- N: Finish during a funding call with an unknown-index lock;
- O: a Fake that returns `NotSent` with the lock still tracked;
- G: two imported rows at one index;
- H: an identity appears at the index after funding with no lock behind it;
- R: an unreadable row plus an unheld lock at another index;
- F: a test hook that writes an outpoint between discard's pre-check and its job.

### Nits

- **N-1** migrations/V2026101001__registration_owner.sql:6. "The writes that only record evidence (the changeset tap's lock outpoint, a stranded lock's new row) name none." This is stale: since DEC-171 rev, the tap and the stranded import write under the owner read with them.
- **N-2** registration_rows.rs:3. The module doc says "the one engine path that writes the asset-lock outpoint" lives here; it doesn't.
- **N-3** registration_flow.rs:1808-1830. Driver `fund`'s `funding_status` gate is unreachable once `index_owner` returned `Free` (mutant X survives, see below). Keep it as defence in depth with a comment saying so, or drop it.
- **N-4** A flow stopped at `IndexOwner::Unknown` reports `waiting = ChainLock`, but only "Finish transfers" resolves it, and a `waits=true` run keeps its lease while it waits. Consider a distinct wait reason, or parking, so the UI can point at "Finish transfers".
- **N-5** `import_stranded`: an unknown-index lock processed before a known-index lock can be handed to the row that owns the known one. The order is the library's map order. Funds are safe (the known lock then gets a new row), but process known-index locks first.
- **N-6** registration.rs `start`: a redeemed lease is dropped when the insert returns `InProgress`. That's harmless, because the lease ends unused, but it's worth a comment.
- **N-7** registration.rs:899. In `finish`, `driver.run().await?` stops the remaining rows on one row's storage error. Log the error and continue.
- **N-8** registration.rs:687. `clear_failure` ignores the update's result (a stale CAS is fine there). Make that explicit with `let _ =` and a comment.
- **N-9** Idle per-wallet actor threads live for the whole session. That's fine, but say so in the actor's doc.

## Confirmations

### 1. State machine

- **Kills at every transition:** the kill matrix (registration_tests.rs:677) passes, and every resume lands on the next idempotent step. MN-3 is the exception: the Fake hides the profile put.
- **Restore and adoption:** `Built` (or a non-zero index) is the only known index. That matches the pin: reconstruction maps registration locks to index 0, and recovery.rs:187-189 can rebuild `Broadcast` at index 0.
- **`index_owner`:** it sends an unfunded flow to a new index only for evidence another row owns, and stops at anything it can't attribute (mutants A and Q are killed; G, H and R survive, see MN-4).
- **Grants and leases:** a lease is taken once per run. `rebind_lease` and the offered lease handle double resume, so the second offer replaces the first, which is harmless.
- **Discard and finish:**
  - Both re-judge inside the actor job. That check is a race guard no test stages (mutant F survives, MN-4); the delete's CAS backs it up.
  - Delete is a CAS on owner, phase and outpoint.
  - Finish never waits for a proof and parks rows alone.
- **Rows this build can't run:** `unsupported` / `invitation.invalid` / `feature_off` are derived and never written. They count as committed whenever a lock, identity or outpoint may be theirs, and the tap and Finish leave such locks in place (DEC-173 (2); Q killed on the Finish side, while B, the tap-side guard, survives: MN-4).
- **Unload:**
  - `revoke_wallet` freezes, then drains the call permits, before `remove_wallet`. So `WalletNotFound` can't reach a keyed funding call.
  - Drain waits at most to each permit's deadline (grant + H, lease/lock.rs:238-275), so session close and unload during the IS wait are bounded, as P2b designed.
  - `forget_wallet` runs as an actor job.

### 2. Funding classification without P2b's signature count

- **NotSent:** `TransactionBroadcast` (the library untracks on `Rejected`), `AssetLockTransaction` (pre-build, signer failures included), `AssetLockInsufficientFunds`, `InputMidBroadcast` and `TransactionBuild` are all raised before anything reaches the wire at the pin. So `unfund` back to `KeysPrepared` is safe.
- **MaybeSent:** everything else, `TransactionBroadcastUnconfirmed` included.
- **A tracked lock wins over any error class.** At the pin no NotSent class arrives with a tracked lock, so this is defence in depth; mutant O, which makes NotSent win, survives (MN-4).

A MaybeSent row cannot pay twice:
- Its outpoint, or its `broadcast_unknown`, keeps `funding_status` `MaybeSent`.
- The driver's gates refuse to fund.
- `resumable` stays false until the lock is tracked (mutants D, L, M, T and Y killed).
- `index_owner` sends no other flow to its index while an unheld lock or the row exists.

It cannot lose funds:
- A swept untrack is a double-spend loser.
- A rejected lock never reached the wire.

The remaining risk is liveness, not funds: MN-1, and the swept-untrack row that stays `MaybeSent` until P2b's `DispatchResolved`.

### 3. Funding fence (DEC-179)

- The order is classify → record → `lock_tracked` → `release_built`.
- `PwLib::fenced` holds `coin_fence` from the protected check through selection (mutant K killed).
- The tap's `lock_tracked(built=true)` calls `release_built`, so the fence ends on `Built` only. Otherwise `FenceHeld` drops at the call's end (mutant C killed).
- Inputs of a tracked `Built`/`Broadcast` lock are a derived reservation (`lock_input`), with no stored registration owner.
- The N1 tests (`a_tapped_built_lock_frees_the_fence_before_its_call_returns`, `a_tapped_broadcast_lock_keeps_the_fence_until_its_call_returns`) are sound. They use index 1, so `built_lock` is attributed by known index.
- **DEC-183 (content-digest trust provenance):** it isn't in this base, and registration has no trust touchpoint.

### 4. Known residuals

| Residual | Verdict |
|---|---|
| Per-lock cap | Harmless in Phase A: in-progress gating allows one funding per wallet, and P2b's permits charge a host lease reused by id. |
| Fee bound recomputed when coins arrive | Harmless. Grant-exceeded is retryable. Overspend is bounded by ~149 duffs per extra input, because the inputs are read outside the fence and the fee is re-quoted inside it. |
| Double resume wasting a grant | Harmless: the second offer replaces the first, and the wasted lease ends unused. |
| Imported `KeysPrepared` row with an outpoint looping | Normalised to `FundingSent` (mutant Z killed). A dead outpoint then waits for ever: the same no-exit class as the swept untrack, so it's a P2b liveness residual and funds are safe. |
| Status flicker | Cosmetic. |
| Opus r1 L7 (the tap only logs a failed outpoint record) | Harmless at a known index, **not at index 0**: see MN-1. |

### 5. Tests

- Coverage is broad. The kill matrix, the tap/fence races, classification per error class, discard/finish under claims, and unknown-row handling are all covered.
- Gaps:
  - 9 contract rules with no test that catches a mutant of them (MN-4);
  - a non-Platform `NameError` (MJ-1);
  - an untapped index-0 funding (MN-1);
  - re-arming the backup after `unfund` (MN-2);
  - a non-idempotent profile put (MN-3).
- Of 25 targeted mutants, 13 are killed and 12 survive. Of the survivors, 9 are gaps and 3 are equivalent or defensive (table below).

### 6. Simplification

- The `funding_status` gate in `Driver::fund` duplicates what `index_owner` already guarantees (N-3).
- `in_progress`, `discard_refused` and `awaiting_a_lock` each restate "failed for good but still holding something". One `row_settled(row, ev)` predicate would put MJ-1's fix in one place.
- Classification (`fund_error` plus the 7-arm post-call match) is compact, and I'd leave it.
- If MN-1's snapshot approach is taken, the outpoint snapshot could replace part of the `in_call` preference in the tap.

## Mutation testing

Each mutant is an env-gated switch (`DP1_MUT=<x>`) in the review-only patch. Each was run as `bin registration --test-threads=3 --skip review_opus` against the 59 registration tests.

| Mutant | What it breaks | Result | Killed by |
|---|---|---|---|
| A | `known_index` always known | killed | an_unreadable_row_parks_a_lock_whose_index_was_lost, reconstructed_nonzero_index_does_not_fund_twice |
| B | tap ignores the unreadable-row guard | **survived** | MN-4 |
| C | skip `release_built` | killed | a_tapped_built_lock_frees_the_fence_before_its_call_returns, a_coin_lock_waits_for_the_funding_it_did_not_precede |
| D | `funding_status` drops the broadcast_unknown clause | killed | an_unbacked_funding_is_maybe_sent_until_its_lock_is_tracked |
| E | `enter_funding` ignores the outpoint | survived (race guard) | — |
| F | discard skips the in-job re-judgment | **survived** | MN-4 |
| G | `index_owner` drops `r.id < row.id` | **survived** | MN-4 |
| H | `create_identity` adopts any identity at the index | **survived** | MN-4 |
| I | Stale write maps to Moved | **survived** | MN-4 |
| J | Decide admit ignores `in_call` | killed | a_decision_takes_the_flow_between_calls |
| K | `fenced` skips the protected check | killed | a_coin_lock_waits_for_the_funding_it_did_not_precede |
| L | `maybe_sent` writes retryable | killed | an_unbacked_funding_is_maybe_sent_until_its_lock_is_tracked |
| M | `resumable` drops the broadcast_unknown clause | killed | keys_prepared_row_with_own_outpoint_is_recovered |
| N | import waiting includes in-call rows | **survived** | MN-4 |
| O | NotSent beats a tracked lock | **survived** | MN-4 |
| P | profile always known | killed | newer_profile_is_refused_before_funding |
| Q | `may_own_an_unrecorded_lock` false | killed | an_unreadable_row_parks_a_lock_whose_index_was_lost |
| R | `unheld` keeps locks of unreadable rows | **survived** | MN-4 |
| T | `clear_failure` skips `resumable` | killed | an_unbacked_funding_is_maybe_sent_until_its_lock_is_tracked |
| U | `funds_committed` ignores `in_call` | survived (masked by `flows.take`) | — |
| V | `enter_funding` ignores the owner | killed | a_run_that_lost_its_generation_stops_before_funding |
| W | tap ignores the in-call preference | **survived** | MN-4 |
| X | skip the driver's `funding_status` gate | survived (unreachable, N-3) | — |
| Y | `unfund` keeps the outpoint | killed | funding_failures_follow_the_tracked_row |
| Z | skip the KeysPrepared+outpoint normalization | killed | keys_prepared_row_with_own_outpoint_is_recovered |

Totals: 13 killed, 12 survived. 3 of the survivors are equivalent or defensive (E, U, X), and 9 are test gaps (MN-4). Every kill is a deterministic assertion, not a timeout, so no flake reruns were needed. The survivors were also run against `journal_tests recovery_tests runtime_tests session::tests coins::tests backup::tests names::tests` (109 tests; 109/0 unmutated) and survived there too. Logs are in /work/scratch/dp1-02-opus/mut/.

## Validation

All runs use `nice -n 10`, `CARGO_BUILD_JOBS=8`, `umask 022`, `TMPDIR=/work/scratch/dp1-02-opus/tmp` (0700) and their own target dir. Load was 120–136.

- **Baseline at 7e331dd, unmodified:**
  - `dw-engine` registration: 59 passed, 0 failed;
  - journal: 32/0;
  - `dw-appdb`: 59/0;
  - `m4_dashpay_contract`: 9/0.
- **With the review-only patch:**
  - registration (probes skipped): 59/0, so the patch doesn't disturb the suite;
  - the non-registration filters used for survivors: 109/0.
- **Probes:** MJ-1 fails as described, MN-1 fails as described, and the MN-1 control passes (probes.log).
- No testnet funds and no network registration were used.

**Verdict:** CHANGES-REQUIRED
