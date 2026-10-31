//! V's deployments (CH3 §2.16, D21): CS4's [`Deployments`] over the node's complete header chain,
//! with Core's `ChainstateManager::m_versionbitscache` and `m_warningcache`, as the connector's
//! context ([`ConnectCx::deployments`](super::ConnectCx)) and `UpdateTip`'s unknown-versionbits
//! warning read them.

use std::sync::Arc;

use dash_chain::{CompleteAncestry, HeaderChain};
use dash_consensus::deployments::{
    Deployments, NoEhfSignals, ThresholdState, VERSIONBITS_NUM_BITS, VersionBitsCache, WarningCache,
};
use dash_consensus::{BlockId, ChainAncestry};
use dash_params::ChainSetup;

/// Core's deployment state on V: one versionbits cache and one warning cache over V's chain.
///
/// `A` is the chain, V's [`CompleteAncestry`]: CS4 requires an ancestry with real MTPs, so it is
/// the complete header chain's (genesis-origin, no window) and never a `RuleContext`. The handle
/// borrows nothing, so the connector's context reads it while V's hold lends the chain mutably
/// to the status writer (a2's `ChainCell`); it reads only fields an entry never changes once set.
///
/// The caches live as long as V, which is as long as its block index: Core keeps both in the
/// `ChainstateManager` and clears them only when it is destroyed (`validation.cpp:6782`, the one
/// `VersionBitsCache::Clear` outside tests). A reorg clears nothing, in Core or here: the keys are
/// blocks, which a reorg leaves as they are, so a disconnected branch keeps the entries Core's
/// keeps. A reindex is a new V (a new `ChainstateManager` in Core).
pub(crate) struct ChainDeployments<A = CompleteAncestry> {
    params: Arc<ChainSetup>,
    chain: A,
    /// The MNHF signals versionbits reads (`CMNHFManager::GetForBlock`): none until CH7a provides
    /// the source. No EHF deployment can start before V20 (`GetSignalsStage`); after it, on a
    /// chain holding an MNHF signal (testnet's and devnet's V24 and EVO_SHARES have started),
    /// such a deployment answers `DEFINED` where Core's moves on, so `ComputeBlockVersion` never
    /// sets its bit and `UpdateTip` counts blocks signalling it as unknown rules, which Core does
    /// not. CH7a's source closes both.
    ehf: NoEhfSignals,
    /// `m_versionbitscache` (`validation.h:1104`).
    cache: VersionBitsCache,
    /// `m_warningcache` (`validation.h:936`).
    warning: WarningCache,
}

impl ChainDeployments {
    /// V's deployments over `chain`, the node's header chain.
    ///
    /// # Panics
    /// If `chain` has a window (an SPV chain): its MTPs are not all real.
    pub(crate) fn new(params: Arc<ChainSetup>, chain: &HeaderChain) -> ChainDeployments {
        let chain = chain.complete_ancestry().expect("V's chain is the node's: no window");
        ChainDeployments::over(params, chain)
    }
}

impl<A: ChainAncestry> ChainDeployments<A> {
    /// Empty caches over `chain`.
    pub(crate) fn over(params: Arc<ChainSetup>, chain: A) -> ChainDeployments<A> {
        ChainDeployments {
            params,
            chain,
            ehf: NoEhfSignals,
            cache: VersionBitsCache::new(),
            warning: WarningCache::new(),
        }
    }

    /// Core's `DeploymentActiveAfter`, `DeploymentActiveAt` and the versionbits queries over V's
    /// chain and cache (`deploymentstatus.h`, `ChainstateManager::m_versionbitscache`).
    pub(crate) fn view(&self) -> Deployments<'_> {
        Deployments::new(&self.params, &self.chain, &self.ehf, &self.cache)
    }

    /// `UpdateTip`'s loop over the version bits (`validation.cpp:3222-3232`): each bit whose
    /// `WarningBitsConditionChecker` state for the block after `tip` is `LOCKED_IN` or `ACTIVE`,
    /// with that state, in bit order.
    pub(crate) fn unknown_rules(&mut self, tip: BlockId) -> Vec<(i32, ThresholdState)> {
        // Not `self.view()`: that would borrow `self.warning` too.
        let deployments = Deployments::new(&self.params, &self.chain, &self.ehf, &self.cache);
        (0..VERSIONBITS_NUM_BITS)
            .filter_map(|bit| {
                let state = deployments.warning_state(bit, Some(tip), &mut self.warning);
                matches!(state, ThresholdState::Active | ThresholdState::LockedIn)
                    .then_some((bit, state))
            })
            .collect()
    }
}

/// Core's `_("Unknown new rules activated (versionbit %i)")` (`validation.cpp:3227`), whose
/// original text the log line and the misc warning carry.
pub(crate) fn unknown_rules_warning(bit: i32) -> String {
    format!("Unknown new rules activated (versionbit {bit})")
}
