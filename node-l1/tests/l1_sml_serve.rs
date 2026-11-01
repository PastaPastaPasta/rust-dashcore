//! L1 differential of the node's simplified masternode list (`dash-dml`, EV4-serve notes D10)
//! against Dash Core v24's in-process oracle (ABI 8): each masternode's entry
//! (`CDeterministicMN::to_sml_entry`, as mnlistdiff carries it) and its `CalcHash()`, and each
//! list's `CalcCbTxMerkleRootMNList` with the entry hashes behind it, computed in full and by the
//! cache.
//!
//! Inputs are the lists Core produced in `dash-dml/tests/data/codec/` (provenance in its
//! README) and testkit lists with banned, revived and Evo masternodes.

use dash_dml::codec::decode_canonical_list;
use dash_dml::mnlistdiff::write_sml_entry;
use dash_dml::testkit::{dmn_bytes, list_disk_bytes, numbered};
use dash_dml::{MnList, SmlRootCache, merkle_root_mn_list_full};
use dash_oracle_sys::{Chain, ListEncoding, Oracle};
use dash_primitives::BlockHash;
use node_l1::dml::core_net_info;

const LISTS: &str = include_str!("../../dash-dml/tests/data/codec/lists.txt");

/// The protocol version the oracle writes the entry at (`PROTOCOL_VERSION`, version.h v24:14).
const PROTOCOL_VERSION: i32 = 70242;

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
}

/// Core's lists, then testkit lists: 64 numbered masternodes (every eighth Evo) with some
/// banned, one revived, and the empty list.
fn lists() -> Vec<(String, MnList)> {
    let mut lists: Vec<(String, MnList)> = LISTS
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .map(|l| {
            let f: Vec<&str> = l.split(' ').collect();
            let bytes = unhex(f[2]);
            let list =
                decode_canonical_list(&mut bytes.as_slice(), core_net_info()).expect("Core's list");
            (f[1].to_string(), list)
        })
        .collect();

    let mut list = MnList::new(BlockHash::from([3; 32]), 2_000_000, 0);
    for i in 0..64 {
        list.try_add(numbered(i), true).expect("distinct masternodes");
    }
    for i in (0..64).step_by(5) {
        let pro_tx = numbered(i).pro_tx_hash;
        let mut hot = list.get(&pro_tx).expect("listed").hot;
        hot.ban_if_not_banned(2_000_000);
        if i % 3 == 0 {
            hot.revive(2_000_001);
        }
        list.update_hot(&pro_tx, hot).expect("hot update");
    }
    lists.push(("testkit-64".into(), list));
    lists.push(("empty".into(), MnList::new(BlockHash::from([4; 32]), 2_000_000, 0)));
    lists
}

/// Each masternode's entry bytes and hash match Core's `to_sml_entry` and `CalcHash()`.
#[test]
fn entries_match_the_oracle() {
    let mut checked = 0;
    for oracle in Oracle::all() {
        for (name, list) in lists() {
            for dmn in list.iter() {
                let (core_entry, core_hash) = oracle
                    .sml_entry_hash(&dmn_bytes(dmn))
                    .unwrap_or_else(|e| panic!("{name}: {e}"));
                let entry = dmn.to_sml_entry();
                let mut ours = Vec::new();
                write_sml_entry(&entry, PROTOCOL_VERSION, &mut ours);
                assert_eq!(ours, core_entry, "{name}: {}", dmn.pro_tx_hash);
                assert_eq!(entry.hash().as_bytes(), &core_hash, "{name}: {}", dmn.pro_tx_hash);
                checked += 1;
            }
        }
    }
    assert!(checked > 64);
}

/// Each list's `merkleRootMNList` and entry hashes match Core's, in full and through one cache
/// walked over every list in turn.
#[test]
fn roots_match_the_oracle() {
    let chain = Chain::main();
    for oracle in Oracle::all() {
        let mut cache = SmlRootCache::new();
        for (name, list) in lists() {
            let core = oracle
                .sml_merkle_root(&chain, ListEncoding::Disk, &list_disk_bytes(&list))
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(core.verdict.is_accept(), "{name}: {:?}", core.verdict);
            let sml = list.project_sml();
            let hashes: Vec<[u8; 32]> =
                sml.entries().iter().map(|e| *e.hash().as_bytes()).collect();
            assert_eq!(hashes, core.entry_hashes, "{name}");
            let full = merkle_root_mn_list_full(&list).expect(&name);
            assert_eq!(full.as_bytes(), &core.root, "{name}");
            assert_eq!(cache.merkle_root(&list).expect(&name), full, "{name}: cached");
        }
    }
}
