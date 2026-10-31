# DW E0-04.P2a — Sol review, round 3

**Verdict: CHANGES-REQUIRED (REQUIRES CHANGES).** The original R2-F1 delete/restore path is closed by the append-only journal and its evidence obligation. One High remains in wallet-removal compaction: it can erase a standing marker for an artifact already possibly on the wire, and a copy admitted between the eligibility check and erase can transport after losing its durable evidence. Consequently I cannot confirm DEC-154's complete compaction guarantee or accept the admit-versus-removal-erase residual for this gate.

## Scope and evidence

- Date: 2026-10-10. Reviewer: Codex, gpt-6.1-sol.
- Desktop worktree: `/work/home/agent/workspace/dw-wt/e0-04-p2a-leases`, branch `dw/e0-04-p2a-leases`, HEAD `973e1da7570cbbe812d91b8a7d35a67b41f7cd10`.
- Fix range: `d99102912a1c64a5aa4bcb2cc6e0277c1db1211f` → `973e1da7570cbbe812d91b8a7d35a67b41f7cd10`.
- Read the actual diff, final journal/fence/session implementation, r2 Sol report, DEC-154, amended E0-04 design, and implementer's final report, including decisions (a)–(e). The report was recovered from the implementation session `21fea979-eb75-4f0d-b2f2-1721c12b713c`; its extracted text is retained with the evidence.
- Source references below are relative to the desktop worktree and use pristine HEAD line numbers. Review-only probes were temporarily included in `lease/tests.rs`, then that file was restored byte-for-byte. No production fix, commit, push or GitHub comment was made. Desktop HEAD is unchanged and the desktop worktree is clean. This review file is deliberately uncommitted.

## Findings, by severity

### R3-F1 — High (major): wallet compaction deletes possibly dispatched step evidence, including beneath a newly admitted copy

**Locations:** `rust/crates/dw-engine/src/platform/lease/fence.rs:2254`, `:2264`, `:2273`, `:2291`, `:1744`, `:2308`; deletion at `rust/crates/dw-appdb/src/dispatch.rs:528`; acknowledged residual at `docs/design/E0-04-grants-leases.md:1427`.

`wallet_possibly_sent` checks registered `entries` and currently running `attempts`. It does not check idle row-less artifacts, standing step markers, final Sent evidence, or their writer obligations. `erase_wallet_rows` then releases J and performs an unconditional wallet-wide delete. The removal's barrier prevents new leases and registrations, but the row-less Resend path does not consult it. Adding the active-attempt check therefore protects only copies present at the check, not copies admitted afterward.

I independently reproduced both failures against a file-backed `DispatchJournal`, without enabling a mutation:

1. **No race is needed to lose possibly-out evidence.** Admit a step First, log its transport, and finish it `MaybeSent`. Its attempt ends, but its durable marker remains standing and its row-less state retains `possibly_out`. Freeze the wallet for removal, retain the barrier, drain, and call `erase_wallet_rows`. The erase runs and a reopened journal has **zero log rows**.
2. **The requested admission-versus-erase gap permits transport with no evidence.** Start from a durable, idle possibly-out step. Hold the backend's erase after the table has passed its eligibility check, with the removal barrier still held. Admit the same artifact and step under the revoked lease: it receives **Resend**. Release the erase and wait for it to complete. That guard still reaches the real `hand_off(None, ...)` transport closure, while SQLite has **zero standing markers**; reopening also finds **zero markers**.

Observed probe output:

```text
completed possibly-out step after removal: log rows=0
admit-vs-removal: copy=true; standing at transport=0; transported=true; reopened markers=0
```

Both required-behavior assertions fail, after successful compilation. This is a fence/journal primitive witness, not an end-to-end demonstration through today's unmarked TxDraft consumer. The current production TxDraft path supplies no resumable step, and later library consumers remain deferred. That limits present consumer exposure; it does not establish the expressly requested DEC-154 (4) guarantee. The ruling requires compaction only where nothing can be on the wire, and these supported primitive interleavings fail that condition. Recording that the gap predates DEC-154 is not a waiver of that new requirement. The r2 deferral concerned a marker writer landing after erase; it did not demonstrate or approve a copy transporting after erase removed its existing evidence.

**Required correction:** make the removal's admission exclusion and compaction eligibility one serialized protocol under J, with journal I/O outside J. Exclude new copies and journal work during erasure; retain evidence for idle possibly-out row-less artifacts, standing markers, final evidence and unresolved writer obligations; join or otherwise safely exclude already-started writers. Checking only the current attempt count, or merely rechecking it before an asynchronous delete, leaves another check-to-delete gap. Add both probes as regressions and exercise removal of the same wallet whose row-less step copies are transported.

The passing stress gate does not exercise this combination: its step-copy disk check uses **W** (`stress_tests.rs:649`), while its removal/erase worker removes **W2** (`:905`). Part 6 models resolution, sighting, copy and crash, not wallet erasure. Their positive results therefore do not refute this finding.

**No additional Medium, Low or nit findings in this narrow review.**

## R2-F1 closure and recovery

The original `keep_sent` deletion/restoration mechanism is gone. Resolution appends NotSent; a sighting begins an artifact-wide Sent obligation under J. Copies defer while that row is committing and rewrite it when ambiguous. A failed or panicking NotSent write remains `Resolving{failed}`, retains its charge, and is retried before another admission. Superseded markers retain their own write status, so an unwritten marker is repaired rather than incorrectly becoming Durable.

I reran the installed probes and independently translated the original r2 gated backend to the append API. Only the operation under test changes: the held/failed restore becomes the held/failed Sent row, and the committed delete becomes the committed NotSent append. The schedules, accounting checks, copy-before-transport assertion and reopen/dead-lease recovery checks remain. All three original cases and the successful control pass. Additional real-journal probes for NotSent committed then panic, Sent committed then error, and Sent committed then panic also pass. Each successful case retains raw marker, NotSent and Sent rows; identical bytes recover as Resend under dead authority, and different bytes for the step report `step_possibly_dispatched: true`.

**R2-F1 is closed for resolution/sighting/copy recovery.** That does not mean the overall "every copy has durable evidence" claim is closed: R3-F1 removes that evidence through wallet compaction.

`standing()` (`dw-appdb/src/dispatch.rs:185`) correctly groups by wallet and artifact, supersedes only markers preceding the latest NotSent row, and gives any Sent or MaybeSent row final precedence. It does not manufacture markers that never landed. The append statements are atomic SQLite writes with FULL synchronization, and the reopened probes preserve the expected evidence across both append orders and ambiguous acknowledgements.

The **startup sweep is sound within its startup precondition**: it removes all rows only for an artifact with neither a standing marker nor any Sent/MaybeSent row. Removing that terminal NotSent history preserves its recovery reading. Standing-marker artifacts and final-row artifacts retain their complete histories. Session close now waits for counted marker/evidence/resolution writers before the next session can open and sweep. The journal tests for supersession, finality and sweep all pass.

The **v1 migration is sound**: it copies every extant v1 marker into `step_log`, drops the old table and advances the schema in the same transaction. Every migrated marker stands, so the immediately following sweep retains it. A crash leaves either the old transaction state or the complete new state. Migration cannot reconstruct evidence already deleted by an older faulty implementation; that is not a new migration loss.

## Decisions (a)–(e) and residuals

| Item | Disposition |
| --- | --- |
| **(a) "In J's transaction" means begin/track in J, write outside J** | **Accept.** This preserves the design's H1 lock rule. The entry becomes Committing/Resolving before the blocking write begins, and owns the obligation until its result is reconciled. It does not require holding the Rust mutex across SQLite I/O. |
| **(b) No MaybeSent rows written in P2a; kind reserved** | **Accept for P2a.** A standing write-ahead marker already supplies conservative possibly-dispatched recovery. MaybeSent/funding-marker consumers remain later work; the journal correctly preserves the reserved kind's finality. |
| **(c) First sighting of every marked artifact starts Sent, even without NotSent** | **Accept.** It conservatively establishes finality and gates copies through the same artifact-wide obligation. |
| **(d) Sweep "terminal" means no standing marker and no Sent row** | **Accept with the implemented qualification: no Sent *or MaybeSent* row, and run before any session dispatch/writer.** This is a sufficient artifact-level condition; the sweep preserves every possible-dispatch reading. It does not justify the separate wallet-wide erase. |
| **(e) A NotSent artifact later seen sent can immediately Resend after its evidence is durable** | **Accept.** Sent outranks NotSent, without refunding an already observed execution; pending/failed Sent evidence still defers or is repaired. |

- **`bind_spend` versus Resolving:** retain the r2 conditional deferral. The current production TxDraft scope has no step marker. Fix before installing a consumer that can introduce the problematic step-scoped Core path; this review does not claim it is fixed.
- **A marker writer landing after removal's erase:** remains an existing conditional consumer deferral, not evidence that removal is safe. The compaction protocol needed for R3-F1 must account for writer obligations as well; do not treat the earlier deferral as permission to delete an already-standing marker beneath a transporting copy.
- **Unacknowledged Sent write lost across crash:** acceptable only as the explicitly limited DEC-154 (3) obligation. Before durable acknowledgement, a sighting may still be lost on restart if its Sent append never lands; copies remain blocked in the old process. The next sighting, successful journal write and close retry an owed row. This can happen if a crash comes before another retry opportunity, not only after a long series of failures. It is not an unconditional promise that every in-memory Sent observation survives a crash. The model deliberately checks recovery once evidence is durable or a copy is out.
- **Admit versus removal erase:** **not accepted for this DEC-154 confirm**, for the concrete evidence-loss and transport failures in R3-F1.

## Independent validation

Commands ran with **umask 022**, **TMPDIR `/dev/shm/e0-04-p2a-r3-sol` mode 0700**, **nice 10**, `CARGO_BUILD_JOBS=12` and `RUSTC_WRAPPER=`. Cargo used this worktree's own target directory; no target directory was shared. Evidence is retained on disk under `/work/scratch/e0-04-p2a-r3-sol/`; the review's TMPDIR is removed after validation.

| Check | Result |
| --- | --- |
| `DW_LEASE_STRESS_SEEDS=10 cargo test -p dw-engine --lib platform::lease --locked -- --nocapture` | **76 passed, 0 failed.** Includes all ten positive stress seeds, eleven stress mutations and installed r2/H1/M2/L1/L2 regressions. |
| `cargo test -p dw-appdb dispatch::tests --locked -- --nocapture` | **12 passed, 0 failed.** Includes v1 migration, standing-marker sweep and Sent/MaybeSent finality. |
| Independent `review_r3_` probes, temporarily included and run serially | **7 passed, 2 failed**, exit 101. The three r2 schedules, control and three committed-write/error/panic cases pass. Both removal safety assertions fail. |
| `DW_LEASE_MUTATION=SkipEvidence` against the held-evidence regression | Fails: Resend instead of Deferred, exit 101. |
| `DW_LEASE_MUTATION=FailedUncommitted` against failed NotSent regression | Fails: Resend instead of Deferred, exit 101. |
| `DW_LEASE_MUTATION=NoSentRow` against held-evidence regression | Fails waiting for the missing Sent writer, exit 101; the stress independently catches its I11 violation. |
| `DW_LEASE_MUTATION=StashDurable` against H1 regression | Fails its assertion that the copy wrote its missing marker, exit 101. |
| `python3 -I docs/design/checks/e0_04_design_model.py` | **86 PASS**, final `ok`; six checks belong to Part 6. |
| `cargo fmt --all --check` | Pass. |
| `cargo clippy -p dw-engine -p dw-appdb --all-targets --locked -- -D warnings` | Pass, exit 0. |
| `git diff --check d991029 973e1da` | Pass. |

The eleven stress mutations were caught under their exact intended labels: CheckThenAct/RevokeInDrain→I2; TransportFirst→I1; EarlyReturn→I5; CopyBeforeMarker/SkipEvidence/FailedUncommitted/NoSentRow→I11; MarkerOutlives→I16; LateEntry→L5; DuplicateRegister→DEC-134. StashDurable is checked by its targeted H1 regression and the model, not counted as a stress mutation.

Logs: `lease.log`, `appdb.log`, `review-witness.log`, `negative-*.log`, `model.log`, `fmt.log`, `clippy.log`. The standalone probe source is `review-witness.rs`; the unchanged original r2 source is `r2-original-witness.rs`. To reproduce the independent probes, temporarily include the standalone source at the end of `rust/crates/dw-engine/src/platform/lease/tests.rs` and run `cargo test -p dw-engine --lib review_r3_ --locked -- --nocapture --test-threads=1` under a fresh private `/dev/shm` TMPDIR. Restore the include afterward.

These file-backed SQLite and reopen checks test persisted rows and ambiguous write acknowledgements, not physical power loss. I did not rerun the complete workspace or later P2b/P4 consumer integration in this narrow confirm. R2-F1's resolution fix is confirmed; the global durability/compaction gate requires R3-F1 to be fixed.

**Approval status: CHANGES-REQUIRED.**

**Sol gate satisfied: no**
