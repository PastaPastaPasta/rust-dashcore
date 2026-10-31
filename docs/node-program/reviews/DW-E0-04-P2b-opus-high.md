# DW E0-04.P2b — Opus high review (final, before merge)

Exact head `52215e3` (`dw/e0-04-p2b-mode-b`, 33 signed commits on desktop main `9b70543`), reviewed in the detached
worktree `/work/scratch/p2b-opus/wt`. Design: `docs/design/E0-04-grants-leases.md` §2a (with §2a.5's table and the
§2a.6 residuals) and `DASHPAY.md`'s Repair text. Prior rounds: P2b Sol r1–r5 and P2a Opus high. Nothing in the
source worktree was edited, committed or pushed, and no GitHub action was taken. This report is uncommitted.

Method: I read the whole status derivation, the call permit, the fence's call-step, signed-lock and self-spend
paths, Repair, the tap and the schema-5 journal myself. Three Opus sub-reviewers took crash consistency, the
migrations, and concurrency/tests/mutations in parallel, and I checked each of their claims I rely on against the
code. All the probes ran in throwaway copies of the worktree, since deleted; their sources are kept. The reviewed
worktree is clean.

## Verdict in one paragraph

The safety core holds. At this head I found no reachable path to `NotSent` (or to a second funding permit) for a
step whose funding could still reach the chain, within the pinned producer and the pinned DP1-02 caller. Sol r5's
argument is sound as far as it goes. The trouble is that it rests on one premise the code does not enforce:
**one lock per funding call.** A probe shows the premise failing silently to `NotSent` with a live second lock.
Two further problems are not safety holes at the pin but matter for this money path and for DP1-02:
- a routine wallet action (any payment, or CoinJoin's automatic pass) can permanently wedge Repair, the design's
  documented way out of a rejected-after-signing funding;
- an unavailable journal reports marked steps as `None` and `funds_committed = false`, so it fails open in the
  reporting API, against the design's own "unknown fails closed" rule.

All three fixes are small and local. **Verdict: CHANGES-REQUIRED**, with a narrow re-review of O-1, O-2 and O-3.
The minors should land in the same round where cheap: O-10's regression with O-8a, and O-9's assertion. The others
can be tracked as follow-ups or DP1-02 inputs.

## Findings

### Major

#### O-1 — The one-lock-per-call premise, which the only self-spend route to `NotSent` rests on, is documented but not enforced

**Where:**
- `platform/signers.rs:250`: `CountingFundingSigner::sign_ecdsa` signs any sighash while the permit runs.
- `platform/lease/call.rs:288`: `SignatureCount::begin` checks only `closed`.
- `platform/lease/fence.rs:3241` and `:3304`: `record_signed_locks` appends a second, distinct lock under the same
  marker without complaint.
- `platform/lease/mode_b.rs:296`: `shows_conflict` treats "every signed lock this process knows" as every lock.
- `mode_b_tests.rs:2250`: `each_lock_a_call_signed_is_recorded` treats two locks under one permit as a supported
  shape.
- Design `E0-04-grants-leases.md:712-715` states the premise and its failure mode in prose.

**Scenario (the code's behaviour, not the pinned producer's):**
1. One funding permit's signer signs lock L1. The tap records L1 durably.
2. The same signer signs lock L2. This takes a library rebuild/retry inside the call, a future pin, or a DP1-02
   caller that calls `create_funded_asset_lock_proof` twice with one permit.
3. L2's `signed_lock` write fails (`fail_signed_lock`), or the process dies between L2's signature and its record.
4. After a restart only L1 is known. Repair admits on L1 (`stuck_funding` needs a non-empty list) and records a
   self-spend covering L1 alone.
5. Once that spend is ChainLocked, the step reads `NotSent` while L2, possibly on the wire, can still confirm.
6. DP1-02 releases the draft on `NotSent` (`registration_flow.rs:938` at 6bcaa2f), and a second funding follows.

**Evidence:** probe `opus_probe_a_second_lock_under_one_permit_is_not_refused`
(`/work/scratch/p2b-opus/probes/opus_probe_mode_b.rs`, run log `probes/probe-run.log`) passes, which means the
hazard is real in code. It shows the signer is still open after the first recorded lock and the status ends
`Some(NotSent)`. Sol r5 verified that the pinned producer builds once
(`build.rs:853/962/278`, `asset_lock_builder.rs:548/417`) and that DP1-02's `fund` calls it once per permit
(`registration_flow.rs:2976`). Both are facts about other code, and both can move: platform pin bumps are routine
in this program, and DP1-02 is being redesigned now.

**Fix (small, crash-proof):** enforce it where the signature leaves.
- Once `record_signed_locks` has recorded a lock for a permit, mark that permit's `SignatureCount` as
  `lock_recorded`.
- `CountingFundingSigner::sign_ecdsa` then refuses (`SignerError::Locked`, or a dedicated error the permit maps to
  `MaybeSent`) any sighash not already in `signed`.

Because the tap runs after every input of the first lock is signed and before its broadcast, this refuses exactly
a second transaction's signatures. Re-signing the same sighash (an identical retry) stays allowed. The call ends
`MaybeSent`, which is fail-closed.

Turn `each_lock_a_call_signed_is_recorded` into a refusal test, and keep a mutation that drops the check. The
§2a.6 prose ("A funding call builds one lock at the pin …") then becomes a code fact.

A softer alternative, if a refusal is unwanted: make `stuck_funding` refuse (a new `SeveralLocks`) and
`shows_conflict` return false once a step has more than one signed lock. Note that this only detects the case
where both records landed, not the lost-second-record case above. The signer-side refusal is the one that closes
it.

#### O-2 — Once a signed lock's row is gone its inputs are free. Any payment or CoinJoin pass that spends them wedges Repair for good, though the lock is then provably dead.

**Where:**
- `coins.rs:390` (`pending_lock_inputs`) and `:589-591`: only tracked `Built`/`Broadcast` rows' inputs are held.
- `send` and `coinjoin.rs:438/596` select any coin that is not `reserved`.
- `repair.rs:152-157` refuses `LockInputsSpent`.
- Design `E0-04-grants-leases.md:839-842` says "A funding call rejected not-ready after signing reads `MaybeSent`;
  Repair's self-spend resolves it."

**Scenario (reachable at the pin, no crash needed):**
1. A funding call signs L and stores it `Built`, so the `signed_lock` row is durable. Its broadcast is
   definitively rejected; Sol r4's probe shows an SPV runtime that hasn't started yet does this after one
   signature. The pin's `Rejected` arm untracks the row and releases key-wallet's reservation (`build.rs:1143`).
2. The step reads `MaybeSent`. L's inputs are now ordinary coins: neither `lock_input` nor `reserved`.
3. The user makes any payment, or the CoinJoin automatic pass funds denominations from BIP44 coins. Either can
   select L's inputs, and the transaction is ChainLocked. L can now never confirm.
4. Repair: `conflict_locks` = [L]; none of L's inputs is spendable, so it refuses `LockInputsSpent` for good. With
   key-wallet's default build, ChainLocked records are dropped (`finalized_txids` only), so nothing can later show
   which transaction spent them.
5. The step reads `MaybeSent` permanently. DP1-02 can never discard, retry or release the draft, and the lease's
   `funds_committed` stays true. A sweep (`remove_tracked_asset_locks_for_swept`) or a restore that drops the row
   reaches the same state.

**Evidence:** probe `opus_probe_a_payment_can_take_a_signed_locks_inputs_and_wedge_repair`
(`probes/opus_probe_session.rs`, real session) passes. A plain `TxDraft` over the signed lock's input estimates Ok.
After that input is spent ChainLocked, `stuck_funding_draft` returns `LockInputsSpent(L)`, and the status stays
`MaybeSent`. The refusal text reads "tracked asset lock … spends no coin …" for a lock that is not tracked (see
O-5).

**Why major:** safety is intact, but the design names Repair as the way out of exactly this state, and ordinary
use (including an unattended mixer) silently removes it. The fix is the one P2b already applies to tracked rows
(F7). This is the "permanently stuck while a safe way out exists" case the brief asks about: the safe way out
exists until the inputs are spent, and nothing keeps them for it.

**Fix:**
- In `coin_snapshot`, treat the inputs of every signed lock of a step that reads neither `Sent` nor `NotSent` as
  `lock_input`. A new `LeaseTable` accessor, read like `pending_lock_inputs` (J is a leaf, and the fence → state →
  J order holds).
- They are then held from every payment, CoinJoin and sweep build, and exempt for Repair (`self_spend_selectable`
  already admits `lock_input`), until the step resolves.
- A regression: the probe above, with the payment refused `OutpointUnavailable` and Repair then succeeding.
- Update the §2a.6 residual list. Residual left after the fix: a second wallet instance with the same seed can
  still spend them; fail closed.

#### O-3 — With the journal unavailable, a marked funding step reads `None` and `funds_committed` false (fail-open reporting)

**Where:**
- `fence.rs:1720-1728`: `load_journal_with(None, …)` sets `Unavailable` and loads no markers.
- `mode_b.rs:514-523`: `CallStepStatus::None` with no matching row derives `None`.
- `mode_b.rs:687`: `funds_committed`.
- `mode_b.rs:1064`: `LeaseTable::funding_status`.
- `flows.rs:301`: `dispatch_status`.
- Contrast `mode_b.rs:499`: unknown rows derive `MaybeSent`, never `None`/`NotSent`.

**Scenario:**
1. A step is signed and reads `MaybeSent`, and its row is lost (the withheld-T1 trace).
2. At the next launch `dispatch.sqlite` cannot be opened. Causes: an I/O error, `NewerSchema` after a downgrade, or
   one corrupt row (the migration sub-review notes a single bad row closes the whole journal).
3. `funding_status` = `None`, `funds_committed` = false, and the lock report says `Cancelled`.
4. The engine's own permits are refused (`JournalUnavailable`), so P2b itself never pays twice.
5. But §2a.5 defines `None` as "never started, a second funding is allowed". DP1-02's ownership model, built on
   exactly these primitives, would read every step as unclaimed (`funding_steps_of` is empty), its rows as
   `Unfunded`, and allow discard. After the journal comes back, a fresh draft is a fresh step, which nothing
   blocks.

**Evidence:** crash sub-review probe `opus_crash_journal_unavailable_after_restart_reads_a_marked_step`
(`/work/scratch/p2b-opus/crash-probes/crash_probes.rs:110`, `run1.log`/`run2.log`) passes, asserting `None`,
`funds_committed` false and `JournalUnavailable` from the permit. I confirmed the production path
(`session.rs:411-413` → `load_journal_with(None …)`).

**Fix:** treat journal state like row state.
- While `i.fence.journal != Ready`, `derive_step` returns `Sent` if a row proves it, else `MaybeSent`. It never
  returns `None`/`NotSent`.
- `funds_committed` is true for every bound funding step.
- The step view proposed for DP1-02 (§5 below) carries `journal_ready` or refuses, so absence can't be read as
  "unclaimed".
- Add a test.

### Minor

**O-4 — Two unnamed crash windows strand a step with no record.** From the crash sub-review, F1; I confirmed both.
- **Window (a):** a kill after the marker is durable but before the first signature (`call.rs:544-560`), or
  before `end()` writes the zero-signature `NotSent`, leaves a standing marker with nothing else. It reads
  `MaybeSent` for good: the gate refuses, Repair refuses `NoKnownLock`, and there is no lock to find. Probe
  `opus_crash_real_sqlite_kill_after_marker_before_first_signature_strands_step` passes over real SQLite.
- **Window (b):** the same state arises if a catch-up resume broadcasts the in-memory `Built` row between
  `track_asset_lock` and `queue_asset_lock_changeset` (`build.rs:1097-1109` at the pin). That is before
  `note_signed` has made the record durable. The design's "the record is on disk before the lock can be on the
  wire" (§2a.6) overclaims by that window.
- **Effect:** both fail closed, so this is liveness only.
- **Fix:** name both windows in the §2a.6 residuals. The resume pass should skip a wallet's rows while a funding
  call of that wallet runs (`funding_call_runs`), which closes (b) outright and costs nothing.
- **Bearing on DP1-02's PA-2 (§5):** this matters for PA-2.

**O-5 — Repair's refusals are not user-actionable at the API.** This answers the brief's question: yes, an
explanation is needed.
- `repair.rs:69-76` flattens every `StuckFundingRefusal` to `EngineError::InvalidArgument(String)`. The host can't
  tell:
  - "wait for a ChainLock" (`SpendUnsettled`);
  - "unlock this coin" (`outpoint_unavailable`);
  - "finish the other registration first" (`UnattributedLock`);
  - "this can't be cancelled; it resolves only if the lock appears" (`NoKnownLock`, `LockInputsSpent`).
- The `LockInputsSpent` text says "tracked asset lock … spends no coin" for a signed lock whose row is gone
  (`mode_b.rs:745`; seen in the O-2 probe).
- **Fix:** keep the enum through to the facade: a reason code per variant, for DP1-02's Repair facade. Reword
  `LockInputsSpent` as "asset lock … : every coin it spends is already spent". The copy for the two permanent cases
  should say:
  - nothing else is blocked;
  - the user's other funds are free;
  - a new registration can be started;
  - the draft resolves itself if the lock ever appears (DP1-05).

**O-6 — An orphan self-spend keeps `awaits_self_spend_lock` true for good** (crash sub-review F3, confirmed).
- **Where:** `mode_b.rs:1015-1025`, `journal.rs:375`, `mode_b.rs:946`.
- **Scenario:** a recorded self-spend that is never broadcast, rejected, or superseded by a second Repair that did
  ChainLock.
- **Effect:** every Core changeset (about one per block) spawns an uncoalesced `refresh_funding`, for the life of
  the wallet. The marker never compacts (D4), so this survives restarts.
- **Evidence:** probe `opus_crash_orphan_self_spend_is_awaited_after_the_step_resolved`.
- **Fix:** await only self-spends of steps not yet `NotSent`/`Sent`, and coalesce refresh requests per wallet.

**O-7 — `Lease::funding_signer()` is `pub` and uncounted** (`table.rs:904`; crash sub-review N5).
- **Hazard:** used for a Mode B funding call instead of `CallPermit::funding_signer`, it releases signatures the
  permit never sees. A failing call then ends `Cancelled` → `NotSent` row → a second funding.
- **Today:** DP1-02 uses it only for `FromExistingAssetLock` (`registration_flow.rs:3030`), which builds no lock.
- **Fix:** rename it `pub(crate) uncounted_funding_signer`, or make it refuse while a funding call permit of the
  lease runs. This is the same class as O-1: a premise held only by documentation.

**O-8 — Journal-layer defence in depth** (migration sub-review; confirmed by reading `dispatch.rs`).
- **(a)** `insert_self_spend` accepts provenance 1 with empty conflicts (`dispatch.rs:894-925`, probe P5), while
  `insert_signed_lock` refuses empty inputs (`:941`). The engine never writes one, and `shows_conflict` rejects one.
  Refuse it at the journal too.
- **(b)** Schema 5's meaning changed at 52215e3 without a version bump. The 56ed271 dev build would open a later
  file as Ready and bring back R4-01's fallback. Dev builds only, since no shipped build has schema ≥ 3. Bump to 6,
  or record "never open a later file with 56ed271".
- **(c)** Open runs migration and sweep in a deferred transaction (`:731`). Use `Immediate`; the single-instance
  lock makes this mostly moot.

**O-9 — The marker supersede silently drops signed locks** (`fence.rs:1051-1059`).
- At this head that can't matter. Only a zero-signature call writes `NotSent` (`call.rs:741`), and a signed lock
  needs a released signature, so a superseded step never has one (both sub-reviews agree).
- But if it ever did, it would be exactly an unsafe `NotSent`.
- **Fix:** `debug_assert!` (or refuse the supersede and keep the marker) when the step has a signed lock or a
  recorded self-spend. That turns another prose invariant into code. It also makes DP1-02's PA-3 vacuous (§5).

**O-10 — The guard that closed R4-01's empty-record route has no test.** The guard is `shows_conflict`'s
`!conflicts.is_empty()` (`mode_b.rs:300`).
- **Evidence:** the mutation sub-review dropped it (mutant M1), and all 199 lease tests still pass. The existing
  empty-record tests (`mode_b_tests.rs:2555`, `:2679`) also carry a signed lock, so `all()` fails anyway.
- **Scenario:** a provenance-1, empty-conflicts row (what the r3 dev build wrote; the journal still accepts one,
  O-8a), under a step with no signed lock, ChainLocked. Without the guard, `signed.iter().all(..)` is vacuously
  true, so the step reads `NotSent`.
- **Probe:** I re-created the probe as
  `opus_probe_an_empty_recorded_self_spend_without_a_signed_lock_never_reads_not_sent`
  (`probes/opus_probe_empty_record.rs`). It passes at the head (`probe-run2.log`) and fails with
  `Some(NotSent)` when line 300 is mutated (`probe-M1.log`).
- **Fix:** commit it as a regression together with O-8a. The code is correct today.

**O-11 — Entry points that bypass the funding call's vouching and fence.** Mutation and concurrency sub-review;
confirmed.
- `Lease::call_permit_step` is `pub` (`call.rs:456`) in a `pub mod`. It accepts `CallStep::Funding` with any key
  and no coin fence, skipping M-3/M-4's draft-key check and §2a.6's fence, which only `funding_call_permit`
  applies. Only tests use it with a funding step.
- It is the same class as O-7. Make it `pub(crate)`, or refuse `CallStep::Funding` there.
- Separately, `StepKeyMismatch` is checked before the funding gate (`call.rs:489` vs `:493`). A step whose
  possibly-sent marker stands under another key gets `internal` rather than `broadcast_unknown{step}`. That fails
  safe but isn't actionable; run the gate first for funding steps.

**O-12 — `note_tracked` releases a funding call's coin fence on a key match, not on provenance**
(`mode_b.rs:1088-1115`; mutation and concurrency sub-review).
- **Scenario:** a row under the call's key that wasn't in the marking snapshot is stored after the call's first
  signature, e.g. a reconstruction at index 0. That releases the fence before the call's own lock is tracked, so a
  "Lock unspent" or a `TxDraft` can read a coin the call already signed.
- **Effect:** a conflicting spend, not a double payment. `mode_b_tests.rs:924-966` codifies the current behaviour.
- **Fix:** release on the txid `record_signed_locks` has just attributed to the call through `signed_all`. It runs
  earlier in the same `WalletStore::store`, so the match is exact.

**O-13 — Smaller concurrency and mapping points** (mutation and concurrency sub-review; I spot-checked each).
- **(a)** A wallet's first `coinjoin_status` blocks its synchronous FFI caller on the coin fence through a funding
  call's signing (`coinjoin.rs:967-976`). Use `try_coin_fence` with a "computing" status.
- **(b)** Transient Repair refusals (`RowsUnknown`, `CallRunning`, `SpendUnsettled`) map to `invalid_argument`
  (`repair.rs:69-76`). They should read retry-later (`unavailable`). This is part of O-5's fix.
- **(c)** `funding_coin_count` (`coins.rs:473`, new in P2b) uses `wallet.state()`, which panics on a wallet whose
  info is gone (a removal during `funding_call_permit`). `SessionTracked` already avoids `state()` for this reason
  (`mode_b.rs:1230`).

### Nits

- **N-1:** design `:827` "a dropped permit leaves its step `MaybeSent`" holds only for a permit that released a
  signature. A zero-signature drop ends `Cancelled`/`NotSent` (`call.rs:741`).
- **N-2:** Repair can be admitted before SPV reaches the tip after a restart. A stale coin snapshot builds a
  self-spend over an input a confirmed lock already spent; it never confirms, then blocks the next Repair with
  `SpendUnsettled` until abandoned. Safe; consider gating on sync.
- **N-3:** Repair-resolved steps never compact (D4: no `NotSent` row), so lock and self-spend txids stay in
  `dispatch.sqlite` for good, including after the wallet's removal. That is a growth and privacy cost; note it.
- **N-4:** the models (`e0_04_design_model.py` Part 5) model no `signed_lock`, `NoKnownLock`, supersede,
  compaction or two-database skew. Part 5 still claims "transport not ready → NotSent", which §2a.6 says is not
  implemented (M-2). The stress run drives no Mode B call, and `CallBeforeMarker` is its only Mode B mutation.
  Worth adding:
  - swap record and broadcast in `repair.rs`;
  - move `note_signed` after `inner.store`;
  - ignore `prior_marker` at `call.rs:741`.
- **N-5:** adopt the sub-reviews' real-SQLite probes as tests
  (`crash-probes/crash_probes.rs` probes 1–3, `mig-probes/probes.rs` P1–P5). §2a.6 itself lists "no engine test
  over a real SQLite journal" as a residual; these close it.
- **N-6:** two more mutants survive.
  - **M11** (`end()` drops `!started`): the surviving direction is conservative (`MaybeSent`, no refund). Add a
    test for a permit refused after its marker write under a prior marker.
  - **M12** (`SignatureCount::close` reads `released` before `signing`): a real lost-signature race that only a
    race would show. Pin the order with a comment naming it, or with a loom or step-hook test.
- **N-7:** `register_name_for_flow` carries an unconditional `#[expect(dead_code)]` (`names.rs:1018`), so it has
  no test even under `cfg(test)`; DP1-02 removes it.
- **N-8:** the `resolve_stuck_funding` doc (`repair.rs:85-94`) and the module doc's "When" section don't mention
  `NoKnownLock`/`MissesSignedLock`, the post-R4-01 prerequisite. The sentence "Refused … unless … while …" is
  garbled.
- **N-9:** `m3-engine.md:111` lists `coinjoin.broadcast_unknown{reason}`, but the sweep result carries
  `unknown_txid` and no reason.
- No `recovery_complete` remnants remain anywhere in the code or design (verified).

## 1. The status derivation: every route to `NotSent` (and to `None`)

`derive_funding` (`mode_b.rs:490`) is the only producer. `funding_status`, `funding_outcome`, `funds_committed`,
the gate, Repair and `rederive` all go through `derive_step` (`:596`) on the current snapshot and fence. The
routes:

| # | Route | Requires | Evidence of every signed lock? |
|---|---|---|---|
| N1 | `self_spend_locked && !unattributed` (`:502`) | No step `Sent` and no proved matching row; rows known; a self-spend of the standing marker ChainLocked whose record `shows_conflict` (`:296`): provenance 1, non-empty conflicts, and each signed lock the process knows named with an actual input of it | **Yes, for the known locks.** For locks the process no longer knows, only through the one-lock premise (O-1). Legacy and empty-conflict rows never count (`:297-300`) |
| N2 | `status == NotSent`, no pending/proved row, no unattributed row (`:521`) | A durable `NotSent` row superseded the marker: written only by `resolve_call_not_sent` after `call_step_end` saw every call end `Cancelled`, i.e. `close()` = signing + released = 0 and no prior marker (`call.rs:736-745`, `fence.rs:3002-3047`) | **Vacuously:** zero signatures means no signed lock, so there is nothing to cover. Supersede then drops the step's (empty) signed locks and self-spends (O-9 makes that an assertion) |
| N3 | `funding_outcome`'s report-only `Cancelled` (`:670-683`) | `call_ended_unsent`: the same condition as N2, before the row lands | Same as N2; the lock report only, never a gate |
| Z1 | `None` (§2a.5: "a second funding is allowed") | No marker loaded, no matching row, no unattributed registration row | No evidence at all. Reachable with a deleted or rolled-back journal and a lost row (documented in §2a.5/§2a.6, crash N4), **and with an unavailable journal (O-3, not documented)** |

What I checked holds:
- `Sent` and proved rows come first (`:496`).
- Unknown rows never give `None`/`NotSent` (`:499`).
- A stale snapshot can only be stale toward "not yet ChainLocked" or "row not yet seen". A ChainLock is final; a
  late unattributed row retracts `NotSent` to `MaybeSent`, which is documented and fails closed at the gate.
- The gate (`:697`) checks the standing marker independently of the derived status, so a step `NotSent` through
  N1 still refuses a second permit.
- `call_key_differs` (`fence.rs:1026`) stops a Resumable permit from slipping past the gate under a funding
  step's marker.
- `funding_call_permit` refreshes before gating (`:1356`).
- The `durable` flag matters only for restart. After a restart only durable records remain, which is exactly the
  "forgotten lock" case N1's one-lock premise covers.

**Is the pin assumption enforced in code?** No (O-1). The only code-level guard is that the gate admits one call
per standing marker. Nothing bounds the number of locks that one call's signer can sign.

## 2. Crash consistency

The crash sub-review walked every pair of durable writes:
- the marker (`step_log` + `funding_marker`);
- `signed_lock`;
- the wallet's tracked row (a separate database);
- the broadcast;
- `self_spend`;
- the `NotSent` row and the DEC-163 `Sent` bundle;
- the open-time sweep.

It also covered failed, cancelled, panicking and "durable then error" writes, wallet close/reopen, wallet removal,
compaction and step-id reuse. Its five probes run over a real `DispatchJournal` and its sweep.

Every reading after a restart is conservative. Here is why:
- The marker is durable before the permit is handed out (`call.rs:547-561`), and `CallBeforeMarker` is caught (I
  re-ran it: 1 failed, as expected).
- `note_signed` runs inside `WalletStore::store` before `inner.store` (`store.rs:104`), so no persisted row
  precedes its record.
- Repair records before it broadcasts (`repair.rs:201-218`). A failed record abandons the draft.
- A `NotSent` row supersedes only after it has landed. Otherwise the marker stays `Ambiguous` and the step reads
  `MaybeSent`.
- Compaction requires no standing marker per artifact.
- The `Sent` bundle is one IMMEDIATE transaction.

**Interleavings the models and tests don't cover** (all fail closed): O-4's two windows, O-3's unavailable journal,
and O-6's orphan self-spend. A second process is out of the engine's scope: it relies on the host's single-instance
lock. Swift's host must hold one too (crash N3).

## 3. Schema 5 migration

The migration sub-review read `dispatch.rs` and ran six probes against real SQLite (31/0) plus the three engine
migration tests (3/0).
- **Paths and atomicity:** every path 1→5, 2→5, 3→5 and 4→5 only adds. Migration and the sweep share one
  transaction, and a failure at 3→4 or 4→5 leaves the old schema and its rows untouched.
- **Legacy rows:** the 4→5 step adds `provenance DEFAULT 0` and `conflicts DEFAULT x''`. Probe P1 checks the raw
  values and that the marker, `funding_marker` and `signed_lock` counts are unchanged. **Nothing is fabricated.**
- **Refusals and decoding:** `NewerSchema`, and a missing or non-positive version, are refused without writes.
  Malformed provenance or conflict lengths are refused at decode (`Corrupt`).
- **Re-recording:** re-recording a legacy txid keeps the legacy row (no promotion).
- **Downgrade:** main at schema 2 reports `NewerSchema` and fails closed (but see O-3 for what that reports).

Its findings are O-8 above, plus test gaps (N-5).

## 4. Repair

- **`MissesSignedLock`** (`mode_b.rs:797-814`, `fence.rs:3148`): correct. It is computed from the inputs of the
  exact signed bytes that will be broadcast, with the txid checked against the prepared summary
  (`repair.rs:186-198`), and re-checked in the recording J step.
- **`NoKnownLock`** (`mode_b.rs:854`): necessary in general. Without a recorded lock there is no evidence any
  self-spend conflicts with it, and R4-01's 0-conf-parent argument rules out chain scans. **No safe automated way
  out exists**, so the permanent `MaybeSent` is the honest state.
  - It reaches users through O-4's windows, a failed `signed_lock` write plus a restart, or a pre-schema-4 marker.
  - What's missing is the product decision, not P2b mechanism. Either DP1-02's user "Release" (accepting the
    risk), or PA-2 (§5).
  - Without either, a stranded draft is permanent. The user's coins are not stranded: nothing reserves them.
- **`LockInputsSpent`:**
  - The legacy-row residual the brief names can't reach users: no shipped build has a `self_spend` table
    (main is schema 2).
  - The same refusal is reachable on the normal path through O-2. That case *has* a safe way out (the lock is
    dead), which the engine throws away by not holding the inputs. Fix O-2. The user-visible text is O-5.
- **Other refusals** fail closed and are recoverable by user action: `SpendUnsettled` (wait, or abandon),
  `outpoint_unavailable` (unlock), and `UnattributedLock` (Finish registration / DP1-05). O-5 is about making that
  action visible.

## 5. API surface for DP1-02's ownership model

DP1-02's three values (signed T / unknown with a marker / unclaimed) are derivable from today's primitives:
- `funding_steps_of` gives the steps whose marker or `Sent` stands.
- `signed_lock_txids` gives T.
- `funding_status` distinguishes "resolved `NotSent` with the marker still standing" (N1, which DP1-02 must read as
  *Dead*, not *Exact*) from the rest.

There are three gaps:
1. **Unavailable journal** (O-3): absence must not read as *unclaimed*.
2. **Consistency:** DP1-02 calls `signed_lock_txids` then `funding_status` in separate J steps
   (`registration_flow.rs:932-938`), so a refresh or a NotSent landing in between can pair a stale T with a new
   status.
3. **Durability:** `signed_lock_txids` includes undurable in-memory txids that vanish on restart, turning *Exact*
   into *Unknown*. DP1-02 can't see that today. It should, at least for its tests and for the R8-01 reasoning.

**Should `FenceState::funding_steps_of` become crate-visible? Recommendation: yes to step-keyed, crate-visible
access; no to exposing that `FenceState` method itself.**
- It is a method on J-internal state. Outside the lease module nobody can hold `&Inner`, so making it `pub(crate)`
  gives DP1-02 nothing it could call without a new `LeaseTable` wrapper anyway.
- A bare list of step names forces N follow-up J steps, which is gap 2's race.
- Add DP1-02's PA-1 as one `pub(crate) LeaseTable::registration_funding_steps(wallet) -> Result<Vec<FundingStepView>,
  JournalUnavailable>` (or with a `journal_ready` field), read in one J step. Per step:
  - the parsed draft;
  - the marker's `FundingKey`;
  - the derived status;
  - the signed txids, each with `durable`;
  - whether a call or a `NotSent` write is in flight (`call_busy`).
- It is read-only and adds no schema.
- Keying ownership by step is the right shape: orphan steps after a row's deletion or restore are a supported
  state, and the fence already keys everything by step.

**On DP1-02's other asks** (from `DP1-02-ownership-model.md` §9, not in this head's scope, but they would change
P2b's derivation):
- **PA-3 is vacuous at this head.** A superseded step never has a signed lock (route N2), so there are no "dead"
  txids to keep. O-9's assertion makes that a code fact instead.
- **PA-2** ("no broadcast before a durable signed record", then `NotSent` for a marked step with no record and no
  running call) **is not sound as stated.** It flips three things from fail-closed to fail-open:
  1. **A missed provenance match.** `StoredLock::of` recognises only exact P2PKH `<sig+ALL> <key>` scriptSigs.
     Today an unrecognised lock is merely unrecorded (`NoKnownLock`). Under PA-2 it would be `NotSent`. PA-2 would
     need the tap to durably record *something* for every asset-lock store during a signed funding call that it
     cannot attribute.
  2. **O-4(b)'s resume window.** Blocking `note_signed` doesn't stop a concurrent resume broadcasting the
     in-memory `Built` row first. PA-2 needs the resume pass fenced off while a funding call runs.
  3. **The journal-not-Ready path** (`fence.rs:3288-3293`), which today records in memory only. PA-2 must block or
     fail there too.
  - Even with all three, PA-2 rests on the library's store-before-broadcast ordering as a safety premise rather
    than a liveness one. It needs its own Opus review. DP1-02's "user Release" fallback is the lower-risk option at
    this pin.
- **Coin reservations.** If DP1-02 ever reserves a lock's inputs with a `SpendOwner` registration variant until
  `NotSent`, Repair can't take them (`self_spend_selectable` exempts only `lock_input`, not engine reservations).
  That deadlocks the only route to `NotSent`. O-2's fix (signed locks' inputs as `lock_input`) is the right
  mechanism, not a DP1-02 reservation.

## 6. Concurrency, error mapping, tests, dead code

**Concurrency (verified, no finding beyond O-12/O-13):**
- Every `with_j` closure is synchronous, and `wait_until` drops J before awaiting.
- Coin fences move only inside J and are dropped after it (`drop_later`).
- Journal writes all run outside J: `spawn_write`, `resolve_call_not_sent`, `record_self_spend` and
  `record_signed_locks`.
- The order fence → wallet state/manager → J holds at every site checked. Nothing takes a fence inside the tap,
  the refresh, resume or the fee bound, and the CoinJoin refresher only uses `try_coin_fence`.
- The tap's blocking `insert_signed_lock` can't deadlock: J is released around it, it runs before `inner.store`,
  and `dispatch.sqlite` is a separate database.
- The gate, the charge, the `Committing` marker and `note_marked` are one J step, so there is no gate/marker
  TOCTOU.
- `record_self_spend` re-checks `stuck_funding` in J, and the journal re-checks that the marker stands. A funding
  call of the same step can't start under the standing marker, and `call_busy` covers running and resolving calls.
- The `SignatureCount` begin/close protocol (SeqCst, counted before release) is sound. M7 kills its `closed`
  check; M12 is N-6.

**Error mapping:**
- `MaybeSent` → `broadcast_unknown{step}`, `WillBeSent` → `will_be_sent`, `RowsUnknown` → `unavailable`, and
  `JournalUnavailable` → `storage`. No possibly-sent state maps to a retryable code.
- An unknown sweep chunk keeps its claim and reports `unknown_txid`.
- The gaps are O-5/O-13b (Repair) and O-11 (`StepKeyMismatch` first).

**Mutation table** (mutation sub-review, 199 lease tests, `--skip stress_tests`, baseline green). 18 of 21 guards
are killed:

| Guard (mutation) | Killed by | Killed |
|---|---|---|
| `shows_conflict`: `all` → `any` (M2) | `a_self_spend_shows_the_conflict_only_over_every_signed_lock` | yes |
| `shows_conflict`: txid only, any input (M21) | `a_self_spend_without_its_conflict_evidence_never_reads_not_sent` | yes |
| `shows_conflict`: drop the non-empty check (M1) | none in the suite; this review's probe (O-10) | **no** |
| Legacy self-spend counts (M3) | `review_r4_legacy…`, `review_r3_schema3…`, `a_legacy_step_resolves…` | yes |
| Gate: drop the standing-marker check (M4) | `an_unsent_or_never_started_funding_allows_funding_again` | yes |
| Self-spend `NotSent` before `Sent` (M5) | `row_1_a_chain_locked_self_spend_reads_not_sent` | yes |
| `end()`: ignore `prior_marker` (M6) | `cancelled_supersedes_the_marker_and_maybe_sent_keeps_it` | yes |
| `end()`: drop `!started` (M11) | none (conservative direction, N-6) | no |
| `begin()`: sign after close (M7) | `the_count_closes_at_the_permits_end`, `mode_b_a_signer_that_outlives_its_permit_signs_nothing` | yes |
| `close()`: read order (M12) | none (race-only, N-6) | no |
| `call_key_differs` → false (M8) | `a_call_step_under_another_funding_key_is_refused` | yes |
| `stuck_funding`: drop `NoKnownLock` (M9) | `a_recorded_self_spend_resolves_only_once_chain_locked` and others | yes |
| `stuck_funding`: drop `CallRunning` (M17) | `repair_refuses_a_step_that_is_not_stuck` | yes |
| `record_self_spend`: drop the in-J re-check (M22) | `a_recorded_self_spend_resolves_only_once_chain_locked` (+3) | yes |
| `self_spend_conflicts`: any input (M14) | `a_self_spend_records_the_input_it_takes_of_each_signed_lock` | yes |
| Unattributed row ignored for the self-spend (M10) | `an_unattributed_row_blocks_the_self_spends_not_sent_and_repair` | yes |
| `markerless_unattributed_row` → none (M18) | `a_markerless_registration_step_reads_maybe_sent_over_an_unattributed_row` | yes |
| `note_tracked` releases before any signature (M13) | `a_funding_calls_fence_is_released_by_its_own_tracked_row_only` | yes |
| Gate: drop the unresumed-row block, F7 (M15) | `a_built_or_broadcast_row_blocks_funding_until_it_is_resumed` | yes |
| `record_signed_locks` without `signed_all` (M16) | `a_lock_is_the_calls_only_when_every_input_carries_its_signature` | yes |
| `signed_all` accepts an input-less lock (M19) | same as M16 | yes |

O-1's enforcement has no mutation to run, because there is nothing to mutate yet.

**Dead code:**
- The `#[cfg_attr(not(test), expect(dead_code))]` items (`signed_lock_txids`, `fenced_coins`,
  `held_funding_coins`, `check_temporary_name`) are all tested. They are justified by DP1-02.
- `register_name_for_flow`'s unconditional expect is N-7.
- The test-only seams (`commit_snapshot`, `call_holds_fence`, `resumed`, `note_released`, `note_signed`) are
  `#[cfg(test)]`.

## Validation

Every run used `nice -n 10`, umask 022, `CARGO_BUILD_JOBS=12`, disk `TMPDIR=/work/scratch/p2b-opus/tmp` (0700),
and a review-owned target (`/work/scratch/p2b-opus/target`) built fresh at this head.
- **Build:** `cargo test --locked -p dw-engine -p dw-appdb --lib --no-run` gave BUILD EXIT 0 (`build.log`).
- **`platform::lease` (all lease, mode_b, call, resume, BL-76, session and stress tests at default seeds):**
  **202 passed / 0 failed**. The first run passed 175 and failed all 27 session tests with `Storage("database
  ancestor /work/scratch/p2b-opus is group/other-writable (mode 0775)")`. That was my scratch directory's mode,
  the known umask-002 trap, not the code. After `chmod go-w` the 27 passed (`lease.log`,
  `lease-session-rerun.log`).
- **`dw-appdb` dispatch:** **25/0** (`appdb-dispatch.log`).
- **`DW_LEASE_STRESS_SEEDS=10` stress:** **3/0** (`stress10.log`).
- **`DW_LEASE_MUTATION=CallBeforeMarker`:** caught (1 failed, as required; `cbm.log`).
- **Opus probes:** **3/0** (`probes/probe-run.log`, `probes/probe-run2.log`). The O-1 and O-2 probes pass by
  demonstrating the hazard. The O-10 probe passes at the head and fails under its mutation (`probes/probe-M1.log`).
  Sources are in `probes/*.rs`, appended to the copy `probe-wt`.
- **Crash sub-review probes:** **5/0** over real SQLite (`crash-probes/run1.log`, `run2.log`).
- **Migration sub-review probes:** **31/0** (dw-appdb) and **3/0** (engine migration tests)
  (`mig-probes/run1.log`, `run-engine.log`).
- **Mutation sub-review:** 21 mutants over 199 lease tests; 18 killed, 3 survive (M1, M11, M12; see §6).
  Logs are `mutations.log` and `mut-*.out`.

Not run here: clippy, the BL-76 dashd regtest, the Python models, the full workspace or Swift. The implementer's
gate (GATE EXIT 0, 995/0) covers those; I inspected the gate script, not its execution.

Every review target and worktree copy is deleted: `target`, `probe-target`, `crash-target`, `mut-target`,
`mig-target`, `probe-wt`, `crash-wt`, `mut-wt` and `mig-wt`. The probe sources stay in `probes/`, `crash-probes/`
and `mig-probes/`, and the logs stay in `/work/scratch/p2b-opus/`. The detached worktree `wt` is clean at
`52215e39c02c6cf78c6ed6adb9ba6b33aa95bfcf`. No process was killed by pattern, and nothing was committed or pushed.

**Verdict:** CHANGES-REQUIRED
