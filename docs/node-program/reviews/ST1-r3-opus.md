# ST1 review, round 3 (Opus, independent): `dashd-store` + storage-engine bake-off

- **Branch:** rust-dashcore `node/st1-store-bakeoff`, 20 commits on 5a96adf8 (93b4ddb5..c54eccf4), worktree `/work/home/agent/workspace/rdc-wt/st1-store-bakeoff`
- **Inputs:** PLAN ST1 row and the §8.1 delta, A1 §2.8 and §3.5, `docs/node-program/designs/ST1-bakeoff.md` (v4 plus results), Core v23.1.8 and v24.0.0-rc.3
- **Method:** I read all of `dashd-store`, `dash-coreimport` and the fuzz target in full, and the bake-off harness (`store.rs`, `micro.rs` D4, `run.py` statistics) where it bears on the conclusions. I checked them against Core and against the RocksDB 11.8.1 sources vendored in `librocksdb-sys-0.19.0`. I used the `code-review` skill at high effort as one input. Its findings are folded in below, each verified or downgraded. I ran three probes of my own against the branch's `dashd-store` from a scratch crate (`/work/scratch/st1-review/probe`); the branch itself was not modified.

## What I ran

| Check | Result |
|---|---|
| `cargo test -p dashd-store -p dash-coreimport --all-features` (TMPDIR=/work/scratch/st1-review/tmp, CARGO_BUILD_JOBS=12, private target dir) | 35 tests pass, 0 fail. `dashd-store` takes 114 s, mostly the RocksDB `model_check` with fsync on a busy disk |
| `cargo clippy -p dashd-store -p dash-coreimport --all-features --all-targets -- -D warnings`, `cargo fmt --check` | clean |
| `cargo check -p dash-fuzz --bin dashd_store_kv_model` | compiles (not fuzzed) |
| **Probe A, ENOSPC then free space, no reopen** (`st1-probe enospc` on a 200 MiB tmpfs with a 120 MiB ballast; default `RocksConfig`) | Fails with `NoSpace` while appending an SST. Retrying immediately after deleting the ballast is refused. **A Durable write is accepted without reopen 6.07 s later.** The LOG shows `flush_reason: "Error Recovery"`, then `Successfully resumed DB`. With only 49 MiB freed, the write is still refused after 30 s: the SST file manager wants 64 MiB (`required disk buffer [67108864 bytes]`) |
| **Probe B, empty Durable batch** (strace of fsync/fdatasync) | `write(empty, Durable)` issues one `fdatasync` on the WAL, and so does `sync_wal()`. The trait's "equivalent to an empty Durable batch" holds |
| **Probe C, refused open of a foreign DB** (a RocksDB with data in `default`; listing, sizes and mtimes taken before and after) | refused with `UnknownDatabase`; no file created or modified. The read-only inspection is in fact read-only |

## Findings

### Major

**M1. Fail-stop isn't enforced: RocksDB resumes writes on its own after `NoSpace`, and D4's "refused until reopened" comes from retrying too early.**
- **Where:**
  - `dashd-store/src/rocks.rs:440-455`: `write`, `sync_wal` and `flush_memtables` have no error latch.
  - `dashd-store/src/engine.rs:73-78`: the trait doc says "RocksDB enters read-only mode on a background error".
  - `docs/node-program/designs/ST1-bakeoff.md:288,292`: §9.5 says "refused (sticky background error)" and "the engine refuses further writes until reopened".
  - `dashd-store-bakeoff/src/micro.rs:222-223`: the retry runs immediately after `remove_file(ballast)`.
- **Mechanism, in RocksDB 11.8.1:**
  - A `NoSpace` from a WAL write, a flush or a MANIFEST write maps to `kHardError` (`db/error_handler.cc` `ErrorSeverityMap`).
  - With one DB path, `DB::Open` calls `EnableAutoRecovery()` (`db_impl_open.cc:2694-2697`).
  - `HandleKnownErrors` → `RecoverFromNoSpace()` → `SstFileManagerImpl::StartErrorRecovery`. That starts a thread which re-checks free space every 5 s and calls `RecoverFromBGError()` once free space ≥ `reserved_disk_buffer_`, which is the sum of write-buffer reservations: 64 MiB with the default config.
  - Recovery flushes the memtables and resumes writes. It does not depend on `max_bgerror_resume_count`.
  - Probe A shows this end to end.
  - D4 never saw it for two reasons. It retried within the first 5-second poll. And on the 512 MiB image, the 64 MiB ballast may not have cleared the 64 MiB reserve.
- **Failure scenario:**
  - W's flush gets `NoSpace` on sub-batch B2, and the batch was not applied. A1 §3.5.6 says abort. But shutdown, or another writer (§3.5.7 commits the non-transactional effects as their own Buffered batches from other threads), runs more than 5 s later after the operator or the OS frees space. Those writes are accepted, so `chainstate/` gets commits after the failed one: a hole in the commit sequence, which is exactly what I5 forbids.
  - It also happens without any space being freed, once RocksDB's own obsolete-file deletion frees enough.
  - Today I5 holds only if every caller on every thread is perfect. For a base that all STATE code sits on, the engine should enforce it. A1's "after which RocksDB may go read-only" was the premise, and it is false.
- **Fix:**
  1. Latch in `RocksEngine`: an `AtomicBool`, or the first `StoreError` kept in a `OnceLock`, set by any error from `write`, `sync_wal` or `flush_memtables`. While it is set, `write`, `sync_wal` and `flush_memtables` return `ErrorKind::ReadOnly` (keep the original kind in the message) until the engine is reopened. Do the same in `MemoryEngine` and ST3a's `FaultyEngine`, so models see the same contract.
  2. Make the trait doc normative: "after any error the engine refuses every later write until reopened".
  3. Add a test. One portable way: make the DB directory read-only, then call `flush_memtables`; SST creation fails, and the next `write` must return `ReadOnly`. Add an `#[ignore]` tmpfs/loop ENOSPC test that waits more than 10 s before retrying.
  4. Correct §9.5's D4 rows and conclusion, and make D4 wait at least 10 s with at least 64 MiB free before it retries.

**M2. The RocksDB-vs-LevelDB open items are routed to a gate that doesn't test them, and OI-1's magnitude is a scale artifact. CH2 needs a corrected brief.**
- **Where:** `ST1-bakeoff.md:26,236-246` (§1 and §9.4).
- **The gate gap:**
  - §9.4 ends: "CH2's bench guard against dashd (`reindex-chainstate`) is where they pass or fail."
  - PLAN CH2's BG is "`reindex_time_s`, `peak_rss_mib` **not worse than the previous head**". That is not dashd.
  - The dashd gates that exist (M3 exit 2, CH4) run node-bench at the default config. There, Core lends the empty mempool's 300 MiB to the coins cache (§3.4's own round-2 caveat, ρ ≈ 1.1). So the regime where OI-1 was found (ρ ≈ 0.5) is not exercised by any planned gate.
  - That regime is not exotic. It is a synced node with a full mempool (no slack), or any `-maxmempool`-reduced or low-memory host. Block-connect latency at the tip is where it would show.
  - As written, OI-1 can't fail anything and will drift.
- **The magnitude:**
  - "3.7× slower" is the median of 5 pairs whose ratios range 0.17–0.61 on a disk at 60–99 % utilisation, where each extra read is inflated by contention. The bound that holds in every pair is about 1.6× (worst pair 0.61).
  - The cell also sits at a cache cliff created by the protocol. §3.4/§5 scale the coins budget and the page-cache allowance to the prefix, but keep the engine budget E literal (24 MiB block cache plus 2 × 16 MiB memtables). So E is about 75 % of the 75 MiB database here, against roughly 10 % at full-chain scale.
  - LevelDB leaves its E/2 block cache empty (mmapped tables), so its effective page cache is about 72 MiB, about the whole DB (it reads 35–227 MiB). RocksDB's anonymous block cache duplicates pages it also holds in page cache, so its unique cache falls short of the DB (410–791 MiB read).
  - The mechanism (double caching under a total-memory constraint) is real and does scale. The 3.7× does not.
- **What CH2 should take from it:**
  1. Keep the engine. The mechanism is configuration, and `rocksdb-bc8` already recovers part of it (0.60).
  2. Size the RocksDB side like Core does: memtable ≈ `nCoinDBCache/4`. Then choose between direct I/O with a larger block cache (no double caching) and a small block cache that relies on page cache. Count page cache in A1 §3.4's split.
  3. Re-measure at W-MF with E scaled consistently, or uncapped.
  4. Gate it against dashd. Add a node-bench cell such as `reindex-chainstate` (or steady-state connect) at `-dbcache=300 -maxmempool=<min>` and/or under a memory cgroup, to CH2's or CH4's BG, compared with dashd at the same flags.
- **Fix:**
  - Correct §9.4's last line: name the actual gates and state the uncovered regime.
  - Restate OI-1 as "≥1.6× in every pair; magnitude inflated by unscaled E and disk contention".
  - Add the E-scaling caveat to §3.4.
  - Ask the manager for the PLAN delta, or record the accepted risk in DECISIONS.

### Minor

**m1. The blk scan resyncs after a record that runs past EOF; Core stops there. Commit e4f9b8f9 claims the opposite.**
- **Where:** `dash-coreimport/src/blockfiles.rs:97-112`; the comment at :100; the test at :376-383.
- **Core's behaviour:**
  - In `LoadExternalBlockFile` (v23.1.8 `validation.cpp:5115ff`, v24.0.0-rc.3 `:5426ff`), once a header is located (magic plus a size in 80..=`MaxBlockSize()`), `blkdat >> header` or `SkipTo(nBlockPos + nSize)` reaches EOF.
  - `Fill()` then throws, the exception is caught, and `eof()` (`m_read_pos == nSrcPos && feof`) ends the `while` loop.
  - Core never rewinds to magic+1 after a size that runs past EOF.
  - Within the file, Core skips the whole claimed span, and Rust matches that (`pos = end`).
- **The divergence:** for an in-range size that reaches EOF, Rust indexes later records that Core's `-reindex` would not load, and it clears `torn_tail`.
- **Impact:** harmless for the trace, since the extra records form a superset. But the function is documented as "Core's `-reindex` scanning semantics", and PLAN ST2 owns "scanner parity on corrupted inputs", so a test now encodes the wrong rule.
- **Fix:** restore `torn_tail = true; break` for blk scanning, and fix the comment and test. For rev files, see m2.
- The code-review skill independently flagged the same thing.

**m2. Duplicate copies cover undo only. Block bytes are always read from the first copy, with no merkle check, and an overlapping garbage record can hide valid rev records.**
- **Where:** `trace.rs:391` reads `loc`, the first copy; `blockfiles.rs:165-170`.
- **How duplicates arise in practice:** after a crash, Core rewrites at the reverted `nSize`/`nUndoSize` from the last flushed file info. That leaves a torn or overwritten older copy, often first in file order, whose 80-byte header still hashes to the canonical block.
- **Failures:**
  - `read_block` fails on the first copy, or parses garbage. Extraction then aborts with "no matching undo record", or, if the outputs are corrupt, builds a trace that only the final count check might catch. It never tries the intact duplicate.
  - In rev files (which Core never scans), the leftover header of an old record whose claimed span covers newer records makes `scan_records` jump over them.
- **Fix:**
  - Iterate `index.copies(hash)` for the block bytes too, and accept a copy only if it parses and its merkle root matches the header.
  - For rev files, consider candidates at every magic offset (each is checksum-gated anyway) instead of skipping spans.
- **Impact on ST1's numbers:** none. Both traces had 0 resync bytes and 0 stale undo, and the live set matches `gettxoutsetinfo` exactly.

**m3. A non-empty directory without a valid RocksDB is always `UnknownDatabase`. This bricks the datadir after a failed first creation and hides corruption.**
- **Where:** `rocks.rs:109-118`.
- **Failure scenarios:**
  - RocksDB creates `LOG` and `LOCK` before `CURRENT`. A first open that dies (ENOSPC, kill, bad option) leaves debris, and every later start says "holds data but no schema record".
  - A damaged `CURRENT`/`MANIFEST` on a real chainstate, or an EIO, gets the same misleading error instead of `Corruption`/`Io`.
  - A dedicated filesystem mounted at `chainstate/` (`lost+found`) is refused.
- **Fix:**
  - If `CURRENT` exists, surface the RocksDB error with its kind.
  - If only RocksDB creation debris is present (`LOG*`, `LOCK`, `IDENTITY`, `OPTIONS*`, `MANIFEST-*` with no `CURRENT`), treat the directory as uncreated.
  - Decide explicitly about `lost+found`.

**m4. The schema record holds only a version, not the database name.**
- **Where:** `schema.rs:98-134`, `rocks.rs:194-233`.
- **Failure:** a different database whose column families are a subset of this schema's, at a version not newer, is accepted. `create_missing_column_families` then adds this schema's CFs to it and reports `Current`.
- Today's schemas probably don't collide, but `meta` plus one common CF name is enough.
- **Fix:** stamp `schema.name` next to the version and check both.

**m5. `size_bytes()` is not "that memory".**
- **Where:** `engine.rs:56-57`, `rocks.rs:316-318`.
- **The problem:** `WriteBatch::size_in_bytes` is the length of the `std::string` rep, but the allocation doubles as it grows.
  - A sub-batch cut right after it passes `-dbbatchsize` (16 MiB) has typically just reallocated to about 32 MiB, with a transient 16 + 16 MiB realloc peak.
  - D3's 1.25–1.46 anonymous bytes per logical byte is consistent with this.
  - CH2's budget invariant, asserted with a counting allocator, will see capacity, not `size_bytes`.
- **Fix:** create batches with `WriteBatch::with_capacity_bytes(dbbatchsize + slack)`, available in `rocksdb` 0.25, or document `size_bytes` as a lower bound and expose capacity. The double-copy fix itself is correct: `write_opt` passes `batch.inner` straight to `rocksdb_write`, with no second encoding.

**m6. The `kv_model` fuzz target and the tests are thin where it matters.**
- **Where:** `fuzz/fuzz_targets/dashd_store/kv_model.rs`, `dashd-store/src/model.rs:393-509`.
- **Gaps:**
  1. The power-loss half is tautological. The model and `MemoryEngine` implement the same rule ("durable state plus a prefix of buffered batches"), so it checks the implementation of one against the other and says nothing about RocksDB. (The module doc admits this. Its phrase "checks barrier placement" is inaccurate: there is no client code under test.)
  2. The RocksDB half has no Reopen op. WAL replay, schema re-check and "Buffered survives a process crash" are never fuzzed; only the unit test `buffered_batches_survive_reopen` covers them.
  3. There is one process-global DB per worker. Tombstones and SSTs from earlier inputs make failures history-dependent and hard to reproduce from a saved input. `dashd_store_kv_model_<pid>` directories leak in `$TMPDIR`, which on agentbox is a quota tmpfs.
  4. No test anywhere exercises an engine error path (ENOSPC, EIO, fail-stop); see M1.
  5. `refuses_a_foreign_database_without_writing` compares only `list_cf`, not the file set. Probe C shows the behaviour is right, so assert it.
- **Fix:**
  - Add `Op::Reopen` for RocksDB.
  - Open a fresh DB per input in a per-input temp directory that is removed afterwards, or use a cheap `DestroyDB`.
  - Add the latch test from M1.

**m7. The statistics are labelled more strongly than the data supports.**
- **Where:** `run.py:298-301`; every "95 % CI" in §9.
- **The problem:** with 5 pairs, the 2.5/97.5 percentiles of the bootstrap median are exactly the minimum and maximum. I reproduced this: for 0.17–0.61 the bootstrap gives 0.17/0.61. With 2–3 pairs, the same holds.
  - That is an order-statistic interval for the median with 93.75 % coverage, assuming independent pairs. The pairs aren't independent under drift on a saturated box.
  - "Interval excludes 1" therefore means "all pairs on one side": a sign test with p = 0.0625 two-sided at n = 5.
- **Fix:** label these as ranges, or as sign-test results. The decision is unaffected (see the verdict).

**m8. Open items the protocol's own §1 rule requires are missing.**
- **The rule:** §1 says any loss to LevelDB beyond the spread is listed in §9.
- **RssAnon:** RocksDB loses on RssAnon in every cell, with non-overlapping ranges: P300 228–265 vs 189–202 MiB, P4096 337–348 vs 296–322, wt-p4096 221–222 vs 179–187. A1 §3.8 explains this (anonymous block cache vs mmap, and total RSS is lower for RocksDB), but it should still be an explicit row with that explanation, because `peak_rss_mib`/`idle_rss_mib` are charter metrics.
- **Flush-stall p99:** RocksDB's is worse at P300 (12.5 vs 2.7 s) and in the uncapped cell (40 vs 16 s). The ranges overlap, so the rule arguably doesn't fire. Still, flush stall is the metric A1 asked for, and the likely cause, memtable count/size write stalls during a 16 MiB-sub-batch flush with 2 × 16 MiB memtables, is a configuration lead for CH2.
- **Fix:** add the rows, and record RocksDB stall counters (`rocksdb.cfstats` stall micros, `is-write-stopped`) in `extra()`.

**m9. `wait_for_compactions` can hang.**
- **Where:** `rocks.rs:169-190`.
- **Failure:** after a background error, `compaction-pending` or `mem-table-flush-pending` can stay above 0 forever, and the loop never checks for an error or a deadline. It is public API.
- **Fix:** add a deadline, and check `rocksdb.background-errors`.

**m10. `MemoryEngine::write(Durable)` applies the batch and runs the barrier under two separate lock acquisitions.**
- **Where:** `memory.rs:180-190`.
- **Failure:** a concurrent `simulate_power_loss` in between drops a batch whose `write(Durable)` then returns `Ok`.
- This is test-only, but ST3a's FaultyEngine builds on this engine and its concurrent harnesses would see spurious "durable batch lost" failures.
- **Fix:** do the barrier under the same write guard.

### Nits

- **n1.** `engine_error` (`rocks.rs:65-84`) maps EDQUOT ("Disk quota exceeded") to `Io`, not `NoSpace`. agentbox's `/tmp` quota produces exactly that. Decide whether quota counts as disk full for the A1 §3.5.6 messages.
- **n2.** `RevCursors::file` adds `resync_bytes` again on every reload after LRU eviction, and rescans the whole file (`blockfiles.rs:278-302`). Park the scan result, not just the cursor.
- **n3.** Allocations are sized from untrusted compact sizes up to `MAX_SIZE` (`tx.rs:156,168,225`, `coin.rs:142,145`), which can mean GB-scale reservations from a corrupt record whose header hashes correctly. Cap the reservation at remaining bytes divided by the minimum element size. (The code-review skill flagged this; downgraded because overcommit makes it virtual, and rev records are checksum-gated.)
- **n4.** `compress_amount` (`coin.rs:32-48`) panics in debug builds on overflow for corrupt values, where Core's `uint64_t` arithmetic wraps. Use wrapping operations.
- **n5.** The VARINT vectors cite a develop commit (`codec.rs` tests). They are identical at both pinned tags (I checked `serialize_tests.cpp` `varints_bitpatterns` at v23.1.8 and v24.0.0-rc.3). Cite the tags.
- **n6.** For the manager: A1 is now stale on two points.
  - §2.8 says "10-bit whole-key bloom filters … L0 pinning", but partitioned index and filters are now the default.
  - §3.5.1 says "ST1's engine does not set it [`atomic_flush`] yet".
  - Also, `atomic_flush` combined with the shared `db_write_buffer_size` flushes every CF together each time any CF fills. With five or more chainstate CFs, that produces many tiny L0 files in `meta`/`block_files`. CH2's multi-CF workload should measure it.
- **n7.** `fuzz` is a workspace member and now pulls RocksDB, with `bindgen-runtime` and libclang, into every workspace-wide build or clippy run.
- **n8.** `inspect_existing` duplicates `check_schema`'s version decode and policy (`rocks.rs:204-227` vs `schema.rs:100-119`). Factor it into one helper.
- **n9.** A leaked `RocksBatch` keeps the DB open, holding `LOCK` and skipping the WAL sync on close (`rocks.rs:295-301`; documented). A `debug_assert` on engine drop that no batch is outstanding would catch it in tests.

## The specific questions

**1. Engine trait contract.**
- **Atomic batches across CFs:** yes. One `WriteBatch` becomes one WAL record, and `atomic_flush = true` is set (`rocks.rs:130`), so no CF can be flushed ahead of the recovered WAL prefix.
- **Durable is durable:**
  - `sync = true` syncs every not-yet-synced live WAL, not only the current one, and fsyncs the WAL directory when a new log was created.
  - Probe B confirmed an `fdatasync` for an empty Durable batch and for `sync_wal`.
  - Buffered writes reach the OS before return, since `manual_wal_flush` is off and the log writer flushes after each record. They therefore survive a process crash.
  - Point-in-time recovery with checksummed records gives a prefix.
- **Read-only inspection:** genuinely read-only (Probe C).
- **Refusing a non-empty non-DB directory:** works; see m3 for its edge cases.
- **Fail-stop:** EIO on the WAL write/sync path is `kFatalError` under `paranoid_checks` and stays sticky. **ENOSPC is not sticky (M1).**

**2. RocksBatch and the power-loss model.**
- The zero-copy fix is real; note the capacity caveat in m5.
- `MemoryEngine`'s model ("state at the last barrier plus a prefix of later Buffered batches"; `flush_memtables` as a barrier) matches RocksDB with WAL, `atomic_flush` and point-in-time recovery.
  - A flush syncs the SSTs and the MANIFEST before old WALs become obsolete, and RocksDB calls `SyncClosedWals` before a flush, on both the regular path with more than one CF and the atomic-flush path (`db_impl_compaction_flush.cc:260,633`).
  - Where the model differs, it is stricter. For example, it doesn't model RocksDB syncing the WAL on a clean close. So a test that passes on it won't lose data on RocksDB for the reasons it models.
- What it can't do is model errors. It never fails, while RocksDB's post-error behaviour is the auto-resume of M1. Code tested only on `MemoryEngine` can therefore pass while violating I5 on RocksDB, until M1's latch is part of both engines' contract and ST3a's FaultyEngine injects errors against it.

**3. blk/rev readers vs Core.**
- **What matches Core:**
  - The magic resync: mismatch, or a size outside 80..=2,000,000 → magic+1. `MaxBlockSize()` is 2,000,000 at both tags.
  - Stopping at the zero tail is equivalent to Core's `FindByte`-to-EOF.
  - Skipping the claimed span inside a file matches.
  - The undo record layout, `SHA256d(prev ‖ CBlockUndo)` (`UndoWriteToDisk`), and the `TxInUndoFormatter` dummy VARINT when the height is non-zero.
  - `ReadVarInt`'s overflow checks, with Core's `unsigned int` widths for the code, script size and dummy, and `uint64_t` for the amount.
  - Script compression, including `IsFullyValid` for uncompressed P2PK.
  - The v24 stable-txid hash for asset unlocks (`transaction.cpp:82-88`).
  - First-copy-wins duplicate handling matches Core's reindex (`HAVE_DATA` check).
- **What diverges:** a record that runs past EOF (m1), and block bytes from damaged first copies (m2).
- **The undo pairing by forward cursor is sound under Core's reorg behaviour.** `WriteUndoDataForBlock` writes undo only when `GetUndoPos().IsNull()`, so a reconnected block reuses its first record, which lies earlier in the file.

**4. Bake-off method.**
- **"Keep RocksDB" is justified, and robustly so.**
  - MDBX must win on time and disk and memory in both decision cells. It fails disk by 4.3–6.7× (file) and 3.6–3.9× (live pages).
  - Disk is a deterministic metric, unaffected by the saturated box. The decision therefore does not depend on the noisy time ratios at all.
  - At P300, MDBX's time pairs straddle 1 (0.68–1.63); at P4096, every pair is ≥ 1.11.
- **Paired ratios on this box:** they partially cancel drift, but each paired "CI" is just the range (m7). Conclusions that rest on wall time alone should be read as sign tests. The CPU-per-event and bytes-read metrics are the trustworthy ones, and they agree with the directions claimed.
- **OI-1 for CH2:** see M2. Treat 3.7× as an artifact of scale and contention, with a real mechanism (double caching, plus an E split that doesn't count page cache). The every-pair bound is about 1.6×. The never-slower-than-dashd rule needs a gate in this regime, because none of the planned gates exercises it.
- **Partitioned filters as the default: justified.** On the stable metric, CPU per event drops to 0.43× and miss p99 from 307 to 37 µs with no disk reads. The mechanism is credible: with a 24 MiB cache, an evicted whole filter or index block of a large SST is re-read and re-checksummed for a single lookup. This is also standard RocksDB guidance at full-chain scale. The evidence is thin (2 uncapped pairs at 75 MiB, and no capped `nopf` arm in the decision cells), so re-check it in the W-MF rerun. Record the A1 §2.8 deviation (n6).

**5. Fuzz and tests:** see m6. The unit coverage of codecs, scan edge cases and schema refusal is good. The durability half of `kv_model` checks the model against itself, and nothing tests error paths.

## Verdict

**CHANGES-REQUIRED.**

The engine is carefully built, and "keep RocksDB" is sound. Two majors block merge:
- **M1:** add the engine-level fail-stop latch with a test, and correct D4 and §9.5.
- **M2:** correct §9.4's routing and OI-1's framing, and ask the manager for the low-memory dashd gate.

m1 and m3 are small and should be fixed in the same pass. The rest can be follow-ups tracked in ST2, ST3a or CH2.
