# ND1 persistence (2b-2 on CH2b b4), Opus-h review round 1

**Head reviewed:** 2b5bf2a7a on `node/nd1-2b2-persist`, a follow-merge of `node/train-21` ea790e9bf
**Reference:** Dash Core v24.0.0-rc.3 (`git show v24.0.0-rc.3:…`)
**Scope:** `git diff ea790e9bf 2b5bf2a7a` (6 files in `dashd-node/src`, +310/−56); read-only. Implementer notes: `/work/scratch/nd1/plan-2b.md` 2b-D4, 2b-D6, 2b-D8, 2b-D24, 2b-D25.

## Method

I read the full diff and these sources at the reviewed head:

- Node: `dashd-node/src/steps.rs` (`load`, `shutdown`, `force_flush`), `chainstate.rs` (`Opening::block_files`, `Chainstate::attach_parts`, `force_flush`), `chain.rs` (`open_parts`, `Chain::{attach_coins, stop}`), `node.rs` (`shutdown`), `sut.rs` (`restart`, the new test).
- dashd-chainstate: `chain/load.rs` (`load_chain_tip`, `check_startup_readiness`), `chain/actor.rs` (`VExit`, `stop_flush`), `chain/view.rs` (`loading_blocks`), `flush/site.rs` (`NodeFlush`), `flush/control.rs` (`stop`), `flush/policy.rs` (`decide`, `LEFT_IBD`), `flush/driver.rs` (`ThreadDriver::drop`). From dash-kv: `memory.rs` (`MemoryEngine::restart`).
- Core: `node/chainstate.cpp:177-296`; `validation.cpp:1970-1993` (`IsInitialBlockDownload`), `:2969-3127` (`FlushStateToDisk`, `ForceFlushStateToDisk`), `:3851` (ABC's PERIODIC), `:4992-5018` (`LoadChainTip`); `init.cpp:354-455` (`Shutdown`), `:2419-2425` and `:2569` (the genesis wait); `node/blockstorage.cpp:876-961` (`CImportingNow`, `ThreadImport`).

I ran the tests in a detached worktree (`/work/scratch/nd1-persist-review/wt`, since removed):

    cargo test --locked -p dashd-node --features test-support
    #   lib 133 passed (1 ignored), asserted_log_lines 11 passed, index_scan_cache 0 (2 ignored); load ~150-450
    # mutant A: LoadChainTip skipped (steps.rs:184, parts filtered out)
    #   -> a_restart_keeps_its_tip_through_the_node_stores FAILED at sut.rs:763 (an "Error" line: the
    #      restart's ABC connects genesis onto coins at b1, as 2b-D25 saw). Killed.
    # mutant B: Chainstate::force_flush returns Ok(()) without the WAL sync (chainstate.rs:458)
    #   -> sut::tests 13 passed. Survives (M3).

## Answers to the review questions

### 1. Init and restart parity

**Non-devnet: the order is Core's.** The chainstate opens first (accepted under 2b-D18), then:

1. `open_parts` loads the stored index and runs BL-127's blk-file check, now over the real `NodeBlockFiles`.
2. `recover` runs on the loaded index (Core's `ReplayBlocks`, chainstate.cpp:251).
3. `load_chain_tip` runs at the coins' best block, and only when there is one (`:272-277`).
4. V starts.
5. The coins attach (`InitCoinsCache`'s products).
6. Step 11's ABC runs under `fImporting`.

The mapping is Core's: a `LoadChainTip` failure is FAILURE "Error initializing block database" (`:275`, through `INIT_BLOCK_DB_FAILED`), which asks the reindex question as Core does. An unknown tip can't be reached, because recovery refuses it first (2b-D25). After `load_chain_tip`, V's published view already has a tip, so `wait_for_genesis` returns at once. That matches Core, where `fHaveGenesis` is set directly when the tip is non-null (init.cpp:2419-2425).

**Devnet: `LoadChainTip` never runs (M1 below).**

**A clean restart does not come back at Core's tip (B1 below).** The test asserts that it doesn't.

### 2. Flush semantics

Every flush that writes anything is V's, through `NodeFlush` (Core's sites: ABC's PERIODIC at validation.cpp:3851, IF_NEEDED, NONE). The one that fired in the test is LEFT_IBD. That cause is ours, not Core's (`flush/policy.rs:50`, "Ours (A1 §3.4)"); v24 has no flush on leaving IBD (`validation.cpp:1990` only latches). It was accepted with CH2, so it isn't scored against this branch, but see N2.

Both shutdown flushes (init.cpp:383, :453) are still the WAL-sync stub (`chainstate.rs:458-463`). By the time they run, V and its `NodeFlush` are already gone, and with them the dirty coins cache and the dirty index entries. That is B1.

### 3. Error classes

No silent skip or reject found in the new code:

- A blk-file read or write fault goes through `AbortPath(node abort, engine latch)` (`chainstate.rs:310-316`, `:386`), which is Core's `AbortNode`.
- An index read fault is a load failure (2b-D14).
- A `LoadChainTip` fault is FAILURE.
- A W or script-cache spawn failure is an init exception (`steps.rs:204-206`), which Core can't have.
- Not wiring `NodeFlush` into the shutdown flush hides no error. It simply writes nothing (B1).

### 4. Shutdown order

There is no write after close:

1. `Chain::stop` makes V answer its in-flight job (`NodeFlush::stop` → `FlushControl::stop` waits for the release, `control.rs:326-329`).
2. V's exit drops `NodeFlush`, whose `ThreadDriver::drop` joins W (`driver.rs:587-600`).
3. Only then do the WAL sync, `flush_background_callbacks`, the sporks, the second sync and `self.chainstate = None` run.
4. `Node::shutdown` releases the datadir lock after `steps.shutdown()` (`node.rs:375-381`).

The order isn't Core's, though. Core flushes at init.cpp:383 while `chainman` and its coins are alive, and resets the views only at :454. Here the views die with V, before the first flush. That is 2b-D4, still open, and it is the cause of B1.

### 5. Tests

- **The restart test proves restart only through the LEFT_IBD flush.** It proves that a block persisted by V's own flush comes back after a clean restart: the stored index loads past BL-127, `recover` runs, `LoadChainTip` sets V's tip to b1, and b2 connects on b1's coins. It does not prove Core's clean-restart behaviour. It asserts the opposite: the tip is b1 where Core's is b2 (`sut.rs:767-770`).
- **Plausible mutants.**
  - Skipping `LoadChainTip`: killed (see above).
  - Skipping or emptying the shutdown flush: survives. Nothing that matters is written there, and `MemoryEngine::restart` keeps every committed batch whether synced or not (`dash-kv/src/memory.rs:129-134`), so even the WAL sync is invisible to the test.
  - Flushing before the tip is set: can't be expressed in this code. The coins attach only after `start_chain`, and `NodeFlush` with no tip is the `Height() == -1` path, which this test never takes.
- **The 82936e1ec race fix is correct and masks nothing.** In Core, the startup ABC runs inside `ThreadImport`'s `CImportingNow` scope (blockstorage.cpp:895-960). The genesis wait (init.cpp:2569) ends on that ABC's `NotifyBlockTip`, so `LoadingBlocks()` can still be true when RPC opens. `IsInitialBlockDownload` returns true then (validation.cpp:1979-1981) and does not latch. V's `is_ibd` reads the same `loading` flag (`actor.rs:1423-1425`, `flush.rs:311-314`), so a block that lands in that window is IBD in both nodes. The test now waits until `loading_blocks()` clears, which is a test-side precondition rather than an ordering change. The commit message's reproduction (a 1.5 s delay in loadblk) shows the failure was the LEFT_IBD flush moving to b2, not a production bug.

## Findings

### B (blocking)

**B1. A clean restart loses every block connected since V's last flush. Core's shutdown flush keeps them.** `dashd-node/src/steps.rs:273-302`, `:330-354`; `dashd-node/src/chainstate.rs:458-463`; the test pins it at `dashd-node/src/sut.rs:760-770`.
- **Core.** `Shutdown()` calls `ForceFlushStateToDisk` (init.cpp:383 and :453) → `FlushStateToDisk(ALWAYS)` (validation.cpp:3125). That writes the blk files, the block index and the whole coins cache (`:3053-3104`), so a clean restart's `LoadChainTip` (`:4992-5018`) comes back at the last connected block.
- **Here.** `ChainSteps::shutdown` stops V first (`steps.rs:274-275`). V's exit drops `NodeFlush` and its `CoinsCache` with whatever is dirty (`VExit` holds neither, `actor.rs:216-223`). The two "flushes" then sync the WAL of an engine that no dirty state can reach.
- **Scenario.** Mainnet, past IBD:
  1. LEFT_IBD fires once.
  2. PERIODIC's coins flush is every 24 h (`DATABASE_FLUSH_INTERVAL`) and its index write is every 1 h.
  3. A clean `stop` or SIGTERM up to 24 h later restarts at the LEFT_IBD point or at the last periodic or cache-pressure flush.

  During IBD, before LEFT_IBD, everything since the last cache-large flush is lost. The test shows it in two blocks: b2 is connected and is the tip before the clean shutdown, and after the restart the tip is b1.
- **Status.** This is the recorded M3/2b-D4 gap ("a stub until M3"), and it is better than the in-memory store before this branch, which kept nothing. But it fails the brief's restart criterion, and a release built from this head would silently drop blocks on every clean restart. Merge it only as a recorded deviation, with `ChainCommand::Flush` (M3) and the V `quiesce`/`finish` split (2b-D4) gating any claim of restart parity.
- **Fix.** M3 plus 2b-D4. At shutdown, V must stay alive through flush #2 so that `Chainstate::force_flush` can reach `NodeFlush::flush(Always, …)`. Then flip the test to assert b2 (see M3 below).

### M (major)

**M1. A devnet restart never runs `LoadChainTip`, so it can't come back on a stored index.** `dashd-node/src/steps.rs:171-195`.
- On a devnet, `parts.take()` (`:171`) hands the parts to V before recovery, so `parts.as_mut()` at `:184` is `None`. The `LoadChainTip` block is skipped with no error, and so is the `(Ok(()), Some(parts))` arm at `:196`.
- Core runs `LoadChainTip` for every chainstate whose coins aren't empty (chainstate.cpp:272-277), devnet included.
- **Scenario** (found by reading, not run: NodeSut has no devnet restart harness). A devnet node flushes (LEFT_IBD or PERIODIC), stops and restarts:
  1. The coins recover at b_n, but V has no active tip.
  2. Step 11's ABC tries to connect from genesis onto coins at b_n.
  3. The connector refuses with "ConnectBlock: the view's best block … is not the parent", which is fatal. 2b-D25 saw this exact failure for the non-devnet case before `LoadChainTip` was wired.
  4. Init ends "Shutdown requested. Exiting.", and the node can't restart on that datadir without `-reindex-chainstate`.
- This is reachable now because this branch made devnet indexes persist.
- **Fix.** Before READY, either:
  - land CH1's `devnet_pending()` with `LoadChainTip` on V's start path (2b-D8, 2b-D29), or
  - make `load` fail loudly when there is a stored devnet coins tip that can't be loaded, rather than skipping it, and record the deviation.

  Add the devnet restart test that 2b-D8 promised.

**M2. `LoadChainTip`'s "Loaded best chain:" line is not Core's, and nothing asserts it.** `dashd-chainstate/src/chain/load.rs:325-330` (CH1-node's code, first reached in the node by this branch).
- Core logs `"Loaded best chain: hashBestChain=%s height=%d date=%s progress=%f\n"`, with `date` as `FormatISO8601DateTime` and `progress` as `GuessVerificationProgress` (validation.cpp:5012-5016).
- The port logs `date=<raw u32>` and has no `progress=` field, through `tracing` into `debug.log`.
- The plan closes row 142 "with the restart proof", but `a_restart_keeps_its_tip_through_the_node_stores` never checks the line, so the ledger row can't go to implemented.
- **Fix.** Route the line to CH1-node to fix the format. Then assert the exact line, with the date and progress for b1, in the restart test (its `lines` capture already exists, `sut.rs:719-723`).

**M3. The restart test pins the deviation instead of Core's behaviour, and a missing shutdown flush survives it.** `dashd-node/src/sut.rs:760-770`.
- It asserts `coins_tip == b1` and `view tip == b1` after b2 was the tip, with the reason "a stub until M3".
- With M3 in place, Core's behaviour is b2. A mutant that empties `Chainstate::force_flush` or drops both `self.force_flush()` calls' writes passes today (mutant B above), and only the call count (`assert_eq!(flushes, 2)`) is checked.
- `MemoryEngine::restart` keeps unsynced batches, so this harness can't see a missing WAL sync either.
- **Fix.**
  - When M3 lands: assert b2 after the restart, with no resubmit, and add a kill-teardown twin that asserts b1. The pair then separates "the shutdown flush wrote" from "V's flush wrote".
  - Until then: rename the test, or add a doc line saying it proves persistence through V's own flush only.

### m (minor)

**m1. `check_startup_readiness` is never called.** `dashd-node/src/steps.rs:182-199`; contract at `dashd-chainstate/src/chain/load.rs:334-350`.
- CH3a D70's guard says "ND1 calls it after `load_chain_tip`, before recovery's replay and init's ABC". The node now attaches a `CoreConnector` with `NoEvo` hooks (`H::COMPLETE == false`), so the guard is live, but `load` never calls it.
- Today the tip can only be one this node connected, and the connector's own G1 refusal gated that at connect time, so I found no reachable wrong result. That stops holding when the readiness rules change between versions over the same datadir.
- Note that the doc's order ("before recovery's replay") and the accepted Core order here (replay, then `LoadChainTip`) disagree.
- **Fix.** Call it after `load_chain_tip` with the active tip and recovery's transition blocks, mapping `NotReady` as `open_parts` already does. Or record why it waits, and fix the doc's ordering sentence with CH3a.

**m2. The coins budget leaves out Core's mempool headroom.** `dashd-node/src/chainstate.rs:378-383`, `:427`.
- Core sizes the coins cache against `nCoinCacheUsage` plus the unused part of `-maxmempool` (300 MB by default; validation.cpp:2937-2966, `GetCoinsCacheSizeState`). With `CoinsBudget::new(.., 0, ..)` and `NoInputs` (mempool 0), CACHE_LARGE and CACHE_CRITICAL fire up to 300 MB earlier than in dashd. That means more full flushes during IBD: a performance difference, not a correctness one, until MP1.
- The same list covers `CheckQueue::serial()` (`-par`), the default sigcache size (`-maxsigcachesize`) and `assume_valid: None`. Under `-assumevalid`, Core skips script checks below the assumed block, so mainnet IBD is much slower here, and a historical block with a bad script would be rejected where Core accepts it (none known).
- 2b-D24 records all of these as prototype gaps. Keep them as named BL rows so 2b-2 proper can't ship without them.

**m3. The 2b-D6 `LoadChainTip` row still has no test.** 2b-D25 says so itself. The row (FAILURE after the coins cache: 2 shutdown flushes, the reindex question) is now reachable through `load_chain_tip`'s `INVALID_TIP` (`load.rs:315-317`). A `NodeSut` test that stores a tip marked `FAILED_VALID` would cover both the message and the flush count.

### N (nits)

**N1. The restart test's file check proves little.** `dashd-node/src/sut.rs:766`. `blk00000.dat` exists after any start, because `LoadGenesisBlock` writes genesis to it. Reading b1 back through V would prove that b1's blk record and its stored `data_pos` survived. Any read works: a `ReadBlockFromDisk` through the reader, or a reorg that disconnects b1.

**N2. Say in the test that LEFT_IBD is ours.** `dashd-node/src/sut.rs:727`. The comment cites A1 §3.4 but not that Core v24 has no such flush (validation.cpp:1990 only latches). Add "(ours; Core has none)" so nobody reads the test as Core parity for that flush.

**N3. Cite the Core lines the new `load` block follows.** `dashd-node/src/steps.rs:127`. The doc cites `:182-212, :220-260`, but the new `LoadChainTip` block is `:272-277` (and validation.cpp:4992-5018). The comment at `:182` cites `:272` only.

## Verdict

**CHANGES REQUIRED** for a restart-parity claim.

- B1 (a clean restart loses connected blocks) and M1 (a devnet restart can't load its tip) both fail the brief's "a restart must come back at the same tip Core would".
- Both are recorded dependencies (M3/2b-D4 and 2b-D8), not wiring mistakes in this diff.
- The wiring itself is sound:
  - the non-devnet init order and failure mapping are Core's;
  - every new fault goes to `AbortNode` or to an init failure;
  - nothing writes after the stores close;
  - the 82936e1ec race fix matches Core's `CImportingNow` semantics.

If the manager rules that this head may ride a train as a recorded deviation:
- B1 needs the DEC and a release note;
- M1 needs either the fail-loud guard or the devnet accessor;
- M2 and M3 are test and log work for 2b-2 proper.
