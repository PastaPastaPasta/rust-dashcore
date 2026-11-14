use super::*;

fn subnet(text: &str) -> Option<String> {
    lookup_subnet(text, &resolve_numeric).map(|s| s.to_string())
}

#[test]
fn subnets_parse_as_lookup_subnet_does() {
    assert_eq!(subnet("1.2.3.4").as_deref(), Some("1.2.3.4/32"));
    assert_eq!(subnet("1.2.3.4/24").as_deref(), Some("1.2.3.0/24"));
    assert_eq!(subnet("1.2.3.4/+24").as_deref(), Some("1.2.3.0/24"));
    assert_eq!(subnet("1.2.3.4/255.255.0.0").as_deref(), Some("1.2.0.0/16"));
    assert_eq!(subnet("1.2.3.4/0").as_deref(), Some("0.0.0.0/0"));
    assert_eq!(subnet("::1").as_deref(), Some("::1/128"));
    assert_eq!(subnet("[::1]").as_deref(), Some("::1/128"));
    assert_eq!(subnet("fe80::/10").as_deref(), Some("fe80::/10"));
    assert_eq!(subnet("::ffff:1.2.3.4/16").as_deref(), Some("1.2.0.0/16"));
    for bad in [
        "",
        "1.2.3.4/33",
        "1.2.3.4/-1",
        "1.2.3.4/255.0.255.0",
        "1.2.3.4/::",
        "::/129",
        "1.2.3",
        "a.b.c.d",
        "1.2.3.4/",
        "1.2.3.4/+-1",
        "1.2.3.4\0",
        "1.2.3.4/24/24",
    ] {
        assert_eq!(subnet(bad), None, "{bad:?}");
    }
}

#[test]
fn subnets_match_their_family_only() {
    let net = lookup_subnet("10.0.0.0/8", &resolve_numeric).unwrap();
    assert!(net.matches("10.9.8.7".parse().unwrap()));
    assert!(net.matches("::ffff:10.9.8.7".parse().unwrap()));
    assert!(!net.matches("11.0.0.1".parse().unwrap()));
    assert!(!net.matches("::a00:1".parse().unwrap()));
    let any6 = lookup_subnet("::/0", &resolve_numeric).unwrap();
    assert!(any6.matches("2001:db8::1".parse().unwrap()));
    assert!(!any6.matches("1.2.3.4".parse().unwrap()));
    let loopback = Subnet::with_prefix("127.0.0.1".parse().unwrap(), 8).unwrap();
    assert!(loopback.matches("127.255.0.9".parse().unwrap()));
}

#[test]
fn host_and_port_split_as_split_host_port_does() {
    let split = |s: &str| split_host_port(s, 9998);
    assert_eq!(split("127.0.0.1"), ("127.0.0.1".into(), 9998));
    assert_eq!(split("127.0.0.1:1234"), ("127.0.0.1".into(), 1234));
    assert_eq!(split("[::1]:1234"), ("::1".into(), 1234));
    assert_eq!(split("[::1]"), ("::1".into(), 9998));
    assert_eq!(split("::1"), ("::1".into(), 9998));
    assert_eq!(split("::1:1234"), ("::1:1234".into(), 9998));
    assert_eq!(split("host:+80"), ("host".into(), 80));
    // A port that does not parse stays part of the host.
    assert_eq!(split("host:65536"), ("host:65536".into(), 9998));
    assert_eq!(split("host:x"), ("host:x".into(), 9998));
    assert_eq!(split(":5"), ("".into(), 5));
    assert_eq!(split(""), ("".into(), 9998));
}

#[test]
fn addresses_print_as_to_string_addr_port() {
    assert_eq!(addr_port_string(&"127.0.0.1:5".parse().unwrap()), "127.0.0.1:5");
    assert_eq!(addr_port_string(&"[::1]:5".parse().unwrap()), "[::1]:5");
    assert_eq!(addr_port_string(&"[::ffff:1.2.3.4]:5".parse().unwrap()), "1.2.3.4:5");
    assert!(is_bind_any("0.0.0.0".parse().unwrap()));
    assert!(is_bind_any("::".parse().unwrap()));
    assert!(!is_bind_any("::1".parse().unwrap()));
}
