//! SP1's mnsync seam over the governance handle: the sync tick's reads of Core's
//! `CGovernanceManager` ([`GovernanceSync`]), its `governance_ready`, and the sync bumps the
//! manager's [`Effects`] carry to `CMasternodeSync::BumpAssetLastTime`.
//!
//! With governance disabled there is no handle: [`NodeGovernanceSync::disabled`] answers as
//! Core's unloaded manager does, invalid and empty.

use std::sync::{Arc, Mutex, PoisonError};

use dash_dml::MnList;
use dash_governance::GovParams;
use dash_governance::manager::Effects;
use dash_governance::vote;
use dash_p2p_core::FilterLoad;
use dashd_masternode::meta::GovHash;
use dashd_masternode::sync::MasternodeSync;
use dashd_masternode::sync_manager::{AskedRecently, GovernanceSync, empty_bloom_filter};
use dashd_p2p::processing::bloom::{BLOOM_UPDATE_ALL, BloomFilter};

use crate::exec::GovExec;
use crate::handle::GovernanceHandle;
use crate::net::GovSyncState;
use crate::rand::GovRng;

/// `GOVERNANCE_FILTER_FP_RATE`.
// Core: governance/object.h:86
pub const GOVERNANCE_FILTER_FP_RATE: f64 = 0.001;

/// The DMN-tip list as the sync tick reads it, off V.
pub trait DmnTip: Send + Sync {
    /// `GetListAtChainTip()`.
    fn dmn_tip_list(&self) -> Arc<MnList>;
}

/// Applies each of `effects`' sync bumps to `sync`, in order.
// Core: governance/governance.cpp:368; governance/net_governance.cpp:245
pub fn apply_sync_bumps(sync: &MasternodeSync, effects: &Effects) {
    for func in &effects.sync_bumps {
        sync.bump_asset_last_time(func);
    }
}

struct Live<E> {
    handle: Arc<GovernanceHandle<E>>,
    dmn: Arc<dyn DmnTip>,
    rng: Mutex<Box<dyn GovRng + Send>>,
    filter_elements: u32,
    /// Runs under the fetch's lock, so a test reads the manager the fetch read.
    #[cfg(any(test, feature = "test-support"))]
    on_fetch: Option<FetchProbe>,
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) type FetchProbe = Box<dyn Fn(&dash_governance::manager::Manager) + Send + Sync>;

/// [`GovernanceSync`] over the governance handle, or over nothing when governance is disabled.
pub struct NodeGovernanceSync<E> {
    live: Option<Live<E>>,
    sync: Arc<MasternodeSync>,
}

impl<E: GovExec + 'static> NodeGovernanceSync<E> {
    pub fn new(
        handle: Arc<GovernanceHandle<E>>,
        sync: Arc<MasternodeSync>,
        dmn: Arc<dyn DmnTip>,
        rng: Box<dyn GovRng + Send>,
        params: &GovParams,
    ) -> Self {
        let elements = params.chain().consensus.governance_filter_elements;
        Self {
            live: Some(Live {
                handle,
                dmn,
                rng: Mutex::new(rng),
                filter_elements: u32::try_from(elements).unwrap_or(0).max(1),
                #[cfg(any(test, feature = "test-support"))]
                on_fetch: None,
            }),
            sync,
        }
    }

    /// `probe` runs on each fetch, under its lock.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn on_fetch(mut self, probe: FetchProbe) -> Self {
        if let Some(l) = &mut self.live {
            l.on_fetch = Some(probe);
        }
        self
    }

    /// `-disablegovernance`: Core's manager with nothing loaded.
    // Core: init.cpp:2257; governance/governance.cpp:99-110
    pub fn disabled(sync: Arc<MasternodeSync>) -> Self {
        Self {
            live: None,
            sync,
        }
    }

    /// `IsValid() && m_mn_sync.IsBlockchainSynced()`: governance invs are taken.
    // Core: governance/net_governance.cpp:280; governance/governance.cpp:629
    pub fn governance_ready(&self) -> bool {
        GovernanceSync::is_valid(self) && self.sync.is_blockchain_synced()
    }

    /// [`apply_sync_bumps`] to this seam's sync.
    pub fn apply_sync_bumps(&self, effects: &Effects) {
        apply_sync_bumps(&self.sync, effects);
    }
}

impl<E: GovExec + 'static> GovSyncState for NodeGovernanceSync<E> {
    fn is_valid(&self) -> bool {
        GovernanceSync::is_valid(self)
    }

    fn is_blockchain_synced(&self) -> bool {
        self.sync.is_blockchain_synced()
    }

    fn is_synced(&self) -> bool {
        self.sync.is_synced()
    }

    fn bump_asset_last_time(&self, func: &str) {
        self.sync.bump_asset_last_time(func);
    }
}

impl<E: GovExec + 'static> GovernanceSync for NodeGovernanceSync<E> {
    // Core: governance/governance.h:279 IsValid
    fn is_valid(&self) -> bool {
        self.live.as_ref().is_some_and(|l| l.handle.published().is_loaded)
    }

    // Core: governance/governance.cpp:202-206
    fn vote_count(&self) -> i32 {
        let Some(l) = &self.live else {
            return 0;
        };
        i32::try_from(l.handle.lock().vote_index_len()).unwrap_or(i32::MAX)
    }

    fn enabled_masternode_count(&self) -> i32 {
        self.live.as_ref().map_or(0, |l| {
            i32::try_from(l.dmn.dmn_tip_list().counts().enabled()).unwrap_or(i32::MAX)
        })
    }

    // Core: governance/governance.cpp:969-993
    fn fetch_governance_object_votes(
        &self,
        peers_per_hash_max: usize,
        now: i64,
        asked: &mut AskedRecently,
    ) -> (Vec<GovHash>, Vec<GovHash>) {
        let Some(l) = &self.live else {
            return (Vec::new(), Vec::new());
        };
        let (triggers, others) = {
            let m = l.handle.lock();
            #[cfg(any(test, feature = "test-support"))]
            if let Some(probe) = &l.on_fetch {
                probe(&m);
            }
            m.fetch_governance_object_votes()
        };
        let mut keep = |hash: &GovHash| {
            asked.get_mut(hash).is_none_or(|peers| {
                peers.retain(|_, expiry| *expiry >= now);
                peers.len() < peers_per_hash_max
            })
        };
        (
            triggers.into_iter().filter(|h| keep(h)).collect(),
            others.into_iter().filter(|h| keep(h)).collect(),
        )
    }

    // Core: governance/governance.cpp:947-964
    fn vote_bloom_filter(&self, hash: &GovHash) -> FilterLoad {
        let Some(l) = &self.live else {
            return empty_bloom_filter();
        };
        let m = l.handle.lock();
        let Some(state) = m.object(hash) else {
            return empty_bloom_filter();
        };
        let tweak = l.rng.lock().unwrap_or_else(PoisonError::into_inner).bloom_tweak();
        let mut filter =
            BloomFilter::new(l.filter_elements, GOVERNANCE_FILTER_FP_RATE, tweak, BLOOM_UPDATE_ALL);
        for v in state.file.votes() {
            filter.insert(&vote::hash(v));
        }
        filter.to_msg()
    }
}

#[cfg(test)]
mod tests;
