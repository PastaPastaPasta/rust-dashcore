//! The node's services behind the CoinJoin host traits.

use std::sync::Arc;

use dash_coinjoin::queue::MixingMeta;
use dash_num::Hash256;
use dash_primitives::{OutPoint, TxHash};
use dashd_masternode::active::ActiveMasternode;
use dashd_masternode::meta::MasternodeMetaMan;

use crate::env::ActiveMn;

/// [`ActiveMn`] over the node's active masternode.
#[derive(Clone)]
pub struct ActiveMnAdapter(pub Arc<ActiveMasternode>);

impl ActiveMn for ActiveMnAdapter {
    fn outpoint(&self) -> OutPoint {
        self.0.outpoint()
    }

    /// The null hash until READY (Core's `m_info.proTxHash` default).
    fn pro_tx_hash(&self) -> TxHash {
        TxHash::from(self.0.pro_tx_hash().unwrap_or_default())
    }

    fn sign_basic(&mut self, hash: &Hash256) -> Vec<u8> {
        self.0.sign_basic(hash.as_bytes())
    }
}

/// [`MixingMeta`] over the node's shared `CMasternodeMetaMan`, so the counters persist in
/// `mncache.dat`.
#[derive(Clone, Debug)]
pub struct MetaManMixing(pub Arc<MasternodeMetaMan>);

fn protx(hash: &TxHash) -> [u8; 32] {
    *hash.as_bytes()
}

impl MixingMeta for MetaManMixing {
    fn is_mixing_threshold_exceeded(&self, pro_tx_hash: &TxHash, mn_count: i32) -> bool {
        self.0.is_mixing_threshold_exceeded(&protx(pro_tx_hash).into(), mn_count)
    }

    fn allow_mixing(&mut self, pro_tx_hash: &TxHash) {
        self.0.allow_mixing(&protx(pro_tx_hash).into())
    }

    fn disallow_mixing(&mut self, pro_tx_hash: &TxHash) {
        self.0.disallow_mixing(&protx(pro_tx_hash).into())
    }

    fn is_valid_for_mixing_txes(&self, pro_tx_hash: &TxHash) -> bool {
        self.0.is_valid_for_mixing_txes(&protx(pro_tx_hash).into())
    }
}
