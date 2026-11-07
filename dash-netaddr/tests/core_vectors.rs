//! Core's own vectors for what dash-netaddr ports, at v24.0.0-rc.3: `src/test/netbase_tests.cpp`
//! (netbase_networks, netbase_properties, netbase_splithost, netbase_lookupnumeric,
//! embedded_test, isbadport, netbase_dont_resolve_strings_with_embedded_nul_characters) and
//! `src/test/net_tests.cpp` (cnetaddr_basic, cnetaddr_tostring_canonical_ipv6). Core's
//! `ResolveIP(s)` is `LookupHost(s, false)`, which strips one pair of brackets and then runs
//! `LookupIntern`; `TestParse(s, canon)` is `LookupNumeric(s, 65535).ToStringAddrPort()`.

use dash_netaddr::{
    NetAddr, Network, Service, is_bad_port, lookup_host_numeric, lookup_numeric,
    maybe_flip_ipv6_to_cjdns, split_host_port, split_host_port_valid,
};

/// Core's `LookupHost(name, false)` (netbase.cpp:176-186), or the default address.
fn resolve_ip(name: &str) -> NetAddr {
    let host = name.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(name);
    lookup_host_numeric(host).unwrap_or_default()
}

const TORV3: &str = "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd.onion";

// Core: test/netbase_tests.cpp:38-46 netbase_networks
#[test]
fn netbase_networks() {
    assert_eq!(resolve_ip("127.0.0.1").network(false), Network::Unroutable);
    assert_eq!(resolve_ip("::1").network(false), Network::Unroutable);
    assert_eq!(resolve_ip("8.8.8.8").network(false), Network::Ipv4);
    assert_eq!(resolve_ip("2001::8888").network(false), Network::Ipv6);
    assert_eq!(resolve_ip(TORV3).network(false), Network::Onion);
    assert_eq!(NetAddr::internal("foo.com").unwrap().network(false), Network::Internal);
}

// Core: test/netbase_tests.cpp:48-85 netbase_properties
#[test]
fn netbase_properties() {
    assert!(resolve_ip("127.0.0.1").is_ipv4());
    assert!(resolve_ip("::FFFF:192.168.1.1").is_ipv4());
    assert!(resolve_ip("::1").is_ipv6());
    assert!(resolve_ip("10.0.0.1").is_rfc1918());
    assert!(resolve_ip("192.168.1.1").is_rfc1918());
    assert!(resolve_ip("172.31.255.255").is_rfc1918());
    assert!(resolve_ip("198.18.0.0").is_rfc2544());
    assert!(resolve_ip("198.19.255.255").is_rfc2544());
    assert!(resolve_ip("2001:0DB8::").is_rfc3849());
    assert!(resolve_ip("169.254.1.1").is_rfc3927());
    assert!(resolve_ip("2002::1").is_rfc3964());
    assert!(resolve_ip("FC00::").is_rfc4193());
    assert!(resolve_ip("2001::2").is_rfc4380());
    assert!(resolve_ip("2001:10::").is_rfc4843());
    assert!(resolve_ip("2001:20::").is_rfc7343());
    assert!(resolve_ip("FE80::").is_rfc4862());
    assert!(resolve_ip("64:FF9B::").is_rfc6052());
    assert!(resolve_ip(TORV3).is_tor());
    assert!(resolve_ip("127.0.0.1").is_local());
    assert!(resolve_ip("::1").is_local());
    assert!(resolve_ip("8.8.8.8").is_routable(false));
    assert!(resolve_ip("2001::1").is_routable(false));
    assert!(resolve_ip("127.0.0.1").is_valid());
    assert!(NetAddr::internal("FD6B:88C0:8724:edb1:8e4:3588:e546:35ca").unwrap().is_internal());
    assert!(NetAddr::internal("bar.com").unwrap().is_internal());
}

// Core: test/netbase_tests.cpp:87-127 netbase_splithost
#[test]
fn netbase_splithost() {
    for (input, host, port, valid) in [
        ("www.bitcoin.org", "www.bitcoin.org", 0, true),
        ("[www.bitcoin.org]", "www.bitcoin.org", 0, true),
        ("www.bitcoin.org:80", "www.bitcoin.org", 80, true),
        ("[www.bitcoin.org]:80", "www.bitcoin.org", 80, true),
        ("127.0.0.1", "127.0.0.1", 0, true),
        ("127.0.0.1:9999", "127.0.0.1", 9999, true),
        ("[127.0.0.1]", "127.0.0.1", 0, true),
        ("[127.0.0.1]:9999", "127.0.0.1", 9999, true),
        ("::ffff:127.0.0.1", "::ffff:127.0.0.1", 0, true),
        ("[::ffff:127.0.0.1]:9999", "::ffff:127.0.0.1", 9999, true),
        ("[::]:9999", "::", 9999, true),
        ("::9999", "::9999", 0, true),
        (":9999", "", 9999, true),
        ("[]:9999", "", 9999, true),
        ("", "", 0, true),
        (":65535", "", 65535, true),
        (":65536", ":65536", 0, false),
        (":-1", ":-1", 0, false),
        ("[]:70001", "[]:70001", 0, false),
        ("[]:-1", "[]:-1", 0, false),
        ("[]:-0", "[]:-0", 0, false),
        ("[]:0", "", 0, false),
        ("[]:1/2", "[]:1/2", 0, false),
        ("[]:1E2", "[]:1E2", 0, false),
        ("127.0.0.1:65536", "127.0.0.1:65536", 0, false),
        ("127.0.0.1:0", "127.0.0.1", 0, false),
        ("127.0.0.1:", "127.0.0.1:", 0, false),
        ("127.0.0.1:1/2", "127.0.0.1:1/2", 0, false),
        ("127.0.0.1:1E2", "127.0.0.1:1E2", 0, false),
        ("www.bitcoincore.org:65536", "www.bitcoincore.org:65536", 0, false),
        ("www.bitcoincore.org:0", "www.bitcoincore.org", 0, false),
        ("www.bitcoincore.org:", "www.bitcoincore.org:", 0, false),
    ] {
        assert_eq!(split_host_port(input, 0), (host, port), "{input:?}");
        assert_eq!(split_host_port_valid(input), valid, "{input:?}");
    }
}

// Core: test/netbase_tests.cpp:129-143 netbase_lookupnumeric
#[test]
fn netbase_lookupnumeric() {
    for (src, canon) in [
        ("127.0.0.1", "127.0.0.1:65535"),
        ("127.0.0.1:9999", "127.0.0.1:9999"),
        ("::ffff:127.0.0.1", "127.0.0.1:65535"),
        ("::", "[::]:65535"),
        ("[::]:9999", "[::]:9999"),
        ("[127.0.0.1]", "127.0.0.1:65535"),
        (":::", "[::]:0"),
        // An internal address fails to resolve, a one-off resolves.
        ("[fd6b:88c0:8724:1:2:3:4:5]", "[::]:0"),
        ("[fd6c:88c0:8724:1:2:3:4:5]", "[fd6c:88c0:8724:1:2:3:4:5]:65535"),
    ] {
        assert_eq!(lookup_numeric(src, 65535).to_string(), canon, "{src:?}");
    }
}

// Core: test/netbase_tests.cpp:145-151 embedded_test
#[test]
fn embedded_test() {
    let addr1 = resolve_ip("1.2.3.4");
    let addr2 = resolve_ip("::FFFF:0102:0304");
    assert!(addr2.is_ipv4());
    assert_eq!(addr1.to_string(), addr2.to_string());
}

// Core: test/netbase_tests.cpp:447-476 isbadport
#[test]
fn isbadport() {
    for p in [1, 22, 6000, 80, 443, 8332, 8333, 18332, 18333] {
        assert!(is_bad_port(p), "{p}");
    }
    for p in [0, 9998, 9999, 26656] {
        assert!(!is_bad_port(p), "{p}");
    }
    let total = (1..=u16::MAX).filter(|&p| is_bad_port(p)).count();
    assert_eq!(total - 1023, 21);
}

// Core: test/netbase_tests.cpp:602-610 (the LookupHost half)
#[test]
fn dont_resolve_strings_with_embedded_nul_characters() {
    assert!(lookup_host_numeric("127.0.0.1").is_some());
    for s in ["127.0.0.1\0", "127.0.0.1\0example.com", "127.0.0.1\0example.com\0"] {
        assert!(lookup_host_numeric(s).is_none(), "{s:?}");
        assert_eq!(lookup_numeric(s, 1), Service::default(), "{s:?}");
    }
}

// Core: test/net_tests.cpp:484-623 cnetaddr_basic (the scoped rows are D36: not modelled)
#[test]
fn cnetaddr_basic() {
    let addr = resolve_ip("0.0.0.0");
    assert!(!addr.is_valid() && addr.is_ipv4() && addr.is_bind_any());
    assert_eq!(addr.to_string(), "0.0.0.0");
    let addr = resolve_ip("255.255.255.255");
    assert!(!addr.is_valid() && addr.is_ipv4() && !addr.is_bind_any());
    assert_eq!(addr.to_string(), "255.255.255.255");
    let addr = resolve_ip("12.34.56.78");
    assert!(addr.is_valid() && addr.is_ipv4());
    assert_eq!(addr.to_string(), "12.34.56.78");
    let addr = resolve_ip("::");
    assert!(!addr.is_valid() && addr.is_ipv6() && addr.is_bind_any());
    assert_eq!(addr.to_string(), "::");
    let addr = resolve_ip("1122:3344:5566:7788:9900:aabb:ccdd:eeff");
    assert!(addr.is_valid() && addr.is_ipv6() && addr.is_addr_v1_compatible());
    assert_eq!(addr.to_string(), "1122:3344:5566:7788:9900:aabb:ccdd:eeff");

    assert!(NetAddr::set_special("6hzph5hv6337r6p2.onion").is_none());
    let tor = NetAddr::set_special(TORV3).unwrap();
    assert!(tor.is_valid() && tor.is_tor() && !tor.is_i2p() && !tor.is_addr_v1_compatible());
    assert_eq!(tor.to_string(), TORV3);
    for bad in [
        "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscsad.onion",
        "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscrye.onion",
        "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd\0wtf.onion",
        "mfrggzak.onion",
        "mf*g zak.onion",
    ] {
        assert!(NetAddr::set_special(bad).is_none(), "{bad:?}");
    }

    let i2p_in = "UDHDrtrcetjm5sxzskjyr5ztpeszydbh4dpl3pl4utgqqw2v4jna.b32.I2P";
    let i2p = NetAddr::set_special(i2p_in).unwrap();
    assert!(i2p.is_valid() && i2p.is_i2p() && !i2p.is_tor() && !i2p.is_addr_v1_compatible());
    assert_eq!(i2p.to_string(), i2p_in.to_lowercase());
    for bad in [
        "udhdrtrcetjm5sxzskjyr5ztpeszydbh4dpl3pl4utgqqw2v4jn=.b32.i2p",
        "udhdrtrcetjm5sxzskjyr5ztpeszydbh4dpl3pl4utgqqw2v4jna=.b32.i2p",
        "udhdrtrcetjm5sxzskjyr5ztpeszydbh4dpl3pl4utgqqw2v\0wtf.b32.i2p",
        "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscsad.b32.i2p",
        "tp*szydbh4dp.b32.i2p",
        "totally bogus",
    ] {
        assert!(NetAddr::set_special(bad).is_none(), "{bad:?}");
    }

    let internal = NetAddr::internal("esffpp").unwrap();
    assert!(!internal.is_valid() && internal.is_internal() && internal.is_addr_v1_compatible());
    assert_eq!(internal.to_string(), "esffpvrt3wpeaygy.internal");
}

// Core: test/net_tests.cpp:625-676 cnetaddr_tostring_canonical_ipv6
#[test]
fn cnetaddr_tostring_canonical_ipv6() {
    for (input, canonical) in [
        ("0000:0000:0000:0000:0000:0000:0000:0000", "::"),
        ("000:0000:000:00:0:00:000:0000", "::"),
        ("000:000:000:000:000:000:000:000", "::"),
        ("00:00:00:00:00:00:00:00", "::"),
        ("0:0:0:0:0:0:0:0", "::"),
        ("0:0:0:0:0:0:0:1", "::1"),
        ("2001:0:0:1:0:0:0:1", "2001:0:0:1::1"),
        ("2001:0db8:0:0:1:0:0:1", "2001:db8::1:0:0:1"),
        ("2001:0db8:85a3:0000:0000:8a2e:0370:7334", "2001:db8:85a3::8a2e:370:7334"),
        ("2001:0db8::0001", "2001:db8::1"),
        ("2001:0db8::0001:0000", "2001:db8::1:0"),
        ("2001:0db8::1:0:0:1", "2001:db8::1:0:0:1"),
        ("2001:db8:0000:0:1::1", "2001:db8::1:0:0:1"),
        ("2001:db8:0000:1:1:1:1:1", "2001:db8:0:1:1:1:1:1"),
        ("2001:db8:0:0:0:0:2:1", "2001:db8::2:1"),
        ("2001:db8:0:0:0::1", "2001:db8::1"),
        ("2001:db8:0:0:1:0:0:1", "2001:db8::1:0:0:1"),
        ("2001:db8:0:0:1::1", "2001:db8::1:0:0:1"),
        ("2001:DB8:0:0:1::1", "2001:db8::1:0:0:1"),
        ("2001:db8:0:0::1", "2001:db8::1"),
        ("2001:db8:0:0:aaaa::1", "2001:db8::aaaa:0:0:1"),
        ("2001:db8:0:1:1:1:1:1", "2001:db8:0:1:1:1:1:1"),
        ("2001:db8:0::1", "2001:db8::1"),
        ("2001:db8:85a3:0:0:8a2e:370:7334", "2001:db8:85a3::8a2e:370:7334"),
        ("2001:db8::0:1", "2001:db8::1"),
        ("2001:db8::0:1:0:0:1", "2001:db8::1:0:0:1"),
        ("2001:DB8::1", "2001:db8::1"),
        ("2001:db8::1", "2001:db8::1"),
        ("2001:db8::1:0:0:1", "2001:db8::1:0:0:1"),
        ("2001:db8::1:1:1:1:1", "2001:db8:0:1:1:1:1:1"),
        ("2001:db8::aaaa:0:0:1", "2001:db8::aaaa:0:0:1"),
        ("2001:db8:aaaa:bbbb:cccc:dddd:0:1", "2001:db8:aaaa:bbbb:cccc:dddd:0:1"),
        ("2001:db8:aaaa:bbbb:cccc:dddd::1", "2001:db8:aaaa:bbbb:cccc:dddd:0:1"),
        ("2001:db8:aaaa:bbbb:cccc:dddd:eeee:0001", "2001:db8:aaaa:bbbb:cccc:dddd:eeee:1"),
        ("2001:db8:aaaa:bbbb:cccc:dddd:eeee:001", "2001:db8:aaaa:bbbb:cccc:dddd:eeee:1"),
        ("2001:db8:aaaa:bbbb:cccc:dddd:eeee:01", "2001:db8:aaaa:bbbb:cccc:dddd:eeee:1"),
        ("2001:db8:aaaa:bbbb:cccc:dddd:eeee:1", "2001:db8:aaaa:bbbb:cccc:dddd:eeee:1"),
        ("2001:db8:aaaa:bbbb:cccc:dddd:eeee:aaaa", "2001:db8:aaaa:bbbb:cccc:dddd:eeee:aaaa"),
        ("2001:db8:aaaa:bbbb:cccc:dddd:eeee:AAAA", "2001:db8:aaaa:bbbb:cccc:dddd:eeee:aaaa"),
        ("2001:db8:aaaa:bbbb:cccc:dddd:eeee:AaAa", "2001:db8:aaaa:bbbb:cccc:dddd:eeee:aaaa"),
    ] {
        let addr = resolve_ip(input);
        assert!(addr.is_ipv6(), "{input}");
        assert_eq!(addr.to_string(), canonical, "{input}");
    }
}

/// Core's IPv6ToString on bytes the std formatter writes dotted (D37): never dotted in Core.
#[test]
fn ipv6_to_string_is_never_dotted() {
    let mut mapped = [0u8; 16];
    mapped[10..].copy_from_slice(&[0xff, 0xff, 1, 2, 3, 4]);
    let addr = NetAddr::new(Network::Ipv6, mapped.to_vec()).unwrap();
    assert_eq!(addr.to_string(), "::ffff:102:304");
    let cjdns = NetAddr::new(Network::Cjdns, mapped.to_vec()).unwrap();
    assert_eq!(cjdns.to_string(), "::ffff:102:304");
}

// Core: test/evo_netinfo_tests.cpp:207 (the CJDNS row) through netbase.cpp:833-840
#[test]
fn maybe_flip_follows_reachability() {
    let s = lookup_numeric("[fc00:3344:5566:7788:9900:aabb:ccdd:eeff]:9998", 0);
    assert!(s.addr.is_ipv6());
    let flipped = maybe_flip_ipv6_to_cjdns(s.clone(), true);
    assert!(flipped.addr.is_cjdns() && flipped.addr.is_valid());
    assert_eq!(flipped.to_string(), "[fc00:3344:5566:7788:9900:aabb:ccdd:eeff]:9998");
    assert_eq!(maybe_flip_ipv6_to_cjdns(s.clone(), false), s);
    let v4 = lookup_numeric("1.2.3.4:5", 0);
    assert_eq!(maybe_flip_ipv6_to_cjdns(v4.clone(), true), v4);
}
