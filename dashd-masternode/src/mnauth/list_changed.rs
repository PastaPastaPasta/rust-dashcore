use dash_consensus::hash::sha256d;
use dash_dml::{MnList, MnListDiff, ProTxHash};

/// The verified facts read from one fully connected peer.
// Core: net.h:1432, net.cpp:4874 at v24.0.0
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Peer {
    pub id: i64,
    pub verified_pro: Option<ProTxHash>,
    pub verified_pubkey_hash: [u8; 32],
}

/// The peers Core marks for disconnect and the corresponding log lines, in peer order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ListChanged {
    pub ids: Vec<i64>,
    pub log: Vec<String>,
}

/// Select verified peers whose old masternode was removed or whose operator key changed.
// Core: evo/mnauth.cpp:179-210 at v24.0.0
pub fn list_changed(old: &MnList, diff: &MnListDiff, peers: &[Peer]) -> ListChanged {
    // Core: evo/mnauth.cpp:182-184 at v24.0.0
    if diff.updated.is_empty() && diff.removed.is_empty() {
        return ListChanged::default();
    }

    let mut result = ListChanged::default();
    for peer in peers {
        let Some(pro_tx_hash) = peer.verified_pro else {
            continue;
        };

        // Core: evo/mnauth.cpp:191-193 at v24.0.0
        let Some(old_dmn) = old.get(&pro_tx_hash) else {
            continue;
        };

        let removed = diff.removed.contains(&old_dmn.internal_id);
        let key_changed =
            diff.updated.iter().find(|(id, _)| *id == old_dmn.internal_id).is_some_and(
                |(_, state)| {
                    state.fields & dash_dml::MnStateDiff::PUB_KEY_OPERATOR != 0
                        && peer.verified_pubkey_hash
                            != sha256d(&state.state().1.pub_key_operator.clone().to_bytes())
                },
            );

        if removed || key_changed {
            result.ids.push(peer.id);
            result.log.push(format!(
                "CMNAuth::NotifyMasternodeListChanged -- Disconnecting MN {pro_tx_hash} due to key changed/removed, peer={}",
                peer.id
            ));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use dash_dml::testkit::{dmn, dmn_with, operator_key};
    use dash_dml::{MnList, MnType};
    use dash_pkc::bls::BlsSchemeKind;

    use super::*;

    fn list(entries: impl IntoIterator<Item = Arc<dash_dml::Dmn>>) -> MnList {
        let mut list = MnList::default_pre_dip3();
        for entry in entries {
            list.insert_for_tests(entry);
        }
        list
    }

    fn peer(id: i64, pro: u8, key_hash: [u8; 32]) -> Peer {
        Peer {
            id,
            verified_pro: Some(dash_dml::testkit::pro_tx(pro)),
            verified_pubkey_hash: key_hash,
        }
    }

    fn key_hash(dmn: &dash_dml::Dmn) -> [u8; 32] {
        sha256d(&dmn.cold.pub_key_operator.clone().to_bytes())
    }

    #[test]
    fn no_updates_or_removals_returns_nothing() {
        let old = list([dmn(1, MnType::Regular)]);
        let result = list_changed(&old, &MnListDiff::default(), &[peer(1, 1, [7; 32])]);
        assert_eq!(result, ListChanged::default());
    }

    #[test]
    fn peers_outside_the_old_list_are_skipped() {
        let old_dmn = dmn(1, MnType::Regular);
        let new_dmn = dmn_with(1, MnType::Regular, |_, cold| {
            cold.version = 2;
            cold.pub_key_operator = operator_key(2, BlsSchemeKind::Basic);
        });
        let old = list([old_dmn]);
        let new = list([new_dmn]);
        let peers = [
            Peer {
                id: 1,
                verified_pro: None,
                verified_pubkey_hash: [0; 32],
            },
            peer(2, 2, [1; 32]),
        ];
        let result = list_changed(&old, &old.diff_to(&new), &peers);
        assert_eq!(result, ListChanged::default());
    }

    #[test]
    fn operator_key_change_selects_mismatches_in_peer_order() {
        let old_one = dmn(1, MnType::Regular);
        let old_two = dmn(2, MnType::Regular);
        let new_one = dmn_with(1, MnType::Regular, |_, cold| {
            cold.version = 2;
            cold.pub_key_operator = operator_key(2, BlsSchemeKind::Basic);
        });
        let old = list([old_one.clone(), old_two]);
        let new = list([new_one, dmn(2, MnType::Regular)]);
        let peers = [
            peer(10, 1, key_hash(&old_one)),
            peer(11, 1, [1; 32]),
            peer(12, 2, [1; 32]),
            peer(13, 3, [1; 32]),
            peer(14, 1, [2; 32]),
        ];
        let result = list_changed(&old, &old.diff_to(&new), &peers);
        assert_eq!(result.ids, [10, 11, 14]);
        let expected = [10, 11, 14]
            .into_iter()
            .map(|id| format!(
                "CMNAuth::NotifyMasternodeListChanged -- Disconnecting MN {} due to key changed/removed, peer={id}",
                old_one.pro_tx_hash
            ))
            .collect::<Vec<_>>();
        assert_eq!(result.log, expected);
    }

    #[test]
    fn operator_key_revocation_selects_the_old_key_peer() {
        let old_dmn = dmn_with(1, MnType::Regular, |_, cold| {
            cold.version = 2;
            cold.pub_key_operator = operator_key(2, BlsSchemeKind::Basic);
        });
        let revoked = dmn_with(1, MnType::Regular, |dmn, cold| {
            dmn.hot.ban_if_not_banned(100);
            cold.pub_key_operator = dash_dml::OperatorKey::new(BlsSchemeKind::Basic);
            cold.version = 2;
        });
        let old = list([old_dmn.clone()]);
        let new = list([revoked]);
        let result = list_changed(&old, &old.diff_to(&new), &[peer(42, 1, key_hash(&old_dmn))]);
        assert_eq!(result.ids, [42]);
    }

    #[test]
    fn removal_selects_the_matching_peer() {
        let old = list([dmn(1, MnType::Regular), dmn(2, MnType::Regular)]);
        let new = list([dmn(2, MnType::Regular)]);
        let result = list_changed(
            &old,
            &old.diff_to(&new),
            &[peer(7, 2, [1; 32]), peer(8, 1, [1; 32]), peer(9, 3, [1; 32])],
        );
        assert_eq!(result.ids, [8]);
    }

    fn unhex<const N: usize>(text: &str, reverse: bool) -> [u8; N] {
        let mut bytes =
            std::array::from_fn(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap());
        assert_eq!(text.len(), 2 * N);
        if reverse {
            bytes.reverse();
        }
        bytes
    }

    fn recorded_list(row: &serde_json::Value) -> MnList {
        use dash_dml::testkit::dmn_with;
        use dash_pkc::bls::{BlsSchemeKind, BlsSchemeSource};
        use dash_primitives::{NetInfo, NetInfoV1, OutPoint, TxHash};
        use dash_types::codec::BaseCodec;

        let mut list = MnList::default_pre_dip3();
        for (n, info) in row["list"].as_array().unwrap().iter().enumerate() {
            let state = &info["state"];
            let service = state["addresses"]["core_p2p"][0]
                .as_str()
                .map(|service| dash_netaddr::Service::from_socket_addr(service.parse().unwrap()))
                .unwrap_or_default();
            let mut bytes = service.addr.v1_bytes().to_vec();
            bytes.extend_from_slice(&service.port.to_be_bytes());
            let dmn = dmn_with(n as u8 + 1, MnType::Regular, |d, c| {
                d.pro_tx_hash = ProTxHash::from(unhex(info["proTxHash"].as_str().unwrap(), true));
                d.collateral = OutPoint::new(
                    TxHash::from(unhex(info["collateralHash"].as_str().unwrap(), true)),
                    info["collateralIndex"].as_u64().unwrap() as u32,
                );
                c.version = state["version"].as_i64().unwrap() as i32;
                c.pub_key_operator = dash_dml::OperatorKey::from_bytes(
                    &unhex(state["pubKeyOperator"].as_str().unwrap(), false),
                    BlsSchemeSource::Version(BlsSchemeKind::from_legacy(c.version == 1)),
                );
                c.net_info =
                    Arc::new(NetInfo::Legacy(NetInfoV1::decode(&mut bytes.as_slice()).unwrap()));
            });
            list.insert_for_tests(dmn);
        }
        list
    }

    // Core: evo/mnauth.cpp:179-210 at v24.0.0
    #[test]
    fn core_state_capture_selects_key_changes_revocations_and_removals() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/am1-state/core-v24.json"))
                .unwrap();
        let rows = fixture["rows"].as_array().unwrap();
        let row = |step: &str| rows.iter().find(|row| row["step"] == step).unwrap();
        for (id, before, after) in [
            (101, "registered", "proupreg"),
            (102, "operator_restored", "prouprev"),
            (103, "prouprev", "reregister"),
        ] {
            let old = recorded_list(row(before));
            let new = recorded_list(row(after));
            let pro =
                ProTxHash::from(unhex(row(before)["info"]["proTxHash"].as_str().unwrap(), true));
            let old_dmn = old.get(&pro).unwrap();
            let peer = Peer {
                id,
                verified_pro: Some(pro),
                verified_pubkey_hash: key_hash(old_dmn),
            };
            let result = list_changed(&old, &old.diff_to(&new), &[peer]);
            assert_eq!(result.ids, [id], "{before} -> {after}");
        }
    }
}
