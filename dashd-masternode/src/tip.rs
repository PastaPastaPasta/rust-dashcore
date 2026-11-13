//! The published tip list and the process BLS scheme at that same view.

use std::sync::Arc;

use dash_consensus::BlsCtx;
#[cfg(feature = "net")]
use dash_consensus::hash::sha256d;
use dash_dml::MnList;
#[cfg(feature = "net")]
use dash_dml::ProTxHash;
#[cfg(feature = "net")]
use dashd_p2p::processing::seams::{MnListView, OperatorKeyRef};

/// One coherent publication of Core's `GetListAtChainTip()` and process scheme.
// Core: evo/deterministicmns.cpp:941-947; bls/bls.h:177-180 at v24.0.0
#[derive(Clone)]
pub struct TipListRef {
    pub list: Arc<MnList>,
    pub bls: BlsCtx,
}

/// A wait-free load of the last published list and scheme, even while publication lags.
/// Reads never wait on validation, take chain locks, or re-enter the processor or network core.
pub trait TipSource: Send + Sync {
    fn load(&self) -> TipListRef;
}

/// A shared tip source for masternode services.
#[derive(Clone)]
pub struct TipMnList(Arc<dyn TipSource>);

impl TipMnList {
    pub fn new(source: Arc<dyn TipSource>) -> Self {
        Self(source)
    }

    /// A constant publication for consumers' tests.
    pub fn fixed(tip: TipListRef) -> Self {
        Self::new(Arc::new(tip))
    }

    pub fn load(&self) -> TipListRef {
        self.0.load()
    }
}

impl TipSource for TipListRef {
    fn load(&self) -> TipListRef {
        self.clone()
    }
}

#[cfg(feature = "net")]
impl MnListView for TipListRef {
    // Core: evo/mnauth.cpp:85, :158; bls/bls.h:545-560; hash.h:117-121 at v24.0.0
    fn operator(&self, pro_reg_tx_hash: &[u8; 32]) -> Option<OperatorKeyRef> {
        let dmn = self.list.get(&ProTxHash::from(*pro_reg_tx_hash))?;
        let scheme = dmn.cold.key_scheme();
        let stored = dmn.cold.pub_key_operator.clone().to_bytes_as(scheme);
        Some(OperatorKeyRef {
            stored,
            stored_legacy: scheme.is_legacy(),
            pubkey_hash: sha256d(&stored),
        })
    }

    fn bls_legacy_scheme(&self) -> bool {
        self.bls.is_legacy()
    }
}

#[cfg(test)]
mod tests;
