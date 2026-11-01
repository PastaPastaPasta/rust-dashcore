//! Core's `evo_simplifiedmns_tests` (L0, SB7).
//!
//! Provenance: `src/test/evo_simplifiedmns_tests.cpp` of Dash Core v24.0.0-rc.3,
//! `simplifiedmns_merkleroots`: the entries the test builds and the hashes and root it expects,
//! copied verbatim. The suite's other case, `simplifiedmns_extended_diff_share_reward`, checks
//! that a DML entry's shares stay out of its SML leaf and in the extended diff only; an
//! [`SmlEntry`] carries no shares, and the projection and diff builder are EV4-serve's.

use dash_consensus::hash::sha256d;
use dash_consensus::primitives::BlockHash;
use dash_evo::sml::{SmlEntry, SmlList, merkle_root_mn_list};
use dash_evo::types::{ProTxHash, RawMnType};
use dash_pkc::bls::{BlsScChia, BlsSecretKey};
use dash_primitives::{AddrV1, NetInfo, NetInfoV1, PlatformNodeId, ServiceV1};
use dash_types::Numeric;

const EXPECTED_HASHES: [&str; 15] = [
    "3a1010e28226558560e5296bcee6bf0b9b963b73a1514f5aa2885e270f6b90c1",
    "85d3d93b28689128daf3a41d706ae5002f447b9b6372776f0ca9d53b31146884",
    "8930eee6bd2e7971a7090edfb79f74c00a12280e59adfc2cc99d406a01e368f9",
    "dc2e69caa0ef97e8f5cf40a9530641bd4933dd8c9ad533054537728f7e5f58c2",
    "3e4a0e0a0d2ed397fa27221de3047de21f50d17d0ba43738cbdb9fee96c7cb46",
    "eb18476a1496e1cb912b1d4dd93314b78c6a679d83cae8e144a717b967dc4b8c",
    "6c0d01fa40ac11d7b523facd2bf5632c83f7e4df3f60fd1b364ea90f6c852156",
    "c9e3e69d54e6e95b280ae102593fe114cf4620fa89dd88da1a146ada08815d68",
    "1023f67f735e8e9403d5f083e7a17489619b1790feac4f6b133e9dda15999ae6",
    "5d5fc77944f7c72df236a5baf460c7b9a947144d54d0953521f1494c8a2f7aaa",
    "ac7db66820de3c7506f8c6415fd352e36ac5f27c6adbdfb74de3e109d0d277df",
    "cbc25ca965d0fa69a1fdc1d796b8ee2726a0e2137414e92fb9541630e3189901",
    "ac9934c4049ae952d41fb38e7e9659a558a5ce748bdb7fb613741598d1b16a27",
    "a61177eb14450bb8c56e5f0547035e0f3a70fe46f36901351cc568b2e48e29d0",
    "67798d40a2196f446114c68d86445fe088750515a96eb65392c6bcfac8f3be9b",
];

const EXPECTED_ROOT: &str = "0bae2176078cf42fa3e1fda761d4255d1c1c54777c6a793d0ab2b07c85ed4022";

/// `uint256::SetHex(strprintf("%064x", i))` / `uint160`: `i` as the low internal byte.
fn small<const N: usize>(i: u8) -> [u8; N] {
    let mut bytes = [0; N];
    bytes[0] = i;
    bytes
}

/// The test's entry `i`: version 1 (`GetMax(legacy, !ext)`), service `0.0.0.i:i`, the legacy
/// public key of the secret key `{i, 0, …}`.
fn core_entry(i: u8) -> SmlEntry {
    let mut secret = [0; 32];
    secret[0] = i;
    let operator_key =
        BlsSecretKey::<BlsScChia>::from_bytes(&secret).unwrap().public_key().to_bytes();
    let mut addr = [0; 16];
    addr[10] = 0xff;
    addr[11] = 0xff;
    addr[15] = i;
    SmlEntry {
        version: 1,
        pro_reg_tx_hash: ProTxHash::from(small::<32>(i)),
        confirmed_hash: BlockHash::from_lendian(small(i)),
        net_info: NetInfo::Legacy(NetInfoV1(ServiceV1 {
            addr: AddrV1(addr),
            port: u16::from(i),
        })),
        operator_key,
        voting_key_id: small(i),
        is_valid: true,
        mn_type: RawMnType::REGULAR,
        platform_http_port: 0,
        platform_node_id: PlatformNodeId::from_lendian([0; 20]),
    }
}

// Core: test/evo_simplifiedmns_tests.cpp:19 simplifiedmns_merkleroots
#[test]
fn simplifiedmns_merkleroots() {
    let entries: Vec<SmlEntry> = (1..16).map(core_entry).collect();
    let hashes: Vec<String> = entries.iter().map(|entry| entry.hash().to_string()).collect();
    assert_eq!(hashes, EXPECTED_HASHES);

    let root = merkle_root_mn_list(&entries).unwrap();
    assert_eq!(root.to_string(), EXPECTED_ROOT);

    // The cached root agrees, whatever order the list was built in.
    let mut list = SmlList::from_entries(entries.iter().rev().cloned());
    assert_eq!(list.merkle_root().unwrap(), root);
    let mut grown = SmlList::new();
    for entry in entries {
        grown.upsert(entry);
    }
    assert_eq!(grown.merkle_root().unwrap(), root);
}

/// The list roots order entries by proTxHash memcmp (Core's `uint256::Compare`, first byte first),
/// not as numbers (last byte most significant): three proTxHashes on which the two disagree, with
/// the root built by hand over the memcmp order. Core's vector above sets only byte 0, where the
/// two orders agree.
// Core: evo/simplifiedmns.cpp:73 CSimplifiedMNList (sort), :81 CalcMerkleRoot
#[test]
fn the_list_root_sorts_protx_hashes_by_memcmp() {
    let entry = |i: u8, first: u8, last: u8| {
        let mut pro_tx = [0; 32];
        pro_tx[0] = first;
        pro_tx[31] = last;
        SmlEntry {
            pro_reg_tx_hash: ProTxHash::from(pro_tx),
            ..core_entry(i)
        }
    };
    // memcmp order: c, a, b. Numeric order: b, c, a.
    let (a, b, c) = (entry(1, 0x01, 0xff), entry(2, 0x02, 0x00), entry(3, 0x00, 0x80));
    let leaf = |entry: &SmlEntry| entry.hash().to_lendian();
    let pair = |left: [u8; 32], right: [u8; 32]| sha256d(&[left, right].concat());
    let memcmp = pair(pair(leaf(&c), leaf(&a)), pair(leaf(&b), leaf(&b)));
    let numeric = pair(pair(leaf(&b), leaf(&c)), pair(leaf(&a), leaf(&a)));
    assert_ne!(memcmp, numeric);

    let entries = [a, b, c];
    assert_eq!(merkle_root_mn_list(&entries).unwrap().to_lendian(), memcmp);
    let mut list = SmlList::from_entries(entries.iter().cloned());
    assert_eq!(list.merkle_root().unwrap().to_lendian(), memcmp);
    let mut grown = SmlList::new();
    for entry in entries.into_iter().rev() {
        grown.upsert(entry);
    }
    assert_eq!(grown.merkle_root().unwrap().to_lendian(), memcmp);
}
