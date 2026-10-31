ACCEPT

## Findings

None.

### M1 / D38: accepted

I independently checked Core v24.0.0-rc.4. Core does not write the EvoDB best-block marker in the genesis special case (src/validation.cpp:2540-2545); its first DIP3-active child checks the previous hash with CEvoDB::VerifyBestBlock (src/validation.cpp:2527-2535, src/evo/evodb.cpp:120-126). A disconnect writes the parent marker (src/validation.cpp:2370-2372), so a marker naming genesis can exist transiently after disconnecting block 1. However, under a fixed, valid deployment configuration, a DIP3-active block 1 could not have been connected from a missing marker in the first place; the normal paths that produce a genesis marker do not subsequently require a DIP3-active block-1 connection. The implementation’s normalization in view.rs:258-273 therefore matches the reachable Core behavior, and the test covers active, frozen, and durable levels while preventing fallback to a lower level.

### m2 / D39: accepted

The guard in view.rs:230-247 rejects a base height above the row’s mining height before the ancestry test. Core’s equivalent readers call pindex->GetAncestor(quorumHeight) and assert the result (src/llmq/blockprocessor.cpp:721-723 and src/llmq/blockprocessor.cpp:764-765). Valid Core writes derive the quorum base from the known quorum block (src/llmq/blockprocessor.cpp:338-340, :400-405) and write that height at the mining row (:437-440), so the stricter corruption refusal cannot reject a valid row. The codec independently enforces the same invariant for newly written rows (codec.rs:289-299).

### n3: accepted

The a_genesis_evo_tip_reads_as_absent test (tests.rs:462-481) exercises the durable metadata, active index, and frozen index cases. The durable block-1 marker beneath the active/frozen genesis tip also means an implementation that falls through after seeing genesis would fail the assertions.

## Regression review

No regressions found in the fix changes to evo/view.rs or evo/codec.rs. The three-level range merge preserves upper-level tombstone hiding (view.rs:140-157), and the codec changes only enforce the intended node-side evo_tip requirement and clarify the unchecked Core-compatible mn_diff decode.

## Commands run

- CARGO_TARGET_DIR=/work/scratch/ch7a1-r2/target-sol TMPDIR=/work/tmp cargo test -p dashd-chainstate --lib evo — 39 passed.
- CARGO_TARGET_DIR=/work/scratch/ch7a1-r2/target-sol TMPDIR=/work/tmp cargo test -p dashd-chainstate --features rocksdb --lib evo — 39 passed.
- CARGO_TARGET_DIR=/work/scratch/ch7a1-r2/target-sol TMPDIR=/work/tmp cargo test -p dashd-chainstate --features crash --test crash — 39 passed, 5 ignored.
- CARGO_TARGET_DIR=/work/scratch/ch7a1-r2/target-sol TMPDIR=/work/tmp cargo test -p dashd-chainstate --features crash,planted-bugs --test crash_planted — 69 passed, 5 ignored.
- Inspected Core v24.0.0-rc.4 at the pinned tag for validation.cpp, evo/evodb.cpp, and llmq/blockprocessor.cpp, including all best-block checks, genesis handling, disconnect/replay writes, and indexed-reader assertions.

