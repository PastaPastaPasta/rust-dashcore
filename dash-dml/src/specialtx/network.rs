//! Core's provider network-field checks (EV1 §3.15, `providertx.cpp:186-313`):
//! `IsNetInfoTriviallyValid`, `CheckNetInfo`, `CheckIPv4Primaries` and
//! `CheckProviderNetworkFields`.

use dash_consensus::validation::{TxValidationError, TxValidationResult};
use dash_primitives::{NIPurpose, NetInfo, PlatformNodeId};

use super::reasons::*;
use crate::MnType;
use crate::netinfo::{self, NetInfoRules, NetInfoStatus};
use crate::types::EXT_ADDR;

fn bad_special(reason: &'static str) -> Result<(), TxValidationError> {
    Err(TxValidationError::new(TxValidationResult::BadSpecial, reason))
}

fn bad_special_debug(reason: &'static str, debug: String) -> Result<(), TxValidationError> {
    Err(TxValidationError::new(TxValidationResult::BadSpecial, reason).with_debug(debug))
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
// Core: evo/providertx.cpp:186-205
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
// Core: evo/providertx.cpp:207-230
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

/// Core's `PurposeToString`.
// Core: evo/netinfo.h:92-103
fn purpose_to_string(purpose: NIPurpose) -> &'static str {
    match purpose {
        NIPurpose::CoreP2p => "core_p2p",
        NIPurpose::PlatformP2p => "platform_p2p",
        NIPurpose::PlatformHttps => "platform_https",
        NIPurpose::Unknown(_) => "",
    }
}

/// Core's `CheckIPv4Primaries`: the first `CORE_P2P` entry must be IPv4, and the first entry of
/// each Platform purpose that has entries must be an IPv4 service on that same address.
// Core: evo/providertx.cpp:232-255
fn check_ipv4_primaries(net_info: &NetInfo) -> Result<(), TxValidationError> {
    let core_primary = netinfo::primary(net_info);
    if !core_primary.addr.is_ipv4() {
        let core = purpose_to_string(NIPurpose::CoreP2p);
        return bad_special_debug(
            BAD_PROTX_NETINFO_ADDR_TYPE,
            format!("first {core} entry must be IPv4"),
        );
    }
    for purpose in [NIPurpose::PlatformP2p, NIPurpose::PlatformHttps] {
        let Some(first) = netinfo::first_service(net_info, purpose) else {
            continue;
        };
        let Some(service) = first.filter(|s| s.addr.is_ipv4()) else {
            let platform = purpose_to_string(purpose);
            return bad_special_debug(
                BAD_PROTX_NETINFO_ADDR_TYPE,
                format!("first {platform} entry must be IPv4"),
            );
        };
        if service.addr != core_primary.addr {
            let (platform, core) =
                (purpose_to_string(purpose), purpose_to_string(NIPurpose::CoreP2p));
            return bad_special_debug(
                BAD_PROTX_NETINFO_ADDR,
                format!("first {platform} entry must use the first {core} address"),
            );
        }
    }
    Ok(())
}

/// Core's `CheckProviderNetworkFields`. `ipv4_optional` is [`SpecialTxRules::ipv4_optional`] for
/// the block the transaction would be included in.
///
/// [`SpecialTxRules::ipv4_optional`]: super::SpecialTxRules::ipv4_optional
// Core: evo/providertx.cpp:257-313
pub fn check_provider_network_fields(
    f: &ProviderNetworkFields<'_>,
    allow_empty: bool,
    ipv4_optional: bool,
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
        // Checked here rather than in `Validate()`, which also checks stored masternode states
        // without a height.
        if !ipv4_optional {
            check_ipv4_primaries(f.net_info)?;
        }
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
