# ST3a checkpoint 1, round 2 — GPT closure review

**Verdict: CHANGES-REQUIRED.** The original GPT counterexamples are fixed, and the checked-in checkpoint suites pass. Two regressions in the fixes still let the checker/explorer miss bugs: the replacement exception suppresses real D1(d) violations, and capped recovery-fault exploration can claim a clean exhaustive result while hiding R8.

Reviewed exact head `990e776c1bc1bd3b80193295ba9a09da6b30e108` in a fresh detached worktree at `/work/scratch/st3a-cp1-r2-gpt/wt`. Inputs: both checkpoint-1 round-1 reports on `node/integration`, REVIEW-COMMON, rev5/DEC-74 design (§§2, 4.4, 5.3–5.4, 8, 10), and DEC-80 Q1. Scope: closure of those findings and regressions introduced by `58010d90`/`990e776c`; no independent review of the Phase C core in `a5b93c5a`. ToyChain's requested suite was run, but passing it is not a Phase C review verdict.

## Round-1 disposition

| Round-1 item | Status | Evidence / remaining limit |
|---|---|---|
| GPT Major 1: decided cells ignored; unsafe recovery unlink | CLOSED | `WorldDurableView` carries branch decisions; engine records include all possible durable values; range queries distinguish Durable/NotDurable/Unknown. `d1_sees_a_record_decided_present_after_a_crash`, `d1_sees_a_file_decided_absent_after_a_crash`, and planted `Dropper` regression pass. Both original GPT D1 sequences were rerun through those equivalent public-API probes. New `r2_undecided_record_protects_ranges` confirms an unread/Unknown record still protects its file before a barrier. |
| GPT Major 2: ignored read fault, recovery R8 gap, clean zero-image run | CLOSED | Both original planted systems (`BadRead`, `BadRecover`) are now passing detection regressions. Injected read errors enter the trace and open fault capture; R8 runs on recovery success and sanctioned abort. Extra probes confirm failure for a never-fired forward fault, empty window, minimum-image shortfall, absent R12 continuation, and never-fired recovery fault. The newly added recovery-fault search has a separate cap-accounting regression (R2-2 below). |
| Opus Major 1: pending rebinding and rename-away | CLOSED | `d1_pending_rebinding_of_a_referenced_name` catches different-content pending rename-over; its synced control is clean. Rename-away now reports D1(e) immediately and D1(a) at the next barrier. |
| Opus Major 2: required set reset at boot | CLOSED | Extractor is carried in `CrashImage`, boot runs a standing evaluation, and attaching an extractor evaluates immediately. Both ProcessKill and PowerLoss pre-first-barrier truncate/unlink controls pass. |
| Opus Major 3: already-decided cells skipped | CLOSED | Recreated-name-without-directory-sync and GPT decided-absent controls pass; durable peeks use actual branch decisions. |
| Opus Major 4: false positive on safe rename-over; one-snapshot coverage | PARTIAL | Safe byte-identical `make_private` replacement and the two-snapshot MicroLog N0 now pass. However the new exception is too broad: short and unsynced source cases suppress real D1(d), R2-1 below. The reviewed design's D1(d) table still does not explicitly state the rename-over exception. |
| Opus Minor 1: cached R1 misses post-read damage; R12 silently skipped; empty tip | CLOSED | MicroLog rereads committed stores in `check`, has a marker append+commit continuation, and normalizes the empty tip. X3 regression catches post-read truncation; X1/X2 controls and all checked-in replays pass. Explicitly enabling R12 without a continuation produces Precondition (extra probe). |
| Opus Minor 2: empty windows pass | CLOSED | Generic nonempty-window guard and minimum-image guard are present and exercised by extra probes. Forward and recovery faults that never fire fail explicitly. |
| Opus Minor 3: always-on model guards missing | PARTIAL | Lookup now asserts that bindings name an existing inode; `Ctx::decide` bounds each choice, including size domains. Synced-value immutability remains M1/offline, and the engine-subset reference remains M6/offline rather than a per-exploration independent guard. See acceptance assessment below. |
| Opus Minor 4: model-suite budget | CLOSED | M10 cut from 60 to 40 seeds; 400-seed nightly remains. Direct model binary measured 3.37 user CPU-s (24 pass, 1 ignored), below the 5.0-s row even at high host load (1-min load roughly 56–67 around this run). Implementer log records 2.94 user CPU-s. This closes the reported over-budget run; it is not a PMU/Work calibration or full §8 acceptance. |
| Opus Minor 5: no D1(b)–(f) regression coverage | CLOSED | Resolver b/c and finalizer-scope tests are checked in and pass; boot truncate/unlink, rename-away and replacement controls cover d/e. |
| Opus nit: feature-scope test vacuous | CLOSED | Test now checks all workspace members' normal feature graph and includes a dev-feature positive control. It passes in both default and no-default runs. |
| Opus nit: InsideWrite only covers write_all_at | OPEN | No expansion/documented design erratum in these fixes. Retains round-1's nonblocking ST2/Phase-D assessment; not re-reviewed here. |
| Opus nit: lazy source copy size domain | OPEN | Copy still carries lazy `src_len` directly and partial copy uses its maximum. Unchanged nit, outside this closure implementation. |
| Opus nit: check.rs overclaims forward completeness | CLOSED | Module documentation now describes boot/barrier/end-recovery evaluation, decided-cell peeking, conservative protection and pending unknowns. |
| Opus nit: no post-crash fault injection in N0 | CLOSED | `recovery_faults` API added; N0 now injects a recovery read fault and checks sanctioned abort. Its truncation-accounting defect is R2-2. |

M18 is also materially stronger: it compares nested outer/inner batch outcomes against an independently constructed set, with and without an outer read. It passes. The implementer's old-head repro log shows precisely 12 checker tests failing on `c89e7ad6`; those same 14 tests (12 regressions plus 2 existing controls) all pass in this review. I inspected that historical log; I did not rebuild the old head.

## New findings in the fixes

### R2-1 — Major: replacement byte comparison ignores EOF and source durability, masking D1(d)

**Locations:** `dashd-store/src/faulty/fs.rs:1243` (`d1_replace`, particularly 1252–1254), `:1061` (`bytes_at`, particularly 1069–1083), called by `copy` at `:1553` and `rename` at `:1601`.

The safe-replacement exception compares only the source and destination's **current unit bytes**. `bytes_at` never verifies that the file length covers the requested range and supplies zero bytes for absent units. Nor does `d1_replace` require the source's referenced range to be durable. Thus matching padded/current bytes are sufficient to suppress D1(d), even when replacement destroys readability or substitutes volatile content.

**Concrete reproduction A (both rename-over and copy-over):**

1. Write 512 zero bytes to `/dd/b`; sync its data and `/dd`.
2. Write a Durable `rec:/dd/b` requiring `[0,512)`.
3. Write **one** zero byte to `/dd/src`; sync its data and `/dd`.
4. Rename `/dd/src` over `/dd/b`, or copy it over `/dd/b`.
5. `/dd/b` is now length **1**, but `d1_violations()` is **empty** in both cases. The 512-byte requirement cannot read back.

`r2_short_zero_source_must_be_d1d` executes both variants and fails with `missed D1d (copy flag): [false, true]`. This is a regression of the blanket D1(d) detection the exception replaced, not a remaining false positive.

**Concrete reproduction B (rename-over):** synced `/dd/b` contains 512 bytes of 1 and is Durably referenced; synced `/dd/src` contains 512 bytes of 2. Overwrite the source with 1s **without syncing**, then rename it over `/dd/b`. `d1_violations()` is empty. A power loss may keep the replacement binding but recover the source's old 2s. `r2_matching_current_but_unsynced_source_must_be_d1d` fails. Later barriers can report D1(a); that does not restore the promised immediate D1(d) detection at the replacement.

**Control:** synced source of 2s over referenced destination of 1s is caught for both rename and copy (`r2_changed_replacements_are_d1d`). The safe synced byte-identical replacement remains clean in the existing test.

**Suggested fix:** require the replacement to cover every protected range with actual readable bytes (no comparison past EOF). For rename-over, grant the exception only when the source's size/bytes are known durable over the whole range, as round 1 requested. Preserve conservative detection for undecided source/destination state. Check copy's resulting size and partial outcome as well as its bytes. Add these positive detection regressions alongside the safe replacement control.

### R2-2 — Major: recovery-fault DFS silently truncates and can hide R8 in a “clean” exhaustive result

**Location:** `dashd-store/src/faulty/harness.rs:934–961`, specifically `:958`.

The new recovery-fault loop stops at `ex.cap()` but never increments `report.truncated` when `dfs.advance()` says another branch exists. It also omits the ordinary image loop's mismatch check and Work/per-image accounting. `Report::is_clean` relies on `truncated == 0`, so the cap no longer prevents a false exhaustive pass in this path.

**Concrete reproduction:** `FaultOnlyBranch` creates a synced file during its initial open; its workload appends Buffered key `k`. Capture the image after that append and use PowerLoss. Normal recovery reads the synced file and does not read `k`, so its ordinary search has only one branch. Inject EIO on the first filesystem read in recovery. The recovery error handler reads `k`; if the batch was lost it illegally creates another file after the fatal read, then emits the sanctioned abort. On the all-survived branch it just aborts.

- With `max_branches_per_image: 1024`: **images 1, branches 3, truncated 0, found {R8}**. The lost-batch branch reports creation after the fatal recovery read; its repro includes `recovery-fault nth fs read 0 io not-applied` and `decide 1:probe:b0=1`.
- With `max_branches_per_image: 1`: **images 1, branches 2, truncated 0, found {}**; `is_clean()` is **true**. The second recovery-fault branch containing R8 was never explored.

`r2_recovery_fault_search_reports_truncation` runs both searches and fails on the capped result. This directly weakens the new recovery-fault support and the non-vacuous exhaustive gate addressed by GPT Major 2.

**Suggested fix:** distinguish “no more branches” from “more branches but cap reached”; increment truncation for the latter in Exhaustive mode. Reuse the common image-search bookkeeping (determinism guard, Work, maximum/per-image branch counts) and respect Witness's remaining global budget. Add the above fault-only-branch regression.

## Validation and evidence

All builds used **CARGO_BUILD_JOBS=6**, with mode-700 **TMPDIR=/dev/shm/st3a-r2-gpt**. No implementer worktree was modified.

- `cargo test -p dashd-store`: **passed**, 122 tests passed, 5 intentionally ignored; doc tests passed. Includes native, model, MicroLog, checker, ToyChain and feature-scope suites. `default.log`.
- `cargo test -p dashd-store --no-default-features --features faulty --test power_loss_checkers --test power_loss_model --test power_loss_micro --test power_loss_toy --test faulty_feature_scope`: **passed**, respectively 14 / 24 / 15 / 14 / 1 tests; model and MicroLog each have one intentionally ignored test. `power-loss.log`.
- `cargo test -p dashd-store --features faulty --test rocks_wal_nonprefix`: **2 passed**, all four native cases. `native.log`. This checks edited RocksDB persisted images, not physical power-cut durability.
- Direct model binary timing: **3.37 user CPU-s**, `model-timing.log`.
- Scratch-only `cargo test -p dashd-store --no-default-features --features faulty --test st3a_r2_probe r2_ -- --nocapture --test-threads=1`: **3 passed, 3 intentionally failing detection assertions**. The failures reproduce R2-1 (two tests) and R2-2 (one); passing tests cover undecided-record protection, changed-content replacement, and five non-vacuity guards. `probes.rs` preserves the full source, `probes.log` the final output, under `/work/scratch/st3a-cp1-r2-gpt/`.
- Historical implementer logs read: `/work/scratch/st3a-power-loss/repro-c89e7ad6.log`, `repro-opus-micro-c89e7ad6.log`, `pass-990e776c.log`. Independent new-head results above agree on the existing regressions.

The scratch-only test was removed, tracked worktree status was clean and `git diff --check` passed. The detached worktree and TMPDIR were removed after validation; no commits or pushes were made. The report was copied to `/work/home/agent/workspace/node-program/reviews/ST3a-cp1-r2-gpt.md`.

The previous standalone GPT probe directory is no longer present. Its two D1 and two fail-stop counterexamples are preserved in the new checked-in checker regressions, which I reran; the unaffected model controls are covered by the requested model suite. No claim is made that the removed standalone source itself was rerun unchanged.

## Checkpoint acceptance assessment

The tri-state durable view and boot seeding close the original unsound D1 lenience. Unknown is exposed as Unknown (boolean/convenience durability APIs reject it); its possible records retain protected ranges; standing checks revisit unknown cells at subsequent evaluations. That optimization is acceptable at this checkpoint with the verified conservative protection.

**Offline-only synced-value immutability is acceptable as a tracked checkpoint-1 limitation, not as closure of §5.4.** M1 provides useful independent evidence, and this omission alone need not block the narrow A/B handoff: round 1 explicitly allowed Minor 3 to land with Phase C. Full design acceptance must either add the promised per-exploration guards (also for engine recovery composition) or record an explicit approved deviation. This review does not certify the always-on self-check contract as complete. The original allowed Phase-D adversary and eager InsideWrite deviations remain unchanged; DEC-80 Q1 is a Phase-C scenario obligation outside this closure check.

The two major regressions above require fixes before the new replacement exception and recovery-fault exhaustive results can be trusted. **CHANGES-REQUIRED.**
