//! S4a on a real V: the production [`ChainExec`], the back end inside `NodeHelper`, and the
//! masternode-list hook, over a `find_tx` fake. The chain is pre-DIP3 regtest with superblocks
//! every 20 blocks from height 20, connected through `NoEvo`'s checks.
//!
//! V's own DMN-tip list is empty before DIP3, so a testkit list stands in for it: the commands
//! read it through [`ListExec`], which wraps [`ChainExec`] and changes nothing else, and the back
//! end and the hook read it through `DmnTip`. `NoEvo` raises no masternode-list change, so
//! [`ListHooks`] calls the listener where V's signal would, from the connect's and the undo's
//! `list_changed`.
//!
//! [`Cfg::dip3`] runs the same rig as a DIP3 chain instead: DIP0003 active and enforced from
//! height 2, [`ListHooks`] complete with every evo check passing, V's own list reads answered by
//! [`TipLists`] (a list naming its block), and a mempool, so `InvalidateBlock`'s adding drains
//! move the DMN tip. There the commands, the back end and the hooks read the production lists.

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::DirBuilderExt as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant};

use dash_chain::{ChainConfig, FailStop, HeaderChain, Services, StaticSporks};
use dash_clock::MockClock;
use dash_consensus::coins::CoinsView;
use dash_consensus::header::HeaderParams;
use dash_consensus::merkle::compute_merkle_root;
use dash_consensus::payments::MnRewardEra;
use dash_consensus::pow::{PowParams, check_proof_of_work};
use dash_consensus::primitives::canonical::TRANSACTION_COINBASE;
use dash_consensus::primitives::{
    Block, BlockHeader, Header, MerkleRoot, OutPoint, Transaction, Tx, TxHash, TxIn, TxOut, TxType,
};
use dash_consensus::script::opcodes::{OP_RESERVED, OP_TRUE};
use dash_consensus::script::{ScriptNum, push_data};
use dash_consensus::subsidy::{SubsidyParams, block_subsidy_inner, genesis_reward};
use dash_consensus::{Amount, BlockId, BlsCtx, TxRef, TxValidationError};
use dash_dml::creditpool::{CreditPoolInfo, PoolError, PoolState};
use dash_dml::testkit;
use dash_dml::{MnList, MnListDiff, StoreError};
use dash_governance::manager::{GovClock, Manager, MetaOp};
use dash_governance::object;
use dash_governance::tally::{ObjectState, outpoint_key};
use dash_governance::vote;
use dash_kv::MemoryEngine;
use dash_num::CompactTarget;
use dash_params::{ChainOption, ChainParamsBuilder, ChainSetup, LlmqType, Network, Release};
use dash_pkc::bls::BlsSigId;
use dash_primitives::{BlockHash, GovVote, QuorumHash, VoteOutcome, VoteSignal};
use dash_types::Numeric as _;
use dashd_chainstate::chain::{
    AbortPath, AttemptOrigin, BlockConnector, BlockState, ChainHandle, ChainViewHandle, ConnectCx,
    CoreChecks, DisconnectPool, EvoReadCx, EvoReads, EvoTarget, Fatal, GuardScope, HeldMempool,
    LocalShutdown, MempoolSink, NoEvoReads, NoFlush, NoInstantSend, NoUi, NodeBlockIndex,
    NodeJoinHandle, NullMempool, ReadAheadSetup, RecordingAbort, RemovalReason, ReorgCx,
    SignalsThread, Staged, VContext, VExit, VParts, ValidationInterface, ValidationSignals,
    ViewTarget, genesis_block, load_genesis_block, spawn,
};
use dashd_chainstate::checkqueue::CheckQueue;
use dashd_chainstate::coins::{CoinsBudget, CoinsCache, Ratio, Salt};
use dashd_chainstate::connect::{
    AttemptId, BlockAt, ClBest, ConnectorParts, CoreConnector, EvoDigest, EvoHooks, NoEvo,
    NoEvoAttempt, NodeHelper, SpecialTxFailure, SpecialTxRules, SuperblockSource,
};
use dashd_chainstate::sigcache::{CacheSizes, ScriptCaches, ScriptExecCache, SigCache};
use dashd_chainstate::store::{
    CHAINSTATE_SCHEMA, CoinsReadError, GlobalFailStop, GuardedEngine, GuardedFiles, NodeBlockFiles,
};
use dashd_store::flatfile::{BlockFiles, BlockFilesConfig, BlockFilesState, OsFs};

use crate::chain::ChainExec;
use crate::exec::fakes::{FakeEnv, RecordingSink};
use crate::exec::{GovChain, GovCommand, GovExec};
use crate::handle::GovernanceHandle;
use crate::helper::GovernanceBackend;
use crate::sync::DmnTip;
use crate::tests::{FakeIndex, clock, io_error, operator_key, proposal, three_masternodes, txid};
use crate::tip::GovernanceListener;

/// The superblock height the tests judge: regtest's cycle is 20.
const SB: u32 = 40;
const OUTPUT_VALUE: i64 = 50_000;

/// What V and the governance runtime did, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ev {
    /// A `ConnectBlock` pinned the view of this generation.
    Pin(Option<u64>),
    /// `IsSuperblockTriggered(h)` answered.
    Asked(u32, bool),
    /// The masternode-list hook ran for the block at this height.
    ListChanged(u32),
    /// A governance command, by posting order, began or ended on V.
    Enter(u64),
    Exit(u64),
    /// A collateral lookup.
    Read([u8; 32]),
    /// The masternode-list hook ran for the undo of the block at this height.
    Undone(u32),
    /// A command's `active_height(block)`: the answer, and the tip height then.
    Active(BlockHash, Option<u32>, u32),
    /// The cached height after a command, with [`Watch`].
    Cached(i32),
    /// With [`Watch`], at a list hook: the block of the list the hook reads, then the block of
    /// V's own DMN tip.
    HookList(BlockHash, BlockHash),
    /// With [`Watch`], at a command's start: the block of the list its `QueryChain` reads, then
    /// the block of V's own DMN tip.
    CommandList(BlockHash, BlockHash),
}

#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<Ev>>>);

impl Log {
    fn push(&self, ev: Ev) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).push(ev);
    }

    fn take(&self) -> Vec<Ev> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// Test steps keyed by block height, each run once.
#[derive(Clone, Default)]
struct Steps(Arc<Mutex<BTreeMap<u32, Step>>>);

type Step = Box<dyn FnOnce() + Send>;

impl Steps {
    fn at(&self, h: u32, step: impl FnOnce() + Send + 'static) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).insert(h, Box::new(step));
    }

    fn run(&self, h: u32) {
        let step = self.0.lock().unwrap_or_else(PoisonError::into_inner).remove(&h);
        if let Some(step) = step {
            step();
        }
    }
}

/// A slot a step on V fills for the test thread.
type Slot<T> = Arc<Mutex<Option<T>>>;

fn fill<T>(slot: &Slot<T>, value: T) {
    *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(value);
}

fn taken<T>(slot: &Slot<T>) -> T {
    slot.lock().unwrap_or_else(PoisonError::into_inner).take().expect("the step ran")
}

/// [`ChainExec`], with the testkit list in place of V's empty pre-DIP3 one (V's own with
/// [`Cfg::dip3`]) and each command logged.
struct ListExec {
    inner: ChainExec,
    list: Option<Arc<MnList>>,
    log: Log,
    next: AtomicU64,
    exited: Arc<AtomicU64>,
    watch: Option<Arc<Watch>>,
}

impl ListExec {
    /// The id the next posted command gets.
    fn next_id(&self) -> u64 {
        self.next.load(Ordering::SeqCst)
    }

    /// Whether every command posted so far has run.
    fn idle(&self) -> bool {
        self.exited.load(Ordering::SeqCst) + 1 == self.next_id()
    }
}

impl GovExec for ListExec {
    fn post(&self, command: GovCommand) {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let (list, log, watch) = (self.list.clone(), self.log.clone(), self.watch.clone());
        let exited = Arc::clone(&self.exited);
        self.inner.post(Box::new(move |chain| {
            log.push(Ev::Enter(id));
            if let Some(watch) = &watch {
                log.push(Ev::CommandList(*chain.dmn_list().block_hash(), watch.own_dmn_tip()));
            }
            command(&WithList {
                chain,
                list,
                log: log.clone(),
            });
            log.push(Ev::Exit(id));
            if let Some(watch) = &watch {
                log.push(Ev::Cached(watch.cached_height()));
            }
            exited.fetch_add(1, Ordering::SeqCst);
        }));
    }
}

struct WithList<'a> {
    chain: &'a dyn GovChain,
    list: Option<Arc<MnList>>,
    log: Log,
}

impl GovChain for WithList<'_> {
    fn active_height(&self, block: &BlockHash) -> Option<u32> {
        let height = self.chain.active_height(block);
        self.log.push(Ev::Active(*block, height, self.chain.tip_height()));
        height
    }

    fn tip_height(&self) -> u32 {
        self.chain.tip_height()
    }

    fn dmn_list(&self) -> Arc<MnList> {
        self.list.clone().unwrap_or_else(|| self.chain.dmn_list())
    }
}

/// What the DIP3 rig reads on V around each command and hook: V's own DMN tip, which
/// `evo_view().dmn_list()` reads, and the cached height.
struct Watch {
    signals: ValidationSignals,
    view: OnceLock<ChainViewHandle>,
    handle: OnceLock<Weak<GovernanceHandle<Exec>>>,
}

impl Watch {
    /// The block of V's own DMN tip.
    fn own_dmn_tip(&self) -> BlockHash {
        let view = self.view.get().expect("V spawned");
        self.signals.dmn_tip_now().and_then(|id| view.entry(id)).map_or(BlockHash::ZERO, |e| e.hash)
    }

    fn cached_height(&self) -> i32 {
        let handle = self.handle.get().and_then(Weak::upgrade).expect("the handle");
        handle.state_for_test().cached_height()
    }
}

/// The testkit list, which a test may swap.
struct FixedTip(Mutex<Arc<MnList>>);

impl FixedTip {
    fn set(&self, list: MnList) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Arc::new(list);
    }
}

impl DmnTip for FixedTip {
    fn dmn_tip_list(&self) -> Arc<MnList> {
        Arc::clone(&self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

type Exec = Arc<ListExec>;
type Handle = Arc<GovernanceHandle<Exec>>;
type Listener = Arc<GovernanceListener<Exec>>;

/// The back end as `NodeHelper` holds it, logging its pins and trigger answers.
struct Probe {
    inner: GovernanceBackend<Exec>,
    log: Log,
}

impl SuperblockSource for Probe {
    fn begin_block(&mut self) {
        self.inner.begin_block();
        self.log.push(Ev::Pin(self.inner.pinned_generation()));
    }

    fn gov_valid(&mut self) -> bool {
        self.inner.gov_valid()
    }

    fn sb_triggered(&mut self, h: u32) -> bool {
        let answer = self.inner.sb_triggered(h);
        self.log.push(Ev::Asked(h, answer));
        answer
    }

    fn sb_payments(&mut self, h: u32) -> Option<Vec<TxOut>> {
        self.inner.sb_payments(h)
    }
}

/// `NoEvo`, running the test's step for a height in the block's special-transaction pass (after
/// its pin, before its superblock answers), and raising the listener's masternode-list hook for
/// the connects and undos at `list_heights`, after the test's list step at a connect. With `DIP3`
/// every hook is complete, its evo checks pass, and V reads [`TipLists`].
struct ListHooks<const DIP3: bool> {
    /// Set once V has spawned, before any block connects.
    listener: Arc<OnceLock<Listener>>,
    list_heights: Arc<Mutex<BTreeSet<u32>>>,
    steps: Steps,
    list_steps: Steps,
    log: Log,
    at: u32,
    watch: Option<Arc<Watch>>,
    lists: TipLists,
}

impl ListHooks<false> {
    fn dip3(self) -> ListHooks<true> {
        ListHooks {
            listener: self.listener,
            list_heights: self.list_heights,
            steps: self.steps,
            list_steps: self.list_steps,
            log: self.log,
            at: self.at,
            watch: self.watch,
            lists: self.lists,
        }
    }
}

impl<const DIP3: bool> EvoHooks for ListHooks<DIP3> {
    const COMPLETE: bool = DIP3;

    type Facade = ();

    type Attempt<'v> = NoEvoAttempt<'v>;

    type ListUpdates = ();

    fn begin_attempt<'v>(
        &mut self,
        facade: &'v (),
        v: &'v mut VContext,
        cx: &'v ConnectCx<'_>,
        id: AttemptId,
        origin: AttemptOrigin,
        target: &ViewTarget<'_>,
    ) -> Result<NoEvoAttempt<'v>, Fatal> {
        NoEvo.begin_attempt(facade, v, cx, id, origin, target)
    }

    fn finish_attempt(&mut self, attempt: NoEvoAttempt<'_>) -> Result<(), Fatal> {
        NoEvo.finish_attempt(attempt)
    }

    fn bls_ctx(
        &mut self,
        attempt: &mut NoEvoAttempt<'_>,
        evo: &mut EvoTarget<'_>,
        params: &ChainSetup,
        at: &BlockAt,
    ) -> BlsCtx {
        NoEvo.bls_ctx(attempt, evo, params, at)
    }

    fn commit_bls_scheme(&mut self, _: &mut NoEvoAttempt<'_>, _: &mut EvoTarget<'_>) {}

    fn verify_best_block(
        &mut self,
        attempt: &mut NoEvoAttempt<'_>,
        evo: &mut EvoTarget<'_>,
        hash: &BlockHash,
    ) -> Result<bool, Fatal> {
        NoEvo.verify_best_block(attempt, evo, hash)
    }

    fn process_special_txs(
        &mut self,
        attempt: &mut NoEvoAttempt<'_>,
        evo: &mut EvoTarget<'_>,
        block: &Block,
        at: &BlockAt,
        rules: SpecialTxRules,
        coins: &dyn CoinsView<Error = CoinsReadError>,
        fees: Amount,
        just_check: bool,
        check_cbtx_merkle_roots: bool,
        cl_best: &mut ClBest<'_>,
    ) -> Result<Result<(), SpecialTxFailure>, Fatal> {
        self.at = at.height;
        self.steps.run(at.height);
        if DIP3 {
            return Ok(Ok(()));
        }
        NoEvo.process_special_txs(
            attempt,
            evo,
            block,
            at,
            rules,
            coins,
            fees,
            just_check,
            check_cbtx_merkle_roots,
            cl_best,
        )
    }

    fn mn_payment_valid(
        &mut self,
        attempt: &mut NoEvoAttempt<'_>,
        evo: &mut EvoTarget<'_>,
        tx: &Tx,
        prev: Option<BlockId>,
        reward: Amount,
        fees: Amount,
        era: MnRewardEra,
        just_check: bool,
    ) -> Result<bool, Fatal> {
        if DIP3 {
            return Ok(true);
        }
        NoEvo.mn_payment_valid(attempt, evo, tx, prev, reward, fees, era, just_check)
    }

    fn evo_reads(&self) -> &dyn EvoReads {
        if DIP3 {
            &self.lists
        } else {
            &NoEvoReads
        }
    }

    fn set_best_block(
        &mut self,
        attempt: &mut NoEvoAttempt<'_>,
        evo: &mut EvoTarget<'_>,
        hash: &BlockHash,
    ) -> Result<(), Fatal> {
        NoEvo.set_best_block(attempt, evo, hash)
    }

    fn list_changed(&mut self, _: &mut NoEvoAttempt<'_>, _: &(), undo: bool) {
        let listed =
            self.list_heights.lock().unwrap_or_else(PoisonError::into_inner).contains(&self.at);
        if !listed {
            return;
        }
        if undo {
            self.log.push(Ev::Undone(self.at));
        } else {
            self.list_steps.run(self.at);
            self.log.push(Ev::ListChanged(self.at));
        }
        if let Some(watch) = &self.watch {
            let read = watch.view.get().expect("V spawned").dmn_tip_list();
            self.log.push(Ev::HookList(*read.block_hash(), watch.own_dmn_tip()));
        }
        let old = Arc::new(MnList::default_pre_dip3());
        let listener = self.listener.get().expect("the listener is set before blocks connect");
        listener.notify_masternode_list_changed(undo, &old, &Arc::new(MnListDiff::default()));
    }

    fn shared_collateral_spends(
        &mut self,
        attempt: &mut NoEvoAttempt<'_>,
        tx: &Tx,
        coins: &dyn CoinsView<Error = CoinsReadError>,
    ) -> Result<Result<(), TxValidationError>, Fatal> {
        NoEvo.shared_collateral_spends(attempt, tx, coins)
    }

    fn undo_special_txs(
        &mut self,
        attempt: &mut NoEvoAttempt<'_>,
        evo: &mut EvoTarget<'_>,
        block: &Block,
        at: &BlockAt,
    ) -> Result<Option<()>, Fatal> {
        self.at = at.height;
        NoEvo.undo_special_txs(attempt, evo, block, at)
    }

    fn begin_tx(&mut self, _: &mut NoEvoAttempt<'_>, _: &mut EvoTarget<'_>) -> Result<(), Fatal> {
        Ok(())
    }

    fn commit_tx(&mut self, _: &mut EvoTarget<'_>) -> Result<(), Fatal> {
        Ok(())
    }

    fn rollback_tx(&mut self, _: &mut EvoTarget<'_>) {}

    fn digest_fields(
        &mut self,
        attempt: &mut NoEvoAttempt<'_>,
        evo: &mut EvoTarget<'_>,
        at: &BlockAt,
    ) -> Result<EvoDigest, Fatal> {
        NoEvo.digest_fields(attempt, evo, at)
    }
}

/// The first height whose list holds `testkit::numbered(3)` on the DIP3 rig.
const MN3_FROM: u32 = SB + 1;

/// `testkit::numbered(1..=n)`.
fn masternodes(n: u32) -> MnList {
    let mut list = crate::tests::empty_list();
    for i in 1..=n {
        list.insert_for_tests(testkit::numbered(i));
    }
    list
}

/// V's evo reads on the DIP3 rig: the list at a block names that block, and holds
/// `testkit::numbered(1..=3)` from [`MN3_FROM`], `numbered(1..=2)` below it. No credit pool,
/// quorum or ChainLock.
#[derive(Clone)]
struct TipLists {
    two: Arc<MnList>,
    three: Arc<MnList>,
}

impl TipLists {
    fn new() -> Self {
        Self {
            two: Arc::new(masternodes(2)),
            three: Arc::new(masternodes(3)),
        }
    }
}

impl EvoReads for TipLists {
    fn list_at(&self, cx: EvoReadCx<'_>, at: BlockId) -> Result<Arc<MnList>, StoreError> {
        let e = cx.reader.entry(at).ok_or_else(|| StoreError::read("no entry".to_owned()))?;
        let base = if e.height >= MN3_FROM {
            &self.three
        } else {
            &self.two
        };
        Ok(Arc::new(MnList::clone(base).with_block_hash(e.hash)))
    }

    fn credit_pool_after(
        &self,
        _: EvoReadCx<'_>,
        _: BlockId,
    ) -> Result<Option<Arc<PoolState>>, StoreError> {
        Ok(None)
    }

    fn credit_pool_info(
        &self,
        cx: EvoReadCx<'_>,
        at: BlockId,
        window_blocks: i32,
    ) -> Result<Result<CreditPoolInfo, PoolError>, StoreError> {
        NoEvoReads.credit_pool_info(cx, at, window_blocks)
    }

    fn has_mined_commitment(
        &self,
        _: EvoReadCx<'_>,
        _: LlmqType,
        _: &QuorumHash,
        _: BlockId,
    ) -> Result<bool, StoreError> {
        Ok(false)
    }

    fn coinbase_chainlock_at(
        &self,
        _: EvoReadCx<'_>,
        _: BlockId,
        _: BlsCtx,
    ) -> Result<Option<u32>, StoreError> {
        Ok(None)
    }
}

/// A mempool holding nothing: V's drains run, and an adding one moves the DMN tip first.
#[derive(Clone, Copy)]
struct EmptyPool;

impl MempoolSink for EmptyPool {
    fn new_disconnect_pool(&mut self) -> Box<dyn DisconnectPool> {
        Box::new(())
    }

    fn begin_hold(&mut self) -> Option<Box<dyn HeldMempool>> {
        Some(Box::new(*self))
    }
}

impl HeldMempool for EmptyPool {
    fn dynamic_memory_usage(&self) -> u64 {
        0
    }

    fn remove_for_block_at(&mut self, _: &[TxRef], _: u32, _: &Arc<MnList>, _: &mut Staged) {}

    fn remove_expired_asset_unlock(&mut self, _: u32, _: &mut Staged) {}

    fn remove_recursive(&mut self, _: &TxRef, _: RemovalReason, _: &mut Staged) {}

    fn add_transactions_updated(&mut self, _: u32, _: &mut Staged) {}

    fn check(&mut self, _: u32, _: &mut Staged) {}

    fn maybe_update_for_reorg_at(
        &mut self,
        _: Box<dyn DisconnectPool>,
        _: Option<&Arc<MnList>>,
        _: &mut dyn ReorgCx,
        _: &mut Staged,
    ) -> Result<(), Fatal> {
        Ok(())
    }
}

/// Each queued `UpdatedBlockTip`, taken after the governance listener's.
#[derive(Default)]
struct TipProbe(Mutex<Vec<(BlockId, Option<BlockId>, bool)>>);

impl TipProbe {
    fn len(&self) -> usize {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    fn saw(&self, tip: BlockId) -> bool {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).iter().any(|(new, ..)| *new == tip)
    }

    fn take(&self) -> Vec<(BlockId, Option<BlockId>, bool)> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

impl ValidationInterface for TipProbe {
    fn updated_block_tip(&self, new: BlockId, fork: Option<BlockId>, ibd: bool) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).push((new, fork, ibd));
    }
}

/// Polls `done` until it holds, for at most a minute.
fn wait_until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting: {what}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

pub(crate) struct NoReads(pub(crate) Arc<RecordingAbort>);

impl FailStop for NoReads {
    fn read_failed(&self, what: &str) {
        dashd_chainstate::chain::AbortSink::read_failed(&*self.0, what);
    }

    fn is_tripped(&self) -> bool {
        false
    }
}

/// Regtest without DIP0003 (or with it from height 2, `dip3`), with budgets from height 10 and
/// superblocks from height 20.
fn chain_params(dip3: bool) -> Arc<ChainSetup> {
    let never = 10_000_000;
    let dip3_from = if dip3 {
        2
    } else {
        never
    };
    let params = ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
        .arg(ChainOption::Dip3Params, format!("{dip3_from}:{dip3_from}"))
        .arg(ChainOption::TestActivationHeight, format!("v20@{never}"))
        .arg(ChainOption::TestActivationHeight, format!("mn_rr@{never}"))
        .arg(ChainOption::BudgetParams, "10:10:20")
        .build()
        .expect("regtest's parameters");
    Arc::new(params)
}

// Core: script/script.h:427 CScript::push_int64
fn height_push(n: u32) -> Vec<u8> {
    let mut out = Vec::new();
    match n {
        1..=16 => out.push(OP_RESERVED + n as u8),
        _ => push_data(&mut out, &ScriptNum::new(i64::from(n)).encode()),
    }
    out
}

/// A coinbase-only block on `parent` at `h` paying `value`, `tag` in its script.
fn block_on(
    pow: &PowParams,
    parent: &Block,
    h: u32,
    tag: u8,
    value: i64,
    cbtx: bool,
) -> Arc<Block> {
    let outputs = vec![TxOut::from_duffs(value, vec![OP_TRUE].into())];
    block_paying(pow, parent, h, tag, outputs, cbtx)
}

/// A coinbase-only block on `parent` at `h` paying `outputs`, `tag` in its script; with `cbtx`,
/// the coinbase is a special transaction of the CbTx type, as DIP0003 requires.
fn block_paying(
    pow: &PowParams,
    parent: &Block,
    h: u32,
    tag: u8,
    outputs: Vec<TxOut>,
    cbtx: bool,
) -> Arc<Block> {
    let mut script_sig = height_push(h);
    script_sig.push(tag);
    let (version, tx_type, payload) = if cbtx {
        (3, TxType::new(TRANSACTION_COINBASE), vec![0; 4])
    } else {
        (2, TxType::new(0), Vec::new())
    };
    let coinbase = Arc::new(Tx::new(Transaction::new(
        version,
        tx_type,
        vec![TxIn::new(
            OutPoint::new(TxHash::from([0; 32]), u32::MAX),
            script_sig.into(),
            u32::MAX,
        )],
        outputs,
        0,
        payload,
    )));
    let txs: Vec<TxRef> = vec![coinbase];
    let prev = parent.header().raw();
    let (root, _) = compute_merkle_root(txs.iter().map(|tx| *tx.txid().as_bytes()).collect());
    let mut raw = BlockHeader::new(
        0x2000_0000,
        parent.hash(),
        MerkleRoot::from_lendian(root),
        prev.time() + 150,
        prev.bits(),
        0,
    );
    let target = CompactTarget::new(raw.bits());
    let header = loop {
        let header = Header::new(raw);
        if check_proof_of_work(header.hash().as_bytes(), target, pow) {
            break header;
        }
        raw.set_nonce(raw.nonce() + 1);
    };
    Arc::new(Block::new(header, txs))
}

/// How a [`Rig`] starts.
#[derive(Clone, Copy)]
struct Cfg {
    loaded: bool,
    /// The listener takes V's queued and init tip signals, from the start height on.
    register: bool,
    /// The DIP3 chain (module doc).
    dip3: bool,
    /// The tip work runs at the start height, as on a running node; without it the cached
    /// height is a restart's 0.
    tip_at_start: bool,
}

impl Default for Cfg {
    fn default() -> Self {
        Self {
            loaded: true,
            register: false,
            dip3: false,
            tip_at_start: true,
        }
    }
}

/// One V with the governance runtime around it.
struct Rig {
    p: Arc<ChainSetup>,
    dip3: bool,
    pow: PowParams,
    abort: Arc<RecordingAbort>,
    clock: Arc<MockClock>,
    v: ChainHandle,
    join: Option<NodeJoinHandle<VExit>>,
    _signals: SignalsThread,
    _dir: tempfile::TempDir,
    handle: Handle,
    exec: Exec,
    env: Arc<FakeEnv>,
    sink: Arc<RecordingSink>,
    index: Arc<FakeIndex>,
    log: Log,
    steps: Steps,
    list_steps: Steps,
    list_heights: Arc<Mutex<BTreeSet<u32>>>,
    /// Collateral lookups made off V or outside the governance lock.
    misplaced: Arc<Mutex<Vec<String>>>,
    /// The active chain's blocks by height, genesis first.
    chain: Vec<Arc<Block>>,
    signals: ValidationSignals,
    listener: Listener,
    probe: Arc<TipProbe>,
    /// The list the pre-DIP3 rig's back end and hooks read.
    fixed: Arc<FixedTip>,
}

impl Rig {
    /// V at height `SB - 2`, governance loaded or not, its cached height at the tip.
    fn start(loaded: bool) -> Rig {
        Rig::with(Cfg {
            loaded,
            ..Cfg::default()
        })
    }

    /// V at height `SB - 2` as `cfg` says.
    fn with(cfg: Cfg) -> Rig {
        let p = chain_params(cfg.dip3);
        let params = dash_governance::GovParams::from_chain_params(&p).expect("its parameters");
        let pow = *HeaderParams::from_chain_params(&p).expect("header rules").pow();
        let abort = Arc::new(RecordingAbort::default());
        let signals = ValidationSignals::default();
        let thread = signals.start(abort.clone()).expect("the signals thread");
        let log = Log::default();
        let list = Arc::new(three_masternodes());
        let watch = cfg.dip3.then(|| {
            Arc::new(Watch {
                signals: signals.clone(),
                view: OnceLock::new(),
                handle: OnceLock::new(),
            })
        });
        let exec = Arc::new(ListExec {
            inner: ChainExec::default(),
            list: (!cfg.dip3).then(|| Arc::clone(&list)),
            log: log.clone(),
            next: AtomicU64::new(1),
            exited: Arc::default(),
            watch: watch.clone(),
        });
        let env = Arc::new(FakeEnv::new(clock()));
        let sink = Arc::new(RecordingSink::default());
        let index = Arc::new(FakeIndex::default());
        let mut manager = Manager::new();
        manager.set_loaded(cfg.loaded);
        let handle = GovernanceHandle::new(
            manager,
            Arc::clone(&exec),
            env.clone(),
            sink.clone(),
            index.clone(),
            params,
        );
        if let Some(watch) = &watch {
            assert!(watch.handle.set(Arc::downgrade(&handle)).is_ok());
        }
        let misplaced = Arc::new(Mutex::new(Vec::new()));
        index.set_probe({
            let (log, misplaced, weak) = (log.clone(), misplaced.clone(), Arc::downgrade(&handle));
            move |key| {
                log.push(Ev::Read(key));
                let on_v = std::thread::current().name() == Some("dashd-v");
                let locked = Weak::upgrade(&weak).is_some_and(|h| h.lock_is_held());
                if !on_v || !locked {
                    let mut m = misplaced.lock().unwrap_or_else(PoisonError::into_inner);
                    m.push(format!("{key:02x?}: on V {on_v}, governance lock {locked}"));
                }
            }
        });
        let fixed = Arc::new(FixedTip(Mutex::new(list)));
        let dmn: Arc<dyn DmnTip> = if cfg.dip3 {
            Arc::new(exec.inner.clone())
        } else {
            fixed.clone()
        };
        let (steps, list_steps) = (Steps::default(), Steps::default());
        let probe = Probe {
            inner: GovernanceBackend::new(Arc::clone(&handle), dmn),
            log: log.clone(),
        };
        let list_heights = Arc::new(Mutex::new(BTreeSet::new()));
        let listener_slot = Arc::new(OnceLock::new());
        let hooks = ListHooks::<false> {
            listener: listener_slot.clone(),
            list_heights: list_heights.clone(),
            steps: steps.clone(),
            list_steps: list_steps.clone(),
            log: log.clone(),
            at: 0,
            watch: watch.clone(),
            lists: TipLists::new(),
        };
        let helper = NodeHelper::with_governance(Some(Box::new(probe)), Some(Arc::new(|| true)));

        let latch = GlobalFailStop::new();
        let dir = tempfile::tempdir().expect("a temporary directory");
        let blocks_dir = dir.path().join("blocks");
        std::fs::DirBuilder::new().mode(0o700).create(&blocks_dir).expect("the blocks directory");
        let config = BlockFilesConfig::new(blocks_dir, p.message_start);
        let files = BlockFiles::with_fs(Arc::new(OsFs::new()), config, BlockFilesState::default());
        let files = Arc::new(GuardedFiles::new(files, latch.clone()));
        let genesis = genesis_block(&p);
        let mock = Arc::new(MockClock::at(i64::from(genesis.header().raw().time())));
        let path = || AbortPath::new(abort.clone(), latch.clone());
        let svc = Services {
            clock: mock.clone(),
            sporks: Arc::new(StaticSporks::for_network(&p, Some(false))),
            fail: Arc::new(NoReads(abort.clone())),
        };
        let mut block_files = NodeBlockFiles::new(files.clone(), pow, Arc::new(path()));
        let checks = ChainConfig {
            consistency_checks: false,
            ..ChainConfig::node(&p)
        };
        let (chain, idx): (HeaderChain, NodeBlockIndex) =
            load_genesis_block(p.clone(), checks, svc, &mut block_files).expect("genesis loads");
        let engine = MemoryEngine::new(&CHAINSTATE_SCHEMA);
        let engine = Arc::new(GuardedEngine::new(engine, latch.clone(), abort.clone()));
        let budget = CoinsBudget::new(1 << 30, 0, Ratio::HALF, 0);
        let cache = CoinsCache::new(engine, budget, Salt::fixed(7), None);
        let sizes = CacheSizes::default();
        let caches = Arc::new(ScriptCaches {
            sig: SigCache::with_nonce(sizes.signature_cache_bytes, [0x5a; 32]).expect("sigcache"),
            exec: ScriptExecCache::with_nonce(sizes.script_execution_cache_bytes, [0xa5; 32])
                .expect("script execution cache"),
        });
        let parts = ConnectorParts {
            coins: Arc::new(Mutex::new(cache)),
            files: files.clone(),
            queue: Arc::new(CheckQueue::serial()),
            caches,
            helper: Box::new(helper),
            abort: path(),
            assume_valid: None,
            digest: None,
            xcheck: false,
        };
        let connector: Box<dyn BlockConnector> = if cfg.dip3 {
            Box::new(CoreConnector::new(&p, parts, hooks.dip3(), Arc::new(())).expect("connector"))
        } else {
            Box::new(CoreConnector::new(&p, parts, hooks, Arc::new(())).expect("connector"))
        };
        let (v, join) = spawn(VParts {
            params: p.clone(),
            chain,
            idx,
            checks: Box::new(CoreChecks::new(p.clone()).expect("regtest's checks")),
            files: Box::new(NodeBlockFiles::new(files, pow, Arc::new(path()))),
            read_ahead: ReadAheadSetup::off(),
            connector,
            facade: None,
            mempool: if cfg.dip3 {
                Box::new(EmptyPool)
            } else {
                Box::new(NullMempool)
            },
            flush: Box::new(NoFlush),
            is: Box::new(NoInstantSend),
            ui: Arc::new(NoUi),
            abort: abort.clone(),
            shutdown: Arc::new(LocalShutdown::default()),
            signals: signals.clone(),
            clock: mock.clone(),
            check_block_index: false,
            have_pruned: false,
            stop_at_height: 0,
            reindex: false,
            guard: GuardScope::of(&p),
        })
        .expect("V starts");
        let view = v.view_handle();
        if let Some(watch) = &watch {
            assert!(watch.view.set(view.clone()).is_ok());
        }
        let dmn: Arc<dyn DmnTip> = if cfg.dip3 {
            Arc::new(view.clone())
        } else {
            fixed.clone()
        };
        let listener = Arc::new(GovernanceListener::new(Arc::clone(&handle), dmn, view));
        assert!(listener_slot.set(Arc::clone(&listener)).is_ok());
        let probe = Arc::new(TipProbe::default());
        let probe_dyn: Arc<dyn ValidationInterface> = probe.clone();
        signals.register(probe_dyn.clone());
        v.activate_best_chain().recv().expect("V").expect("the startup ABC");
        assert!(exec.inner.bind(v.clone()), "bound once");
        let mut rig = Rig {
            p,
            dip3: cfg.dip3,
            pow,
            abort,
            clock: mock,
            v,
            join: Some(join),
            _signals: thread,
            _dir: dir,
            handle,
            exec,
            env,
            sink,
            index,
            log,
            steps,
            list_steps,
            list_heights,
            misplaced,
            chain: vec![genesis],
            signals,
            listener,
            probe,
            fixed,
        };
        while rig.height() < SB - 2 {
            rig.extend(0, OUTPUT_VALUE);
        }
        // The start's queued tip signals are dispatched before the listener joins, so none
        // reaches it; it then comes before the probe, which sees each signal after it.
        let tip = rig.id(rig.height());
        wait_until("the start's tip signals", || rig.probe.saw(tip));
        rig.signals.unregister(&probe_dyn);
        rig.probe.take();
        if cfg.tip_at_start {
            rig.handle.tip(rig.height() as i32).wait().expect("ran").result.expect("tip");
        }
        if cfg.register {
            rig.signals.register(rig.listener.clone());
            rig.signals.register(probe_dyn);
        }
        rig.sink.take();
        rig.log.take();
        rig
    }

    /// Waits until the listener has taken `n` more queued tip signals and V has run every
    /// command posted; those signals, as heights.
    fn settle(&self, n: usize) -> Vec<(u32, Option<u32>, bool)> {
        wait_until("the queued tip signals", || self.probe.len() >= n);
        wait_until("V runs the posted commands", || self.exec.idle());
        let height = |id| self.v.view_handle().entry(id).expect("indexed").height;
        let signals: Vec<_> = self
            .probe
            .take()
            .into_iter()
            .map(|(new, fork, ibd)| (height(new), fork.map(height), ibd))
            .collect();
        assert_eq!(signals.len(), n, "{signals:?}");
        signals
    }

    /// The governance cached height.
    fn cached(&self) -> i32 {
        self.handle.state_for_test().cached_height()
    }

    /// The index id of the active block at `h`.
    fn id(&self, h: u32) -> BlockId {
        self.v.view_handle().lookup(&self.at(h).hash()).expect("indexed")
    }

    fn height(&self) -> u32 {
        self.chain.len() as u32 - 1
    }

    fn at(&self, h: u32) -> &Arc<Block> {
        &self.chain[h as usize]
    }

    /// A block on `parent` at `h`.
    fn on(&self, parent: &Block, h: u32, tag: u8, value: i64) -> Arc<Block> {
        block_on(&self.pow, parent, h, tag, value, self.dip3)
    }

    /// A block on the tip, connected.
    fn extend(&mut self, tag: u8, value: i64) -> Arc<Block> {
        let block = self.on(self.at(self.height()), self.height() + 1, tag, value);
        self.connect(&block);
        self.chain.push(block.clone());
        block
    }

    /// `ProcessNewBlock` with its ABC: whether the block was accepted, `None` once V stopped.
    fn submit(&self, block: &Arc<Block>) -> Option<bool> {
        self.clock.set_mocktime(i64::from(block.header().raw().time()));
        let result = self.v.process_block(block.clone(), true, None, true).recv().ok()?;
        Some(result.accepted)
    }

    fn connect(&self, block: &Arc<Block>) {
        assert_eq!(self.submit(block), Some(true), "{}", block.hash());
        assert_eq!(self.tip(), block.hash(), "{:?}", self.abort.messages());
    }

    fn tip(&self) -> BlockHash {
        self.v.view().snap.tip.tip_hash
    }

    /// A block at `SB` on `parent` paying the miner `OUTPUT_VALUE`, then `payments`.
    fn superblock_on(&self, parent: &Block, tag: u8, payments: &[TxOut]) -> Arc<Block> {
        let miner = TxOut::from_duffs(OUTPUT_VALUE, vec![OP_TRUE].into());
        let outputs = std::iter::once(miner).chain(payments.iter().cloned()).collect();
        block_paying(&self.pow, parent, SB, tag, outputs, self.dip3)
    }

    /// The payments of the best superblock at `SB` in the published view, over the back end's
    /// list.
    fn best_payments(&self) -> Option<Vec<TxOut>> {
        let view = self.handle.published();
        let list = self.fixed.dmn_tip_list();
        let p = self.handle.params();
        view.superblocks.payments(&list, SB as i32, &view.objects, p)
    }

    /// `TestBlockValidity` on the tip: the reject reason, `None` when valid.
    fn verdict(&self, block: &Arc<Block>) -> Option<String> {
        match self.v.test_block_validity(block.clone(), false, true).recv().expect("V") {
            Ok(BlockState::Valid) => None,
            Ok(BlockState::Invalid(e)) => Some(e.reject_reason().to_owned()),
            other => panic!("{other:?}, want a verdict"),
        }
    }

    /// The trigger at `SB`, admitted on V, and its hash.
    fn admit_trigger(&self) -> [u8; 32] {
        let trigger = crate::tests::trigger(3, [9; 32], SB as i32);
        let admitted = self.handle.admit(trigger.clone()).wait().expect("ran");
        assert!(admitted.outcome.expect("admit").held);
        object::hash(&trigger)
    }

    /// The reward of the block at `SB`, which pays nothing else.
    fn reward(&self) -> Amount {
        let genesis = dash_consensus::params::genesis_block(&self.p);
        let base = genesis_reward(&genesis).expect("the genesis reward");
        let rule = SubsidyParams::from_chain_params(&self.p, base).expect("regtest's subsidy");
        let prev = self.at(SB - 1).header().raw();
        block_subsidy_inner(prev.bits(), (SB - 1) as i32, false, &rule).expect("a subsidy")
    }

    /// The live state of the object `hash`.
    fn live(&self, hash: &[u8; 32]) -> Option<ObjectState> {
        self.handle.state_for_test().object(hash).cloned()
    }

    fn list_at(&self, heights: &[u32]) {
        self.list_heights.lock().unwrap_or_else(PoisonError::into_inner).extend(heights);
    }

    fn assert_lock_order(&self) {
        let misplaced = self.misplaced.lock().unwrap_or_else(PoisonError::into_inner);
        assert!(misplaced.is_empty(), "{misplaced:?}");
    }

    /// Returns once V ran every command posted before this call, their exits logged: a command's
    /// reply goes out before its exit is.
    fn drain_v(&self) {
        let (tx, rx) = std::sync::mpsc::channel();
        self.exec.inner.post(Box::new(move |_| tx.send(()).expect("the barrier waits")));
        rx.recv().expect("V ran the barrier");
    }

    fn assert_running(&self) {
        assert!(self.abort.messages().is_empty(), "{:?}", self.abort.messages());
        assert!(!self.v.shutdown_requested());
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.v.shutdown();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Runs `f` on a new thread, as a peer handler or the scheduler would, and returns its answer.
fn off_v<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::spawn(f).join().expect("the poster")
}

/// A yes vote with `signal` on `parent` by `testkit::numbered(n)`, signed with its operator key.
fn trigger_vote(n: u32, parent: [u8; 32], signal: i32) -> GovVote {
    let dmn = testkit::numbered(n);
    let unsigned = |sig| {
        GovVote::new(
            dmn.collateral,
            TxHash::from_lendian(parent),
            VoteOutcome::new(vote::OUTCOME_YES as u32),
            VoteSignal::new(signal as u32),
            clock().now,
            sig,
        )
    };
    let hash = vote::signature_hash(&unsigned(Vec::new()));
    unsigned(operator_key(n).sign_with(&hash, BlsSigId::Basic).to_bytes().to_vec())
}

fn cast(handle: &Handle, n: u32, parent: [u8; 32], signal: i32) {
    let (outcome, _) = handle.process_vote(&trigger_vote(n, parent, signal), &three_masternodes());
    assert_eq!(outcome.result, Ok(true), "vote {n} signal {signal}");
}

fn position(log: &[Ev], ev: Ev) -> usize {
    log.iter().position(|e| *e == ev).unwrap_or_else(|| panic!("{ev:?} in {log:?}"))
}

fn pins(log: &[Ev]) -> Vec<Option<u64>> {
    log.iter()
        .filter_map(|e| match e {
            Ev::Pin(g) => Some(*g),
            _ => None,
        })
        .collect()
}

/// Whether the trigger's masternode has a rate record.
fn rate_record_held(handle: &Handle) -> bool {
    let key = outpoint_key(&testkit::numbered(3).collateral);
    handle.state_for_test().last_object_record(&key).is_some()
}

/// A trigger arriving while V connects a two-block batch is admitted at the release point after
/// the batch: both blocks pinned the view without it, and neither its object nor its rate record
/// existed while the second block asked for it.
#[test]
fn a_command_posted_in_a_batch_runs_at_the_release_point_after_it() {
    let mut rig = Rig::start(true);
    let a39 = rig.extend(0, OUTPUT_VALUE);
    let b39 = rig.on(rig.at(SB - 2), SB - 1, 1, OUTPUT_VALUE);
    let b40 = rig.on(&b39, SB, 1, OUTPUT_VALUE);
    assert_eq!(rig.submit(&b39), Some(true));
    assert_eq!(rig.tip(), a39.hash());
    rig.log.take();
    let before = rig.handle.published().generation;
    let trigger = crate::tests::trigger(3, [9; 32], SB as i32);
    let hash = object::hash(&trigger);
    let first = rig.exec.next_id();

    let replies = Slot::default();
    rig.steps.at(SB - 1, {
        let (handle, replies) = (Arc::clone(&rig.handle), replies.clone());
        move || fill(&replies, off_v(move || (handle.admit(trigger), handle.clean())))
    });
    let at_sb = Slot::default();
    rig.steps.at(SB, {
        let (handle, at_sb) = (Arc::clone(&rig.handle), at_sb.clone());
        move || {
            let held = handle.state_for_test().object(&hash).is_some();
            fill(&at_sb, (held, rate_record_held(&handle)));
        }
    });
    rig.connect(&b40);
    let (admit, clean) = taken(&replies);
    assert!(admit.wait().expect("ran").outcome.expect("admit").held);
    clean.wait().expect("ran");

    assert_eq!(taken(&at_sb), (false, false));
    let log = rig.log.take();
    let asked = position(&log, Ev::Asked(SB, false));
    let entered = position(&log, Ev::Enter(first));
    assert!(asked < entered && entered < position(&log, Ev::Enter(first + 1)), "{log:?}");
    assert_eq!(pins(&log[..entered]), vec![Some(before); 2], "{log:?}");
    assert!(rig.handle.published().superblocks.get(&hash).is_some());
    assert!(rate_record_held(&rig.handle));
    rig.assert_running();
}

/// The sentinel update of the block's superblock query, written back, decides the masternode-
/// list hook's clean inside the same connect: the trigger it marked for deletion is erased
/// there, with no command run.
#[test]
fn the_written_back_sentinel_decides_the_list_hook_clean() {
    let mut rig = Rig::start(true);
    rig.extend(0, OUTPUT_VALUE);
    let hash = rig.admit_trigger();
    cast(&rig.handle, 1, hash, vote::SIGNAL_DELETE);
    cast(&rig.handle, 2, hash, vote::SIGNAL_DELETE);
    assert!(rig.live(&hash).expect("held").dirty);
    rig.list_at(&[SB]);
    let commands = rig.exec.next_id();
    let later = clock().now + 600;
    rig.list_steps.at(SB, {
        let env = rig.env.clone();
        move || env.set(GovClock::new(later, later), true, true)
    });
    rig.sink.take();
    rig.extend(0, OUTPUT_VALUE);

    let log = rig.log.take();
    assert!(position(&log, Ev::Asked(SB, false)) < position(&log, Ev::ListChanged(SB)));
    assert_eq!(rig.exec.next_id(), commands);
    assert!(rig.live(&hash).is_none());
    assert!(rig.handle.published().superblocks.get(&hash).is_none());
    let [hook] = &rig.sink.take()[..] else {
        panic!("one submission, from the hook");
    };
    assert!(hook.meta.contains(&MetaOp::RemoveObject(hash)), "{:?}", hook.meta);
    rig.assert_running();
}

/// A block that fails its coinbase check after asking for the trigger keeps the sentinel update:
/// the trigger is marked for deletion at the query's time and no longer dirty, and the list hook
/// never ran.
#[test]
fn a_failed_block_keeps_its_sentinel_update() {
    let mut rig = Rig::start(true);
    let a39 = rig.extend(0, OUTPUT_VALUE);
    let hash = rig.admit_trigger();
    cast(&rig.handle, 1, hash, vote::SIGNAL_DELETE);
    cast(&rig.handle, 2, hash, vote::SIGNAL_DELETE);
    rig.list_at(&[SB]);
    let overpaid = rig.on(&a39, SB, 0, rig.reward() + 1);
    rig.submit(&overpaid);
    assert_eq!(rig.tip(), a39.hash());

    let log = rig.log.take();
    position(&log, Ev::Asked(SB, false));
    assert!(!log.contains(&Ev::ListChanged(SB)), "{log:?}");
    let live = rig.live(&hash).expect("held");
    assert!(live.cached_delete && !live.dirty);
    assert_eq!(live.deletion_time, clock().now);
    rig.assert_running();
}

/// A proposal arriving during the postponed pass on V runs after the pass, in its own command:
/// each collateral lookup is made on V, under the governance lock, inside its own command.
#[test]
fn q_during_the_postponed_pass_on_v_runs_after_it() {
    let mut rig = Rig::start(true);
    let pending: Vec<_> = (0..3).map(proposal).collect();
    for (obj, tx) in &pending {
        rig.index.put(tx, rig.at(SB - 6).hash());
        let admitted = rig.handle.admit(obj.clone()).wait().expect("ran");
        assert!(admitted.outcome.expect("admit").postponed);
    }
    rig.extend(0, OUTPUT_VALUE);
    let (q, q_tx) = proposal(9);
    rig.index.put(&q_tx, rig.at(SB - 10).hash());
    let (entered, release) = rig.index.hold(txid(&pending[1].0));
    rig.log.take();
    let first = rig.exec.next_id();

    let tip = std::thread::spawn({
        let (handle, h) = (Arc::clone(&rig.handle), rig.height() as i32);
        move || handle.tip(h).wait().expect("ran")
    });
    entered.recv_timeout(Duration::from_secs(60)).expect("the pass reached the held lookup");
    let q_key = txid(&q);
    let admit = off_v({
        let handle = Arc::clone(&rig.handle);
        move || handle.admit(q)
    });
    release.wait();
    let tip_done = tip.join().expect("tip");
    let q_done = admit.wait().expect("ran");
    tip_done.result.expect("tip");
    assert!(!tip_done.index_fault && !q_done.index_fault);
    assert!(q_done.outcome.expect("admit").held);

    rig.drain_v();
    let log = rig.log.take();
    let (t, a) = (first, first + 1);
    let reads = |from: Ev, to: Ev| -> BTreeSet<[u8; 32]> {
        log[position(&log, from)..position(&log, to)]
            .iter()
            .filter_map(|e| match e {
                Ev::Read(k) => Some(*k),
                _ => None,
            })
            .collect()
    };
    assert!(position(&log, Ev::Exit(t)) < position(&log, Ev::Enter(a)), "{log:?}");
    let p: BTreeSet<_> = pending.iter().map(|(o, _)| txid(o)).collect();
    assert_eq!(reads(Ev::Enter(t), Ev::Exit(t)), p);
    assert_eq!(reads(Ev::Enter(a), Ev::Exit(a)), BTreeSet::from([q_key]));
    let state = rig.handle.state_for_test();
    assert!(state.postponed().is_empty() && state.objects().len() == 4);
    drop(state);
    rig.assert_lock_order();
    rig.assert_running();
}

/// A vote cast between the two blocks of a batch is pinned by the second block only: it funds
/// the trigger the second asks for, so the second, which does not pay it, fails its value check;
/// V keeps running and goes back to the first-seen block at 39. The collateral lookup before it ran on V under the governance lock.
#[test]
fn a_vote_between_two_blocks_of_a_batch_is_pinned_by_the_next() {
    let mut rig = Rig::start(true);
    let (obj, tx) = proposal(1);
    rig.index.put(&tx, rig.at(SB - 10).hash());
    assert!(rig.handle.admit(obj.clone()).wait().expect("ran").outcome.expect("admit").held);
    let a39 = rig.extend(0, OUTPUT_VALUE);
    let hash = rig.admit_trigger();
    let b39 = rig.on(rig.at(SB - 2), SB - 1, 1, OUTPUT_VALUE);
    let b40 = rig.on(&b39, SB, 1, OUTPUT_VALUE);
    assert_eq!(rig.submit(&b39), Some(true));
    rig.log.take();
    let before = rig.handle.published().generation;
    rig.steps.at(SB - 1, {
        let handle = Arc::clone(&rig.handle);
        move || off_v(move || cast(&handle, 1, hash, vote::SIGNAL_FUNDING))
    });
    rig.submit(&b40);

    let log = rig.log.take();
    let pinned = pins(&log);
    assert_eq!(pinned.len(), 3, "B39, B40, then A39 again: {log:?}");
    assert_eq!(pinned[0], Some(before));
    assert!(pinned[1] > pinned[0], "{log:?}");
    position(&log, Ev::Asked(SB, true));
    assert_eq!(rig.tip(), a39.hash(), "{log:?}");
    rig.assert_running();
    assert_eq!(rig.index.read_count(&txid(&obj)), 1);
    rig.assert_lock_order();
}

/// A dry run's superblock query writes its sentinel update back, as Core's update of the shared
/// object stands; a trigger whose votes moved after the pin takes the flags and stays dirty.
#[test]
fn a_dry_run_writes_its_sentinel_update_back() {
    let mut rig = Rig::start(true);
    let a39 = rig.extend(0, OUTPUT_VALUE);
    let hash = rig.admit_trigger();
    cast(&rig.handle, 1, hash, vote::SIGNAL_FUNDING);
    assert!(rig.live(&hash).expect("held").dirty);
    let c40 = rig.on(&a39, SB, 2, OUTPUT_VALUE);
    assert_eq!(rig.verdict(&c40).as_deref(), Some("bad-cb-amount"));
    let log = rig.log.take();
    position(&log, Ev::Asked(SB, true));
    let live = rig.live(&hash).expect("held");
    assert!(live.cached_funding && !live.dirty);

    rig.steps.at(SB, {
        let handle = Arc::clone(&rig.handle);
        move || off_v(move || cast(&handle, 2, hash, vote::SIGNAL_VALID))
    });
    assert_eq!(rig.verdict(&c40).as_deref(), Some("bad-cb-amount"));
    let live = rig.live(&hash).expect("held");
    assert!(live.cached_funding && live.dirty);
    assert_eq!(rig.tip(), a39.hash());
    rig.assert_running();
}

/// A funded superblock gets Core's verdict on V, dry run and tip path alike, and V never stops:
/// paying the amount to another script or giving the miner more than the reward fails the value
/// check ("bad-cb-amount", which checks the payees too and comes first); paying the best
/// superblock's payments connects.
// Core: masternode/payments.cpp v24.0.2:378-386, :425-436
#[test]
fn a_funded_superblock_gets_cores_verdict_and_v_keeps_running() {
    let mut rig = Rig::start(true);
    let a39 = rig.extend(0, OUTPUT_VALUE);
    let hash = rig.admit_trigger();
    cast(&rig.handle, 1, hash, vote::SIGNAL_FUNDING);
    let payments = rig.best_payments().expect("a best superblock");
    assert!(!payments.is_empty());

    let elsewhere: Vec<_> =
        payments.iter().map(|o| TxOut::from_duffs(o.value(), vec![OP_TRUE].into())).collect();
    let mut over = payments.clone();
    over.push(TxOut::from_duffs(rig.reward(), vec![OP_TRUE].into()));
    for (tag, outputs) in [(1, elsewhere), (2, over)] {
        let block = rig.superblock_on(&a39, tag, &outputs);
        rig.submit(&block);
        assert_eq!(rig.tip(), a39.hash(), "{tag}");
        rig.assert_running();
        assert_eq!(rig.verdict(&block).as_deref(), Some("bad-cb-amount"), "{tag}");
    }

    let paid = rig.superblock_on(&a39, 3, &payments);
    assert_eq!(rig.verdict(&paid), None);
    rig.connect(&paid);
    let log = rig.log.take();
    assert!(log.contains(&Ev::Asked(SB, true)), "{log:?}");
    rig.assert_running();
}

/// DEC-1756 on V: a miss and an index fault both refuse the proposal, only the fault is reported
/// as ours, V keeps running, and neither is cached.
#[test]
fn a_collateral_miss_or_fault_on_v_is_not_cached() {
    for (fault, index_fault) in [(None, false), (Some(io_error()), true)] {
        let mut rig = Rig::start(true);
        let (obj, tx) = proposal(1);
        let key = txid(&obj);
        if let Some(e) = fault {
            rig.index.set(key, Err(e));
        }
        let refused = rig.handle.admit(obj.clone()).wait().expect("ran");
        let out = refused.outcome.expect("no overflow");
        assert!(!out.ok && !out.held && !out.postponed);
        assert_eq!(refused.index_fault, index_fault);
        rig.extend(0, OUTPUT_VALUE);
        rig.assert_running();

        rig.index.put(&tx, rig.at(SB - 10).hash());
        let admitted = rig.handle.admit(obj).wait().expect("ran");
        assert!(admitted.outcome.expect("admit").held && !admitted.index_fault);
        assert_eq!(rig.index.read_count(&key), 2);
        rig.assert_lock_order();
    }
}

/// Governance not loaded: `IsValid` is false, nothing triggers and the list hook does nothing.
#[test]
fn disabled_governance_triggers_nothing_and_the_hook_does_nothing() {
    let mut rig = Rig::start(false);
    let published = rig.handle.published();
    rig.list_at(&[SB - 1, SB]);
    rig.extend(0, OUTPUT_VALUE);
    rig.extend(0, OUTPUT_VALUE);
    let log = rig.log.take();
    assert_eq!(pins(&log), vec![Some(published.generation); 2]);
    assert!(log.contains(&Ev::ListChanged(SB)), "{log:?}");
    assert!(log.iter().all(|e| !matches!(e, Ev::Asked(_, true))), "{log:?}");
    assert!(rig.sink.take().is_empty());
    assert!(Arc::ptr_eq(&published, &rig.handle.published()));
    rig.assert_running();
}

mod hooks;
