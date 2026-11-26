//! The active masternode's operator key as the DKG session reads it (DK1-L3, OPKEY-1).

use std::sync::Arc;

use dash_llmq::dkg::bls::{BlsIesMultiBlobs, BlsPkValue, BlsSigValue, BlsSkValue};
use dash_llmq::dkg::session::OperatorKey;
use dashd_masternode::active::ActiveMasternode;

/// `CActiveMasternodeManager` behind the session's [`OperatorKey`]: each call forwards to the
/// active masternode's own method.
#[derive(Clone)]
pub struct MnOperatorKey(pub Arc<ActiveMasternode>);

impl OperatorKey for MnOperatorKey {
    // Core: v24.0.0 src/active/masternode.cpp:308-312 CActiveMasternodeManager::GetPubKey
    fn public_key(&self) -> BlsPkValue {
        self.0.pubkey()
    }

    // Core: v24.0.0 src/active/masternode.cpp:290-294 CActiveMasternodeManager::Sign
    fn sign(&self, hash: &[u8; 32], legacy: bool) -> BlsSigValue {
        self.0.sign(hash, legacy)
    }

    // Core: v24.0.0 src/active/masternode.cpp:278-284 CActiveMasternodeManager::Decrypt
    fn decrypt_share(&self, blobs: &BlsIesMultiBlobs, idx: usize) -> Option<BlsSkValue> {
        self.0.decrypt_share(blobs, idx)
    }
}
