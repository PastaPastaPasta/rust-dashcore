# DW E0-04.P2a — Sol review, round 5

R4-F1 is closed by the atomic full-marker Sent bundle. The erase-specific evidence window is removed without losing the ordinary copy obligations. No blocker or major production finding remains. One minor finding is a reproducible test synchronization race.

## Scope and method

- Reviewed `92d676b404ec103d28534197cbe2701d1bd7cab2` → `6421be2fd00654a18940313a675612bab741210b`, branch `dw/e0-04-p2a-leases`, in the specified desktop worktree.
- Read REVIEW-COMMON, the r4 report and probe helpers, DEC-160/DEC-163, the actual five-file diff, the final journal/fence code, removal and close callers, the amended design and relevant regressions/checker. Applied the code-review-validator workflow. PLAN §7 handling: codenames only.
- Did not edit, commit, push or comment on the reviewed branch. Independent Rust schedules use a scratch source copy and the worktree's own dependency artifacts and `rust/target/review-r5/`. The only existing-test change in that scratch copy is the proposed synchronization fix described below. Production source is unchanged. This report is uncommitted.

## Findings

### R5-N1 — minor: the reported flake is a real race in the test's wait condition

**Location:** `rust/crates/dw-engine/src/platform/lease/tests.rs:2249`; safe admission checks at `fence.rs:588` and `:1501`; Sent acknowledgement at `fence.rs:1571`.

`r1_f2_a_marker_write_never_settles_a_sent_artifact` waits for the fake journal's rows to read Sent, then assumes the next admission must return Resend. The backend publishes those rows before its return and before `spawn_write` acknowledges the evidence under J. If the waiting test wins that interval, evidence is still `Committing`, so admission correctly returns Deferred and the test panics with **"expected Resend, got Deferred"**.

I reproduced **11 failures in 800 runs** under eight concurrent test processes: **7/400 without a mutation**, **4/400 with `ForgetNewer`**. Every failure has that same message. `ForgetNewer` is incidental: this test does not remove a wallet.

The deterministic scratch probe `review_r5_disk_sent_does_not_mean_the_fence_has_acknowledged_it` holds a real file-backed Sent bundle after commit but before the backend returns. Both markers and Sent are present on disk; the copy's first admission and its disk-based retry still return Deferred. Once the backend returns and J acknowledges Durable, the same copy returns Resend, both markers survive reopen, and the charge remains spent. This is a test race, not evidence loss, a refund bug or a production liveness failure.

**Suggested fix:** after the original copy returns Deferred, wait for `t.with_j(|i, _| i.fence.evidence_mark(&W, &a)) == Some(fence::Mark::Durable)` before expecting Resend. Keep the existing disk-marker/finality and no-refund assertions. Do not loosen production admission or add a fixed sleep.

With only that wait-condition correction in the scratch copy, **800/800 runs pass** under the same eight-process split: 400 baseline and 400 ForgetNewer.

## Requested confirms

### 1. R4-F1 and the transaction boundary

**Closed.** Both installed `r4_f1_` regressions pass. The original failed-erase/sighting regression reopens at `tests.rs:3109`, before a copy, repair or close, then proves identical bytes Resend under dead authority and different bytes are refused with `step_possibly_dispatched: true`.

The **literal** claim that this one original probe also fails under `SplitBundle` is not true: it fails under `BundleOwedOnly`, but passes under `SplitBundle`, because its delete precedes the split marker append. Atomicity is instead checked by the installed overtaken-bundle regression at `tests.rs:3139`: with `SplitBundle`, NotSent and DELETE occur between the separate marker and Sent operations, and the assertion finds **0 standing markers instead of 1**. The regression set therefore covers both failure modes. This distinction does not reopen R4-F1.

Re-ran the independent SQL proof against HEAD's actual `APPEND_MARKER`, `APPEND_RESOLUTION`, schema and `compact` DELETE string, and checked that actual `resolve_sent` invokes both append helpers in one IMMEDIATE transaction before commit. **1,440 file-backed WAL/FULL orderings pass**: empty and settled starts, two complete bundles, two NotSent writes, two deletes, and reopen. The separate-statement negative control reproduces bare Sent; rollback after the first marker publishes no partial bundle. The installed `dw-appdb` permutation regression also passes against Rust's real journal.

The invariant is now at the correct boundary (`dispatch.rs:529`): if the bundle commits first, finality excludes the artifact from DELETE; if DELETE commits first, the bundle reinstates all supplied markers with Sent. A later NotSent cannot undo finality. SQLite does not publish part of a committed bundle.

### 2. Every production final-row writer

**Confirmed.** The only production engine backend call writing Sent is `backend.resolve_sent` at `fence.rs:1544`. The direct `backend.resolve(..., Resolution::Sent)` at `:1542` is compiled only for the `SplitBundle` test mutation. There is no production MaybeSent row writer in P2a. Outcome/status/event enums named Sent or MaybeSent are not database writes.

`marker_set` (`fence.rs:549`) gathers every matching step plus the superseded cache under J, deduplicates the names, and does not filter by Durable/Ambiguous/Committing. First and repeat sightings and executed results use `begin_sent_row` → `begin_sent_bundle`; successful-write retries and close retry use the same bundle helper. Admission repair's Sent target also carries `marker_set`. SQLite I/O occurs in the blocking task, outside J. Ordinary failed-write debt remains; erase-time debt, the NotSent pause, Sent-writer joining, `doubt_wallet`, and the re-append after erase are gone.

### 3. No erase-time copy deferral; stale returned IDs

**The design argument holds**, with the ordinary Resolving/evidence/marker gates doing essential work:

- A durable, standing marker makes DEC-160's DELETE retain the artifact. A Resend based on it cannot lose it to that DELETE.
- A NotSent may have superseded a marker on disk while memory still calls the marker Durable. During that acknowledgement gap the artifact is Resolving, so its copies defer. Once NotSent is acknowledged, its tombstone/superseded cache is not Resend authority.
- A sighting restores the superseded identities and starts a counted full bundle. Copies defer while its evidence is Committing, or repair an Ambiguous bundle. A successful bundle includes cached Durable markers even if DELETE already removed their old rows.
- `forget_erased` (`fence.rs:776`) evaluates its guard in the same J step as the returned IDs are applied. Any current marker, any evidence state, or Admitted/Resolving row-less state preserves the artifact. A failed DELETE applies no forgetting. Only a terminal artifact with no current marker or final-row obligation is forgotten.

Independent schedules hold the DELETE **after it commits and computes its returned IDs**, allowing newer operations before the memory result is applied. **All four guard probes pass, including both pending and Durable bundle phases.** All four fail under `ForgetNewer`: it removes the new copy marker, loses the unleased Admitted state, drops the Resolving state, or makes the later Sent acknowledgement unable to find its evidence. The covered states are pending and Durable Sent bundles, a NotSent acknowledgement whose disk marker is already superseded, an unleased admission with no current marker, and a new copy marker. The unleased-admission probe exercises the fence primitive's guard, not an end-to-end removed-wallet consumer.

`ForgetNewer` is caught by both installed overlapping-bundle regressions; their evidence acknowledgement can no longer find its erased memory entry. No separate erase-time copy deferral is needed.

### 4. Flake

**Real test race, reproduced and isolated**, as R5-N1 above. The safe production answer in that interval is Deferred. It is not evidence against the bundle or forgetting guard.

### 5. Rows landing after removal

**Acceptable under DEC-160**, subject to the existing admission obligations. The DELETE can precede a queued marker append, which then becomes a new standing row before any dependent transport. This conservative residue is explicitly accepted; it cannot be treated as an erased durable marker. A delayed/retried NotSent can leave a bare NotSent row after DELETE, which carries no resend authority and is compactable at the next sweep. Any final bundle landing later brings its complete marker set and is retained.

**Both independent file-backed residue schedules pass.** They check refusal while resolution is pending, the bare NotSent row before sweep and its removal at open, and a cancelled First's delayed marker followed by a Resend only after the marker is Durable and standing. No wrong Resend or evidence loss was observed.

### 6. J, removal cancellation and close

**No new re-entrancy or lock-cycle defect found.** `forget_erased` only reconciles maps under J; it neither calls the backend nor acquires J recursively. Journal-slot access and SQLite work remain outside J. The delete increments the write count before spawning and decrements it while applying its own outcome under J (`fence.rs:2411`, `:2435`), so dropping the awaiting caller cannot skip that outcome. Close gives owed bundles their final retry, seals new normal writes, then waits for zero counted writes. Waiters enable their notifications before checking the predicate.

The installed abandoned-erase/close regression passes. Production removal retains its operation guard and barrier (`wallets.rs:217`, `:226`) through `erase_dispatch_rows`; close waits for the operation gate (`session.rs:618`) before `close_journal` (`:677`). This covers erase's register-join/preflight interval before it is counted; the standalone primitive is not an authorization to schedule new removals after journal close. The in-flight-register regression also passes.

R4-N1 is closed: design §6.2 now accurately describes startup sweep inside open's default deferred transaction, and removal and the Sent bundle in IMMEDIATE transactions.

## Validation and limits

All test/build/probe workloads ran with umask 022, `nice -n 10`, `CARGO_BUILD_JOBS=8`, `RUSTC_WRAPPER=`, and disk TMPDIR `/work/scratch/e0-04-p2a-r5-sol/tmp` mode 0700, removed after validation. No `/dev/shm`, shared target, branch edit, commit, push or GitHub action was used.

| Check | Result |
| --- | --- |
| HEAD `cargo test -p dw-engine --lib r4_f1_ --locked -- --nocapture --test-threads=1` | 2 passed. |
| HEAD lease suite, `DW_LEASE_STRESS_SEEDS=10`, four test threads | 83 passed, including all ten positive seeds and the twelve-mutation stress catcher. Each positive seed erases the flows' own wallet 17–39 times. |
| `cargo test -p dw-appdb dispatch::tests --locked -- --nocapture` | 15 passed, including the actual bundle permutation and 300-log SQL/Rust predicate comparison. |
| SQL proof on actual installed SQL | 1,440 orderings passed; split-write and rollback controls passed. |
| Seven independent Rust schedules against file-backed journal | 7 passed; all four guard schedules fail under ForgetNewer, as expected. |
| Original reported flaky test, eight concurrent processes | 11/800 failures, all expected-Resend/got-Deferred; 7/400 baseline, 4/400 ForgetNewer. |
| Same test with only the scratch wait-condition correction | 800/800 passed: 400 baseline, 400 ForgetNewer. |
| Installed bundle/removal mutation regressions | BundleOwedOnly caught by 3, SplitBundle by 2, ForgetNewer by 2, EraseAll by 5 including abandoned erase/close. |
| `python3 -I docs/design/checks/e0_04_design_model.py` | 86 PASS, final `ok`. |
| `cargo fmt --all --check`; `git diff --check 92d676b..6421be2` | Passed. |

Evidence is in `/work/scratch/e0-04-p2a-r5-sol/`: `compile-capture.log`, `appdb.log`, `lease.log`, `atomic-sql-proof.py`/`.log`, `mutations.py`/`.log` and individual mutation logs, `stress-flake.py`, `flake-stress.log` and eleven failure logs, `review-witness.rs`, `review-witness.log`, `review-witness-ForgetNewer.log`, `fixed-flake-stress.log`, `stress-fixed-flake.py`, `compile-probes.py`, the scratch source snapshot, probe logs, `model.log`, and `fmt.log`.

This narrow review does not repeat the full 703-test gate, consumer integration or physical power-loss testing. The SQL ordering proof uses Python SQLite; the independent Rust schedules use the actual DispatchJournal and its bundled SQLite. The existing stress checker's failed-NotSent coverage limitation described in r4 remains; passing that checker alone is not the justification for approval. No private PLAN §7 material was accessed.

**Verdict:** APPROVE-WITH-NITS

Sol gate satisfied: yes
