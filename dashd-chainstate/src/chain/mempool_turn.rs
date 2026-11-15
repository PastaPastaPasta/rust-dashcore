//! The chainstate context for serial admission and held-pool reorg filtering.

use std::sync::Arc;

use dash_chain::{ChainReader, CompleteAncestry, EntryView};
use dash_clock::Clock;
use dash_consensus::coins::Coin;
use dash_consensus::ctx::{Deployment, DeploymentView};
use dash_consensus::script::ScriptFlags;
use dash_consensus::{BlockId, BlsCtx, ChainAncestry, TxRef};
use dash_dml::MnList;
use dash_dml::creditpool::PoolState;
use dash_primitives::{BlockHash, OutPoint};

use super::seams::{
    AtmpCoinsCall, BlockConnector, DisconnectPool, EvoReadCx, Fatal, LlmqCall, RemovalReason,
};
use super::signals::{Staged, ValidationSignals};
use crate::connect::{BlockAt, SpecialTxRules};
use crate::sigcache::ScriptCaches;

/// The closure returns with its pool guard released and its events already queued.
pub type MempoolCommand = Box<dyn FnOnce(&mut MempoolTurn<'_>) -> MempoolOutcome + Send>;
pub type MempoolOutcome = Result<MempoolCompletion, Fatal>;

/// Deferred completion: V uncaches, runs PERIODIC and ends the scope before invoking `reply`.
pub struct MempoolCompletion {
    pub uncache: Vec<OutPoint>,
    pub reply: Box<dyn FnOnce() + Send>,
}

/// Shared input to admission's and reorg filtering's lock-point calculations.
pub trait TipFacts {
    fn tip(&self) -> Option<BlockId>;
    fn ancestry(&self) -> &dyn ChainAncestry;

    fn tip_height(&self) -> Option<u32> {
        self.tip().map(|tip| self.ancestry().header(tip).height)
    }

    fn tip_mtp(&self) -> i64 {
        self.tip().map_or(0, |tip| self.ancestry().header(tip).mtp)
    }
}

pub trait ReorgCx: TipFacts {
    /// Core's `TestLockPointValidity`/`Contains` binding (`validation.cpp:476-518` at v24.0.0).
    fn chain_contains(&self, block: BlockId) -> bool;
    /// Core's `AccessCoin`/`FetchCoin` binding (`validation.cpp:476-518` at v24.0.0). `None` is
    /// absent/spent; a read error aborts the hold without a verdict or publication.
    fn access_coin(&mut self, outpoint: &OutPoint) -> Result<Option<Coin>, Fatal>;
}

/// Owns the pool write guard. Events join the hold's single stream at each mutation.
pub trait HeldMempool {
    /// Core `DynamicMemoryUsage` (`txmempool.cpp:1984-1988` at v24.0.0), sampled while V owns the
    /// pool write guard and before it enters the coins cell.
    fn dynamic_memory_usage(&self) -> u64;
    /// Core `removeForBlock(vtx, nHeight)` (`txmempool.cpp:1321-1350` at v24.0.0), including the
    /// deterministic-masternode-list lookup used by ProTx conflict removal. Implementations must
    /// stage each removal in call order.
    fn remove_for_block_at(
        &mut self,
        vtx: &[TxRef],
        height: u32,
        tip_list: &Arc<MnList>,
        staged: &mut Staged,
    );
    /// Core's `removeExpiredAssetUnlock(nHeight)` (`validation.cpp:3480` at v24.0.0).
    fn remove_expired_asset_unlock(&mut self, height: u32, staged: &mut Staged);
    /// Core `removeRecursive(tx, reason)` (`txmempool.cpp:907-935` at v24.0.0).
    fn remove_recursive(&mut self, tx: &TxRef, reason: RemovalReason, staged: &mut Staged);
    /// Core's `AddTransactionsUpdated` (`txmempool.cpp:422-425`, called at
    /// `validation.cpp:3220-3222` at v24.0.0).
    fn add_transactions_updated(&mut self, n: u32, staged: &mut Staged);
    /// Core's `CTxMemPool::check(coins_tip, nSpendHeight)` (`validation.cpp:3690` at v24.0.0).
    fn check(&mut self, next_height: u32, staged: &mut Staged);
    /// `Some(tip_list)` encodes adding; `None` encodes removal only and reads no list.
    /// Core `MaybeUpdateMempoolForReorg(disconnectpool, fAddToMempool)` (`validation.cpp:425-525`
    /// at v24.0.0, calling `removeForReorg`, `txmempool.cpp:937-955`, at `:522`). `tip_list` is `Some` for an adding drain and is
    /// the list at V's moved DMN tip; a non-adding drain reads no list.
    fn maybe_update_for_reorg_at(
        &mut self,
        pool: Box<dyn DisconnectPool>,
        tip_list: Option<&Arc<MnList>>,
        cx: &mut dyn ReorgCx,
        staged: &mut Staged,
    ) -> Result<(), Fatal>;
}

pub(crate) struct ReorgTurn<'a> {
    pub(crate) tip: Option<BlockId>,
    pub(crate) ancestry: CompleteAncestry,
    pub(crate) connector: &'a mut dyn BlockConnector,
}

impl TipFacts for ReorgTurn<'_> {
    fn tip(&self) -> Option<BlockId> {
        self.tip
    }
    fn ancestry(&self) -> &dyn ChainAncestry {
        &self.ancestry
    }
}

impl ReorgCx for ReorgTurn<'_> {
    fn chain_contains(&self, block: BlockId) -> bool {
        self.tip.is_some_and(|tip| {
            self.ancestry.ancestor(tip, self.ancestry.header(block).height) == Some(block)
        })
    }

    fn access_coin(&mut self, outpoint: &OutPoint) -> Result<Option<Coin>, Fatal> {
        let mut coin = None;
        self.connector.with_atmp_coins(&mut |coins| {
            coin = coins.access(outpoint)?;
            Ok(())
        })?;
        Ok(coin)
    }
}

/// Coherent chain facts for one class-M admission. All methods are synchronous; no pool guard
/// may survive the admission closure or a wait for V.
pub struct MempoolTurn<'a> {
    pub(crate) reorg: ReorgTurn<'a>,
    pub(crate) reader: &'a ChainReader,
    pub(crate) deployments: &'a dyn DeploymentView,
    pub(crate) params: &'a dash_params::ChainSetup,
    pub(crate) mn_list: Arc<MnList>,
    pub(crate) credit_pool: Option<Arc<PoolState>>,
    pub(crate) bls: BlsCtx,
    pub(crate) best_header: BlockId,
    pub(crate) ibd: bool,
    pub(crate) clock: &'a dyn Clock,
    pub(crate) signals: &'a ValidationSignals,
}

impl TipFacts for MempoolTurn<'_> {
    fn tip(&self) -> Option<BlockId> {
        self.reorg.tip()
    }
    fn ancestry(&self) -> &dyn ChainAncestry {
        self.reorg.ancestry()
    }
}

impl ReorgCx for MempoolTurn<'_> {
    fn chain_contains(&self, block: BlockId) -> bool {
        self.reorg.chain_contains(block)
    }
    fn access_coin(&mut self, outpoint: &OutPoint) -> Result<Option<Coin>, Fatal> {
        self.reorg.access_coin(outpoint)
    }
}

impl MempoolTurn<'_> {
    pub fn with_atmp_coins(&mut self, f: &mut AtmpCoinsCall<'_>) -> Result<Option<()>, Fatal> {
        self.reorg.connector.with_atmp_coins(f)
    }
    pub fn block_script_flags(&self) -> Option<ScriptFlags> {
        self.tip().map(|tip| dash_consensus::connect::block_script_flags(self.deployments, tip))
    }
    pub fn deployment_active_after(&self, deployment: Deployment) -> Result<bool, Fatal> {
        self.deployments
            .try_active_after(self.tip(), deployment)
            .map_err(super::deployments::ehf_fault)
    }
    pub fn special_tx_rules(&self) -> Result<SpecialTxRules, Fatal> {
        SpecialTxRules::for_block(
            self.params,
            self.deployments,
            &BlockAt {
                hash: BlockHash::from([0; 32]),
                height: self.tip_height().map_or(0, |height| height + 1),
                prev: self.tip(),
                id: None,
            },
        )
        .map_err(super::deployments::ehf_fault)
    }
    pub fn mn_list(&self) -> &Arc<MnList> {
        &self.mn_list
    }
    pub fn list_at_block(&self, block: BlockId) -> Result<Arc<MnList>, Fatal> {
        self.reorg
            .connector
            .evo_reads()
            .list_at(self.read_cx(), block)
            .map_err(|e| crate::evo::node::store_fault("GetListForBlock", &e))
    }
    pub fn credit_pool(&self) -> Option<&Arc<PoolState>> {
        self.credit_pool.as_ref()
    }
    pub fn bls(&self) -> BlsCtx {
        self.bls
    }
    /// The binding contains V's full ancestry, so the caller can select any active-chain block.
    pub fn with_llmq(
        &self,
        call: &mut LlmqCall<'_>,
    ) -> Result<(), super::command::OnVMembersError> {
        self.reorg.connector.evo_reads().with_llmq(self.read_cx(), self.bls, call)
    }
    pub fn script_caches(&self) -> Option<Arc<ScriptCaches>> {
        self.reorg.connector.script_caches()
    }
    pub fn is_ibd(&self) -> bool {
        self.ibd
    }
    pub fn best_header(&self) -> EntryView {
        super::index::entry(self.reader, self.best_header)
    }
    pub fn clock(&self) -> &dyn Clock {
        self.clock
    }
    pub fn signals(&self) -> &ValidationSignals {
        self.signals
    }
    fn read_cx(&self) -> EvoReadCx<'_> {
        EvoReadCx {
            reader: self.reader,
            deployments: self.deployments,
        }
    }
}

/// Debug-build lock ranks shared with pool implementations. A token lives as long as its guard.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LockRank {
    V,
    Mempool,
    InstantSend,
    Coins,
}

thread_local! {
    static LOCKS: std::cell::RefCell<Vec<LockRank>> = const { std::cell::RefCell::new(Vec::new()) };
}

pub struct LockOrderGuard {
    rank: LockRank,
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl LockOrderGuard {
    pub fn enter(rank: LockRank) -> Self {
        if cfg!(debug_assertions) {
            LOCKS.with_borrow_mut(|held| {
                assert!(
                    held.last().is_none_or(|last| {
                        *last < rank || (*last == rank && rank != LockRank::Mempool)
                    }),
                    "lock order: {held:?} -> {rank:?}"
                );
                held.push(rank);
            });
        }
        Self {
            rank,
            _thread: std::marker::PhantomData,
        }
    }
}

impl Drop for LockOrderGuard {
    fn drop(&mut self) {
        if cfg!(debug_assertions) {
            LOCKS.with_borrow_mut(|held| {
                assert_eq!(held.pop(), Some(self.rank));
            });
        }
    }
}
