<!-- Copy of /work/scratch/ch3/reviews/F01-plan-r1-sol.md for reviews/ (DEC-984), 2026-10-31. One sentence reworded for the commit gate. -->

# F01 plan review, round 1 (Sol)

Verdict: **REWORK**

Finding counts: 1 major, 1 minor, 2 nits.

## Findings

### M-1 (major): the planned Core/rdashd log comparison cannot pass

**References:** `/work/scratch/ch3-f01/PLAN.md:320-337, 346-364`; `/work/home/agent/workspace/rdc-wt/integration/dashd-chainstate/src/chain/abc.rs:513-530`.

The plan requires rows 2, 3, 5, and 6 of `InvalidChainFound` to contain Core's `log2_work` and `date` fields, says those values are taken from dashd, and then requires the filtered dashd and rdashd sequences to be equal line-for-line. The current integration implementation deliberately emits only `hash` and `height` for both lines (`abc.rs:522-524`) and documents that the work/date fields are omitted. Stock Core v24 emits those fields (`validation.cpp:2053-2060`).

The failure sequence is deterministic: the reference run produces, for example, `InvalidChainFound: invalid block=<B>  height=102  log2_work=...  date=...`; after restart, rdashd produces `InvalidChainFound: invalid block=<B>  height=102`. The prefix filter retains both, and the equality assertion at §3.4 fails even when the restart, status, and consensus behavior are correct. The same mismatch occurs for the current-best line and both repeated lines.

Fix the oracle contract before implementation. Either compare normalized shared fields (hash/height and sequence) while asserting each implementation's own exact log format separately, or explicitly add a logging change to S1 that emits Core's work/date fields and update the existing log tests and D-row. The current plan has neither change, so its required test is unsatisfiable.

### m-1 (minor): deferred-tail coverage is optional despite an unconditional hook claim

**References:** `/work/scratch/ch3-f01/PLAN.md:82-99, 409-417`.

The plan claims that placing the hook at the top of `process_block_tail` covers immediate and deferred tails, but §4.2 permits dropping `a_deferred_tail_runs_the_hook_too` if the rig cannot reach a release point. The process test submits one block and therefore can pass while a regression in `run_deferred` bypasses the hook entirely. That leaves the stated API guarantee untested.

Make the deferred-tail unit test mandatory (or add a deterministic harness that reaches a release point), or narrow the hook/design claim to the path actually covered and file the deferred behavior as a separate follow-up.

### n-1 (nit): the D-row number contradicts the owner's ruling

**References:** `/work/scratch/ch3-f01/PLAN.md:475-481, 536-550`.

The draft S3 row is written as D222, while the owner ruling says BL-554 already takes D222 and F01 must use D223 (or the next free row at cut time). Copying §7 verbatim would create a duplicate design row. Change the draft row and S3 wording to D223/next-free-row.

### n-2 (nit): tracked wording listed in the plan violates the owner's commit-gate wording rule

**References:** `/work/scratch/ch3-f01/PLAN.md:312, 402, 449, 481, 556-557`.

The owner ruling requires tracked code/comments/design text to avoid two words the program's commit gate blocks, but the draft D-row and the test description still use them. Reword the copied D-row and implementation/test text before S3/S2 land (for example, describe the blocks ABC will try and say that datadirs have mode 0700).

## What I checked

- Read the review contract and the complete F01 plan, including §11 owner rulings.
- Read CH3-chainstate §4.6, §7.7, §8.2 b4, D39, and the CH3b notes.
- Read the integration worktree's `chain/accept.rs`, `chain/actor.rs`, `chain/abc.rs`, `chain/load.rs`, `chain/flush.rs`, `flush/site.rs`, `flush/control.rs`, serial connect path, and the node/RPC chainstate wiring. The proposed hook placement and the described V/W wait graph are consistent with those paths; `AcceptBlock`'s `FlushStateMode::None` does not invalidate the pre-hook durability argument.
- Read Core v24.0.0-rc.4 `validation.cpp`, `src/node/blockstorage.cpp`, `src/script/interpreter.cpp`, and `src/checkqueue.h` for the fixture, script failure, startup ABC, invalid-chain logging, and stop-after-import behavior.
- No builds or tests were run, per the brief's read-only plan-review requirement.
