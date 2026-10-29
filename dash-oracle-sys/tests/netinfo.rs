//! `oracle_netinfo_validate` and `oracle_netinfo_add_entry` against Core's own NetInfo
//! verdicts (ABI 13; `-allowprivatenet` ABI 16).
//!
//! Provenance: the rows are transcribed from Core's `src/test/evo_netinfo_tests.cpp` at the
//! pin (`pins.txt`), each group naming its test case and lines; Core's unit tests run the
//! main-chain tables under `BasicTestingSetup` (mainnet) and the others under
//! `RegTestingSetup`. Rows Core's file does not hold (the Validate() statuses of objects only
//! a payload can carry, which ProUpServ's trivial checks pre-empt) name the line of
//! `src/evo/netinfo.cpp` or `src/netaddress.cpp` that returns the status; every expectation
//! is Core's, none was read off the oracle. The last test checks that the two entries
//! together return every `NetInfoStatus`.

use std::cell::RefCell;
use std::collections::BTreeSet;

use dash_oracle_sys::{
    Chain, ErrorKind, NetAddr, NetInfoAdd, NetInfoKind, NetInfoStatus, Oracle, Pin,
};
use dashcore::VarInt;
use dashcore::consensus::encode::serialize;

use NetInfoKind::{Ext, Mn};
use NetInfoStatus::*;

const CORE_P2P: u8 = 0;
const PLATFORM_P2P: u8 = 1;
const PLATFORM_HTTPS: u8 = 2;

/// The onion address of Core's tables, alone or with a port.
macro_rules! onion {
    () => {
        "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd.onion"
    };
    ($port:literal) => {
        concat!(onion!(), ":", $port)
    };
}

const ONION: &str = onion!();

fn oracle() -> Oracle {
    Oracle::new(Pin::V24_0)
}

/// An empty object of `kind`, as a payload carries it: `MnNetInfo` is an empty `CService`,
/// `ExtNetInfo` its version and an empty map.
fn empty(kind: NetInfoKind) -> Vec<u8> {
    match kind {
        Mn => legacy_service([0; 16], 0),
        Ext => vec![1, 0],
    }
}

/// `CService` without `ADDRV2_FORMAT`: the IPv6(-mapped) address and the big-endian port.
fn legacy_service(addr: [u8; 16], port: u16) -> Vec<u8> {
    let mut out = addr.to_vec();
    out.extend(port.to_be_bytes());
    out
}

/// An `MnNetInfo` holding the IPv4 service `ip:port`.
fn mn_ipv4(ip: [u8; 4], port: u16) -> Vec<u8> {
    legacy_service(ipv4_mapped(ip), port)
}

fn ipv4_mapped(ip: [u8; 4]) -> [u8; 16] {
    let mut addr = [0u8; 16];
    addr[10..12].copy_from_slice(&[0xff, 0xff]);
    addr[12..].copy_from_slice(&ip);
    addr
}

/// A `NetInfoEntry` holding a `CService` (type 0x01, then the BIP155 address and the port).
fn service_entry(bip155_net: u8, addr: &[u8], port: u16) -> Vec<u8> {
    let mut out = vec![0x01, bip155_net, addr.len() as u8];
    out.extend(addr);
    out.extend(port.to_be_bytes());
    out
}

fn ipv4_entry(ip: [u8; 4], port: u16) -> Vec<u8> {
    service_entry(1, &ip, port)
}

/// A `NetInfoEntry` holding a `DomainPort` (type 0x02, the string, the big-endian port);
/// `claimed` overrides the length prefix.
fn domain_entry(domain: &str, port: u16, claimed: Option<usize>) -> Vec<u8> {
    let mut out = vec![0x02];
    out.extend(compact_size(claimed.unwrap_or(domain.len())));
    out.extend(domain.as_bytes());
    out.extend(port.to_be_bytes());
    out
}

fn compact_size(n: usize) -> Vec<u8> {
    serialize(&VarInt(n as u64))
}

/// `ExtNetInfo` of `version` with `purposes` (each a purpose byte and its entries).
fn ext(version: u8, purposes: &[(u8, Vec<Vec<u8>>)]) -> Vec<u8> {
    let mut out = vec![version];
    out.extend(compact_size(purposes.len()));
    for (purpose, entries) in purposes {
        out.push(*purpose);
        out.extend(compact_size(entries.len()));
        for entry in entries {
            out.extend(entry);
        }
    }
    out
}

/// Every status each entry returned, for the coverage test.
#[derive(Default)]
struct Seen {
    validate: BTreeSet<u8>,
    add_entry: BTreeSet<u8>,
}

thread_local! {
    static SEEN: RefCell<Seen> = RefCell::default();
}

fn validate(chain: &Chain, kind: NetInfoKind, netinfo: &[u8]) -> NetInfoStatus {
    let status = oracle().netinfo_validate(chain, kind, netinfo).expect("netinfo_validate");
    SEEN.with(|seen| seen.borrow_mut().validate.insert(status as u8));
    status
}

/// AddEntry on an empty object of `kind`; checks Core's follow-up (the object validates as
/// Success with the entry, as Malformed without) and returns the status and the entry.
fn add(
    chain: &Chain,
    kind: NetInfoKind,
    purpose: u8,
    input: &str,
) -> (NetInfoStatus, Option<Vec<u8>>) {
    let added = add_to_empty(chain, kind, purpose, input.as_bytes());
    let expected = if added.status == Success {
        Success
    } else {
        Malformed
    };
    assert_eq!(validate(chain, kind, &added.netinfo), expected, "{kind:?} {input}");
    if added.status != Success {
        assert_eq!(
            added.netinfo,
            empty(kind),
            "{kind:?} {input}: a failed AddEntry leaves the object"
        );
    }
    (added.status, added.entry)
}

fn add_to(
    chain: &Chain,
    kind: NetInfoKind,
    netinfo: &[u8],
    purpose: u8,
    input: &[u8],
) -> NetInfoAdd {
    let added = oracle()
        .netinfo_add_entry(chain, kind, netinfo, purpose, input)
        .expect("netinfo_add_entry");
    SEEN.with(|seen| seen.borrow_mut().add_entry.insert(added.status as u8));
    added
}

fn add_to_empty(chain: &Chain, kind: NetInfoKind, purpose: u8, input: &[u8]) -> NetInfoAdd {
    add_to(chain, kind, &empty(kind), purpose, input)
}

/// `{purpose, input}, expected MnNetInfo, expected ExtNetInfo`.
type Row = ((u8, &'static str), NetInfoStatus, NetInfoStatus);

fn check_rows(chain: &Chain, rows: &[Row]) {
    for &((purpose, input), mn_status, ext_status) in rows {
        assert_eq!(add(chain, Mn, purpose, input).0, mn_status, "MnNetInfo {purpose} {input}");
        assert_eq!(add(chain, Ext, purpose, input).0, ext_status, "ExtNetInfo {purpose} {input}");
    }
}

/// `addr_vals_main` (evo_netinfo_tests.cpp:28-75; mnnetinfo_rules_main and
/// extnetinfo_rules_main, mainnet).
const ADDR_VALS_MAIN: &[Row] = &[
    ((CORE_P2P, "1.1.1.1:9999"), Success, Success),
    ((CORE_P2P, "1.1.1.1"), Success, BadPort),
    ((CORE_P2P, "1.1.1.1:9998"), BadPort, Success),
    ((CORE_P2P, "127.0.0.1:9999"), NotRoutable, NotRoutable),
    ((CORE_P2P, "10.0.0.1:9999"), NotRoutable, NotRoutable),
    ((CORE_P2P, "0.0.0.0:9999"), BadAddress, BadAddress),
    ((CORE_P2P, "1.1.1.1:99999"), BadInput, BadInput),
    ((CORE_P2P, "[2606:4700:4700::1111]:9999"), BadInput, Success),
    ((CORE_P2P, "example.com:9999"), BadInput, BadInput),
    ((CORE_P2P, onion!(9999)), BadInput, Success),
    ((PLATFORM_P2P, "example.com:9999"), MaxLimit, BadInput),
    ((PLATFORM_P2P, onion!(9999)), MaxLimit, Success),
    ((PLATFORM_HTTPS, "example.com:9999"), MaxLimit, Success),
    ((PLATFORM_HTTPS, onion!(9999)), MaxLimit, Success),
    ((PLATFORM_HTTPS, "1.1.1.1:443"), MaxLimit, Success),
    ((PLATFORM_HTTPS, "[2606:4700:4700::1111]:443"), MaxLimit, Success),
    ((PLATFORM_HTTPS, onion!(443)), MaxLimit, Success),
    ((PLATFORM_HTTPS, "1.1.1.1:80"), MaxLimit, BadPort),
    ((PLATFORM_P2P, "1.1.1.1:443"), MaxLimit, BadPort),
    ((CORE_P2P, "1.1.1.1:443"), BadPort, BadPort),
    ((CORE_P2P, "1.1.1.256:9999"), BadInput, BadInput),
    ((CORE_P2P, ":9999"), BadInput, BadInput),
    ((64, "1.1.1.1:9999"), MaxLimit, MaxLimit),
    ((PLATFORM_P2P, "1.1.1.1:9999"), MaxLimit, Success),
];

/// `addr_vals_reg` (evo_netinfo_tests.cpp:200-210; mnnetinfo_rules_reg and
/// extnetinfo_rules_reg, regtest).
const ADDR_VALS_REG: &[Row] = &[
    ((CORE_P2P, "1.1.1.1:0"), Success, BadPort),
    ((CORE_P2P, "1.1.1.1:9999"), BadPort, Success),
    ((CORE_P2P, "1.1.1.1:22"), Success, BadPort),
];

/// extnetinfo_rules_reg's `domain_vals` (evo_netinfo_tests.cpp:278-289).
const DOMAIN_VALS_REG: &[Row] = &[
    ((PLATFORM_HTTPS, "example.com:80"), MaxLimit, BadPort),
    ((PLATFORM_HTTPS, "example.com:443"), MaxLimit, Success),
    ((PLATFORM_HTTPS, "example.123:443"), MaxLimit, BadInput),
    ((PLATFORM_HTTPS, "somebodys-macbook-pro.local:9998"), MaxLimit, BadInput),
    (
        (PLATFORM_HTTPS, "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd:9998"),
        MaxLimit,
        BadInput,
    ),
];

/// The same table under `-allowprivatenet` (netinfo_rules_main_allowprivatenet,
/// evo_netinfo_tests.cpp:149-157).
#[test]
fn addr_vals_main() {
    check_rows(&Chain::main(), ADDR_VALS_MAIN);
    check_rows(&Chain::new("main", &["-allowprivatenet"]), ADDR_VALS_MAIN);
}

#[test]
fn addr_vals_reg() {
    check_rows(&Chain::regtest(), ADDR_VALS_REG);
    check_rows(&Chain::regtest(), DOMAIN_VALS_REG);
}

/// The entry Core adds is returned in Core's serialization (`ADDRV2_FORMAT` whatever the
/// stream), and an `MnNetInfo` writes back as the plain `CService` (cservice_compatible,
/// evo_netinfo_tests.cpp:445-475, mainnet).
#[test]
fn entries_and_cservice_compatibility() {
    let chain = Chain::main();
    for input in ["1.1.1.1:9999", "1.1.1.1"] {
        let added = add_to_empty(&chain, Mn, CORE_P2P, input.as_bytes());
        assert_eq!(added.status, Success);
        assert_eq!(added.netinfo, mn_ipv4([1, 1, 1, 1], 9999), "{input}");
        assert_eq!(added.entry, Some(ipv4_entry([1, 1, 1, 1], 9999)), "{input}");
    }
    for input in ["example.com", "[2606:4700:4700::1111]:9999"] {
        let added = add_to_empty(&chain, Mn, CORE_P2P, input.as_bytes());
        assert_eq!((added.status, added.entry), (BadInput, None), "{input}");
        assert_eq!(added.netinfo, empty(Mn), "{input}");
    }

    // ExtNetInfo appends under the purpose: version 1, one purpose, one entry.
    let added = add_to_empty(&chain, Ext, CORE_P2P, b"1.1.1.1:9999");
    let entry = ipv4_entry([1, 1, 1, 1], 9999);
    assert_eq!(added.entry.as_ref(), Some(&entry));
    assert_eq!(added.netinfo, ext(1, &[(CORE_P2P, vec![entry])]));
    let added = add_to_empty(&chain, Ext, PLATFORM_HTTPS, b"example.com:443");
    assert_eq!(added.entry, Some(domain_entry("example.com", 443, None)));
}

/// mnnetinfo_rules_main (evo_netinfo_tests.cpp:127-144): one entry, Core P2P only.
#[test]
fn mnnetinfo_limits() {
    let chain = Chain::main();
    let one = add_to_empty(&chain, Mn, CORE_P2P, b"1.1.1.1:9999");
    assert_eq!(one.status, Success);
    let again = add_to(&chain, Mn, &one.netinfo, CORE_P2P, b"1.1.1.2:9999");
    assert_eq!((again.status, again.entry), (MaxLimit, None));
    assert_eq!(again.netinfo, one.netinfo);
    for purpose in [PLATFORM_HTTPS, PLATFORM_P2P] {
        assert_eq!(add_to_empty(&chain, Mn, purpose, b"1.1.1.1:9999").status, MaxLimit);
    }
}

/// extnetinfo_rules_reg (evo_netinfo_tests.cpp:236-273, regtest): four entries per purpose,
/// and duplicates within and across lists.
#[test]
fn extnetinfo_limits_and_duplicates() {
    let chain = Chain::regtest();
    let mut netinfo = empty(Ext);
    for idx in 1..=4 {
        let added = add_to(&chain, Ext, &netinfo, CORE_P2P, format!("1.1.1.{idx}:9998").as_bytes());
        assert_eq!(added.status, Success, "entry {idx}");
        netinfo = added.netinfo;
    }
    assert_eq!(add_to(&chain, Ext, &netinfo, CORE_P2P, b"1.1.1.5:9998").status, MaxLimit);
    let added = add_to(&chain, Ext, &netinfo, PLATFORM_P2P, b"1.1.1.5:9998");
    assert_eq!(added.status, Success);
    assert_eq!(validate(&chain, Ext, &added.netinfo), Success);

    let first = add_to_empty(&chain, Ext, CORE_P2P, b"1.1.1.1:9998");
    assert_eq!(first.status, Success);
    for (purpose, input, expected) in [
        (CORE_P2P, "1.1.1.1:9998", Duplicate),
        (CORE_P2P, "1.1.1.1:9997", Duplicate),
        (PLATFORM_P2P, "1.1.1.1:9998", Duplicate),
        (PLATFORM_P2P, "1.1.1.1:9997", Success),
    ] {
        let added = add_to(&chain, Ext, &first.netinfo, purpose, input.as_bytes());
        assert_eq!(added.status, expected, "{purpose} {input}");
    }
}

/// `privacy_addr_vals` (evo_netinfo_tests.cpp:218-228), stored by ExtNetInfo under Core P2P
/// (extnetinfo_rules_reg, regtest).
#[test]
fn privacy_addr_vals() {
    let chain = Chain::regtest();
    let i2p = "udhdrtrcetjm5sxzskjyr5ztpeszydbh4dpl3pl4utgqqw2v4jna.b32.i2p";
    for (input, expected) in [
        ("[fc00:3344:5566:7788:9900:aabb:ccdd:eeff]:9998".to_string(), Success),
        (format!("{i2p}:0"), Success),
        (format!("{ONION}:9998"), Success),
        (format!("{i2p}:9998"), BadPort),
        (format!("{ONION}:0"), BadPort),
    ] {
        assert_eq!(add(&chain, Ext, CORE_P2P, &input).0, expected, "{input}");
    }
    // The CJDNS entry is stored as BIP155 network 6 (MaybeFlipIPv6toCJDNS).
    let (_, entry) = add(&chain, Ext, CORE_P2P, "[fc00:3344:5566:7788:9900:aabb:ccdd:eeff]:9998");
    let cjdns =
        [0xfc, 0, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];
    assert_eq!(entry, Some(service_entry(6, &cjdns, 9998)));
}

/// domainport_rules (evo_netinfo_tests.cpp:516-550): the two domains `DomainPort::Set()`
/// accepts are stored for Platform HTTPS. Core's test passes the refused ones to
/// `DomainPort::Set()` itself; through AddEntry each is BadInput. An input of hex digits and
/// dots only ("abcd", "abc.", ".abc", "ac") is parsed as an address and fails `Lookup()`
/// (netinfo.cpp:546), so each also appears with other letters ("wxyz", ...), which reach
/// `DomainPort::Set()` and its refusal (netinfo.cpp:526, 534); characters outside RFC 1035
/// fail before either (netinfo.cpp:509-511).
#[test]
fn domainport_rules() {
    let chain = Chain::main();
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
        assert_eq!(add(&chain, Ext, PLATFORM_HTTPS, &input).0, expected, "{domain}");
    }
    assert_eq!(add(&chain, Ext, PLATFORM_HTTPS, ":0").0, BadInput);
}

/// extnetinfo_validate_deser (evo_netinfo_tests.cpp:663-798, regtest): what only a
/// deserialized object can hold, and the purpose order its reader refuses (:725-756).
#[test]
fn extnetinfo_validate_deser() {
    let chain = Chain::regtest();
    let ok = || ipv4_entry([1, 1, 1, 1], 9998);
    let twice = ext(1, &[(CORE_P2P, vec![ok(), ok()])]);
    assert_eq!(validate(&chain, Ext, &twice), Duplicate);

    let five = (1..=5).map(|i| ipv4_entry([1, 1, 1, i], 9998)).collect();
    assert_eq!(validate(&chain, Ext, &ext(1, &[(CORE_P2P, five)])), MaxLimit);

    let domain = ext(1, &[(CORE_P2P, vec![domain_entry("example.com", 443, None)])]);
    assert_eq!(validate(&chain, Ext, &domain), BadInput);

    // A repeated purpose, a descending pair and an unknown purpose fail the read
    // (netinfo.h:431-433), even where the repeat's entry would only be cleared.
    for netinfo in [
        ext(1, &[(CORE_P2P, vec![ok()]), (CORE_P2P, vec![vec![0x03]])]),
        ext(1, &[(PLATFORM_HTTPS, vec![ok()]), (CORE_P2P, vec![ok()])]),
        ext(1, &[(3, vec![ok()])]),
    ] {
        let err = oracle().netinfo_validate(&chain, Ext, &netinfo).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Decode, "{netinfo:02x?}");
        assert_eq!(err.message, "ExtNetInfo: purpose unknown or out of order: iostream error");
    }

    // A cleared entry under a known purpose reads but does not validate: an unknown type, the
    // explicit invalid marker, the unspecified address and a truncated service.
    for entry in [vec![0x03], vec![0xff], service_entry(2, &[0; 16], 0), vec![0x01, 2, 16]] {
        let netinfo = ext(1, &[(CORE_P2P, vec![entry.clone()])]);
        assert_eq!(validate(&chain, Ext, &netinfo), Malformed, "{entry:02x?}");
    }

    for (purpose, expected) in
        [(PLATFORM_HTTPS, Success), (PLATFORM_P2P, BadPort), (CORE_P2P, BadPort)]
    {
        let netinfo = ext(1, &[(purpose, vec![ipv4_entry([1, 1, 1, 1], 443)])]);
        assert_eq!(validate(&chain, Ext, &netinfo), expected, "purpose {purpose}");
    }
}

/// Validate() on objects ProUpServ's trivial checks reject first (providertx.cpp:166-185,
/// 421-428: an empty object, one without Core P2P entries), on objects with a cleared entry
/// (which only Validate() refuses since rc.4), and MnNetInfo's rules on a deserialized
/// service. Expectations cite the line returning them.
#[test]
fn validate_rows_trivial_checks_preempt() {
    let main = Chain::main();
    // MnNetInfo: an empty or invalid service is a trivially invalid entry (netinfo.cpp:353).
    assert_eq!(validate(&main, Mn, &empty(Mn)), Malformed);
    assert_eq!(validate(&main, Mn, &mn_ipv4([0, 0, 0, 0], 9999)), Malformed);
    // A valid non-IPv4 service deserializes, then fails the type rule (netinfo.cpp:291).
    let ipv6 = [0x26, 0x06, 0x47, 0, 0x47, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x11, 0x11];
    assert_eq!(validate(&main, Mn, &legacy_service(ipv6, 9999)), BadType);
    assert_eq!(validate(&main, Mn, &mn_ipv4([127, 0, 0, 1], 9999)), NotRoutable);
    assert_eq!(validate(&main, Mn, &mn_ipv4([1, 1, 1, 1], 9998)), BadPort);
    assert_eq!(validate(&main, Mn, &mn_ipv4([1, 1, 1, 1], 9999)), Success);
    // Not on mainnet, the mainnet port is the bad one (netinfo.cpp:298).
    let reg = Chain::regtest();
    assert_eq!(validate(&reg, Mn, &mn_ipv4([1, 1, 1, 1], 9999)), BadPort);

    // ExtNetInfo (netinfo.cpp:585-623).
    let ok = || ipv4_entry([1, 1, 1, 1], 9999);
    assert_eq!(validate(&main, Ext, &empty(Ext)), Malformed);
    // Versions 0 and above 1 stop after the version byte (netinfo.h:423).
    assert_eq!(validate(&main, Ext, &[0]), Malformed);
    assert_eq!(validate(&main, Ext, &[2]), Malformed);
    assert_eq!(validate(&main, Ext, &ext(1, &[(CORE_P2P, vec![])])), Malformed);
    // An unknown entry type, an invalid service (0.0.0.0, the internal-in-IPv6 prefix that
    // BIP155 refuses: netaddress.cpp) and a domain DomainPort::Validate() refuses all read
    // back as the empty entry (netinfo.h:242-265), which Validate() refuses (netinfo.cpp:607-617).
    for entry in [
        vec![0x03],
        ipv4_entry([0, 0, 0, 0], 9999),
        service_entry(2, &[0xfd, 0x6b, 0x88, 0xc0, 0x87, 0x24, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1], 9999),
        domain_entry("abcd", 443, None),
    ] {
        let netinfo = ext(1, &[(CORE_P2P, vec![ok()]), (PLATFORM_HTTPS, vec![entry.clone()])]);
        assert_eq!(validate(&main, Ext, &netinfo), Malformed, "{entry:02x?}");
    }
    // No Core P2P entry (ProUpServ: bad-protx-netinfo-empty first).
    let platform_only = ext(1, &[(PLATFORM_P2P, vec![ipv4_entry([1, 1, 1, 1], 443)])]);
    assert_eq!(validate(&main, Ext, &platform_only), BadPort);
    // A routable-only rule under mainnet (netinfo.cpp:456).
    let local = ext(1, &[(CORE_P2P, vec![ipv4_entry([10, 0, 0, 1], 9999)])]);
    assert_eq!(validate(&main, Ext, &local), NotRoutable);
    // A bad TLD under Platform HTTPS (netinfo.cpp:485-487).
    let tld = ext(
        1,
        &[(CORE_P2P, vec![ok()]), (PLATFORM_HTTPS, vec![domain_entry("host.local", 443, None)])],
    );
    assert_eq!(validate(&main, Ext, &tld), BadInput);
}

/// `-allowprivatenet` (ABI 16): Core's startup assigns it to `fAllowPrivateNet` (init.cpp:1276),
/// which `CService::IsRoutable()` reads (netaddress.cpp:468). The NetInfo rules refuse an RFC1918
/// or a loopback address on mainnet whatever it holds (netinfo.cpp:294, 456;
/// netinfo_rules_main_allowprivatenet, evo_netinfo_tests.cpp:149-157), so `GetGroup()` shows the
/// value the selection assigned: the unroutable class without it, the address's /16 with it
/// (netgroup.cpp:32, 43-50). The option is part of the chain selection, so in one process: the
/// default, allowed, the default again (an explicit `=0` is the default), for both kinds through
/// both entries.
#[test]
fn allow_private_net_is_a_chain_option() {
    let default = Chain::main();
    let allowed = Chain::new("main", &["-allowprivatenet"]);
    let disallowed = Chain::new("main", &["-allowprivatenet=0"]);
    let private = |chain: &Chain| {
        let ext_netinfo = ext(1, &[(CORE_P2P, vec![ipv4_entry([10, 0, 0, 1], 9999)])]);
        [
            validate(chain, Mn, &mn_ipv4([10, 0, 0, 1], 9999)),
            validate(chain, Ext, &ext_netinfo),
            add(chain, Mn, CORE_P2P, "10.0.0.1:9999").0,
            add(chain, Ext, CORE_P2P, "192.168.1.1:9999").0,
        ]
    };
    let (unroutable, slash16) = (vec![0], vec![1, 10, 0]);
    for (chain, group) in [
        (&default, &unroutable),
        (&allowed, &slash16),
        (&default, &unroutable),
        (&allowed, &slash16),
        (&disallowed, &unroutable),
    ] {
        assert_eq!(private(chain), [NotRoutable; 4], "{:?}", chain.args);
        assert_eq!(validate(chain, Mn, &mn_ipv4([127, 0, 0, 1], 9999)), NotRoutable);
        assert_eq!(add(chain, Ext, CORE_P2P, "127.0.0.1:9999").0, NotRoutable);
        // On the chain itself: another test's selection may come between two calls (BL-436).
        let (got, _) =
            oracle().netgroup_on(chain, &NetAddr::ipv4([10, 0, 0, 1]), &[]).unwrap().unwrap();
        assert_eq!(&got, group, "{:?}", chain.args);
    }
    // Testnet requires a routable address too (CE4-02 holds on every network that does).
    for args in [&[][..], &["-allowprivatenet"], &["-allowprivatenet=0"]] {
        assert_eq!(private(&Chain::new("test", args)), [NotRoutable; 4], "test {args:?}");
    }
}

/// Core's deserializer refusals reach the caller as decode errors: the oversized domains of
/// netinfoentry_domain_deser_rejects_oversized_string, extnetinfo_domain_amplification_rejected
/// and domain_port_wire_compatibility (evo_netinfo_tests.cpp:608-661, 800-822); bytes after
/// the object are refused as GetTxPayload() refuses them.
#[test]
fn decode_refusals() {
    let oracle = oracle();
    let chain = Chain::main();
    let err =
        |netinfo: &[u8], kind| oracle.netinfo_validate(&chain, kind, netinfo).unwrap_err().kind;

    let long = "a".repeat(1000);
    let oversized = ext(1, &[(PLATFORM_HTTPS, vec![domain_entry(&long, 443, None)])]);
    assert_eq!(err(&oversized, Ext), ErrorKind::Decode);
    let amplified: Vec<Vec<u8>> =
        (0..8).map(|_| [vec![0x02], compact_size(1 << 16)].concat()).collect();
    assert_eq!(err(&ext(1, &[(PLATFORM_HTTPS, amplified)]), Ext), ErrorKind::Decode);
    let just_over = ext(1, &[(PLATFORM_HTTPS, vec![domain_entry(&"a".repeat(254), 443, None)])]);
    assert_eq!(err(&just_over, Ext), ErrorKind::Decode);

    assert_eq!(err(&[empty(Ext), vec![0]].concat(), Ext), ErrorKind::Trailing);
    assert_eq!(err(&[empty(Mn), vec![0]].concat(), Mn), ErrorKind::Trailing);
    assert_eq!(err(&[0; 5], Mn), ErrorKind::Decode);

    let added = oracle.netinfo_add_entry(&chain, Mn, &[0; 5], CORE_P2P, b"1.1.1.1:9999");
    assert_eq!(added.unwrap_err().kind, ErrorKind::Decode);
}

/// The input reaches AddEntry as raw bytes: a NUL or a non-UTF-8 byte fails Core's
/// character filters (netinfo.cpp:317-318, 509-511), and no bytes fail `Lookup()`
/// (netinfo.cpp:329, 546).
#[test]
fn raw_input_bytes() {
    let chain = Chain::main();
    for input in [&b"1.1.1.1\0:9999"[..], b"1.1.1.1:9999\xff", b""] {
        for kind in [Mn, Ext] {
            assert_eq!(add_to_empty(&chain, kind, CORE_P2P, input).status, BadInput, "{input:?}");
        }
    }
}

/// Run every row on this thread, then check the two entries together return every status.
#[test]
fn every_status_is_reached() {
    addr_vals_main();
    addr_vals_reg();
    entries_and_cservice_compatibility();
    mnnetinfo_limits();
    extnetinfo_limits_and_duplicates();
    privacy_addr_vals();
    domainport_rules();
    extnetinfo_validate_deser();
    validate_rows_trivial_checks_preempt();
    raw_input_bytes();
    SEEN.with(|seen| {
        let seen = seen.borrow();
        let all: BTreeSet<u8> = seen.validate.union(&seen.add_entry).copied().collect();
        assert_eq!(
            all,
            (0..=8).collect(),
            "validate {:?}, add_entry {:?}",
            seen.validate,
            seen.add_entry
        );
        // AddEntry never returns Malformed or BadType (netinfo.cpp:307-330, 496-547);
        // Validate() never BadAddress (the reader clears an invalid service, netinfo.h:251-252).
        assert_eq!(seen.add_entry, BTreeSet::from([0, 1, 2, 3, 4, 6, 8]));
        assert_eq!(seen.validate, BTreeSet::from([0, 1, 2, 4, 5, 6, 7, 8]));
    });
}
