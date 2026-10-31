//! The state edits of `RebuildListFromBlock` (EV2 §3.2.1, §3.2.2, §3.2.6): the registration
//! state, `SetStateVersion` and `ApplyProUpServTx`.

use std::sync::Arc;

use dash_pkc::bls::BlsSchemeKind;
use dash_primitives::{MnType as PayloadMnType, PayoutShare, ProRegTx, ProUpServTx};
use dash_types::Numeric;

use super::{DmlBuildError, DmlException};
use crate::netinfo::{self, NetInfoRules};
use crate::types::{MnCold, OperatorKey, ext_addr, is_default_operator_key, scheme_for};
use crate::{MnType, RawMnType};

/// `MasternodePayoutShare::MAX_REWARD`: the whole owner reward, in basis points.
// Core: evo/providertx.h MasternodePayoutShare::MAX_REWARD
const MAX_REWARD: u16 = 10_000;

/// `CDeterministicMNState(proTx)` with `nRegisteredHeight = height` (`dmnstate.h` v24:74-89,
/// `specialtxman.cpp` v24:431-432). The payload's NetInfo is taken as Core reads it (D7).
// Core: evo/dmnstate.h:74 CDeterministicMNState::CDeterministicMNState(const CProRegTx&)
pub(super) fn from_pro_reg(p: &ProRegTx, height: i32) -> MnCold {
    MnCold {
        version: i32::from(u16::from(p.version)),
        registered_height: height,
        key_id_owner: p.key_id_owner,
        pub_key_operator: p.pub_key_operator.clone(),
        key_id_voting: p.key_id_voting,
        net_info: Arc::new(netinfo::as_core_reads(&p.net_info)),
        script_payout: p.script_payout.as_bytes().to_vec(),
        payouts: p.payouts.clone(),
        shares: p.shares.clone(),
        early_period_blocks: p.early_period_blocks,
        early_penalty: p.early_penalty,
        platform_node_id: p.platform_node_id.unwrap_or_default(),
        platform_p2p_port: p.platform_p2p_port.unwrap_or(0),
        platform_http_port: p.platform_http_port.unwrap_or(0),
        ..default_state()
    }
}

/// Core's default `CDeterministicMNState` with the default operator key under the basic scheme
/// (the default key serializes as zeros under either; EV2a D17).
fn default_state() -> MnCold {
    let mut cold = MnCold::core_default();
    cold.pub_key_operator = OperatorKey::new(BlsSchemeKind::Basic);
    cold.reset_derived();
    cold
}

/// `LegacyPayoutAsList(scriptPayout)`: one share of the whole reward.
// Core: evo/providertx_util.cpp:19 LegacyPayoutAsList
pub(super) fn legacy_payout_as_list(script: &[u8]) -> Vec<PayoutShare> {
    vec![PayoutShare {
        script_payout: script.to_vec().into(),
        reward: MAX_REWARD,
    }]
}

/// `SetStateVersion(state, nVersion, nType)` (EV2 §3.2.6; FR-EVO-05): migrates `scriptPayout`
/// into the payout list when raising to ExtAddr, re-encodes a non-default operator key to the
/// version's scheme (`Set`, so the bytes and the unique slot change), and converts the NetInfo to
/// the version's kind unless both already match.
// Core: evo/specialtxman.cpp:51 SetStateVersion
pub(super) fn set_state_version(
    cold: &mut MnCold,
    target: u16,
    mn_type: RawMnType,
    rules: &NetInfoRules,
) -> Result<(), DmlBuildError> {
    let needs_extended = ext_addr(target);
    if needs_extended && cold.payouts.is_empty() && !cold.script_payout.is_empty() {
        cold.payouts = legacy_payout_as_list(&cold.script_payout);
        cold.script_payout.clear();
    }
    if !is_default_operator_key(&cold.pub_key_operator) {
        let point = cold.pub_key_operator.get();
        if !point.is_valid() {
            return Err(DmlBuildError::Exception(DmlException::OutOfDomain(
                "SetStateVersion: undecodable operator key",
            )));
        }
        cold.pub_key_operator = OperatorKey::from_value(point, scheme_for(i32::from(target)));
    }
    let kind = mn_type.stored_type().unwrap_or(MnType::Regular);
    match netinfo::migrate(&cold.net_info, cold.state_version(), target, kind, rules) {
        Ok(None) => {}
        Ok(Some(converted)) => {
            if needs_extended {
                cold.platform_p2p_port = 0;
                cold.platform_http_port = 0;
            }
            cold.version = i32::from(target);
            cold.net_info = Arc::new(converted);
        }
        Err(reason) => return Err(DmlBuildError::invalid(reason)),
    }
    cold.reset_derived();
    Ok(())
}

/// `ApplyProUpServTx(state, proTx, is_v24_active)`: the service fields, and under V24 the
/// version pre-set to the payload's and `SetStateVersion(max(old, payload))`.
// Core: evo/specialtxman.cpp:110 ApplyProUpServTx
pub(super) fn apply_pro_up_serv(
    cold: &mut MnCold,
    p: &ProUpServTx,
    v24: bool,
    rules: &NetInfoRules,
) -> Result<(), DmlBuildError> {
    let current = cold.state_version();
    let target = if v24 {
        current.max(u16::from(p.version))
    } else {
        current
    };
    if v24 {
        cold.version = i32::from(u16::from(p.version));
    }
    cold.net_info = Arc::new(netinfo::as_core_reads(&p.net_info));
    cold.script_operator_payout = p.script_operator_payout.as_bytes().to_vec();
    if p.mn_type == PayloadMnType::Evo {
        cold.platform_node_id = p.platform_node_id.unwrap_or_default();
        if !ext_addr(u16::from(p.version)) {
            cold.platform_p2p_port = p.platform_p2p_port.unwrap_or(0);
            cold.platform_http_port = p.platform_http_port.unwrap_or(0);
        } else {
            cold.platform_p2p_port = 0;
            cold.platform_http_port = 0;
        }
    }
    cold.reset_derived();
    if v24 {
        set_state_version(cold, target, RawMnType(p.mn_type.to_base()), rules)?;
    }
    Ok(())
}
