# PF1 addendum: CH4-0, dashd's reindex budget on mainnet's pre-DIP3 window

CH4-0 (DEC-694; PLAN-CH4 §CH4-0) measures where dashd v24.0.0-rc.4 spends its critical path during `-reindex-chainstate` over mainnet's pre-DIP3 window, the M3 replay range: heights 1 to 1,028,159 (`A1-architecture.md:2559`; DIP3 activates at 1,028,160, Core `chainparams.cpp:185`). For each CH4 lever it gives the share of that path the lever could remove, and the Amdahl ceiling of removing it. CH4-2, CH4-3, CH4-4 and CH4-5 make no speed claim beyond these ceilings, and CH4-3's S2 claim is measured only against the B_DEC43 run (DEC-754).

All Core citations are to `0e7382df80d` (v24.0.0-rc.4), the binary measured.

## Results

**The critical path is loadblk.** In `-reindex-chainstate`, the `loadblk` thread runs ActivateBestChain under `cs_main`: it is the thread rdashd's V replaces. With assumevalid (AV-E, below) at the program's coins budget B_DEC43, loadblk is on-CPU 1,417 s of the window's 2,005 s wall. Script checks, the validation work dashd hands to worker threads, are skipped below the assumevalid block, so loadblk carries all of it.

**Ceilings at B_DEC43 with assumevalid** (AV-E, `-dbcache=263 -maxmempool=5`, `-par=0`; cpu-clock share of loadblk, then the ceiling of removing that lever's on-CPU time from the window's wall):

| lever | instructions:u | cpu-clock | ceiling |
|---|---|---|---|
| serial-rest | 48.2% | 49.3% | 1.535× |
| read | 27.5% | 23.6% | 1.200× |
| check-in-connect | 10.7% | 7.5% | 1.056× |
| events | 7.0% | 5.4% | 1.040× |
| flush | 3.0% | 4.6% | 1.034× |
| log-tip | 1.6% | 4.5% | 1.033× |
| coins-db | 1.8% | 4.0% | 1.029× |
| special-tx | 0.2% | 1.0% | 1.007× |
| block-undo-write | 0 | 0.0% | 1.000× |
| scripts | 0 | 0 | – |

**`CTxMemPool::removeForBlock` is the largest single item on the path**, with an empty mempool. Bench off, it is 11.6% of loadblk's instructions:u and 7.7% of its cpu-clock without assumevalid (the no-AV replicate), and 20.0% / 11.2% with AV-E at B_DEC43, where scripts no longer dilute it. It is not noise; see "removeForBlock" below for the Core lines. By instructions, half of it is a SHA-256 of every block input against an empty masternode list and the other half the fee estimator's per-block decay; by CPU time the decay is about 60%.

**S2 (CH4-3) has a lever only below the all-cached size.**
- At `-dbcache=4096` Core never reads the coins database in the window: the cache peaks at 981.8 MiB (8,940,039 txo at the stop height), no coins flush happens before shutdown, and `CCoinsViewDB::GetCoin`/`HaveCoin` have no samples. These runs are the all-cached ceiling.
- At B_DEC43 the cache flushes 5 times in the window. `coins-db` (the tip cache's misses, `CCoinsViewCache::FetchCoin → CCoinsViewErrorCatcher::GetCoin → CCoinsViewDB::GetCoin`) is 1.8% of loadblk's instructions:u and 4.0% of its cpu-clock, a ceiling of 1.029×; `flush` is 4.6% of cpu-clock, 1.034×.
- The small cache's whole cost, from the exact stat passes (all threads): B_DEC43 runs 9.65% more instructions:u than the all-cached AV-E run (2,777,090,091,934 against 2,532,702,024,095) and 17.5% more task-clock (1,702.5 s against 1,448.8 s). Its wall is 259 s (stat passes) to 510 s (sampled passes) longer, 14–25% of its own wall: that is the most S2 and W together could remove, a ceiling of 1.16–1.34×, since neither makes the cache larger.
- Each of the 5 flushes empties the cache: the UpdateTip after it reads `cache=0.3MiB(0txo)` (heights 347,232, 646,348, 773,453, 834,362, 930,464; 257.6–257.7 MiB before each). This answers PF1 §8's open question (`PF1-dashd-v24-profile.md:273`) for a size-triggered flush in v24 rc.4: Core empties its cache at each one.
- The flushes also make LevelDB compact: its unnamed background thread runs 116,940,021,439 instructions:u in the window (4.3% of the run's sampled instructions; none at 4096), off loadblk.
- cpu-clock does not see a miss's I/O wait. loadblk's off-CPU time rises from 393 s (AV-E, 4096) to 588 s at B_DEC43, but the two runs ran at different load, so that difference bounds the I/O wait only loosely. CH4-3 may claim at most the on-CPU coins-db share plus what it shows of that wait, measured against the B_DEC43 configuration.

**Read-ahead (CH4-4)**: `read` (ReadBlockFromDisk, including X11) is 23.6% of loadblk's cpu-clock at B_DEC43, a ceiling of 1.200× (1.23× at 4096).

**S1 (CH4-2)**: the CheckBlock inside ConnectBlock (`check-in-connect`) is 7.5%, a ceiling of 1.056×.

**W's append queue (CH4-5) cannot be judged from these windows.** `-reindex-chainstate` writes no blocks, and Core writes a block's undo only when it has no undo position yet (`node/blockstorage.cpp:777`), which a reindexed chainstate keeps from the original sync. `block-undo-write` is 0–0.03% here by construction. The gate (appends ≥ ~3% of V's per-block time) needs an IBD or a full `-reindex` window. Proposed: decline CH4-5 (Q6) unless the manager orders such a window.

**Without assumevalid, scripts decide.** Script checks are 26.70–26.73 × 10¹² instructions:u, 91.4% of all, in all three no-AV runs (loadblk plus the check-queue workers, exact-scaled; they agree to 0.09%). How much of that loadblk runs itself depends on how the queue is scheduled: 39.6% and 43.8% of loadblk's instructions in two `-par=0` runs of the same work, 91.7% with `-par=1`, where every script runs on loadblk. Without assumevalid the ceilings are on instructions:u (the wall is mostly loadblk waiting on the queue under the box's load): at `-par=0`, serial-rest 1.39–1.43×, read 1.19–1.22×, check-in-connect 1.07–1.08×.

**The rdashd half is not measured here.** The plan asks for rdashd's V-thread breakdown on the same window at T47's head, but rdashd refuses `-reindex-chainstate` until CH5 (`dashd-node/src/init/support.rs:131`). Proposed: take it when CH5 lands, or earlier from an rdashd IBD of the window off a local dashd peer if CH4-3 needs it first.

## Method

**Binary.** dashd v24.0.0-rc.4 (`0e7382df80d`), built with PF1's recipe (debian:bookworm, `-O2 -g -fno-omit-frame-pointer -mno-omit-leaf-frame-pointer`), sha256 `1e8eb7c386cb97e7e827bb148422c93de28dcfd9fef65e82ccff9c73453641ef`.

**Data.** A reflink of a local mainnet `blocks/` directory with its index, read only from a unit with `ReadOnlyPaths` (DEC-600, DEC-694). Its last block file ends at height 2,094,869; nothing was fetched (DEC-671). Each run clones it, reindexes the chainstate, and is deleted once read.

**Configuration.** `-reindex-chainstate -stopatheight=1028159 -connect=0 -listen=0 -dnsseed=0 -fixedseeds=0 -disablewallet -txindex=0 -disablegovernance -checkblockindex=0`, CPUs 32–47 (16 logical CPUs), nice 19, idle I/O class. `-par=0` gives 15 script workers (GetNumCores() − 1 = 47, capped at `MAX_SCRIPTCHECK_THREADS` = 15: `init.cpp:1554-1565`, `validation.h:77`), so loadblk plus the workers fill the 16 CPUs; `-parbls=0` adds 33 BLS workers, idle before DIP3. The window runs from the first UpdateTip (height 1) to the stop height.

| configuration | flags beyond the common ones | runs |
|---|---|---|
| AV-E | `-assumevalid=<1,028,159's hash> -minimumchainwork=0 -dbcache=4096 -par=0` | avE-rec, avE-stat2 |
| AV-E, B_DEC43 | as AV-E, `-dbcache=263 -maxmempool=5` | avE263-rec, avE263-stat |
| no-AV | `-assumevalid=0 -dbcache=4096 -par=0` | noav-rec, noav-stat |
| no-AV replicate | dashd's default assumevalid, `-dbcache=4096 -par=0` (inert on this data, below) | av-rec-nb, av-stat-nb |
| no-AV, par 1 | `-assumevalid=0 -dbcache=4096 -par=1 -parbls=1` | noav1-rec, noav1-stat |

**AV-E is an emulation (DEC-780).** ConnectBlock skips scripts only when the assumevalid block is in the block index and the best header's work is at least `MinimumChainWork()` (`validation.cpp:2548-2576`). v24's mainnet values are both at height 2,487,500 (`chainparams.cpp:73-74`), past the data's end at 2,094,869, so dashd's default assumevalid skips nothing here: those runs check every script, and their exact instructions equal the `-assumevalid=0` runs' to 0.02%. They are kept as a no-AV replicate. AV-E names the stop block as the assumevalid block and sets `-minimumchainwork=0`: every window block is then its ancestor and years of work behind the best header, so every script is skipped, as in a real v24 IBD below 2,487,500. `-minimumchainwork` changes nothing else in the window: `-reindex-chainstate` keeps IBD latched by `LoadingBlocks()` (`validation.cpp:1985-1987`), the remaining use is AcceptBlock's (`:4820`), which the reindex never reaches, and there are no peers. The AV-E runs log `Assuming ancestors of block 000000000000003e8b908ef8e6694fcb5fb18c3b311a56f962a2c84c1d6652b6 have valid signatures.`, and their scripts lever has no samples.

**B_DEC43.** DEC-43's coins budget, 252.75 MiB, is reproduced as PF1 did (`PF1-dashd-v24-profile.md:241-242`): `-dbcache=263` with txindex off, and `-maxmempool=5` so the empty mempool lends its space to the cache only up to 4.8 MiB. The run logs `Using 253.0 MiB for in-memory UTXO set (plus up to 4.8 MiB of unused mempool space)`. Without `-maxmempool`, Core lends 286.1 MiB more (a first attempt, stopped and discarded).

**Profiles.** `perf record -g` (frame pointers) of `instructions:u` (period 60,000,011) and `cpu-clock` (97 Hz) (DEC-717), folded per thread. Each loadblk stack is charged to its outermost frame matching a lever, else to `serial-rest`:

| lever | frames |
|---|---|
| read | `ReadBlockFromDisk` (X11 included) |
| flush | `FlushStateToDisk` |
| check-in-connect | `CheckBlock` (inside ConnectBlock) |
| coins-db | `CCoinsViewDB::GetCoin`, `HaveCoin` |
| scripts | `CheckInputScripts`, `CScriptCheck::operator()`, `CCheckQueue` |
| block-undo-write | `WriteUndoDataForBlock`, `SaveBlockToDisk`, `FlushUndoFile`, `FlushBlockFile` |
| events | `CMainSignals::`, the validation interface queue |
| special-tx | `ProcessSpecialTxsInBlock`, `CSpecialTxProcessor` |
| log-tip | the UpdateTip log line |
| serial-rest | everything else on loadblk: ConnectBlock's and ConnectTip's own work |

A lever's ceiling is 1 / (1 − s): the speedup if the lever cost nothing and loadblk stayed the critical path. With AV-E, s is the lever's cpu-clock weight on loadblk over the window's wall. Without assumevalid, s is the lever's instructions:u share of loadblk, since that wall is mostly loadblk waiting on the script queue under the box's load; scripts there are quoted as their total over all threads, with loadblk's part beside it.

**Sampling and exact totals (DEC-720).** On this kernel `perf_event_max_sample_rate` (1000) equals HZ, so each sample throttles its event until the next tick and every run has THROTTLE = SAMPLE; DEC-720 accepts that and discards only runs with more throttles than samples. Lever shares come from the samples. Exact window totals come from a separate `perf stat -I 1000` pass of the same configuration, cut to the same window; the unobserved fraction is 1 − sampled / exact. For instructions:u it is 1.5–2.7% in every pair. For cpu-clock the "unobserved" figure ranges from −12% to 19%, and is 1.3% in the one pair whose two passes ran at similar load: on-CPU time varies with SMT contention, so it measures run-to-run variance, not throttle loss.

**Bench logging off.** `-debug=bench` is not used. In a run with it (av-rec, AV default, kept as the instrumentation reference; THROTTLE 1,407,845 = SAMPLE 1,407,845), the bench log lines cost 9.60% of loadblk's instructions:u and 14.78% of its cpu-clock, enough to skew every share.

**Load and wall.** The box ran other work throughout (1-minute load 20–330). Instruction counts are unaffected; on-CPU time moves a little; wall time and the off-CPU part move a lot. Ceilings use each run's own wall and on-CPU time, so they are internally consistent, but wall comparisons between runs are not.

**Discarded runs.** Run 1 (PF1's period 6,000,011) throttled 9.1 M times against 13.6 M samples and was discarded; its wall also holds a 14,451 s gap from the manager's DEC-705 `systemctl freeze` during the gate switch, a manager action, not a node artefact. The first AV-E stat pass was stopped by the manager at height 681,806 when /work fell under its hard floor, and was rerun (avE-stat2).

## Tables

#### Validity and exact totals

| config | THROTTLE = SAMPLE | window wall (rec / stat) | unobserved instr / cpu | exact instructions:u | exact task-clock | coins flushes in window (peak cache) |
|---|---|---|---|---|---|---|
| AV-E B_DEC43 | 216,025 = 216,025 | 2,004.6 s / 1,870.2 s | 1.46% / 1.28% | 2,777,090,091,934 | 1,702.5 s | 5 (257.7 MiB) |
| AV-E | 179,380 = 179,380 | 1,494.7 s / 1,611.4 s | 1.76% / 10.93% | 2,532,702,024,095 | 1,448.8 s | 0 (981.8 MiB) |
| no-AV | 1,528,619 = 1,528,619 | 6,905.6 s / 5,493.4 s | 2.17% / 4.77% | 29,233,804,113,365 | 11,215.7 s | 0 (981.8 MiB) |
| no-AV replicate | 1,419,970 = 1,419,970 | 3,331.7 s / 5,479.9 s | 2.27% / 19.43% | 29,239,024,497,386 | 11,886.2 s | 0 (981.8 MiB) |
| no-AV par 1 | 1,305,054 = 1,305,054 | 9,874.4 s / 7,955.2 s | 2.66% / -12.18% | 29,220,549,169,789 | 7,469.0 s | 0 (981.8 MiB) |

#### loadblk lever shares, instructions:u (sampled)

| lever | AV-E B_DEC43 | AV-E | no-AV | no-AV replicate | no-AV par 1 |
|---|---|---|---|---|---|
| scripts | 0 | 0 | 39.58% | 43.84% | 91.68% |
| serial-rest | 48.17% | 50.20% | 29.94% | 28.05% | 4.14% |
| read | 27.48% | 28.85% | 17.86% | 16.21% | 2.44% |
| check-in-connect | 10.75% | 11.52% | 7.08% | 6.51% | 0.95% |
| coins-db | 1.77% | 0 | 0 | 0 | 0 |
| flush | 2.98% | 0.03% | 0.02% | 0.03% | 0.00% |
| events | 7.04% | 7.42% | 4.38% | 4.24% | 0.62% |
| special-tx | 0.21% | 0.29% | 0.17% | 0.15% | 0.02% |
| block-undo-write | 0 | 0 | 0 | 0.00% | 0 |
| log-tip | 1.57% | 1.65% | 0.95% | 0.94% | 0.14% |
| log-bench | 0.03% | 0.03% | 0.02% | 0.03% | 0.00% |
| loadblk sampled total | 2,539,320,465,542 | 2,407,560,441,386 | 3,928,980,720,313 | 4,227,000,774,950 | 28,354,745,198,369 |

##### serial-rest's largest callees, instructions:u (share of loadblk)

| callee | AV-E B_DEC43 | AV-E | no-AV | no-AV replicate | no-AV par 1 |
|---|---|---|---|---|---|
| `CTxMemPool::removeForBlock` | 20.04% | 20.85% | 12.60% | 11.62% | 1.73% |
| `CBlockHeader::GetHash` | 6.86% | 7.12% | 4.32% | 4.17% | 0.61% |
| `CCoinsViewCache::Flush` | 6.30% | 6.37% | 3.98% | 3.74% | 0.56% |
| `Consensus::CheckTxInputs` | 4.14% | 4.43% | 2.64% | 2.40% | 0.36% |
| `AddCoins` | 2.16% | 2.38% | 1.44% | 1.35% | 0.19% |
| `GetTransactionSigOpCount` | 1.94% | 1.93% | 1.23% | 1.18% | 0.18% |
| `GetLegacySigOpCount` | 1.10% | 1.10% | 0.68% | 0.68% | 0.09% |
| `GetBlockProofEquivalentTime` | 1.05% | 1.11% | – | – | – |
| `UpdateCoins` | 0.95% | 1.07% | 0.61% | 0.59% | 0.09% |

#### loadblk lever shares, cpu-clock (sampled)

| lever | AV-E B_DEC43 | AV-E | no-AV | no-AV replicate | no-AV par 1 |
|---|---|---|---|---|---|
| scripts | 0 | 0 | 18.17% | 24.08% | 79.66% |
| serial-rest | 49.31% | 54.87% | 53.31% | 47.64% | 11.94% |
| read | 23.61% | 25.38% | 15.48% | 15.69% | 4.45% |
| check-in-connect | 7.52% | 7.88% | 4.65% | 4.76% | 1.37% |
| coins-db | 4.00% | 0.00% | 0 | 0 | 0 |
| flush | 4.59% | 0.13% | 0.11% | 0.09% | 0.04% |
| events | 5.43% | 5.89% | 4.14% | 3.84% | 1.27% |
| special-tx | 0.95% | 0.95% | 0.73% | 0.69% | 0.21% |
| block-undo-write | 0.03% | 0.02% | 0.03% | 0.02% | 0.01% |
| log-tip | 4.50% | 4.77% | 3.34% | 3.14% | 1.03% |
| log-bench | 0.07% | 0.10% | 0.04% | 0.06% | 0.01% |
| loadblk on-CPU (s) | 1,417 | 1,101 | 2,518 | 2,116 | 8,125 |

##### serial-rest's largest callees, cpu-clock (share of loadblk)

| callee | AV-E B_DEC43 | AV-E | no-AV | no-AV replicate | no-AV par 1 |
|---|---|---|---|---|---|
| `CTxMemPool::removeForBlock` | 11.18% | 11.95% | 7.80% | 7.69% | 2.40% |
| `CCoinsViewCache::Flush` | 10.46% | 13.70% | 9.87% | 9.58% | 2.88% |
| `Consensus::CheckTxInputs` | 6.47% | 7.60% | 5.03% | 5.02% | 1.48% |
| `CBlockHeader::GetHash` | 3.39% | 3.75% | 1.79% | 1.94% | 0.56% |
| `CBlockIndex::GetAncestor` | 2.95% | 2.91% | 1.81% | 2.12% | 0.65% |
| `std::unique_lock<std::recursive_mutex>::unlock` | 1.52% | 1.32% | 1.10% | 0.76% | 0.50% |
| `AddCoins` | 1.52% | 1.62% | 1.12% | 1.17% | 0.36% |
| `GetTransactionSigOpCount` | 1.24% | 1.23% | 1.04% | 0.93% | 0.31% |
| `pthread_cond_broadcast@@GLIBC_2.3.2` | – | – | 11.11% | 8.58% | – |
| `pthread_cond_signal@@GLIBC_2.3.2` | – | – | 3.10% | 1.40% | – |
| `CChain::FindFork` | 1.05% | 1.04% | 1.27% | 0.83% | 0.47% |

#### Script instructions, all threads (exact-scaled)

Loadblk plus the check-queue workers, sampled weight over the observed fraction of instructions:u. Loadblk's own share moves with how the queue is scheduled; the total does not.

| config | script instructions:u | share of all instructions:u | on loadblk (share of loadblk) | on the workers |
|---|---|---|---|---|
| no-AV | 26,722,287,318,892 | 91.41% | 1,589,639,037,779 (39.58%) | 25,132,648,281,112 |
| no-AV replicate | 26,727,333,437,966 | 91.41% | 1,895,961,793,573 (43.84%) | 24,831,371,644,392 |
| no-AV par 1 | 26,704,488,947,391 | 91.39% | 26,704,488,947,391 (91.68%) | 0 |

#### Critical path and Amdahl ceilings, AV-E (cpu-clock weight over the window's wall)

| config | wall | loadblk on-CPU | off-CPU | serial-rest | read | check-in-connect | events |
|---|---|---|---|---|---|---|---|
| AV-E B_DEC43 | 2,005 s | 1,417 s | 588 s | 1.535× | 1.200× | 1.056× | 1.040× |
| AV-E | 1,495 s | 1,101 s | 393 s | 1.679× | 1.230× | 1.062× | 1.045× |

#### Lever ceilings without assumevalid (instructions:u share of loadblk)

Without assumevalid the wall is mostly loadblk waiting on the script queue under the box's load, so these ceilings use 1 / (1 − s) with s the lever's instructions:u share of loadblk, not wall time.

| config | scripts | serial-rest | read | check-in-connect | events |
|---|---|---|---|---|---|
| no-AV | 1.655× | 1.427× | 1.217× | 1.076× | 1.046× |
| no-AV replicate | 1.781× | 1.390× | 1.193× | 1.070× | 1.044× |
| no-AV par 1 | 12.018× | 1.043× | 1.025× | 1.010× | 1.006× |

## removeForBlock

ConnectTip calls `m_mempool->removeForBlock(blockConnecting.vtx, height)` whenever the node has a mempool, reindex included (`validation.cpp:3473-3474`). In `txmempool.cpp:1321-1351`:
- `:1334` calls `minerPolicyEstimator->processBlock` once per block. It runs `ClearCurrent` and `UpdateMovingAverages` for three stats (`policy/fees.cpp:669-676`), each of which decays every bucket for every confirmation period (`:227-238`), whatever the block holds.
- `:1344` calls `removeProTxConflicts` for every transaction, whose first step, `removeProTxSpentCollateralConflicts` (`:1066-1082`), calls `mnList.GetMNByCollateral(in.prevout)` for every input (`:1076`). That reaches `GetUniquePropertyHash` (`evo/deterministicmns.cpp:104-107`, `evo/deterministicmns.h:446-448, 526`), which serializes and SHA-256-hashes the outpoint before the map lookup, so it costs the same with an empty pre-DIP3 list. The box has no SHA-NI.

The split, bench off:

| run | removeForBlock (share of loadblk) | `removeProTxConflicts` (of which inside `CSHA256`) | `processBlock` (of which `UpdateMovingAverages`) |
|---|---|---|---|
| AV-E, B_DEC43, instructions:u | 20.04% | 49.5% (94.6%) | 48.5% (97.7%) |
| AV-E, B_DEC43, cpu-clock | 11.18% | 34.4% (84.4%) | 60.1% (95.6%) |
| no-AV replicate, instructions:u | 11.62% | 48.6% (95.5%) | 49.0% (97.8%) |
| no-AV replicate, cpu-clock | 7.69% | 32.2% (81.4%) | 61.6% (95.4%) |

## Q10: the window

The window here is the pre-DIP3 range, 1–1,028,159, which this addendum measures. The data reaches 2,094,869 and DEC-671 forbids fetching more, so a second segment must end at or before that height. Proposed: the data's last 100,000 blocks, 1,994,870–2,094,869, measured once CH5 and P3 land, when rdashd can replay them.

## Artifacts

Scratch `/work/scratch/ch4` (owner CH3): `runs/<run>/` holds each run's `run.json`, `debug.log`, folded stacks, `levers.json` and, for stat passes, `stat.csv`; tools `pf1/run_main.py`, `pf1/analyze_levers.py`, `pf1/addendum_tables.py`, `bin/c2run.sh`. perf.data per sampled run: av-rec (bench on) 416 MB, av-rec-nb 420 MB, noav-rec 453 MB, noav1-rec 447 MB, avE-rec 54 MB, avE263-rec 65 MB; 1.86 GB in all, plus thr-smoke's 79 MB.
