//! dashd's `protx list registered true` and V's masternode list in the same JSON, shared by the
//! dashd differentials that compare lists (`ch7a5_dashd`, `v19_window_dashd`).

use std::collections::BTreeMap;

use dash_dml::netinfo::{is_empty, primary};
use dash_dml::{Dmn, MnList};
use dash_netaddr::Service;
use dash_primitives::NetInfo;
use dashd_node::sut::NodeSut;
use dashd_rpc::key_io::{Destination, KeyIoParams, encode_destination, extract_destination};
use serde_json::{Value, json};

use super::{Harness, hex};

/// `key_io.cpp`'s regtest parameters, as `dashd-node`'s RPC takes them.
pub fn key_io(sut: &NodeSut) -> KeyIoParams {
    let a = &sut.params().addr_params;
    KeyIoParams {
        pubkey_address: a.pubkey_addr,
        script_address: a.script_addr,
        secret_key: a.secret_key,
        ext_public_key: a.ext_pubkey,
        ext_secret_key: a.ext_secret,
        platform_hrp: sut.params().platform_hrp.map(str::to_owned),
    }
}

/// Core's `CDeterministicMN::ToJson()` for `dmn` (`rpc/evo.cpp:63-86`, its state's
/// `evo/core_write.cpp:154-196`), without `collateralAddress`, which needs `-txindex`: what
/// `protx list registered true` prints of it, less `BuildDMNListEntry`'s confirmations, wallet
/// and meta fields. The deprecated `service` fields are off by default. Only what the recordings
/// have: version 1 and 2 states (legacy `netInfo`, `scriptPayout`), no shared masternodes.
// Core: rpc/evo.cpp:63-86; evo/core_write.cpp:38-57, :154-196; evo/netinfo.cpp:359-374
fn core_json(dmn: &Dmn, key_io: &KeyIoParams) -> Value {
    let (hot, cold) = (&dmn.hot, &*dmn.cold);
    assert!(cold.version < 3 && cold.shares.is_empty(), "not in the recording: {dmn:?}");
    let pkh = |id: &[u8; 20]| encode_destination(&Destination::PkHash(*id), key_io);
    let address =
        |script: &[u8]| extract_destination(script).map(|d| encode_destination(&d, key_io));

    let mut addresses = serde_json::Map::new();
    if !is_empty(&cold.net_info) {
        let p = primary(&cold.net_info);
        addresses.insert("core_p2p".into(), json!([p.to_string()]));
        // `GetNetInfoWithLegacyFields`: an EvoNode's platform ports on the primary address.
        if dmn.is_evo() && matches!(*cold.net_info, NetInfo::Legacy(_)) {
            let at = |port| {
                Service {
                    addr: p.addr.clone(),
                    port,
                }
                .to_string()
            };
            addresses.insert("platform_https".into(), json!([at(cold.platform_http_port)]));
            addresses.insert("platform_p2p".into(), json!([at(cold.platform_p2p_port)]));
        }
    }
    let mut state = json!({
        "version": cold.version,
        "addresses": addresses,
        "registeredHeight": cold.registered_height,
        "lastPaidHeight": hot.last_paid_height,
        "consecutivePayments": hot.consecutive_payments,
        "PoSePenalty": hot.pose_penalty,
        "PoSeRevivedHeight": hot.pose_revived_height,
        "PoSeBanHeight": hot.banned_height(),
        "revocationReason": cold.revocation_reason,
        "ownerAddress": pkh(cold.key_id_owner.as_bytes()),
        "votingAddress": pkh(cold.key_id_voting.as_bytes()),
        "pubKeyOperator": hex(&cold.pub_key_operator.to_bytes_as(cold.key_scheme())),
    });
    let o = state.as_object_mut().expect("an object");
    if dmn.is_evo() {
        // `uint160::ToString`: the bytes reversed.
        let mut id = *cold.platform_node_id.as_bytes();
        id.reverse();
        o.insert("platformNodeID".into(), json!(hex(&id)));
    }
    if let Some(a) = address(&cold.script_payout) {
        o.insert("payoutAddress".into(), json!(a));
    }
    if let Some(a) = address(&cold.script_operator_payout) {
        o.insert("operatorPayoutAddress".into(), json!(a));
    }
    // `static_cast<double>(nOperatorReward) / 100`, which UniValue prints without a fraction
    // when it has none.
    let reward = dmn.operator_reward;
    let reward = if reward.is_multiple_of(100) {
        json!(reward / 100)
    } else {
        json!(f64::from(reward) / 100.0)
    };
    json!({
        "type": if dmn.is_evo() { "Evo" } else { "Regular" },
        "proTxHash": dmn.pro_tx_hash.to_string(),
        "collateralHash": dmn.collateral.txid().to_string(),
        "collateralIndex": dmn.collateral.index(),
        "operatorReward": reward,
        "state": state,
    })
}

/// `list` as [`core_json`]'s objects by proTxHash.
pub fn ours(list: &MnList, key_io: &KeyIoParams) -> BTreeMap<String, Value> {
    list.iter().map(|dmn| (dmn.pro_tx_hash.to_string(), core_json(dmn, key_io))).collect()
}

/// dashd's `protx list registered true height` by proTxHash, without the fields [`core_json`]
/// leaves out.
pub fn dashd_list(h: &Harness, height: usize) -> BTreeMap<String, Value> {
    let list = h.dashd().call("protx", json!(["list", "registered", true, height]));
    let list = list.as_array().expect("an array").clone();
    list.into_iter()
        .map(|mut mn| {
            let o = mn.as_object_mut().expect("an object");
            for key in ["collateralAddress", "confirmations", "metaInfo", "wallet"] {
                o.remove(key);
            }
            (o["proTxHash"].as_str().expect("a hash").to_owned(), mn)
        })
        .collect()
}
