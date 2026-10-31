# CH5 rev0 plan review — Sol r1

(Durable copy of /work/scratch/ch5/reviews/plan-r1-sol.md; three words reworded for the docs commit gate, content unchanged.)

Reviewed `/work/scratch/ch5/PLAN.md` against Core **v24.0.0-rc.5** (using `git show`, without checkout), the requested program rows and decisions, and the integration worktree. The integration worktree was at `565137d977` when inspected, rather than the plan's older `adc2587a0`; the relevant baseline claims still largely hold. No repository files were changed, no GitHub operations were performed, and no managed chain, benchmark, cache or restricted tree was accessed.

## Findings

### R1 — major — PLAN.md:35, 168–170: the proposed Core memory counter uses the wrong allocation model

Core's coins map does **not** use the ordinary unordered-map accounting implemented by `reorg/memusage.rs`. At rc.5, `coins.h:135–150` makes it a `PoolAllocator` map; `memusage.h:186–202` counts retained pool chunks, their list nodes, and buckets. `coins.cpp:40–42` adds script allocations. Pool chunks survive erases (`support/allocators/pool.h:231–247`) and an empty resource already owns a chunk (`:174–187`). Current entry counts and script capacities cannot recover that history. The existing disconnect-pool map formula charges individually allocated nodes instead.

Concrete failure: a scratch/tip cache that previously grew and erased entries can exceed Core's L3 budget while the proposed entry-based counter says it fits. rdashd then returns success or attempts L4 where Core returns `SKIPPED_L3_CHECKS`; explicit startup checks can start where Core refuses with insufficient dbcache.

**Fix:** specify a coins-specific logical accounting model covering pool allocation/reuse/high-water history, Core bucket history, Core's FRESH/DIRTY/clean-entry operations and script capacities. A shadow of Core's cache history may be necessary because rdashd's native cache and flush histories differ. Reuse `MallocUsage` and bucket helpers where applicable, not the ordinary map formula. Add an rc.5 accounting differential and exact budget-boundary tests; a merely large `-dbcache=4` chain will not kill `<=` → `<`.

An isolated C++ witness compiled against the **unmodified rc.5 allocator and memusage headers**, under `reviews/evidence-sol/`, passed:

```
new     entries=0     buckets=1     chunks=1 CoreDynamicUsage=262240
grown   entries=10000 buckets=10273 chunks=5 CoreDynamicUsage=1393248
cleared entries=0     buckets=10273 chunks=5 CoreDynamicUsage=1393248
```

The key/value stand-ins reproduce the relevant layout; this is an allocator/accounting witness, not a full-node process test.

### R2 — major — PLAN.md:34, 61, 169: the RPC parent is not read-only in Core

Core does isolate coin **mutations** in the scratch view, but its parent `CoinsTip()` can acquire clean cache entries. `VerifyDB` constructs `CCoinsViewCache coins(&coinsview)` (`validation.cpp:5066`); on RPC that parent is `CoinsTip()` (`rpc/blockchain.cpp:1438–1440`). `CCoinsViewCache::GetCoin` calls `FetchCoin`, whose parent lookup and emplacement cache a hit (`coins.cpp:44–65`). Reads through scratch therefore populate the parent too. Core evaluates both layers' usage before each disconnect (`validation.cpp:5119–5124`).

Concrete failure: verification reads a coin absent from the tip cache but present in the DB. The plan's read-only parent leaves the tip usage unchanged while Core retains a clean copy. Later L3 gates, a subsequent verification, and cache-size/flush behavior can differ. Counting the native active layer alone does not fix the missing Core read history.

**Fix:** preserve authoritative coin values while reproducing the parent clean-cache effects, either directly or through the logical Core cache model. Add cold-parent DB-hit tests and repeated calls near the budget boundary, without a `gettxoutsetinfo` flush between the calls. Keep startup's parent as the DB, which is correctly distinguished in the plan.

### R3 — major — PLAN.md:34, 36, 39–44, 61: “with its effects” cannot be verified by unchanged healthy RPC answers

The proposed audit omits observable effects outside EvoDB, and the acceptance requires pre/post equality where Core does not promise it. EvoDB rollback (`evo/evodb.cpp:17–34, 69–97`) does not undo arbitrary manager state or another database.

At rc.5:

* L3 and L4 call `NotifyMasternodeListChanged` on the active chainstate (`validation.cpp:2379–2382, 2929–2932`). `CMNAuth::NotifyMasternodeListChanged` can set a peer's `fDisconnect` when a historical key update/removal is replayed (`evo/mnauth.cpp:179–208`). Undoing an operator-key update can disconnect a peer authenticated with the current key; reconnecting scratch coins does not restore that connection.
* L4 executes the ChainLock conflict check (`validation.cpp:2527–2530`) and the non-IBD IS filter. A ChainLocked block can remove a conflicting IS lock and its children (`:2812–2826`; `evo/chainhelper.cpp:93–98`; `instantsend/instantsend.cpp:342–352`). Those changes are not in the rolled-back EvoDB transaction.
* `CDeterministicMNManager::ProcessBlock` calls `ResetPlatformBan` for service updates (`evo/deterministicmns.cpp:734–748`; `masternode/meta.cpp:187–195`), another non-EvoDB effect.
* L4 really invokes undo writing and conditional validity/dirty-index updates (`validation.cpp:2899–2913`), rather than having an unconditional scratch suppression. Existing undo is a no-op after a successful L3 read, but blanket suppression of validity writes needs a proof for the supported loaded states, including failed/lower-validity index records. Script-cache probes/removals (`validation.cpp:2181–2186`) and the IBD latch also are not EvoDB state.

Concrete failure: an implementation suppressing these effects passes the listed `protx`, `quorum list`, chain-info and UTXO battery yet leaves a peer connected, an IS lock present, or metadata unchanged after Core changed it. The current battery also exercises only successful L4, missing L3-only, skipped, interrupted and failed verification states.

**Fix:** complete the effect table before claiming this slice's acceptance. Separate rollback of coins/EvoDB from Core's retained cache, signal, metadata, lock, script-cache and index effects. Test equality **with Core's observed transition**, not universal equality with pre-call state. Include key-change/removal with an authenticated peer, appropriate CL/IS cases, level 3 alone, skip and failure/interrupt exits. Record prerequisites and defer unavailable cases explicitly rather than considering the healthy L4 battery sufficient.

### R4 — major — PLAN.md:55–57, 63, 72, 80: the proposed negative fixtures do not exercise the promised checks

There are two distinct gaps:

1. A wrong coin value for an output created within the disconnected range is detected by **L3**: `DisconnectBlock` compares the stored output, height and coinbase bit with the block (`validation.cpp:2342–2352`), returns UNCLEAN, and VerifyDB returns at `:5141–5143`, before L4. It cannot produce the specified L4 `found unconnectable block`. A wrong unrelated coin might produce no failure at all. Thus this fixture does not establish L4 checking or kill the level-4-to-3 mutant.
2. Merely mining with BIP34 disabled and enabling it earlier at restart does not make Core-mined coinbases invalid. The miner writes the height unconditionally before DIP3 (`node/miner.cpp:274–280`). Both full and chainstate reindex can accept that chain. The proposed m-2 example therefore does not establish the contextual-check skip.

**Fix:** name and pre-prove actual fixtures against rc.5. For L4, use a valid-checksum undo mutation that leaves L3 clean but restores an input with a script/value that fails reconnect, or another demonstrated L4-only failure. Assert L3 succeeds and L4 fails. For m-2, submit a deliberately constructed, solved pre-BIP34 block with a wrong height prefix while BIP34 is disabled and DIP3 inactive; then enable BIP34 at the earlier height. Assert the old-parameter chain is accepted, chainstate replay accepts it under the new parameters, and fresh full reindex rejects it for `bad-cb-height`. Core-generated normal mining alone is insufficient. Include a separate header-context fixture/mutant for the `ContextualCheckBlockHeader` half of m-2.

### R5 — major — PLAN.md:83, 86–90, 105: a stored reindex flag does not request a fresh wipe

The plan conflates a user-requested fresh reindex and resumption. Core initializes `fReindex` from the CLI (`init.cpp:1976`). Only the explicit option wipes the block tree and sets the marker (`node/chainstate.cpp:155, 169–175`); coins/EvoDB/early IS/LLMQ wipes likewise use the startup options (`:149–160, 233–236`; `init.cpp:2041–2043`). With only a stored flag, `LoadBlockIndex` enters its `!fReindex` branch, loads the persisted index/file info, discovers the flag inside `LoadBlockIndexDB` (`validation.cpp:5336–5389`; `node/blockstorage.cpp:362–403`), and completes that branch. DEC-914 explicitly corrects the earlier contrary description.

Concrete failure: after an interrupted reindex with some index and genesis/coins state persisted, a restart without `-reindex` wipes that state again in rdashd. Core retains it, skips already-known block bodies during scanning (`validation.cpp:5518–5535`), and resumes. Loaded flags, validation status, progress logs, undo positions and subsequent behavior can differ. A final-tip state hash alone cannot distinguish restart-from-zero from resume.

**Fix:** distinguish explicit fresh rebuild, incomplete wipe recovery, and stored-flag scan resumption. A durable local wipe-intent protocol is reasonable for rdashd, but `reindexing=true` must not imply another wipe once that phase completed. Preserve the combined flag at BL-549's later sites, without applying it retroactively to every earlier wipe. Add a restart fixture with persisted index/file info and already-accepted records, and assert retained records/positions plus Core's already-have behavior. Reset fresh-reindex metadata such as `meta.last_file` along with block-file/index records; the existing chainstate wipe does not do that.

### R6 — major — PLAN.md:98, 106, 172: fresh `-reindex` rebuilds undo offsets; it does not append after old undo data

Keeping the old rev **files** is different from preserving their allocation cursors. A fresh reindex destroys `blocks/index` (`node/chainstate.cpp:155`), so it loses `CBlockFileInfo.nUndoSize`. Reimport rebuilds the file-info entries with zero undo size (`node/blockstorage.cpp:665–668, 706–708`). `FindUndoPos` starts at that logical size (`:728–735`), not filesystem EOF. `FlatFileSeq::Open` uses `rb+` and seeks to the specified position (`flatfile.cpp:34–53`). Fresh undo overwrites from offset zero. Older completed files can be finalized/truncated too (`node/blockstorage.cpp:793–794`; `flatfile.cpp:82–99`).

Concrete failure: appending at old EOF duplicates undo, gives different index positions and file bytes, and retains obsolete records where Core rebuilt them. The normal-chain byte comparison is useful but can hide overwrite versus append mistakes if the fixture/setup is inadvertently fresh or empty.

**Fix:** replace the append/never-truncate premise with logical file-info reconstruction. Test starting from nonempty rev files, repeated fresh reindexes, out-of-order/fork history and more than one blk file. Keep byte equality for identical starting files, options, block ordering and completion point, alongside logical undo offsets/checksums. Stored-flag resumption preserves the loaded cursor, unlike fresh reindex (R5).

### R7 — major — PLAN.md:95, 105–107: the flag-clear mutant marks Core's own order as wrong

Scope correctly places clear → genesis retry → `-loadblock` → final ABC, but the mutant list requires killing “the flag cleared before the ABC.” Core explicitly clears at `node/blockstorage.cpp:922–923`, retries genesis at `:926`, imports external files at `:929–942`, and runs final ABC at `:949–955`.

Concrete failure: an implementer can satisfy this mutant only by moving the clear after final ABC, changing reindex state during connection and the restart behavior of a kill between flag clear and connection. The planned crash points all stop before clear and cannot establish the required transition.

**Fix:** mutate clear-before-**completion-of-the-blk-scan**, not clear-before-ABC. Add post-clear/pre-final-ABC and mid-final-ABC crashes; restart must follow the non-reindex load/recovery/ABC path. Describe persistence ordering of the reconstructed index/file metadata and the marker explicitly. Also distinguish rdashd's proposed stronger durability from Core's literal calls: Core wipes the fresh block-tree DB before `WriteReindexing(true)`, and `WriteReindexing`/erase use the default non-sync `CDBWrapper::Write`/`Erase` (`txdb.cpp:188–193`; `dbwrapper.h:330–335, 370–378`). A separate durable wipe phase can improve crash recovery without reversing the observable import phases.

### R8 — major — PLAN.md:113–122: atomic renames do not define a resumable, durable import transaction

The move-aside protocol has no durable intent/completion state or specified lock lifetime. Each rename being atomic does not make the five-directory migration atomic or durable. After the last Core directory is renamed, ordinary Core detection can stop recognizing the source before a durable reindex intent exists. A crash there can start an apparently new rdashd DB beside the old blk files without importing them. Earlier crashes can also leave backups present while source directories are missing; fixed rename order alone does not identify the correct backup or distinguish completion from an interrupted migration. A power loss additionally requires directory persistence, not just successful `rename()`.

There is a concrete startup integration obstacle too: `init::init_config` invokes `refuse_core_base_dir` before selecting the network and invokes `refuse_core_dir` again during selection (`dashd-node/src/init/mod.rs:59–67`). The base helper scans and refuses **regtest too** (`datadir.rs:367–383`). “Keep refuse_core_base_dir” unchanged would prevent the new auto-import from running.

**Fix:** specify a journal/state machine outside the directories being moved: chosen backup ID, original directory set, migration/reindex intent, completion phase and idempotent recovery. Persist intent and parent directories before destructive phase transitions; fsync affected source/destination directories after renames. Hold the selected Core-compatible datadir lock throughout detection, migration and transition to open; a busy source must fail without moving anything. Route both early refusal hooks so selected regtest can reach this protocol while other-network protections remain. Test every rename boundary, the last-rename/pre-DB-open gap, existing backups, source lock contention and modeled power-loss persistence, not just one process kill.

### R9 — major — PLAN.md:126–134: large memtables plus no auto-compaction do not keep the file set stable

Current production tuning has other SST-creation paths. `RocksTuning::config` installs a charged write-buffer manager and `max_total_wal_bytes=mem.wbm` (`dashd-chainstate/src/tuning.rs:168–183`); the store applies both (`dashd-store/src/rocks.rs:216–223`). More directly, **every full Bk** calls `RocksTuning::after_bk`, which flushes memtables (`flush/machine.rs:760–763`; `tuning.rs:215–217`), independently of the `live_copy` flag. Making individual memtables huge and disabling compaction does not disable these paths.

Concrete failure: after the copy-point reply, busy writes trigger an ordinary chainstate flush. `copytree` lists a directory before the resulting SST exists and copies the MANIFEST after it references the SST. The copy cannot open despite the proposed two settings. An opens-only unit also does not establish consistency with copied blk/rev files or correct recovery of a split coins/EvoDB flush.

**Fix:** specify how regtest suppresses or safely handles all file-generation/deletion triggers during the promised copy interval: the WBM/WAL triggers, `after_bk`, normal flushes, and another explicit flush point. If the uncoordinated-copy contract cannot ensure a stable generation, use a protocol that pins a consistent copyable generation and its files. Add a deterministic interleaving with a paused directory enumeration, ordinary full flush, then MANIFEST copying. Reopen through the node's recovery path and verify block availability, coins/EvoDB consistency, state hash and later continuation, not just RocksDB open. Three successful nights are supplementary to this deterministic case.

### R10 — major — PLAN.md:28, 53, 61, 78, 136–138: positive evo and second-segment gates have unstated prerequisites

The ordered slices name only CH3b/ST2 as merged dependencies, yet require successful production replay/verification of a chain with non-null quorum commitments and an evo StateDigest. Current integration still returns `SpecialTxFailure::NotReady(QUORUM_VALIDATION)` at G1q (`dashd-chainstate/src/evo/node.rs:500–517`) and explicitly refuses post-DIP3 state hash fields (`:1239–1248`). CH7b-2 owns production quorum capability/G1q removal; CH7a-5 owns those state hash fields (PLAN CH7a/b rows). Mapping NotReady correctly under BL-440 is necessary, but cannot make these positive tests pass. DEC-834 also places Q10's second segment after CH5 **and P3**, whereas the measurement slice is scheduled immediately after CH5-2.

Concrete failure: following the slice gates as written either leaves CH5-1/2 unable to reach READY, or encourages bypassing a readiness refusal / using a test double while claiming the specified process comparison. The same issue applies to CL/IS effect checks until their production hooks exist.

**Fix:** explicitly split runnable non-evo/refusal acceptance from the positive evo/effects follow-up, with named CH7a-5/CH7b-2 and relevant CL/IS dependencies and an owner/completion gate. Do not remove BL-524's remaining positive-parity claim until its stated coverage actually runs. Put P3 and the required evo capabilities on the second-segment measurement gate; keep pre-DIP3 counters independently runnable. No new consensus capability should be silently implemented in CH5 to satisfy these tests.

### r11 — minor — PLAN.md:116: the copy direction is reversed

The program acceptance and A1 require **dashd source → rdashd destination**. This sentence promises rdashd node 0 → dashd. Live-copy safety does not make rdashd's RocksDB schema readable by Dash Core's LevelDB loader.

**Fix:** correct the direction and test the actual `dynamically_initialize_datadir` and `copy_datadir` flows, as well as rdashd → rdashd live copies. A reverse-format export would be separate work.

### r12 — minor — PLAN.md:36, 96, 107: make the precise reconnect/log path unambiguous

Core returns on `pindexFailure` before reconnect (`validation.cpp:5141–5143`); L4 advances from the loop's last `pindex`, the predecessor of the disconnected range (`:5157–5165`), not from an UNCLEAN “failure point.” Also the principal reindex/import/loaded-count lines are unconditional `LogPrintf` (`node/blockstorage.cpp:914, 924, 933, 940`; `validation.cpp:5623`), whereas out-of-order diagnostics use REINDEX. Sending every import line only to REINDEX would hide required messages without `-debug=reindex`.

**Fix:** name the saved predecessor and explicit UNCLEAN early return, and separate unconditional from category-gated logs. Test depth 1/full depth and debug-category off/on.

## Answers to Q1–Q5

**Q1. Use Core accounting, but not the proposed entry-count formula.** R1 and R2 are required corrections. The exact formula is `(MallocUsage(3 pointers) + MallocUsage(chunk bytes)) * retained chunks + MallocUsage(pointer bytes * bucket_count) + cached script usage` for each Core coins cache. Track Core operations/history, including read fills of the RPC parent, erases that retain chunks/buckets, fresh-entry deletion and cache reallocation. The existing ordinary disconnect-pool model is insufficient. Startup parents on `CoinsDB`; RPC parents on `CoinsTip`; both still include `CoinsTip` usage in the gate. Core compares before disconnect and permits equality. A logical-counter oracle test plus exact-boundary integration/seam cases is needed.

**Q2. No, the list is incomplete; yes, manager caches survive the rollback and later readers use them.** In addition to R3's observable signals/IS/metadata effects:

* Dmn undo erases list/diff cache entries (`evo/deterministicmns.cpp:793–801`); connect repopulates them (`:729, 751–753`). Subsequent list RPC resolution uses those caches (`:843–858`). Cache misses can also establish `m_initial_snapshot_index` (`:882–887`).
* Quorum undo recomputes members with reset, invalidates caches and adds mineable commitments (`llmq/blockprocessor.cpp:523, 550–556`); connect drops caches and removes mineable entries (`:443–452`). L3 alone leaves the mineable pool populated even though DB rows roll back. `quorum dkgstatus` reads mineable commitments (`rpc/quorums.cpp:429–439`); already-mined filtering can mask the difference until a later operation. It is not sound to assert all post-call outputs must change, nor that all effects disappear because the healthy battery is unchanged.
* MNHF processing fills `mnhfCache`, including during historical reconstruction (`evo/mnhftx.cpp:218–250, 334–381`); credit-pool lookups/construction fill `creditPoolCache` and static per-block data caches (`evo/creditpool.cpp:74–112, 122–170`). Their EvoDB snapshot writes roll back; their C++ cache containers do not.
* BLS restoration is correct at rc.5: VerifyDB's outer guard is at `validation.cpp:5055`; the startup V19 handling/reset is exactly `node/chainstate.cpp:429–465`. `ResetBlockFailureFlags` is a **startup** post-VerifyDB effect, not a `verifychain` RPC side effect.

It is unnecessary to duplicate every internal optimization if behavior is provably equivalent. The plan must establish that equivalence for later reads/transitions and preserve the observable effects, including early exits, rather than infer it from EvoDB rollback.

**Q3. Byte equality is a useful controlled-fixture bar; the append/never-truncate premise is false.** See R6. Compare equivalent starting files, flags, import ordering and completion stage; include logical offsets, checksum-correct undo and repeated/multi-file reindexes. Fresh and stored-flag reindex have different cursor histories. Blk bytes should remain unchanged when positions are supplied.

**Q4. Read the actual key for automatic move-aside; do not authorize a move from comparator/name heuristics alone.** A generic LevelDB can use `leveldb.BytewiseComparator`, and RocksDB often uses that comparator name too. Preserve explicit rdashd marker/IDENTITY discrimination for half-created stores. The serialized Core key includes the string length prefix before the leading NUL; `CDBWrapper::OBFUSCATE_KEY_KEY` has length 14 (`dbwrapper.cpp:228–234`). The reader must consider live tables **and WAL**, with normal sequence/tombstone rules, and avoid opening/recovering the source in a mode that writes. `DB::Open` with Core's normal options is not a read-only probe (`dbwrapper.cpp:139–157`). Older non-obfuscated/nonempty stores can legitimately lack the key (`:166–177`); an ambiguous database should be refused intact rather than treated as ours or automatically moved. That fallback preserves safety without broadening automatic detection beyond A1.

**Q5. Keep CH5-4 after a working CH5-3; merging them is optional, not a correctness requirement.** Auto-import depends on the real reindex/resume machinery and R8's crash protocol. Starting it earlier with a second temporary replay path would weaken the evidence. The important acceptance ordering is to run the 199-block cache and real dashd-source copy flows only after that machinery exists. CH5-5 can be developed separately, but the copied-node recovery test must compose with it.

## Citation, baseline and acceptance audit

The listed rc.5 ranges for VerifyDB, VerifyLoadedChainstate, defaults/help, RPC, ThreadImport, external scanning and `-dbcrashratio` resolve to the named functions/behavior. The material errors are the behavioral inferences in R1–R7, not a wholesale rc.4/rc.5 line drift. In particular: Core runs non-`fJustCheck` ConnectBlock at L4; `require_full_verification` is set by explicit presence of either check argument; default checks can continue after L3 skipping; genesis-only VerifyDB returns before disk reads; and `-stopafterblockimport` runs after ABC while `fImporting` remains true. The partial-batch crash roll is after `WriteBatch`, including during recovery replay, never the final consistency-marker batch (`txdb.cpp:152–171`); retain both production flush and recovery wiring and test ratio 1 with and without a partial batch.

The incompatibility citations are correct: prune checks precede the coinstats/filter/txindex chainstate-reindex checks, whose order is coinstats → filter → txindex (`init.cpp:1248–1259, 1373–1384`). Tests need multiple-invalid-option cases to establish precedence against rdashd's support table. The scanner citations correctly cover unknown-parent recording, `AcceptBlock` with `dbp`, genesis ABC, child BFS and error/skip handling. Exercise multiple blk files, unknown-parent children across files, known records, malformed records, missing/open-failed input and shutdown; the existing scanner tests alone do not prove the new sink integration.

The §0 spot checks agree on the stubbed L3/L4, existing reset, support-table refusals, stored-flag refusal, no production scratch implementation, scanner/accept seam, ABC-only import thread, filename-based datadir probe, `live_copy=false`, default non-quiet production RocksDB and hard-coded zero crash rolls. One important additional fact is that existing `NodeEvo::commit_tx` already puts successfully reconstructed lists into the shared list cache for scratch attempts (`evo/node.rs:1212–1230`): “never CoreConnector state” must not be interpreted as suppressing all retained manager/cache effects. The named steps file is `dashd-node/src/steps.rs`.

BL-524, BL-119, BL-440, BL-278(b), BL-275, BL-549, BL-528 and BL-29 belong in the named slices. BL-119's code is already present; the real-startup regression remains valuable. BL-275's genesis case follows Core's early return. BL-507 must remain a **pending device/VM acceptance gate** after implementation, not close on a ping or process kill test; the model case is independently required by M3 exit 2. Deferring prune execution to CH8 is consistent with the current refusal. The v23-only BLS/profile behavior can remain out; rc.5's guard really restores the scheme.

The func-compat branch has the claimed 13 cache entries plus `rpc_invalidateblock` and `feature_loadblock`. Exporting linearize is necessary. Deferring full `feature_reindex` to IX1 and `feature_dbcrash` to MP1 is a real dependency, not proof those CH5 PLAN acceptance FTs pass: retain their pending status and run them unadapted once dependencies land. The interim process tests should reproduce reorg/restart and repeated crash-during-recovery behavior from `feature_dbcrash`, not merely one crash followed by a healthy restart. Tier-1 completion/M3 exit 5 must not be claimed from owner changes in the expected-failure list. DEC-594's cache-blocked tests are correctly assigned to CH5-4; DEC-698 correctly assigns `-dbcrashratio` and reindex crash coverage here. DEC-834/835/873 measurements are correctly identified but need R10's prerequisite gate. DEC-909/914 and BL-549 require combined-flag later startup behavior **without** R5's fresh/resume conflation.

No full-node process tests were run: this is an unimplemented plan, and no executable exists at the inspected Core checkout's `src/dashd`. The rc.5 allocator witness above was run successfully; all other behavioral evidence is cited source tracing. No unrelated suite results are presented as validation of future code.

## Verdict

**REWORK**
