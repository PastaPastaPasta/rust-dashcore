<!-- Copy of /work/scratch/mgr/review-bl546-r1-sol.md for reviews/ (DEC-984), 2026-10-31. One passage reworded under DEC-939. -->

# BL-546 round 1 correctness review

**Verdict: ACCEPT**

**Delta findings: M = 0, m = 0, n = 0.** No correctness defect introduced or worsened by the reviewed delta was found. There are consequently no numbered findings or required fixes.

Reviewed `node/ch3-bl546`, exact head `6f86fced59f4dd71e2b23b6b6a9b99cb9b1d5a61`, against `ae6d9b0a4`: all four commits and all 14 changed files. Read DEC-937, DEC-939, BL-546, and BL-554. The frozen worktree and `/work/scratch/ch3-bl546` were not modified. No commits, pushes, PRs, or GitHub posts were made.

## Text and report ownership

The changed funnel is correct for its two selected sites. `GuardedEngine::mutate` latches the exact returned engine error's display text, after converting an engine-origin `ReadOnly` to `Io`. `Halt::system` reports that same error with the shared `SYSTEM_ERROR` prefix. `site.rs::reported` now reconstructs precisely that report from the immutable first failure, independently of which wait observes it. The latch retains its raw engine text.

| Failure | Result of the changed mapping | Reporting |
| --- | --- | --- |
| Chainstate write | `System error while flushing: <latched engine text>` | The winning store call's caller reports; V's returned Fatal does not report again. |
| Chainstate barrier, including WAL sync and memtable flush | The same single prefix | Same ownership as a write. |
| Flush's own failure, including failed space query and low disk | Existing latched report text passes through unchanged | No extra or doubled prefix; low disk keeps its explicit user message through BL-482's saved report. |
| Block-file failure, validation failure, read failure, other store's failure, or simulated crash | Existing latched text passes through unchanged | The mapping adds neither an AbortNode call nor a user error. |
| Fresh evo snapshot-write failure | One prefixed report from `write_failed`, preceded by the engine line and debug hint | The helper returns `Fatal::reported`; the normal V stop adds no report. |
| Snapshot write refused by an existing latch | Text of the original failure, mapped by its original site | No engine lines, AbortNode, or user error are added by the refused write. |

Compared against Core `v24.0.0-rc.5`: `validation.cpp`'s `FlushStateToDisk` catch, `AbortNode(state, ...)`, `ForceFlushStateToDisk`, `ConnectTip`, `DisconnectTip`, `ActivateBestChainStep`, ABC's periodic flush, and `ProcessNewBlock`; `dbwrapper.cpp::HandleError`/`WriteBatch`; `shutdown.cpp::AbortNode`; `chainlock/handler.cpp::EnforceBestChainLock`; `node/blockstorage.cpp::ThreadImport`; and init's loadblk and shutdown-flush paths. The flush prefix, debug hint, shutdown failure line, default user message, and ChainLock/ProcessNewBlock caller strings agree on the scoped paths. One AbortSink call invokes one `***` line and one user error in `NodeAbort`.

The snapshot prefix follows the DEC-939 ruling: a failed evo snapshot write stops the node with one report (EV3 D21: store faults are not consensus rejects). The rest is the §5 trace, see manager.

## Fatal propagation and shutdown assertion

`Fatal::reported` is `Report::Done, Class::AbortNode`: it changes report ownership, not the failure class or stopping behavior. Snapshot application still returns before recording a presumed effect. The sole non-test caller of `store_quorum_snapshot` is `StoreSnapshots::write`; returning the guard's error preserves the `ReadOnly` refusal discriminator. Effect application and connector completion retain the Fatal channel, rather than turning it into a consensus rejection. Recovery's direct database writers and their init-error mappings do not pass through this newly changed applier or funnel.

Confirmed the current reachability limit: connect member calls at `evo/node.rs:606` and `:648`, and undo precompute/member calls at `:1169`, use `NoMembers`. That implementation never emits a snapshot effect. Its pre-DIP3 undo wrapper can swallow the readiness refusal, but still emits no effect. Thus there is no newly reachable undo snapshot write whose report should already have been changed to `Failed to disconnect block; see debug.log for details`. D221 correctly leaves that requirement to CH7b-2.

The `steps.rs:681` assertion is sound on the current shutdown paths. The report index is captured before the flush, excluding earlier reports. `VExit::force_flush` joins W before returning a latched failure. Store failures therefore compare the same raw engine text with the same shared prefix; non-store failures compare the already formatted latched report. Shutdown has stopped V and drained its prior signals. A failed flush queues no new `ChainStateFlushed`. BL-482's second report is in the separate latched-repeat branch, after the saved first report has been retained; a previously latched store skips the asserted branch entirely. The first/second write-failure and low-disk node tests passed with debug assertions enabled.

BL-554's report/reply ordering race is excluded as instructed; this delta does not repair it or make it worse. Report-count checks after joining W do not establish ordering before that join.

## Tests and mutation checks

Before building, the required standalone resource check reported **248G available**, five-minute load **25.75**. Reflink-copied the frozen tree to `/work/scratch/bl546-r1-sol`, touched the test source files relied on, and used **CARGO_BUILD_JOBS=8**, **nice 19**, and TMPDIR inside that copy. Every long command was awaited in this turn; no watcher or unattended job was left running.

Built and ran the unmodified head:

- `cargo test -p dashd-chainstate --lib --features alloc-tag,crash,planted-bugs,statedigest,test-support -- chain::tests::node_stores:: flush::machine::tests:: chain::tests::node_evo:: evo::effects_tests:: store::guarded::tests::` — **91 passed**.
- `cargo test -p dashd-node --lib --features planted-bugs -- sut:: abort::tests::` — **47 passed**.
- `git diff --check ae6d9b0a4 6f86fced5` — clean.

T1 directly checks the mapping. T2 reaches production `VExit`/`NodeFlush`/W with a write and a live-copy barrier failure. T3 reaches init's ABC reply. T4 reaches the production ProcessNewBlock tail. T5 reaches deferred ChainLock enforcement and its ABC. The B1 assertion compares the real failing machine's report with the guard's real latched failure. The snapshot test reaches the actual `StoreSnapshots` and guard/helper, but does not run a member computation or a complete node shutdown; its report-ownership assertion exercises the predicate V's stop uses.

Independently re-executed the supplied instrumented test binaries, from the review copy with all new output and temporary files kept there. First compared their four instrumented source files with the frozen head. The differences were the documented environment gate and mutants. Results:

| Mutant | Independently observed kill |
| --- | --- |
| 1: omit prefix everywhere | Seven chainstate tests; both node shutdown write-failure tests also fail at the new assertion. |
| 2: prefix every site | Mapping, own-flush failure, W-panic tests; supplied logs also show the low-disk node kill. |
| 3: omit barrier mapping | Mapping and live-copy barrier case. |
| 4: omit write mapping | Seven chainstate tests. |
| 5: unprefix W's report | B1, every-store-call, and the four node_stores paths. |
| 6: discard BL-482's saved report | Low-disk node shutdown test. |
| 7: restore unreported snapshot Fatal | Snapshot failure test. |

The supplied baseline passed **75 chainstate tests** and the **three selected node tests** in the independent mutant run. All seven mutants were killed. I inspected the supplied full mutant summary as well, rather than treating its claims as the only evidence.

Added an ignored, review-only matrix in the copy: fresh `Io`, `Corruption`, `NoSpace`, engine-origin `ReadOnly`, and `Other`; repeated refusal; and prior latches at flush, barrier, read, and another store. It passed, checking the Fatal class, exact engine/hint/`***` sequence, one report with `user=None`, unchanged presumed-effect ledger on failure, and no extra logs or reports after refusal.

**Cheap surviving mutant:** changing only `write_failed`'s `abort_node(&text, None)` to `abort_node(&text, Some(&text))` survives the existing selected suite (**91 passed**). It substitutes engine details for Core's default user error. The review matrix kills it by checking `RecordingAbort::reports()`'s user field. This is a coverage limitation, not a defect in the current implementation, which correctly passes `None`; it is not counted as a correctness finding.

## Separate pre-existing boundary observed, not a delta finding

A review-only reorg probe also confirmed a broader Core-equivalence limit at **`dashd-chainstate/src/chain/abc.rs:605`**, with the flush propagation at **`:760` / `:768`**. While importing, build two equal-work branches, keep the first active, clear importing, arm the next chainstate write failure, and make the side tip precious. A flush during ABC's disconnect returns Fatal through `?`; the caller consequently skips its `Ok(false)` arm and its final `Failed to disconnect block; see debug.log for details` AbortNode. The probe observed one flush AbortNode and the flush text as the final Fatal. Core's `DisconnectTip` returns false for a failed flush, so ABC adds the disconnect AbortNode and uses its text.

This is distinct from the deferred *snapshot* undo case and from BL-554. The same `?` propagation and skipped second-report arm are present unchanged in `ae6d9b0a4`; BL-546 merely makes that pre-existing returned Fatal match the first flush report. I have therefore not charged it to this delta or blocked the scoped fix on it. A separate fix should preserve/report the inner flush failure, then apply Core's outer disconnect failure handling for an in-ABC disconnect flush failure, without treating every unrelated Fatal/assert as Core's false. The probe and its output are retained for a follow-up if desired.

## Artifacts and limits

All fresh logs are under `/work/scratch/bl546-r1-sol/review-logs/`: `build-chainstate.log`, `targeted-chainstate.log`, `build-node.log`, `targeted-node.log`, `supplied-mutants-summary.log`, individual supplied-mutant logs, `review-matrix-summary.log`, `matrix-baseline.log`, `user-mutant-original-suite.log`, `user-mutant-review-matrix.log`, and `abc-disconnect-probe.log`.

`review-experiments.patch` preserves the review-only tests and mutant. The three experimentally edited files in the copy were restored byte-for-byte to the frozen source afterward; the copy is left in place as requested. Its last chainstate test binary contains the experiments and will be rebuilt by cargo because the restored source files have new mtimes. The initial baseline logs and node binary are from unmodified sources.

No full-workspace run or live dashd failure-injection comparison was claimed. The evidence is source comparison with the named Core tag, local production-path fixtures, an independent mutant rerun, and the additional copy-only probes.
