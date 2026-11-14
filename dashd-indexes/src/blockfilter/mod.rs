// SPDX-License-Identifier: MIT

//! Basic filter construction and the block filter index's row codecs.

use dash_consensus::Block;
use dash_consensus::coins::BlockUndo;
use dash_filters::{BlockFilter, BuildError};

use crate::IndexError;

pub mod codec;

/// Builds the basic filter using the decoded undo's scripts in transaction/input order.
/// The undo must contain one record per non-coinbase transaction and one coin per input.
// Core: index/blockfilterindex.cpp:268-277,294; blockfilter.cpp:188-215,228-235
pub fn build_filter(block: &Block, undo: &BlockUndo) -> Result<BlockFilter, IndexError> {
    let Some((_, transactions)) = block.txs().split_first() else {
        return Err(IndexError::Corrupt("filter block has no coinbase".into()));
    };
    if transactions.len() != undo.txs.len() {
        return Err(IndexError::Corrupt("filter undo transaction count mismatch".into()));
    }
    for (tx, tx_undo) in transactions.iter().zip(&undo.txs) {
        if tx.inputs().len() != tx_undo.coins.len() {
            return Err(IndexError::Corrupt("filter undo input count mismatch".into()));
        }
    }

    // Core: undo.h:53-68; blockfilter.cpp:206-211
    let mut coins = undo.txs.iter().flat_map(|tx| &tx.coins);
    BlockFilter::build(&block.hash(), block.txs().iter().map(|tx| tx.raw()), |_| {
        coins
            .next()
            .map(|coin| coin.script_pubkey().as_slice())
            .ok_or_else(|| IndexError::Corrupt("filter undo coin missing".into()))
    })
    .map_err(|error| match error {
        BuildError::Source(error) => error,
        BuildError::Filter(error) => IndexError::Corrupt(format!("filter encoding: {error}")),
    })
}

#[cfg(test)]
mod tests {
    use dash_consensus::coins::{Coin, TxUndo};
    use dash_consensus::decode_block;
    use dash_core_vectors::hex::parse_hex;
    use dash_core_vectors::read_core_text;
    use dash_filters::FilterHeader;
    use serde_json::Value;

    use super::*;

    fn undo_from_scripts(block: &Block, scripts: &Value) -> BlockUndo {
        let mut scripts = scripts.as_array().unwrap().iter();
        let undo = BlockUndo {
            txs: block
                .txs()
                .iter()
                .skip(1)
                .map(|tx| TxUndo {
                    coins: tx
                        .inputs()
                        .iter()
                        .map(|_| {
                            let script = parse_hex(scripts.next().unwrap().as_str().unwrap());
                            Coin::new(0, &script, 0, false)
                        })
                        .collect(),
                })
                .collect(),
        };
        assert!(scripts.next().is_none());
        undo
    }

    fn check_vector(
        raw: &Value,
        scripts: &Value,
        hash: &Value,
        prev: &Value,
        filter: &Value,
        header: &Value,
    ) {
        let bytes = parse_hex(raw.as_str().unwrap());
        let block = decode_block(&mut bytes.as_slice()).unwrap().into_value();
        assert_eq!(block.hash().to_string(), hash.as_str().unwrap());
        let undo = undo_from_scripts(&block, scripts);
        let built = build_filter(&block, &undo).unwrap();
        assert_eq!(built.encoded(), parse_hex(filter.as_str().unwrap()));
        let prev: FilterHeader = prev.as_str().unwrap().parse().unwrap();
        assert_eq!(built.header(&prev).to_string(), header.as_str().unwrap());
    }

    #[test]
    fn blockfilters_json_matches_core() {
        let rows: Value =
            serde_json::from_str(&read_core_text("src/test/data/blockfilters.json")).unwrap();
        let rows = &rows.as_array().unwrap()[1..];
        assert_eq!(rows.len(), 7);
        for row in rows {
            check_vector(&row[2], &row[3], &row[1], &row[4], &row[5], &row[6]);
        }
    }

    #[test]
    fn special_transaction_filters_match_core() {
        let data: Value = serde_json::from_str(include_str!(
            "../../../dash-filters/tests/data/core_v24_special_tx_filters.json"
        ))
        .unwrap();
        let mut count = 0;
        for chain in data["chains"].as_array().unwrap() {
            for row in chain["blocks"].as_array().unwrap() {
                check_vector(
                    &row["block"],
                    &row["prev_scripts"],
                    &row["hash"],
                    &row["prev_header"],
                    &row["filter"],
                    &row["header"],
                );
                count += 1;
            }
        }
        assert_eq!(count, 149);
    }

    #[test]
    fn mismatched_undo_is_rejected() {
        let rows: Value =
            serde_json::from_str(&read_core_text("src/test/data/blockfilters.json")).unwrap();
        let row = rows
            .as_array()
            .unwrap()
            .iter()
            .skip(1)
            .find(|row| !row[3].as_array().unwrap().is_empty())
            .unwrap();
        let bytes = parse_hex(row[2].as_str().unwrap());
        let block = decode_block(&mut bytes.as_slice()).unwrap().into_value();
        let undo = undo_from_scripts(&block, &row[3]);
        for extra in [false, true] {
            let mut wrong = undo.clone();
            if extra {
                wrong.txs.push(TxUndo::default());
            } else {
                wrong.txs.pop();
            }
            assert!(matches!(build_filter(&block, &wrong), Err(IndexError::Corrupt(_))));
            let mut wrong = undo.clone();
            if extra {
                wrong.txs[0].coins.push(Coin::new(0, &[], 0, false));
            } else {
                wrong.txs[0].coins.pop();
            }
            assert!(matches!(build_filter(&block, &wrong), Err(IndexError::Corrupt(_))));
        }
    }
}
