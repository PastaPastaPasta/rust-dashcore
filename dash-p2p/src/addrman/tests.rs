//! Core's `src/test/addrman_tests.cpp` (v24.0.0-rc.3), transcribed with Core's expected values,
//! plus `feature_addrman.py`'s load outcomes and port-specific checks.
//!
//! Core's suite runs with `-checkaddrman` defaulting to 100 (`GetCheckRatio`), which draws from
//! the RNG on every call; the expected values depend on it, so every AddrMan here uses ratio 100
//! too. Core reads the wall clock where these tests pass [`NOW`].

use super::imp::{AddrInfo, HashWriter, TIME_INIT, pow_066};
use super::*;
use dash_p2p_core::ServiceFlags;
use std::collections::BTreeSet;

/// Any time well after `TIME_INIT` + 30 days (Core's tests read the clock).
const NOW: i64 = 1_700_000_000;
/// Mainnet's message start (Core's unit tests run on main).
const MAGIC: [u8; 4] = [0xbf, 0x0c, 0x6b, 0xbd];
/// `GetCheckRatio(m_node)` with no `-checkaddrman`.
const RATIO: u32 = 100;
/// `test/data/asmap.raw` at v24.0.0-rc.3.
const ASMAP_RAW: &[u8] = include_bytes!("../../tests/data/asmap.raw");

fn ip(s: &str) -> NetAddr {
    NetAddr::from_ip(s.parse().expect("IP literal"))
}

fn svc(s: &str, port: u16) -> Service {
    Service::new(ip(s), port)
}

/// `CAddress(service, NODE_NONE)`: `nTime = TIME_INIT`.
fn caddr(service: &Service) -> Address {
    Address {
        service: service.clone(),
        services: ServiceFlags::NONE,
        time: TIME_INIT as u32,
    }
}

fn caddr_at(service: &Service, time: i64) -> Address {
    Address {
        time: time as u32,
        ..caddr(service)
    }
}

fn empty_netgroup() -> NetGroupManager {
    NetGroupManager::default()
}

fn asmap_netgroup() -> NetGroupManager {
    NetGroupManager::new(NetGroupManager::decode_asmap(ASMAP_RAW), false)
}

fn addrman_with(netgroup: NetGroupManager, deterministic: bool) -> AddrMan {
    AddrMan::new(netgroup, deterministic, RATIO, false, MAGIC)
}

fn addrman() -> AddrMan {
    addrman_with(empty_netgroup(), true)
}

/// `addrman->Size()`.
fn size(am: &AddrMan) -> usize {
    am.size(None, None)
}

/// `Select(new_only).first.ToStringAddrPort()`, with Core's `[::]:0` for an empty result.
fn select_str(am: &AddrMan, new_only: bool) -> String {
    am.select(new_only, None, NOW)
        .map(|s| s.addr.service.to_string())
        .unwrap_or_else(|| "[::]:0".into())
}

fn collision_str(am: &AddrMan) -> String {
    am.select_tried_collision()
        .map(|s| s.addr.service.to_string())
        .unwrap_or_else(|| "[::]:0".into())
}

fn selected(am: &AddrMan, new_only: bool, net: Option<Network>) -> Option<Service> {
    am.select(new_only, net, NOW).map(|s| s.addr.service)
}

/// `CNetAddr::SetSpecial` for a `.b32.i2p` name (base32, case-insensitive).
fn i2p(name: &str) -> Service {
    let b32 = &name[..52];
    assert!(name[52..].eq_ignore_ascii_case(".b32.i2p"));
    let mut bits = 0u32;
    let mut acc = 0u32;
    let mut out = vec![];
    for c in b32.chars() {
        let c = c.to_ascii_lowercase();
        let v = match c {
            'a'..='z' => c as u32 - 'a' as u32,
            '2'..='7' => c as u32 - '2' as u32 + 26,
            _ => panic!("base32"),
        };
        acc = (acc << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    assert_eq!(out.len(), 32);
    Service::new(NetAddr::new(Network::I2p, out).unwrap(), 0)
}

/// `(HashWriter{} << n).GetHash()` for an `int`.
fn key_of(n: i32) -> [u8; 32] {
    dash_sha256::sha256d(&n.to_le_bytes())
}

/// Replaces the trailing checksum after editing a `peers.dat` image in place.
fn rehash(file: &mut [u8]) {
    let n = file.len() - 32;
    let hash = dash_sha256::sha256d(&file[..n]);
    file[n..].copy_from_slice(&hash);
}

#[test]
fn addrman_simple() {
    let mut am = addrman();
    let source = ip("252.2.2.2");

    // Test: Does Addrman respond correctly when empty.
    assert_eq!(size(&am), 0);
    assert_eq!(select_str(&am, false), "[::]:0");

    // Test: Does Addrman::Add work as expected.
    let addr1 = svc("250.1.1.1", 8333);
    assert!(am.add(&[caddr(&addr1)], &source, 0, NOW));
    assert_eq!(size(&am), 1);
    assert_eq!(select_str(&am, false), "250.1.1.1:8333");

    // Test: Does IP address deduplication work correctly.
    let addr1_dup = svc("250.1.1.1", 8333);
    assert!(!am.add(&[caddr(&addr1_dup)], &source, 0, NOW));
    assert_eq!(size(&am), 1);

    // Test: New table has one addr and we add a diff addr we should have at least one addr.
    let addr2 = svc("250.1.1.2", 8333);
    assert!(am.add(&[caddr(&addr2)], &source, 0, NOW));
    assert!(size(&am) >= 1);

    // Test: reset addrman and test AddrMan::Add multiple addresses works as expected
    am = addrman();
    let v = [caddr(&svc("250.1.1.3", 8333)), caddr(&svc("250.1.1.4", 8333))];
    assert!(am.add(&v, &source, 0, NOW));
    assert!(size(&am) >= 1);
}

#[test]
fn addrman_ports() {
    let am = addrman();
    let source = ip("252.2.2.2");
    assert_eq!(size(&am), 0);

    // Test 7; Addr with same IP but diff port does not replace existing addr.
    let addr1 = svc("250.1.1.1", 8333);
    assert!(am.add(&[caddr(&addr1)], &source, 0, NOW));
    assert_eq!(size(&am), 1);

    let addr1_port = svc("250.1.1.1", 8334);
    assert!(am.add(&[caddr(&addr1_port)], &source, 0, NOW));
    assert_eq!(size(&am), 2);
    let ret2 = select_str(&am, false);
    assert!(ret2 == "250.1.1.1:8333" || ret2 == "250.1.1.1:8334");

    // Test: Add same IP but diff port to tried table; this converts the entry with
    // the specified port to tried, but not the other.
    am.good(&addr1_port, NOW);
    assert_eq!(size(&am), 2);
    assert_eq!(select_str(&am, true), "250.1.1.1:8333");
}

#[test]
fn addrman_select() {
    let am = addrman();
    assert!(am.select(false, None, NOW).is_none());
    assert!(am.select(true, None, NOW).is_none());

    let source = ip("252.2.2.2");

    // Add 1 address to the new table
    let addr1 = svc("250.1.1.1", 8333);
    assert!(am.add(&[caddr(&addr1)], &source, 0, NOW));
    assert_eq!(size(&am), 1);

    assert_eq!(selected(&am, true, None), Some(addr1.clone()));
    assert_eq!(selected(&am, false, None), Some(addr1.clone()));

    // Move address to the tried table
    assert!(am.good(&addr1, NOW));

    assert_eq!(size(&am), 1);
    assert!(selected(&am, true, None).is_none());
    assert_eq!(selected(&am, false, None), Some(addr1.clone()));
    assert_eq!(size(&am), 1);

    // Add one address to the new table
    let addr2 = svc("250.3.1.1", 8333);
    assert!(am.add(&[caddr(&addr2)], &addr2.addr, 0, NOW));
    assert_eq!(selected(&am, true, None), Some(addr2.clone()));

    // Add two more addresses to the new table
    let addr3 = svc("250.3.2.2", 9999);
    let addr4 = svc("250.3.3.3", 9999);

    assert!(am.add(&[caddr(&addr3)], &addr2.addr, 0, NOW));
    assert!(am.add(&[caddr(&addr4)], &ip("250.4.1.1"), 0, NOW));

    // Add three addresses to tried table.
    let addr5 = svc("250.4.4.4", 8333);
    let addr6 = svc("250.4.5.5", 7777);
    let addr7 = svc("250.4.6.6", 8333);

    assert!(am.add(&[caddr(&addr5)], &addr3.addr, 0, NOW));
    assert!(am.good(&addr5, NOW));
    assert!(am.add(&[caddr(&addr6)], &addr3.addr, 0, NOW));
    assert!(am.good(&addr6, NOW));
    assert!(am.add(&[caddr(&addr7)], &ip("250.1.1.3"), 0, NOW));
    assert!(am.good(&addr7, NOW));

    // 6 addrs + 1 addr from last test = 7.
    assert_eq!(size(&am), 7);

    // Select pulls from new and tried regardless of port number.
    let mut ports = BTreeSet::new();
    for _ in 0..20 {
        ports.insert(am.select(false, None, NOW).unwrap().addr.service.port);
    }
    assert_eq!(ports.len(), 3);
}

#[test]
fn addrman_select_by_network() {
    let am = addrman();
    assert!(selected(&am, true, Some(Network::Ipv4)).is_none());
    assert!(selected(&am, false, Some(Network::Ipv4)).is_none());

    // add ipv4 address to the new table
    let source = ip("252.2.2.2");
    let addr1 = svc("250.1.1.1", 8333);
    assert!(am.add(&[caddr(&addr1)], &source, 0, NOW));

    assert_eq!(selected(&am, true, Some(Network::Ipv4)), Some(addr1.clone()));
    assert_eq!(selected(&am, false, Some(Network::Ipv4)), Some(addr1.clone()));
    assert!(selected(&am, false, Some(Network::Ipv6)).is_none());
    assert!(selected(&am, false, Some(Network::Onion)).is_none());
    assert!(selected(&am, false, Some(Network::I2p)).is_none());
    assert!(selected(&am, false, Some(Network::Cjdns)).is_none());
    assert!(selected(&am, true, Some(Network::Cjdns)).is_none());
    assert_eq!(selected(&am, false, None), Some(addr1.clone()));

    // add I2P address to the new table
    let i2p_addr = i2p("udhdrtrcetjm5sxzskjyr5ztpeszydbh4dpl3pl4utgqqw2v4jna.b32.i2p");
    assert!(am.add(&[caddr(&i2p_addr)], &source, 0, NOW));

    assert_eq!(selected(&am, true, Some(Network::I2p)), Some(i2p_addr.clone()));
    assert_eq!(selected(&am, false, Some(Network::I2p)), Some(i2p_addr.clone()));
    assert_eq!(selected(&am, false, Some(Network::Ipv4)), Some(addr1.clone()));
    assert!(selected(&am, false, Some(Network::Ipv6)).is_none());
    assert!(selected(&am, false, Some(Network::Onion)).is_none());
    assert!(selected(&am, false, Some(Network::Cjdns)).is_none());

    // bump I2P address to tried table
    assert!(am.good(&i2p_addr, NOW));

    assert!(selected(&am, true, Some(Network::I2p)).is_none());
    assert_eq!(selected(&am, false, Some(Network::I2p)), Some(i2p_addr.clone()));

    // add another I2P address to the new table
    let i2p_addr2 = i2p("c4gfnttsuwqomiygupdqqqyy5y5emnk5c73hrfvatri67prd7vyq.b32.i2p");
    assert!(am.add(&[caddr(&i2p_addr2)], &source, 0, NOW));

    assert_eq!(selected(&am, true, Some(Network::I2p)), Some(i2p_addr2.clone()));

    // ensure that both new and tried table are selected from
    let (mut new_selected, mut tried_selected) = (false, false);
    let mut counter = 256;
    loop {
        counter -= 1;
        if counter <= 0 || (new_selected && tried_selected) {
            break;
        }
        let s = selected(&am, false, Some(Network::I2p)).unwrap();
        assert!(s == i2p_addr || s == i2p_addr2);
        if s == i2p_addr {
            tried_selected = true;
        } else {
            new_selected = true;
        }
    }
    assert!(new_selected);
    assert!(tried_selected);
}

#[test]
fn addrman_select_special() {
    // use a non-deterministic addrman to ensure a passing test isn't due to setup
    let am = addrman_with(empty_netgroup(), false);
    let source = ip("252.2.2.2");

    // add I2P address to the tried table
    let i2p_addr = i2p("udhdrtrcetjm5sxzskjyr5ztpeszydbh4dpl3pl4utgqqw2v4jna.b32.i2p");
    assert!(am.add(&[caddr(&i2p_addr)], &source, 0, NOW));
    assert!(am.good(&i2p_addr, NOW));

    // add ipv4 address to the new table
    let addr1 = svc("250.1.1.3", 8333);
    assert!(am.add(&[caddr(&addr1)], &source, 0, NOW));

    // since the only ipv4 address is on the new table, ensure that the new
    // table gets selected even if new_only is false.
    assert_eq!(selected(&am, false, Some(Network::Ipv4)), Some(addr1));
}

#[test]
fn addrman_new_collisions() {
    let am = addrman();
    let source = ip("252.2.2.2");
    let mut num_addrs = 0usize;
    assert_eq!(size(&am), num_addrs);

    // Magic number! 250.1.1.1 - 250.1.1.22 do not collide with deterministic key = 1
    while num_addrs < 22 {
        num_addrs += 1;
        let addr = svc(&format!("250.1.1.{num_addrs}"), 0);
        assert!(am.add(&[caddr(&addr)], &source, 0, NOW));
        // Test: No collision in new table yet.
        assert_eq!(size(&am), num_addrs);
    }

    // Test: new table collision!
    num_addrs += 1;
    let addr1 = svc(&format!("250.1.1.{num_addrs}"), 0);
    let collisions = 1;
    assert!(am.add(&[caddr(&addr1)], &source, 0, NOW));
    assert_eq!(size(&am), num_addrs - collisions);

    num_addrs += 1;
    let addr2 = svc(&format!("250.1.1.{num_addrs}"), 0);
    assert!(am.add(&[caddr(&addr2)], &source, 0, NOW));
    assert_eq!(size(&am), num_addrs - collisions);
}

#[test]
fn addrman_new_multiplicity() {
    let am = addrman();
    let service = svc("253.3.3.3", 8333);
    let start_time = NOW;

    // test that multiplicity stays at 1 if nTime doesn't increase
    for i in 1u32..20 {
        let source = ip(&format!("{}.{}.1.1", i % 256, i >> 8));
        am.add(&[caddr_at(&service, start_time)], &source, 0, NOW);
    }
    let pos = am.find_address_entry(&service).unwrap();
    assert_eq!(pos.multiplicity, 1);
    assert_eq!(size(&am), 1);

    // if nTime increases, an addr can occur in up to 8 buckets
    for i in 1u32..400 {
        let source = ip(&format!("{}.{}.1.1", i % 256, i >> 8));
        am.add(&[caddr_at(&service, start_time + i64::from(i))], &source, 0, NOW);
    }
    let pos = am.find_address_entry(&service).unwrap();
    assert_eq!(pos.multiplicity, 8);
    // multiplicity doesn't affect size
    assert_eq!(size(&am), 1);
}

#[test]
fn addrman_tried_collisions() {
    let am = addrman();
    let source = ip("252.2.2.2");
    let mut num_addrs = 0;
    assert_eq!(size(&am), 0);

    // Magic number! 250.1.1.1 - 250.1.1.35 do not collide in tried with deterministic key = 1
    while num_addrs < 35 {
        num_addrs += 1;
        let addr = svc(&format!("250.1.1.{num_addrs}"), 0);
        assert!(am.add(&[caddr(&addr)], &source, 0, NOW));
        // Test: Add to tried without collision
        assert!(am.good(&addr, NOW));
    }

    // Test: Unable to add to tried table due to collision!
    num_addrs += 1;
    let addr1 = svc(&format!("250.1.1.{num_addrs}"), 0);
    assert!(am.add(&[caddr(&addr1)], &source, 0, NOW));
    assert!(!am.good(&addr1, NOW));

    // Test: Add the next address to tried without collision
    num_addrs += 1;
    let addr2 = svc(&format!("250.1.1.{num_addrs}"), 0);
    assert!(am.add(&[caddr(&addr2)], &source, 0, NOW));
    assert!(am.good(&addr2, NOW));
}

#[test]
fn addrman_getaddr() {
    let am = addrman();

    // Test: Sanity check, GetAddr should never return anything if addrman is empty.
    assert_eq!(size(&am), 0);
    assert_eq!(am.get_addr(0, 0, None, true, NOW).len(), 0);

    let addr1 = caddr_at(&svc("250.250.2.1", 8333), NOW);
    let addr2 = caddr_at(&svc("250.251.2.2", 9999), NOW);
    let addr3 = caddr_at(&svc("251.252.2.3", 8333), NOW);
    let addr4 = caddr_at(&svc("252.253.3.4", 8333), NOW);
    let addr5 = caddr_at(&svc("252.254.4.5", 8333), NOW);
    let source1 = ip("250.1.2.1");
    let source2 = ip("250.2.3.3");

    // Test: Ensure GetAddr works with new addresses.
    assert!(am.add(&[addr1.clone(), addr3, addr5], &source1, 0, NOW));
    assert!(am.add(&[addr2.clone(), addr4], &source2, 0, NOW));

    assert_eq!(am.get_addr(0, 0, None, true, NOW).len(), 5);
    // Net processing asks for 23% of addresses. 23% of 5 is 1 rounded down.
    assert_eq!(am.get_addr(2500, 23, None, true, NOW).len(), 1);

    // Test: Ensure GetAddr works with new and tried addresses.
    assert!(am.good(&addr1.service, NOW));
    assert!(am.good(&addr2.service, NOW));
    assert_eq!(am.get_addr(0, 0, None, true, NOW).len(), 5);
    assert_eq!(am.get_addr(2500, 23, None, true, NOW).len(), 1);

    // Test: Ensure GetAddr still returns 23% when addrman has many addrs.
    for i in 1u32..(8 * 256) {
        let octet1 = i % 256;
        let octet2 = i >> 8;
        let s = format!("{octet1}.{octet2}.1.23");
        // Ensure that for all addrs in addrman, isTerrible == false.
        let addr = caddr_at(&svc(&s, 0), NOW);
        am.add(std::slice::from_ref(&addr), &ip(&s), 0, NOW);
        if i % 8 == 0 {
            am.good(&addr.service, NOW);
        }
    }
    let v = am.get_addr(2500, 23, None, true, NOW);

    let percent23 = size(&am) * 23 / 100;
    assert_eq!(v.len(), percent23);
    assert_eq!(v.len(), 461);
    // (addrman.Size() < number of addresses added) due to address collisions.
    assert_eq!(size(&am), 2006);
}

#[test]
fn getaddr_unfiltered() {
    let am = addrman();

    // Set time on this addr so isTerrible = false
    let addr1 = caddr_at(&svc("250.250.2.1", 8333), NOW);
    // Not setting time so this addr should be isTerrible = true
    let addr2 = caddr(&svc("250.251.2.2", 9999));

    let source = ip("250.1.2.1");
    assert!(am.add(&[addr1, addr2], &source, 0, NOW));

    // Filtered GetAddr should only return addr1
    assert_eq!(am.get_addr(0, 0, None, true, NOW).len(), 1);
    // Unfiltered GetAddr should return addr1 and addr2
    assert_eq!(am.get_addr(0, 0, None, false, NOW).len(), 2);
}

fn info(addr: &str, port: u16, source: &str) -> AddrInfo {
    AddrInfo::new(&caddr(&svc(addr, port)), &ip(source))
}

#[test]
fn caddrinfo_get_tried_bucket_legacy() {
    let ng = empty_netgroup();
    let info1 = info("250.1.1.1", 8333, "250.1.1.1");

    let key1 = key_of(1);
    let key2 = key_of(2);

    assert_eq!(info1.tried_bucket(&key1, &ng), 40);

    // Test: Make sure key actually randomizes bucket placement. A fail on
    //  this test could be a security issue.
    assert_ne!(info1.tried_bucket(&key1, &ng), info1.tried_bucket(&key2, &ng));

    // Test: Two addresses with same IP but different ports can map to
    //  different buckets because they have different keys.
    let info2 = info("250.1.1.1", 9999, "250.1.1.1");
    assert_ne!(info1.service.key(), info2.service.key());
    assert_ne!(info1.tried_bucket(&key1, &ng), info2.tried_bucket(&key1, &ng));

    let buckets: BTreeSet<i32> = (0..255)
        .map(|i| {
            let a = format!("250.1.1.{i}");
            info(&a, 0, &a).tried_bucket(&key1, &ng)
        })
        .collect();
    // Test: IP addresses in the same /16 prefix should
    // never get more than 8 buckets with legacy grouping
    assert_eq!(buckets.len(), 8);

    let buckets: BTreeSet<i32> = (0..255)
        .map(|j| {
            let a = format!("250.{j}.1.1");
            info(&a, 0, &a).tried_bucket(&key1, &ng)
        })
        .collect();
    // Test: IP addresses in the different /16 prefix should map to more than
    // 8 buckets with legacy grouping
    assert_eq!(buckets.len(), 160);
}

#[test]
fn caddrinfo_get_new_bucket_legacy() {
    let ng = empty_netgroup();
    let source1 = ip("250.1.2.1");
    let info1 = info("250.1.2.1", 8333, "250.1.2.1");

    let key1 = key_of(1);
    let key2 = key_of(2);

    // Test: Make sure the buckets are what we expect
    assert_eq!(info1.new_bucket(&key1, &ng), 786);
    assert_eq!(info1.new_bucket_from(&key1, &source1, &ng), 786);

    // Test: Make sure key actually randomizes bucket placement. A fail on
    //  this test could be a security issue.
    assert_ne!(info1.new_bucket(&key1, &ng), info1.new_bucket(&key2, &ng));

    // Test: Ports should not affect bucket placement in the addr
    let info2 = info("250.1.2.1", 9999, "250.1.2.1");
    assert_ne!(info1.service.key(), info2.service.key());
    assert_eq!(info1.new_bucket(&key1, &ng), info2.new_bucket(&key1, &ng));

    let buckets: BTreeSet<i32> = (0..255)
        .map(|i| {
            let a = format!("250.1.1.{i}");
            info(&a, 0, &a).new_bucket(&key1, &ng)
        })
        .collect();
    // Test: IP addresses in the same group (\16 prefix for IPv4) should
    //  always map to the same bucket.
    assert_eq!(buckets.len(), 1);

    let buckets: BTreeSet<i32> = (0..4 * 255)
        .map(|j| {
            let a = format!("{}.{}.1.1", 250 + j / 255, j % 256);
            info(&a, 0, "251.4.1.1").new_bucket(&key1, &ng)
        })
        .collect();
    // Test: IP addresses in the same source groups should map to NO MORE
    //  than 64 buckets.
    assert!(buckets.len() <= 64);

    let buckets: BTreeSet<i32> = (0..255)
        .map(|p| info("250.1.1.1", 0, &format!("250.{p}.1.1")).new_bucket(&key1, &ng))
        .collect();
    // Test: IP addresses in the different source groups should map to MORE
    //  than 64 buckets.
    assert!(buckets.len() > 64);
}

// The following three test cases use asmap.raw
// We use an artificial minimal mock mapping
// 250.0.0.0/8 AS1000
// 101.1.0.0/16 AS1
// 101.2.0.0/16 AS2
// 101.3.0.0/16 AS3
// 101.4.0.0/16 AS4
// 101.5.0.0/16 AS5
// 101.6.0.0/16 AS6
// 101.7.0.0/16 AS7
// 101.8.0.0/16 AS8
#[test]
fn caddrinfo_get_tried_bucket() {
    let ng = asmap_netgroup();
    let info1 = info("250.1.1.1", 8333, "250.1.1.1");

    let key1 = key_of(1);
    let key2 = key_of(2);

    assert_eq!(info1.tried_bucket(&key1, &ng), 236);

    // Test: Make sure key actually randomizes bucket placement. A fail on
    //  this test could be a security issue.
    assert_ne!(info1.tried_bucket(&key1, &ng), info1.tried_bucket(&key2, &ng));

    // Test: Two addresses with same IP but different ports can map to
    //  different buckets because they have different keys.
    let info2 = info("250.1.1.1", 9999, "250.1.1.1");
    assert_ne!(info1.service.key(), info2.service.key());
    assert_ne!(info1.tried_bucket(&key1, &ng), info2.tried_bucket(&key1, &ng));

    let buckets: BTreeSet<i32> = (0..255)
        .map(|j| {
            let a = format!("101.{j}.1.1");
            info(&a, 0, &a).tried_bucket(&key1, &ng)
        })
        .collect();
    // Test: IP addresses in the different /16 prefix MAY map to more than
    // 8 buckets.
    assert!(buckets.len() > 8);

    let buckets: BTreeSet<i32> = (0..255)
        .map(|j| {
            let a = format!("250.{j}.1.1");
            info(&a, 0, &a).tried_bucket(&key1, &ng)
        })
        .collect();
    // Test: IP addresses in the different /16 prefix MAY NOT map to more than
    // 8 buckets.
    assert_eq!(buckets.len(), 8);
}

#[test]
fn caddrinfo_get_new_bucket() {
    let ng = asmap_netgroup();
    let source1 = ip("250.1.2.1");
    let info1 = info("250.1.2.1", 8333, "250.1.2.1");

    let key1 = key_of(1);
    let key2 = key_of(2);

    // Test: Make sure the buckets are what we expect
    assert_eq!(info1.new_bucket(&key1, &ng), 795);
    assert_eq!(info1.new_bucket_from(&key1, &source1, &ng), 795);

    // Test: Make sure key actually randomizes bucket placement. A fail on
    //  this test could be a security issue.
    assert_ne!(info1.new_bucket(&key1, &ng), info1.new_bucket(&key2, &ng));

    // Test: Ports should not affect bucket placement in the addr
    let info2 = info("250.1.2.1", 9999, "250.1.2.1");
    assert_ne!(info1.service.key(), info2.service.key());
    assert_eq!(info1.new_bucket(&key1, &ng), info2.new_bucket(&key1, &ng));

    let buckets: BTreeSet<i32> = (0..255)
        .map(|i| {
            let a = format!("250.1.1.{i}");
            info(&a, 0, &a).new_bucket(&key1, &ng)
        })
        .collect();
    // Test: IP addresses in the same /16 prefix usually map to the same bucket.
    assert_eq!(buckets.len(), 1);

    let buckets: BTreeSet<i32> = (0..4 * 255)
        .map(|j| {
            let a = format!("{}.{}.1.1", 250 + j / 255, j % 256);
            info(&a, 0, "251.4.1.1").new_bucket(&key1, &ng)
        })
        .collect();
    // Test: IP addresses in the same source /16 prefix should not map to more
    // than 64 buckets.
    assert!(buckets.len() <= 64);

    let buckets: BTreeSet<i32> = (0..255)
        .map(|p| info("250.1.1.1", 0, &format!("101.{p}.1.1")).new_bucket(&key1, &ng))
        .collect();
    // Test: IP addresses in the different source /16 prefixes usually map to MORE
    // than 1 bucket.
    assert!(buckets.len() > 1);

    let buckets: BTreeSet<i32> = (0..255)
        .map(|p| info("250.1.1.1", 0, &format!("250.{p}.1.1")).new_bucket(&key1, &ng))
        .collect();
    // Test: IP addresses in the different source /16 prefixes sometimes map to NO MORE
    // than 1 bucket.
    assert_eq!(buckets.len(), 1);
}

#[test]
fn addrman_serialization() {
    let mut addrman_asmap1 = addrman_with(asmap_netgroup(), true);
    let addrman_asmap1_dup = addrman_with(asmap_netgroup(), true);
    let mut addrman_noasmap = addrman_with(empty_netgroup(), true);

    let addr = caddr(&svc("250.1.1.1", 0));
    let default_source = NetAddr::default();

    addrman_asmap1.add(std::slice::from_ref(&addr), &default_source, 0, NOW);

    // serizalizing/deserializing addrman with the same asmap
    addrman_asmap1_dup.deserialize(&addrman_asmap1.serialize()).unwrap();

    let addr_pos1 = addrman_asmap1.find_address_entry(&addr.service).unwrap();
    let addr_pos2 = addrman_asmap1_dup.find_address_entry(&addr.service).unwrap();
    assert_ne!(addr_pos1.multiplicity, 0);
    assert_ne!(addr_pos2.multiplicity, 0);
    assert_eq!(addr_pos1, addr_pos2);

    // deserializing asmaped peers.dat to non-asmaped addrman
    addrman_noasmap.deserialize(&addrman_asmap1.serialize()).unwrap();
    let addr_pos3 = addrman_noasmap.find_address_entry(&addr.service).unwrap();
    assert_ne!(addr_pos3.multiplicity, 0);
    assert_ne!(addr_pos1.bucket, addr_pos3.bucket);
    assert_ne!(addr_pos1.position, addr_pos3.position);

    // deserializing non-asmaped peers.dat to asmaped addrman
    addrman_asmap1 = addrman_with(asmap_netgroup(), true);
    addrman_noasmap = addrman_with(empty_netgroup(), true);
    addrman_noasmap.add(std::slice::from_ref(&addr), &default_source, 0, NOW);
    addrman_asmap1.deserialize(&addrman_noasmap.serialize()).unwrap();

    let addr_pos4 = addrman_asmap1.find_address_entry(&addr.service).unwrap();
    assert_ne!(addr_pos4.multiplicity, 0);
    assert_ne!(addr_pos4.bucket, addr_pos3.bucket);
    assert_eq!(addr_pos4, addr_pos2);

    // used to map to different buckets, now maps to the same bucket.
    addrman_asmap1 = addrman_with(asmap_netgroup(), true);
    addrman_noasmap = addrman_with(empty_netgroup(), true);
    let addr1 = caddr(&svc("250.1.1.1", 0));
    let addr2 = caddr(&svc("250.2.1.1", 0));
    addrman_noasmap.add(&[addr.clone(), addr2.clone()], &default_source, 0, NOW);
    let addr_pos5 = addrman_noasmap.find_address_entry(&addr1.service).unwrap();
    let addr_pos6 = addrman_noasmap.find_address_entry(&addr2.service).unwrap();
    assert_ne!(addr_pos5.bucket, addr_pos6.bucket);
    addrman_asmap1.deserialize(&addrman_noasmap.serialize()).unwrap();
    let addr_pos7 = addrman_asmap1.find_address_entry(&addr1.service).unwrap();
    let addr_pos8 = addrman_asmap1.find_address_entry(&addr2.service).unwrap();
    assert_eq!(addr_pos7.bucket, addr_pos8.bucket);
    assert_ne!(addr_pos7.position, addr_pos8.position);
}

fn find(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len()).position(|w| w == needle).expect("pattern in stream")
}

#[test]
fn remove_invalid() {
    // Confirm that invalid addresses are ignored in unserialization.
    let mut am = addrman();

    let new1 = caddr(&svc("5.5.5.5", 0));
    let new2 = caddr(&svc("6.6.6.6", 0));
    let tried1 = caddr(&svc("7.7.7.7", 0));
    let tried2 = caddr(&svc("8.8.8.8", 0));

    am.add(&[new1, tried1.clone(), new2, tried2.clone()], &NetAddr::default(), 0, NOW);
    am.good(&tried1.service, NOW);
    am.good(&tried2.service, NOW);
    assert_eq!(size(&am), 4);

    let mut stream = am.serialize();

    // 0.0.0.0 is !IsValid()
    let pos = find(&stream, &[6, 6, 6, 6]);
    stream[pos..pos + 4].copy_from_slice(&[0, 0, 0, 0]);

    // 255.255.255.255 is !IsValid()
    let pos = find(&stream, &[8, 8, 8, 8]);
    stream[pos..pos + 4].copy_from_slice(&[255, 255, 255, 255]);
    // Core's test stream has no checksum; the file form needs a valid one.
    rehash(&mut stream);

    am = addrman();
    am.deserialize(&stream).unwrap();
    assert_eq!(size(&am), 2);
}

#[test]
fn addrman_selecttriedcollision() {
    let am = addrman();
    assert_eq!(size(&am), 0);

    // Empty addrman should return blank addrman info.
    assert_eq!(collision_str(&am), "[::]:0");

    // Add twenty two addresses.
    let source = ip("252.2.2.2");
    for i in 1..23 {
        let addr = svc(&format!("250.1.1.{i}"), 0);
        assert!(am.add(&[caddr(&addr)], &source, 0, NOW));

        // No collisions in tried.
        assert!(am.good(&addr, NOW));
        assert_eq!(collision_str(&am), "[::]:0");
    }

    // Ensure Good handles duplicates well.
    // If an address is a duplicate, Good will return false but will not count it as a collision.
    for i in 1..23 {
        let addr = svc(&format!("250.1.1.{i}"), 0);

        // Unable to add duplicate address to tried table.
        assert!(!am.good(&addr, NOW));

        // Verify duplicate address not marked as a collision.
        assert_eq!(collision_str(&am), "[::]:0");
    }
}

#[test]
fn addrman_noevict() {
    let am = addrman();

    // Add 35 addresses.
    let source = ip("252.2.2.2");
    for i in 1..36 {
        let addr = svc(&format!("250.1.1.{i}"), 0);
        assert!(am.add(&[caddr(&addr)], &source, 0, NOW));

        // No collision yet.
        assert!(am.good(&addr, NOW));
    }

    // Collision in tried table between 36 and 19.
    let addr36 = svc("250.1.1.36", 0);
    assert!(am.add(&[caddr(&addr36)], &source, 0, NOW));
    assert!(!am.good(&addr36, NOW));
    assert_eq!(collision_str(&am), "250.1.1.19:0");

    // 36 should be discarded and 19 not evicted.
    // This means we keep 19 in the tried table and
    // 36 stays in the new table.
    am.resolve_collisions(NOW);
    assert_eq!(collision_str(&am), "[::]:0");

    // Lets create two collisions.
    for i in 37..59 {
        let addr = svc(&format!("250.1.1.{i}"), 0);
        assert!(am.add(&[caddr(&addr)], &source, 0, NOW));
        assert!(am.good(&addr, NOW));
    }

    // Cause a collision in the tried table.
    let addr59 = svc("250.1.1.59", 0);
    assert!(am.add(&[caddr(&addr59)], &source, 0, NOW));
    assert!(!am.good(&addr59, NOW));

    assert_eq!(collision_str(&am), "250.1.1.10:0");

    // Cause a second collision in the new table.
    assert!(!am.add(&[caddr(&addr36)], &source, 0, NOW));

    // 36 still cannot be moved from new to tried due to colliding with 19
    assert!(!am.good(&addr36, NOW));
    assert_ne!(collision_str(&am), "[::]:0");

    // Resolve all collisions.
    am.resolve_collisions(NOW);
    assert_eq!(collision_str(&am), "[::]:0");
}

#[test]
fn addrman_evictionworks() {
    let am = addrman();
    assert_eq!(size(&am), 0);

    // Empty addrman should return blank addrman info.
    assert_eq!(collision_str(&am), "[::]:0");

    // Add 35 addresses
    let source = ip("252.2.2.2");
    for i in 1..36 {
        let addr = svc(&format!("250.1.1.{i}"), 0);
        assert!(am.add(&[caddr(&addr)], &source, 0, NOW));

        // No collision yet.
        assert!(am.good(&addr, NOW));
    }

    // Collision between 36 and 19.
    let addr = svc("250.1.1.36", 0);
    assert!(am.add(&[caddr(&addr)], &source, 0, NOW));
    assert!(!am.good(&addr, NOW));

    let info = am.select_tried_collision().unwrap().addr;
    assert_eq!(info.service.to_string(), "250.1.1.19:0");

    // Ensure test of address fails, so that it is evicted.
    // Update entry in tried by setting last good connection in the deep past.
    assert!(!am.good(&info.service, 1));
    am.attempt(&info.service, false, NOW - 61);

    // Should swap 36 for 19.
    am.resolve_collisions(NOW);
    assert_eq!(collision_str(&am), "[::]:0");
    let addr_pos = am.find_address_entry(&addr).unwrap();
    assert!(addr_pos.tried);

    // If 36 was swapped for 19, then adding 36 to tried should fail because we
    // are attempting to add a duplicate.
    // We check this by verifying Good() returns false and also verifying that
    // we have no collisions.
    assert!(!am.good(&addr, NOW));
    assert_eq!(collision_str(&am), "[::]:0");

    // 19 should fail as a collision (not a duplicate) if we now attempt to move
    // it to the tried table.
    let addr19 = svc("250.1.1.19", 0);
    assert!(!am.good(&addr19, NOW));
    assert_eq!(collision_str(&am), "250.1.1.36:0");

    // Eviction is also successful if too much time has passed since last try
    let later = NOW + 4 * 60 * 60;
    am.resolve_collisions(later);
    assert_eq!(collision_str(&am), "[::]:0");
    // Now 19 is in tried again, and 36 back to new
    let addr_pos19 = am.find_address_entry(&addr19).unwrap();
    assert!(addr_pos19.tried);
    let addr_pos36 = am.find_address_entry(&addr).unwrap();
    assert!(!addr_pos36.tried);
}

#[test]
fn load_addrman() {
    let am = addrman();

    let addr1 = svc("250.7.1.1", 8333);
    let addr2 = svc("250.7.2.2", 9999);
    let addr3 = svc("250.7.3.3", 9999);

    // Add three addresses to new table.
    let source = svc("252.5.1.1", 8333);
    assert!(am.add(&[caddr(&addr1), caddr(&addr2), caddr(&addr3)], &source.addr, 0, NOW));
    assert_eq!(size(&am), 3);

    // Test that the de-serialization does not throw an exception.
    let peers1 = am.serialize();
    let addrman1 = addrman_with(empty_netgroup(), false);
    assert_eq!(size(&addrman1), 0);
    assert_eq!(addrman1.deserialize(&peers1), Ok(()));
    assert_eq!(size(&addrman1), 3);

    // Test that ReadFromStream creates an addrman with the correct number of addrs.
    let addrman2 = addrman_with(empty_netgroup(), false);
    assert_eq!(size(&addrman2), 0);
    addrman2.deserialize(&am.serialize()).unwrap();
    assert_eq!(size(&addrman2), 3);
}

/// `MakeCorruptPeersDat`: a format-1 file claiming 10 new and 10 tried entries with only one.
fn make_corrupt_peers_dat() -> Vec<u8> {
    let mut s = MAGIC.to_vec();
    s.push(1);
    s.push(32);
    let mut one = [0u8; 32];
    one[0] = 1;
    s.extend_from_slice(&one);
    s.extend_from_slice(&10i32.to_le_bytes()); // nNew
    s.extend_from_slice(&10i32.to_le_bytes()); // nTried
    s.extend_from_slice(&(1024i32 ^ (1 << 30)).to_le_bytes());
    // AddrInfo(CAddress(252.1.1.1:7777, NODE_NONE), 252.2.2.2), SER_DISK, CLIENT_VERSION
    s.extend_from_slice(&220_000u32.to_le_bytes());
    s.extend_from_slice(&(TIME_INIT as u32).to_le_bytes());
    s.extend_from_slice(&0u64.to_le_bytes());
    s.extend_from_slice(&ip("252.1.1.1").v1_bytes());
    s.extend_from_slice(&7777u16.to_be_bytes());
    s.extend_from_slice(&ip("252.2.2.2").v1_bytes());
    s.extend_from_slice(&0i64.to_le_bytes());
    s.extend_from_slice(&0i32.to_le_bytes());
    s
}

#[test]
fn load_addrman_corrupted() {
    // Test that the de-serialization of corrupted peers.dat throws an exception.
    let addrman1 = addrman_with(empty_netgroup(), false);
    assert_eq!(size(&addrman1), 0);
    let err = addrman1.deserialize(&make_corrupt_peers_dat()).unwrap_err();
    // Test that ReadFromStream fails (std::ios_base::failure) if peers.dat is corrupt
    assert_eq!(err, LoadError::Corrupt("AutoFile::read: end of file: iostream error".into()));
    assert!(!err.is_unsupported_version());
    assert_eq!(size(&addrman1), 0);
}

#[test]
fn addrman_update_address() {
    // Tests updating nTime via Connected() and nServices via SetServices() and Add()
    let am = addrman();
    let source = ip("252.2.2.2");
    let service = svc("250.1.1.1", 8333);

    let start_time = NOW - 10000;
    let addr = caddr_at(&service, start_time);
    assert!(am.add(std::slice::from_ref(&addr), &source, 0, NOW));
    assert_eq!(size(&am), 1);

    // Updating an addrman entry with a different port doesn't change it
    let addr_diff_port = svc("250.1.1.1", 8334);
    am.connected(&addr_diff_port, NOW);
    am.set_services(&addr_diff_port, ServiceFlags::NODE_NETWORK_LIMITED);
    let v1 = am.get_addr(0, 0, None, true, NOW);
    assert_eq!(v1.len(), 1);
    assert_eq!(i64::from(v1[0].time), start_time);
    assert_eq!(v1[0].services, ServiceFlags::NONE);

    // Updating an addrman entry with the correct port is successful
    am.connected(&service, NOW);
    am.set_services(&service, ServiceFlags::NODE_NETWORK_LIMITED);
    let v2 = am.get_addr(0, 0, None, true, NOW);
    assert_eq!(v2.len(), 1);
    assert!(i64::from(v2[0].time) >= start_time + 10000);
    assert_eq!(v2[0].services, ServiceFlags::NODE_NETWORK_LIMITED);

    // Updating an existing addr through Add() (used in gossip relay) can add additional services
    // but can't remove existing ones.
    let addr_v2 = Address {
        services: ServiceFlags::NODE_P2P_V2,
        ..caddr_at(&service, start_time)
    };
    assert!(!am.add(std::slice::from_ref(&addr_v2), &source, 0, NOW));
    let v3 = am.get_addr(0, 0, None, true, NOW);
    assert_eq!(v3.len(), 1);
    assert_eq!(v3[0].services, ServiceFlags::NODE_P2P_V2 | ServiceFlags::NODE_NETWORK_LIMITED);

    // SetServices() (used when we connected to them) overwrites existing service flags
    am.set_services(&service, ServiceFlags::NODE_NETWORK);
    let v4 = am.get_addr(0, 0, None, true, NOW);
    assert_eq!(v4.len(), 1);
    assert_eq!(v4[0].services, ServiceFlags::NODE_NETWORK);

    // Promoting to Tried does not affect the service flags
    assert!(am.good(&service, NOW)); // addr has NODE_NONE
    let v5 = am.get_addr(0, 0, None, true, NOW);
    assert_eq!(v5.len(), 1);
    assert_eq!(v5[0].services, ServiceFlags::NODE_NETWORK);

    // Adding service flags even works when the addr is in Tried
    assert!(!am.add(&[addr_v2], &source, 0, NOW));
    let v6 = am.get_addr(0, 0, None, true, NOW);
    assert_eq!(v6.len(), 1);
    assert_eq!(v6[0].services, ServiceFlags::NODE_NETWORK | ServiceFlags::NODE_P2P_V2);
}

#[test]
fn addrman_size() {
    let am = addrman();
    let source = ip("252.2.2.2");

    // empty addrman
    assert_eq!(am.size(None, None), 0);
    assert_eq!(am.size(Some(Network::Ipv4), None), 0);
    assert_eq!(am.size(None, Some(true)), 0);
    assert_eq!(am.size(Some(Network::Ipv4), Some(false)), 0);

    // add two ipv4 addresses, one to tried and new
    let addr1 = caddr(&svc("250.1.1.1", 8333));
    assert!(am.add(std::slice::from_ref(&addr1), &source, 0, NOW));
    assert!(am.good(&addr1.service, NOW));
    let addr2 = caddr(&svc("250.1.1.2", 8333));
    assert!(am.add(&[addr2], &source, 0, NOW));

    assert_eq!(am.size(None, None), 2);
    assert_eq!(am.size(Some(Network::Ipv4), None), 2);
    assert_eq!(am.size(None, Some(true)), 1);
    assert_eq!(am.size(None, Some(false)), 1);
    assert_eq!(am.size(Some(Network::Ipv4), Some(true)), 1);
    assert_eq!(am.size(Some(Network::Ipv4), Some(false)), 1);

    // add one i2p address to new
    let i2p_addr = i2p("UDHDrtrcetjm5sxzskjyr5ztpeszydbh4dpl3pl4utgqqw2v4jna.b32.I2P");
    let addr3 = caddr(&i2p_addr);
    assert!(am.add(&[addr3], &source, 0, NOW));
    assert_eq!(am.size(None, None), 3);
    assert_eq!(am.size(Some(Network::Ipv4), None), 2);
    assert_eq!(am.size(Some(Network::I2p), None), 1);
    assert_eq!(am.size(Some(Network::I2p), Some(true)), 1);
    assert_eq!(am.size(None, Some(true)), 2);
    assert_eq!(am.size(None, Some(false)), 1);
}

// ---- feature_addrman.py: peers.dat load outcomes -------------------------------------------

/// `serialize_addrman` in `feature_addrman.py`: an empty AddrMan file.
fn serialize_addrman(
    format: u8,
    lowest_compatible: i32,
    magic: [u8; 4],
    bucket_key: u8,
    len_new: i32,
    len_tried: i32,
    mock_checksum: Option<[u8; 32]>,
) -> Vec<u8> {
    let mut r = magic.to_vec();
    r.push(format);
    r.push((32 + lowest_compatible) as u8);
    let mut key = [0u8; 32];
    key[0] = bucket_key;
    r.extend_from_slice(&key);
    r.extend_from_slice(&len_new.to_le_bytes());
    r.extend_from_slice(&len_tried.to_le_bytes());
    r.extend_from_slice(&(1024i32 ^ (1 << 30)).to_le_bytes());
    for _ in 0..1024 {
        r.extend_from_slice(&0i32.to_le_bytes());
    }
    let checksum = dash_sha256::sha256d(&r);
    r.extend_from_slice(&mock_checksum.unwrap_or(checksum));
    r
}

fn empty_file() -> Vec<u8> {
    serialize_addrman(1, 4, MAGIC, 1, 0, 0, None)
}

#[test]
fn feature_addrman_load_outcomes() {
    let am = addrman_with(empty_netgroup(), false);
    // Check that mocked addrman is valid
    assert_eq!(am.deserialize(&empty_file()), Ok(()));
    assert_eq!(size(&am), 0);

    // Check that addrman with negative lowest_compatible cannot be read
    assert_eq!(
        am.deserialize(&serialize_addrman(1, -32, MAGIC, 1, 0, 0, None)),
        Err(LoadError::Corrupt(
            "Corrupted addrman database: The compat value (0) is lower than the expected \
             minimum value 32.: iostream error"
                .into()
        ))
    );

    // Check that addrman from future is overwritten with new addrman
    let err = am.deserialize(&serialize_addrman(1, 111, MAGIC, 1, 0, 0, None)).unwrap_err();
    assert_eq!(
        err,
        LoadError::Incompatible {
            format: 1,
            lowest_compatible: 111
        }
    );
    assert!(err.is_unsupported_version());
    assert_eq!(
        err.to_string(),
        "Unsupported format of addrman database: 1. It is compatible with formats >=111, but the \
         maximum supported by this version of Dash Core is 4.: iostream error"
    );

    // Check that corrupt addrman cannot be read (EOF)
    let f = empty_file();
    assert_eq!(
        am.deserialize(&f[..f.len() - 1]),
        Err(LoadError::Corrupt("AutoFile::read: end of file: iostream error".into()))
    );

    // Check that corrupt addrman cannot be read (magic)
    assert_eq!(
        am.deserialize(&serialize_addrman(1, 4, [0xe2, 0xca, 0xff, 0xce], 1, 0, 0, None)),
        Err(LoadError::Corrupt("Invalid network magic number".into()))
    );

    // Check that corrupt addrman cannot be read (checksum)
    assert_eq!(
        am.deserialize(&serialize_addrman(1, 4, MAGIC, 1, 0, 0, Some([b'a'; 32]))),
        Err(LoadError::Corrupt("Checksum mismatch, data corrupted".into()))
    );

    // Check that corrupt addrman cannot be read (len_tried)
    for (len_new, len_tried, text) in [
        (0, -1, "nTried=-1, should be in [0, 16384]"),
        (0, 16385, "nTried=16385, should be in [0, 16384]"),
        (-1, 0, "nNew=-1, should be in [0, 65536]"),
        (65537, 0, "nNew=65537, should be in [0, 65536]"),
    ] {
        assert_eq!(
            am.deserialize(&serialize_addrman(1, 4, MAGIC, 1, len_new, len_tried, None)),
            Err(LoadError::Corrupt(format!(
                "Corrupt AddrMan serialization: {text}: iostream error"
            )))
        );
    }

    // Check that corrupt addrman cannot be read (failed check)
    let err = am.deserialize(&serialize_addrman(1, 4, MAGIC, 0, 0, 0, None)).unwrap_err();
    assert_eq!(err, LoadError::Inconsistent(-16));
    assert_eq!(err.to_string(), "Corrupt data. Consistency check failed with code -16");

    // Error-class order (P2a §4.1): the consistency check runs before the checksum, so a file
    // that fails both is an inconsistency (silently reset), not a fatal checksum error.
    assert_eq!(
        am.deserialize(&serialize_addrman(1, 4, MAGIC, 0, 0, 0, Some([b'a'; 32]))),
        Err(LoadError::Inconsistent(-16))
    );
    // ...and so is the version check.
    assert_eq!(
        am.deserialize(&serialize_addrman(1, 111, MAGIC, 1, 0, 0, Some([b'a'; 32]))),
        Err(LoadError::Incompatible {
            format: 1,
            lowest_compatible: 111
        })
    );
    // The magic comes first of all.
    assert_eq!(
        am.deserialize(&serialize_addrman(1, 111, [0; 4], 0, 0, 0, Some([b'a'; 32]))),
        Err(LoadError::Corrupt("Invalid network magic number".into()))
    );
    // Bytes after the checksum are ignored.
    let mut f = empty_file();
    f.extend_from_slice(b"trailing");
    assert_eq!(am.deserialize(&f), Ok(()));
}

#[test]
fn failed_load_keeps_contents() {
    let am = addrman();
    let a = svc("250.1.1.1", 8333);
    am.add(&[caddr(&a)], &ip("252.2.2.2"), 0, NOW);
    assert!(am.deserialize(&serialize_addrman(1, 4, MAGIC, 0, 0, 0, None)).is_err());
    assert_eq!(size(&am), 1);
    am.clear();
    assert_eq!(size(&am), 0);
}

// ---- Port-specific checks ------------------------------------------------------------------

#[test]
fn chance_table_is_glibc_pow() {
    let mut x = 1.0f64;
    for n in 0..=8 {
        // Exactly rounded powers: the table and repeated multiplication agree up to n = 3.
        if n <= 3 {
            assert_eq!(pow_066(n), x, "n = {n}");
        }
        x *= 0.66;
    }
    assert_eq!(pow_066(1).to_bits(), 0x3fe5_1eb8_51eb_851f);
    assert_eq!(pow_066(8).to_bits(), 0x3fa2_6f1f_cdd5_02a3);
    // A negative nAttempts read from disk raises the chance above 1 (core m-9).
    assert!(pow_066(-1) > 1.0);
    assert_eq!(pow_066(i32::MIN), f64::INFINITY);
}

/// glibc 2.43's `pow(0.66, n)` for negative `n` (Core keeps a negative `nAttempts`).
#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[test]
fn chance_negative_attempts_match_glibc() {
    for (n, bits) in [
        (-1, 0x3ff8_3e0f_83e0_f83eu64),
        (-2, 0x4002_5d8f_a1f8_01e1),
        (-8, 0x403b_c64f_2156_0e34),
        (-100, 0x43ae_d42c_88d6_1fd7),
        (-1000, 0x6566_0a4f_8f24_15d5),
        (-1700, 0x7fa0_fa24_7008_65a2),
        (-1740, 0x7ff0_0000_0000_0000),
    ] {
        assert_eq!(pow_066(n).to_bits(), bits, "n = {n}");
    }
}

#[test]
fn hash_writer_cheap_hash() {
    // GetCheapHash = ReadLE64(SHA256d(...)).
    let h = dash_sha256::sha256d(b"abc");
    assert_eq!(
        HashWriter::new().raw(b"abc").cheap(),
        u64::from_le_bytes(h[..8].try_into().unwrap())
    );
}

/// `-checkaddrman=1` never draws (`randrange(1)`), a larger ratio draws on every call. (With
/// ratio 2 the 1-bit draws can land in bits a later refill would discard anyway, so the select
/// stream may resynchronise; ratio 3 draws 2 bits with rejection and does not here.)
#[test]
fn check_ratio_draws() {
    let run = |ratio: u32| {
        let am = AddrMan::new(empty_netgroup(), true, ratio, false, MAGIC);
        let source = ip("252.2.2.2");
        for i in 1..40 {
            am.add(&[caddr(&svc(&format!("250.1.{i}.1"), 8333))], &source, 0, NOW);
        }
        (0..20).map(|_| am.select(false, None, NOW).unwrap().addr).collect::<Vec<_>>()
    };
    assert_eq!(run(0), run(1));
    assert_ne!(run(0), run(3));
}

fn populated(netgroup: NetGroupManager) -> AddrMan {
    let am = addrman_with(netgroup, true);
    for i in 0u32..600 {
        let s = format!("{}.{}.{}.7", 1 + i % 200, i / 200, i % 7);
        let src = ip(&format!("{}.{}.3.3", 2 + i % 13, i % 5));
        let addr = caddr_at(&svc(&s, 9999 - (i % 3) as u16), NOW - i64::from(i));
        am.add(std::slice::from_ref(&addr), &src, 0, NOW);
        if i % 5 == 0 {
            am.good(&addr.service, NOW);
        }
        if i % 7 == 0 {
            am.attempt(&addr.service, true, NOW - 100);
        }
    }
    // Extra references for some entries.
    for i in 0u32..300 {
        let s = format!("{}.{}.{}.7", 1 + i % 200, i / 200, i % 7);
        let src = ip(&format!("{}.9.3.3", 40 + i % 50));
        am.add(&[caddr_at(&svc(&s, 9999 - (i % 3) as u16), NOW)], &src, 0, NOW);
    }
    am
}

/// The bucket layout survives a round trip, keyed by address (ids are renumbered on load).
fn layout(am: &AddrMan) -> BTreeSet<(bool, i32, i32, String)> {
    let c = am.canonical();
    let name: std::collections::HashMap<i32, String> =
        c.entries.iter().map(|(id, e)| (*id, e.service.to_string())).collect();
    let mut out = BTreeSet::new();
    for (i, &id) in c.new_table.iter().enumerate() {
        if id != -1 {
            out.insert((false, (i / 64) as i32, (i % 64) as i32, name[&id].clone()));
        }
    }
    for (i, &id) in c.tried_table.iter().enumerate() {
        if id != -1 {
            out.insert((true, (i / 64) as i32, (i % 64) as i32, name[&id].clone()));
        }
    }
    out
}

#[test]
fn serialization_round_trip() {
    for ng in [empty_netgroup(), asmap_netgroup()] {
        let am = populated(ng.clone());
        assert_eq!(am.check(), 0);
        let file = am.serialize();
        let loaded = addrman_with(ng, false);
        loaded.deserialize(&file).unwrap();
        assert_eq!(loaded.check(), 0);
        assert_eq!(layout(&am), layout(&loaded));
        assert_eq!(size(&am), size(&loaded));
        assert_eq!(am.size(None, Some(false)), loaded.size(None, Some(false)));
        let entry = |m: &AddrMan| {
            let mut v: Vec<_> = m
                .canonical()
                .entries
                .into_iter()
                .map(|(_, e)| {
                    (e.service.to_string(), e.time, e.services, e.last_success, e.attempts)
                })
                .collect();
            v.sort();
            v
        };
        assert_eq!(entry(&am), entry(&loaded));
        assert_eq!(am.canonical().key, loaded.canonical().key);
    }
}

#[test]
fn serialize_layout_and_header() {
    let am = addrman();
    let a = caddr(&svc("250.1.1.1", 8333));
    am.add(&[a], &ip("252.2.2.2"), 0, NOW);
    let f = am.serialize();
    assert_eq!(&f[..4], &MAGIC);
    assert_eq!(f[4], 4); // V4_MULTIPORT
    assert_eq!(f[5], 32 + 4);
    assert_eq!(f[6], 1); // nKey = uint256{1}
    assert!(f[7..38].iter().all(|&b| b == 0));
    assert_eq!(&f[38..42], &1i32.to_le_bytes());
    assert_eq!(&f[42..46], &0i32.to_le_bytes());
    assert_eq!(&f[46..50], &(1024i32 ^ (1 << 30)).to_le_bytes());
    // The entry: version word 220000 | ADDRV2, nTime, CompactSize services, BIP155 address,
    // big-endian port, BIP155 source, m_last_success, nAttempts.
    let mut entry = (220_000u32 | (1 << 29)).to_le_bytes().to_vec();
    entry.extend_from_slice(&(TIME_INIT as u32).to_le_bytes());
    entry.extend_from_slice(&[0, 1, 4, 250, 1, 1, 1, 0x20, 0x8d, 1, 4, 252, 2, 2, 2]);
    entry.extend_from_slice(&[0; 12]);
    assert_eq!(&f[50..50 + entry.len()], &entry[..]);
    // 1024 bucket sizes, one index, a zero asmap checksum, then SHA-256d.
    assert_eq!(f.len(), 50 + entry.len() + 1024 * 4 + 4 + 32 + 32);
    assert!(f[f.len() - 64..f.len() - 32].iter().all(|&b| b == 0));
    assert_eq!(&f[f.len() - 32..], &dash_sha256::sha256d(&f[..f.len() - 32]));
}

/// A tried entry without a success fails `CheckAddrman` with −1 after the load.
#[test]
fn load_tried_without_success_is_inconsistent() {
    let am = addrman();
    let a = svc("250.1.1.1", 8333);
    am.add(&[caddr(&a)], &ip("252.2.2.2"), 0, NOW);
    am.good(&a, NOW);
    let mut f = am.serialize();
    // m_last_success sits 12 bytes before the end of the entry (then nAttempts), which ends
    // before the 1024 bucket sizes, the asmap checksum and the file checksum.
    let end = f.len() - 32 - 32 - 1024 * 4;
    f[end - 12..end - 4].copy_from_slice(&0i64.to_le_bytes());
    rehash(&mut f);
    assert_eq!(addrman().deserialize(&f), Err(LoadError::Inconsistent(-1)));
}

/// Format 2 files (v1 addresses, no BIP155) load; a bucket count other than 1024 re-buckets by
/// source; an addrv2 version word is refused without `ADDRV2_FORMAT` in the stream.
#[test]
fn load_pre_bip155_format() {
    let mut s = MAGIC.to_vec();
    s.push(2);
    s.push(32);
    let mut key = [0u8; 32];
    key[0] = 1;
    s.extend_from_slice(&key);
    s.extend_from_slice(&1i32.to_le_bytes());
    s.extend_from_slice(&0i32.to_le_bytes());
    s.extend_from_slice(&(1023i32 ^ (1 << 30)).to_le_bytes());
    let entry_at = s.len();
    s.extend_from_slice(&220_000u32.to_le_bytes());
    s.extend_from_slice(&(NOW as u32).to_le_bytes());
    s.extend_from_slice(&1u64.to_le_bytes());
    s.extend_from_slice(&ip("250.1.1.1").v1_bytes());
    s.extend_from_slice(&8333u16.to_be_bytes());
    s.extend_from_slice(&ip("252.2.2.2").v1_bytes());
    s.extend_from_slice(&0i64.to_le_bytes());
    s.extend_from_slice(&0i32.to_le_bytes());
    // 1023 buckets: the entry listed once in bucket 0, then the (zero) asmap checksum.
    s.extend_from_slice(&1i32.to_le_bytes());
    s.extend_from_slice(&0i32.to_le_bytes());
    for _ in 1..1023 {
        s.extend_from_slice(&0i32.to_le_bytes());
    }
    s.extend_from_slice(&[0; 32]);
    let mut f = s.clone();
    f.extend_from_slice(&dash_sha256::sha256d(&s));
    let am = addrman();
    am.deserialize(&f).unwrap();
    assert_eq!(size(&am), 1);
    let service = svc("250.1.1.1", 8333);
    let pos = am.find_address_entry(&service).unwrap();
    assert_eq!(pos.multiplicity, 1);
    // Re-bucketed into the source's bucket, not bucket 0.
    let c = am.canonical();
    assert_eq!(c.new_table[(pos.bucket * 64 + pos.position) as usize], 0);
    assert_ne!(pos.bucket, 0);
    assert_eq!(am.get_address_info(&service).unwrap().services, ServiceFlags(1));

    let mut g = s.clone();
    g[entry_at + 3] |= 0x20; // DISK_VERSION_ADDRV2 without ADDRV2_FORMAT in the stream
    g.extend_from_slice(&dash_sha256::sha256d(&s));
    assert_eq!(
        am.deserialize(&g),
        Err(LoadError::Corrupt("Unsupported CAddress disk format version: iostream error".into()))
    );
}

#[test]
fn bip155_decode_rules() {
    // An IPv4 address with the wrong BIP155 length.
    let file = {
        let good = addrman();
        good.add(&[caddr(&svc("250.1.1.1", 8333))], &ip("252.2.2.2"), 0, NOW);
        good.serialize()
    };
    let pos = find(&file, &[1, 4, 250, 1, 1, 1]);
    let mut bad = file.clone();
    bad[pos + 1] = 5;
    rehash(&mut bad);
    assert_eq!(
        addrman().deserialize(&bad),
        Err(LoadError::Corrupt(
            "BIP155 IPv4 address with length 5 (should be 4): iostream error".into()
        ))
    );
    // An unknown network id is skipped and read as an invalid address (then pruned).
    let mut unknown = file.clone();
    unknown[pos] = 0x99;
    rehash(&mut unknown);
    let am2 = addrman();
    am2.deserialize(&unknown).unwrap();
    assert_eq!(size(&am2), 0);
    // A non-canonical CompactSize.
    let mut nc = file[..pos + 1].to_vec();
    nc.extend_from_slice(&[253, 4, 0]);
    nc.extend_from_slice(&file[pos + 2..]);
    rehash(&mut nc);
    assert_eq!(
        addrman().deserialize(&nc),
        Err(LoadError::Corrupt("non-canonical ReadCompactSize(): iostream error".into()))
    );
}

// ---- Golden traces from Dash Core ----------------------------------------------------------
//
// `tests/data/core_addrman_gen.cpp`, linked against a v24.0.0-rc.3 build (libstdc++), ran the
// operations below on a deterministic AddrMan with `-checkaddrman=100` and recorded every
// output (`core_addrman_trace.txt`), the `peers.dat` it dumped (`core_peers.dat`) and the
// checksums of three more dumps: after reloading that file, after reloading it with an asmap
// (re-bucketing), and after a pruning load (five new entries patched to 0.0.0.0). Matching
// them checks the bucket math, every RNG draw (including `Check()`'s), the libstdc++ `mapInfo`
// order of `Serialize` and of the prune loop's `Delete`s, and the file bytes.

const CORE_PEERS: &[u8] = include_bytes!("../../tests/data/core_peers.dat");
const CORE_TRACE: &str = include_str!("../../tests/data/core_addrman_trace.txt");

fn core_ops_host(i: u32) -> String {
    let first = if i.is_multiple_of(4) {
        "250".to_string()
    } else {
        (1 + i % 200).to_string()
    };
    format!("{first}.{}.{}.7", i / 4 % 256, i % 7)
}

fn trace(am: &AddrMan, tag: &str, out: &mut Vec<String>) {
    for i in 0..12 {
        let (a, t) = am
            .select(i % 3 == 0, None, NOW)
            .map(|s| (s.addr.service.to_string(), s.last_try))
            .unwrap_or_else(|| ("[::]:0".into(), 0));
        out.push(format!("{tag} select {a} {t}"));
    }
    let a4 = am
        .select(false, Some(Network::Ipv4), NOW)
        .map(|s| s.addr.service.to_string())
        .unwrap_or_else(|| "[::]:0".into());
    out.push(format!("{tag} select4 {a4}"));
    for a in am.get_addr(40, 23, None, true, NOW) {
        out.push(format!("{tag} getaddr {} {} {}", a.service, a.time, a.services.0));
    }
    out.push(format!(
        "{tag} size {} {} {}",
        am.size(None, None),
        am.size(None, Some(true)),
        am.size(None, Some(false))
    ));
}

fn dump_line(name: &str, file: &[u8]) -> String {
    let hex: String = file[file.len() - 32..].iter().map(|b| format!("{b:02x}")).collect();
    format!("dump {name} {hex}")
}

#[test]
fn core_golden_trace() {
    let mut out = vec![];
    let am = addrman();
    for i in 0u32..160 {
        let services = if i % 2 == 1 {
            ServiceFlags::NODE_NETWORK
        } else {
            ServiceFlags::NONE
        };
        let addr = Address {
            service: svc(&core_ops_host(i), 9999 - (i % 3) as u16),
            services,
            time: (NOW - i64::from(i)) as u32,
        };
        let src = ip(&format!("{}.{}.3.3", 2 + i % 13, i % 5));
        let penalty = if i % 4 == 1 {
            7200
        } else {
            0
        };
        am.add(std::slice::from_ref(&addr), &src, penalty, NOW);
        if i % 5 == 0 {
            am.good(&addr.service, NOW);
        }
        if i % 7 == 0 {
            am.attempt(&addr.service, true, NOW - 100);
        }
    }
    for i in 0u32..100 {
        let addr = caddr_at(&svc(&core_ops_host(i), 9999 - (i % 3) as u16), NOW + i64::from(i));
        am.add(&[addr], &ip(&format!("{}.9.3.3", 40 + i % 50)), 0, NOW);
    }
    am.resolve_collisions(NOW);
    let c = am.select_tried_collision().map(|s| s.addr.service.to_string());
    out.push(format!("ops collision {}", c.unwrap_or_else(|| "[::]:0".into())));
    trace(&am, "ops", &mut out);
    let file = am.serialize();
    assert_eq!(file, CORE_PEERS, "peers.dat bytes differ from Core's");

    let reload = addrman();
    reload.deserialize(CORE_PEERS).unwrap();
    trace(&reload, "reload", &mut out);
    let reload_file = reload.serialize();

    let asmapped = addrman_with(asmap_netgroup(), true);
    asmapped.deserialize(CORE_PEERS).unwrap();
    trace(&asmapped, "asmap", &mut out);
    let asmap_file = asmapped.serialize();

    let mut pruned_in = CORE_PEERS.to_vec();
    for k in [3usize, 10, 50, 90, 125] {
        let at = 50 + 35 * k + 11;
        pruned_in[at..at + 4].copy_from_slice(&[0, 0, 0, 0]);
    }
    rehash(&mut pruned_in);
    let pruned = addrman();
    pruned.deserialize(&pruned_in).unwrap();
    trace(&pruned, "pruned", &mut out);
    let pruned_file = pruned.serialize();

    out.push(dump_line("core_peers", &file));
    out.push(dump_line("core_peers_reload", &reload_file));
    out.push(dump_line("core_peers_asmap", &asmap_file));
    out.push(dump_line("core_peers_pruned", &pruned_file));

    let expected: Vec<&str> = CORE_TRACE.lines().collect();
    for (i, (got, want)) in out.iter().zip(&expected).enumerate() {
        assert_eq!(got, want, "trace line {}", i + 1);
    }
    assert_eq!(out.len(), expected.len());
}

/// `nAttempts` is read from `peers.dat` unchecked; Core's `++nAttempts` wraps at INT_MAX (and
/// `GetChance`/`IsTerrible` already cope with the negative result), so the port must not panic.
#[test]
fn attempt_counter_wraps() {
    let am = addrman();
    let source = ip("252.2.2.2");
    let addr = svc("250.1.1.1", 8333);
    assert!(am.add(&[caddr(&addr)], &source, 0, NOW));
    {
        let mut m = am.inner.lock();
        let id = m.find(&addr).unwrap();
        m.info_mut(id).attempts = i32::MAX;
    }
    // m_last_count_attempt (0) < m_last_good (1): the failure is counted.
    am.attempt(&addr, true, NOW);
    assert_eq!(am.get_address_info(&addr).unwrap().attempts, i32::MIN);
    assert!(am.select(false, None, NOW).is_some());
}

/// `anchors.dat`: `SerializeDB(magic, std::vector<CAddress>)` with `ADDRV2_FORMAT` (addrdb.cpp:
/// 233-250), laid out byte by byte, then read back; a wrong magic, a bad checksum or a cut image
/// is an error (Core's `ReadAnchors` then starts without anchors).
#[test]
fn anchors_image() {
    let anchors = [
        Address {
            service: svc("1.2.3.4", 9999),
            services: ServiceFlags(0x405),
            time: 1_700_000_123,
        },
        Address {
            service: svc("2001:db8::1", 19999),
            services: ServiceFlags::NODE_NETWORK,
            time: 7,
        },
    ];
    let bytes = write_anchors(&MAGIC, &anchors);
    let mut expected = MAGIC.to_vec();
    expected.push(2);
    for (a, net_and_len) in anchors.iter().zip([[1u8, 4], [2, 16]]) {
        expected.extend_from_slice(&(220_000u32 | 1 << 29).to_le_bytes());
        expected.extend_from_slice(&a.time.to_le_bytes());
        expected.extend_from_slice(match a.services.0 {
            0x405 => &[0xfd, 0x05, 0x04][..],
            _ => &[0x01][..],
        });
        expected.extend_from_slice(&net_and_len);
        expected.extend_from_slice(a.service.addr.bytes());
        expected.extend_from_slice(&a.service.port.to_be_bytes());
    }
    let hash = dash_sha256::sha256d(&expected);
    expected.extend_from_slice(&hash);
    assert_eq!(bytes, expected);
    assert_eq!(read_anchors(&MAGIC, &bytes).unwrap(), anchors);
    assert_eq!(read_anchors(&MAGIC, &write_anchors(&MAGIC, &[])).unwrap(), []);

    let mut bad = bytes.clone();
    *bad.last_mut().unwrap() ^= 1;
    assert!(read_anchors(&MAGIC, &bad).is_err());
    assert!(read_anchors(&[0xfc, 0xc1, 0xb7, 0xdc], &bytes).is_err());
    assert!(read_anchors(&MAGIC, &bytes[..bytes.len() - 33]).is_err());
}
