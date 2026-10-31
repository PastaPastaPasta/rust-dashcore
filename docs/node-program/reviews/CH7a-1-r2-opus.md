ACCEPT-WITH-NITS

# CH7a-1 narrow r2 review (Opus): the r1 fix round

- **Tree:** /work/scratch/ch7a1-r2/wt at a8baccb51. The worktree is clean and I wrote no files in it.
- **Commits reviewed:** 55c34ea60, 54b185387, fa4d25745 and a8baccb51, each against its first parent. I did not review the follow-merge 1bf8d41ac.
- **Core:** 375c64d2317 (v24.0.0-rc.4-151), whose line numbers match the notes' `v:NNNN` citations.
  - `bp` = `src/llmq/blockprocessor.cpp`, `v` = `src/validation.cpp`, `dbw.cpp` = `src/dbwrapper.cpp`, `timer` = `src/logging/timer.h`.
- **My scope:** Opus r1 m1–m3; the nits in CH2's `flush/machine.rs` and `recovery/tests.rs` and in dashd-node's `sut.rs`/`chainstate.rs`; and regressions across the fix diff.

## Verdict on the r1 findings

| r1 | Status | Evidence |
|---|---|---|
| m1: the freeze check forgets the evo tip after a release | **Fixed** | See (a) below. |
| m2: tombstone hiding in the range read untested | **Fixed** | See (b) below. |
| m3: the D7 B1-only image with evo rows not asserted | **Fixed** | See (c) below. |
| n1: D42, the evodb pair on a failed B1 | **Fixed** | See (d) below. One stale doc comment remains (Nit 1). |
| n2: D41, `decode_diff`'s doc | Fixed | The doc at `codec.rs:319-320` now says the read is unchecked. This matches Core's unchecked `DB_LIST_DIFF` read. |
| n3: D25, wrappers that don't forward the seek | Fixed | See (e) below. |
| n4: D23, the quorum index's width | Fixed | The note now states it. Core's key is `tuple<string, LLMQType, int, uint32_t>` (`bp:316-319`), written with `int{qc.quorumIndex}` (`bp:480`). |
| n5: D31, the pending-delta level | Fixed | D31 now defers the pending-delta level to CH7a-2. |
| n6: D40, a delta with no evo tip | Fixed | See (f) below. |
| n7: D34, what the bench measures | Fixed | D34 now says the bench times `EvoIndex`'s own reads, not the view's merge. |

**(a) m1.** `EvoLayer::committed_tip` is set only by a successful `commit` (`layer.rs:254`). `take` and `release` do not touch it, and `best_block` returns it (`layer.rs:286-288`).
- On a failed commit the layer is unchanged, including this field.
- `resume` seeds it.
- My r1 scenario now aborts in `check_pair` (`control.rs:648-651`): a released window, a skipped commit for T+1, then a flush at T+1 compares `Some(T) != T+1`.
- An empty window still passes, and so does a flush at genesis after block 1's disconnect (`evo/tests.rs`, "the freeze check's tip").
- Production has no caller yet; CH7a-2 wires it.
- Mutants 3 and 4 below are killed.

**(b) m2.** `a_tombstone_hides_the_row_below_from_the_mined_reads` (`evo/tests.rs:519-563`) covers all three placements I asked for:
- active over frozen;
- active over the DB;
- frozen over the DB.

It asserts both `mined_index` and `mined_commitment` at b4, whose ancestry still holds the erased row. r1's surviving mutant 3 is now killed (mutant 1 below).

**(c) m3.**
- **Durable image.** `a_split_flushs_b1_alone_holds_the_evo_window_and_the_marker` (`flush/machine/tests.rs:1895-1931`) stops at the `Durable` mark after `[Space, Files, B1]`. It reads the unprobed engine and checks:
  - the credit row (16 bytes) is present;
  - both tombstoned keys are gone, which `evo_window` (`:1813-1818`) writes durably first, so the absence check is not vacuous;
  - `head_blocks = (new, old)`, no `meta.tip`, and `meta.evo_tip = new`.
- **Recovery.** `a_b1_only_image_recovers_the_coins_and_keeps_the_evo_rows` (`recovery/tests.rs:721-795`) recovers to `new` with the coins replayed and `meta.evo_tip = new`. All six evo column families are byte-identical before and after recovery. Recovery's own check is `replay.rs:357`.
- The recovery test builds its image by hand rather than from the machine run. The machine test pins the same image shape, so together they cover §7.2's L0 row for D7.
- Mutants 2 and 5 below are killed.

**(d) n1, D42.** I checked the claim against Core:
- `CEvoDB::CommitRootTransaction` returns `db->WriteBatch(...)` (`evo/evodb.cpp:98-108`).
- `CDBWrapper::WriteBatch` calls `HandleError`, which logs "Fatal LevelDB error: …" and throws `dbwrapper_error` (`dbw.cpp:217-218`, `:271-278`). It never returns false.
- So `v:3166-3167`'s "Failed to commit EvoDB" is unreachable. `~Timer` logs "completed" during unwind (`timer:42-48`). The `catch (const std::runtime_error&)` at `v:3202-3203` logs "System error while flushing: …".
- The node's `Halt::system` (`machine.rs:217-220`) uses the same prefix.
- `evo_timed` (`machine.rs:1384-1396`) wraps both the split write and the non-split `write_final`, and logs `done` whatever the result.
- `a_failed_b1_still_closes_the_evodb_pair` runs both shapes, with failure at write 1 and `next == B1`, and asserts the pair with no Durable mark.
- Core's coin-write failure comes before the pair, while the node's non-split B1 prints the pair for either. D42 declares that difference.

**(e) n3, D25.**
- `SharedMemory` (`sut.rs:65-72`) and `WalFault` (`chainstate.rs:569-576`) now forward `iter_prefix_from`.
- Their snapshot types are the inner engines' (`MemorySnapshot`, and `RocksEngine::Snapshot` at `chainstate.rs:~590`), which already seek.
- `FaultyEngine` keeps the default, and D25 says why (one fingerprinted trace op per scan).
- `WalFault` is `cfg(any(test, feature = "test-hooks"))` (`chainstate.rs:126-128`, `:524`), so production `Engine` is `RocksEngine` and the change has no production effect.

**(f) n6, D40.** `EvoCodec::rows` refuses `evo_tip == None` as `Inconsistent` (`codec.rs:307-308`), and `codec_tests.rs:302-305` tests it.
- No producer in the node feeds `None` through `EvoCodec`. The bench and testkit stage `Some`.
- `dash-dml/benches/qc_block.rs:270` uses `None`, but only against `MemEvoStore`, whose default is unchanged.

## Regressions across the fix diff

I found none.
- `RocksEvoStore::new`'s new `genesis` argument has only test and bench callers. The bench passes `hash(0, 0)`, and no block in the bench has height 0.
- The mutant runs below confirm the `planted!` paths T3/T4/T24/T17 still route through `evo_timed`/Bk as before. `crash_planted` passes 69/69.
- Clippy (`-D warnings`, all targets) and `cargo fmt --check` are clean on dashd-chainstate, dashd-node and dash-dml.

## Major

None.

## Minor

None.

## Nit

### 1. The D42 test's doc keeps the account a8baccb51 corrected

- **Rust:** `dashd-chainstate/src/flush/machine/tests.rs:1868-1869`: "as Core's `Timer` logs its completed line on scope exit after the failed commit's abort".
- **Core:** `dbw.cpp:271-278` throws, so `v:3166-3167`'s abort is unreachable. `~Timer` logs during unwind, before the abort at `v:3202-3203`.
- **Problem:** a8baccb51 fixed this wording in `evo_timed`'s doc (`machine.rs:1385-1389`) and in D42, but not in the test that cites D42. A reader of the test gets Core's order backwards: "after the abort" instead of "before 'System error while flushing'".
- **Also:** the design's D20 (`CH7-evo-llmq-chainstate.md:792`) still says "'Failed to commit EvoDB' on failure". D42 supersedes it, so this is only worth a touch when §11.2's wording pass runs.
- **Fix:** reword the test doc to match `machine.rs:1387-1389`.

### 2. D37 should name the raw read that `resume` takes

- **Rust:** `EvoLayer::resume` (`layer.rs:193-200`). `EvoReadView::best_block` normalises a genesis tip to `None` (`view.rs:257-272`, D38). The raw value is `Meta::read(..).evo_tip` (`store/meta.rs:56`, `:75`).
- **Core:** `VerifyBestBlock` compares the raw `b_b4` (`evo/evodb.cpp:120-126`).
- **Problem:** D37 says CH7a-2 resumes "from `meta.evo_tip` as read after recovery". `RocksEvoStore`'s only public evo-tip read is the view's normalised one. If CH7a-2 seeds `resume` from the view, then on a store whose durable tip is genesis the layer resumes with `None`, and the first freeze check after that open accepts any tip. This is a silent weakening, not a false refusal. The case needs a disconnect back to genesis, a flush and a restart.
- **Fix:** add to D37 that `resume` takes `Meta::evo_tip` unnormalised. Better, give `RocksEvoStore` a method that builds the resumed layer from the raw meta row, so CH7a-2 cannot pick the wrong read.

## Commands run

All runs used `CARGO_TARGET_DIR=/work/scratch/ch7a1-r2/target-opus` and `TMPDIR=/work/tmp` in /work/scratch/ch7a1-r2/wt, unless noted.

- `cargo test -p dashd-chainstate --features rocksdb --lib evo`: 39 passed.
- `cargo test -p dashd-chainstate --lib evo`: 39 passed.
- `cargo test -p dashd-chainstate --features rocksdb --lib recovery`: 28 passed.
- `cargo test -p dashd-chainstate --features rocksdb --lib flush::`: 143 passed.
- `cargo test -p dashd-chainstate --features crash --test crash`: 39 passed, 5 ignored.
- `cargo test -p dashd-chainstate --features crash,planted-bugs --test crash_planted`: 69 passed, 5 ignored.
- `cargo test -p dashd-node --lib`: 159 passed, 3 ignored. This includes the `SharedMemory`/`WalFault` users.
- `cargo clippy -p dashd-chainstate --features rocksdb,crash --all-targets -- -D warnings`: clean.
- `cargo clippy -p dashd-node --all-targets -- -D warnings`: clean.
- `cargo fmt --check -p dashd-chainstate -p dashd-node -p dash-dml`: clean.
- **Mutants.** I ran these in my own reflink copy (`opus-scratch/wt-mut` with `target-mut`), with one build gated by `OPMUT=n`. Each run used the lib test binary filtered to `evo flush:: recovery` (204 tests). Baseline `OPMUT=0`: 204 passed. All mutants were killed:
  1. `view.rs` `mined_scan`: an upper level's tombstone is ignored (r1's surviving mutant 3). **Killed** by `a_tombstone_hides_the_row_below_from_the_mined_reads`.
  2. `machine.rs`: a split flush writes the evo window and `meta.evo_tip` in Bk instead of B1 (the T4 shape, unplanted). **Killed** by `a_split_flushs_b1_alone_holds_the_evo_window_and_the_marker`, `a_large_flush_is_split_under_the_marker` and `dbcrashratio_needs_the_synchronous_freeze`.
  3. `layer.rs` `release` clears `committed_tip`. **Killed** by `the_freeze_checks_evo_tip_survives_the_release`.
  4. `layer.rs` `best_block` returns `evo_tip()` (r1 behaviour). **Killed** by `the_freeze_checks_evo_tip_survives_the_release`.
  5. `machine.rs`: B1 omits `meta.evo_tip`. **Killed** by three machine tests, including the new split-image test.

  The copy and its 81 GB target are deleted (`rm-if-idle.sh`).
- I did not check Sol's scope (M1/D38's Core-site argument, D39's strictness, n3), beyond confirming the genesis test runs green.
