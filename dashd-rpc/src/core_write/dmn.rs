//! A deterministic masternode's `ToJson` as `protx list`, `protx info` and `protx listdiff`
//! print it: `CDeterministicMN::ToJson`, `CDeterministicMNState::ToJson`,
//! `CDeterministicMNStateDiff::ToJson` and `CMasternodeMetaInfo::ToJson`.
//!
//! `collateralAddress` is never printed: Core prints it only with `-txindex`, which the node
//! does not have.

use dash_dml::{Dmn, MnCold, MnHot, MnStateDiff};

use super::evo::{
    AddressMode, EvoNet, key_address, lazy_key, payout_list, script_address, share_list, uint_hex,
};
use super::{CheckFailed, TxJsonParams};
use crate::UniValue;
use crate::evo::MetaFacts;

/// `ProTxVersion::ExtAddr`.
// Core: evo/types.h:19
const PROTX_EXT_ADDR: i32 = 3;

/// The network info of a state as Core's writers see it, with the legacy platform ports.
fn evo_net(mn_type: u16, cold: &MnCold) -> EvoNet<'_> {
    EvoNet::new(
        &cold.net_info,
        mn_type,
        Some(cold.platform_p2p_port),
        Some(cold.platform_http_port),
    )
}

/// `UniValue::pushKV`: an existing key keeps its position and takes the new value.
fn push_kv(obj: &mut UniValue, key: &str, value: UniValue) {
    if let UniValue::Obj(entries) = obj
        && let Some((_, slot)) = entries.iter_mut().find(|(k, _)| k == key)
    {
        *slot = value;
        return;
    }
    obj.push_kv_end(key, value);
}

/// `GetMnType(nType).description`.
// Core: evo/dmn_types.h:33-62
fn mn_type_description(mn_type: u16) -> &'static str {
    match mn_type {
        0 => "Regular",
        1 => "Evo",
        _ => "Invalid",
    }
}

/// `CDeterministicMN::ToJson` without `collateralAddress` (no txindex).
// Core: rpc/evo.cpp:63-84
pub(crate) fn dmn_to_json(dmn: &Dmn, p: &TxJsonParams) -> Result<UniValue, CheckFailed> {
    let mut obj = UniValue::new_object();
    obj.push_kv_end("type", mn_type_description(dmn.mn_type.0).into());
    obj.push_kv_end("proTxHash", dmn.pro_tx_hash.to_string().into());
    obj.push_kv_end("collateralHash", dmn.collateral.txid().to_string().into());
    obj.push_kv_end("collateralIndex", i64::from(dmn.collateral.index()).into());
    let reward = UniValue::from_f64(f64::from(dmn.operator_reward) / 100.0)
        .expect("a finite operator reward");
    obj.push_kv_end("operatorReward", reward);
    obj.push_kv_end("state", state_to_json(dmn.mn_type.0, &dmn.hot, &dmn.cold, p)?);
    Ok(obj)
}

/// `CDeterministicMNState::ToJson(nType)`.
// Core: evo/core_write.cpp:150-191
pub(crate) fn state_to_json(
    mn_type: u16,
    hot: &MnHot,
    cold: &MnCold,
    p: &TxJsonParams,
) -> Result<UniValue, CheckFailed> {
    let net = evo_net(mn_type, cold);
    let shared = !cold.shares.is_empty();
    let mut obj = UniValue::new_object();
    obj.push_kv_end("version", i64::from(cold.version).into());
    if p.service {
        obj.push_kv_end("service", net.service());
    }
    obj.push_kv_end("addresses", net.addresses(AddressMode::FullState));
    obj.push_kv_end("registeredHeight", i64::from(cold.registered_height).into());
    obj.push_kv_end("lastPaidHeight", i64::from(hot.last_paid_height).into());
    obj.push_kv_end("consecutivePayments", i64::from(hot.consecutive_payments).into());
    obj.push_kv_end("PoSePenalty", i64::from(hot.pose_penalty).into());
    obj.push_kv_end("PoSeRevivedHeight", i64::from(hot.pose_revived_height).into());
    obj.push_kv_end("PoSeBanHeight", i64::from(hot.banned_height()).into());
    obj.push_kv_end("revocationReason", i64::from(cold.revocation_reason).into());
    if !shared {
        obj.push_kv_end("ownerAddress", key_address(&cold.key_id_owner, p));
    }
    obj.push_kv_end("votingAddress", key_address(&cold.key_id_voting, p));
    net.push_evo_fields(Some(&cold.platform_node_id), &mut obj, p)?;
    if shared {
        obj.push_kv_end("shares", share_list(&cold.shares, p));
        obj.push_kv_end("earlyPeriodBlocks", i64::from(cold.early_period_blocks).into());
        obj.push_kv_end("earlyPenalty", cold.early_penalty.into());
    } else if cold.version >= PROTX_EXT_ADDR {
        obj.push_kv_end("payouts", payout_list(&cold.payouts, p));
    } else if let Some(address) = script_address(&cold.script_payout, p) {
        obj.push_kv_end("payoutAddress", address);
    }
    obj.push_kv_end("pubKeyOperator", lazy_key(&cold.pub_key_operator));
    if let Some(address) = script_address(&cold.script_operator_payout, p) {
        obj.push_kv_end("operatorPayoutAddress", address);
    }
    Ok(obj)
}

/// `CDeterministicMNStateDiff::ToJson(nType)` of `target`'s diff, then `protx listdiff`'s
/// replacement: when the diff sets `netInfo` and `target`'s own `netInfo` is not empty,
/// `addresses` becomes `target`'s full legacy form in place. The diff's set fields print from
/// the diff's state; the replacement reads `target`'s full state, as Core reads `dmn->pdmnState`.
// Core: evo/dmnstate.cpp:50-169; rpc/evo.cpp:1991-1996
pub(crate) fn state_diff_to_json(target: &Dmn, diff: &MnStateDiff, p: &TxJsonParams) -> UniValue {
    let target_type = target.mn_type.0;
    let (hot, cold) = diff.state();
    let has = |field: u32| diff.fields & field != 0;
    let net = evo_net(target_type, cold);
    let evo = target_type == 1;
    let mut obj = UniValue::new_object();
    if has(MnStateDiff::VERSION) {
        obj.push_kv_end("version", i64::from(cold.version).into());
    }
    if has(MnStateDiff::NET_INFO) && p.service {
        obj.push_kv_end("service", net.service());
    }
    let ints: [(u32, &str, i64); 7] = [
        (MnStateDiff::REGISTERED_HEIGHT, "registeredHeight", cold.registered_height.into()),
        (MnStateDiff::LAST_PAID_HEIGHT, "lastPaidHeight", hot.last_paid_height.into()),
        (MnStateDiff::CONSECUTIVE_PAYMENTS, "consecutivePayments", hot.consecutive_payments.into()),
        (MnStateDiff::POSE_PENALTY, "PoSePenalty", hot.pose_penalty.into()),
        (MnStateDiff::POSE_REVIVED_HEIGHT, "PoSeRevivedHeight", hot.pose_revived_height.into()),
        (MnStateDiff::POSE_BAN_HEIGHT, "PoSeBanHeight", hot.banned_height().into()),
        (MnStateDiff::REVOCATION_REASON, "revocationReason", cold.revocation_reason.into()),
    ];
    for (field, key, value) in ints {
        if has(field) {
            obj.push_kv_end(key, value.into());
        }
    }
    if has(MnStateDiff::KEY_ID_OWNER) {
        obj.push_kv_end("ownerAddress", key_address(&cold.key_id_owner, p));
    }
    if has(MnStateDiff::KEY_ID_VOTING) {
        obj.push_kv_end("votingAddress", key_address(&cold.key_id_voting, p));
    }
    if has(MnStateDiff::SCRIPT_PAYOUT)
        && let Some(address) = script_address(&cold.script_payout, p)
    {
        obj.push_kv_end("payoutAddress", address);
    }
    if has(MnStateDiff::PAYOUTS) {
        obj.push_kv_end("payouts", payout_list(&cold.payouts, p));
    }
    if has(MnStateDiff::SHARES) {
        obj.push_kv_end("shares", share_list(&cold.shares, p));
    }
    if has(MnStateDiff::EARLY_PERIOD_BLOCKS) {
        obj.push_kv_end("earlyPeriodBlocks", i64::from(cold.early_period_blocks).into());
    }
    if has(MnStateDiff::EARLY_PENALTY) {
        obj.push_kv_end("earlyPenalty", cold.early_penalty.into());
    }
    if has(MnStateDiff::SCRIPT_OPERATOR_PAYOUT)
        && let Some(address) = script_address(&cold.script_operator_payout, p)
    {
        obj.push_kv_end("operatorPayoutAddress", address);
    }
    if has(MnStateDiff::PUB_KEY_OPERATOR) {
        obj.push_kv_end("pubKeyOperator", lazy_key(&cold.pub_key_operator));
    }
    let has_net_info = has(MnStateDiff::NET_INFO);
    if evo {
        if has(MnStateDiff::PLATFORM_NODE_ID) {
            obj.push_kv_end("platformNodeID", uint_hex(&cold.platform_node_id));
        }
        if p.service {
            // From ExtAddr the scalar ports are unused; an extended netInfo in the diff reports
            // its own platform entries.
            let has_ext = has_net_info && net.can_store_platform();
            let port = |p2p: bool, stored: u16| {
                let live = net.first_platform_port(p2p).filter(|_| has_ext);
                i64::from(live.unwrap_or(stored)).into()
            };
            if has(MnStateDiff::PLATFORM_P2P_PORT) {
                obj.push_kv_end("platformP2PPort", port(true, cold.platform_p2p_port));
            }
            if has(MnStateDiff::PLATFORM_HTTP_PORT) {
                obj.push_kv_end("platformHTTPPort", port(false, cold.platform_http_port));
            }
        }
    }
    let mut addresses = if has_net_info {
        net.net_info_json()
    } else {
        UniValue::new_object()
    };
    if evo && (!has_net_info || !net.can_store_platform()) {
        // An address the diff does not carry prints as 255.255.255.255 at the changed port.
        let at = |port: u16| {
            if has_net_info {
                net.primary_at(port)
            } else {
                UniValue::Arr(vec![format!("255.255.255.255:{port}").into()])
            }
        };
        if has(MnStateDiff::PLATFORM_P2P_PORT) {
            push_kv(&mut addresses, "platform_p2p", at(cold.platform_p2p_port));
        }
        if has(MnStateDiff::PLATFORM_HTTP_PORT) {
            push_kv(&mut addresses, "platform_https", at(cold.platform_http_port));
        }
    }
    if !matches!(&addresses, UniValue::Obj(entries) if entries.is_empty()) {
        obj.push_kv_end("addresses", addresses);
    }
    let full = evo_net(target_type, &target.cold);
    if has_net_info && !full.is_empty() {
        push_kv(&mut obj, "addresses", full.addresses(AddressMode::FullState));
    }
    obj
}

/// `CMasternodeMetaInfo::ToJson()`, its elapsed times against the mockable `now`.
// Core: masternode/meta.cpp:38-52
pub(crate) fn meta_to_json(meta: &MetaFacts, now: i64) -> UniValue {
    let mut obj = UniValue::new_object();
    obj.push_kv_end("lastDSQ", meta.last_dsq.into());
    obj.push_kv_end("mixingTxCount", i64::from(meta.mixing_tx_count).into());
    obj.push_kv_end("outboundAttemptCount", i64::from(meta.outbound_attempt_count).into());
    obj.push_kv_end("lastOutboundAttempt", meta.last_outbound_attempt.into());
    obj.push_kv_end(
        "lastOutboundAttemptElapsed",
        now.wrapping_sub(meta.last_outbound_attempt).into(),
    );
    obj.push_kv_end("lastOutboundSuccess", meta.last_outbound_success.into());
    obj.push_kv_end(
        "lastOutboundSuccessElapsed",
        now.wrapping_sub(meta.last_outbound_success).into(),
    );
    obj.push_kv_end("is_platform_banned", meta.is_platform_banned.into());
    obj.push_kv_end(
        "platform_ban_height_updated",
        i64::from(meta.platform_ban_height_updated).into(),
    );
    obj
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;

    use dash_dml::Dmn;

    use super::*;
    use crate::key_io::KeyIoParams;

    pub(crate) fn params(service: bool) -> TxJsonParams {
        TxJsonParams {
            key_io: KeyIoParams {
                pubkey_address: 140,
                script_address: 19,
                secret_key: 239,
                ext_public_key: [0; 4],
                ext_secret_key: [0; 4],
                platform_hrp: Some("tdash".into()),
            },
            service,
        }
    }

    fn evo(ip: u8) -> Dmn {
        evo_dmn(1, ip)
    }

    /// A version-2 Evo masternode in evodb's encoding: proTxHash, internal id, collateral index
    /// and owner key from `pro_tx`, a
    /// legacy network info at `1.2.3.<ip>:19999`, platform ports 26656 and 443, and an operator
    /// reward of 12.34%.
    pub(crate) fn evo_dmn(pro_tx: u8, ip: u8) -> Dmn {
        let mut bytes = vec![pro_tx; 32];
        bytes.push(pro_tx);
        bytes.extend([2; 32]);
        bytes.extend(u32::from(pro_tx).to_le_bytes());
        bytes.extend(1234u16.to_le_bytes());
        for n in [2i32, 1, 0, 0, 0, -1, -1] {
            bytes.extend(n.to_le_bytes());
        }
        bytes.extend(0u16.to_le_bytes());
        bytes.extend([0; 64]);
        bytes.extend([pro_tx; 20]);
        bytes.extend([0; 48 + 20]);
        bytes.extend([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 255, 1, 2, 3, ip]);
        bytes.extend(19999u16.to_be_bytes());
        bytes.extend([0; 2 + 20]);
        bytes.extend(26656u16.to_le_bytes());
        bytes.extend(443u16.to_le_bytes());
        bytes.extend(1u16.to_le_bytes());
        let mut input = bytes.as_slice();
        let dmn = dash_dml::codec::core_disk::decode_dmn(&mut input).unwrap();
        assert!(input.is_empty() && dmn.is_evo());
        dmn
    }

    fn with_cold(dmn: &Dmn, f: impl FnOnce(&mut MnCold)) -> Dmn {
        let mut cold = (*dmn.cold).clone();
        f(&mut cold);
        Dmn {
            cold: Arc::new(cold),
            ..dmn.clone()
        }
    }

    fn json(text: &str) -> UniValue {
        UniValue::read(text.as_bytes()).unwrap()
    }

    #[test]
    fn dmn_and_state_print_core_keys_in_core_order() {
        let dmn = evo(4);
        let out = dmn_to_json(&dmn, &params(false)).unwrap();
        let keys = |v: &UniValue| match v {
            UniValue::Obj(fields) => fields.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>(),
            other => panic!("{}", other.write()),
        };
        assert_eq!(
            keys(&out),
            ["type", "proTxHash", "collateralHash", "collateralIndex", "operatorReward", "state"]
        );
        let field = |v: &UniValue, k: &str| match v {
            UniValue::Obj(fields) => fields.iter().find(|(key, _)| key == k).unwrap().1.clone(),
            _ => unreachable!(),
        };
        assert_eq!(field(&out, "type"), "Evo".into());
        assert_eq!(field(&out, "collateralIndex").write(), "1");
        assert_eq!(field(&out, "operatorReward").write(), "12.34");
        let state = field(&out, "state");
        assert_eq!(
            keys(&state),
            [
                "version",
                "addresses",
                "registeredHeight",
                "lastPaidHeight",
                "consecutivePayments",
                "PoSePenalty",
                "PoSeRevivedHeight",
                "PoSeBanHeight",
                "revocationReason",
                "ownerAddress",
                "votingAddress",
                "platformNodeID",
                "pubKeyOperator",
            ]
        );
        assert_eq!(
            field(&state, "addresses"),
            json(
                r#"{"core_p2p":["1.2.3.4:19999"],"platform_https":["1.2.3.4:443"],"platform_p2p":["1.2.3.4:26656"]}"#
            )
        );
        let service = dmn_to_json(&dmn, &params(true)).unwrap();
        let state = field(&service, "state");
        assert_eq!(keys(&state)[1], "service");
        assert_eq!(field(&state, "service"), "1.2.3.4:19999".into());
        assert_eq!(field(&state, "platformP2PPort").write(), "26656");
        assert_eq!(field(&state, "platformHTTPPort").write(), "443");
        let regular = Dmn {
            mn_type: dash_evo::types::RawMnType(0),
            ..dmn.clone()
        };
        let out = dmn_to_json(&regular, &params(true)).unwrap();
        assert_eq!(field(&out, "type"), "Regular".into());
        assert!(!keys(&field(&out, "state")).iter().any(|k| k.starts_with("platform")));
        let unknown = Dmn {
            mn_type: dash_evo::types::RawMnType(7),
            ..dmn
        };
        assert_eq!(
            field(&dmn_to_json(&unknown, &params(false)).unwrap(), "type"),
            "Invalid".into()
        );
    }

    #[test]
    fn state_diff_prints_set_fields_only() {
        let a = evo(4);
        let mut b = a.clone();
        b.hot.last_paid_height = 5;
        b.hot.consecutive_payments = 1;
        let diff = MnStateDiff::between(&a, &b);
        assert_eq!(
            state_diff_to_json(&b, &diff, &params(true)),
            json(r#"{"lastPaidHeight":5,"consecutivePayments":1}"#)
        );
    }

    #[test]
    fn port_only_change_prints_the_unknown_address() {
        let a = evo(4);
        let b = with_cold(&a, |cold| cold.platform_p2p_port = 26657);
        let diff = MnStateDiff::between(&a, &b);
        assert_eq!(diff.fields, MnStateDiff::PLATFORM_P2P_PORT);
        assert_eq!(
            state_diff_to_json(&b, &diff, &params(false)),
            json(r#"{"addresses":{"platform_p2p":["255.255.255.255:26657"]}}"#)
        );
        assert_eq!(
            state_diff_to_json(&b, &diff, &params(true)),
            json(
                r#"{"platformP2PPort":26657,"addresses":{"platform_p2p":["255.255.255.255:26657"]}}"#
            )
        );
        // A Regular node's platform fields print nothing.
        assert_eq!(state_diff_to_json(&regular(&b), &diff, &params(true)), UniValue::new_object());
    }

    #[test]
    fn net_info_change_replaces_addresses_with_the_full_legacy_form() {
        let a = evo(4);
        let b = with_cold(&evo(5), |cold| cold.platform_http_port = 444);
        let diff = MnStateDiff::between(&a, &b);
        assert_eq!(
            diff.fields,
            MnStateDiff::NET_INFO | MnStateDiff::VERSION | MnStateDiff::PLATFORM_HTTP_PORT
        );
        let full = r#"{"core_p2p":["1.2.3.5:19999"],"platform_https":["1.2.3.5:444"],"platform_p2p":["1.2.3.5:26656"]}"#;
        assert_eq!(
            state_diff_to_json(&b, &diff, &params(false)),
            json(&format!(r#"{{"version":2,"addresses":{full}}}"#))
        );
        assert_eq!(
            state_diff_to_json(&b, &diff, &params(true)),
            json(&format!(
                r#"{{"version":2,"service":"1.2.3.5:19999","platformHTTPPort":444,"addresses":{full}}}"#
            ))
        );
        // A Regular node prints its core address only.
        assert_eq!(
            state_diff_to_json(&regular(&b), &diff, &params(false)),
            json(r#"{"version":2,"addresses":{"core_p2p":["1.2.3.5:19999"]}}"#)
        );
    }

    /// The replacement reads the target's full state, not the diff's: a diff whose state holds
    /// only the new `netInfo` (its unchanged platform ports unset) still prints the target's
    /// ports.
    #[test]
    fn net_info_replacement_reads_the_target_not_the_diff_state() {
        let no_ports = |dmn: &Dmn| {
            with_cold(dmn, |cold| {
                cold.platform_p2p_port = 0;
                cold.platform_http_port = 0;
            })
        };
        let target = evo(5);
        let diff = MnStateDiff::between(&no_ports(&evo(4)), &no_ports(&target));
        assert_eq!(diff.fields, MnStateDiff::NET_INFO | MnStateDiff::VERSION);
        assert_eq!(diff.state().1.platform_p2p_port, 0);
        assert_eq!(
            state_diff_to_json(&target, &diff, &params(false)),
            json(
                r#"{"version":2,"addresses":{"core_p2p":["1.2.3.5:19999"],"platform_https":["1.2.3.5:443"],"platform_p2p":["1.2.3.5:26656"]}}"#
            )
        );
    }

    fn regular(dmn: &Dmn) -> Dmn {
        Dmn {
            mn_type: dash_evo::types::RawMnType(0),
            ..dmn.clone()
        }
    }

    #[test]
    fn meta_elapsed_times_count_from_now() {
        let meta = MetaFacts {
            last_dsq: 7,
            mixing_tx_count: 2,
            outbound_attempt_count: 3,
            last_outbound_attempt: 100,
            last_outbound_success: 0,
            is_platform_banned: true,
            platform_ban_height_updated: 9,
        };
        assert_eq!(
            meta_to_json(&meta, 1_000),
            json(
                r#"{"lastDSQ":7,"mixingTxCount":2,"outboundAttemptCount":3,"lastOutboundAttempt":100,"lastOutboundAttemptElapsed":900,"lastOutboundSuccess":0,"lastOutboundSuccessElapsed":1000,"is_platform_banned":true,"platform_ban_height_updated":9}"#
            )
        );
    }
}
