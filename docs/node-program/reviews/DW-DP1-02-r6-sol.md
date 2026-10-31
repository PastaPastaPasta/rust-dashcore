# DW-DP1-02 Phase A — Sol r6

Reviewed **18a127cc8cfd51f9ee896cf94c6c47f773437b97**, branch `dw/dp1-02-registration`, range **7e331dd..18a127c**, in the unchanged detached worktree `/work/scratch/dp1-02-r6-sol/wt`. Basis: the r5 approval, Opus-high review of 7e331dd, m4 Contract-Version 11, DASHPAY registration/restore design, and DEC-171/173/179. Prior approved issues are not reopened without new evidence.

**Changes are required.** The ordinary name-taken wedge is fixed, as are backup re-arming, the nine named MN-4 coverage gaps, and continuing Finish past a row error. MN-1 still has a proof-arrival interleaving that produces a second row and permanently blocks the original. The new settlement classifier also mistakes some real library/SDK transient or unproven outcomes for permanent refusals.

No double funding or loss of an asset lock's value was reproduced. This is not an unconditional money-safety certification: the findings below defeat the required ownership/recovery and settlement rules. The accepted Phase A/P2b durable-marker and signer-accounting boundaries remain deferred; this review does not reopen them.

## New findings

### R6-01 — Major: an untapped own lock gaining a proof before post-call attribution recreates MN-1's permanent wedge

**Locations:** [call-window status filter](/work/scratch/dp1-02-r6-sol/wt/rust/crates/dw-engine/src/platform/registration_flow.rs:1062), [post-call attribution and own_lock](/work/scratch/dp1-02-r6-sol/wt/rust/crates/dw-engine/src/platform/registration_flow.rs:2130), [Finish's waiting-row eligibility](/work/scratch/dp1-02-r6-sol/wt/rust/crates/dw-engine/src/platform/registration_flow.rs:1011), [Finish skips an unfunded/non-resumable row](/work/scratch/dp1-02-r6-sol/wt/rust/crates/dw-engine/src/platform/registration.rs:906).

**Scenario:** index 0 funds; its outpoint does not reach appdb; the call ends with `IsTimeout`. Before the driver's evidence read, an asynchronous proof update changes that same lock from Broadcast to InstantSendLocked (ChainLocked has the same issue). `known_index` remains unknown at index 0. `built_by_the_call` excludes the proved lock, so neither `tracked` nor `own_lock` captures it. `maybe_sent` writes non-retryable `platform.broadcast_unknown` without an outpoint. Finish excludes that failed row from `awaiting_a_lock`, gives the lock a new unnamed row, and completes that new row. The original still has no identity/outpoint and still counts as committed/in progress; resume and discard cannot release it.

**Evidence:** `review_r6_own_lock_proof_race_must_not_wedge` fails deterministically. Its hook changes the actual fake funding lock's status after the funding error is decided and before the driver receives the result; no implementation rule is changed.

```text
original: keys_prepared, broadcast_unknown, retryable=false, outpoint=None
Finish: resumed=1, completed=1, still_pending=0
rows: original failed row + a new unnamed Done row holding its lock
next start: Err(InProgress)
funding calls / identity creates: 1 / 1
```

This also makes Finish's `still_pending=0` misleading while the original registration remains blocked. It is new evidence for MN-1, not a reopening of an unrelated Phase B residual.

**Fix:** retain exact local-builder provenance before a proof/status update or a persister failure can hide it. Also make explicit Finish capable of settling the original `broadcast_unknown` row instead of importing beside it; retain the unreadable-row and running-call protections. Add the error-return/proof-arrival race at index 0 to the committed regressions. Simply accepting every proved lock in the snapshot heuristic widens R6-03; use provenance rather than status as the ownership mechanism.

### R6-02 — Major: arbitrary `internal` errors settle unproven execution and network failures

**Locations:** [post-identity fallback](/work/scratch/dp1-02-r6-sol/wt/rust/crates/dw-engine/src/platform/registration_flow.rs:2488), [failed's catch-all](/work/scratch/dp1-02-r6-sol/wt/rust/crates/dw-engine/src/platform/registration_flow.rs:1840), [settle drops the retryable distinction](/work/scratch/dp1-02-r6-sol/wt/rust/crates/dw-engine/src/platform/registration_flow.rs:2503), [SDK error conversion](/work/scratch/dp1-02-r6-sol/wt/rust/crates/dw-engine/src/platform/errors.rs:460). The conversion predates this range; the new settlement behavior makes its information loss material here.

**Scenario/evidence:** the pinned SDK's `ExecutionNotProved` means a proof authenticated affected state without proving this transition executed. It is not one of `sdk_leaf`'s recognized proof errors, so it maps to `Internal`. `after_identity` falls through to `failed`, yielding `Fail("internal", true)`; `settle` converts every Fail into a settled step. An absent/nonmatching profile recheck then ends the row Done with `profile_failure="internal"`, rather than waiting for execution evidence.

`review_r6_real_sdk_proof_and_nonce_errors_must_not_settle` exercises the real error conversion. It also covers `IdentityNonceNotFound`, whose SDK documentation expressly describes a lagging-node result that should retry. That part proves the conversion/classifier behavior, not construction of that variant by a live DAPI response.

The name path has a broader reachable variant: pinned platform-wallet `wallet/identity/network/dpns.rs:260` flattens errors from `register_dpns_name` into `InvalidIdentityData`, including timeouts following a preorder/write. This maps to `Internal`. `names.rs:1119` returns the original flattened error when the reconciliation lookup fails or finds no conclusion. `review_r6_flattened_name_network_error_must_not_settle` reproduces this envelope and the row ends Done with `name_failure="internal"`. Thus a genuine network/unknown-write outcome can settle despite the explicit facade Timeout/ProofInvalid arms being correct.

**Fix:** preserve/classify error provenance before flattening. Treat unproven execution, transient nonce/read failures and unclassified operation errors conservatively. Settle only positively identified repeatable refusals; arbitrary `Internal` is not such evidence. Add production-envelope tests for the name timeout and SDK execution-proof error, alongside the existing direct-facade tests.

### R6-03 — Minor: call-window plus Broadcast is still a heuristic, not exact ownership

**Locations:** [built_by_the_call](/work/scratch/dp1-02-r6-sol/wt/rust/crates/dw-engine/src/platform/registration_flow.rs:1056), [which errors permit snapshot adoption](/work/scratch/dp1-02-r6-sol/wt/rust/crates/dw-engine/src/platform/registration_flow.rs:2133), [test assuming Broadcast is local](/work/scratch/dp1-02-r6-sol/wt/rust/crates/dw-engine/src/platform/registration_tests.rs:3016). Pinned platform-wallet `asset_lock/sync/reconstruction.rs:118` reconstructs Mempool/InBlock registration locks as **Broadcast**, with synthetic identity index **0** (`funding_family`, same file). `recovery.rs:187` also permits Broadcast reconstruction.

**Scenario:** a first registration's local funding is definitely rejected and untracked. During that call, the wallet sees another device's same-seed registration lock in the mempool/unconfirmed chain. It is newly tracked, Broadcast, account 0, index 0, and may have the identical amount. The driver's `built` boolean is true for `NotSent`, even though the error may mean no local lock remains. The reconstructed foreign lock passes every snapshot predicate and is automatically recorded as this row's own, overriding the definite local rejection. Finish was never invoked.

**Evidence:** `review_r6_foreign_broadcast_during_rejected_call_must_wait_for_finish` injects a distinct outpoint after the rejected local call and shows it being attached automatically. `review_r6_reconstructed_broadcast_during_failed_call_is_not_own` isolates the predicate. The pinned reconstruction code supplies the production status/index basis; these are offline evidence injections, not a claimed two-device network experiment.

No loss/double funding was demonstrated by this probe, hence Minor. It nevertheless leaves the manager's “any other stranded lock only via Finish” requirement unsatisfied. The local fence excludes another local builder, not SPV reconstruction of another device's transaction.

**Fix:** capture a genuine local Built/builder event or the call's own returned outpoint, with its source, before relying on the outpoint later. Without that evidence, leave a newly reconstructed Broadcast lock for explicit Finish. Add the non-final reconstruction case to the ownership tests; the present synthetic status test asserts the unsafe assumption.

### R6-04 — Minor: profile reconciliation swallows read failures and can settle from stale absence

**Location:** [post-put read and settlement](/work/scratch/dp1-02-r6-sol/wt/rust/crates/dw-engine/src/platform/registration_flow.rs:2453).

**Scenario/evidence:** after a refused put, `landed.is_ok_and(...)` turns every recheck error into false and then classifies the original put error. `review_r6_refused_profile_recheck_must_preserve_errors` gives the put a repeatable InvalidArgument refusal, then the verification read ProofInvalid: the run succeeds and the row becomes Done with `profile_failure="invalid_argument"`. The same branch suppresses TrustMismatch, Unavailable, Storage and NetworkNotOpen instead of applying the manager's wait/end/stop rules.

`review_r6_stale_absence_must_not_settle_duplicate_profile` supplies two stale `None` reads while the exact requested profile exists on Platform. The non-idempotent fake rejects the duplicate. The row settles Done with `profile_failure="internal"`; a later fresh read cannot reconcile it because Done does not run the profile step. The SDK verifies query proofs and freshness, but `fetch_many` drops metadata here, and the pin's default one-block height tolerance is not read-after-write consistency (`sdk.rs:1123` documents this). A recent authentic absence proof can predate the successful put. The probe scripts read results; it does not claim a forged proof was accepted.

**Fix:** handle the recheck's Err explicitly through `after_identity`, including returning Storage and stopping on session closure. Keep duplicate/revision-race and inconclusive stale-read outcomes reconcilable until a sufficiently current query resolves them. Add failed-recheck and stale-absence regressions. The happy-path read-before-write and matching-profile recheck do work.

## Opus item closures and manager rulings

- **MJ-1: original name-taken wedge closed; overall settlement ruling incomplete (R6-02/R6-04).** Taken/Locked/ContestOpen/Invalid settle with the name code and still publish the profile; genuine profile InvalidArgument settles; earlier readable failed-for-good rows with identities no longer gate start and may be discarded. Direct known unknown/proof/trust/network variants are handled conservatively. The production-envelope/recheck gaps above remain.
- **MN-1: partial, not closed (R6-01/R6-03).** Untapped index-0 Broadcast attribution, three-attempt writes, unfailed KeysPrepared after persistent record failure, same-process own_lock recovery, restart waiting for Finish, and concurrent-Finish deferral pass their committed tests. The restored `reconstructed_nonzero_index_does_not_fund_twice` function is byte-for-byte identical to 7e331dd. The proof race creates a second row, and non-final foreign reconstruction defeats the narrowed provenance claim.
- **MN-2: closed.** A successfully persisted definite rejection clears `backed_up`; the retry schedules the second backup. Its mutation is checked independently.
- **MN-3: partial, not closed.** Read Platform → match/differ/absent behavior and field-preserving updates are implemented; the fake rejects duplicate creates, and the committed kill matrix includes a post-put interruption. Rechecks and production error classification still have R6-02/R6-04 gaps. No direct proof-verification bypass was found.
- **MN-4: the nine named coverage gaps are closed by the new rule tests and independent mutations.** B/I/N/O/W/G/H/R/F have focused controls. This does not certify the missing proof-arrival, real-error-envelope or stale-read cases above.
- **N-1: closed by the new migration's explicit clarification.** The old migration is retained unchanged, appropriately preserving its checksum.
- **N-2, N-3, N-4, N-5, N-6, N-8, N-9: retained non-blocking suggestions, not addressed in this range.** Respectively: stale module writer description; defensive funding gate explanation; ChainLock wait copy for an attribution wait; known-index-first Finish ordering; unused redeemed grant comment; explicit ignored CAS result; actor lifetime documentation. No new evidence makes these earlier nits blockers.
- **N-7 / manager ruling 5: closed.** Finish catches a row's run/claim error, accounts for that row as pending and continues. The committed storage-fault test produces `(resumed, completed, still_pending)=(2,1,1)` and completes the other row. Aggregate initial-import/read errors still fail Finish as a whole; the ruling concerns per-row errors.
- **Manager ruling 1: incomplete** because of R6-02/R6-04, despite the ordinary MJ-1 closure.
- **Manager ruling 2, including 18a127c narrowing: incomplete** because of R6-01/R6-03. The intended same-process/restart separation is implemented for the tested Broadcast case, but the attribution predicates do not prove source.
- **Manager ruling 3: satisfied** (MN-2).
- **Manager ruling 4: incomplete** (MN-3 reconciliation edges).
- **Manager ruling 5: satisfied** (N-7).

## Point 6 verdict

**Accept the conservative discard trade-off for Phase A.** After a restart, an outpoint-less KeysPrepared row beside a stranded lock cannot distinguish its own lost funding from another device's funding. Letting it discard allows the exact sequence the manager forbids: remove the intended row, then Finish imports the same value into a second row. Deriving MaybeSent from the row that Finish would fund protects against that sequence and prevents a replacement funding. A false-positive discard refusal is preferable while provenance is unavailable; explicit Finish is an authorized recovery action and Phase B's durable signed-txid marker is the proper way to narrow this later.

This acceptance depends on Finish recovering the existing row. R6-01 breaks that condition for a non-retryable broadcast_unknown row; fix it without weakening the conservative discard gate. Also retain N-4 as a UI suggestion: ChainLock alone does not tell a user that Finish is required.

## Validation and limits

- **Exact-head local build:** `cargo test --locked --offline -p dw-engine -p dw-appdb --lib --no-run` succeeded in the detached worktree, with its owned target. The initial capture wrapper caused jobserver-fd warnings in a few dependency builds; the wrapper was corrected before engine compilation. These were tooling warnings, not source diagnostics.
- **Unmodified engine library suite:** **481 passed, 0 failed, 2 ignored**; [log](/work/scratch/dp1-02-r6-sol/engine-tests.log). The broad run includes registration, rows, journal, recovery, session, coin/owner, lease and backup coverage. Both lease stress tests completed successfully.
- **Unmodified appdb suite:** **59 passed, 0 failed**; [log](/work/scratch/dp1-02-r6-sol/appdb-tests.log), including schema/checksum and wallet-row migration/export tests. The new nullable columns also flow through the existing column-name-based backup import/export.
- **Formatting and range whitespace:** `cargo fmt --all -- --check` and `git diff --check 7e331dd..18a127c` passed. `git apply --check` accepts the review-only patch against the unchanged worktree.
- **Independent targeted mutations:** **17/17 killed**, each with a passing unmutated control run alone: the nine Opus MN-4 rules **B/I/N/O/W/G/H/R/F**, plus **OLD_HEURISTIC, OWN_MEM, DESTINED, FINISH_CALL, MJ1_UNKNOWN, BACKUP, PROFILE_READ, PROFILE_RECHECK**. These are independent semantic probes, not a claim to have replayed the implementer's identically numbered mutation suite. [Results and exact killing tests](/work/scratch/dp1-02-r6-sol/mutation-results.json), [runner output](/work/scratch/dp1-02-r6-sol/mutation-runs.log). I is killed by the bounded stopped-run assertion (20-second eventually check), consistent with the injected lost-generation loop; the other 16 kills are state/data assertions. There were no outer process timeouts or baseline wall-clock failures.
- **New regression probes:** **seven fail under original behavior**, covering the four findings; [full evidence](/work/scratch/dp1-02-r6-sol/fresh-probes.log). They are intentionally failing assertions of required behavior, not failures of the unmodified committed suite. The first four also failed in a separately built pre-mutation probe artifact.
- **Diagnostic controls:** **5/5 pass**: broadening the post-call status filter removes the own-proof race; treating Internal as uncertain removes the real-SDK, flattened-name and stale-duplicate failures; handling the secondary read error removes the recheck failure. The latter control exercises ProofInvalid, TrustMismatch, Unavailable, Storage and NetworkNotOpen. [Results](/work/scratch/dp1-02-r6-sol/diagnostic-results.json). These are causal controls, not proposed production patches: broad status adoption remains unsafe under R6-03, and real refusal classification needs typed provenance.
- **Reproducible review-only changes:** [patch](/work/scratch/dp1-02-r6-sol/review-probes-and-mutations.patch), [compiler recipe](/work/scratch/dp1-02-r6-sol/review-compiler.json), [mutation source generator](/work/scratch/dp1-02-r6-sol/add-mutations.py), and [mutation runner](/work/scratch/dp1-02-r6-sol/run-mutations.py).

Heavy copied dependency and incremental build directories were removed after verification. The unmodified engine/appdb binaries remain under [artifacts](/work/scratch/dp1-02-r6-sol/artifacts), and the review binary, source archive, patch and logs remain available. Recompiling the saved recipe requires rebuilding its dependency target first.

The original worktree is unchanged. No implementation edits, commits, pushes, GitHub actions/comments, testnet funding or private-path reads. All builds/tests ran nice 10, CARGO_BUILD_JOBS=8, umask 022, disk TMPDIR `/work/scratch/dp1-02-r6-sol/tmp` (0700), private fixture directories, and scratch-owned targets. Dependency artifacts were copied into the owned target; no shared CARGO_TARGET_DIR was used. Review-only probes/mutations are in a separate exact-head archive.

## Assumptions and remaining boundaries

The supplied 1033/0 whole gate and prior mutation counts are implementer evidence; they do not replace the independently reproduced counterexamples. Tests here are offline trait-boundary/state injections with actual appdb rows and error conversion. No real DAPI, chain, multi-device funding or process-kill experiment was performed in r6; the reachable pin behaviors are identified in source. Previously accepted Phase B/P2b limitations remain outside this narrow gate.

**Verdict:** REQUIRES CHANGES — MN-1 recovery/provenance and post-identity settlement/reconciliation remain incomplete.
Sol gate satisfied: no
