//! The consensus context and the views later tasks implement (CS0 §8.3, §8.4).
//!
//! These are interface skeletons for EV0, CS3, CS4 and CH1, which own their semantics. Their
//! owners review them before EV0 starts (PLAN row EV0). CS4's deployments are in
//! [`crate::deployments`].

use dash_num::{Arith256, CompactTarget};
use dash_params::{BuriedDeployment, ChainSetup};
use dash_primitives::BlockHash;

use crate::deployments::ThresholdState;

/// A block in CH1's block-index arena.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockId(pub u32);

/// The header fields of a `CBlockIndex` that consensus code reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HeaderCtx {
    /// `nHeight`.
    pub height: u32,
    /// `GetBlockHash()`.
    pub hash: BlockHash,
    /// `nTime`.
    pub time: u32,
    /// `nBits`.
    pub bits: CompactTarget,
    /// `nVersion`.
    pub version: i32,
    /// `GetMedianTimePast()`.
    pub mtp: i64,
    /// `nChainWork`.
    pub chain_work: Arith256,
}

/// Walks the block index, as Core does through `pprev` and `GetAncestor` (implemented by CH1's
/// arena).
///
/// Contract, for every implementation:
/// - every `BlockId` passed in was issued by this arena and stays valid for the arena's
///   lifetime; implementations may panic otherwise;
/// - `prev(id)` is `Some` exactly when `header(id).height > 0`, and then
///   `header(prev(id)).height == header(id).height - 1` (Core's index holds every ancestor of
///   every entry; only genesis has a null `pprev`);
/// - `ancestor(id, h)` is `Some` exactly when `h <= header(id).height`; `ancestor(id,
///   header(id).height)` is `id`, and the result has height `h`.
pub trait ChainAncestry: Sync {
    /// The entry's header fields, by value, so an implementation can sit behind a lock or an
    /// epoch-managed arena.
    fn header(&self, id: BlockId) -> HeaderCtx;

    /// `GetAncestor(height)`: `None` when `height` is above `id`'s height (Core returns null
    /// there and for negative heights, which `u32` excludes).
    // Core: chain.cpp:94 CBlockIndex::GetAncestor (v23.1.8 :93)
    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId>;

    /// `pprev`: `None` exactly for genesis (height 0).
    fn prev(&self, id: BlockId) -> Option<BlockId>;
}

/// Core's `DeploymentPos`: the versionbits deployments, as the chain parameters index them.
// Core: consensus/params.h DeploymentPos
pub use dash_params::DeploymentPos;

/// A deployment: buried (a height in the chain parameters) or versionbits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Deployment {
    Buried(BuriedDeployment),
    Versionbits(DeploymentPos),
}

/// Deployment state per call site, as in Core ([`crate::deployments::Deployments`] implements
/// it).
///
/// `DeploymentDIP0003Enforced(nHeight, params)` is not here: it is a height comparison whose
/// height each call site picks ([`crate::deployments::dip0003_enforced`], CS0 deviation 9).
pub trait DeploymentView: Sync {
    /// `DeploymentActiveAfter(pindexPrev, params, dep)`; `None` is genesis's null `pindexPrev`.
    fn active_after(&self, prev: Option<BlockId>, deployment: Deployment) -> bool;

    /// `DeploymentActiveAt(index, params, dep)`.
    fn active_at(&self, block: BlockId, deployment: Deployment) -> bool;

    /// `m_versionbitscache.State(prev, params, pos)` and `StateSinceHeight(prev, params, pos)`:
    /// the versionbits answers for the block after `prev`.
    ///
    /// `None` means this view keeps no versionbits cache to answer from; that default suits the
    /// views that only answer activation (tests, header windows, evo checks). The node's
    /// [`crate::deployments::Deployments`] answers.
    fn versionbits(&self, prev: Option<BlockId>, pos: DeploymentPos) -> Option<VersionbitsAnswer> {
        let _ = (prev, pos);
        None
    }
}

/// One versionbits deployment's answers for a block: [`DeploymentView::versionbits`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VersionbitsAnswer {
    /// `VersionBitsCache::State`.
    pub state: ThresholdState,
    /// `VersionBitsCache::StateSinceHeight`: the first height of the period the state began.
    pub since: i32,
}

/// Checkpoints in the block index.
pub trait CheckpointView: Sync {
    /// The height of the highest checkpoint the index holds; `None` when it holds none and when
    /// checkpoints are disabled.
    fn highest_checkpoint_in_index(&self) -> Option<u32>;
}

/// The BLS scheme to decode with (A1 Appendix A, §1.4.1): base-sdk's, next to its BLS schemes
/// (DEC-24 D2, BS11a-t).
pub use dash_pkc::bls::BlsCtx;

/// The scheme for the contents of block `height`: legacy below `V19Height` (A1 §1.4.1).
pub fn bls_ctx_for_block(params: &ChainSetup, height: u32) -> BlsCtx {
    BlsCtx::from_legacy(height < params.consensus.v19_height.to_u32())
}

/// The context of one consensus check: one chain position.
///
/// `prev`, `height` (= `prev`'s height + 1), `median_time_past` (`prev`'s), the views and `bls`
/// all describe the same chain; CH1 builds it in one place.
#[derive(Clone, Copy)]
pub struct ConsensusCtx<'a> {
    pub params: &'a ChainSetup,
    pub height: u32,
    pub prev: Option<BlockId>,
    pub chain: &'a dyn ChainAncestry,
    pub deployments: &'a dyn DeploymentView,
    pub checkpoints: &'a dyn CheckpointView,
    pub bls: BlsCtx,
    pub median_time_past: i64,
    /// The time `time-too-new` compares against: v24's `NodeClock::now()`, which the caller
    /// reads (mockable local time, no peer offset).
    pub header_check_time: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::test_support::chain;
    use dash_params::Network;

    #[test]
    fn bls_scheme_switches_at_v19() {
        let params = &chain(Network::Main);
        let v19 = params.consensus.v19_height.to_u32();
        assert!(bls_ctx_for_block(params, v19 - 1).is_legacy());
        assert!(!bls_ctx_for_block(params, v19).is_legacy());
    }
}
