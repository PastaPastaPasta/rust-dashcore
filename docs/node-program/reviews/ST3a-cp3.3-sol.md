# ST3a CP3.3 — Sol review

Reviewed **dcca3731a1c17109479856f219b50d226038d6a9..500dabd090fe1d9081136f63041a3ec420542944**, the exact head of `node/st3a-power-loss`. **CP3.2-1, CP3.2-2 and CP3.2-3 are closed. Two new major findings prevent approval:** the judge manufactures a missing SUT abort marker, and the nested sampler strongly favors short decision paths rather than sampling the remaining branches uniformly.

Read the reviewer contract at `briefs/REVIEW-COMMON.md` in `~/workspace/node-program` (the requested `docs/node-program/REVIEW-COMMON.md` is absent), the prior CP3/CP3.1/CP3.2 Sol reports, DEC-95/148/161/162/165, and the design. The implementation checkout's `docs/node-program/designs/ST3a-power-loss.md` supplies the current I35–I41 and I42; the external design copy is older. Applied the code-review-validator workflow and inspected the actual range diff, including the deleted provenance code and all seven DEC-165 commits.

Work used the requested detached `/work/scratch/st3a-cp33-sol/wt`, with its own `target/`. All Cargo builds/tests/checks used `nice -n 10`, `CARGO_BUILD_JOBS=8`, `RUSTC_WRAPPER=`, and mode-700, on-disk `TMPDIR=/work/scratch/st3a-cp33-sol/tmp`. The implementation worktree was untouched. No commits, pushes, GitHub actions/comments, systemd operations, process-pattern kills, chain/bench access, private issue access, or full nightly run.

## New findings, by severity

### CP3.3-1 — Major: the harness supplies the abort whose absence R8 must detect

**Locations at 500dabd09:** `dashd-store/src/faulty/harness.rs:905–916`, `:925–931`; R8's missing-marker check at `:655`; reference classification at `:1393`, `:1409`, `:1421`.

The new block applies to **every** `Sut::open` error, including an error after a fatal store failure. It inserts `Mark::Abort` before R8, `torn_fail_stop`, and `sanctioned_stop` inspect the trace. Consequently a system that failed to emit its required abort marker passes the same check as a system that emitted it. This contradicts the R8 contract: after the first fatal error, the epoch must end with the **system's** `Mark::Abort` carrying a sanctioned string. The distinction matters because this trace is also evidence for the DEC-161/165 fail-stop allowances.

**Concrete scenario and independent evidence.** My `Marker` SUT writes and syncs `/dd/f` and its directory. After ProcessKill, it enters recovery and `ConnectTip`, hits an injected EIO on its first file read, and returns `Err(Abort::new("Failed to read block"))`. The defective mode emits **no** abort mark. There is no later write and no unsanctioned message to distract the check.

At the exact head:

```text
MARKER unmarked: []
trace ends: ReadFailed(fs, Read, /dd/f), Mark::Abort("Failed to read block")
```

The SUT did not produce that last entry; the harness did. The demanding assertion that this branch report R8 fails. The correctly marked control passes. A wrong-message control still reports R0 and R8, and a write-after-error control still reports R8.

I temporarily restored **only `harness.rs` to 9e45f2c5d** in the detached worktree; its difference from the head is exactly these twelve added lines. The identical defective SUT then reports:

```text
R8: epoch 1 hit a fatal error and did not abort
Model: epoch 1 ended inside the Core paths ["ConnectTip"]
```

Its trace ends at `ReadFailed`, and the demanding assertion passes. Restored the head afterward. Evidence: `probes.rs`, `probes-head-extended.log`, `marker-before-head.log`, and `head.diff` in the evidence directory.

This demonstrates a **marker-compliance false pass**, not physical data loss. The change does not bypass the unsanctioned-message R0 check, nor hide successful mutations after a fatal error. Ordinary T4/T5 refusals still report R0. It does erase the independent evidence that a fatal-error handler emitted its abort, and can make the synthesized trace qualify for a nested sanctioned fail-stop. It also makes the paths inconsistent: `open_recorded` and `reference_outcome` call the SUT without manufacturing the mark, whereas `reopen_and_check` does.

**Suggested fix:** preserve the original SUT trace for R8 and fail-stop classification. Treat a returned error as a terminal outcome for **path-balance accounting** without claiming the SUT emitted `Mark::Abort`, or have the toy's refusal paths explicitly mark their outcomes. If a refusal-only normalization is retained, distinguish an ordinary refusal from an error after a fatal operation and never let a harness-generated mark satisfy the latter's R8 obligation. Add the unmarked-fatal-return negative alongside the correctly marked, wrong-message and late-write controls. Do not fix the reference inconsistency by manufacturing markers there too.

### CP3.3-2 — Major: per-cell walks can hide almost the entire remainder after the cap

**Locations:** `dashd-store/src/faulty/harness.rs:1264–1304`, especially `:1291–1295`; `dashd-store/src/faulty/decide.rs:179–183`; `Report::is_clean` at `harness.rs:340`. DEC-165 (2) requires a deterministic seeded **uniform sample of the remaining branches**. I41 instead describes the implementation's weaker per-cell measure.

Each draw chooses uniformly at each cell it reaches. A decision-tree leaf therefore has probability equal to the product of the reciprocals of its encountered domains. Short paths can be vastly more likely than long ones. Rejecting previously explored leaves and duplicate draws does not turn this into uniform sampling of the remaining leaves. The fixed `8 × samples` draw budget can be spent almost entirely on those short paths, after which the report is clean regardless of how little of the remainder was sampled.

**Concrete scenario and independent evidence.** My `Skew` SUT carries earlier power-loss cells into an outer ProcessKill and two nested recovery images. Fifteen binary reads can each terminate recovery early. If all fifteen survive, another binary read selects one of two dense subtrees, each containing 4,096 leaves from twelve further binary reads. A planted R1 violation occurs throughout the second dense subtree. The test uses `Extended` and the real explorer/sampler, not a separate random-number simulation.

For each inner image the tree has **8,207 leaves**. The first 4,096 DFS leaves are exactly the first dense subtree. The remaining 4,111 leaves include **4,096 failing leaves**: 99.64% of the remainder. A branch-uniform sample of that remainder should find this class readily. A per-cell draw reaches it with probability only **2^-16**.

```text
full cap 9000:       branches 16415, truncated 0, found {R1}
cap 4096, no sample: branches  8193, truncated 2, found {}
cap 4096, sample 512 per image:
  seed 7:           branches  8216, sampled 2 (23 branches), clean, found {}
  seed 20261010:    branches  8216, sampled 2 (23 branches), clean, found {}
  seed 20261011:    branches  8217, sampled 2 (24 branches), clean, found {}
```

All sampled runs have zero unjudged branches. The full search's R1 finding replays at the exact head:

```text
crash 49 process-kill
nested 0 process-kill
decide 1:probe:b15=1
adversary extended
expect R1
```

The two positive controls pass: full enumeration finds the defect, and disabling sampling leaves the capped search visibly truncated. The demanding sampling assertion fails for all three seeds. Repeated runs under Realistic and Extended produce the same counts for these engine-cell trees. Evidence: `probes.rs`, `probes-head.log`, `probes-head-extended.log`.

This is a distribution/coverage failure, not an objection that every finite random sample must find every bug. The failing class dominates the remaining **branches**, yet the implementation samples under a different measure. I41 acknowledges that measure; DEC-165 does not authorize replacing its branch-uniform requirement with it. The checked-in sampling regression only covers a small remainder that happens to be exhausted, so it does not test this case.

**Suggested fix:** retain and sample the unexplored DFS frontier, with explicit coverage across its sibling subtrees, and use subtree weights/counts if claiming branch-uniform sampling. Report an exhausted draw budget and achieved sample size per image instead of allowing duplicate-heavy walks to stand for the requested sample. If the intended measure is deliberately uniform complete cell assignments rather than uniform remaining decision branches, obtain an explicit amendment to DEC-165 and document the resulting short-path bias and coverage limits; the present uniform-branch claim is unsupported. Add the uneven-tree negative control and preserve deterministic seed/repro behavior.

## Closure of CP3.2-1..3

- **CP3.2-1 — Closed by mechanism.** `inherit_seen`, `observed`, and the permanently conservative Boolean are deleted. `InheritedDecision` now supplies only a start value for reference construction; it cannot grant a pass. `References::search` (`harness.rs:1668`) requires an actual matching outcome under every enumerated completion. The pinned CopyFinalizer replay reports R7a both with the old cell unresolved and with `decide-old` resolving it. The no-copy defect is caught and the properly barred finalizer is clean (`power_loss_cp33_witness.rs:204`). My fresh variant adds a half-range zero write before the original zero write, creating another older source version; both copies and resolved-cell controls remain caught. Its full defective/fixed searches cover **554/408 branches**, with zero unjudged.
- **CP3.2-2 — Closed by mechanism.** `When`, `deferred`, and the unresolved-older-cell `unwrap_or(true)` exemption are deleted. The Write-granularity FloorFinalizer reports R7a both with its older dependency unresolved and resolved; its proper-barrier control is clean (`power_loss_cp33_witness.rs:217`). The same additional older-version mutation produces **296 defective / 280 fixed branches**, with zero unjudged. Increasing the assignment burden cannot silently restore an exemption: the existing bound control reports **11 unjudged at bound 1**, refuses a clean report, and has **zero at bound 64**.
- **CP3.2-3 — Closed by removal of the erroneous state.** `len_floor`, the floor-driven size observations and the read-provenance machinery are gone. The real Fallocate path advances length only after successful `zero_gap` (`fs.rs:2085–2090`); a failed advisory allocation cannot establish an imagined comparison floor. The pinned SizeDeriver passes under both Fallocate and Fail (`power_loss_cp33_witness.rs:335`). My fresh variant allocates at offset 512 for length 8,192, instead of at offset zero; both modes remain clean. This matches the advisory nature of Core v24's `AllocateFileRange` (`util/fs_helpers.cpp:238`, `:266`, `:284`).

Deleting `power_loss_cp32_reference.rs` and the flag-oracle tests is appropriate for the removed mechanism. The required behavioral controls remain: Deriver plain and sync-first, complete overwrite, partial overwrite, binding/operation matching, durable-byte preservation, and the new finalizer/witness regressions. No replacement provenance-based pass remains in the reviewed source.

## DEC-161/162 and DEC-165 assessment

**Witness rule.** The references compare `Outcome::Completed(projection)` and `Outcome::Aborted(message)` exactly; a different abort or a panic cannot witness a branch. Outer recovery decisions are fixed separately from projection-only decisions. The latter's omitted values cannot use the convergence shortcut (`harness.rs:1562–1569`). Joint completion uses one shared choice per cell; domain/order/early-exit mismatches poison the enumeration and report unjudged (`:1791–1838`). The bound participates in `Report::is_clean`. The outcome, projection-only, bound and finalizer regressions pass. I found no remaining instance of the CP3.2 exemption classes; this is not a proof over arbitrary SUTs.

**Torn-block fail-stop.** `torn_fail_stop` (`harness.rs:676`) requires R8 to hold, identifies the unreadable operation at the first fatal error or the latest preceding one without intervening successful mutation, and rejects an earlier non-torn unavailable failure. The filesystem classifies the actual current failing unit's EIO wrapper against the current boot's torn set (`fs.rs:866–883`), rather than treating any historical torn version as sufficient. The positive log and P1 controls pass. Unsanctioned messages, writes after the read error, aborts with no unreadable block, outer-only tears, and copied outer EIO are caught. These guarantees are weakened by CP3.3-1 because R8 is then evaluated against a repaired trace.

**Recovered after fail-stop.** Subject to CP3.3-1, the DEC-165 (1) guard itself is correctly wired. `reopen_and_check` includes the store checks, view checks, R8 and D1 in its violations; explore, sweep and replay pass `violations.is_empty()` into the common judge. At `harness.rs:1574–1581`, the exemption requires both an actual fatal-error-based reference fail-stop and clean branch checks; it does not erase any other finding. Restarter's correct mode has **14 recovered-after-failstop branches**; its bad completed state reports **R1 and R7a**, with **zero** allowed recovered-after-failstop branches, and its findings replay. A voluntary refusal is not a fail-stop merely because its message is sanctioned: the voluntary control's relevant completing branches still report R7a. The rule intentionally permits a different completed projection under DEC-165; unimplemented or incomplete SUT checks/extractors cannot be replaced by that rule.

**Fail-stop after fail-stop (9e45f2c5d, I42).** Mechanically sound as an extension of DEC-165's allowance for a later start: it requires both runs to be sanctioned fatal-error fail-stops, R8 holding, no matching reference, and every check available on the aborted branch clean (`harness.rs:1628–1635`). It is counted separately and does not swallow store/D1/R0/R8 findings. The two S1 nightly repros replay clean. Restarter's `again` control has **14** such branches; `again-bad` reports its store-check R1 and R7a and permits none, with replayed findings. An aborted branch has no completed ChainstateView on which to run the view checks; the claim is consequently limited to store checks, R0, R8 and D1. I found no independent masking counterexample to this extension. I42 records an implementer ruling under “decide, record, continue,” rather than an explicit new manager decision; acceptance of that policy extension remains the manager's choice. CP3.3-1 still invalidates the evidence of a missing abort.

**Sampling.** Seed recording, stable ordering, mixing the outer image/kind/decisions and inner event/kind, deduplication by full decided branch, and using the common branch judge/repro builder support reproducibility. The checked-in tests reproduce the small remainder and replay sampled findings. Only `nested_depth1_extended` opts into sampling; outer images and the other components retain truncation failure. CP3.3-2 is the unresolved coverage problem.

## Core v24.0.0-rc.3 message comparison

Verified against tag **da567d7dba945143eee4d2ce1621c21b32089989**, not the checkout HEAD. The reviewed table at `toy/mod.rs:2677–2713` and the actual emitters match the following call paths:

| Call path / first fatal error | Message and Core citation |
|---|---|
| Startup ReplayBlocks, block/undo read | `Unable to replay blocks. You will need to rebuild the database using -reindex-chainstate.` — roll-forward returns false at `src/validation.cpp:5188`; rollback/undo failures return false at `:5270` / `:5275`; outer message `src/node/chainstate.cpp:252` |
| LoadChainstate, chainstate engine read/write/sync, replay included | `Error opening block database` — `src/dbwrapper.cpp:268` throws; `src/init.cpp:2035–2037` catches. This supersedes `Failed to write to coin database` for those modeled startup engine errors |
| Activation ConnectTip, block read | `Failed to read block` — `src/validation.cpp:3412` |
| Activation DisconnectTip, undo read | `Failed to disconnect block; see debug.log for details` — undo failure `src/validation.cpp:2300`, disconnect returns false `:3282`, outer abort `:3621` |
| ConnectTip, coins read | `Error reading from database, shutting down.` — callback `src/init.cpp:2089`; the catcher aborts at `src/coins.cpp:369` |
| BaseIndex::ThreadSync, connect block read | `ThreadSync: Failed to read block <hash> from disk` — `src/index/base.cpp:183` |
| CoinStatsIndex::CustomAppend, connect undo read | `ThreadSync: Failed to write block <hash> to index database` — `src/index/coinstatsindex.cpp:129` returns false; caller `src/index/base.cpp:190` |
| BaseIndex::Rewind, block/undo read | `ThreadSync: Failed to rewind index toyindex to a previous chain tip` — `src/index/coinstatsindex.cpp:287` / `:413`; outer caller `src/index/base.cpp:173` |
| Path-free coins flush / block-file flush / block write / undo write | Respectively `Failed to write to coin database`, the full `Flushing block file to disk failed. This is likely the result of an I/O error.`, `Failed to write block`, `Failed to write undo data` — `src/validation.cpp:3093`; `src/node/blockstorage.cpp:600`, `:869`, `:784` |

The independent targeted `recovery_read_faults_use_each_paths_message` run reaches **all six filesystem-read rows** across S4, S11, S11r and activation. The startup replay and first-engine-read regressions pass too. The EFFECTS/index-store read normalization, the toy-only parent read labelled ConnectTip, and aborting on a failed index commit remain explicitly documented toy abstractions, not exact Core parity. In particular, Core only logs a failed index commit at `src/index/base.cpp:232`; the toy's stricter stop is not a new literal Core abort message. I found no additional mismatch in the changed per-path table. Tag excerpts are archived in `core-v24-excerpts.txt`.

## Validation, retained evidence and limits

- **Independent exact-head tests:** `power_loss_cp31` **10**, `power_loss_cp32` **2**, `power_loss_cp33` **9**, `power_loss_cp33_witness` **18**: **39 passed, zero failures**. `targeted.log` includes the branch counts and counterexample replays.
- **Fresh witness mutations:** an additional older half-range source write, followed by all **18 passing tests** in the copied witness target (`fresh-witness.log`); a changed advisory-allocation offset, **one passing test / two modes** (`fresh-allocation.log`). The original regression suite is unchanged.
- **Additional witness controls:** Deriver plain and sync-first both pass, with the plain branch witnessed by (b) (`deriver.log`; one test / two modes). Both joint enumeration/poisoning unit tests pass (`joint.log`).
- **Message check:** one passing targeted test, all six file-read paths reached (`toy-messages.log`).
- **Subscriber-free evidence:** strengthened the supplied `zz_subfree.rs` to assert both `found_all` and successful replay for each expected invariant. **All 27 b1/b2 planted cases are caught with `sub: None` and replay** (`subfree.log`, `subfree.rs`). The supplied scratch check only printed its Boolean results; this review asserts them.
- **Independent negative probes:** **two positive tests pass and two demanding tests intentionally fail** at the exact head, proving CP3.3-1/2 (`probes-head-extended.log`). The marker assertion passes with the predecessor's harness (`marker-before-head.log`). Realistic sampling runs show the same coverage problem (`probes-head.log`). These are checker counterexamples, not failures of the checked-in suites.
- **Archived exact-head gate:** independently summed `gate-cp33-refuse.log` and checked its status file: **288 passed, zero failed, 20 ignored**, at the exact reviewed SHA. `archived-gate-audit.json` records the audit. I did not rerun all-features/RocksDB tests or the workspace gate; faulty-only builds avoid the approximately 12 GB native target. The reported all-features clippy result is implementer evidence.
- **Archived extended nightly:** `nested-9e45f2c5d/head.txt` names **9e45f2c5d90a8687af4e0c8f9d31101ee5f1ec26**. Its summary records nested seed **20261010**, successful component status and `DONE failed=0 unwritten=0`. All **14 scenario/release rows** have no findings and zero unjudged. This is **not** a run at 500dabd09. The three-component clean claim is supplied evidence; I independently audited this extended component, not every other nested component. No full nightly was rerun. CP3.3-2 explains why its sampled success cannot establish uniform coverage of the remainder.
- **Formatting/diff/lint:** `cargo fmt --all -- --check`, exact-head range diff-check, and `cargo clippy -p dashd-store --no-default-features --features faulty --all-targets -- -D warnings` **pass** (`fmt.log`, `clippy.log`).

Evidence is retained under `/work/scratch/st3a-cp33-sol/`: `review.diff`, `head.diff`, `toy.diff`, probe/mutation/subscriber-free sources, their logs, Core tag excerpts, errata excerpts and audit JSON. To reproduce, copy `probes.rs` to the detached head's `dashd-store/tests/st3a_cp33_sol_probe.rs`, then run `cargo test -p dashd-store --no-default-features --features faulty --test st3a_cp33_sol_probe -- --nocapture --test-threads=1` with the prescribed environment and nice value. The two demanding assertions are expected to fail. The full Skew finding is also asserted to replay. No probe remains in the checked-out source, and the historical harness edit was restored exactly to 500dabd09. The private TMPDIR was removed after validation. The report is left uncommitted as requested.

**Verdict:** CHANGES-REQUIRED

Sol gate satisfied: no
