//! V's deployments (CH3 §2.16, D21): CS4's [`Deployments`] over the node's complete header chain,
//! with Core's `ChainstateManager::m_versionbitscache` and `m_warningcache`, as the connector's
//! context ([`ConnectCx::deployments`](super::ConnectCx)) and `UpdateTip`'s unknown-versionbits
//! warning read them.

use std::sync::Arc;

use dash_chain::{ChainReader, CompleteAncestry, HeaderChain};
use dash_consensus::deployments::{
    Deployments, EhfReadError, EhfSignalSource, EhfSignals, ThresholdState, VERSIONBITS_NUM_BITS,
    VersionBitsCache, WarningCache,
};
use dash_consensus::{BlockId, ChainAncestry};
use dash_dml::mnhf::GetForBlockError;
use dash_params::ChainSetup;

use super::{EhfRows, Fatal};

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
    /// The MNHF signals versionbits reads (`CMNHFManager::GetForBlock`, DEC-1028): the
    /// connector's rows over V's index, none for a connector that keeps no evo state.
    ehf: EhfSource,
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
        let reader = chain.reader();
        let chain = chain.complete_ancestry().expect("V's chain is the node's: no window");
        let mut deployments = ChainDeployments::over(params, chain);
        deployments.ehf.reader = Some(reader);
        deployments
    }

    /// The connector's MNHF rows (DEC-1028), which V's versionbits read from now on. Core's
    /// manager exists before versionbits' first read (`versionbits.cpp:243`); V's rows come with
    /// its connector, at the spawn or the coins attach, so any state computed without them is
    /// dropped: the versionbits cache starts again empty, and so does the warning cache, whose
    /// condition reads versionbits through `ComputeBlockVersion` (`validation.cpp:2426` at
    /// v24.0.0).
    pub(crate) fn set_ehf_rows(&mut self, rows: Option<Arc<dyn EhfRows>>) {
        self.ehf.rows = rows;
        self.cache = VersionBitsCache::new();
        self.warning = WarningCache::new();
    }

    /// Whether the versionbits cache and the warning cache are each as a new one, empty.
    #[cfg(test)]
    pub(crate) fn caches_empty(&self) -> (bool, bool) {
        let cache = format!("{:?}", self.cache) == format!("{:?}", VersionBitsCache::new());
        let warning = format!("{:?}", self.warning) == format!("{:?}", WarningCache::new());
        (cache, warning)
    }

    /// Core `GetSignalsStage(block)` with `GetForBlock`'s exceptions, as `getdeploymentinfo`
    /// reads it before its report (RP1a D52): a block with no row is rebuilt from disk.
    // Core: evo/mnhftx.cpp:61-92, :285-321
    pub(crate) fn ehf_signals_stage(&self, block: BlockId) -> Result<EhfSignals, GetForBlockError> {
        self.view().ehf_signals_stage_from(Some(block), |id| self.ehf.get_for_block(id))
    }
}

/// [`ChainDeployments`]' EHF source: `GetForBlock` through the connector's rows over V's index.
/// It reads no deployment state (CS4's contract), and where Core's `GetForBlock` throws out of
/// versionbits it returns the error, which versionbits returns before it caches any state the
/// read decides; V's paths make it [`ehf_fault`].
#[derive(Default)]
struct EhfSource {
    rows: Option<Arc<dyn EhfRows>>,
    /// V's index; `None` for [`ChainDeployments::over`]'s chains, which take no rows.
    reader: Option<ChainReader>,
}

impl EhfSource {
    fn get_for_block(&self, block: BlockId) -> Result<EhfSignals, GetForBlockError> {
        // The rows take the evo layer's mutex: never under the mineable pool's leaf lock.
        #[cfg(any(test, feature = "test-support"))]
        assert!(
            !crate::llmq::facade::mineable_held_here(),
            "an EHF rows read under the mineable pool's leaf lock"
        );
        match (&self.rows, &self.reader) {
            (Some(rows), Some(reader)) => rows.get_for_block(reader, block),
            (Some(_), None) => unreachable!("rows are set only on V's own chain"),
            (None, _) => Ok(EhfSignals::new()),
        }
    }
}

impl EhfSignalSource for EhfSource {
    // Core: evo/mnhftx.cpp:285 CMNHFManager::GetForBlock
    fn signals_for_block(&self, block: BlockId) -> Result<EhfSignals, EhfReadError> {
        self.get_for_block(block).map_err(EhfReadError::new)
    }
}

/// D5's fault for an EHF read error met by versionbits on V's paths: Core's `GetForBlock`
/// exception out of its caller (DEC-1028). Always [`Fatal::new`]: an engine error or a row that
/// does not decode was reported as the guarded engine tripped the latch, so V reports only what
/// the latch does not show (`failed-getehfforblock-read`, `-construct`), once.
// Core: evo/mnhftx.cpp:285-321 CMNHFManager::GetForBlock
pub(crate) fn ehf_fault(e: EhfReadError) -> Fatal {
    Fatal::new(format!("GetForBlock: {e}"))
}

impl<A: ChainAncestry> ChainDeployments<A> {
    /// Empty caches over `chain`.
    pub(crate) fn over(params: Arc<ChainSetup>, chain: A) -> ChainDeployments<A> {
        ChainDeployments {
            params,
            chain,
            ehf: EhfSource::default(),
            cache: VersionBitsCache::new(),
            warning: WarningCache::new(),
        }
    }

    /// The entries of the versionbits cache and of the warning cache, each as its `Debug`: maps
    /// keyed by block, so equal entries print equally.
    #[cfg(test)]
    pub(crate) fn cache_entries(&self) -> (String, String) {
        (format!("{:?}", self.cache), format!("{:?}", self.warning))
    }

    /// These deployments reading `rows` through `reader`: a stand-in chain's rows, which may
    /// ignore the reader.
    #[cfg(test)]
    pub(crate) fn with_rows(mut self, rows: Arc<dyn EhfRows>, reader: ChainReader) -> Self {
        self.ehf = EhfSource {
            rows: Some(rows),
            reader: Some(reader),
        };
        self
    }

    /// Core's `DeploymentActiveAfter`, `DeploymentActiveAt` and the versionbits queries over V's
    /// chain and cache (`deploymentstatus.h`, `ChainstateManager::m_versionbitscache`).
    pub(crate) fn view(&self) -> Deployments<'_> {
        Deployments::new(&self.params, &self.chain, &self.ehf, &self.cache)
    }

    /// `UpdateTip`'s loop over the version bits (`validation.cpp:3222-3232`): each bit whose
    /// `WarningBitsConditionChecker` state for the block after `tip` is `LOCKED_IN` or `ACTIVE`,
    /// with that state, in bit order. An EHF read error ends the loop at its bit, as Core's throw
    /// leaves `UpdateTip`: no later bit is asked.
    pub(crate) fn unknown_rules(
        &mut self,
        tip: BlockId,
    ) -> Result<Vec<(i32, ThresholdState)>, Fatal> {
        // Not `self.view()`: that would borrow `self.warning` too.
        let deployments = Deployments::new(&self.params, &self.chain, &self.ehf, &self.cache);
        let states = (0..VERSIONBITS_NUM_BITS)
            .map(|bit| {
                let state = deployments.warning_state(bit, Some(tip), &mut self.warning);
                state.map(|state| (bit, state)).map_err(ehf_fault)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(states
            .into_iter()
            .filter(|(_, state)| matches!(state, ThresholdState::Active | ThresholdState::LockedIn))
            .collect())
    }
}

/// Core's `_("Unknown new rules activated (versionbit %i)")` (`validation.cpp:3227`), whose
/// original text the log line and the misc warning carry.
pub(crate) fn unknown_rules_warning(bit: i32) -> String {
    format!("Unknown new rules activated (versionbit {bit})")
}
