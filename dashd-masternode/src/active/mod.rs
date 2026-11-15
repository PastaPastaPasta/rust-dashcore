//! Core parity: active/masternode.cpp:52-110 at v24.0.0.

use dash_netaddr::Service;
use dash_primitives::OutPoint;
use parking_lot::RwLock;

pub mod key;
pub mod state;
pub mod worker;

pub use key::{ActiveMnInitError, OperatorKey, parse};

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

    // Core: active/masternode.h:67-69 at v24.0.0
    pub fn snapshot(&self) -> ActiveState {
        self.st.read().clone()
    }
}

#[cfg(test)]
mod tests;
