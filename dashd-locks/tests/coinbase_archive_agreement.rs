//! Supplementary agreement with carried CbTx locks in the v24.0.0-rc.3 archive.
//! Recorded best heights check extraction arithmetic, not Core handler causality.

use std::collections::HashMap;

use dash_consensus::{Block, decode_block};
use dash_core_vectors::{hex::try_parse_hex, read_vector_file};
use dash_evo::bls::BlsCtx;
use dash_evo::cbtx::{CoinbaseFacts, nonnull_coinbase_chainlock};
use dash_evo::locks::ClsigHashSchemes;
use dash_primitives::BlockHash;
use dashd_locks::chainlock::coinbase::coinbase_clsig;
use serde_json::Value;

fn recording(name: &str, file: &str) -> Vec<u8> {
    let bytes = read_vector_file(&format!("statedigest/regtest-v24/{name}/node0/{file}.zst"));
    zstd::decode_all(bytes.as_slice()).unwrap()
}

fn check_recording(name: &str, v20_height: u32) {
    let raw = recording(name, "blocks.dat");
    let mut rest = raw.as_slice();
    let mut blocks: Vec<Block> = Vec::new();
    while !rest.is_empty() {
        assert_eq!(&rest[..4], &[0xfc, 0xc1, 0xb7, 0xdc]);
        let size = u32::from_le_bytes(rest[4..8].try_into().unwrap()) as usize;
        let mut bytes = &rest[8..8 + size];
        blocks.push(decode_block(&mut bytes).unwrap().into_value());
        assert!(bytes.is_empty());
        rest = &rest[8 + size..];
    }
    let raw = recording(name, "decisions.jsonl");
    let rows: Vec<Value> =
        String::from_utf8(raw).unwrap().lines().map(|s| serde_json::from_str(s).unwrap()).collect();
    let decisions: HashMap<_, _> = rows
        .iter()
        .filter(|r| r["org"] == "tip")
        .map(|r| (r["h"].as_u64().unwrap() as u32, r))
        .collect();
    let mut known = HashMap::new();
    for row in &rows {
        for q in row["q"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|q| q[0] == "cl_best" && !q[1].is_null())
        {
            let mut bytes: [u8; 32] =
                try_parse_hex(q[2].as_str().unwrap()).unwrap().try_into().unwrap();
            bytes.reverse();
            known.insert(
                (q[1].as_i64().unwrap() as i32, BlockHash::from(bytes)),
                try_parse_hex(q[3].as_str().unwrap()).unwrap(),
            );
        }
    }
    let (mut candidates, mut matched, mut suppressed, mut newer) = (0, 0, 0, 0);
    for (height, block) in blocks.iter().enumerate() {
        let height = height as u32;
        if height > 0 {
            assert_eq!(block.header().prev_hash(), blocks[height as usize - 1].hash());
        }
        let facts = CoinbaseFacts::of(Some(block));
        let ancestor = |h: u32| blocks.get(h as usize).map(Block::hash);
        let extracted = coinbase_clsig(
            &facts,
            height,
            height >= v20_height,
            true,
            true,
            -1,
            ancestor,
            ClsigHashSchemes::both(BlsCtx::BASIC),
        );
        let reference =
            nonnull_coinbase_chainlock(&facts, height, height >= v20_height, BlsCtx::BASIC);
        assert_eq!(extracted.is_some(), reference.is_some(), "height {height}");
        let Some((read, _)) = extracted else {
            continue;
        };
        let sig = read.msg;
        candidates += 1;
        let (reference_sig, diff) = reference.unwrap();
        assert_eq!(sig.height(), (height - diff - 1) as i32);
        assert_eq!(sig.sig().as_bytes(), &reference_sig.to_bytes(BlsCtx::BASIC.scheme()));
        if let Some(recorded_sig) = known.get(&(sig.height(), sig.block_hash())) {
            assert_eq!(sig.sig().as_bytes().as_slice(), recorded_sig.as_slice());
            matched += 1;
        }
        let Some(row) = decisions.get(&height) else {
            continue;
        };
        let best = row["q"].as_array().unwrap().iter().find(|q| q[0] == "cl_best").unwrap();
        let best_height = best[1].as_i64().map_or(-1, |h| h as i32);
        let filtered = coinbase_clsig(
            &facts,
            height,
            true,
            true,
            true,
            best_height,
            ancestor,
            ClsigHashSchemes::both(BlsCtx::BASIC),
        );
        assert_eq!(filtered.is_some(), sig.height() > best_height, "recorded best at {height}");
        if filtered.is_some() {
            newer += 1;
        } else {
            suppressed += 1;
        }
    }
    if v20_height == 200 {
        assert_eq!((blocks.len(), candidates, matched, suppressed, newer), (353, 44, 41, 40, 4));
    } else {
        assert_eq!((blocks.len(), candidates), (244, 0));
    }
    eprintln!(
        "{} blocks, {candidates} coinbase locks, {matched} recorded signatures, {suppressed} arithmetic height suppressions, {newer} newer locks",
        blocks.len()
    );
}

#[test]
fn extraction_agrees_with_rc3_archives_carried_coinbase_locks() {
    check_recording("feature_llmq_chainlocks", 200);
}

#[test]
fn recorded_conflict_blocks_before_v20_have_no_coinbase_locks() {
    check_recording("feature_llmq_is_cl_conflicts", 2000);
}
