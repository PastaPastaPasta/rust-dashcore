<!-- Copy of /work/scratch/mgr/review-bl546-r1-opus.md for reviews/ (DEC-984), 2026-10-31. One passage reworded under DEC-939. -->

# BL-546 r1 review (Opus): node/ch3-bl546 @ 6f86fced5 (base ae6d9b0a4)

**Verdict: CHANGES-REQUIRED.** One major finding (M-1), one minor (m-1), two nits (n-1, n-2).

The funnel is right wherever V reads the latch through `site.rs reported()`. The shutdown-flush `debug_assert` holds. `write_failed` gives exactly one report and keeps the AbortNode class.

The delta claims every chainstate write failure reaches ABC's Err with Core's text. That doesn't hold in production when W fails asynchronously while V is inside a block's ConnectBlock. V's refused undo write then carries its own text, not the funnel's. A probe confirms it (below), and a three-line fix makes the probe pass.

## Findings

### M-1: a W write failure that lands during V's ConnectBlock reaches ABC's Err as "undo write: …", not Core's text

- **Where:** `dashd-chainstate/src/connect/serial.rs:1212` (`undo_failed`, the `FlatFileError::Failed` arm), reached from `:1171-1173`.
- **Sequence (production node, async W; the node has no synchronous policy):**
  1. ABC is connecting block N+1.
  2. `at_site` runs `service()`, whose `latch().check()` (`flush/control.rs:767-771`) passes, because W is still writing block N's flush.
  3. V enters ConnectBlock. During IBD this is the bulk of V's per-block time.
  4. W's guarded write fails. `GuardedEngine::mutate` trips the latch at `chainstate write`, and W reports `*** System error while flushing: <e>`.
  5. V reaches `write_undo`. `GuardedFiles::mutate` sees the tripped latch and returns `FlatFileError::Failed`.
  6. `undo_failed` returns `Fatal::reported("undo write: block files refuse writes after an earlier I/O failure")`.
  7. `abort_at_fault` keeps that text (`chain/actor.rs:1617-1625`). So `ProcessNewBlock: ActivateBestChain failed (…)` (`chain/accept.rs:195`), EnforceBestChainLock's line (`chain/locks.rs:342`), init's ABC reply, and D216's second abort (`Failed to connect best block (…)`) all name the refusal. Core names `System error while flushing: <e>` (`validation.cpp:3122-3123`, `:2245-2249`).
- Nothing between `at_site` and the undo write reads the latch. I grepped `chain/abc.rs`, `connect/serial.rs` and `chain/actor.rs`.
- So the text still depends on which reader sees the latch first. D221 says "the text no longer depends on which of V's waits reads the latch first", but V's refused mutations are readers outside the funnel.
- This is not BL-554 (BL-554 is report order; this is the text). It predates the delta, but it is BL-546's stated goal ("every reply and log names Core's text") and the implementer's claim 1.
- **Probe (review copy only, env-gated):**
  - In `/work/scratch/bl546-r1-opus/wt`, `write_undo` trips the latch at `chainstate write` and reports `System error while flushing: memory error (Io): late W write` just before the undo mutate. This stands in for W's thread inside the window.
  - Test `r1_probe_late_w_failure_text` (`node_stores.rs`, appended in the copy) runs a default async rig and one `process_block`. The sink holds the one report (that assert passes).
  - V's line is `ProcessNewBlock: ActivateBestChain failed (undo write: block files refuse writes after an earlier I/O failure)` (assert fails, output kept).
- **Fix:** in `undo_failed`'s `Failed` arm, take the latched failure's text: `Fatal::reported(abort_text(self.abort.latch().tripped().expect(..).clone()))` (`AbortPath::latch()`, `chain/seams.rs:249`). Export `abort_text` alongside `write_failed`.
  - I applied exactly that in the copy behind `BL546FIX`. The probe then passes, and all 504 `chain::`/`connect::` lib tests pass with it on.
  - Add a test like the probe that doesn't depend on timing, plus a mutant (the arm reverted).
  - While there, sweep the other `FlatFileError::Failed` consumers. `store/blocks.rs:62` turns a refused block save into AcceptBlock's "Failed to find position…" state. It's not a text-funnel case, but it's the same window; file a BL if it's out of scope.

### m-1: the funnel's "every other site latched its report's own text" is false for block-file I/O failures

- **Where:** the `abort_text` doc (`flush/machine.rs:224-228`) and D221's "Every other site latched its own report's text (…, the block files', …), which passes unchanged".
- `GuardedFiles::mutate` (`store/guarded.rs:531-536`) latches `e.to_string()`. For `FlatFileError::Io` that is `"{context}: {path}: {source}"` (`dashd-store/src/flatfile/error.rs:11`). But `NodeBlockFiles::save_failed` (`store/blocks.rs:58-100`) reports something else:
  - `WRITE_BLOCK_FAILED` alone (`:105-108`);
  - the bare `context` (`:92-95`);
  - `System error: {what}` (`:85-87`).
- **Sequence:**
  1. V's `SaveBlockToDisk` fails opening the block file. `*** Failed to write block` is reported, and the latch holds `Failed to write block: <path>: <io error>`.
  2. V's next latch read goes through `reported()` (AcceptBlock's NONE flush, or the next `at_site`), so V's Fatal text is `Failed to write block: <path>: <io error>`.
  3. ProcessNewBlock's and ABC's lines then disagree with the one report.
- W's own block-file steps (`Halt::files_at`, which reports `e.to_string()`) do match.
- This is the same class of bug as BL-546 (latched text ≠ reported text) at a site the delta declares fixed by construction. The delta doesn't make it worse.
- **Fix:** correct the doc and D221 and file a BL; or have the block-files reporters latch, and name, the text they report.

### n-1: if the connector already holds a Fatal and the log then fails to apply, V carries the first Fatal's text while the sink holds the snapshot's

- **Where:** `connect/connector.rs:127-139` (`finished_outcome`) and the disconnect twin `:413-420`. Latent: nothing emits a snapshot at this head (`NoMembers`).
- **Sequence:** the connect returns `Err(f_out)`, a `Fatal::new` that V would report (for example `coins: <logic_error>`), with the latch still open. `finish_attempt` then applies an earlier-emitted `StoreQuorumSnapshot` whose write fails. `write_failed` reports the snapshot text and trips the latch. `finished_outcome` keeps `f_out`, and V's stop suppresses it (UnlessLatched + latched). The result is one report, but V's state text is `f_out`'s, not the reported one.
- D75's "keep the outcome over a failed apply" predates applies that report.
- **Fix (with CH7b-2, or now):** when `finished` is `Err` and `out` is an unreported `Err`, prefer the apply's `Fatal` (it is the one that reported). Or record this in D221 next to the CH7b-2 undo note.

### n-2: the crash harness's subscriber store now gets the flush prefix in V's Fatal text

- **Where:** `crash/subs.rs:309-323`, `:630-645`.
- The subscriber's `GuardedEngine` shares the node latch, and every `GuardedEngine` latches at `WRITE_SITE`. So a failed subscriber write reported as `toyindex: <e>` now reaches V's `reported()` as `System error while flushing: <e>` (before: `<e>`).
- Test harness only: there are no subscriber stores in dashd-node/rdashd, and the node has a single production `GuardedEngine`, `dashd-node/src/chainstate.rs:319`.
- It contradicts "no other site gets the prefix" only in the harness.
- **Fix:** none needed for the product. Optionally give the harness's subscriber guard its own site, or note it in `crash/stores.rs:80`'s row.

## Claims checked

1. **Text and count against Core.**
   - Write and barrier inside W:
     - `Halt::system` reports `SYSTEM_ERROR + e`, where `e` is the guard's (ReadOnly→Io remapped) error. `mutate` latches `e.to_string()` of the same value (`store/guarded.rs:262-293`). So `abort_text` reproduces W's report exactly.
     - The machine's second trip at `chainstate flush` (`flush/machine.rs:518-522`) fails because the latch is already held, so the guard's site survives.
     - `after_bk` and `write_barrier` go through `BARRIER_SITE` and `WRITE_SITE`.
     - One `***`, one user error, and HandleError's pair.
   - Sites that must stay unprefixed, all latching their reporter's text:
     - space check and free-space read (`chainstate flush`, `halt.abort.log`);
     - V's AbortNode at a flush site (`control.rs:1034-1043`);
     - W's panic (`driver.rs:349-356`);
     - AbortPath (`seams.rs:256-262`);
     - `dbcrashratio`.
   - Reads abort the process in production (`CoreAbortSink::read_failed`).
   - The block files' exception is m-1, and V's refused undo write is M-1.
   - Production engines: one chainstate `GuardedEngine`. The only V-side chainstate write at runtime is `store_quorum_snapshot`. The meta wipe, recovery and replay writes run at init, before V.
   - Second shutdown flush after a W write failure: `latched_by_flush` is false (wrong site). `force_flush`'s refused sync returns the latched engine text and reports `System error while flushing: <e>` again, as LevelDB does. Unchanged.
2. **Fatal plumbing.**
   - `Fatal::reported` is `Report::Done` plus `Class::AbortNode` (`chain/seams.rs:51-91`), so `abort_at_fault`, `stop_serving` and `carry_past` behave as for every other reported AbortNode.
   - `store_quorum_snapshot`'s only production caller is `StoreSnapshots::write`; `evo/tests.rs` unwraps. The control flow in `connect_block`, `disconnect_block` and `test_block_validity` is unchanged: the same `Err` arms, now carrying `Done` and the funnel text.
   - Replay doesn't use appliers.
   - `expect("a failed write has latched")` is sound: both error kinds from `mutate` imply a tripped latch.
3. **The steps.rs debug_assert.**
   - It's only reached when the latch was open at the first shutdown flush's start. `force_flush` joins W, so W's report has landed.
   - Every W, V or flush-site failure latches the text it reports (point 1). Net, RPC, the scheduler and V are stopped, so no other reporter runs.
   - BL-482's repeat and later flushes never reach it (the latch is tripped).
   - The 15 dashd-node `flush` tests pass with it active, including `an_abort_node_while_verifying_flushes_twice_on_vs_state` and the low-disk BL-482 test.
   - I found no sequence that fires it.
4. **Snapshot writes.**
   - `write_failed` gives the HandleError pair via `tracing::info` (unconditional INFO, so it reaches debug.log), one `abort_node`, and no V report. A refused write logs and reports nothing.
   - The emitters are refused by `NoMembers` (`evo/node.rs:606`, `:648`, `:1169`), which matches D221's CH7b-2 note.
   - The snapshot path follows the DEC-939 ruling: a failed evo snapshot write stops the node with one report (EV3 D21: store faults are not consensus rejects). The rest is the §5 trace, see manager.
5. **Tests and mutants.**
   - T1–T5 and the B1 assert run the production `reported()` path: through `exit.force_flush` (T1), init's ABC (T3), and ProcessNewBlock and EnforceBestChainLock with the synchronous policy (T4, T5).
   - The snapshot test calls the production `StoreSnapshots` directly, not through V. That's acceptable at this head.
   - Cheap mutants I checked by reading the tests (I didn't run them):
     - `write_failed` reporting on ReadOnly too;
     - dropping either HandleError line;
     - `Fatal::new` for `Fatal::reported`;
     - `format!("{SYSTEM_ERROR}{e}")` for `abort_text(latched)` (killed by the refused-write assert).
   - The steps.rs log of `r.log` against `f.0` is equivalent by design and survives, as expected.
   - Not covered: M-1's arm (no test reaches a refused undo write after a W failure).

## What I ran

- Copy: `/work/scratch/bl546-r1-opus/wt` (reflink of the frozen worktree, head 6f86fced5; test files touched). `CARGO_BUILD_JOBS=8`, nice 19, TMPDIR inside the copy. Before building: /work 242G free, load 20.
- `cargo test -p dashd-chainstate --features alloc-tag,crash,planted-bugs,statedigest,test-support --lib -- reported_text abort_text a_failed_b1 a_failed_snapshot_write`: 8/8 pass at the head.
- `cargo test -p dashd-node --features test-support --lib -- flush`: 15/15 pass (debug build, so the `debug_assert` is live).
- M-1 probe (env-gated edits in the copy only: `connect/serial.rs` `write_undo` and `undo_failed`, `flush/mod.rs` re-export, a `node_stores.rs` test):
  - `BL546R1=1 … -- r1_probe_late_w_failure_text`: fails with V's line naming the undo-write refusal; one sink report.
  - `BL546R1=1 BL546FIX=1`: passes.
  - `BL546FIX=1 … -- chain:: connect::`: 504/504 pass.
- I didn't touch the worktree or `/work/scratch/ch3-bl546`. The copy is left in place.
