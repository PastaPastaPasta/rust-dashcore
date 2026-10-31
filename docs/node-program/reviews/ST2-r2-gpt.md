Verdict: **CHANGES-REQUIRED**

# ST2 independent pre-merge review, round 2

Reviewed `node/st2-block-files` at **fca86fe6** (21 commits above the current `node/integration`) in a fresh detached worktree at `/work/scratch/st2-r2-gpt/repo`. Review date: 2026-10-08. No branch edits, commits, pushes, or GitHub messages. The new findings are two allocation failures in Rust; I found no ordinary block-file byte or index-position discrepancy in the tested cases.

Read REVIEW-COMMON, the ST2 PLAN/STATUS rows, DEC-41, WAL-45, CONS-24/28, BL-29, and ST2-r1-opus. Core citations below refer to `git show` at **v23.1.8** and **v24.0.0-rc.3**, not to the mutable Core checkout. The reference-oracles memory note was available; the named blockfile-parity-gotchas note was not found in the local memory copies. The r1 report supplied the relevant import/debug-assertion and pin gotchas.

## Numbered findings

### 1. Major — streaming scanner retains an unbounded history of an untrusted file

**Location:** `dash-coreimport/src/blockfiles/external.rs:350–359`, `:399`, `:435`, `:446` (in particular `report.headers.push(block_pos)` at **:435**).

`BufferedFile` now bounds its ring buffer to 4,000,000 bytes, and a decoded block is bounded by the record limit. However, `ScanReport` retains every header, every undecodable record, and every unexpected-data offset in three `Vec<u64>`s. Its memory therefore grows with the input's record count. `load_external_block_file` offers no production mode that disables or caps this collection. This contradicts the module's claim that a file of any size streams in bounded memory and leaves r1 m6 only partially closed.

**Concrete failure:** import a bootstrap stream consisting of repeated `magic | size=80 | header` records whose parent is unknown. The sink retains nothing and never receives a block. The scanner still retains one offset per record. I used a generated `Read` implementation with only an 88-byte record in memory:

```
records=100000  input=8800000  retained_headers=100000  capacity=131072  retained_bytes=1048576
records=1000000 input=88000000 retained_headers=1000000 capacity=1048576 retained_bytes=8388608
```

Those bytes are just the header-offset vector, in addition to the ring buffer. A much larger file eventually exhausts memory; malformed `New` records can grow all three vectors. Repetition of the same header does not bound this history. A successful scan returns and retains it all.

**Core at both pins:** v23 `src/validation.cpp:5115–5259`, especially `:5131–5137` and `:5177–5183`; v24 `src/validation.cpp:5426–5593`, especially `:5441–5447` and `:5482–5489`. The `-loadblock` scan logs/skips unknown-parent records without retaining their positions. Its unknown-parent multimap is conditional on the separate `-reindex` caller; there is no unconditional header/error-history vector. The input buffer is fixed at `2 * MaxBlockSize()` (`src/streams.h:634–650,673–680` in v23; `:617–633,656–663` in v24).

**Fix:** use counters and a fixed-size diagnostic sample in the production report, or send events to an optional caller-owned collector. Keep exhaustive position collection explicitly opt-in for differential tests. Add a generated-source regression that scans increasingly many skipped/malformed records while asserting a constant bound on retained diagnostic capacity. Do not truncate the scan itself to bound its report.

### 2. Major — forged record size can abort the process before a typed corruption error

**Location:** `dashd-store/src/flatfile/blocks.rs:709–720`, especially **:719** (`out.resize(len as usize, 0)`); reachable from `read_block` at :730 and `read_undo` at :743.

`read_sized` trusts the on-disk `u32` size once the declared range fits inside the file. File length is not a bound on a safe allocation: an attacker-controlled/sparse file can satisfy that check cheaply. `Vec::resize` then allocates and zeroes up to approximately 4 GiB, with no protocol bound, caller budget, or fallible reservation. This is a process abort rather than `FlatFileError::Corrupt`/`Io`. The documented choice to use framing size instead of deserializing does not justify an unchecked allocation.

**Concrete failure and both-pin differential:** I copied each private, valid boundary-test datadir, changed only the first blk record's size field to `0xffffffff`, and extended that blk file sparsely to `8 + 0xffffffff` bytes. The original genesis payload and Core index position remained intact. Both release binaries started and successfully returned the **306-byte** genesis through `getblock <genesis> 0`. Reading the identical position through `BlockFileReader::read_block(FlatFilePos::new(0, 8), ...)` under a 256 MiB address-space limit produced:

```
memory allocation of 4294967295 bytes failed
exit=134
```

The limit makes the failure deterministic and safe to reproduce; on an unrestricted process the same request attempts a multi-GiB allocation/zero-fill. The undo path shares this allocation before it checks its checksum.

**Core at both pins:** `ReadBlockFromDisk`, v23 `src/node/blockstorage.cpp:748–770`, v24 `:805–831`, opens at the indexed payload position and deserializes the block without reading the preceding size field. `UndoReadFromDisk`, v23 `:516–548`, v24 `:540–574`, likewise deserializes and verifies the consumed undo bytes rather than allocating from that framing size. The external scanner already rejects block record sizes above 2,000,000 at both pins (v23 `src/validation.cpp:5153–5155`; v24 `:5461–5463`).

**Fix:** reject block framing sizes above the scanner/protocol maximum before allocation. Give undo reads a justified protocol/resource bound or stream their decode/checksum; do not assume undo shares the block-size bound. Use checked conversions and fallible reservation and turn impossible/over-budget records into a typed error. Preserve valid oversized-framing records within the accepted block limit. Add sparse-file tests for both public read methods that assert a returned error without allocating the declared size.

## Closeout of every r1 finding

“Closed” addresses the particular r1 objection, not a blanket proof against every filesystem fault. “Partial/open” entries owned by later tasks are distinguished from the two ST2 merge blockers above.

| r1 ID | Status | Evidence at fca86fe6 / remaining action |
|---|---|---|
| M1: v23 fastprune growth | **Closed** | `blocks.rs:345–352` uses size + 1 under both releases, cites WAL-45, and no longer has `FastpruneBlockTooLarge`. The both-profile unit regression passes. My v24 fastprune replay includes the >64 KiB block; v23 Core replay stops before it as WAL-45 requires. Core v23 `blockstorage.cpp:629–641`; v24 `:671–689`. |
| m1: unsafe `state()` | **Closed** | Removed. `sync_and_state` (:189–199) and `DirtyFiles { infos, last_file }` returned after `sync_inner` (:201–212) provide the barrier. Inspection docs warn against persistence. `adopt_existing_files` (:215–228) marks blk/rev files and the directory; its regression passes. Core file/index ordering: v23 `validation.cpp:2850` onward; v24 `:3058` onward. |
| m2: missing empty rev file | **Closed** | `sync_inner` (:511–518) first calls `flush_block_file_inner(false, false)`. The empty-rev and current/lagging-pair ordering tests pass. Parity rebuilds now use the barrier, without explicit extra `flush_block_file` calls. Core v23 `blockstorage.cpp:557–570`; v24 `:585–607`; both `flatfile.cpp:71–95`. |
| m3: eviction of unsynced writable handles | **Closed** | `fs.rs:108–155` marks writable handles dirty before modification and excludes dirty handles from LRU eviction; :157–163 clears that flag only for the descriptor synced. Allocation/truncation/copy use dirty handles (:237–285). The >64 dirty-handle regression passes. This closes the LRU hazard; I did not inject a kernel writeback error. |
| m4: finalize profile has no O17 row | **Partial** | BL-29 records the manager/O17 erratum. `blocks.rs:46–60,360–363` and ARCHITECTURE :26–32 correctly retain the one finalize switch. O17 still lacks the requested row and still calls #27746 unreachable. Add the row/ReleaseProfile field before profile integration. CONS-28's other concern is closed: startup unlink is now ungated (:652–662). |
| m5: prune unlink errors fail-stop | **Closed** | `blocks.rs:634–649` logs/skips removal errors and still syncs the directory. Deviation 11 is explicit (ARCHITECTURE :75–76); the failure/startup-retry test passes under V23_1. Core v23 `blockstorage.cpp:584–592`; v24 `:621–633`; O17 CONS-28 allows the hygiene choice without a profile field. |
| m6: slice-only, uninterruptible scanner | **Partial** | It now takes `Read`, models short reads, uses a ring buffer, and checks `interrupted()` before every record (:368–383). Trickle, EOF, wrap, I/O-error, and interruption tests pass, as do 124 new-seed Core differential cases. Its **total** allocation remains unbounded because of finding 1. |
| m7: no reorg/prune/default-rollover evidence | **Closed** | Independently ran both new reorg/prune pairs and the rest of the suite. Reconnected original blocks reuse their undo; fork blocks have their own checksum-bound undo. Each prune removed 40 pairs and matched Core's RPC answer and surviving file set. Added real Core cases at exact 128 MiB and 64 KiB equality under both pins; all four match. |
| m8: pruning panics on short infos | **Closed** | `with_fs` (:153–159) resizes once through `last_file`; the inconsistent-state test passes. Core loading has the same sizing precondition; this is not evidence that arbitrary gigantic caller-supplied state is resource-bounded. |
| m9: reserve/write split | **Open, CH2/CH3 follow-up** | The API still reserves and writes in one `&mut self` call (:272–280); docs specify one writer (:127–131). No CH3 brief recording the integration decision was found. r1 explicitly allowed this Core-shaped ST2 API; record validation-thread versus db-writer ownership before CH2/CH3 integration. |
| n1: private copy lacks space check | **Closed** | `seq.rs:133–138` checks available space against the 50 MiB floor plus copy length; deviation 12 is recorded. |
| n2: link count zero | **Closed** | `fs.rs:203–215` discards an unlinked cached inode and rechecks the path; `seq.rs:120–125` treats zero/missing as creation. |
| n3: ancestors above datadir not synced | **Closed by explicit ownership** | ARCHITECTURE :78–79 assigns ancestors above blocks/ and its parent to ND1. The first parent sync remains in `seq.rs:239–248`. ND1 must implement that responsibility. |
| n4: release spelling | **Closed** | `Release::V23_1` at `blocks.rs:57`, consistently used by callers. |
| n5: generation guard/resume history | **Closed** | `gen-blockfile-chains.py:97–104` removes the nonexistent RPC field; :128–131,178–183,236–237 retain every resume height. I used the revised driver successfully. |
| n6: incomplete-record header omitted from report | **Closed** | `external.rs:435` records it before `skip_to`; the torn-tail test passes. This observation should remain available through bounded/opt-in reporting when finding 1 is fixed. |
| n7: standalone property fuzz target vs DEC-30 N5 | **Open, program fuzz follow-up** | `fuzz/fuzz_targets/dash_coreimport/external_scan.rs:1–4,55–69` still compares Rust scanner/cursor invariants, not Core. No CS0/O3-runner registration or exemption was found. Daily/integration differential tests do not close the runner requirement. Register it or record the intended exception/owner. |
| n8: intentional duplicate tx stream decoder | **Closed** | `external.rs:13–15` explains why stream-consumption parity requires the duplication. |

## Independent validation and scope of the parity result

All test datadirs lived below `/dev/shm/st2-r2-gpt-tmp` (mode 700); scratch source, binaries, logs, and harnesses are below `/work/scratch/st2-r2-gpt`. No `/work/chains` or `/work/bench` access was needed. Initial `uptime` load was 45.81/44.18/42.66 on 32 vCPUs and later fell to approximately 30. Builds used two jobs. Differential scanner execution used four internal workers and one test at a time; a scratch copy changed the original small test's hardcoded eight workers to four. Case generation ran at most two dashds concurrently. None of these adjustments touched the review worktree.

Release binary SHA-256 values match r1's release artifacts:

- v23.1.8: `7bdb2ce35e2be3e0e388eb9bfb99b2d742d07c813b93b970069d42043875aa36`
- v24.0.0-rc.3: `e204e3163185b1bffcd3bf3532c0957d8cc11019eaf8f3dbc3b2fc82a7ab5bb4`

The branch's oracle-shim pins are `350ce242a59c19782dc9eebc8e23dead0a2e16ee` and `23769258c239e69282569e9f9e219ce9e2c4ee2e`; the byte comparisons used the actual release binaries above, not the shared live oracles or the setup script's default v23.1.0 binary.

| Check | Independent result / evidence |
|---|---|
| Targeted build/tests | `cargo test -p dashd-store --no-default-features --features raw-bytes -p dash-coreimport -- --test-threads=4` passes, including 33 store and 25 importer unit tests; `tests.log`. Environment-dependent tests in this initial command were subsequently exercised explicitly. |
| Static checks | Targeted all-target Clippy with `-D warnings`, and workspace `cargo fmt --all -- --check`, pass; `clippy.log`, `fmt.log`. No full workspace/rocksdb rebuild was needed for this isolated flat-file review. |
| Existing 40,000-block dataset | All 40,001 records and both files match; `base-parity.log`. |
| Full parity driver | **16/16**: dataset + freshly generated 4,000-block transaction-bearing base + fourteen pin/flag/restart/import/reorg/prune cases. `parity.log`, `cases.log`, `generate.log`. This reruns every case shape, not the author's full 40,000-block replay in every shape. |
| Exact rollover boundaries, my cases | **4/4** additional real-Core cases. The default chains have 244 blocks with 950,000-byte records and an adjusted final record making the old size plus the next record **exactly 134,217,728**; fastprune chains have 9 blocks and equality at **65,536**. Both pins roll at equality. File bytes, finalized lengths, and preallocated current-file tails match. `mine_boundaries.py`, `boundaries.log`, `boundary-parity.log`. |
| Core block index, independently decoded | A scratch harness recorded returned writer positions; `index_check.py` used LevelDB's C API on **copies of stopped private indexes** and decoded Core `CDiskBlockIndex`. **56,549 nFile/nDataPos checks and 56,532 nUndoPos/checksum checks match**, across the fifteen generated driver cases and four boundary cases. Pruned entries have no data/undo position to compare. `index-check.log`; Core `chain.h:387–410` (v23), `:384–407` (v24). |
| Scanner differential | **124/124** fresh-seed cases: 120 damaged 401-block inputs and four damaged 40,001-block inputs near ring-fill boundaries, split across both release pins. `scanner.log`. This is additional evidence, not a claim to have rerun all 660 author cases. |
| Torn writes and restart, my cases | **6/6**: prefix-write failure in a blk header, blk payload, and rev record under each release. Retain the previous barrier's state, drop the failed writer/cache, reopen through a fresh OsFs, and retry. Returned positions and complete files equal the uninterrupted control. `harness/tests/torn_restart.rs`, `torn-restart.log`. This is operation-level fault injection with real files, **not** a power-loss VM or actual SIGKILL test; ST3's unsynced-page loss/writeback-error model remains separate. |
| Hostile-source allocation | Generated repeated unknown-parent records prove finding 1 without allocating the input; `harness/src/main.rs` (`scan`). Sparse-file read abort and both-pin intact-genesis success prove finding 2; `forged_size_core.py`, `forged-size-core.log`, `forged-size-rust.log`. |

The normal writer is a faithful port in the examined paths: rollover uses `>=`, preallocation uses the correct ceil-to-chunk arithmetic, positions point past the 8-byte header, undo adds 40 bytes and hashes the parent's raw hash followed by undo serialization, and the pin finalize switch/reset/max-height update match Core. Rollover flush occurs before switching the active file. Current-pair flush precedes syncing lagging files and exporting index state; unlink follows the caller's durable index boundary. Reconnection does not call `write_undo` again for an index entry already carrying `BLOCK_HAVE_UNDO` (a caller obligation clearly documented at `blocks.rs:400–403`), matching Core v23 `blockstorage.cpp:718–747` and v24 `:773–802`.

Typed I/O/corruption errors and fail-stop behavior fit the store's patterns. I found no data-driven index/slice panic in the external scan with its fixed buffer parameters and checked limits. The remaining abort/resource defects are the unchecked reader allocation and unbounded report. Framing-size corruption can deliberately yield a stricter read than Core (ARCHITECTURE deviation 6); that should return a bounded typed error. It must not terminate the process as demonstrated here.

Private dashds were stopped and the review TMPDIR was removed after evidence capture. The detached Rust review worktree remains unchanged for inspection; scratch harness modifications are outside it.

**Final verdict: CHANGES-REQUIRED.** Fix findings 1 and 2 before merging ST2. Preserve the passing parity cases and close the bounded-memory regressions; BL-29, writer ownership, and the fuzz-runner entry remain explicit program follow-ups.
