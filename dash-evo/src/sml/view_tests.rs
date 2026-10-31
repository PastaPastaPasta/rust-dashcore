//! `SmlMember` and `SmlScratch` (SB8, Opus r1 F1/F2): each property Core's `AddMN` registers
//! that the SML carries (`evo/deterministicmns.cpp` v24:439-505), on synthetic entries.

use dash_consensus::primitives::BlockHash;
use dash_pkc::bls::{BlsEagerPk, BlsScIetf, BlsSecretKey};
use dash_primitives::{AddrV1, AddrV2, NIPurpose, NetInfoV2, PlatformNodeId, ServiceV1, ServiceV2};

use super::*;
use crate::types::RawMnType;

fn legacy(addr: [u8; 16], port: u16) -> NetInfo {
    NetInfo::Legacy(NetInfoV1(ServiceV1 {
        addr: AddrV1(addr),
        port,
    }))
}

/// `::ffff:a.b.c.d`, the legacy form of an IPv4 address.
fn v4_mapped(ip: [u8; 4]) -> [u8; 16] {
    let mut a = [0u8; 16];
    a[10] = 0xff;
    a[11] = 0xff;
    a[12..].copy_from_slice(&ip);
    a
}

fn extended(entries: Vec<(NIPurpose, Vec<NIEntry>)>) -> NetInfo {
    NetInfo::Extended(NetInfoV2 {
        version: 1,
        entries,
    })
}

fn service(ip: [u8; 4], port: u16) -> NIEntry {
    NIEntry::Service(ServiceV2 {
        addr: AddrV2::Ipv4(ip),
        port,
    })
}

fn domain(name: &str, port: u16) -> NIEntry {
    NIEntry::Domain {
        name: name.as_bytes().to_vec(),
        port,
    }
}

/// A valid regular masternode `n` with no network entry, operator key or platform id.
fn entry(n: u8) -> SmlEntry {
    SmlEntry {
        version: 2,
        pro_reg_tx_hash: ProTxHash::from([n; 32]),
        confirmed_hash: BlockHash::from_lendian([0; 32]),
        net_info: legacy([0; 16], 0),
        operator_key: [0; 48],
        voting_key_id: [0; 20],
        is_valid: true,
        mn_type: RawMnType::REGULAR,
        platform_http_port: 0,
        platform_node_id: PlatformNodeId::from_lendian([0; 20]),
    }
}

fn with(n: u8, edit: impl FnOnce(&mut SmlEntry)) -> SmlEntry {
    let mut e = entry(n);
    edit(&mut e);
    e
}

/// A valid regular masternode `n` with the network info `net_info`.
fn with_net(n: u8, net_info: NetInfo) -> SmlEntry {
    with(n, |e| e.net_info = net_info)
}

/// One scratch after adding each entry in turn, and whether each was accepted.
fn add_all<'a>(entries: impl IntoIterator<Item = &'a SmlEntry>) -> (SmlScratch, Vec<bool>) {
    let mut scratch = SmlScratch::default();
    let accepted =
        entries.into_iter().map(|e| scratch.try_add(&SmlMember::new(e)).is_ok()).collect();
    (scratch, accepted)
}

/// Whether each entry in turn is accepted by one scratch.
fn adds(entries: &[SmlEntry]) -> Vec<bool> {
    add_all(entries).1
}

#[test]
fn a_duplicate_pro_tx_hash_is_rejected() {
    let other = with_net(1, legacy(v4_mapped([1, 2, 3, 4]), 9999));
    assert_eq!(adds(&[entry(1), entry(1)]), [true, false]);
    assert_eq!(adds(&[entry(1), other]), [true, false]);
}

/// Core keeps a legacy service only when `CNetAddr::IsValid` (`evo/netinfo.h` v24:212-217).
#[test]
fn a_legacy_service_is_unique_only_when_core_keeps_it() {
    let kept = |addr| {
        legacy_service_kept(&NetInfoV1(ServiceV1 {
            addr: AddrV1(addr),
            port: 9999,
        }))
    };
    let mut rfc3849 = [0u8; 16];
    rfc3849[..4].copy_from_slice(&[0x20, 0x01, 0x0d, 0xb8]);
    rfc3849[15] = 1;
    let mut ipv6 = [0u8; 16];
    ipv6[..2].copy_from_slice(&[0x2a, 0x01]);
    ipv6[15] = 1;
    for (addr, valid) in [
        ([0u8; 16], false),
        (v4_mapped([0, 0, 0, 0]), false),
        (v4_mapped([255, 255, 255, 255]), false),
        (rfc3849, false),
        (v4_mapped([1, 2, 3, 4]), true),
        (ipv6, true),
    ] {
        assert_eq!(kept(addr), valid, "{addr:02x?}");
        let at = |n| with_net(n, legacy(addr, 9999));
        // A second holder is rejected only for an address Core registers.
        assert_eq!(adds(&[at(1), at(2)]), [true, !valid], "{addr:02x?}");
    }
    // Another port is another service.
    let a = with_net(1, legacy(v4_mapped([1, 2, 3, 4]), 9999));
    let b = with_net(2, legacy(v4_mapped([1, 2, 3, 4]), 9998));
    assert_eq!(adds(&[a, b]), [true, true]);
}

/// Core hashes a `CService` the same way whichever entry form holds it.
#[test]
fn a_service_is_one_value_in_either_entry_form() {
    let old = with_net(1, legacy(v4_mapped([1, 2, 3, 4]), 9999));
    let new = with_net(2, extended(vec![(NIPurpose::CoreP2p, vec![service([1, 2, 3, 4], 9999)])]));
    assert_eq!(adds(&[old.clone(), new.clone()]), [true, false]);
    assert_eq!(adds(&[new, old]), [true, false]);
}

#[test]
fn extended_services_and_domains_are_unique() {
    let net = |n, entries| with_net(n, extended(vec![(NIPurpose::CoreP2p, entries)]));
    let svc = |n, port| net(n, vec![service([5, 6, 7, 8], port)]);
    assert_eq!(adds(&[svc(1, 9999), svc(2, 9999)]), [true, false]);
    assert_eq!(adds(&[svc(1, 9999), svc(2, 9998)]), [true, true]);
    let dom = |n, name, port| net(n, vec![domain(name, port)]);
    assert_eq!(
        adds(&[dom(1, "mn.example.com", 443), dom(2, "mn.example.com", 443)]),
        [true, false]
    );
    assert_eq!(adds(&[dom(1, "mn.example.com", 443), dom(2, "mn.example.com", 444)]), [true, true]);
    assert_eq!(adds(&[dom(1, "mn.example.com", 443), dom(2, "mo.example.com", 443)]), [true, true]);
    // A group of another purpose holds the same value.
    let platform =
        with_net(2, extended(vec![(NIPurpose::PlatformP2p, vec![service([5, 6, 7, 8], 9999)])]));
    assert_eq!(adds(&[svc(1, 9999), platform]), [true, false]);
}

/// "Can't add a masternode … with invalid address".
#[test]
fn an_invalid_network_entry_is_rejected() {
    let invalid = with_net(1, extended(vec![(NIPurpose::CoreP2p, vec![NIEntry::Invalid])]));
    assert_eq!(adds(&[invalid]), [false]);
    let after_a_good_one = with_net(
        1,
        extended(vec![(NIPurpose::CoreP2p, vec![service([1, 1, 1, 1], 1), NIEntry::Invalid])]),
    );
    assert_eq!(adds(&[after_a_good_one, entry(2)]), [false, true]);
}

#[test]
fn an_evo_platform_node_id_is_unique_and_a_null_one_is_exempt() {
    let node = |n, mn_type, id| {
        with(n, |e| {
            e.mn_type = mn_type;
            e.platform_node_id = PlatformNodeId::from_lendian([id; 20]);
        })
    };
    let evo = RawMnType::EVO;
    let regular = RawMnType::REGULAR;
    assert_eq!(adds(&[node(1, evo, 7), node(2, evo, 7)]), [true, false]);
    assert_eq!(adds(&[node(1, evo, 7), node(2, evo, 8)]), [true, true]);
    assert_eq!(adds(&[node(1, evo, 0), node(2, evo, 0)]), [true, true]);
    // A regular masternode registers no platform id.
    assert_eq!(adds(&[node(1, regular, 7), node(2, regular, 7)]), [true, true]);
    assert_eq!(adds(&[node(1, regular, 7), node(2, evo, 7)]), [true, true]);
}

#[test]
fn a_non_null_operator_key_is_unique() {
    let key = |n, k| with(n, |e| e.operator_key = [k; 48]);
    assert_eq!(adds(&[key(1, 3), key(2, 3)]), [true, false]);
    assert_eq!(adds(&[key(1, 0), key(2, 0)]), [true, true]);
}

/// `AddMN` restores the property map when a later property conflicts: nothing of the rejected
/// masternode stays registered.
#[test]
fn a_later_conflict_rolls_back_every_property() {
    let first = with(1, |e| e.operator_key = [3; 48]);
    let rejected = with(2, |e| {
        e.net_info = legacy(v4_mapped([9, 9, 9, 9]), 9999);
        e.operator_key = [3; 48];
    });
    let reuses_its_service = with_net(3, legacy(v4_mapped([9, 9, 9, 9]), 9999));
    let (scratch, results) = add_all([&first, &rejected, &reuses_its_service]);
    assert_eq!(results, [true, false, true]);
    assert!(!scratch.contains(&rejected.pro_reg_tx_hash));
    assert_eq!(scratch.total(), 2);
}

/// `AddUniqueProperty`: a value the same masternode already holds counts again.
#[test]
fn the_same_masternode_may_hold_a_value_twice() {
    let twice = with_net(
        1,
        extended(vec![
            (NIPurpose::CoreP2p, vec![service([4, 4, 4, 4], 9999)]),
            (NIPurpose::PlatformP2p, vec![service([4, 4, 4, 4], 9999)]),
        ]),
    );
    let other =
        with_net(2, extended(vec![(NIPurpose::CoreP2p, vec![service([4, 4, 4, 4], 9999)])]));
    assert_eq!(adds(&[twice, other]), [true, false]);
}

#[test]
fn for_each_only_valid_skips_banned_entries() {
    let banned = with(2, |e| e.is_valid = false);
    let list = SmlList::from_entries([entry(1), banned.clone(), entry(3)]);
    let seen = |only_valid| {
        let mut out = Vec::new();
        list.for_each(only_valid, &mut |m: &SmlMember| out.push(m.entry().pro_reg_tx_hash));
        out
    };
    let ids = |ns: &[u8]| ns.iter().map(|&n| ProTxHash::from([n; 32])).collect::<Vec<_>>();
    assert_eq!(seen(false), ids(&[1, 2, 3]));
    assert_eq!(seen(true), ids(&[1, 3]));
    assert_eq!(
        list.counts(),
        ListCounts {
            total: 3,
            enabled: 2
        }
    );
    assert!(list.is_pose_banned(&banned.pro_reg_tx_hash));

    let mut scratch = list.new_scratch();
    for e in list.entries() {
        scratch.try_add(&SmlMember::new(e)).expect("distinct");
    }
    let mut valid = Vec::new();
    scratch.for_each(true, &mut |m| valid.push(m.entry().pro_reg_tx_hash));
    assert_eq!(valid, ids(&[1, 3]));
    let mut all = 0;
    scratch.for_each(false, &mut |_| all += 1);
    assert_eq!(all, 3);
}

#[test]
fn a_member_reads_what_cores_state_stores() {
    let unconfirmed = SmlMember::new(&entry(1));
    assert!(unconfirmed.confirmed_hash_is_null());
    assert_eq!(unconfirmed.confirmed_hash_with_protx(), &[0u8; 32]);
    let confirmed = with(1, |e| e.confirmed_hash = BlockHash::from_lendian([9; 32]));
    let member = SmlMember::new(&confirmed);
    assert!(!member.confirmed_hash_is_null());
    let pre =
        [confirmed.pro_reg_tx_hash.as_bytes().as_slice(), confirmed.confirmed_hash.as_bytes()]
            .concat();
    assert_eq!(member.confirmed_hash_with_protx(), &sha256(&pre));
    // The SML carries no collateral: the null outpoint.
    assert_eq!(member.collateral().index, u32::MAX);
    assert_eq!(member.collateral().hash.as_bytes(), &[0u8; 32]);
    assert!(!member.is_banned());
    assert!(SmlMember::new(&with(1, |e| e.is_valid = false)).is_banned());
    assert!(SmlMember::new(&with(1, |e| e.mn_type = RawMnType::EVO)).is_evo());
}

/// The operator key decodes under the entry version's scheme, legacy for version 1, with no
/// fallback, and is the invalid object when it does not decode.
#[test]
fn the_operator_key_reads_under_the_entry_versions_scheme() {
    let secret = BlsSecretKey::<BlsScIetf>::from_ikm(&[5; 32]).expect("a key");
    let value = BlsPkValue::from_canonical(&secret.public_key().to_bytes(), BlsSchemeKind::Basic)
        .expect("canonical");
    let key = BlsEagerPk::new(value.clone(), BlsSchemeKind::Basic);
    let basic = key.to_bytes(BlsSchemeKind::Basic);
    let legacy_bytes = key.to_bytes(BlsSchemeKind::Legacy);
    let at = |version, bytes: [u8; 48]| {
        SmlMember::new(&with(1, |e| {
            e.version = version;
            e.operator_key = bytes;
        }))
        .operator_key()
    };
    assert_eq!(at(2, basic), value);
    assert_eq!(at(1, legacy_bytes), value);
    assert_ne!(at(1, basic), value);
    assert_ne!(at(2, legacy_bytes), value);
    assert_eq!(at(2, [0xff; 48]), BlsPkValue::invalid());
}
