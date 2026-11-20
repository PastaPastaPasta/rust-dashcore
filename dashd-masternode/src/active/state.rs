//! Core parity: active/masternode.cpp:112-276 at v24.0.0.

use std::sync::Arc;

use dash_dml::{Dmn, MnList, ProTxHash, store::StoreError};
use dash_netaddr::Service;
use dash_primitives::BlockHash;

use super::{ActiveState, State, key::OperatorPubKey};

// Core: active/masternode.cpp:116, :119, :177 at v24.0.0
#[derive(Clone, Copy, Debug)]
pub struct TipCtx {
    pub height: u32,
    pub dip3_enforced: bool,
    pub require_routable: bool,
}

// Core: active/masternode.cpp:132-151, :184-221 at v24.0.0
pub trait MnLookup {
    fn by_operator_key(&self, key: &OperatorPubKey) -> Option<Arc<Dmn>>;
    fn is_valid(&self, pro: &[u8; 32]) -> bool;
    fn is_pose_banned(&self, pro: &[u8; 32]) -> bool;
    fn get(&self, pro: &[u8; 32]) -> Option<Arc<Dmn>>;
}

impl MnLookup for MnList {
    fn by_operator_key(&self, key: &OperatorPubKey) -> Option<Arc<Dmn>> {
        self.get_by_operator_key(key).cloned()
    }

    fn is_valid(&self, pro: &[u8; 32]) -> bool {
        self.is_valid(&ProTxHash::from(*pro))
    }

    fn is_pose_banned(&self, pro: &[u8; 32]) -> bool {
        self.is_pose_banned(&ProTxHash::from(*pro))
    }

    fn get(&self, pro: &[u8; 32]) -> Option<Arc<Dmn>> {
        self.get(&ProTxHash::from(*pro)).cloned()
    }
}

// Core: active/masternode.cpp:186-194 at v24.0.0
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListError {
    Unavailable(BlockHash),
    Fault(StoreError),
}

impl From<StoreError> for ListError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::ListUnavailable(block) => Self::Unavailable(block),
            other => Self::Fault(other),
        }
    }
}

// Core: active/masternode.cpp:123, :149, :154, :159, :166, :193, :263 at v24.0.0
#[derive(Clone, Debug)]
pub enum LogLine {
    Init(Arc<Dmn>),
    CheckingInbound(Service),
    Error {
        function: &'static str,
        message: String,
    },
    ListUnavailable(BlockHash),
}

impl LogLine {
    // Core: active/masternode.cpp:123, :149, :154, :159, :166, :193, :263 at v24.0.0
    pub fn render(&self, describe_mn: impl FnOnce(&Dmn) -> String) -> String {
        match self {
            Self::Init(dmn) => format!(
                "CActiveMasternodeManager::Init -- proTxHash={}, proTx={}\n",
                dmn.pro_tx_hash,
                describe_mn(dmn)
            ),
            Self::CheckingInbound(service) => format!(
                "CActiveMasternodeManager::Init -- Checking inbound connection to '{service}'\n"
            ),
            Self::Error {
                function,
                message,
            } => format!("CActiveMasternodeManager::{function} -- ERROR: {message}\n"),
            Self::ListUnavailable(block) => format!(
                "CActiveMasternodeManager::UpdatedBlockTip -- masternode list unavailable: {}\n",
                StoreError::ListUnavailable(*block)
            ),
        }
    }
}

// Core: active/masternode.cpp:250-259 at v24.0.0
#[derive(Clone, Debug)]
pub struct PeerLocal {
    pub is_ipv4: bool,
    pub local_addr: Option<Service>,
}

// Core: active/masternode.cpp:119, :160-163, :238-276 at v24.0.0
#[derive(Clone, Debug)]
pub struct LocalFacts {
    pub listen: bool,
    pub require_routable: bool,
    pub local_toward_v4: Option<Service>,
    pub loopback_listen: Service,
    pub peers: Vec<PeerLocal>,
    pub valid: Vec<Service>,
    pub self_connect_ok: bool,
}

// Core: active/masternode.cpp:230-267 at v24.0.0
pub fn get_local_address(st: &mut ActiveState, local: &LocalFacts) -> Result<(), LogLine> {
    if let Some(addr) = &local.local_toward_v4 {
        // Core: active/masternode.cpp:239 at v24.0.0
        st.service = addr.clone();
        if local.valid.contains(addr) {
            return Ok(());
        }
    }
    if !local.require_routable {
        st.service = local.loopback_listen.clone();
        return Ok(());
    }
    for peer in &local.peers {
        if let Some(addr) =
            peer.local_addr.as_ref().filter(|addr| peer.is_ipv4 && local.valid.contains(addr))
        {
            st.service = addr.clone();
            return Ok(());
        }
    }
    if local.peers.is_empty() {
        st.error = "Can't detect valid external address. Please consider using the externalip configuration option if problem persists. Make sure to use IPv4 address only.".into();
        return Err(LogLine::Error {
            function: "GetLocalAddress",
            message: st.error.clone(),
        });
    }
    Ok(())
}

// Core: active/masternode.cpp:112-173 at v24.0.0
pub fn init(
    st: &mut ActiveState,
    key: &OperatorPubKey,
    tip: &TipCtx,
    list: &dyn MnLookup,
    local: &LocalFacts,
) -> Vec<LogLine> {
    let mut log = Vec::new();
    if !tip.dip3_enforced {
        return log;
    }
    if !local.listen && tip.require_routable {
        init_error(st, &mut log, "Masternode must accept connections from outside. Make sure listen configuration option is not overwritten by some another parameter.".into());
        return log;
    }
    if let Err(line) = get_local_address(st, local) {
        st.state = State::SomeError;
        log.push(line);
        return log;
    }
    let Some(dmn) = list.by_operator_key(key) else {
        return log;
    };
    let pro = *dmn.pro_tx_hash.as_bytes();
    if !list.is_valid(&pro) {
        st.state = if list.is_pose_banned(&pro) {
            State::PoseBanned
        } else {
            State::Removed
        };
        return log;
    }
    log.push(LogLine::Init(Arc::clone(&dmn)));
    if st.service != dash_dml::netinfo::primary(&dmn.cold.net_info) {
        init_error(st, &mut log, "Local address does not match the address from ProTx".into());
        return log;
    }
    log.push(LogLine::CheckingInbound(st.service.clone()));
    if !local.self_connect_ok && tip.require_routable {
        init_error(st, &mut log, format!("Could not connect to {}", st.service));
        return log;
    }
    st.pro_tx_hash = Some(pro);
    st.outpoint = Some(dmn.collateral);
    st.state = State::Ready;
    log
}

fn init_error(st: &mut ActiveState, log: &mut Vec<LogLine>, message: String) {
    st.state = State::SomeError;
    st.error = message.clone();
    log.push(LogLine::Error {
        function: "Init",
        message,
    });
}

// Core: active/masternode.cpp:175-228 at v24.0.0
pub fn updated_block_tip(
    st: &mut ActiveState,
    key: &OperatorPubKey,
    new: &TipCtx,
    lists: Result<(&dyn MnLookup, &dyn MnLookup), ListError>,
    local: &LocalFacts,
) -> Result<Vec<LogLine>, StoreError> {
    if !new.dip3_enforced {
        return Ok(Vec::new());
    }
    let (old_list, new_list) = match lists {
        Ok(lists) => lists,
        Err(ListError::Unavailable(block)) if st.state == State::Ready => {
            return Ok(vec![LogLine::ListUnavailable(block)]);
        }
        Err(error) => {
            let error = match error {
                ListError::Unavailable(block) => StoreError::ListUnavailable(block),
                ListError::Fault(error) => error,
            };
            st.state = State::SomeError;
            st.pro_tx_hash = None;
            st.outpoint = None;
            return Err(error);
        }
    };
    if st.state != State::Ready {
        return Ok(init(st, key, new, new_list, local));
    }
    let pro = st.pro_tx_hash.unwrap_or([0; 32]);
    let reset_state = if !new_list.is_valid(&pro) {
        Some(State::Removed)
    } else {
        match (old_list.get(&pro), new_list.get(&pro)) {
            (Some(old), Some(new)) => {
                if new.cold.pub_key_operator != old.cold.pub_key_operator {
                    Some(State::OperatorKeyChanged)
                } else if dash_dml::netinfo::primary(&new.cold.net_info)
                    != dash_dml::netinfo::primary(&old.cold.net_info)
                {
                    Some(State::ProtxIpChanged)
                } else {
                    None
                }
            }
            _ => Some(State::SomeError),
        }
    };
    if let Some(state) = reset_state {
        // Core: active/masternode.cpp:196-203 at v24.0.0
        st.state = state;
        st.pro_tx_hash = None;
        st.outpoint = None;
        return Ok(init(st, key, new, new_list, local));
    }
    Ok(Vec::new())
}

#[cfg(test)]
mod tests;
