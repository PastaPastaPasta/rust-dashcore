REWORK

## Major

### 1. Genesis is not treated as an absent EvoDB marker

- **Rust:** `dashd-chainstate/src/evo/view.rs:246-255` returns the active/frozen tip at `:248-250` and the durable `meta.evo_tip` at `:251-255` without the genesis special case. `dashd-chainstate/src/flush/machine.rs:942-943` writes that marker even for a full flush at genesis.
- **Core:** `src/evo/evodb.cpp:120-126` makes `VerifyBestBlock` false when the best-block key is absent; genesis never calls `WriteBestBlock` because `src/validation.cpp:2586-2592` returns from the genesis special case, while the check at `src/validation.cpp:2577-2583` requires the marker before a DIP3-active child.
- **Problem:** the view does not implement the approved D7/§3.1 rule that a marker naming genesis reads as absent. Both the in-memory levels and the durable fallback return the hash unchanged, so the consistency result at the first post-genesis block changes.
- **Scenario:** use a chain whose DIP3 activation is at height 1 and perform a full B1 flush after connecting genesis. Core has no EvoDB best-block row, so connecting block 1 aborts with EvoDB inconsistency. This view returns `Some(genesis)`, allowing the H2/`VerifyBestBlock` caller to pass. Normalize genesis to `None` in the read path (and add a regression test); retaining the marker on disk is the design’s intentional B1 behavior.

## Minor

### 2. A malformed height-index value escapes the store as a valid row

- **Rust:** `dashd-chainstate/src/evo/codec.rs:448-455` only checks that an index value is syntactically 36 bytes. `dashd-chainstate/src/evo/view.rs:223-234` checks that the stored mining block is on the queried chain, then pushes `base_height` without checking `base_height <= mined_height`.
- **Core:** the writer stores the quorum-base height at `src/llmq/blockprocessor.cpp:479-483`; the reader maps that value with `pindex->GetAncestor(quorumHeight)` and asserts it exists at `src/llmq/blockprocessor.cpp:761-767` (and the rotated reader at `src/llmq/blockprocessor.cpp:804-809`).
- **Problem:** a syntactically valid but impossible value is exposed through the raw `mined_index` API as `Ok`, rather than being reported as a store fault. Higher helpers may fail later in `base_through`, but the malformed row has already escaped the store boundary.
- **Scenario:** with the queried chain ending exactly at height 100, corrupt an `llmq_mined_h` row whose key says mining height 100 and whose stored mining block is that height-100 ancestor, but whose value says base height 101. `mined_index` returns that row; Core’s equivalent calls `pindex->GetAncestor(101)` and hits its corruption assertion. Reject the row before pushing it (and test both index families).

## Nit

### 3. The evo view tests do not cover the genesis marker rule

- **Rust:** `dashd-chainstate/src/evo/tests.rs:323-351` starts at block 1 and only asserts non-genesis tips, so the surviving “return `Some(genesis)`” mutant is not exercised.
- **Core:** the required distinction follows from `src/evo/evodb.cpp:120-126` and the genesis early return at `src/validation.cpp:2586-2592`.
- **Scenario:** the existing pinned-view test passes after a genesis B1 implementation that violates the first finding. Add a test that writes `meta.evo_tip = genesis` and asserts `best_block() == None`.

Validation run:

- `CARGO_TARGET_DIR=/work/scratch/ch7a1-review/target-sol TMPDIR=/work/tmp cargo test -p dashd-chainstate --lib evo` (32 passed)
- `CARGO_TARGET_DIR=/work/scratch/ch7a1-review/target-sol TMPDIR=/work/tmp cargo test -p dashd-chainstate --features rocksdb --lib evo` (32 passed)
- `CARGO_TARGET_DIR=/work/scratch/ch7a1-review/target-sol TMPDIR=/work/tmp cargo test -p dashd-chainstate --features crash --test crash` (39 passed, 5 ignored)
- `CARGO_TARGET_DIR=/work/scratch/ch7a1-review/target-sol TMPDIR=/work/tmp cargo test -p dashd-store --test engine_contract prefix_seek` (passed)

