# ST3a CP1.1 and Phase C checkpoint 2 — Sol review

**Verdict: CHANGES-REQUIRED for CP2. CP1.1's R2-1 and R2-2 are CLOSED.** The checked-in suites pass, and the planted bugs generally reach the intended failures. Independent probes nevertheless find gaps in the checkers: R5 can certify lost or changed metadata, D1 can reject a correct recovered image, recovery faults can hit the checker instead of recovery, R7a misses inner power losses and cannot replay its own findings, and R6 ignores script-check provenance. Sol is not satisfied; the sequential Opus high pass should follow the fix round, per DEC-92.

Reviewed `0cc7126bb0f9a591539c727d8746131bbfe1d07b`, CP1.1 `97a92ab1`, and the diffs from CP1 `990e776c`. Used a detached worktree at `/work/scratch/st3a-cp2-sol/wt`; did not edit the implementer's worktree, commit, push, or post anything. Inputs: the worktree's rev5/DEC-74 design including I1–I23, DEC-80 Q1, DEC-101, both checkpoint-1 rounds from both reviewers, and REVIEW-COMMON. DEC-95/99 affect CH2's memory policy, not the storage obligations reviewed here.

The findings below are newly discovered in this review; the R7 limitations and parts of the R5/R6 incompleteness predate CP2. They are not regressions of the two CP1.1 fixes.

## Prior-item disposition

| Item | Status | Evidence |
|---|---|---|
| GPT R2-1: replacement exemption ignores EOF/source durability | **CLOSED** | `bytes_at` checks the selected size before comparing bytes. Rename-over requires `inode_range == Durable` and equality with the destination's durable bytes over the entire protected range. Copy-over has no exemption. Both original GPT cases pass detection. |
| GPT R2-2: recovery-fault DFS truncation uncounted | **CLOSED** | Recovery-fault exploration uses `Explorer::search`, including the determinism guard, Work accumulation, per-image bookkeeping, remaining witness budget, and exhaustive truncation accounting. `FaultOnlyBranch` tests caps 1 and 1024. |
| Opus A: short rename, identical copy, pending size reductions | **CLOSED** | Shorter-source rename and identical copy-over of both ones and zeros are detected; the latter also reports D1(a) after the next WAL barrier. `inode_range` examines pending sizes, not just the durable size. |
| Opus B: extractor lookups of undecided records | **PARTIAL** | The added `get` tri-state tracking and one-unknown regression work. The combination of an undecided owner record and an undecided extractor read still reports a false D1(b): CP2-2. |
| Opus C: repeated boot D1/cost | **Cost concern closed; cache correctness minor remains** | Boot D1 is cached by image, crash kind and boot decisions; refresh borrows the stores and can update only conservative ranges. Timings fit the budget. Cache configuration reuse has a separate defect, CP2-6. |
| Opus minor 3: always-on synced-value guard | **PARTIAL** | New assertions retain the durable unit and size among PowerLoss outcomes (`fs.rs:859`, `:918`). There is still no per-barrier assertion in `Inode::sync` (`:226`), and no declared deviation from §5.4. Add the requested barrier guard or explicitly disposition the deviation. |

The exact CP1.1 checker suite passed **19/19**. I then put its regression tests on `990e776c`: **all five added `r2_*` tests failed** (short zero source, matching-but-unsynced source, identical copy, shorter rename, and capped recovery-fault search). Thus these are genuine old-head regressions, not merely passing tests on CP2. The CP2 checker suite passes **20/20**.

## Findings and reproductions

All scratch probes are preserved in [st3a_cp2_probe.rs](/work/scratch/st3a-cp2-sol/st3a_cp2_probe.rs), with output in [probes.log](/work/scratch/st3a-cp2-sol/probes.log). To rerun at the reviewed head, copy the probe into `dashd-store/tests/` in a new detached worktree and run:

```sh
mkdir -p /dev/shm/st3a-cp2-sol
chmod 700 /dev/shm/st3a-cp2-sol
CARGO_BUILD_JOBS=6 RUSTC_WRAPPER= TMPDIR=/dev/shm/st3a-cp2-sol \
  cargo test -p dashd-store --no-default-features --features faulty \
  --test st3a_cp2_probe -- --nocapture --test-threads=1
```

The probes deliberately assert the promised checker behavior. **One passes (determinism), nine fail**, demonstrating the findings below. These failures are separate from the green checked-in suites.

### CP2-1 — Major: R5 omits file records, allows index metadata changes, and trusts an ineligible prune mark

**Locations:** `dashd-store/src/faulty/toy/mod.rs:1527`, `:2150–2207`; `:2323` (forward assertions).

R5's normative contract preserves every `block_index` and `block_files` value of E except specific entitled changes. The implementation captures `e_files` but never compares it. Its index comparison checks only retained status bits, `data_pos` and `file`; it omits height, parent, existing undo position and unauthorized added status bits.

Three independently executable probes open the correct toy, connect and flush through height 10, crash/recover, and first assert the unmodified recovered toy is clean:

- `cp2_r5_rejects_changed_file_metadata`: change file 0's recorded `height_last` from 10 to 110, preserving its byte sizes. **`check == Ok(())`, D1 empty.** This metadata controls future prune eligibility.
- `cp2_r5_rejects_changed_index_height`: change block 5's stored height to 999, preserving its hash and byte positions. **`check == Ok(())`, D1 empty.** R1 still reads the correct bytes, so it does not substitute for the missing metadata comparison.
- `cp2_r5_requires_an_entitled_recovery_selection`: clear every data/undo flag and position in file 0. Without a selection mark, R5 correctly reports changes. Add a recovery-epoch `PruneDecision` and `PruneSelected { files: [0], bound: 0, lock: None }`, and the same damage becomes **`Ok(())`**. File 0 ends at height 10 and is not eligible at bound 0. No completed Bk/Bp is established by these marks. A matching lock alone grants the exemption.

This last case exposes I11's weakening of “matches the reference's selection.” Moreover, FA2 is invoked by `run_forward`, not by `reopen_and_check`, so the statement that selection correctness is “FA2's job” does not protect a recovery-time selection.

**Fix:** compare all captured file records and all immutable index fields, allowing only proven activation additions/status transitions and precise prune resets. Grant prune exemptions only for eligible selections whose corresponding recovery Bk/Bp completed; validate the recovery selections too. I11 needs either restoration of the normative check or an explicit ruling accepting its reduced coverage. DEC-101 Q2's narrowed FA1 obligation does not make the demonstrated R5 loss safe.

### CP2-2 — Major: two undecided records leave a stale D1 failure that becomes a false positive

**Location:** `dashd-store/src/faulty/check.rs:446–451`, `:496–503`.

`Pending.via_reads` is set to `via_reads && own_known`. When both the evaluated record and an extractor-read record are undecided, this incorrectly turns an uncertainty-dependent failure into one that will be reported solely when the owner becomes present.

`cp2_two_unknowns_do_not_report_a_false_dependency_failure`:

1. Sync a valid 512-byte file.
2. Submit Buffered `idx = good` and Buffered `loc` referencing `idx`.
3. PowerLoss with all-survived decisions. At boot, the resolver returns an error for Unknown `idx`; D1 initially holds it pending.
4. Read both `loc` and `idx`, establishing that both survived and the dependency resolves to the synced file.
5. Evaluate D1. **It reports `D1b: index undecided`**, despite the now-valid dependency and unchanged durable bytes.

**Fix:** preserve lookup uncertainty whenever `via_reads` is true, even if the owner's presence is also uncertain. Re-resolve that dependency before emitting its pending failure. Add this two-unknown control beside the current one-unknown test. Opus B is not fully closed.

### CP2-3 — Major: pre-open store checks consume recovery read faults

**Locations:** `dashd-store/src/faulty/harness.rs:622–627`; `dashd-store/src/faulty/toy/mod.rs:2304–2320`.

`reopen_and_check` calls `S::store_check` with the recovery fault already armed. It disarms faults only after `S::open`. ToyChain's new store check performs three engine reads and swallows errors via `.ok().flatten()`.

`cp2_recovery_fault_does_not_hit_store_check` uses the correct toy with `c10;f`, captures B1's completed barrier, and injects EIO on recovery's first chainstate read. It reports:

```text
images 1 branches 4 truncated 0 max/image 1 found {R2}
R2: the booted markers disagree: evo is not at the tip or the marker's new
```

The stores are consistent. The fault hit the checker's `evo_tip` read, was treated as an absent value, and was spent before the SUT opened. Recovery then succeeds instead of exercising the sanctioned read-error abort. The existing S9 engine-read fault at ordinal 3 avoids these first three checker reads, so it does not expose this boundary.

**Fix:** keep fault injection disabled for checker observations, arming recovery faults only around the actual open/recovery calls. Do not convert an injected checker read error into a marker value. Add first-read engine and filesystem controls that verify the SUT, rather than its checker, received the fault.

### CP2-4 — Major: R7a excludes inner PowerLoss and its findings do not replay

**Locations:** `dashd-store/src/faulty/harness.rs:899–912`, `:1069–1118`.

I6 correctly takes the baseline projection before R12; I21 correctly separates recovery decisions from later check/R12 decisions. Two inherited limitations remain:

- R7a is gated by `k2 == ProcessKill`, even for a wholly durable outer image. `cp2_r7a_covers_inner_power_loss` has recovery write Buffered `flag`, then Durable `done` without a pre-final barrier. Inner PowerLoss at `done`'s Append can keep `done` and lose `flag`, producing a different projection. The barrier-correct control and the buggy variant both report **`images 1 branches 22 truncated 0 … found {}`**. ProcessKill keeps both and converges. There are no undecided outer cells in this fixture.
- `cp2_r7a_repro_replays_r7a` uses a recovery that Durably increments its projected counter on every open. Exploration correctly finds R7a after an inner ProcessKill. **`replay` returns `[]` for that exact finding's repro**, because nested replay never recomputes the uninterrupted projection or performs the convergence comparison. Consequently `reproduces` and minimization cannot preserve an R7a-only failure.

**Fix:** apply convergence comparison to both inner crash kinds when the outer image really is durable/stabilized, retaining the mixed-kind/unstabilized distinction. Share that comparison with replay, including the baseline and applicable outer decisions. T7/T27 are Phase D acceptance, not falsely claimed completed here; this defect must be closed before that acceptance or a claim that the existing nested checker is sound.

### CP2-5 — Major: R6 ignores the `scripts` provenance it promises to validate

**Location:** `dashd-store/src/faulty/toy/mod.rs:2208–2273`.

R6(a) requires an applied connect with the reference's assumevalid/script-check decision before the flush. The checker gathers only hashes of `applied: true` connects and ignores `scripts` entirely.

`cp2_r6_checks_script_provenance` starts with the same clean recovered height-10 control and changes every `Connected.scripts` from `Full` to `Skipped` in the trace. The reference and state are unchanged; the toy's expected connects are Full. **The checker still returns `Ok(())`.** T20/T21's missing/invalid-connect witnesses do not test this part of R6.

**Fix:** expose the reference's expected script decision and compare it with each connect needed for the flushed transition. Add a skipped-check negative control. Also complete the promised exact replay-path and recovery-epoch activation provenance checks: the current replay loop merely checks that each replayed hash was connected somewhere in the combined trace. That latter observation is from code inspection; the executable probe specifically proves the script-decision gap.

### CP2-6 — Minor: boot D1 cache does not respect configuration changes

**Locations:** `dashd-store/src/faulty/world.rs:1004`, `:1326–1343`.

`cp2_boot_cache_honors_d1_enablement` creates a durably referenced file, boots the same image once with `d1 = false`, then boots it with D1 enabled and unlinks the file before a barrier. The first boot cached an empty D1 state; the second reuses it. **No D1(e) is reported.** Current `Explorer` boots consistently with D1 enabled, so this does not invalidate the measured normal scenarios.

**Fix:** do not populate/use the enabled checker cache on a disabled boot, and ensure any boot settings that can change the evaluated store image are represented in the key or constrained explicitly. Keep a mixed-configuration control.

## Planted-bug audit

I inspected every mapping in `planted`, the corresponding toy mutation branches, and the checked-in repro set. All **41 configured witnesses/repros** pass (T1–T42 plus T23a/b, excluding T7/T27). The following 24 bugs received the requested explicit precondition/control audit. “Detected” means the named invariant is present; additional consequences are allowed. The corresponding correct controls report no findings and no truncation, including the separately run nightly controls.

| Bugs | Real precondition and clean control | Bug result |
|---|---|---|
| T1, T2 | S1 flush/index write includes newly written block and undo bytes; correct files barrier precedes publication | R1 + D1(a) |
| T5, T11 | S5 selects file 0; replay from durable 5 to 72 crosses its heights 6–62. Clean S5: 21 images / 48 branches | R0 + D1(e) |
| T6 | S6's 900-byte records and accepted-ahead rollover leave old-file undo dirty while current pair is file 1; correct control runs both releases | R1 + D1(a), both releases |
| T10 | S1 commits a locator for new blocks while the prior chainstate tip remains guaranteed until B1. Correct commit occurs after B1 | R11a + D1(b) |
| T12 | Last block's undo is deferred into the next connect, leaving its advertised record absent at publication | R1 + D1(a) |
| T13, T19 | File 0 is selected and intersects the replay path; S5b additionally makes heights 6–72 newly dirty. Correct S5b: 21 / 48 | R0 + D1(b) |
| T15 | First V-ahead flush: correct bound 52 selects nothing, V-tip bound 62 selects file 0; next marker's path includes 61–62. Correct S5c: 65 / 487 | R0 + D1(b) |
| T16 | Durable locator 10 versus processed height 70 makes selections ∅ versus {0}; correct S11: 6 / 13 | R11b + D1(b) |
| T22 | Subscriber data is submitted but unbarriered before locator Append. Correct S1 and reorg controls are clean | R11c + D1(f), including archive rows on S2 |
| T28 | Startup activation of 121–130 has a prune check due before subscriber start; loaded lock selects ∅, late lock selects {0}. Correct S13: 38 / 114 | R11b + D1(b); also R5 |
| T29 | Presumed snapshot at 25 is pending before B1, and continuation block 31 consumes it | R12 + D1(f) |
| T30, T36 | Locator A70, fork 50, target B80; height lock can remove A51–62 needed for disconnects. Correct S11r: 25 / 60, including rewind/catch-up | T30: R11b + D1(b); T36: R11c(i) |
| T33 | Progress 25/50/60 exceeds T_ack 20/40/40 at cadence points. Correct commits are 20,40,40,60 | R11a + D1(b) + FA4 |
| T34 | First split flush leaves a Buffered finalizer whose untouched coin keys are still needed when B1′ overwrites its marker | R3 + D1(f); correct S4b clean |
| T35 | Synced backlog 2 has not processed new at step 0; correct commits at 40 and 72 select {0}, bug commits neither and selects ∅ | FA3 + FA5 |
| T38, T41 | At limit p=96, s=49; correct checkpoint submits to 96, syncs, commits, then resumes. All three correct backlog runs terminate and are clean | T38: R11c(i) after the Buffered checkpoint; T41: R11c(i), b=1 and 2 |
| T39, T40 | Retained D={0} met target; V growth creates shortfall. Correct selection stays {0}, re-arms, next check selects {1}; both selector fixtures run both releases | FA2: subset and re-arm, respectively |
| T42 | Backward flush after invalidating below durable 20; rewind to 14 precedes B1 in correct synced/backlogged/syncing controls | R11a + D1(b) |

T23a specifically asserts that D1(a) is silent; R1 observes the torn body. I5's nonzero per-block padding restores that observability. T23b exercises the additional ProcessKill → PowerLoss non-promotion case. T37 is detected directly by FA6 for backlog 1 and 2. The DEC-80 archive/state-key controls and the linked-inode write control detect R11c and R10. The remaining configured T3/T4/T8/T9/T14/T17/T18/T20/T21/T24/T25/T26/T31 tests also passed; they were inspected, but are outside the table's detailed selection/lock audit.

## Coverage and errata assessment

The coverage is substantial, but the scenario tables should be read with the phase and nightly qualifications:

- S1–S11d, S14 and S14b have real forward/clean tests; S8/S9 include fatal and best-effort faults. S2b is present. S5b, S11r, S11c and S13 clean runs are ignored by default; I ran all four successfully. S11c is the intended refusal control, not a claim of recoverable pruned data.
- S12/N5 already runs nested searches (21 outer images, 4,010 branches, zero truncation). S11d's ordinary crash window covers the checkpoint's submit/sync/Append/return/resume interval; **its separately promised nested checkpoint run is not present yet**. T7/T27, W1–W5, twin/fuzz and production nightly remain Phase D/E work. Do not treat “T1–T42 implemented” as completed nested acceptance.
- Release coverage is narrower than N1's “every scenario, both releases.” S6 and the selector protocol explicitly loop over both releases; many other scenarios default to V24. The design schedules the second release of several heavy scenarios nightly, but there is no corresponding general second-release nightly matrix in this head. In particular the S10 test does not loop over both releases. State this as incomplete coverage, not a second green matrix.
- Preconditions mostly live in the integration tests rather than a ToyChain `Sut::preconditions` implementation. They are real and run in the gate, but the generic explorer alone does not enforce the table's selected-file/path fixtures. Recovery-side R5 entitlement needs its own validation, as CP2-1 demonstrates.

| Errata | Assessment |
|---|---|
| I4 | Explicitly ruled by DEC-101 Q1. Measured N7 max/image is 216; R12 is disabled in that fixture and recorded in repros. |
| I5–I8 | Fixture observability, pre-R12 baseline, realistic adoption setup, and distinction between single-loss L2 and nested non-promotion. Implementation refinements, not protocol changes. R7 still needs CP2-4. |
| I9 | Durable disabled-state representation models the authorized S11c scope exception; no additional policy ruling needed. |
| I10, I18 | Epoch/commit tags and booted-tag ancestry implement DEC-80's monotone archive requirement. No new policy; controls pass. |
| **I11** | **Substantive weakening of R5's entitlement test, not just representation.** Lock equality does not establish selection equality/eligibility or completed finalization. Restore it or obtain an explicit reduced-coverage ruling; CP2-1 proves a real escape. |
| I12 | Recommitting the same locator is normal commit behavior and fixes the documented cadence fixture. |
| I13 | Window narrowing and designated nightly clean runs agree with §8.2's scope. They need explicit coverage accounting. DEC-101 Q5 additionally moves S5c/S14b clean runs; that move is not applied at this head. |
| I14–I16 | Strengthen enforcement of the existing R12/R3/R11c contracts; catch-up's DMax flush is already authorized by §5.6. No additional ruling. Correct catch-up past D_max has a regression; its present control runs ProcessKill only. |
| I17 | Narrowed FA1 obligation already ruled by DEC-101 Q2; FA3–FA5 refinements implement the existing liveness intent. |
| I19 | Init prune-check timing under already loaded locks is consistent with the existing open protocol. |
| I20 | Explicitly ruled by DEC-101 Q3; the single-thread toy uses the not-ahead check for B1's cross-store ordering. An explicit CH2b scope remains downstream work. |
| I21–I22 | Correct provenance of nested decisions and faithful reproduction of R12 enablement. No protocol change. |
| I23 | R10 observability control passes; epoch-limited R13 is a reduced coverage obligation explicitly deferred across epochs by DEC-101 Q4. No new ruling requested. |

FA1's selected-file oracle limitation is accepted under DEC-101; I am not reopening that ruling. FA2 presently checks subset/re-arm assertions against trace inputs, not an independent equality oracle for `D ∩ eligible(actual)`. The two explicit selector fixtures are valuable real controls. The recovery-time gap in CP2-1 is a separate issue.

## Validation, cost, and determinism

Every cargo validation used **CARGO_BUILD_JOBS=6**, with mode-700 **TMPDIR=/dev/shm/st3a-cp2-sol**. The shared Rust compiler cache stalled the first attempt; subsequent review builds used `RUSTC_WRAPPER=`. Each worktree retained its own target; no target directory was shared. CPU measurements below exclude compilation and use four test threads.

- `cargo test -p dashd-store`: **195 passed, 10 intentionally ignored**, doc tests passed. Includes 51 unit tests, fail-stop, feature scope, 20 checkers, 15 MicroLog, 24 model, 81 ToyChain and two native tests. [cargo-test.log](/work/scratch/st3a-cp2-sol/cargo-test.log).
- `cargo test -p dashd-store --no-default-features --features faulty --test power_loss_checkers --test power_loss_model --test power_loss_micro --test power_loss_toy --test faulty_feature_scope`: **passed**, 20 / 24 / 15 / 81 / 1. [no-default-power-loss.log](/work/scratch/st3a-cp2-sol/no-default-power-loss.log).
- Four ignored Phase C clean controls: **4 passed**, no truncation; excludes repro regeneration. [toy-nightly-controls.log](/work/scratch/st3a-cp2-sol/toy-nightly-controls.log).
- Native WAL regression: **2 tests / all four cases passed**, 0.42 CPU-s. This is the edited-image model/native evidence, not a physical power-cut claim.
- `cargo fmt --all -- --check` and targeted `cargo clippy -p dashd-store --no-default-features --features faulty --all-targets -- -D warnings`: **passed** on the unmodified reviewed sources (scratch probes removed before linting).
- CP1.1 historical closure: 19 checker tests pass at `97a92ab1`; all five added regressions fail at `990e776c`. [cp11-checkers.log](/work/scratch/st3a-cp2-sol/cp11-checkers.log), [old-head-regressions.log](/work/scratch/st3a-cp2-sol/old-head-regressions.log).

| Measured suite | User + system CPU-s |
|---|---:|
| ToyChain with DEC-101's S5c/S14b clean moves simulated by `--skip` | 38.30 |
| S5c clean, separately | 4.45 |
| S14b clean, separately | 3.86 |
| Model / MicroLog / checker suites | 3.34 / 0.11 / 0.01 |
| Native regression / feature-scope test | 0.42 / 1.22 |
| **Current power-loss gate including both heavy clean tests** | **51.71** |
| **After DEC-101 Q5's two moves** | **43.40** |

These measured parts fit roughly 50/60 CPU-s. They are not a forecast for all remaining Phase D/E additions or the unrelated store unit suites. Apply Q5 and replace the stale §8.2 forecasts with measured rows as Q1 directs. There is no ToyChain test asserting a calibrated aggregate `Work` budget at this head; the enforced gates are branch caps/zero truncation, with explicit bound checks such as N7's 216. The statement that the whole PR gate asserts Work caps is ahead of the implementation.

The independent determinism control repeats a backlog-2 reorg workload and compares the entire forward trace, findings summary, Work counters and per-image branch/variable inventory. Both runs are clean and equal: trace hash **`c2afc8094daecbb0`**, **10 images / 26 branches / max 3 / zero truncation**. M11 and the model differential tests pass. BTree collections, fixed generated blocks and single-thread store calls support the intended determinism; the cache/configuration issue above still needs its control.

Evidence remains under `/work/scratch/st3a-cp2-sol/`. The review worktree and temporary RAM directory are removed after the report is finalized. No implementation changes were made.

**Final verdict: CHANGES-REQUIRED.** Accept CP1.1's two major closures; fix the checker findings before advancing the critical review, and keep the explicitly deferred Phase D/E coverage visible.
