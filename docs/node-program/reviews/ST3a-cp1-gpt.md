# ST3a checkpoint 1 review — GPT

Revision: `c89e7ad6c3feb546d6024761f9fa31e0b5b434a8` (`node/st3a-power-loss`), phases A/B. Fresh detached worktree: `/work/scratch/st3a-cp1-gpt/wt`. Read REVIEW-COMMON, rev5 (especially §§2, 5, 6.4, 7, 8), and the GPT round-1 through round-6 review findings/closure history. This file was written before validation and updated as findings were confirmed. No source edits, commits, pushes or GitHub comments.

## Findings by severity

### Major 1 — D1 ignores already-decided crash cells and misses unsafe recovery ordering

**Locations:** `dashd-store/src/faulty/engine.rs:288`, `dashd-store/src/faulty/fs.rs:286`, `dashd-store/src/faulty/fs.rs:837`, `dashd-store/src/faulty/check.rs:264`.

`WorldDurableView` clones stores but carries no decision state. `durable_get` explicitly passes a callback that always returns `None`; `durable_binding` also treats every variable as unknown regardless of its recorded decision. The filesystem treats an unknown binding as success in `range_durable`, and `file_durable` returns true. Consequently lenience is not restricted to cells the SUT has left undecided.

**Executable planted ordering bug:** sync `/dd/f` and its directory, append a Buffered `ref` whose extractor requires its first 512 bytes, then PowerLoss with the batch surviving. Read `ref`, resolving its survival to true. Sync the file (a barrier that must re-evaluate standing requirements), unlink it before removing `ref`, and sync the directory. The actual durable record survives and its file is absent; **D1 reports no violations**, and the durable view still returns `Unknown` for the already-read record. The identical forward-run ordering reports D1(e) and D1(a).

**Second control:** start with a synced file and an unsynced unlink. After PowerLoss retain the unlink and observe `len(file) == None`. Now append a *new Durable* `ref` to this absent file. D1 again reports nothing and `file_durable(file)` returns true. This is not merely skipping a speculative old durable record: a newly finalized record references an already-observed missing file.

**Fix:** include branch decisions in the durable view and evaluate decided batch bits, directory bits, size cells and unit cells without making new decisions. Preserve unknown as a distinct result; do not silently certify it as durable. For genuinely unresolved standing requirements, supply a conservative/pending check that will be revisited when relevant cells become decided, or explicitly resolve the necessary cells in checker branches. Add these recovery regressions. The claim that later R-checks cover lenience does not establish D1's promised standing cross-store coverage at intermediate recovery barriers, and phase-C checkers are not implemented at this checkpoint.

### Major 2 — Fail-stop checking misses recovery errors; read faults can produce a clean zero-image exploration

**Locations:** `dashd-store/src/faulty/world.rs:873`, `dashd-store/src/faulty/harness.rs:414`, `dashd-store/src/faulty/harness.rs:492`, `dashd-store/src/faulty/harness.rs:509`.

`begin_read` removes and returns a matching fault but records no failed observation/trace entry. `check_fail_stop` sees only mutating events and explicit `StoreFailed` marks, so it cannot enforce §4.4's fatal read rule in Recovery/Connect when a buggy SUT ignores the error. The fault-run capture window also starts by matching the read selector against a *mutating* event hook. A read fault never matches that hook, so there are no repaired crash/recovery images. In addition, the generic R8 checker is called only for the forward run, not by `reopen_and_check`.

**Executable independently planted bug:** a small SUT enters `RecoveryBegin`, performs a read with injected EIO, ignores the error, creates another file, ends recovery and returns success; it then performs another mutation. `explore` with this read fault and Exhaustive mode returns **images 0, branches 0, truncated 0, found {}**, and `is_clean()` is true. Neither the illegal ordering after a fatal read nor the missing exploration fails the gate.

**Recovery-path control:** a second small SUT behaves normally in epoch 0. On reopen only, it enters Recovery, calls `rename` on an absent source (a failed mutating event), ignores that error and creates a file before ending recovery successfully. Exhaustive exploration reports **images 1, branches 2, truncated 0, found {}**, again clean. Thus fixing read-fault recording alone does not fix R8: a failed mutation is already in the recovery trace, but the generic checker never examines it. MicroLog's `check` also does not call `check_fail_stop`, so the current built-in SUT does not supply an alternative generic recovery check.

**Fix:** record injected read failures and their region in the trace while keeping successful reads out of the crash-point set. Activate fault-run capture when the selected fault actually fires; capture the fatal-fault state and boot repaired stores as §4.4 requires. Run R8 checks on recovery traces, including sanctioned-abort exits, and assert that specified fault plans actually execute. Add this ignored-read regression and a control that aborts before mutation. Read-fault injection is present in phases A/B and §4.4 is part of its contract; this cannot be justified solely by deferring Extended or nested scenarios to phase D.

## Counterexample probes and validation

The standalone probe source is `/work/scratch/st3a-cp1-gpt/probes/probes.rs`; output is `probes/results.log`. It uses the public API against the reviewed compiled library; the reviewed tree remains clean.

- Two sectors with durable/intermediate/newest values 1/2/3: all nine combinations are reachable, including (1,3) and (3,2). Sector tearing across versions is modeled.
- Synced size 1024, unsynced append to 1536, rename and completed directory sync: the renamed file can have either size. Rename/directory sync does not over-grant data or size durability.
- One directory entry synced before two pending creates, followed by file sync of the second: the old entry always survives and all four subsets of the later entries are reachable, including later-without-earlier. No file-sync directory bonus is granted.
- Two Buffered WAL batches: all four subsets are reachable, including later-without-earlier, as DEC-62 requires.
- The two independent planted bugs above escape their generic checkers; the D1 forward control is caught.

Validation completed:

- `cargo test -p dashd-store --no-default-features --features faulty --test power_loss_model --test power_loss_micro`: **24 model tests + 11 MicroLog tests passed**, with two intentionally ignored tests (scale sweep and repro regeneration). All P1–P8 witnesses and checked-in repro replay pass. Test-body wall times: model 4.11 s, MicroLog 0.06 s.
- `cargo test -p dashd-store --features faulty --test rocks_wal_nonprefix`: **2 tests / all four native cases passed**, pinned `librocksdb-sys v0.19.0+11.8.1`, test-body wall time 0.37 s. This validates edited persisted-image recovery, not a physical power-cut guarantee.
- `cargo test -p dashd-store --no-default-features --features faulty --test faulty_feature_scope`: **passed**; runtime binary feature isolation holds.
- Standalone probes compiled with `rustc` against the exact reviewed no-default/faulty rlib and passed all assertions, including assertions documenting checker escapes. Logs are `tests.log`, `native.log`, `feature-scope.log` and `probes/results.log` under the scratch root.
- `git status --short` is empty and `git diff --check` passes in the detached worktree. No reviewed source changed.

Builds used `CARGO_BUILD_JOBS=8`; test/compiler temporary files used mode-700 `/dev/shm/st3a-cp1-gpt-tmp`, removed after validation. The checked runs provide checkpoint-level timing evidence, not the unimplemented ToyChain's full §8 43.8 CPU-s calibration or a nightly result.

## Model and deviation assessment

**No confirmed blocker from over-granting actual crash survival was found in the reviewed Realistic storage probes.** The WAL subset, independent directory operations, sector histories, size reversion across rename, and ProcessKill-to-PowerLoss pending-state tests support the intended adversary. D1's durability certification is unsound as above; that is a checker failure, not evidence that the crash image itself incorrectly persists additional data. An exact-design fidelity/acceptance claim is nevertheless unjustified while these checker and fault-exploration gaps remain.

- **D1 lenience after a crash: not acceptable as implemented.** Avoiding forced decisions can be a reasonable search optimization, but already-decided cells must be reflected faithfully and unresolved requirements cannot be silently certified. Major 1 gives concrete losses of standing coverage.
- **`AfterMark` occurrence index: acceptable.** It selects an unambiguous repeated mark for a narrow witness window without changing storage survival domains. Keep repro selectors deterministic.
- **`InsideWrite` decided at boot: acceptable timing deviation for this checkpoint.** It eagerly enumerates the finite page-prefix choice rather than waiting for observation; the recorded decision remains branch-consistent. The implementation limits interruption to the last write's event, and its regression checks that later events cannot retroactively tear a completed write or regrow a synced truncated file. This costs extra irrelevant branches but does not itself grant durability. No claim of completed lazy `InsideWrite` optimization is warranted.
- **Phase-D adversaries: acceptable deferral within §10's stated split.** Extended garbage/byte behavior, deferred sync/read errors, failed-sync old/new read variants, assumption witnesses (including WAL A-PSOW), and nested/nightly acceptance must be completed and validated there. They are not grounds to claim the entire rev5 adversary is complete now. The existing Realistic read-fault path and ordinary recovery fail-stop enforcement in Major 2 are separate from this allowed deferral.

The historical design findings about finalizer scopes, locator submission, prune selection and subscriber locks remain phase-C obligations; this review does not reopen their approved rev5 design closures or claim those unimplemented protocols have passed.

## Verdict

CHANGES-REQUIRED.
