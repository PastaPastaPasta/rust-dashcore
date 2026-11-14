use super::*;

#[test]
fn a_fixed_tip_keeps_its_list_and_scheme() {
    let list = Arc::new(MnList::default_pre_dip3());
    for bls in [BlsCtx::LEGACY, BlsCtx::BASIC] {
        let tip = TipMnList::fixed(TipListRef {
            list: Arc::clone(&list),
            bls,
        });
        let loaded = tip.clone().load();
        assert!(Arc::ptr_eq(&loaded.list, &list));
        assert_eq!(loaded.bls, bls);
    }
}

#[cfg(feature = "net")]
mod adapter {
    use super::*;
    use dash_dml::testkit::{dmn_with, pro_tx};
    use dash_dml::{MnType, OperatorKey, ProTxHash};
    use dash_pkc::bls::{BlsSchemeKind, BlsSchemeSource};
    use dashd_p2p::processing::seams::MnListView;

    fn unhex<const N: usize>(hex: &str) -> [u8; N] {
        assert_eq!(hex.len(), N * 2);
        std::array::from_fn(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap())
    }

    fn vectors() -> serde_json::Value {
        serde_json::from_str(include_str!("../../tests/fixtures/tip-mn-list/operator.json"))
            .unwrap()
    }

    fn pro_reg_tx() -> ProTxHash {
        ProTxHash::from(std::array::from_fn(|i| i as u8))
    }

    fn with_operator(row: &serde_json::Value, outside_valid: bool) -> TipListRef {
        let bytes = unhex::<48>(row["pubkey"].as_str().unwrap());
        let scheme = if row["version"] == 1 {
            BlsSchemeKind::Legacy
        } else {
            BlsSchemeKind::Basic
        };
        let entry = dmn_with(1, MnType::Regular, |dmn, cold| {
            dmn.pro_tx_hash = pro_reg_tx();
            cold.version = row["version"].as_i64().unwrap() as i32;
            cold.pub_key_operator =
                OperatorKey::from_bytes(&bytes, BlsSchemeSource::Version(scheme));
            if outside_valid {
                dmn.hot.ban_if_not_banned(100);
            }
        });
        let mut list = MnList::default_pre_dip3();
        list.insert_for_tests(entry);
        assert_eq!(list.get_valid(&pro_reg_tx()).is_none(), outside_valid);
        TipListRef {
            list: Arc::new(list),
            bls: BlsCtx::BASIC,
        }
    }

    fn with_service() -> (TipListRef, dash_netaddr::Service, [u8; 32]) {
        use dash_primitives::{AddrV1, NetInfo, NetInfoV1, ServiceV1};

        let service =
            dash_netaddr::Service::new(dash_netaddr::NetAddr::ipv4([10, 20, 30, 40]), 19_899);
        let entry = dmn_with(1, MnType::Regular, |_, cold| {
            cold.net_info = Arc::new(NetInfo::Legacy(NetInfoV1::new(ServiceV1::new(
                AddrV1::from([10, 20, 30, 40]),
                service.port,
            ))));
        });
        let pro_reg_tx_hash = *entry.pro_tx_hash.as_bytes();
        let mut list = MnList::default_pre_dip3();
        list.insert_for_tests(entry);
        (
            TipListRef {
                list: Arc::new(list),
                bls: BlsCtx::BASIC,
            },
            service,
            pro_reg_tx_hash,
        )
    }

    #[test]
    fn by_service_returns_the_matching_pro_reg_tx_hash() {
        let (view, service, pro_reg_tx_hash) = with_service();
        assert_eq!(view.by_service(&service), Some(pro_reg_tx_hash));
    }

    #[test]
    fn by_service_returns_none_for_an_unknown_service() {
        let (view, mut service, _) = with_service();
        service.addr = dash_netaddr::NetAddr::ipv4([10, 20, 30, 41]);
        assert_eq!(view.by_service(&service), None);
    }

    #[test]
    fn by_service_includes_the_port_in_the_match() {
        let (view, mut service, _) = with_service();
        service.port += 1;
        assert_eq!(view.by_service(&service), None);
    }

    #[test]
    fn operator_matches_legacy_and_basic_hash_vectors() {
        for row in vectors().as_array().unwrap() {
            let view = with_operator(row, false);
            let key = view.operator(pro_reg_tx().as_bytes()).unwrap();
            assert_eq!(key.stored, unhex::<48>(row["pubkey"].as_str().unwrap()));
            assert_eq!(key.stored_legacy, row["version"] == 1);
            assert_eq!(key.pubkey_hash, unhex::<32>(row["pubkey_hash"].as_str().unwrap()));
        }
    }

    #[test]
    fn operator_is_absent_for_an_unknown_pro_reg_tx() {
        let view = with_operator(&vectors()[0], false);
        assert_eq!(view.operator(pro_tx(2).as_bytes()), None);
    }

    // Core: evo/mnauth.cpp:85 (`GetMN`, not `GetValidMN`) at v24.0.0
    #[test]
    fn operator_follows_get_mn_for_an_entry_get_valid_mn_skips() {
        for row in vectors().as_array().unwrap() {
            let view = with_operator(row, true);
            let key = view.operator(pro_reg_tx().as_bytes()).unwrap();
            assert_eq!(key.stored, unhex::<48>(row["pubkey"].as_str().unwrap()));
            assert_eq!(key.stored_legacy, row["version"] == 1);
            assert_eq!(key.pubkey_hash, unhex::<32>(row["pubkey_hash"].as_str().unwrap()));
        }
    }

    #[test]
    fn a_legacy_stored_key_keeps_its_hash_under_the_basic_process_scheme() {
        let rows = vectors();
        let mut view = with_operator(&rows[0], false);
        let key = view.operator(pro_reg_tx().as_bytes()).unwrap();
        assert_eq!(key.stored, unhex::<48>(rows[0]["pubkey"].as_str().unwrap()));
        assert!(key.stored_legacy);
        assert_eq!(key.pubkey_hash, unhex::<32>(rows[0]["pubkey_hash"].as_str().unwrap()));
        assert!(!view.bls_legacy_scheme());
        view.bls = BlsCtx::LEGACY;
        assert!(view.bls_legacy_scheme());
        assert_eq!(view.operator(pro_reg_tx().as_bytes()), Some(key));
    }
}
