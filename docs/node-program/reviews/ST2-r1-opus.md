# ST2 review r1 (Opus 5.5): Core-identical block files

| | |
|---|---|
| Task | PLAN ST2: `dashd-store::flatfile` and `dash-coreimport::blockfiles::external` |
| Branch | rust-dashcore `node/st2-block-files` @ `157a5193` (worktree `~/workspace/rdc-wt/st2-block-files`). Reviewed only ST2's 12 commits `e6f00f02..157a5193` (`252df9a9` … `157a5193`), on top of ST1's merge `e6f00f02`. |
| Class | STATE. GPT unavailable (DEC-42): this is one of the two Opus reviews. The `code-review` skill ran at high effort as one input (§6). |
| Core | `~/workspace/dash`, tags v23.1.8 and v24.0.0-rc.3: `node/blockstorage.{h,cpp}`, `flatfile.cpp`, `util/fs_helpers.cpp`, `validation.cpp` (`LoadExternalBlockFile`, `FlushStateToDisk`, `CheckBlockIndex`), `streams.h` (`CBufferedFile`), `primitives/{block,transaction}.h`, `chain.h`. |
| Verdict | **CHANGES-REQUIRED**: one major (M1, a one-line fix). Otherwise the port is faithful at both pins, and every parity claim I re-ran held. |

## 1. What I ran

All runs used `TMPDIR=/dev/shm/st2-review-tmp` (mode 700), a private `CARGO_TARGET_DIR` under `/work/scratch/st2-review/`, and release dashd binaries copied from `/work/scratch/cs2-pow-subsidy/fpgate/bin/` (v23.1.8 `7bdb2ce3…`, v24.0.0-rc.3 `e204e316…`). Nothing under `/work/chains`, `/work/bench` or `/work/bench-store` was written to.

| Check | Result |
|---|---|
| `cargo test -p dashd-store flatfile`, `cargo test -p dash-coreimport` | pass (18 flatfile unit tests; all coreimport suites) |
| `clippy --all-targets -D warnings` (both crates, `raw-bytes`), `fmt --check`, `cargo bench --no-run` (flatfile) | clean |
| `blockfiles_parity` on `regtest-40000` (`DASHD_TEST_DATA`) | byte-identical (40,001 blocks, 1 blk/rev pair) |
| **All 11 driver cases plus the base**, built by the branch's own `st2-parity-cases.sh` + `gen-blockfile-chains.py` on a **4,000-block** base chain (v23.1.8, big block at 3,990, 6,153 wallet txs). The driver needed 50 s on /dev/shm, the base 2 min 45 s. | **12/12 byte-identical**: r23/r24 default, tinyblk, fastprune (3 restarts before rollovers, 176/180 files), l23-default, l24-fastprune, x23-default, x24-fastprune, base |
| Discrimination: the same parity run with the release **swapped** (scratch copy of the test) | r23-fastprune with v24 rules fails (`rev00024.dat: Core 4776 bytes, ours 1048576`), and l24/r24-fastprune with v23 rules hit `FastpruneBlockTooLarge`. The default, tinyblk and reindex cases pass either way (§4.1). |
| Parity with Core's shutdown `FlushBlockFile()` replaced by `sync()` | still 12/12 on these cases (§3 m2 explains why the gap exists anyway) |
| `scanner_parity` with `DASHD_PATH` (23.1.0) | 24 + 2 cases identical |
| `scanner_parity` with both release dashds, the branch's seeds | 120 small + 6 large identical |
| `scanner_parity` with both release dashds, **fresh seeds** (`SEED_BASE=777000`, scratch copy) | 300 small + 8 large identical |
| `read_core_datadir` on the testnet oracle, read-only (`ST2_READ_DIRS=testnet=…/testnet3/blocks`) | pass: 1,513,363 blocks and 1,513,348 undo records, 0 unmatched (§5) |

So I independently reproduced 434 damaged-file cases (308 on seeds the author never ran) and all 12 block-file cases, but at 4,000 rather than 40,000 blocks. The 40,000-block chain only adds restarts inside a single 128 MiB file, which cannot discriminate anything (§4.1).

## 2. Core equivalence (both pins)

I checked every ported function line by line against both tags.

- **`FlatFileSeq`** (`flatfile.cpp`, identical at both pins except log text):
  - `Allocate`'s chunk arithmetic, and `CheckDiskSpace` (`free ≥ 50 MiB + inc`, statvfs `f_bavail·f_frsize` = `fs::space().available`);
  - `AllocateFileRange`'s Linux branch (`posix_fallocate(fd, 0, end)`, then a zero-write fallback);
  - `Flush`: open, or create as `wb+` does; truncate if finalizing; `FileCommit`, where `fdatasync` treats EINVAL as success; `DirectoryCommit`.

  All match. The deviations are listed in ARCHITECTURE.md (open failure is an error, directory-sync errors are reported, fail-stop). I agree with all of them.
- **Sizes.** `BLOCKFILE_CHUNK_SIZE` 16 MiB, `UNDOFILE_CHUNK_SIZE` 1 MiB and `MAX_BLOCKFILE_SIZE` 128 MiB all match. So do `-fastprune` (64 KiB files, 16 KiB blk chunk, rev chunk unchanged) and Dash's `-tinyblk` (64 KiB chunk for blk and rev; `-fastprune` wins for blk). The undo record is `+40`, and `BLOCK_SERIALIZATION_HEADER_SIZE` is 8.
- **`FindBlockPos`.**
  - Known positions: `nSize = max(nPos + nAddSize, nSize)`, where `nAddSize` is the block's serialized size without the header.
  - Rollover loop: `nSize + nAddSize >= max`.
  - Flush of the old file: `FlushBlockFile(!fKnown, finalize_undo)` runs before `m_last_blockfile` changes.
  - `AddBlock` and `m_dirty_fileinfo` match.
  - **v23** `finalize_undo = nHeightLast == Tip()->nHeight`. **v24** `== m_undo_height_in_last_blockfile`, which starts at 0 and is reset on every file switch, known or not (`blocks.rs:302-323`). This is correct.
  - The v24 `-fastprune` growth (`max = nAddSize + 1`) and the assert are in `blocks.rs:287-301`.
- **`FindUndoPos` / `WriteUndoDataForBlock`.**
  - A lagging rev file is finalized when the file's last height is written (`_pos.nFile < m_last_blockfile && nHeight == nHeightLast`).
  - The v24 `else if` that tracks the undo height is ported. Under v23 rules it is tracked but never read, which is harmless.
- **`FlushBlockFile` / `FlushUndoFile`.**
  - `if (!fFinalize || finalize_undo)` matches.
  - v24's early return before infos are loaded is covered by `infos.get(last_file)`.
- **Undo checksum.** It is `SHA256d(prev_hash ‖ CBlockUndo)` (Core `HashWriter << hashBlock << blockundo`), written as the raw 32-byte digest.
  - `CBlockFileInfo` is 7 VARINTs; `FlatFilePos` is `VARINT(NONNEGATIVE_SIGNED) ‖ VARINT`; `AddBlock` matches.
  - The test-only `write_block_undo` dummy byte (`nHeight > 0`) matches `TxInUndoFormatter::Ser`.
- **Pruning.**
  - `FindFilesToPruneManual`: the unsigned `tip − 288` wrap is reproduced.
  - `FindFilesToPrune`: the buffer of 16 MiB + 1 MiB, the `is_ibd` 10 % term, and the skip/break order all match.
  - `PruneOneBlockFile` (the file half), `CalculateCurrentUsage` and v24's `ScanAndUnlinkAlreadyPrunedFiles` match.
- **Scanner** (`LoadExternalBlockFile` and `CBufferedFile`). The scan is identical at both pins; v24's `FindByte` only optimises the loop and yields the same positions.
  - Covered: 32-bit `Fill` arithmetic, the short-`fread` EOF flag, `AdvanceStream`'s limit check, `SetPos` clamping, and `nRewind` bookkeeping in all three places.
  - Covered: the "no valid header → break" versus "exception → unexpected data at `nRewind-1`" split.
  - Covered: Dash tx deserialization (`int16` version, payload iff `nVersion >= 3 && nType != 0`), canonical CompactSize with `MAX_SIZE` 0x02000000, and byte vectors read in 5,000,000-byte chunks.
  - `MaxBlockSize()` is 2,000,000 on every network: `LoadExternalBlockFile` calls it with the DIP0001 default. The skill's "hardcoded network parameter" finding is therefore not a finding.
- **Reading by the size field rather than by deserializing** (ARCHITECTURE deviation 6). Safe for every position Core can produce:
  - `WriteBlockToDisk`/`UndoWriteToDisk` write the exact `GetSerializeSize`.
  - The `-reindex` scanner only accepts a record whose block fits inside `nSize` (`SetLimit`). So the size field is always at least the block, and the block is a prefix of what `read_block` returns.
  - CS0's `decode_block(&mut &[u8])` accepts trailing bytes and exposes the consumed `wire`, so the documented caller contract is implementable.
  - Undo is stricter only under corruption: a flipped size field with intact data fails here and reads in Core. Acceptable.

## 3. Findings

### Major

**M1. v23.1 `-fastprune` with a block of 64 KiB or more must follow v24, not fail-stop** (`dashd-store/src/flatfile/blocks.rs:287-299`, `error.rs:30-36`, `ARCHITECTURE.md` deviation 7, test `tests.rs:309-319`).
- O17 row **WAL-45** (`O17-pin-delta.md:500`, `:511`; `O17-pin-delta/wallet.md:62`) classes this as **`v24-all`, policy B** — "A non-terminating branch is not a behaviour to reproduce". It is one of the declared deviations DEC-37 approved, so the rule under the v23.1 profile is v24's growth (`max = nAddSize + 1`).
- ST2 instead returns `FastpruneBlockTooLarge`, and `mutate()` turns that into permanent fail-stop.
- **Failure scenario:** an rdashd regtest node running `-fastprune` under the v23.1 profile receives a block of 64 KiB or more, for example from a peer or `submitblock`, as v24's `feature_fastprune.py` sends. From then on every `save_block`, `write_undo` and `sync` returns `Failed`, and the node must abort, where dashd v24 and the declared deviation keep running.
- Byte parity with v23.1.8 is not at stake: v23.1.8 never produces such a file, because it loops until OOM.
- **Fix:** delete the `Release::V23_1_8` arm and the error variant, so that both releases use the v24 rule. Update deviation 7 and the test, and cite WAL-45 in the comment. `st2-parity-cases.sh` can keep stopping the *dashd* v23 replay before the big block.

### Minor

**m1. `BlockFiles::state()` is documented as "to persist with the block index" but is not barrier-protected** (`blocks.rs:169-175`).
- A CH2 implementer who follows the doc and writes `state()` into B1 commits `block_files` sizes covering unsynced bytes, which violates I1. The safe path is `sync_and_take_dirty_infos()`, which also lacks `last_file` even though Core writes `DB_LAST_BLOCK` in the same batch.
- **Fix:**
  - Reword `state()` as "for reopening and tests only".
  - Make `sync_and_take_dirty_infos` return `(last_file, dirty infos)`.
  - Add an explicit `adopt_existing_files()` (mark every file and the directory unsynced) for IM1/CH6, whose Core-datadir import will reference bytes another process wrote. Today only `save_block(known_pos)` marks such files.

**m2. The production barrier does not reproduce Core's `FlushStateToDisk → FlushBlockFile()`** (`blocks.rs:455-463`).
- Core's flush always opens the current blk/rev pair with `rb+`/`wb+`, so it creates an empty `revNNNNN.dat` when the newest blk file has no undo yet. That happens during IBD with blocks ahead of the tip, or on shutdown in the middle of `-loadblock`. Core then fsyncs the pair.
- `sync_inner` neither creates nor syncs an untouched current pair.
- The parity test hides this: it calls `flush_block_file(false, false)` explicitly at every restart and at shutdown (`blockfiles_parity.rs:179`, `:225`). My `sync()`-only variant still passed only because no case ends with a blk file that lacks undo.
- **Failure scenario:** stop rdashd and dashd at the same point of an IBD with blocks ahead of the tip. Core's datadir has one more (empty) `rev` file than ours, so the "byte-identical blk/rev" claim fails on file sets.
- **Fix:** have `sync_inner` call `flush_block_file_inner(false, false)` first, which is exactly Core's flush; or document that CH2 must call it at every `FlushStateToDisk` point and add a case that ends with blocks ahead of the tip.

**m3. `OsFs` can evict and close the writable handle of a file with unsynced pages** (`fs.rs:118-124`).
- With more than 64 files touched between barriers, the LRU closes a dirty writable fd. This happens in `-reindex`, where `mark_unsynced` makes the barrier open one handle per blk file.
- If writeback then fails and the inode is evicted before the barrier reopens the file, `fdatasync` on the new fd succeeds although the data is gone (the fsyncgate pattern).
- Core shares the hazard (it opens per operation), but A1 §3.5.6 promises more than Core.
- **Fix:** never evict a handle whose file is in `unsynced`, or give the barrier its own sync-only path that does not go through the cache.

**m4. No O17 row covers the rev-file finalize rule (bitcoin#27746 in Dash #7553), and O17 calls it unreachable.**
- `O17-pin-delta/consensus.md:61` files #27746 under "unreachable in production".
- `p2p-init.md:160` notes "#7553 → bitcoin#27746: rev-file finalize heuristic (file truncation timing only)" without a row.
- In fact `m_undo_height_in_last_blockfile` runs at every blk rollover on every node. My swap experiment shows it changes rev-file lengths after a restart: Core 4,776 bytes versus an untruncated 1 MiB.
- It is INT: contents are equal and only trailing preallocation and lengths differ. It still needs a row, because ST2's `Release` switch is meant to become a `ReleaseProfile` field (ARCHITECTURE.md) and has no spec home.
- **Fix (manager/O17):** add an INT row for the finalize rule (profile or `none`; ST2 already implements both).
- **CONS-28** (`ScanAndUnlinkAlreadyPrunedFiles`) is keyed `none`, so ST2's `Release` gate on it (`blocks.rs:588-596`) also has no field. Either always unlink (harmless hygiene) or have the O17 row cover it.

**m5. Unlink failures fail-stop the store** (`blocks.rs:574-583`).
- v24's `UnlinkPrunedFiles` ignores `remove` errors (`std::error_code`, CONS-28 says so). v23's throwing `fs::remove` leads to `AbortNode` via `FlushStateToDisk`'s catch.
- A1 §3.5.6's fail-stop list covers writes and syncs, not unlinks after a durable B1. The unlink is harmless to consistency once B1 is durable.
- **Fix:** log and continue on non-ENOENT unlink errors (v24); or list the choice as deviation 11 and keep it; but decide explicitly.

**m6. The scanner takes `&[u8]` and cannot be interrupted** (`external.rs:302-329`).
- `-loadblock` accepts any file. A single `bootstrap.dat` of tens of GB must be read into memory or mmapped. A1 §2.11 keeps reads off mmap for RssFile honesty, though a one-shot import is a different case.
- Core checks `ShutdownRequested()` before every record. Here the sink can stop only from `block()` or `after()`, which a run of unknown-parent records never reaches.
- **Fix (now, or recorded as a CH5 prerequisite):** a `Read + Seek`-backed `BufferedFile` with Core's real 2·MaxBlockSize ring buffer, so that positions and EOF behaviour are unchanged and the differential harness re-proves it. Also an `ExternalBlockSink::interrupted()` check at the top of the loop.

**m7. Parity coverage gaps** (methodology; the claims hold, but they say less than "12 cases" suggests):
- Only the `-fastprune` cases roll over. The default and tinyblk cases are single-file at 40,000 blocks too, so they are insensitive to the release rules (§4.1).
- No reorg case. v23's `Tip()->nHeight` rule and v24's undo-height rule diverge most when the tip moves backwards, and undo is then written out of height order.
- No pruning parity case: `FindFilesToPrune*` and the unlink are checked only by a unit test.
- **Fix:** add a `-fastprune` replay with an `invalidateblock` + competing-branch reorg straddling a rollover, and a `-prune=1 -fastprune` + `pruneblockchain` case compared on file sets.

**m8. Prune selection panics on an inconsistent state** (`blocks.rs:501-502`, `543-544`, from the skill). `infos[file]` for `file < last_file` panics if the caller's `BlockFilesState` has fewer infos than `last_file + 1`.
- Core has the same precondition: `LoadBlockIndexDB` resizes to `last + 1` and v24 asserts it.
- **Fix:** validate in `with_fs` (resize, or reject), matching v24's assert at one place.

**m9. A1 §2.11's asynchronous write split is not offered.**
- A1 has `accept_block` get the position synchronously, while the bytes are written on `db-writer`. `BlockFiles` reserves and writes in one `&mut self` call (`save_block`).
- That is Core's model and is fine, but CH3/CH2 will need to decide one of two things: put `BlockFiles` behind a mutex and write on the validation thread, as Core does; or split the call into `reserve_block_pos` + `write_at`.
- Record the decision in the CH3 brief; nothing to change in ST2 unless the manager wants the split now.

### Nits

- n1. `make_private` copies up to 128 MiB with no `CheckDiskSpace` (`seq.rs:133-143`). Out of space then shows up as an `Io` fail-stop rather than "Disk space is too low!".
- n2. `link_count == Some(0)`: a cached handle whose file was unlinked externally goes into `make_private`, whose `copy` then fails with NotFound and fail-stops. Treat 0 as "missing" (skill).
- n3. Only the first `blocks/` parent is synced. If `create_dir_all` created several levels (fresh datadir), the grandparent entry is not synced. That is probably ND1's datadir-lock job; say so.
- n4. `Release::V23_1_8` vs the release name O17 settled on, `V23_1` (`O17-pin-delta.md:750`, "`V23_1_8` → `V23_1`").
- n5. `gen-blockfile-chains.py:101` waits on `initialblockdownload_reindex`, which Dash's `getblockchaininfo` does not have, so the guard is dead. It is harmless today, since with no peers the height reaches the target only at the end of the import. The `--resume` manifest records only the last resume (`:226`). Both are from the skill.
- n6. `ScanReport.headers` omits records whose header was read but whose `SkipTo` hit EOF (`external.rs:359-364`).
- n7. The `external_scan` fuzz target is a property target, not a differential one. DEC-30 N5 says every fuzz target runs through the CS0/O3 runner, so register it there or record why it is exempt.
- n8. `external.rs` duplicates `MAX_SIZE` and the tx layout of `tx.rs`. The duplication is intentional, because stream positions matter; say so in the module doc.

## 4. Method notes

### 4.1 What the parity cases discriminate

Swapping the release in the test shows that only the `-fastprune` save-then-connect and loadblock cases are sensitive to the v23/v24 rules. Without `-fastprune`, 40,000 regtest blocks (about 40 MB) never leave `blk00000.dat`, and known positions (`-reindex`) never finalize. The author's restarts at 10,000 and 25,000 in the default cases are therefore no-ops for the finalize rule. That is fine, but the evidence for the release switch is r23/r24-fastprune with `--restart-before-rollover 25`, not "12 cases". m7 asks for the reorg and prune cases that are missing.

### 4.2 Crash and kill scenarios I worked through

| Scenario | Outcome |
|---|---|
| SIGKILL after `save_block`/`write_undo`, before the barrier | The page cache keeps the bytes. The durable index references only synced bytes, so the restart appends at the durable `nSize`/`nUndoSize` and overwrites. Same as Core. |
| Power loss after `sync_and_take_dirty_infos` returns, before B1 | Durable but unreferenced bytes; harmless. |
| Power loss after B1 | Every byte B1 references was covered: each file in `unsynced` got `fdatasync`; creations and truncations were followed by a directory fsync. The first barrier of every process syncs `blocks/` and its parent (`seq.rs:91-92`), which covers a directory entry created by a killed predecessor. |
| Rollover finalize (truncate) followed by a crash before B1 | The truncation itself is synced (`flush` = truncate + `fdatasync` + directory fsync). The restart continues in the same file from the older durable size and re-extends it. Same as Core. |
| `make_private`, crash between copy and rename | A `.unlink` temporary is left. The shared original is untouched and the next attempt removes the temporary first. |
| `make_private`, crash between rename and directory sync | Either inode is complete and identical. No write goes through before `sync_dir` returns. |
| Hardlink appears while running (an O7 builder links our files) | The link count is re-checked through `fstat` on the cached handle before every modifying call, so the next write is redirected to a private copy that includes the earlier unsynced pages. |
| Lagging rev file | It is synced by the barrier even when not current (deviation 1, test `sync_covers_every_written_file_and_the_directory`). This is a real improvement on Core. |
| Any write, allocation, truncation or sync error | Followed by `Failed` on every mutating call, and a failed `fdatasync` is never retried (tests `an_io_failure_stops_all_writes`, `a_failed_data_sync_is_fatal_and_not_retried`). Prune selection still mutates in-memory infos after a failure, but nothing can reach B1 except through the refusing barrier, provided m1 is fixed. |
| Remaining holes | m3 (handle eviction) and m1 (the `state()` footgun). |

**`FlatFs` seam for ST3a.** It is adequate: it is path-based, and every write, size change, data sync and directory sync goes through it. `allocate` is advisory and returns `()`, which is Core's semantics, so ST3a injects space failures through `available_space` and write errors through `write_all_at`. Two notes for ST3a:
- `copy` is one call that creates a new unsynced file;
- `len()` decides whether `flush` creates a file.

## 5. CoreBytes (DEC-30 N3) and the CS0 swap

**What exists today.** The placeholders (`bytes.rs`) have the same representation and accessors as CS0's `dash_consensus::CoreBytes` on `node/cs0-node-primitives` (`serialize.rs:272-305`):
- `Arc<[u8]>`;
- `as_slice`, `len`, `is_empty`, `AsRef<[u8]>`;
- `Debug` that prints only the length.

The difference is construction:
- CS0 has no raw constructor at all, only `core_bytes(&value)` and `Decoded::core_bytes()`, and `compile_fail` doctests forbid forging one.
- ST2's only constructor is `from_raw` behind the test-only `raw-bytes` feature, which only `dash-coreimport`'s dev-dependency and the bench enable. With resolver 2 it does not leak into normal builds.

**Swap plan:**
1. Have `dashd-store` depend on `dash-consensus` (pure, with no I/O). This adds an edge to A1's crate table, where `dashd-store` lists only `rocksdb`, so the manager should approve it. The alternative is for CS0 to move `CoreBytes` into a leaf crate. Then `pub use dash_consensus::CoreBytes` and delete the placeholder.
2. Unit tests and the bench: build blocks of a chosen size as real CS0 `Block`s and use `core_bytes(&block)`; for example, one coinbase with a padded `OP_RETURN` output. Alternatively, make `save_block_inner` `pub(crate)` for the in-crate tests, which already pass `&[u8]`.
3. Parity tests in `dash-coreimport`: `decode_block(&mut slice)?.core_bytes()` on the bytes read from Core's files. This is also an extra check, because CS0's canonical comparison has to agree with Core's files.
4. Keep `CoreUndoBytes` (and `raw-bytes`, for undo only) until CS5a ships the `CBlockUndo` codec, then swap the same way.
5. The CH5 `-reindex` path must pass the scanner's exact block slice, or `read_block` → `decode_block` → `core_bytes()`, to `save_block(known_pos)`. Never pass the whole record: Core's known-position `nSize` uses `GetSerializeSize(block)`, not the size field, and an oversized record would otherwise inflate `info.size`. CS0's type makes the wrong way impossible, but the CH5 brief should say it.

**Read path on the oracles.** I ran `read_core_datadir` on the live testnet oracle (read-only `OsFs`, release build, 904 s under load). Across 28 files (4.30 GB), all 1,513,363 blocks read back byte-equal and decoded. All 1,513,348 undo records passed Core's checksum, and every one was matched to a block of its file (`unmatched_undo: 0`). There are 15 fewer undo records than blocks: the genesis, plus blocks that are stored but not connected. I did not re-run mainnet; its 16 GB is still filling.

## 6. The author's findings

1. **Core aborts on a damaged `-loadblock` under `-checkblockindex`.** Confirmed at both pins.
   - `AcceptBlock` → `CheckBlockIndex()` (v23 `validation.cpp:4405`; v24 `ChainstateManager::CheckBlockIndex`, `:5618-5619`). With no active chain it asserts `m_block_index.size() <= 1`.
   - On a fresh datadir the genesis is in the index (`LoadGenesisBlock`) but not activated until the file's genesis record is scanned. So any import whose first accepted record is not the genesis aborts the node: a damaged, reordered or genesis-less bootstrap file.
   - `-checkblockindex` is `DEBUG_ONLY` and defaults on only on regtest (`fDefaultConsistencyChecks`), and the input is a local file. It is a debug-assert crash, not a security issue.
   - **For the program:** the harness is right to pass `-checkblockindex=0`. If rdashd ports `CheckBlockIndex` (CH3b/CH5), it should keep the early return without reproducing an abort on `-loadblock` input. Record that as a declared deviation (policy B, like WAL-45) in the CH5 brief. Core's own `feature_loadblock` puts the genesis first and never hits it.
2. **The v23.1.8 `-fastprune` hang.** Confirmed: `while (nSize + nAddSize >= 0x10000)` never ends when `nAddSize >= 64 KiB`, and it grows `m_blockfile_info` every iteration until OOM.
   - O17 already decided it (WAL-45, `v24-all`).
   - ST2's handling contradicts that decision: see **M1**.
   - The driver correctly stops the v23 dashd replay before the big block.

## 7. The code-review skill (high effort) as input

The skill ran in a forked context and reported 10 items, none of them a parity break. I verified each:
- adopted as m8, m5, n2, n5 (both script items) and n6;
- partly adopted: the performance note (an extra `fstat` per write) is real but small. Cache the link-count verdict per handle if the criterion bench shows it.
- the bash-3.2 empty-array note does not apply on agentbox.
- the "hardcoded MaxBlockSize" item is wrong (§2).
- the duplication note is n8.

Disclosure: the skill read Core sources under `/work/bench/src`. That was read-only, and nothing ran there.

## 8. Verdict

**CHANGES-REQUIRED**: fix M1 (WAL-45), which takes a few lines. m1–m3 are cheap and should land in this PR; they are the I1/I5 hardening this task exists for. m4 is a manager/O17 action. m6, m7 and m9 can be recorded as CH5/CH2/CH3 prerequisites if the manager prefers. The port itself is faithful at both pins: every equivalence claim I checked holds, and every parity run I repeated, including 308 damaged files on fresh seeds, was byte-identical to dashd.
