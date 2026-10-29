//! L0 vectors for the provider network fields (EV1 §3.15, §6.1): every reject row of
//! `providertx.cpp:166-263`, with Core's class and reason, plus provider_network_fields_main
//! (evo_netinfo_tests.cpp:148-186) and the empty-netInfo platform-port cases (D31). The census
//! closure test (`reasons::tests`) checks that each row here is a census row.

use dash_consensus::validation::TxValidationResult;
use dash_params::{ChainParamsBuilder, NetworkKind, Release};
use dash_primitives::{NetInfo, NetInfoV1, NetInfoV2, PlatformNodeId};
use dash_types::codec::BaseCodec;

use super::*;
use crate::netinfo::NetInfoConfig;

use NetworkKind::{Devnet, Mainnet, Regtest, Testnet};

fn rules(kind: NetworkKind) -> NetInfoRules {
    let params = |k| ChainParamsBuilder::new(k, Release::V24_0).build().expect("Core's parameters");
    let config = NetInfoConfig {
        cjdns_reachable: true,
    };
    NetInfoRules::new(&params(kind), &params(Mainnet), config)
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

fn mn_ipv4(ip: [u8; 4], port: u16) -> NetInfo {
    let mut addr = [0; 16];
    addr[10..12].copy_from_slice(&[0xff, 0xff]);
    addr[12..].copy_from_slice(&ip);
    mn(addr, port)
}

fn empty_mn() -> NetInfo {
    NetInfo::Legacy(decode::<NetInfoV1>(&[0; 18]))
}

fn ipv4_entry(ip: [u8; 4], port: u16) -> Vec<u8> {
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

fn ext(purposes: &[(u8, Vec<Vec<u8>>)]) -> NetInfo {
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

/// A complete EvoNode ExtNetInfo on mainnet: Core P2P, Platform P2P and Platform HTTPS.
fn ext_evo() -> NetInfo {
    ext(&[
        (0, vec![ipv4_entry([1, 1, 1, 1], 9999)]),
        (1, vec![ipv4_entry([1, 1, 1, 2], 26656)]),
        (2, vec![domain_entry("server.example.com", 443)]),
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
        }
    }

    fn check(&self, kind: NetworkKind) -> Option<(TxValidationResult, String)> {
        let node_id = self.node_id.map(PlatformNodeId::from);
        let fields = ProviderNetworkFields {
            net_info: &self.net_info,
            mn_type: self.mn_type,
            version: self.version,
            platform_node_id: node_id.as_ref(),
            platform_p2p_port: self.ports.0,
            platform_http_port: self.ports.1,
        };
        let res = check_provider_network_fields(&fields, self.allow_empty, &rules(kind));
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
    let cases: Vec<(u32, NetworkKind, Case)> = vec![
        // IsNetInfoTriviallyValid.
        (170, Mainnet, ext3(&[(1, vec![ipv4_entry([1, 1, 1, 2], 26656)])])),
        (175, Mainnet, ext3(&[core(ok()), (1, vec![ipv4_entry([1, 1, 1, 2], 26656)])])),
        (
            181,
            Mainnet,
            Case::evo(ext(&[core(ok()), (1, vec![ipv4_entry([1, 1, 1, 2], 26656)])]), 3, (0, 0)),
        ),
        // CheckNetInfo.
        (193, Mainnet, Case::regular(mn_ipv4([1, 1, 1, 1], 9998), 2)),
        // Only a legacy object can hold a valid service of the wrong type: ExtNetInfo admits
        // every network a valid BIP155 service can be on.
        (
            195,
            Mainnet,
            Case::regular(
                mn([0x26, 6, 0x47, 0, 0x47, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x11, 0x11], 9999),
                2,
            ),
        ),
        (197, Mainnet, Case::regular(mn_ipv4([10, 0, 0, 1], 9999), 2)),
        (199, Mainnet, ext3(&[core(vec![0x03])])),
        (203, Mainnet, ext3(&[core(domain_entry("x.example.com", 443))])),
        (205, Mainnet, ext3(&[(0, vec![ok(), ok()])])),
        (207, Mainnet, ext3(&[(0, (1..=5).map(|i| ipv4_entry([1, 1, 1, i], 9999)).collect())])),
        // CheckProviderNetworkFields.
        (217, Mainnet, Case::regular(mn_ipv4([1, 1, 1, 1], 9999), 3)),
        (217, Mainnet, Case::regular(ext(&[core(ok())]), 2)),
        (221, Mainnet, Case::regular(empty_mn(), 2)),
        (231, Mainnet, Case::evo(ext_evo(), 3, (0, 0)).node_id([0; 20])),
        (235, Mainnet, Case::evo(ext_evo(), 3, (26656, 0))),
        (238, Mainnet, Case::evo(ext_evo(), 3, (0, 443))),
        (245, Mainnet, Case::evo(mn_ipv4([1, 1, 1, 1], 9999), 2, (26657, 443))),
        (248, Mainnet, Case::evo(mn_ipv4([1, 1, 1, 1], 9999), 2, (26656, 444))),
        (252, Testnet, Case::evo(mn_ipv4([1, 1, 1, 1], 19999), 2, (9999, 22001))),
        (255, Testnet, Case::evo(mn_ipv4([1, 1, 1, 1], 19999), 2, (22000, 9999))),
        (260, Testnet, Case::evo(mn_ipv4([1, 1, 1, 1], 19999), 2, (22000, 22000))),
        (260, Testnet, Case::evo(mn_ipv4([1, 1, 1, 1], 19999), 2, (19999, 22001))),
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
    let case = |ports| Case::evo(net_info.clone(), 2, ports).check(Mainnet);
    assert_eq!(case((26656, 443)), None);
    assert_eq!(case((26657, 443)), expect(245));
    assert_eq!(case((26656, 444)), expect(248));
    assert_eq!(case((9999, 443)), expect(245));

    let ext_local = ext(&[
        (0, vec![ipv4_entry([127, 0, 0, 1], 9999)]),
        (1, vec![ipv4_entry([1, 1, 1, 2], 22200)]),
        (2, vec![domain_entry("server.example.com", 443)]),
    ]);
    assert_eq!(Case::evo(ext_local, 3, (0, 0)).check(Mainnet), expect(197));
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
    for kind in [Testnet, Regtest, Devnet] {
        assert_eq!(case(kind, 2, (0, 22001), empty_mn()), expect(260), "{kind:?}");
        assert_eq!(case(kind, 2, (22001, 0), empty_mn()), expect(260), "{kind:?}");
        assert_eq!(case(kind, 2, (22000, 22001), empty_mn()), None, "{kind:?}");
    }
    assert_eq!(case(Mainnet, 2, (0, 443), empty_mn()), expect(245));
    assert_eq!(case(Testnet, 3, (0, 0), empty_ext()), None);
    // allow_empty only covers the empty object; a regular node passes without platform fields.
    let mut regular = Case::regular(empty_ext(), 3);
    regular.allow_empty = true;
    assert_eq!(regular.check(Mainnet), None);
}

/// The step order of `CheckProviderNetworkFields`: version kind, then the netInfo, then the
/// regular-node accept, then the EvoNode fields.
#[test]
fn step_order() {
    // A non-Evo node skips the platform checks entirely.
    let mut case = Case::regular(mn_ipv4([1, 1, 1, 1], 9999), 2);
    case.ports = (1, 1);
    case.node_id = Some([0; 20]);
    assert_eq!(case.check(Mainnet), None);
    // The netInfo rejects come before the node id.
    let case = Case::evo(mn_ipv4([1, 1, 1, 1], 9998), 2, (26656, 443)).node_id([0; 20]);
    assert_eq!(case.check(Mainnet), expect(193));
    // Trivial validity comes before Validate(): a Platform entry on a regular node with a bad
    // Core P2P port is bad-protx-netinfo-bad (175), not -port.
    let both = ext(&[
        (0, vec![ipv4_entry([1, 1, 1, 1], 1)]),
        (2, vec![domain_entry("a.example.com", 443)]),
    ]);
    assert_eq!(Case::regular(both, 3).check(Mainnet), expect(175));
    // The version-kind check precedes everything, even an empty object.
    let mut case = Case::regular(empty_ext(), 2);
    case.allow_empty = true;
    assert_eq!(case.check(Mainnet), expect(217));
    // Core's null pointer (RPC only) skips the node-id check; the null id does not.
    let mut case = Case::evo(ext_evo(), 3, (0, 0));
    case.node_id = None;
    assert_eq!(case.check(Mainnet), None);
    case.node_id = Some([0; 20]);
    assert_eq!(case.check(Mainnet), expect(231));
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
