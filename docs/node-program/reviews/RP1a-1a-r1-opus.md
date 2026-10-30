# RP1a-1a review: round 1 (Opus-h)

Branch `node/rp1a-1`, head c0eb7ac3c on base bbfa09004. STATE class (DEC-92). Core reference v24.0.0-rc.4.
Sol's r1 dropped with no findings, so this is round 1 in full. Sol follows on the fixed head.

**Verdict: CHANGES-REQUIRED.** One major (M-1), three minors and four nits. The RPC replies themselves are in very good shape: 70 of the 72 extra edge cases I added match dashd rc.4 byte for byte, and the 2 that differ come from one cause, a Core overflow artefact (n-3). The major is in the new coins-snapshot path, D11. It bypasses the copy-point schedule the flush design already provides for this RPC (E-b1.8). The fix is small.

## What I ran

All runs were in reflink copies under `/work/scratch/rp1a-1a-rv-opus/` (since removed), with `CARGO_BUILD_JOBS=12` and `TMPDIR=/dev/shm/rp1a-1a-rv-opus-tmp`.

| Run | Result |
|---|---|
| `cargo test -p dashd-rpc` | 102 passed, RC 0 |
| `cargo test -p dashd-node --features test-support -- getblock the_chain_commands` | 3 passed, RC 0 |
| `cargo test -p dashd-chainstate --lib -- stats_job` | 2 passed |
| `cargo test -p dash-consensus --lib -- utxo_set_scan` | 1 passed |
| DIFF-RPC `chain_commands_match_dashd_v24` vs dashd rc.4 (cached by setup-dashd) | ok |
| Review probe `review_edge_cases_match_dashd_v24`: the branch's 102 cases, then 72 extra edge cases on the resulting state | 70 SAME, 2 DIFF (n-3) |
| 12 env-gated mutants (one build, `RVMUT=n`) | 10 killed by unit tests; M2 survives everything; M3 survives the unit tiers and is killed only by DIFF-RPC |

The extra edge cases included:
- `getblockhash` at tip+1, -1 and 2^31.
- `getblock` with verbosity 2^31, -2^31-1, -2^31, 1.5 and null; an uppercase hash; a numeric hash; no arguments.
- `getblockheader` in uppercase, hex for a header-only entry, and verbose=null.
- `getchaintips` with `[1]`, `[0]`, `[-1]`, `[2,1]`, `[null,3]`, `["x"]`, `[2^31]` and `[1.5]`, including invalid and headers-only branches.
- `submitheader` with:
  - a high-hash header and a time-too-old header;
  - a duplicate of an invalidated header, and a header on an invalid parent (bad-prevblk);
  - trailing bytes, odd-length hex, an empty string, and a 79-byte header.
- `invalidateblock` and `reconsiderblock` on a header-only entry and on genesis, and `reconsiderblock` with `ignore_chainlocks` set to true and to "x".
- `preciousblock` on a header-only entry and on an unknown hash.
- `gettxoutsetinfo` with `[null,null,false]`, `["muhash",null,true]`, a numeric `hash_type`, a string `hash_or_height`, `hash_serialized_2` with a height, and an empty type.
- `setmocktime` with 1.5, `[]`, null, the maximum, the maximum + 1 and 0; `getblockheader`, `submitheader`, `getchaintips` and `gettxoutsetinfo` while at the maximum.
- `mockscheduler` with -1, 1, 1.5 and `[]`.
- Extra arguments to `syncwithvalidationinterfacequeue` and `getbestblockhash`.

Results:

| Mutant | What it changes | Killed by |
|---|---|---|
| M1 | drop the `IsBlockPruned` check | `getblock_refuses_a_pruned_block_before_its_read` |
| M2 | `IsBlockPruned` without `nTx > 0` | **survives** (n-2) |
| M3 | skip the ALWAYS flush before the snapshot | **survives the unit tests** (m-2); DIFF-RPC kills it (10/102 differ, every `gettxoutsetinfo`) |
| M4 | don't wait for the snapshot ack | `the_scan_reads_the_snapshot_taken_at_its_start` |
| M5 | ignore the interruption point | `the_scan_stops_when_interrupted…` |
| M6 | confirmations off by one | 3 dashd-node tests |
| M7 | branch filter `<` becomes `<=` | `getchaintips_filters_by_branch_length…` |
| M8 | `setmocktime` lower bound 1 | `setmocktime_is_regtest_only_and_range_checked` |
| M9 | `mockscheduler` accepts 0 | `mockscheduler_forwards_…` |
| M10 | no `ReadBlockFromDisk` log line | `getblock_reads_the_slot_s_own_block_files` |
| M11 | count a transaction per coin | `a_utxo_set_scan_counts_transactions_by_txid_runs` |
| M17 | verbosity `false` becomes 1 | `getblock_is_cores_block_to_json…` |

Reading from the recent cache, one of the mutants the brief suggested, can't be written: the RPC slot's `Writer` holds only its own `NodeBlockFiles` and has no handle on NodeChain's `recent` LRU. That makes DEC-606's rule hold by construction. Its test also proves the cache would have served the block that the RPC refuses.

## Findings

### M-1 (major): `coins_stats` bypasses E-b1.8's copy point, so the walk can pin the flush's memtables and stall W (and V) for the whole scan

**Where:** `dashd-chainstate/src/flush/site.rs:292-301`.

`NodeFlushSite::coins_stats` calls `self.flush(FlushStateMode::Always, …)`, which reaches `FlushControl::execute(…, copy_point = false)` (control.rs:426-434). It then starts the scan thread, which takes its snapshot after V has seen the release.

`FlushControl::copy_point` (control.rs:436-463) exists for exactly this case. It is documented as "An RPC copy point (§2.6): an ALWAYS flush, then a snapshot … so the snapshot … pins none of the memtables Bk filled (E-b1.8)". It differs from a plain ALWAYS flush in two ways:
- It submits with `copy_point = true`, so `FlushMachine::after_bk_first` (machine.rs:1011) orders the tail as `AfterBk → Release`, not `Release → AfterBk`. For the RocksDB tuning, `AfterBk` is `flush_memtables` (tuning.rs:216).
- It asserts that `snapshot.epoch() == released_epoch`.

It has no production caller today; `gettxoutsetinfo` is the caller it was written for.

**Failure scenario (mainnet scale):**
1. A large ALWAYS flush fills memtables (charged to the block cache by a `WriteBufferManager` with `allow_stall = true`; dashd-store/src/rocks.rs:50-53, :206).
2. W releases and only then runs `flush_memtables`.
3. In the meantime V has started the scan thread. The thread takes its snapshot and builds the cursor iterator (`table.cursor(snap)` in stats_job.rs), most likely while W's memtable flush is still in progress. The iterator holds the SuperVersion current at that moment, so the memtables Bk filled stay alive for the whole walk, which takes minutes on mainnet.
4. RocksDB frees a memtable's WBM charge only in `~MemTable` (librocksdb-sys 11.8.1, db/memtable.cc:271). `ShouldStall()` compares the total `memory_usage()` with `buffer_size` (include/rocksdb/write_buffer_manager.h:126-141).
5. So the next flush whose writes take the charge to the budget is refused (`no_slowdown`) and retried (DEC-164, rocks.rs:365) until the walk ends. V waits with it at its next ALWAYS or critical flush, both of which wait for the release (policy.rs:251).

The brief asks whether the walk can block V; through this path it can. The copy point exists to prevent exactly this.

**Correctness of the stats is not affected:** see "Checked and fine" below. This is a liveness and memory finding.

**Fix:**
- Give `FlushControl` a copy-point flush that does what `copy_point` does but returns at the release instead of taking the snapshot, for example `copy_point_flush(cx) -> released_epoch`. It runs `at_site(Always)` and then `execute(…, true)`.
- Route `NodeFlushSite::coins_stats` through it.
- Have the scan thread send `snap.epoch()` with its ack, and have V `assert_eq!` it against the released epoch, as `copy_point` does.
- Add a test, with the recording tuning in `flush/machine/tests.rs` or a RocksDB engine, showing the hook runs before the snapshot. `a_copy_point_runs_the_hook_before_the_release` is the model.

Once this lands, the "pins none of the memtables" claim in D11 and stats_job.rs holds.

### m-1 (minor): an interrupted scan answers -32603 "Unable to read UTXO set"; Core answers -9 "Shutting down"

**Where:** `dashd-node/src/rpc.rs:464-476`; seam `dashd-rpc/src/blockchain.rs` `txout_set_info -> Option<…>`.

Core's `rpc_interruption_point` is `RpcInterruptionPoint()`, which throws `JSONRPCError(RPC_CLIENT_NOT_CONNECTED, "Shutting down")` (rpc/server.cpp:343-346) from inside `ComputeUTXOStats`. rdashd folds `CoinsStatsError::Interrupted` into `None`, which the handler turns into `RPC_INTERNAL_ERROR "Unable to read UTXO set"`.

Core's worker finishes and writes its reply during `StopHTTPServer`, so a client that called `gettxoutsetinfo` just before `stop` sees -9 from dashd and -32603 from rdashd. `RpcServer::interruption_point()` (server.rs:299-306) already produces Core's error.

**Fix:** make the seam return `Result<TxOutSetInfo, TxOutSetError { Unreadable, Interrupted }>`, and map `Interrupted` to `RpcError::new(RPC_CLIENT_NOT_CONNECTED, "Shutting down")`.

### m-2 (minor, tests): no in-tree test runs the real flush+snapshot path; the skip-the-flush mutant survives every unit tier

**Where:** `dashd-node/src/chain/tests.rs:1073-1075` uses a no-database flush site, so `gettxoutsetinfo` there only checks the unreadable reply. `coins/tests/stats_job.rs` starts from a hand-written database.

With M3 (no ALWAYS flush before the snapshot), dashd-rpc, dashd-node, dashd-chainstate and dash-consensus all pass. Only `chain_commands_match_dashd_v24` catches it (10/102 replies differ), and only where dashd is installed.

D11's central claim is that one class M turn gives a flushed tip and its snapshot. That deserves a dashd-free test:
- Use a `NodeFlushSite` over a `MemoryEngine` or RocksDB (the sut harness or the CH2 control-test fixtures).
- Connect blocks, leaving the cache dirty, then call `ChainHandle::coins_stats`.
- Assert that `best_block == tip` and that the stats equal a scan of the coins after the flush.
- Then connect one more block during the walk and check the result does not move.

This test is also where M-1's hook-ordering check belongs.

### m-3 (minor): the detached scan thread releases the snapshot and the engine only after sending the result

**Where:** `dashd-chainstate/src/coins/stats_job.rs:76-80`.

The closure runs `let snap = db.snapshot(); taken_tx.send(()); result_tx.send(scan(…));`, and only then drops `snap` and the captured `Arc<GuardedEngine>`. So the RPC worker can return, and shutdown can go on, while a detached thread still holds a RocksDB snapshot and possibly the engine's last `Arc`.

Two consequences:
- RocksDB can end up closing on that detached thread, racing process exit. This is the class of BL-366's "RocksDB closed at exit" abort.
- For ND1's BL-271 shape B (DEC-613), "the RPC worker answered" does not yet mean "nobody else holds the engine".

The window is microseconds, because interruption ends the walk at the next coin. It is free to close.

**Fix:** compute the result, `drop(snap); drop(db);`, and only then `result_tx.send(r)`. Optionally keep the `JoinHandle` in `CoinsStatsJob` and join it in `wait`.

With that change, shutdown is sound:
- The RPC server stops running before V stops, so the walk ends at the next coin.
- The worker's `wait` returns.
- The thread never waits on V (LO-9), so neither VExit's handback nor ND1's forced flushes can deadlock with it.
- A concurrent snapshot reader does not conflict with ND1's ALWAYS flushes.

### n-1 (nit): `getblock`'s position is not gated on `HAVE_DATA`, and the test checks only a prefix of the null position

**Where:** `dashd-node/src/rpc.rs:395-398` builds `FlatFilePos { file: e.file.unwrap_or(-1), pos: e.data_pos }` from the raw entry. Core's `GetBlockPos()` returns the null position unless `BLOCK_HAVE_DATA` is set, and `ChainReader::have_data_pos` already does the same. There is no reachable difference today: a pruned entry returns `Pruned` first, and a header-only entry has no file.

Separately, `getblock_reads_the_slot_s_own_block_files` asserts only `starts_with("…FlatFilePos(nFile=-1")`. A non-zero `nPos` would pass where Core logs `FlatFilePos(nFile=-1, nPos=0)`.

**Fix:** use `e.status.has(HAVE_DATA).then(…)`, or `have_data_pos`, and assert the whole line.

### n-2 (nit, tests): mutant M2 survives

**Where:** `dashd-node/src/rpc.rs:395`.

Dropping `IsBlockPruned`'s `nTx > 0` term passes every test. On a pruned node, `getblock` of a header-only entry must give -1 "Block not found on disk" with the open-failed line, not "pruned data".

**Fix:** add that case to `getblock_refuses_a_pruned_block_before_its_read`.

### n-3 (nit, record it): at `setmocktime 9223372036` dashd rejects every header as `time-too-new` and rdashd does not

These are the probe's only 2 DIFFs: `x_submitheader_fresh_at_max`, plus the extra tip it leaves in `x_getchaintips_at_max`.

Core's `ContextualCheckBlockHeader` computes `NodeClock::now() + 2h` in int64 nanoseconds. At the maximum mocktime, 9,223,372,036 × 10⁹ + 7.2 × 10¹² overflows and wraps, so every header is "too new". rdashd's seconds arithmetic doesn't overflow.

I don't recommend emulating signed overflow. Record it in `ALLOWED-DIFFS` or `quirks.toml` as a Core overflow artefact at the range bound. No rdashd panic or overflow showed up at the maximum: `getblockheader`, `getchaintips` and `gettxoutsetinfo` there were SAME.

### n-4 (nit, D-row): a fatal ALWAYS flush inside `gettxoutsetinfo`

Core's `ForceFlushStateToDisk` logs "failed to flush state" and still scans the database. rdashd's V stops on the `Fatal`, so the reply disconnects and becomes -32603 "Unable to read UTXO set". This only happens on the AbortNode path, like D9. Note it in D11.

## Checked and fine

- **D11 consistency vs Core.** Core flushes, then reads `GetBestBlock()` under one `cs_main` and builds its cursor (an implicit LevelDB snapshot) later with no lock, so Core can report a hash that a later flush has already moved past. rdashd flushes and takes its snapshot in one V turn, reading `meta.tip` from the same snapshot. That is one of Core's schedules, so nothing is reachable that Core's `GetUTXOStats` could not produce.
- **ALWAYS is synchronous.** With a job in flight, `decide` returns `WaitThenFreeze` for ALWAYS (policy.rs:189-197). After the wait, the result is either `IndexOnly{signal:true}` (`clean_at_durable` requires `durable == tip`, no dirty coins, and synced locators; then `wait_release`) or `Freeze{wait_done:true}`. After the release, W writes no coins and no meta.
- **LO-9.** The scan thread never waits on V, and V waits only for the snapshot ack. `CoinsStats` follows `Flush`'s pattern: `with_flush`, then `scope_end`, then the reply.
- **Stats math** against `kernel/coinstats.cpp:101-203`:
  - `transactions` counts runs of the same txid, as Core's `prevkey` grouping does.
  - `bogosize` is 50 plus the script length.
  - `total_amount` uses `CheckedAdd` semantics, and a `None` total gives CHECK_NONFATAL's -1.
  - `hash_serialized_2` takes PrepareHash's best block; its internal order relies on VARINT(n) keys sorting bytewise in numeric order, which holds. `muhash` is order-free.
  - The field order is Core's.
  - DIFF-RPC agrees at genesis, a5, a3, c6 and the final tip in all three hash types; only `disk_size` is masked (ST1, D11).
- **gettxoutsetinfo arguments.** These are Core's errors in Core's order; moving the `hash_or_height` refusal before the flush (D10) is not observable.
- **getblock per DEC-606.**
  - The lookup, the header facts, `IsBlockPruned` (Core's blockstorage.cpp:443-447) and the position are read in one V query; the read happens outside it.
  - The reader's order is Core's: open, deserialize, PoW, then the hash check, each with its own line.
  - The three errors carry Core's codes and texts.
- **The interim refusal is consistent with DEC-611.** Verbosity ≥ 2 (any value, including 2^31-1) gives -32603. At verbosity 1 a coinbase with a payload is refused too. Verbosity ≤ 0 returns the hex, and bool verbosity maps as in Core.
- **submitheader, setmocktime, mockscheduler, getchaintips and blockheaderToJSON** match rc.4 in argument handling, texts, codes and order (see the probe list above).
- **setmocktime.** It is set inside a V class R turn when V runs, which is Core's `cs_main` scope (D6), and set directly before the load or after a stop. Off a mockable chain it refuses with Core's text (unit test). It doesn't overflow at the maximum.
- **mockscheduler.** It forwards in `[1, 3600]` and then runs `SyncWithValidationInterfaceQueue`, as Core does.
- **DIFF-RPC is not normalised beyond the `Date` header and `disk_size`.** Content-Length is masked only on replies that carry `disk_size`.
