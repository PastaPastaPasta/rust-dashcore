use super::*;
use dash_primitives::TxHash;

#[test]
fn the_default_list_is_cores() {
    let list = MnList::default_pre_dip3();
    assert!(list.block_hash().is_null());
    assert_eq!((list.height(), list.total_registered(), list.len()), (-1, 0, 0));
    assert_eq!(list.provenance(), Provenance::FromGenesis);
    assert!(list.same_record(&MnList::scratch()));
}

#[test]
fn a_positioned_list_is_decoded_and_compares_by_content() {
    let hash = BlockHash::from([1u8; 32]);
    let list = MnList::new(hash, 1_028_160, 5);
    assert_eq!(list.provenance(), Provenance::Decoded);
    assert_eq!((list.height(), list.total_registered()), (1_028_160, 5));
    // Provenance is not content.
    let other = MnList {
        provenance: Provenance::FromGenesis,
        ..list.clone()
    };
    assert!(list.same_record(&other));
    assert!(!list.same_record(&MnList::new(hash, 1_028_160, 6)));
}

/// A list changed through the public mutators (`try_add`, `update_hot`, `remove`) is `Decoded`
/// (EV2 rev4): two masternodes paid at its height break P1, which only a list built block by
/// block proves.
#[test]
fn the_public_mutators_make_a_list_decoded() {
    use crate::MnType;
    use crate::testkit::dmn;

    let mut list = MnList::default_pre_dip3().with_height(1_000_100);
    let (a, b) = (dmn(1, MnType::Regular), dmn(2, MnType::Regular));
    list.try_add(Arc::clone(&a), true).expect("addable");
    assert_eq!(list.provenance(), Provenance::Decoded);
    let mut list = MnList {
        provenance: Provenance::FromGenesis,
        ..list
    };
    list.try_add(Arc::clone(&b), true).expect("addable");
    let paid = MnHot {
        last_paid_height: list.height(),
        ..a.hot
    };
    list.update_hot(&a.pro_tx_hash, paid).expect("present");
    list.update_hot(&b.pro_tx_hash, paid).expect("present");
    assert_eq!(list.provenance(), Provenance::Decoded);
    assert_eq!(list.check_invariants(), Ok(()));
    let mut list = MnList {
        provenance: Provenance::FromGenesis,
        ..list
    };
    list.remove(&b.pro_tx_hash).expect("present");
    assert_eq!(list.provenance(), Provenance::Decoded);
}

#[test]
fn lookups_on_an_absent_masternode_follow_core() {
    let list = MnList::default_pre_dip3();
    let pro_tx = ProTxHash::from([3u8; 32]);
    assert!(list.get(&pro_tx).is_none());
    assert!(!list.is_valid(&pro_tx));
    assert!(!list.is_pose_banned(&pro_tx));
    assert!(list.get_by_internal_id(0).is_none());
    let op = OutPoint {
        hash: TxHash::from([4u8; 32]),
        index: 1,
    };
    assert!(list.get_by_collateral(&op).is_none());
    assert_eq!(list.iter().count(), 0);
}

#[test]
fn lookups_on_valid_and_banned_masternodes_follow_core() {
    use crate::MnType;
    use crate::testkit::dmn;

    let mut list = MnList::default_pre_dip3();
    let valid = dmn(1, MnType::Regular);
    let mut banned = (*dmn(2, MnType::Evo)).clone();
    banned.hot.ban_if_not_banned(50);
    let banned = Arc::new(banned);
    list.insert_for_tests(Arc::clone(&valid));
    list.insert_for_tests(Arc::clone(&banned));
    assert_eq!(list.provenance(), Provenance::Decoded);

    assert!(list.is_valid(&valid.pro_tx_hash) && !list.is_pose_banned(&valid.pro_tx_hash));
    assert!(!list.is_valid(&banned.pro_tx_hash) && list.is_pose_banned(&banned.pro_tx_hash));
    assert!(list.get(&banned.pro_tx_hash).is_some());
    assert!(list.get_valid(&banned.pro_tx_hash).is_none());
    assert_eq!(list.get_by_internal_id(2).map(|d| d.pro_tx_hash), Some(banned.pro_tx_hash));

    assert!(list.get_by_collateral(&banned.collateral).is_some());
    assert!(list.get_valid_by_collateral(&banned.collateral).is_none());
    assert!(list.get_valid_by_collateral(&valid.collateral).is_some());
    let owner = UniqueKey::key_id(&valid.cold.key_id_owner);
    assert!(list.has_unique(&owner));
    assert_eq!(list.unique_owner(&owner).map(|d| d.pro_tx_hash), Some(valid.pro_tx_hash));

    let counts = list.counts();
    assert_eq!((counts.total(), counts.enabled(), counts.total_weighted), (2, 1, 5));
    let order: Vec<_> = list.iter().map(|d| d.pro_tx_hash).collect();
    assert_eq!(order, [valid.pro_tx_hash, banned.pro_tx_hash]);
    assert_eq!(list.iter_valid().count(), 1);
}

mod mutations {
    use super::super::*;
    use crate::MnType;
    use crate::OperatorKey;
    use crate::testkit::{dmn, dmn_with, operator_key, pro_tx};
    use dash_pkc::bls::BlsSchemeKind;
    use dash_primitives::{
        AddrV1, AddrV2, NIEntry, NIPurpose, NetInfo, NetInfoV1, NetInfoV2, ServiceV1, TxHash,
    };

    fn legacy(ip: [u8; 4], port: u16) -> Arc<NetInfo> {
        let mut mapped = [0u8; 16];
        mapped[10..12].copy_from_slice(&[0xff, 0xff]);
        mapped[12..].copy_from_slice(&ip);
        Arc::new(NetInfo::Legacy(NetInfoV1(ServiceV1 {
            addr: AddrV1(mapped),
            port,
        })))
    }

    fn extended(entries: Vec<NIEntry>) -> Arc<NetInfo> {
        Arc::new(NetInfo::Extended(NetInfoV2 {
            version: 1,
            entries: vec![(NIPurpose::CoreP2p, entries)],
        }))
    }

    fn service(addr: AddrV2, port: u16) -> NIEntry {
        NIEntry::Service(ServiceV2 {
            addr,
            port,
        })
    }

    /// Every mutation leaves a list whose invariants hold.
    fn checked(list: &MnList) -> &MnList {
        list.check_invariants().expect("invariants hold");
        list
    }

    /// `f` fails with `err` and leaves `list` exactly as it was (all or nothing).
    fn fails(
        list: &mut MnList,
        err: MnListError,
        f: impl FnOnce(&mut MnList) -> Result<(), MnListError>,
    ) {
        let before = list.clone();
        assert_eq!(f(list), Err(err));
        assert!(list.same_record(&before));
        assert_eq!(list.unique, before.unique);
        assert_eq!(list.indexes, before.indexes);
        checked(list);
    }

    fn list_of(mns: impl IntoIterator<Item = Arc<Dmn>>) -> MnList {
        let mut list = MnList::default_pre_dip3();
        for dmn in mns {
            list.try_add(dmn, true).expect("addable");
        }
        checked(&list);
        list
    }

    /// `AddMN`'s checks fire in Core's order: with every property duplicated, each failure shows
    /// once the earlier ones are cleared.
    #[test]
    fn try_add_fails_in_cores_order() {
        let op = operator_key(1, BlsSchemeKind::Basic);
        let first = dmn_with(1, MnType::Evo, |_, cold| {
            cold.version = 2;
            cold.pub_key_operator = op.clone();
            cold.net_info = legacy([1, 2, 3, 4], 9999);
        });
        let mut list = list_of([Arc::clone(&first)]);
        let mut cand = (*first).clone();
        let add = |cand: &Dmn| {
            let cand = Arc::new(cand.clone());
            move |l: &mut MnList| l.try_add(cand, true)
        };
        fails(&mut list, MnListError::DuplicateProTxHash(first.pro_tx_hash), add(&cand));
        cand.pro_tx_hash = pro_tx(2);
        fails(&mut list, MnListError::DuplicateInternalId(1), add(&cand));
        cand.internal_id = 2;
        fails(
            &mut list,
            MnListError::DuplicateCollateral {
                pro_tx: pro_tx(2),
                collateral: first.collateral,
            },
            add(&cand),
        );
        cand.collateral.index = 1;
        fails(
            &mut list,
            MnListError::DuplicateAddress {
                pro_tx: pro_tx(2),
                address: "1.2.3.4:9999".into(),
            },
            add(&cand),
        );
        let mut cold = (*cand.cold).clone();
        cold.net_info = legacy([1, 2, 3, 5], 9999);
        cold.derived = Default::default();
        cand.cold = Arc::new(cold.clone());
        fails(&mut list, MnListError::DuplicateOwner(pro_tx(2)), add(&cand));
        cold.key_id_owner = [2; 20].into();
        cold.derived = Default::default();
        cand.cold = Arc::new(cold.clone());
        fails(&mut list, MnListError::DuplicateOperator(pro_tx(2)), add(&cand));
        cold.pub_key_operator = operator_key(2, BlsSchemeKind::Basic);
        cold.derived = Default::default();
        cand.cold = Arc::new(cold.clone());
        fails(&mut list, MnListError::DuplicatePlatformNodeId(pro_tx(2)), add(&cand));
        cold.platform_node_id = [0x82; 20].into();
        cold.derived = Default::default();
        cand.cold = Arc::new(cold);
        list.try_add(Arc::new(cand), true).expect("every property is free");
        checked(&list);
        assert_eq!((list.len(), list.total_registered()), (2, 3));
        assert_eq!(list.unique.len(), 10);
    }

    /// Null values (EV2 §2.2): a null collateral, owner key or service entry is refused like a
    /// duplicate; a default operator key and a null platformNodeID are skipped; a regular
    /// masternode never holds its platformNodeID.
    #[test]
    fn null_values_follow_cores_rules() {
        let mut list = MnList::default_pre_dip3();
        let null_collateral = dmn_with(1, MnType::Regular, |dmn, _| {
            dmn.collateral = OutPoint {
                hash: TxHash::from([0; 32]),
                index: u32::MAX,
            };
        });
        fails(
            &mut list,
            MnListError::DuplicateCollateral {
                pro_tx: pro_tx(1),
                collateral: null_collateral.collateral,
            },
            |l| l.try_add(null_collateral, true),
        );
        let null_owner = dmn_with(1, MnType::Regular, |_, cold| cold.key_id_owner = [0; 20].into());
        fails(&mut list, MnListError::DuplicateOwner(pro_tx(1)), |l| l.try_add(null_owner, true));
        let null_service = dmn_with(1, MnType::Regular, |_, cold| {
            cold.version = 3;
            cold.net_info = extended(vec![service(AddrV2::Ipv6([0; 16]), 0)]);
        });
        fails(
            &mut list,
            MnListError::DuplicateAddress {
                pro_tx: pro_tx(1),
                address: "[::]:0".into(),
            },
            |l| l.try_add(null_service, true),
        );
        let invalid = dmn_with(1, MnType::Regular, |_, cold| {
            cold.version = 3;
            cold.net_info = extended(vec![NIEntry::Invalid]);
        });
        fails(&mut list, MnListError::InvalidAddress(pro_tx(1)), |l| l.try_add(invalid, true));

        // Default operator keys (either scheme) and null platformNodeIDs: never held.
        let a = dmn_with(1, MnType::Evo, |_, cold| cold.platform_node_id = [0; 20].into());
        let b = dmn_with(2, MnType::Evo, |_, cold| {
            cold.platform_node_id = [0; 20].into();
            cold.pub_key_operator = OperatorKey::new(BlsSchemeKind::Basic);
        });
        // A regular masternode's platformNodeID is not a property: an owner key with its bytes
        // is free.
        let c = dmn_with(3, MnType::Regular, |_, cold| cold.platform_node_id = [9; 20].into());
        let d = dmn(9, MnType::Regular);
        let list = list_of([a, b, c, d]);
        assert_eq!(list.unique.len(), 4 * 2);
    }

    pub(super) fn owner_key_and_platform_node_id_collide_in_both_orders() {
        let evo = dmn_with(1, MnType::Evo, |_, cold| cold.platform_node_id = [7; 20].into());
        let regular = dmn(7, MnType::Regular);
        let mut list = list_of([Arc::clone(&evo)]);
        fails(&mut list, MnListError::DuplicateOwner(pro_tx(7)), |l| {
            l.try_add(Arc::clone(&regular), true)
        });
        let mut list = list_of([regular]);
        fails(&mut list, MnListError::DuplicatePlatformNodeId(pro_tx(1)), |l| l.try_add(evo, true));
    }

    pub(super) fn addrv1_and_addrv2_forms_share_a_slot() {
        let legacy_mn =
            dmn_with(1, MnType::Regular, |_, cold| cold.net_info = legacy([1, 2, 3, 4], 9999));
        let extended_mn = dmn_with(2, MnType::Regular, |_, cold| {
            cold.version = 3;
            cold.net_info = extended(vec![service(AddrV2::Ipv4([1, 2, 3, 4]), 9999)]);
        });
        let mut list = list_of([legacy_mn]);
        fails(
            &mut list,
            MnListError::DuplicateAddress {
                pro_tx: pro_tx(2),
                address: "1.2.3.4:9999".into(),
            },
            |l| l.try_add(extended_mn, true),
        );
        let found = list.get_by_service(&ServiceV2 {
            addr: AddrV2::Ipv4([1, 2, 3, 4]),
            port: 9999,
        });
        assert_eq!(found.map(|d| d.pro_tx_hash), Some(pro_tx(1)));
    }

    /// A CJDNS service serializes to 20 bytes like a key id, a TORv3 one to 36 like an outpoint.
    pub(super) fn length_classes_collide_across_types() {
        // 06 10 ‖ ip[16] ‖ port: the owner key id of masternode 2.
        let mut ip = [0xfc; 16];
        ip[15] = 0x01;
        let mut id = [0u8; 20];
        id[..2].copy_from_slice(&[6, 16]);
        id[2..18].copy_from_slice(&ip);
        id[18..].copy_from_slice(&9999u16.to_be_bytes());
        let cjdns = dmn_with(1, MnType::Regular, |_, cold| {
            cold.version = 3;
            cold.net_info = extended(vec![service(AddrV2::Cjdns(ip), 9999)]);
        });
        let owner = dmn_with(2, MnType::Regular, |_, cold| cold.key_id_owner = id.into());
        let mut list = list_of([cjdns]);
        fails(&mut list, MnListError::DuplicateOwner(pro_tx(2)), |l| l.try_add(owner, true));

        // 04 20 ‖ addr[32] ‖ port: the collateral outpoint of masternode 2.
        let onion = [0x5a; 32];
        let mut txid = [0u8; 32];
        txid[..2].copy_from_slice(&[4, 32]);
        txid[2..].copy_from_slice(&onion[..30]);
        let mut n = [0u8; 4];
        n[..2].copy_from_slice(&onion[30..]);
        n[2..].copy_from_slice(&9999u16.to_be_bytes());
        let tor = dmn_with(1, MnType::Regular, |_, cold| {
            cold.version = 3;
            cold.net_info = extended(vec![service(AddrV2::TorV3(onion), 9999)]);
        });
        let collateral = OutPoint {
            hash: TxHash::from(txid),
            index: u32::from_le_bytes(n),
        };
        let spender = dmn_with(2, MnType::Regular, |dmn, _| dmn.collateral = collateral);
        let mut list = list_of([tor]);
        fails(
            &mut list,
            MnListError::DuplicateCollateral {
                pro_tx: pro_tx(2),
                collateral,
            },
            |l| l.try_add(spender, true),
        );
        // GetMNByCollateral answers with the slot's owner, whatever property it is.
        assert_eq!(list.get_by_collateral(&collateral).map(|d| d.pro_tx_hash), Some(pro_tx(1)));
    }

    /// One point under its two encodings is two slots: `AddMN` accepts both holders (the
    /// pre-V24 state), and `HasOperatorKeyUnderAnyScheme`, which V24 paths call, sees the other.
    pub(super) fn one_point_under_two_encodings() {
        let legacy_key = operator_key(5, BlsSchemeKind::Legacy);
        let basic_key = operator_key(5, BlsSchemeKind::Basic);
        let point = basic_key.get();
        let a = dmn_with(1, MnType::Regular, |_, cold| cold.pub_key_operator = legacy_key);
        let b = dmn_with(2, MnType::Regular, |_, cold| {
            cold.version = 2;
            cold.pub_key_operator = basic_key;
        });
        let list = list_of([a, b]);
        assert!(list.has_operator_key_under_any_scheme(&point, &pro_tx(1)));
        assert!(list.has_operator_key_under_any_scheme(&point, &pro_tx(2)));
        assert!(list.has_operator_key_under_any_scheme(&point, &pro_tx(3)));
        let other = operator_key(6, BlsSchemeKind::Basic).get();
        assert!(!list.has_operator_key_under_any_scheme(&other, &pro_tx(3)));
        let single = list_of([dmn_with(1, MnType::Regular, |_, cold| {
            cold.pub_key_operator = operator_key(5, BlsSchemeKind::Legacy);
        })]);
        assert!(!single.has_operator_key_under_any_scheme(&point, &pro_tx(1)));
        assert!(single.has_operator_key_under_any_scheme(&point, &pro_tx(2)));
        // GetMNByOperatorKey matches by point: the first holder in Core's order.
        let first = list.core_order()[0];
        assert_eq!(list.get_by_operator_key(&point).map(|d| d.pro_tx_hash), Some(first));
    }

    /// A shared masternode holds every share's owner key; `keyIDOwner` stays out of the map.
    #[test]
    fn a_shared_masternode_holds_its_share_owners() {
        use dash_primitives::CollateralShare;
        let share = |b: u8| CollateralShare {
            amount: 1,
            script_refund: Default::default(),
            script_reward: Default::default(),
            key_id_owner: [b; 20].into(),
        };
        let shared = dmn_with(1, MnType::Regular, |_, cold| {
            cold.version = 3;
            cold.net_info = extended(Vec::new());
            cold.shares = vec![share(0x31), share(0x32)];
        });
        let mut list = list_of([shared]);
        assert!(list.has_unique(&UniqueKey::key_id(&[0x32; 20].into())));
        assert!(!list.has_unique(&UniqueKey::key_id(&[1; 20].into())));
        let clash = dmn_with(2, MnType::Regular, |_, cold| {
            cold.version = 3;
            cold.net_info = extended(Vec::new());
            cold.shares = vec![share(0x33), share(0x31)];
        });
        fails(&mut list, MnListError::DuplicateShareOwner(pro_tx(2)), |l| l.try_add(clash, true));
        list.remove(&pro_tx(1)).expect("removable");
        assert!(checked(&list).unique.is_empty());
    }

    #[test]
    fn the_counter_follows_add_mn() {
        let mut list = MnList::default_pre_dip3();
        list.try_add(dmn(5, MnType::Regular), true).expect("addable");
        assert_eq!(list.total_registered(), 6);
        list.try_add(dmn(2, MnType::Regular), true).expect("addable");
        assert_eq!(list.total_registered(), 6, "max(id + 1, counter)");
        list.try_add(dmn(9, MnType::Regular), false).expect("addable");
        assert_eq!(list.total_registered(), 6, "no bump");
        list.remove(&pro_tx(5)).expect("removable");
        assert_eq!(list.total_registered(), 6, "never decremented");
        checked(&list);
    }

    #[test]
    fn update_moves_slots_and_rolls_back() {
        let a = dmn_with(1, MnType::Evo, |_, cold| {
            cold.net_info = legacy([1, 1, 1, 1], 1);
            cold.pub_key_operator = operator_key(1, BlsSchemeKind::Legacy);
        });
        let b = dmn_with(2, MnType::Evo, |_, cold| cold.net_info = legacy([2, 2, 2, 2], 2));
        let mut list = list_of([Arc::clone(&a), b]);
        let state = |f: &dyn Fn(&mut MnCold)| {
            let mut cold = (*a.cold).clone();
            f(&mut cold);
            cold.derived = Default::default();
            Arc::new(cold)
        };
        let hot = a.hot;

        // Taken slots: nothing changes.
        fails(
            &mut list,
            MnListError::UpdateAddresses {
                pro_tx: pro_tx(1),
                reason: "duplicate (2.2.2.2:2)".into(),
            },
            |l| l.update(&pro_tx(1), hot, state(&|c| c.net_info = legacy([2, 2, 2, 2], 2))),
        );
        fails(&mut list, MnListError::UpdateOwner(pro_tx(1)), |l| {
            l.update(&pro_tx(1), hot, state(&|c| c.key_id_owner = [2; 20].into()))
        });
        fails(&mut list, MnListError::UpdatePlatformNodeId(pro_tx(1)), |l| {
            l.update(&pro_tx(1), hot, state(&|c| c.platform_node_id = [0x82; 20].into()))
        });
        fails(&mut list, MnListError::NotFound("UpdateMN", pro_tx(3)), |l| {
            l.update(&pro_tx(3), hot, state(&|_| {}))
        });

        // Free slots: the old ones are released.
        list.update(
            &pro_tx(1),
            hot,
            state(&|c| {
                c.net_info = legacy([3, 3, 3, 3], 3);
                c.key_id_owner = [3; 20].into();
                c.platform_node_id = [0x83; 20].into();
                c.pub_key_operator = operator_key(3, BlsSchemeKind::Legacy);
            }),
        )
        .expect("free slots");
        checked(&list);
        for gone in [
            UniqueKey::key_id(&[1; 20].into()),
            UniqueKey::platform_node_id(&[0x81; 20].into()),
            UniqueKey::operator_key(&operator_key(1, BlsSchemeKind::Legacy)),
        ] {
            assert!(!list.has_unique(&gone));
        }
        // The old address is free again: masternode 2 may take it.
        let b2 = Arc::clone(list.get(&pro_tx(2)).expect("present"));
        let mut cold = (*b2.cold).clone();
        cold.net_info = legacy([1, 1, 1, 1], 1);
        cold.derived = Default::default();
        list.update(&pro_tx(2), b2.hot, Arc::new(cold)).expect("the old address is free");
        checked(&list);
    }

    /// An operator key whose serialization stays keeps its slot, whatever the object (Core
    /// v24's `UpdateUniqueProperty` on hashes); a re-encoding moves it.
    #[test]
    fn operator_slots_move_with_the_serialization() {
        let legacy_key = operator_key(4, BlsSchemeKind::Legacy);
        let a = dmn_with(1, MnType::Regular, |_, cold| cold.pub_key_operator = legacy_key.clone());
        let mut list = list_of([Arc::clone(&a)]);
        // The same bytes, read back undecoded.
        let raw = OperatorKey::from_bytes(
            &legacy_key.to_bytes(),
            dash_pkc::bls::BlsSchemeSource::Version(BlsSchemeKind::Legacy),
        );
        let mut cold = (*a.cold).clone();
        cold.pub_key_operator = raw;
        cold.derived = Default::default();
        let before = list.unique.clone();
        list.update(&pro_tx(1), a.hot, Arc::new(cold.clone())).expect("same bytes");
        assert_eq!(list.unique, before);
        // The basic encoding of the point: a new slot.
        cold.version = 2;
        cold.pub_key_operator = operator_key(4, BlsSchemeKind::Basic);
        cold.derived = Default::default();
        list.update(&pro_tx(1), a.hot, Arc::new(cold)).expect("re-encoded");
        checked(&list);
        assert!(!list.has_unique(&UniqueKey::operator_key(&legacy_key)));
        assert!(list.has_unique(&UniqueKey::operator_key(&operator_key(4, BlsSchemeKind::Basic))));
    }

    #[test]
    fn remove_releases_every_slot() {
        let a = dmn_with(1, MnType::Evo, |_, cold| {
            cold.net_info = legacy([1, 1, 1, 1], 1);
            cold.pub_key_operator = operator_key(1, BlsSchemeKind::Legacy);
        });
        let mut list = list_of([a, dmn(2, MnType::Regular)]);
        fails(&mut list, MnListError::NotFound("RemoveMN", pro_tx(3)), |l| l.remove(&pro_tx(3)));
        list.remove(&pro_tx(1)).expect("present");
        checked(&list);
        assert_eq!(list.unique.len(), 2);
        assert!(list.get_by_internal_id(1).is_none());
        assert_eq!(list.counts().total(), 1);
    }

    /// Removing a masternode whose slot another masternode owns fails with Core's text and
    /// changes nothing (reachable only through decoded lists).
    #[test]
    fn remove_of_a_foreign_slot_fails() {
        let mut list = list_of([dmn(1, MnType::Regular)]);
        // Masternode 1's owner slot handed to masternode 9 behind the list's back.
        let owner = UniqueKey::key_id(&[1; 20].into());
        let prop = crate::unique::Prop {
            key: owner,
            null: false,
        };
        assert!(list.unique.delete(&prop, &pro_tx(1)));
        assert!(list.unique.add(&prop, &pro_tx(9)));
        assert_eq!(list.check_invariants(), Err(InvariantViolation("I-U")));
        let before = list.clone();
        assert_eq!(
            list.remove(&pro_tx(1)),
            Err(MnListError::Delete {
                pro_tx: pro_tx(1),
                what: "keyIDOwner",
            })
        );
        assert_eq!(list.unique, before.unique);
        assert!(list.same_record(&before));
    }

    #[test]
    fn indexes_follow_the_masternodes() {
        let mut banned = (*dmn(2, MnType::Regular)).clone();
        banned.hot.ban_if_not_banned(50);
        let mut list = list_of([dmn(1, MnType::Evo), Arc::new(banned), dmn(3, MnType::Regular)]);
        assert_eq!(list.payee_queue().count(), 2);
        assert_eq!(list.unconfirmed().count(), 3);
        let a = Arc::clone(list.get(&pro_tx(1)).expect("present"));
        let mut hot = a.hot;
        hot.pose_penalty = 10;
        hot.consecutive_payments = 2;
        hot.last_paid_height = 300;
        list.update_hot(&pro_tx(1), hot).expect("present");
        checked(&list);
        assert_eq!(list.decaying().copied().collect::<Vec<_>>(), [pro_tx(1)]);
        assert_eq!(list.evo_streaks().copied().collect::<Vec<_>>(), [pro_tx(1)]);
        let last = list.payee_queue().next_back().map(|k| k.pro_tx);
        assert_eq!(last, Some(pro_tx(1)));
        assert_eq!(
            list.update_hot(&pro_tx(4), MnHot::default()),
            Err(MnListError::NotFound("UpdateMN", pro_tx(4)))
        );
    }

    /// Collision buckets (EV2 §2.7): a set moves the key to the front, an erase keeps the rest's
    /// order, a single survivor leaves the bucket.
    #[test]
    fn collision_buckets_follow_immer() {
        let colliding = |n: u8| {
            dmn_with(n, MnType::Regular, |dmn, _| {
                let mut bytes = [7u8; 32];
                bytes[31] = n;
                dmn.pro_tx_hash = ProTxHash::from(bytes);
            })
        };
        let key = |n: u8| colliding(n).pro_tx_hash;
        let hash = key(1).immer_hash();
        let bucket = |list: &MnList| {
            list.collision_bucket(hash).map(|b| b.iter().copied().collect::<Vec<_>>())
        };
        let mut list = list_of([colliding(1), dmn(9, MnType::Regular)]);
        assert_eq!(bucket(&list), None);
        list.try_add(colliding(2), true).expect("addable");
        assert_eq!(bucket(checked(&list)), Some(vec![key(2), key(1)]));
        list.try_add(colliding(3), true).expect("addable");
        assert_eq!(bucket(checked(&list)), Some(vec![key(3), key(2), key(1)]));
        // An update is a set: the key moves to the front.
        let one = Arc::clone(list.get(&key(1)).expect("present"));
        list.update_hot(&key(1), one.hot).expect("present");
        assert_eq!(bucket(checked(&list)), Some(vec![key(1), key(3), key(2)]));
        let order = list.core_order();
        let tail: Vec<_> = order.iter().filter(|k| k.immer_hash() == hash).copied().collect();
        assert_eq!(tail, [key(1), key(3), key(2)]);
        // A failed mutation changes nothing, the bucket included.
        fails(&mut list, MnListError::DuplicateProTxHash(key(2)), |l| {
            l.try_add(colliding(2), true)
        });
        list.remove(&key(3)).expect("present");
        assert_eq!(bucket(checked(&list)), Some(vec![key(1), key(2)]));
        list.remove(&key(1)).expect("present");
        assert_eq!(bucket(checked(&list)), None);
        assert_eq!(list.collision_buckets().count(), 0);
    }

    #[test]
    fn errors_render_cores_text() {
        let e = MnListError::DuplicateAddress {
            pro_tx: pro_tx(1),
            address: "1.2.3.4:9999".into(),
        };
        assert_eq!(
            e.to_string(),
            format!(
                "AddMN: Can't add a masternode {} with a duplicate address=1.2.3.4:9999",
                pro_tx(1)
            )
        );
        assert_eq!(
            MnListError::Delete {
                pro_tx: pro_tx(2),
                what: "collateralOutpoint",
            }
            .to_string(),
            format!("RemoveMN: Can't delete a masternode {} with a collateralOutpoint", pro_tx(2))
        );
    }
}

/// LQ1 §2.2.2's view contract for `MnList` (EV2a acceptance).
mod view_contract {
    use super::super::*;
    use crate::MnType;
    use crate::testkit::{dmn, dmn_with, pro_tx};
    use dash_evo::llmq::{MnListView, ScratchMnList};
    use std::collections::BTreeSet;

    fn list() -> MnList {
        let mut banned = (*dmn(2, MnType::Evo)).clone();
        banned.hot.ban_if_not_banned(50);
        let mut list = MnList::default_pre_dip3();
        for dmn in [dmn(1, MnType::Regular), Arc::new(banned), dmn(3, MnType::Evo)] {
            list.try_add(dmn, true).expect("addable");
        }
        list
    }

    #[test]
    fn counts_contains_and_bans() {
        let list = list();
        let counts = MnListView::counts(&list);
        assert_eq!((counts.total, counts.enabled), (3, 2));
        assert!(MnListView::contains(&list, &pro_tx(2)));
        assert!(!MnListView::contains(&list, &pro_tx(4)));
        assert!(MnListView::is_pose_banned(&list, &pro_tx(2)));
        assert!(!MnListView::is_pose_banned(&list, &pro_tx(1)));
        assert!(!MnListView::is_pose_banned(&list, &pro_tx(4)), "false when absent");
    }

    /// `for_each` visits each masternode once; its order is unspecified, so compare sets.
    #[test]
    fn for_each_visits_the_valid_or_all_masternodes() {
        let list = list();
        let visit = |only_valid| {
            let mut seen = Vec::new();
            MnListView::for_each(&list, only_valid, &mut |e| seen.push(e.pro_tx_hash));
            let set: BTreeSet<_> = seen.iter().copied().collect();
            assert_eq!(set.len(), seen.len());
            set
        };
        assert_eq!(visit(true), BTreeSet::from([pro_tx(1), pro_tx(3)]));
        assert_eq!(visit(false), BTreeSet::from([pro_tx(1), pro_tx(2), pro_tx(3)]));
    }

    /// The scratch list is `CDeterministicMNList()` + `AddMN(dmn, true)`: rejections are
    /// `MnList::try_add`'s, all or nothing, and entries keep their own state.
    #[test]
    fn the_scratch_list_is_add_mn() {
        let list = list();
        let mut scratch = MnList::scratch();
        assert_eq!(ScratchMnList::total(&scratch), 0);
        MnListView::for_each(&list, false, &mut |e| {
            ScratchMnList::try_add(&mut scratch, e).expect("addable")
        });
        assert_eq!(ScratchMnList::total(&scratch), 3);
        let mut valid = 0;
        ScratchMnList::for_each(&scratch, true, &mut |_| valid += 1);
        assert_eq!(valid, 2);
        let again = Arc::clone(list.get(&pro_tx(1)).expect("present"));
        assert_eq!(
            ScratchMnList::try_add(&mut scratch, &again),
            Err(dash_evo::llmq::AddMnRejected)
        );
        let owner_clash =
            dmn_with(4, MnType::Regular, |_, cold| cold.key_id_owner = [3; 20].into());
        assert!(ScratchMnList::try_add(&mut scratch, &owner_clash).is_err());
        assert!(!ScratchMnList::contains(&scratch, &pro_tx(4)));
        assert_eq!(scratch.total_registered(), 4);
        scratch.check_invariants().expect("invariants hold");
    }
}

/// Quirk 23 (`docs/node-program/quirks.toml`): the unique-property map is one map for every
/// property type, keyed by the value's serialization. An owner keyID and a platformNodeID
/// with the same bytes collide in either order, and so do a service's ADDRv1 and ADDRv2 forms
/// and values of other types with equal length. One operator-key point under its two
/// encodings takes two slots.
#[test]
fn quirk_23_unique_property_map() {
    mutations::owner_key_and_platform_node_id_collide_in_both_orders();
    mutations::addrv1_and_addrv2_forms_share_a_slot();
    mutations::length_classes_collide_across_types();
    mutations::one_point_under_two_encodings();
}

/// EV2a fix r1 (R1-M2): a state's version decides which fields Core serializes, and every
/// Core write site leaves the others at their defaults. The list refuses a state outside that
/// domain at `try_add` and `update`, so its own records always rebuild it.
mod state_domain {
    use super::*;
    use crate::MnType;
    use crate::testkit::{dmn_with, operator_key, pro_tx};
    use dash_pkc::bls::BlsSchemeKind;
    use dash_primitives::{NetInfo, NetInfoV2, PayoutShare};
    use dash_types::codec::BaseCodec;

    fn hidden(version: i32, field: &'static str) -> MnListError {
        MnListError::VersionHiddenField {
            pro_tx: pro_tx(1),
            version,
            field,
        }
    }

    /// `OP_TRUE`, reward 10000.
    fn payout() -> PayoutShare {
        PayoutShare::decode(&mut &[0x01, 0x51, 0x10, 0x27][..]).expect("a payout share")
    }

    fn extended(cold: &mut MnCold) {
        cold.version = crate::types::EXT_ADDR;
        cold.net_info = Arc::new(NetInfo::Extended(NetInfoV2 {
            version: 1,
            entries: Vec::new(),
        }));
    }

    fn refused_at_add(
        mn_type: MnType,
        version: i32,
        field: &'static str,
        f: impl FnOnce(&mut MnCold),
    ) {
        let mut list = MnList::new(BlockHash::from([1; 32]), 1000, 2);
        let dmn = dmn_with(1, mn_type, |_, cold| f(cold));
        assert_eq!(list.try_add(dmn, false), Err(hidden(version, field)));
        assert!(list.is_empty());
        list.check_invariants().expect("invariants hold");
    }

    /// Sol r1's own-record rebuild sequence starts from a version-3 state with a
    /// `scriptPayout`, which no Core write site produces: it is now refused before it can be
    /// listed.
    #[test]
    fn sol_r1_rebuild_sequence_is_refused() {
        refused_at_add(MnType::Regular, 3, "scriptPayout", |cold| {
            extended(cold);
            cold.script_payout = vec![0x51];
        });
    }

    #[test]
    fn every_hidden_field_is_refused_at_add() {
        refused_at_add(MnType::Evo, 3, "platformP2PPort", |cold| {
            extended(cold);
            cold.platform_p2p_port = 26656;
        });
        refused_at_add(MnType::Evo, 3, "platformHTTPPort", |cold| {
            extended(cold);
            cold.platform_http_port = 443;
        });
        refused_at_add(MnType::Regular, 3, "netInfo", |cold| cold.version = 3);
        refused_at_add(MnType::Regular, 2, "netInfo", |cold| {
            extended(cold);
            cold.version = 2;
        });
        refused_at_add(MnType::Regular, 2, "payouts", |cold| {
            cold.version = 2;
            cold.payouts = vec![payout()];
        });
        refused_at_add(MnType::Regular, 1, "nEarlyPeriodBlocks", |cold| {
            cold.early_period_blocks = 1;
        });
        refused_at_add(MnType::Regular, 1, "nEarlyPenalty", |cold| cold.early_penalty = 1);
    }

    /// `ResetOperatorFields` sets version 1 and keeps the payouts; Core always restores the
    /// version (`SetStateVersion`) before the state is installed. An update that skips that
    /// step is refused and leaves the list as it was.
    #[test]
    fn a_reset_without_set_state_version_is_refused_at_update() {
        let dmn = dmn_with(1, MnType::Regular, |_, cold| {
            extended(cold);
            cold.payouts = vec![payout()];
        });
        let mut list = MnList::new(BlockHash::from([1; 32]), 1000, 2);
        list.try_add(Arc::clone(&dmn), false).expect("in Core's state domain");
        let before = list.clone();

        let mut cold = (*dmn.cold).clone();
        cold.reset_operator_fields(BlsSchemeKind::Basic);
        cold.derived = Default::default();
        assert_eq!(
            list.update(&pro_tx(1), dmn.hot, Arc::new(cold.clone())),
            Err(hidden(1, "payouts"))
        );
        assert!(list.same_record(&before));
        list.check_invariants().expect("invariants hold");

        // Version 3 with the reset's legacy network info: still outside the domain.
        cold.version = crate::types::EXT_ADDR;
        assert_eq!(
            list.update(&pro_tx(1), dmn.hot, Arc::new(cold.clone())),
            Err(hidden(3, "netInfo"))
        );
        // With the version restored as `SetStateVersion` does, network info converted too, the
        // update is accepted.
        extended(&mut cold);
        list.update(&pro_tx(1), dmn.hot, Arc::new(cold)).expect("in Core's state domain");
        list.check_invariants().expect("invariants hold");
    }

    /// D17: a non-default operator key captured under the other scheme from its version's is
    /// refused at add and at update (the version change without `SetStateVersion`'s re-capture);
    /// the default key, which `ResetOperatorFields` leaves under the global flag, is not.
    #[test]
    fn an_operator_key_under_the_other_scheme_is_refused() {
        refused_at_add(MnType::Regular, 1, "pubKeyOperator", |cold| {
            cold.pub_key_operator = operator_key(5, BlsSchemeKind::Basic);
        });
        refused_at_add(MnType::Regular, 2, "pubKeyOperator", |cold| {
            cold.version = 2;
            cold.pub_key_operator = operator_key(5, BlsSchemeKind::Legacy);
        });
        let mut list = MnList::new(BlockHash::from([1; 32]), 1000, 2);
        for (n, version, scheme) in [(2, 1, BlsSchemeKind::Basic), (3, 2, BlsSchemeKind::Legacy)] {
            let dmn = dmn_with(n, MnType::Regular, |_, cold| {
                cold.version = version;
                cold.pub_key_operator = crate::OperatorKey::new(scheme);
            });
            list.try_add(dmn, false).expect("a default key under either scheme");
        }

        let dmn = dmn_with(1, MnType::Regular, |_, cold| {
            cold.pub_key_operator = operator_key(5, BlsSchemeKind::Legacy);
        });
        list.try_add(Arc::clone(&dmn), false).expect("in Core's state domain");
        let before = list.clone();
        let mut cold = (*dmn.cold).clone();
        cold.version = 2;
        cold.derived = Default::default();
        assert_eq!(
            list.update(&pro_tx(1), dmn.hot, Arc::new(cold.clone())),
            Err(hidden(2, "pubKeyOperator"))
        );
        assert!(list.same_record(&before));
        cold.pub_key_operator = operator_key(5, BlsSchemeKind::Basic);
        list.update(&pro_tx(1), dmn.hot, Arc::new(cold)).expect("re-captured");
        list.check_invariants().expect("invariants hold");
    }

    /// I-S: `check_invariants` reports a state outside the domain however it got there.
    #[test]
    fn check_invariants_asserts_the_state_domain() {
        let mut list = MnList::new(BlockHash::from([1; 32]), 1000, 2);
        list.try_add(dmn_with(1, MnType::Regular, |_, _| {}), false).expect("addable");
        let mut dmn = (*list.mns.get(&pro_tx(1)).expect("listed").0).clone();
        let mut cold = (*dmn.cold).clone();
        cold.early_penalty = 1;
        dmn.cold = Arc::new(cold);
        list.mns.insert(pro_tx(1), Shared(Arc::new(dmn)));
        assert_eq!(list.check_invariants(), Err(InvariantViolation("I-S")));

        // D17: a legacy key left under version 2 (same captured bytes, so the unique map holds).
        let mut list = MnList::new(BlockHash::from([1; 32]), 1000, 2);
        let dmn = dmn_with(1, MnType::Regular, |_, cold| {
            cold.pub_key_operator = operator_key(5, BlsSchemeKind::Legacy);
        });
        list.try_add(Arc::clone(&dmn), false).expect("addable");
        let mut dmn = (*dmn).clone();
        let mut cold = (*dmn.cold).clone();
        cold.version = 2;
        dmn.cold = Arc::new(cold);
        list.mns.insert(pro_tx(1), Shared(Arc::new(dmn)));
        assert_eq!(list.check_invariants(), Err(InvariantViolation("I-S")));
    }
}
