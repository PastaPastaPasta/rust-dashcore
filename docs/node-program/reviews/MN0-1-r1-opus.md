# MN0-1 review, round 1 (Opus, high)

- Branch `node/mn0-1`, head `8a892fe0a`, base `df5de9c1c`: 7 commits, 16 files, +1,076 lines.
- Core reference: `v24.0.0-rc.4` (49caf372bb).
- Rulings: DEC-595, DEC-607. Plan: `/work/scratch/mn0/PLAN-MN0.md` r1.
- I was the first reviewer; both Sol attempts dropped without findings.
- Scratch: reflink copy `/work/scratch/mn0-1-rv-opus/wt`, own target, `TMPDIR=/dev/shm/mn0-1-rv-opus-tmp`.

## Runs (mine)

| Run | Result |
|---|---|
| `cargo test -p dashd-mining` | 10/10 ok |
| `mn0_template_dashd` (`--features test-support`, dashd `v24.0.0-rc.4` from setup-dashd's cache, `DASHD_PATH`/`DASHD_TEST_DATA` from `setup-dashd.py`) | 3/3 ok in 9.9 s. No skip line; I checked that dashd was found. The run covered heights 1–431, NotReady at 432, `-blockversion=1337` at heights 1–5, and the -1 TBV text |
| `cargo test -p dashd-node --lib init::support` | 5/5 ok, including `every_refusal_matches_pf0_n_s_classifier` at floor 160 |
| `cargo clippy -p dashd-mining --all-targets -D warnings` | clean |
| Reviewer probe `dashd-node/tests/mn0_probe.rs` (scratch copy only): fresh `-testnet` SUT, one `create_new_block` with no grind and no dashd | base: height 1, bits `0x1e0fffff`, ok. **With M13 applied** (bits = tip's bits): `Failed("CreateNewBlock: TestBlockValidity failed: bad-diffbits, incorrect proof of work at 1")`, so the mutant is killed (see m1) |

I did not rerun tier 1. The implementer reports EXIT 0, and the targeted runs above cover every crate this diff touches.

## Checked against rc.4 and found correct

1. **Coinbase (`assemble.rs`)**
   - Core reference: `CreateNewBlock` miner.cpp:176-363.
   - `CMutableTransaction` defaults are `nVersion = CURRENT_VERSION = 2` (transaction.h:242), type 0 and lock_time 0. The input has a null prevout and `nSequence` `0xffffffff`. There is one output, `scriptPubKeyIn`, of value `blockSubsidy + nFees` (fees are 0 here).
   - scriptSig is `CScript() << nHeight << OP_0` (miner.cpp:278). `push_int64` (script.h:398) emits OP_1..OP_16 for 1–16, OP_0 for 0, and otherwise a direct push of `CScriptNum::serialize`. The port matches. The unit tests round-trip the edges, and the differential covers 1–431 byte for byte.
   - Dash v24 has no `IncrementExtraNonce`, so OP_0 is the only extra-nonce byte.
   - `FillBlockPayments` adds nothing below DIP3:
     - The era is `Classic`, because V20 is not active (validation.cpp:5903-5909).
     - `GetBlockTxOuts` pushes the platform OP_RETURN only in `EvoReward`, and the MN list is empty.
     - A superblock trigger needs MN votes, so none can fire before DIP3, `-budgetparams` or not.
2. **Subsidy**
   - `GetBlockSubsidyInner(pindexPrev->nBits, pindexPrev->nHeight, …, fV20Active_context)` (miner.cpp:271) takes the previous block's bits and height. `tip_facts` passes `tip_header.bits` and `tip.height`, as Core does.
   - `v20` comes from `active_after(prev, V20)`, which is false wherever G1 lets the build through.
3. **nVersion**
   - Core computes `ComputeBlockVersion(pindexPrev)` and then, when `NetworkIDString() != MAIN`, applies `GetIntArg("-blockversion", nVersion)`. That is miner.cpp:209-214, and devnet honours the override too.
   - `block_version_arg` returns `None` only on `NetworkKind::Mainnet`. It uses `setting_to_int`, a faithful `SettingToInt`, so a non-number gives `LocaleIndependentAtoi` 0 as in Core. The `as i32` truncation matches the C++ int64 → int32 store.
   - Reading the value once at construction is equivalent: no RPC changes `-blockversion` at runtime.
4. **nTime**
   - Core reads `NodeClock::now()` (miner.cpp:216), then `UpdateTime` takes `max(MTP+1, NodeClock::now())` if that is larger (:49-57), with uint32 stores.
   - `block_time` reads the clock once. Core's two reads collapse to the later one, so one read is an outcome Core can reach.
   - Under `-mocktime`, `NodeClock::now()` returns the mock time. The frozen-clock stretch 200..260 proves `raised > 0`.
5. **nBits**
   - Core calls `GetNextWorkRequired(pindexPrev, pblock)` at the final nTime (:346), and also inside `UpdateTime` on min-difficulty networks; both give the same value. `tip_facts` passes the final `time`.
6. **grind**
   - `GenerateBlock` (mining.cpp:139-163) sets `hashMerkleRoot` first. Its loop condition order is `max_tries`, `nNonce < MAX`, `!CheckProofOfWork`, `!ShutdownRequested`.
   - After the loop, Core checks `max_tries == 0 || Shutdown` first (→ false), then `nNonce == MAX` (→ true, no block).
   - The port keeps that order exactly. Splitting OutOfTries from Shutdown is fine: Core returns false for both.
7. **TBV failure**
   - Core throws `runtime_error("CreateNewBlock: TestBlockValidity failed: <state>")` (:353), which becomes RPC -1. The differential compares the whole JSON error object with dashd's.
   - The counters and the log line are set before the verdict (:259-261), and are now recorded once per V verdict (d1f65e847).
   - The log text matches Core's byte for byte: `total size 1000 txs: 0 fees: 0 sigops 100`. Nothing tests it (n5).
8. **G1 boundary**
   - `g1_rule(deployments, Some(tip.id), None)` evaluates `active_after(prev, ·)` for DIP0003, V20, MN_RR, V24 and EVO_SHARES. That is Core's `DeploymentActiveAfter(pindexPrev, …)`, the same predicate that sets `fDIP0003Active_context`, `fV20Active_context` and `fV24Active_context` (miner.cpp:200-204), so there is no off-by-one.
   - Regtest: `DIP0003Height` = 432, and the asserts give `V20Height ≥ DIP0003Height` and `MN_RRHeight ≥ V20Height` (chainparams.cpp CRegTestParams).
   - The differential pins both sides: 431 is byte-equal and 432 is `NotReady{432,"DIP0003","MN0-2"}`.
   - Below DIP3, nothing else adds content:
     - quorum commitments are inside `if (fDIP0003Active_context)`;
     - `mn_list_builder` and CbTx are the same;
     - payments: see 1.
   - V24 and EVO_SHARES only affect DIP3-gated code in CreateNewBlock. Refusing them below DIP3 is stricter than Core, but it is within D4 and matches connect's own G1.
9. **M13 and M15 on regtest: confirmed equivalent.**
   - Regtest genesis bits are `0x207fffff`, which is powLimit's compact form. `fPowNoRetargeting` = true, `fPowAllowMinDifficultyBlocks` = true, and KGW starts at 15200.
   - Below KGW, `GetNextWorkRequiredBTC` returns one of three things:
     - powLimit;
     - the walk-back over powLimit blocks, which is powLimit;
     - at the 576-interval, `CalculateNextWorkRequired` → `fPowNoRetargeting` → `pindexLast->nBits`.
   - From KGW on, `fPowNoRetargeting` returns powLimit before KGW/DGW are reached.
   - By induction, every regtest block carries powLimit, so next-work equals the tip's bits at every height. `-powtargetspacing`, `-minimumdifficultyblocks` and `-highsubsidyblocks` are devnet-only (`CDevNetParams`), so no regtest arg changes this.
   - M15 is equivalent for the same reason, and the 500-DASH cap also absorbs it on low-difficulty testnet.
   - **M13 is not equivalent off regtest.** See m1.
10. **D2 (`ChainQuery.deployments`)**
    - `ChainDeployments::view()` is `Deployments::new(&params, &chain, &ehf, &cache)`: four borrows (`Arc` deref, no refcount), no allocation and no lock. Building it per Query costs nothing on the hot path.
    - The `VersionBitsCache` mutex is taken only by a closure that queries a deployment. It is uncontended, because only V's thread uses it. Populating the cache from a query is what Core's `CreateNewBlock`/GBT do to `m_versionbitscache`.
    - Exposure: `params` is already public; `chain` is the same complete chain `q.chain` exposes; `ehf` is `NoEhfSignals`. The one widening is n1.
    - I sign off D2 in CH3's stead, subject to n1 (a nit). dashd-chainstate's lib changed, so its PF0 rows still judge the codegen in T45, as DEC-607 (3) says.
11. **D3**
    - `("-blockversion", Implemented)` and the floor 161 → 160 are both correct: one fewer refusable option, and the classifier test passes.
    - The registration (`ALLOW_ANY | DEBUG_ONLY`, BlockCreation) matches init.cpp:857.
    - Nothing reads the value until RP1a-1 wires generate*. DEC-607 (2) covers that by putting both in train T45.
12. **The comparison test is honest**
    - It compares the full `core_serialize(block)` hex with `generateblock`'s `hex`, plus the hash. Nothing is normalised.
    - Both nodes are regtest with no consensus args. They share `-mocktime` and set the same per-height `setmocktime`.
    - dashd builds with `generateblock <out> [] false`, which uses the `nullptr` mempool, the same as `TemplateTxs::None`.
    - Each block is then `submitblock`ted to dashd and checked with `getbestblockhash`, and given to V.
    - V's tip is checked only indirectly: the next height's `template.height == h`, prev inside the compared bytes, and NotReady at exactly 432. That is sufficient.
    - Placement: dashd-node/tests with `required-features = ["test-support"]`, and the test under `mod sut` matches the gate filter.
    - CI's `node-runtime` group runs `--all-features` with `DASHD_V24_PATH` = rc.4. Tier 1 does not run it, which the implementer's note records.
    - Moving it after RP1a is DEC-607's follow-up.

## Findings

### m1 (minor): M13 is equivalent on regtest only, and nothing tests nBits off regtest

- **Where:** `dashd-mining/src/source.rs:173` (`get_next_work_required(chain, tip.id, time, …)`) and the mutant claim.
- **Scenario:**
  - The source is network-agnostic, and Core's `generate*` and GBT run on every network.
  - On a fresh testnet chain, block 1 is dated more than 2 × 150 s after the 2014 genesis. It must carry powLimit `0x1e0fffff` (the min-difficulty rule), while genesis carries `0x1e0ffff0`.
  - With M13 applied, the template fails its own TBV with `bad-diffbits`. The probe above shows this in 0.13 s, with no dashd and no grind.
  - Any mutant that passes the wrong time to `get_next_work_required` (`now` instead of `time`) survives the same way, because it is only visible where `fPowAllowMinDifficultyBlocks` and retargeting both matter.
- **Fix:** add the probe (appendix A, about 30 lines; my scratch copy is deleted) as a `sut::` test. Record M13 as killed, and M15 as equivalent on regtest and capped on early testnet.

### m2 (minor): the two-turn window has one outcome Core can't produce, and the retry path has no test

- **Where:** `dashd-mining/src/source.rs:91-138`.
- **Livelock:**
  - None with finite input. `continue` runs only when a block reached V between the two turns.
  - It is unbounded during sustained block arrival, for example an IBD burst from a peer: the RPC waits until the stream stops. Core would build on whatever tip it holds `cs_main` on.
  - The difference is latency only. Every template returned is one Core could have built.
- **Stale template:** none. TBV checks `prev == tip` on V, and nothing moves between TBV and the return that couldn't also move after Core releases `cs_main`.
- **Double stats:** none. The counters and the log line are written once, after a verdict; a `NotBestPrev` turn writes nothing (d1f65e847).
- **The Core-impossible outcome: time.**
  - `now` is read before the Query turn, and TBV judges `time-too-new` against V's clock in the second turn.
  - A `setmocktime` that moves the clock back more than 2 h (`MAX_FUTURE_BLOCK_TIME`) between the two turns makes the template fail with -1 `CreateNewBlock: TestBlockValidity failed: time-too-new…`.
  - Core can't do this: rc.4 `setmocktime` takes `LOCK(cs_main)` (rpc/node.cpp:272) and CreateNewBlock holds it, so the mock time can't change mid-assembly.
  - It needs a concurrent, test-only RPC, so this is minor. But PLAN §6.3 says "there is a test for it", and there isn't one.
- **Coverage:** no test reaches `Ok(Err(NotBestPrev)) => continue`. A mutant that turns it into `return Err(Failed(..))` or `BlockState::Error` survives everything.
- **Fix, either of:**
  - a test-support hook between the turns: a closure run after `tip_facts`, which a test uses to connect one block or step the mock clock back. Then add two tests, "rebuilds on the new tip" and "a mock step back → the template's time is re-read" (re-reading `now` on a time-too-new verdict closes the window).
  - Or record the time window as a known divergence with DEC-607's single-turn follow-up as its closer. In that case, file the missing retry test with that follow-up.

### n1 (nit): D2 lets a read-only Query clear V's versionbits cache

- **Where:** `dashd-chainstate/src/chain/command.rs:31`.
- **Why:** `Deployments` has `pub cache: &VersionBitsCache`, and `VersionBitsCache::clear(&self)` is public, so any `ChainCommand::Query` closure can now clear `m_versionbitscache`. The results stay correct, because states are recomputed from the chain, but it is a mutation reachable through the read-only command, and nothing in plan §4 needs it.
- **Fix:** expose `&dyn DeploymentView` plus a `compute_block_version` accessor, or say in the field doc that queries must not call `cache.clear()`.

### n2 (nit): Core line cites in dashd-mining don't match rc.4

| Cited | rc.4 |
|---|---|
| `UpdateTime :12-20` | 49-57 |
| `:211` (nTime) | 216 |
| `:204-209` (nVersion/-blockversion) | 209-214 |
| `:252-254` (counters, log) | 259-261 |
| `:257-271` (coinbase) | 263-279 |
| `:264` (subsidy) | 271 |
| `:362-366` (header) | 343-347 |
| `:363-364` | 346 |
| `:370-373` (TBV) | 351-354 (throw at 353) |
| `:190-192` (tip and height) | 194-197 |
| `node/miner.cpp:174-385` (lib.rs) | 176-363 |
| `rpc/mining.cpp:180` (generateBlocks' assembler) | 173 |

The `rpc/mining.cpp` cites at 139-163 and 380 are right. The plan carries the same offsets.

### n3 (nit): NotReady leaves `last_block_stats` alone, where Core sets them

- Core sets `m_last_block_*` (:259) before any CbTx work.
- At a G1 height, rdashd's `getmininginfo` keeps the previous `currentblocktx`/`currentblocksize`, or omits them if nothing was assembled yet.
- This falls inside D4's divergence. Mention it in the ALLOWED-DIFFS seed row (DEC-607 (1)) so O14 doesn't rediscover it.

### n4 (nit): `TemplateTxs::Mempool` is ignored silently

- `_txs` is unused, which is correct while the mempool is `NullMempool`.
- `ChainTemplateSource::new` takes no mempool, so nothing forces MP1a to wire one in. Once MP1a lands, generate would quietly mine empty blocks where Core includes the mempool.
- **Fix:** a BACKLOG row owned by MP1a/MN1, or a `debug_assert`/doc line naming MP1a.

### n5 (nit): the byte-exact CreateNewBlock log line isn't asserted

- Plan §4 lists it as a deliverable. Capture the tracing output in one dashd-mining or SUT test.

## Answers to the brief

1. **Coinbase:** correct against rc.4 in every field, and the differential confirms 1–431. **M13/M15:** equivalent on regtest (proof in 9). M13 is killable on testnet (m1); M15 is effectively equivalent everywhere MN0-1 can reach.
2. **G1:** exact, the same `DeploymentActiveAfter(pindexPrev)` predicate Core uses. 432 is the first refused height on default regtest, and `-dip3params`/`-vbparams` moves follow, because `g1_rule` reads V's deployments.
3. **Two turns:**
   - No livelock with finite input. The retry is unbounded under a sustained block stream (latency only).
   - No stale template, and no double stats.
   - One Core-impossible outcome: a backward mock-time step of more than 2 h between the turns gives -1 time-too-new. The retry path is untested (m2).
4. **D2:** borrow-only, with no allocation or lock on the Query path. Exposure is within §4 except the clearable cache (n1). Signed off for CH3.
5. **D3:** parsing, range (int64 → int32 truncation, atoi semantics), network restriction (everything except mainnet, devnet included) and floor 161 → 160 are all correct.
6. **Tests:** a real byte comparison with no normalisation, and dashd args match. Placing it in dashd-node/tests is sound for now (CI node-runtime runs it with rc.4; tier 1 doesn't).

## Verdict

**APPROVE-WITH-NITS.**

- No blockers and no majors.
- m1 and m2 are test gaps around code that is correct on regtest. Neither changes a block rdashd mines on regtest.
- I recommend landing the m1 probe test with this slice. m2 can go to DEC-607's single-turn follow-up if the manager records the time window.

## Appendix A: the m1 probe (`dashd-node/tests/mn0_probe.rs`, built with `--features test-support`)

```rust
//! Reviewer probe (MN0-1 r1 Opus): a template on a fresh testnet chain, where the next work
//! (min-difficulty rule: > 2 spacings after genesis) is powLimit 0x1e0fffff and genesis carries
//! 0x1e0ffff0. Kills M13 (bits = tip's bits) without dashd or grinding.
#![cfg(feature = "test-support")]

use std::sync::Arc;

use dash_clock::Clock;
use dashd_mining::{BlockTemplateSource, ChainTemplateSource, TemplateTxs};
use dashd_node::logging::Logger;
use dashd_node::sut::NodeSut;
use dashd_node::ui::Ui;

#[test]
fn testnet_height_one_takes_the_min_difficulty_bits() {
    let dir = tempfile::tempdir().unwrap();
    let ui = Arc::new(Ui::with_stderr(Arc::new(Logger::new()), Box::new(|_| {})));
    let sut = NodeSut::new(dir.path(), &["-testnet", "-printtoconsole=0"], 1_700_000_000, ui)
        .expect("the SUT starts");
    let chain = sut.chain().expect("V runs").clone();
    let clock: Arc<dyn Clock> = sut.clock().clone();
    let source = ChainTemplateSource::new(chain, Arc::new(sut.params().clone()), clock, None);
    let r = source.create_new_block(&[0x51], TemplateTxs::None);
    match r {
        Ok(t) => {
            eprintln!("PROBE height {} bits {:#010x}", t.height, t.block.header().bits());
            assert_eq!(t.height, 1);
            assert_eq!(t.block.header().bits(), 0x1e0f_ffff);
        }
        Err(e) => panic!("PROBE template refused: {e:?}"),
    }
}
```
