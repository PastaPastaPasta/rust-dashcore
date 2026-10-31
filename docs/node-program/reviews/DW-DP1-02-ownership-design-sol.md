# DP1-02 ownership design — Sol review

Reviewed **fb6483f2698762b0ad3190e187749d850a4b0187**, including **§13 and all manager rulings**, against DP1-02 **6bcaa2f8eaec676587ea899bb89953aa036e1711** and P2b **52215e39c02c6cf78c6ed6adb9ba6b33aa95bfcf**. PA-1/2/3 are approved P2b work; Q1 recovery rows, Q4's separate display predicate, Q5's full-key condition, Q6's amount narrowing and Q7's orphan entries are treated as requirements. The user-Release fallback is not assumed available.

**Changes are required.** Step-keyed evidence and one ledger are the right replacement for the old filters, but the proposed predicates do not yet close the class. PA-2's store-hook condition does not exclude an actual parallel rebroadcast path. Two terminal evidence states have no consumer recovery transition, and a running funding attempt has a window before the marker appears in which the ledger still permits substitution. Changing individual consumers would repeat the earlier problem; these need changes to the model and the P2b contract.

The DP1-02 worktree advanced during review. All conclusions below use the requested revisions, saved as [immutable design](/work/scratch/dp1-02-design-sol/DP1-02-ownership-model-fb6483f.md), [DP1 source snapshots](/work/scratch/dp1-02-design-sol/dp1/rust/crates/dw-engine/src/platform/registration_flow.rs) and [P2b source snapshots](/work/scratch/dp1-02-design-sol/p2b/rust/crates/dw-engine/src/platform/lease/mode_b.rs). I did not review the subsequent implementation or PA changes.

## Findings, by severity

### OD-01 — Blocker: PA-2 fences the original broadcaster, but not a concurrent resume of the published Built row

**Design:** [§9 PA-2](/work/scratch/dp1-02-design-sol/DP1-02-ownership-model-fb6483f.md:268), [§13 soundness ruling](/work/scratch/dp1-02-design-sol/DP1-02-ownership-model-fb6483f.md:418).

**Scenario:** The marked funding call signs T1. The pin's [builder publishes T1 as Built](/work/home/agent/.cargo/git/checkouts/platform-7a21f318038a582f/bc41f1b/packages/rs-platform-wallet/src/wallet/asset_lock/build.rs:1098) under the wallet-manager write guard, releases that guard, then calls `queue_asset_lock_changeset`. PA-2 blocks inside `note_signed` because its dispatch-journal INSERT fails. The original build cannot reach its broadcast. However, T1 is already readable by another task.

The real P2b [resume pass reads those in-memory rows](/work/scratch/dp1-02-design-sol/p2b/rust/crates/dw-engine/src/platform/lease/resume.rs:235), for example on a peer reconnection, and [calls `resume_asset_lock`](/work/scratch/dp1-02-design-sol/p2b/rust/crates/dw-engine/src/platform/lease/mode_b.rs:1254). The library [snapshots the transaction](/work/home/agent/.cargo/git/checkouts/platform-7a21f318038a582f/bc41f1b/packages/rs-platform-wallet/src/wallet/asset_lock/sync/recovery.rs:958), probes for local finality, waits for transport if necessary, and [broadcasts it directly](/work/home/agent/.cargo/git/checkouts/platform-7a21f318038a582f/bc41f1b/packages/rs-platform-wallet/src/wallet/asset_lock/sync/recovery.rs:1176). It persists the ensuing status **after** this send. `claim_resume_dispatch` protects against rejected-build cleanup; it does not require signed-record durability. [WalletPersister](/work/home/agent/.cargo/git/checkouts/platform-7a21f318038a582f/bc41f1b/packages/rs-platform-wallet/src/wallet/persister.rs:66) supplies no shared store/read lock that would incidentally exclude the resume.

Thus `mark → publish Built → blocked hook → parallel resume/send → restart` leaves **marker, no durable signed record, no running call**, although T1 may already be on the wire. The proposed `NotSent` derivation then authorizes exactly the r8 discard/replacement/substitution class. Stopping the process on write failure does not eliminate the publication-before-hook window: the parallel resume may send before the hook reaches the failure.

**Fix:** strengthen PA-2 to the actual invariant: **no sender or exporter of a step's signed transaction can run before its record is durable**. Either record durably before publishing a resumable row, or put a durable-record gate shared by the builder, direct resume, deferred resume and startup/reconnection resumers before every broadcast. Preserve attribution to the correct funding step at that gate. Merely returning a store error is insufficient: [the queue helper logs persistence errors and returns](/work/home/agent/.cargo/git/checkouts/platform-7a21f318038a582f/bc41f1b/packages/rs-platform-wallet/src/wallet/asset_lock/manager.rs:220).

Required regression: block the actual signed-record hook before durability, expose the actual Built row, trigger the actual P2b resume path on another worker, and assert **zero broadcaster calls**. Repeat with restart at the publication boundary. This needs the parallel path, not only a Fake whose original `fund` callback waits for the hook.

### OD-02 — Blocker for supported old journals: PA-2 is not evidence about markers written before PA-2

**Design:** [no-schema claim](/work/scratch/dp1-02-design-sol/DP1-02-ownership-model-fb6483f.md:234), [restart derivation](/work/scratch/dp1-02-design-sol/DP1-02-ownership-model-fb6483f.md:273), [r8 closure](/work/scratch/dp1-02-design-sol/DP1-02-ownership-model-fb6483f.md:319).

**Scenario:** Open with the new implementation the real dispatch database produced by the old r8 failed-INSERT/restart probe. The previous writer allowed signed bytes to proceed despite losing every signed-record write. The durable marker survives; its signed record does not. No call now runs. Even a completely repaired PA-2 implementation cannot infer `NotSent` for this historical marker: its writer did not enforce PA-2.

This is an existing, independently demonstrated initialization state, not hypothetical disk corruption. See [the actual SQLite/reopen probe](/work/scratch/dp1-02-r8-sol/r8-probes.rs:17) and [distinct replacement reproduction](/work/scratch/dp1-02-r8-sol/distinct-replacement-alone.log). The design explicitly accommodates legacy inconsistent rows and restore orphans but carries no per-marker guarantee distinguishing old unsafe writers from new ones. A current schema version or a database-wide upgrade bit does not retroactively establish the invariant for existing markers.

**Fix:** make the stronger durability guarantee identifiable for **each marker/attempt** when it is created, and permit absence-based `NotSent` only for attempts carrying that guarantee. Existing markers remain `Unknown` until positive evidence resolves them. Specify the migration/reconciliation recovery for those markers; simply preserving `Unknown` without an exit would fail the requested liveness gate. If the intended product has never admitted any pre-PA-2 journal, explicitly establish and constrain that initialization contract instead. It is currently contradicted by the design's stated r8/legacy coverage.

With the approved no-Release ruling, the design cannot promise both automatic release and recovery of arbitrary old empty-record markers by asserting PA-2 about the new executable. This requires an explicit migration decision, not another row-level exception.

### OD-03 — Major: a consumed signed lock without an appdb holder cannot reconcile or settle its signer

**Design:** [settlement requires a holding row](/work/scratch/dp1-02-design-sol/DP1-02-ownership-model-fb6483f.md:72), [owner excludes consumed locks](/work/scratch/dp1-02-design-sol/DP1-02-ownership-model-fb6483f.md:96), [own_lock requires may_take](/work/scratch/dp1-02-design-sol/DP1-02-ownership-model-fb6483f.md:154), [R7-02 closure](/work/scratch/dp1-02-design-sol/DP1-02-ownership-model-fb6483f.md:318).

**Scenario:** Use the actual r7 identity-restoration fixture: A is readable, `KeysPrepared`, with neither outpoint nor identity; S_A has durable signed txid T1; discovery knows `identity_for(T1)` and T1 is `Consumed`. [The saved probe constructs precisely this state](/work/scratch/dp1-02-r7-sol/r7-probes.rs:49). It can also be reached when another device completes IdentityCreate while this device's appdb association is missing.

`own(S_A)={T1}`, but no row holds T1, so `settled(S_A)=false`. A is `Awaiting`. `owner(T1)=None` because it is consumed, so `may_take(A,T1)=false` and `own_lock(A)` returns nothing. Finish imports only unconsumed locks and its unchanged run loop skips outpoint-less `KeysPrepared` rows. Repair cannot resolve a Sent/consumed funding as NotSent. Proofs and discovery have already delivered the terminal positive evidence. The step remains outstanding, pending is 1, discard is refused and `start` remains blocked indefinitely.

The proposed rule does stop B stealing T1, but it does **not** complete the r7 fixture's recovery of A. Q1's recovery row also cannot help: it has no permitted route for a consumed lock.

**Fix:** separate allocating **unspent value** from reconciling **already-consumed value**. Add one ledger decision that uses S_A's exact txid and `identity_for(outpoint)` to attach/reconcile the already-existing identity to its signer/heir, or record its recovery attribution when its signer cannot take it. It must never issue another IdentityCreate or fresh funding for that value. Then settle the step through that association. Merely declaring every consumed signed txid settled can free an outpoint-less A to fund again without reconciling its already-funded intent.

Also establish the induction base for restored/legacy recorded outpoints. [Backup import writes dp_registration columns directly](/work/scratch/dp1-02-design-sol/dp1/rust/crates/dw-appdb/src/rows.rs:133), rather than passing `may_take`. A historical B already holding A's consumed T1 makes the proposed `settled(S_A)` true despite A remaining unbound; the old StepKeyMismatch/refunding failure can then recur when A moves. I3's induction covers new controlled writes, not arbitrary supported starting rows. Reconcile exact contrary ownership or specify a safe terminal legacy association before trusting those rows.

Required regression: preserve the r7 consumed-lock/discovered-identity fixture, demand recovery of A as well as refusal of B, then exercise a restore of the historical wrong-holder state.

### OD-04 — Major: Repair's NotSent does not release a row that already records the now-impossible outpoint

**Design:** [Funded takes precedence](/work/scratch/dp1-02-design-sol/DP1-02-ownership-model-fb6483f.md:90), [Funded run rule](/work/scratch/dp1-02-design-sol/DP1-02-ownership-model-fb6483f.md:190), [claimed Repair exit](/work/scratch/dp1-02-design-sol/DP1-02-ownership-model-fb6483f.md:241).

**Scenario:** The tap successfully records T1 in A. Library tracking of T1 is subsequently lost. S_A still has the durable signed txid and inputs. Repair self-spends an input, the conflict is ChainLocked, and P2b positively derives `NotSent` for T1. This is the design's advertised exit for a Funded row with an untracked Exact lock.

The outpoint in app.sqlite does not disappear when the dispatch evidence changes. `consumed(T1)` is false: this was an input-conflicting self-spend, not an identity consuming T1. The row therefore remains `Funded(T1)` and committed by §3.3's first applicable funded rule, even though S_A is no longer outstanding. The unchanged [create_identity path waits when its recorded lock is missing](/work/scratch/dp1-02-design-sol/dp1/rust/crates/dw-engine/src/platform/registration_flow.rs:2366). That transaction can never acquire a proof. Finish sees a holder; discard refuses; the existing `unfund` helper is reached only from the funding-call result, not from this later Repair outcome.

**Fix:** define a common reconciliation transition for **a recorded outpoint positively invalidated by its own step's NotSent evidence**, after the relevant call has stopped. CAS-clear that outpoint, normalize the phase/failure and backup state, and release the internal commitment. Apply it through the ledger in run, resume, Finish and status/gate reconciliation as appropriate. Require an exact match to the invalidated signed lock; do not clear a legacy substitute T2 merely because A's separate signed T1 was resolved. Respect P2b's policy where Repair requires a new draft rather than retrying the old funding step.

Required regression: successful tap → lost tracking → actual recorded-input self-spend/ChainLock → NotSent → resume/Finish/discard. The old r8 NotSent/tap control starts **without** a recorded outpoint and therefore does not exercise this hold.

### OD-05 — Major: in_call without a marker still permits a Free lock to replace an intent that is about to fund

**Design:** [Unfunded while calling](/work/scratch/dp1-02-design-sol/DP1-02-ownership-model-fb6483f.md:94), [Free/Row permission](/work/scratch/dp1-02-design-sol/DP1-02-ownership-model-fb6483f.md:163), [Finish selection](/work/scratch/dp1-02-design-sol/DP1-02-ownership-model-fb6483f.md:169), [deleting running-call deferral](/work/scratch/dp1-02-design-sol/DP1-02-ownership-model-fb6483f.md:228).

**Scenario:** A passed its pre-funding checks and [enter_funding set its in_call flag on the actor](/work/scratch/dp1-02-design-sol/dp1/rust/crates/dw-engine/src/platform/registration_flow.rs:2084). The asynchronous funding-permit path has not yet installed its marker. It performs [fee/snapshot/draft work before taking the marked permit](/work/scratch/dp1-02-design-sol/p2b/rust/crates/dw-engine/src/platform/lease/mode_b.rs:1354). A foreign same-seed T2 becomes visible. Finish reads its ledger: A is still `Unfunded` and open, no step exists in PA-1, and T2 is `Free`. `may_take(Row(A),T2)` returns true despite `committed(A)` being true through in_call. Finish's Free selection can give T2 to A.

The actor and J are separate: a PA-1 read does not exclude the permit's marking J step. In particular, finish the permit's funding snapshot first, let Finish read no marker, then let the J step mark while Finish attaches T2. The draft key is unchanged by the attachment, and row CAS does not compare against dispatch-marker state. The original driver can continue building T1 against its already-entered call. This makes one intent funded by T2 while another lock is built for it; T1 later needs recovery or can be hidden by the post-call/holder interaction. PA-2 does not help: both actual sends may have durable signed records.

**Fix:** put the running attempt into the ownership predicate before marking. For a calling row, deny **Free** substitutions; allow only its positively attributed Step lock. Retain the already-modelled in_call token through post-call reconciliation. One predicate inside `may_take` is sufficient; no separate Finish heuristic/counter is needed. Alternatively, serialize funding admission with ledger mutations all the way through marking, with a demonstrated lock-order contract. A row-only CAS and one J read are not that serialization.

There is also a smaller contradiction in the same running-call mapping: §6 says `Step → row_of(S) if open and not in call, else Recovery`, but §5.1 denies Recovery when the signer is open, including while it calls. The safe common rule is to leave this lock pending for the open caller, not offer Recovery. Update the path mapping to match it.

Required regression: pause between actor call admission and P2b marking, and between the PA-1 read and its resulting write; introduce T2 and run Finish/tap. A must not take T2. The marked-Unknown and marked-Exact running controls are necessary but do not cover the pre-marker window.

### OD-06 — Minor: PA-1's standing-step enumeration cannot expose PA-3's superseded dead txids as specified

**Design:** [PA-1 enumeration](/work/scratch/dp1-02-design-sol/DP1-02-ownership-model-fb6483f.md:263), [PA-3](/work/scratch/dp1-02-design-sol/DP1-02-ownership-model-fb6483f.md:278), [Dead owner rule](/work/scratch/dp1-02-design-sol/DP1-02-ownership-model-fb6483f.md:100).

**Scenario:** Preserve T1's signed txid after its marker is superseded by a durable NotSent row, as PA-3 requests. The approved PA-1 contract still says to enumerate `funding_steps_of`. At 52215e3 [that enumerator reads the standing funding-key map](/work/scratch/dp1-02-design-sol/p2b/rust/crates/dw-engine/src/platform/lease/fence.rs:953), and [supersede removes that key](/work/scratch/dp1-02-design-sol/p2b/rust/crates/dw-engine/src/platform/lease/fence.rs:1051). Keeping the txid alone does not put the step back in the returned view. T1 is Free rather than Dead; a still-tracked Built T1 can receive the recovery row that PA-3 was meant to prevent.

**Fix:** explicitly extend PA-1 to enumerate the retained resolved ownership tombstones as well as standing steps, and specify their retention over reopen/compaction. Distinguish resolved txids from a later active attempt of the same logical step if such retries are allowed; a single new active status must not resurrect older dead txids. This can reuse journal marker-generation information; it does not require another DP1 ownership store. Include a supersede → restart → Finish regression, plus the allowed retry policy.

## Closure table audit

These are **design judgments**, checked against the earlier scenarios and retained probe sources/results. No unimplemented Rust ledger was represented as passing those integration tests. “Preserved” means the existing mechanism can remain; it is not new proof supplied by the ledger.

| §10 entry | Judgment against the earlier probes |
|---|---|
| R1-01 synthetic recovered index / restore double funding | **Covered in principle.** Preserve `known_index`: chain-reconstructed index 0 remains unknown. A Free unindexed lock blocks automatic funding through `index_owner`; Finish allocates under the retained index guard. Keep both original nonzero-index and synthetic-zero probes. |
| R1-02 overlapping discard guards | **Preserved.** Exclusive `take`, actor recheck and guarded delete remain necessary. The ledger does not replace decision ownership. |
| R1-03 KeysPrepared with an explicit outpoint | **Covered for a live lock.** Funded standing normalizes and runs it. The claimed later Repair exit is missing: OD-04. |
| R1-04 unknown funding encoding hides commitment | **Covered in principle.** Unreadable standing keeps DEC-171/173's conservative may-own evidence; unsupported does not mean unfunded. |
| R1-05 index identity adopted before other-holder check | **Safety covered.** Held/consumed locks cannot be donated to a different row. Consumed reconciliation still needs OD-03. |
| R1-06 unsupported profile after funding | **Preserved.** `interpret` must still run before any signing/write. |
| R1-07 real manual locks/reservations ignored | **Preserved.** The real fenced coin snapshot and selection/signing fence stay. A Ledger of identity locks is not a source-coin exclusion mechanism. |
| R2-04 snapshot-to-signing coin-policy race | **Preserved.** Retain the same fence for policy mutation, reservations and selection/signing. Retain the real offline-builder probe, both reservation orderings and no-tap control. |
| R3-01 rejected-lock reservation blocks its own cleanup | **Preserved.** P2b's derived pending-input overlay, rather than old registration-owned host reservations, supplies the exit. The ordinary start-as-first-snapshot probe remains essential. |
| R4-01 stale cleanup erases a send's same-coin reservation | **Preserved.** No registration cleanup may remove another operation's claim. The real same-coin send probe, not a different-coin control, remains the relevant evidence. |
| R1-08 retired cleanup clears reopened driver | **Preserved.** Process-wide run identity and run-scoped cleanup remain. |
| R2-01 retired queued acquisition invalidates new owner | **Preserved.** Admission and persisted acquisition stay one actor job; generation loss stops the run. A ledger snapshot is not a replacement for these fences. |
| R2-02 Finish consumes an unreadable row's ambiguous lock | **Covered in principle.** Parked wins over Free import. Keep unknown funding/profile/phase, malformed outpoint and absent-index variants. The future-reader exit is an explicit compatibility hold. |
| R2-03 stale association after index reassignment | **Preserved for row mutation.** Selection/write must stay inside the actor with concrete owner and CAS. The different actor/J pre-marker interleaving is OD-05. |
| R5 N1 whole-path tap regression | **Preserved test obligation.** Keep classify → signed record → store → tap/fence behavior, with Built index 0 and Broadcast controls; direct helper calls are insufficient. |
| Opus MJ-1 post-identity permanent refusal blocks the queue | **Covered subject to the existing policy.** Settled identities release the internal gate; Q4 preserves funds_committed display history. An additional unconsumed signed lock keeps the row Closed/committed. |
| Opus MN-1 untapped index-0 lock and non-Ok call | **Partial.** Exact txid attribution ignores proof-status/index races. The consumed terminal state is OD-03; restart absence depends on OD-01/02. |
| Opus MN-2 backup re-arming after unfund | **Preserved.** A later reconciliation release also needs the backup reset specified in OD-04. |
| Opus MN-3 profile idempotency | **Preserved.** Platform reconciliation and field-preserving edits are unrelated to funding ownership. |
| Opus MN-4 mutation coverage | **Not automatic closure.** Preserve the named B/I/N/O/W/G/H/R/F rule tests with updated semantics. In particular the Finish-running exclusion needs the pre-marker test from OD-05. |
| R6-01 own lock proves before attribution | **Covered for an unconsumed lock.** Exact signed txid wins at Built/Broadcast/IS/CL/reconstruction; no status heuristic. Consumed must also reconcile: OD-03. |
| R6-02 settlement of flattened/unproven execution errors | **Preserved.** Keep the typed refusal allow-list; Internal/proof/nonce/unknown-write outcomes remain conservative. |
| R6-04 failed/stale profile reconciliation reads | **Preserved.** `after_read` and the freshness/reconciliation policy stay. Ownership evidence does not authenticate Platform operation execution. |
| R6-03 foreign reconstructed Broadcast appears during failed funding | **Covered after marking.** Exact local provenance beats the call-window heuristic; a foreign lost-index lock becomes explicit-Finish-only once uncertainty ends. Before marking, OD-05 remains. |
| R7-01 Finish substitutes T2 and suppresses returning T1 | **Covered in principle.** Awaiting accepts only its Step locks; legacy Closed T1 goes to approved Recovery. Preserve the signed-rejected → foreign T2 → returning proved T1 probe; update the ordinary Done-row control to Q1 recovery. |
| R7-02 identity fallback steals A's signed consumed lock | **Incomplete.** B is denied, but A wedges in the actual consumed/discovered fixture: OD-03. Also cover restored historical wrong holders. |
| R8-01 failed signed-record writes/restart | **Incomplete.** Unknown is correct consumer evidence. PA-2 as stated does not establish its absence-based release, and cannot retroactively cover old records: OD-01/02. |
| R8-02 deleting legacy settled A frees signed T1 | **Covered in principle.** PA-1 reads S_A without A; outstanding own T1 makes A Closed. Q1 recovers T1; only then does Q4 allow local-row deletion. |
| R8-03 held path drops the reserved-away count | **Covered.** One fresh `pending()` counts the outstanding step even with no tracked lock, including the held path. Its deliberate Unknown/Contested double count is conservative and should be surfaced as such. |

The three r8 mutation sites remain meaningful: **KNOWN_INDEX** must die at `index_owner`'s shared permission, **RUN_RESERVATION** at Awaiting/Closed admission, and **ORPHAN_IMPORT** at Step/Recovery permission. Update the third mutation's expected recovery recipient according to Q1; do not preserve the old permanent-pending result. The old mutation counts do not cover OD-01 through OD-05.

## Outpoint-route audit

The proposed §6 does identify the existing production adoption sites. It does not yet make every trust exemption sound for the entire supported state space.

| Route | Required common-model treatment |
|---|---|
| `record_lock` before the funding call, from own_lock or index restore | Inside the mutation/admission actor job, verify the current ledger permission and row generation. Exact Step selection comes first; a calling row cannot take Free evidence. |
| `record_lock` after Ok, timeout or another error | Re-read the ledger and use exact ownership rather than trusting Ok by itself. Include the already-held own outpoint case: a successful tap makes `owner=None`, so unheld `own_lock` alone cannot recognize it. Preserve the normal tap-first/return-later control. |
| tap `attach_lock` | The same permission, with selection and CAS in its actor job. A known index is candidate eligibility, not contrary-exact ownership evidence. Include pre-marker calling state. |
| Finish attachment to an existing row | The same permission; no Free gift to a running attempt. An open Step signer in call remains pending if Finish defers it. |
| `adopt_registration_lock` | Safe as a CAS/unique-outpoint **writer** only after `may_take(Recovery,L)` in the actor job. It cannot decide ownership on its own. Q1 is an intentional, approved recovery recipient. |
| `lock_of` with an already-recorded outpoint | Safe by induction for writes made under the new rule. Restored/legacy rows are initialization inputs and need the reconciliation/contrary-owner treatment in OD-03; the blanket exemption is otherwise unjustified. |
| consumed-lock/identity discovery | Missing route: exact reconciliation needs a common ledger authorization, distinct from allocating an unspent lock. OD-03. |

The baseline's direct successful-return bypass is explicitly addressed by the design. I found no production alternate successful `PwLib::fund` returning a different build's outpoint; the strict mismatch check is useful contract hardening, rather than a demonstrated current-pin wrong-Ok bug.

## PA-2 source audit and restart boundaries

The design cites **bc41f1bc233dec4607d387101c1d9c2f111019b2**. The supplied desktop Cargo manifest actually pins **ebe37f8a679b1a552483dc7ff12c306291e9a32b**. I compared `asset_lock/build.rs`, `manager.rs`, `sync/tracking.rs`, `sync/recovery.rs` and `sync/reconstruction.rs` between both available checkouts: each corresponding file is byte-for-byte identical. The PA-2 counterexample therefore applies to both pins; update the citation when implementing.

| Surface | Observed ordering / conclusion |
|---|---|
| Real DP1 funding | `CountingFundingSigner` from the marked permit → `create_funded_asset_lock_proof` → one build → publish Built → synchronous store hook → original broadcast. PA-2 can gate the original send, provided every matching-hook success proves durability. |
| WalletStore | Classify → `note_signed` → inner store → recorded tap. Dispatch durability can be established before wallet persistence. The current 52215e3 hook logs/keeps undurable records; PA-2 must replace that behavior, including missing backend and unsuccessful attribution/write outcomes. `insert_signed_lock` returning false is not durable success. |
| Direct Built/Broadcast resume | Reads published signed bytes and sends before its own status store. **Bypass: OD-01.** It is reachable from the real P2b resume pass. |
| Deferred transport-ready retry | Re-enters the same resume path; it supplies no independent signed-record gate. Covered by the OD-01 fix, not by waiting for the original hook. |
| Build-only public APIs | `build_asset_lock_transaction` and its funding variant return signed transaction bytes without tracking/store. DP1's production `PwLib::fund` does **not** call them, so they are an explicit current-consumer exclusion. PA-2's broad claim about all library exporters is false unless these are restricted/gated when using a marked permit signer. |
| Identity registration | Uses `FromExistingAssetLock`; the funding signer authorizes consumption of that existing credit. This is not another build/broadcast of Core registration funding. Keep the outpoint-derived identity checks. |

Restart coverage must include:

1. Before the marker lands: no funded library call may start; in-process call admission still prevents substitution while permit acquisition is outstanding.
2. After the marker, before signing: absence-based NotSent is safe only for a marker created under the strengthened PA-2 guarantee, with no running attempt or unresolved publication capable of sending.
3. After signing/publication, before signed-record durability: the current parallel resume makes the state unsafe. After OD-01's fix, no sender/exporter may obtain that transaction until durability.
4. After the signed record lands, before wallet persistence/broadcast: preserve Exact even if wallet tracking disappears. Neither ordinary error nor signature revocation proves NotSent.
5. After appdb association, before/after proof or consumption: recorded outpoint recovery remains conservative, but terminal consumed discovery needs OD-03.
6. After Repair conflict finality, before appdb normalization: preserve and apply the exact NotSent invalidation, OD-04. A failed appdb normalization is retryable evidence reconciliation, not a reason to discard unrelated funding.
7. After supersede/compaction/reopen: PA-3 dead ownership must still be present in PA-1, OD-06.
8. Restore/removal: standing funding evidence must outlive the local row. The baseline `erase_wallet` compacts only resolved artifacts and retains possibly-sent step evidence; include the requested real remove/restore test. Scope dead-tombstone retention separately.

A Ledger is a decision snapshot, not a lock on P2b's concurrent state. One actor job plus one J read avoids the old row-read/write race, but it does not prevent a funding marker from being installed after that read. Call admission must cover that interval, and writes must retain their CAS checks. PA-1 must also distinguish a ready empty journal from unavailable evidence; an unavailable journal must not become an empty ownership set.

## Liveness, heir safety and simplification

**The no-cycle proof is not yet a liveness proof.** Q1 gives a useful exit for an unconsumed Exact lock whose signer is permanently closed. However OD-03 and OD-04 are terminal-evidence wedges that wait on events which have already happened or can no longer happen; OD-01/02 undermine the asserted Unknown exit. Parked unreadable rows intentionally wait for a compatible build, so any “every hold ends” claim must explicitly retain that compatibility qualification, along with functioning storage/network and P2b's documented `LockInputsSpent` limitation.

**Q5 is safe with its ruling applied.** Compare the full wallet-scoped FundingKey, not only identity_index; require a readable, open row with no identity/outpoint and no outstanding own step. The actual FundingKey is `(funding_type, identity_index, account)`, not a cryptographic public key. Deriving the same full tuple from a restored CoreBalance row grants the approved heir association; it does not manufacture exact transaction provenance. Keep exact txid ownership in S_A and prevent inherited rows from funding or moving index. If the match fails, Recovery handles the orphan's unconsumed lock. The consumed-orphan case still needs OD-03. The small probe checks exact-key inheritance and independent type/account/index mismatches.

**Q4/Q6/Q7 remain necessary.** Keep the historical funds_committed display separate from the internal unresolved-funding gate. Use FundingUnknown for uncertainty rather than pretending proof wait is the only action. Report orphan steps as read-only entries. Amount narrowing may shorten transient holds; it must compare to the actual authorised funding amount and remain conservative where an orphan's amount is unavailable. It is not an ownership proof and must not release pre-PA-2 uncertainty.

**The core model is not excessive.** Keep three-valued evidence: the running pre-signature state and supported historical uncertainty remain real even with a sound PA-2. Keep step-keyed enumeration, one permission function, exact-key heirs, Recovery and unified pending reporting. Avoid retaining the rejected Release table or the old `Provenance`/destined/reserved/orphaned filter family beside the ledger.

The smallest corrections are a shared durability gate at send/publication, an explicit legacy-marker guarantee, a common terminal-evidence reconciliation decision, and the already-present calling token in Free adoption permission. Delete path-specific running-call heuristics only after those predicates subsume them. Define settlement from those authoritative associations instead of adding more phase/failure exceptions. PA-3 should reuse the same step/marker history, not introduce a second ownership ledger.

## Validation and limits

- Read the entire requested design, all eight Sol reports, the relevant retained r7/r8 probe sources/results, the r8 adoption audit and mutation results, the exact baseline consumers, the P2b signing/dispatch/resume/Repair paths, and both pinned wallet source copies.
- Ran [small literal decision-table probes](/work/scratch/dp1-02-design-sol/design_probes.py) with `nice -n 10`, `python3 -I -B` and **TMPDIR=/work/scratch/dp1-02-design-sol/tmp**, mode **0700**. [Output](/work/scratch/dp1-02-design-sol/probes.log) confirms the consumed-without-holder, held-after-Repair, pre-marker Free permission, running-Exact Recovery contradiction, PA-1/PA-3 omission and publication/resume-order witnesses, with positive reconciliation/key/gate controls.
- These probes model the document's predicates and observed source order. They are **not** an exhaustive model check or execution of an implemented new ledger. No new real broadcaster, live DAPI/chain experiment, workspace build, test suite or old mutation suite was run in this design review. The historical r8 restart evidence remains the prior actual SQLite/LeaseTable reproduction.
- No implementation/design source edits, commits, pushes, GitHub actions or process-pattern kills. Only immutable scratch snapshots, review probes/log and this requested uncommitted report were written. Concurrent worktree changes were left alone.

**Verdict:** CHANGES-REQUIRED
Sol gate satisfied: no
