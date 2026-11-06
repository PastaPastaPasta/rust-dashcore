// SPDX-License-Identifier: MIT

//! L0: Dash Core's `src/test/data/blockfilters.json` (v24.0.0-rc.3), the vectors of its
//! `blockfilter_tests.cpp` `blockfilters_json_test`, through `dash-filters` alone.
//!
//! Row layout: height, block hash, block, the scripts the block spends (non-coinbase inputs, in
//! order), previous basic header, basic filter, basic header, notes. The first row is the header.

use std::collections::BTreeMap;

use dash_consensus::decode_block;
use dash_core_vectors::hex::{parse_hex, uint256_from_hex};
use dash_core_vectors::read_core_text;
use dash_filters::{
    BlockFilter, ChainError, FilterError, FilterHeader, FilterQuery, check_filter, check_links,
    extend_headers, filter_hash, filter_header,
};
use dash_primitives::OutPoint;
use dash_types::Numeric as _;
use serde_json::Value;

struct Row {
    block_hash: [u8; 32],
    block: Vec<u8>,
    scripts: Vec<Vec<u8>>,
    prev_header: FilterHeader,
    filter: Vec<u8>,
    header: FilterHeader,
}

fn rows() -> Vec<Row> {
    let text = read_core_text("src/test/data/blockfilters.json");
    let data: Value = serde_json::from_str(&text).unwrap();
    let str_at = |t: &Value, i: usize| t[i].as_str().unwrap().to_owned();
    data.as_array()
        .unwrap()
        .iter()
        .skip(1)
        .map(|t| Row {
            block_hash: uint256_from_hex(&str_at(t, 1)),
            block: parse_hex(&str_at(t, 2)),
            scripts: t[3]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| parse_hex(s.as_str().unwrap()))
                .collect(),
            prev_header: FilterHeader::from_lendian(uint256_from_hex(&str_at(t, 4))),
            filter: parse_hex(&str_at(t, 5)),
            header: FilterHeader::from_lendian(uint256_from_hex(&str_at(t, 6))),
        })
        .collect()
}

#[test]
fn blockfilters_json() {
    let rows = rows();
    assert_eq!(rows.len(), 7, "the file holds 7 blocks after its header row");
    for row in &rows {
        let block = decode_block(&mut row.block.as_slice()).unwrap().into_value();
        assert_eq!(block.hash().to_lendian(), row.block_hash);

        // Core's BlockUndo, as the closure: the spent scripts in input order.
        let mut spent = BTreeMap::<OutPoint, Vec<u8>>::new();
        let mut scripts = row.scripts.iter();
        for tx in block.txs().iter().skip(1) {
            for input in tx.inputs() {
                spent.insert(input.prevout, scripts.next().expect("a script per input").clone());
            }
        }
        assert!(scripts.next().is_none(), "no script left over");

        let txs: Vec<_> = block.txs().iter().map(|tx| tx.raw()).collect();
        let filter = BlockFilter::build(&block.hash(), txs, |o| spent.get(o).ok_or(*o)).unwrap();
        assert_eq!(filter.encoded(), row.filter, "filter bytes");
        assert_eq!(filter.header(&row.prev_header), row.header, "filter header");
        assert_eq!(filter.hash(), filter_hash(&row.filter));

        // Core's own decode check accepts it, and it matches every script it holds.
        let received = BlockFilter::from_encoded(&block.hash(), row.filter.clone(), false).unwrap();
        assert_eq!(received, filter);
        let all: FilterQuery = spent.values().filter(|s| !s.is_empty()).collect();
        assert!(received.match_all(&all).unwrap());
        for script in spent.values().filter(|s| !s.is_empty()) {
            let one: FilterQuery = std::iter::once(script).collect();
            assert!(received.match_any(&one).unwrap());
        }
    }
}

#[test]
fn filter_header_links() {
    // Core's rows are single blocks (heights 0, 2, 3, ...): each is one link of the chain.
    let rows = rows();
    assert_eq!(
        rows[0].prev_header,
        FilterHeader::default(),
        "the genesis filter follows the zero header"
    );
    for row in &rows {
        let hash = filter_hash(&row.filter);
        assert_eq!(filter_header(&hash, &row.prev_header), row.header);
        assert_eq!(extend_headers(&row.prev_header, &[hash]), [row.header]);
        check_filter(&row.filter, &row.prev_header, &row.header).unwrap();
        check_links(&row.prev_header, &[hash], &[row.header]).unwrap();
    }

    let row = &rows[4];
    // A filter whose bytes differ does not match its header.
    let mut tampered = row.filter.clone();
    *tampered.last_mut().unwrap() ^= 1;
    let err = check_filter(&tampered, &row.prev_header, &row.header).unwrap_err();
    assert!(
        matches!(err, ChainError::Mismatch { index: 0, expected, .. } if expected == row.header)
    );
    // Nor does a filter chained to the wrong predecessor.
    assert!(check_filter(&row.filter, &rows[2].header, &row.header).is_err());
    // A batch needs one header per hash.
    let hash = filter_hash(&row.filter);
    assert_eq!(
        check_links(&row.prev_header, &[hash, hash], &[row.header]),
        Err(ChainError::Length {
            hashes: 2,
            headers: 1
        })
    );
    // The chain is order-sensitive: the second link of a pair depends on the first.
    let two = extend_headers(&row.prev_header, &[hash, hash]);
    assert_eq!(two[0], row.header);
    assert_eq!(two[1], filter_header(&hash, &row.header));
    assert_ne!(two[0], two[1]);
}

#[test]
fn received_filters_are_checked_as_core_checks_them() {
    let rows = rows();
    let row = rows.iter().find(|r| r.filter.len() > 4).unwrap();
    let block = decode_block(&mut row.block.as_slice()).unwrap().into_value();
    let hash = block.hash();

    let mut excess = row.filter.clone();
    excess.push(0);
    assert_eq!(
        BlockFilter::from_encoded(&hash, excess.clone(), false),
        Err(FilterError::ExcessData)
    );
    // `skip_decode_check` keeps it, as Core's does, but not a count Core refuses.
    assert!(BlockFilter::from_encoded(&hash, excess, true).is_ok());
    assert_eq!(
        BlockFilter::from_encoded(&hash, vec![0xfd, 0x05, 0x00], true),
        Err(FilterError::BadCount)
    );
    assert_eq!(
        BlockFilter::from_encoded(&hash, vec![0xfe, 0x01, 0x00, 0x00, 0x02], true),
        Err(FilterError::BadCount)
    );
    let cut = row.filter[..row.filter.len() - 1].to_vec();
    assert_eq!(BlockFilter::from_encoded(&hash, cut, false), Err(FilterError::Truncated));
}
