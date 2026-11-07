# CH1-node: the validation actor, chain selection and the node's block index

| | |
|---|---|
| Status | **rev3.1, final** (Opus high APPROVE-WITH-CHANGES at c29315b34 closed under DEC-156, §17; DEC-157 addendum applied, §17.1), for the manager's check. rev3: f8ee7352; rev2: 20ccc613 (Sol r2 closed, §16); rev1: 7f49c748 (Sol r1 closed, §15); rev0: 3c7abeda |
| PLAN row | CH1-node (PLAN §4, M2, `dashd-chainstate`, deps ST1, SB4, i:SB5; D O-h, impl O-h, rev Sol→O-h) |
| Inputs | SB0 rev6.1 + errata E1–E4 (DEC-119); SB4 rev7.1 (DEC-149) and its shipped code at C2 6ef0a8e75 (`node/sb4-dash-chain`; the asks of §11 are stated against it); SB5 at 17750c58 + a7297d13 and DEC-151/152/153/155; DEC-156 (LO-9, publish before notify); CH2 rev5 (DEC-103); EV addendum rev6 (DEC-109, DEC-113, DEC-117); ST3a errata (DEC-148); P2a rev3 §2.5.3 (R1–R8); A1 §2.10; the `sb4-core-scenarios` harness at 7c35a6c6 and DEC-147; DEC-95/99/102 (Core semantics over stricter invariants) |
| Semantics | Dash Core v24.0.0-rc.3. `v:` = `src/validation.cpp`, `bs:` = `src/node/blockstorage.cpp`, `h:` = `src/chainlock/handler.cpp`, `cl:` = `src/chainlock/chainlock.cpp`, `rpc:` = `src/rpc/blockchain.cpp`, `np:` = `src/net_processing.cpp`, `cs:` = `src/node/chainstate.cpp`, `is:` = `src/instantsend/net_instantsend.cpp`, `vi:` = `src/validationinterface.cpp`, `sch:` = `src/scheduler.cpp`, all at that tag |
| Feeds | the node runtime (`ValidationSignals`, §8.2), CH3a (the `BlockConnector` seam), CH3b (reorg integration), CH2b (flush sites, B1 index records, recovery steps 4 and 11), CH5 (VerifyDB, import), CH7a (evo connect, `mn_list`), LK1a (`ClIntake`, `RecordChainLock`), P2a-node/P2b (`ChainAccess`), MP1 (`MempoolSink`), ND1 (init and shutdown), RP0/RPC rows (`Query`), O9a |

## 0. The design on one page

1. **V is one thread that owns `&mut HeaderChain` and CH1-node's `NodeIndex`.** Every mutation of the block index, the candidate set, the ChainLock signatures and the tip runs on V, from a bounded `ChainCommand` channel (§2). Readers use SB4's lock-free snapshot and seqlocked entries, wrapped in CH1-node's `ChainView` (§6).
2. **Core's two locks become two command classes.** `cs_main`-class commands (class M: headers, the accept half of a block, chainlock recording, RPC reads, ATMP) may run at Core's release points inside a long operation. `m_chainstate_mutex`-class commands (class C: `ActivateBestChain`, `InvalidateBlock`, enforcement) never overlap; one that arrives mid-operation is deferred (§2.4). This is R6, at Core's granularity.
3. **The tip mutex is held where Core holds `cs_main`.** Each `ActivateBestChain` inner loop (`v:3763-3827`) and each `InvalidateBlock` iteration (`v:3936-4004`) is one hold of SB4's tip mutex. Inside a hold, steps get their own `seq`, each step's F1 event is fanned out at the step and its pending fanout is drained before the next step (SB5 §3.3, DEC-153), and one snapshot is stored at release. Readers therefore never see a mid-reorg fork-point tip, as Core's `cs_main` readers never do, while subscribers keep receiving every step. **Notifications follow publication** (rev3, DEC-156 (2)): the hold's queued validation callbacks are staged and spliced into the callback queue in O(1) at the release, after the snapshot and `ChainView` stores, and the UI and `TipWatch` notifications fire after them, so a party woken for tip X always reads a published state that contains X (§2.5, invariant N1). This generalizes SB4's `enforce_block` composite to V's other two Core scopes and needs one SB4 API (CH1-A1, §11). **Q1.**
4. **The control flow is Core's, line by line** (§4): `ActivateBestChain`, `ActivateBestChainStep`, `ConnectTip`, `DisconnectTip`, `FindMostWorkChain`, `InvalidateBlock`, `ResetBlockFailureFlags`' node half, `PreciousBlock`, `AcceptBlock`, `ReceivedBlockTransactions`, `LoadChainTip` and the load-time candidate rules. Block contents are behind seams: `BlockConnector` (CH3a/CH7a), `MempoolSink` (MP1), `FlushSite` (CH2b), `BlockChecks` (CS3).
5. **CH1-node owns what Core keeps beside `CBlockIndex`:** `nChainTx`, `nSequenceId`, the two sequence counters, `nLastPreciousChainwork`, `setBlockIndexCandidates` (Core's comparator, arena id for the address, D9), `m_blocks_unlinked` (a true multimap) and `m_best_invalid` (§3). It writes the block-data group (`n_tx`, `file`, `data_pos`, `undo_pos`, `HAVE_DATA`, `HAVE_UNDO`) through SB4's `modify`, at Core's sites and with Core's dirtiness.
6. **ChainLocks on V** (§5). Every CLSIG starts with a class M `ClIntake` on V, which performs Core's `CheckActiveState` before any early return (`h:136`). `dashd-locks` then does the seen check, the height precheck and the signature arithmetic, and V **records** a verified lock as a class M command (Core's `UpdateBestChainlock`, so relay is not delayed behind a reorg) and **enforces** it as a class C task (Core's 0 s scheduler task), notifying with the `(floor, signature)` pair it captured at entry (`h:283`). V mirrors Core's `bestChainLock`, `bestChainLockWithKnownBlock` and `bestChainLockHash`, including the late-header transition (`cl:156-171`), and keeps `lastNotifyChainLockBlockIndex`.
7. **Flush sites are Core's; the freeze follows the step.** CH2b's decision (and any CRITICAL wait) runs at `v:3304` and `v:3457`. The freeze it decides runs right after the tip step, under the same guard, and captures `FreezePacket.{new, tip_seq}` as **V's private tip after the step and that step's `seq`**: `(id, q+1)` after connecting `id`, `(parent(d), q+1)` after disconnecting `d`. V asserts `new == coins.best_block() == evo.best_block()` (§7.1, D3). **Q3.**
8. **The index persists only in CH2's B1** (D34): `take_dirty()` at every freeze and every index-only write. CH1-node marks dirty exactly at Core's `m_dirty_blockindex` sites. Recovery calls SB4's `load`, then rebuilds CH1-node's tables with Core's load rules, then `LoadChainTip` (§7).
9. **`ChainView` is published at every snapshot store, unthrottled** (§6). It carries the SB4 snapshot plus `mn_list`, the ChainLock signatures, `mtp` and the fork warning. IBD is a lazy, latched query (R4), not a field. A1's 100 ms throttle is withdrawn, because SB4 already publishes per step and the wrapper costs one `Arc` per step (PF0 line `chainstate-view-publish` guards it). **Q5.**
10. **Core's validation callbacks get their own queue** (§8.2, DEC-153). `BlockConnected`, `UpdatedBlockTip` and the other queued `CValidationInterface` callbacks go through `ValidationSignals`, a FIFO run on its own thread like Core's `SingleThreadedSchedulerClient`. Its enqueue is O(1) and non-blocking under a hold (DEC-152), and backpressure happens only at Core's `LimitValidationInterfaceQueue` sites. SB5's F1 stream stays `TipStep` and carries none of them. **Nothing V waits on ever waits on V** (rev3, LO-9, DEC-156 (1)): F1 subscriber pop tasks, the callback thread and W may post to V or any actor but never await an actor reply, and a task-local marker makes every reply future refuse such an await at runtime.
11. **Tests** (§9): the PLAN row's acceptance through V; every existing harness scenario replayed through **real** V (lifting SB4's D40 limitation, since CH1-node implements `FindMostWorkChain`), plus 17 new Core-judged `ch1-*` scenarios; directed ChainLock-schedule, freeze-labelling and notification-ordering tests; a brute-force candidate property suite; a lock-and-wait model with LO-9's negative controls; R6/R8 latency and coherence tests; seqlock stress. FT ports run at M3, once CH3a and ND1 land. Size: 25 agent-days, about 15 on the critical path with two implementers (§10). The Core-judged scenarios need harness task HX1 (§11, about 4 agent-days, its own PLAN row), which only N9b waits for. **Q4** proposes moving the index-side ABC, invalidate, reconsider and precious work from CH3b to CH1-node.

## 1. Scope

### 1.1 The PLAN row and SB0 §12.1, item by item

| Item | Where |
|---|---|
| `ChainCommand` actor on V | §2 |
| `apply_chainlock` from `dashd-locks` (m9), as `ClIntake` + `RecordChainLock` + `ClEnforce` | §5 |
| best-header hooks at `InvalidateBlock`, `MarkConflictingBlock`, `InvalidChainFound` (DEC-112 O1) | §4.6 (`v:3951`, `v:3969`), §4.3 (`v:2043`); `MarkConflictingBlock`'s (`v:4089`, `v:4107`) run inside SB4's `mark_conflicting_block` and are checked through V by the harness (§9.2) |
| block-data status and positions on the shared entries | §3.2, §4.9 |
| `ChainView` (+ `mn_list`, `best_chainlock`, `ibd`, `mtp`, `bls`, `epoch`) | §6 |
| index records in CH2's B1 | §7.2 |
| R6 | §2.4, §9.4 |
| SB4 §1.1's CH1-node column: `DisconnectTip`, candidates, `m_best_invalid`, `check_cl_active_state`/`enforce_best_chainlock` at the handler's sites, `nSequenceId`, `nChainTx`, `m_blocks_unlinked`, the `ChainAccess` wrapper with `loading_blocks`, `chain_tips_with(has_txs)`, candidate admission after `reset_failure_flags`, the `!pindex` `m_best_invalid` substitution | §3, §4, §5, §6 |
| the CH1 row's tests (full headers, parallel load, `-checkblockindex`, seqlock stress, walk-backs vs the oracle) | §9.1 |

### 1.2 The split with CH3a and CH3b (Q4)

SB4 §1.2 assigns "the candidate set and `ActivateBestChain`" to CH1-node, while PLAN's CH3b row still lists `activate_best_chain`, candidates, invalidate/reconsider/precious and invalid-chain handling. The best-header hooks sit inside those functions, and SB4's `TipHost` needs V's `DisconnectTip`, so the control flow cannot wait for CH3b. Proposal:

- **CH1-node** implements the whole V-side control flow of §4 against seams. The only block-content work it does itself is reading and writing block files (ST2) and recording `ReceivedBlockTransactions`.
- **CH3a** implements `BlockConnector` (`ConnectBlock`/`DisconnectBlock`, undo, the evo transaction through CH7a) and the `VALID_SCRIPTS`/`HAVE_UNDO` writes through CH1-node's `BlockStatusWriter` (§4.8).
- **CH3b** shrinks to the reorg *integration*: the disconnect pool and `MaybeUpdateMempoolForReorg` through `MempoolSink` (CONS-25), ChainLock-conflict flag persistence across restart (CONS-11; RP-V24/DEC-82 left one target release, so there are no profiles), the real-block FT suites (`rpc_invalidateblock`, `rpc_preciousblock`, `rpc_getchaintips`, `p2p_invalid_block`), the F01 restart case and the reorg-across-flush crash test. Its size drops from L to M.
- CH1-node grows from M to L (§10). Proposed row texts are in §12.

### 1.3 Non-goals

- Header acceptance, the arena, `best_header`, snapshots, the anchor, the ChainLock floor and `enforce_block`/`mark_conflicting_block` (SB4); the F1 tip feed and residency (SB5). The callback queue `ValidationSignals` is **defined** here (§8.2) and owned and run by the node runtime (ND1), per DEC-153.
- `ConnectBlock`/`DisconnectBlock` and undo (CH3a); evo (CH7a); flush, recovery and subscribers (CH2b); pruning policy (CH8); VerifyDB levels (CH5).
- ChainLock signature verification, the seen map, relay and `-chainlocknotify` (LK1a). The coinbase-ChainLock trigger (`np:2164`) is LK1a's; it ends in `ClIntake` and `RecordChainLock { from: None }` (§5.1).
- assumeutxo (NG-6). Core's background-chainstate branches (`m_disabled`, snapshot-base filters, `MaybeCompleteSnapshotValidation`) are omitted; with one chainstate they are unreachable (D9).

## 2. The actor and its messages

### 2.1 Placement

`dashd-chainstate::chain`:

```text
chain/
  actor.rs      V's loop: command classes, deferred queue, release points, timers, shutdown (§2.3–§2.5)
  command.rs    ChainCommand, replies, ChainQuery (§2.2)
  index.rs      NodeIndex: nChainTx, nSequenceId, candidates, unlinked, best_invalid (§3)
  abc.rs        ActivateBestChain, ActivateBestChainStep, FindMostWorkChain, ConnectTip, DisconnectTip (§4.1–§4.5)
  failure.rs    InvalidateBlock, ResetBlockFailureFlags (node half), PreciousBlock, InvalidChainFound, InvalidBlockFound, fork warnings (§4.3, §4.6, §4.7)
  accept.rs     ProcessHeaders, ProcessBlock, AcceptBlock, ReceivedBlockTransactions, NotifyHeaderTip (§4.9)
  locks.rs      record/enforce, ClSigs mirror, TipHost for V, timers (§5)
  view.rs       ChainView, ChainViewHandle (ChainAccess, ConnChainView, ClReader) (§6)
  load.rs       load rules, LoadGenesisBlock, LoadChainTip (§7.3)
  check.rs      CheckBlockIndex, node half (§4.10)
  seams.rs      BlockConnector, BlockStatusWriter, MempoolSink, FlushSite, BlockChecks (§8.1)
  signals.rs    ValidationSignals: Core's CMainSignals queue and synchronous listeners (§8.2)
```

### 2.2 `ChainCommand`

It replaces A1 §2.10's enum (erratum E-A1-1, §12):

```rust
pub enum ChainCommand {
    // ---- class M: Core runs these under cs_main only
    ProcessHeaders { batch: HeaderBatch, from: Option<NodeId>, reply: oneshot::Sender<HeadersResult> },  // P2a R7
    ProcessBlock   { block: Arc<Block>, hash: BlockHash, force: bool, from: Option<NodeId>,
                     reply: Option<BlockReply> },                     // accept half is class M; its ABC tail is class C
    ClIntake       { reply: oneshot::Sender<ClIntakeReply> },         // §5.1: CheckActiveState before any early return (h:136)
    RecordChainLock { sig: Arc<ChainLockSig>, sig_hash: Uint256, from: Option<NodeId>,
                      reply: oneshot::Sender<ClReply> },              // §5.1; with ClIntake replaces A1's EnforceChainLock
    ReconsiderBlock { hash: BlockHash, ignore_chainlocks: bool, reply: Reply },   // reset is class M, ABC tail class C
    PreciousBlock  { hash: BlockHash, reply: Reply },                 // first scope class M, ABC tail class C
    TestBlockValidity { block: Arc<Block>, check_pow: bool, check_merkle: bool, reply: Reply<BlockValidationState> },
    VerifyDb       { check_level: u32, check_blocks: i32, reply: Reply<VerifyDbResult> },   // EV addendum §2.6; CH5 implements
    Flush          { mode: FlushStateMode, manual_prune_height: Option<u32>, reply: Option<Reply> },  // CH2 §3.3
    Mempool(MempoolCommand),                                           // MP1's ATMP phase (CH2 ruling 9)
    OnV(Box<dyn FnOnce(&OnV<'_>, &mut ChainQueryMut<'_>) + Send>),    // EV dispatched routes (shared gate first)
    LoadMempoolDone,
    // ---- class R: reads that need cross-entry consistency (A1 §2.10.2)
    Query(Box<dyn FnOnce(&ChainQuery<'_>) + Send>),
    // ---- class C: Core takes m_chainstate_mutex
    ActivateBestChain { reply: Option<Reply> },                        // init, -loadblock end, generate*, IS conflicts
    InvalidateBlock { hash: BlockHash, reply: Reply },                 // InvalidateBlock + the RPC's ABC (rpc:2015-2018)
    IsConflictInvalidate { blocks: Vec<BlockHash>, activate: bool, reply: Reply },   // is:614-645 (§5.6)
    ClEnforce { source: ClTrigger },                                   // h:63-72, h:170-176, h:196-205 (§5.2)
    // ---- control
    SetLoading(LoadingFlags),                                          // fImporting / fReindex (CH5)
    Shutdown,
}
pub enum ClTrigger { Timer, Recorded, TipScheduled }
pub struct ClReply { pub relay: bool }       // Core: ProcessNewChainLock returns the inv iff UpdateBestChainlock succeeded
pub struct ClIntakeReply { pub best_height: i32 }   // GetBestChainLockHeight() after CheckActiveState (h:146)
pub struct BlockResult { pub new_block: bool, pub state: BlockValidationState, pub accepted: bool }
pub enum BlockReply {                         // rev2 (R2-M1): which completion the caller waits for
    Activated(ReplySender<BlockResult>),      // Core's ProcessNewBlock return: after the block's own ABC (v:4899-4906). C-dependent
    Accepted(ReplySender<BlockResult>),       // sent at the accept scope's end, before the ABC tail runs. R/M-complete
}
// rev3 (DEC-156): every actor reply is a ReplySender/ActorReply pair (§8.3); ActorReply refuses
// to be awaited from a task that carries the no-wait marker (LO-9).
```

- `ProcessHeaders` keeps P2a's R7 shape. `HeaderBatch.hashes[i] == X11(headers[i])` is checked under `debug_assertions` only, as `ASSERT_IF_DEBUG` (`v:4770`).
- `ProcessBlock`'s ordinary reply, `BlockReply::Activated`, is sent after the block's own ABC, as `ProcessNewBlock` returns after `ActivateBestChain` (`v:4899-4906`). `BlockReply::Accepted` is the distinct acceptance-only reply (R2-M1): same `BlockResult`, sent when the class M accept scope ends; the ABC tail still runs, deferred if necessary. Core has no such return; the only difference is when the caller resumes. **Every Core caller of `ProcessNewBlock` uses `Activated`** (rev3, OH-m4), normatively: P2P's block, `cmpctblock` and `blocktxn` paths (msghand reads the peer's next message only after ABC, so a `block` followed by `getheaders` is answered with B), `submitblock`, `generate*` and `-loadblock` import. `Accepted` has no Core caller. It is kept for a poster that wants the accept verdict without waiting on activation, and under LO-9 a marked task can use it only by forwarding the receiver to an unmarked task (§8.3). `BlockResult.state` also carries what Core passes to `BlockChecked` on failure (`v:4890`), so P2b's `block_checked` sees it in the reply as well as on the event.
- `TestBlockValidity` and `VerifyDb` are routed here; CH3a/CH7a and CH5 implement them under the EV addendum's `AttemptHandle` (§2.6).
- `Query` gets a read-only `ChainQuery` (`&HeaderChain`, `&NodeIndex`, `&ClSigs`). Examples: `getchaintips` via `chain_tips_with(&|id| idx.n_chain_tx(id) != 0)` (SB4 §10.2), `getblockheader` confirmations, `getblockchaininfo`.
- A1's `PruneTo` folds into `Flush { mode: None, manual_prune_height }` (CH2 §3.3), and A1's `EnforceChainLock` into `ClIntake` + `RecordChainLock` + `ClEnforce`.

### 2.3 V's state

```rust
pub(crate) struct V<S, R, B: BlockConnector, M: MempoolSink, F: FlushSite> {
    cx: VContext,                 // EV addendum: zero-sized, created once here (private ctor)
    chain: HeaderChain<S, R>,     // SB4; V is its only writer
    idx: NodeIndex,               // §3
    cl: ClState,                  // §5: ClSigs mirror, last_notify, tip_scheduled
    io: VIo<B, M, F>,             // connector, mempool sink, flush site, block files, events, checks
    view: Arc<ArcSwap<ChainView>>,// §6
    loading: Arc<LoadingFlags>,   // Core LoadingBlocks(): fImporting || fReindex
    rx: Receiver<ChainCommand>,
    deferred: VecDeque<Deferred>, // class C work waiting for the running composite (§2.4)
    running: Option<Composite>,   // Some while an ABC or InvalidateBlock is in progress
    shutdown: Arc<AtomicBool>,    // ShutdownRequested()
}
```

`TipHost` is implemented by a short-lived `VHost<'a>` that borrows `idx`, `cl` and `io`, never `chain`: every `TipHost` method receives `&mut HeaderChain` from SB4.

### 2.4 Command classes and release points (R6)

Core serializes chain mutation with two locks. `cs_main` is released between `ActivateBestChain`'s outer iterations (`v:3763`, `v:3828`) and between `InvalidateBlock`'s iterations (`v:3941`, `v:4004`). `m_chainstate_mutex` is held for a whole ABC (`v:3738`), a whole `InvalidateBlock` (`v:3908`) and a whole `EnforceBlock` (`v:4053`). V maps that onto three classes:

| Class | Core lock | Commands | When V runs it |
|---|---|---|---|
| R | `cs_main` (read) | `Query`, read-only `OnV` routes | idle; at a release point; in CH2's wait loops **outside** a hold |
| M | `cs_main` | `ProcessHeaders`, `ProcessBlock`'s accept half, `ClIntake`, `RecordChainLock`, `ReconsiderBlock`'s reset, `PreciousBlock`'s first scope, `TestBlockValidity`, `VerifyDb`, `Flush`, `Mempool`, mutating `OnV`, `LoadMempoolDone`, `SetLoading` | idle; at a release point |
| C | `m_chainstate_mutex` | ABC (explicit, or the tail of a class M command), `InvalidateBlock`, `IsConflictInvalidate`, `ClEnforce` | idle only. At a release point it goes to `deferred` |

**Release points** are V's equivalent of Core releasing `cs_main` inside a class C operation: the top of each ABC outer iteration and of each `InvalidateBlock` iteration. At a release point V takes the commands queued at that moment (`rx.len()`, so the operation still progresses) and runs each class R or M command to completion. A class C command, or the class C tail of a class M command, goes to the back of `deferred`. When V goes idle, `deferred` drains before `rx`. Core's waiters on `m_chainstate_mutex` are not FIFO; FIFO is one of its schedules.

**Inside a hold** (§2.5) V's waits run the **in-hold service loop**, which does only what the feeds and CH2 require: `chain.retry_pending()` for SB5's pending F1 fanout, W's release and completion wakes, W's `PruneClears`, and subscribers' `FlushDue` (CH2 §4.2; a `FlushDue` between a connector commit and its tip step is latched, §7.1). No class R or M command runs there. A class R command that arrives during a hold waits for the next release point. Core's `cs_main` readers cannot run there, and answering would expose a state Core never shows (a fork-point tip, a half-applied `InvalidateBlock` iteration). This narrows CH2's service loop and SB5 §3.3's "answers … queries" for waits inside holds only; outside holds their rules stand (E-CH2-1, E-SB5-1).

**R6 restated** (erratum to P2a §2.5.3, E-P2a-1). "Between `activate_best_chain` steps" means at Core's release points. The inner loop ends (Core `v:3812`, `while (!m_chain.Tip() || (starting_tip && CBlockIndexWorkComparator()(m_chain.Tip(), starting_tip)))`) as soon as the tip is **not worse than `starting_tip` under the full comparator**, or earlier when there is nothing to do: no candidate or the candidate is the tip (`v:3777-3780`), or a failed connect cleared `most_work` (`v:3791-3794`) and the next search breaks. Under the comparator an equal-work branch with a smaller (for example precious, negative) `nSequenceId` is already better, so the hold can end at **equal** work. Inside `abc_step` the batch loop stops at strictly more work than the step's old tip (`v:3805`), or at `most_work`. With valid candidates in IBD, each outer iteration connects one block, so a `ProcessHeaders` waits for at most one block connect; this is not a bound on the work spent rejecting invalid candidates. In a reorg, a `ProcessHeaders` waits for the whole hold, exactly as `ProcessNewBlockHeaders` waits for `cs_main` in Core; a deep reorg can take arbitrarily long, as Core's can, and R6 promises no fixed real-time bound. Progress is guaranteed by bounded servicing at each release point plus FIFO deferral of class C work, given finite operations and eventual I/O completion. The R6 test (§9.4) asserts these predicates.

### 2.5 Holds

A **hold** is one critical section of SB4's tip mutex owned by V across several Core statements: `chain.hold_tip()` → steps → `chain.release_tip()` (SB4 API CH1-A1, §11). Inside a hold:

- `chain.connect_tip`/`disconnect_tip` assign each step its `seq` and fan its F1 event out at the step (D43, H5), but store no snapshot;
- **after every step, before anything else that could start another step**, V runs `while chain.retry_pending() == Blocked { in_hold_service_wait() }` with the held guard (SB5 §3.3's node rule, extended from `enforce_block` to V's holds; DEC-153 (1)). So SB5's writer rule (no step with `pending` non-empty) holds inside holds, and a slow subscriber is handled per step, never deferred to an outer iteration;
- the Core validation callbacks of the step are **staged** (rev3, DEC-156 (2)): V appends them, in emission order, to a hold-local `Staged` list whose nodes it allocates itself (§8.2). Nothing reaches the shared queue during the hold. That includes the callbacks of seams V calls under the hold (`MempoolSink`'s `TransactionRemovedFromMempool`, CH2b's `ChainStateFlushed`, CH7a's queued signals), which receive V's staging sink in their context and never enqueue through a clone of `ValidationSignals` while V holds L1;
- the UI notifications (`NotifyBlockTip`) and `TipWatch` are recorded, not fired;
- synchronous listeners run inline as in Core (LO-5) and receive every V-evaluated fact they need as arguments (`ibd`, the new tip, the fork). They never read the published view, which still shows the pre-hold state;
- `publish_status()` is deferred to the release;
- V's only cross-thread waits are the in-hold service loop's: subscriber pops for the pending fanout (SB5 §3.3, CH2 rule 5), W's release (CACHE_CRITICAL/EVO_CRITICAL) and completion (`-dbcrashratio`), and the `BlockFiles` mutex during W's barrier. Every one of them runs `retry_pending` and answers W's `PruneClears` and `FlushDue`. SB4 §13.4's H1–H7 make each complete (§2.6);
- `release_tip()` stores one snapshot; V then runs the rest of `scope_end()` (the mirror reconcile and the `ChainView` store, §4.12, §6.2); **then** V splices `Staged` into the shared queue in one O(1) L2s section (§8.2) and wakes the callback thread; **then** it sets `TipWatch` to the final tip and fires the recorded UI notifications in order. `release(hold)` below means this whole sequence.

**Invariant N1 (publish before notify, normative, DEC-156 (2)).** No notification of tip X, through any path (a queued callback, a UI or `-blocknotify` notification, `TipWatch`, a ZMQ message derived from either), is observable before a published snapshot and `ChainView` contain X. Core gets this from `cs_main`: every queued callback, `uiInterface.NotifyBlockTip` and `g_best_block` is emitted under it (`v:3797-3826`, `v:3212-3216`), and every reader that acts on one and then reads chain state takes `cs_main` first (`np:6338`, `rpc:300-305` then `getblockcount`), so it sees the post-scope state. Staging is one of Core's schedules (the scheduler thread got no CPU until `cs_main` was released). It adds no wait: nothing waits for these callbacks inside a hold (LO-4), and `limit_queue` runs only outside holds, where `Staged` is empty. Signals V emits outside holds (class M scopes, `ClEnforce`'s `NotifyChainLock`) are enqueued after that scope's `scope_end` (§4.12) for the same reason. `enforce_block`'s callbacks (`conflicting_marked`, §5.4) are staged the same way and spliced after `enforce_best_chainlock` returns and V publishes.

Other producers (IS, governance, MP1's non-V paths) may enqueue while V holds; their entries then precede the hold's staged ones. That is one of Core's schedules for every producer that does not hold `cs_main` when it enqueues. A producer that enqueues under `cs_main` in Core runs on V in this design (ATMP is `Mempool`, ChainLock notification is `ClEnforce`), so it cannot enqueue during a hold.

| Hold | Core scope | Snapshot stores |
|---|---|---|
| ABC inner loop | `LOCK(cs_main)` `v:3763-3827` | one per outer iteration |
| `InvalidateBlock` iteration | `LOCK(cs_main)` `v:3942-4003`, then the final scope `v:4008-4039` | one per iteration, one for the final scope |
| `enforce_block` (SB4) | `LOCK2(m_chainstate_mutex, cs_main)` `v:4053` | one (SB4 §9.1) |
| `ReconsiderBlock` reset, `PreciousBlock` first scope, accept half | `LOCK(cs_main)` (`rpc:2058`, `v:3863`, `v:4878`) | none needed: status-only changes, then one `publish_status()` |

SB4's D28 says that ABC and `InvalidateBlock` "release `cs_main` between steps" and therefore publish per step. That is true of `InvalidateBlock` but not of ABC's inner loop, which keeps `cs_main` (and the mempool lock) "until we've made forward progress" (`v:3769-3770`). Q1 asks the manager to accept the hold as a D28 refinement. SB4's tests are unaffected: A-26's trailing tip, A-39's handoff exclusion and A-40/A-41's waits all reason about one hold, and V's holds obey the same contract.

**Q1 against SB5's per-step servicing (rev1).** The hold withholds exactly what Core's `cs_main` withholds and nothing a subscriber needs:

| Party | During an ABC hold | Core |
|---|---|---|
| F1 subscribers (CH2 driver, indexes, LK1b) | receive every step's event at the step; V drains pending fanout per step; their pop/ack paths take no L1 (H4) and await no actor reply (LO-9); externally visible state they derive is fenced by `tip_seq` (§6.2) | index callbacks are queued and run off `cs_main` |
| W | its acks arrive on its own channel, which every in-hold wait services before any deferred control mirror; `PruneClears`/`FlushDue` are answered | Core's flush is synchronous; nothing to starve |
| snapshot readers (P2a, RPC fast paths) | see the complete pre-hold snapshot | `cs_main` readers wait, then see the post-loop state |
| class R/M commands | wait for the release point | wait for `cs_main` |
| `register`, `subscribe_at`, `deregister` (rev3.1, DEC-157 (2)) | never block on the hold. `register` and `subscribe_at` return `HoldActive` with a release wake, and the caller retries after the release. `deregister`'s step 4 uses `try_lock` and leaves the rest to SB5's lazy cleanup | `NextSyncBlock`/`m_synced` run under `cs_main` (`index/base.cpp:163-170`); the caller waits for the scope, which our `HoldActive` retry reproduces without blocking |
| `ValidationSignals` consumers | run concurrently on earlier entries; the hold's own callbacks reach them only after the release (N1); V never waits on them inside the hold | the scheduler thread runs concurrently with `cs_main`; a callback that reads chain state takes `cs_main` and so sees the post-scope state |
| UI, `-blocknotify`, `waitfor*`, GBT long-poll (`TipWatch`) | fired after the release (N1) | emitted under `cs_main`; their readers take `cs_main` |

Per-step snapshot publication would add only one effect: a reader could observe the fork point or a half-connected branch mid-reorg, which no Core `cs_main` reader can. The IBD cost is nil (one connect per iteration). DEC-144's pressure decision and CRITICAL wait stay at **every** disconnect, and DEC-145's events stay at each step.

### 2.6 Lock order

From outermost to innermost. A thread acquires only downward.

| # | Lock | Holders |
|---|---|---|
| L1 | SB4 tip mutex (`TipCell.state`) | V (steps, holds, `publish_status`, `retry_pending`); SB5's `register`, `subscribe_at` (with `finish()`) and `deregister` callers, which never wait out a hold (`HoldActive`, `try_lock`; DEC-157 (2)) |
| L2 | SB5 slot mutexes and hub lock (DEC-152 leaves) | V's producer under L1; subscribers alone (H4) |
| L2s | `ValidationSignals` queue mutex (a DEC-152 leaf, §8.2) | V's enqueue under L1; the signals thread for O(1) pops |
| L3 | EV gate (`MembersFacade::gate`) | V exclusive inside `begin_attempt`…`release_attempt`; V shared for `OnV`; caller threads shared for the allow-list |
| L4 | EV facade cache mutex | under L3 |
| L5 | EV snapshot cache lock | under L4 or L3 |
| L6 | ST2 `BlockFiles` mutex | V's appends and undo writes; W's barrier (I/O only) |
| L7 | SB4 index lock | write: V only, to insert, **never under L1** (SB4 §13). Read: any thread, including V under L1 or L3 and EV routes under L3; no thread other than V ever writes, so a reader under L1/L3 waits only for V's own insert, which never holds L1 |
| L8 | entry seqlocks, store-internal locks | leaves |

Rules (normative):
- **LO-1.** V takes L1 → L3 → L4 → L5 → L6, never backwards. The attempt (L3 exclusive) opens inside `connect_block`/`disconnect_block` and is released (CH7a's log applied) before the connector returns.
- **LO-2.** No thread except V takes L1 while holding L3–L6 or L2. SB5's `subscribe_at`/`deregister` callers hold nothing else (H4).
- **LO-3.** V never waits on another thread while holding L3, with one bounded exception (rev3, OH-n2): a block or undo write inside the connector takes L6 under L3 and can wait while W's files barrier holds L6. W holds L6 only for I/O and takes no other lock under it (CH2 H1), so that wait ends when the I/O does. CH2's IF_NEEDED site comes after the connector returns (§4.4 step 8, §4.5 step 6), so a CRITICAL wait holds L1 only.
- **LO-4.** Under L1, V's cross-thread waits are only the in-hold service loop's (§2.5). V does not touch the shared `ValidationSignals` queue during a hold; it appends to its own `Staged` list and splices it at the release with one O(1) L2s section (§2.5, §8.2). The queue's bound is enforced only at Core's `LimitValidationInterfaceQueue` sites (`v:3716`, called at `v:3760` and `v:3940`), where V holds nothing and `Staged` is empty (DEC-153 (2)).
- **LO-5.** Synchronous listeners that V calls inline (`BlockChecked`, `NewPoWValidBlock`, `SynchronousUpdatedBlockTip`, `AcceptedBlockHeader`, `NotifyHeaderTip`, `NotifyMasternodeListChanged`; §8.2), callbacks run under SB5's registration `finish()`, and every waker V fires (DEC-152 (2): notification-only) never send a `ChainCommand` and await it, and never take L1. A1 §2.10.2 already forbids `ChainHelper` back ends from calling V.
- **LO-6.** No thread awaits an actor reply (a `ChainCommand` reply or any other actor's, since any actor may await V) while holding L1–L6. This is the EV addendum's dispatch rule, extended to L1, L2, L6 and to every actor (rev3, LO-9). The EV gate's shared guard sets the no-wait marker of LO-9 for its lifetime, so the rule is checked at runtime for L3.
- **LO-7.** V accepts headers (`accept`, `accept_header`, which take L7) only outside holds.
- **LO-8** (rev2, R2-M1; rev3: a special case of LO-9). The `ValidationSignals` thread never awaits a V reply that needs a class C command or tail. V waits for the queue in `limit_queue` while a class C operation runs (Core: `v:3738` takes `m_chainstate_mutex`, `v:3754-3760` waits for the queue under it and warns that a callback reaching ABC may deadlock), and services only R/M there, deferring class C work. rev2 still let a listener await an R/M-complete reply; LO-9 withdraws that, so §8.3's classification now matters only for unmarked callers.
- **LO-9** (rev3, DEC-156 (1), normative). **Nothing V waits on ever waits on V.** The tasks V can wait on are: every F1 subscriber's pop/ack task (the in-hold drain), the `ValidationSignals` thread and so every listener it runs (`limit_queue`), W (CRITICAL, completion and files waits) and shared EV-gate holders (the exclusive acquire). Each of them may **post** a command to V or to any actor, but never **awaits** a reply that needs V's service, directly or transitively through another actor. Since any actor may itself await V, the enforced form is: a marked task awaits no `ActorReply`, no `JoinHandle` of the node's `spawn` and no `signals.sync()`. It may wait only on leaf mutexes, I/O, and the feed and ack primitives whose other end is V's producer side or another marked task (SB5 slots, W's ack channel). Cycle A (an F1 pop task awaiting `OnV` while V drains its slot) and cycle B (a listener awaiting the net actor, which awaits `ProcessBlock`'s `Activated` reply while V waits in `limit_queue`) are both excluded. In Core, callbacks run without `cs_main` and must not drive ABC synchronously (`v:3738`).
- **LO-9 enforcement (every build).** The node runtime sets a **no-wait marker** on each such task: a task-local for async tasks, plus a thread-local on the callback thread, W's thread and V-spawned helper threads. The node's `spawn` propagates it into child tasks. `ActorReply<T>`, the only receiver type the node's actor handles return (`ChainHandle` included), and the `spawn` wrapper's `JoinHandle` check the marker on first poll or blocking wait. From a marked task they fail at once with `WaitError::WaitOnVFromNoWaitTask`. The command, if already sent, still runs; its reply is dropped, which is posting. A marked task that needs a reply forwards the `ActorReply` to an unmarked task. The marker is a **backstop, not the proof**: it cannot see a raw channel to an unmarked helper that awaits V. Node crates therefore ban raw `tokio::sync::oneshot`/`mpsc` receivers in marked code paths with clippy `disallowed_types` (allow-listed in the actor and feed implementations), and the proof is §9.6's lock-and-wait model.
- **LO-9 and registration** (rev3.1, DEC-157 (2)). SB5's release wake after `HoldActive` is a wait on V, because it fires only when V releases its hold. A marked task that registers or re-subscribes (a subscriber's own task re-subscribing after a gap, CH2b's driver under its hub lock, LK1b) therefore never parks on that wake alone. It returns to its loop, keeps serving its pops, acks and `FlushDue`, and selects on the wake beside them. An unmarked caller (ND1 at init, an RPC-driven index enable) may simply wait for the wake. **V itself never calls `register`, `subscribe_at` or `deregister`**: during its own hold it would get `HoldActive` for a hold only it can release.

**Why no cycle.** V is the only thread that waits while holding L1. Each wait target is W (whose freeze → release path takes only L6 and store locks, H1), a `BlockFiles` holder (W, doing only I/O), or a subscriber (whose pop and ack path takes no L1 and waits on nothing V does, H4/H6, **including V's and other actors' replies**, LO-9; a closing subscriber's obligations are retired, H7). A subscriber that is itself waiting for V (a `FlushDue` request, or W's ack that needs a not-yet-delivered event) is unblocked because every in-hold wait answers `FlushDue` and runs `retry_pending`; those are feed primitives that V's in-hold loop serves, not actor replies. At `limit_queue`, V waits on the `ValidationSignals` thread, which waits only on its own leaf mutex and on listener code bound by LO-5, LO-6 and LO-9, so it awaits no actor at all. None of them waits on V's guard, on L1 or on a reply, so every wait ends. L3 is taken exclusively only by V, and only inside L1 or with nothing held; a shared L3 holder on another thread waits on nothing V holds (LO-6), so V's exclusive acquire ends. §9.6's model checks this, including CH2's waits under the ABC hold.

## 3. CH1-node's index tables (`NodeIndex`)

*Implementation note (DEC-192, D22):* the code names this type `NodeBlockIndex`, so SB4's public `dash_chain::NodeIndex` keeps its name; this document keeps `NodeIndex`.

### 3.1 Fields

```rust
pub struct NodeIndex {
    n_chain_tx: Vec<u32>,           // Core nChainTx (unsigned int, chain.h:184); 0 = !HaveTxsDownloaded
    sequence_id: Vec<i32>,          // Core nSequenceId: 0 for loaded and header-only entries (bs:100)
    next_sequence_id: i32,          // nBlockSequenceId, starts at 1 (validation.h:996)
    reverse_sequence_id: i32,       // nBlockReverseSequenceId, starts at -1 (:998)
    last_precious_chainwork: Work,  // nLastPreciousChainwork, starts at 0 (:1000)
    candidates: BTreeSet<CandKey>,  // setBlockIndexCandidates
    cand_key: HashMap<BlockId, CandKey>,   // the ONE live key of each member of `candidates` (D2, §3.3)
    unlinked: HashMap<BlockId, Vec<BlockId>>,   // m_blocks_unlinked: prev → children, insertion order, duplicates kept
    best_invalid: Option<BlockId>,  // m_best_invalid
    last_notified_header: Option<BlockId>,   // NotifyHeaderTip's static pindexHeaderOld (v:3696)
    large_work_invalid_chain: bool, // SetfLargeWorkInvalidChainFound
}
#[derive(Ord, PartialOrd, Eq, PartialEq)]
pub struct CandKey { work: Work, seq: Reverse<i32>, id: Reverse<BlockId> }   // greatest = best
```

`CandKey` orders as Core's `CBlockIndexWorkComparator` (`bs:32-49`): more work is better; at equal work the smaller `nSequenceId` is better; then the lower address. SB4's D9 substitutes the arena id for the address. `candidates.last()` is Core's `rbegin()`, and "a < tip" is `key(a) < key(tip)`.

`n_chain_tx` and `sequence_id` are dense, indexed by `BlockId`, and grown with the arena (8 B per entry; 2.6 M entries ≈ 21 MiB, counted in `idle_rss_mib`). Neither is persisted: Core's `CDiskBlockIndex` stores neither.

### 3.3 One live key per candidate (D2, rev1)

Core keeps `CBlockIndex*` in a `std::set` ordered by the entry's *current* fields; CH1-node keeps immutable `CandKey`s. The adaptation is complete only with these rules, which every write path obeys:

- **Invariant C1.** `cand_key` and `candidates` are a bijection: `candidates == cand_key.values()` and every key's `id` is its map key. So each block has at most one key in the set.
- **Insert.** `candidates_insert(b)`: if `cand_key[b]` exists, remove that key from the set first; then build `key(b)` from the current work, `sequence_id[b]` and id, insert it, and record it. Inserting a present block therefore **replaces** its key; it never adds a second one.
- **Re-key.** Any write to `sequence_id[b]` goes through `set_sequence_id(b, s)`: if `b` is a member, remove its key, write `s`, re-insert with the new key; else just write. `nChainWork` never changes after acceptance, so the sequence id is the only field that can move a key.
- **Erase.** `candidates_erase(b)` removes `cand_key.remove(b)` from the set; erasing a non-member is a no-op, as Core's `erase(pindex)`.
- **Prune.** `prune_block_index_candidates` pops the lowest key while it is `< key(tip)` and removes its `cand_key` entry too.

Core's duplicate unlinked visits stay: `unlinked[P] = [C, C]` makes `received_block_transactions` dequeue C twice, assign `sequence_id[C] = 10` then `11`, and call `try_add_candidate(C)` twice (`v:4286-4289`). Under the rules above the set then holds exactly one key for C, with `seq 11`, which is the key Core's set would find after its in-place mutation. Sol's r1 failure (two keys, a ghost that `FindMostWorkChain` selects forever) cannot arise. Core's own behaviour in that case mutates a set member's ordering field in place, which leaves its tree mis-ordered; D2 declares our replacement semantics as the defined version of it.

### 3.2 Writes to SB4's shared entries

CH1-node writes the mutable group through `chain.modify(id, |g| …)` and dirties through `chain.mark_dirty(id)`, only at Core's sites:

| Core site | Write | Dirty |
|---|---|---|
| `ReceivedBlockTransactions` `v:4269-4276` | `n_tx`, `file`, `data_pos`, `undo_pos = 0`, `HAVE_DATA`, raise to `VALID_TRANSACTIONS` | yes |
| `AcceptBlock` check failure `v:4821-4824` | `FAILED_VALID` unless `BLOCK_MUTATED` | yes |
| `InvalidBlockFound` `v:2082-2085` | `FAILED_VALID` unless mutated | yes |
| `FindMostWorkChain` `v:3553-3559` | `FAILED_CHILD` or `CONFLICT_CHAINLOCK` on the walked path | yes |
| `InvalidateBlock` `v:3979-3988`, `v:4016-4017` | `FAILED_VALID`; the `FAILED_VALID → FAILED_CHILD` swap | yes |
| `PruneOneBlockFile` `bs:137-145` (CH2 step 7's `PruneClears`) | clear `HAVE_DATA`, `HAVE_UNDO`; `file = None`, `data_pos = undo_pos = 0` | yes |
| `ConnectBlock`'s undo write and `VALID_SCRIPTS` (CH3a, through `BlockStatusWriter`, §4.8) | `undo_pos`, `HAVE_UNDO`, raise to `VALID_SCRIPTS` | yes |

`MarkConflictingBlock`'s marks and `ResetBlockFailureFlags`' clears are SB4's, with SB4's dirtiness (D15). Core's `nFile = 0` after a prune is `file: None`; the record codec writes `nFile` only with `HAVE_DATA | HAVE_UNDO`, as `CDiskBlockIndex` does, so the bytes are equal.

## 4. Control flow, mapped to Core

Each listing is a port. `→` marks a seam call (§8). Every `assert` in Core is a `debug_assert!` unless it guards memory safety, in which case it is an `AbortNode`.

### 4.1 `activate_best_chain(pblock)` — `v:3724-3856`

```text
debug_assert!(running.is_none() || running is this ABC)          // m_chainstate_mutex :3738
most_work = None; new_tip = None
loop {                                                            // outer do-while :3752
    release_point()                                               // §2.4: Core's cs_main is free here
    → signals.limit_queue(&mut service)                           // LimitValidationInterfaceQueue :3760 → :3716 (no lock held; §8.2)
    hold = chain.hold_tip()                                       // LOCK(cs_main) :3763
    starting_tip = tip(); blocks_connected = false
    loop {                                                        // inner do-while :3768
        trace = ConnectTrace::new()                               // :3771
        most_work = most_work.or_else(|| find_most_work_chain())  // :3773-3775
        if most_work.is_none() || most_work == tip() { break }    // :3778-3780
        invalid_found = false
        abc_step(most_work, pblock if its hash == most_work's, &mut invalid_found, &mut trace)?   // :3784-3787; Err = fatal
        blocks_connected = true
        if invalid_found { most_work = None }                     // :3791-3794
        new_tip = tip()
        for (id, block) in trace { staged.push(BlockConnected(block, id)) }   // :3797-3800, staged (§2.5, N1)
        if !(tip().is_none() || starting_tip.is_some_and(|s| key(tip()) < key(s))) { break }
                                                                  // :3812 verbatim: while (!Tip() || (starting_tip && Comparator(Tip(), starting_tip))); m_disabled: D9
    }
    if !blocks_connected { release(hold); return Ok(true) }       // :3813, no CheckBlockIndex, no PERIODIC
    fork = find_fork(starting_tip); ibd = chain.is_ibd_private(now, loading)   // :3815-3816 (API CH1-A8)
    if fork != new_tip {                                          // :3819
        → signals.sync_updated_block_tip(new_tip, fork, ibd)      // vi:211, inline listeners (LO-5), facts as arguments
        staged.push(UpdatedBlockTip(new_tip, fork, ibd))          // vi:193, queued: spliced at release (N1)
        staged_ui.push(NotifyBlockTip(sync_state(ibd), new_tip))  // uiInterface: fired after publication (N1)
        cl_on_updated_block_tip(new_tip, fork, ibd)               // §5.2: h:185-207
    }
    release(hold)                                                 // end of the cs_main scope :3827: snapshot, ChainView, splice, TipWatch, UI (§2.5)
    if stop_at_height > 0 && height(new_tip) >= stop_at_height { request_shutdown() }   // :3830
    if shutdown.load() { break }                                  // :3841
    if new_tip == most_work { break }                             // while (pindexNewTip != pindexMostWork) :3842
}
check_block_index()                                               // :3844
→ flush.at_site(Periodic)?                                        // :3851
Ok(true)
```

`starting_tip` is never null after genesis. Before genesis (`Tip() == nullptr`), `!m_chain.Tip()` keeps the inner loop running until any tip exists, and then the null `starting_tip` ends it. V reproduces this through `private_tip() == None` in SB4's pre-genesis state (CH1-A2, CH1-A11). `tip()` in these listings is `private_tip()`.

### 4.2 `abc_step(most_work, pblock, invalid_found, trace)` — `v:3601-3684`

```text
old_tip = tip(); fork = find_fork(most_work)                      // :3606-3607 (m_chain.FindFork)
pool = → mempool.new_disconnect_pool()                            // MAX_DISCONNECTED_TX_POOL_SIZE * 1000 :3611
disconnected = false
while tip() != fork {                                             // :3612
    if !disconnect_tip(Some(&mut pool))? {
        → mempool.maybe_update_for_reorg(pool, false)             // :3616
        return abort("Failed to disconnect block; see debug.log for details")   // :3621
    }
    disconnected = true
}
height = fork.map_or(-1, height); cont = true                     // :3629-3630
while cont && height != height(most_work) {                       // :3631
    target = min(height + 32, height(most_work))                  // :3634
    to_connect = ancestors of most_work from target down to height + 1   // descending, :3636-3641
    height = target
    for c in to_connect.iter().rev() {                            // ascending :3645
        match connect_tip(c, pblock if c == most_work, trace, &mut pool)? {
            Connected => {
                prune_block_index_candidates()                    // :3664
                if old_tip.is_none() || work(tip()) > work(old_tip) { cont = false; break }   // :3665-3668
            }
            Invalid(state) => {
                if state.result != BLOCK_MUTATED { invalid_chain_found(to_connect[0]) }   // :3648-3650: the batch's TOP, Core's front()
                invalid_found = true; cont = false; break         // :3651-3654
            }
            SystemError => { → mempool.maybe_update_for_reorg(pool, false); return Err }  // :3656-3661
        }
    }
}
if disconnected { → mempool.maybe_update_for_reorg(pool, true) }  // :3675-3678
→ mempool.check(height(tip()) + 1)                                // :3679
check_fork_warning_conditions()                                   // :3681
```

`InvalidChainFound(vpindexToConnect.front())` passes the **top** of the 32-block batch, not the failed block. Ported as is; the `ch1-invalid-in-batch` scenario (§9.2) pins it.

### 4.3 `find_most_work_chain`, `invalid_chain_found`, `invalid_block_found`, fork warnings

`find_most_work_chain` — `v:3519-3581`:

```text
loop {
    new = candidates.last()?                                      // rbegin; None → return None :3524-3528
    test = Some(new); invalid_ancestor = false
    while let Some(t) = test.filter(|t| !active_contains(t)) {    // m_chain.Contains under the hold (API CH1-A2)
        debug_assert!(n_chain_tx[t] != 0 || height(t) == 0)       // :3540
        failed = status(t) & FAILED_MASK; conflicting = status(t) & CONFLICT; missing = !(status(t) & HAVE_DATA)
        if failed || missing || conflicting {
            if failed && best_invalid.map_or(true, |b| work(new) > work(b)) { best_invalid = Some(new) }   // :3549-3551
            f = new
            while f != t {
                if failed      { modify(f, |= FAILED_CHILD); mark_dirty(f) }   // :3553-3555
                else if conflicting { modify(f, |= CONFLICT); mark_dirty(f) }  // :3556-3558
                else if missing { unlinked.entry(prev(f)).push(f) }            // :3559-3561
                candidates_erase(f); f = prev(f)                                // :3562-3563
            }
            candidates_erase(t); invalid_ancestor = true; break   // :3565-3567
        }
        test = prev(t)
    }
    if !invalid_ancestor { return Some(new) }
}
```

`invalid_chain_found(n)` — `v:2034-2056`: `best_invalid = n` if none or more work; **`chain.best_header_invalid_chain(n)`** (`v:2043-2045`: if `n` is an ancestor-or-self of `best_header`, `best_header` becomes the active tip, i.e. V's private tip under a hold); the two log lines; `check_fork_warning_conditions()`.

`invalid_block_found(b, state)` — `v:2077-2089`: unless `BLOCK_MUTATED`: `FAILED_VALID`, `chain.failed_blocks().insert(b)`, dirty, `candidates_erase(b)`, `invalid_chain_found(b)`.

`conflicting_chain_found(n)` — `v:2058-2073`: two log lines, `check_fork_warning_conditions()`.

`check_fork_warning_conditions` — `v:2015-2031`: return if `is_ibd_private`; if `best_invalid` exists and its work exceeds the tip's work + 6 × `block_proof(tip)`, log Core's warning and set `large_work_invalid_chain`, else clear it. The flag is published in `ChainView` (`getblockchaininfo.warnings`, ND1's `-alertnotify` path).

### 4.4 `connect_tip(id, pblock, trace, pool)` — `v:3395-3513`

```text
debug_assert!(prev(id) == tip())                                  // :3400
block = pblock or → files.read_block(id)  (error → abort "Failed to read block")   // :3407-3418
bls = BlsCtx::for_block(id)                                       // ScopedBLSLegacyScheme :3401 (CH7a's type)
r = → connector.connect_block(&mut cx, &block, id, &mut BlockStatusWriter::new(chain, id), bls)   // :3427-3431, + :3446-3448 commit
→ signals.block_checked(&block, &state, is_ibd_private(now))    // :3432, vi:258 inline (LO-5); IBD as an argument (§2.5)
match r {
    Invalid(state) => { invalid_block_found(id, &state); return Invalid(state) }   // :3433-3438
    SystemError    => return SystemError
    Connected      => {}
}
pending = → flush.at_site(IfNeeded)?                              // :3457 decision and any CRITICAL wait (§7.1, D3)
→ mempool.remove_for_block(&block.vtx, height(id))                // :3467
→ mempool.remove_expired_asset_unlock(height(id))                 // :3468
pool.remove_for_block(&block.vtx)                                 // :3469
step = chain.connect_tip(id)?                                     // SetTip :3473: seq q+1, F1 event fanned out at the step
pending.freeze_after_step(chain.private_tip(), step.seq())?       // §7.1: new = id, tip_seq = q+1 (API CH1-A2)
drain_fanout()                                                    // while retry_pending() == Blocked { in_hold_service_wait() }
bls.commit()                                                      // :3474 (published in the snapshot's BlsCtx)
update_tip(id)                                                    // :3478, §4.11
trace.push(id, block)                                             // :3511
Connected
```

### 4.5 `disconnect_tip(pool)` — `v:3249-3342`

This is also V's `TipHost::disconnect_tip` (§5.4).

```text
d = tip(); debug_assert!(prev(d).is_some())                       // :3254-3256
block = → files.read_block(d)  (error → log, return Ok(false))    // :3259-3263
bls = BlsCtx::for_block_legacy_iff(!V19 active at d)              // :3265-3266
ok = → connector.disconnect_block(&mut cx, &block, d, bls)        // :3268-3287 (evo dbTx, view.Flush, Commit)
if !ok { log; return Ok(false) }
→ flush.move_prune_locks_back(height(d))                          // :3292-3301 prune-lock moveback (D61)
pending = → flush.at_site(IfNeeded)?                              // :3304 (§7.1, D3)
if let Some(pool) = pool {                                        // :3308-3314
    for tx in pool.add_from_block(&block.vtx) { → mempool.remove_recursive(tx, REORG) }
}
step = chain.disconnect_tip()?                                    // SetTip(prev) :3316: Disconnected { block: d, seq: q+1 }
pending.freeze_after_step(chain.private_tip(), step.seq())?       // §7.1: new = parent(d), NOT step.block() (= d); tip_seq = q+1
drain_fanout()                                                    // SB5 §3.3 per step, before any next step
bls.commit()                                                      // :3317
update_tip(prev(d))                                               // :3318
staged.push(BlockDisconnected(block, d))                          // :3320-3322, vi:241 queued, staged (N1)
Ok(true)
```

Core's prune-lock move-back (`v:3292-3301`) lowers each index's `height_first` to `h − 1`. V calls `FlushSite::move_prune_locks_back(h)` there (D61, N7b-2), and CH2b's adapter lowers each subscriber's in-memory Core-side lock through `SubscriberHub::on_disconnect_tip` and logs Core's `BCLog::PRUNE` line. What is pruned is unchanged by it: CH2's prune bound is computed at selection from `height(LCA(durable locator, new)) − PRUNE_LOCK_BUFFER − 1` (CH2 §4.2 step 7), which is already at or below `h − 1` for any lock on the disconnected branch.

### 4.6 `invalidate_block(id)` — `v:3889-4046`, and the RPC's ABC (`rpc:2015-2018`)

```text
if height(id) == 0 { return Ok(false) }                           // :3895
to_mark_failed = id; was_in_chain = false; disconnected = 0       // :3897-3899
by_work: BTreeMap<Work, Vec<BlockId>> = {                         // multimap :3916-3933, one scope
    for c in chain.ids() where !active_contains(c) && !(key(c) < key(prev(id)))
        && valid(c, VALID_TRANSACTIONS) && !conflict(c) && n_chain_tx[c] != 0 { push(work(c), c) } }
loop {
    if shutdown.load() { break }                                  // :3937
    release_point(); → signals.limit_queue(&mut service)          // :3940 → :3716 (cs_main free here)
    hold = chain.hold_tip()                                       // LOCK(cs_main) :3942
    if !active_contains(id) { release(hold); break }              // :3944
    was_in_chain = true; walk_tip = tip(); old_tip = tip()        // :3945-3948
    if id == chain.best_header() {                                // :3951 — hook 1 (DEC-112 O1)
        best_invalid = Some(chain.best_header()); chain.best_header_walk_back(id) }
    pool = → mempool.new_disconnect_pool()
    ret = disconnect_tip(Some(&mut pool))?                        // :3959
    disconnected += 1
    → mempool.maybe_update_for_reorg(pool, disconnected <= 10 && ret)   // :3965
    if !ret { release(hold); return Ok(false) }                   // :3966
    debug_assert!(prev(walk_tip) == tip())
    if old_tip == chain.best_header() {                           // :3969 — hook 2
        best_invalid = Some(old_tip); chain.best_header_walk_back(old_tip) }
    modify(walk_tip, |= FAILED_VALID); mark_dirty(walk_tip)       // :3979-3980
    candidates_erase(walk_tip); candidates_insert(prev(walk_tip)) // :3981-3982 (unconditional, as Core)
    if prev(walk_tip) == to_mark_failed && status(to_mark_failed) & FAILED_VALID {   // :3983-3988, verbatim
        modify(to_mark_failed, s = (s ^ FAILED_VALID) | FAILED_CHILD); mark_dirty(to_mark_failed) }
    for (w, c) in by_work.range(work(prev(walk_tip))..) {         // :3991-3999
        if !(key(c) < key(prev(walk_tip))) { candidates_insert(c); remove c from by_work } }
    to_mark_failed = walk_tip                                     // :4002
    release(hold)                                                 // :4003
}
check_block_index()                                               // :4006
hold = chain.hold_tip()                                           // :4008
if active_contains(to_mark_failed) { release(hold); return Ok(false) }   // :4010-4013
modify(to_mark_failed, |= FAILED_VALID); mark_dirty; candidates_erase; chain.failed_blocks().insert   // :4016-4019
for c in chain.ids() where valid(c, VALID_TRANSACTIONS) && !conflict(c) && n_chain_tx[c] != 0
    && !(key(c) < key(tip())) { candidates_insert(c) }            // :4028-4032
invalid_chain_found(to_mark_failed)                               // :4034 — hook 3
ibd = chain.is_ibd_private(now, loading)
→ signals.sync_updated_block_tip(tip(), None, ibd); staged.push(UpdatedBlockTip(tip(), None, ibd))   // :4035-4038
cl_on_updated_block_tip(tip(), None, ibd)
if was_in_chain { staged_ui.push(NotifyBlockTip(sync_state(ibd), prev(to_mark_failed))) }   // :4042-4044, Core: after the scope
release(hold)                                                     // the UI notification fires after publication
Ok(true)
```

The `InvalidateBlock` command then runs ABC if the state is valid (`rpc:2017-2019`). Genesis returns `false` with a valid state, so the RPC still runs ABC, as Core does.

The `:3983` condition compares the walk tip's parent with the previously disconnected block, which lies *above* it, so from the second iteration on it never holds. In the first iteration it holds only if the invalidated block is the tip's parent and already carries `FAILED_VALID`. In practice every disconnected block therefore keeps `FAILED_VALID`. The port keeps the condition verbatim rather than "fixing" it; `a18-invalidate-walkback` and `ch1-invalidate-equal-work` compare statuses exactly.

### 4.7 Reconsider and precious

**`ReconsiderBlock`** — `rpc:2058-2063` and `v:4152-4231`. The flag clearing is SB4's `reset_failure_flags(id, ignore_chainlocks) → ResetReport { reconsidered, promoted }`. CH1-node does the rest, in Core's order, inside the same class M command:

```text
report = chain.reset_failure_flags(id, ignore_chainlocks)
for r in report.reconsidered { if best_invalid == Some(r) { best_invalid = None } }   // :4186-4189, :4205-4208
for p in report.promoted { candidates_erase(p) }                  // :4215
for r in report.reconsidered {                                    // :4224-4230
    if valid(r, VALID_TRANSACTIONS) && n_chain_tx[r] != 0 { try_add_candidate(r) } }
scope_end()                                                       // §4.12
// class C tail: activate_best_chain(None)                        // rpc:2063
```

`reset_failure_flags_null()` is Core's `ResetBlockFailureFlags(nullptr)` (`v:4155-4163`): if `best_invalid` exists and `ancestor(best_invalid, height(tip)) == tip`, log Core's line and proceed with `best_invalid`; otherwise return. CH5 calls it after VerifyDB at level ≥ 3 (`cs:463-465`).

The same post-processing runs on `EnforceReport.reset` after SB4's `enforce_block` resets the locked block (`v:4075-4077`, SB4 D18; API CH1-A7).

**`PreciousBlock`** — `v:3858-3887`:

```text
// class M scope :3863
if work(id) < work(tip()) { reply Ok; return }                    // :3864-3867: no ABC
if work(tip()) > last_precious_chainwork { reverse_sequence_id = -1 }   // :3868-3871
last_precious_chainwork = work(tip())
candidates_erase(id)                                              // erase before the key changes (§3.3)
set_sequence_id(id, reverse_sequence_id)                          // :3874; id is no longer a member, so no re-insert here
if reverse_sequence_id > i32::MIN { reverse_sequence_id -= 1 }    // :3875-3879
if valid(id, VALID_TRANSACTIONS) && !conflict(id) && n_chain_tx[id] != 0 {
    candidates_insert(id); prune_block_index_candidates() }       // :3880-3883
// class C tail: activate_best_chain(None)                        // :3886
```

### 4.8 Candidates and the status writer

- `try_add_candidate(b)` — `v:4233-4263`, active chainstate only: skip if `CONFLICT`; skip if a tip exists and `key(b) < key(tip())`; `candidates_insert(b)`.
- `prune_block_index_candidates()` — `v:3584-3593`: pop the lowest key, and its `cand_key` entry, while it is `< key(tip())`; `debug_assert!(!candidates.is_empty())`.
- `candidates_insert`, `candidates_erase` and `set_sequence_id` follow §3.3's replacement rules; `debug_assertions` builds check C1 after every command.
- **`BlockStatusWriter<'a>`** wraps `&mut HeaderChain` for one id and exposes `set_undo(file, pos)`, `raise_validity(level) -> bool` and `n_tx()`. Each write dirties, as Core's `ConnectBlock` does. CH3a receives it; it cannot reach other entries or the tip.

### 4.9 Headers and blocks

**`ProcessHeaders`** — `v:4729-4756`:

```text
res = chain.accept(batch, from)          // SB4: AcceptBlockHeader per header in one scope; publishes before returning (R3)
apply_cl_late_header(res.floor_moved)    // §5.3: cl:156-171, also for a partial batch (CH1-A12)
check_block_index()                      // once per batch, D5 (Core: after each header, :4737)
scope_end()                              // §4.12: on success AND on an invalid header, before the reply
if res.invalid.is_none() {               // a failed batch returns at :4739-4741, before NotifyHeaderTip
    notified = notify_header_tip()       // :4747, §4.11
    if notified && is_ibd(now) && res.last_accepted.is_some() { log "Synchronizing blockheaders, height: %d (~%.2f%%)" }   // :4748-4753
}
reply(HeadersResult { last_accepted, invalid, snap })
```

The accepted prefix of a failed batch is published (SB4 R3), but `last_notified_header` does not move, so the next successful operation emits the `NotifyHeaderTip` Core would emit then.

**`ProcessBlock`** — `v:4867-4907`:

```text
// class M scope :4878
ok = → checks.check_block(&block, &mut state, pow = true, merkle = true)   // :4884 (CS3)
let mut id = None; let mut new_block = false
if ok { ok = accept_block(&block, &mut state, &mut id, force, None, &mut new_block, Some(hash))? }   // :4887
if !ok { → signals.block_checked(&block, &state, is_ibd_private(now)); log "AcceptBlock FAILED"   // :4889-4893, inline
         scope_end(); reply; return }   // R1-M4: accept_header may have moved best_header or the floor; no NotifyHeaderTip (Core returns first)
scope_end()                              // the cs_main scope ends
notify_header_tip()                      // :4896
// class C tail: activate_best_chain(Some(block)); log "ACCEPTED"; reply      // :4899-4905
```

**`accept_block(block, state, id, requested, dbp, new_block, known_hash)`** — `v:4759-4865`:

```text
debug_assert!(known_hash.map_or(true, |h| h == block.hash()))     // :4774
accepted = chain.accept_header(&block.header, &hash)              // AcceptBlockHeader :4777 (SB4; does not publish)
apply_cl_late_header(accepted.floor_moved)                        // §5.3 (CH1-A12), before any return
check_block_index()                                               // :4778
if !accepted { return false }
already = status(id) & HAVE_DATA                                  // :4787
more_or_same = tip().map_or(true, |t| work(id) >= work(t))        // :4788: true with a null tip (OH-n1)
too_far = height(id) > tip_height() + MIN_BLOCKS_TO_KEEP          // :4794; ActiveHeight() is -1 with a null tip
if already { return true }                                        // :4804
if !requested {                                                   // :4805-4815
    if n_tx(id) != 0 || !more_or_same || too_far || work(id) < min_chain_work { return true } }
if !→ checks.check_block(.., true, true, Some(hash)) || !→ checks.contextual_check_block(.., prev(id)) {   // :4819-4820
    if state.is_invalid() && state.result != BLOCK_MUTATED { modify(id, |= FAILED_VALID); mark_dirty(id) }   // :4821-4824
    log; return false }
if !is_ibd_private(now) && tip() == prev(id) { → signals.new_pow_valid_block(id, block) }   // :4830-4831, vi:264 inline
new_block = true
pos = → files.save_block(&block, height(id), dbp)                 // SaveBlockToDisk :4837
    (null → state.error("…Failed to find position to write new block to disk"); I/O error → abort "System error: …")
received_block_transactions(&block, id, pos)                      // :4842
→ flush.at_site(None)?                                            // :4855 (only pruning can happen)
check_block_index()                                               // :4858
true
```

**`received_block_transactions(block, id, pos)`** — `v:4266-4305`:

```text
modify(id, |g| { g.n_tx = block.vtx.len(); g.file = Some(pos.file); g.data_pos = pos.pos; g.undo_pos = 0;
                 g.status |= HAVE_DATA; g.status.raise_validity(VALID_TRANSACTIONS) })   // :4269-4275
n_chain_tx[id] = 0; mark_dirty(id)                                                       // :4270, :4276
if prev(id).map_or(true, |p| n_chain_tx[p] != 0) {                                       // :4278
    queue = VecDeque::from([id])
    while let Some(b) = queue.pop_front() {
        n_chain_tx[b] = prev(b).map_or(0, |p| n_chain_tx[p]) + n_tx(b)                   // :4285
        set_sequence_id(b, next_sequence_id)                                              // :4286; re-keys a member (§3.3)
        next_sequence_id = next_sequence_id.checked_add(1).ok_or(abort("nSequenceId overflow"))?   // Core's int32 overflow is UB (OH-n3)
        try_add_candidate(b)                                                              // :4287-4289
        for c in unlinked.remove(&b).unwrap_or_default() { queue.push_back(c) }          // :4290-4297, insertion order
    }
} else if let Some(p) = prev(id).filter(|p| valid(p, VALID_TREE)) {                     // :4301
    unlinked.entry(p).or_default().push(id)                                              // :4302
}
```

`unlinked` keeps duplicates and insertion order, as `std::multimap` does (`equal_range` iterates in insertion order). Duplicates are reachable in pruned mode, where `FindMostWorkChain` can re-insert a pair (`v:3559-3561`); the queue then visits the child twice, as Core does, assigning two sequence ids, and §3.3 leaves exactly one candidate key, the second.

### 4.10 `check_block_index` — `v:5607-5855`

It runs where Core calls `CheckBlockIndex()`: after each `ProcessHeaders` batch (D5), in `accept_block` (`v:4773`, `v:4858`), at the end of ABC (`v:3844`) and of the `InvalidateBlock` loop (`v:4006`). It is enabled by `-checkblockindex`, default on for regtest (`v:6768`, `DefaultConsistencyChecks`).

One DFS, through SB4's visitor (API CH1-A9): SB4 asserts the header half (heights, chain work, skip, tree validity, `FAILED`/`CONFLICT` ancestry, `v:5724-5737`); CH1-node adds Core's block-data half with the same `pindexFirst*` state:
- `nSequenceId <= 0` unless `HaveTxsDownloaded` (`v:5691`);
- `HAVE_DATA ⇔ nTx > 0` and `pindexFirstMissing == pindexFirstNeverProcessed` when nothing was pruned (`v:5696-5705`); `HAVE_DATA ⇒ nTx > 0` (`v:5708`); `HAVE_UNDO ⇒ HAVE_DATA` (`v:5710`); `VALID_TRANSACTIONS ⇔ nTx > 0` (`v:5719`);
- `HaveTxsDownloaded ⇔` no never-processed and no non-`TRANSACTIONS` ancestor (`v:5722-5723`);
- the candidate membership rules (`v:5741-5763`);
- the `m_blocks_unlinked` rules (`v:5768-5803`).

A failure is an `AbortNode` with the failed rule's line, as Core's `assert` terminates the process.

### 4.11 `update_tip`, `notify_header_tip`

`update_tip(new)` — `v:3190-3237`:
- `→ mempool.add_transactions_updated(1)`;
- `TipWatch` (Core's `g_best_block` + `g_best_block_cv`, `v:3212-3216`; it serves `getblocktemplate` long-poll) records `hash(new)`. Inside a hold the value is set and its waiters notified only at the release, after publication (N1); setting it once with the final tip is one of Core's schedules, because a long-poll waiter then takes `cs_main`. The `waitfornewblock`/`waitforblock`/`waitforblockheight` RPCs do **not** use it: they wait on `latestblock`, which `uiInterface.NotifyBlockTip` → `RPCNotifyBlockChange` updates (`rpc:300-305`, `rpc:337-434`). Both paths fire after publication (rev3, OH-M1);
- if not `is_ibd_private`, the unknown-versionbits warning check via the deployments seam (`v:3215-3229`);
- the `UpdateTip:` log line (`UpdateTipLog`), with CH2's cache usage and CH7a's evodb usage from `TipLogFacts`.

`notify_header_tip()` — `v:3693-3714`: if `best_header != last_notified_header`, record it, then call `signals.notify_header_tip(best_header, is_ibd)` (a synchronous signal, `vi:274`) and ND1's UI notification, and return true.

### 4.12 `scope_end` (rev1, R1-M4)

Every class M scope and every hold release ends with `scope_end()`: `chain.publish_status()` (a no-op inside a hold, CH1-A13), reconcile the ChainLock mirror (§5.3), then `publish_view()`. It runs on **every non-fatal exit**, success or failure, **before the reply**. Snapshot publication and Core's notifications are separate obligations: `scope_end` never emits `NotifyHeaderTip`, `BlockChecked` or any other signal, so Core's omissions on failure paths stay omissions. Signals a scope does emit come **after** its `scope_end` when they are queued or UI notifications (N1), and inline when they are synchronous (with V-evaluated arguments, §2.5). A fatal exit aborts the node, which makes publication moot.

## 5. ChainLocks on V

### 5.1 Intake and recording (class M)

Core's `ProcessNewChainLock` (`h:133-183`) runs on `msghand`, in this order: `CheckActiveState()` (`h:136`); the seen check and insert (`h:138-143`); the height precheck against `GetBestChainLockHeight()` (`h:147-151`); `SelectQuorumForSigningStartBlock` under `cs_main` (`h:154-155`); signature verification, with `Misbehaving(10)` for a peer (`h:156-164`); the block lookup (`h:166`); `UpdateBestChainlock` (`h:168`); the 0 s enforcement task if the block is known (`h:169-176`); the inv for relay (`h:181-182`). Its enforcement is a scheduler task. V splits it at the same seams. This needs SB4 to separate `UpdateBestChainlock` from `apply_chainlock` (API CH1-A5); SPV keeps the fused call.

**LK1a's sequence** (normative for LK1a; the coinbase path `h:104-131` enters at step 1 with `from = None`):

1. `ClIntake` → V. V runs Core's `CheckActiveState` **before anything can return early**, reconciles a reset (§5.3), runs `scope_end()`, and replies `best_height`. Every CLSIG goes through this step, including duplicates, stale ones and ones whose signature will fail (R1-M3), so a completed rejection is never observable before Core's reset.
2. LK1a: the seen check and insert. A duplicate returns here.
3. LK1a: the precheck `sig.height <= best_height` returns here.
4. Quorum selection and verification, through the EV addendum's dispatched route (route B: `select_quorum_for_signing`, `GetQuorum`, which runs whole on V as `OnV`); signature arithmetic may run off V. A peer's invalid signature returns `Misbehaving(10)` to P2a.
5. `RecordChainLock` → V, below.

Steps 1, 4 and 5 are separate V commands, so other commands can run between them: a `ClEnforce` (the 5 s timer's `CheckActiveState`), another CLSIG, a block. Each such interleaving is a Core schedule: the scheduler thread runs `CheckActiveState` and enforcement concurrently with `msghand`, and no Core lock is held across `h:136`–`h:168`. Two CLSIGs can pass the precheck concurrently, and the second is then `Stale` on V; Core's comment at `h:144-145` describes the same race. The price is one extra V round trip per CLSIG (class M, so at most one hold of waiting, as Core's `msghand` waits for `cs_main` at `h:154`). rev0's off-V `precheck_height` prediction is withdrawn: it predicted the height but did not perform the reset.

```text
ClIntake:
    cl_active_state()                                 // h:136 → h:209-222, §5.3
    scope_end()
    reply(ClIntakeReply { best_height: cl.sigs.best.map_or(-1, |s| s.height) })   // GetBestChainLockHeight

RecordChainLock { sig, sig_hash, from }:              // after LK1a's steps 2–4
    rec = chain.record_chainlock(ChainLockRef { height, hash })   // cl:106-132 UpdateBestChainlock + the lookup at h:166 (CH1-A5)
    match rec {                                       // SB4's shipped ClOutcome; Floor's `enforced` is always None here
        Stale          => relay = false               // cl:110-113: nothing changes
        HeightMismatch => { cl.sigs.best = sig; cl.sigs.best_hash = sig_hash; relay = false }   // cl:115-125: best updated, not relayed
        Pending        => { cl.sigs.best = sig; cl.sigs.best_hash = sig_hash; relay = true }    // cl:129-131
        Floor { id, enforced: None } => { cl.sigs.best = sig; cl.sigs.best_hash = sig_hash; cl.sigs.best_known = sig;
                            relay = true; deferred.push_back(ClEnforce { source: Recorded }) }   // h:169-176, queued (OH-m3)
    }
    scope_end()                                       // ClSigs and the floor visible before the reply
    reply(ClReply { relay })                          // LK1a returns the inv to P2a for relay (h:181-182)
    // the queued ClEnforce runs only after this reply: next, when V is idle (deferred drains before rx),
    // or after the running composite. It never runs inside RecordChainLock (§0 item 6, §2.4)
```

### 5.2 Enforcement (class C)

`ClEnforce` runs Core's scheduler tasks: the 5 s timer (`h:63-72`), the post-record task (`h:170-176`) and the `UpdatedBlockTip` task (`h:196-205`). Core's `EnforceBestChainLock` captures the pair `(clsig, currentBestChainLockBlockIndex)` **once** (`h:283`) and uses that pair for the enforcement, the activation check, the dedupe and the notification (`h:296-320`). Note that Core reads `ActiveTip()->GetAncestor` and V reads its private tip; outside a hold they are the same. V does the same (R1-M2):

```text
ClEnforce { source }:
    cl_active_state()                                 // CheckActiveState (h:66, h:173, h:200)
    if !chain.cl_enabled_observed() { finish(source); return }          // h:275-277
    let (floor, sig) = match (chain.cl_floor(), cl.sigs.best_known.clone()) {   // h:283 GetBestChainlockWithPindex (M1 makes them a pair)
        (Some(f), Some(s)) => (f, s),
        _ => { finish(source); return }                                 // h:284-287
    };
    report = chain.enforce_best_chainlock(&mut VHost)?.expect("observed on, floor set")   // h:296 EnforceBlock (CH1-A7)
    debug_assert_eq!(report.floor, floor)              // nothing ran between the capture and EnforceBlock
    apply_reset(report.reset); scope_end()             // §4.7, API CH1-A7
    if ancestor(private_tip(), height(floor)) != Some(floor) {            // h:299-300
        if !activate_best_chain(None)? { log "ActivateBestChain failed"; finish(source); return }   // h:302-305
        if ancestor(private_tip(), height(floor)) != Some(floor) { finish(source); return }          // h:307-309
    }
    if cl.last_notify == Some(floor) { finish(source); return }       // h:314
    cl.last_notify = Some(floor)                                       // h:315
    → signals.enqueue(NotifyChainLock(floor, sig))    // h:318, vi:287 queued, after scope_end (N1); the CAPTURED sig, never the current mirror
    → ui.notify_chain_lock(sig.block_hash, sig.height) // h:320 (+ ND1's -chainlocknotify)
    finish(source)                                     // TipScheduled: cl.tip_scheduled = false (h:203)
```

The ABC inside enforcement services class M commands at its release points, so a `RecordChainLock` for a higher lock, or a `ClIntake` that resets the state, can run before the notification. Core allows both schedules (the record runs on `msghand` while the scheduler thread is inside ABC), and Core still notifies with the captured pair; so does V. V does not add a stronger check that the captured lock is still the best one. A reset during the activation clears `last_notify`, after which the running enforcement notifies, as Core's would.

**Triggers.**
- `Timer`: ND1's scheduler posts `ClEnforce { Timer }` every 5 s. It is the scheduler that `mockscheduler` forwards, as Core's `CScheduler` is (`setmocktime` does not move it). LK1a runs the timer's `Cleanup()` on its own side (D6).
- `Recorded`: from §5.1, when the block is known.
- `TipScheduled`: `cl_on_updated_block_tip(new, fork, ibd)` runs wherever V emits `UpdatedBlockTip` (§4.1, §4.6, §5.4). If SB4's `updated_block_tip_schedules(new, fork, ibd)` holds (`h:187-190`) and `cl.tip_scheduled` is false, it sets the flag and queues `ClEnforce { TipScheduled }`. The flag is Core's `tryLockChainTipScheduled`.

### 5.3 Keeping the mirror equal to Core's three fields

V is the only writer of both SB4's ChainLock state and the `ClSigs` mirror, and it reconciles the mirror **before any `scope_end`, reply or snapshot wrap** at every SB4 call that can move them:

| SB4 call | Core | Mirror update |
|---|---|---|
| `check_cl_active_state()`, wrapped as `cl_active_state()` | `h:209-222`: on off→on, `ResetChainlock` and `lastNotify = null` | on `ClActiveState::TurnedOnReset` (or `cl_reset_gen()` moved, CH1-A6): `best`, `best_known` = `None`, `best_hash = Uint256::ZERO` (Core's `uint256{}`, `cl:98-104`), `last_notify = None`, `enabled_observed` refreshed |
| `record_chainlock` | `cl:106-132` | §5.1's table |
| `accept` / `accept_header` → `floor_moved: Some(id)` (shipped on `HeadersResult`; CH1-A12 adds it to `accept_header` and per-header semantics) | `cl:152-172` `AcceptedBlockHeader`: hash equals `bestChainLock`'s and the heights match | `best_known = best.clone()`; `debug_assert!(best.block_hash == hash(id))`. Reported per header, so a partial batch that accepts the locked header before failing still applies it |
| same, height mismatch | `cl:161-166`: log, return; the old known pair stays | nothing: SB4 reports `floor_moved: None` and keeps its floor; the mirror keeps the old `best_known` |
| `enforce_best_chainlock`, `mark_conflicting_block` | do not change the three fields | nothing |

`apply_cl_late_header(x)` is the third row. **Invariant M1**, checked after every command in `debug_assertions` builds and in every V test: `chain.cl_state().best == best.map(|s| (s.height, s.block_hash))`, `cl_floor.is_some() == best_known.is_some()`, and `cl_floor.map(hash) == best_known.map(|s| s.block_hash)`.

### 5.4 `TipHost` on V

| Method | V's implementation |
|---|---|
| `tip(chain)` | `chain.private_tip()` (API CH1-A2): Core's `m_chain.Tip()` under the lock |
| `contains(chain, id)` | `chain.active_contains_private(id)` (CH1-A2) |
| `disconnect_tip(chain)` | §4.5 with the mark's pool: V opens `pool` at the first disconnect of a `mark_conflicting_block` (`v:4090-4091`) and records each disconnected id in `marked_walk`. The IF_NEEDED split, the freeze labelling of §7.1 and the per-step `drain_fanout` are the same as in V's own holds; inside `enforce_block` SB5's `retry_pending` uses the held composite guard (SB5 §3.3) |
| `conflicting_marked(chain, marked, was_in_chain)` | Core `v:4116-4148` in order: erase `marked_walk` and `marked` from the candidates (`v:4116`, `v:4122`); `maybe_update_for_reorg(pool, true)` (`v:4125`); the full re-add scan (`v:4129-4137`); `conflicting_chain_found(marked)`; `signals.sync_updated_block_tip(tip, None, ibd)`, `staged.push(UpdatedBlockTip(tip, None, ibd))` and `cl_on_updated_block_tip`; `staged_ui.push(NotifyBlockTip(prev(marked)))` if `was_in_chain`. The staged entries are spliced and fired after `enforce_best_chainlock` returns and V publishes (N1) |

A `disconnect_tip` failure inside a mark makes Core `assert(false)` (`v:4067`); V aborts the node with that line.

### 5.5 What V keeps

```rust
pub struct ClSigs {                                // published in ChainView (§6)
    pub best: Option<Arc<ChainLockSig>>,           // bestChainLock         (cl:37-41 GetBestChainLock); None = Core's null sig
    pub best_hash: Uint256,                        // bestChainLockHash: Uint256::ZERO when unset or reset (cl:98-104, rev3 OH-m2)
    pub best_known: Option<Arc<ChainLockSig>>,     // bestChainLockWithKnownBlock
    pub enabled_observed: bool,                    // chain.cl_enabled_observed() at publication
}
impl ClSigs {
    pub fn best_height(&self) -> i32 { self.best.as_ref().map_or(-1, |s| s.height) }   // GetBestChainLockHeight
    /// GetChainLockByHash (cl:140-150): `bestChainLock` iff `bestChainLockHash == h`. With no lock
    /// recorded that is the null sig (height -1, chainlock.h:25) for h = 0, as Core answers
    /// getdata(MSG_CLSIG, 0) with a `clsig` (np:3041-3046).
    pub fn by_hash(&self, h: &Uint256) -> Option<Arc<ChainLockSig>> {
        (self.best_hash == *h).then(|| self.best.clone().unwrap_or_else(ChainLockSig::null_arc)) }
}
pub(crate) struct ClState { sigs: Arc<ClSigs>, last_notify: Option<BlockId>, tip_scheduled: bool }
```

`bestChainLockBlockIndex` is SB4's `cl_floor`; §5.3's invariant M1 ties the four together. Nothing is persisted: Core keeps ChainLock state in memory only and starts empty after a restart.

### 5.6 InstantSend conflicts

`NetInstantSend::ResolveBlockConflicts` (`is:600-646`) invalidates the conflicting blocks, then either resets each (`ResetBlockFailureFlags`, without the lock token) or runs ABC. The IS owner sends `IsConflictInvalidate { blocks, activate }`. V runs, for each block, `invalidate_block` (aborting on `false`, as `is:621-625` asserts) and, unless `activate`, the node half of `reconsider` with `ignore_chainlocks = false`; then ABC if `activate`. Serializing the per-block scopes in one class C command is one of Core's schedules.

## 6. `ChainView`

### 6.1 Contents

```rust
pub struct ChainView {
    pub snap: Arc<ChainReadSnapshot>,   // SB4: TipFacts (tip, best_header, work, time), tip_seq, active, bls, cl_floor,
                                        // cl_enabled_observed, epoch — R1–R8
    pub mn_list: Arc<MnList>,           // at snap.tip (CH7a sets it after each connect/disconnect; empty before CH7a)
    pub cl: Arc<ClSigs>,                // §5.5; `best_chainlock()` = cl.best
    pub mtp: i64,                       // GetMedianTimePast(snap.tip)
    pub tip_chain_tx: u32,              // nChainTx(tip): GuessVerificationProgress, getblockchaininfo
    pub large_work_invalid_chain: bool, // §4.3
    pub view_seq: u64,                  // bumps on every ChainView store
}
impl ChainView {
    pub fn bls(&self) -> &BlsCtx { &self.snap.bls }
    pub fn epoch(&self) -> u64 { self.snap.epoch }
    pub fn best_chainlock(&self) -> Option<&Arc<ChainLockSig>> { self.cl.best.as_ref() }
    /// Core IsInitialBlockDownload() against this view's snapshot and the shared latch (R4).
    pub fn ibd(&self, reader: &ChainReader, loading: &LoadingFlags, now: NodeSeconds) -> bool;
}
```

`ibd` is a method, not a field (P2a R4). A cached boolean would stay stale across `setmocktime`, which flips Core's answer on the next call. This is an A1 erratum (E-A1-2).

### 6.2 Publication

- V stores a `ChainView` **after every SB4 snapshot store** (`accept`, `release_tip`, `publish_status`, `init_active_tip`, `enforce_block`'s return) and after every `ClSigs` change, before it handles anything else.
- It is **not throttled**. SB4 publishes per step anyway, and the wrapper costs one `Arc` allocation and one `ArcSwap` store per step, against a block connect measured in milliseconds. A1's 100 ms IBD throttle is withdrawn (E-A1-3). PF0's `chainstate-view-publish` line keeps it honest; if the line ever shows more than 1% of the null-connector per-block cost, the throttle returns, for IBD only, and Q5 is reopened.
- A reader that needs node fields coherent with a tip reads them from one `ChainView`, whose `snap` is that tip. `ChainAccess::snapshot()` reads SB4's `TipCell` directly and may be one store ahead of `ChainView`; `snap.epoch` tells them apart.
- **Notifications come after both stores** (N1, §2.5): a listener or notified RPC that reads either handle sees the announced tip.
- **F1-derived state is fenced** (rev3, OH-M1). F1 fanout stays per step inside holds (DEC-153), so an F1 subscriber can learn of tip X before any snapshot contains X. Externally visible state derived from F1 events (an index's synced height and `getindexinfo`, an IS or ChainLock-signing state, CH2's reported flush height) must not be served ahead of `getblockcount`. The owner (RP0 for RPC, P2b for P2P, IX/LK1b and the index rows for their state) either serves it behind a `tip_seq` fence (answer only from events with `seq <= published.tip_seq`, so it never runs ahead of the snapshot) or through `Query`. Errata in §12.

### 6.3 `ChainViewHandle`: the read-side implementations

```rust
pub struct ChainViewHandle { view: Arc<ArcSwap<ChainView>>, tip: Arc<TipCell<..>>, reader: ChainReader,
                             loading: Arc<LoadingFlags>, tx: Sender<ChainCommand> }
```

- **`ChainAccess`** (P2a §2.4): `snapshot` → `tip.snapshot()`; `lookup`, `entry`, `ancestor`, `locator`, `min_chain_work` → `ChainReader`; `loading_blocks` → `loading`; `is_initial_block_download(snap, now)` → `reader.is_initial_block_download(snap, now, loading.any())`; `process_headers` → `ChainCommand::ProcessHeaders`, blocking on the reply.
- **`ConnChainView`** (P2a): `mn_list_at_tip` → `view.mn_list`; `ibd_service_cache` → the latch as above.
- **`ClReader`** (P2b's getdata and inv, RPC, miner): `best`, `best_height`, `by_hash(h)` (Core `GetChainLockByHash`: `best` iff `best_hash == h`, so `by_hash(0)` is the null sig while no lock is recorded, §5.5). LK1a's precheck uses `ClIntake`'s reply, never the published view (§5.1).
- `getchaintips`, confirmations and other cross-entry reads go through `Query` (A1 §2.10.2).

## 7. Persistence, crash and recovery (with CH2b)

### 7.1 Flush sites

| Core site | Mode | Here |
|---|---|---|
| `ConnectTip` `v:3457` | IF_NEEDED | §4.4, after the connector returns, before `chain.connect_tip` |
| `DisconnectTip` `v:3304` | IF_NEEDED | §4.5, likewise before `chain.disconnect_tip` |
| ABC end `v:3851` | PERIODIC | §4.1 |
| `AcceptBlock` `v:4855` | NONE | §4.9 |
| ATMP `v:1748`, `v:1780` | PERIODIC | MP1, through `Mempool` |
| `Flush { mode }` | as given | CH2 §3.3 |

**The freeze follows the step (D3, Q3).** At both IF_NEEDED sites Core's coins view is already at the new state (`v:3285` for a disconnect, `v:3446` for a connect) while `m_chain` still points at the old tip, because `SetTip` comes after (`v:3316`, `v:3473`). Core's full flush writes the coins cache's own `hashBlock` (`v:3092`, `coins.cpp:254-255`): `prev(d)` after disconnecting `d`, `id` after connecting `id`. CH2 and SB0 E4 define `FreezePacket.{new, tip_seq}` as V's private tip and the `seq` of the event that made it the tip (SB4 §9.2, D45). At Core's site those two disagree with the coins. CH1-node therefore splits the site:

- `flush.at_site(IfNeeded)` runs CH2's decision at Core's site, including any CACHE_CRITICAL/EVO_CRITICAL wait for the in-flight F's release (DEC-144: at every disconnect, under the hold). It returns a `PendingFreeze`. It never freezes.
- `pending.freeze_after_step(chain.private_tip(), step.seq())` runs right after `chain.connect_tip`/`disconnect_tip`, under the same guard. **`new` is V's private tip after the step and `tip_seq` that step's `seq`** (R1-B1). For a connect of `id` at `q+1` that is `(id, q+1)`. For a disconnect of `d` at `q+1` it is `(parent(d), q+1)`: the event is `Disconnected { block: d, seq: q+1 }`, and `step.block()` names the **removed** block, so it is never used as `new` (SB4 C0 `tip.rs:47-63`). The `seq` is the disconnect's, because that event is the one that made `parent(d)` the tip; a synced subscriber has applied it exactly when its own best block is `parent(d)`, which is what CH2's case A/B test (rule 1) needs.
- **Erratum DEC-166 (2026-10-10), overrides the rest of this bullet and §8.2, §9.4 (e), E-CH2-1, D3 and OH-m1 where they say `freeze_after_step` stages or enqueues the signal:** `ChainStateFlushed` is handed to ValidationSignals only when V observes W's release of that flush (`FlushDone`, after B1 is Durable), still with the locator captured at `at_site`. It is no longer staged at `freeze_after_step`. Core flushes before it signals, and the wallet locator must never lead durable state. The signal can therefore reach the queue after later `BlockConnected` events, so the locator lags; this is CH2 D16 ('lags, never leads'). At a disconnect site the locator still names `d`, as Core's does (`v:3303` flush, `v:3316` `SetTip`). Inside a hold, a release observed under it is staged with the hold's callbacks (N1). Test §9.4 (e) becomes: the signal is handed after that flush's B1 returned, and names `new`, an ancestor of it, or (at a disconnect site) `d`.
- **`ChainStateFlushed` names the pre-step tip** (rev3, OH-m1). Core enqueues it at the site, before `SetTip`, with `m_chain.GetLocator()` (`v:3114`): the old tip (`d` on a disconnect, `prev(id)` on a connect), while the coins are already at the new state. `at_site` therefore captures `locator(private_tip())` into the `PendingFreeze`, and `freeze_after_step` stages `ChainStateFlushed(locator)` with that captured locator, not the post-step one. That keeps Core's FIFO position: before the step's `BlockDisconnected` (§4.5 stages it after the step) and before the batch's `BlockConnected`. The wallet's `WriteBestBlock` locator is then Core's. When CH2 completes the flush asynchronously, CH2b still enqueues with the captured locator. Outside the two IF_NEEDED sites the pre- and post-step tips are equal.
- **Check (every build).** Before handing the packet to W, V checks `new.hash == coins.best_block() == evo.best_block()` (the evo half when CH7a's delta tracks a best block, which `meta.evo_tip == new` in B1 requires) and that `new == chain.private_tip()`. A mismatch is an `AbortNode`: writing a mislabelled packet would corrupt `meta.tip`/`head_blocks`.

**Every freeze cause respects the pairing.** Between a connector's commit and its tip step the coins are at the new state while the tip is not. In that window (the CRITICAL wait inside `at_site` is the only wait in it) V does not freeze for any reason. A subscriber's `FlushDue` serviced there is **latched** into the `PendingFreeze`, and `freeze_after_step` executes the decided freeze and any latched cause as one freeze with the post-step pair. Outside the window every freeze, whatever its cause (IF_NEEDED, PERIODIC, `FlushDue`, `Flush`), captures `(private_tip, last assigned seq)`, which then matches the coins. `PruneClears` is index-only and is answered anywhere.

Between the two calls V only updates the mempool (`v:3308-3314`, `v:3467-3471`) and takes the tip step, neither of which touches the frozen coins, evo delta or index records, so the packet's payload is byte-identical to a freeze at Core's site. Under `-dbcrashratio` (CH2 D14) the synchronous wait for completion comes inside `freeze_after_step`, and it is an in-hold service wait that runs `retry_pending`, so the step's F1 event reaches a synced subscriber before W needs its ack. Done at Core's exact site, a correctly labelled freeze would deadlock there: W's rewind gate (backward) or ack wait (forward) would need an event V had not yet produced, while V waits for W. Core's synchronous flush has no subscriber-ack wait of this kind. The pressure decision and the wait for a previous F stay at Core's sites; the rejected end-of-hold pressure deferral is not reinstated.

### 7.2 The index in B1

- At every freeze, CH2b's V side calls `chain.take_dirty()` and puts the records in `FreezePacket.index` (CH2 §4.2). Core does the same: it drains `m_dirty_blockindex` when it builds the batch (`bs` `WriteBlockIndexDB`).
- CH2's index-only write (Core's `fPeriodicWrite`) takes the same set.
- `PruneClears { files }` (CH2 step 7) runs `prune_clear(files)` on V: for every entry with `file ∈ files`, the `PruneOneBlockFile` writes of §3.2 plus Core's `m_blocks_unlinked` erase of **every** `(prev, id)` entry, duplicates included (`bs:150-156` walks `equal_range(pprev)` and erases each match; rev3, OH-m8). Erasing one copy would leave a stale duplicate, from §3.3's reachable duplicate pairs, and `CheckBlockIndex`'s unlinked rules (`v:5768-5803`) would then fail. V returns the cleared records for Bk/Bp. It is a status-only write, serviced inside holds too (H3).
- A B1 failure aborts the node (CH2 †4), so nothing is re-dirtied. Records dirtied after the freeze go to the next B1.
- The ChainLock state is not written (§5.5). `MarkConflictingBlock`'s marks are not dirty in Core (SB4 D15) and reach disk only if a later dirtying write to the same entry happens. Restart persistence of conflict flags is CH3b's CONS-11.

### 7.3 Recovery: CH2's steps 4 and 11

**Step 4** (`open_chainstate`, CH2 §4.3): `HeaderChain::load(params, cfg, svc, records, pool)` (SB4 C7: parallel decode, PoW against the stored hash, chain work, skip, `FAILED_CHILD` propagation, `best_header`). Then `NodeIndex::load(&chain)` applies Core's rules in height order:
- `nChainTx` and `m_blocks_unlinked` (`bs:305-320`): if `nTx > 0`, `nChainTx = prev.nChainTx + nTx` when the parent's is nonzero, else 0 plus `unlinked[prev].push(id)`; genesis gets `nTx`;
- candidates (`v:5351-5358`): `try_add_candidate(id)` for `VALID_TRANSACTIONS` entries with `nChainTx != 0` or no parent (the tip is null at this point, so the tip filter is a no-op);
- `best_invalid` (`v:5359-5361`);
- every `sequence_id` is 0 and `next_sequence_id = 1`, as Core's loaded entries.

An empty index is Core's `needs_init` (`v:5362-5370`): `load_genesis_block()` (`v:5390-5424`) saves the genesis block (ST2), `HeaderChain::new_genesis`, then `received_block_transactions`. SB4 leaves the active chain empty (CH1-A11).

**Step 11**, in ND1's init order:
1. `load_chain_tip(coins_best)` (`v:4992-5018`), when the coins view is not empty: if the tip is already there, return; `chain.init_active_tip(id)` (`tip_seq = 0`, no event); `prune_block_index_candidates()`; log `Loaded best chain: hashBestChain=%s height=%d date=%s progress=%f`. An unknown hash is Core's `false` (an init error);
2. `VerifyDb` (CH5), then `reset_failure_flags_null()` at level ≥ 3 (`cs:463-465`);
3. V starts serving commands, and ND1 posts `ActivateBestChain`.

On a fresh datadir, ABC connects genesis through the ordinary `connect_tip` (Core's `ConnectTip` with a null tip; `ConnectBlock`'s genesis case is CH3a's), so subscribers see `Connected(genesis)` at `seq 1`. ND1 starts P2P and RPC only after that first ABC returns (D8).

**Interplay rules.**
- **I1 (CH2).** `HAVE_DATA` is set after the block's append returned, and B1 follows the files barrier, so a recovered `HAVE_DATA` has durable bytes.
- **DEC-148 and read errors** (rev3, OH-m5). A durable block in a 4 KiB page shared with an unsynced tail may return EIO after a crash. DEC-148 requires only that a read error is never treated as data loss; it does not require an abort. A read error therefore follows **Core's per-site handling**, unchanged:
  - `ConnectTip` aborts: "Failed to read block" (`v:3410`, §4.4).
  - `DisconnectTip` returns `false` (`v:3259-3263`, §4.5). Inside ABC, `ActivateBestChainStep` then aborts: "Failed to disconnect block" (`v:3621`).
  - `InvalidateBlock` returns `false` with a **valid** state (`v:3966`). The RPC then still runs ABC and reports success (`rpc:2017-2023`), as Core does.
  - Inside `MarkConflictingBlock` it is `assert(false)` (`v:4067`), an `AbortNode` here (§5.4); the IS conflict path asserts on `InvalidateBlock`'s `false` (`is:621-625`, §5.6).
  
  At every site, nothing clears `HAVE_DATA`, marks the block invalid or retries.
- **`ScanAndUnlinkAlreadyPrunedFiles`** (`v:5322`, `bs:412-427`) runs between `LoadBlockIndexDB` and the candidate rules. It is file-only (it unlinks block files the index already marks pruned), so it is CH8's; ND1's init order calls it at that point when CH8 lands, and step 4 leaves the slot.
- **I3 (CH2).** Blocks above the recovered tip are reconnected by ABC through the ordinary `ConnectBlock`.
- **Statuses** recovered from B1 are whatever the last durable B1 held. A lost `invalidateblock` or `reconsiderblock` is Core's (its `m_dirty_blockindex` was unflushed too).
- CH2b's CRASH suite gains the cases in §9.7.

## 8. Seams

### 8.1 The connector, mempool, flush and check seams

```rust
pub trait BlockConnector {   // CH3a (non-evo) + CH7a (evo); Recorded/Null implementations for CH1-node's tests
    fn connect_block(&mut self, cx: &mut ConnectCx<'_>, block: &Block, id: BlockId,
                     st: &mut BlockStatusWriter<'_>, bls: BlsCtx) -> Result<ConnectOutcome, Fatal>;
    fn disconnect_block(&mut self, cx: &mut ConnectCx<'_>, block: &Block, id: BlockId, bls: BlsCtx) -> Result<bool, Fatal>;
    fn tip_mn_list(&self) -> Arc<MnList>;
    fn test_block_validity(&mut self, cx: &mut ConnectCx<'_>, block: &Block, prev: BlockId,
                           check_pow: bool, check_merkle: bool) -> BlockValidationState;
}
pub enum ConnectOutcome { Connected, Invalid(BlockValidationState), SystemError(BlockValidationState) }
pub struct ConnectCx<'a> { pub v: &'a mut VContext, pub chain: ChainLockView<'a> /* has_conflicting_chainlock, has_chainlock */,
                           pub reader: &'a ChainReader, pub deployments: &'a dyn DeploymentView /* … CH3a/CH7a extend */ }
pub trait MempoolSink {      // MP1; NullMempool until then; CH3b's integration
    type Pool: DisconnectPool;
    fn new_disconnect_pool(&mut self) -> Self::Pool;
    fn maybe_update_for_reorg(&mut self, pool: Self::Pool, add: bool);
    fn remove_for_block(&mut self, vtx: &[Arc<Tx>], height: u32);
    fn remove_expired_asset_unlock(&mut self, height: u32);
    fn remove_recursive(&mut self, tx: Arc<Tx>, reason: RemovalReason);
    fn add_transactions_updated(&mut self, n: u32);
    fn check(&mut self, next_height: u32);
}
pub trait FlushSite {        // CH2b; NoFlush until then
    fn at_site(&mut self, mode: FlushStateMode, v: &mut FlushCx<'_>) -> Result<PendingFreeze, Abort>;
}
pub trait BlockChecks {      // CS3: CheckBlock, ContextualCheckBlock (v24 only)
    fn check_block(&self, b: &Block, st: &mut BlockValidationState, pow: bool, merkle: bool, hash: Option<&BlockHash>) -> bool;
    fn contextual_check_block(&self, b: &Block, st: &mut BlockValidationState, prev: Option<BlockId>, r: &ChainReader) -> bool;
}
```

The recorded and null connectors exist only for tests. `RecordedConnector` replays Core's recorded connector **transcript** (HX1's X1, §11; R1-m4): an ordered list of `(phase, block hash, occurrence, result, state)` for every `ConnectBlock`/`DisconnectBlock` invocation, so the same bytes can fail once (for example `HasConflictingChainLock`, `v:2516-2518`) and connect on a later attempt. Each V call consumes the next entry, the test asserts that the phase and hash match and that the transcript is fully consumed, and the connector never computes a verdict. Acceptance-time verdicts (`CheckBlock`/`ContextualCheckBlock` failures and their `BlockChecked`) are not connector invocations; they come through a separate `RecordedChecks` for `BlockChecks`. `NullConnector` connects everything (PF0). `ScenarioConnector` (rev3, OH-M3) is N9a's stand-in until X1 lands: it derives each verdict from the scenario's build ops (a block built with a named defect such as `cb-amount` is `Invalid` with Core's reject reason; every other block connects) plus V's own `has_conflicting_chainlock` through `ConnectCx`, so the `v:2516-2518` state dependence is computed, not recorded. N9b cross-checks it invocation by invocation against X1's transcript. None of them is a `cfg(test)` constructor of an EV capability: tests run a real V loop (EV addendum §2.6).

### 8.2 `ValidationSignals`: Core's callback queue (DEC-153)

Core has two kinds of `CValidationInterface` signal (`vi:193-330`). **Queued** signals are pushed to `SingleThreadedSchedulerClient` under the caller's locks and run in FIFO order on the scheduler thread: `UpdatedBlockTip`, `TransactionAddedToMempool`, `TransactionRemovedFromMempool`, `BlockConnected`, `BlockDisconnected`, `ChainStateFlushed`, `NotifyTransactionLock`, `NotifyChainLock`, `NotifyGovernanceVote`, `NotifyGovernanceObject`, `NotifyInstantSendDoubleSpendAttempt`, `NotifyRecoveredSig`. **Synchronous** signals iterate the listeners inline on the caller: `InitializeCurrentBlockTip`, `SynchronousUpdatedBlockTip`, `BlockChecked`, `NewPoWValidBlock`, `AcceptedBlockHeader`, `NotifyHeaderTip`, `NotifyMasternodeListChanged`. CH1-node defines one type for both; the node runtime (ND1) owns it, runs its thread, and hands V, MP1, LK1a, IS and governance a clone.

```rust
pub struct ValidationSignals { inner: Arc<SignalsInner> }
struct SignalsInner {
    queue: Mutex<LinkedList<Queued>>,    // L2s: a DEC-152 leaf; only O(1) splices under it (rev2, R2-M2; below)
    pending: AtomicUsize,                // CallbacksPending: queued + detached, not yet started (sch:196-200)
    wake: Notify,                        // notification-only (DEC-152 (2))
    listeners: ArcSwap<Vec<Arc<dyn ValidationInterface>>>,
}
pub struct Staged(LinkedList<Queued>);   // rev3: V-local, no lock; V pushes during a hold, release splices it (§2.5)
pub enum Queued {                         // one variant per queued signal above, plus:
    Barrier(oneshot::Sender<()>),         // CallFunctionInValidationInterfaceQueue (vi:160-163)
    /* UpdatedBlockTip { new, fork, ibd }, BlockConnected(Arc<Block>, BlockId), BlockDisconnected(..), ChainStateFlushed(Locator), NotifyChainLock(BlockId, Arc<ChainLockSig>), … */
}
impl ValidationSignals {
    pub fn enqueue(&self, e: Queued);                  // node allocated before the lock; O(1) splice + wake; never waits for a consumer
    pub fn splice(&self, staged: &mut Staged);         // rev3 (N1): the whole staged list in one O(1) L2s section + one wake
    pub fn callbacks_pending(&self) -> usize;          // vi:115-119 → sch:196-200
    pub fn sync(&self, wait: &mut dyn ServiceWait);    // SyncWithValidationInterfaceQueue (vi:165-174): Barrier, then wait for it
    pub fn limit_queue(&self, wait: &mut dyn ServiceWait) {   // v:3716-3722
        if self.callbacks_pending() > 10 { self.sync(wait) } }
    pub fn register(&self, l: Arc<dyn ValidationInterface>);  // RegisterSharedValidationInterface
    pub fn unregister(&self, l: &Arc<dyn ValidationInterface>); // vi:145-150; quiescence needs a following sync(), as in Core
    // synchronous signals, inline on the caller (LO-5):
    pub fn sync_updated_block_tip(&self, new: BlockId, fork: Option<BlockId>, ibd: bool);
    pub fn block_checked(&self, b: &Block, st: &BlockValidationState, ibd: bool);   // rev3: P2b's IBD gate, V-evaluated
    pub fn new_pow_valid_block(&self, id: BlockId, b: &Arc<Block>);
    pub fn accepted_block_header(&self, id: BlockId);
    pub fn notify_header_tip(&self, id: BlockId, ibd: bool);
    pub fn notify_masternode_list_changed(&self, undo: bool, old: &MnList, diff: &MnListDiff);
    pub fn initialize_current_block_tip(&self, tip: Option<BlockId>, ibd: bool);
}
```

- **The queue is a linked FIFO with O(1) critical sections** (rev2, R2-M2). A growable contiguous buffer (`VecDeque`) is only amortized O(1): a push into a full buffer reallocates and copies the backlog under L2s (and, for V, under L1). Core's queue is a `std::list` whose node allocation happens in `emplace_back` under `m_callbacks_mutex` (`sch:176-183`) and whose pop moves out one element (`sch:145-155`); we keep the list shape and move allocation and freeing out of the lock:
  - **Enqueue.** The producer builds a one-node `LinkedList<Queued>` (`push_back` on an empty local list: the only allocation) **before** taking L2s. Under L2s it does `queue.append(&mut one)` (a constant number of pointer writes; `LinkedList::append` neither allocates nor walks), then `pending.fetch_add(1)`. After releasing L2s it fires the wake. The cost under the lock is independent of the backlog and of consumer progress.
  - **Dequeue.** The consumer takes the whole list under L2s with `mem::take(&mut *queue)` (two pointer swaps, no walk) into a private batch, releases L2s, and runs the batch's callbacks in order outside the lock. Nodes are freed outside the lock as the batch drains. A `Barrier` completes when it is reached, so it still completes after every earlier entry and never waits for later producers (`vi:160-174`).
  - **Pending count.** `pending` counts every callback that is queued **or detached into the private batch and not yet started**: it is incremented at enqueue and decremented when a callback starts, not when the batch is detached. This keeps Core's `CallbacksPending()` (`sch:196-200`, the size of `m_callbacks_pending`, from which only the running callback has been popped) and so `limit_queue`'s `> 10` threshold. The running callback is not counted, as in Core.
  - Any representation with these properties is acceptable, for example a segmented queue of fixed-size chunks allocated outside the lock. A contiguous growable buffer is not.
- **Thread.** One thread detaches batches and runs each queued signal over the listener list current at dispatch, in FIFO order, as Core's scheduler client does.
- **Under a hold** V touches only its `Staged` list (no lock) and the synchronous signals; at the release, after the snapshot and `ChainView` stores, `splice` appends the whole list with `LinkedList::append` (constant pointer writes whatever its length) and adds its length to `pending` (rev3, N1). It never waits for the queue there (LO-4). Seams called by V under a hold receive the staging sink, not a `ValidationSignals` clone (§2.5).
- **Backpressure** happens only at Core's two `LimitValidationInterfaceQueue` call sites (`v:3760`, `v:3940`), where V holds nothing, plus Core's other `SyncWithValidationInterfaceQueue` callers (RPC `syncwithvalidationinterfacequeue`, `waitfor*`, wallet loading; their owners call `sync`). On V the wait is a release-point wait (`ServiceWait`), so it services class R and M commands. Since rev3 a listener awaits no actor reply at all (LO-9), so this servicing exists for Core's other `cs_main` users (RPC, P2P, LK1a) and not for the listeners. A listener that needs chain state reads `ChainView` or the snapshot, which by N1 already contains every tip it has been told about.
- **SB5's per-subscriber queues are already O(1) under their leaf** (checked for R2-M2). Each slot's `queue`, `staging` and `spare` are `VecDeque`s with capacity Q reserved at `register`, before the tip lock (SB5 §3.2, §3.4, §4.3), and a push happens only when `used < Q`, so it never reallocates; intake swaps 32-byte headers and appends at most Q items into reserved capacity; the reader-owned `ctl` grows outside the slot lock (SB5 §3.4; §4.5's critical-section table). SB5's `Pending` segments are not under a leaf: they live under the tip lock and hold at most one step's fanout (SB5 D8). So DEC-152's leaf rule holds there without change.
- **Not the F1 feed.** SB5's F1 stream carries only `TipStep` with its `seq` and compression semantics, and V drains its pending fanout per step (§2.5). Neither path carries the other's events (DEC-153). A subscriber that needs Core's callbacks registers with `ValidationSignals`; CH2's driver reads F1.
- **`AcceptedBlockHeader`.** SB4's `accept` and `accept_header` report the ids they inserted (CH1-A12); V calls `accepted_block_header` for each, in order, after SB4 returns and before `scope_end` (D11). SB4 has already applied the ChainLock half (`cl:152-172`) per header.

Signals V emits, at Core's sites: `BlockChecked` (`v:3432`, `v:4890`), `NewPoWValidBlock` (`v:4831`), `AcceptedBlockHeader` (`v:4724`), `BlockConnected` (`v:3797-3800`, after each `abc_step`, in trace order), `BlockDisconnected` (`v:3320-3322`), `SynchronousUpdatedBlockTip` + `UpdatedBlockTip` (`v:3819-3826`, `v:4035-4038`, `v:4143-4146`), `NotifyHeaderTip` (`v:3712`), `NotifyChainLock` (`h:318`); UI `NotifyBlockTip` (`v:3825`, `v:4043`, `v:4147`) and `NotifyChainLock` (`h:320`) go to ND1's UI path. `ChainStateFlushed` is CH2b's: it enqueues it here, staged when inside a hold, with the locator captured at Core's site (§7.1). `NotifyMasternodeListChanged` is CH7a's, called from inside the connector.

### 8.3 Reply dependencies and the no-wait marker (LO-8, LO-9)

A reply is **R/M-complete** if V sends it when a class R or M scope ends; it is **C-dependent** if V sends it only after a class C command or tail runs. During `limit_queue`, V runs R/M scopes and defers class C work until the running class C operation, which is itself waiting for the queue, finishes. So a listener awaiting a C-dependent reply while V waits for the queue is a cycle: the listener blocks the callback thread, the barrier cannot be reached, the running ABC cannot continue, and the deferred tail cannot run. No mutex is held, so LO-5 and LO-6 do not catch it (rev2, R2-M1). The same shape arises through another actor (cycle B) and through the per-step drain (cycle A, whose reply is R/M-complete). So since rev3 the rule is LO-9: a **marked** task (an F1 pop/ack task, the callback thread, W, a shared EV-gate holder) awaits **no** actor reply of either kind. The classification below now governs only unmarked callers' expectations (how long a reply can take) and which replies may be forwarded.

| Reply | Kind | A marked task may await it |
|---|---|---|
| `Query`, `OnV` (read-only and mutating), `ProcessHeaders`, `ClIntake`, `RecordChainLock` (its `ClEnforce` is queued without a reply), `TestBlockValidity`, `VerifyDb`, `Mempool`, `LoadMempoolDone`, `SetLoading` | R/M-complete | **no** (LO-9; rev2 allowed it for listeners) |
| `ProcessBlock` with `BlockReply::Accepted` | R/M-complete | **no** |
| `Flush` | R/M-complete as far as V is concerned; its waits are W's and subscribers' (CH2) | **no** |
| `ProcessBlock` with `BlockReply::Activated` (Core's `ProcessNewBlock` return, `v:4899-4906`; every Core caller's choice, §2.2) | C-dependent | **no** |
| `ReconsiderBlock`, `PreciousBlock` (their replies follow the ABC tail; precious's early return at `v:3864-3867` is not known in advance) | C-dependent | **no** |
| `ActivateBestChain`, `InvalidateBlock`, `IsConflictInvalidate` | C-dependent | **no** |
| any other actor's reply (P2a/P3 net actor, MP1, LK1a, IS) | may depend on V | **no** |
| `ClEnforce` (no reply) | — | posting only |

**Posting is always allowed.** A marked task may send any command with no reply, or with a reply it hands to an unmarked task (which may await it), and then return.

**Enforcement (every build, DEC-156 (1)).** Every actor handle returns `ActorReply<T>`; `ChainHandle` is one of them. On first poll, or on a blocking `recv`, `ActorReply` reads the no-wait marker (task-local, or thread-local for the callback thread, W's thread and V-spawned helper threads). If the marker is set it returns `Err(WaitError::WaitOnVFromNoWaitTask)` at once and never parks. The same check guards the node `spawn` wrapper's `JoinHandle` and `ValidationSignals::sync`. The send itself is never refused: the command runs and its reply is dropped, which is posting. So the cycles of LO-8/LO-9 cannot be built through these types. rev2's send-time `CDependentReplyOnSignalsThread` check is subsumed: the await is refused instead, for every reply and every marked task. Raw channel receivers are banned in marked code paths by clippy `disallowed_types`; the explicit-state model (§9.6) is the proof that the rule excludes every cycle among V, W, subscribers, the callback thread and the actors.

**Who sets the marker.** ND1's runtime, when it spawns the callback thread, W and each SB5 subscriber's pop/ack task; the EV gate's shared guard, for its lifetime; V, for helper threads it waits on. SB5 and CH2b receive this as a requirement on their pop/ack paths (§11, CH1-B4).

### 8.4 V's signal contract with P3: C5′ and C8 (DEC-273)

P3 lists its contracts on V as C1–C8 (P3-block-download.md §2.4, asks A-1 and A-1b in its §10). DEC-273 has CH1-node state two of them here, C5′ (DEC-240) and C8, before P3 S6's host adapter lands. Their Rust shape is CH1-node-notes D67, agreed by P3 (D111) together with BL-236's per-view reads:
- `ValidationInterface::new_pow_valid_block(&NewBlock)`;
- the `release_point` and `before_accept` hooks;
- `ServiceWait::wait_until`;
- the idle-point wait, the same `VWait` as a release point's (D67 r1 N7).

They landed as one CH1-node slice, with A-1b's hook, condition wait and idle-point wait, before S6's adapter, which cannot land without them (DEC-273, DEC-278). What follows is what V guarantees.

**C8. `NewPoWValidBlock` runs inline on V, inside the accept, with V's own tree (P3 D50).**
- **Where.** V calls the listener at Core's site, `accept.rs:276-277` (`v:4831-4832`), under the same guard: V's IBD latch false and the active tip the block's parent (C4). The call is inside the accept's class M scope and returns before `save_block` (`v:4834-4837`), so `BlockResult.new_block` is still unset when it runs.
- **What V passes.**
  - The entry's `EntryFacts` (`dashd-p2p` `seams.rs:69`): its id, hash, parent, height, time and chain work.
  - The block's `Arc`.
  - The stamp: one more than the `view_seq` V last published (`actor.rs` `publish_view`). That is the `view_seq` of the `ChainView` the accept's `scope_end` stores (§4.12; `scope_end` always stores one). No publication falls between the call and that `scope_end`: `accept_header` and `accept_block` publish nothing, so the stamp names the first publication after the push (P3 C7 (a)). If V stops or aborts after the call and before that `scope_end`, no view ever carries the stamp, so a C7 wait for it must also end on V's stop (D67 r1 N8).
  - A `TreeRead` over V's in-scope block tree, the arena V writes rather than a published view. It gives lookup by hash, `ancestor(id, height)` and chain work. A header that SB4's `accept_header` inserted earlier in this same accept is visible to it, as `LookupBlockIndex` finds it under `cs_main` (np:2253-2258). It is valid only for the call: the listener must not keep it, or anything borrowed from it, past the return.
- **Locks.**
  - **What V holds during the call.** Nothing a reader takes:
    - no tip lock, because the accept is a class M scope and never runs inside a hold (§2.5);
    - no `NodeIndex` guard (`accept_block` drops the one it takes on the rejection path before it returns);
    - no block-file lock (the write comes after the call).
  - **Whose locks do not block.**
    - `snapshot()`, `drain_snapshot()` and a `ChainAccess` read go through V's read side (`ChainViewHandle`, §6.3), which loads the published view.
    - A status read's `status_at` holds a status journal's log mutex for one journal's scan. V takes that mutex only to append a status write, never across the call.
    - `read_block` reads the block LRU or the files (A-3).
  - **So none of these waits on V.** The listener, or any thread holding a P3 header cell, may call them while V is in the call.
- **Waiting.**
  - **Waiting inside the call.** The call may wait for one P3 header cell at a time. That is allowed by LO-5's condition for a lock V may wait behind: no holder of a header cell waits on V while it holds one (P3 §3.6, §3.8.1). The listener sends V no command and awaits no actor reply (LO-5, LO-9).
  - **Why nothing is queued.** `NewPoWValidBlock` queues nothing, unlike `BlockChecked` (P3 C5). The announcement has finished before the accept publishes, so it never waits in P3's queue behind a `BlockChecked` or for the net actor.

**C5′. Before an accept outside IBD, no signal is outstanding (DEC-240, P3 D55).**
- **Scope.** This covers a `ProcessBlock` command's accept (`actor.rs:1114` → `process_block_accept`) and an import's per-block accept.
- **The rule.** When V's own IBD latch is false, V first waits until P3's queue holds no outstanding entry: none queued, and none drained by the actor but not yet processed. This keeps a `BlockChecked`-driven high-bandwidth `sendcmpct` from being overtaken on the same connection by a later accept's `cmpctblock`. Core cannot produce that order, because its `BlockChecked` runs inline in `ConnectTip` (`v:3432`).
- **The wait is a C5 wait (DEC-229, DEC-230).**
  - **Where it waits.** Only at points C5 already lists: V's idle point before it starts the command, an ABC release point that serves the command, and the point before an import's accept. Never inside an ABC step or an accept, never while `holding`, never while the queue is `PreStart`, and never nested inside another `wait_for_space` wait.
  - **How it runs.** It rings the doorbell first. It ends when the condition holds, when the queue is `PreStart` or closed, or on shutdown.
  - **What it serves.** It serves class R and M commands from both lanes, as every `VWait` does, `held` first (D67 R2). After an `AbortNode` it serves nothing more, and the listener returns the `Err` (r1 N1).
  - **The listener's side (D67 r1 N2, N3).** A command served in the wait may call the listener's own inline methods (`block_checked`, `accepted_block_header`, `notify_header_tip`) inside its hook, so it holds no lock across the wait that those take, and `done` takes and releases its own; `done`'s lock is one V waits behind (LO-5): no holder of it waits on V. In IBD a `release_point` wait does run `ProcessBlock` accepts (tails deferred) but never moves the tip, so C8 and C5′ are not re-entered. `wake` is unbounded, or bounded with capacity at least 1 (never a rendezvous channel), rung after the change `done` reads is visible, and read by V alone while V waits; dropping every sender means closed.
- **Deferral.**
  - **What a C5 wait defers.** It never runs such an accept itself: it defers the `ProcessBlock` command, as it defers class C, and V runs it after the wait, behind the same check. An accept that finds an entry outstanding therefore waits for the actor's next drain point.
  - **The accepted residual.** One actor drain, even when no high-bandwidth frame is at stake (DEC-240), which P3's §6.3 timing probe reports.
  - **In IBD.** V emits no `NewPoWValidBlock` (`v:4831`), so it does not wait.
- **No cycle.** The actor awaits V only through draining waits or replies that every release point serves (DEC-229, P3 §3.8.1), so this wait adds no edge to V's wait graph that LO-9 or DEC-229 does not already admit.
- **Tests and the model** come with the code slice:
  - a directed V test where an outstanding `BlockChecked` holds back a second `ProcessBlock`'s accept until the drain, then lets it run;
  - the IBD and `PreStart` exemptions;
  - the lock-and-wait model's N-sender case (A-1b (5)), with init's ABC, a class M command the actor awaits, and the mutants as controls (r1 N5).

## 9. Test plan

### 9.1 The PLAN row's acceptance, through V

| # | Test | Pass |
|---|---|---|
| T1 | Full mainnet and testnet headers from the oracle files through `ProcessHeaders` in 2,000-header batches | chain work at every 10,000th height, `best_header`, and `getchaintips` (via `Query`) equal to the v24 oracles' (memory: dashd reference oracles) |
| T2 | Parallel load: B1-shaped records of T1's chains plus a regtest index with data, unlinked and failed entries; reload through recovery step 4 | the arena, `best_header`, `n_chain_tx`, `unlinked`, candidates and `best_invalid` equal to a sequential reference load; `startup_s` measured (§10's BG) |
| T3 | `-checkblockindex` on in every V test | Core's asserts hold after every command (§4.10) |
| T4 | Seqlock coherence stress: 8 reader threads on `ChainViewHandle` while V runs a 50,000-block synthetic IBD with `NullConnector`, 200 reorgs and 50 invalidate/reconsider pairs | each `EntryView` is one written version (the writer derives `data_pos` and `file` from a per-write counter, and readers check the relation); every snapshot's `active` matches its `tip` |
| T5 | The walk-backs at the three hook sites against the oracle | `a18-invalidate-walkback`, `a18-invalid-chain-found`, `a36-conflicting-marked-order`, `a29-x-kept-mark-trailing` and `a26-ii-enforcement-reorg` replayed through real V: `best_header` after every op equal to the harness's (§9.2) |

### 9.2 Harness scenarios through real V

`ScriptedV` replays only the trace's tip steps (SB4 §15). CH1-node replays the **inputs** instead: each `build … submit: connect` block as `ProcessBlock`, `pnbh` as `ProcessHeaders`, `cl` as `ClIntake` + `RecordChainLock` (signatures are `--wrap`ped in the harness, and V receives verified locks), `timer`/`enforce` as `ClEnforce`, `invalidate`/`reconsider` as commands, `spork19` through `StaticSporks`, `mocktime` through `dash-clock`. V's own control flow produces everything else. The comparison is DEC-147's semantic-state comparison: exact through each lock's first enforcement and its ABC, then idempotent repeats allowed.

- **Fixture ops** (`seed-cl`, `mark-conflicting`, `notify`, checkpoints) go through narrow test adapters in `dashd-chainstate`'s test-support: each sends an ordinary command to a real V loop, or seeds SB4 through its existing test constructors before V starts. None constructs a `VContext`, `OnV` or `AttemptHandle` (EV addendum §2.6).
- **`reload`.** The harness force-flushes before tearing Core down. The replay does the same, a `Flush { mode: Always }`, then a clean shutdown and recovery. It is a restart test, not a power-loss test; power loss is §9.7's.
- **Scheduling.** V's schedule (release-point FIFO, `ClEnforce` timer and tip-scheduled tasks) is recorded in each run's output, beside the harness's own callback-delivery policy, so a mismatch can be attributed to the schedule or to the semantics.

- **Every existing scenario** (39 at 7c35a6c6) runs, comparing tip, `best_header`, `locator[0]`, `m_failed_blocks`, ChainLock state (including `best_known`, after every op), statuses and the signal trace (`UpdatedBlockTip`, `BlockConnected`, `BlockDisconnected`, `NotifyChainLock` with its signature). Because CH1-node implements `FindMostWorkChain`, statuses are compared **exactly after ABC as well**, not only at the `EnforceBlock` boundary. This lifts SB4's D40 limitation for the node.
- **Through `ScenarioConnector` first** (rev3, OH-M3): N9a replays all 39 with `ScenarioConnector` (§8.1), which needs none of HX1; N9b re-runs them with `RecordedConnector` on X1's transcripts and requires the two runs' connector invocations to agree.
- **New Core-judged scenarios** (N9b; HX1's X1–X3 and X5, §11):

| Scenario | What it pins |
|---|---|
| `ch1-unlinked-out-of-order` | headers first, blocks delivered in a permuted order across three branches: `nSequenceId` order, `m_blocks_unlinked` drain order, the chosen tip |
| `ch1-equal-work-first-seen` | two equal-work tips with data, received in each order: first received wins (`nSequenceId`) |
| `ch1-precious-sequence` | `preciousblock` on alternating equal-work tips, then an extension resets the counter (`rpc_preciousblock` port) |
| `ch1-invalid-in-batch` | `cb-amount` at the 7th block of a 40-block reorg: `InvalidChainFound(front)` (the batch top), `FAILED_CHILD` propagation in `FindMostWorkChain`, `best_invalid`, the fork warning |
| `ch1-invalidate-equal-work` | `invalidateblock` 40 deep with two equal-work side branches: the multimap re-adds and the final scan |
| `ch1-reconsider-promote` | reconsider an ancestor with `FAILED_CHILD` children: promotion to `FAILED_VALID`, `best_invalid` reset |
| `ch1-reconsider-ignore-cl` | `reconsiderblock(hash, true)` after a ChainLock conflict |
| `ch1-conflict-downloaded-branch` | a lock that marks a downloaded heavier branch: `FindMostWorkChain`'s `CONFLICT` propagation and dirtiness, and `MarkConflictingBlock`'s re-add scan |
| `ch1-acceptblock-unrequested` | `pnb {force: false}` with less work, too far ahead and below `MinimumChainWork` (`p2p_unrequested_blocks` port) |
| `ch1-invalidate-genesis-and-tip` | genesis refusal; invalidating the tip, then reconsidering it |
| `ch1-reload-candidates` | `reload` after a reorg leaves two data-bearing branches of distinct work: load-time candidates and `LoadChainTip` |
| `ch1-cl-late-block` | a lock on a header-only block, then the block arrives: floor via `AcceptedBlockHeader`, enforcement at the next `timer`/`UpdatedBlockTip` task, `NotifyChainLock` once |
| `ch1-cl-late-header-known` (R1-M1) | lock on unknown A85, then `pnbh` A81…A85: `best_known = A85@85` right after `pnbh`, before any timer (Core: `a29-ii-late-header-before-check`); variants: a partial batch A81…A85 + an invalid A86; a prior lower known lock A80; a lock whose height mismatches its header, which keeps the old pair |
| `ch1-cl-late-header-via-block` (R1-M1) | the locked header first arrives inside a `pnb` (`accept_header`), both when the block is accepted and when it fails contextual checks |
| `ch1-accept-contextual-reject` (R1-M4) | a higher-work block whose header is new and valid but whose coinbase height is wrong (`bad-cb-height`, `v:4580-4585`): `best_header` and `FAILED_VALID` visible in the published snapshot right after the failed `pnb`; no `NotifyHeaderTip`; a pending lock on that header resolved and published |
| `ch1-headers-partial-fail` (R1-m3) | a `pnbh` batch accepting H1 and rejecting H2, then a valid batch: the accepted prefix published, no notification for the failed batch, exactly one for the next |
| `ch1-cl-intake-reset` (R1-M3) | spork off with an old lock A100 recorded, spork on, then a lower-height CLSIG with a bad signature: after the rejection, `best = None` and observed on, before any timer; also for a duplicate and a stale CLSIG |

Equal-work ties between **loaded** entries are excluded: Core breaks them by heap address, which is not reproducible (D9).

The ChainLock scenarios that need a command at a specific release point (R1-M2: a higher lock recorded while enforcement's ABC runs) cannot be produced by the harness, which has no hook inside Core's ABC. They are the directed V tests of §9.4, whose expected outcome is Core's captured-pair rule (`h:283`, `h:318`).

### 9.3 Property and differential tests

- **Candidates vs brute force** (proptest; random trees up to 400 entries; random data arrival including duplicate unlinked pairs, invalid verdicts, invalidate/reconsider/precious, locks, prunes, reloads). **At quiescence** (rev3, OH-m7): when V's `rx` and `deferred` are both empty and no composite runs, because ABC caches `pindexMostWork` across outer iterations (`v:3774`, `v:3842`), so a better candidate added by a class M command at a release point is legitimately ignored until the deferred tail runs. Runs that ended by `-stopatheight`, shutdown or a system error are exempt. Then the tip equals a brute-force reference, namely the best entry under Core's comparator among the active tip and the entries whose **non-active suffix** (the blocks between the entry and its fork from the active chain) all have data, `nChainTx`, no `FAILED` and no `CONFLICT`, given the same verdict oracle. Active blocks are not re-examined, so an active chain whose old blocks were pruned keeps its tip (`v:3537`). `check_block_index` and §3.3's C1 hold after every command, and `find_most_work_chain` terminates.
- **Candidate keys** (R1-M5), directed: duplicate `(P, C)` unlinked visits; a re-download that re-keys a member; `invalidateblock` on the re-keyed block; `preciousblock` on it. Each checks C1, that the set holds one key per block, and that selection terminates.
- **Load ≡ Core's load rules** (R1-m2): `take_dirty()` returns deltas, so the test applies every index batch in order to a retained in-memory database image, then runs `NodeIndex::load` on the image and compares with **Core's load rules applied to the same records** (§7.3): `n_chain_tx`, `unlinked` and `best_invalid` as Core rebuilds them, every `sequence_id` 0, candidates per `v:5351-5358`. It does not compare against the live side tables, whose sequence ids and candidate keys reload intentionally resets.
- **Random differential** (X4, N9b): `regen.py --random N` produces Core-judged random op sequences, and V must match under DEC-147. These run in O3's farm as a fuzz campaign (`fuzzctl`), not in the PR gate.

### 9.4 R6 and R8

- **R6** `v_release_points` (R1-m1): (a) a 1,000-block IBD with `NullConnector` and valid candidates while a thread sends `ProcessHeaders` every 200 µs: each is answered after at most one further block connect from its arrival (one that arrives just after a release point sampled `rx.len()`, §2.4, waits for exactly that connect; OH-n4); (b) a 1,000-block reorg: `ProcessHeaders` are answered at the first release point after the tip is **not worse than `starting_tip` under the full comparator**, and none earlier (the published tip never shows the fork point); (c) an **equal-work** reorg to a precious branch: the hold ends at equal work; (d) a candidate that fails to connect: the hold ends at the early exit (`v:3777-3780`, `v:3791-3794`) without a fixed per-block bound; (e) `invalidateblock` 1,000 deep: each answered within one iteration.
- **ChainLock schedules** (R1-M2, R1-M3), deterministic, with a test hook that injects a command at a chosen release point: (a) enforcement captures A100, its ABC starts, `ClIntake` + `RecordChainLock(A101, known)` run at a release point: the notification is `NotifyChainLock(A100, sig(A100))` and A101's enforcement follows; (b) the same with a higher lock on another branch, and with an off→on reset during the activation (notification still with the captured pair, after `last_notify` was cleared); (c) R1-M3's rejected-CLSIG reset, observed through `ClReader` immediately after LK1a's rejection, for an invalid signature, a duplicate and a stale height.
- **R8** `reader_sees_one_epoch` (P2a G4): a deterministic reorg between a reader's two loads; the reader that keeps its `Arc` sees one complete epoch.
- **Publish before notify** (rev3, OH-M1, N1). Each runs under an adversarial schedule: a test hook parks V just before `release_tip` and the callback thread and notified readers run at top priority. A missing N1 would therefore show deterministically.
  - (a) `listener_sees_announced_tip`: a listener reads `ChainAccess::snapshot()` and `ChainView` inside its `UpdatedBlockTip`, `BlockConnected` and `BlockDisconnected` callbacks. It must see the announced tip, across a connect, a 3-block reorg, an `InvalidateBlock` iteration and an `enforce_block` mark.
  - (b) `sendheaders_after_tip` (the reviewer's P2P case): a peer that prefers headers; B extends the tip. A P2b-shaped listener pushes B to `m_blocks_for_headers_relay` (`np:2273-2308`), and a `SendMessages`-shaped reader checks `snapshot.active[h(B)] == B` (`np:6425-6427`). It must take the `headers` branch, never `fRevertToInv`.
  - (c) `waitforblockheight_then_getblockcount` (the reviewer's RPC case): an RPC-shaped waiter on `latestblock`, woken through `ui.notify_block_tip` (`rpc:300-305`, `rpc:431`), then reads `getblockcount` from the `ChainView` fast path. It must return the new height. The same for `-blocknotify` followed by `getbestblockhash`, and for a `TipWatch` long-poll waiter.
  - (d) `inline_listener_gets_ibd`: P2b's `BlockChecked` gate receives V's `ibd` argument while the published view still says pre-hold. The argument wins, and the listener never reads the view.
  - (e) `chainstateflushed_locator_pre_step`: an IF_NEEDED full flush at a disconnect of `d` stages `ChainStateFlushed(locator(d))` before `BlockDisconnected(d)` (OH-m1).
  - (f) `clsig_getdata_zero`: with no lock recorded, `ClReader::by_hash(0)` returns the null sig, and P2b's `getdata(MSG_CLSIG, 0)` replies with a `clsig` (OH-m2). A dde-probe raw-P2P regtest probe against v24 confirms the bytes; the probe also covers the post-reset case.
  - (g) `recorded_enforce_after_reply`: `RecordChainLock` for a known block on an idle V. The reply (and its relay) is observed before the queued `ClEnforce` starts (OH-m3).

### 9.5 SB4's node tests, re-run with real V

A-26 (both cases), A-36 and A-39 run with V in place of `ScriptedV`. A-40 and A-41 run with test-support's model W until CH2b, then with the real W (CH2b's acceptance).

**Freeze labelling** (R1-B1), with the model W until CH2b, then the real W: disconnect B110 → B109 under an ABC hold and freeze, (a) as one batch and (b) split (`head_blocks = [B109, old]`). Each asserts the packet is `(B109, q+1)` and `new == coins.best_block() == evo.best_block()`; then crash after B1, reopen and replay: the tip is B109 over B109's coins, with no replay toward B110; and a synced subscriber that has applied `Disconnected(B110, q+1)` classifies `FlushCommit { B109, q+1 }` correctly and acks it. The same for a connect, and with a `FlushDue` latched during the CRITICAL wait.

### 9.6 Model checks

- **Lock-and-wait model** (explicit state, in `node-fuzz` beside CH2 §4.4.3's): threads V, W, two subscribers (one closing; each with a pop/ack task), the `ValidationSignals` thread with two listeners, a P2a/P3 **net actor** that awaits `ProcessBlock` replies, an EV external reader, a P2a `ProcessHeaders` sender, an RPC thread and LK1a. It models the real channel, feed and reply waits, not only mutex order: Q-bounded F1 queues, `retry_pending`, W's ack channel, `FlushDue` and its latch, the signals queue, `Staged` and the barrier, and `ActorReply` awaits with the no-wait marker. Actions: ABC holds with connects and disconnects, `InvalidateBlock` iterations, `enforce_block`, CRITICAL waits, `-dbcrashratio` completion waits, per-step drains, `PruneClears`, attempts with the exclusive gate, shared-gate reads, `register`/`subscribe_at` returning `HoldActive` during a hold and retrying on the release wake, `deregister` with a failed `try_lock` and lazy cleanup (also while V's drain waits on that subscriber's slot), `LimitValidationInterfaceQueue`, releases with their splice, and any marked task's send or await. Properties: no deadlock; no registration caller ever parks on L1 during a hold, and every `HoldActive` retry eventually registers (rev3.1); no step starts with `pending` non-empty; no class R/M command runs inside a hold; every freeze's `(new, tip_seq)` equals the coins' best block and the private tip's `seq`; every reader's snapshot is a published one; **N1**, so no listener or notified reader observes a tip notification before a published snapshot contains it; **LO-9**, so with the marker on, no marked task is ever parked on an `ActorReply`, `JoinHandle` or `sync`.
- **LO-9 controls** (rev3, DEC-156 (1); supersede rev2's LO-8 controls), in that model and as threaded tests. Each *negative* control, run with the runtime check **disabled**, must reach the deadlocked state in the model; run with the check **enabled**, the await fails with `WaitOnVFromNoWaitTask`, nothing parks and the run terminates.
  - (N-A) **Cycle A**, OH-M2: an LK1b-style F1 subscriber whose pop task awaits an `OnV` route-B reply (an ISDLOCK's quorum selection) while V holds an ABC hold for a 2-block reorg with Q = 1. Disconnect 1 fills its slot, disconnect 2's fanout is pending, and V's drain waits for the pop. Variants await a `Query` and a `ClIntake` reply instead.
  - (N-B) **Cycle B**, OH-M2: a listener (P2P's `UpdatedBlockTip`) awaits a net-actor reply; the net actor awaits `ProcessBlock { Activated }`; V waits in `limit_queue` with its barrier behind that listener.
  - (N-C) Sol's R2-M1 schedule: a listener awaits `Activated` directly; also `ReconsiderBlock` and `PreciousBlock` replies, Separately, a listener awaiting an R/M-complete `ProcessHeaders` reply: rev2 allowed it, and it would not deadlock, but LO-9 refuses it. That checks that the marker covers R/M replies too.
  - (N-D) a subscriber's pop task and W each calling `signals.sync()`; a listener awaiting `Flush` whose completion needs the ack of a subscriber.
  - *Positive:* (P-1) the same listeners **post** instead: the `Activated` receiver is forwarded to an unmarked task, the listener returns, the barrier completes, the ABC continues and the forwarded reply arrives. (P-2) An unmarked RPC thread's `Query` and a P2a `ProcessHeaders` sent while V waits in `limit_queue` are serviced at that release-point wait. (P-3) The LK1b subscriber posts its `OnV` with the reply forwarded to its verification task; the drain completes. (P-4) A marked task's send succeeds and the command runs.
- **Queue growth under a paused consumer** (R2-M2), threaded: a test hook pauses the signals thread; producers (V and two others) enqueue until the backlog is 1, 128, 16,384 and 1,048,576. Also, a release splices a `Staged` list of 1, 128, 16,384 and 1,048,576 entries (rev3). A counting allocator asserts **zero** allocations, reallocations and frees while L2s is held, and PF0's instruction counter shows the per-enqueue critical section constant within noise across the four backlogs. `callbacks_pending()` equals the backlog throughout, including after the consumer resumes and detaches the whole list (it falls by one as each callback starts), and FIFO order and barrier completion hold.
- **DEC-153's case** (R1-M6), required in that model and as a threaded test: Q = 1, an ABC hold with two consecutive disconnects, and a subscriber parked (it is waiting for V's answer to its `FlushDue`). Disconnect 1's event fills its queue; disconnect 2's fanout is pending; V's in-hold wait answers the `FlushDue`, the subscriber pops, `retry_pending` delivers, and only then does a third step start. Variants: the subscriber closes while parked (H7 retires it); `-dbcrashratio` with a freeze after disconnect 2 (W's completion needs the subscriber's ack of `Disconnected(…, q+2)`).
- **`loom`**: the release-point servicing and the deferred queue (a class C command arriving at a release point runs after the composite, never inside it); the release sequence (snapshot store → `ChainView` store → splice → `TipWatch`/UI) against a callback-thread reader, which must never observe a staged entry before both stores (N1); the marker propagation through `spawn`.

### 9.7 Crash cases for CH2b's CRASH suite

`invalidate_then_crash`, `reconsider_then_crash`, `received_block_then_crash` (`HAVE_DATA` without B1), `prune_clear_then_crash`, `reorg_across_freeze` (a freeze inside an ABC hold), `disconnect_freeze_then_crash` (§9.5's freeze labelling), and (rev3, OH-m9):
- `genesis_then_crash`: genesis is saved and connected, and the node crashes before the first B1. Reopen finds no index, hits `needs_init` again (`v:5362-5370`), and connects genesis through ABC.
- `prune_clear_in_hold_then_crash`: `PruneClears` is serviced inside an ABC hold while a freeze is in flight. The cleared records go to the **next** B1, never the one in flight, and a crash between the two leaves an index that is a committed prefix.

Each checks that after reopen the index equals the image of some prefix of committed B1s, `load` succeeds, and then (R1-m2) **either** ordinary ABC from that durable prefix reaches the tip that prefix selects under Core's rules, **or** the run fail-stops as DEC-148 permits (a read error on a block in a shared page). It does not require the uncrashed run's final tip: with A100 durable, `invalidateblock A90` and a crash before its flags reach B1, the live run ends at A89 while the reopened node may keep A100, as Core would.

### 9.8 Functional tests (M3, after CH3a, ND1 and RP0)

`rpc_invalidateblock`, `rpc_preciousblock`, `rpc_getchaintips`, `p2p_invalid_block` and the F01 restart case belong to CH3b (§1.2). The rest are tracked here and run when their owners land: `p2p_unrequested_blocks` (§4.9's gates), `feature_llmq_chainlocks` and `feature_llmq_is_cl_conflicts` (LK1a, IS), `mining_basic`'s proposal checks (`TestBlockValidity`), `feature_pruning` (CH8), `rpc_blockchain` (`getblockchaininfo` fields from `ChainView`, and its `waitfornewblock`/`waitforblock`/`waitforblockheight` cases, each followed by a chain read, N1), `p2p_sendheaders` (headers, not `inv`, after a tip change, N1), `interface_zmq` (`hashblock` then `getbestblockhash`, N1), and `feature_reindex` (CH5). §9.4's directed N1 tests stand in for the last three until M3.

## 10. Performance and the commit plan

### 10.1 PF0 lines

Bench target `dashd-chainstate/benches/chain.rs`, lines for `contrib/node/perfgate/workloads.toml`:

```toml
chainstate-load-index-mainnet = { bench = "dashd-chainstate/chain", id = "load/mainnet-index" }          # BG: startup_s
chainstate-process-headers-v  = { bench = "dashd-chainstate/chain", id = "v/process-headers-2000" }      # V round trip, accept + notify
chainstate-abc-connect-1k     = { bench = "dashd-chainstate/chain", id = "abc/connect-1k-null" }         # per-block control flow
chainstate-reorg-100          = { bench = "dashd-chainstate/chain", id = "abc/reorg-100-null" }
chainstate-invalidate-1k      = { bench = "dashd-chainstate/chain", id = "invalidate/depth-1k-null" }
chainstate-received-txs-10k   = { bench = "dashd-chainstate/chain", id = "accept/received-unlinked-10k" }
chainstate-find-most-work     = { bench = "dashd-chainstate/chain", id = "candidates/fmwc-1k-forks" }
chainstate-checkblockindex    = { bench = "dashd-chainstate/chain", id = "check/regtest-50k" }
chainstate-view-publish       = { bench = "dashd-chainstate/chain", id = "view/publish-per-step" }       # Q5 guard
```

- **Core comparisons.** `chainstate-load-index-mainnet` is measured against dashd's `LoadBlockIndexDB` + `LoadBlockIndex` time on the same index (the PF1 profile's bench-log phase). `abc-connect-1k` is compared with the non-`ConnectBlock` share of Core's `-debug=bench` per-block lines ("Load block from disk", "Connect postprocess"). Both use PF0's counter method.
- **BG (row).** In the window, `startup_s` and `idle_rss_mib` must be ≤ dashd's. The side tables add 8 B per entry (§3.1).
- **Expected costs.** Per connected block in IBD: one hold (one mutex acquire), one `seq`, one snapshot and one `ChainView` store, one `BTreeSet` insert and one `prune` step for the candidates, and the event handoffs. All are O(1) amortized beside the connector's work. Linear scans happen only where Core scans the whole index (`v:3918`, `v:4028`, `v:4131`).

### 10.2 Commits

| # | Commit | Content | Days | After | Unblocks |
|---|---|---|---|---|---|
| N0 | `feat(dashd-chainstate): V actor, ChainCommand and command classes` | §2: loop, classes, release points, deferred queue, timers, shutdown, `VContext` creation; seams of §8.1 with Null/Recorded impls | 1.5 | SB4 C0 | P2a-node's `process_headers` wiring (stub replies) |
| N0s | `feat(dashd-chainstate): ValidationSignals, ActorReply and the no-wait marker` | §8.2–§8.3: the linked FIFO, pending count, thread, barrier/`sync`, `limit_queue`, `Staged` and `splice`, synchronous signals, register/unregister; `ActorReply`, the marker (task- and thread-local, `spawn` propagation, gate guard) and the `disallowed_types` lint (LO-9); loom with a reader preempted in the leaf section (DEC-152); the growth test (§9.6) | 1.5 | N0 | ND1, MP1, LK1a, IS, every actor handle |
| N1 | `feat(dashd-chainstate): NodeIndex tables, candidates and ReceivedBlockTransactions` | §3, §4.8, §4.9's `received_block_transactions`, §7.3's load rules | 2 | N0, SB4 C2 | — |
| N2 | `feat(dashd-chainstate): ProcessHeaders, ProcessBlock and AcceptBlock on V` | §4.9, §4.11's `notify_header_tip` | 1.5 | N1, SB4 C3; i:CS3 (real checks; the stub accepts until CS3 merges) | P2a-node, P3 |
| N3 | `feat(dashd-chainstate): ActivateBestChain, ConnectTip, DisconnectTip, FindMostWorkChain` | §4.1–§4.5, holds and the release sequence (snapshot → `ChainView` → splice → `TipWatch`/UI, N1), per-step drains, staged signals, `update_tip`, `scope_end`; §9.4's N1 directed tests (a)–(e) | 3.5 | N1, N0s, **SB4 C6** (rev3, OH-m6: through SB5 F3) + CH1-A1/A2/A8/A11/A13, **SB5 F3 extended to V's holds (CH1-B1–B4)** | CH3a |
| N4 | `feat(dashd-chainstate): InvalidateBlock, ReconsiderBlock, PreciousBlock` | §4.3, §4.6, §4.7, CH1-A3 | 2 | N3, SB4 C6 | RPC rows |
| N5 | `feat(dashd-chainstate): ChainLock intake, record and enforcement on V` | §5 (CH1-A5–A7, CH1-A12), the mirror with Core's zero-hash default and M1, captured-pair enforcement, queued `Recorded` enforcement, TipHost, timers, IS conflicts; §9.4 (f), (g) | 2.5 | N3, SB4 C6 + CH1-A5–A7/A12 | LK1a |
| N6 | `feat(dashd-chainstate): ChainView and the read-side handles` | §6, `Query`, the `tip_seq` fence helper | 1.5 | N3, SB4 C5 | P2a-node, RP0 |
| N7 | `feat(dashd-chainstate): flush sites, B1 index records, recovery steps 4 and 11` | §7 (`FlushSite` with `NoFlush` and the model W, the post-step pair and its check, the pre-step `ChainStateFlushed` locator, the `FlushDue` latch, `prune_clear` over every duplicate, `load_genesis_block`, `load_chain_tip`) | 1.5 | N3, SB4 C7; CH2b codes its side later and re-runs §9.5's freeze tests with the real W | CH2b |
| N8 | `feat(dashd-chainstate): CheckBlockIndex, node half` | §4.10 (CH1-A9) | 1 | N3, SB4 C9b's visitor | — |
| N9a | `test(dashd-chainstate): models, properties, R6/R8 and replays through ScenarioConnector` | §9.1, §9.3 (brute force at quiescence, candidate keys, load ≡ Core), §9.4, §9.5, §9.6 (lock-and-wait model with N1, LO-9 negative and positive controls, growth, DEC-153's case, loom), §10.1's PF0 lines, and the 39 existing scenarios through `ScenarioConnector` (§8.1, §9.2). **No HX1 dependency** | 4.5 | N2–N8 (the model and loom parts start after N0s) | the row's acceptance except the Core-judged additions |
| N9b | `test(dashd-chainstate): Core-judged ch1 scenarios and the random differential` | §9.2's 17 `ch1-*` scenarios; the 39 replays re-run with `RecordedConnector` on X1's transcripts and cross-checked against N9a's `ScenarioConnector` runs; X3's dump comparison; X4's campaign registered with O3 | 2 | N9a, **HX1** (X1–X5) | the row |

**Arithmetic** (rev3, OH-M3). These are estimates: **25 agent-days** in total (rev2: 21; rev3 adds the marker and staging in N0s and N3, and N9 grows from 3.5 to 6.5 days split as N9a 4.5 + N9b 2). With two implementers, counting from N0's start and assuming the SB4/SB5 gates are open:

| Implementer | Sequence | Ends |
|---|---|---|
| A | N0 [0–1.5] → N1 [1.5–3.5] → N3 [3.5–7] → N4 [7–9] → N5 [9–11.5] → half of N9a's tail [11.5–12.5] → half of N9b [12.5–13.5] | 13.5 |
| B | N0s [1–2.5] (N0's types stable at day 1) → N9a's model and loom, part 1 [2.5–3.5] → N2 [3.5–5] → N9a's model, part 2 [5–6] → `ScenarioConnector` and the replay scaffold [6–7] → N6 [7–8.5] → N7 [8.5–10] → N8 [10–11] → property and R6 suites on N3/N4 [11–11.5] → the other half of N9a's tail [11.5–12.5] → the other half of N9b [12.5–13.5] | 13.5 |

Integration and review fixes add about a day, so the **critical path is about 15 days (3 weeks)**: N0 → N1 → N3 → N4 → N5 → N9a → N9b. Two calendar gates sit on it, outside CH1-node's own days:
- **N3 waits for SB4 C6** (not C5): it needs SB5 F3, and F3 needs C6 plus CH1-A1 and SB5's A11/A12 (SB5 `:1157`).
- **N9b waits for HX1** (about 4 agent-days plus a Sol review). HX1 stays off the critical path if it starts by CH1-node's day 6.

The row stays L, offset by CH3b dropping to M (Q4). The manager reruns `tools/cpm_plan.py` with the size change, the C6 gate and HX1.

**Dependencies.**
- SB4: C0 now; then C2, C3, C5, C6, C7 and the asks CH1-A1…A13 (§11), restated against the shipped C2 code. Once the manager rules on the SB4 erratum (E-SB4-2), C5/C6 can absorb them: about 2 SB4 days, mostly CH1-A1, CH1-A5, CH1-A11 and CH1-A12. CH1-A10 moves to the `TestBlockValidity` owner.
- SB5: stubs (`i:SB5`), and F3 extended to V's holds (CH1-B1–B3), before N3; CH1-B4 (LO-9 on pop/ack paths) with F3.
- CS3: `check_block`/`contextual_check_block` for N2's real checks. CS3 is waiting on RP-V24; until it lands, N2 tests with Core-recorded verdicts.
- CS5b: none direct (CH3a's).
- CH2a: none direct. CH2b: CH1-node defines the `FlushSite` seam and the index side; CH2b (M3, later) implements `FlushSite` and adds its row item "wire CH1-node's `FlushSite`, `prune_clear` and recovery steps 4/11".
- CH3a: implements `BlockConnector` (and depends on CH1-node, as PLAN has it).
- EV: `VContext` from EV0. If EV0 has not merged when N0 starts, the loop holds a placeholder that EV0's type replaces with no API change.
- **HX1** (the harness task, §11): N9b only.
- ND1: sets the no-wait marker on the tasks it spawns (LO-9) and owns `ValidationSignals`.

## 11. API changes requested

Labels are namespaced (rev3, OH-m6): **CH1-A*n*** are CH1-node's asks of SB4, **CH1-B*n*** its asks of SB5, and **X*n*** the items of harness task HX1. SB5's own asks of SB4 (its A11 `StepBudget` and A12 product-agnostic `hold_tip`) are **SB5-A11** and **SB5-A12** here. SB5's F3 row, which lists "SB4 A1, A11, A12", means CH1-A1, SB5-A11 and SB5-A12 (erratum E-SB5-2).

**SB4** (rev3: restated against the shipped code at C2 6ef0a8e75; none of these exists there yet). SB4 rev7.1 has not adopted them, so they need a manager ruling as an SB4 erratum before C5 starts (E-SB4-2).

| # | Change | Shipped at 6ef0a8e75 | Why |
|---|---|---|---|
| CH1-A1 | `hold_tip()` / `release_tip(hold) -> Arc<ChainReadSnapshot>`: a V-owned composite hold that generalizes `enforce_block`'s owned guard. Inside it, steps keep their own `seq` and per-step fanout (D43); `retry_pending` uses the held guard and pending drains complete before the next step (SB5 §3.3); `publish_status` is deferred to the release (CH1-A13); the pre/post handoff exclusion holds; `enforce_block` must not be entered (`debug_assert!`). SB5-A12 asks the same API product-agnostic | `TipCell.state` is already `Arc<Mutex<TipState>>` for `enforce_block`'s owned guard (`tip.rs`, "`lock_arc`"); additive | §2.5, Q1; needs the D28 erratum (E-SB4-1) and CH1-B1 |
| CH1-A2 | Writer-side `private_tip() -> Option<(BlockRef, u64)>` (`None` before genesis; the `u64` is the last assigned `seq`), `active_contains_private(id)`, `find_fork_private(id)`, `ancestor_private(h)`. The best-header hooks (`best_header_walk_back`, `best_header_invalid_chain`, shipped) must use the private active chain | new methods; the `Option` is new API, no shipped type changes | Core reads `m_chain` under `cs_main`; inside a hold the published snapshot lags (E4). The freeze takes `new` from here after the step (R1-B1) |
| CH1-A3 | `ids() -> impl Iterator<Item = BlockId>` over resident node entries | new; additive | Core's full-index scans (`v:3918`, `v:4028`, `v:4131`) and load |
| CH1-A4 | *Optional*: a borrowed, read-only writer-side `entry(id)` without seqlock retries | new | a convenience, added only if PF0 shows the retries cost |
| CH1-A5 | `record_chainlock(cl: ChainLockRef) -> ClOutcome`: `UpdateBestChainlock` plus the block lookup (`cl:106-132`, `h:166`), with no spork gate and no enforcement, returning the **shipped** `ClOutcome` with `Floor { id, enforced: None }` always | `apply_chainlock(cl, host) -> ClOutcome` fuses record and enforce; `ClOutcome { Stale, Pending, HeightMismatch, Floor { id, enforced } }` is reused unchanged. `apply_chainlock` stays SPV's composition | §5.1: Core's active-state check, record and enforcement run in different scopes |
| CH1-A6 | *Optional*: `cl_reset_gen() -> u64`, only if some SB4 path calls `check_cl_active_state` internally without surfacing `TurnedOnReset` | `check_cl_active_state() -> ClActiveState { Unchanged, TurnedOff, TurnedOnReset }` is shipped and suffices when V is its only caller | §5.3's reset row (`h:216-220`) |
| CH1-A7 | `EnforceReport.floor: BlockId`; `enforce_best_chainlock` returns `Some` for an enforcement that ran and changed nothing, `None` only when skipped (`h:275`, `h:284`) | `EnforceReport { marked, disconnected, reset }` and `-> Option<EnforceReport>` shipped; add one field | §5.2's captured pair |
| CH1-A8 | `is_ibd_private(now, loading)`: the shipped shared latch evaluated on V's private tip | `ChainReader::is_initial_block_download` shipped (`reader.rs:105`); additive | `UpdatedBlockTip`'s `fInitialDownload`, fork warnings, `NewPoWValidBlock` and `BlockChecked`'s argument are evaluated under `cs_main` |
| CH1-A9 | `check_index_with(&mut dyn IndexVisitor)`: one DFS with Core's shared `pindexFirst*` state per node, read-only | `check_index() -> Result<(), IndexInconsistency>` shipped; add the visitor form | §4.10 |
| CH1-A10 | **Moved** (rev3, OH-m6) to the `TestBlockValidity` owner (CH3a/CH7a): a non-inserting `check_header_at(prev, &header, now)`. CH1-node only routes the command | — | `v:4934-4963` |
| CH1-A11 | **Node-only pre-genesis state without changing shipped types** (rev3). `TipHost::tip -> BlockId`, `TipFacts.tip: BlockId`, `TipFacts.tip_height: u32`, `ActiveChainView::tip() -> BlockId` and `TipState.tip: BlockRef` **stay**. Additions: (a) a crate-private `TipState.pre_genesis: bool`. The node product's `new_genesis` and `load` leave it set until V calls `init_active_tip` (Core `LoadChainTip`, §7.3 step 11) or `connect_tip(genesis)`. While it is set, the active chain is empty and `TipFacts` names genesis as a placeholder with `tip_seq = 0`; (b) `ChainReadSnapshot::has_tip() -> bool`, `false` only in that state; (c) `connect_tip(genesis)` allowed from it, assigning `seq 1` and emitting `Connected(genesis, 1)`; (d) `private_tip()` (CH1-A2) returns `None` there. SPV's anchor constructor is unchanged | rev2's ask changed four shipped types to `Option`; dropped | Core's `ConnectTip` with a null tip (§7.3). The placeholder is never read: `TipHost` cannot run before genesis (no lock can arrive, D8), ND1 starts every reader after the first ABC (D8), and an SB5 `register` before genesis sees `has_tip() == false` |
| CH1-A12 | `accept_header` reports `floor_moved: Option<BlockId>` like `accept`; both report the ids they inserted, in order (for `AcceptedBlockHeader`); `floor_moved` is set per header, so also inside a batch that later stops, and `None` on a height mismatch (`cl:161-166`) | `HeadersResult.floor_moved` shipped (`batch.rs:34`) for `accept` only; `AcceptOne` carries neither | §5.3's mirror (R1-M1), §8.2 |
| CH1-A13 | `publish_status()` is a no-op inside a hold; the release publishes | `publish_status()` shipped; behaviour change only under CH1-A1 | §2.5, §4.12 |

**SB5** (rev1, DEC-153; rev3 adds CH1-B4):
- **CH1-B1.** `F3`'s composite-guard support extends from `enforce_block` to V's `hold_tip` holds: `retry_pending` with the held guard, the node writer rule unchanged (no step with `pending` non-empty), and pending drains between every node step.
- **CH1-B2** (rev3.1, as DEC-157 (2) rules). During any hold (V's `hold_tip` holds and `enforce_block`), `register` and `subscribe_at` return `HoldActive` with a release wake instead of waiting on the tip mutex, and `deregister`'s step 4 uses `try_lock` and defers the rest to lazy cleanup. CH1-A1's `release_tip` fires that wake after its snapshot store. The wake is notification-only (DEC-152 (2)). A slot whose `deregister` cleanup is deferred is retired from V's in-hold drain (H7), so V never waits on a deregistered subscriber's pop.
- **CH1-B3.** SB5 §3.3's in-hold wait answers `FlushDue`, `PruneClears` and W's wakes but not class R/M queries (E-SB5-1).
- **CH1-B4** (rev3, LO-9; H6's companion). A node subscriber's pop/ack path waits on nothing V does under a hold, **including any actor reply**: it never awaits `ActorReply`, `JoinHandle` or `signals.sync()`, and runs with the no-wait marker. SB5 states this beside H6, and its tests use the marked spawn.

rev0's SB5 asks (then unprefixed B1–B3: a non-blocking `ChainEvent` enqueue and synchronous listener registration on SB5) were withdrawn in rev1; those belong to `ValidationSignals` (§8.2).

**HX1: the harness task** (rev3, OH-M3; a new PLAN row for the harness owner, about **4 agent-days plus a Sol review**, gating only N9b). It extends `node/sb4-core-scenarios` (`harness.cpp`, scenario files, `regen.py`), whose seams today wrap only `VerifyChainLock` and `EnforceBlock`.

| Item | Content | Days | Acceptance |
|---|---|---|---|
| X0 | **Seam:** `--wrap` of `Chainstate::ConnectBlock` and `Chainstate::DisconnectBlock` (their mangled symbols), using the existing seams' technique | 0.5 | The existing reloc proof extended to both symbols: every call site resolves through the wrap, with no inlined copy. All 40 existing scenarios' outputs are byte-identical with the pass-through wrap and without it |
| X1 | **Transcript:** an ordered list, per run, of `(phase ∈ {connect, disconnect, check}, block hash, occurrence, result, state)` for every invocation (`check` = `fJustCheck` from `TestBlockValidity`), plus each submitted block's bytes. Acceptance-time verdicts (`CheckBlock`/`ContextualCheckBlock` in `AcceptBlock` with their `BlockChecked`) go in a separate list (R1-m4) | 1 | The invocation count equals a counter on Core's `ConnectTip` + `DisconnectTip` + `TestBlockValidity` calls in every scenario. `a18-invalid-chain-found` shows `cb-amount` failing at connect. A scenario where the same bytes fail by `HasConflictingChainLock` (`v:2516-2518`) and connect later shows occurrences 1 and 2 with different results |
| X2 | **Ops:** `pnb {force}`, `precious` and the invalid variant `cb-height` | 0.5 | Each op has a scenario that exercises it, judged by Core's own state (the tip changes for `precious`, an unrequested block is or is not stored, `bad-cb-height` is rejected) |
| X3 | **Dumps after every op:** `nSequenceId`, `nChainTx`, `setBlockIndexCandidates` in comparator order, `m_blocks_unlinked` in multimap order, unprojected `nStatus`, `bestChainLock`, `bestChainLockHash` and `bestChainLockWithKnownBlock` | 1 | Two runs produce identical dumps, except heap-address ties between loaded entries, which are flagged (D1) |
| X4 | **`regen.py --random N --seed S`:** a random operation generator, distinct from and combinable with the random callback-delivery policy, keeping X1's association | 0.75 | The same seed gives identical operation sequences and outputs; a 1,000-sequence smoke runs under `fuzzctl`; the MANIFEST checks and DEC-147's schedule recording still pass |
| X5 | **Fixture-op adapters**, documented per op (§9.2): what each op does in Core and which V command or test adapter reproduces it | 0.25 | Every op used by the 39 existing and 17 new scenarios is documented |

HX1 must land, reviewed, before N9b, which falls about CH1-node day 12.5 (§10.2). Started by CH1-node day 6, it stays off the critical path. Until then N9a covers the existing scenarios through `ScenarioConnector` (§8.1).

## 12. Errata proposed to other documents

| ID | Document | Change |
|---|---|---|
| E-A1-1 | A1 §2.10 | `ChainCommand` is §2.2's enum. `EnforceChainLock` becomes `ClIntake` + `RecordChainLock` + `ClEnforce`; `PruneTo` folds into `Flush`; `ReconsiderBlock` gains `ignore_chainlocks`; adds `Query`, `OnV`, `VerifyDb`, `IsConflictInvalidate`, `SetLoading` |
| E-A1-2 | A1 §2.10 `ChainView` | `ibd` is a method over the latch (P2a R4); the struct is §6.1's |
| E-A1-3 | A1 §2.10 | the 100 ms IBD throttle on `ChainView` is withdrawn (Q5) |
| E-A1-4 | A1 §2.10 events | Core's `CValidationInterface` signals go through `ValidationSignals` (§8.2), not the tip feed; the "backpressure above 10 pending" rule is Core's `LimitValidationInterfaceQueue` at its two call sites (DEC-153) |
| E-P2a-1 | P2a §2.5.3 R6 | "between ABC steps" means Core's release points; the R6 test is §9.4's |
| E-SB4-1 | SB4 D28 | ABC's inner loop is one hold (Q1); `InvalidateBlock` is one hold per iteration plus the final scope |
| E-CH2-1 | CH2 §4.2 | the IF_NEEDED decision is at Core's site and the freeze follows the tip step (D3, Q3); every freeze captures `(private tip after the step, its seq)`, so a disconnect freeze names `parent(d)` (R1-B1), with the `new == coins.best_block() == evo.best_block()` check; a `FlushDue` between a connector commit and its step is latched; class R commands are not served inside a hold; the `-dbcrashratio` completion wait runs `retry_pending`; (rev3) `ChainStateFlushed` at an IF_NEEDED site carries the **pre-step** locator captured at Core's site (`v:3114`, §7.1), and is staged inside a hold (N1); W is a no-wait task (LO-9) |
| E-SB5-1 | SB5 §3.3 | the node writer rule's in-hold wait (V's holds and `enforce_block`) answers `FlushDue`, `PruneClears` and W's wakes, not queries; F3 covers V's `hold_tip` holds (CH1-B1) |
| E-SB4-2 | SB4 rev7.1 §11 asks (rev3, OH-m6) | adopt CH1-A1, A2, A3, A5, A7, A8, A9, A11, A12 and A13 as restated against C2 6ef0a8e75 (§11), with A4 and A6 optional and A10 moved to the `TestBlockValidity` owner; CH1-A11 no longer changes shipped tip types. **Needs the manager's ruling before C5** |
| E-SB5-2 | SB5 §11 asks, F3 row, H-list (rev3) | F3's "SB4 A1, A11, A12" reads CH1-A1, SB5-A11, SB5-A12; add CH1-B4 beside H6 (a node subscriber's pop/ack path awaits no actor reply, LO-9) |
| PLAN | CH1-node row | add: "the index-side control flow of `ActivateBestChain`, `ConnectTip`/`DisconnectTip`, `InvalidateBlock`, `ReconsiderBlock`, `PreciousBlock`, `AcceptBlock`/`ReceivedBlockTransactions` and load, behind `BlockConnector`/`MempoolSink`/`FlushSite` seams"; size L; acceptance adds §9.2's replays through real V and R6/R8; (rev3) commits N9a (no harness dependency) and N9b (after HX1); 25 agent-days, critical path about 15 days; N3 gated on SB4 C6 |
| PLAN | CH3b row | becomes "reorg integration: `MempoolSink` disconnect pool (CONS-25), CL-conflict persistence across restart (CONS-11), the real-block FT suites, the F01 restart case and the reorg-across-flush crash test"; size M |
| PLAN | CH2b row | add: "implement CH1-node's `FlushSite` with the post-step pair and the `FlushDue` latch; wire `prune_clear` and recovery steps 4/11; re-run CH1-node's freeze-labelling tests (§9.5) with the real W" |
| PLAN | ND1 row | add: "own and run `ValidationSignals` (CH1-node §8.2); set LO-9's no-wait marker on the callback thread, W and every F1 subscriber's pop/ack task (CH1-node §8.3); start readers only after the first ABC (D8)" |
| PLAN | **new row HX1** (rev3, OH-M3) | "Harness additions for CH1-node: the `ConnectBlock`/`DisconnectBlock` `--wrap` seam and X1–X5 of CH1-node §11 (transcript, ops, dumps, random operation generator, adapter docs)", on `node/sb4-core-scenarios`, owner: the harness owner; size about 4 agent-days (S/M), rev Sol; deps the harness at its current head; acceptance per §11's table; unblocks CH1-node N9b |
| PLAN | RP0, P2b, IX/LK1b and index rows (rev3, OH-M1) | add: "externally visible state derived from F1 events is served behind a `tip_seq` fence or through `Query`, never ahead of `getblockcount` (CH1-node §6.2)"; LK1b and every F1 subscriber also: "the pop/ack task awaits no actor reply (LO-9)" |
| PLAN | P2b row (rev3, OH-m2) | add: "`getdata(MSG_CLSIG, 0)` answers with the null `clsig` from `ClReader::by_hash`, as Core (`np:3041-3046`)" |

## 13. Declared deviations

None changes a healthy node's RPC answers, the log lines an FT asserts, or P2P behaviour, except where a row says so.

| # | Ours | Core | Visible when |
|---|---|---|---|
| D1 | candidate ties between `nSequenceId 0` entries broken by arena id (SB4 D9) | heap address | two equal-work data-bearing tips after a restart; Core's choice is not reproducible either |
| D2 | candidates hold one immutable key per block; a sequence change or a repeated insert **replaces** the key, and erase and prune remove the stored key (§3.3) | `std::set<CBlockIndex*>` ordered by mutable fields; `ReceivedBlockTransactions` can change a member's `nSequenceId` in place (`v:4286-4289`) | only when a member is re-keyed: duplicate unlinked pairs or a re-downloaded pruned block. Core's set is then mis-ordered (its behaviour is unspecified); ours keeps the member with its current key |
| D3 | IF_NEEDED: decision and CRITICAL wait at Core's site; the freeze right after the tip step | both at `v:3304`/`v:3457` | never: no coins, evo or index write lies between the two points (§7.1), and `ChainStateFlushed` keeps Core's pre-step locator and FIFO position because it is captured at the site (rev3, OH-m1) |
| D4 | class R commands wait out a hold | — (CH2 and SB5 §3.3 serve read-only queries in their waits) | an RPC during a CRITICAL wait or a fanout drain inside a reorg answers after the hold, as Core's would |
| D5 | `-checkblockindex` once per `ProcessHeaders` batch | after each header (`v:4737`) | only a debug check's timing |
| D6 | the CL handler's `Cleanup()` runs on LK1a's own timer | inside the 5 s and `UpdatedBlockTip` tasks (`h:69`, `h:202`) | timing of seen-map pruning only (it is time-gated in Core too) |
| D7 | `ProcessNewChainLock` runs as `ClIntake`, LK1a's checks, the EV verification route and `RecordChainLock`; enforcement is a separate `ClEnforce` | one `msghand` call and a scheduler task | — (other commands between them are Core schedules; §5.1) |
| D8 | on a fresh datadir, ND1 starts P2P and RPC after genesis is connected | `ThreadImport`'s ABC races `StartNode` | a `version` sent in the first milliseconds of a fresh node: start height −1 in Core, 0 here |
| D9 | no assumeutxo or background-chainstate code | `m_disabled`, snapshot-base filters, `MaybeCompleteSnapshotValidation` | never (NG-6; one chainstate) |
| D10 | statsd gauges and timings (`g_stats_client`) are not emitted | `v:3330-3338`, `v:3500-3506`, `v:3848`, `h:321` | only with `-statshost`, which ND1 refuses (CH2 §3.3) |
| D11 | `AcceptedBlockHeader` listeners run after SB4's `accept` returns, once per inserted header in order | inline per header inside `AcceptBlockHeader` (`v:4724`) | never: the listeners are synchronous on V, and SB4 applies the ChainLock half per header |
| D12 | `cl_on_updated_block_tip` runs inline where V emits `UpdatedBlockTip` (§5.2) | the ChainLock handler's `UpdatedBlockTip` is a **queued** callback (`h:185-207`) | never: inline is the schedule where the scheduler dispatched it at once, and it only sets a flag and queues `ClEnforce` (DEC-147; OH-n5) |
| D13 | a hold's queued callbacks, UI notifications and `TipWatch` are delivered after the release's publication; other threads' callbacks enqueued during the hold precede them (N1) | emitted under `cs_main`, interleaved with unsynchronized producers by enqueue time | never for a reader: Core's readers take `cs_main` first. The order against producers that do not hold `cs_main` is one of Core's schedules (§2.5) |
| D14 | a callback, F1 pop task or W never awaits an actor reply (LO-9); a listener reads `ChainView` instead of taking the chain lock | a callback may take `cs_main` and read chain state synchronously | never: by N1 the published view already contains every tip the listener was told about, which is what `cs_main` gave it |

Stricter invariants rejected (DEC-95/99/102): V never asserts that the tip contains the ChainLock floor (R7-O1), never refuses a target below it, and never "repairs" Core's `v:3983` condition or the `InvalidChainFound(front)` choice.

## 14. Open questions, with the decisions taken

| # | Question | Decision (recorded; work continues) |
|---|---|---|
| Q1 | Should ABC's inner loop be one tip-mutex hold, refining SB4 D28? | **Yes** (Sol r1 accepts). Core holds `cs_main` across it (`v:3763-3827`); per-step publication would show readers the fork point mid-reorg. With DEC-153's per-step fanout draining, subscribers and W keep progressing inside the hold; only snapshot readers' view, class R/M commands and handoffs wait, as under `cs_main` (§2.5's table). It costs nothing in IBD. Needs SB4 CH1-A1 and SB5 CH1-B1 |
| Q2 | What does R6 promise? | **Core's release points**, with Core's full-comparator predicate and early exits, and no fixed real-time bound (E-P2a-1, R1-m1) |
| Q3 | At the IF_NEEDED sites, which tip does the freeze name? | **V's private tip after the step, with that step's `seq`**, which equals the coins' best block: `(parent(d), q+1)` after a disconnect (R1-B1). Checked in every build. This keeps E4/D45 as written and avoids a `-dbcrashratio` deadlock. CH2b confirms in its design |
| Q4 | Who owns the index-side ABC/invalidate/reconsider/precious flow? | **CH1-node** (SB4 §1.2 already says so); CH3b keeps the integration and FTs; sizes L and M (§1.2, §12) |
| Q5 | Is the `ChainView` throttle needed? | **No**, while PF0's `chainstate-view-publish` shows < 1% of the per-block control-flow cost |
| Q6 | Record and enforce: one SB4 call or two? | **Two** (CH1-A5); SPV keeps the composition |
| Q7 | Can LK1a precheck off V? | **The precheck arithmetic yes, the active-state transition no** (R1-M3). `ClIntake` performs `CheckActiveState` on V before any early return and returns the height; rev0's `precheck_height(live)` is withdrawn (§5.1) |
| Q8 | Before genesis, what is the tip? | **None** for V's private tip (CH1-A2); SB4's published types keep their shipped non-`Option` form with a placeholder and `has_tip() == false` (CH1-A11, rev3); genesis connects through `ConnectTip`; services start after it (D8) |
| Q9 | Per-header `CheckBlockIndex` in `ProcessHeaders`? | **Per batch** (D5). Ask SB4 for a per-header hook only if a test needs it |
| Q10 | Who builds the harness additions? | **HX1, its own PLAN row** for the harness owner (rev3, OH-M3): the `ConnectBlock`/`DisconnectBlock` seam plus X1–X5 (rev2's H1–H5), about 4 agent-days and a Sol review. Only N9b needs it; N9a replays the existing scenarios through `ScenarioConnector` |
| Q11 | One command for IS conflicts? | **Yes**, `IsConflictInvalidate` (§5.6); the IS owner (IX/LK1b) confirms the shape |
| Q12 | Does CH8 need a hook for Core's prune-lock move-back (`v:3292-3301`)? | **No; V calls one itself** (D61): `FlushSite::move_prune_locks_back` in DisconnectTip, forwarded by CH2b's adapter to the hub's Core-side locks and `BCLog::PRUNE` line; CH2's LCA-evaluated bound still decides what is pruned (§4.5) |
| Q13 | May a `ValidationSignals` listener (or an F1 pop task, or W) await V? | **No reply at all** (rev3, LO-9, DEC-156): it posts, and forwards any reply it needs to an unmarked task; `ActorReply` refuses the await at runtime in every build. rev2's narrower rule (R/M-complete replies only) is withdrawn. `BlockReply::Accepted` remains for posters; every Core caller of `ProcessNewBlock` uses `Activated` (§2.2) |
| Q14 | (rev1) Where does `ValidationSignals` live? | **Defined in `dashd-chainstate::chain::signals`, owned and run by ND1** (DEC-153); MP1, LK1a, IS and governance enqueue through clones |
| Q15 | (rev3) Do the SB4 asks need a ruling? | **Yes, before SB4 C5** (E-SB4-2): restated against 6ef0a8e75, all additive except CH1-A1's hold, and CH1-A11 no longer changes shipped types. About 2 SB4 days |
| Q16 | (rev3) Should F1 fanout also wait for the release, for N1? | **No.** DEC-153 keeps it per step (subscribers and W must progress inside holds). The externally visible state derived from it is fenced by `tip_seq` instead (§6.2, errata to RP0/P2b/IX/index rows) |

## 15. Sol r1 closure (rev1)

Review: `reviews/CH1-node-design-r1-sol.md` at a73b0c25 (REWORK). DEC-153 rules R1-M6.

| Finding | Disposition | Core | Where |
|---|---|---|---|
| **R1-B1** a disconnect freeze labelled the parent's coins with the removed block | **Fixed.** Every freeze captures `(private tip after the step, that step's seq)`: `(parent(d), q+1)` after disconnecting `d`; `step.block()` is never `new`. Checked in every build: `new.hash == coins.best_block() == evo.best_block()` and `new == private_tip`. Tests: one-batch and split freezes after a disconnect, crash, reopen and replay, a synced subscriber's `FlushCommit` classification; a latched `FlushDue` | `v:3285`, `v:3304`, `v:3316`; `v:3092`, `coins.cpp:254-255` | §0 item 7; §4.4; §4.5; §7.1; §9.5; §9.7; CH1-A2; E-CH2-1; Q3 |
| **R1-M1** a late header never updated `best_known` | **Fixed.** SB4 reports `cl_late` per header from `accept` and `accept_header` (CH1-A12); V sets `best_known = best` before `scope_end` or a reply, also for a partial batch; a height mismatch keeps the old pair. Invariant M1 checks floor ⇔ `best_known` and their hash. Scenarios cover `pnbh`, `pnb` (accepted and rejected), partial batches, a prior lower known lock and the mismatch | `cl:152-172` | §4.9; §5.3; §9.2 `ch1-cl-late-header-*` |
| **R1-M2** enforcement notified with a newer command's signature | **Fixed.** `ClEnforce` captures `(floor, best_known)` once after `CheckActiveState` and uses the pair for `EnforceBlock`, the activation checks, the dedupe and `NotifyChainLock`; no "still the best lock" check is added. Directed test with a higher lock recorded at an ABC release point, on the same and another branch, and with a reset during the activation | `h:283`, `h:296-320` | §5.2; §9.4; CH1-A7 |
| **R1-M3** a rejected CLSIG skipped Core's active-state reset | **Fixed.** Every CLSIG (duplicate, stale, bad signature, coinbase) starts with class M `ClIntake`, which runs `CheckActiveState` on V and publishes before LK1a's seen check, precheck and verification can return. Quorum selection and `GetQuorum` stay EV route B, dispatched whole to V. The off-V height prediction is withdrawn | `h:136-164`, `h:209-222` | §0 item 6; §2.2; §5.1; §9.2 `ch1-cl-intake-reset`; §9.4; CH1-A5; Q7 |
| **R1-M4** an `AcceptBlock` failure left the published best header and floor stale | **Fixed.** `scope_end()` (publish status, reconcile the mirror, publish `ChainView`) runs on every non-fatal exit of a class M scope before the reply; it emits no signal, so Core's omission of `NotifyHeaderTip` on a failed `ProcessNewBlock` stays. Contextual-rejection scenario with a pending lock resolved by the accepted header | `v:4580-4585`, `v:4777`, `v:4819-4824`, `v:4887-4893` | §4.9; §4.12; §9.2 `ch1-accept-contextual-reject` |
| **R1-M5** cached candidate keys had no replacement semantics | **Fixed.** One live key per block (invariant C1): insert replaces, every sequence write goes through `set_sequence_id` (remove, write, re-insert), erase and prune remove the map entry. Core's duplicate queue visits and sequence increments stay. D2 restated. Directed and property tests check C1 and termination | `v:3584-3593`, `v:4286-4297`, `v:3873-3883`, `bs:149-156` | §3.1; §3.3; §4.7; §4.8; §4.9; §9.3; D2 |
| **R1-M6** Core's callback queue and SB5's tip feed had incompatible contracts | **Fixed per DEC-153.** F1 stays `TipStep`; V drains pending fanout between every node step inside its holds, with the held guard (CH1-B1). Core's callbacks go through `ValidationSignals` (§8.2): FIFO on its own thread, O(1) non-blocking enqueue under a hold (DEC-152), backpressure only at `LimitValidationInterfaceQueue` (`v:3716`, called at `v:3760`, `v:3940`), `SyncWithValidationInterfaceQueue` semantics via a barrier; ND1 owns it. Model and threaded test: Q = 1, two consecutive disconnects, a parked subscriber; closing and `-dbcrashratio` variants | `vi:115-174`, `vi:193-330`, `sch:176-180`, `v:3716-3722` | §0 item 10; §2.5; §2.6; §4.1; §4.4; §4.5; §8.2; §9.6; §11 SB5; E-A1-4; E-SB5-1; Q13; Q14 |
| **R1-m1** R6's work bound was stronger than Core's predicate | **Fixed.** R6 states the full-comparator inner-loop predicate (equal work can end the hold) and the early exits; no fixed real-time or per-block bound for invalid candidates. Tests add an equal-work precious reorg and a failed-candidate exit | `v:3777-3780`, `v:3791-3794`, `v:3805`, `v:3812`, `v:3873-3883` | §2.4; §9.4; Q2 |
| **R1-m2** reference and crash oracles rejected states Core allows | **Fixed.** The brute force checks only the non-active suffix and keeps an active tip over pruned ancestors; load equivalence applies all B1 batches to a retained image and compares with Core's load rules; crash recovery is judged against the durable prefix plus ordinary ABC, or DEC-148's fail-stop | `v:3537`, `bs:295-331`, `v:5313-5373` | §9.3; §9.7 |
| **R1-m3** a failed header batch emitted `NotifyHeaderTip` | **Fixed.** The accepted prefix is published; notification and the progress log only on full success; `last_notified_header` does not move on failure | `v:4733-4741`, `v:4747` | §4.9; §9.2 `ch1-headers-partial-fail` |
| **R1-m4** H1 recorded verdicts by hash | **Fixed.** H1 is an ordered per-invocation transcript (phase, hash, occurrence, result, state) with consumption assertions; acceptance verdicts are recorded separately for `BlockChecks`; H4 keeps the association | `v:2516-2518` | §8.1; §11 harness |
| Review notes beyond the findings | **Applied.** "Per profile" wording removed (RP-V24/DEC-82); SB4 index read locks placed in the lock table; LO-5 extended to registration `finish()` callbacks and wakers; Q1 re-justified against per-step servicing (§2.5's table), including W's direct ack channel; CH1-A4 and CH1-A6 marked optional, CH1-A7 reduced to `floor` plus `Some` for a no-op enforcement, CH1-A10 and CH1-A11 made concrete; N3/N5 gated on the SB4 APIs and SB5's extension, flush re-tested at CH2b; harness fixture ops via narrow test adapters, `reload` is a force-flushed restart, the schedule policy recorded | — | §1.2; §2.5; §2.6; §9.2; §10.2; §11 |

## 16. Sol r2 closure (rev2)

Review: `reviews/CH1-node-design-r2-sol.md` at 59d4fc42 (APPROVE-WITH-CHANGES; all r1 closures confirmed).

| Finding | Disposition | Core | Where |
|---|---|---|---|
| **R2-M1** LO-8 classified by command class, so a listener could await `ProcessBlock`'s full reply, whose ABC tail V defers while waiting for the queue | **Fixed.** LO-8 is stated by reply dependency: a listener may await only a reply V completes entirely through R/M release-point service. §8.3 classifies every reply; `ProcessBlock`'s `Activated` reply, reconsider, precious and all class C replies are forbidden, posting stays allowed, and `BlockReply::Accepted` is the distinct acceptance-only reply. `ChainHandle` refuses a C-dependent send from the signals thread in every build. §9.6 adds Sol's schedule as the negative control and `ProcessHeaders`/`Query` waits as positive controls | `v:3738`, `v:3754-3760`, `v:4899-4906`, `v:3864-3867` | §2.2; §2.6 LO-8; §8.2; §8.3; §9.6; Q13 |
| **R2-M2** a `VecDeque` push can reallocate and copy the backlog under L2s | **Fixed.** The queue is a linked FIFO: the producer allocates a one-node list before the lock and splices it with `LinkedList::append` under L2s; the consumer detaches the whole list with `mem::take` and frees nodes outside the lock; a segmented fixed-chunk queue is an allowed alternative, a growable contiguous buffer is not. `pending` counts queued and detached-but-not-started callbacks. Growth test under a paused consumer up to 1,048,576 entries with a counting allocator. SB5's slot queues checked: Q-reserved at `register`, never reallocated, `Pending` under the tip lock and bounded to one step, so no change | `sch:145-155`, `sch:173`, `sch:176-183`, `sch:196-200`, `v:3716-3722` | §8.2; §9.6; §10.2 N0s |

## 17. Opus high closure (rev3, final)

Review: `reviews/CH1-node-design-opus-high.md` at c29315b34 (APPROVE-WITH-CHANGES; Core fidelity confirmed on the five riskiest paths, r1/r2 closures hold). DEC-156 rules OH-M1 and OH-M2.

| Finding | Disposition | Core | Where |
|---|---|---|---|
| **OH-M1** queued callbacks, UI and `TipWatch` fired inside a hold, before the snapshot naming their tip was published | **Fixed per DEC-156 (2).** A hold's queued callbacks, including seams' (mempool removals, `ChainStateFlushed`), are staged in a V-local list and spliced in one O(1) L2s section at the release, after the snapshot and `ChainView` stores; `TipWatch` and the UI notifications fire after that. Normative invariant N1. Inline listeners get V-evaluated facts (`ibd`) as arguments and never read the view. F1 stays per step, and F1-derived visible state is fenced by `tip_seq`. `TipWatch` renamed correctly (GBT long-poll; `waitfor*` uses `latestblock`). Tests: §9.4's adversarial-schedule N1 tests, including `p2p_sendheaders` (`headers`, not `inv`) and `waitforblockheight` → `getblockcount`, loom on the release sequence, model property N1, FT ports `p2p_sendheaders`, `interface_zmq`, `rpc_blockchain`'s `waitfor*` | `v:3797-3826`, `v:3212-3216`, `np:2273-2308`, `np:6338`, `np:6425-6427`, `rpc:300-305`, `rpc:431` | §0 items 3, 10; §2.5 (N1); §2.6 LO-4; §4.1; §4.5; §4.6; §4.11; §4.12; §5.4; §6.2; §8.2; §9.4; §9.6; §9.8; §12; D13; Q16 |
| **OH-M2** waits through tasks and actors were outside LO-1…LO-8; a subscriber pop task awaiting a V reply deadlocked against the per-step drain, and a listener could reach `Activated` through another actor | **Fixed per DEC-156 (1).** New LO-9: nothing V waits on (F1 pop/ack tasks, the callback thread, W, shared EV-gate holders) awaits any actor reply, `JoinHandle` or `sync`; they post. Enforced in every build by the no-wait marker: `ActorReply`, the `spawn` wrapper's `JoinHandle` and `sync` refuse an await from a marked task. The marker is propagated by `spawn` and set by the gate's shared guard, and raw channels are linted out. The marker is a backstop; the model is the proof. LO-8 is now a special case, and rev2's allowance for R/M replies is withdrawn. LO-6 covers every actor. Model negative controls for cycle A (LK1b-style pop task awaiting `OnV`, Q = 1, 2-block reorg) and cycle B (listener → net actor → `Activated`), plus `sync` and `Flush` variants, each deadlocking with the check disabled and terminating with it; positive posting controls. SB5 ask CH1-B4 | `v:3738`, `v:3754-3760` | §0 item 10; §2.6 LO-6, LO-8, LO-9, "Why no cycle"; §8.2; §8.3; §9.6; §11 CH1-B4; §12; D14; Q13 |
| **OH-M3** N9 depended on unscheduled harness work and was underestimated | **Fixed.** N9 split: N9a (models, loom, properties, R6/R8, N1 tests, PF0, the 39 replays through the new `ScenarioConnector`, which computes verdicts from build ops and `has_conflicting_chainlock`) with no harness dependency, and N9b (17 `ch1-*` scenarios, `RecordedConnector` cross-check, X4 campaign) after HX1. HX1 specified as its own task: the `ConnectBlock`/`DisconnectBlock` `--wrap` seam plus X1–X5 (rev2's H1–H5), with acceptance criteria, about 4 agent-days and a Sol review. Re-estimated: 25 agent-days, N9a 4.5 + N9b 2, critical path about 15 days | `v:2516-2518` | §0 item 11; §8.1; §9.2; §9.3; §10.2; §11 HX1; §12 PLAN rows; Q10 |
| **OH-m1** `ChainStateFlushed` locator | **Fixed.** Captured at Core's site (pre-step tip) into `PendingFreeze`, staged at `freeze_after_step`, keeping Core's FIFO position; test §9.4 (e) | `v:3114` | §7.1; §8.2; D3; E-CH2-1 |
| **OH-m2** `by_hash(0)` | **Fixed.** `best_hash` is `Uint256::ZERO` when unset or reset; `by_hash(0)` returns the null sig (height −1), so `getdata(MSG_CLSIG, 0)` gets a `clsig`; directed test plus a dde-probe confirmation | `cl:98-104`, `cl:140-150`, `chainlock.h:25`, `np:3041-3046` | §5.3; §5.5; §6.3; §9.4 (f); P2b erratum |
| **OH-m3** `deferred_or_now` | **Fixed.** `ClEnforce { Recorded }` is pushed to the back of `deferred`; it runs after the reply, never inside the record command; test §9.4 (g) | `h:169-176` | §5.1 |
| **OH-m4** which `BlockReply` P2P uses | **Fixed.** Every Core caller of `ProcessNewBlock` (P2P, `submitblock`, `generate*`, import) uses `Activated`, normatively; `Accepted` has no Core caller | `v:4899-4906` | §2.2; §8.3 |
| **OH-m5** read errors in §7.3 vs §4.5/§4.6 | **Fixed.** §7.3 follows Core per site (ConnectTip abort, step abort after a failed disconnect, `InvalidateBlock` false with a valid state, mark/IS asserts); nothing clears data or marks invalid | `v:3259-3263`, `v:3410`, `v:3621`, `v:3966`, `v:4067`, `rpc:2017-2023`, `is:621-625` | §7.3 |
| **OH-m6** SB4/SB5 asks | **Fixed.** Asks restated against 6ef0a8e75 with a shipped-state column; CH1-A5 reuses the shipped `ClOutcome`, CH1-A12 the shipped `floor_moved`; CH1-A11 rebased to keep the shipped non-`Option` tip types (crate-private flag, `has_tip()`); CH1-A10 moved to the `TestBlockValidity` owner; labels namespaced (CH1-A*, CH1-B*, SB5-A*, X*); N3 gated on SB4 C6; ruling requested (E-SB4-2, Q15) | — | §10.2; §11; §12; Q8; Q15 |
| **OH-m7** brute force vs cached `pindexMostWork` | **Fixed.** Compared only at quiescence (`rx` and `deferred` empty, no composite), exempting stop, shutdown and system-error exits | `v:3774`, `v:3842` | §9.3 |
| **OH-m8** `PruneOneBlockFile` duplicates | **Fixed.** `prune_clear` erases every matching `(prev, id)` entry | `bs:150-156`, `v:5768-5803` | §7.2 |
| **OH-m9** crash gaps | **Fixed.** `genesis_then_crash` and `prune_clear_in_hold_then_crash` added; `ScanAndUnlinkAlreadyPrunedFiles` noted as CH8's slot in recovery | `v:5322`, `v:5362-5370`, `bs:412-427` | §7.3; §9.7 |
| **OH-n1** null-tip edges | **Fixed.** §4.1's inner predicate written out verbatim; `more_or_same` is `true` and `ActiveHeight()` −1 with a null tip; `AcceptBlock` line numbers corrected | `v:3812`, `v:4788`, `v:4794` | §4.1; §4.9 |
| **OH-n2** L6 under L3 | **Fixed.** LO-3 states the bounded I/O exception | — | §2.6 |
| **OH-n3** sequence overflow | **Fixed.** `checked_add`, abort on overflow | `v:4286` | §4.9 |
| **OH-n4** R6 test bound | **Fixed.** "at most one further block connect from arrival" | — | §9.4 |
| **OH-n5** inline `cl_on_updated_block_tip` | **Recorded** as D12 | `h:185-207` | §13 |

### 17.1 DEC-157 addendum (rev3.1)

DEC-157 (ae529e3de) comes from SB5's Opus-high review.

| Item | Disposition | Where |
|---|---|---|
| (4) Ask labels prefixed by the asking design | **Already applied in rev3.** The asks are `CH1-A1`…`CH1-A13` of SB4 and `CH1-B1`…`CH1-B4` of SB5. SB5's own asks are `SB5-A#`, and HX1's items are `X#`. rev3.1 also prefixes the remaining unprefixed references in §11 and §15. Bare `B1` elsewhere is CH2's index batch, and bare `H#` is SB4 §13.4's list | §11; §15 |
| (2) Registration never blocks on a hold | **Adopted.** CH1-B2 is restated: `register`/`subscribe_at` return `HoldActive` plus a release wake, which CH1-A1's `release_tip` fires; `deregister` step 4 uses `try_lock` and lazy cleanup, and a slot whose cleanup is deferred is retired from V's drain (H7). CH1-node's registration paths are checked as follows:<br>• V never registers.<br>• A marked registrant (a subscriber task re-subscribing, CH2b's driver under its hub lock, LK1b) selects on the release wake beside its feed duties rather than parking on it, an LO-9 consequence.<br>• Unmarked callers (ND1 init, RPC-driven index enable) may wait for the wake.<br>• `ValidationSignals::register`/`unregister` swap an `ArcSwap` list and take no L1, so they are unaffected.<br>• The lock-and-wait model gains `HoldActive` retries and deferred deregistration while V's drain waits on that slot, with the property that no registration caller parks on L1 during a hold.<br>• Test-support's subscriber harness (§9.5, §9.6, replays) retries on the wake. | §2.5 table; §2.6 L1 row, LO-9 and registration; §9.6; §11 CH1-B2 |
| (1) SPV-LO9 | No change for CH1-node: the node's LO-9 (§2.6) is DEC-156's, and the SPV twin is SB14's ask SB5-Y2 | — |
| (3) SPV overlay budget | Not CH1-node's (SPV only) | — |

