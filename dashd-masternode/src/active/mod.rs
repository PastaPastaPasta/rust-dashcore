//! Core parity: active/masternode.cpp:52-110 at v24.0.0.

use std::sync::Arc;

use dash_args::ArgsManager;
use dash_evo::types::ProTxHash;
use dash_llmq::dkg::bls::{BlsIesMultiBlobs, BlsSchemeKind, BlsSigValue, BlsSkValue};
use dash_llmq::dkg::wire::BlsIesBlobBytes;
use dash_llmq::qdata::source::OperatorDecrypt;
use dash_llmq::sigenv::OperatorSigner;
use dash_netaddr::Service;
use dash_primitives::{OutPoint, TxHash};
use parking_lot::RwLock;

pub mod key;
pub mod state;
pub mod worker;

pub use key::{ActiveMnInitError, OperatorKey, parse};

/// Step 7c's active masternode: present exactly when `-masternodeblsprivkey` holds a key, with
/// Core's init error for any other non-empty value.
// Core: init.cpp:2156-2167 at v24.0.0
pub fn load(args: &ArgsManager) -> Result<Option<Arc<ActiveMasternode>>, ActiveMnInitError> {
    Ok(parse(args)?.map(|key| Arc::new(ActiveMasternode::new(key))))
}

// Core: active/masternode.h:27-42 at v24.0.0
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum State {
    #[default]
    WaitingForProtx,
    PoseBanned,
    Removed,
    OperatorKeyChanged,
    ProtxIpChanged,
    Ready,
    SomeError,
}

// Core: active/masternode.h:38-42 at v24.0.0
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActiveState {
    pub state: State,
    pub error: String,
    pub pro_tx_hash: Option<[u8; 32]>,
    pub outpoint: Option<OutPoint>,
    pub service: Service,
}

impl ActiveState {
    // Core: active/masternode.cpp:67-87 at v24.0.0
    pub fn state_string(&self) -> &'static str {
        match self.state {
            State::WaitingForProtx => "WAITING_FOR_PROTX",
            State::PoseBanned => "POSE_BANNED",
            State::Removed => "REMOVED",
            State::OperatorKeyChanged => "OPERATOR_KEY_CHANGED",
            State::ProtxIpChanged => "PROTX_IP_CHANGED",
            State::Ready => "READY",
            State::SomeError => "ERROR",
        }
    }

    // Core: active/masternode.cpp:89-110 at v24.0.0
    pub fn status(&self) -> String {
        match self.state {
            State::WaitingForProtx => "Waiting for ProTx to appear on-chain".into(),
            State::PoseBanned => "Masternode was PoSe banned".into(),
            State::Removed => "Masternode removed from list".into(),
            State::OperatorKeyChanged => "Operator key changed or revoked".into(),
            State::ProtxIpChanged => "IP address specified in ProTx changed".into(),
            State::Ready => "Ready".into(),
            State::SomeError => format!("Error. {}", self.error),
        }
    }

    /// `GetOutPoint`: the collateral, or the null outpoint until READY.
    // Core: active/masternode.h:67 at v24.0.0
    pub fn outpoint(&self) -> OutPoint {
        self.outpoint.unwrap_or_else(|| OutPoint::new(TxHash::from([0; 32]), u32::MAX))
    }
}

// Core: active/masternode.h:23-42 at v24.0.0
pub struct ActiveMasternode {
    pub(crate) key: OperatorKey,
    pub(crate) st: RwLock<ActiveState>,
}

impl ActiveMasternode {
    // Core: active/masternode.cpp:52-58; active/masternode.h:38-42 at v24.0.0
    pub fn new(key: OperatorKey) -> Self {
        Self {
            key,
            st: RwLock::new(ActiveState::default()),
        }
    }

    // Core: active/masternode.h:55; active/masternode.cpp:112-173 at v24.0.0
    pub fn init(
        &self,
        tip: &state::TipCtx,
        list: &dyn state::MnLookup,
        local: &state::LocalFacts,
    ) -> Vec<state::LogLine> {
        state::init(&mut self.st.write(), &self.key.pubkey(), tip, list, local)
    }

    // Core: active/masternode.cpp:175-228 at v24.0.0
    pub fn updated_block_tip(
        &self,
        tip: &state::TipCtx,
        lists: Result<(&dyn state::MnLookup, &dyn state::MnLookup), state::ListError>,
        local: &state::LocalFacts,
    ) -> Result<Vec<state::LogLine>, dash_dml::store::StoreError> {
        state::updated_block_tip(&mut self.st.write(), &self.key.pubkey(), tip, lists, local)
    }

    // Core: active/masternode.h:68 at v24.0.0
    pub fn pro_tx_hash(&self) -> Option<[u8; 32]> {
        self.st.read().pro_tx_hash
    }

    // Core: active/masternode.cpp:67-87 at v24.0.0
    pub fn state_string(&self) -> &'static str {
        self.st.read().state_string()
    }

    // Core: active/masternode.cpp:89-110 at v24.0.0
    pub fn status(&self) -> String {
        self.st.read().status()
    }

    // Core: active/masternode.cpp:308-312 at v24.0.0
    pub fn pubkey(&self) -> key::OperatorPubKey {
        self.key.pubkey()
    }

    /// The operator key, for a consumer that holds the key rather than the active masternode.
    pub fn operator_key(&self) -> &OperatorKey {
        &self.key
    }

    // Core: active/masternode.h:67-69 at v24.0.0
    pub fn snapshot(&self) -> ActiveState {
        self.st.read().clone()
    }

    /// `GetOutPoint`: the collateral, or the null outpoint until READY.
    // Core: active/masternode.h:67 at v24.0.0
    pub fn outpoint(&self) -> OutPoint {
        self.st.read().outpoint()
    }

    /// `SignBasic`: the operator key's basic-scheme signature bytes.
    // Core: active/masternode.cpp:296-303 at v24.0.0
    pub fn sign_basic(&self, hash: &[u8; 32]) -> Vec<u8> {
        self.key.sign_basic(hash)
    }

    /// `Sign(hash, is_legacy)` with the operator key.
    // Core: active/masternode.cpp:290-294 at v24.0.0
    pub fn sign(&self, hash: &[u8; 32], legacy: bool) -> BlsSigValue {
        self.key.sign(hash, legacy)
    }

    /// `Decrypt` of a DKG session's contributions for recipient `idx`.
    // Core: active/masternode.cpp:278-284 at v24.0.0
    pub fn decrypt_share(&self, blobs: &BlsIesMultiBlobs, idx: usize) -> Option<BlsSkValue> {
        self.key.decrypt_share(blobs, idx)
    }

    /// `Decrypt` of a quorum member's encrypted secret key share for recipient `idx`.
    // Core: active/masternode.cpp:278-284 at v24.0.0
    pub fn decrypt_sk(&self, blob: &BlsIesBlobBytes, idx: usize) -> Option<BlsSkValue> {
        self.key.decrypt_sk(blob, idx)
    }
}

/// The operator key `qdata` intake decrypts its contributions with (Core's `m_nodeman`).
impl OperatorDecrypt for ActiveMasternode {
    // Core: active/masternode.h:68 at v24.0.0
    fn pro_tx_hash(&self) -> Option<ProTxHash> {
        ActiveMasternode::pro_tx_hash(self).map(ProTxHash::from)
    }

    // Core: active/masternode.cpp:278-284 at v24.0.0
    fn decrypt_sk(&self, b: &BlsIesBlobBytes, idx: usize) -> Option<BlsSkValue> {
        ActiveMasternode::decrypt_sk(self, b, idx)
    }
}

/// The operator key single-member sig shares are signed with (Core's `m_mn_activeman`).
impl OperatorSigner for ActiveMasternode {
    // Core: llmq/signing_shares.cpp:1580-1588 at v24.0.0
    fn sign(&self, hash: &[u8; 32], scheme: BlsSchemeKind) -> Option<BlsSigValue> {
        OperatorSigner::sign(&self.key, hash, scheme)
    }
}

#[cfg(test)]
mod tests;
