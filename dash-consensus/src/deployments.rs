//! Deployments (CS4): buried deployments by height, Dash's versionbits state machine with its
//! dynamic threshold and EHF signal height, `ComputeBlockVersion`, and the deployment report of
//! `getdeploymentinfo` ([`info`]).
//!
//! [`Deployments`] is the node's [`DeploymentView`]: Core's `deploymentstatus.h` over a block
//! index ([`ChainAncestry`]), the EHF signals of its blocks ([`EhfSignalSource`]) and one
//! [`VersionBitsCache`], which the node keeps for the lifetime of its block index, as Core's
//! `ChainstateManager::m_versionbitscache`. Versionbits answers depend on that cache exactly as
//! Core's do: each query reads the EHF signal height at the block it is asked about, and the
//! period states it computes stay cached.

pub mod ehf;
pub mod info;
mod versionbits;

use dash_params::{Bip9Deployment, BuriedDeployment, ChainSetup, DeploymentPos};

pub use ehf::{EhfSignalSource, EhfSignals, NoEhfSignals};
pub use versionbits::{
    Bip9Stats, ThresholdState, VERSIONBITS_LAST_OLD_BLOCK_VERSION, VERSIONBITS_NUM_BITS,
    VERSIONBITS_TOP_BITS, VERSIONBITS_TOP_MASK, VersionBitsCache, WarningCache,
};

use crate::ctx::{BlockId, ChainAncestry, Deployment, DeploymentView, VersionbitsAnswer};
use versionbits::{Checker, ThresholdConditionChecker as _, WarningBits};

/// Core's deployment queries over one chain (see the module documentation).
///
/// The parameters are a v24 release's (DEC-82: the versionbits positions include EVO_SHARES).
/// The ancestry must give every block a query walks its real `GetMedianTimePast()`: the walk
/// goes back period by period until a cached state, `MinBIP9WarningHeight` or a time below the
/// start, and every state it computes stays cached, so a stand-in time (dash-chain's unknown MTP
/// of a cold record) would cache a wrong state for good; an MTP outside `0..=u32::MAX` panics.
/// So the node wires it on the complete header chain (dash-chain's `HeaderChain`), never on a
/// `RuleContext`, whose reads outside its envelope answer with the top header in release builds.
#[derive(Clone, Copy)]
pub struct Deployments<'a> {
    pub params: &'a ChainSetup,
    pub chain: &'a dyn ChainAncestry,
    pub ehf: &'a dyn EhfSignalSource,
    pub cache: &'a VersionBitsCache,
}

impl<'a> Deployments<'a> {
    pub fn new(
        params: &'a ChainSetup,
        chain: &'a dyn ChainAncestry,
        ehf: &'a dyn EhfSignalSource,
        cache: &'a VersionBitsCache,
    ) -> Self {
        Self {
            params,
            chain,
            ehf,
            cache,
        }
    }

    /// `nHeight` as Core's `int`.
    fn height(&self, id: BlockId) -> i32 {
        i32::try_from(self.chain.header(id).height).expect("block heights are Core's int")
    }

    /// `GetMedianTimePast()` of `id`. Core's is a median of `uint32_t` block times, so any other
    /// value is a stand-in (dash-chain's `MTP_UNKNOWN`) that would decide a state the cache keeps
    /// for good: refuse it.
    fn mtp(&self, id: BlockId) -> i64 {
        let mtp = self.chain.header(id).mtp;
        assert!(
            (0..=i64::from(u32::MAX)).contains(&mtp),
            "a versionbits walk read MTP {mtp} at block {id:?}: the ancestry must give real MTPs"
        );
        mtp
    }

    /// `GetAncestor(height)` of a non-null index: null for a negative height or one above it.
    fn ancestor(&self, id: BlockId, height: i32) -> Option<BlockId> {
        self.chain.ancestor(id, u32::try_from(height).ok()?)
    }

    fn min_bip9_warning_height(&self) -> i32 {
        i32::try_from(self.params.consensus.min_bip9_warning_height.to_u32())
            .expect("MinBIP9WarningHeight is Core's int")
    }

    /// `params.vDeployments[pos]`.
    fn bip9(&self, pos: DeploymentPos) -> &'a Bip9Deployment {
        self.params.deployment(pos).expect("v24 chain parameters hold every DeploymentPos")
    }

    /// `params.DeploymentHeight(dep)` as Core's `int`.
    fn buried_height(&self, dep: BuriedDeployment) -> i32 {
        i32::try_from(self.params.consensus.deployment_height(dep).to_u32())
            .expect("deployment heights are Core's int")
    }

    // Core: deploymentstatus.h:14-18 DeploymentActiveAfter (BuriedDeployment)
    fn buried_active_after(&self, prev: Option<BlockId>, dep: BuriedDeployment) -> bool {
        prev.map_or(0, |p| self.height(p) + 1) >= self.buried_height(dep)
    }

    /// `VersionBitsCache::State(pindexPrev, params, pos)`: the state of the block after `prev`.
    // Core: versionbits.cpp:280-284 VersionBitsCache::State
    pub fn state(&self, prev: Option<BlockId>, pos: DeploymentPos) -> ThresholdState {
        let mut caches = self.cache.lock();
        Checker::new(self, pos).get_state_for(prev, &mut caches[pos.index()])
    }

    /// `VersionBitsCache::Statistics(pindex, params, pos, signalling_blocks)`.
    // Core: versionbits.cpp:286-290 VersionBitsCache::Statistics
    pub fn statistics(
        &self,
        block: Option<BlockId>,
        pos: DeploymentPos,
        signalling_blocks: Option<&mut Vec<bool>>,
    ) -> Bip9Stats {
        let mut caches = self.cache.lock();
        Checker::new(self, pos).get_state_statistics_for(
            block,
            &mut caches[pos.index()],
            signalling_blocks,
        )
    }

    /// `VersionBitsCache::StateSinceHeight(pindexPrev, params, pos)`.
    // Core: versionbits.cpp:292-296 VersionBitsCache::StateSinceHeight
    pub fn state_since_height(&self, prev: Option<BlockId>, pos: DeploymentPos) -> i32 {
        let mut caches = self.cache.lock();
        Checker::new(self, pos).get_state_since_height_for(prev, &mut caches[pos.index()])
    }

    /// `VersionBitsCache::Mask(params, pos)`.
    // Core: versionbits.cpp:298-301 VersionBitsCache::Mask
    pub fn mask(&self, pos: DeploymentPos) -> u32 {
        Checker::new(self, pos).mask()
    }

    /// `WarningBitsConditionChecker(chainman, bit).GetStateFor(pindex, params,
    /// m_warningcache.at(bit))`, `UpdateTip`'s question about unknown version bit `bit` for the
    /// block after `prev`: whether the network's blocks have locked in, or activated, rules this
    /// release does not know. Its condition calls [`Deployments::compute_block_version`], which
    /// locks the versionbits cache per counted block, so call this with that lock free.
    ///
    /// # Panics
    /// If `bit` is not below `VERSIONBITS_NUM_BITS` (`std::array::at`'s `out_of_range`).
    // Core: validation.cpp:3222-3226 (UpdateTip), :2395-2417 WarningBitsConditionChecker
    pub fn warning_state(
        &self,
        bit: i32,
        prev: Option<BlockId>,
        cache: &mut WarningCache,
    ) -> ThresholdState {
        WarningBits {
            deployments: self,
            bit,
        }
        .get_state_for(prev, cache.at(bit))
    }

    /// `VersionBitsCache::ComputeBlockVersion(pindexPrev, params)`: the version of a new block
    /// after `prev`.
    // Core: versionbits.cpp:303-317 VersionBitsCache::ComputeBlockVersion
    pub fn compute_block_version(&self, prev: Option<BlockId>) -> i32 {
        let mut caches = self.cache.lock();
        let mut version = VERSIONBITS_TOP_BITS;
        for &pos in self.params.release.deployments() {
            let checker = Checker::new(self, pos);
            let state = checker.get_state_for(prev, &mut caches[pos.index()]);
            if state == ThresholdState::LockedIn || state == ThresholdState::Started {
                version |= checker.mask() as i32;
            }
        }
        version
    }
}

/// `DeploymentEnabled(params, dep)` for a buried deployment: it has an activation height.
// Core: deploymentstatus.h:52-56 DeploymentEnabled (BuriedDeployment)
pub fn buried_enabled(params: &ChainSetup, dep: BuriedDeployment) -> bool {
    params.consensus.deployment_height(dep).to_u32() != i32::MAX as u32
}

/// `DeploymentEnabled(params, dep)` for a versionbits deployment: it can ever be active.
// Core: deploymentstatus.h:58-62 DeploymentEnabled (DeploymentPos)
pub fn bip9_enabled(params: &ChainSetup, pos: DeploymentPos) -> bool {
    params.deployment(pos).expect("v24 chain parameters hold every DeploymentPos").start_time
        != Bip9Deployment::NEVER_ACTIVE
}

/// `DeploymentDIP0003Enforced(nHeight, params)`.
// Core: deploymentstatus.h:65-68 DeploymentDIP0003Enforced
pub fn dip0003_enforced(height: i32, params: &ChainSetup) -> bool {
    i64::from(height) >= i64::from(params.consensus.dip0003_enforcement.0.to_u32())
}

impl DeploymentView for Deployments<'_> {
    // Core: deploymentstatus.h:14-31 DeploymentActiveAfter
    fn active_after(&self, prev: Option<BlockId>, deployment: Deployment) -> bool {
        match deployment {
            Deployment::Buried(dep) => self.buried_active_after(prev, dep),
            Deployment::Versionbits(pos) => self.state(prev, pos) == ThresholdState::Active,
        }
    }

    // Core: deploymentstatus.h:34-50 DeploymentActiveAt
    fn active_at(&self, block: BlockId, deployment: Deployment) -> bool {
        match deployment {
            Deployment::Buried(dep) => self.height(block) >= self.buried_height(dep),
            Deployment::Versionbits(_) => self.active_after(self.chain.prev(block), deployment),
        }
    }

    // Core: versionbits.cpp:280-284 State, :292-296 StateSinceHeight
    fn versionbits(&self, prev: Option<BlockId>, pos: DeploymentPos) -> Option<VersionbitsAnswer> {
        Some(VersionbitsAnswer {
            state: self.state(prev, pos),
            since: self.state_since_height(prev, pos),
        })
    }
}
