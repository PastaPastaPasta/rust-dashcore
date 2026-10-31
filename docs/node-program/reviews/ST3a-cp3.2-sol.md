# ST3a CP3.2 — Sol confirmation

Reviewed **162fe639 → dcca3731a1c17109479856f219b50d226038d6a9**. The three original CP3.1 reproductions are closed. The Sol gate is nevertheless not satisfied: two concrete R7a masking cases remain, including the recorded I36 gap, and a failed advisory allocation creates an invalid size floor that produces a false R7a failure.

Read REVIEW-COMMON, my CP3.1 report, DEC-148/DEC-161/DEC-95, the design and I35–I39, and the actual commit-range diff. Applied the code-review-validator workflow. Reviewed the fixed commit in `/work/scratch/st3a-cp32-sol/wt`, a detached worktree with its own target directory. Builds and probes used `nice -n 10`, `CARGO_BUILD_JOBS=8`, `RUSTC_WRAPPER=`, and mode-700 `TMPDIR=/work/scratch/st3a-cp32-sol/tmp`. No implementation-branch edits, commits, pushes, GitHub comments, full nightly run, or embargo access. DEC-161 implementation is excluded from this review.

## Findings

### CP3.2-1 — Major: a copied earlier cell permanently exempts a branch even after it resolves to unchanged bytes

**Locations at dcca3731:** `dashd-store/src/faulty/fs.rs:628`, `:1744`, `:2676`; `dashd-store/src/faulty/world.rs:977`; `dashd-store/src/faulty/harness.rs:1152`.

The per-unit path still uses `inherit_seen`, which evaluates `changes_read` once, while constructing the crash cells. If the version the copy read contains an undecided earlier cell, `None != Some(0)` sets `observed=true` permanently. Unlike the size path, this judgment is not deferred until the branch's final decisions. It remains true even when that earlier cell subsequently resolves to exactly the bytes selected by the inner crash.

My `CopyFinalizer` first leaves an earlier power-loss cell in `src`: choice 0 supplies a zero first sector, choice 1 supplies sevens. A rule-L2 copy leaves that cell as a pending version of `f` over its durable zero sector, with a 4 KiB size. After the outer ProcessKill, recovery promotes the engine, copies `f` into `g`, and performs CP3.1's faulty finalizer: Buffered `flag`, then Durable `done`, without the intervening barrier. The inner crash follows `done`'s Append and loses `flag`. The next recovery reads `f`'s zero sector and, in the stronger control, reads `src`, deciding the earlier cell at **0**.

```text
crash 13 process-kill
nested 3 power-loss
decide 3:probe:b1=1
decide 3:fs:i2:u0^=1

uninterrupted projection: 0
interrupted projection:   9
earlier 1:fs:i1:u0:       Some(0)
changed 3:fs:i2:u0^:      observed=true
replay violations:       []
```

The earlier source sector, the selected destination sector, and the corresponding uninterrupted copy all contain the same zeros. A direct reference built with `start_choices` has projection 0, and its whole copied file equals the original uninterrupted copy. The faulty engine result is independent of the copy. Removing the copy reports R7a; adding the proper barrier is clean. The separate `resolved_earlier_copy_cell_should_not_mask_the_bug` fails at dcca3731 because replay still returns `[]` after the earlier cell has been decided.

This is **broader than the disclosed gap** about a cell that remains undecided after recovery. Here the final decisions completely settle the equality, but the unit path retains its earlier conservative answer.

**Suggested fix:** defer per-unit comparisons through earlier cells, including the version the read saw, and evaluate them against final branch/reference decisions. Audit the Write-granularity `changes_read` table too (`fs.rs:1572`): it uses the same immediate Boolean judgment. Preserve actual changed-byte exemptions, but drop the exemption when the supplied and selected bytes resolve equal. Add the resolved-copy negative control and retain the missing-barrier/fixed controls.

### CP3.2-2 — Major: the recorded I36 undecided-cell gap masks an independent finalizer failure

**Locations:** `dashd-store/src/faulty/fs.rs:648`, especially `:658`; `:681`; `dashd-store/src/faulty/harness.rs:1152`; design I36, `docs/node-program/designs/ST3a-power-loss.md:1388`.

The `deferred` fallback takes unresolved cells from the current crash at choice 0, but returns **true** when an earlier cell remains unresolved. This can exempt an entire branch although the matching uninterrupted reference decides that cell to a value that leaves the interrupted read unchanged.

My `FloorFinalizer` constructs this exact case under **Write granularity**. An earlier crash's cell supplies either zeros (choice 0) or sevens (choice 1); a lazy copy puts it into `f`. The forward epoch sets `f` to 4 KiB, then leaves a truncate to 512 bytes pending. After ProcessKill, recovery writes at 8192, reads byte 512, which its extension zero-filled, then runs the faulty finalizer. The inner cut keeps the copied 4 KiB state but loses the inherited shrink and the recovery's extension. The next recovery reads only `f`'s size, leaving the earlier content cell undecided.

```text
crash 14 process-kill
nested 3 power-loss
decide 3:probe:b1=1
decide 3:fs:i2:w^=2

uninterrupted projection: 0
interrupted projection:   9
earlier 1:fs:i1:w:        None
changed 3:fs:i2:w^:       observed=true
matching reference cut:  3:fs:i2:w^=1
reference's earlier cell: choice 0; byte 512 remains zero
replay violations:       []
```

The constructed reference has projection 0 and actually decides the older cell at 0. The byte the interrupted recovery read stays zero, so it cannot justify the independent finalizer divergence. A control that makes the next recovery read the earlier source decides that same cell at 0: the deferred judgment then becomes false, and replay correctly reports R7a. The properly barred finalizer is clean.

**Disposition of the known gap:** it is real and constructible. I do not consider recording it sufficient to pass this confirmation or close it later while claiming this R7a coverage. It suppresses the same independent missing-barrier defect this checkpoint is meant to catch, under Realistic storage outcomes; it is unrelated to DEC-161.

**Suggested fix:** refine an unresolved earlier dependency against the matching reference's decisions, or explicitly enumerate/refine the unresolved inputs before judging the exemption. If a dependency cannot be resolved, report incomplete coverage rather than silently counting the branch as an allowed observed-input exemption. The reference and the exemption must use the same inherited input assignment.

### CP3.2-3 — Major: a failed advisory allocation raises a floor it never established and creates false R7a failures

**Locations:** `dashd-store/src/faulty/fs.rs:2589`, `:2594`; `:992`; design I36 at `docs/node-program/designs/ST3a-power-loss.md:1388`.

The Fallocate path raises `len_floor` **before** `zero_gap`, which can fail with EIO. The operation then leaves the actual length unchanged, but the provenance model assumes the larger floor. `observe_len` consequently drops the lower-bound dependency of the size it really returned.

My `SizeDeriver` is a permitted pre-promotion control like the original Deriver: it memoizes the size read from inherited pending metadata. Its setup uses the model's L2 copy to carry an earlier mixed-block EIO into a pending 1,024-byte file. Recovery attempts the advisory allocation to 8,192 bytes. Its allocation event fails while zeroing the partial EOF block; the file stays **1,024** bytes. Recovery reads 1,024 and durably stores that result. No fatal read is swallowed: `allocate` is expressly non-fatal in §4.4, and the trait reports no allocation error.

An inner crash keeps that durable result but selects inherited size 0. A matching uninterrupted reference starts at size 0; its allocation succeeds without reading the old partial EOF block and it returns **8,192**. This is an allowed divergence from a changed inherited size. Instead:

```text
crash 13 process-kill
nested 3 power-loss
decide 3:fs:i2:len^=1

allocation event:         failed
actual interrupted size: 1024
recorded floor:           8192
interrupted projection:   1024
matching reference:       8192
size cell observed:       false
replay: R7a, "nested recovery after inner event 3 diverged"
```

The AllocateMode::Fail control leaves the true floor intact and is clean. This counterexample has no inner-crash EIO read or sanctioned-abort classification, so I39/DEC-161 cannot explain it away.

Core v24's `AllocateFileRange` (`src/util/fs_helpers.cpp:238`, `:266`, `:284`) is advisory: failed `posix_fallocate` falls back to writes whose failures are ignored. A failed allocation does not promise the requested length. A correct caller can ignore it and inspect the length, exactly as this control does. Requiring an 8,192-byte floor after failure is stricter than Core's semantics and violates the requested DEC-95 spirit.

**Suggested fix:** record the floor only after the allocation actually establishes it; preserve raising it for a successful no-op allocation under an already larger file. A scratch-only correction moving the update after successful `zero_gap`/`push_len` makes both advisory-allocation probes pass, with the changed size now correctly `observed=true`; all 25 existing size-reference tests still pass (**27 passed**). Its patch and validation are archived as `experimental-floor-fix.diff` and `floor-fix-controls.log`. The correction was subsequently reverted in scratch; the implementation branch is unchanged.

## Confirmation of CP3.1 and Core behavior

- **CP3.1-1: original reproduction closed.** The Overwriter reports R7a at dcca3731 and fails its expected-behavior assertion at 162fe639. The no-read negative and properly barred controls hold. The partial-overwrite control remains exempt where a preserved byte changes. The original Deriver's plain and sync-first variants both pass; the sync-first variant has no read or unmatched exemptions. Binding replacement and the supplied-operation lookup controls pass at the head. The residual masking cases are CP3.2-1/2 above.
- **CP3.1-2: closed.** Mixed durable/pending blocks have an EIO cell. The prefix is either unavailable or byte-identical; D1 accepts its preserved bytes, and the read-success branches return those exact bytes. R8 catches a recovery that carries on after model EIO; the sanctioned fail-stop control is clean. Partial-block zeroing failures and whole-block repair controls hold. I did not repeat the physical device-mapper controls; their archived native evidence is the basis DEC-148 accepts.
- **436a763e: the two changed toy paths are confirmed.** Independent targeted probes show that a recovered-parent read error aborts before another block is accepted, and activation's re-learn read aborts with `Failed to read block`, with no successful mutation after the failure. Both fail at 162fe639: the parent path returns `Ok(())`, and re-learn returns the unsanctioned generic replay error.
- **Core comparison uses the tag, not the checkout HEAD:** `v24.0.0-rc.3` = `da567d7dba945143eee4d2ce1621c21b32089989`. `ReadBlockFromDisk` catches I/O/deserialization exceptions and returns no hash (`src/node/blockstorage.cpp:805`, `:820`). `ConnectTip` turns that into `AbortNode(..., "Failed to read block")` (`src/validation.cpp:3409`), matching activation's re-learn path. Core's actual startup replay has a different outer message: `RollforwardBlock`/`ReplayBlocks` return false after the read failure (`validation.cpp:5186`, `:5266`), and `node/chainstate.cpp:251` returns `Unable to replay blocks. You will need to rebuild the database using -reindex-chainstate.` Thus `Failed to read block` is a real Core sanctioned read-error message, not the literal startup-replay caller's message. The model's normalization should not be described as exact message parity for every replay path.
- **CP3.1-3: closed.** Independent blocked-summary controls return 0 with the old runner and 1 with the head runner. Blocked component logs/time files, a nonremovable stale time directory, a failed component followed by success, and a zero-test component all return nonzero at the head. Writable success stays green.
- **Successful size floors:** `max(inherited_size, write_or_allocation_end)` and an absolute size after `ftruncate` match the inspected Linux Core operations. Eighteen native runtime controls cover writes inside/past EOF, successful allocations inside/past EOF, an unchanged truncate, and shrink/regrow; lengths and zero-filled gaps agree. These are runtime semantics controls, not physical crash experiments. The failed-allocation exception in CP3.2-3 prevents unconditional approval of I36's floor model.

## Validation and retained evidence

- **Independent head checks:** 46 targeted tests across `power_loss_cp31` (12), `power_loss_cp32` (9), and `power_loss_cp32_reference` (25): all pass. The original Deriver (1) and independent toy probes (2) also pass: **49 passing tests**. Four copy/size finalizer controls in one additional passing test show the missing-copy bug is reported, both fixed finalizers are clean, and deciding the earlier size dependency restores R7a detection.
- **Independent negative probes at the exact head:** `probe-head-final.log`: one control test passes and **three deliberately demanding assertions fail**, demonstrating unresolved-copy masking, resolved-copy masking, and the disclosed deferred-size gap. `floor-head.log`: its advisory-Fail control passes and its demanding floor assertion fails with the false R7a above. These are intentional checker-counterexample failures, not failures of the checked-in suite.
- **Historical contrast:** restored only the faulty source directory to 162fe639 in the detached scratch worktree and supplied the old runner. `baseline-source-diff.txt` is empty against that commit. The current CP3.1 regression file gives **4 passing controls, 8 expected failures**; the two independent toy probes also fail. The reviewed source was restored afterward. Internal validator archives demonstrate additional regressions at the predecessor of their respective fixes; I did not independently rerun every one of those historical rounds.
- **Archived PR gate:** `gate-cp3.2i.log` totals **291 passed, 0 failed, 19 ignored**, agreeing with the report. I did not rerun the whole gate or clippy under this box's load. Independent head formatting and `git diff --check 162fe639 dcca3731` pass.
- **Archived nightly:** `nightly-dcca3731/head.txt` names the exact reviewed commit; all **15** summary rows are successful and match their time files. Every component log records a nonzero passing-test count. The sweep records seed **20261010**, **20,000 cases**, and the summary ends `DONE failed=0 unwritten=0`. `artifact-audit.json` records these checks. Nested Extended coverage remains excluded under I39; DEC-161's subsequent implementation has not been reviewed here.
- **Reproduction:** copy `probes.rs`, `floor-probes.rs`, or `toy-probes.rs` from the evidence directory into a detached dcca3731 worktree's `dashd-store/tests/` under the corresponding `st3a_cp32_sol_*.rs` name, then run the relevant single test target with `cargo test -p dashd-store --no-default-features --features faulty --test TARGET -- --nocapture --test-threads=1`, using the prescribed nice/environment/TMPDIR constraints. Setup crashes are deliberate and included in the probe code; the printed repros replay with these SUTs.

Evidence is retained in `/work/scratch/st3a-cp32-sol/`: the exact review diff, probe sources and logs, historical contrasts, runner/native controls, Core tag excerpts, artifact audit, and the experimental floor correction. The detached worktree's tracked source is restored exactly to dcca3731; its probe files are untracked. The private TMPDIR and historical blocked-output leftovers were removed. The implementation branch remains untouched. This report is uncommitted in the integration worktree.

**Verdict:** CHANGES-REQUIRED

Sol gate satisfied: no
