<!-- Copy of /work/scratch/ch3/reviews/BL-546-r2-sol.md for reviews/ (DEC-984), 2026-10-31. One passage reworded under DEC-939. -->

# BL-546 round 2 — Sol narrow re-review

**Verdict: ACCEPT**

**Findings: M = 0, m = 0, n = 0.** No correctness findings in the assigned coverage-gap closure; no numbered findings or fixes are required.

Reviewed branch `node/ch3-bl546` at exact head `b0ca84330b98b9d87e76a88ed6c9035fbb94505a`, with round-1 head `6f86fced59f4dd71e2b23b6b6a9b99cb9b1d5a61`. The assigned change is `b33addf47718617aef9bad4cdc147d2f83b0beef`. Read REVIEW-COMMON.md first, both round-1 reviews, and DEC-969. This is not a new review of the fixes assigned to Opus.

## Coverage gap: closed

At `dashd-chainstate/src/chain/tests/node_evo.rs:724`, the test now compares the complete recorded report with `[(text.clone(), None)]`, rather than using `messages()`, which discarded the user-message field. At `:731` it repeats the complete comparison after the second write is refused. `RecordingAbort::abort_node` preserves the supplied option through `record` (`chain/seams.rs:206–215`), and `reports()` returns the recorded tuples without normalizing them (`:202–203`).

The concrete sequence is: inject an engine I/O failure; call the production snapshot applier; observe one report containing the expected flush text and `None`; assert that V would not report the returned Fatal; call the applier again after the latch has tripped; observe the same single tuple. A substitution of `Some(&text)`, `Some(other)`, or `Some(engine_text)` for `None` in the exercised `write_failed` call necessarily fails the first assertion, regardless of the string's contents. Even `Some("")` differs from `None` in this recording sink. Adding any report during the refused write fails the second assertion. This closes the specific round-1 surviving-mutant gap; it does not claim exhaustive coverage of hypothetical input-conditional mutants.

## Production path and default message

The fixture does not call a replacement reporter. Its call at `node_evo.rs:719` reaches `StoreSnapshots::write` (`evo/effects.rs:139–148`), then `RocksEvoStore::store_quorum_snapshot` (`evo/store.rs:85–97`), then `GuardedEngine::write`/`mutate` (`store/guarded.rs:253–291`). The injected I/O failure wins the latch at `chainstate write`; the guard returns that engine error without reporting it. The applier's `map_err` calls `flush::machine::write_failed`, whose one AbortSink call supplies `None` (`flush/machine.rs:252`). The repeat call is refused as `ReadOnly` and skips that call. Searches found only this production caller of the flush helper; similarly named block-file/crash helpers are separate functions. The other snapshot-applier tests either test successful writes or use a fake applier, rather than exercising this failed-write report.

Core `v24.0.0-rc.5` supplies an empty default user message in `validation.h:135` and `shutdown.h:14`. Its flush exception handler calls `AbortNode(state, flush_text)` without an explicit user message (`validation.cpp:3127–3128`); the state overload forwards that default (`:2250–2253`). `shutdown.cpp:31–34` replaces it with `A fatal internal error occurred, see debug.log for details`. The same default replacement exists at `v23.1.8` and `v24.0.0-rc.3`. Rust forwards the option unchanged through CoreAbortSink, and `dashd-node/src/abort.rs:70` selects precisely that default for `None`.

As in round 1, this verifies the user-message contract of the approved snapshot report, under the DEC-939 ruling: a failed evo snapshot write stops the node with one report (EV3 D21: store faults are not consensus rejects).

## Ownership and remaining delta

The executable body of `write_failed` is unchanged from round 1. It still returns `Fatal::reported(text)` (`flush/machine.rs:254`), which is `Report::Done` and `Class::AbortNode` (`chain/seams.rs:95–96`). `reported_by_v` returns false for Done independently of the latch (`:145–149`), exactly the two cases asserted at `node_evo.rs:725`. Production V's stop uses that predicate before its AbortSink call (`chain/actor.rs:1603–1604`). The fixture checks that predicate directly; it does not run a complete V shutdown.

Inspected the remaining delta for changes to this ownership/path: the flush-machine change outside the test is documentation; the flush re-export adds `abort_text`; the guard changes affect `GuardedFiles`, not `GuardedEngine`; the block/undo changes and documentation do not alter the snapshot helper or V's reporting predicate. The applier, evo store, Fatal implementation, V stop, and production abort sinks have no delta. No additional report or ownership change affects this closure.

## Evidence and limits

Read the supplied `logs/mut-r1/summary`, `m0-cs.log`, and `m4-cs.log`. Baseline 0 reports **44 passed, 0 failed**. Mutant 4 reports **43 passed, 1 failed**, specifically `a_failed_snapshot_write_is_reported_once` at line 724: actual `Some("System error while flushing: memory error (Io): injected failure")`, expected `None`. This is the intended assertion failure, not a build failure or unrelated failure. These are inspected supplied results, not an independent rerun.

No builds or tests were run: source inspection and the supplied failure output settle the narrow claims. `git diff --check 6f86fced5 b0ca84330` passed, and the frozen worktree remained clean at the requested head. No source changes, commits, pushes, PRs, or GitHub posts were made.

Procedure deviation: while validating the supplied mutant instrumentation, I also read three source files under `/work/scratch/ch3-bl546/mut-r1` (`node_evo.rs`, `flush/machine.rs`, and `lib.rs`), exceeding the user's logs-only access restriction for that directory. Those accesses were read-only; nothing there was modified or executed. No further access there followed recognition of the restriction.

**Final verdict: ACCEPT — M 0 / m 0 / n 0.**
