//! What one LLMQ reading takes (LQ1 §2.3, EV addendum §2.2): the chain parameters, a fallible
//! chain view, deployment state, the caller's BLS scheme context and the per-block state source.

use std::fmt;
use std::sync::Arc;

use dash_consensus::BlockId;
use dash_consensus::ctx::DeploymentView;
use dash_params::{ChainParams, LlmqParams};
use dash_primitives::{BlockHash, LlmqType};
use dash_types::Numeric;

use super::chain::LlmqChain;
use super::members::MemberError;
use super::snapshot::QuorumSnapshot;
use super::view::MnListView;
use crate::bls::BlsCtx;
use crate::cbtx::CoinbaseFacts;

/// An LLMQ computation's error: [`MemberError`] (EV addendum §2.2; LQ1's `LlmqError` had the
/// same variants).
pub type LlmqError<E> = MemberError<E>;

/// One reading context (LQ1 §2.3). Runtime callers build it from the tip view; validation from
/// its consensus context. `E` is the chain view's error, the one error type of every call that
/// takes the context (EV addendum §2.2).
///
/// LQ1 §2.3's `rules` field is not here: the only reader was the coinbase-ChainLock predicate,
/// which takes the scheme context alone.
pub struct LlmqCtx<'a, E> {
    params: &'a ChainParams,
    chain: &'a dyn LlmqChain<Error = E>,
    deployments: &'a dyn DeploymentView,
    bls: BlsCtx,
}

impl<E> Clone for LlmqCtx<'_, E> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<E> Copy for LlmqCtx<'_, E> {}

impl<'a, E> LlmqCtx<'a, E> {
    /// Builds the context and checks the parameters' domain once (LQ1 B-n4, §3.10): every
    /// configured type has a positive `dkgInterval` (Core divides by it), positive
    /// `keepOldConnections`, `keepOldKeys` and `signingActiveQuorumCount` (the scans' and the
    /// quorum caches' sizes) and a scan-cache size in `1..=i32::MAX` (Core's `int`), and every
    /// rotated type has `signingActiveQuorumCount <= 65536` (the `log2` domain) and a size that is
    /// not negative (Core's quarter size is the `size_t` of it, quartered). Every table Core ships
    /// is inside it.
    pub fn new(
        params: &'a ChainParams,
        chain: &'a dyn LlmqChain<Error = E>,
        deployments: &'a dyn DeploymentView,
        bls: BlsCtx,
    ) -> Result<Self, ParamsOutOfDomain> {
        for p in &params.consensus.llmqs {
            let count = p.signing_active_quorum_count;
            let window = i64::from(p.dkg_mining_window_end) - i64::from(p.dkg_mining_window_start);
            let scan_cache = i64::from(p.max_cycles(p.keep_old_connections)) * window;
            if p.dkg_interval < 1
                || p.keep_old_connections < 1
                || p.keep_old_keys < 1
                || count < 1
                || !(1..=i64::from(i32::MAX)).contains(&scan_cache)
                || (p.use_rotation && (count > 65_536 || p.size < 0))
            {
                return Err(ParamsOutOfDomain(p.llmq_type));
            }
        }
        Ok(LlmqCtx {
            params,
            chain,
            deployments,
            bls,
        })
    }

    /// The chain parameters.
    pub fn params(&self) -> &'a ChainParams {
        self.params
    }

    /// The chain view.
    pub fn chain(&self) -> &'a dyn LlmqChain<Error = E> {
        self.chain
    }

    /// Deployment state.
    pub fn deployments(&self) -> &'a dyn DeploymentView {
        self.deployments
    }

    /// The caller's BLS scheme context (LQ1 §3.3): `for_block(h)` in validation, the tip's
    /// elsewhere.
    pub fn bls(&self) -> BlsCtx {
        self.bls
    }

    /// `chainparams.GetLLMQ(type)`. Core asserts that a type it computes members for is
    /// configured; here an unconfigured type is [`MemberError::UnknownLlmqType`].
    pub fn llmq(&self, llmq_type: LlmqType) -> Result<&'a LlmqParams, LlmqError<E>> {
        self.params
            .consensus
            .llmq(llmq_type)
            .ok_or(MemberError::UnknownLlmqType(llmq_type.to_base()))
    }

    /// `pindex->nHeight`.
    pub(crate) fn height(&self, id: BlockId) -> Result<u32, LlmqError<E>> {
        Ok(self.chain.header(id).map_err(MemberError::View)?.height)
    }

    /// `pindex->GetBlockHash()`.
    pub(crate) fn hash(&self, id: BlockId) -> Result<BlockHash, LlmqError<E>> {
        Ok(self.chain.header(id).map_err(MemberError::View)?.hash)
    }

    /// `pindex->GetAncestor(height)` for a height at or below `id`'s: a view that answers
    /// `Ok(None)` there breaks its contract, and the block counts as not held.
    pub(crate) fn ancestor_below(&self, id: BlockId, height: u32) -> Result<BlockId, LlmqError<E>> {
        self.chain.ancestor(id, height).map_err(MemberError::View)?.ok_or(MemberError::Missing(id))
    }
}

/// A configured LLMQ type whose parameters are outside the domain [`LlmqCtx::new`] checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParamsOutOfDomain(pub LlmqType);

impl fmt::Display for ParamsOutOfDomain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "LLMQ type {} has parameters outside the supported domain", self.0.to_base())
    }
}

impl std::error::Error for ParamsOutOfDomain {}

/// Per-block state LQ1 reads (LQ1 §2.3, EV addendum §2.2). CH7b implements it over the evo read
/// view, SPV over its held lists, tests over maps.
///
/// `Ok(None)` means the view does not hold the block's input, which the caller turns into
/// [`MemberError::Missing`]: the node's views hold every connected block's, so for it that is an
/// inconsistency (D4), and SPV requests it (EV addendum §2.3).
pub trait LlmqSource<L: MnListView> {
    /// The source's error.
    type Error;

    /// Core `GetListForBlock(pindex)`: the list after `at`.
    fn mn_list(&self, at: BlockId) -> Result<Option<Arc<L>>, Self::Error>;

    /// The block data Core's `GetNonNullCoinbaseChainlock(pindex)` reads for `at` (LQ1 §2.3);
    /// LQ1 applies the predicate ([`nonnull_coinbase_chainlock`]). `None` when the view does not
    /// hold it; [`CoinbaseFacts::BlockDataUnavailable`] is Core's own failure to read the block
    /// (a pruned block), never a block the view merely lacks (EV addendum §2.1). LQ1 asks only
    /// for blocks at which V20 is active.
    ///
    /// [`nonnull_coinbase_chainlock`]: crate::cbtx::nonnull_coinbase_chainlock
    fn coinbase_facts(&self, at: BlockId) -> Result<Option<CoinbaseFacts>, Self::Error>;
}

/// Core's `CQuorumSnapshotManager` (`llmq/snapshot.cpp` v24:301-330): the only way LQ1 reads or
/// writes snapshot rows (LQ1 §2.3, §3.7.2). Inside a validation attempt it is the attempt's
/// overlay over the runtime rows (EV addendum §2.6).
pub trait SnapshotRows {
    /// The rows' error.
    type Error;

    /// `GetSnapshotForBlock`. Keyed by hash, so ancestry-free; sees every earlier `store`.
    /// `Ok(None)` is Core's absence only, a normal outcome (quirks 28a, 28e, young chains); a row
    /// the view merely lacks is `Err` (EV addendum §2.1). A stored row that does not decode is
    /// `Ok(None)` too: Core's `CDBWrapper::Read` catches the deserialization error and reports
    /// the row absent, so the rotation stops at it instead of failing (a contract on CH7b's row
    /// store, which persists rows as bytes).
    fn get(
        &self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
    ) -> Result<Option<QuorumSnapshot>, Self::Error>;

    /// `StoreSnapshotForBlock`: an unconditional overwrite keyed by (type, cycle-base hash).
    fn store(
        &mut self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
        snapshot: &QuorumSnapshot,
    ) -> Result<(), Self::Error>;
}
