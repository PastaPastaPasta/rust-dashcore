//! L0 vectors for the provider network fields (EV1 §3.15, §6.1): every reject row of
//! `providertx.cpp:166-263`, with Core's class and reason, plus provider_network_fields_main
//! (evo_netinfo_tests.cpp:148-186) and the empty-netInfo platform-port cases (D31). The census
//! closure test (`reasons::tests`) checks that each row here is a census row.

use dash_consensus::validation::TxValidationResult;
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_primitives::{NetInfo, NetInfoV1, NetInfoV2, PlatformNodeId};
use dash_types::Numeric as _;
use dash_types::codec::BaseCodec;

use super::*;
use crate::netinfo::NetInfoConfig;

use Network::{Devnet, Main, Regtest, Testnet3};

fn rules(kind: Network) -> NetInfoRules {
    let params = |k| ChainParamsBuilder::new(k, Release::V24_0).build().expect("Core's parameters");
    let config = NetInfoConfig {
        cjdns_reachable: true,
    };
    NetInfoRules::new(&params(kind), &params(Main), config)
}

fn decode<T: BaseCodec>(bytes: &[u8]) -> T {
    let mut rest = bytes;
    let value = T::decode(&mut rest).expect("decodes");
    assert!(rest.is_empty());
    value
}

fn mn(addr: [u8; 16], port: u16) -> NetInfo {
    let mut wire = addr.to_vec();
    wire.extend(port.to_be_bytes());
    NetInfo::Legacy(decode::<NetInfoV1>(&wire))
}

pub(crate) fn mn_ipv4(ip: [u8; 4], port: u16) -> NetInfo {
    let mut addr = [0; 16];
    addr[10..12].copy_from_slice(&[0xff, 0xff]);
    addr[12..].copy_from_slice(&ip);
    mn(addr, port)
}

pub(crate) fn empty_mn() -> NetInfo {
    NetInfo::Legacy(decode::<NetInfoV1>(&[0; 18]))
}

pub(crate) fn ipv4_entry(ip: [u8; 4], port: u16) -> Vec<u8> {
    let mut out = vec![0x01, 1, 4];
    out.extend(ip);
    out.extend(port.to_be_bytes());
    out
}

fn domain_entry(domain: &str, port: u16) -> Vec<u8> {
    let mut out = vec![0x02, domain.len() as u8];
    out.extend(domain.as_bytes());
    out.extend(port.to_be_bytes());
    out
}

pub(crate) fn ext(purposes: &[(u8, Vec<Vec<u8>>)]) -> NetInfo {
    let mut wire = vec![1, purposes.len() as u8];
    for (purpose, entries) in purposes {
        wire.push(*purpose);
        wire.push(entries.len() as u8);
        wire.extend(entries.iter().flatten());
    }
    NetInfo::Extended(decode::<NetInfoV2>(&wire))
}

fn empty_ext() -> NetInfo {
    ext(&[])
}

/// A complete EvoNode ExtNetInfo on mainnet: Core P2P, Platform P2P and Platform HTTPS, each on
/// one IPv4 address and its purpose's standard port.
fn ext_evo() -> NetInfo {
    ext(&[
        (0, vec![ipv4_entry([1, 1, 1, 1], 9999)]),
        (1, vec![ipv4_entry([1, 1, 1, 1], 26656)]),
        (2, vec![ipv4_entry([1, 1, 1, 1], 443)]),
    ])
}

const NODE_ID: [u8; 20] = [7; 20];

struct Case {
    net_info: NetInfo,
    mn_type: MnType,
    version: u16,
    node_id: Option<[u8; 20]>,
    ports: (u16, u16),
    allow_empty: bool,
    ipv4_optional: bool,
}

impl Case {
    fn regular(net_info: NetInfo, version: u16) -> Self {
        Self {
            net_info,
            mn_type: MnType::Regular,
            version,
            node_id: None,
            ports: (0, 0),
            allow_empty: false,
            ipv4_optional: false,
        }
    }

    fn evo(net_info: NetInfo, version: u16, ports: (u16, u16)) -> Self {
        Self {
            net_info,
            mn_type: MnType::Evo,
            version,
            node_id: Some(NODE_ID),
            ports,
            allow_empty: false,
            ipv4_optional: false,
        }
    }

    fn check(&self, kind: Network) -> Option<(TxValidationResult, String)> {
        let node_id = self.node_id.map(PlatformNodeId::from);
        let fields = ProviderNetworkFields {
            net_info: &self.net_info,
            mn_type: self.mn_type,
            version: self.version,
            platform_node_id: node_id.as_ref(),
            platform_p2p_port: self.ports.0,
            platform_http_port: self.ports.1,
        };
        let res = check_provider_network_fields(
            &fields,
            self.allow_empty,
            self.ipv4_optional,
            &rules(kind),
        );
        res.err().map(|e| (e.result(), e.reject_reason().to_owned()))
    }
}

const S: TxValidationResult = TxValidationResult::BadSpecial;
const C: TxValidationResult = TxValidationResult::Consensus;

/// The rows of `providertx.cpp:166-263` this file reaches, by census line, and the line each
/// vector below asserts. `reasons::tests::network_rows_are_census_rows` checks these against the
/// census.
pub(crate) const REACHED: &[(u32, TxValidationResult, &str)] = &[
    (170, S, BAD_PROTX_NETINFO_EMPTY),
    (175, S, BAD_PROTX_NETINFO_BAD),
    (181, S, BAD_PROTX_NETINFO_EMPTY),
    (193, S, BAD_PROTX_NETINFO_PORT),
    (195, S, BAD_PROTX_NETINFO_ADDR_TYPE),
    (197, S, BAD_PROTX_NETINFO_ADDR_UNROUTABLE),
    (199, S, BAD_PROTX_NETINFO_BAD),
    (203, S, BAD_PROTX_NETINFO_ENTRY),
    (205, S, BAD_PROTX_DUP_NETINFO_ENTRY),
    (207, S, BAD_PROTX_NETINFO_MAXLIMIT),
    (217, C, BAD_PROTX_NETINFO_VERSION),
    (221, S, BAD_PROTX_NETINFO_EMPTY),
    (231, S, BAD_PROTX_PLATFORM_NODEID),
    (235, S, BAD_PROTX_PLATFORM_P2P_PORT),
    (238, S, BAD_PROTX_PLATFORM_HTTP_PORT),
    (245, S, BAD_PROTX_PLATFORM_P2P_PORT),
    (248, S, BAD_PROTX_PLATFORM_HTTP_PORT),
    (252, S, BAD_PROTX_PLATFORM_P2P_PORT),
    (255, S, BAD_PROTX_PLATFORM_HTTP_PORT),
    (260, S, BAD_PROTX_PLATFORM_DUP_PORTS),
];

fn expect(line: u32) -> Option<(TxValidationResult, String)> {
    let &(_, class, reason) = REACHED.iter().find(|r| r.0 == line).expect("a REACHED row");
    Some((class, reason.to_owned()))
}

#[test]
fn every_row_has_a_vector() {
    let ext3 = |entries: &[(u8, Vec<Vec<u8>>)]| Case::regular(ext(entries), 3);
    let core = |e: Vec<u8>| (0, vec![e]);
    let ok = || ipv4_entry([1, 1, 1, 1], 9999);
    let cases: Vec<(u32, Network, Case)> = vec![
        // IsNetInfoTriviallyValid.
        (170, Main, ext3(&[(1, vec![ipv4_entry([1, 1, 1, 2], 26656)])])),
        (175, Main, ext3(&[core(ok()), (1, vec![ipv4_entry([1, 1, 1, 2], 26656)])])),
        (
            181,
            Main,
            Case::evo(ext(&[core(ok()), (1, vec![ipv4_entry([1, 1, 1, 2], 26656)])]), 3, (0, 0)),
        ),
        // CheckNetInfo.
        (193, Main, Case::regular(mn_ipv4([1, 1, 1, 1], 9998), 2)),
        // Only a legacy object can hold a valid service of the wrong type: ExtNetInfo admits
        // every network a valid BIP155 service can be on.
        (
            195,
            Main,
            Case::regular(
                mn([0x26, 6, 0x47, 0, 0x47, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x11, 0x11], 9999),
                2,
            ),
        ),
        (197, Main, Case::regular(mn_ipv4([10, 0, 0, 1], 9999), 2)),
        (199, Main, ext3(&[core(vec![0x03])])),
        (203, Main, ext3(&[core(domain_entry("x.example.com", 443))])),
        (205, Main, ext3(&[(0, vec![ok(), ok()])])),
        (207, Main, ext3(&[(0, (1..=5).map(|i| ipv4_entry([1, 1, 1, i], 9999)).collect())])),
        // CheckProviderNetworkFields.
        (217, Main, Case::regular(mn_ipv4([1, 1, 1, 1], 9999), 3)),
        (217, Main, Case::regular(ext(&[core(ok())]), 2)),
        (221, Main, Case::regular(empty_mn(), 2)),
        (231, Main, Case::evo(ext_evo(), 3, (0, 0)).node_id([0; 20])),
        (235, Main, Case::evo(ext_evo(), 3, (26656, 0))),
        (238, Main, Case::evo(ext_evo(), 3, (0, 443))),
        (245, Main, Case::evo(mn_ipv4([1, 1, 1, 1], 9999), 2, (26657, 443))),
        (248, Main, Case::evo(mn_ipv4([1, 1, 1, 1], 9999), 2, (26656, 444))),
        (252, Testnet3, Case::evo(mn_ipv4([1, 1, 1, 1], 19999), 2, (9999, 22001))),
        (255, Testnet3, Case::evo(mn_ipv4([1, 1, 1, 1], 19999), 2, (22000, 9999))),
        (260, Testnet3, Case::evo(mn_ipv4([1, 1, 1, 1], 19999), 2, (22000, 22000))),
        (260, Testnet3, Case::evo(mn_ipv4([1, 1, 1, 1], 19999), 2, (19999, 22001))),
        (260, Regtest, Case::evo(mn_ipv4([1, 1, 1, 1], 19899), 2, (22200, 19899))),
    ];
    for (line, kind, case) in &cases {
        assert_eq!(case.check(*kind), expect(*line), "providertx.cpp:{line} on {kind:?}");
    }
    let lines: std::collections::BTreeSet<_> = cases.iter().map(|c| c.0).collect();
    assert_eq!(lines, REACHED.iter().map(|r| r.0).collect(), "every REACHED row has a vector");
}

impl Case {
    fn node_id(mut self, id: [u8; 20]) -> Self {
        self.node_id = Some(id);
        self
    }
}

/// provider_network_fields_main (evo_netinfo_tests.cpp:148-186): an EvoNode v2 on mainnet with
/// the mainnet ports, then each port wrong in turn; an ExtAddr EvoNode with an unroutable Core
/// P2P address.
#[test]
fn provider_network_fields_main() {
    let net_info = mn_ipv4([1, 1, 1, 1], 9999);
    let case = |ports| Case::evo(net_info.clone(), 2, ports).check(Main);
    assert_eq!(case((26656, 443)), None);
    assert_eq!(case((26657, 443)), expect(245));
    assert_eq!(case((26656, 444)), expect(248));
    assert_eq!(case((9999, 443)), expect(245));

    let ext_local = ext(&[
        (0, vec![ipv4_entry([127, 0, 0, 1], 9999)]),
        (1, vec![ipv4_entry([1, 1, 1, 2], 22200)]),
        (2, vec![domain_entry("server.example.com", 443)]),
    ]);
    assert_eq!(Case::evo(ext_local, 3, (0, 0)).check(Main), expect(197));
}

/// EV1 §6.1 "Empty netInfo platform ports" (D31), the `CheckProviderNetworkFields` half: an
/// empty netInfo's primary port is 0 (`CService{}`), so a zero platform port is a duplicate of
/// it off mainnet; on mainnet the port rule rejects it first; ExtAddr returns before the ports.
#[test]
fn empty_net_info_platform_ports() {
    let case = |kind, version, ports, net_info: NetInfo| {
        let mut case = Case::evo(net_info, version, ports);
        case.allow_empty = true;
        case.check(kind)
    };
    for kind in [Testnet3, Regtest, Devnet] {
        assert_eq!(case(kind, 2, (0, 22001), empty_mn()), expect(260), "{kind:?}");
        assert_eq!(case(kind, 2, (22001, 0), empty_mn()), expect(260), "{kind:?}");
        assert_eq!(case(kind, 2, (22000, 22001), empty_mn()), None, "{kind:?}");
    }
    assert_eq!(case(Main, 2, (0, 443), empty_mn()), expect(245));
    assert_eq!(case(Testnet3, 3, (0, 0), empty_ext()), None);
    // allow_empty only covers the empty object; a regular node passes without platform fields.
    let mut regular = Case::regular(empty_ext(), 3);
    regular.allow_empty = true;
    assert_eq!(regular.check(Main), None);
}

/// The step order of `CheckProviderNetworkFields`: version kind, then the netInfo, then the
/// regular-node accept, then the EvoNode fields.
#[test]
fn step_order() {
    // A non-Evo node skips the platform checks entirely.
    let mut case = Case::regular(mn_ipv4([1, 1, 1, 1], 9999), 2);
    case.ports = (1, 1);
    case.node_id = Some([0; 20]);
    assert_eq!(case.check(Main), None);
    // The netInfo rejects come before the node id.
    let case = Case::evo(mn_ipv4([1, 1, 1, 1], 9998), 2, (26656, 443)).node_id([0; 20]);
    assert_eq!(case.check(Main), expect(193));
    // Trivial validity comes before Validate(): a Platform entry on a regular node with a bad
    // Core P2P port is bad-protx-netinfo-bad (175), not -port.
    let both = ext(&[
        (0, vec![ipv4_entry([1, 1, 1, 1], 1)]),
        (2, vec![domain_entry("a.example.com", 443)]),
    ]);
    assert_eq!(Case::regular(both, 3).check(Main), expect(175));
    // The version-kind check precedes everything, even an empty object.
    let mut case = Case::regular(empty_ext(), 2);
    case.allow_empty = true;
    assert_eq!(case.check(Main), expect(217));
    // Core's null pointer (RPC only) skips the node-id check; the null id does not.
    let mut case = Case::evo(ext_evo(), 3, (0, 0));
    case.node_id = None;
    assert_eq!(case.check(Main), None);
    case.node_id = Some([0; 20]);
    assert_eq!(case.check(Main), expect(231));
}

/// Every NetInfoStatus maps to its `CheckNetInfo` reason (providertx.cpp:187-210), including
/// BadAddress, which no Validate() returns (census: unreachable).
#[test]
fn check_net_info_maps_every_status() {
    use crate::netinfo::NetInfoStatus;
    let pairs = [
        (NetInfoStatus::BadAddress, BAD_PROTX_NETINFO_ADDR),
        (NetInfoStatus::BadPort, BAD_PROTX_NETINFO_PORT),
        (NetInfoStatus::BadType, BAD_PROTX_NETINFO_ADDR_TYPE),
        (NetInfoStatus::NotRoutable, BAD_PROTX_NETINFO_ADDR_UNROUTABLE),
        (NetInfoStatus::Malformed, BAD_PROTX_NETINFO_BAD),
        (NetInfoStatus::BadInput, BAD_PROTX_NETINFO_ENTRY),
        (NetInfoStatus::Duplicate, BAD_PROTX_DUP_NETINFO_ENTRY),
        (NetInfoStatus::MaxLimit, BAD_PROTX_NETINFO_MAXLIMIT),
    ];
    for (status, reason) in pairs {
        let err = status_reject(status).expect_err("a reject");
        assert_eq!((err.result(), err.reject_reason()), (S, reason), "{status:?}");
    }
    assert!(status_reject(NetInfoStatus::Success).is_ok());
}

/// `(purpose, "addr:port")` entries in insertion order.
type Entries = Vec<(u8, String)>;

/// One [`Entries`] item.
fn s(purpose: u8, input: &str) -> (u8, String) {
    (purpose, input.to_owned())
}

const IPV6: &str = "[2606:4700:4700::1111]";
const ONION: &str = "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd.onion";
const DOMAIN: &str = "platform.example.com";

/// An ExtNetInfo built with AddEntry under `kind`'s rules, each entry accepted.
fn added(kind: Network, entries: &[(u8, String)]) -> NetInfo {
    let mut net_info = empty_ext();
    for (purpose, input) in entries {
        let purpose = dash_primitives::NIPurpose::from_base(*purpose);
        let status = crate::netinfo::add_entry(&mut net_info, purpose, input, &rules(kind));
        assert_eq!(status, crate::netinfo::NetInfoStatus::Success, "{input} on {kind:?}");
    }
    net_info
}

/// `CheckProviderNetworkFields` as Core's ExtAddr cases call it: no node id, zero platform ports,
/// `allow_empty` false.
fn ext_fields(net_info: &NetInfo, mn_type: MnType, ipv4_optional: bool) -> Case {
    let mut case = Case::regular(net_info.clone(), 3);
    case.mn_type = mn_type;
    case.ipv4_optional = ipv4_optional;
    case
}

/// `ipv4_primaries_follow_activation` (evo_netinfo_tests.cpp v24.0.0:366-434): while IPV4_OPTIONAL
/// is not active, the first Core P2P entry must be IPv4 and the first Platform entries IPv4 on its
/// address; once active, none of that applies. The rule follows the activation, not the network:
/// the same objects give the same verdicts on testnet and mainnet, and `Validate()` accepts them
/// all.
#[test]
fn ipv4_primaries_follow_activation() {
    use MnType::{Evo, Regular};
    let vals: Vec<(MnType, Entries, Option<&str>)> = vec![
        (
            Regular,
            vec![s(0, &format!("{IPV6}:9999")), s(0, "1.1.1.1:9999")],
            Some(BAD_PROTX_NETINFO_ADDR_TYPE),
        ),
        (Regular, vec![s(0, &format!("{ONION}:9999"))], Some(BAD_PROTX_NETINFO_ADDR_TYPE)),
        (
            Evo,
            vec![s(0, "1.1.1.1:9999"), s(1, "1.1.1.2:26656"), s(2, "1.1.1.1:443")],
            Some(BAD_PROTX_NETINFO_ADDR),
        ),
        (
            Evo,
            vec![s(0, "1.1.1.1:9999"), s(1, "1.1.1.1:26656"), s(2, "1.1.1.2:443")],
            Some(BAD_PROTX_NETINFO_ADDR),
        ),
        (
            Evo,
            vec![
                s(0, "1.1.1.1:9999"),
                s(0, &format!("{IPV6}:9999")),
                s(1, &format!("{IPV6}:26656")),
                s(2, "1.1.1.1:443"),
            ],
            Some(BAD_PROTX_NETINFO_ADDR_TYPE),
        ),
        (
            Evo,
            vec![s(0, "1.1.1.1:9999"), s(1, "1.1.1.1:26656"), s(2, &format!("{DOMAIN}:443"))],
            Some(BAD_PROTX_NETINFO_ADDR_TYPE),
        ),
        (
            Evo,
            vec![
                s(0, "1.1.1.1:9999"),
                s(0, &format!("{IPV6}:9999")),
                s(1, "1.1.1.1:26656"),
                s(1, &format!("{ONION}:26656")),
                s(2, "1.1.1.1:443"),
                s(2, &format!("{DOMAIN}:443")),
            ],
            None,
        ),
    ];
    for (mn_type, entries, reason) in &vals {
        let net_info = added(Testnet3, entries);
        for kind in [Testnet3, Main] {
            assert_eq!(
                crate::netinfo::validate(&net_info, &rules(kind)),
                crate::netinfo::NetInfoStatus::Success
            );
            assert_eq!(ext_fields(&net_info, *mn_type, true).check(kind), None, "{entries:?}");
            let inactive = ext_fields(&net_info, *mn_type, false).check(kind);
            assert_eq!(inactive, reason.map(|r| (S, r.to_owned())), "{entries:?} on {kind:?}");
        }
    }
}

/// Core's reasons and debug messages for the `CheckIPv4Primaries` rejects (providertx.cpp
/// v24.0.0:235-252), which `TxValidationState::ToString()` carries.
#[test]
fn ipv4_primaries_reasons_and_debug_messages() {
    let refusal = |entries: &[(u8, String)], mn_type| {
        let net_info = added(Main, entries);
        let fields = ProviderNetworkFields {
            net_info: &net_info,
            mn_type,
            version: 3,
            platform_node_id: None,
            platform_p2p_port: 0,
            platform_http_port: 0,
        };
        check_provider_network_fields(&fields, false, false, &rules(Main))
            .err()
            .map(|e| (e.reject_reason().to_owned(), e.debug_message().to_owned()))
    };
    let r = |reason: &str, debug: &str| Some((reason.to_owned(), debug.to_owned()));
    let core = |first: &str| s(0, &format!("{first}:9999"));
    let evo = |p2p: &str, https: &str| {
        vec![core("1.1.1.1"), s(1, &format!("{p2p}:26656")), s(2, &format!("{https}:443"))]
    };
    let type_msg = |purpose: &str| format!("first {purpose} entry must be IPv4");
    let addr_msg =
        |purpose: &str| format!("first {purpose} entry must use the first core_p2p address");
    // A first entry of another type is refused; one after an IPv4 first entry is not.
    assert_eq!(
        refusal(&[core(IPV6)], MnType::Regular),
        r(BAD_PROTX_NETINFO_ADDR_TYPE, &type_msg("core_p2p"))
    );
    assert_eq!(refusal(&[core("1.1.1.1"), core(IPV6)], MnType::Regular), None);
    assert_eq!(
        refusal(&evo(IPV6, "1.1.1.1"), MnType::Evo),
        r(BAD_PROTX_NETINFO_ADDR_TYPE, &type_msg("platform_p2p"))
    );
    assert_eq!(
        refusal(&evo("1.1.1.1", DOMAIN), MnType::Evo),
        r(BAD_PROTX_NETINFO_ADDR_TYPE, &type_msg("platform_https"))
    );
    assert_eq!(
        refusal(&evo("1.1.1.2", "1.1.1.1"), MnType::Evo),
        r(BAD_PROTX_NETINFO_ADDR, &addr_msg("platform_p2p"))
    );
    assert_eq!(
        refusal(&evo("1.1.1.1", "1.1.1.2"), MnType::Evo),
        r(BAD_PROTX_NETINFO_ADDR, &addr_msg("platform_https"))
    );
    // Platform P2P is checked before Platform HTTPS, the type before the address, and the Core
    // P2P entry before both.
    assert_eq!(
        refusal(&evo("1.1.1.2", DOMAIN), MnType::Evo),
        r(BAD_PROTX_NETINFO_ADDR, &addr_msg("platform_p2p"))
    );
    assert_eq!(
        refusal(&evo(IPV6, "1.1.1.2"), MnType::Evo),
        r(BAD_PROTX_NETINFO_ADDR_TYPE, &type_msg("platform_p2p"))
    );
    let ipv6_core = vec![core(IPV6), s(1, "1.1.1.2:26656"), s(2, &format!("{DOMAIN}:443"))];
    assert_eq!(
        refusal(&ipv6_core, MnType::Evo),
        r(BAD_PROTX_NETINFO_ADDR_TYPE, &type_msg("core_p2p"))
    );
}

/// The rule runs after `CheckNetInfo` and before the platform fields, on non-empty objects only,
/// and a legacy object never reaches it with another type (`MnNetInfo::Validate()` refuses it
/// first).
#[test]
fn ipv4_primaries_step_order() {
    // CheckNetInfo first: a duplicate entry behind an IPv6 first entry is a duplicate.
    let dup = ext(&[(0, vec![ipv6_entry(9999), ipv6_entry(9999)])]);
    assert_eq!(Case::regular(dup, 3).check(Main), expect(205));
    // Before the node id.
    let net_info = added(
        Main,
        &[(0, format!("{IPV6}:9999")), (1, "1.1.1.1:26656".into()), (2, "1.1.1.1:443".into())],
    );
    let case = Case::evo(net_info, 3, (0, 0)).node_id([0; 20]);
    assert_eq!(case.check(Main), Some((S, BAD_PROTX_NETINFO_ADDR_TYPE.to_owned())));
    // An empty object is not checked.
    let mut empty = Case::regular(empty_ext(), 3);
    empty.allow_empty = true;
    assert_eq!(empty.check(Main), None);
    // A legacy IPv6 service is BadType in Validate() (census row 195), whatever IPV4_OPTIONAL.
    let ipv6_mn = mn(ipv6_bytes(), 9999);
    for ipv4_optional in [false, true] {
        let mut case = Case::regular(ipv6_mn.clone(), 2);
        case.ipv4_optional = ipv4_optional;
        assert_eq!(case.check(Main), expect(195));
    }
}

fn ipv6_bytes() -> [u8; 16] {
    [0x26, 6, 0x47, 0, 0x47, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x11, 0x11]
}

pub(crate) fn ipv6_entry(port: u16) -> Vec<u8> {
    let mut out = vec![0x01, 2, 16];
    out.extend(ipv6_bytes());
    out.extend(port.to_be_bytes());
    out
}

/// `extnetinfo_ports_mainnet_only` (evo_netinfo_tests.cpp v24.0.0:308-364): objects built on
/// testnet, where the mainnet port pins don't apply, then read on mainnet, whose consensus applies
/// them to the same objects: `Validate()` is BadPort and `CheckProviderNetworkFields` refuses
/// with `bad-protx-netinfo-port` unless each purpose's first entry is on its standard port.
#[test]
fn extnetinfo_ports_mainnet_only() {
    use crate::netinfo::NetInfoStatus::{BadPort, Success};
    use MnType::{Evo, Regular};
    let vals: Vec<(MnType, Entries, bool)> = vec![
        (Regular, vec![s(0, "1.1.1.1:19999")], false),
        (Evo, vec![s(0, "1.1.1.1:9999"), s(1, "1.1.1.1:22000"), s(2, "1.1.1.1:443")], false),
        (Evo, vec![s(0, "1.1.1.1:9999"), s(1, "1.1.1.1:26656"), s(2, "1.1.1.1:8443")], false),
        (
            Evo,
            vec![
                s(0, "1.1.1.1:9999"),
                s(0, &format!("{IPV6}:19999")),
                s(0, "1.1.1.2:9999"),
                s(1, "1.1.1.1:26656"),
                s(1, "1.1.1.2:22000"),
                s(1, "1.1.1.3:26656"),
                s(2, "1.1.1.1:443"),
                s(2, "1.1.1.2:443"),
                s(2, &format!("{DOMAIN}:443")),
            ],
            true,
        ),
    ];
    for (mn_type, entries, valid_on_mainnet) in &vals {
        let net_info = added(Testnet3, entries);
        assert_eq!(crate::netinfo::validate(&net_info, &rules(Testnet3)), Success);
        assert_eq!(ext_fields(&net_info, *mn_type, false).check(Testnet3), None, "{entries:?}");
        let expected = if *valid_on_mainnet {
            Success
        } else {
            BadPort
        };
        assert_eq!(crate::netinfo::validate(&net_info, &rules(Main)), expected, "{entries:?}");
        let verdict = ext_fields(&net_info, *mn_type, false).check(Main);
        let reason = (!valid_on_mainnet).then(|| (S, BAD_PROTX_NETINFO_PORT.to_owned()));
        assert_eq!(verdict, reason, "{entries:?}");
        // No height lifts it.
        assert_eq!(ext_fields(&net_info, *mn_type, true).check(Main), reason, "{entries:?}");
    }
}

/// `provider_network_fields_main`'s primary cases (evo_netinfo_tests.cpp v24.0.0:236-301) that
/// reach AddEntry and `CheckProviderNetworkFields`: on mainnet a purpose's first entry must use
/// its standard port when it is added (`BadPort`), additional entries may use any address, port
/// and type, and with IPV4_OPTIONAL active only the port pins remain.
#[test]
fn mainnet_primaries_through_add_entry() {
    use crate::netinfo::NetInfoStatus::{BadPort, Success};
    use MnType::{Evo, Regular};
    let (a, b) = ("1.1.1.1", "1.1.1.2");
    let i2p = "udhdrtrcetjm5sxzskjyr5ztpeszydbh4dpl3pl4utgqqw2v4jna.b32.i2p:0";
    let add = |net_info: &mut NetInfo, p: u8, input: &str| {
        let purpose = dash_primitives::NIPurpose::from_base(p);
        crate::netinfo::add_entry(net_info, purpose, input, &rules(Main))
    };
    // Valid, with additional entries of any type, address and port.
    let valid: Vec<(MnType, Entries)> = vec![
        (Regular, vec![(0, format!("{a}:9999"))]),
        (
            Regular,
            vec![
                (0, format!("{a}:9999")),
                (0, format!("{b}:9998")),
                (0, format!("{IPV6}:9999")),
                (0, format!("{ONION}:9999")),
            ],
        ),
        (Regular, vec![(0, format!("{a}:9999")), (0, i2p.to_owned())]),
        (Evo, vec![(0, format!("{a}:9999")), (1, format!("{a}:26656")), (2, format!("{a}:443"))]),
        (
            Evo,
            vec![
                (0, format!("{a}:9999")),
                (0, format!("{IPV6}:9999")),
                (1, format!("{a}:26656")),
                (1, format!("{b}:26657")),
                (1, format!("{IPV6}:26656")),
                (1, format!("{ONION}:26656")),
                (2, format!("{a}:443")),
                (2, format!("{DOMAIN}:443")),
                (2, format!("{IPV6}:8443")),
            ],
        ),
        (Regular, vec![(0, format!("{a}:9999")), (0, format!("{b}:9999"))]),
        (Regular, vec![(0, format!("{a}:9999")), (0, format!("{b}:26656"))]),
    ];
    for (mn_type, entries) in &valid {
        let net_info = added(Main, entries);
        assert_eq!(ext_fields(&net_info, *mn_type, false).check(Main), None, "{entries:?}");
    }
    // The first entry of a purpose must use its port: AddEntry refuses it.
    let first_port: Vec<(Entries, (u8, String))> = vec![
        (vec![], (0, format!("{a}:9998"))),
        (vec![], (0, i2p.to_owned())),
        (vec![(0, format!("{a}:9999"))], (1, format!("{a}:26657"))),
        (vec![(0, format!("{a}:9999")), (1, format!("{a}:26656"))], (2, format!("{a}:8443"))),
        (vec![(0, format!("{a}:9999")), (1, format!("{a}:26656"))], (2, format!("{DOMAIN}:8443"))),
    ];
    for (before, (p, input)) in &first_port {
        let mut net_info = added(Main, before);
        assert_eq!(add(&mut net_info, *p, input), BadPort, "{input}");
        // The same entry behind a valid first entry of its purpose, on another address, is
        // accepted.
        let primary = match p {
            0 => "1.1.1.3:9999",
            1 => "1.1.1.3:26656",
            _ => "1.1.1.3:443",
        };
        if !before.iter().any(|(q, _)| q == p) {
            assert_eq!(add(&mut net_info, *p, primary), Success);
            assert_eq!(add(&mut net_info, *p, input), Success, "{input} second");
        }
    }
    // The port pin holds for any address type: an IPv6 primary off the standard port is BadPort.
    let mut net_info = empty_ext();
    assert_eq!(add(&mut net_info, 0, &format!("{IPV6}:9998")), BadPort);
}
