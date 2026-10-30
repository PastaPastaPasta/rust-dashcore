//! Core's provider network-field checks (EV1 §3.15, `providertx.cpp:166-263`):
//! `IsNetInfoTriviallyValid`, `CheckNetInfo` and `CheckProviderNetworkFields`.

use dash_consensus::validation::{TxValidationError, TxValidationResult};
use dash_primitives::{NIPurpose, NetInfo, PlatformNodeId};

use super::reasons::*;
use crate::MnType;
use crate::netinfo::{self, NetInfoRules, NetInfoStatus};
use crate::types::EXT_ADDR;

fn bad_special(reason: &'static str) -> Result<(), TxValidationError> {
    Err(TxValidationError::new(TxValidationResult::BadSpecial, reason))
}

/// The ProTx fields `CheckProviderNetworkFields` reads.
#[derive(Clone, Copy, Debug)]
pub struct ProviderNetworkFields<'a> {
    pub net_info: &'a NetInfo,
    pub mn_type: MnType,
    /// The payload's `nVersion`.
    pub version: u16,
    /// The payload's `platformNodeID`, or `None` for Core's null pointer, which skips the check.
    /// Core's consensus callers always pass the field (the null id when the payload has none);
    /// only its RPC passes the null pointer (EV1-1 D44).
    pub platform_node_id: Option<&'a PlatformNodeId>,
    pub platform_p2p_port: u16,
    pub platform_http_port: u16,
}

/// Core's `IsNetInfoTriviallyValid`.
// Core: evo/providertx.cpp:166-185
pub fn is_net_info_trivially_valid(
    net_info: &NetInfo,
    mn_type: MnType,
) -> Result<(), TxValidationError> {
    let has = |purpose| netinfo::has_entries(net_info, purpose);
    if !has(NIPurpose::CoreP2p) {
        // Mandatory for all nodes.
        return bad_special(BAD_PROTX_NETINFO_EMPTY);
    }
    if mn_type == MnType::Regular && (has(NIPurpose::PlatformHttps) || has(NIPurpose::PlatformP2p))
    {
        // Regular nodes shouldn't populate Platform-specific fields.
        return bad_special(BAD_PROTX_NETINFO_BAD);
    }
    let stores_platform = matches!(net_info, NetInfo::Extended(_));
    if stores_platform
        && mn_type == MnType::Evo
        && (!has(NIPurpose::PlatformHttps) || !has(NIPurpose::PlatformP2p))
    {
        // Platform fields are mandatory for EvoNodes.
        return bad_special(BAD_PROTX_NETINFO_EMPTY);
    }
    Ok(())
}

/// Core's `CheckNetInfo`: `Validate()`'s status as a reject.
// Core: evo/providertx.cpp:187-210
pub fn check_net_info(net_info: &NetInfo, rules: &NetInfoRules) -> Result<(), TxValidationError> {
    status_reject(netinfo::validate(net_info, rules))
}

/// `CheckNetInfo`'s map from a `Validate()` status to its reject.
fn status_reject(status: NetInfoStatus) -> Result<(), TxValidationError> {
    match status {
        NetInfoStatus::BadAddress => bad_special(BAD_PROTX_NETINFO_ADDR),
        NetInfoStatus::BadPort => bad_special(BAD_PROTX_NETINFO_PORT),
        NetInfoStatus::BadType => bad_special(BAD_PROTX_NETINFO_ADDR_TYPE),
        NetInfoStatus::NotRoutable => bad_special(BAD_PROTX_NETINFO_ADDR_UNROUTABLE),
        NetInfoStatus::Malformed => bad_special(BAD_PROTX_NETINFO_BAD),
        NetInfoStatus::Success => Ok(()),
        NetInfoStatus::BadInput => bad_special(BAD_PROTX_NETINFO_ENTRY),
        NetInfoStatus::Duplicate => bad_special(BAD_PROTX_DUP_NETINFO_ENTRY),
        NetInfoStatus::MaxLimit => bad_special(BAD_PROTX_NETINFO_MAXLIMIT),
    }
}

/// Core's `CheckProviderNetworkFields`.
// Core: evo/providertx.cpp:212-263
pub fn check_provider_network_fields(
    f: &ProviderNetworkFields<'_>,
    allow_empty: bool,
    rules: &NetInfoRules,
) -> Result<(), TxValidationError> {
    let extended = i32::from(f.version) >= EXT_ADDR;
    if matches!(f.net_info, NetInfo::Extended(_)) != extended {
        return Err(TxValidationError::new(
            TxValidationResult::Consensus,
            BAD_PROTX_NETINFO_VERSION,
        ));
    }
    if netinfo::is_empty(f.net_info) {
        if !allow_empty {
            return bad_special(BAD_PROTX_NETINFO_EMPTY);
        }
    } else {
        is_net_info_trivially_valid(f.net_info, f.mn_type)?;
        check_net_info(f.net_info, rules)?;
    }

    if f.mn_type != MnType::Evo {
        return Ok(());
    }
    if f.platform_node_id.is_some_and(|id| id.is_null()) {
        return bad_special(BAD_PROTX_PLATFORM_NODEID);
    }
    let (p2p, http) = (f.platform_p2p_port, f.platform_http_port);
    if extended {
        if p2p != 0 {
            return bad_special(BAD_PROTX_PLATFORM_P2P_PORT);
        }
        if http != 0 {
            return bad_special(BAD_PROTX_PLATFORM_HTTP_PORT);
        }
        return Ok(());
    }

    let (main_default, main_p2p, main_http) = rules.main_ports();
    if rules.on_mainnet() {
        if p2p != main_p2p {
            return bad_special(BAD_PROTX_PLATFORM_P2P_PORT);
        }
        if http != main_http {
            return bad_special(BAD_PROTX_PLATFORM_HTTP_PORT);
        }
    }
    if p2p == main_default {
        return bad_special(BAD_PROTX_PLATFORM_P2P_PORT);
    }
    if http == main_default {
        return bad_special(BAD_PROTX_PLATFORM_HTTP_PORT);
    }
    let core_port = netinfo::primary(f.net_info).port;
    if p2p == http || p2p == core_port || http == core_port {
        return bad_special(BAD_PROTX_PLATFORM_DUP_PORTS);
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests;
