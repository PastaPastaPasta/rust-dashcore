# DW-DP1-02 Phase A — Sol r7

Reviewed **a2087646f81f3987f89ff740361f7355a0015583**, detached at `/work/scratch/dp1-02-r7-sol/wt`, against **11bbb1f520d5fbbdec8679e861c1cdbbaee9c194**. Scope is the **19 signed DP1-02 commits** above that base. P2b's separate r3 fix round is outside this review; its permit, signature accounting and journal are treated as the supplied boundary.

The seven r6 regressions pass, and their reported failure mechanisms are repaired. **Changes are still required:** two other paths defeat the new ownership rule. One permanently hides a returned, funded asset lock from Finish; the other gives a signed lock to a different restored row and wedges its original registration.

## Closure status

- **R6-01 — Closed for the reported proof-arrival race.** Ownership now comes from the step's recorded signed txid, independent of the lock's proof/status or synthetic index. The regression passes both with provenance and without it: explicit Finish gives an unsigned/unattributed lock to the original `broadcast_unknown` row. Overall Finish recovery remains incomplete because of **R7-01** below.
- **R6-02 — Closed under the manager's specified settlement policy.** `after_identity` uses the explicit refusal list; other `Fail` classifications become a Network wait. Actual SDK `ExecutionNotProved` and `IdentityNonceNotFound` conversions still yield `internal`, but no longer settle. The real library flattened DPNS envelope also waits. Both regressions pass. The list is not a promise that budgets, balances or keys can never change; see the allow-list discussion below.
- **R6-03 — Closed for the call-window/Broadcast heuristic.** That heuristic and `own_lock` are gone. A foreign reconstructed Broadcast lock at synthetic index 0 is neither adopted nor funded over; resume refuses. Both regressions pass. The manager-approved index restore exception remains, and a separate identity restore branch fails to respect contrary signed provenance (**R7-02**).
- **R6-04 — Closed.** Both initial and reconciliation profile reads use `after_read`. A failed recheck never settles; Storage is returned, session closure stops, and proof/trust/network errors wait. The regression exercises all five error families. The stale-absence/duplicate regression waits, then completes on a fresh matching read.
- **MN-2 / M-2 — Correct at this pin.** A signed rejection without a tracked lock remains possibly sent. A tapped outpoint stays funded, with its backup armed; an untapped row fails non-retryable `broadcast_unknown`. The changed r6 assertion is correct. Re-arming a backup for a signed, rejected funding is unreachable under M-2; `a_rejection_after_signing_keeps_the_row_and_its_backup` is the appropriate replacement.
- **Point 6 — Acceptance retained.** Refusing discard beside a stranded lock is still the accepted Phase A trade-off. This acceptance does not cover giving away known signed ownership or losing recovery of that lock.

## New findings, by severity

### R7-01 — Blocker: Finish substitutes another lock, then permanently filters out the row's original signed funding

**Locations:** [putting a row with no currently tracked signed lock in the general waiting list](/work/scratch/dp1-02-r7-sol/wt/rust/crates/dw-engine/src/platform/registration_flow.rs:1482), [amount-based assignment](/work/scratch/dp1-02-r7-sol/wt/rust/crates/dw-engine/src/platform/registration_flow.rs:1504), [excluding every lock signed by any existing row](/work/scratch/dp1-02-r7-sol/wt/rust/crates/dw-engine/src/platform/registration_flow.rs:1065), [waiting-row eligibility](/work/scratch/dp1-02-r7-sol/wt/rust/crates/dw-engine/src/platform/registration_flow.rs:978).

**Scenario:** row A signs T1, whose txid is recorded by P2b. The appdb tap does not record its outpoint, and the library rejects/untracks T1. M-2 correctly leaves A at `KeysPrepared`, non-retryable `broadcast_unknown`. Another same-seed lock T2, reconstructed with index 0 and enough value, is available when the user runs Finish. `ev.signed_lock(A)` returns None because T1 is not tracked, so A joins the general waiting list. Finish attaches T2 to A and completes its identity/name/profile.

Later T1 is relayed and reconstructed with a proof, the outcome M-2 explicitly requires us to preserve. T1 remains signed by A, so `unheld()` excludes it. A is now Done and holds T2, so neither the waiting-row import nor A's driver takes T1. Every subsequent Finish reports nothing pending and gives T1 no recovery row. A signed provenance record has become a permanent suppression record for a different, unconsumed lock.

**Evidence:** `review_r7_finish_substitution_must_not_orphan_a_signed_lock` fails deterministically, including when rerun alone. It uses the real lease table, signed-lock record and appdb writer with the committed Fake library; no implementation predicate is changed.

```text
A signed: T1 = 01…01:0; first call rejected, no tracked T1 or appdb outpoint
first Finish: resumed=1, completed=1, still_pending=0
A: Done, outpoint=T2=63…63:0
T1 returns as RecoveredFromChain, unconsumed
two later Finish calls: resumed=0, completed=0, still_pending=0
rows holding T1: 0; funding calls / identity creates: 1 / 1
```

[Individual run](/work/scratch/dp1-02-r7-sol/r7-finish-alone.log). This proves a recovery orphan, not destruction of the cryptographic key or a live network loss experiment. It strands the funded value through the supported recovery path, which is a money-safety blocker. No second funding is needed to trigger it.

**Fix:** distinguish “this step signed no known lock” from “this step's known signed lock is temporarily absent.” A row with outstanding signed provenance must remain reserved for that lock until a definite resolution; do not substitute an unrelated lock merely because the signed lock is absent. Apply that constraint to both Finish and automatic index restore. Also handle an already inconsistent row that holds T2 but signed unconsumed T1: keep T1 visible and recoverable, or explicitly pending. Simply removing the signed-lock exclusion everywhere would reopen ownership races. Commit the rejected-T1 → Finish-with-T2 → proved-T1 regression.

### R7-02 — Major: the identity restore fallback bypasses another row's exact signed ownership

**Locations:** [identity-at-index fallback](/work/scratch/dp1-02-r7-sol/wt/rust/crates/dw-engine/src/platform/registration_flow.rs:1139), [held-outpoint-only ownership test](/work/scratch/dp1-02-r7-sol/wt/rust/crates/dw-engine/src/platform/registration_flow.rs:1147), [later shared-index check](/work/scratch/dp1-02-r7-sol/wt/rust/crates/dw-engine/src/platform/registration_flow.rs:1164), [adopting its answer](/work/scratch/dp1-02-r7-sol/wt/rust/crates/dw-engine/src/platform/registration_flow.rs:2026).

**Scenario:** original row A signed a lock at index 0, but persistent appdb record failures leave A unfailed at `KeysPrepared` without an outpoint. The signed txid remains recorded. Wallet discovery finds the identity created from that lock. A different restored row B also has `KeysPrepared`, index 0 and no outpoint. Shared indexes in rows restored behind reality are an explicitly handled condition, not a uniqueness invariant.

B's own signed-lock lookup finds nothing. `unheld()` correctly excludes A's lock, but the next branch searches all locks for the identity at the index and tests only whether an outpoint is already held. It ignores `signed_for_another`, the original row's derived lock and the later shared-index protection. B therefore records A's lock and completes using its identity.

A's retry sees its lock held by B and changes to index 1. Its funding step still has the marker for index 0, so P2b correctly refuses the new key (`StepKeyMismatch`, mapped to retryable `internal`). Finish skips A's unfunded phase and reports no pending row; new registration remains `InProgress`. Signed provenance exists, but no supported run assigns the lock back to A.

**Evidence:** `review_r7_identity_restore_must_respect_another_rows_signed_lock` fails both in the grouped run and alone. It starts with the committed three-write-failure fixture, then models a discovery result and a second stale row.

```text
signed owner A: KeysPrepared, index=0, outpoint=None, signed txid=T1
restored B: Done, index=0, outpoint=T1, identity=identity_for(T1)
A after retry: KeysPrepared, index=1, internal, retryable=true
Finish: resumed=0, completed=0, still_pending=0
next start: Err(InProgress); funding calls: 1
```

[Individual run](/work/scratch/dp1-02-r7-sol/r7-identity-alone.log). This requires a stale/restored duplicate index and discovery of the consumed identity; it is not claimed to arise from fresh single-device index allocation. It falls outside the manager's accepted index ambiguity because contrary exact provenance is present.

**Fix:** use one ownership decision for every adoption path. The identity fallback must reject a lock signed by another row and honor that row's reservation before returning `IndexOwner::Lock`. Audit the index-only tap against the same invariant. Keep the original signed owner at its funding key and let its reconciliation complete; move only the unrelated unfunded row. Add the shared-index, signed-owner/discovered-identity regression.

## Focused judgments

**Known-index restore remains an index-based exception, not exact provenance.** Automatic adoption is possible when an account-0 registration lock retains `Built` metadata at index 0, or a nonzero index in any status, matching an outpoint-less row. The exception assumes such metadata identifies the earlier work the restored row should resume. The review control confirms adoption without any signature record for `(Built, 0)`, `(Broadcast, 3)` and `(RecoveredFromChain, 3)`.

A foreign lock can pass this rule if separately preserved same-seed wallet metadata carries the matching live index, or if a discovered same-seed identity at that index supplies its lock. Ordinary SPV reconstruction alone reports index 0 past Built and does not pass the direct known-index predicate; that r6 hole is closed. A genuine metadata/index collision remains the expressly allowed §3.4 ambiguity. Exact contrary signed ownership must nevertheless win, and R7-02 shows it currently does not.

**The allow-list matches the ruling but “repeatable” is conditional.** A larger/rebound grant can remove `grant_exceeded`; a top-up or a fresher balance query can remove `insufficient_credits`; a key update/discovery can change `keys_missing`; feature availability and name ownership/contest state can also change. In particular, the names flow constructs InsufficientCredits from a balance query ([names.rs](/work/scratch/dp1-02-r7-sol/wt/rust/crates/dw-engine/src/platform/names.rs:1077)), and the pinned SDK permits a response one block behind. The list cannot guarantee that retrying later with fresher state meets the same refusal.

Under the explicit manager policy, those results settle the *name/profile step* with a surfaced failure; the identity remains usable through `register_name`/`update_profile`, and the registration queue is released. I retain that policy rather than reopen its explicit categories as blockers. It does not guarantee eventual automatic success of the original requested name/profile after state changes. If that stronger behavior is required, low-balance/read-derived refusals need reconciliation or a park rather than terminal settlement. No additional proof/nonce/unknown-execution error was found masquerading as an allow-listed typed refusal on the reviewed pin; the known flattened cases remain `internal` and wait.

**M-2 is handled correctly until Finish chooses a substitute.** The guard permits `unfund` only when the step is neither Sent, WillBeSent nor MaybeSent. Both tapped and untapped signed rejections retain the required uncertainty. R7-01 is an integration failure after that correct decision, not an objection to P2b's M-2 ruling or the changed assertion.

**Normal Finish recovery survives a real close/reopen.** The additional positive control closes the actual network session, reopens it, verifies the signed txid was reloaded from `dispatch.sqlite`, and finishes the original appdb row with zero new fundings. The committed running-call, retired-owner, unreadable-row, kill-matrix and per-row-storage-failure controls also pass. These guards work in their covered cases; they do not prevent R7-01's completed-row orphan or R7-02's identity-branch reassignment. The durable signed-record/Done-row combination in R7-01 also survives a restart by mechanism.

## Validation and limits

- **Rebase/provenance:** all 19 commits verify `G`. `git range-diff backup/prerebase-18a127c~17..backup/prerebase-18a127c 11bbb1f..a208764~2` has 14 exact patch matches and three changes confined to context around P2b's `lock_txs` collection; the earlier DP1-02 edits are preserved. [Range-diff](/work/scratch/dp1-02-r7-sol/range-diff.txt), [pin/signatures](/work/scratch/dp1-02-r7-sol/scope.json).
- **Exact-head local build:** `cargo test --locked --offline -p dw-engine -p dw-appdb --lib --no-run` passes. [Build log](/work/scratch/dp1-02-r7-sol/build.log).
- **Seven r6 probes:** **7 passed, 0 failed**, run serially. [Log](/work/scratch/dp1-02-r7-sol/r6-probes.log). The foreign-Broadcast assertion follows the reviewed M-2 change; the removed heuristic probe is adapted to the new evidence API.
- **Unmodified engine library suite:** **513 passed, 0 failed, 2 ignored**, including both lease stress tests, all registration rules and the running-call/retired-owner controls. The ignored tests require explicit public-testnet fixture configuration. [Log](/work/scratch/dp1-02-r7-sol/engine-tests.log).
- **Unmodified appdb suite:** **62 passed, 0 failed**, including migrations and registration row/CAS behavior. [Log](/work/scratch/dp1-02-r7-sol/appdb-tests.log).
- **Formatting/whitespace:** `cargo fmt --all -- --check` and `git diff --check 11bbb1f..a208764` pass.
- **Additional review probes:** **2 failing required-behavior assertions, 2 passing controls** in the final review-only artifact. Each ownership failure also reproduces alone; neither is a wall-clock failure. The controls verify the accepted known-index behavior and actual session close/reopen recovery. [Final log](/work/scratch/dp1-02-r7-sol/r7-probes-final.log), [probe source](/work/scratch/dp1-02-r7-sol/r7-probes.rs), [appended-test patch](/work/scratch/dp1-02-r7-sol/review-probes.patch), [compiler recipe](/work/scratch/dp1-02-r7-sol/probe-compiler.json).

The supplied whole-gate **1068/0** and five mutation kills are implementer evidence; this narrow review does not claim to have replayed that full workspace gate or mutation suite. The new failures use offline library/discovery state injections with actual leases, signed-lock journaling and appdb rows. The positive close/reopen control reloads the real dispatch journal, but no real DAPI, chain or multi-device funding experiment was performed.

The original detached worktree is unchanged. No edits to implementation code, commits, pushes, GitHub actions, private-path reads or testnet funding. Review-only tests are appended to an exact-head source archive outside the worktree. All builds/tests use nice 10, `CARGO_BUILD_JOBS=8`, umask 022, disk TMPDIR `/work/scratch/dp1-02-r7-sol/tmp` (0700), private fixture directories and an owned target. Dependency artifacts were copied into that target; no target directory was shared with another worktree.

**Verdict:** REQUIRES CHANGES — Finish can orphan a signed funded lock, and identity restore can steal a different row's signed ownership and wedge registration.
Sol gate satisfied: no
