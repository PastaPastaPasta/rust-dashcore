// SPDX-License-Identifier: MIT

//! Oracle: Core v24.0.0-rc.3's basic filters of regtest blocks that hold special transactions
//! (`tests/data/PROVENANCE.md`). The filters, filter headers and spent scripts in
//! `core_v24_special_tx_filters.json` were read from a dashd running with
//! `-blockfilterindex=1`; every element of every filter must come out of `dash-filters` as it
//! came out of Core, for the ProRegTx (versions 1 to 3, shared masternodes), ProUpServTx,
//! ProUpRegTx, ProUpRevTx, asset-lock and shared-masternode transaction types, and for the types
//! that add nothing (asset unlock, quorum commitment, MNHF signal, coinbase).

use std::collections::{BTreeMap, BTreeSet};

use dash_consensus::decode_block;
use dash_core_vectors::hex::{parse_hex, uint256_from_hex};
use dash_filters::elements::{add_output_scripts, add_script, add_special_tx_elements};
use dash_filters::{
    BlockFilter, ChainError, FilterHash, FilterHeader, FilterQuery, GcsParams, check_filter,
    check_links, encode, extend_headers, filter_hash,
};
use dash_primitives::OutPoint;
use dash_types::Numeric as _;
use serde_json::Value;

const DATA: &str = include_str!("data/core_v24_special_tx_filters.json");

/// Types whose payload adds fields to the filter.
const ADDS_ELEMENTS: [u64; 8] = [1, 2, 3, 4, 8, 10, 11, 12];

struct Vector {
    test: String,
    height: u64,
    kind: String,
    tx_types: BTreeSet<u64>,
    block: Vec<u8>,
    block_hash: [u8; 32],
    scripts: Vec<Vec<u8>>,
    prev_header: FilterHeader,
    filter: Vec<u8>,
    header: FilterHeader,
}

fn vectors() -> Vec<Vector> {
    let data: Value = serde_json::from_str(DATA).unwrap();
    assert_eq!(data["dashd"], "Dash Core version v24.0.0-rc.3");
    let mut out = Vec::new();
    for chain in data["chains"].as_array().unwrap() {
        for b in chain["blocks"].as_array().unwrap() {
            let s = |key: &str| b[key].as_str().unwrap();
            out.push(Vector {
                test: chain["test"].as_str().unwrap().to_owned(),
                height: b["height"].as_u64().unwrap(),
                kind: s("kind").to_owned(),
                tx_types: b["tx_types"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|t| t.as_u64().unwrap())
                    .collect(),
                block: parse_hex(s("block")),
                block_hash: uint256_from_hex(s("hash")),
                scripts: b["prev_scripts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|x| parse_hex(x.as_str().unwrap()))
                    .collect(),
                prev_header: FilterHeader::from_lendian(uint256_from_hex(s("prev_header"))),
                filter: parse_hex(s("filter")),
                header: FilterHeader::from_lendian(uint256_from_hex(s("header"))),
            });
        }
    }
    out
}

/// A vector's block and the spent-script lookup Core's undo data gives.
fn load(v: &Vector) -> (dash_consensus::Block, BTreeMap<OutPoint, Vec<u8>>) {
    let block = decode_block(&mut v.block.as_slice()).unwrap().into_value();
    assert_eq!(block.hash().to_lendian(), v.block_hash);
    let mut spent = BTreeMap::new();
    let mut scripts = v.scripts.iter();
    for tx in block.txs().iter().skip(1) {
        for input in tx.inputs() {
            spent.insert(input.prevout, scripts.next().expect("a script per input").clone());
        }
    }
    assert!(scripts.next().is_none());
    (block, spent)
}

#[test]
fn the_vectors_cover_every_special_transaction_type() {
    let all = vectors();
    let seen: BTreeSet<u64> = all.iter().flat_map(|v| v.tx_types.iter().copied()).collect();
    // 0 plain, 1-4 provider, 5 coinbase, 6 quorum commitment, 7 MNHF, 8/9 asset lock/unlock, 10-12 shared.
    assert_eq!(seen, (0..=12).collect::<BTreeSet<u64>>());
    assert_eq!(all.len(), 149);
}

#[test]
fn filters_equal_cores() {
    for v in vectors() {
        let what = format!("{} block {} {:?}", v.test, v.height, v.tx_types);
        let (block, spent) = load(&v);
        let txs: Vec<_> = block.txs().iter().map(|tx| tx.raw()).collect();
        let filter = BlockFilter::build(&block.hash(), txs, |o| spent.get(o).ok_or(*o)).unwrap();
        assert_eq!(filter.encoded(), v.filter, "{what}: filter");
        assert_eq!(filter.header(&v.prev_header), v.header, "{what}: header");
        check_filter(&v.filter, &v.prev_header, &v.header).unwrap();

        // The filter Core serves passes Core's decode check (`from_encoded` runs it), and matches
        // the scripts of the block's outputs.
        let received = BlockFilter::from_encoded(&block.hash(), v.filter.clone(), false).unwrap();
        let mut outputs = FilterQuery::new();
        for tx in block.txs() {
            add_output_scripts(&mut outputs, tx.raw());
        }
        if !outputs.is_empty() {
            assert!(received.match_all(&outputs).unwrap(), "{what}: outputs match");
        }
    }
}

/// Every vector that has a type adding elements must differ from the filter without the special
/// elements: the vectors exercise the code, not only the base filter.
#[test]
fn special_elements_are_in_cores_filters() {
    let mut types_exercised = BTreeSet::new();
    for v in vectors().iter().filter(|v| v.kind == "special") {
        let (block, spent) = load(v);
        let params = GcsParams::basic(&v.block_hash);
        let mut base = FilterQuery::new();
        for (i, tx) in block.txs().iter().enumerate() {
            add_output_scripts(&mut base, tx.raw());
            if i > 0 {
                for input in tx.inputs() {
                    add_script(&mut base, &spent[&input.prevout]);
                }
            }
        }
        let without = encode(&params, &mut base.clone()).unwrap();

        let mut with = base;
        for tx in block.txs() {
            add_special_tx_elements(&mut with, tx.raw());
        }
        let with = encode(&params, &mut with).unwrap();
        assert_eq!(with, v.filter, "{} block {}", v.test, v.height);

        let adds = v.tx_types.iter().any(|t| ADDS_ELEMENTS.contains(t));
        if adds {
            assert_ne!(
                without, v.filter,
                "{} block {}: the base filter alone equals Core's",
                v.test, v.height
            );
            types_exercised.extend(v.tx_types.iter().filter(|t| ADDS_ELEMENTS.contains(t)));
        } else {
            assert_eq!(
                without, v.filter,
                "{} block {}: a type that adds nothing changed the filter",
                v.test, v.height
            );
        }
    }
    assert_eq!(types_exercised, ADDS_ELEMENTS.into_iter().collect::<BTreeSet<u64>>());
}

/// Consecutive blocks of one chain are a run of Core's `cfheaders`: the headers recompute from
/// the filter hashes and the header before the run, and a wrong hash fails at its own link.
#[test]
fn consecutive_blocks_form_a_header_chain() {
    let all = vectors();
    let mut longest = 0;
    let mut runs = 0;
    let mut i = 0;
    while i < all.len() {
        let mut end = i + 1;
        while end < all.len()
            && all[end].test == all[i].test
            && all[end].height == all[end - 1].height + 1
        {
            end += 1;
        }
        let run = &all[i..end];
        let hashes: Vec<FilterHash> = run.iter().map(|v| filter_hash(&v.filter)).collect();
        let headers: Vec<FilterHeader> = run.iter().map(|v| v.header).collect();
        assert_eq!(extend_headers(&run[0].prev_header, &hashes), headers);
        check_links(&run[0].prev_header, &hashes, &headers).unwrap();
        if run.len() > 1 {
            runs += 1;
            let mut bad = hashes.clone();
            bad[1] = filter_hash(b"not the filter");
            match check_links(&run[0].prev_header, &bad, &headers) {
                Err(ChainError::Mismatch {
                    index: 1,
                    expected,
                    ..
                }) => assert_eq!(expected, headers[1]),
                other => panic!("{other:?}"),
            }
        }
        longest = longest.max(run.len());
        i = end;
    }
    assert!(runs >= 10 && longest >= 5, "{runs} runs, longest {longest}");
}
