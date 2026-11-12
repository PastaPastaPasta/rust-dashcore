# ST3a checkpoint 3 — Sol review

**Verdict: CHANGES-REQUIRED. Sol gate satisfied: no.** The checked-in PR suites pass, and the original CP2 regressions are real old-head regressions. Independent controls expose remaining checker false negatives and a mismatch between Extended's partial-write semantics and Linux. The nightly runner also still returns success after a failed component. Fix these before the sequential Opus high review under DEC-92.

Reviewed **546c4626a06871c397a2ed12f200bb2f67f73d74**, the entire `git diff 0cc7126b 546c4626`, the worktree design including I24–I34 and §8.4, DEC-101 Q1–Q5, the CP2 and prior checkpoint/design reviews, the implementation memory, and REVIEW-COMMON. Applied the code-review-validator workflow. The implementer worktree is actually `~/workspace/rdc-wt/st3a-power-loss`; it was left unchanged. Experiments used a detached worktree under `/work/scratch/st3a-cp3-review/` and mode-700 `TMPDIR=/dev/shm/st3a-cp3-tmp`. No commits, pushes, GitHub comments, or Opus dispatch were made.

## Findings

### CP3-1 — Major: D1 certifies an unreadable Extended unit

**Locations:** `dashd-store/src/faulty/fs.rs:150`, `:1257`, and `:1309` (`bytes_at`).

`Unit::peek` represents `Unreadable` as the same value as `Zero`. After PowerLoss, the selected unreadable unit is in a one-entry base history, which `inode_range` skips entirely. Consequently the durability query and standing D1 check certify a range that cannot be read.

The probe writes 512 unsynced bytes to `/dd/f`, syncs its directory, and boots PowerLoss under Extended with `1:fs:i1:u0=3`, selecting the unreadable outcome. It then commits a Durable record declaring that byte range as a dependency:

```text
read_exact_at: Err("EIO: an unreadable sector (Extended)")
range_durable(/dd/f[0,512)): Ok(())
D1: []
```

This is a new failure mode from Phase D's `Unreadable` state. It does not violate A-DEV or A-PSOW: the unit contained no pre-crash durable bytes. The planted bug is publishing a reference to an unreadable post-crash range, which D1(a) should reject. R1 may catch a particular ToyChain record later, but it does not make D1's certification sound for other SUTs.

**Fix:** preserve unreadability separately from zero bytes in the non-observing view. Return NotDurable for a decided unreadable range and Unknown when readability is still undecided, including one-entry base histories. Audit byte-identity and replacement comparisons so EIO is never treated as identical zero content. Add the committed-reference regression.

### CP3-2 — Major: the prune exemption still hides immutable index changes

**Location:** `dashd-store/src/faulty/toy/mod.rs:2344`.

The new `kept` path compares height, parent and file number, but `pruned` does not. Since the final acceptance is `pruned || kept`, an entitled prune bypasses those comparisons entirely.

The probe starts with a clean recovered height-72 toy in prune mode. File 0 is eligible at bound 64. It clears only that file's data/undo flags and positions, resets its file-info record, supplies the eligible selection and a completed Durable write, and additionally changes block 5's stored height to **999**. The result is **`check == Ok(())`, D1 empty**. The intact file-1 records, chainstate, bytes and provenance remain valid. A prune cannot entitle rewriting a block's height, parent or file identity.

The original CP2-1 tests now pass, but this part of CP2-1 remains open. This probe is about preservation of metadata, not DEC-101 Q2's deferred independent selected-file oracle. Also, `entitled_prunes` at `:1724` accepts any later successful chainstate Barrier; it does not establish that the barrier completed the selected flush's Bk/Bp. The executable height corruption already suffices to demonstrate the defect.

**Fix:** compare immutable fields before either exemption; permit only the precise prune flag/position resets. Associate entitlement with the selected flush and its actual completed finalizer. Keep an eligible-prune positive control and height/parent/file tampering negatives.

### CP3-3 — Major: I33's global `^` skip suppresses unrelated R7a failures after engine promotion

**Locations:** `dashd-store/src/faulty/harness.rs:1039`, `:914`, `:1161`, and `:1444`; provenance construction in `fs.rs:981` and `:1050`.

**Manager question (3): I do not confirm the blanket predicate as an adequate reading of §5.5.** Permitting divergence before promotion when the inherited input can change is correct; the existing Deriver control is reasonable. But §5.5 identifies stabilization with the engine promotion barrier. I33 additionally treats any newly decided inherited filesystem cell as a reason to suppress the entire comparison, even after that barrier and even when the cell retains exactly the original, irrelevant input.

The planted `FileFinalizer` starts with a durable engine baseline and an unsynced, unreferenced scratch file. Every recovery first completes the required engine `sync_wal`. It best-effort reads the scratch file and ignores its contents. It then writes Buffered `flag`, followed by Durable `done`, deliberately omitting the pre-final barrier. Crash after `done`'s Append and before its Barrier; inner PowerLoss keeps `done` and loses `flag`.

The uninterrupted projection is 0 and the nested projection is 9. The scratch file keeps its original contents: its newly constructed `^` cells choose **0**, while only the recovery's flag is lost. Nevertheless **replay returns `[]`**. Syncing the unrelated scratch file in the forward run, with the same finalizer bug, produces **R7a**. The proper pre-final-barrier control is clean. A second engine-only probe demonstrates the same masking before engine promotion; that second case alone would not refute §5.5.

This proves suppression of a particular R7a failure branch in a custom SUT. It does not claim that every stock ToyChain manifestation escapes all its other invariants. D1 cannot substitute for R7a in systems without that particular dependency declaration, as in the accepted CP2 finalizer control.

**Fix:** represent the actual comparison eligibility and inherited inputs, rather than treating every forced `^` cell as a global veto. Compare against an uninterrupted run constrained to the same surviving inherited inputs, or otherwise establish which inputs can affect the projection. Retain the genuine pre-promotion divergence control. Report compared/skipped nested branches. If the intended contract requires stabilizing all filesystem state, including unreferenced scratch data, record that additional narrowing explicitly and provide a checker for the recovery failures it excludes; it is not established by the present engine-barrier wording.

### CP3-4 — Major: partial writes use adversary-unit boundaries rather than Linux I/O boundaries

**Location:** `dashd-store/src/faulty/fs.rs:648`; design I33(3).

`read_for_partial_write` checks only the first/last **model units**, and only when the write partly covers one. This does not implement the stated Linux page-read behavior. With Byte granularity every write covers whole model units, so this guard cannot generate EIO at all.

The model probe creates an unsynced 4-KiB file, boots Extended with sector 7 unreadable, confirms a read of that sector fails, and overwrites sector 0. **The model returns `Ok(())`.** Independent native controls on this host's Linux **7.0.0-38-generic**, with both ext4 and XFS formatted with 4-KiB filesystem blocks, evict only the test file's cache and use a private loop/device-mapper image to make the last sector of that file's block unreadable:

```text
                    ext4             XFS
pwrite(0, 1)        EIO              EIO
pwrite(0, 512)      EIO              EIO
pwrite(0, 4096)     4096             4096
```

The first two writes need the remainder of the filesystem block, even though they do not address the bad sector. These are I/O behavior controls, not physical power-cut or A-DEV validation. Only private scratch images were used; their mounts, mappings and loop devices were removed.

**Fix:** separate fault/read-modify-write granularity from adversarial byte/sector version granularity, and cover unreadable portions of partially overwritten filesystem blocks/pages. Add Byte and Sector controls with full-block repair as the positive case. A separately injected per-call EIO can exercise some abort paths, but it does not make this unreadable-unit transition faithful.

tmpfs has no corresponding persistent block-device ECC path, and a host reboot loses its data. Design §2.3 already documents its lack of durability as **not modelled**. I do not accept a broader claim of power-loss equivalence to tmpfs; the temporary RAM directory is test infrastructure.

### CP3-5 — Major: the nightly runner still exits 0 after a component fails

**Locations:** `/work/scratch/st3a-power-loss/nightly/run.sh:11`–`:14`; `full.sh:8`. These supporting scripts are outside the reviewed commit.

Recording `rc=$?` immediately after the component fixes the originally reported status-row bug. The runner then discards that status, ends with successful `echo DONE`, and returns 0. `full.sh` inherits that success. An isolated copy of the current runner, with only its paths/binary replaced by a stub exiting 17, produces:

```text
run.sh aggregate exit: 0
0.00 0.00 0.01 forced-failure exit=17
DONE
```

An automated caller therefore still sees a failed nightly as successful. This does **not** invalidate the particular 99fce744 green run: I independently checked all fifteen component logs for an actual passed test, alongside the status rows, and summed **3,212.07 CPU-s**.

**Fix:** retain an aggregate failure status and return nonzero if any component fails, while optionally continuing to collect the other results. Fail when a named component executes zero tests; `--exact` otherwise makes a stale/misspelled name silently green. Select the binaries emitted by the current Cargo build rather than the newest matching directory entry. Put the reproducible runner and its status controls under version control before relying on it as a gate.

### CP3-6 — Minor: the nightly seed does not rotate

**Location:** `dashd-store/tests/power_loss_deep.rs:167`.

The test always uses `0x5733_a5ee`; only the case count is configurable. Neither nightly script changes that seed. Thus repeated nights run the same 20,000 cases, contrary to the manager's stated rotation ruling.

**Fix:** accept and print a configured seed, with the nightly launcher choosing a reproducible rotation. Keep the PR smoke seeds fixed. The approved 20,000-case count and accepted 0.89 CPU-h cost are not reopened here.

## CP2 disposition and other adversarial checks

- **CP2-1: partial.** File-record and unpruned-index tampering and the ineligible selection are now caught. The entitled-prune path still omits immutable fields (CP3-2).
- **CP2-2: closed.** The two-unknown control passes; uncertainty through extractor reads is retained and re-derived.
- **CP2-3: closed.** Checker reads do not consume recovery faults. First engine/filesystem read controls pass.
- **CP2-4: original cases closed.** The original inner-PowerLoss and replay controls pass; T7/T27 are nested and their checked-in repros replay. I33 introduces the additional comparison coverage problem above.
- **CP2-5: closed for the reported gaps.** Script decisions, exact booted-marker connect path, and current-epoch activation provenance are checked. The skipped-script positive control passes.
- **CP2-6: closed.** Disabled boots neither use nor fill the enabled D1 cache; the adversary/granularity/allocation settings participate in its key. The mixed-enablement regression passes.

I independently ran the CP2 regression file with the whole faulty source directory restored to **0cc7126b** inside the detached scratch worktree: **nine failures, one passing determinism control**. Restored CP3 afterwards. At CP3 all ten pass. This confirms the historical regression claim, without editing either task branch.

**FA4:** I34's reset before a commit can land is justified. The checked-in pre-`T_ack` control passes. My extra control removes cadence-commit marks from an otherwise correct syncing trace after `T_ack` exists and the cap can land; FA4 still reports progress without a commit. I found no demonstrated false negative from that particular relaxation.

**R13:** my additional control writes effect `i:12` after the height-10 flush, ProcessKills, and lets the next epoch's promotion barrier cover it. The unchanged recovered toy is clean; deleting that effect then reports R13. The extension across ProcessKills works. I25 deliberately permits loss across an intervening PowerLoss; I do not present that stated scope as full tracking of every selected survivor. Keep the explicit cross-epoch loss control in the committed suites.

**Evo refill:** the I34 regression passes. For the toy's deterministic fold, rebuilding historical states on the recovered path is the right replacement for zeros, and retaining the stored tip state preserves detection of a wrong current digest. Both Core pins reconstruct previous deterministic-MN lists using stored snapshots/diffs (`deterministicmns.cpp`, v23 `UndoBlock:736`, `GetListForBlockInternal:778`; v24 `UndoBlock:784`, `GetListForBlockInternal:826`) and update EvoDB's best-block marker on disconnect. The refill matches that high-level historical-state behavior. It is an abstraction, not a port of EvoDB undo: the toy does not exercise reads/loss of actual per-block EvoDB diffs/snapshots, and Core's special-transaction undo covers more than this one fold. Do not use this regression as proof of real EvoDB undo durability.

**PR trim:** e0db8d66 reduces the two smoke sweeps and restricts the assumevalid control to the flush publishing both Skipped and Full connects. Both properties remain exercised. DEC-101 Q5's S5c/S14b clean searches moved to nightly, with PR preconditions and planted witnesses retained. The older ignored S5b/S11r/S13/S11c searches also remain in the nightly/common scenario inventory; S11c and S14b's renamed/split tests account for the old function names. I found no additional required coverage lost specifically by the trim. The deterministic aggregate Work caps claimed by §8.3 remain unimplemented; this was already called out at CP2 and should not be described as an enforced gate.

## Validation and evidence

- `cargo test -p dashd-store --no-default-features --features faulty`: **205 passed, 17 ignored**, doc tests passed. The model, MicroLog, checker, CP2, Phase D, ToyChain, and feature-scope suites are green.
- `cargo test -p dashd-store --all-features`: **223 passed, 19 ignored**, doc tests passed, including the native RocksDB regression's two tests/four cases. The native dependency was linked from the existing RocksDB 11.8.1 static archive; all Rust artifacts were rebuilt in the review worktree's own target. No `CARGO_TARGET_DIR` was shared.
- The PR suites include W1's prefix-dependent post-crash controls with D1(f) retained and T17 caught, W2/W3/W5 violations, and MicroLog's W4 bonus control.
- Independent checker/control probes: **five pass, five deliberately fail**, demonstrating four code findings (two R7a probes cover the same suppression class). The passing probes include FA4, cross-epoch R13, the barrier-correct finalizer, and both durable-scratch detection controls.
- Nightly component `witnesses_with_large_records_extended_byte` at unmodified CP3: **one exact test passed**, all **43** planted bugs found (T1–T42 with T23a/b), zero truncated searches. Release execution used **323.98 CPU-s** (321.28 user + 2.70 system), 1,220.45 wall-seconds under host load. Independent repro regeneration was not rerun; the PR suites replay the checked-in repros, and the archived regeneration component passed.
- `cargo fmt --all -- --check`: **passed**.
- `cargo clippy -p dashd-store --all-features --all-targets -- -D warnings`: **passed** at restored, unmodified CP3, using the same native archive.
- `git diff --check`: **passed**. Before removal, the detached worktree was clean at exactly 546c4626. Removed that worktree and its build target, the mode-700 TMPDIR, and private native test images/mount directories. Verified no private device-mapper mapping or loop device remained. The report remains uncommitted in integration; retained evidence is listed below.

Evidence is retained in `/work/scratch/st3a-cp3-review/`: `review.diff`, `st3a_cp3_probe.rs`, `probes-final.log`, `cp2-regressions-at-old.log`, both `pr-gate-*.log`, `nightly-build.log`, `nightly-witnesses.{log,err}`, `fmt.log`, `clippy.log`, `nightly-status-probe.py`, `nightly-status.log`, and `native/eio.{sh,py}` with ext4/XFS logs. To repeat the checker probes, copy `st3a_cp3_probe.rs` into `dashd-store/tests/` of a detached CP3 worktree and run `cargo test -p dashd-store --no-default-features --features faulty --test st3a_cp3_probe -- --nocapture --test-threads=1` with the required mode-700 TMPDIR. The assertions intentionally require the promised behavior, so the five failures are expected evidence, not failures of the checked-in PR gate.

**Final verdict: CHANGES-REQUIRED. Sol gate satisfied: no.**
