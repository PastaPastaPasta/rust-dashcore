//! Boundaries Core's own AddrMan tests leave open, each asserting the result Core's code gives
//! (v24.0.0-rc.3 `addrman.cpp`; the cited lines decide it) on a crafted table or `peers.dat`.
//! Each one fails under a mutation the SB6 review found surviving (m12).

use super::imp::{AddrInfo, NEW_BUCKET_COUNT, NEW_BUCKETS_PER_ADDRESS};
use super::*;
use dash_p2p_core::ServiceFlags;

const NOW: i64 = 1_700_000_000;
/// Mainnet's message start.
const MAGIC: [u8; 4] = [0xbf, 0x0c, 0x6b, 0xbd];
/// `-checkaddrman=1`: `CheckAddrman` after every operation, so each crafted state is also one
/// Core's consistency check accepts.
const CHECK_EVERY_TIME: u32 = 1;
/// The crafted files' `nKey`.
const KEY: [u8; 32] = [0x5a; 32];

fn ip(s: &str) -> NetAddr {
    NetAddr::from_ip(s.parse().expect("IP literal"))
}

fn svc(s: &str, port: u16) -> Service {
    Service::new(ip(s), port)
}

fn addr_at(service: &Service, time: i64) -> Address {
    Address {
        service: service.clone(),
        services: ServiceFlags::NODE_NETWORK,
        time: time as u32,
    }
}

fn addrman(ratio: u32) -> AddrMan {
    AddrMan::new(NetGroupManager::default(), true, ratio, false, MAGIC)
}

fn info(service: &Service, source: &NetAddr) -> AddrInfo {
    AddrInfo {
        service: service.clone(),
        source: source.clone(),
        ..AddrInfo::default()
    }
}

/// `GetBucketPosition(nKey, true, bucket)` of `service` (it depends on the address alone).
fn new_pos(service: &Service, bucket: i32) -> i32 {
    info(service, &NetAddr::default()).bucket_position(&KEY, true, bucket)
}

/// The new-table slots `service` holds, in bucket order.
fn new_slots(am: &AddrMan, service: &Service) -> Vec<(i32, i32)> {
    am.get_entries(false)
        .into_iter()
        .filter(|(e, _)| e.service == *service)
        .map(|(_, p)| (p.bucket, p.position))
        .collect()
}

/// A `peers.dat` built byte by byte as Core's `SerializeDB` + `AddrManImpl::Serialize` lay it
/// out (addrdb.cpp:38-60, addrman.cpp:126-226): mainnet magic, format 4 (`V4_MULTIPORT`) with
/// compat 32 + 4, [`KEY`], `nNew` new entries (IPv4, `NODE_NETWORK`, time `NOW - 1000`, no tried
/// entries), `nUBuckets = 1024 ^ (1 << 30)`, then for each bucket its listed entry indices, the
/// null asmap checksum, and SHA-256d of everything before it.
fn crafted_peers_dat(entries: &[(Service, NetAddr)], buckets: &[(i32, &[i32])]) -> Vec<u8> {
    fn ipv4_v2(out: &mut Vec<u8>, addr: &NetAddr) {
        // BIP155 network id 1 (IPv4), CompactSize length 4, the bytes.
        out.extend_from_slice(&[1, 4]);
        out.extend_from_slice(&addr.addr_bytes()[12..]);
    }
    let mut f = MAGIC.to_vec();
    f.extend_from_slice(&[4, 32 + 4]);
    f.extend_from_slice(&KEY);
    f.extend_from_slice(&(entries.len() as i32).to_le_bytes());
    f.extend_from_slice(&0i32.to_le_bytes());
    f.extend_from_slice(&((NEW_BUCKET_COUNT as i32) ^ (1 << 30)).to_le_bytes());
    for (service, source) in entries {
        // `CAddress`: version word `DISK_VERSION_INIT | DISK_VERSION_ADDRV2`, nTime, nServices
        // as CompactSize, the address, the port big-endian.
        f.extend_from_slice(&(220_000u32 | (1 << 29)).to_le_bytes());
        f.extend_from_slice(&((NOW - 1000) as u32).to_le_bytes());
        f.push(1);
        ipv4_v2(&mut f, &service.addr);
        f.extend_from_slice(&service.port.to_be_bytes());
        // `source`, `m_last_success`, `nAttempts`.
        ipv4_v2(&mut f, source);
        f.extend_from_slice(&0i64.to_le_bytes());
        f.extend_from_slice(&0i32.to_le_bytes());
    }
    for bucket in 0..NEW_BUCKET_COUNT as i32 {
        let listed: &[i32] =
            buckets.iter().find(|(b, _)| *b == bucket).map(|(_, l)| *l).unwrap_or(&[]);
        f.extend_from_slice(&(listed.len() as i32).to_le_bytes());
        for index in listed {
            f.extend_from_slice(&index.to_le_bytes());
        }
    }
    f.extend_from_slice(&[0; 32]);
    let hash = dash_sha256::sha256d(&f);
    f.extend_from_slice(&hash);
    f
}

/// `GetAddr_` filters by `GetNetClass()` (addrman.cpp:842): a 6to4 address is an IPv6 address
/// whose class is IPv4, so it answers an IPv4 request and not an IPv6 one.
#[test]
fn getaddr_filters_by_net_class() {
    let am = addrman(CHECK_EVERY_TIME);
    let six_to_four = svc("2002:102:304::1", 9999);
    let ipv4 = svc("250.1.1.1", 9999);
    let ipv6 = svc("2a01:4f8::1", 9999);
    assert_eq!(six_to_four.addr.network(false), Network::Ipv6);
    assert_eq!(six_to_four.addr.net_class(false), Network::Ipv4);
    for s in [&six_to_four, &ipv4, &ipv6] {
        assert!(am.add(&[addr_at(s, NOW - 100)], &ip("252.2.2.2"), 0, NOW));
    }
    let got = |net| {
        let mut v: Vec<Service> =
            am.get_addr(0, 0, Some(net), false, NOW).into_iter().map(|a| a.service).collect();
        v.sort();
        v
    };
    let mut want_v4 = vec![six_to_four.clone(), ipv4];
    want_v4.sort();
    assert_eq!(got(Network::Ipv4), want_v4);
    assert_eq!(got(Network::Ipv6), vec![ipv6]);
}

/// `AddSingle` overwrites an occupied new-table slot only when the occupant is terrible, or has
/// more than one reference *and* the incoming address has none (addrman.cpp:612). An address
/// already in the new table (one reference) whose next bucket holds a healthy two-reference
/// entry is refused every time, and neither entry changes.
#[test]
fn new_table_overwrite_needs_an_unreferenced_newcomer() {
    let a = svc("250.1.1.1", 9999);
    let source = ip("252.2.2.2");
    let netgroup = NetGroupManager::default();
    // The bucket and slot a re-announcement of `a` from `source` lands in.
    let bucket = info(&a, &source).new_bucket(&KEY, &netgroup);
    let pos = new_pos(&a, bucket);
    // The first address that takes the same slot in that bucket.
    let b = (0..=u16::MAX)
        .map(|i| svc(&format!("250.2.{}.{}", i >> 8, i & 0xff), 9999))
        .find(|b| new_pos(b, bucket) == pos)
        .expect("a colliding address");
    let other = |k: i32| (bucket + k) % NEW_BUCKET_COUNT as i32;
    // `a` (entry 0) in one other bucket; `b` (entry 1) in the contested bucket and one more.
    let file = crafted_peers_dat(
        &[(a.clone(), ip("253.3.3.3")), (b.clone(), ip("253.3.3.3"))],
        &[(bucket, &[1]), (other(1), &[1]), (other(2), &[0])],
    );
    let am = addrman(CHECK_EVERY_TIME);
    am.unserialize(&file).expect("Core loads the crafted file");
    assert_eq!(new_slots(&am, &a), [(other(2), new_pos(&a, other(2)))]);
    let b_slots = {
        let mut s = vec![(bucket, pos), (other(1), new_pos(&b, other(1)))];
        s.sort();
        s
    };
    assert_eq!(new_slots(&am, &b), b_slots);

    // Each announcement is newer than the stored time but within the hour (no nTime update,
    // addrman.cpp:575-578), so it reaches the stochastic test (1 in 2 for one reference), and
    // whenever that passes, the slot rule.
    for i in 1..=32 {
        assert!(!am.add(&[addr_at(&a, NOW - 1000 + i)], &source, 0, NOW), "announcement {i}");
    }
    assert_eq!(new_slots(&am, &a), [(other(2), new_pos(&a, other(2)))]);
    assert_eq!(new_slots(&am, &b), b_slots);
    assert_eq!(am.check(), 0);
}

/// `ResolveCollisions_` replaces a tried entry that was tried and failed recently only once
/// more than 60 seconds have passed since that try (addrman.cpp:957): at exactly 60 the
/// collision stays.
#[test]
fn resolve_collisions_waits_more_than_60_seconds() {
    // Core's addrman_evictionworks layout: 250.1.1.36 collides with 250.1.1.19 in tried.
    let am = addrman(100);
    let source = ip("252.2.2.2");
    for i in 1..37 {
        let addr = svc(&format!("250.1.1.{i}"), 0);
        assert!(am.add(&[addr_at(&addr, NOW - 100)], &source, 0, NOW));
        assert_eq!(am.good(&addr, NOW), i != 36);
    }
    let old = svc("250.1.1.19", 0);
    let new = svc("250.1.1.36", 0);
    assert_eq!(am.select_tried_collision().map(|s| s.addr.service), Some(old.clone()));
    // Last success in the deep past; tried and failed exactly 60 s ago.
    assert!(!am.good(&old, 1));
    am.attempt(&old, false, NOW - 60);

    am.resolve_collisions(NOW);
    assert_eq!(am.select_tried_collision().map(|s| s.addr.service), Some(old.clone()));
    assert!(!am.find_address_entry(&new).expect("in new").tried);

    am.resolve_collisions(NOW + 1);
    assert!(am.select_tried_collision().is_none());
    assert!(am.find_address_entry(&new).expect("in tried").tried);
}

/// `Unserialize` gives an entry at most `ADDRMAN_NEW_BUCKETS_PER_ADDRESS` (8) references: an
/// index listed in nine buckets takes the first eight and the ninth is skipped
/// (addrman.cpp:368, `nRefCount >= 8`).
#[test]
fn unserialize_caps_references_at_eight() {
    let x = svc("250.1.1.1", 9999);
    let buckets: Vec<(i32, &[i32])> = (0..9).map(|b| (b, &[0][..])).collect();
    let file = crafted_peers_dat(&[(x.clone(), ip("252.2.2.2"))], &buckets);
    let am = addrman(CHECK_EVERY_TIME);
    am.unserialize(&file).expect("Core loads the crafted file");
    let expected: Vec<(i32, i32)> =
        (0..NEW_BUCKETS_PER_ADDRESS).map(|b| (b, new_pos(&x, b))).collect();
    assert_eq!(new_slots(&am, &x), expected);
    assert_eq!(am.find_address_entry(&x).map(|p| p.multiplicity), Some(NEW_BUCKETS_PER_ADDRESS));
}

/// `Unserialize` keeps a bucket entry only when `entry_index >= 0 && entry_index < nNew`
/// (addrman.cpp:336): a negative index other than -1, and one past the entries, are skipped.
#[test]
fn unserialize_skips_negative_entry_indices() {
    let x = svc("250.1.1.1", 9999);
    let file = crafted_peers_dat(
        &[(x.clone(), ip("252.2.2.2"))],
        &[(0, &[-2, 0]), (1, &[i32::MIN]), (2, &[1])],
    );
    let am = addrman(CHECK_EVERY_TIME);
    am.unserialize(&file).expect("Core loads the crafted file");
    assert_eq!(new_slots(&am, &x), [(0, new_pos(&x, 0))]);
    assert_eq!(am.size(None, None), 1);
}
