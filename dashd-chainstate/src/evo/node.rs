//! `NodeEvo` (CH7 §2.1): the production [`EvoHooks`], H1–H11 at Core's sites over the evo store,
//! its layer and the caches, with D2's readiness stubs where CH7b's answers belong and EV1's
//! checks behind [`Ev1Checks`] (D44).
//!
//! CH7a-3 adds the MN-list signal (H7, D15), VerifyDB's scratch target (§6.3 step 5) and the
//! reads V publishes and serves ([`EvoReads`], D16, D17). CH7a-4 sets `COMPLETE`, so readiness
//! guard G1 is lifted: the hooks judge every block. CH7b-2 removes G1q (D3): H4 and H9 read the
//! members, the quorums and the precompute through the attempt's [`AttemptBinding`] (route C).

use std::cell::Cell;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::Instant;

use dash_chain::{ChainReader, EntryView, FlatFilePos, Status};
use dash_consensus::coins::CoinsView;
use dash_consensus::ctx::{Deployment, DeploymentView, bls_ctx_for_block};
use dash_consensus::payments::{MnRewardEra, PaymentParams};
use dash_consensus::primitives::canonical::TRANSACTION_COINBASE;
use dash_consensus::{
    Amount, Block, BlockId, BlockValidationError, BlockValidationResult, BlsCtx, ChainAncestry,
    ConsensusCtx, HeaderCtx, Tx, TxValidationError,
};
use dash_dml::blockprocessor::{
    BlockLookup, QcBlockAt, QcBlockError, QcCtx, QcHashCache, QcMode, quorum_root,
};
use dash_dml::creditpool::{
    BlockAt as PoolBlockAt, ConnectAt, CreditPool, CreditPoolInfo, PoolEras, PoolError, PoolState,
    check_block_diff, connect_records, credit_data_at, credit_pool_info, platform_reward,
};
use dash_dml::mnhf::{EhfSignals, GetForBlockError, MnhfError, MnhfManager, MnhfView};
use dash_dml::netinfo::{CoreNetInfoValidity, NetInfoConfig, NetInfoRules};
use dash_dml::payments::{BlockReward, masternode_payee_valid};
use dash_dml::specialtx::QuorumSeams;
use dash_dml::specialtx::qc::NodeCommitmentVerifier;
use dash_dml::specialtx::quorum::{QuorumKey, QuorumLookup, QuorumScan, SeamError};
use dash_dml::store::{CacheHint, ListCache, resolve_list, resolve_list_traced};
use dash_dml::{
    DmlBuildError, DmlParams, DmlRules, Dmn, EvoDelta, EvoStore, MemEvoStore, MnList, MnListChange,
    NoTrace, NodeMemberError, PayeeRules, PrevBlock, StoreError, build_new_list_from_block,
};
use dash_evo::cbtx::{
    CoinbaseFacts, check_mn_list_root, check_quorums_root, nonnull_coinbase_chainlock,
};
use dash_evo::llmq::{
    CompleteChain, LlmqCtx, LlmqError, LlmqSource as _, MemberError, MinedCommitmentReader,
    PrecomputeMode, QuorumMemberSource, QuorumMembers, QuorumPrecompute, ScanEvent, ScanOverload,
    SerialBlsChecks, is_quorum_rotation_enabled, scan_quorums,
};
use dash_evo::locks::{QuorumScan as LockScan, ScannedQuorum};
use dash_evo::{Effect, EffectKind, EffectSink};
use dash_num::CompactTarget;
use dash_params::LlmqType;
use dash_params::{BuriedDeployment, ChainSetup};
use dash_primitives::{BlockHash, QuorumHash};
use dash_types::Numeric as _;
use dashd_store::KvEngine;

use super::caches::EvoCaches;
#[cfg(any(test, feature = "test-support"))]
use super::cl::ClPass;
use super::cl::{ClAttempt, ClStep, ClVerifier, check_cbtx_best_chainlock, invalid};
use super::effects::{EffectAppliers, MnListSignal};
use super::ev1::{Ev1Checks, Ev1NotBuilt, PrevList, SpecialTxCtx};
use super::state_hash::{DigestEras, evo_digest};
use super::{EvoCodec, EvoLayer, EvoWindowDelta, RocksEvoStore};
use crate::chain::{
    AttemptOrigin, BlockFiles, ConnectCx, EhfRows, EvoReadCx, EvoReads, EvoTarget, Fatal, LlmqCall,
    OnVMembersError, SchemeGuardHolder, TipLlmq, VContext, ViewTarget, credit_window_start,
};
use crate::connect::{
    AttemptId, BlockAt, ClBest, EvoAttempt, EvoDigest, EvoHooks, NoCheckpoints, SpecialTxFailure,
    SpecialTxRules,
};
use crate::flush::EvoWindow;
use crate::llmq::facade::{AttemptHandle, MembersFacade, MinedViews};
use crate::llmq::members::{members, precompute};
use crate::llmq::quorum::{AttemptQuorums, FacadeScans, Materializer, NoQuorumHook};
use crate::llmq::source::NodeLlmqSource;
use crate::store::{CoinsReadError, GuardedSnapshot, HookTry};

/// A scratch target with no evo layer: the view's evo transaction keeps no state for the hooks.
pub const SCRATCH_WITHOUT_LAYER: &str = "VerifyDB's evo transaction keeps no evo layer";

/// The evo layer the node commits into (D48), behind one mutex. Its users:
/// - V's connector commits each block's rows into it, and its hooks build their views of it;
/// - V's flush takes its window and releases it, each under the lock alone, and writes the
///   window with no lock held;
/// - route A builds a view of it on the caller's thread, under the facade's gate held shared
///   ([`CommittedMinedViews`], D94);
/// - V's versionbits read their MNHF signals over it ([`CommittedEhfRows`], DEC-1028).
///
/// Each holds the lock only while it builds a view or applies one step. The lock order is the
/// facade's gate, then the layer: nothing takes the gate while it holds the layer.
///
/// A panic under the lock leaves the layer as its last whole step left it: [`EvoLayer::commit`]
/// builds the next active index on a copy and swaps it in at its end, and `take` and `release`
/// move whole indexes. So a reader still holding a route-A token after V's panic trips the node
/// latch (CH3a D71) reads one committed state, and [`Self::lock`] recovers the poisoned lock.
#[derive(Clone, Default)]
pub struct SharedEvoLayer(Arc<Mutex<EvoLayer>>);

impl SharedEvoLayer {
    pub fn new(layer: EvoLayer) -> Self {
        Self(Arc::new(Mutex::new(layer)))
    }

    pub fn lock(&self) -> MutexGuard<'_, EvoLayer> {
        // A panic under the lock left a whole state (see the type's doc).
        self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Route A's mined rows (D94): views of the layer `NodeEvo` commits into and its store, each built
/// on the caller's thread with the caller's own ancestry.
pub struct CommittedMinedViews<E: KvEngine> {
    store: Arc<RocksEvoStore<E>>,
    layer: SharedEvoLayer,
    /// Run once inside the next view's build, under the layer's lock: after the index clone,
    /// before the DB snapshot (C10's schedule, r1b F2).
    #[cfg(test)]
    pub(crate) in_view: TestHook,
    /// Run once, after the next view is built (C10's schedules).
    #[cfg(test)]
    pub(crate) after_view: TestHook,
}

/// A step a test runs once at a point of a reader's thread.
#[cfg(test)]
pub(crate) type TestHook = Mutex<Option<Box<dyn FnOnce() + Send>>>;

/// Runs and clears `hook`'s step, if any.
#[cfg(test)]
fn run_hook(hook: &TestHook) {
    if let Some(step) = hook.lock().unwrap_or_else(|p| p.into_inner()).take() {
        step();
    }
}

impl<E: KvEngine> CommittedMinedViews<E> {
    pub fn new(store: Arc<RocksEvoStore<E>>, layer: SharedEvoLayer) -> Self {
        Self {
            store,
            layer,
            #[cfg(test)]
            in_view: Mutex::default(),
            #[cfg(test)]
            after_view: Mutex::default(),
        }
    }
}

impl<E: KvEngine + 'static> MinedViews for CommittedMinedViews<E>
where
    for<'a> GuardedSnapshot<'a, E>: Sync,
{
    fn with_view(
        &self,
        reader: &ChainReader,
        f: &mut dyn FnMut(&dyn MinedCommitmentReader<Error = StoreError>),
    ) {
        let chain = Ancestry(reader);
        // One statement: the layer's guard lives through the index clone and the DB snapshot.
        let view = self.store.view_with(&self.layer.lock(), &chain, || {
            #[cfg(test)]
            run_hook(&self.in_view);
        });
        #[cfg(test)]
        run_hook(&self.after_view);
        f(&view);
    }
}

impl EvoWindow<EvoWindowDelta> for SharedEvoLayer {
    fn take(&mut self) -> EvoWindowDelta {
        self.lock().take()
    }

    fn bytes(&self) -> u64 {
        self.lock().bytes()
    }

    fn best_block(&self) -> Option<BlockHash> {
        self.lock().best_block()
    }

    fn release(&mut self) {
        self.lock().release();
    }
}

impl SharedEvoLayer {
    /// VerifyDB's evo transaction over the active layer ([`EvoLayer::scratch`]).
    pub fn scratch(&self) -> EvoLayer {
        self.lock().scratch()
    }
}

/// The layer an attempt's [`EvoTarget`] reads and commits into: the shared active one, or
/// VerifyDB's scratch copy (§6.3 step 5).
enum TargetLayer<'a> {
    Active(MutexGuard<'a, EvoLayer>),
    Scratch(&'a mut EvoLayer),
}

impl std::ops::Deref for TargetLayer<'_> {
    type Target = EvoLayer;

    fn deref(&self) -> &EvoLayer {
        match self {
            TargetLayer::Active(guard) => guard,
            TargetLayer::Scratch(layer) => layer,
        }
    }
}

impl std::ops::DerefMut for TargetLayer<'_> {
    fn deref_mut(&mut self) -> &mut EvoLayer {
        match self {
            TargetLayer::Active(guard) => guard,
            TargetLayer::Scratch(layer) => layer,
        }
    }
}

/// `evo`'s layer: the active one through `shared`, or the scratch transaction's. A reader builds
/// its view from it in one statement, as the view copies the indexes: the active layer's lock is
/// never held across a hook's callouts (block reads, the abort sink, the checks).
fn target_layer<'a>(
    shared: &'a SharedEvoLayer,
    evo: &'a mut EvoTarget<'_>,
) -> Result<TargetLayer<'a>, Fatal> {
    match evo {
        EvoTarget::Active => Ok(TargetLayer::Active(shared.lock())),
        EvoTarget::Scratch(txn) => {
            txn.layer().map(TargetLayer::Scratch).ok_or_else(|| Fatal::new(SCRATCH_WITHOUT_LAYER))
        }
    }
}

/// Refuses a scratch target with no layer, for the hooks that only stage.
fn check_target(evo: &mut EvoTarget<'_>) -> Result<(), Fatal> {
    match evo {
        EvoTarget::Scratch(txn) => {
            txn.layer().map(drop).ok_or_else(|| Fatal::new(SCRATCH_WITHOUT_LAYER))
        }
        EvoTarget::Active => Ok(()),
    }
}

/// `ReadBlockFromDisk(pindex)` for step 10's parent read (`cbtx:181-198`) and `GetForBlock`'s
/// rebuild (DEC-1028): `None` where the read fails, as `GetNonNullCoinbaseChainlock` answers
/// then.
pub trait EvoBlocks: Send + Sync {
    fn read(&self, reader: &ChainReader, id: BlockId) -> Option<Arc<Block>>;
}

/// [`EvoBlocks`] over V's block files, read at the entry's position as `ReadBlockFromDisk` does
/// (`abc`'s `read_block`).
pub struct FileBlocks<B>(pub B);

impl<B: BlockFiles + Send + Sync> EvoBlocks for FileBlocks<B> {
    fn read(&self, reader: &ChainReader, id: BlockId) -> Option<Arc<Block>> {
        let e = reader.entry(id)?;
        let file = e.file.filter(|_| e.status.has(Status::HAVE_DATA))?;
        let pos = FlatFilePos {
            file,
            pos: e.data_pos,
        };
        self.0.read_block(pos, &e.hash).ok()
    }
}

/// The Active target's open evo transaction (D1). NodeEvo owns it, not the attempt, because
/// `finish_attempt` consumes the attempt before `commit_tx` runs.
#[derive(Default)]
struct PendingTx {
    /// H4 or H9 stages it; H6 sets its evo tip. `None` for genesis, which writes nothing (D7).
    delta: Option<EvoDelta>,
    /// The list after a connected block: H11 hashes it, and the commit offers it to the list
    /// cache (§3.7) unless `over_initial_snapshot`.
    list: Option<Arc<MnList>>,
    /// Whether `list` was built over a parent list that stood on the initial snapshot: such a list
    /// enters no shared cache (OD-CP-51), but it is still the block's list, the one Core's
    /// `ProcessBlock` builds over its own initial snapshot.
    over_initial_snapshot: bool,
    /// Step 12's scheme (D9).
    post_scheme: Option<BlsCtx>,
    /// The scheme the commit publishes, once H1's commit has run (D9).
    publish: Option<BlsCtx>,
}

/// D19: `ProcessSpecialTxsInBlock`'s bench statics (`stm:866-871`, `:985-987` at v24.0.0), per
/// NodeEvo and so per chainstate.
#[derive(Default)]
struct BenchTotals {
    payload: i64,
    loop_: i64,
    credit_pool: i64,
    quorum: i64,
    dmn: i64,
    merkle_mnl: i64,
    merkle_quorums: i64,
    cbtx_cl: i64,
    mnehf: i64,
}

/// `LogPrint(BCLog::BENCHMARK, "      - %s: %.2fms [%.2fs]\n", …)`, adding the step's time to its
/// total; returns the step's end.
fn bench(name: &str, total: &mut i64, from: Instant) -> Instant {
    let now = Instant::now();
    let micros = i64::try_from(now.duration_since(from).as_micros()).unwrap_or(i64::MAX);
    *total = total.saturating_add(micros);
    tracing::debug!(
        target: "bench",
        "      - {name}: {:.2}ms [{:.2}s]",
        micros as f64 * 0.001,
        *total as f64 * 0.000_001
    );
    now
}

/// What [`NodeEvo::new`] takes (D12, D48).
pub struct NodeEvoParts<E: KvEngine, C: Ev1Checks = Ev1NotBuilt> {
    pub store: Arc<RocksEvoStore<E>>,
    pub layer: SharedEvoLayer,
    pub appliers: EffectAppliers,
    pub params: Arc<ChainSetup>,
    /// `NetInfoRules::new(params, mainnet, config)`, with the node's `-cjdnsreachable`.
    pub net_info: NetInfoRules,
    pub ev1: C,
    pub cl: Box<dyn ClVerifier + Send>,
    pub blocks: Box<dyn EvoBlocks>,
    /// The scheme the tip committed, Core's startup `bls_legacy_scheme`: the node's is
    /// `bls_ctx_for_block(params, tip + 1)` (D9, §3.4).
    pub scheme: BlsCtx,
    /// The MN-list signal (D15), whose applier is `appliers.notify`.
    pub mn_list: MnListSignal,
}

/// The production `EvoHooks` (CH7 §2.1), one per chainstate, owned by the connector.
pub struct NodeEvo<E: KvEngine, C: Ev1Checks = Ev1NotBuilt> {
    store: Arc<RocksEvoStore<E>>,
    layer: SharedEvoLayer,
    pending: Option<PendingTx>,
    caches: EvoCaches,
    qc_hashes: QcHashCache,
    appliers: EffectAppliers,
    params: Arc<ChainSetup>,
    dml: DmlParams,
    net_info: NetInfoRules,
    payments: PaymentParams,
    ev1: C,
    cl: Box<dyn ClVerifier + Send>,
    /// Shared with V's versionbits' rows ([`CommittedEhfRows`]).
    blocks: Arc<dyn EvoBlocks>,
    /// The committed scheme: Core's `bls::bls_legacy_scheme` between attempts (D9).
    scheme: BlsCtx,
    /// The open tip or walk guard (CH3a D192, CH5 D1).
    guard: Option<SchemeGuard>,
    mn_list: MnListSignal,
    bench: BenchTotals,
    /// BL-564's census cells: its checks' registered ones, or its own (`Ev1Checks::gate_counts`).
    #[cfg(any(test, feature = "test-support"))]
    edges: Arc<super::gates::GateCounts>,
    /// The height at which H11 reads the staged credit pool as Core's `GetCreditPool`
    /// exception (CH7a-5b §8 answer 9's plant): the staged delta is unchanged.
    #[cfg(any(test, feature = "test-support"))]
    h11_pool_error_at: Option<u32>,
}

/// [`NodeEvo`]'s attempt (CH7 §2.1): the facade's handle, its log, H1's scheme and the connect
/// context it is bound to (D48). The handle, the log and the scheme are separate fields, so a hook
/// lends the handle (through [`AttemptBinding`]) and the log together.
pub struct NodeEvoAttempt<'v> {
    /// The facade's gate, exclusively, until `finish_attempt` releases it (EV addendum §2.6). It
    /// has no accessor: [`AttemptBinding`] is the only use of it.
    handle: AttemptHandle<'v>,
    /// The facade the handle is released through.
    facade: &'v MembersFacade,
    log: Vec<Effect>,
    cx: &'v ConnectCx<'v>,
    /// `cx`'s index, which a binding's view walks.
    chain: Ancestry<'v>,
    /// H1's answer, fixed for the attempt (D9).
    bls: Option<BlsCtx>,
    /// H1's commit ran.
    committed: bool,
    /// The attempt's target is VerifyDB's scratch view: its MN-list signal goes through the log
    /// (D15).
    scratch: bool,
    /// H9 ran: the attempt is a disconnect's, whose failed snapshot write the disconnect reports
    /// (OD-CP-18).
    undo: bool,
    /// The attempt is a disconnect's, under its holder's guard (`EvoHooks::mark_disconnect`).
    disconnect: bool,
    /// The attempt ended in one of Core's asserts or a coins read fault
    /// (`EvoHooks::mark_unlogged`): its guard's revert is not logged.
    unlogged: bool,
}

// Core: validation.cpp:147-175 ScopedBLSLegacyScheme (v24.0.0)
/// A tip or walk holder's `ScopedBLSLegacyScheme` (CH3a D192, CH5 D1): the published scheme it
/// saved, and the scheme its holder's attempts have left so far, Core's global under the guard.
struct SchemeGuard {
    holder: SchemeGuardHolder,
    saved: BlsCtx,
    current: BlsCtx,
    /// The scheme the last disconnect's undo was entered with, which its catch restores
    /// (`evo/specialtxman.cpp:1065`, `:1087` at v24.0.0).
    entry: Option<BlsCtx>,
    committed: bool,
}

/// Where an effect is applied in Core's `ConnectBlock` (`validation.cpp:2501-2946` at v24.0.0),
/// so which scheme a failed one leaves (OD-CP-182 R1).
enum ConnectPhase {
    /// Before `ProcessSpecialTxsInBlock`'s switch (`evo/specialtxman.cpp:1043-1047` at v24.0.0):
    /// H1's.
    BeforeSwitch,
    /// After it (the conflicting IS lock's removal, `validation.cpp:2821`): step 12's.
    AfterSwitch,
    /// After `Commit()` (`:2927`; the MN-list signal, `:2929-2931`; at v24.0.0): kept.
    AfterCommit,
}

fn connect_phase(kind: EffectKind) -> ConnectPhase {
    match kind {
        EffectKind::RemoveConflictingIsLock => ConnectPhase::AfterSwitch,
        EffectKind::NotifyMnListChanged => ConnectPhase::AfterCommit,
        EffectKind::StoreQuorumSnapshot
        | EffectKind::EraseMineableCommitment
        | EffectKind::ReinsertMineableCommitment
        | EffectKind::ResetPlatformBan => ConnectPhase::BeforeSwitch,
    }
}

impl EvoAttempt for NodeEvoAttempt<'_> {
    fn id(&self) -> AttemptId {
        self.handle.id()
    }

    fn sink(&mut self) -> &mut dyn EffectSink {
        &mut self.log
    }
}

impl NodeEvoAttempt<'_> {
    /// The attempt's members binding (CH7 §4.2.3, Q2), with its log for the calls' effects. Its
    /// scheme is H1's: before H1 sets it there is no binding, never one on the published scheme.
    /// Its view is the hook's own read of the attempt's target layer, so a hook takes one
    /// snapshot (X3).
    ///
    /// # Errors
    ///
    /// A bind before H1, or parameters outside the LLMQ domain.
    fn bind<'a>(
        &'a mut self,
        view: &'a dyn EvoStore,
        params: &'a ChainSetup,
        lists: &'a dyn ListCache,
        blocks: &'a dyn EvoBlocks,
    ) -> Result<(AttemptBinding<'a>, &'a mut dyn EffectSink), Fatal> {
        let bls = self.bls.ok_or_else(|| Fatal::new("members before H1's scheme"))?;
        let complete = CompleteChain::<StoreError>::new(&self.chain);
        LlmqCtx::new(params, &complete, self.cx.deployments, bls)
            .map_err(|e| Fatal::new(format!("LLMQ parameters: {e:?}")))?;
        let binding = AttemptBinding {
            handle: &self.handle,
            chain: &self.chain,
            deployments: self.cx.deployments,
            reader: self.cx.reader,
            params,
            bls,
            view,
            lists,
            blocks,
        };
        Ok((binding, &mut self.log))
    }
}

#[cfg(test)]
impl<'v> NodeEvoAttempt<'v> {
    /// The attempt's handle with its log, for a test's members calls over a source of its own;
    /// H4 and H9 call through [`AttemptBinding`].
    pub(crate) fn capability(&mut self) -> (&AttemptHandle<'v>, &mut dyn EffectSink) {
        (&self.handle, &mut self.log)
    }
}

#[cfg(test)]
impl<E: KvEngine + 'static, C: Ev1Checks> NodeEvo<E, C>
where
    for<'a> GuardedSnapshot<'a, E>: Sync,
{
    /// The list cache the bindings read, for a test that evicts from it.
    pub(crate) fn list_cache(&self) -> &dash_dml::store::LruListCache {
        &self.caches.lists
    }

    /// `f` over `attempt`'s binding, as H4 and H9 bind it: over a view of `evo`'s layer and the
    /// hooks' parameters, list cache and blocks.
    ///
    /// # Errors
    ///
    /// `evo` a scratch target without a layer, or the bind's.
    pub(crate) fn with_binding<R>(
        &self,
        attempt: &mut NodeEvoAttempt<'_>,
        evo: &mut EvoTarget<'_>,
        f: impl FnOnce(AttemptBinding<'_>, &mut dyn EffectSink) -> R,
    ) -> Result<R, Fatal> {
        let chain = Ancestry(attempt.cx.reader);
        let view = self.store.view(&*target_layer(&self.layer, evo)?, &chain);
        let (binding, sink) =
            attempt.bind(&view, &self.params, &self.caches.lists, &*self.blocks)?;
        Ok(f(binding, sink))
    }
}

/// An attempt's members binding (CH7 §4.2.3, Q2): route C's members, precompute and quorums
/// through the attempt's handle, over its chain and deployments, H1's scheme and the hook's read
/// of its target's layer. Only `NodeEvoAttempt::bind` builds one, and it borrows the attempt,
/// so it cannot outlive the attempt or leave V.
pub struct AttemptBinding<'a> {
    handle: &'a AttemptHandle<'a>,
    chain: &'a Ancestry<'a>,
    deployments: &'a dyn DeploymentView,
    reader: &'a ChainReader,
    params: &'a ChainSetup,
    bls: BlsCtx,
    view: &'a dyn EvoStore,
    lists: &'a dyn ListCache,
    blocks: &'a dyn EvoBlocks,
}

impl AttemptBinding<'_> {
    /// `f` over the LLMQ context and the source, built for the call from the bound fields (CH7
    /// §4.2.3).
    pub(crate) fn with_llmq<T>(
        &self,
        f: impl FnOnce(&LlmqCtx<'_, StoreError>, &NodeLlmqSource<'_>) -> T,
    ) -> T {
        let complete = CompleteChain::<StoreError>::new(self.chain);
        let Ok(ctx) = LlmqCtx::new(self.params, &complete, self.deployments, self.bls) else {
            unreachable!("the bind checked the parameters' domain");
        };
        let src = NodeLlmqSource::new(
            self.chain,
            self.deployments,
            self.view,
            self.lists,
            self.blocks,
            self.reader,
        );
        f(&ctx, &src)
    }

    /// The block processor's context over the binding (route C): its members and precompute,
    /// its chain as `LookupBlockIndex`, and its view's mined commitments. H4's step 6 and H9
    /// build theirs only here.
    fn qc<'q>(&'q self, llmq: LlmqCtx<'q, StoreError>) -> QcCtx<'q> {
        QcCtx {
            llmq,
            index: self.chain,
            mined: self.view,
            members: self,
            precompute: self,
            bls_checks: &SerialBlsChecks,
        }
    }

    /// Route C's quorum manager for types 7 and 9 on the block path: `GetQuorum` and
    /// `ScanQuorums` over the chain ending at `tip`, the block's `pindexPrev` (D4).
    fn quorums(&self, tip: BlockId) -> BoundQuorums<'_, '_> {
        BoundQuorums {
            binding: self,
            tip,
        }
    }

    /// Step 10's `ScanQuorums(type, start, signingActiveQuorumCount)` with
    /// `IsQuorumRotationEnabled(params, start)` (CH7b-3 C4; X1, OD-CP-1): the overload
    /// `SelectQuorumForSigning` calls, so the facade's scan cache, and each quorum through the
    /// non-chain `GetQuorum`, mined on the active chain, which ends at `tip`, the block's
    /// `pindexPrev`, while the block connects. A build goes through the attempt's handle, its
    /// effects to `sink`. Each entry is its mined commitment's key, as Core takes it
    /// ([`ScannedQuorum::mined`]); the scan's cache event goes with it.
    ///
    /// # Errors
    ///
    /// A read fault, or a member computation's (`NodeMemberError`'s mapping).
    // Core: v24.0.0 llmq/quorumsman.cpp:754-805 SelectQuorumForSigning, :164-169, :179-295
    // ScanQuorums
    pub(super) fn signing_scan(
        &self,
        tip: BlockId,
        llmq_type: LlmqType,
        start: BlockId,
        sink: &mut dyn EffectSink,
    ) -> Result<(LockScan, Option<ScanEvent>), StoreError> {
        self.with_llmq(|ctx, src| {
            let llmq = ctx.llmq(llmq_type).map_err(scan_error)?;
            let rotation_enabled =
                is_quorum_rotation_enabled(ctx, llmq, start).map_err(scan_error)?;
            let mined: &dyn MinedCommitmentReader<Error = StoreError> = self.view;
            let mut mat = Materializer {
                cap: self.handle,
                ctx,
                src,
                mined,
                tip,
                effects: sink,
                hook: &NoQuorumHook,
            };
            let count = usize::from(llmq.signing_active_quorum_count);
            let made_up = || src.made_up_lists();
            let scanned = scan_quorums(
                ctx,
                mined,
                &mut FacadeScans::new(self.handle, &made_up),
                &mut mat,
                llmq_type,
                start,
                count,
                ScanOverload::Cached,
            )
            .map_err(scan_error)?;
            let entries =
                scanned.quorums.iter().map(|q| ScannedQuorum::mined(q.base, &q.commitment));
            let scan = LockScan {
                rotation_enabled,
                entries: entries.collect(),
            };
            Ok((scan, scanned.event))
        })
    }
}

/// A step-10 scan's fault as a store error: an unknown type cannot reach the scan, as the lock
/// check resolved the ChainLock type's parameters before it.
fn scan_error(e: LlmqError<StoreError>) -> StoreError {
    match NodeMemberError::from(e) {
        NodeMemberError::Store(e) => e,
        NodeMemberError::UnknownLlmqType(t) => {
            StoreError::inconsistent(format!("step 10's quorum scan of LLMQ type {t}"))
        }
    }
}

impl QuorumMemberSource for AttemptBinding<'_> {
    type Entry = Arc<Dmn>;
    type Error = StoreError;

    /// Route C's `GetAllQuorumMembers`: the facade's caches, and the attempt's rows.
    fn members(
        &self,
        llmq_type: LlmqType,
        base: BlockId,
        effects: &mut dyn EffectSink,
    ) -> Result<QuorumMembers<Arc<Dmn>>, MemberError<StoreError>> {
        self.with_llmq(|ctx, src| members(self.handle, ctx, src, effects, llmq_type, base, false))
    }
}

impl QuorumPrecompute for AttemptBinding<'_> {
    type Error = StoreError;

    /// Route C's `PreComputeQuorumMembers`.
    fn precompute(
        &self,
        block: BlockId,
        mode: PrecomputeMode,
        effects: &mut dyn EffectSink,
    ) -> Result<(), MemberError<StoreError>> {
        self.with_llmq(|ctx, src| precompute(self.handle, ctx, src, effects, block, mode))
    }
}

/// [`AttemptBinding::quorums`]: each call runs [`AttemptQuorums`] over the binding's LLMQ context
/// and source, built for the call as a members call builds them, its index the attempt's chain.
/// A build runs no hook: QM1a's is not built (D89).
struct BoundQuorums<'b, 'a> {
    binding: &'b AttemptBinding<'a>,
    tip: BlockId,
}

impl BoundQuorums<'_, '_> {
    fn with<T>(
        &self,
        f: impl FnOnce(&mut AttemptQuorums<'_, '_, AttemptHandle<'_>, MnList>) -> T,
    ) -> T {
        let b = self.binding;
        b.with_llmq(|ctx, src| {
            f(&mut AttemptQuorums {
                cap: b.handle,
                ctx,
                src,
                mined: b.view,
                index: b.chain,
                tip: self.tip,
                hook: &NoQuorumHook,
            })
        })
    }
}

impl QuorumLookup for BoundQuorums<'_, '_> {
    fn get_quorum(
        &mut self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
        effects: &mut dyn EffectSink,
    ) -> Result<Option<QuorumKey>, SeamError> {
        self.with(|q| q.get_quorum(llmq_type, quorum_hash, effects))
    }

    fn scan_quorums(
        &mut self,
        llmq_type: LlmqType,
        start: BlockId,
        count: usize,
        out: &mut QuorumScan,
        effects: &mut dyn EffectSink,
    ) -> Result<(), SeamError> {
        self.with(|q| q.scan_quorums(llmq_type, start, count, out, effects))
    }
}

/// Core's `%d` of `bls_legacy_scheme`.
fn legacy(bls: BlsCtx) -> u8 {
    u8::from(bls == BlsCtx::LEGACY)
}

// Core: validation.cpp:162-167 ~ScopedBLSLegacyScheme (v24.0.0)
/// The scheme guard's revert line, back to `saved`.
fn log_reverted(saved: BlsCtx) {
    tracing::info!("ScopedBLSLegacyScheme: reverted bls_legacy_scheme={}", legacy(saved));
}

/// D5 for a store error: D2's readiness refusal, else a fault.
pub(super) fn store_failure(what: &str, e: &StoreError) -> Result<SpecialTxFailure, Fatal> {
    match e.capability() {
        Some(capability) => Ok(SpecialTxFailure::NotReady(capability)),
        None => Err(store_fault(what, e)),
    }
}

/// D5's fault for an evo store error, reported once (OD-CP-96 F1): an engine read error tripped
/// the latch as the guarded engine reported it, as a coins read's does (CH3a D60), so V reports
/// only a fault the latch does not show, a row that does not decode or does not fit.
pub(crate) fn store_fault(what: &str, e: &dyn std::fmt::Display) -> Fatal {
    Fatal::new(format!("{what}: {e}"))
}

/// D5 for the quorum processor: a verdict, D2's refusal, or a fault (`NoReason` and SB8's
/// no-verdict domain included).
pub(crate) fn qc_failure(e: QcBlockError) -> Result<SpecialTxFailure, Fatal> {
    match e {
        QcBlockError::Invalid(err) => Ok(SpecialTxFailure::Invalid(err)),
        QcBlockError::NoReason(what) => Err(Fatal::new(format!("quorum block processor: {what}"))),
        QcBlockError::BlsUnsupported => {
            Err(Fatal::abort_node("quorum block processor: a signature outside the BLS domain"))
        }
        QcBlockError::Fault(e) => match e.capability() {
            Some(capability) => Ok(SpecialTxFailure::NotReady(capability)),
            None => Err(store_fault("quorum block processor", &e)),
        },
    }
}

/// Core's catch-all (`stm:1053-1056` at v24.0.0): the FAILURE line and `failed-procspectxsinblock`.
fn exception(what: impl std::fmt::Display) -> SpecialTxFailure {
    tracing::error!("CSpecialTxProcessor::ProcessSpecialTxsInBlock -- FAILURE! {what}");
    invalid("failed-procspectxsinblock")
}

/// The block's index entry, which every attempt has: a proposal's is the dummy index
/// (MN0-2b-1, `validation.cpp:4954-4958` at v24.0.0).
fn indexed(at: &BlockAt) -> Result<BlockId, Fatal> {
    at.id.ok_or_else(|| Fatal::assert(format!("H4: the block at {} has no index entry", at.height)))
}

/// The delta of `at` before any step fills it.
fn empty_delta(at: &BlockAt) -> EvoDelta {
    EvoDelta {
        block: at.hash,
        height: at.height,
        dml: None,
        mined_commitments: Vec::new(),
        credit_data: None,
        credit_pool: None,
        mnhf_signals: None,
        erased_mined: Vec::new(),
        evo_tip: None,
    }
}

fn height_i32(h: u32) -> Result<i32, Fatal> {
    i32::try_from(h).map_err(|_| Fatal::new("a height past Core's int"))
}

/// The block index as the evo ports walk it, through V's reader: inside the hold every entry an
/// attempt asks about is published.
pub(crate) struct Ancestry<'a>(pub(crate) &'a ChainReader);

impl ChainAncestry for Ancestry<'_> {
    fn header(&self, id: BlockId) -> HeaderCtx {
        let e = crate::chain::index::entry(self.0, id);
        HeaderCtx {
            height: e.height,
            hash: e.hash,
            time: e.header.time(),
            bits: CompactTarget::new(e.header.bits()),
            version: e.header.version(),
            mtp: e.mtp,
            chain_work: e.chain_work,
        }
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        self.0.ancestor(id, height)
    }

    fn prev(&self, id: BlockId) -> Option<BlockId> {
        crate::chain::index::entry(self.0, id).prev
    }
}

impl BlockLookup for Ancestry<'_> {
    fn lookup(&self, hash: &BlockHash) -> Option<(BlockId, u32)> {
        let id = self.0.lookup(hash)?;
        Some((id, self.0.entry(id)?.height))
    }
}

/// The evo store below DIP0003, where no evo row exists and no port reads one: MNHF's stage reads
/// its cache's empty map below V20, and step 10's quorum root finds no commitment.
static NO_ROWS: LazyLock<MemEvoStore> = LazyLock::new(MemEvoStore::default);

fn buried_after(cx: &ConnectCx<'_>, prev: Option<BlockId>, d: BuriedDeployment) -> bool {
    cx.deployments.active_after(prev, Deployment::Buried(d))
}

impl<E: KvEngine + 'static, C: Ev1Checks> NodeEvo<E, C>
where
    for<'a> GuardedSnapshot<'a, E>: Sync,
{
    /// # Errors
    ///
    /// The network's payment parameters outside Core's domain.
    pub fn new(parts: NodeEvoParts<E, C>) -> Result<Self, Fatal> {
        #[cfg(any(test, feature = "test-support"))]
        let edges = parts.ev1.gate_counts().unwrap_or_default();
        let payments = PaymentParams::from_chain_params(&parts.params)
            .map_err(|e| Fatal::new(format!("payment parameters: {e:?}")))?;
        Ok(Self {
            dml: DmlParams::new(&parts.params),
            payments,
            store: parts.store,
            layer: parts.layer,
            pending: None,
            caches: EvoCaches::default(),
            qc_hashes: QcHashCache::new(),
            appliers: parts.appliers,
            params: parts.params,
            net_info: parts.net_info,
            ev1: parts.ev1,
            cl: parts.cl,
            blocks: Arc::from(parts.blocks),
            scheme: parts.scheme,
            guard: None,
            mn_list: parts.mn_list,
            bench: BenchTotals::default(),
            #[cfg(any(test, feature = "test-support"))]
            edges,
            #[cfg(any(test, feature = "test-support"))]
            h11_pool_error_at: None,
        })
    }

    /// The node's one MNHF manager (D16): the connector's `GetForBlock` reads its cache, and
    /// [`CommittedEhfRows`]' rebuild fills it.
    #[cfg(test)]
    pub(crate) fn mnhf_manager(&self) -> &MnhfManager {
        &self.caches.mnhf
    }

    /// The committed scheme (D9), which `ChainView.bls` publishes ([`EvoHooks::published_bls`]).
    pub fn scheme(&self) -> BlsCtx {
        self.scheme
    }

    /// The scheme an attempt's guard saves, Core's global as it opens: in VerifyDB's walk, a
    /// scratch attempt's is the walk's (OD-CP-182 R3, CH5 D1); any other's is the published one.
    fn base(&self, scratch: bool) -> BlsCtx {
        match &self.guard {
            Some(walk) if scratch && walk.holder == SchemeGuardHolder::Walk => walk.current,
            _ => self.scheme,
        }
    }

    /// The open guard, if `holder` holds it.
    fn guard_of(&mut self, holder: SchemeGuardHolder) -> Option<&mut SchemeGuard> {
        self.guard.as_mut().filter(|g| g.holder == holder)
    }

    /// Arms CH7a-5b's plant: at `height`, H11 reads the staged credit pool as Core's
    /// `GetCreditPool` exception, without changing what is staged or committed.
    #[cfg(any(test, feature = "test-support"))]
    pub fn plant_h11_pool_error_at(&mut self, height: u32) {
        self.h11_pool_error_at = Some(height);
    }

    /// BL-564's census counts: the evo edges H4 has entered, as its cells hold them.
    #[cfg(any(test, feature = "test-support"))]
    pub fn edge_counts(&self) -> super::gates::EvoEdgeCounts {
        self.edges.edges()
    }

    /// The `p.` mirrors of [`Self::edge_counts`]: the evo edges proposals have entered.
    #[cfg(any(test, feature = "test-support"))]
    pub fn proposal_edge_counts(&self) -> super::gates::EvoEdgeCounts {
        self.edges.proposal_edges()
    }

    fn pending(&mut self) -> Result<&mut PendingTx, Fatal> {
        self.pending.as_mut().ok_or_else(|| Fatal::new("an evo hook outside its transaction (D1)"))
    }

    /// H4's body, `ProcessSpecialTxsInBlock` (`stm:857-1059` at v24.0.0), in Core's order. The
    /// block's rows go to the open transaction only once every step has passed.
    #[expect(clippy::too_many_arguments, reason = "H4's arguments")]
    #[expect(clippy::too_many_lines, reason = "Core's one function, step by step")]
    fn special_txs(
        &mut self,
        att: &mut NodeEvoAttempt<'_>,
        layer: &EvoLayer,
        block: &Block,
        at: &BlockAt,
        rules: SpecialTxRules,
        coins: &dyn CoinsView<Error = CoinsReadError>,
        subsidy: Amount,
        just_check: bool,
        check: bool,
        cl_best: &mut ClBest<'_>,
    ) -> Result<Result<Option<MnListChange>, SpecialTxFailure>, Fatal> {
        let (cx, facade) = (att.cx, att.facade);
        let bls = att.bls.ok_or_else(|| Fatal::new("ProcessSpecialTxsInBlock before H1"))?;
        let prev = at.prev.ok_or_else(|| Fatal::new("ProcessSpecialTxsInBlock at genesis"))?;
        let prev_entry = crate::chain::index::entry(cx.reader, prev);
        let h = at.height;
        let after = |d| buried_after(cx, at.prev, d);
        let dip3 = after(BuriedDeployment::Dip0003);
        let v20 = after(BuriedDeployment::V20);
        let Self {
            store,
            caches,
            qc_hashes,
            params,
            dml,
            net_info,
            payments,
            ev1,
            cl,
            blocks,
            bench: totals,
            #[cfg(any(test, feature = "test-support"))]
            edges,
            ..
        } = self;
        #[cfg(any(test, feature = "test-support"))]
        if !check {
            edges.record_roots(|r| r.unchecked.push(h));
        }
        let mut delta = empty_delta(at);
        let t1 = Instant::now();

        // The view step 4's list and steps 3 to 10 read: from DIP0003 on; below it none is pinned
        // (§7.1).
        let chain = Ancestry(cx.reader);
        let view = dip3.then(|| store.view(layer, &chain));
        let cons = ConsensusCtx {
            params: cx.params,
            height: h,
            prev: at.prev,
            chain: &chain,
            deployments: cx.deployments,
            checkpoints: &NoCheckpoints,
            bls,
            median_time_past: prev_entry.mtp,
            header_check_time: 0,
        };

        // Step 0 (EV1-2).
        let payloads = ev1.payloads(block);

        // The LLMQ context steps 4 to 10 read, and the attempt's members binding over the view
        // (route C, D4): from DIP0003 on, where steps 4 and 6 to 10 ask quorums.
        let complete = CompleteChain::<StoreError>::new(&chain);
        let llmq_ctx = || {
            LlmqCtx::new(params, &complete, cx.deployments, bls)
                .map_err(|e| Fatal::new(format!("LLMQ parameters: {e:?}")))
        };
        let lists = &caches.lists;
        let (llmq, binding, mut sink) = match &view {
            Some(view) => {
                let (binding, sink) = att.bind(view, params, lists, &**blocks)?;
                (Some(llmq_ctx()?), Some(binding), Some(sink))
            }
            None => (None, None, None),
        };
        // Whether the parent's list stood on the initial snapshot: then the list this block builds
        // over it is not offered to the shared cache on commit (OD-CP-51).
        let parent_made_up = Cell::new(false);
        let resolve = || match &view {
            Some(view) => {
                let resolved = resolve_list_traced(prev, &chain, cx.deployments, view, lists)?;
                parent_made_up.set(parent_made_up.get() || resolved.over_initial_snapshot);
                Ok(resolved.list)
            }
            // Below DIP0003 no check reads the list: one that does fails closed.
            None => Err(StoreError::inconsistent("GetListForBlock below DIP0003 in step 4")),
        };
        let parent_list = PrevList::new(&resolve);
        let mut stx = SpecialTxCtx {
            cons: &cons,
            rules,
            coins,
            indexes: None,
            check_sigs: check,
            list: &parent_list,
            net_info: *net_info,
        };

        // Step 1 (`stm:875-893` at v24.0.0) and step 2 (`:894-899` at v24.0.0). Step 1 keys on the
        // type bits alone, Core's `nType`, at every height and version (D56).
        let raw_cbtx =
            block.txs().first().is_some_and(|tx| tx.tx_type().to_base() == TRANSACTION_COINBASE);
        let cbtx = if check && raw_cbtx {
            match ev1.check_cbtx(block, &payloads, stx)? {
                Ok(cbtx) => Some(cbtx),
                Err(failure) => return Ok(Err(failure)),
            }
        } else {
            None
        };
        if check && dip3 && cbtx.is_none() {
            return Ok(Err(invalid("bad-cbtx-version")));
        }
        let t2 = bench("GetTxPayload", &mut totals.payload, t1);

        // Step 3 (`stm:907-911` at v24.0.0): the parent's pool, which step 5 and the block's
        // records reuse (§3.5.2). V20 at the block implies DIP0003, so the view is pinned.
        let prev_pool = match &view {
            Some(view) if v20 => {
                let v20_at_prev =
                    cx.deployments.active_at(prev, Deployment::Buried(BuriedDeployment::V20));
                match caches.pools.pool_after(view, &prev_entry.hash, v20_at_prev) {
                    Ok(Ok(pool)) => Some(pool),
                    // EV3 D8: Core's construction throws at the read.
                    Ok(Err(e)) => return Ok(Err(exception(format!("{e:?}")))),
                    Err(e) => return store_failure("GetCreditPool", &e).map(Err),
                }
            }
            _ => None,
        };
        stx.indexes = prev_pool.as_ref();

        // Step 4 (`stm:913-936` at v24.0.0). Every transaction's seams: the attempt's LLMQ context,
        // index and sink, LQ2's verifier over the binding's members for type 6 (DEC-990), and the
        // binding's quorum manager on the chain ending at `pindexPrev` for types 7 and 9
        // (`stm:260`, `:265`).
        let verifier = llmq.zip(binding.as_ref()).map(|(llmq, binding)| NodeCommitmentVerifier {
            llmq,
            members: binding,
        });
        let mut quorums = binding.as_ref().map(|binding| binding.quorums(prev));
        for i in 0..block.txs().len() {
            let quorum = match (&verifier, &mut quorums, &mut sink) {
                (Some(verifier), Some(quorums), Some(sink)) => Some(QuorumSeams {
                    llmq: verifier.llmq,
                    index: &chain,
                    commitments: verifier,
                    quorums,
                    effects: &mut **sink,
                }),
                _ => None,
            };
            if let Err(failure) = ev1.check_tx(block, &payloads, i, stx, quorum)? {
                return Ok(Err(failure));
            }
        }
        let t3 = bench("Loop", &mut totals.loop_, t2);

        // Step 5 (`stm:942-948` at v24.0.0; `CheckCreditPoolDiffForBlock`, `:1096-1127` at
        // v24.0.0).
        if let Some(cbtx) = &cbtx {
            let dip8 = after(BuriedDeployment::Dip0008);
            if dip8 && v20 {
                let mn_rr = after(BuriedDeployment::MnRr);
                // Core: masternode/payments.cpp:58; evo/creditpool.cpp:293 at v24.0.0
                let reward = platform_reward(mn_rr, height_i32(h)?, subsidy, payments)
                    .map_err(|e| Fatal::assert(format!("PlatformShare: {e:?}")))?;
                let pool = prev_pool.as_ref().ok_or_else(|| Fatal::new("step 5 without a pool"))?;
                let balance = cbtx.credit_pool_balance().unwrap_or(0);
                if let Err(err) = check_block_diff(block, balance, pool, reward, dip8, v20) {
                    tracing::error!(
                        "CSpecialTxProcessor: CheckCreditPoolDiffForBlock for block {} failed with {}",
                        at.hash,
                        err.reject_reason()
                    );
                    return Ok(Err(SpecialTxFailure::Invalid(err)));
                }
            }
        }
        let t4 = bench("CheckCreditPoolDiffForBlock", &mut totals.credit_pool, t3);

        // Steps 6–10 read the quorums through the binding.
        // Step 6 (`stm:955-958` at v24.0.0). Below DIP0003 `ProcessBlock` returns at once.
        #[cfg(any(test, feature = "test-support"))]
        edges.bump("w.quorum");
        if let (Some(llmq), Some(binding), Some(sink)) = (llmq, &binding, &mut sink) {
            #[cfg(any(test, feature = "test-support"))]
            edges.bump("g.quorum");
            // A proposal's is the dummy index (MN0-2b-1, `validation.cpp:4954-4958` at v24.0.0).
            let id = indexed(at)?;
            let qc = binding.qc(llmq);
            let mode = QcMode {
                just_check,
                bls_checks: check,
            };
            let at_qc = QcBlockAt {
                id,
                height: h,
            };
            let mined = dash_dml::blockprocessor::process_block(
                &qc,
                block,
                at_qc,
                Some(prev),
                mode,
                &mut **sink,
            );
            match mined {
                Ok(mined) => delta.mined_commitments = mined,
                Err(e) => return qc_failure(e).map(Err),
            }
        }
        let t5 = bench("m_qblockman.ProcessBlock", &mut totals.quorum, t4);

        // Step 7 (`stm:960-977` at v24.0.0).
        let mut list = None;
        let mut change = None;
        if let (Some(binding), Some(sink)) = (&binding, &mut sink) {
            #[cfg(any(test, feature = "test-support"))]
            edges.bump("g.dmn");
            let prev_list = match parent_list.get() {
                Ok(list) => list,
                Err(e) => return store_failure("GetListForBlock", &e).map(Err),
            };
            let dml_rules = DmlRules {
                params: dml,
                net_info,
                v19: after(BuriedDeployment::V19),
                mn_rr: after(BuriedDeployment::MnRr),
                v24: rules.v24,
            };
            let prev_block = PrevBlock {
                id: prev,
                hash: prev_entry.hash,
                height: height_i32(prev_entry.height)?,
                chain: &chain,
            };
            let built = build_new_list_from_block(
                block,
                prev_block,
                prev_list,
                coins,
                binding,
                &dml_rules,
                &mut **sink,
                &mut NoTrace,
            );
            let built = match built {
                Ok(built) => built.list.with_block_hash(at.hash),
                Err(DmlBuildError::Invalid(err)) => return Ok(Err(SpecialTxFailure::Invalid(err))),
                Err(DmlBuildError::Exception(e)) => return Ok(Err(exception(e))),
                Err(DmlBuildError::Store(e)) => {
                    return store_failure("BuildNewListFromBlock", &e).map(Err);
                }
            };
            if just_check {
                list = Some(Arc::new(built));
            } else {
                let height = height_i32(h)?;
                // The first DIP3 block (DEC-11), or a child of an initial-snapshot block, which
                // `parent_list` above may just have added, as Core's `GetListForBlockInternal
                // (pprev)` does before its check (`deterministicmns.cpp:710`, `:722` at v24.0.0).
                let extra_snapshot =
                    height == dml.dip3_height || lists.is_initial_snapshot(&prev_entry.hash);
                let processed = dash_dml::process_block(
                    block,
                    (at.hash, height),
                    prev_list,
                    built,
                    dml,
                    extra_snapshot,
                    &mut **sink,
                );
                match processed {
                    Ok(processed) => {
                        delta.dml = Some(processed.delta);
                        list = Some(processed.list);
                        change = Some(processed.change);
                    }
                    Err(err) => return Ok(Err(SpecialTxFailure::Invalid(err))),
                }
            }
        }
        let t6 = bench("m_dmnman.ProcessBlock", &mut totals.dmn, t5);

        // Steps 8–10 (`stm:979-1029` at v24.0.0). Below DIP0003 steps 6 and 7 did nothing: the list
        // is Core's default `mn_list`, empty, and no commitment is mined (D57).
        if let Some(cbtx) = &cbtx {
            let (pre_dip3, no_rows);
            let (list, mined, llmq): (&MnList, &dyn MinedCommitmentReader<Error = StoreError>, _) =
                match (&list, &view, llmq) {
                    (Some(list), Some(view), Some(llmq)) => (list, view, llmq),
                    (None, None, None) => {
                        pre_dip3 = MnList::default_pre_dip3();
                        no_rows = NO_ROWS.view(&chain);
                        (&pre_dip3, &no_rows, llmq_ctx()?)
                    }
                    _ => return Err(Fatal::new("a CbTx's roots with DIP0003 half applied")),
                };
            match caches.sml.merkle_root(list) {
                Ok(root) => {
                    #[cfg(any(test, feature = "test-support"))]
                    edges.record_roots(|r| r.mn_list.push(h));
                    if let Err(err) = check_mn_list_root(cbtx, &root) {
                        return Ok(Err(SpecialTxFailure::Invalid(err)));
                    }
                }
                Err(err) => return Ok(Err(SpecialTxFailure::Invalid(err))),
            }
            let t6_1 = bench("CalcCbTxMerkleRootMNList", &mut totals.merkle_mnl, t6);

            let mut fault = None;
            let quorums = check_quorums_root(cbtx, || {
                #[cfg(any(test, feature = "test-support"))]
                edges.record_roots(|r| r.quorums.push(h));
                match quorum_root(&llmq, mined, qc_hashes, block, prev) {
                    Ok(root) => root,
                    Err(e) => {
                        fault = Some(e);
                        Err(BlockValidationError::new(BlockValidationResult::Consensus, ""))
                    }
                }
            });
            if let Some(e) = fault {
                return qc_failure(e).map(Err);
            }
            if let Err(err) = quorums {
                return Ok(Err(SpecialTxFailure::Invalid(err)));
            }
            let t6_2 = bench("CalcCbTxMerkleRootQuorums", &mut totals.merkle_quorums, t6_1);

            let step = ClStep {
                height: h,
                id: indexed(at)?,
                prev,
                v20_at_prev: cx
                    .deployments
                    .active_at(prev, Deployment::Buried(BuriedDeployment::V20)),
                prev_block: &|| blocks.read(cx.reader, prev),
                bls,
                cell: facade.cbtx_cl(),
            };
            let mut attempt = match (&binding, &mut sink) {
                (Some(binding), Some(sink)) => ClAttempt::bound(binding, &mut **sink, prev),
                // Below DIP0003 there is no binding, and no CbTx there reaches the verify: V20
                // follows DIP0003, so its version is below `CLSIG_AND_BALANCE`.
                _ => ClAttempt::detached(prev),
            };
            #[cfg(any(test, feature = "test-support"))]
            let cl_best = &mut || {
                let best = cl_best();
                if let Ok(lock) = &best {
                    edges.record_roots(|r| r.cl_asks.push((h, lock.as_ref().map(|l| l.height))));
                }
                best
            };
            let passed = check_cbtx_best_chainlock(cbtx, &step, cx, &mut attempt, cl_best, &**cl)?;
            #[cfg(any(test, feature = "test-support"))]
            if let Ok(pass @ (ClPass::BestAtParent | ClPass::OutdatedBest)) = passed {
                edges.record_roots(|r| r.cl_shortcuts.push((h, pass)));
            }
            if let Err(failure) = passed {
                return Ok(Err(failure));
            }
            #[cfg(test)]
            edges.record_roots(|r| r.steps.push((h, 10)));
            bench("CheckCbTxBestChainlock", &mut totals.cbtx_cl, t6_2);
        }

        // Step 11 (`stm:1031-1041` at v24.0.0), a proposal's too: it walks the dummy index
        // (MN0-2b-1), and its one write (`AddToCache`) is skipped under `fJustCheck`.
        let t7 = Instant::now();
        // BL-564: Core's call is made for a proposal too, and gets past its no-signal return
        // whenever the block's signals extract to at least one (`mnhftx.cpp:214-225`).
        #[cfg(any(test, feature = "test-support"))]
        {
            edges.bump("w.mnhf");
            if dash_dml::mnhf::extract_signals(block).is_ok_and(|bits| !bits.is_empty()) {
                edges.bump("g.mnhf");
            }
        }
        {
            let id = indexed(at)?;
            let no_rows = NO_ROWS.view(&chain);
            let store: &(dyn EvoStore + Sync) = match &view {
                Some(view) => view,
                None => &no_rows,
            };
            let mnhf = MnhfView {
                manager: &caches.mnhf,
                params,
                chain: &chain,
                store,
            };
            let scope = HookTry::open();
            let processed = mnhf.process_block(block, id, just_check);
            // Core: evo/mnhftx.cpp:257-260 at v24.0.0, inside H4's `try` (BL-787, CH5 D23).
            if let Some(what) = scope.caught() {
                tracing::info!("CMNHFManager::ProcessBlock -- failed: {what}");
                return Ok(Err(invalid("failed-proc-mnhf-inblock")));
            }
            match processed {
                Ok(processed) => delta.mnhf_signals = processed.row,
                Err(MnhfError::Invalid(err)) => return Ok(Err(SpecialTxFailure::Invalid(err))),
                Err(MnhfError::Store(e)) => {
                    return store_failure("CMNHFManager::ProcessBlock", &e).map(Err);
                }
            }
        }
        #[cfg(test)]
        edges.record_roots(|r| r.steps.push((h, 11)));
        bench("m_mnhfman.ProcessBlock", &mut totals.mnehf, t7);

        // The block's credit records (D10), written with it.
        if let (Some(view), false) = (&view, just_check) {
            let period = u32::try_from(params.credit_pool_period_blocks).ok();
            let window_start = match period.and_then(|p| h.checked_sub(p)) {
                Some(w) if v20 => {
                    let start = cx
                        .reader
                        .ancestor(prev, w)
                        .ok_or_else(|| Fatal::new("no credit window start in the index"))?;
                    let start_hash = crate::chain::index::entry(cx.reader, start).hash;
                    let dip3_at = cx
                        .deployments
                        .active_at(start, Deployment::Buried(BuriedDeployment::Dip0003));
                    match credit_data_at(view, &start_hash, dip3_at) {
                        Ok(data) => Some(data),
                        Err(e) => return store_failure("GetDataFromBlock", &e).map(Err),
                    }
                }
                _ => None,
            };
            let connect_at = ConnectAt {
                dip3,
                v20,
                eras: PoolEras {
                    v24: rules.v24,
                    withdrawals: after(BuriedDeployment::Withdrawals),
                },
            };
            let empty = CreditPool::default();
            let prev_pool = prev_pool.as_ref().unwrap_or(&empty);
            let records = connect_records(block, connect_at, prev_pool, window_start.as_ref());
            delta.credit_data = records.data;
            delta.credit_pool = records.pool.map(Arc::new);
        }

        // Step 12 (`stm:1043-1048` at v24.0.0): V19 active after the block, under the legacy
        // scheme. On a proposal that is a question about the dummy's child, which is a buried
        // height check (`ProposalDeployments` answers it so too).
        let mut post_scheme = None;
        if bls == BlsCtx::LEGACY && v19_after_block(params, h) {
            post_scheme = Some(BlsCtx::BASIC);
            tracing::info!(
                "CSpecialTxProcessor::ProcessSpecialTxsInBlock -- bls_legacy_scheme={}",
                legacy(BlsCtx::BASIC)
            );
        }

        // The view's snapshot ends before the transaction is touched.
        drop(view);
        let pending = self.pending()?;
        pending.delta = Some(delta);
        // A proposal's list is never offered (at v24.0.0, `specialtxman.cpp:973` skips
        // `ProcessBlock`), nor one built over a made-up parent list (OD-CP-51); H11 still hashes
        // the latter.
        pending.list = list.filter(|_| !just_check);
        pending.over_initial_snapshot = parent_made_up.get();
        pending.post_scheme = post_scheme;
        Ok(Ok(change))
    }
}

/// `DeploymentActiveAfter(pindex, V19)`: V19 is buried at `v19_height`.
fn v19_after_block(params: &ChainSetup, h: u32) -> bool {
    h.checked_add(1).is_some_and(|next| next >= params.consensus.v19_height.to_u32())
}

impl<E: KvEngine + 'static, C: Ev1Checks + Send> EvoHooks for NodeEvo<E, C>
where
    for<'a> GuardedSnapshot<'a, E>: Sync,
{
    const COMPLETE: bool = true;

    type Facade = MembersFacade;

    type Attempt<'v>
        = NodeEvoAttempt<'v>
    where
        Self: 'v;

    type ListUpdates = Option<MnListChange>;

    /// D1: no transaction outlives its attempt. Then the facade's gate, exclusively (EV addendum
    /// §2.6): a refused attempt opens no handle.
    fn begin_attempt<'v>(
        &mut self,
        facade: &'v MembersFacade,
        v: &'v mut VContext,
        cx: &'v ConnectCx<'_>,
        id: AttemptId,
        origin: AttemptOrigin,
        target: &ViewTarget<'_>,
    ) -> Result<NodeEvoAttempt<'v>, Fatal> {
        // BL-564 (OD-L3-43): a proposal's entries count the `p.` mirrors. Set on every attempt
        // first, so an attempt that ended without `finish_attempt` leaves no mode behind.
        #[cfg(any(test, feature = "test-support"))]
        self.edges.set_proposal(origin == AttemptOrigin::Proposal);
        #[cfg(not(any(test, feature = "test-support")))]
        let _ = origin;
        if self.pending.is_some() {
            return Err(Fatal::new("an evo transaction outlived its attempt (D1)"));
        }
        self.mn_list.reset();
        Ok(NodeEvoAttempt {
            handle: facade.begin_attempt(v, id),
            facade,
            log: Vec::new(),
            cx,
            chain: Ancestry(cx.reader),
            bls: None,
            committed: false,
            scratch: matches!(target, ViewTarget::Scratch(_)),
            undo: false,
            disconnect: false,
            unlogged: false,
        })
    }

    /// The log (D12), whose application releases the attempt, then `ConnectBlock`'s guard exit
    /// (D9): a connect that did not commit, or whose log failed before Core's `Commit()`, reverts
    /// the scheme it left at the failure, by the failed effect's phase (OD-CP-182 R1), when it is
    /// not its base, the published scheme or the walk's (`base`), and logs it unless the attempt
    /// or its log's application ended in one of Core's asserts or a coins read fault, where Core
    /// aborts (CH3b D231, OD-CP-184). A kept scratch connect leaves its scheme for the walk (R3).
    /// A disconnect's guard is its holder's (CH3a D192).
    /// A failed application trips the latch with its failure before the gate is released (D4).
    /// An undo's log writes its snapshots so that a failed one is the disconnect's to report
    /// (OD-CP-18).
    // Core: validation.cpp:162-167 ~ScopedBLSLegacyScheme, :2501-2502, :2927 (v24.0.0)
    fn finish_attempt(&mut self, attempt: NodeEvoAttempt<'_>) -> Result<(), Fatal> {
        #[cfg(any(test, feature = "test-support"))]
        self.edges.set_proposal(false);
        let id = attempt.handle.id();
        let applied = self.appliers.apply_log_as(id, &attempt.log, attempt.undo);
        if !attempt.disconnect {
            let base = self.base(attempt.scratch);
            let left = self.pending.as_ref().and_then(|p| p.post_scheme).or(attempt.bls);
            let (kept, scheme) = match &applied {
                Err((kind, _)) if attempt.committed => match connect_phase(*kind) {
                    ConnectPhase::BeforeSwitch => (false, attempt.bls),
                    ConnectPhase::AfterSwitch => (false, left),
                    ConnectPhase::AfterCommit => (true, left),
                },
                _ => (attempt.committed, left),
            };
            let scheme = scheme.unwrap_or(base);
            let unlogged = attempt.unlogged
                || matches!(&applied, Err((_, failure)) if failure.is_reported_assert());
            if !kept && scheme != base && !unlogged {
                log_reverted(base);
            }
            if attempt.scratch
                && kept
                && let Some(walk) = self.guard_of(SchemeGuardHolder::Walk)
            {
                walk.current = scheme;
            }
        }
        match applied {
            Ok(applied) => attempt.facade.release_attempt(attempt.handle, applied),
            Err((_, failure)) => Err(attempt.handle.fail(failure)),
        }
    }

    /// H1 (D9): the block's scheme, with its guard's "entered" line where it is not the scheme the
    /// guard saved: `ConnectBlock`'s or `DisconnectTip`'s. VerifyDB's disconnect holds no guard and
    /// enters nothing; its undo's switch is H9's (CH5 D1).
    // Core: validation.cpp:150-161, :2501-2502, :3283-3284; DisconnectBlock (:2293-2323), from
    // VerifyDB (:5125), holds none (v24.0.0)
    fn bls_ctx(
        &mut self,
        attempt: &mut NodeEvoAttempt<'_>,
        _: &mut EvoTarget<'_>,
        params: &ChainSetup,
        at: &BlockAt,
    ) -> BlsCtx {
        let bls = bls_ctx_for_block(params, at.height);
        let walk_undo = attempt.disconnect && attempt.scratch;
        if !walk_undo && bls != self.base(attempt.scratch) {
            tracing::info!("ScopedBLSLegacyScheme: entered bls_legacy_scheme={}", legacy(bls));
        }
        // `DisconnectTip`'s guard enters the block's scheme (`validation.cpp:3283` at v24.0.0).
        if attempt.disconnect
            && let Some(tip) = self.guard_of(SchemeGuardHolder::Tip)
        {
            tip.current = bls;
        }
        attempt.bls = Some(bls);
        bls
    }

    fn commit_bls_scheme(&mut self, attempt: &mut NodeEvoAttempt<'_>, _: &mut EvoTarget<'_>) {
        attempt.committed = true;
        if let Some(pending) = self.pending.as_mut() {
            pending.publish = pending.post_scheme.or(attempt.bls);
        }
    }

    fn mark_unlogged(&mut self, attempt: &mut NodeEvoAttempt<'_>) {
        attempt.unlogged = true;
    }

    fn mark_disconnect(&mut self, attempt: &mut NodeEvoAttempt<'_>) {
        attempt.disconnect = true;
        if let Some(guard) = self.guard.as_mut() {
            guard.entry = None;
        }
    }

    // Core: evo/specialtxman.cpp:1086-1088 at v24.0.0
    /// The catch restores the undo's entry scheme, which in VerifyDB's walk is the walk's: the
    /// next block's ([`undo_entry_scheme`](crate::connect)), CH5 D23.
    fn undo_caught(&mut self, entered: BlsCtx) {
        if let Some(guard) = self.guard.as_mut()
            && let Some(entry) = guard.entry
        {
            debug_assert_eq!(entry, entered, "the catch's line names the undo's entry scheme");
            guard.current = entry;
        }
    }

    // Core: validation.cpp:150-161 ScopedBLSLegacyScheme (v24.0.0)
    fn open_scheme_guard(&mut self, holder: SchemeGuardHolder) {
        debug_assert!(self.guard.is_none(), "a scheme guard open at the next open");
        self.guard = Some(SchemeGuard {
            holder,
            saved: self.scheme,
            current: self.scheme,
            entry: None,
            committed: false,
        });
    }

    // Core: validation.cpp:173, :3331, :3488 (v24.0.0)
    fn commit_scheme_guard(&mut self) {
        if let Some(guard) = self.guard.as_mut() {
            debug_assert_eq!(guard.holder, SchemeGuardHolder::Tip, "VerifyDB never commits");
            guard.committed = true;
        }
    }

    // Core: validation.cpp:162-167 ~ScopedBLSLegacyScheme (v24.0.0)
    /// Core's destructor stores the saved scheme back (OD-CP-182 R5), whether or not it logs.
    fn exit_scheme_guard(&mut self, log: bool) {
        let guard = self.guard.take();
        debug_assert!(guard.is_some(), "a scheme guard exited twice, or never opened");
        if let Some(guard) = guard
            && !guard.committed
            && guard.current != guard.saved
        {
            if log {
                log_reverted(guard.saved);
            }
            self.scheme = guard.saved;
        }
    }

    fn published_bls(&self) -> Option<BlsCtx> {
        Some(self.scheme)
    }

    fn evo_reads(&self) -> &dyn EvoReads {
        self
    }

    fn ehf_rows(&self) -> Option<Arc<dyn EhfRows>> {
        Some(Arc::new(CommittedEhfRows {
            store: Arc::clone(&self.store),
            layer: self.layer.clone(),
            manager: Arc::clone(&self.caches.mnhf),
            params: Arc::clone(&self.params),
            blocks: Arc::clone(&self.blocks),
        }))
    }

    fn scratch_layer(&self) -> Option<crate::evo::EvoLayer> {
        Some(self.layer.scratch())
    }

    /// H2 (D7): the evo best block through the layer, genesis read as absent.
    fn verify_best_block(
        &mut self,
        attempt: &mut NodeEvoAttempt<'_>,
        evo: &mut EvoTarget<'_>,
        hash: &BlockHash,
    ) -> Result<bool, Fatal> {
        let chain = Ancestry(attempt.cx.reader);
        let view = self.store.view(&*target_layer(&self.layer, evo)?, &chain);
        let best = EvoStore::best_block(&view).map_err(|e| store_fault("VerifyBestBlock", &e))?;
        Ok(best == Some(*hash))
    }

    fn process_special_txs(
        &mut self,
        attempt: &mut NodeEvoAttempt<'_>,
        evo: &mut EvoTarget<'_>,
        block: &Block,
        at: &BlockAt,
        rules: SpecialTxRules,
        view: &dyn CoinsView<Error = CoinsReadError>,
        subsidy: Amount,
        just_check: bool,
        check_cbtx_merkle_roots: bool,
        cl_best: &mut ClBest<'_>,
    ) -> Result<Result<Option<MnListChange>, SpecialTxFailure>, Fatal> {
        // An O(1) copy (`EvoLayer::scratch`), so the lock is released before H4 runs.
        let layer = target_layer(&self.layer, evo)?.scratch();
        self.special_txs(
            attempt,
            &layer,
            block,
            at,
            rules,
            view,
            subsidy,
            just_check,
            check_cbtx_merkle_roots,
            cl_best,
        )
    }

    /// H5 (§3.6): `IsTransactionValid` over the parent's list, and `IsBlockPayeeValid`'s line.
    fn mn_payment_valid(
        &mut self,
        attempt: &mut NodeEvoAttempt<'_>,
        evo: &mut EvoTarget<'_>,
        coinbase: &Tx,
        prev: Option<BlockId>,
        subsidy: Amount,
        fees: Amount,
        era: MnRewardEra,
        strict: bool,
    ) -> Result<bool, Fatal> {
        let cx = attempt.cx;
        let prev = prev.ok_or_else(|| Fatal::new("IsTransactionValid at genesis"))?;
        let height = height_i32(crate::chain::index::entry(cx.reader, prev).height + 1)?;
        let chain = Ancestry(cx.reader);
        let view = self.store.view(&*target_layer(&self.layer, evo)?, &chain);
        let list = resolve_list(prev, &chain, cx.deployments, &view, &self.caches.lists)
            .map_err(|e| store_fault("GetListForBlock", &e))?;
        let payee = PayeeRules {
            v19: buried_after(cx, Some(prev), BuriedDeployment::V19),
            mn_rr: buried_after(cx, Some(prev), BuriedDeployment::MnRr),
        };
        let reward = BlockReward {
            height,
            block_subsidy: subsidy,
            fee_reward: fees,
            era,
        };
        let valid = masternode_payee_valid(coinbase, &list, payee, &reward, strict, &self.payments)
            .map_err(|e| Fatal::assert(format!("IsTransactionValid: {e:?}")))?;
        if !valid {
            tracing::info!(
                "CMNPaymentsProcessor::IsBlockPayeeValid -- ERROR! Invalid masternode payment \
                 detected at height {height}: {}",
                coinbase.txid()
            );
        }
        Ok(valid)
    }

    /// H6 (D7): the transaction's evo tip.
    fn set_best_block(
        &mut self,
        _: &mut NodeEvoAttempt<'_>,
        evo: &mut EvoTarget<'_>,
        hash: &BlockHash,
    ) -> Result<(), Fatal> {
        check_target(evo)?;
        let delta = self.pending()?.delta.as_mut().ok_or_else(|| {
            Fatal::new("WriteBestBlock before ProcessSpecialTxsInBlock or UndoSpecialTxsInBlock")
        })?;
        delta.evo_tip = Some(*hash);
        Ok(())
    }

    /// H7 (D15): only a change whose diff has one, as Core's condition; `just_check` produces
    /// none. An Active attempt's listeners run inline; a scratch attempt's signal goes through
    /// its log, applied with it.
    // Core: validation.cpp:2379-2382 (disconnect, v24.0.0), :2929-2932 (connect, v24.0.0)
    fn list_changed(
        &mut self,
        attempt: &mut NodeEvoAttempt<'_>,
        updates: &Option<MnListChange>,
        undo: bool,
    ) {
        let Some(change) = updates.as_ref().filter(|c| c.diff.has_changes()) else {
            return;
        };
        if !attempt.scratch {
            self.mn_list.notify(undo, change);
            return;
        }
        // The staged delta's block: H4's or H9's. H7 follows H6, so one is staged; without one the
        // effect goes unqueued and its application fails closed.
        let at = self.pending.as_ref().and_then(|p| p.delta.as_ref()).map(|d| d.block);
        debug_assert!(at.is_some(), "H7 follows H6, which staged the block's delta");
        attempt.log.push(match at {
            Some(at) => self.mn_list.defer(at, undo, change),
            None => Effect::NotifyMnListChanged {
                at: *change.new.block_hash(),
                undo,
            },
        });
    }

    /// H8 (EV1-3).
    fn shared_collateral_spends(
        &mut self,
        _: &mut NodeEvoAttempt<'_>,
        tx: &Tx,
        view: &dyn CoinsView<Error = CoinsReadError>,
    ) -> Result<Result<(), TxValidationError>, Fatal> {
        self.ev1.shared_collateral_spends(tx, view)
    }

    /// H9, `UndoSpecialTxsInBlock` (`stm:1061-1094` at v24.0.0): MNHF, the list, the quorum
    /// processor. Its switch to the legacy scheme below V19 never fires under `DisconnectTip`'s
    /// guard, whose H1 has entered the block's scheme, legacy there (D9); in VerifyDB's walk it
    /// moves the walk guard's scheme and logs Core's line, once, at the last block without V19 (CH5
    /// D1). A store fault is `Fatal`: Core's undo reads no verdict.
    fn undo_special_txs(
        &mut self,
        attempt: &mut NodeEvoAttempt<'_>,
        evo: &mut EvoTarget<'_>,
        block: &Block,
        at: &BlockAt,
    ) -> Result<Option<Option<MnListChange>>, Fatal> {
        attempt.undo = true;
        let cx = attempt.cx;
        let bls = attempt.bls.ok_or_else(|| Fatal::new("UndoSpecialTxsInBlock before H1"))?;
        // Core: evo/specialtxman.cpp:1065 (v24.0.0), the entry scheme; :1068-1073, the switch to
        // legacy for a block without V19 against it, whatever the guard: `DisconnectTip`'s has
        // entered legacy already, so only VerifyDB's walk switches (CH5 D1), before the undo's
        // reads (:1075).
        if let Some(guard) = self.guard.as_mut() {
            guard.entry = Some(guard.current);
            if bls == BlsCtx::LEGACY && guard.current != BlsCtx::LEGACY {
                guard.current = BlsCtx::LEGACY;
                tracing::info!(
                    "CSpecialTxProcessor::UndoSpecialTxsInBlock -- bls_legacy_scheme={}",
                    legacy(guard.current)
                );
            }
        }
        let id = at.id.ok_or_else(|| Fatal::new("UndoSpecialTxsInBlock without an index entry"))?;
        let prev = at.prev.ok_or_else(|| Fatal::new("UndoSpecialTxsInBlock at genesis"))?;
        let dip3 = buried_after(cx, at.prev, BuriedDeployment::Dip0003);
        let fault = |what: &str, e: StoreError| store_fault(what, &e);
        let chain = Ancestry(cx.reader);
        // At every height: the quorum processor's precompute reads the members below DIP0003 too.
        let view = self.store.view(&*target_layer(&self.layer, evo)?, &chain);
        let store: &(dyn EvoStore + Sync) = &view;

        let mnhf = MnhfView {
            manager: &self.caches.mnhf,
            params: &self.params,
            chain: &chain,
            store,
        };
        if !mnhf.undo_block(block, id).map_err(|e| fault("CMNHFManager::UndoBlock", e))? {
            return Ok(None);
        }

        // `CDeterministicMNManager::UndoBlock` (`dmn:784-817` at v24.0.0), in Core's shape: the
        // stored diff first, a missing one read as empty (Core ignores `Read`'s false, `:793`);
        // only a diff with changes resolves `pprev`'s list (`:795-798`), and the block's list is
        // that list with the diff applied (`:803-805`). A DIP3 block without a diff row, as a store
        // that predates the DIP3 height has (BL-650), undoes nothing and resolves no list. No row
        // is erased (D6). Core reads the row ungated (`dmn:793-805` at v24.0.0); below DIP0003 the
        // node reads none (D23).
        let mut change = None;
        if dip3 {
            let stored = store.mn_list_diff(&at.hash).map_err(|e| fault("GetListDiff", e))?;
            if let Some(stored) = stored.filter(|stored| stored.diff.has_changes()) {
                // A row at another height is the walk's corruption fail-stop (A1 I5): Core's
                // `ApplyDiff(pindex, diff)` takes the height from `pindex`, so nothing in Core
                // reads it (OD-CP-51 R2-N1).
                if i64::from(stored.height) != i64::from(at.height) {
                    let e = StoreError::inconsistent(format!(
                        "mn_diff row for block {} has height {}, the block {}",
                        at.hash, stored.height, at.height
                    ));
                    return Err(fault("UndoBlock", e));
                }
                let lists = &self.caches.lists;
                let prev_list = resolve_list(prev, &chain, cx.deployments, store, lists)
                    .map_err(|e| fault("GetListForBlock", e))?;
                let mut cur = (*prev_list).clone();
                cur.apply_record(at.hash, &stored).map_err(|e| {
                    store_fault(&format!("UndoBlock: mn_diff row for block {}", at.hash), &e)
                })?;
                change = dash_dml::undo_block(&Arc::new(cur), &prev_list, &stored.diff);
            }
        }

        // `CQuorumBlockProcessor::UndoBlock` through the attempt's binding (route C). Its
        // precompute runs at every height, as Core's does (`bp:523`): below DIP0003 the list is
        // empty, so the rotated members it computes at a cycle base are too (X13).
        let complete = CompleteChain::<StoreError>::new(&chain);
        let llmq = LlmqCtx::new(&self.params, &complete, cx.deployments, bls)
            .map_err(|e| Fatal::new(format!("LLMQ parameters: {e:?}")))?;
        let (binding, sink) =
            attempt.bind(store, &self.params, &self.caches.lists, &*self.blocks)?;
        let qc = binding.qc(llmq);
        let erased_mined = match dash_dml::blockprocessor::undo_block(&qc, block, id, sink) {
            Ok(erased) => erased,
            Err(QcBlockError::NoReason(_)) => return Ok(None),
            Err(QcBlockError::Fault(e)) => {
                return Err(store_fault("CQuorumBlockProcessor::UndoBlock", &e));
            }
            Err(e) => {
                return Err(Fatal::abort_node(format!("CQuorumBlockProcessor::UndoBlock: {e:?}")));
            }
        };

        // The mined rows' erases, in the disconnect's evo transaction (`bp:536-548`, CH7 §4.1.4).
        drop(view);
        self.pending()?.delta = Some(EvoDelta {
            erased_mined,
            ..empty_delta(at)
        });
        Ok(Some(change))
    }

    /// H10: opens the Active target's transaction (D1).
    fn begin_tx(
        &mut self,
        _: &mut NodeEvoAttempt<'_>,
        evo: &mut EvoTarget<'_>,
    ) -> Result<(), Fatal> {
        check_target(evo)?;
        if self.pending.is_some() {
            return Err(Fatal::new("an evo transaction began twice (D1)"));
        }
        self.pending = Some(PendingTx::default());
        Ok(())
    }

    /// H10, `CommitCurTransaction`: the delta into the target's layer (a scratch one is never
    /// flushed), then the published scheme (D9) and the connected list into the cache (§3.7),
    /// which Core's VerifyDB fills too. A scratch commit publishes no scheme: VerifyDB's own guard
    /// is never committed, so the scheme after it is the one before it (D61).
    // Core: validation.cpp:5056 (VerifyDB's ScopedBLSLegacyScheme), :162-168
    fn commit_tx(&mut self, evo: &mut EvoTarget<'_>) -> Result<(), Fatal> {
        let pending =
            self.pending.take().ok_or_else(|| Fatal::new("commit_tx without begin_tx (D1)"))?;
        if let Some(delta) = &pending.delta {
            self.store
                .commit(&mut *target_layer(&self.layer, evo)?, delta)
                .map_err(|e| store_fault("CommitCurTransaction", &e))?;
        }
        if let Some(scheme) = pending.publish.filter(|_| matches!(evo, EvoTarget::Active)) {
            self.scheme = scheme;
            if let Some(tip) = self.guard_of(SchemeGuardHolder::Tip) {
                tip.current = scheme;
            }
        }
        if let Some(list) = pending.list.as_ref().filter(|_| !pending.over_initial_snapshot) {
            self.caches.lists.offer(list, CacheHint::Tip);
        }
        Ok(())
    }

    fn rollback_tx(&mut self, _: &mut EvoTarget<'_>) {
        self.pending = None;
    }

    /// H11 (O8 §5.2): the block's evo keys from what this attempt staged (CH7a-5b X2), for
    /// either target: the new list H4 left and its delta's commitments, pool and MNHF row. The
    /// eras are the deployments active at the block; genesis has none (`v:2545-2551` at v24.0.0
    /// returns before any tap).
    fn digest_fields(
        &mut self,
        attempt: &mut NodeEvoAttempt<'_>,
        evo: &mut EvoTarget<'_>,
        at: &BlockAt,
    ) -> Result<EvoDigest, Fatal> {
        check_target(evo)?;
        if at.prev.is_none() {
            return Ok(EvoDigest::default());
        }
        let eras = DigestEras {
            dip3: buried_after(attempt.cx, at.prev, BuriedDeployment::Dip0003),
            v20: buried_after(attempt.cx, at.prev, BuriedDeployment::V20),
        };
        let pending = self.pending.as_ref();
        let list = pending.and_then(|p| p.list.as_deref());
        let delta = pending.and_then(|p| p.delta.as_ref());
        #[cfg(any(test, feature = "test-support"))]
        if self.h11_pool_error_at == Some(at.height) {
            // The plant reads a copy: the staged delta is committed as it is.
            let planted = delta.map(|d| dash_dml::EvoDelta {
                credit_pool: Some(Arc::new(Err(dash_dml::creditpool::PoolError::IndexDuplicated))),
                ..d.clone()
            });
            return evo_digest(eras, list, planted.as_ref());
        }
        evo_digest(eras, list, delta)
    }
}

/// V's versionbits' MNHF signals (DEC-1028): `GetForBlock` over the layer [`NodeEvo`] commits into
/// and its store, with the node's one MNHF manager (D16) and its block reads for the rebuild.
pub struct CommittedEhfRows<E: KvEngine> {
    store: Arc<RocksEvoStore<E>>,
    layer: SharedEvoLayer,
    manager: Arc<MnhfManager>,
    params: Arc<ChainSetup>,
    blocks: Arc<dyn EvoBlocks>,
}

impl<E: KvEngine + 'static> EhfRows for CommittedEhfRows<E>
where
    for<'a> GuardedSnapshot<'a, E>: Sync,
{
    fn get_for_block(
        &self,
        reader: &ChainReader,
        block: BlockId,
    ) -> Result<EhfSignals, GetForBlockError> {
        let chain = Ancestry(reader);
        // No re-entry: the layer's mutex is not reentrant, so V must not hold it here. It never
        // does across a deployments read: H4 runs on a scratch copy, `commit_tx` reads none, and
        // every view is built in one statement, as this one is, whose guard ends with it.
        let view = self.store.view(&self.layer.lock(), &chain);
        let mnhf = MnhfView {
            manager: &self.manager,
            params: &self.params,
            chain: &chain,
            store: &view,
        };
        mnhf.get_for_block_from_disk(block, &|id| self.blocks.read(reader, id))
    }
}

/// V's reads over the active layer (D16, D17), each through a view pinned for the call.
impl<E: KvEngine + 'static, C: Ev1Checks> EvoReads for NodeEvo<E, C>
where
    for<'a> GuardedSnapshot<'a, E>: Sync,
{
    fn with_llmq(
        &self,
        cx: EvoReadCx<'_>,
        bls: BlsCtx,
        call: &mut LlmqCall<'_>,
    ) -> Result<(), OnVMembersError> {
        let chain = Ancestry(cx.reader);
        let complete = CompleteChain::<StoreError>::new(&chain);
        let ctx = LlmqCtx::new(&self.params, &complete, cx.deployments, bls)
            .map_err(OnVMembersError::Params)?;
        let view = self.store.view(&self.layer.lock(), &chain);
        let src = NodeLlmqSource::new(
            &chain,
            cx.deployments,
            &view,
            &self.caches.lists,
            &*self.blocks,
            cx.reader,
        );
        Ok(call(TipLlmq {
            ctx: &ctx,
            src: &src,
            mined: &view,
            index: &chain,
        })?)
    }

    fn list_at(&self, cx: EvoReadCx<'_>, at: BlockId) -> Result<Arc<MnList>, StoreError> {
        let chain = Ancestry(cx.reader);
        let view = self.store.view(&self.layer.lock(), &chain);
        resolve_list(at, &chain, cx.deployments, &view, &self.caches.lists)
    }

    fn credit_pool_after(
        &self,
        cx: EvoReadCx<'_>,
        at: BlockId,
    ) -> Result<Option<Arc<PoolState>>, StoreError> {
        let v20 = Deployment::Buried(BuriedDeployment::V20);
        if !cx.deployments.active_after(Some(at), v20) {
            return Ok(None);
        }
        let chain = Ancestry(cx.reader);
        let view = self.store.view(&self.layer.lock(), &chain);
        let hash = crate::chain::index::entry(cx.reader, at).hash;
        let pool = self.caches.pools.pool_after(&view, &hash, cx.deployments.active_at(at, v20))?;
        Ok(Some(Arc::new(pool)))
    }

    fn credit_pool_info(
        &self,
        cx: EvoReadCx<'_>,
        at: BlockId,
        window_blocks: i32,
    ) -> Result<Result<CreditPoolInfo, PoolError>, StoreError> {
        /// `id`'s entry `e` as the pool reads take it: DIP0003 and V20 active at it.
        fn pool_at<'e>(d: &dyn DeploymentView, id: BlockId, e: &'e EntryView) -> PoolBlockAt<'e> {
            let active = |b| d.active_at(id, Deployment::Buried(b));
            PoolBlockAt {
                hash: &e.hash,
                height: e.height,
                dip3: active(BuriedDeployment::Dip0003),
                v20: active(BuriedDeployment::V20),
            }
        }
        let entry = |id| (id, crate::chain::index::entry(cx.reader, id));
        let at_entry = entry(at);
        let start_entry = credit_window_start(cx.reader, at, window_blocks)?.map(entry);
        let chain = Ancestry(cx.reader);
        let view = self.store.view(&self.layer.lock(), &chain);
        let block_at = pool_at(cx.deployments, at, &at_entry.1);
        let window_at = start_entry.as_ref().map(|(id, e)| pool_at(cx.deployments, *id, e));
        // Not through `self.caches.pools`: an RPC read fills no connect-path cache, and the pool
        // it reads is the one stored at connect.
        credit_pool_info(&view, block_at, window_at, window_blocks)
    }

    fn has_mined_commitment(
        &self,
        cx: EvoReadCx<'_>,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
        tip: BlockId,
    ) -> Result<bool, StoreError> {
        let chain = Ancestry(cx.reader);
        let view = self.store.view(&self.layer.lock(), &chain);
        Ok(view.mined_commitment(llmq_type, quorum_hash, tip)?.is_some())
    }

    fn coinbase_chainlock_at(
        &self,
        cx: EvoReadCx<'_>,
        at: BlockId,
        bls: BlsCtx,
    ) -> Result<Option<u32>, StoreError> {
        let height = crate::chain::index::entry(cx.reader, at).height;
        let facts = CoinbaseFacts::of(self.blocks.read(cx.reader, at).as_deref());
        let v20 = cx.deployments.active_at(at, Deployment::Buried(BuriedDeployment::V20));
        Ok(nonnull_coinbase_chainlock(&facts, height, v20, bls).map(|(_, diff)| diff))
    }
}

/// The node's NetInfo rules (`SetStateVersion`, `Validate()`): `MainParams()` at `params`' release
/// and `-cjdnsreachable` as step 6 leaves it (D51).
///
/// # Errors
///
/// Mainnet's parameters at the release that do not build.
pub fn node_net_info(params: &ChainSetup, cjdns_reachable: bool) -> Result<NetInfoRules, Fatal> {
    let mainnet = dash_params::ChainParamsBuilder::new(dash_params::Network::Main, params.release)
        .build()
        .map_err(|e| Fatal::new(format!("mainnet parameters: {e:?}")))?;
    Ok(NetInfoRules::new(
        params,
        &mainnet,
        NetInfoConfig {
            cjdns_reachable,
        },
    ))
}

/// The node's evo codec: Core's `Validate()` under `net_info` (EV2 §1.3).
pub fn node_codec(net_info: NetInfoRules) -> EvoCodec {
    EvoCodec::new(Arc::new(CoreNetInfoValidity {
        rules: net_info,
    }))
}
