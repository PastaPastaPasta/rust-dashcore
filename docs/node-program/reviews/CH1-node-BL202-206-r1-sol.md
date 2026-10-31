# CH1-node BL-202/D62a and BL-206 review — Sol round 1

Reviewed `8ecfadf39..9d525fecd` (Part A) and `9d525fecd..cfa2fc0a1` (Part B) in an independent detached worktree. Core references below are from the pinned `dashpay/dash` `0364ed626e` (`v24.0.0-rc.3`), not the later checkout tip.

## Findings

None at Blocker, Major, Minor, or Nit severity in these ranges. No Core deviation was found. Part A: **ACCEPT**. Part B: **ACCEPT**.

## Part A — BL-202 and D62a

- `dashd-chainstate/src/chain/actor.rs:755-799,802-829,922-941` and `chain/flush.rs:91-113`: a plain `Fatal::new` reaches `abort(AbortNode)`, leaves an untripped store open, reports V's failure, then services and stops W. If the store guard already tripped the latch, `abort` reports that fact and V neither duplicates its report nor runs the stop. A V panic, including one in the exit flush stop, reaches `abort(Panic)` before unwinding. This matches Core `shutdown.cpp:27-37` and its two conditional shutdown flushes at `init.cpp:379-385,449-455`; Core's asserts abort instead.
- `dashd-chainstate/src/chain/seams.rs:29-64` separates assert-mirroring `Fatal`s from ordinary `AbortNode`s. The changed sites at `chain/actor.rs:1334-1343`, `chain/locks.rs:415-422`, and `chain/failure.rs:268-273` take the panic class. Their Core counterparts are `validation.cpp:5609` onward (CheckBlockIndex's assertions), `validation.cpp:4064-4067` (failed MarkConflictingBlock), and `instantsend/net_instantsend.cpp:620-623` (failed InvalidateBlock). C1/M1 are V's own checked invariants. The other Fatal conversions in the inspected V paths remain AbortNode; the store's I/O failures are latched at the guard, not inferred from the error text.
- The Part A tests cover non-store AbortNode, prelatched store failure, normal panic, panic during either exit stop, CheckBlockIndex/C1/M1, and both additional Core assert ports. The production CH2b `FlushSite` adapter is not in these ranges; the tests use the branch's model W seam.

## Part B — BL-206

- `dash-chain/src/chain.rs:2046-2052,2137-2153`, `status_journal.rs:117-145`, and `tip.rs:173-183`: the one writer records the previous status before its seqlocked arena write. `mark` runs at snapshot construction, on that writer, with the new snapshot's epoch and moves the next write's tag to `epoch + 1`. Writes before the first build have tag 0 and are correctly included in epoch 0; writes after a build have a higher tag and are hidden from that snapshot. There is no concurrent status writer or status write between this snapshot's build and its store.
- `dash-chain/src/reader.rs:166-175` reads the arena before walking the journal. `status_journal.rs:41-59` reads `next` before locking/scanning each log and stops after a journal that was still open at that first read. Thus a write the live read saw has its earlier journal entry visible, while a write after the live read cannot cause the walk to skip the first relevant preimage. The mutex, `OnceLock`, seqlock's acquire/release sequence (`entry.rs:85-117`), and `ArcSwap` publication supply the cross-thread ordering; the relaxed epoch atomic is accessed by the single writer for protocol state. The 33rd status write links a new journal before appending to it; each journal holds at most 32 entries.
- `dashd-chainstate/src/chain/view.rs:142-156`: `status` and `valid_scripts` use the held view's snapshot. `entry()` deliberately remains a *live* seqlocked entry: its status can be newer than a held `ChainView`, as the test at `chain/tests/view.rs:58-87` demonstrates. It must not be substituted for the view-bound APIs when P3's production ChainAccess is wired. That live behavior predates this range and is the documented D63 contract, so it is not a finding here.
- Reorg and invalidation status changes, including lowers and flag clearing, pass through `HeaderChain::modify`: `dashd-chainstate/src/chain/failure.rs:63-70,98-100`, `dash-chain/src/chain/enforce.rs:343-358`, and `chain.rs:1492,2165-2172`. The journal compares complete status values, so lowering is captured as well as raising. This corresponds to Core's failed-bit swap at `validation.cpp:3979-3987` and flag clearing at `validation.cpp:4177-4199`.
- `status_journal.rs:76-84,128-145` releases old journal chains when the last snapshot referencing their start drops, with iterative destruction. A promptly dropped view does not pin unbounded history across publications; a held old view retains every later journal, the BL-224 follow-up. No change here increases DEC-261's waived PF0 status-publish-coalesce residual.

## Independent mutations

Two temporary probes passed on the original tree: `sol_probe_lowered_status_keeps_prior_snapshot` (lower Scripts to Tree after a publication) and `sol_probe_lowering_across_handoff` (lower, publish, then raise across a one-entry test journal). I restored the original source after the experiment. Each of these **five new mutations** produced an assertion failure rather than a compile failure:

| Mutation | Test that failed |
| --- | --- |
| Journal only numeric status raises (`chain.rs:2147`) | Lowering probe |
| Record the new status instead of the preimage (`chain.rs:2148`) | Lowering probe |
| Hand off one write past journal capacity (`status_journal.rs:132`) | Handoff probe |
| Tag a write with the snapshot after next (`status_journal.rs:138`) | Both probes |
| Move journals without linking `next` (`status_journal.rs:134`) | Handoff probe |

## Validation

- `cargo test -p dash-chain --lib status_journal`: 3 passed. `cargo test -p dash-chain --lib a_status_write`: 2 passed.
- `cargo test -p dashd-chainstate --lib a_reader_sees_the_status_of_the_view_it_holds`: 1 passed. `cargo test -p dashd-chainstate --lib chain::tests::flush::`: 43 passed. The two lock/InstantSend assert-port tests passed individually from the same built test binary.
- Scratch probes: 2 passed at baseline and after all mutations were restored; five of five mutants failed the intended assertions. The detached source worktree was clean after restoration.
- I used the owner's stated GATE-FAST/PF0/mutant evidence for the broader gate and did not rerun perfgate.

VERDICT: ACCEPT
