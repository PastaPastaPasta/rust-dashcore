ACCEPT-WITH-NITS

# CH7a-1 review r1 (Opus): evo store, layer and read view

- **Tree:** /work/scratch/ch7a1-review/wt at 22ba00909. **Range:** fae761b83..22ba00909 (15 commits, 57 files).
- **Core:** v24 at 375c64d2317. `bp` = `src/llmq/blockprocessor.cpp`, `v` = `src/validation.cpp`, `edbh` = `src/evo/evodb.h`, `dbw` = `src/dbwrapper.h`, `snap` = `src/llmq/snapshot.cpp`, `timer` = `src/logging/timer.h`.
- **Design:** CH7 rev3.2 (§6.1, §7.2, §8.1, §11.2) and CH7-notes D23–D36.

## What I ran

All runs used `CARGO_TARGET_DIR=/work/scratch/ch7a1-review/target-opus` and `TMPDIR=/work/tmp`.

- `cargo test -p dashd-chainstate --features rocksdb --lib evo`: 32 passed. This includes the memory and RocksDB `MemEvoStore` differentials, the flush controller and machine evo tests, and the policy test.
- `cargo test -p dashd-chainstate --features crash --test crash`: 39 passed, 5 ignored.
- `cargo test -p dashd-chainstate --features crash,planted-bugs --test crash_planted`: 69 passed, 5 ignored. This covers FA7/X16 at read 18.
- `prefix_seek` contract: passes on `dash-kv` memory and latched, on `dashd-store` RocksDB, and on `dash-kv-sqlite`.
- Mutants: six mutants, each gated by an environment variable, in a reflink copy with its own target dir. Both are deleted. Five were killed and one survived (Minor 2).
  1. `read_below`: a frozen tombstone falls through to the DB. Killed by both differentials.
  2. `commit`: an active tombstone is read as absent. Killed by both differentials.
  3. `mined_scan`: an upper level's tombstone is ignored. **Survived all 23 evo tests.**
  4. `mined_row`: a tombstone is skipped. Killed by `mined_reads_keep_to_the_ancestry_they_name`.
  5. `mined_index`: the ancestry filter is off. Killed by `mined_reads_keep_to_the_ancestry_they_name`.
  6. `mined_commitment`: the ancestry filter is off. Killed by three tests.
- Not re-run: D34's `evo-index-depth` figures. I read the bench but did not measure it.

## Parity verified (no finding)

- **Byte layout.** Byte-exactness with Core's evodb is not this design's bar. D10, D23 and D24 make the store the node's own, and CH6b translates. I checked that each declared layout difference is real and that the semantics hold:
  - `llmq_mined` adds the mining height to Core's value (`bp:473`).
  - `llmq_mined_h` uses a family byte in place of Core's two prefixes `q_mcih` and `q_mcihi` (`bp:310-320`). Its value adds the mining block (`bp:480-482`).
  - The inverted BE32 height gives the newest-first order of Core's `BuildInversedHeightKey`.
  - `llmq_snapshot` keeps A1's key, not Core's `SerializeHash` (`snap:324-329`). Its value is Core's `CQuorumSnapshot` encoding (`snapshot.h:59-71`), and padding bits are refused, as `ReadFixedBitSet` refuses them.
  - `meta.evo_tip` stands in for `b_b4` (`v:2389`, `v:2967`).
- **Height-index reads.** These match `GetMinedCommitmentsUntilBlock` and `GetLastMinedCommitmentsByQuorumIndexUntilBlock` (`bp:731-773`, `bp:775-821`) for every chain end on the active chain:
  - the seek starts at the chain end's height;
  - the read stops at height 0 (`curKey >= lastKey`, `bp:749`, `bp:792`);
  - it stays within one type and family. Core's indexed loop does not compare `quorumIndex`, but with non-negative indexes LE byte order never reaches a numerically smaller index, so the node's prefix is equivalent.

  The node also filters by ancestry, which changes nothing on the active chain because undo erases the rows. Core's serving callers that could pass a stale block check `ActiveChain().Contains` first (`smldiff.cpp:177`, `snapshot.cpp:107`).
- **`WriteDerived`** (`edbh:159-205`, `bp:473-476`): a rewrite reads through the active index, then the frozen one, then the DB. An equal row is not rewritten, and a different one is refused with the layer unchanged. A tombstone reads as absent, as `CDBTransaction::Read` treats a pending delete (`dbw:654`). The height index is a plain `Write` (`bp:480-482`).
  - One stricter case is acceptable under D5: Core's `Read` fails on an undecodable row and Core then overwrites it, while the node refuses the commit with `Inconsistent`.
- **Tombstones** name both rows of the undo (`bp:579-589`). B1 writes them as deletes, and a later put revives them.
- **Flush:**
  - B1 carries the window's rows, `meta.evo_tip = new` and the marker before any coin batch. So a crash leaves either the B1-only image (evo at `new`, coins replayed) or the complete one. Core's order is the reverse: coins, then the evodb commit (`v:3157-3169`).
  - `EFFECT_CFS = ["llmq_snapshot"]`, and with no tip, mined rows count as corruption.
  - D35's wait is tested at exactly 64 MiB. Release is tested once per take, on both the tip and the no-tip paths.
- **Faults:**
  - A DB error in the seek, a height-index key of the wrong length, an undecodable value and a malformed `meta.evo_tip` each fail the read.
  - None is skipped: `a_db_fault_fails_the_mined_reads` and the `decoders_refuse_*` tests cover them.
- **The dash-kv seek contract:**
  - RocksDB seeks with `iterate_lower_bound = prefix` and `IteratorMode::From(max(start, prefix))`, and the upper bound comes from the prefix.
  - The in-memory engine uses `range(max(start, prefix)..)`.
  - The default implementation is a skip-while.
  - All three agree on the contract's starts below, inside, between and above the prefix.

## Major

None.

## Minor

### 1. After a release the freeze check accepts any tip, so a lost evo commit can become durable

- **Rust:**
  - `dashd-chainstate/src/evo/layer.rs:247-249` and `:271-273`: the layer's best block is the active or frozen `evo_tip`, otherwise `None`.
  - `layer.rs:276-278`: `release` drops the frozen index, together with its tip.
  - `dashd-chainstate/src/flush/control.rs:650`: `evo.is_none_or(|h| h == tip.hash)`.
  - CH7-notes D27 accepts `None`.
- **Core:** Core's freshness check is per block: `VerifyBestBlock` against `pprev` in `ConnectBlock` (`v:2579`) and against `pindex` in `DisconnectBlock` (`v:2311`). Its flush writes whatever the transaction holds (`v:3164-3168`).
- **Problem:** once a window has been released, the layer forgets the evo tip it knew. The next freeze then checks evo against nothing. D22 says "CH2's freeze check guards that the frozen index's `evo_tip` is the flush's `new`", but that guard is empty for any window that follows a released one.
- **Failing scenario** (with CH7a-2 wired):
  1. A flush to T is released, and the active index is empty.
  2. V connects T+1, but a path skips `commit_tx` (a bug, or a check-only flag that leaks).
  3. The coins move to T+1, and the site flushes right after that block.
  4. `check_pair` sees `best_block() == None` and passes.
  5. B1 writes `meta.evo_tip = T+1` with no rows for T+1.
  6. On restart, recovery accepts it (`evo_tip == tip`). H2 at T+2 reads the layer (`None`), then `meta.evo_tip = T+1 == pprev`, and passes.

  So the missing `mn_diff`/`credit_pool`/`mnhf` rows for T+1 are never detected. If the flush had come one block later, H2 would have caught it.
- **Fix:** keep the last committed `evo_tip` in `EvoLayer` across `take`/`release`, or seed it from `meta.evo_tip` at open, and report it from `best_block`. The empty-window case still passes, because the remembered tip equals `new`.

### 2. Tombstone hiding in the range read is untested; mutant 3 survives

- **Rust:** `dashd-chainstate/src/evo/view.rs:139-153` (`mined_scan`: the upper level wins a key, and its tombstone hides the rows below).
- **Core:** `CDBTransactionIterator` skips keys pending deletion in the transaction (`dbw:547`), so after `UndoBlock`'s erase (`bp:587-589`) Core's height-index loop (`bp:738-765`) never meets the row.
- **Problem:** if `mined_scan` ignores an upper level's `None`, the row beneath answers instead. That mutant passes every evo test, including both differentials and `mined_reads_keep_to_the_ancestry_they_name`. Every test read that meets a tombstoned `llmq_mined_h` row resolves at a block off that row's mining ancestry, so `in_ancestry` drops the row anyway (`view.rs:230`). In `mined_reads_keep_to_the_ancestry_they_name`, c4's put also overwrites the tombstoned key. So D22's "its tombstones hiding the rows below" has no test of its own, and any later change to the ancestry filter would expose erased rows unseen.
- **Failing scenario** (mutant 3):
  1. Connect b4 mining Q, then flush.
  2. Undo b4 with no replacement at height 4. The tombstone sits in the active index over the DB row.
  3. A view with a chain end whose ancestry includes b4 (b4 itself, read through a pinned chain) returns `[entry(1)]`. The code under review returns `[]`.

  No test makes this read.
- **Fix:** add a read at the undone block, or at a block above it on a chain that still holds it, for each of three placements of the tombstone:
  - active index over frozen;
  - active index over the DB;
  - frozen index over the DB.

  Assert both the `llmq_mined_h` range read and the `llmq_mined` point read, with no row rewritten at that key.

### 3. The D7 split-flush image with evo rows is not asserted

- **Rust:** `dashd-chainstate/src/flush/machine/tests.rs:1911-1927` asserts the split flush's steps and its log pair only. The non-split half (`:1880-1886`) checks the rows and `meta.evo_tip`.
- **Core:** `v:3157-3169` writes coins, then evo. The node's order is the reverse, so its B1-only image has no Core analogue and needs its own test.
- **Problem:** CH7 §7.2's L0 row says: "D7 … after a kill between B1 and Bk the marker is present, `meta.tip` is absent, `meta.evo_tip = new`, and recovery replays the coins only and ends at `new`". §9.1 assigns "`EvoDelta` in the B1 batch (D7)" to CH7a-1. With a real `EvoWindowDelta`, nothing checks the B1-only durable image (the evo rows and tombstone deletes at `new`, the marker, no tip) or a recovery from it.

  CH2's tests cover the marker with `()` only, and the CRASH model's evo is still `StubEvo`.
- **Failing scenario:** a regression that moves `delta.write_b1` after the split decision, into Bk (the planted T3/T4/T24 shape at `machine.rs:956-965`, but unplanted), still passes this test. The test only counts the log pair and the step order.
- **Fix:** in the split half, read the unprobed DB at the `Durable` mark after B1:
  - the credit-data row is present and the tombstoned keys are absent;
  - `head_blocks = (new, old)`, no `meta.tip`, and `meta.evo_tip = new`.

  Then run recovery on that image and check the evo rows are unchanged.

## Nit

1. **The D20 pair on a failed B1.**
   - **Rust:** `dashd-chainstate/src/flush/machine.rs:980-984` and `:989-994`. **Core:** `v:3165-3168` and `timer:38-47`.
   - Core's `Timer` destructor logs "FlushStateToDisk: write evodb cache to disk completed (%.2fs)" on scope exit, after `AbortNode(…, "Failed to commit EvoDB")`. The node's `?` returns before `done`, so a failing B1 logs "started" with no "completed".
   - **Scenario:** B1's `engine.write` returns an I/O error. Core's log has the started line, the "Failed to commit EvoDB" error and the completed line. The node's log has only the started line, then CH2's halt.
   - The path is fail-stop either way, but the log-line ratchet treats Core's lines as assertions. Either declare it in D20/D30 or log `done` on the error path.
2. **`decode_diff` promises a check it does not make.**
   - **Rust:** `dashd-chainstate/src/evo/codec.rs:315-325`. Its doc says "An `mn_diff` value, which must be a diff of `block`'s height", but the function takes no block and checks nothing. `decode_snapshot` does check the block (`:338-342`).
   - Core reads `DB_LIST_DIFF` unchecked too (`deterministicmns.cpp`), so this is no parity gap.
   - **Scenario:** an `mn_diff` row under B holding the diff of another height decodes, and `resolve_list` applies it.
   - Either fix the doc, or pass the height and refuse a mismatch as `Inconsistent`, matching `decode_snapshot`.
3. **D25's list of wrappers that forward the seek is incomplete.**
   - These wrappers do not override `iter_prefix_from` and fall back to the default skip-while:
     - `dash-kv/src/faulty/engine.rs:668` (`FaultyEngine`, under the CRASH model);
     - `dashd-node/src/sut.rs:57` (`SharedMemory`);
     - `dashd-node/src/chainstate.rs:561` (`WalFault`).
   - Results are correct, since they are test-only wrappers. But the note says "the wrappers forward it", and a CRASH run with NodeEvo will do a full prefix walk where production seeks.
   - Add the three to D25 as non-seeking, or forward the call. The contract suite does not run on `FaultyEngine`.
4. **D23 leaves out the width of Core's quorum index.** Core's indexed key is `(string, LLMQType, int quorumIndex, uint32)`, and `int{qc.quorumIndex}` serializes it as a 4-byte LE int32 (`bp:316`, `:480`, `:587`). The node's key is a BE16. Reads are per index prefix, so nothing observable changes. CH6b translates, so the note should state the width.
5. **D22's pending-delta level is not in the view, and the notes don't say so.** D22 says "A read checks the attempt's pending delta (one block), then the active index…". `EvoReadView::new` (`view.rs:56-69`) has only the active index, the frozen one and the snapshot. D31 defers `at` and `evo_view` but not the pending level, which §4.1.2's `QcBinding` needs ("the layer plus this attempt's delta"). Add the deferral to D31 so CH7a-2 inherits it explicitly.
6. **A missing `evo_tip` defaults to the delta's own block, even for a disconnect** (`codec.rs:311`; D27). Core always writes `pprev` on disconnect (`v:2389`). A CH7a-2 disconnect that forgets to stage it records the disconnected block. Today that is caught only by `check_pair`, and only while the active index is non-empty (see Minor 1). Consider making `EvoCodec::rows` refuse `evo_tip == None` when `erased_mined` is non-empty, or for every node-built delta, and keep the default only in `MemEvoStore`.
7. **The bench measures one index, not the view's merge.** `benches/evo_index_depth.rs:137-150` times `EvoIndex::mined`, `get` and `mined_from` on one active index with an empty DB. The view's three-way merge (`mined_scan`, its per-row `Vec` of heads, the DB iterator) is not measured. That matches §7.1's letter ("`EvoIndex` point reads and … range reads"), but D34 should say the bench covers the index alone.
