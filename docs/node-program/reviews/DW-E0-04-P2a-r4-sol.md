# DW E0-04.P2a — Sol review, round 4

R3-F1 is closed and DEC-160's compaction rule is implemented. One major remains: the acknowledged failed-erase/sighting residual really loses the step association on recovery, even after the Sent write succeeds. The erase window protects the ordinary in-flight Sent schedule, but is not a complete substitute for an atomic, all-markers-plus-Sent journal operation.

## Scope

- Reviewed `973e1da7570cbbe812d91b8a7d35a67b41f7cd10` → `92d676b404ec103d28534197cbe2701d1bd7cab2` in `/work/home/agent/workspace/dw-wt/e0-04-p2a-leases`, branch `dw/e0-04-p2a-leases`.
- Read REVIEW-COMMON, the r3 Sol report and original probe source, DEC-160/DEC-154, the actual diff, final journal/fence/removal/close code, amended design and relevant regressions/checker. Applied the code-review-validator workflow. PLAN §7 handling: codenames only.
- This is a read-only branch review. No source file in the desktop worktree was edited, including temporary test includes. Independent Rust probes were included in a **scratch source copy** and compiled with the worktree's existing dependency artifacts into its own `rust/target/review-r4/`. No commit, push or GitHub action was made. This report is uncommitted.

## Findings

### R4-F1 — major / High: a successful sighting after an ambiguously committed erase persists Sent without its step markers

**Locations:** `rust/crates/dw-engine/src/platform/lease/fence.rs:534`, `:1714`, `:1757`, `:1552`, `:2539`; journal calls at `rust/crates/dw-appdb/src/dispatch.rs:451` and `:477`; recovery at `rust/crates/dw-engine/src/platform/lease/fence.rs:1119`. The installed test masks the gap at `rust/crates/dw-engine/src/platform/lease/tests.rs:3069`–`:3075` by transporting a repairing copy before reopening.

The independent `review_r4_failed_committed_erase_then_sighting_before_any_copy` reproduces this without a mutation:

1. A marked First finishes NotSent; its marker and NotSent row are durable and compactable.
2. Removal's ruled DELETE commits, but the fault-injecting backend reports an error. `doubt_wallet` correctly leaves the cached superseded marker Ambiguous.
3. A subsequent `note_seen` restores that cached marker into `steps`, but `begin_sent_row` starts `Target::Sent(Vec::new())`. It appends **only Sent** and acknowledges that evidence Durable.
4. Reopen immediately, before any copy, subsequent successful journal write or close can repair the marker.

Observed:

```text
raw rows: Resolved(Sent) only
standing markers: 0
in-memory step marker: Some(Ambiguous)
identical bytes under dead authority:
  Refused { cleanup: false, step_possibly_dispatched: false }
different bytes for that step under dead authority:
  Refused { cleanup: false, step_possibly_dispatched: false }
```

The required standing-marker assertion fails, exit 101. Recovery loads finality only through the returned `StepRow`s; a bare Sent row has no step association and produces no such row. The final row survives the startup sweep, but its artifact/step dispatch history is unusable by the fence. A process exit at this point loses an **acknowledged** sighting's step evidence. This is stronger than the accepted DEC-154 residual where the owed Sent write itself never succeeds before a crash.

The installed failed-erase regression passes because `r3_transport` first invokes `admit`, which sees the Ambiguous marker, writes it, and repairs the state before checking reopen. Its transport check remains useful; it does not establish recovery safety before that repair. A successful close retry is likewise not a substitute for the missing evidence during the acknowledged interval.

**Required correction:** route **every Sent write for a marked artifact**—first sighting, repeat sighting, admission repair, successful-write retry, erase repair and close retry—through one backend operation that writes the artifact's complete current marker set and Sent in **one SQLite transaction**. Include markers cached as Durable as well as Ambiguous: a delete may have removed a Durable marker behind a queued sighting. Gather superseded/current step identities under J; do SQLite I/O outside J; acknowledge the bundle only after commit. Failed/possibly committed bundles retain the existing stronger obligation. Add a regression that reopens immediately after Sent becomes Durable, without invoking a repairing copy first.

Merely passing marker names in more `Target::Sent` calls is insufficient for the general guarantee. `spawn_write` currently invokes `insert_step` separately for each name and then `resolve`; each obtains/releases the backend connection and performs its own SQLite statement. **One blocking task is not one SQLite transaction.**

This is a supported fence/journal primitive witness, within the gate's requested recovery guarantee. Today's production TxDraft consumer supplies no resumable step; this review does not claim an end-to-end production spending exploit.

### R4-N1 — nit: the design overstates the startup sweep's transaction mode

**Locations:** `docs/design/E0-04-grants-leases.md:1430`; `rust/crates/dw-appdb/src/dispatch.rs:341`, `:378`.

The design says both deletes run in IMMEDIATE transactions. Wallet removal does, but startup uses `conn.transaction()` and invokes the sweep within that default deferred transaction. The predicate is still inside the actual DELETE and the startup precondition excludes this session's dispatch writers, so this does not reopen R3-F1. Describe the actual startup transaction, or deliberately change its mode with appropriate validation.

## Requested confirms

### 1. R3-F1 closure

**Confirmed.** All five installed `r3_f1_` regressions pass. The two original removal witnesses are now regressions: the idle possibly-out artifact retains its standing marker, and a copy attempted during erase defers and then transports on retained evidence. Both fail under `EraseAll`: the idle test finds no log; the copy test gets Refused instead of Resend after removal erased its evidence. The second test's admission timing now reflects the new deferral, but it checks the original safety requirement at transport/reopen.

### 2. DEC-160 as ruled

**Confirmed for the ruled compaction behavior.**

- Rust `compactable` (`dispatch.rs:186`) excludes every artifact with a standing marker or any Sent/MaybeSent row. SQL `COMPACTABLE` (`:205`) expresses the same rule: no final row and every marker has a later artifact-wide NotSent row. With a final row, the whole history is retained.
- `compact` (`:217`) evaluates that predicate in the DELETE's subquery and returns erased wallet/artifact pairs. Wallet removal wraps dispatch deletion, seeded-mark deletion and compaction in an IMMEDIATE transaction (`:584`–`:593`). There is no asynchronous check-to-delete eligibility gap.
- Startup calls the same compaction SQL before publishing the journal. The 300-log SQL/Rust agreement test and the journal preservation tests pass.
- `apply_erase` (`fence.rs:2534`–`:2567`) limits step/evidence/superseded/row-less cleanup to returned artifact IDs. Kept artifacts survive in both memory and SQLite. An erased artifact sighted during the window is retained and rewritten rather than forgotten. The registered dispatch table keeps its separate existing entry check.

The new R4-F1 concerns the subsequent ambiguous-write/sighting protocol, not a violation of the DELETE predicate.

### 3. Is the erase window correct and necessary?

**It protects the ordinary success-path race in the current implementation, but is incomplete on R4-F1 and should not be the durable-evidence mechanism.** The independent semantic in-flight probe has one standing marker with the window and zero with `NoEraseWindow`, after waiting for the Sent SQLite write before reopening. This confirms the motivation against the current bare-Sent API.

**Answer to the proposed invariant: no, a Sent row cannot commit without its bundled current markers under the ruled DELETE if every Sent operation really includes them in the same SQLite transaction.** Let S denote that transaction and D the DEC-160 delete:

- S commits before D: the final row makes the predicate retain the artifact's entire log.
- D commits before S: S writes/reinstates the complete marker set and Sent together.
- They overlap: SQLite serializes their writes into one of those orders. No other connection observes a partial bundle. A later NotSent cannot override finality.
- S rolls back: no partial acknowledged bundle is published. A commit followed by an error is handled conservatively, and its next attempt carries the complete set again.

I exercised the proposed boundary using the **actual reviewed marker, resolution, schema and compaction SQL**, extracted directly from `dispatch.rs`: **1,440 file-backed transaction orderings pass**, from empty and settled histories, with two Sent bundles, two NotSent writes, two deletes, and reopen. The separate-call control (`marker → NotSent → delete → Sent`) produces a bare Sent. A rollback after the first marker publishes no partial bundle. This validates the boundary, not an installed production fix or physical-power-loss behavior.

**Ruling by purpose:**

| Window component | With atomic all-markers-plus-Sent |
| --- | --- |
| Record a sighting as Sent-owed solely because erase is active | Redundant for disk safety. Begin the usual counted atomic bundle; retain the ordinary failed-write obligation. |
| Pause NotSent writes solely to protect markers from compaction | Redundant for disk safety. Either bundle ordering is safe, and finality outranks NotSent. |
| Retry/join Sent writers before allowing DELETE | Redundant for this disk invariant. DELETE is safe before or after an atomic bundle. Keep ordinary retry opportunities for failed obligations. |
| Defer copies during erase | Unnecessary for the SQL evidence invariant, but **not removable mechanically from the current memory protocol**. It presently also protects memory cleanup/publication. |
| Count the blocking delete for close; let it own its outcome after caller cancellation | Retain. These are operation-lifetime and session-ordering requirements, independent of the evidence window. |
| Removal barrier and in-flight register join | Retain. DEC-134 and the separate dispatch table are unaffected by the step-log argument. |

**Recommended simplification:** make the atomic Sent bundle the single durable invariant. Remove erase-specific sighting debt and NotSent exclusion as evidence mechanisms; use the normal bundle writer/failed-write obligation. Retain only the synchronization needed to reconcile DELETE results with memory, or replace it with artifact operation versions that prevent a returned ID from erasing a newer state.

That memory qualification matters. Current `apply_erase` preserves a returned ID only when evidence is **Ambiguous** (`fence.rs:2552`). Without the window, a bundle can be Committing or Durable after D but before memory reconciliation; forgetting that ID would discard newer evidence despite correct SQLite rows. Already-running marker/resolution writers also need their results reconciled before terminal state is discarded. A sound removal must preserve newer/pending state and forget only the erased terminal incarnation. Until that is implemented, keep a narrow publication/cleanup guard and the necessary writer reconciliation. **Do not simply switch on `NoEraseWindow` after adding a SQL transaction and call the whole protocol proved.**

I do not rule the present full window necessary: its cited durable-evidence race is made unrepresentable by the atomic bundle. Nor do I approve an untested deletion of all its memory-ordering functions.

### 4. Listed residuals

- **Failed erase after committed delete, then sighting:** real recovery evidence loss, **R4-F1**, not accepted for this gate. A complete atomic Sent-plus-markers operation closes this sequence, including before any copy repairs it. The existing split operation does not.
- **Checker gap for failed NotSent writes:** real coverage limitation, pre-existing. `Superseding` is logged when the write begins (`fence.rs:2206`), and the checker makes markers non-durable immediately (`stress_tests.rs:431`) without recording whether the database append actually committed. The fake-journal transport oracle and targeted failed-resolution tests supplement it; passing the checker alone does not establish all failed-write schedules. No new production defect was demonstrated from this gap in this narrow review.
- **Index nit:** non-blocking. `step_log_artifact(wallet, artifact, seq)` serves the correlated artifact and later-NotSent lookups. EXPLAIN QUERY PLAN shows the nullable wallet-filter form scans the outer covering index even for a wallet-specific erase. Separate wallet/all-wallet SQL forms could let the outer selection seek by wallet; avoid claiming a correctness failure or measured performance regression from that plan alone.
- **Best-effort checkpoint:** accepted. A checkpoint failure after a successful commit no longer falsely reports the delete failed; returned erased IDs still reconcile memory. This does not remove the general ambiguous-commit requirement.

### 5. J, deadlock and close/removal ordering

**No new J re-entrancy or lock-cycle defect found.** Journal-slot reads happen outside J. Under J, window/retry helpers update state and schedule blocking work; they do not execute SQLite calls or recursively acquire J. Backend calls finish before their tasks acquire J to apply results. `EraseWindow::drop` gets the backend outside J and invokes the retry helper with the already-held `Inner`, rather than reacquiring the mutex. Waiters register before checking predicates, avoiding a lost wake-up.

The blocking delete owns the window and reconciliation. My independent cancellation probe aborts the erase's awaiting caller while the backend is held: copies remain Deferred, close remains pending, then releasing the backend lets close finish and the standing marker survives reopen. It passes.

Production removal holds an operation guard from `wallets.rs:217` through `erase_dispatch_rows`; session close waits for those operations through `gate.close()` (`session.rs:618`) before reaching `close_journal` (`:677`). This is important because the primitive's preflight is not yet counted as a delete. Once scheduled, the delete is counted through applying its outcome, and any repair it starts is counted too. Preserve that session gate and cancellation ownership in a simplification. No synchronous journal call runs while J is held.

## Independent validation and limits

All builds/tests/probes used umask 022, `CARGO_BUILD_JOBS=8`, `RUSTC_WRAPPER=`, `nice -n 10`, and TMPDIR `/work/scratch/e0-04-p2a-r4-sol/tmp`, checked mode 0700 and removed after validation. No `/dev/shm` or shared target directory was used. Cargo and direct rustc compilation used this desktop worktree's own target. The first probe compiler inherited an additional nice increment; the reproducible compiler helper was corrected to inherit the launcher's nice setting once.

| Check | Result |
| --- | --- |
| `cargo test -p dw-engine --lib r3_f1_ --locked -- --nocapture --test-threads=1` | 5 passed. |
| HEAD's test binary, `platform::lease --nocapture`, `DW_LEASE_STRESS_SEEDS=10` | 81 passed; all ten positive seeds and twelve stress mutations pass their expected gates. The positive seeds erase the flows' own wallet 17–39 times each. |
| `cargo test -p dw-appdb dispatch::tests --locked -- --nocapture` | 14 passed, including the 300 random-log predicate comparison. |
| Both original removal regressions with `DW_LEASE_MUTATION=EraseAll` | Each fails, exit 101, for missing retained evidence. |
| Independent failed-erase/sighting probe, pristine production source | Fails, exit 101, for bare Sent and lost recovery association: R4-F1. |
| Independent caller-abort/close probe | Passes. |
| Independent in-flight Sent semantic probe | Passes normally; fails with `NoEraseWindow`, exit 101, for zero standing markers after Sent lands. |
| Atomic SQL ordering probe | 1,440 orderings pass; separate-call negative control reproduces bare Sent; rollback control passes. |
| `python3 -I docs/design/checks/e0_04_design_model.py` | 86 PASS, final `ok`; this model does not establish the new erase-window schedules. |
| `cargo fmt --all --check` | Pass. |
| `git diff --check 973e1da 92d676b` | Pass. |

The installed `NoEraseWindow` running-Sent test failed twice at concurrent reopen with SQLite DatabaseBusy, before its intended evidence assertion. I therefore do **not** count those runs as semantic mutation evidence. The independent semantic probe waits for the actual Sent append before reopening and deterministically reports the missing marker instead.

Evidence directory: `/work/scratch/e0-04-p2a-r4-sol/`. Important files: `fix.diff`, `r3-regressions.log`, `lease.log`, `appdb.log`, `negative-eraseall-{idle,copy}.log`, `review-witness.rs`, `review-witness.log`, `semantic-window-{control,mutation}.log`, `atomic-sql-proof.py`, `atomic-sql-proof.log`, `model.log`, and `compile-probes.py`. The compiler helper reconstructs the captured Cargo test invocation against a scratch source snapshot; the reviewed source stays unchanged.

I did not rerun the full engine/workspace suite, the integration gate, later consumer integration or a filesystem power-loss test. The SQL ordering probe uses Python's SQLite; the Rust witness uses the real DispatchJournal and its bundled SQLite. No private PLAN §7 material was accessed.

**Verdict:** CHANGES-REQUIRED

Sol gate satisfied: no
