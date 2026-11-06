# ST3a checkpoint 3.1 — Sol confirmation

**Verdict: CHANGES-REQUIRED.** The original CP3 reproductions are fixed and the requested PR gate passes. A new counterexample still suppresses R7a, and I35's prohibition on unreadable blocks containing durable bytes does not follow from A-PSOW or the native filesystem controls. The nightly aggregates component failures correctly, but ignores failure to write its required summary.

Reviewed **546c4626 → 162fe63980dd779f263cb1d873b83ac43310bbf1**, nine commits, in `~/workspace/rdc-wt/st3a-power-loss`. Read REVIEW-COMMON, the prior Sol CP3 review, the ST3a design including I35–I38, and the relevant program context. Applied the code-review-validator workflow. The implementation branch was kept unchanged. Experiments used a detached worktree under `/work/scratch/st3a-cp3.1-review/`, with `TMPDIR=/dev/shm/st3a-cp3.1-sol-tmp` mode 700, `RUSTC_WRAPPER=`, and nice 10. No commits, pushes, or GitHub comments.

## Findings, by severity

### CP3.1-1 — Major: a read after a complete overwrite incorrectly exempts an independent R7a failure

**Locations:** `dashd-store/src/faulty/fs.rs:297`, `:719`; `dashd-store/src/faulty/harness.rs:1152`; design I36 at `docs/node-program/designs/ST3a-power-loss.md:1388`.

`inherited_unit` asks whether **any historical version** was pending before the epoch. It does not ask whether the version actually read still contains inherited bytes. Consequently, reading a sector fully overwritten by the recovery records a read of its obsolete inherited input. A subsequent crash that selects that old input sets `observed=true`, and `r7a_reference` exempts the entire comparison.

The new `Overwriter` probe starts with a durable engine baseline and an unsynced 512-byte scratch file containing 7. After the outer ProcessKill, recovery promotes the engine, overwrites **all 512 bytes** with 8, reads those new bytes and ignores them, and runs CP3's faulty finalizer: Buffered `flag`, then Durable `done`, without the intervening barrier. When `done` already exists, recovery also best-effort reads the scratch file before overwriting it; this forces the surviving filesystem cell on the interrupted branch. None of the scratch reads contributes to the projection.

Crash after `done`'s Append, before its Barrier. Lose `flag` and select the scratch sector's older zero version:

```text
crash 3 process-kill
nested 3 power-loss
decide 2:probe:b1=1
decide 2:fs:i1:u0^=2

uninterrupted projection: 0
interrupted projection:  9
inherited u0: changed, observed=true
replay violations: []
```

The interrupted recovery before this crash read only its own complete replacement, **8**, rather than the inherited **7**. Selecting another version of that inherited input cannot justify pre-promotion divergence from that read. Engine promotion had completed before the faulty finalizer. The same missing-barrier bug, with only the irrelevant read after the overwrite removed, reports R7a. Adding the proper barrier is clean. A direct image/projection probe independently verifies 0 versus 9.

**Suggested fix:** track the inherited bytes contributing to the current version. A complete overwrite must remove its inherited read dependency while preserving historical versions for crash selection; a partial overwrite must retain the dependencies of the bytes it preserves. Audit the analogous binding/size observation paths for historical operations hidden by newer ones. Add this negative control and retain the partial-overwrite/Deriver positive controls. Merely narrowing the global skip to a cell whose history was read is insufficient when that history did not supply the observed value.

### CP3.1-2 — Major: I35 assumes block readability from byte preservation and excludes a native EIO outcome

**Locations:** `dashd-store/src/faulty/fs.rs:1215`, `:1241`; `docs/node-program/designs/ST3a-power-loss.md:1387` (I35); the A-DEV/A-PSOW definitions at `:242` and `:243`.

**I confirm the 4 KiB buffered-I/O boundary and the usefulness of one readability cell per block. I do not confirm the additional ruling that A-PSOW forbids EIO whenever any byte in that block is durable.** A-PSOW says bytes outside an unsynced write's range are unchanged. It does not guarantee that a larger buffered read succeeds when an unreadable sector lies inside the unsynced range. A-DEV likewise preserves synced data; the design separately permits read EIO and excludes corruption of durable data, not all read errors involving it.

I independently repeated private loop/device-mapper controls on Linux **7.0.0-38-generic**, with both ext4 and XFS using **4 KiB filesystem blocks**. This time the file has a **512-byte prefix written and fsynced**, followed by **3,584 appended bytes without fsync**. `sync_file_range` performs writeback only, then the test file's cache is evicted. Only the block's last sector, inside the appended range, is mapped to an error target. Both filesystems give:

```text
O_DIRECT pread(0, 512): 512 bytes, original synced prefix unchanged
buffered pread(0, 512): EIO
buffered pread(3584, 512): EIO
buffered pread(0, 4096): EIO
pwrite(0, 1), (0, 512), (3584, 512): EIO
pwrite(0, 4096): 4096 bytes written
```

Thus the durable prefix's bytes remain intact and independently readable, while a buffered read fails because it also reads the unsynced tail's bad sector. No write or error target touches the prefix. These are native I/O controls, **not a physical power-cut or device-ECC experiment**; they establish the filesystem consequence of the unreadable-sector outcome that Extended already claims to model. They do not prove how frequently a particular device produces that outcome.

The corresponding model setup produces **no `eio0` cell**: `durable_block` suppresses it. Its prefix read always succeeds and `range_durable` returns `Ok(())`. This removes recovery/read-error paths even though byte preservation alone does not exclude them. The new test deliberately requiring the demonstrated native outcome fails at the reviewed head.

**Suggested fix:** retain block-level propagation and a shared readability cell, but distinguish preservation of durable bytes from read availability. Allow the unreadable unsynced-sector outcome in a mixed block without corrupting the durable bytes, and reconcile D1/abort semantics with that outcome. If the program deliberately grants the stronger assumption that such blocks always remain readable, document it as an additional filesystem/device bonus, supply its witness and ST3b validation owner, and narrow the filesystem-coverage claim. Do not derive that bonus from the current A-PSOW wording.

### CP3.1-3 — Minor: the nightly returns success after failing to write its summary

**Locations:** `contrib/node/st3a-nightly.sh:43`, `:46`, `:88`, `:90`.

With an existing **directory** at `OUT_DIR/summary.txt`, each summary redirection fails. A passing component still runs and the runner exits **0**, despite writing neither the seed nor the component rows nor DONE. The independent control emits four `Is a directory` errors and returns success. This also covers unchecked reporting-write failures such as exhausted output storage.

This does **not** refute the component-status fix: failure followed by success, termination by signal, a zero-test match, a missing binary, and a failed build all return nonzero in independent controls. It does refute an unconditional claim that the runner cannot return green after something required failed.

**Suggested fix:** check summary creation and every required artifact write, and propagate reporting failures into the aggregate status. Keep explicit handling around component execution so a component failure can still be collected without prematurely skipping later components. Add a blocked-output regression.

## Disposition of CP3 and additional controls

- **CP3-1: original finding closed.** Decided unreadability is NotDurable; undecided readability remains Unknown without forcing a cell. The original committed-reference probe passes. A new reference to an untouched zero hole in the same unreadable block also remains uncertified and produces D1a once a read decides EIO. The model-coverage concern in CP3.1-2 is separate.
- **CP3-2: closed.** Immutable height, parent and file comparisons precede both acceptance paths. Eligible finalized pruning passes; height/parent/file tampering and an unfinalized selection fail. New arbitrary status-bit corruption, nonzero reset positions, and a finalizer naming a different flush all report R5. The actual toy emits the finalization mark only after that flush's successful Durable write; unrelated later barriers no longer entitle it.
- **CP3-3: original probes closed; masking class remains open.** Both original pending-scratch finalizers now report R7a. Changed unread inputs are compared against constrained recoveries, including cells decided by projection. The checked-in searches find the bugs and their fixed controls are clean. The new complete-overwrite counterexample is CP3.1-1. A partial-overwrite Deriver control still permits the intended pre-promotion divergence.
- **CP3-4: boundary defect closed; I35 eligibility not confirmed.** Sector and Byte controls now fail reads anywhere in an unreadable block, reject partial-block writes, and permit full-block repair. New crossing-boundary reads, a write partly covering the good block but wholly replacing the bad block, and truncate/re-extension controls pass. One block has one EIO cell; the checked-in Byte control explores 33 branches. The mixed durable/pending block outcome is still missing.
- **CP3-5: original component aggregation and zero-test gaps closed.** Independent failure/status controls pass as described above. Required reporting errors remain CP3.1-3.
- **CP3-6: closed.** With the configured seed unset, mocked UTC dates 20261009 and 20261010 produce the corresponding different recorded seeds, even under a non-UTC `TZ`. The real ignored sweep test accepts seed 20261009, prints its decimal/hex representation and runs successfully for one case. The seed is passed through the environment and recorded in `summary.txt`; fixed PR seeds remain unchanged.

The two disclosed nits—duplicate D1a/D1d diagnostics for an unreadable referenced range and no per-component timeout—do not independently block this confirmation. No timeout means a component can hang; it does not make a failed component green.

## Validation and evidence

- **Requested gate:** `cargo test -p dashd-store --features faulty` at the unmodified head: **245 passed, 19 ignored, 12 test binaries**, doc tests passed, exit 0. `/usr/bin/time -v` records **201.64 user + 17.11 system CPU-s**, **139.60 wall-s**, including compilation. This is not a like-for-like comparison with the implementer's test-only 79 CPU-s claim.
- **Historical independent probes:** restored only the faulty source directory to 546c4626 in the detached scratch worktree and ran the original CP3 probe file: **five expected failures, five passing controls**. At the reviewed head all **ten pass**, changing only the two unreadable-choice names to the new block cells. The expected old failures are D1, R5, partial writes, and both R7a masking probes. The provided `cp3.1-demo/regressions-at-546c4626.log` separately records 12 failing checks and seven controls; it is supporting evidence rather than my own execution of that newer file on the old source.
- **Additional head controls:** 30 tests (the 22 checked-in CP3.1 tests plus eight added adversarial/control tests): **28 pass, two deliberately fail**, demonstrating CP3.1-1 and CP3.1-2. Runner blocked-output control separately demonstrates CP3.1-3. The detached worktree's tracked source was restored to 162fe639 before final cleanup.
- **Static checks:** `cargo fmt --all -- --check`, `cargo clippy -p dashd-store --features faulty --all-targets -- -D warnings`, and `git diff --check 546c4626..162fe639`: passed.
- **Archived nightly limitation:** `nightly-162fe639/summary.txt` at seed 20261009 has **two** successful components—witnesses and nested depth 1—then `DONE failed=0`. It is not a complete 15-component nightly. Its witness CPU is 441.01 s versus the archived CP3 295.23 s, approximately **49.4% more**, consistent with the reported cost. I did not independently run a full nightly at this head; the earlier complete 99fce744 run does not establish that head's full nightly result.
- **Cleanup:** private native mounts, device-mapper mappings and loop devices removed; no test instances under `/work/chains` or `/work/bench` touched. Removed the detached experiment worktree, private image files, and mode-700 TMPDIR after the checks. The review is uncommitted in integration; unrelated integration changes were preserved.

Retained evidence: `/work/scratch/st3a-cp3.1-review/` contains `review.diff`, `pr-gate.log`, `original-probes.rs`, `original-probes.log`, `original-probes-at-546c4626.log`, `adversarial-probes.rs`, `adversarial-probes-final.log`, `native/eio.{sh,py}`, `native/{ext4,xfs}.log`, `nightly-controls/probe.py`, `nightly-controls.log`, `nightly-controls/extra.log`, `seed-sweep.log`, `fmt.log`, and `clippy.log`. To reproduce the new checker controls, copy `adversarial-probes.rs` to `dashd-store/tests/st3a_adversarial_probe.rs` in a detached head worktree and run `cargo test -p dashd-store --no-default-features --features faulty --test st3a_adversarial_probe -- --nocapture --test-threads=1` with the required environment. Its two failing assertions intentionally demand the behavior missing from this head.

**Final verdict: CHANGES-REQUIRED.**

Sol gate satisfied: no
