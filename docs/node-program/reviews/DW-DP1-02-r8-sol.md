**DW-DP1-02 Phase A — Sol r8**

Reviewed exact head **6bcaa2f8eaec676587ea899bb89953aa036e1711**, detached at `/work/scratch/dp1-02-r8-sol/wt`, against P2b **5a1e3abbddcc46c37b21ac38c65e6b230549ef01**. The branch contains 19 signed DP1-02 commits. I consulted the prior Sol r1–r7 and Opus-high reports, traced the new ownership rule and its consumers, independently exercised restart/restore interleavings, and ran three decision-site mutations. No changes were made to the implementation worktree or detached review worktree.

**Changes are required.** The two reported r7 paths close when signed provenance survives, but the unlanded-record gap is a real consumer-side money-safety bug: a durable `MaybeSent` funding marker does not prevent DP1-02 from deleting the row and permitting a replacement funding. There is also a rule-4 escape through legacy settled-row discard, and a pending-count omission during a funding hold.

**R8-01 — Blocker: losing the signed-lock record loses DP1-02's funding protection even though the durable marker survives.**

Locations: [skipping rows with no recorded txid](/work/scratch/dp1-02-r8-sol/wt/rust/crates/dw-engine/src/platform/registration_flow.rs:933), [reservation requires a recorded txid](/work/scratch/dp1-02-r8-sol/wt/rust/crates/dw-engine/src/platform/registration_flow.rs:956), [DP1 funding status ultimately returns None](/work/scratch/dp1-02-r8-sol/wt/rust/crates/dw-engine/src/platform/registration_flow.rs:1336), [discard gate](/work/scratch/dp1-02-r8-sol/wt/rust/crates/dw-engine/src/platform/registration.rs:823), [Finish's waiting-row assignment](/work/scratch/dp1-02-r8-sol/wt/rust/crates/dw-engine/src/platform/registration_flow.rs:1606).

The failure is not in `signed_lock_txids`: that accessor correctly makes no promise that an unlanded record survives restart. The failure is treating its empty result as absence of funding uncertainty. `Provenance::read` does not even read the step's status in that case. Consequently, an outpoint-less, unfailed `KeysPrepared` row can read unfunded despite a FULL standing funding marker.

Independent reproduction uses the actual appdb and dispatch SQLite journal. A trigger rejects only `signed_lock` INSERTs; the funding marker is allowed to land. The committed three-lock-write-failure fixture then leaves A at `KeysPrepared`, index 0, with no outpoint and no error after its signed Broadcast funding. Before close, the accessor returns T1; SQL contains zero signed rows. I remove the trigger and perform an actual network close/reopen. No signed txid is reloaded, but P2b reads **Some(MaybeSent)**. The restart's library snapshot contains no T1, matching the supported lost-wallet-row state.

```text
P2b funding_status(A): Some(MaybeSent)
DP1 RegistrationStatus(A).funds_committed: false
A: KeysPrepared, index=0, outpoint=None, error=None
discard_registration(A): Ok(())
start_registration("replace2"): accepted
old/new funding-call counts: 1 / 1
replacement row: Done
```

A second probe includes a separately discovered same-seed identity at index 1 so the Fake's deterministic builder produces different outpoints: original **01…01:0**, replacement **03…03:0**, new row at index 2. The real funding permits admit both calls across the restart. This is an offline trait-boundary proof of the engine admitting a replacement funding while the original remains possibly sent; it is not a live demonstration of two mined transactions.

P2b does correctly veto a second call of the **same** row/step: the positive control produces zero new funding calls and `platform.broadcast_unknown`. That does not save discard → new draft, because the new draft has a different funding step. This sequence needs no `NotSent`, legacy self-spend, recovery-complete assertion, or weakening of P2b's rules.

The same gap defeats adoption protection. After the actual restart, Finish gives unsigned T2 to A and completes it. When T1 returns proved, Finish gives T1 to new row B, with zero pending; A holds T2. This is the r7 substitution class with the exact reservation lost on restart. The original signer is no longer distinguished from a row that never called funding.

Evidence: [first eight probes](/work/scratch/dp1-02-r8-sol/r8-probes.log), [final ten probes](/work/scratch/dp1-02-r8-sol/r8-final-probes.log), [isolated discard reproduction](/work/scratch/dp1-02-r8-sol/unlanded-discard-alone.log), [isolated distinct replacement](/work/scratch/dp1-02-r8-sol/distinct-replacement-alone.log), [isolated Finish reproduction](/work/scratch/dp1-02-r8-sol/unlanded-finish-alone.log).

Required fix: make the common ownership/evidence model read outstanding funding uncertainty even when the exact txid is unavailable. A marked step that may still be out must keep its row committed, reserved and at its funding key, and must prevent substitution and delete-and-replace until positive resolution. Preserve the distinction between **unclaimed** and **ownership unknown after a marked call**. Do not fix only `Driver::fund` or rely solely on the permit's same-step veto. A returning unattributed lock also needs conservative handling while that ownership uncertainty stands; simply reserving A while allowing a fresh row to take its possible T1 does not implement rule 1. Retain both failed-write/restart regressions.

**R8-02 — Major: the legacy settled-row discard shortcut erases known ownership and defeats rule 4.**

Locations: [settled legacy failure definition](/work/scratch/dp1-02-r8-sol/wt/rust/crates/dw-engine/src/platform/registration.rs:443), [discard returns before checking committed funds](/work/scratch/dp1-02-r8-sol/wt/rust/crates/dw-engine/src/platform/registration.rs:820), [provenance is loaded only for surviving appdb rows](/work/scratch/dp1-02-r8-sol/wt/rust/crates/dw-engine/src/platform/registration_flow.rs:930), [unknown owner defaults to Unclaimed](/work/scratch/dp1-02-r8-sol/wt/rust/crates/dw-engine/src/platform/registration_flow.rs:950).

Rule 4 explicitly handles a row that an earlier build gave T2 while its own signed T1 was away. Its ordinary Done-row case is now protected: the review control refuses discard and successive Finish calls keep T1 pending. But an earlier build can instead have stopped that same row with a non-retryable name/profile error after creating T2's identity. That is exactly the legacy category `settled(row)` allows the user to discard.

The probe uses the same inconsistent-row injection as the committed rule-4 test, with A holding T2, T2's identity, `NameRequested`, and non-retryable `name.taken`. Its T1 signature record remains known. With proved, unspent T1 present:

```text
Finish before discard: resumed=0, completed=0, still_pending=1
legacy settled A discard: Ok(())
Finish after discard: resumed=1, completed=1, still_pending=0
T1 holder: new row B
```

Deleting A does not delete the dispatch signature record, but `Provenance::read` no longer queries A's step. Known `Signed(A)` therefore becomes `Unclaimed`, and `may_take(None, T1)` permits the new row. This violates rules 1 and 4 despite having durable exact provenance. The finding concerns the historical inconsistent-row class rule 4 promises to quarantine; it does not claim fresh current-head flows normally create such a row, nor that T1 is consumed twice.

Evidence: [isolated settled-row reproduction](/work/scratch/dp1-02-r8-sol/settled-orphan-alone.log), [final probes, including the passing ordinary Done-row control](/work/scratch/dp1-02-r8-sol/r8-final-probes.log).

Required fix: preserve signed ownership independently of whether the signer row still exists, or refuse deleting that row while any of its own signed funding remains unconsumed or unresolved. The ordinary “identity already exists” settlement exemption must distinguish that consumed lock from a separate outstanding signed lock. Keep the ownership decision in the common model rather than adding another unrelated adoption filter.

**R8-03 — Minor: a reserved row whose lock is away disappears from Finish's pending count during a funding hold.**

Location: [early held return](/work/scratch/dp1-02-r8-sol/wt/rust/crates/dw-engine/src/platform/registration_flow.rs:1567), versus [reservation count computed only later](/work/scratch/dp1-02-r8-sol/wt/rust/crates/dw-engine/src/platform/registration_flow.rs:1642).

Start from the committed signed-then-rejected fixture, with durable signed provenance and no tracked lock. Set the Fake's sync hold. Finish returns `(0,0,0)` even though the row remains reserved and unresolved; its later row loop skips outpoint-less `KeysPrepared`. The isolated assertion fails identically. [Evidence](/work/scratch/dp1-02-r8-sol/sync-pending-alone.log).

This does not permit another funding: the reservation still protects the row. It is a reporting/contract defect, not an independent money-safety blocker. Count the reserved-away row on the held path too, without double-counting a held lock.

**Answers to the ownership and interleaving questions.**

- **R7-01 closes for retained provenance.** `reserved` blocks unsigned substitution in the run, tap, known-index Finish path and amount-based Finish path. Missing signed T1 reads MaybeSent; returning T1 is selected by exact ownership before index restore. The committed regression passes. R8-01 shows why this is not complete across an unlanded-write restart.
- **R7-02 closes for retained provenance.** The identity fallback now uses `may_take(Some(row), lock)`; B cannot take A's signed consumed lock. Shared-index handling recognizes a reserved signer and moves an unrelated older row. A remains at its signed key. The committed regression passes, as does my actual close/reopen → older shared-index row moves → signed lock returns → A completes control.
- **Rules 1, 2 and 4 are compatible when ownership survives.** A holds only its own signed lock; B moves off a shared index; A waits while T1 is absent; returning T1 settles A. A historically completed with T2 leaves signed T1 pending, with no second holder. They are incomplete when the ownership model drops evidence on restart or row deletion, as above.
- **`may_take` is not literally the only route to recording an outpoint.** Candidate selection in `signed_lock`, `lock_of`'s index fallback, the identity fallback, tap, and Finish goes through it. New-row Finish admission reaches it indirectly through `stranded → unclaimed → unheld → may_take(None, ...)`; the later appdb insertion adds no ownership check. The direct known-index restore branch uses new-row eligibility and the driver's earlier reservation guard together. Index allocation itself gives no lock and needs no `may_take` call.
- **One explicit bypass remains:** successful `fund` returns go straight from `Ok(outpoint)` to `record_lock` ([line 2240](/work/scratch/dp1-02-r8-sol/wt/rust/crates/dw-engine/src/platform/registration_flow.rs:2240)). Also, `lock_of` trusts an already recorded outpoint. I found no current-pin scenario where successful `PwLib::fund` returns somebody else's lock: it calls the one-build entry point with the permit's counting signer and returns that build's outpoint. This is a boundary assumption, not evidence that every path consults `may_take`. Routing the successful result through the common rule would make the manager's literal requirement true. The demonstrated adoption failures here arise from missing ownership evidence, not an invented wrong successful return.

The [adoption grep](/work/scratch/dp1-02-r8-sol/adoption-audit.txt) and [ownership call-site list](/work/scratch/dp1-02-r8-sol/ownership-sites.txt) cover outpoint writes, adoption calls and index assignments. The appdb writer remains a CAS/unique-outpoint mechanism, not an ownership authority.

**Rebase and the declared gaps.**

The [range-diff](/work/scratch/dp1-02-r8-sol/range-diff.txt) gives exact patch matches for all 18 substantive pre-existing DP1-02 commits. The old standalone accessor commit is omitted because P2b now supplies it; the nineteenth rebased commit is the new common ownership rule. All 19 commits verify `G` ([signatures](/work/scratch/dp1-02-r8-sol/signatures.txt)). The entire DP1-02 delta to P2b's `lease/mode_b.rs` removes the accessor's `expect(dead_code)` attribute; it changes no status derivation.

Schema 5, `SelfSpend` conflict provenance and `MissesSignedLock` require no DP1-02 source adaptation. DP1-02 does not decode or construct those storage records or match that refusal variant. It consumes the unchanged txid accessor and funding status. I additionally recorded self-spend input-conflict evidence through the actual LeaseTable against the fixture's signed input, injected its ChainLock into Mode B's snapshot, observed actual **NotSent**, and verified the previously reserved row can then take an unsigned lock through the real tap. That control passes; it is stronger than manually setting `Evidence.resolved`. It does not exercise a mined self-spend or the entire UI/dispatch-notification retry flow.

P2b's upcoming “legacy self-spend never yields NotSent” rule only tightens this reservation release. DP1-02 does not require legacy evidence to resolve, and the positive probe uses authenticated input-conflict provenance. The unsafe legacy fallback still exists in the exact supplied P2b pin and remains P2b's known r4 blocker, currently dormant under production default-false recovery completeness. It is not the cause of R8-01 or R8-02, and fixing it alone leaves both defects present.

Of the implementer's declared gaps:

- **Unlanded signed records are a real money-safety risk in this DP1-02 consumer.** P2b's marker survives, but DP1-02's status/discard/adoption model ignores it. My prior P2b r4 judgment that the accessor gap is safe was conditional on the consumer retaining the marker's conservative status; r8 demonstrates that condition does not hold on all DP1-02 paths.
- **Evidence-only NotSent-release coverage was a test gap, not a demonstrated money-safety error.** The release condition is conservative if P2b supplies sound NotSent; the real LeaseTable/self-spend-record/tap control passes. Full release/resume notification coverage remains unverified. The legacy P2b negative-evidence issue must still be fixed upstream of this consumer.
- **Mutable balance/grant allow-list refusals are acceptable under the explicit post-identity settlement policy.** They are not permanently repeatable: balance, grant, discovered keys or feature state can change. Here they settle the name/profile step with a surfaced error after the asset lock already created its identity; they do not unfund the identity or authorize replacement Core funding. The identity remains available for later name/profile operations. Keep that policy qualification; no newly found unknown-execution error was shown to enter the typed refusal list.

**Independent mutations and validation.**

I independently changed three sites in a disposable source copy, activating one mutation per process. Each named test passed with mutations disabled, then failed with exit 101 on the appropriate mutated predicate:

1. **KNOWN_INDEX:** direct index restore sees a raw unheld known-index lock without signed-ownership filtering. My new B-at-A's-index probe kills it by detecting B taking A's lock.
2. **RUN_RESERVATION:** the driver's signed-away reservation stop is bypassed. The committed per-path test kills it by detecting A moving off its signed key.
3. **ORPHAN_IMPORT:** Finish also imports signed orphans through its fresh-row path. The committed rule-4 test kills it when the pending lock is reassigned.

Result: **3/3 killed**, all deterministic state assertions, no mutation timeout. This validates those sites; it does not establish that the implementer's 18 mutations covered restart evidence loss, legacy settled-row deletion, or the held-return count. [Results](/work/scratch/dp1-02-r8-sol/mutation-results.json), [runner](/work/scratch/dp1-02-r8-sol/run-mutations.py), [exact mutations](/work/scratch/dp1-02-r8-sol/mutations.patch).

- Exact-head local build: `cargo test --locked --offline -p dw-engine -p dw-appdb --lib --no-run` **passes** ([log](/work/scratch/dp1-02-r8-sol/build.log)). Cargo's exact engine compiler arguments were captured for the disposable probes ([recipe](/work/scratch/dp1-02-r8-sol/compiler-recipe.json)).
- Unmodified engine suite: **522 passed, 0 failed, 2 ignored**, including both lease stress tests and the R7 regressions ([log](/work/scratch/dp1-02-r8-sol/engine-tests.log)).
- Unmodified appdb suite: **64 passed, 0 failed** ([log](/work/scratch/dp1-02-r8-sol/appdb-tests.log)).
- `cargo fmt --all -- --check` and `git diff --check 5a1e3ab..6bcaa2f`: **pass**.
- Final review-only probes against unchanged production predicates: **5 passed, 5 failed**. The failures are two replacement-funding probes and one substitution probe for R8-01, one legacy discard probe for R8-02, and one held-count probe for R8-03. Each reported failure mechanism also reproduces in isolation. The passing controls cover ordinary Done-row discard refusal, actual LeaseTable NotSent/tap release, same-row P2b veto, shared-index restart recovery and direct known-index ownership.

The supplied whole-gate **1,086/0** and **18/18** mutation account are implementer evidence; I did not replay the whole workspace gate. Review probes use real sessions, leases, SQLite, funding markers and failed writes, with the committed Fake for wallet/network outcomes and injected historical/recovered state. No live DAPI or mined-chain experiment is claimed. Probe sources are retained in [original probes](/work/scratch/dp1-02-r8-sol/r8-probes.rs) and [additional probes](/work/scratch/dp1-02-r8-sol/r8-additional-probes.rs), with compiler recipes and logs.

All builds/tests used `nice -n 10`, umask 022, CARGO_BUILD_JOBS=12, a scratch-owned target and disk TMPDIR `/work/scratch/dp1-02-r8-sol/tmp` at 0700. Dependency artifacts were copied into that owned target; no other worktree's target was used for compilation. Both owned build targets and the temporary fixture directory were deleted after validation. No implementation edits, commits, pushes, GitHub actions/comments or process-pattern kills were performed. This review file is uncommitted.

**Verdict:** REQUIRES CHANGES — retained-provenance R7-01/R7-02 close, but restart marker loss in DP1's evidence model permits replacement funding; legacy settled-row deletion defeats rule 4.
Sol gate satisfied: no
