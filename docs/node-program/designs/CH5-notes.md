# CH5 implementation notes

Decisions taken while implementing CH5 (`/work/scratch/ch5/PLAN.md` rev3; E4-1 ruled by DEC-981,
DEC-981a and DEC-981b). Each row settles a point the plan leaves open, or records where the node or
its tests depart from Core and why. Core lines are at **v24.0.0-rc.5**: `v:N` = `src/validation.cpp`,
`cs:N` = `src/node/chainstate.cpp`, `rb:N` = `src/rpc/blockchain.cpp`, `coins:N` = `src/coins.cpp`,
`stm:N` = `src/evo/specialtxman.cpp`, `log:N` = `src/logging.cpp`.

## CH5-1a: `verifychain` levels 3–4 (non-evo) and the startup check

| ID | Decision | Reason |
|---|---|---|
| D1 | **VerifyDB's disconnects enter the block's BLS scheme (H1).** `SerialCore::disconnect` runs H1 for every disconnect, not only `DisconnectTip`'s: the VerifyDB walk (`tip` false) enters the scheme of the block it undoes. Core's walk runs under VerifyDB's `ScopedBLSLegacyScheme` (`v:5055`), the tip's scheme, which `UndoSpecialTxsInBlock` makes legacy for the undo of a block without V19 (`stm:1065-1088`); the scheme in force for the undo is the block's either way. The log lines differ across V19: Core logs one `UndoSpecialTxsInBlock -- bls_legacy_scheme=1` and its revert, the node its per-attempt enter and revert, the revert at the disconnect's end, after its lines, the catch's included (CH3a D191). Regtest has V19 at height 1, so no test sees it. | Without a scheme, H9 refused the undo ("UndoSpecialTxsInBlock before H1") and a startup VerifyDB aborted the node. The lines are CH5-1b's (BL-524). |
| D2 | **VerifyDB skips level 3 at every G1 block (BL-440, DEC-993).** The walk's disconnect meets the readiness refusal for a block a G1 deployment is active for (DIP0003, V20, MN_RR, V24, EVO_SHARES; `g1_rule`), before any evo state is read, whatever the hooks, with the text `VerifyDB's evo checks (CH5-1b) for <rule>` (ALLOWED-DIFFS D18's format). VerifyDB takes it as Core's own outcome, `SKIPPED_L3_CHECKS` (`v:5119-5136`, `:5181-5183`), with the cause kept (`L3Skip::NotReady`, against a memory skip's `L3Skip::Dbcache`). The first skip of either cause ends level 3 for the walk, so every deeper block skips it too, non-evo blocks below an evo tip included, and the scratch view stays at the block it was not disconnected over; levels 0 to 2 check every block in the depth, and level 4 does not run. Only walks with no evo block (tips below DIP0003) match dashd's level 3 and 4. `verifychain` returns false (`rb:1440`). A default startup proceeds and init's reset runs at level 3 (`cs:441-465`); explicit `-checkblocks` or `-checklevel` (`require_full_verification`) fail the load with the refusal text as an `InitError` (exit 1), where a memory skip keeps Core's "Insufficient dbcache for block verification". dashd verifies all of it. A readiness refusal a level-4 reconnect meets, of any cause, ends the same way; while the walk's gate keeps evo blocks out of level 4 that path is defensive, and a seam test pins it. PLAN CH5-1a calls the gap a known one, not an allowed diff. | The production hooks are complete, so without the gate the scratch disconnect ran the full H9 undo: positive evo verification and its effects, which are CH5-1b's (R10). Core's skip outcome keeps a default start running over an evo tip (DEC-993, option C), where the refusal stopped it. |
| D3 | **The evo skip's reason is logged.** VerifyDB logs `Skipped verification of level >=3: <refusal>` where Core logs its memory skip's line (`v:5144-5146`), which stays for memory skips only. A walk logs one of the two: the first skip ends level 3. The line is the node's own. | Core verifies there and its help says "If false, check debug.log for reason"; without the line the log does not say why. |
| D4 | **The scratch layer's parent read panics on a held or poisoned tip cell.** On the `verifychain` path every read below the scratch layer is the tip's fetching read (`coins:44-65`), under `try_lock`; a held or poisoned cell is a panic, where the connector's `lock()` gives a `Fatal`. Every locker of the cell runs on V's thread, and the guards `VerifyScratch` takes are temporaries, so no production path holds it during a scratch read. | A wiring error, not a state the node can reach (CH5-1a review). |
| D5 | **The two nodes' figures.** `UpdateTip`'s `cache=` is the active layer's allocated bytes and entry count (`FlushSite::cache_usage`), while the level-3 gate measures native `Usage::total()` with the scratch layer charged as an open overlay (DEC-981a), which adds the unbacked shard ceilings, prefetch, held-out undo coins and the flush reserve. The regtest `UpdateTip` ratio band is withdrawn (DEC-1005): the nodes are compared by DEC-1005's crossing bands, the first tip height at which each node's full gate figure (the tip plus an empty scratch cache, what a VerifyDB starting there compares at its first block) exceeds its coins share, over a sweep of `-dbcache` budgets per window, recorded in the node program's ALLOWED-DIFFS row AD-CH5-1. Neither node's `UpdateTip` `cache=` is that figure: Core's logs only `CoinsTip()`, to 0.1 MiB, and its gate adds the scratch cache (validation.cpp rc.5 :5119–5122), 262,240 bytes empty (its first 256 KiB pool chunk, the chunk list node and one bucket). The conformance tool's L2 replay (`--usage-log`, its budget the node's: D14) records rdashd's tip term and its full gate figure, `CoinsCache::verify_usage` with an empty scratch (40,960 bytes more; `the_usage_log_records_the_coins_after_each_step`), for a blocks-directory replay with no flush. **rdashd's crossing is defined over its cold window only (DEC-1027):** startup, and a running node before its first coins freeze. The replay never freezes, but the node does, so its window ends at its first coins freeze from any cause: a THRESHOLD or CRITICAL boundary, or a forced, periodic, IBD-exit, prune or evo flush (`flush/policy.rs`). On the coin side the manifest's `first_freeze_height` marks it: the first THRESHOLD or CRITICAL boundary (`CoinsCache::boundary` reports CRITICAL where both hold), checked at every block (`a_boundary_past_critical_at_once_is_the_first_freeze`). In every recorded run that first freeze is the first THRESHOLD, which the log's `boundary_state` column and the manifest's `first_threshold_height`, checked the same way, mark (`the_first_threshold_is_checked_at_every_block`); a node whose first freeze has another cause leaves its cold window there. Past the first freeze the figure follows flush timing, and no crossing band is given. With F's charge F and the active's charge a, it is a + spare + prefetch + held-out + P after the release, and F + a + R + the same while F is in flight. a is at least A(F), the ceiling of `presize(B, F)` = min(f·(B − R − B/16), max(B − R − B/16 − F, 256 buckets a shard)), which `freeze` sizes and `release` keeps; it grows past A(F) while F is in flight, where THRESHOLD is not checked; spare is 40,960 to 302,592 bytes after any block, prefetch at most B/16. Its lower end is 1,281,536 bytes (the smallest ceiling, 1,199,616, with the smallest spare and P). It has no upper bound in B: F can exceed B, since CRITICAL is seen only at the boundary after the block that crossed it, and a grows while F is in flight. So it straddles S at every `-dbcache` from 4 to 2,000, with and without `-txindex`, at 5 and 300 MB. Pinned by `below_the_threshold_level_3_runs_after_a_large_block_s_freeze_and_release` and `above_the_threshold_level_3_skips_while_the_first_freeze_is_in_flight`. | The two figures can differ, most on a cold cache; the gate's boundary is `total()`'s. |
| D6 | **The process DIFFs run against the tests' rc.4 pin, with fixtures proved on rc.5.** `git diff v24.0.0-rc.4 v24.0.0-rc.5` touches none of VerifyDB, `DisconnectBlock`, `ConnectBlock` or `VerifyLoadedChainstate`; the init change between them is the pruned-datadir warning. | The pin is the integration tests' (`contrib/setup-dashd.py`). |
| D7 | **The L3 fixture's damage on rdashd** is the same coin rewritten in its coins column family through rdc's coin codec (Core's coin serialization); dashd's is a coins database Core's own code wrote (`tests/data/verifychain/PROVENANCE.md`). | rdashd's store is not Core's LevelDB. |
| D8 | **`-dbcache=4` is below rdashd's level-3 threshold (D14).** rdashd's cold figure there is 3,002,656 bytes (1,461,248 backed and 1,500,448 unbacked shard ceilings, and the scratch layer's first 40,960) against a coins share of 1,605,632, so it skips level 3 before the first disconnect; dashd's full gate figure (its tip, logged as 0.3 MiB, plus the empty scratch's 262,240 bytes) does not: both skip with the same results, but rdashd from the first block and dashd after the tip. Exact lines are compared at `-dbcache=12`, above the threshold, where both skip from the second block. | DEC-981: DIFF cases compare exact lines only where both nodes skip at the same block. |
| D9 | **The VerifyDB process tests run under one `-mocktime`**, a day past the fixture tip, so both nodes are in IBD on any date (without it the state turned 6 h after the fixture was made). The VerifyDB line parser cuts Core's ` (mocktime: N)` stamp (`log:401-404`). | A date-dependent IBD state changes the flush points the parent-fill test reads. |
| D10 | **The parent-fill proof compares coin counts**, the next block's `UpdateTip` entry count on both nodes after each walk, not which coins the tip holds. Its walks run at level 3, depth 6 and the default `-dbcache`, outside the band, with no flush (DEC-981b). The resubmission half (`txn-already-known`) waits for rdashd's mempool (MP1). | Neither node exposes its cache's membership. |
| D11 | **`UpdateTip`'s count label.** Core's `(%utxo)` format prints `(<n>txo)`, and so does rdashd since BL-725; the tests' parser reads only that. | Predated CH5; BL-725 (CH3) fixed the label. |
| D12 | **The abortnode restart compares without the undo read's cause lines.** After an `AbortNode`, dashd's shutdown flush recreates an empty `rev00000.dat` (`FlatFileSeq::Flush` opens it for writing), so its restart logs an end-of-file deserialize error; rdashd leaves the file absent and logs `Unable to open file …` and `OpenUndoFile failed`. VerifyDB's lines, the result and the init text match. | A block-files flush gap, its own follow-up. |
| D13 | **Init's failure-flag reset at Core's scope (BL-119, DEC-992).** At `-checklevel` 3 or above, after VerifyDB, `ResetBlockFailureFlags(nullptr)` (`cs:463-465`, `v:4163-4242`) acts only when the best invalid block's ancestor at the tip's height is the tip. It clears the FAILED flags of that block, its descendants and its ancestors, keeps `BLOCK_CONFLICT_CHAINLOCK` (`ignore_chainlocks` false) and turns each cleared ancestor's `FAILED_CHILD` children into `FAILED_VALID` (`v:4219-4227`); SB4's index half does the same, so V clears no more than Core. Shutdown's flush stores it: a later start at level 2 keeps the reset state. Order: a loaded tip marked failed with no valid block of at least its work stops Core's `LoadChainTip` at its prune's assert (`v:3603`) before VerifyDB, and V at DEC-196 (1)'s `INVALID_TIP` refusal, at every level, so neither reaches the reset there; a tip a valid block ties is DEC-196 (2), which the reset can un-fail (CH1-node D39, D64). The process tests compare each clause with dashd; regtest ChainLocks need a quorum, so the kept conflict mark is pinned in V's tests. | DEC-992's scope; the `INVALID_TIP` text names every `-checklevel`. |
| D14 | **rdashd's level-3 threshold (DEC-1007).** At startup, with no coins cached, level 3 runs when C(x) + P ≤ S. S is the coins share from `-dbcache` and `-txindex` (`CacheSizes::calculate`); B = S + M, M being `-maxmempool` in bytes with an empty mempool; R = min(16 MiB, max(⌊S/8⌋, 64 KiB)) + 64 KiB, RocksDB's batch reserve at the default `-dbbatchsize`; x = ⌊(B − R − ⌊B/16⌋)/2⌋, the presize at f = 1/2; C(x) its 64 shard tables at their ceilings, backed and unbacked (`ShardSizes::for_bytes`, 73 bytes a bucket, whole 4 KiB pages from 64 KiB up, the 2,560-byte shard array); P = 40,960, the scratch layer's first tables. Frozen, prefetch, overlay, held-out and reserve are 0. rdashd's default `-blocksonly` sets M to 5 MB until P5 (DEC-322): the threshold is `-dbcache=10` with `-txindex` (Dash's default) and 9 without; with `-blocksonly=0` (300 MB) it is 281 and 246. Pinned by `coins::tests::scratch::the_node_s_level_3_gate_at_0_coins`, `chainstate::tests::the_replays_coins_budget_is_the_node_s` (the replay's `CoinsBudget::node` is the node's `coins_budget`), and against dashd by `just_below_the_level_3_threshold_rdashd_skips_from_the_first_block`, `just_above_the_level_3_threshold_both_nodes_verify`, `with_blocksonly_off_the_threshold_is_at_dbcache_281` and `an_explicit_checklevel_below_the_threshold_fails_rdashd_only`. **The threshold holds at startup and qualified in the cold window (DEC-1027):** before its first coins freeze a running node's figure is active's charge (C(x) plus long coins and any shard grown past its ceiling) plus the spare block overlay (40,960 to 302,592 bytes after any block), held-out undo coins, prefetch (at most B/16; none in an IBD from an empty database, which holds no coins before the first flush) and P, never below C(x) + P. So below the threshold rdashd skips level 3 at every tip before its first freeze; above it, it runs at startup, and before its first freeze it skips wherever those additions exceed S − C(x) − P: 41,056 bytes at `-dbcache=10`, less than one block's spare (`above_the_threshold_one_block_s_spare_overlay_can_skip_level_3_before_any_freeze`). After a freeze no threshold holds (D5): below it, a freeze of an F charged past its ceilings (at `-dbcache=9`, at least 218,749 bytes past C(x)) leaves a next active small enough for level 3 to run. | DEC-1007 (a)–(c); the threshold moves with `-maxmempool`, so P5's switch is caught. DEC-1027 (3) limits the threshold statement to the cold window. |
| D15 | **`-storageprofile=<livecopy\|production>` is an rdashd-only, regtest-only, debug-only option (BL-634, DEC-1070, OD-CH5-32).** Core v24 has no such option and refuses it as an invalid parameter (`src/util/system.cpp:292`). rdashd registers it as it registers `-consensusxcheck` (DEC-697 Q9; O9b-notes D1): `ALLOW_ANY \| DEBUG_ONLY` in `OptionsCategory::DebugTest`, so `-help` leaves it out and `-help-debug` lists it, in every build including `--release`. It is accepted on regtest only. On main, test and devnet any value, `production` included, is refused at init in Core's regtest-only wording (`storageprofile is not supported on <chain> chain.`, as `-acceptstalefeeestimates`, `init.cpp:1723-1725`), in rdashd's own refusal slot after Core's argument checks and before the chainstate is read or written. Unset, or `livecopy`, regtest keeps CH5-5's live profile; `production` gives it `(RocksTuning::default(), live_copy = false)`, the pair mainnet and testnet run, and logs `Chainstate storage profile: production (-storageprofile=production); a copy of the running data directory is not promised to reopen`. It touches no RPC, P2P, consensus or `getblockchaininfo` surface. It replaces the `RDASHD_TEST_HOOK_PROFILE` seam: test-hooks builds refuse the old variable and refuse the live-copy hooks together with the production profile. | dbcrash and ST3b's release power-loss campaign must cover the production profile on the shipped binary, and a test-hooks build would give evidence from a different binary. The rule for every rdashd-only startup option is DEC-1070 (2). |

## CH5-5: regtest live-copy contract (m-6)

The design of CH5-5, closed under OD-CH5-19, with the implementation's rulings (OD-CH5-20) applied. Core's copy of a running datadir (`dynamically_initialize_datadir`) is promised to reopen on rdashd regtest, inside a stated contract, and the tests prove it.

**Sources** (the cites below are at the design's base; the implementation follows the code where lines moved):
- rdc `node/integration` at **86078f190**; every rdc cite is `path:line` there.
- Dash Core `v24.0.0-rc.5` (`tf` = `test/functional/test_framework/test_framework.py`, `util` = `.../util.py`, `v` = `src/validation.cpp`).
- RocksDB **11.8.1** (`librocksdb-sys 0.19.0+11.8.1`, Cargo.lock:3126-3127; rust-rocksdb 0.25.0, :4012-4013). RocksDB paths are relative to the crate's `rocksdb/`.

**Scope:** rdashd → rdashd, regtest only. Mainnet, testnet and devnet tuning and write paths do not change (§6.7).

---

### CH5-5 1. Inventory: what integration already has

In short:
- **The plumbing:** `FlushConfig::live_copy` (`flush/machine.rs:53-55`); the copy-point order (`needs_after_bk` :1240-1242, `after_bk_first` :1247-1249, `tail` :1251-1271); `copy_point_flush` (`flush/control.rs:515-552`). The copy-point sites are site.rs:317-351, :353-371 and :373-399, and the shutdown flush is actor.rs:380-410.
- **The engine:** the barrier calls `flush_memtables` + `sync_wal` (`dash-kv/src/engine.rs:148-152`; `flush_all` at rocks.rs:766-783); `wait_idle` is at rocks.rs:661-673; the quiet profile and its test are at rocks.rs:1069-1070 and :2352-2395.
- **Today's tests prove ordering only, on the model engine:**
  - `flush/machine/tests.rs:980`, :1013-1047, :1620-1663;
  - `flush/control/tests.rs:2002`, :554;
  - `chain/tests/node_stores.rs:1034`, :1089, :1593-1608;
  - `rdashd/tests/dbcrash.rs:360-466`.
- **Not guaranteed today:**
  - the flag is off in production (`dashd-node/src/chainstate.rs:660`; the tuning selections at :257-258 and `node.rs:287-293`);
  - no test looks at files around a copy point;
  - the flush/purge gap (`db_impl_compaction_flush.cc:3179-3182`, :3221-3249 vs :3978-4017).
- **CH5-5 adds:**
  - the live profile;
  - the purge wait;
  - the no-op `after_bk`;
  - the measurement and its warning;
  - one selection function;
  - the test-hooks profile seam, control hooks and counters;
  - the oracle;
  - the tests.
- **CH5-5 does not duplicate:** the step-machine order, the copy-point epoch checks (site.rs:340-349, :364-369, :390-395), the ALWAYS shortcut (`flush/policy.rs:243-249`), or the quiet test (it extends it).

---

### CH5-5 2. The framework's copy flows (Core v24.0.0-rc.5)

Core's flows:
- **The one live flow** is `dynamically_initialize_datadir`: tf:627-650, called from tf:1642, with its copy point `gettxoutsetinfo` (tf:634).
  - It copies the whole datadir (tf:635).
  - It removes `wallets/` and `llmq/` (tf:637-638), and keeps only `chainstate`, `blocks`, `indexes` and `evodb` under `regtest/` (tf:640-642).
- `copy_datadir` (util:491-502), the chain cache (tf:1004-1021) and the `feature_init` backups copy stopped nodes.
- The seven callers are in M7 (`contrib/node/func-compat/phases.toml:94-104`).

**Assumptions:**
- **A1.** No RPC that writes chain state or flushes is sent to the copied node during the copy. This is a stated harness assumption. The §2.2 adapter issues only read-only RPCs, before and after `copytree`.
- **A2′.** In the interval the node performs no block- or undo-writing activation, no disconnect or reconnect, no pending import and no prune work. This includes ChainLock enforcement that was queued, timer-driven or tip-scheduled, and InstantSend conflict invalidation:
  - `record_chainlock` defers `ClEnforce{Recorded}` (`chain/locks.rs:176-221`);
  - enforcement runs as a composite (:271-273), also from the 5 s timer (`actor.rs:894-899`, posted by `cl-schdlr` since BL-626) and the tip schedule (`abc.rs:941` → locks.rs:261-269);
  - for a floor outside the active chain it calls `activate_best_chain(None)` (:341-346);
  - IS conflicts are a second source (`actor.rs:902-910`);
  - activation writes undo (`flatfile/blocks.rs:536-564`) and allocates space (:572-579) inside preallocated chunks (`flatfile/seq.rs:175-205`), so file length is **not** evidence.
- **A3.** One copy at a time.

#### 2.1 The M7 caller audit (A2′)

There are two establishing conditions:
- **E1:** no CL/IS activity can exist, and there is no fork.
- **E2:** CL/IS may be active, but node0's block tree has a single tip, and no conflicting transaction exists.

| Caller (add site) | Status | Basis |
|---|---|---|
| `feature_asset_locks.py:341` | E1 | `set_sporks` (:336) turns CL/IS off (:284-286); the adds come before any fork |
| `feature_llmq_evo.py:76` | E1 | No `mine_quorum` before :89 |
| `feature_llmq_singlenode.py:79-80` | E1 | The first actions |
| `p2p_platform_ban.py:42-44` | E1 | The first actions, on one node |
| `feature_llmq_signing.py:217` | E2 | After `mine_quorum`; no forks or conflicts |
| `feature_protx_version.py` | E1 at :91-102, E2 at :119-150 | No forks |
| `feature_llmq_chainlocks.py:246-250` | **Gated** | Earlier forks (:204, :210-242) and the CL spork toggled just before the add |

**Count: 6 established, 1 gated.**

**The gate, in counters.** All seven callers run in M7 on a **test-hooks** rdashd (func-compat `RDASHD_BIN` pointing at the hooks build) with the §2.2 adapter.
- **The six established callers:** every copy in their runs has a zero counter delta and unchanged super versions. Any non-zero delta fails the run, and the finding reopens the audit.
- **`feature_llmq_chainlocks`** stays gated (BACKLOG, owner CH5) until 10 consecutive runs at func-compat's default parallelism show a zero delta on every copy. Any non-zero delta keeps it gated and is filed against the caller, with the delta.

I5 shows that the counters catch the enforcement case (§6.5).

#### 2.2 The test-side tripwire (Q6: no production warning)

**The counters.** `dashd_store::livecopy::Counters` holds monotonic `AtomicU64`s. They exist only under `#[cfg(feature = "test-hooks")]`, with the increment calls `cfg`-gated at each site, so shipping builds have no counter and no call. The counters, and where each is incremented:

| Counter | Incremented in |
|---|---|
| `activations` | `ActivateBestChain`'s connect step, `chain/abc.rs:685` `connect_tip` |
| `disconnects` | Once, in the common `InHold::disconnect_tip` (`chain/abc.rs:753-789`). Every disconnect route goes through it: ABC (`abc.rs:616`), failure handling (`chain/failure.rs:42`), and ChainLock enforcement, whose `VHost::disconnect_tip` (`chain/locks.rs:432-436`) calls it and is **not** counted separately |
| `flatfile_mutations` | Every mutating `FlatSeq` call: `prepare_write`'s create (`flatfile/seq.rs:111-127`), `make_private` (:129-147), `write` (:149), `allocate` (:179), `flush` on its finalize/truncate branch only (:207-227), and `remove` (:284). Plain fsyncs (`sync_files` :238, `sync_dir` :251) are not counted |
| `undo_writes` | `BlockFiles::write_undo_inner` (`flatfile/blocks.rs:536`) |

**The snapshot, inside the copy-point barrier.**
- In step 11 of a copy point (mach:936-938, which runs before Release, mach:1247-1249), W invokes the snapshot closure **inside** `RocksEngine::live_barrier`'s exclusive section, after `sync_wal` and before the reset, before `live_barrier` returns, and before Release (§3.2 steps 5–8).
- The snapshot holds the four counters and the per-CF `rocksdb.current-super-version-number`s that end the barrier (`sv0`, §3.2).
- It is atomic with respect to every counted event:
  - V is parked in `copy_point_flush`, waiting for this job's release (control.rs:515-552). V is the only thread that activates or disconnects, and V's connector also writes undo (`connect/serial.rs:1201-1205`);
  - with V parked, W is the only executing flatfile-mutating path, and it is executing this step;
  - the exclusive engine lock excludes every RocksDB write, so no super version can move.
- The snapshot travels with the release to `coins_stats` (site.rs:317-351). The counter fields, every increment site, the reply field and the RPC registration below all carry `#[cfg(feature = "test-hooks")]`. In test-hooks builds, `gettxoutsetinfo`'s reply object gains `"livecopy_counters": {activations, disconnects, flatfile_mutations, undo_writes, super_versions: {<cf>: n}}`.

**The re-read.** `livecopycounters` is a read-only RPC registered only under test-hooks. It is absent from shipping builds and so from `help`. It returns the same object, live: the counters, plus the current super-version numbers read via `property_int_value_cf`.

**The adapter** is the Q5 func-compat adapter (§8 Q5), on `rust_nodes`.
- It wraps `dynamically_initialize_datadir`, calls `gettxoutsetinfo`, and keeps `livecopy_counters`.
- After `copytree` returns, it calls `livecopycounters`.
- **Any increase in any counter, or any changed super version, fails the test** with `live-copy tripwire: <field> <before> → <after>`.

This covers the whole interval from the barrier to the re-read, including the time before the reply and a disconnect + reconnect that returns to the same tip. On Core nodes the adapter does not check, since dashd has no counters; Core's LevelDB copy behaviour is V2.

---

### CH5-5 3. The contract

C1–C4 hold only on regtest's livecopy profile, the default. `-storageprofile=production` gives no live-copy promise (BL-634, D15): a production-profile regtest node keeps a write buffer manager, auto-compaction, the after-Bk flush and concurrent memtable writes, and has no barrier, no measurement and no C4 lines.

#### 3.1 C1–C4

**C1.** Take the interval on regtest from an explicit copy point's reply to the start of the next explicit flush point. In it, no file in `regtest/chainstate/` is created, renamed or deleted, provided that A1 holds and the interval's measurement (§3.2) stays **valid and ≤ B** for every CF. WAL and `LOG` appends are allowed; a WAL copied mid-append reopens as a prefix (rocks.rs:382, record checksums).

**C2.** In the same interval, under A2′, no file in `regtest/blocks/` is created, renamed or deleted, and no flatfile byte the copied index references changes.

**C3. At the copy point, before the reply:**
1. the ALWAYS flush;
2. `flush_memtables`;
3. **the purge wait:** `rocksdb_wait_for_compact(db.as_raw_ptr(), opts, &err)` (c.h:4644-4645, c.cc:13510-13514).
   - `opts` comes from `rocksdb_wait_for_compact_options_create` + `_set_wait_for_purge(opts, 1)` (c.h:5878, c.cc:11176-11179). `flush` and `close_db` stay 0.
   - A `Raw` guard destroys it, as in `flush_all` (rocks.rs:769-783). The raw form is needed because rust-rocksdb keeps its own options pointer inside the crate (`rocksdb-0.25.0/src/db_options.rs:4897-4898`).
   - It waits for `bg_flush_scheduled_`, `bg_purge_scheduled_`, the `unscheduled_*` counts and error recovery (`db_impl_compaction_flush.cc:5400-5450`). An `Err` latches;
4. `sync_wal`.

After step 4 there is **no outstanding file-mutating engine work**. `quiet` turns off even the stats dump, so the periodic tasks left (R12) append only to `LOG`.

`sync_wal`'s rationale (Q8):
- rows written before step 2 are persisted by the flush;
- the sync fences rows committed before it that sit in the new memtable (R17's `Buffered` rows);
- it does not cover a writer that commits after the sync.

**C4.** One warning per CF per interval:
- `live copy: column family <cf> memtable allocation <a> bytes (RocksDB's approximate arena figure) exceeds the bound <B>; <w> batch bytes were committed to it since the last copy point; a copy of the running datadir is not promised to reopen`;
- `live copy: column family <cf> switched or flushed a memtable since the last copy point (super version <s0> → <s1>); a copy of the running datadir is not promised to reopen`.

#### 3.2 B, its samples, and its serialization

**The quantity.** Per CF, `rocksdb.cur-size-active-mem-table` (`internal_stats.cc:1315-1321`) = `ApproximateMemoryUsageFast` (`memtable.h:611-615`), which returns the cache `approximate_memory_usage_`.

**The live profile's write-path settings** (D2-2):
- `allow_concurrent_memtable_write = false`, set with rust-rocksdb's `Options::set_allow_concurrent_memtable_write` (`rocksdb-0.25.0/src/db_options.rs:2150`; RocksDB default true, `options.h:1426-1434`);
- explicitly pinned at their defaults: `enable_pipelined_write = false` (:1398, via `set_enable_pipelined_write`, db_options.rs:2926), `unordered_write = false` (:1424, via `set_unordered_write`, db_options.rs:2669), and `two_write_queues = false` (:1558; never set by rocks).

Every other profile leaves all four untouched (§6.7).

**Who writes the active memtable's cache, and why it is monotonic in a valid interval.**
1. **Writer 1:** `MemTable::ShouldFlushNow` stores `table_->ApproximateMemoryUsage() + arena_.MemoryAllocatedBytes()` at memtable.cc:322-325. It is called only from `UpdateFlushState` (:378-385), and only while `flush_state_ == FLUSH_NOT_REQUESTED`. `UpdateFlushState` is called from `MemTableInserter`'s post-processing for each insert (`memtable.h:738-754`; `write_batch.cc:2217-2225`).
2. **Writer 2:** `MemTable::ApproximateMemoryUsage()` stores a recomputed total (memtable.cc:276-292). For a memtable, its callers are:
   - the flush job (`flush_job.cc:163`, :961);
   - mempurge (:571, :620, :639, :856; disabled, R16);
   - `MemTableList` add/remove (`memtable_list.cc:113-121`, :790), which runs at a switch or flush install;
   - `SuperVersion::Cleanup` when the memtable's last reference drops (`column_family.cc:540-545`).

   None of these runs in a valid interval, because each needs a switch, a flush, or the freeing of a memtable, and a switch or flush changes the CF's super version, which invalidates the interval (below). The active memtable is never freed while it is active.
3. **With concurrent memtable writes off**, `parallel` is false (`db_impl_write.cc:1269-1270`). The group leader inserts every batch of its group, in order, on its own thread (`WriteBatchInternal::InsertInto(write_group, …)`, :1453-1460). Groups are serialized: the next leader starts only after `ExitAsBatchGroupLeader` (:1566). With `enable_pipelined_write` false, the pipelined path (:1700-1760) is not taken. So writer-1 stores happen one at a time, in commit order.
4. Both summands only grow during a memtable's life: the arena allocates and never frees, and the skiplist's memory grows. So each writer-1 store is ≥ the previous one, and the cache is **non-decreasing** until a flush request.
5. **After a request** there are no more writer-1 stores (:379), so the cache keeps the request-time value. That value is ≥ W − 0.4 × arena block (memtable.cc:327-339), and the arena block is ≤ 1 MiB (`column_family.cc:239-248`), so it is > B.
6. `write_batch` (rocks.rs:717-731) holds the shared live lock across `db.write(..)` and its sample. `db.write` returns only after its group's memtable insert, so the sample is taken after the cache store for that commit's last insert. The last commit's sample is therefore ≥ every earlier store, and it also captures a last-write request. Sol's stale-overwrite schedule needs two concurrent `ShouldFlushNow` calls on one memtable, and point 3 rules that out.
   - **Correction (OD-CH5-20).** The claim that the last commit's sample is ≥ every earlier store holds for one writer, not for two under the shared lock: a later writer's write can switch the memtable (`db_impl_write.cc:2145-2148`) between an earlier writer's commit and that writer's sample, which then reads the new memtable and leaves `hwm` short. The switch moves the column family's super version, so step 1 invalidates the interval (`CfVerdict::invalidation` = `SuperVersionChanged`), and the verdict is a violation either way. L2c asserts `(over_b && hwm ≥ W − 1 arena block) || (!valid && the CF's own super-version change)` per iteration and logs the branch counts.

**The serialization.**
- `RocksEngine` gets `live: Option<RwLock<LiveState>>`, which is `Some` only for the live profile. `None` takes no lock and does no sampling (§6.7).
- Writers hold the lock **shared** across commit + sample.
- `live_barrier` holds it **exclusive** across these steps:
  1. SV check;
  2. verdict;
  3. `flush_all(true)`;
  4. the purge wait;
  5. `sync_wal`;
  6. the counter snapshot (§2.2, taken by W's step-11 caller through a closure the barrier runs here);
  7. `sv0` read;
  8. reset.

**The samples.** After each committed batch the engine reads `cur-size-active-mem-table` for each of the schema's CFs: 11 property reads. Per CF it keeps:
- `hwm`, a `fetch_max` of the samples;
- `bytes`, the sum of the batch `size_bytes` (rocks.rs:1292-1295), reported only;
- `over_b`, set at the first sample > B, when the C4 line is logged;
- `sv0`.

**No final resample (D2-6).** The barrier does not resample. Once it holds the exclusive lock, every earlier commit's sample has been taken (points 3 and 6), and no insert can follow. Only writer 1 stores into the cache in a valid interval (points 1–2), so a resample would read the value the last commit's sample already read. The barrier's step 2 builds the verdict from `hwm`, `bytes`, `over_b` and the SV check, **before** step 8 resets them. That order is what M10 tests (§6.6).

**The invalidation.** At step 1 the barrier reads each CF's `rocksdb.current-super-version-number` (`internal_stats.cc:1412-1417`). A change since `sv0` marks the interval **invalid**. It logs C4's second line and counts as a violation. A switch, a flush install, a compaction install or `SetOptions` all change it.

**The verdict:** `{cf, hwm, bytes, over_b, valid, invalidation, super_version}` per CF (`dash_kv::CfVerdict`): `invalidation` names the cause of an invalid interval (`SuperVersionChanged { from, to }`), and `super_version` is the one the barrier ends at, which begins the next interval and goes into the counter snapshot. `bytes` is the sum of the `size_bytes` of the batches that recorded into the CF. It is returned to the test-hooks control (§6.2) and logged one line per CF per barrier, in test-hooks builds only.

**Numbers (Q4):** W = 64 MiB and B = 48 MiB per CF, accepted by the measurement below (OD-CH5-27). No arena margin is assumed beyond point 5.

**The measurement, a READY gate:**
- The runs: the M2–M6 passlist on rdashd (func-compat `run`, `RDASHD_NODES=all`), mixed-a, the `rdashd/tests` process suite, and the synthetic snapshot-writer workload (an engine-level loop of `RocksEvoStore::store_quorum_snapshot`, `evo/store.rs:102-115`, at a peer's request rate, `llmq/rows.rs:81-91`).
- **Acceptance:** every interval is valid, and B ≥ 8 × the maximum `hwm` per CF.
- If the margin fails: W = 256 MiB and B = 224 MiB, re-measured; if that fails too, escalate.
- The M7 BACKLOG row (owner CH5) repeats the measurement when M7 runs on rdashd, with OD-CH5-27's exclusion list.

**The exclusions (OD-CH5-27).** The 8× margin guards the write rate a copy interval can see. A workload sized by construction from another budget measures that budget instead, so the 8× rule does not apply to it. It must still meet every other gate, as a hard gate: every interval valid, and the workload's own assertions. Each new exclusion needs an owner ruling.

| Excluded workload | Cite | Why | Gate it still meets |
|---|---|---|---|
| I2a, I2b, I4 and their twins (§6.4, §6.5) | `rdashd/tests/live_copy.rs` (`i2a_…`, `i2b_…`, `i4_…`) | Sized from W and B: I2a/I2b's window ends at B, I4 at W + 1 MiB | Valid intervals plus their designed assertions (allocation in its window ≤ B; A1 + A2 ≥ W + 1 MiB, A2 ≤ B) |
| `wide_chain` (`verifychain_level_3_budget_…`, the threshold tests) | `rdashd/tests/verifychain.rs:372` | A single bulk submit of fan-out blocks, sized to the `-dbcache` level-3 thresholds (up to 246 MB at `-dbcache=280`/281) | Valid; coins hwm ≤ B with `over_b` false: 30,704,416 against B = 50,331,648 (1.64×) |

**The measurement at W = 64 MiB, B = 48 MiB (B/8 = 6,291,456).** Maxima are per CF; a CF not listed stayed at 2,048 (an empty memtable).

| Run | Logs / verdict lines | Invalid | `over_b` | Max `hwm` |
|---|---|---|---|---|
| func-compat M2–M6 passlist on rdashd, wallet tree | 265 / 4,775 | 0 | 0 | block_index 3,441,440; coins 2,392,864 |
| func-compat M2–M6 passlist, non-wallet tree | 187 / 4,103 | 0 | 0 | block_index 3,441,440; coins 2,392,864 |
| mixed-a | 15 / 176 | 0 | 0 | block_index 1,344,288 |
| Synthetic snapshot writer (18,000 `store_quorum_snapshot`, `dashd-chainstate` `tuning.rs`) | — | 0 | 0 | llmq_snapshot 4,196,352 |
| `rdashd/tests` process suite without `live_copy` and `wide_chain` | 442 / 11,066 | 0 | 0 | block_index 2,392,864; coins 2,392,864 |
| `live_copy` without I2a/I2b/I4 (I0, I1, I3, I5 and its control, the oracle fixtures) | 55 / 1,540 | 0 | 0 | block_index 2,392,864; coins 2,392,864; block_files, credit_pool, meta, mn_diff 1,050,624 |
| **Under the 8× rule** | | **0** | **0** | **4,196,352 ≤ 6,291,456: accepted** |
| `wide_chain` (excluded) | 6 / 462 | 0 | 0 | coins 30,704,416 (bytes 18,382,829) |
| I2a / I2b / I4 (excluded) | — | 0 | — | coins 41,188,128; block_index 8,682,272; A1 36,993,824 / A2 32,799,520 |
| A twin replaying I4's whole log (excluded) | — | 0 | 1 | coins 67,404,576 (valid, over B: the twin's replay is not an interval of the contract) |

**Cost:**
- at most 11 × W unaccounted memtable memory in an interval: 704 MiB, or 2.75 GiB at the 256 MiB escalation. This is the price of no WBM;
- 11 property reads per batch;
- no parallel memtable insertion, regtest only;
- the barrier's exclusive section.

---

### CH5-5 4. Trigger and file-lifetime table

There are **34 rows**. Abbreviations: `rocks` = `dashd-store/src/rocks.rs`, `tun` = `dashd-chainstate/src/tuning.rs`, `mach` = `flush/machine.rs`.

| # | Trigger | Where | Closed by (regtest) | Proof |
|---|---|---|---|---|
| R1 | Per-CF memtable size: the request comes during an insert (memtable.cc:378-385) and the switch is scheduled on the next write (`db_impl_write.cc:2145-2148`) | tun:59-75, :232-237; rocks:1050-1056 | `write_buffer_size` = W; a valid interval with every `hwm` ≤ B (§3.2) | L1, L2a/b/c, I2a/b (M8) |
| R2 | `max_write_buffer_number` stall | rocks:33-35, :1056, :1065; `column_family.cc:1016-1017` | 2; no switch in a valid interval | L1 |
| R3 | The WBM (`HandleWriteBufferManagerFlush`, our `start_due_flush`) | tun:57, :71, :229-232; rocks:364-370, :741-764; `db_impl_write.cc:2128-2137` | `charged_write_buffer_bytes = None` | L3, I2a (M4) |
| R4 | The DB-wide fallback cap | rocks:36-37, :141, :369; `options.h:1255` | `db_write_buffer_size = 0` | L3 |
| R5 | `max_total_wal_size`, checked before the write (`db_impl_write.cc:2114-2126`). 0 = 4 × Σ(wbs × mwbn) (:2802-2809) | tun:242; rocks:65-67, :371 | `u64::MAX` | L3, I2a (M5) |
| R6 | WAL rotation and recycling (`db_impl_write.cc:3145`) | `options.h:978`; `io_posix.cc:1674` | Follows from R1/R3/R5/R20; `recycle_log_file_num = 0` pinned | L1, L5 |
| R7 | Obsolete-file deletion (`db_impl_compaction_flush.cc:3978-4017`; inline: `options.h:2353`, :1601) | — | C3 step 3; no job in a valid interval | L4; call order (M2b) |
| R8 | `RocksTuning::after_bk` | tun:274-281; mach:932-935, :1240-1242, :1251-1271 | A no-op in the live profile | I1 (M3) |
| R9 | Ordinary flushes (WAL appends, files barriers) | `chain/accept.rs:374`; `chain/abc.rs:730`, :782, :1062; `flush/control.rs:340-353`, :568-574; `flush/policy.rs:166-260` | Appends only; their triggers in an interval need A2′-excluded work, ATMP growth or ≥ 1 h | I1, I2a/b |
| R10 | Auto-compaction | rocks:1069-1070; `column_family.cc:1262-1265`, :1018-1040 | `quiet` | L5 (M6) |
| R11 | Periodic, TTL and read-triggered compaction | `db_impl.cc:7522-7558`, :1233-1236 | The R10 gate | L5 |
| R12 | Other periodic tasks | `db_impl.cc:272-283`, :1224-1236 | `quiet` (stats 0); defaults pinned | L5 |
| R13 | Info `LOG` rotation | `options.h:956`, :963; rocks:378 | Defaults, pinned | L5 |
| R14 | MANIFEST/CURRENT via `LogAndApply` | `options.h:991-1015`; rocks:385 | No `LogAndApply` in a valid interval | I1, L1 |
| R15 | OPTIONS file | rocks:354, :400-404 | No runtime setter | L5 (source scan) |
| R16 | IDENTITY, LOCK, marker, blob, mempurge, range-delete limit, op-scan triggers | rocks:908-932; `advanced_options.h:1082`, :374, :1383, :1402; `options.h:357` | Defaults, pinned | L5 |
| R17 | `Buffered` writes outside W (`evo/store.rs:102-115`, `llmq/rows.rs:81-91`) | — | Appends, sampled like any batch; fenced by C3 | L1, the synthetic workload |
| R18 | The copy points themselves (site.rs:317-399; actor.rs:380-410) | control.rs:544-552 | C3 | L4; I0–I3 |
| R19 | Concurrent copy points (`flush/driver.rs:300`, :349-363; `db_impl_compaction_flush.cc:2968-2974`) | — | A second copy point is the next explicit flush point; C1's interval ends at it (ALWAYS writes meta; `flush_memtables` switches the WAL). Q1's caller contract excludes any explicit flush point during a copy, a second copy point included (OD-CH5-25) | I3 |
| R20 | `async_wal_precreate` (`options.h:980-989`; `db_impl_open.cc:2533-2553`, :2593-2618) | — | Pinned false | L5 (M9) |
| R21 | The internal SstFileManager, rate 0 (`db_impl_open.cc:192-197`; `sst_file_manager.h:132-137`; `file_util.cc:142-148`; `delete_scheduler.cc:60-82`) | rocks never sets it | Default | L5 (scan), L4 (M12) |
| R22 | Open-time L0 compaction (Q2) | New in rocks's open (rocks:335-441) | Before the engine is returned, so outside every interval | L6 |
| F1 | blk/rev appends, preallocation, finalize/truncate (`flatfile/blocks.rs:440-506`, :536-582; `seq.rs:175-227`) | — | Appends are safe (referenced bytes were synced before B1, mach:1195-1198). Finalize/truncate needs a write or rollover: **A2′** | I1; the §2.2 counters |
| F2 | A new blk/rev file (`seq.rs:110-127`, :207-227; `blocks.rs:452-476`) | — | **A2′** | I5; the counters |
| F3 | Cross-store commit vs `copytree` order | mach:1195-1198 | **A2′** only (as in Core) | I1 blocks-first, a recorded outcome |
| F4 | Prune unlink (mach:965-970, :1309-1325; `blocks.rs:496-499`, :578-581) | — | **A2′ and A1** | the counters (`remove`) |
| F5 | `make_private` (`seq.rs:124-151`) | — | **A2′** | the counters |
| D1 | `peers.dat`, `anchors.dat`, `banlist.json` (`dashd-net/src/store.rs:33-40`, :67-85) | Core `net.cpp:73`, :4123; `banman.h:22` | Out of scope, as in Core; removed from the copy (tf:640-642) | — |
| D2 | `debug.log` | — | Out of scope | — |
| D3 | `mncache.dat`, `netfulfilled.dat`, `sporks.dat` (`flatdb.rs:497-540`) | Core `init.cpp:416` | Written at shutdown only, outside the interval | — |
| D4 | `mempool.dat`, `settings.json`, `fee_estimates.dat` | Core `fees.h:26` | RPC-driven ones by A1; out of scope | — |
| D5 | `.lock`, `rdashd.pid`, `.cookie` | — | Outside the interval | — |
| D6 | `indexes/` (none at this head; chainstate.rs:257) | — | IX1 BACKLOG gate | — |
| D7 | `evodb/`, `llmq/`, `wallets/` (none exist; `store/schema.rs:6-20`; `datadir.rs:290-293`) | tf:637-638 | The Q5 adapter | the adapter's tests |

**Not closable by tuning:**
- F1's finalize/truncate, and F2–F5: closed by A2′, evidenced by the counters.
- R19: a second copy point during a copy is excluded by the caller contract (OD-CH5-25).
- D1/D4: out of scope, as in Core.

---

### CH5-5 5. Design choice

**Option 1, chosen:** a regtest RocksDB profile plus the C3 barrier. Option 2 (a pinned checkpoint) is rejected.

**`dashd-store` (rocks.rs).** `RocksConfig` gains:
- `live_copy: Option<LiveCopy { write_buffer_bytes: usize, bound_bytes: u64 }>`;
- `concurrent_memtable_write: bool`. `Default` is true (today's RocksDB default, unchanged); `LiveCopy` requires false, and `open` asserts it.

With `live_copy = Some`, `open`:
- sets `quiet = true` (R10, R12, reusing rocks:1069-1070);
- sets every CF to `write_buffer_size = W` and `max_write_buffer_number = 2`;
- uses no WBM, with `db_write_buffer_size = 0`;
- sets `max_total_wal_size = u64::MAX`;
- sets `async_wal_precreate = false` and `recycle_log_file_num = 0`;
- applies the four §3.2 write-path settings;
- asserts `charged_write_buffer_bytes.is_none()`;
- runs R22.

`RocksEngine` gains:
- `live: Option<RwLock<LiveState>>`, plus the per-commit sample inside the `Some` branch of `write_batch` only;
- `live_barrier(&self, snapshot: &mut dyn FnMut()) -> Result<LiveVerdict>`, the §3.2 exclusive section;
- `wait_idle_purged(&self) -> Result<()>`, through `purge_wait_options()`;
- under `cfg(any(test, feature = "test-hooks"))` only: `probe: ProbeCounters { live_lock_acquires, live_property_reads, live_state_updates, c4_bound_lines, c4_switch_lines }`, a per-engine `AtomicU64` set incremented at its sites (§6.7; the last two count C4's two lines).

**`dashd-chainstate` (tuning.rs).**
- `RocksTuning` gains `live_copy` and `RocksTuning::for_regtest()`. `Default` is unchanged.
- `EngineTuning` gains `fn live_copy(&self) -> bool` and `fn live_copy_barrier<K: KvEngine>(&self, db: &GuardedEngine<K>, snapshot: &mut dyn FnMut()) -> Result<Option<LiveVerdict>, StoreError>` (`None` on the default barrier).
  - The default barrier is today's two calls followed by `snapshot()`.
  - `RocksTuning` with live copy calls the guarded `live_barrier`; a tripped latch refuses it (tun:105-112).
- With live copy, `after_bk` returns `Ok(())`.
- mach:936-938 calls `env.tuning.live_copy_barrier(engine, &mut snapshot)`. In test-hooks builds the closure fills the job's `CopyPointSnapshot` (§2.2); otherwise it is a no-op closure.

**`dashd-node`.** `chainstate_profile(network, requested) -> (RocksTuning, bool)` (chainstate.rs:550) is called at node.rs:295, its pair goes into `coins_configuration` at node.rs:301-302, the chainstate is opened at node.rs:360, the tuning opens the database at chainstate.rs:258, and the pair is asserted at chainstate.rs:659 (anchors at the BL-634 head). `requested` is `-storageprofile` (BL-634), read by `storage_request`.
- Regtest gives `(for_regtest(), true)`, unless `-storageprofile=production` asks for production; then `(Default, false)`.
- Every other network gives `(Default, false)`.

**The storage-profile option (Q3, amended by BL-634).**
- `-storageprofile=<livecopy|production>` is registered `DEBUG_ONLY` in Debugging/Testing, so `-help-debug` lists it and `-help` does not. It is present in every build and accepted on regtest only; any value on main, test or devnet is refused at init (D15, DEC-1070).
- It **replaces** the test-hooks seam `RDASHD_TEST_HOOK_PROFILE`. Test-hooks builds refuse that variable (`RDASHD_TEST_HOOK_PROFILE was replaced by -storageprofile (BL-634)`), so a leftover script cannot silently get the live profile. They also refuse `RDASHD_TEST_HOOK=live-copy` together with `-storageprofile=production`, so no tripwire reports on a node whose copies carry no promise.
- When set, it logs the profile once (`Chainstate storage profile: …`). The test-hooks line `test-hooks: {live-copy|production} storage profile on regtest` stays.
- The engine and bench harnesses keep `RocksTuning::default()` (e.g. `dash-testkit/src/crash/inspect.rs:145-148`).
- Process-level crash coverage now runs on both profiles: dbcrash's P1/P2 per PR and its two rdashd reorg loops nightly (`Storage::{Live, Production}`, each launch checking the start-up line and the chainstate's `OPTIONS`), on the non-hooks rdashd binary, which carries the option in every build (debug builds in dbcrash, release in ST3b). ST3b's `--release` campaigns do **not** yet pass `-storageprofile=production`: adding the flag after `--`, and the label observed from the start-up line, is ST3b work after BL-634 merges (OD-CH5-28; design §2.8). Until then those campaigns run the live profile. The production profile also keeps its engine-level coverage (the RocksDB engine tests on `RocksConfig::default()`: `dashd-store` `fail_stop`, `rocks_wal_nonprefix`, `engine_contract`; the inspection open in `dash-testkit/src/crash/inspect.rs:145`), and the faulty-model crash suites are profile-independent. PF0 may instead measure the live profile (OD-CH5-16).
- The shared launcher never sets the option (§6.7); func-compat never passes it, so the M7 copy flow runs the live profile by construction.

**L0 growth (Q2):** at most one L0 SST per CF holding data, per explicit flush point, for a memtable kept below W by a valid interval. This is a per-flush-point bound, not a lifetime bound; R22 compacts any CF at ≥ 16 files at the next open. Until then a long-lived regtest node with no auto-compaction keeps every such SST (and, with the default `max_open_files`, its descriptor) and pays the level-0 read amplification; RocksDB skips the level-0 stop trigger with auto-compaction off (`column_family.cc:1018-1040`), so writes never stall on it.

---

### CH5-5 6. Tests and acceptance

#### 6.1 Engine level (`dashd-store`, phase 1)

These are in `dashd-store/src/rocks/live_copy_tests.rs`. L5 extends the quiet test. `dir_state` is every file's name and inode, plus its length except for `*.log` and `LOG`.

- **L1. Quiet interval.**
  - For the 11-CF and a 1-CF schema, write up to B, in one CF and spread across CFs, with `Buffered` and `Durable` batches.
  - Per write: the verdict is valid, `num-immutable-mem-table` = 0, `dir_state` is unchanged apart from the current WAL's and `LOG`'s lengths, and the WAL's inode is unchanged.
- **L2a. Cross W, let the automatic flush finish, then the barrier.**
  - Write until a request (allocation ≥ W − 1 MiB), then a 1-byte follow-up, then `wait_idle_purged`, then `live_barrier`.
  - Assert: that CF's verdict is `valid == false`, C4's second line is logged once, and `over_b == true`.
- **L2b. Cross B on the last write.**
  - Stay below B, then make one write that takes `hwm` past B, then call `live_barrier` with no further write.
  - Assert: the **verdict** has `over_b == true` and `hwm > B`, `bytes` is reported separately, and C4's first line is logged exactly once.
- **L2c. Controlled two writers, the last-request schedule (D2-2).**
  - Setup: a test `LiveCopy { W = 2 MiB, B = 1.5 MiB }`, which keeps B below W − 1 arena block (256 KiB); 1 CF; 64 KiB values.
  - Each iteration:
    1. pre-fill to an allocation of 1.25 MiB, read from the property;
    2. threads A (one 4 KiB put) and B (twelve 64 KiB puts, which take the allocation past the request threshold) are released together by a `std::sync::Barrier` into `engine.write`;
    3. join both, then call `live_barrier`.
  - Assert, every iteration (OD-CH5-20): `over_b == true` and `hwm ≥ W − 1 arena block`, **or** the interval is invalid because that column family's super version changed (point 6's correction). The order of A and B is left to the scheduler, so both orders occur. There are 2 000 iterations; the branch counts are logged.
  - With the profile's setting (§3.2 point 3), this must never fail.
  - Under M13 (concurrent insertion on), the stale overwrite needs a preemption inside memtable.cc:322-325, which no callback or sync point reaches in a release RocksDB. So L2c's failure under M13 is **probabilistic**. M13's deterministic killers are the pins below (§6.6).
- **L3. Production triggers off.**
  - Commit 6 MiB to one CF, then a 1-byte follow-up write, then `wait_idle_purged`.
  - On the profile: `dir_state` is unchanged and L0 = 0.
  - Under M4 or M5: L0 ≥ 1 and the old WAL is gone.
- **L4. The barrier.**
  - After `live_barrier`: no older `NNNNNN.log`; no `*.trash`; `num-running-flushes`, `mem-table-flush-pending` and `num-running-compactions` are 0; `dir_state` is stable over 2 s.
  - 200 repetitions (supplementary).
  - Under M12 (SstFileManager rate 1 B/s): a `.trash` file exists right after the barrier.
- **L5. Pins and compaction.** This extends `a_quiet_database_does_not_compact` (rocks.rs:2352-2395) with a live mode, which must behave like `quiet`: L0 ≥ 4 and `compaction-pending` = 1.
  - In live mode the OPTIONS file must contain: `disable_auto_compactions=true`, `recycle_log_file_num=0`, `max_log_file_size=0`, `log_file_time_to_roll=0`, `persist_stats_to_disk=false`, `enable_blob_files=false`, `memtable_max_range_deletions=0`, the op-scan triggers 0, `experimental_mempurge_threshold=0`, `db_write_buffer_size=0`, `max_total_wal_size=18446744073709551615`, `write_buffer_size=W`, `max_write_buffer_number=2`, `async_wal_precreate=false`, **`allow_concurrent_memtable_write=false`**, `enable_pipelined_write=false`, `unordered_write=false` and `two_write_queues=false`.
  - In default and quiet modes it must contain `allow_concurrent_memtable_write=true`.
  - The source scan asserts that `dashd-store/src` has no `set_options`, `set_db_options`, `create_cf` or `set_sst_file_manager` call.
- **L6. Open-time compaction (Q2, OD-CH5-15).**
  - Seed 16 overlapping L0 files in two CFs (16 flushes of keys `a` and `z`), record each CF's rows (§6.3 reader), close, and reopen live.
  - The postconditions:
    - `num-files-at-level0 == 0` per compacted CF;
    - `rocksdb.background-errors` unchanged.

    They are checked inside `open`, through `prop_u64(cf, name) -> Result<u64>`. It maps `Err` → `StoreError` and `Ok(None)` → `StoreError::Engine("property <name> missing")`, so open fails closed. That is needed because `rocksdb_compact_range_cf_opt` drops `CompactRange`'s Status (`db/c.cc:3286-3298`).
  - Then: the rows are equal; `live_barrier`; `dir_state` stable over 2 s.
  - A unit test calls `prop_u64` with `rocksdb.no-such-property` and gets `Err`.
- **The raw option:** `purge_wait_options()` reads back `_get_wait_for_purge == 1` and `_get_flush == 0` (c.h:5878-5883).
- **Calibration:** these tests measure the per-row allocation of a `raw(51)` coins row and of a headers-only block_index record (value 83–110 B, `dash-chain/src/record.rs:38-64`; key 32 B), and assert the §6.4 constants.

#### 6.2 Node seams (phase 3)

The control seam is enabled by `RDASHD_TEST_HOOK=live-copy` and `RDASHD_TEST_HOOK_DIR=<dir>`, beside today's env hooks (`dashd-node/src/node.rs:487-519`). It is file-based: the test writes `<req>`, and the node writes `<req>.done`. Without `RDASHD_TEST_HOOK=live-copy` a test-hooks build answers as the production build: `gettxoutsetinfo` has no `livecopy_counters`, and `livecopycounters` is not registered (`livecopy::enabled`, OD-CH5-27 (b)), so the Core-diff targets (dbcrash, `rpc_diff`) pass in a feature selection that unifies `dashd-node/test-hooks`. `hooks.rs` asserts both absences.
- **`flush`.**
  - OD-CH5-22 (a): the hook calls `ChainHandle::scripted_flush` (`chain/actor.rs:831`), which asks for the cause and sends `Flush{None}`; `NodeFlush::flush` takes the request into `scripted` (`flush/site.rs:306-311`, test-hooks only), so V's policy decides an ordinary SCRIPTED full flush (policy.rs:124, :194-196).
  - **Tickets (Sol r2 C-3).** Each call's request carries a ticket (`dashd_store::livecopy`, test-hooks only). The tickets live under one lock: requested, taken, running (frozen into a job W has not finished), done, or refused (below). V's take (`take_scripted_flush`) claims the request and records it as taken in one step, so no caller ever sees a request that is in neither place. The freeze moves every taken ticket into the job (`FlushMachine::scripted`, test-hooks only), and `JobDone` names the job's tickets. A call returns only the `JobDone` that carries its own ticket. Every critical section is a few loads and stores, and no caller holds the lock across a wait, so V never waits on a caller.
  - It re-sends only while its ticket is untaken or the cause pending, never once the job is frozen, so one request makes exactly one job. `FLUSH_PATIENCE` bounds the whole call (Sol r1 C-1, r2 C-5): every reply wait and every polling pause is bounded by the time left, expiry is checked before the request is published and after each blocking step, so a parked V cannot hold the call past it. On expiry the call withdraws its ticket only if V has not taken it, and writes `error=` to `flush.done` (`a_scripted_flush_fails_at_its_deadline_while_v_is_parked`).
  - With no best block, V's flush writes only the index, and no job would carry a taken ticket. So V's take refuses the request instead: the ticket is recorded as refused, nothing stays outstanding, and the call fails at once with `no best block: no scripted flush` rather than at its deadline (Opus r1 O-3; `a_request_with_no_best_block_is_refused`).
  - A ticket V took before its call timed out stays outstanding until its job is done, and every call first waits until no ticket is outstanding (`scripted_outstanding`). So a call never shares a job with an earlier ticket, never returns an earlier request's job, and never leaves its own job unawaited: `one_scripted_flush_call_adds_exactly_one_job`'s third case, and `a_call_after_a_timeout_in_vs_take_returns_its_own_job` (V parked right after its take, at the test-hooks `take_park`, while a job in flight keeps the cause pending; mutant TR, the claim split again, fails it).
  - When the job's **last** step returns, in the driver's job loop (`flush/driver.rs:349-363`), W emits `JobDone{steps}`. For an ordinary full flush that is after Release and AfterBk (machine.rs:1251-1271).
  - The node then writes `flush.done` with the step list and the verdict.
- **`quiesce`:** `wait_idle_purged` with no flush, then `quiesce.done`.
- The hooks engine (`WalFault`, `dashd-node/src/chainstate.rs`) must forward `wait_idle_purged` and `live_barrier` to RocksDB, or the barrier silently falls back to the default steps.
- **`spork-update <name> <value>`:** calls `SporkManager::update_spork(id, value)` (`dashd-masternode/src/sporks.rs:153`), the function Core's `sporkupdate` RPC reaches. It needs `-sporkkey` (`dashd-node/src/sporks.rs:35`); the tests pass Core's functional-test key (`dashd-node/src/sut.rs:660`).
- **`cl-floor <hash> <height>`:** builds `ChainLockSig { height, block_hash, sig: <default> }` and calls the actor's public `record_chainlock(sig, sig_hash, None)` (`chain/actor.rs:792-806`). That is the post-verification entry LK1a uses, so it runs `locks.rs:178-225`:
  - a `Floor` outcome;
  - `set_best_chainlock(.., true)`, which sets `best_known` (:233-237);
  - the deferred `ClEnforce{Recorded}`.

  Only the BLS verification is skipped, because no regtest quorum exists here.
- **The events.** These are test-hooks counters, separate from the §2.2 tripwire set and also in `Counters`:
  - `cl_turned_on_reset`, incremented in `V::cl_active_state`'s `TurnedOnReset` branch (`dashd-chainstate/src/chain/locks.rs:151-155`), the node's only caller of dash-chain's `check_cl_active_state`, so every off→on reset. OD-CH5-21: dash-chain is layer S and the counters' dashd-store is layer N (`contrib/node/layers.toml:86`, `:95`), so the `(false, true)` arm itself (`enforce.rs:103-107`) cannot count;
  - `cl_enforced`, plus `cl_enforced_floor` (the last floor hash), recorded in `enforce_best_chainlock` after `EnforceBlock` returns (`chain/locks.rs:302` onward);
  - `flush_jobs_done`, incremented at `JobDone`.

  They are read through `livecopycounters`.
- **`verdicts`:** every barrier verdict since start.

Phase-3 tests:
- **Acknowledgement:** a model engine whose `after_bk` blocks on a latch; `flush.done` appears only after the latch opens, and its steps end `…, Release, AfterBk`.
- **Quiesce** returns at once on the profile.
- **The counter snapshot is atomic (D3-3).** This uses the in-process model harness (`node_stores.rs` `HookTuning` over `MemoryTuning`), with a `cfg(test)` sync point `AfterRelease` placed right after W publishes Release (machine.rs:1266-1271; the driver's report, `flush/driver.rs:364-377`, :583-607) and before any later W operation:
  1. While W executes the copy point's step 11, the test queues an activation for V.
  2. W publishes Release and parks at `AfterRelease`.
  3. V processes the queued activation, increments `activations`, and acknowledges the increment to the test; only then does the test release W.
  - Assert: the reply's `livecopy_counters.activations` **excludes** that increment, and the later `livecopycounters` read **includes** it (+1).
  - With the correct code the snapshot is taken in step 11, before the sync point. **M15** (OD-CH5-22 (b): the reply's snapshot read when the reply is built, `CoinsStatsJob::live_copy()` reading `COUNTERS.tripwire()` at access) includes the increment and deterministically fails the exclusion assertion.
  - An explicitly stepped model with exactly this ordering is an acceptable alternative. Production scheduling does not change.
- **The seam** reports its profile at start-up.

#### 6.3 The reopen oracle (phase 4)

**6.3.1** It compares the copied store's persistent rows: never a replay, never a recording. The plan's "StateDigest equal" becomes per-CF row fingerprints (OD-CH5-14).

**6.3.2 Steps on a copy.**
1. **Offline.** `dash-testkit::livecopy::rows` opens a second copy through `ChainstateOpen::open` (`dash-testkit/src/crash/inspect.rs:142-160`). It then:
   - runs C0's decision-key and I1 checks;
   - asserts no marker, so nothing is replayed; no test here claims ST3a replay;
   - for each of the 11 CFs (`store/schema.rs:6-20`), records `(count, SHA-256 over len-prefixed key ‖ value in key order)` via `KvEngine::iter_prefix(cf, &[])` (rocks.rs:1346-1358, unbounded).
2. **Start rdashd on the copy** at the default checks. Then check:
   - the tip equals the expected tip (per test);
   - `getblock` succeeds for every block;
   - `gettxoutsetinfo`'s `hash_serialized_2` and `txouts` equal the source's at that tip.

   Stop it cleanly; step 1's fingerprints are unchanged.
3. **Twin.**
   - The fixture records the source's **full submission log**: every `submitblock` and `submitheader`, in order, with its result. That includes side-branch blocks and headers-only submissions (OD-CH5-14).
   - A second rdashd replays the log up to the copy's tip, then stops cleanly.
   - **All 11 fingerprints must be equal, `meta` included, byte for byte.**
   - **The `meta` allowlist is empty.** No key is excluded or normalized. `tip`, `evo_tip`, `head_blocks` and `last_file` (`store/meta.rs:20-27`) are compared like every other row, and the decision keys are also checked independently in step 1.
   - Any difference fails. A difference found later is fixed in code, or brought back to review with a source-backed encoding cite; it is never added to a list by the test.
4. **Decoded rows:** coins (count == `txouts`), `block_index`, and evo rows through `EvoCodec::decode_diff`/`decode_snapshot`, `decode_mined`/`decode_index`/`decode_credit_data` (`evo/codec.rs:238-365`, :433-480).
5. **Continuation.** The next block(s) go to the copy and the twin, and step 3 holds after them. In the evo fixture they come from dashd and carry Core's CbTx roots, which the copy checks against its copied evo state: `check_mn_list_root` (`evo/node.rs:1164`) and `check_quorums_root` (:1173), with script checks on (`connect/serial.rs:564-566`).

**6.3.3 Fixtures.**
- **Plain:** rdashd mines 30 blocks to `raw(51)`. Mandatory non-empty CFs: `meta`, `block_index`, `block_files`, `coins`.
- **Evo (D2-3).** BL-645 restored D2-3 as first written once CH7b-2 (T92) had removed G1q: DIP3 at 5, a populated 576 snapshot, and V20 with `credit_pool` from 432. The interim form that sat between two rotation cycle bases (DIP3 at 217, tip 238, continuation 239) is gone.
  - **Heights,** derived from regtest's parameters (`rdashd/tests/live_copy/fixture.rs` `heights()`), and asserted as 5/110/111/112/432/576/577:
    - DIP3 at 5 (`-dip3params=5:5` on every node, as `verifychain.rs`), asserted 1 < DIP3 < V20;
    - funding through `COINBASE_MATURITY + 10` = 110, so the coinbases of heights 1–10 have matured;
    - registration at 111 and the update at 112;
    - V20 at regtest's `v20_height`, 432. Core's `V20Height` copies the default `DIP0003Height` before `-dip3params` moves only the DIP3 heights (rc.3 `chainparams.cpp:818`, :828, :880), and dashd and rdashd both report v20 at 432 in `getdeploymentinfo`;
    - the tip T = `DISK_SNAPSHOT_PERIOD` = 576 (Core `evo/deterministicmns.h:762`; `dash-dml/src/process.rs:17`, :73), declared by the test and checked against the snapshot rows;
    - Core's T + 1 = 577 is the continuation.
    A throwaway copy of the source takes 577. Every block is accepted, the 24 DIP3-active rotation cycle bases among them.
  - Two rdashd nodes (source and twin) and one dashd generator. dashd runs **without** `-disablewallet` and with `-fallbackfee=0.0002`. Its submission log is replayed to the source by `submitblock`, and from the log to the twin. dashd mines ten blocks per `generatetoaddress`, so no call nears the RPC timeout on a loaded host.
  1. **The wallet:** `createwallet "w"`. Then `getnewaddress` ×5: `fund`, `collateral`, `owner`, `voting`, `payout`.
  2. **Funding:** mine to 110 to `<fund>`. Assert `getbalance ≥ 1001` DASH (`COINBASE_MATURITY` 100, Core `src/consensus/consensus.h:24`).
  3. **The first DIP3 block** (5): assert that its `mn_snapshot` row decodes to an explicit **empty** list (`dash-dml/src/process.rs:45-67`).
  4. **Registration** as before (`bls generate`, `protx register_fund … "127.0.0.1:<port>" …`), mined at 111. Its `mn_diff` row decodes with one added entry.
  5. **The update:** `protx update_service <proTxHash> "127.0.0.2:<same port>" <operator secret> "" <fund>`, mined at 112. Its `mn_diff` row changes the service (`NET_INFO`).
  6. **Through V20 to T** (576).
  7. **Mandatory non-empty:**
     - the four plain CFs;
     - `mn_diff`: one row per DIP3 block, 5–576 (572 rows);
     - `mn_snapshot`: exactly the rows at 5 (empty) and 576;
     - `credit_pool`: a pool row per block from V20, 432–576 (145 rows), each a pool with `locked > 0`; and a credit-data row per DIP3 block (572 rows), whose balance is 0 below 432 and non-zero from 432.

     The list at T (`built_list`: the nearest snapshot at or below T, then each later block's record, as `resolve.rs:259-324` builds it; at T that is the 576 snapshot) holds exactly the registered entry with `service == 127.0.0.2:<port>`. `mnhf` (145 rows), `llmq_snapshot` (20) and `llmq_mined`/`llmq_mined_h` (none: no quorum forms with one masternode) are compared with the twin's and their counts logged.
  8. **Time budget:** the fixture builds in about 30 s alone (25 s for the source, 5 s for the twin), and in 37–61 s at load 95–165 with every test process at nice 19; one gate run at load ~158 took 1,482 s. It is built once per test binary into stopped datadirs (source, twin, the log, and Core's 577), and every evo test copies them.

**6.3.4 Self-tests** (the phase-4 gate).
- **Twin vs twin:** two twins, one flushed (`flush` hook) after every submission and one only at stop. All 11 fingerprints must be equal, with the empty allowlist. That proves the fingerprints don't depend on flush history.
- **Damage.** Each case runs on its own independent copy, opened offline with `RocksTuning::default()`. The offline writer is closed before any rdashd starts on the copy, and each rdashd is a fresh process on that copy, so no list cache survives. Each case states the oracle failure:
  - **Coins:** swap two coins rows' values, on the evo fixture, between two coins whose compressed txouts differ. Step 3's `coins` fingerprint and step 2's `hash_serialized_2` differ. The legacy hash writes 1 for every transaction's height code (quirk 33, `dash-consensus/src/coins/utxo_hash.rs:89-92`; Core `kernel/coinstats.cpp:72-88`), so a swap of the plain fixture's coinbases, which differ only in height, would not change it.
  - **Coins:** delete one row. Step 3's count and `txouts` differ.
  - **Evo, the continuation gate (D3-2):** replace the populated **576** `mn_snapshot` row with an empty list at its own position.
    1. Decode the original row, take its counter `original_total_registered`, and build `MnList::new(hash_576, 576, original_total_registered)` (`dash-dml/src/list.rs:286`).
    2. Encode it with `dash_dml::codec::encode_snapshot_record` (`dash-dml/src/codec.rs:1052`).
    3. **Assert first:** `EvoCodec::decode_snapshot(value, &hash_576)` (`evo/codec.rs:340-350`) succeeds and returns an empty list at 576. `built_list(T)` on the damaged copy is empty, because the node's walk stops at a stored snapshot (`resolve.rs:283-286`); on the undamaged copy it holds exactly the one masternode at `127.0.0.2:<port>`.
    4. Write it under the 576 key, close the offline writer, and start a fresh rdashd on that copy.
    - **Control:** on a separate undamaged copy, a fresh process accepts Core's 577 (`submitblock` → `null`).
    - **Damaged copy, all required:**
      - step 3 differs in exactly `["mn_snapshot"]`;
      - in a fresh process, `submitblock` of Core's 577 is rejected with **`bad-cbtx-mnmerkleroot`** (`dash-evo/src/cbtx.rs:42-43`), because the empty list reaches the root comparison (`evo/node.rs:1160-1167`) before the later payee check;
      - a later tip query (`getbestblockhash`/`getblockcount`) returns 576.
  - **Evo, storage corruption (separate case, not the continuation gate; OD-CH5-41):** under the 576 key, the height-5 row's value verbatim, an empty list stamped with block 5's hash. Before the write, `decode_snapshot(value, &hash_576)` fails its block-identity check ("mn_snapshot of another block", `evo/codec.rs:340-350`). The copy fail-stops through AbortNode at start's tip read (`chain/dmn.rs:36`), before any block reaches it, and exits 1. The read runs on the block-import thread after init's warmup ends, so a call may be answered first. `debug.log` holds "GetListAtChainTip: Found EvoDB inconsistency, you must reindex: mn_snapshot of another block" and "A fatal internal error occurred".
  - **Evo:** delete the `mn_diff` row at the update height (112; OD-CH5-42).
    - Step 3 differs in exactly `["mn_diff"]`.
    - Nothing the node reads needs the row, because the list at its tip is the 576 snapshot (`resolve.rs:283-286`). So step 2 passes and Core's 577 is accepted: only the fingerprints see this damage.
    - The end-to-end EvoDB inconsistency → AbortNode path is the storage-corruption case's. The missing-diff walk to Core's initial snapshot (BL-650, DEC-1064) is unit-covered: `dash-dml/tests/reconstruct.rs` `a_missing_diff_without_the_marker_is_the_initial_snapshot` and `dashd-chainstate/src/chain/tests/node_evo.rs` `a_connect_over_a_row_less_parent_writes_its_snapshot`.
- **Positive:** an undamaged stopped-node copy passes on both fixtures.

**6.3.5 Copier.** `rdashd/tests/common/copier.rs` `PausedCopy`: it lists the whole tree, pauses at `Listed` and before each file, copies in a test-given order, and treats a vanished file as an error.

**6.3.6 Prerequisites.** None needs another owner:
- the row reader, new, owner CH5;
- evo decoding, which is already public (`evo/codec.rs`, `pub mod codec` at evo/mod.rs:16);
- the dashd-wallet evo fixture, owner CH5.

Not used: an evo state RPC (none at this head; the CbTx root check stands in) and VerifyDB's evo checks (CH5-1b, `verifychain.rs:983-987`).

#### 6.4 Workloads (D1-4, D2-4)

**Coins, before the copy point:**
- `generatetodescriptor 160 raw(51)`. The coinbases at heights 1–60 have matured for blocks ≥ 161.
- For each of those 60 coinbases, the fixture reads the `raw(51)` output's index `n` and value `V` with `getblock <hash> 2`, matching `scriptPubKey.hex == "51"`. It asserts `V ≥ 9 500 000` duffs.

**Coins, in the interval: three blocks.** Block k = 0, 1, 2 holds 20 fan-out transactions, `generateblock raw(51) [tx₁ … tx₂₀]`.
- **Inputs:** transaction j of block k spends the coinbase of height 20k + j + 1, j = 0…19. Each coinbase output is spent exactly once, and no transaction spends another fan-out output, so there is no double spend within or across blocks.
- **Each transaction:** `nVersion = 1`, type 0.
  - One input: the coinbase outpoint, an empty `scriptSig` (an `OP_TRUE` prevout needs none), sequence `0xffffffff`.
  - **9 500 outputs** of 1 000 duffs each to `raw(51)`; `nLockTime = 0`.
  - Fee = `V − 9 500 000` duffs, ≥ 0, taken by the block's coinbase.
- **Size:** 4 (version and type) + 1 (input count) + 41 (input) + 3 (output count, `0xfd` + u16) + 9 500 × 10 (8 value + 1 length + 1 script) + 4 (locktime) = **95 053 B**.
  - The fixture asserts `tx.len() == 95_053 < 100_000` (`MAX_STANDARD_TX_SIZE`, `dash-consensus/src/tx.rs:43`; `bad-txns-oversize` at :180-182; Core `v:386-388`).
  - After mining, it asserts `getblock(hash).size < 2_000_000` (tx.rs:36). The expected size is ≈ 1 901 060 + coinbase + 81.
  - No output has sigops.
- **Total:** 3 × 20 × 9 500 = **570 000** new coins rows.
- **Asserted at run time** from the verdicts: S + 1 MiB ≤ Δcoins allocation ≤ B. If calibration disagrees, the fixture changes the transaction count per block, keeping every transaction ≤ 9 500 outputs; the assertion never changes.

**block_index:**
- **50 000 headers**, built test-side on the tip (regtest PoW) and sent with `submitheader`. They are headers-only, so no activation is possible, which is within A2′.
- Each record is ≥ 115 B raw, so ≥ 5.75 MB in total.
- Asserted: 5 MiB ≤ Δblock_index allocation ≤ B. The forced `flush` writes one record per dirty block (machine.rs:1475-1479).
- Mechanics: each header takes the least time the median-time-past rule allows, which grows 1 s per 6 headers. Under the fixed mocktime a single chain of 50 000 would pass `MAX_FUTURE_BLOCK_TIME` (2 h, `dash-consensus/src/header.rs:17`), so the 50 000 are 10 branches of 5 000 on the tip. I2b's nodes run with `-checkblockindex=0`: regtest's default walks the whole index per header, quadratic over 50 000; it checks memory only.

**Follow-up write:** after each workload's forced `flush`, submit one more header and send another `flush`. That schedules a pending switch (`db_impl_write.cc:2145-2148`) or WAL switch (:2114-2126) under a mutant. Then `quiesce`.

#### 6.5 Process tests (phase 5, `rdashd/tests/live_copy.rs`, `--features test-hooks`, regtest, default `-dbcache`, fixed mocktime)

Every copy runs the oracle (§6.3) and the §2.2 counter check, comparing the reply's `livecopy_counters` with `livecopycounters` after the copy. Where a test forces in-interval work (I1, I2a, I2b, I5), it states the expected deltas.

- **I0. M1's killer (D2-5).** The plain fixture, but with no copy point before step 4.
  1. **Leave IBD.**
     - Record `j_pre = flush_jobs_done`. Mine 1 block, then wait until `getblockchaininfo.initialblockdownload == false`.
     - Then wait (timeout 30 s) until `flush_jobs_done > j_pre`: the LEFT_IBD job. LEFT_IBD fires once (`policy.rs:191-193`) and is marked done when its freeze is decided (:250-252).
     - Record `j0 = flush_jobs_done`.
  2. **Durable baseline.** Mine 9 more blocks (tip height 10). `flush` → `flush.done`. The baseline is height 10, read from `getbestblockhash` and recorded. Set `j1 = flush_jobs_done`, which must equal `j0 + 1` (only the scripted job).
  3. **Dirty.** Mine 10 more blocks (tip height 20) and record `T = getbestblockhash`.
     - Assert `flush_jobs_done == j1`: no LEFT_IBD (already done), no PERIODIC or index-only write (fixed mocktime keeps `now` below the 1 h and 24 h intervals, `policy.rs:183-187`), no DMAX, no cache-size flush (20 empty regtest blocks).
     - Assert that the baseline (10) is older than T (20).
  4. **The copy point and an immediate copy.**
     - `gettxoutsetinfo` → reply R.
     - Immediately run `PausedCopy` with no pauses, over the whole tree, in listing order.
     - No RPC goes to the source until it returns. Then, with no flush issued and the copy already finished, poll the read-only `livecopycounters` until the copy-point job's `JobDone` acknowledgement is observed (`flush_jobs_done == j1 + 1`, timeout 10 s), and read the final `flush_jobs_done`. It must equal `j1 + 1` with the correct code. Under M1 no copy-point job exists, so it stays at `j1` and the wait times out at the expected M1 value: no flush ran during the copy.
  5. **Assertions, in this order:**
     - (a) `R.height == 20` and `R.bestblock == T`;
     - (b) the copy's recovered tip == T;
     - (c) step 3's fingerprints == the twin at T (the log replayed through height 20).
  - **Under M1** (site.rs:329-337's `copy_point_flush` skipped, `released = None`), `coins_stats` scans the database as of the baseline. **(a) fails first**, with `R.height == 10`. Without (a), (b) fails (copy tip at height 10) and (c) fails (equal to the twin at height 10, not at T).
  - Nothing in steps 3–4 can heal the store: step 3's and step 4's job-count assertions prove that no flush ran.
- **I1. The ordinary-flush interleaving (M3's killer).**
  1. Mine 30 blocks, `gettxoutsetinfo`, list.
  2. Mine 5 more blocks. This is beyond A2′ in its safe direction: `blocks/` is copied after `chainstate/`.
  3. `flush` → `flush.done` (tail included), then `quiesce`.
  4. Copy `chainstate/` except `MANIFEST-*`/`CURRENT`, then `blocks/`, then `MANIFEST-*`/`CURRENT`.
  - **Assert:**
    - `dir_state(chainstate)` unchanged since the reply;
    - every `super_versions` entry unchanged;
    - the oracle passes at the post-flush tip (35);
    - expected counter deltas: `activations` +5 and `flatfile_mutations` > 0 (the forced work), `disconnects` 0.
  - The blocks-first variant's outcome is recorded (F3).
- **I2a / I2b.** As I1, with §6.4's coins or headers workload, then the follow-up write and `quiesce`. Assert:
  - the allocation window, from the verdict of a second copy point taken after the copy (the next explicit flush point, which ends the interval): a column family's allocation is its `hwm` less the verdict's smallest `hwm` (a family no batch wrote: the empty memtable's, the same arena for every family at one W). S is the default sub-batch, 16 MiB;
  - expected deltas: I2a `activations` +3, `flatfile_mutations` > 0, `disconnects` 0; I2b none (the §2.2 check holds);
  - the oracle's twin replays the whole log, the headers and the follow-up included;
  - `dir_state` and every super version unchanged;
  - the oracle.
- **I3. A second copy point while paused** (OD-CH5-25), with no write between. It is the next explicit flush point, so C1's interval ends at it, and Q1's caller contract excludes it during a copy. The test pins what it does: the second reply's tripwire equals the first's, and only meta's super version moves; `flush_jobs_done` and the verdicts each go up by exactly 1, the new verdict valid. The files change by exactly one WAL switch and at most one new SST, meta's (its properties block's `rocksdb.column.family.name`); nothing else is created, renamed or deleted (the MANIFEST grows in place), and the block files are unchanged. The copy paused across it fails on the vanished WAL, the known outcome (if it ever passes, look). A fresh copy started after the second reply passes the full oracle and the §2.2 check against that reply.
- **I4. Cumulative volume (supplementary).** A₁ ≈ 0.55 W at the first copy point; then A₂ with A₁ + A₂ ≥ W + 1 MiB and A₂ ≤ B; then the follow-up write and `quiesce`. Assert `dir_state` and the super versions unchanged. Both are coins fan-outs (§6.4): A₁ is 54 transactions, measured by the first copy point's verdict; A₂'s count is set from A₁'s allocation per transaction, and measured as I2's.
- **I5. A side branch and late enforcement (the D2-1 boundary, outside A2′).**
  1. Start S with `-sporkkey=<Core FT key>` and `RDASHD_TEST_HOOK=live-copy`. S mines A1..A10.
  2. A second rdashd, T, connects, syncs to A5, then disconnects (`disconnectnode` both ways) and mines C6..C9 (less work).
  3. `submitblock` C6..C9 to S. They are stored, not connected (`getchaintips` shows them as `valid-headers`/`valid-fork`). The log records all of these submissions.
  4. **Enable ChainLocks:** `spork-update SPORK_19_CHAINLOCKS_ENABLED 0` on S. Wait (timeout 30 s; the ChainLock handler's 5 s task, which rdashd's `cl-schdlr` thread posts since BL-626, `dashd-node/src/node.rs:693-713` → `chainlock_timer`, `chain/actor.rs:894-899`) until `cl_turned_on_reset == 1`: the off→on reset has run before the copy point, so later floors survive. `cl_turned_on_reset` stays 1 through the end of the test: a second reset in the interval would wipe the floor.
  5. `gettxoutsetinfo` (the copy point); keep `livecopy_counters`; list; pause the copier.
  6. **Late enforcement:** `cl-floor <C9> 9`.
     - Wait until `cl_enforced` has grown and `cl_enforced_floor == C9`, then until `getbestblockhash == C9`.
     - Enforcement disconnected A6..A10 and connected C6..C9: `disconnects` exactly +5 (one count per disconnect, from the common `InHold::disconnect_tip` only), `activations` +4, `undo_writes` +4, `flatfile_mutations` > 0.
  7. `flush`, then `quiesce`, then finish the copy.
  - **Assert:** the §2.2 check **fires** (at least `disconnects`, `activations` and `undo_writes` increased), and so do the enforcement events. The reopen outcome is recorded, not asserted.
  - **Positive control:** the same steps 1–5 and 7, with **no** `cl-floor`, and a 12 s pause in step 6, which covers ≥ 2 timer runs. Assert:
    - `cl_enforced_floor` unchanged (the timer's enforcement returns at `locks.rs:294-297`, since `best_known` is absent after the reset);
    - `cl_turned_on_reset` stays 1;
    - every §2.2 counter and super version unchanged;
    - the oracle passes at A10, with the twin replaying the full log, C6..C9 included.
  - **Known limit (Opus r1 O-4):** the control does not observe a timer run *during* the pause. A run with no best lock leaves no trace: `enforce_best_chainlock` returns at `locks.rs:294-297` before its only log line, and with no flip `CheckActiveState` counts nothing. The `Cleanup` half logs nothing either, and rdashd has no counter for the task. Only step 4 shows the task running (the reset it waits for comes from a timer run). That the pause covers timer runs rests on the 5 s period and the 12 s pause, not on an observation; adding product logging for it is out of scope.

#### 6.6 The mutant table

| Mutant | Forced event | The assertion that fails |
|---|---|---|
| **M1** copy-point flush skipped at `gettxoutsetinfo` (site.rs:329-337) | **I0** (no flush between baseline and copy, proven by `flush_jobs_done`) | I0(a): `R.height == 20` (it is 10); then I0(b): copy tip == T |
| **M2** barrier memtable flush skipped | The phase-2 call-order seam | `calls == [flush_memtables, wait_idle_purged, sync_wal]`; supplementary: I4 |
| **M2b** whole idle wait removed | The phase-2 call-order seam | The same sequence; plus the failure-propagation test (an injected `wait_idle_purged` error is not seen) |
| ~~`wait_for_purge` only~~ | — | Dropped (Q7); pinned by the raw-option test |
| **M3** `after_bk` flush left on | I1 step 3 (`flush.done` after AfterBk, then `quiesce`) | I1: `dir_state` and `super_versions` unchanged |
| **M4** WBM left on | L3; I2a + follow-up + `quiesce` | L3: L0 == 0; I2a: `dir_state` and super versions unchanged |
| **M5** WAL cap at the WBM size | As M4 (pre-write check on the follow-up) | As M4 |
| **M6** auto-compaction on | L5 | L5: L0 ≥ 4 and `compaction-pending == 1` |
| **M7** `live_copy: false` on regtest | The phase-3 selection test | `chainstate_profile(Regtest, None) == (for_regtest(), true)`; supplementary: I4 |
| **M8** W left at production size | I2a/I2b + follow-up + `quiesce` | `dir_state` and super versions unchanged; the verdict is valid |
| **M9** `async_wal_precreate` true | L5 | The OPTIONS pin (configuration only; not a plan-required mutant) |
| **M10** verdict computed **after** the reset (step 8 before step 2) | L2b | The verdict has `over_b == true` and `hwm > B` (after a reset it would show 0 / false) |
| **M11** super-version invalidation dropped | L2a | `valid == false`, and C4's second line is present |
| **M12** SstFileManager rate > 0 | L4 | No `*.trash`, and no old WAL, right after the barrier |
| **M13** `allow_concurrent_memtable_write` left true in the live profile | L5; T7's `for_regtest` golden; `live_path_isolation` (§6.7) | L5: `allow_concurrent_memtable_write=false`; T7: `concurrent_memtable_write == false`; isolation: the live engine's OPTIONS show false. Supplementary, probabilistic: L2c |
| **M14** live sampling or locking reachable on a non-live profile | `live_path_isolation` | Non-live: `live_lock_acquires == live_property_reads == live_state_updates == 0` |
| **M15** the reply's snapshot read when the reply is built (`CoinsStatsJob::live_copy()` reads `COUNTERS.tripwire()`; OD-CH5-22 (b)) | The phase-3 atomic-snapshot test: W parked at `AfterRelease` until V has processed and acknowledged the queued activation | The reply snapshot excludes the activation (under M15 it includes it, +1) |
| **M16** the common `disconnects` increment in `InHold::disconnect_tip` (`abc.rs:753-789`) omitted | I5 | `disconnects` exactly +5 (under M16 it is 0). I5 makes no claim about omissions at other increment sites it doesn't exercise |

#### 6.7 Other acceptance items

- **T7** (phase 2): literal field-by-field goldens of `RocksTuning::default().config(..)` at dbcache 4/300/1000 with indexes off and on: today's values, plus `concurrent_memtable_write == true`. A `for_regtest()` golden adds `live_copy == Some(W, B)` and `concurrent_memtable_write == false`.
- **`live_path_isolation`** (phase 3, `dashd-node`, D2-8 / OD-CH5-17).
  - For each non-live profile, `chainstate_profile(Mainnet | Testnet | Devnet, None)` and `chainstate_profile(Regtest, Some(StorageProfile::Production))`:
    - open a chainstate engine in a tempdir and commit 1 000 batches spread across all 11 CFs;
    - assert `probe.live_lock_acquires == 0`, `live_property_reads == 0` and `live_state_updates == 0`;
    - assert the OPTIONS file has `allow_concurrent_memtable_write=true`.
  - Then `chainstate_profile(Regtest, None)` with the same 1 000 batches:
    - `live_lock_acquires == 1000`;
    - `live_property_reads == 11 000`;
    - `live_state_updates ≥ 1000`;
    - OPTIONS has `allow_concurrent_memtable_write=false`.
  - The probe is per engine, compiled only under `cfg(any(test, feature = "test-hooks"))`, so parallel tests don't interfere and shipping builds have none.
- **Selection tests** (phase 3): main/test/dev → `(Default, false)`; regtest → `(for_regtest(), true)`, or `(Default, false)` with `-storageprofile=production`; the option is refused off regtest and a bad value on it (`storage_request_parses`, `storage_request_refuses_off_regtest`); the shared launcher's default start-up reports the live profile. Both profiles run in the selection tests and the live suite, and (BL-634) in dbcrash: P1/P2 per PR and the two rdashd reorg loops nightly, each launch checking the profile it ran by the start-up line and the chainstate's `OPTIONS` (`allow_concurrent_memtable_write`, `disable_auto_compactions`). `contrib/node/b4b-nightly.sh` fails a split or pressure log without exactly one `B4B-SUMMARY` line a storage profile (`NO-PROFILE`).
- **Barrier tests** (phase 2): call order; and failure propagation (a seam `wait_idle_purged` → `Err` means `live_copy_barrier` returns `Err`, `sync_wal` is not called, the latch is tripped, and the caller sees W's error).
- **Nightly** (supplementary): `contrib/node/livecopy/nightly.sh`, Core's `copytree` against a live test-hooks rdashd with relay and quorum-row requests, plus the oracle and the §2.2 check. A report-only variant lets blocks arrive and counts the deltas. At this head `getqrinfo` and `getmnlistd` are P6b deviations, so no quorum-row request reaches the snapshot writer (R17); the nightly's traffic is a dashd peer's relay (and, in the variant, its blocks), and the synthetic snapshot writer (§3.2) covers R17's rows. The source runs `-blocksonly=0` (OD-CH5-31: rdashd's default `-blocksonly=1`, DEC-322, would ask its peer not to relay transactions) and `-debug=net`. rdashd refuses an explicit `-maxmempool` (MP1b, `dashd-node/src/init/support.rs:163`), so the source also loses `-blocksonly`'s soft `-maxmempool=5` (`init/interaction.rs:79-85`): its coins budget takes the default 300 MB mempool share, all unused since rdashd has no mempool (`node.rs:820`, :841-843), instead of 5 MB. W and B are unchanged. Any traffic RPC error fails the run, and only successful submissions and blocks are counted. While the copier is held in the interval, a known transaction K must reach the source, and the receipt is K's own (Sol r2 C-2, option (iii)). `received:` lines name no txid and no existing source line names a received transaction, so the background traffic starts only once K's receipt holds, and the receipt (`tx_receipt`) requires the source's net log to hold exactly one transaction inv, K's, and exactly one transaction body, after it and from the same peer. Nothing else was ever offered to the source, so no other body can stand in for K's. `the_known_tx_receipt_needs_ks_own_body` checks that the receipt fails with no body, a body before K's inv, a body from another peer, another transaction's inv and body before K's, or a second body. The blocks variant asserts that receipt and its counts, and only reports its copy outcome.
- **The measurement** (§3.2) is a READY gate.

---

### CH5-5 7. Divergences from Core

| # | Difference | Core cite | Row? |
|---|---|---|---|
| V1 | Regtest's engine profile, including no parallel memtable insertion, differs from other networks, unless `-storageprofile=production` (D15) | `src/dbwrapper.cpp:113-125` | No (internal tuning). If CH2c's figures reach an RPC on regtest: possible row, owner CH2c |
| V2 | rdashd promises more than LevelDB gives during a copy | `src/leveldb/db/db_impl.cc:1322-1378` | No |
| V3 | The C4 lines, plus in test-hooks builds the verdict lines, `livecopy_counters` and `livecopycounters` | `test_node.py:494-511` | No FT compares unlisted log lines. The RPC additions exist only in test-hooks builds under `RDASHD_TEST_HOOK=live-copy` (OD-CH5-27 (b)); the M7 gate runs are the only FT runs that set it, and they read the field only through the adapter. If a compared log ever includes C4: possible row, owner CH5 |

---

### CH5-5 8. Owner rulings applied

- **Q1–Q8:**
  - Q1: the caller contract. It excludes any explicit flush point during a copy, a second copy point included (OD-CH5-25);
  - Q2: R22 and L6, with postconditions;
  - Q3: no shipping option, a test-hooks seam; amended by BL-634 (OD-CH5-32, DEC-1070): the regtest-only, debug-only `-storageprofile` replaced the seam (D15);
  - Q4: provisional numbers, the READY measurement, the M7 row, the synthetic writer;
  - Q6: no production warning;
  - Q7: call order;
  - Q8: `sync_wal`.
- **Q5, the func-compat adapter** on `node/func-compat-v24.0` (BACKLOG, owner CH5, a prerequisite of the M7 live flow).
  - tf:637-638's two `rmtree`s become `rmtree_if_present(p)`: `try: shutil.rmtree(p)` / `except FileNotFoundError as e: if e.filename != p: raise`.
  - It carries §2.2's counter check on `rust_nodes`.
  - Its tests: a Core layout (both directories present → removed); an rdashd layout (both absent → no error); a permission error inside `wallets/` → raised; a fake node returning a `livecopy_counters` delta → the tripwire fails.
- **OD-CH5-14:** row fingerprints plus the full-log twin.
- **OD-CH5-15:** postconditions plus fail-closed property reads.
- **OD-CH5-16:** PF0 may measure the live profile and re-anchor once.
- **OD-CH5-17:** live-only write-path work, tested by `live_path_isolation`.
- **OD-CH5-31:** the nightly's source runs `-blocksonly=0`, test-only, so its dashd peer relays transactions to it; dashd binds its port explicitly with no onion service (§6.7).
- **OD-CH5-30:** after CH7b-2 (T92) the evo fixture dropped its 240 refusal probe; BL-645 then restored D2-3 as designed (§6.3.3).
- **OD-CH5-41:** the storage-corruption case writes the height-5 row verbatim under the 576 key; the node fail-stops at start's tip read (§6.3.4).
- **OD-CH5-42:** the deleted update-height `mn_diff` row is caught by step 3 alone; step 2 passes and 577 is accepted (§6.3.4).
- **OD-CH5-43:** `credit_pool` is asserted from V20 (pool rows exactly V20..=T, funds locked; credit-data balance non-zero exactly from V20), and the test declares `DISK_SNAPSHOT_PERIOD` = 576, checked against the snapshot rows (§6.3.3).
- **OD-CH5-29:** I5 step 4 runs on the ChainLock handler's 5 s task (BL-626); the `cl-intake` hook (OD-CH5-26) is retired.
- **OD-CH5-28:** dbcrash and ST3b on the production profile are BL-634; §5 and §6.7 state what runs. BL-634 landed dbcrash on both profiles; ST3b adds `-storageprofile=production` and the profile label in its own scripts.
- **OD-CH5-27:** (a) W = 64 MiB and B = 48 MiB accepted; the 8× rule excludes workloads sized from another budget (§3.2's exclusion list), which still meet every other gate. (b) The live-copy RPC additions are runtime-gated by the seam, besides the compile-time cfg (§6.2).

---

### CH5-5 9. Implementation phases (each gated by its own tests)

1. **dashd-store.**
   - Work: the `LiveCopy` profile (through `quiet`, plus the §3.2 settings); `live_barrier` (lock, samples, verdict, SV check, no resample); the raw `wait_idle_purged`; R22 with `prop_u64`; `ProbeCounters`; `livecopy::Counters` and the flatfile increment sites.
   - Tests: L1, L2a, L2b, L2c, L3, L4, L5, L6, the raw option, the calibration; M4–M6 and M8–M13 at engine level.
   - **Gate:** `cargo test -p dashd-store` and the crate's clippy.
2. **dashd-chainstate.**
   - Work: `for_regtest`, `EngineTuning::{live_copy, live_copy_barrier}` with the snapshot closure, the no-op `after_bk`, mach:936-938, and the activation/disconnect counter sites.
   - Tests: T7 (both goldens), call order (M2, M2b), failure propagation; the existing machine and control tests stay green.
   - **Gate:** `cargo test -p dashd-chainstate`.
3. **dashd-node and the hooks.**
   - Work: `chainstate_profile`; the seam; the §6.2 hooks (`flush`/`JobDone`, `quiesce`, `spork-update`, `cl-floor`, events, `verdicts`); the `livecopy_counters` reply field and the `livecopycounters` RPC (test-hooks only).
   - Tests: selection (M7), the launcher default, `live_path_isolation` (M13, M14), acknowledgement, quiesce, the atomic snapshot (M15).
   - **Gate:** those tests, plus `cargo build -p rdashd --features test-hooks` and `cargo test -p dashd-node`.
4. **The oracle.**
   - Work: `dash-testkit::livecopy::rows`, the submission log and twin replay, the copier, the plain and evo fixtures.
   - Tests: twin vs twin, the damage cases, the positives, the fixture budget log.
   - **Gate:** all oracle self-tests.
5. **The process tests and the measurement.**
   - Work and tests: I0–I5 with the I5 positive control; M1, M3, M4, M5, M8 and M16 at process level; the §3.2 measurement (which fixes W and B in code and here); the nightly.
   - BACKLOG rows: the M7 measurement; `feature_llmq_chainlocks`'s counter gate; IX1; the Q5 adapter with the counter tripwire. All are owner CH5 except IX1.
   - **Gate:** READY = phase 5 green plus the measurement accepted.

---

### CH5-5 10. Code-review and READY conditions (OD-CH5-19)

The code reviews must confirm each condition below on the implementation, and READY evidence must show the listed results passing.

| Id | Code-review condition (checked on the implementation) | READY evidence |
|---|---|---|
| **D3-1** | `disconnects` is incremented exactly once per disconnect, in the common `InHold::disconnect_tip` (`chain/abc.rs:753-789`). `VHost::disconnect_tip` (`chain/locks.rs:432-436`) has no increment. The counter is behind `cfg(feature = "test-hooks")` | I5 shows `disconnects` **exactly +5**, and the M16 run (common increment omitted) shows **0** |
| **D3-2** | The damage case builds `MnList::new(hash_576, 576, original_total_registered)`, encodes it with `dash_dml::codec::encode_snapshot_record`, and asserts that `EvoCodec::decode_snapshot(value, &hash_576)` returns an empty list at 576 **before** writing. It closes the offline writer and uses a fresh process on an independent copy. An undamaged control copy accepts Core's block 577 (BL-645 restored this form) | On the damaged copy: the fingerprint difference, `submitblock` → **`bad-cbtx-mnmerkleroot`**, and a subsequent tip query returning **576**. The control accepts 577 |
| **D3-3** | A test-only sync point `AfterRelease` (`cfg(test)`, or an explicitly stepped model) sits right after Release is published and before any later W operation. W parks there until V has processed the queued activation and acknowledged its increment. The snapshot is taken in step 11, before it. Production scheduling is unchanged | The reply snapshot excludes the increment and the live read includes it. **M15** (snapshot after the sync point) is killed by the exclusion assertion (M15's form: OD-CH5-22(b), see the M15 row) |

## CH5-2a: `-reindex-chainstate` and `-stopafterblockimport`

Core lines in this section are at **v24.0.0** (OD-CH5-45); `i:N` = `src/init.cpp`, `bs:N` =
`src/node/blockstorage.cpp`, `dbw:N` = `src/dbwrapper.cpp`. Every cited line is the same at rc.5
but for its number; the process tests run against the rc.5 pin with `-txindex=0`.

| ID | Decision | Reason |
|---|---|---|
| D16 | **The chainstate wipe is Core's whole `-reindex-chainstate` wipe set.** `-reindex-chainstate` wipes rdashd's one chainstate database: `coins` and every evo column family (`mn_diff`, `mn_snapshot`, `credit_pool`, `mnhf`, `llmq_mined`, `llmq_mined_h`, `llmq_snapshot`), with `meta`'s `tip`, `evo_tip` and `head_blocks` (`wipe_chainstate`). That is Core's chainstate, evodb, `llmq/isdb`, quorum and recsig databases (`cs:233-236`, `i:2031-2033`, `cs:149`, `:160`); rdashd has no InstantSend, recsig, quorum or DKG store. `block_index`, `block_files` and `meta`'s `reindexing`, `last_file` and `pruned_files` are kept, as Core keeps its block tree. Step 11's ABC reconnects every block from the block files (`bs:950-956`). The wipe writes no coins batch, so `-dbcrashratio` never rolls in it, as Core's `DestroyDB` never reaches `BatchWrite`. | rdashd has one database for what Core keeps in several. |
| D17 | **The wipe runs at recovery's step 2, before the block index loads.** Core wipes the evodb and IS DB before `LoadChainstate` (`i:2031-2033`) and the coins after `LoadBlockIndex` (`cs:233-236`); rdashd wipes all of it at the chainstate's open (`ChainstateOpen::open_wiping`), before the index load, which reads the same database. Neither node logs between the two points but the index load's own errors, and a start that fails there is wiped again by the next `-reindex-chainstate`, so the order is observable only in what such a failed start leaves on disk: Core has wiped its evodb and IS DB but still holds its coins, while rdashd has wiped all of it. Both nodes refuse that start and wipe again at the next one, so no RPC or later start differs. Under the flag only the kept keys (`pruned_files`, `last_file`, `reindexing`, `wiping`; `Meta::read_kept`) are read before the wipe (OD-CH5-48): Core destroys its databases unopened (`dbw:144-147`), so a `tip`, `evo_tip` or `head_blocks` that does not decode is wiped, not refused, and `-reindex-chainstate` recovers a corrupt coins or evo meta on both nodes. A kept key that does not decode is still corruption, before anything is written. | Recovery opens the database once; the index load reads it after step 2. |
| D18 | **The wipe resumes, and a stored reindex is not wiped.** The wipe's `wiping` marker makes an interrupted wipe finish at the next start, with or without the flag (BL-72, logged "Resuming the interrupted wipe of the chainstate" only for a set marker); Core's `DestroyDB` is not resumed, and its next `-reindex-chainstate` destroys again, so the end state is the same and rdashd's is reached without the flag too. Under the flag, a chainstate with `reindexing` set is not wiped (OD-CH5-47): the load then refuses it as for `-reindex` having written nothing (BL-528), where Core wipes for either flag (`cs:236`). CH5-3 lifts the guard when it serves `-reindex`. | The stronger resume costs nothing; the guard keeps BL-528's refusal write-free. |
| D19 | **Log comparisons leave out the storage lines and BL-718's.** dashd logs "Wiping LevelDB in …", "Opening LevelDB in …", "Opened LevelDB successfully" and "Wrote new obfuscate key for …" for each database (`dbw:145-158`, `dbw:180`); rdashd's RocksDB chainstate logs none. dashd's "Skipping evodb repair during reindex" (`i:2489-2490`) waits for EV2b's repair (BL-718); the tests pin that rdashd does not log it. The compared lines are the replay's `UpdateTip`s (without the cache sizes, CH5-1a D5) and the skipped "Loaded best chain" and VerifyDB lines. | Each node's storage is its own. |
| D20 | **`-stopafterblockimport`'s exit status races init.** The stop comes after a successful ABC only (`bs:958-962`), with `fImporting` set, then `LoadMempool` is skipped and `loadblk` continues (`i:2453-2491`). rdashd has no mempool load yet; its `after_import` still runs the MP1 slot after the stop (empty today), so whoever wires `LoadMempool` there must skip it after a stop, as Core does (BL-724). Init waits for genesis only, then checks `ShutdownRequested()` once (`i:2570-2573`): a stop before it exits 1 ("Shutdown requested. Exiting.", `bitcoind.cpp:279-283`), after it 0. On a restart with a loaded tip the ABC has nothing to connect and the stop lands next to that check: both nodes exited 0 in 3 of 3 runs, but only the line and the tip are pinned. Under `-reindex-chainstate` the replay outlasts init's shutdown check, and the exit 0 is pinned; the stop still lands within tens of milliseconds of "Done loading" on dashd (15–35 ms in 8 runs, either side possible on a loaded box), so that order is not compared. rdashd mirrors the check and the text (`node.rs`). | The race is Core's. |
| D21 | **The "VerifyDB not skipped" mutant is equivalent (OD-CH5-46).** `coins_empty` ORs `-reindex` and `-reindex-chainstate` as Core's `is_coinsview_empty` does (`cs:415-417`), but after the wipe the coins tip is `None`, so the third term is true alone and `verify_loaded` returns before VerifyDB. In Core the same holds: the wiped coins' best block is null, and VerifyDB on a null tip returns before its first line (`v:5051-5053`, `:5066`). The mutant is recorded as equivalent, not planted. | No input tells the OR from its absence. |
| D22 | **The crash tests compare each node with the uncrashed reference, not the two crash nodes' roll points.** At `-dbcache=4` rdashd's first cache-pressure flush in the replay comes earlier than dashd's (CH3b-notes D225's pressure-flush difference), so the "Rolling forward" lines' count differs; each round crashes inside the replay (rdashd's height varies between starts with its coins salt; since BL-723, CH3b-notes D236, the test fixes the salt under `test-hooks` and holds each crash node to its own height, the same in both rounds), rolls forward only (no "Rolling back") to the crashed flush's block, and reaches the reference's tip, tips and UTXO set. The net caches are compared by their log lines, the managers' summaries with every count kept (only the trailing "  <n>ms" durations are removed, `flat-database.h:70`, `:154`, `:202`), and under the flag each summary's counts are asserted to be 0: rdashd's flat files use its own layout (SP1 D3); the populated-cache clear is pinned in the node's lib tests. | Flush timing is each node's own. |
