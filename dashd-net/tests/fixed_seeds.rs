//! The fixed-seed tables against Dash Core v24.0.0-rc.3's own `chainparamsseeds.h`
//! (`tests/data/PROVENANCE.md`): the bytes `dash-params` carries for mainnet and testnet, and
//! the addresses SB6's `ConvertSeeds` makes of them, compared as raw BIP155 tuples decoded here.

use dash_p2p::rng::FastRandom;
use dash_p2p::seeds::convert_seeds;
use dash_p2p::{Service, ServiceFlags};
use dash_params::{ChainParamsBuilder, NetworkKind, Release};

const HEADER: &str = include_str!("data/chainparamsseeds.h");

/// The bytes of `static const uint8_t <name>[] = { … };`.
fn core_table(name: &str) -> Vec<u8> {
    let start = HEADER.find(&format!("{name}[] = {{")).expect("table in the header");
    let body = &HEADER[start..];
    let body = &body[body.find('{').unwrap() + 1..body.find("};").unwrap()];
    body.split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| u8::from_str_radix(t.strip_prefix("0x").unwrap(), 16).unwrap())
        .collect()
}

/// A BIP155 `(networkID, addr, port)` tuple.
type Tuple = (u8, Vec<u8>, u16);

/// The tuples as `generate-seeds.py` writes them: a one-byte network id, a one-byte length
/// (always below 253), the address, a big-endian port.
fn decode(mut rest: &[u8]) -> Vec<Tuple> {
    let mut out = Vec::new();
    while !rest.is_empty() {
        let (id, len) = (rest[0], usize::from(rest[1]));
        assert!(matches!((id, len), (1, 4) | (2, 16) | (4, 32) | (5, 32) | (6, 16)), "{id} {len}");
        let port = u16::from_be_bytes([rest[2 + len], rest[3 + len]]);
        out.push((id, rest[2..2 + len].to_vec(), port));
        rest = &rest[4 + len..];
    }
    out
}

/// A converted address as its BIP155 tuple, read through `NetAddr`'s accessors only.
fn tuple(s: &Service) -> Tuple {
    let a = &s.addr;
    let id = if a.is_ipv4() {
        1
    } else if a.is_ipv6() {
        2
    } else if a.is_tor() {
        4
    } else if a.is_i2p() {
        5
    } else if a.is_cjdns() {
        6
    } else {
        panic!("unexpected network for {s}")
    };
    (id, a.bytes().to_vec(), s.port)
}

fn check(kind: NetworkKind, table: &str, min: usize) {
    let params = ChainParamsBuilder::new(kind, Release::V24_0).build().unwrap();
    let core = core_table(table);
    assert_eq!(params.fixed_seeds, &core[..], "{table}: dash-params differs from Core");

    let want = decode(&core);
    assert!(want.len() >= min, "{table}: {} seeds", want.len());
    let now = 1_700_000_000;
    let week = 7 * 24 * 60 * 60;
    let got = convert_seeds(params.fixed_seeds, now, &mut FastRandom::deterministic());
    let tuples: Vec<Tuple> = got.iter().map(|a| tuple(&a.service)).collect();
    assert_eq!(tuples, want, "{table}: ConvertSeeds order or contents");
    for a in &got {
        assert_eq!(a.services, ServiceFlags::NODE_NETWORK);
        let t = i64::from(a.time);
        assert!(now - 2 * week < t && t <= now - week, "{table}: time {t}");
    }
}

#[test]
fn mainnet_seeds_are_cores() {
    check(NetworkKind::Mainnet, "chainparams_seed_main", 100);
}

#[test]
fn testnet_seeds_are_cores() {
    check(NetworkKind::Testnet, "chainparams_seed_test", 1);
}
