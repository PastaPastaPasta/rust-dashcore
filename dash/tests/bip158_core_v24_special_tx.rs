// SPDX-License-Identifier: CC0-1.0

//! The dashcore adapter of `dash-filters` against Dash Core v24.0.0-rc.3's own basic filters of
//! 149 regtest blocks that hold special transactions: `BlockFilter::new_script_filter` must
//! produce, for each block, the bytes `getblockfilter` returned. The vectors are the ones of
//! `dash-filters` (`dash-filters/tests/data/PROVENANCE.md`), read from there rather than copied.

use std::collections::HashMap;
use std::path::Path;

use dashcore::bip158::{BlockFilter, Error};
use dashcore::consensus::encode::deserialize;
use dashcore::{Block, OutPoint, ScriptBuf};
use serde_json::Value;

fn hex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2));
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

#[test]
fn new_script_filter_equals_cores_filters() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../dash-filters/tests/data/core_v24_special_tx_filters.json");
    let data: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(data["dashd"], "Dash Core version v24.0.0-rc.3");

    let mut blocks = 0;
    let mut special = 0;
    for chain in data["chains"].as_array().unwrap() {
        for b in chain["blocks"].as_array().unwrap() {
            let block: Block = deserialize(&hex(b["block"].as_str().unwrap())).unwrap();
            let mut scripts = b["prev_scripts"].as_array().unwrap().iter();
            let mut spent = HashMap::new();
            for tx in block.txdata.iter().skip(1) {
                for input in &tx.input {
                    let script = hex(scripts.next().expect("a script per input").as_str().unwrap());
                    spent.insert(input.previous_output, ScriptBuf::from_bytes(script));
                }
            }
            assert!(scripts.next().is_none());

            let filter = BlockFilter::new_script_filter(&block, |o: &OutPoint| {
                spent.get(o).cloned().ok_or(Error::UtxoMissing(*o))
            })
            .unwrap();
            assert_eq!(
                filter.content,
                hex(b["filter"].as_str().unwrap()),
                "{} block {}",
                chain["test"],
                b["height"]
            );
            blocks += 1;
            special += usize::from(b["kind"] == "special");
        }
    }
    assert_eq!((blocks, special), (149, 110));
}
