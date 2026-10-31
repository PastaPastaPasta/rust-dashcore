//! L0 NetInfo vectors (EV1 §6.1). Provenance: the rows of Core's `src/test/evo_netinfo_tests.cpp`
//! at the pin, each group naming its test case and lines (Core runs the main tables under
//! `BasicTestingSetup`, the others under `RegTestingSetup`, both with every network reachable and
//! `-allowprivatenet` off); the other rows cite the `evo/netinfo.cpp` line that returns their
//! status. Values are built as wire bytes and decoded with base-sdk, so the decode rules (an entry
//! Core cannot use reads as invalid) are part of every vector; every value's purposes are in the
//! order rc.4 reads ([`purposes_read_strictly`]).

use dash_params::{ChainParamsBuilder, ChainSetup, Network as CoreNetwork, Release};
use dash_primitives::{NIPurpose, NetInfo, NetInfoV1, NetInfoV2};
use dash_types::Numeric;
use dash_types::codec::{BaseCodec, DecodeError};

use super::NetInfoStatus::*;
use super::*;
use crate::MnType;
use crate::specialtx::reasons::BAD_PROTX_NETINFO_VERSION;

const CORE_P2P: u8 = 0;
const PLATFORM_P2P: u8 = 1;
const PLATFORM_HTTPS: u8 = 2;

const ONION: &str = "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd.onion";
const I2P: &str = "udhdrtrcetjm5sxzskjyr5ztpeszydbh4dpl3pl4utgqqw2v4jna.b32.i2p";

use CoreNetwork::{Devnet, Main, Regtest, Testnet3};

fn params(kind: CoreNetwork) -> ChainSetup {
    ChainParamsBuilder::new(kind, Release::V24_0).build().expect("Core's parameters")
}

/// Core's unit-test globals: every network reachable.
fn rules(kind: CoreNetwork) -> NetInfoRules {
    let config = NetInfoConfig {
        cjdns_reachable: true,
    };
    NetInfoRules::new(&params(kind), &params(Main), config)
}

fn decode<T: BaseCodec>(bytes: &[u8]) -> T {
    let mut rest = bytes;
    let value = T::decode(&mut rest).expect("decodes");
    assert!(rest.is_empty(), "trailing bytes");
    value
}

/// An `MnNetInfo` from its wire form: the legacy service.
fn mn(addr: [u8; 16], port: u16) -> NetInfo {
    let mut wire = addr.to_vec();
    wire.extend(port.to_be_bytes());
    NetInfo::Legacy(decode::<NetInfoV1>(&wire))
}

fn ipv4_mapped(ip: [u8; 4]) -> [u8; 16] {
    let mut addr = [0u8; 16];
    addr[10..12].copy_from_slice(&[0xff, 0xff]);
    addr[12..].copy_from_slice(&ip);
    addr
}

fn mn_ipv4(ip: [u8; 4], port: u16) -> NetInfo {
    mn(ipv4_mapped(ip), port)
}

fn empty_mn() -> NetInfo {
    mn([0; 16], 0)
}

fn empty_ext() -> NetInfo {
    ext(1, &[])
}

/// A service entry's wire form: type 0x01, the BIP155 network, size and address, the port.
fn service_entry(bip155_net: u8, addr: &[u8], port: u16) -> Vec<u8> {
    let mut out = vec![0x01, bip155_net, addr.len() as u8];
    out.extend(addr);
    out.extend(port.to_be_bytes());
    out
}

fn ipv4_entry(ip: [u8; 4], port: u16) -> Vec<u8> {
    service_entry(1, &ip, port)
}

fn ipv6_entry(ip: [u8; 16], port: u16) -> Vec<u8> {
    service_entry(2, &ip, port)
}

fn compact_size(n: usize) -> Vec<u8> {
    match n {
        0..0xfd => vec![n as u8],
        _ => [vec![0xfd], (n as u16).to_le_bytes().to_vec()].concat(),
    }
}

/// A domain entry's wire form: type 0x02, the string, the big-endian port.
fn domain_entry(domain: &str, port: u16) -> Vec<u8> {
    let mut out = vec![0x02];
    out.extend(compact_size(domain.len()));
    out.extend(domain.as_bytes());
    out.extend(port.to_be_bytes());
    out
}

/// The wire form of an `ExtNetInfo` of `version`, with `purposes` in the given order.
fn ext_wire(version: u8, purposes: &[(u8, Vec<Vec<u8>>)]) -> Vec<u8> {
    let mut wire = vec![version];
    wire.extend(compact_size(purposes.len()));
    for (purpose, entries) in purposes {
        wire.push(*purpose);
        wire.extend(compact_size(entries.len()));
        wire.extend(entries.iter().flatten());
    }
    wire
}

/// An `ExtNetInfo` of `version` from its wire form, which rc.4 must read.
fn ext(version: u8, purposes: &[(u8, Vec<Vec<u8>>)]) -> NetInfo {
    NetInfo::Extended(decode::<NetInfoV2>(&ext_wire(version, purposes)))
}

/// Whether base-sdk refuses an `ExtNetInfo` wire form at a group's purpose, as rc.4 does
/// (CE4-01), rather than for any other reason.
fn purpose_refused(wire: &[u8]) -> bool {
    matches!(NetInfoV2::decode(&mut &wire[..]), Err(DecodeError::InvalidValue { .. }))
}

fn purpose(p: u8) -> NIPurpose {
    NIPurpose::from_base(p)
}

/// AddEntry on `net_info`; a failure must leave it unchanged.
fn add_to(net_info: &NetInfo, kind: CoreNetwork, p: u8, input: &str) -> (NetInfoStatus, NetInfo) {
    let mut out = net_info.clone();
    let status = add_entry(&mut out, purpose(p), input, &rules(kind));
    if status != Success {
        assert_eq!(&out, net_info, "{input}: a failed AddEntry leaves the object");
    }
    (status, out)
}

/// AddEntry on an empty object, with Core's follow-up (evo_netinfo_tests.cpp:84-120): the object
/// validates with the entry and is malformed (empty) without it.
fn add(empty: NetInfo, kind: CoreNetwork, p: u8, input: &str) -> (NetInfoStatus, NetInfo) {
    let (status, out) = add_to(&empty, kind, p, input);
    let expected = if status == Success {
        Success
    } else {
        Malformed
    };
    assert_eq!(validate(&out, &rules(kind)), expected, "{input}");
    assert_eq!(has_entries(&out, purpose(p)), status == Success, "{input}");
    (status, out)
}

/// `{purpose, input}, expected MnNetInfo, expected ExtNetInfo`.
type Row = ((u8, &'static str), NetInfoStatus, NetInfoStatus);

fn check_rows(kind: CoreNetwork, rows: &[Row]) {
    for &((p, input), mn_status, ext_status) in rows {
        assert_eq!(add(empty_mn(), kind, p, input).0, mn_status, "MnNetInfo {p} {input}");
        assert_eq!(add(empty_ext(), kind, p, input).0, ext_status, "ExtNetInfo {p} {input}");
    }
}

/// `addr_vals_main` (evo_netinfo_tests.cpp:28-74, with rc.4:39's RFC1918 row; mnnetinfo_rules_main,
/// extnetinfo_rules_main).
#[test]
fn addr_vals_main() {
    let onion_9999 = format!("{ONION}:9999").leak();
    let onion_443 = format!("{ONION}:443").leak();
    let rows: &[Row] = &[
        ((CORE_P2P, "1.1.1.1:9999"), Success, Success),
        ((CORE_P2P, "1.1.1.1"), Success, BadPort),
        ((CORE_P2P, "1.1.1.1:9998"), BadPort, Success),
        ((CORE_P2P, "127.0.0.1:9999"), NotRoutable, NotRoutable),
        ((CORE_P2P, "10.0.0.1:9999"), NotRoutable, NotRoutable),
        ((CORE_P2P, "0.0.0.0:9999"), BadAddress, BadAddress),
        ((CORE_P2P, "1.1.1.1:99999"), BadInput, BadInput),
        ((CORE_P2P, "[2606:4700:4700::1111]:9999"), BadInput, Success),
        ((CORE_P2P, "example.com:9999"), BadInput, BadInput),
        ((CORE_P2P, onion_9999), BadInput, Success),
        ((PLATFORM_P2P, "example.com:9999"), MaxLimit, BadInput),
        ((PLATFORM_P2P, onion_9999), MaxLimit, Success),
        ((PLATFORM_HTTPS, "example.com:9999"), MaxLimit, Success),
        ((PLATFORM_HTTPS, onion_9999), MaxLimit, Success),
        ((PLATFORM_HTTPS, "1.1.1.1:443"), MaxLimit, Success),
        ((PLATFORM_HTTPS, "[2606:4700:4700::1111]:443"), MaxLimit, Success),
        ((PLATFORM_HTTPS, onion_443), MaxLimit, Success),
        ((PLATFORM_HTTPS, "1.1.1.1:80"), MaxLimit, BadPort),
        ((PLATFORM_P2P, "1.1.1.1:443"), MaxLimit, BadPort),
        ((CORE_P2P, "1.1.1.1:443"), BadPort, BadPort),
        ((CORE_P2P, "1.1.1.256:9999"), BadInput, BadInput),
        ((CORE_P2P, ":9999"), BadInput, BadInput),
        ((64, "1.1.1.1:9999"), MaxLimit, MaxLimit),
        ((PLATFORM_P2P, "1.1.1.1:9999"), MaxLimit, Success),
    ];
    check_rows(Main, rows);
}

/// `addr_vals_reg` (evo_netinfo_tests.cpp:189-199) and extnetinfo_rules_reg's `domain_vals`
/// (:267-278), on regtest.
#[test]
fn addr_vals_reg() {
    let onionless = "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd:9998";
    let rows: &[Row] = &[
        ((CORE_P2P, "1.1.1.1:0"), Success, BadPort),
        ((CORE_P2P, "1.1.1.1:9999"), BadPort, Success),
        ((CORE_P2P, "1.1.1.1:22"), Success, BadPort),
        ((PLATFORM_HTTPS, "example.com:80"), MaxLimit, BadPort),
        ((PLATFORM_HTTPS, "example.com:443"), MaxLimit, Success),
        ((PLATFORM_HTTPS, "example.123:443"), MaxLimit, BadInput),
        ((PLATFORM_HTTPS, "somebodys-macbook-pro.local:9998"), MaxLimit, BadInput),
        ((PLATFORM_HTTPS, onionless), MaxLimit, BadInput),
    ];
    check_rows(Regtest, rows);
}

/// What AddEntry stores (cservice_compatible, evo_netinfo_tests.cpp:420-450): an `MnNetInfo`
/// holds the plain service, an `ExtNetInfo` appends the entry under its purpose, a domain is
/// stored lowercased (`DomainPort::Set`, netinfo.cpp:115-128).
#[test]
fn stored_entries() {
    for input in ["1.1.1.1:9999", "1.1.1.1"] {
        let (status, out) = add(empty_mn(), Main, CORE_P2P, input);
        assert_eq!((status, out), (Success, mn_ipv4([1, 1, 1, 1], 9999)), "{input}");
    }
    let (_, out) = add(empty_ext(), Main, CORE_P2P, "1.1.1.1:9999");
    assert_eq!(out, ext(1, &[(CORE_P2P, vec![ipv4_entry([1, 1, 1, 1], 9999)])]));
    let (_, out) = add(empty_ext(), Main, PLATFORM_HTTPS, "Example.COM:443");
    assert_eq!(out, ext(1, &[(PLATFORM_HTTPS, vec![domain_entry("example.com", 443)])]));
    assert_eq!(primary(&out), dash_netaddr::Service::default());

    // A new purpose goes in at its sorted place (Core's map order, D41).
    let (_, out) = add(empty_ext(), Regtest, PLATFORM_HTTPS, "example.com:443");
    let (status, out) = add_to(&out, Regtest, CORE_P2P, "1.1.1.1:9998");
    assert_eq!(status, Success);
    let NetInfo::Extended(v2) = &out else {
        unreachable!()
    };
    let order: Vec<u8> = v2.entries.iter().map(|(p, _)| p.to_base()).collect();
    assert_eq!(order, [CORE_P2P, PLATFORM_HTTPS]);
    assert_eq!(primary(&out).to_string(), "1.1.1.1:9998");
}

/// mnnetinfo_rules_main (evo_netinfo_tests.cpp:126-143): one entry, Core P2P only.
#[test]
fn mnnetinfo_limits() {
    let (_, one) = add(empty_mn(), Main, CORE_P2P, "1.1.1.1:9999");
    assert_eq!(add_to(&one, Main, CORE_P2P, "1.1.1.2:9999").0, MaxLimit);
    for p in [PLATFORM_HTTPS, PLATFORM_P2P] {
        assert_eq!(add_to(&empty_mn(), Main, p, "1.1.1.1:9999").0, MaxLimit);
    }
}

/// extnetinfo_rules_reg (evo_netinfo_tests.cpp:225-262): four entries per purpose, duplicates
/// within and across purposes.
#[test]
fn extnetinfo_limits_and_duplicates() {
    let mut net_info = empty_ext();
    for idx in 1..=4 {
        let (status, out) = add_to(&net_info, Regtest, CORE_P2P, &format!("1.1.1.{idx}:9998"));
        assert_eq!(status, Success, "entry {idx}");
        net_info = out;
    }
    assert_eq!(add_to(&net_info, Regtest, CORE_P2P, "1.1.1.5:9998").0, MaxLimit);
    let (status, out) = add_to(&net_info, Regtest, PLATFORM_P2P, "1.1.1.5:9998");
    assert_eq!(status, Success);
    assert_eq!(validate(&out, &rules(Regtest)), Success);

    let (_, first) = add(empty_ext(), Regtest, CORE_P2P, "1.1.1.1:9998");
    for (p, input, expected) in [
        (CORE_P2P, "1.1.1.1:9998", Duplicate),
        (CORE_P2P, "1.1.1.1:9997", Duplicate),
        (PLATFORM_P2P, "1.1.1.1:9998", Duplicate),
        (PLATFORM_P2P, "1.1.1.1:9997", Success),
    ] {
        assert_eq!(add_to(&first, Regtest, p, input).0, expected, "{p} {input}");
    }
}

/// `privacy_addr_vals` (evo_netinfo_tests.cpp:207-217, 280-305) under Core P2P on regtest. The
/// CJDNS entry is stored as BIP155 network 6 (`MaybeFlipIPv6toCJDNS`); with CJDNS unreachable it
/// stays IPv6 (D40).
#[test]
fn privacy_addr_vals() {
    for (input, expected) in [
        ("[fc00:3344:5566:7788:9900:aabb:ccdd:eeff]:9998".to_string(), Success),
        (format!("{I2P}:0"), Success),
        (format!("{ONION}:9998"), Success),
        (format!("{I2P}:9998"), BadPort),
        (format!("{ONION}:0"), BadPort),
    ] {
        assert_eq!(add(empty_ext(), Regtest, CORE_P2P, &input).0, expected, "{input}");
    }
    let cjdns =
        [0xfc, 0, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];
    let input = "[fc00:3344:5566:7788:9900:aabb:ccdd:eeff]:9998";
    let (_, out) = add(empty_ext(), Regtest, CORE_P2P, input);
    assert_eq!(out, ext(1, &[(CORE_P2P, vec![service_entry(6, &cjdns, 9998)])]));

    let mut unreachable = rules(Regtest);
    unreachable.config.cjdns_reachable = false;
    let mut out = empty_ext();
    assert_eq!(add_entry(&mut out, NIPurpose::CoreP2p, input, &unreachable), Success);
    assert_eq!(out, ext(1, &[(CORE_P2P, vec![ipv6_entry(cjdns, 9998)])]));
}

/// domainport_rules (evo_netinfo_tests.cpp:491-525) through AddEntry for Platform HTTPS. An
/// input of hex digits and dots only is parsed as an address and fails `Lookup()`
/// (netinfo.cpp:550); characters outside RFC 1035 fail first (:513-515).
#[test]
fn domainport_rules() {
    let long = "Loremipsumdolorsitametconsecteturadipiscingelitseddoeiusmodtempor\
                incididuntutlaboreetdoloremagnaaliquaUtenimadminimveniamquisnostrud\
                exercitationullamcolaborisnisiutaliquipexeacommodoconsequatDuisaute\
                iruredolorinreprehenderitinvoluptatevelitessecillumdoloreeufugiatnullapariat.ur";
    for (domain, expected) in [
        ("r.server-1.ab.cd", Success),
        ("9998.9example7.ab", Success),
        ("abcd", BadInput),
        ("abc.", BadInput),
        (".abc", BadInput),
        ("a..dot..b", BadInput),
        ("somebody's macbook pro.local", BadInput),
        ("somebodys macbook pro.local", BadInput),
        ("-a-.bc.de", BadInput),
        ("ac", BadInput),
        ("wxyz", BadInput),
        ("wxy.", BadInput),
        (".wxy", BadInput),
        ("wx", BadInput),
        (long, BadInput),
        ("loremipsumdolorsitametconsecteturadipiscingelitseddoeiusmodtempo.ri.nc", BadInput),
    ] {
        let input = format!("{domain}:443");
        assert_eq!(add(empty_ext(), Main, PLATFORM_HTTPS, &input).0, expected, "{domain}");
    }
    assert_eq!(add(empty_ext(), Main, PLATFORM_HTTPS, ":0").0, BadInput);
    // `DomainPort::Set` refuses port 0 (netinfo.cpp:117), which AddEntry reports as BadInput.
    assert_eq!(add(empty_ext(), Main, PLATFORM_HTTPS, "example.com:0").0, BadInput);
    assert_eq!(add(empty_ext(), Main, PLATFORM_HTTPS, "example.com").0, BadInput);
    // `MatchSuffix` is case-sensitive: an upper-case privacy TLD goes to `DomainPort::Set`, is
    // lowercased, and fails the TLD rule (netinfo.cpp:489).
    let upper = format!("{}:443", ONION.to_uppercase());
    assert_eq!(add(empty_ext(), Main, PLATFORM_HTTPS, &upper).0, BadInput);
}

/// A stored domain made only of `SAFE_CHARS_IPV4_6` characters validates, but AddEntry sends the
/// same string to the numeric `Lookup` (netinfo.cpp:512, :541-550), which fails: the one valid
/// stored entry `AddNetInfoEntries` cannot carry over (EV1-1 Opus r1 N2). One letter outside the
/// hex alphabet takes the domain path (netinfo.cpp:530) and round-trips.
#[test]
fn hex_only_domain_fails_the_round_trip() {
    let main = rules(Main);
    for (domain, round_trip) in [("dead.beef", BadInput), ("dead.beeg", Success)] {
        let stored = ext(
            1,
            &[
                (CORE_P2P, vec![ipv4_entry([1, 1, 1, 1], 9999)]),
                (PLATFORM_HTTPS, vec![domain_entry(domain, 443)]),
            ],
        );
        assert_eq!(validate(&stored, &main), Success, "{domain}");
        let input = format!("{domain}:443");
        assert_eq!(add_to(&empty_ext(), Main, PLATFORM_HTTPS, &input).0, round_trip, "{domain}");
    }
}

/// extnetinfo_validate_deser (evo_netinfo_tests.cpp:638-716, regtest): what only a decoded
/// object can hold.
#[test]
fn extnetinfo_validate_deser() {
    let reg = rules(Regtest);
    let entry = ipv4_entry([1, 1, 1, 1], 9998);
    assert_eq!(validate(&ext(1, &[(CORE_P2P, vec![entry.clone(), entry])]), &reg), Duplicate);
    let five = (1..=5).map(|i| ipv4_entry([1, 1, 1, i], 9998)).collect();
    assert_eq!(validate(&ext(1, &[(CORE_P2P, five)]), &reg), MaxLimit);
    let domain = ext(1, &[(CORE_P2P, vec![domain_entry("example.com", 443)])]);
    assert_eq!(validate(&domain, &reg), BadInput);
    for (p, expected) in [(PLATFORM_HTTPS, Success), (PLATFORM_P2P, BadPort), (CORE_P2P, BadPort)] {
        let net_info = ext(1, &[(p, vec![ipv4_entry([1, 1, 1, 1], 443)])]);
        assert_eq!(validate(&net_info, &reg), expected, "purpose {p}");
    }
}

/// Validate() rows that ProUpServ's trivial checks pre-empt (providertx.cpp:166-185): empty
/// objects, objects without Core P2P entries, trivially invalid entries; each cites the line
/// returning it.
#[test]
fn validate_direct_statuses() {
    let main = rules(Main);
    // MnNetInfo: an empty or invalid service is the invalid entry (netinfo.cpp:353).
    assert_eq!(validate(&empty_mn(), &main), Malformed);
    assert_eq!(validate(&mn_ipv4([0, 0, 0, 0], 9999), &main), Malformed);
    let ipv6 = [0x26, 0x06, 0x47, 0, 0x47, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x11, 0x11];
    assert_eq!(validate(&mn(ipv6, 9999), &main), BadType); // :291
    assert_eq!(validate(&mn_ipv4([127, 0, 0, 1], 9999), &main), NotRoutable); // :294
    assert_eq!(validate(&mn_ipv4([1, 1, 1, 1], 9998), &main), BadPort); // :298
    assert_eq!(validate(&mn_ipv4([1, 1, 1, 1], 9999), &main), Success);
    assert_eq!(validate(&mn_ipv4([1, 1, 1, 1], 9999), &rules(Regtest)), BadPort);

    let ok = || ipv4_entry([1, 1, 1, 1], 9999);
    assert_eq!(validate(&empty_ext(), &main), Malformed); // :590
    assert_eq!(validate(&NetInfo::Extended(decode(&[0])), &main), Malformed);
    assert_eq!(validate(&NetInfo::Extended(decode(&[2])), &main), Malformed);
    // rc.3's unknown-purpose Malformed (:597) is gone: rc.4 does not read such an object.
    assert!(purpose_refused(&ext_wire(1, &[(3, vec![ok()])])));
    assert_eq!(validate(&ext(1, &[(CORE_P2P, vec![])]), &main), Malformed); // :601
    // An unknown entry type, an invalid service (0.0.0.0; the internal prefix in IPv6) and a
    // domain `DomainPort::Validate()` refuses all read as the invalid entry (netinfo.h:243-263).
    for entry in [
        vec![0x03],
        ipv4_entry([0, 0, 0, 0], 9999),
        ipv6_entry([0xfd, 0x6b, 0x88, 0xc0, 0x87, 0x24, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1], 9999),
        domain_entry("abcd", 443),
        domain_entry("Example.com", 443),
        domain_entry("example.com", 0),
    ] {
        let net_info = ext(1, &[(CORE_P2P, vec![ok()]), (PLATFORM_HTTPS, vec![entry.clone()])]);
        assert_eq!(validate(&net_info, &main), Malformed, "{entry:02x?}"); // :617
    }
    let platform_only = ext(1, &[(PLATFORM_P2P, vec![ipv4_entry([1, 1, 1, 1], 443)])]);
    assert_eq!(validate(&platform_only, &main), BadPort); // :474
    let local = ext(1, &[(CORE_P2P, vec![ipv4_entry([10, 0, 0, 1], 9999)])]);
    assert_eq!(validate(&local, &main), NotRoutable); // :460
    let tld =
        ext(1, &[(CORE_P2P, vec![ok()]), (PLATFORM_HTTPS, vec![domain_entry("a.local", 443)])]);
    assert_eq!(validate(&tld, &main), BadInput); // :489
    let numeric = domain_entry("example.123", 443);
    let tld = ext(1, &[(CORE_P2P, vec![ok()]), (PLATFORM_HTTPS, vec![numeric])]);
    assert_eq!(validate(&tld, &main), BadInput); // :492
    let http80 =
        ext(1, &[(CORE_P2P, vec![ok()]), (PLATFORM_HTTPS, vec![domain_entry("a.com", 80)])]);
    assert_eq!(validate(&http80, &main), BadPort); // :485
    // Validate() never returns BadAddress: a trivially valid entry holds a valid service.
}

/// Two invalid entries are equal (`std::set<NetInfoEntry>`, netinfo.cpp:383-392), so a decoded
/// object holding two is a Duplicate before either is Malformed, even across purposes.
#[test]
fn invalid_entries_are_duplicates() {
    let main = rules(Main);
    let ok = ipv4_entry([1, 1, 1, 1], 9999);
    let one = ext(1, &[(CORE_P2P, vec![ok.clone()]), (PLATFORM_P2P, vec![vec![0x03]])]);
    assert_eq!(validate(&one, &main), Malformed);
    let two = ext(1, &[(CORE_P2P, vec![ok, vec![0x03]]), (PLATFORM_P2P, vec![vec![0xff]])]);
    assert_eq!(validate(&two, &main), Duplicate);
}

/// EV1 §6.1 "NetInfo nesting" (M6): all of a purpose's checks run before the next purpose's,
/// so an earlier purpose's entry error wins over a later purpose's count.
#[test]
fn nesting_follows_purpose_order() {
    let main = rules(Main);
    let unroutable = vec![ipv4_entry([10, 0, 0, 1], 9999)];
    let five: Vec<_> = (1..=5).map(|i| ipv4_entry([1, 1, 2, i], 26656)).collect();
    let net_info = ext(1, &[(CORE_P2P, unroutable.clone()), (PLATFORM_P2P, five.clone())]);
    assert_eq!(validate(&net_info, &main), NotRoutable);
    let net_info = ext(1, &[(CORE_P2P, five), (PLATFORM_P2P, unroutable)]);
    assert_eq!(validate(&net_info, &main), MaxLimit);
    // On regtest the same unroutable entry passes, and the later count decides.
    let unroutable = vec![ipv4_entry([10, 0, 0, 1], 9998)];
    let five = (1..=5).map(|i| ipv4_entry([1, 1, 2, i], 26656)).collect();
    let net_info = ext(1, &[(CORE_P2P, unroutable), (PLATFORM_P2P, five)]);
    assert_eq!(validate(&net_info, &rules(Regtest)), MaxLimit);
    // A purpose's address duplicates come before its entries' rules.
    let dup = vec![ipv4_entry([1, 1, 1, 1], 1), ipv4_entry([1, 1, 1, 1], 9999)];
    assert_eq!(validate(&ext(1, &[(CORE_P2P, dup)]), &main), Duplicate);
}

/// A 253-byte domain: four labels of 63, 63, 63 and 61 bytes, the second of `second`. Its
/// letters are not hex digits, so AddEntry reads it as a domain, not an address.
fn long_domain(second: char) -> String {
    let label = |c: char, n| std::iter::repeat_n(c, n).collect::<String>();
    format!("{}.{}.{}.{}", label('q', 63), label(second, 63), label('r', 63), label('s', 61))
}

/// EV1 §6.1 "Long addresses" (D15): addresses are compared whole.
#[test]
fn long_addresses_are_compared_whole() {
    let main = rules(Main);
    let (a, b) = (long_domain('u'), long_domain('v'));
    assert_eq!((a.len(), b.len()), (DOMAIN_MAX_LEN, DOMAIN_MAX_LEN));
    assert_eq!(a[..64], b[..64]);
    let core = (CORE_P2P, vec![ipv4_entry([1, 1, 1, 1], 9999)]);
    let two = (PLATFORM_HTTPS, vec![domain_entry(&a, 443), domain_entry(&b, 443)]);
    assert_eq!(validate(&ext(1, &[core.clone(), two]), &main), Success);
    let same_port = (PLATFORM_HTTPS, vec![domain_entry(&a, 443), domain_entry(&a, 443)]);
    assert_eq!(validate(&ext(1, &[core.clone(), same_port]), &main), Duplicate);
    let other_port = (PLATFORM_HTTPS, vec![domain_entry(&a, 443), domain_entry(&a, 8443)]);
    assert_eq!(validate(&ext(1, &[core.clone(), other_port]), &main), Duplicate);

    // The maximal domain-and-port string through AddEntry, then a second long domain and the
    // first again under another port.
    let (status, out) = add(empty_ext(), Main, PLATFORM_HTTPS, &format!("{a}:65535"));
    assert_eq!(status, Success);
    let (status, out) = add_to(&out, Main, PLATFORM_HTTPS, &format!("{b}:65535"));
    assert_eq!(status, Success);
    assert_eq!(add_to(&out, Main, PLATFORM_HTTPS, &format!("{a}:443")).0, Duplicate);
    // One byte longer is BadLen (netinfo.cpp:86).
    let over = format!("{a}s:443");
    assert_eq!(add(empty_ext(), Main, PLATFORM_HTTPS, &over).0, BadInput);
}

/// Tor v3 and I2P services are compared by their `ToStringAddr()` with the domains of their
/// purpose (netinfo.cpp:401-418), so a domain spelling the same name is a Duplicate before the
/// domain's TLD rule; IPv6 and CJDNS services with the same bytes print the same.
#[test]
fn privacy_services_dedupe_by_string() {
    let main = rules(Main);
    let core = (CORE_P2P, vec![ipv4_entry([1, 1, 1, 1], 9999)]);
    let onion = dash_netaddr::NetAddr::set_special(ONION).unwrap();
    let i2p = dash_netaddr::NetAddr::set_special(I2P).unwrap();
    for (net, addr, name, port) in [(4, onion.bytes(), ONION, 443), (5, i2p.bytes(), I2P, 0)] {
        let service = service_entry(net, addr, port);
        let https = (PLATFORM_HTTPS, vec![service.clone(), domain_entry(name, 443)]);
        assert_eq!(validate(&ext(1, &[core.clone(), https]), &main), Duplicate, "{name}");
        let alone = (PLATFORM_HTTPS, vec![service]);
        assert_eq!(validate(&ext(1, &[core.clone(), alone]), &main), Success, "{name}");
    }
    let bytes = [0xfc, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
    let reg = rules(Regtest);
    let both = (CORE_P2P, vec![ipv6_entry(bytes, 9998), service_entry(6, &bytes, 9997)]);
    assert_eq!(validate(&ext(1, &[both]), &reg), Duplicate);
    // Across purposes only equal entries clash, and these differ by network.
    let split = [
        (CORE_P2P, vec![ipv6_entry(bytes, 9998)]),
        (PLATFORM_P2P, vec![service_entry(6, &bytes, 9998)]),
    ];
    assert_eq!(validate(&ext(1, &split), &reg), Success);
}

/// CE4-01: rc.4 reads a purpose only if it is known and above the one before it; a repeated,
/// descending or unknown purpose fails the read (`evo_netinfo_tests.cpp` rc.4:725-756 in
/// `extnetinfo_validate_deser`; `netinfo.h` rc.4:419-437). rc.3 kept a repeated purpose's first
/// group, sorted the groups and left an unknown purpose to `Validate()` (D41).
#[test]
fn purposes_read_strictly() {
    let good = || ipv4_entry([1, 1, 1, 1], 9999);
    let p2p: Vec<_> = (1..=5).map(|i| ipv4_entry([1, 1, 2, i], 26656)).collect();
    ext(1, &[(CORE_P2P, vec![good()])]);
    ext(1, &[(CORE_P2P, vec![good()]), (PLATFORM_HTTPS, vec![good()])]);
    ext(1, &[(CORE_P2P, vec![good()]), (PLATFORM_P2P, p2p.clone())]);
    let refused = |purposes: &[(u8, Vec<Vec<u8>>)]| purpose_refused(&ext_wire(1, purposes));
    assert!(refused(&[(CORE_P2P, vec![good()]), (CORE_P2P, vec![good()])]));
    assert!(refused(&[(PLATFORM_P2P, p2p.clone()), (CORE_P2P, vec![good()])]));
    assert!(refused(&[(CORE_P2P, vec![good()]), (3, vec![good()])]));
    assert!(refused(&[(0xff, vec![good()])]));
    // The first bad purpose fails the read, whatever the groups after it hold.
    let mut wire = ext_wire(1, &[(CORE_P2P, vec![good()]), (CORE_P2P, vec![good()])]);
    wire.truncate(wire.len() - 1);
    assert!(purpose_refused(&wire));
    // Core reads nothing past a version it does not know.
    for version in [0, 2] {
        let wire = ext_wire(version, &[(3, vec![]), (3, vec![])]);
        assert!(NetInfoV2::decode(&mut &wire[..]).is_ok(), "{version}");
    }
    assert!(purpose_follows(PLATFORM_HTTPS, Some(PLATFORM_P2P)));
    assert!(!purpose_follows(PLATFORM_P2P, Some(PLATFORM_P2P)));
    assert!(!purpose_follows(3, None));
}

/// CE4-02: an RFC1918 address is never routable for a masternode on the networks that require
/// routable addresses, legacy or extended (`netinfo.cpp` rc.4:294, 456;
/// `netinfo_rules_main_allowprivatenet`, `evo_netinfo_tests.cpp` rc.4:149-157); rc.3 accepted
/// it under `-allowprivatenet`, which the rules no longer read. Each range's edges, and the
/// addresses just outside it.
#[test]
fn rfc1918_is_never_routable() {
    let rows: [([u8; 4], bool); 10] = [
        ([9, 255, 255, 255], true),
        ([10, 0, 0, 0], false),
        ([10, 255, 255, 255], false),
        ([11, 0, 0, 0], true),
        ([172, 15, 255, 255], true),
        ([172, 16, 0, 0], false),
        ([172, 31, 255, 255], false),
        ([172, 32, 0, 0], true),
        ([192, 168, 0, 0], false),
        ([192, 168, 255, 255], false),
    ];
    for kind in [Main, Testnet3, Devnet, Regtest] {
        let r = rules(kind);
        let port = if kind == Main {
            9999
        } else {
            19999
        };
        for (ip, routable) in rows {
            let expected = if routable || kind == Regtest {
                Success
            } else {
                NotRoutable
            };
            let legacy = mn_ipv4(ip, port);
            assert_eq!(validate(&legacy, &r), expected, "{kind:?} legacy {ip:?}");
            let extended = ext(1, &[(CORE_P2P, vec![ipv4_entry(ip, port)])]);
            assert_eq!(validate(&extended, &r), expected, "{kind:?} extended {ip:?}");
        }
    }
}

/// One vector per Validate() status on each network, plus the AddEntry statuses no Validate()
/// can show (EV1 §6.1 "NetInfo order", "Direct NetInfo statuses"). Expected values follow
/// `IsNodeOnMainnet()`, `RequireRoutableExternalIP()` (all but regtest) and the mainnet port.
#[test]
fn statuses_per_network() {
    let nets = [Main, Testnet3, Regtest, Devnet];
    let core = |e: Vec<u8>| ext(1, &[(CORE_P2P, vec![e])]);
    // (object, [mainnet, testnet, regtest, devnet])
    let rows: Vec<(NetInfo, [NetInfoStatus; 4])> = vec![
        (mn_ipv4([1, 1, 1, 1], 9999), [Success, BadPort, BadPort, BadPort]),
        (mn_ipv4([1, 1, 1, 1], 19999), [BadPort, Success, Success, Success]),
        (mn_ipv4([10, 0, 0, 1], 19999), [NotRoutable, NotRoutable, Success, NotRoutable]),
        (mn_ipv4([0, 0, 0, 0], 9999), [Malformed; 4]),
        (mn([0x20, 1, 0x48, 0x60, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x88, 0x88], 9999), [BadType; 4]),
        (core(ipv4_entry([1, 1, 1, 1], 9999)), [Success; 4]),
        (core(ipv4_entry([10, 0, 0, 1], 9999)), [NotRoutable, NotRoutable, Success, NotRoutable]),
        (core(ipv4_entry([1, 1, 1, 1], 0)), [BadPort; 4]),
        (core(ipv4_entry([1, 1, 1, 1], 22)), [BadPort; 4]),
        (core(domain_entry("example.com", 443)), [BadInput; 4]),
        (core(vec![0x03]), [Malformed; 4]),
        (ext(1, &[(CORE_P2P, vec![ipv4_entry([1, 1, 1, 1], 9999); 2])]), [Duplicate; 4]),
        (
            ext(1, &[(CORE_P2P, (1..=5).map(|i| ipv4_entry([1, 1, 1, i], 9999)).collect())]),
            [MaxLimit; 4],
        ),
    ];
    for (i, (net_info, expected)) in rows.iter().enumerate() {
        for (kind, expected) in nets.iter().zip(expected) {
            assert_eq!(validate(net_info, &rules(*kind)), *expected, "row {i} on {kind:?}");
        }
    }
    // AddEntry: BadAddress (a valid form of an invalid address, netinfo.cpp:288, :454).
    for kind in nets {
        assert_eq!(add(empty_mn(), kind, CORE_P2P, "0.0.0.0:9999").0, BadAddress);
        assert_eq!(add(empty_ext(), kind, CORE_P2P, "[::]:9999").0, BadAddress);
    }
}

/// glibc's numeric forms reach AddEntry through `Lookup()` (D35): `inet_aton` shorthand, octal
/// and hex parts pass Core's IPv4 filter or IPv4/IPv6 filter.
#[test]
fn numeric_forms_follow_getaddrinfo() {
    for (input, stored) in [
        ("1.1.257:9999", [1, 1, 1, 1]),
        ("16843009:9999", [1, 1, 1, 1]),
        ("01.1.1.1:9999", [1, 1, 1, 1]),
    ] {
        let (status, out) = add(empty_mn(), Main, CORE_P2P, input);
        assert_eq!((status, out), (Success, mn_ipv4(stored, 9999)), "{input}");
    }
    let (status, out) = add(empty_ext(), Main, CORE_P2P, "1.1.257:9999");
    assert_eq!(
        (status, out),
        (Success, ext(1, &[(CORE_P2P, vec![ipv4_entry([1, 1, 1, 1], 9999)])]))
    );
    // `x` is outside both IP filters: "0x1.1.1.1" is read as a domain, whose last label is not
    // alphabetic (netinfo.cpp:492); the IPv4 filter refuses it outright.
    assert_eq!(add(empty_ext(), Main, PLATFORM_HTTPS, "0x1.1.1.1:9999").0, BadInput);
    assert_eq!(add(empty_mn(), Main, CORE_P2P, "0x1.1.1.1:9999").0, BadInput);
    // "08" is not octal: Lookup() fails.
    assert_eq!(add(empty_mn(), Main, CORE_P2P, "08.1.1.1:9999").0, BadInput);
}

/// Core's AddEntry takes bytes (evo_netinfo_tests via O2a-SP raw_input_bytes): a NUL or a
/// non-UTF-8 byte fails the character filters, the empty string fails `Lookup()`.
#[test]
fn raw_input_bytes() {
    for input in [&b"1.1.1.1\0:9999"[..], b"1.1.1.1:9999\xff", b"\xff:9999", b""] {
        for empty in [empty_mn(), empty_ext()] {
            let mut out = empty.clone();
            let status = add_entry_bytes(&mut out, NIPurpose::CoreP2p, input, &rules(Main));
            assert_eq!((status, &out), (BadInput, &empty), "{input:?}");
        }
    }
    let (_, one) = add(empty_mn(), Main, CORE_P2P, "1.1.1.1:9999");
    let mut out = one.clone();
    assert_eq!(add_entry_bytes(&mut out, NIPurpose::CoreP2p, b"\xff", &rules(Main)), MaxLimit);
    let mut out = empty_ext();
    assert_eq!(add_entry_bytes(&mut out, purpose(9), b"\xff", &rules(Main)), MaxLimit);
}

/// D15: every `ToStringAddr[Port]()` form fits [`AddrStr`], and an overflow is an error, never a
/// truncation.
#[test]
fn addr_str_fits_every_form() {
    use dash_netaddr::{NetAddr, Network, Service};
    let ff = |n| vec![0xff; n];
    let mut cjdns = ff(16);
    cjdns[0] = 0xfc;
    let addrs = [
        NetAddr::ipv4([255; 4]),
        NetAddr::new(Network::Ipv6, ff(16)).unwrap(),
        NetAddr::new(Network::Onion, ff(32)).unwrap(),
        NetAddr::new(Network::I2p, ff(32)).unwrap(),
        NetAddr::new(Network::Cjdns, cjdns).unwrap(),
        NetAddr::new(Network::Internal, ff(10)).unwrap(),
    ];
    for addr in addrs {
        let service = Service::new(addr.clone(), u16::MAX);
        let s = AddrStr::format(&service).expect("fits");
        assert_eq!(s.as_str(), service.to_string());
        assert_eq!(AddrStr::format(&addr).expect("fits").as_str(), addr.to_string());
    }
    let domain = long_domain('u');
    let s = AddrStr::format(format_args!("{domain}:65535")).expect("fits");
    assert_eq!(s.as_str().len(), DOMAIN_MAX_LEN + 6);
    assert!(AddrStr::format("x".repeat(NETINFO_ADDR_MAX)).is_some());
    assert!(AddrStr::format("x".repeat(NETINFO_ADDR_MAX + 1)).is_none());
}

/// `CoreNetInfoValidity` is `Validate() == Success`.
#[test]
fn core_validity_is_validate_success() {
    let v = CoreNetInfoValidity {
        rules: rules(Main),
    };
    assert!(v.is_valid(&mn_ipv4([1, 1, 1, 1], 9999)));
    assert!(!v.is_valid(&mn_ipv4([1, 1, 1, 1], 9998)));
    assert!(!v.is_valid(&empty_ext()));
}

/// EV1 §6.1 "Migration predicate": one vector per §3.16.1 reject row and per early return.
#[test]
fn migration_reject_rows() {
    use MnType::{Evo, Regular};
    let main = rules(Main);
    let reg = rules(Regtest);
    let reject = Some(BAD_PROTX_NETINFO_VERSION);
    let mn_ok = mn_ipv4([1, 1, 1, 1], 9999);

    // Step 4: the version and kind already match, whatever the entries.
    assert_eq!(migration_reject(&mn_ok, 2, 2, Evo, &main), None);
    assert_eq!(migration_reject(&mn_ipv4([10, 0, 0, 1], 1), 2, 2, Regular, &main), None);
    let ext_bad = ext(1, &[(CORE_P2P, vec![vec![0x03], vec![0x03]])]);
    assert_eq!(migration_reject(&ext_bad, 3, 3, Evo, &main), None);

    // 6a: a legacy Core P2P service that ExtNetInfo refuses (regtest port 0 or 22).
    assert_eq!(migration_reject(&mn_ok, 2, 3, Regular, &main), None);
    assert_eq!(migration_reject(&mn_ipv4([1, 1, 1, 1], 0), 2, 3, Regular, &reg), reject);
    assert_eq!(migration_reject(&mn_ipv4([1, 1, 1, 1], 22), 1, 3, Regular, &reg), reject);
    assert_eq!(migration_reject(&mn_ipv4([1, 1, 1, 1], 19899), 2, 3, Regular, &reg), None);
    // An empty legacy object carries nothing.
    assert_eq!(migration_reject(&empty_mn(), 2, 3, Evo, &main), None);

    // 6b: an extended object's Platform entries are carried too, P2P before HTTPS.
    let platform = |p2p: Vec<u8>, https: Vec<u8>| {
        ext(
            1,
            &[
                (CORE_P2P, vec![ipv4_entry([1, 1, 1, 1], 9999)]),
                (PLATFORM_P2P, vec![p2p]),
                (PLATFORM_HTTPS, vec![https]),
            ],
        )
    };
    let good = platform(ipv4_entry([1, 1, 1, 2], 26656), domain_entry("example.com", 443));
    assert_eq!(migration_reject(&good, 2, 3, Evo, &main), None);
    let bad_p2p = platform(vec![0x03], domain_entry("example.com", 443));
    assert_eq!(migration_reject(&bad_p2p, 2, 3, Evo, &main), reject);
    let bad_https = platform(ipv4_entry([1, 1, 1, 2], 26656), domain_entry("a.local", 443));
    assert_eq!(migration_reject(&bad_https, 2, 3, Evo, &main), reject);
    let core_bad = ext(1, &[(CORE_P2P, vec![ipv4_entry([1, 1, 1, 1], 1)])]);
    assert_eq!(migration_reject(&core_bad, 2, 3, Regular, &main), reject);

    // 6c: a non-empty legacy object of an EvoNode cannot be raised (its Platform ports would be
    // lost); a regular node's can.
    assert_eq!(migration_reject(&mn_ok, 2, 3, Evo, &main), reject);
    assert_eq!(migration_reject(&mn_ok, 2, 3, Regular, &main), None);

    // 7: back to a legacy object: one IPv4 Core P2P entry fits, a second or a non-IPv4 one does
    // not, Platform entries are dropped.
    let one = ext(1, &[(CORE_P2P, vec![ipv4_entry([1, 1, 1, 1], 9999)])]);
    assert_eq!(migration_reject(&one, 3, 2, Regular, &main), None);
    assert_eq!(migration_reject(&good, 3, 2, Evo, &main), None);
    let two =
        ext(1, &[(CORE_P2P, vec![ipv4_entry([1, 1, 1, 1], 9999), ipv4_entry([1, 1, 1, 2], 9999)])]);
    assert_eq!(migration_reject(&two, 3, 2, Regular, &main), reject);
    let ipv6 = [0x26, 0x06, 0x47, 0, 0x47, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x11, 0x11];
    let v6 = ext(1, &[(CORE_P2P, vec![ipv6_entry(ipv6, 9999)])]);
    assert_eq!(migration_reject(&v6, 3, 2, Regular, &main), reject);
    // The legacy object's mainnet-port rule applies to what is carried back.
    let testnet_port = ext(1, &[(CORE_P2P, vec![ipv4_entry([1, 1, 1, 1], 9998)])]);
    assert_eq!(migration_reject(&testnet_port, 3, 2, Regular, &main), reject);
    // A legacy object at another legacy version is rebuilt: its entry must pass again.
    assert_eq!(migration_reject(&mn_ok, 1, 2, Regular, &main), None);
    assert_eq!(migration_reject(&mn_ipv4([1, 1, 1, 1], 9998), 1, 2, Regular, &main), reject);
}
