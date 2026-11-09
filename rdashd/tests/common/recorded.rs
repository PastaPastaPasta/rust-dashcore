//! The O8 recorder's dashd runs under `test-vectors/statedigest/regtest-v24`, read for
//! `submitblock`: DIFF-RPC's replays (`tests/rpc_diff.rs`) and the dbcrash replay of Core-made
//! special transactions (`tests/dbcrash.rs`).

use std::path::Path;

use dash_args::univalue::SettingsValue as UniValue;

use super::mine::{hex, parse_hex};

/// One O8 recorder run's node0 (dashd v24, `test-vectors/statedigest/regtest-v24/<name>`): its
/// `chain_args`, and its blocks after genesis as `submitblock` hex with their hashes in RPC hex,
/// the first at height 1.
pub struct RecordedRun {
    pub args: Vec<String>,
    pub blocks: Vec<String>,
    pub hashes: Vec<String>,
}

pub fn recorded_run(name: &str) -> RecordedRun {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../test-vectors/statedigest/regtest-v24")
        .join(name)
        .join("node0");
    let extract = std::fs::read_to_string(dir.join("extract.json")).unwrap();
    let UniValue::Obj(fields) = UniValue::read(extract.as_bytes()).unwrap() else {
        panic!()
    };
    let UniValue::Arr(args) = &fields.iter().find(|(k, _)| k == "chain_args").unwrap().1 else {
        panic!("chain_args")
    };
    let args = args.iter().map(|a| a.get_str().unwrap().to_owned()).collect();
    let raw = zstd::decode_all(std::fs::File::open(dir.join("blocks.dat.zst")).unwrap()).unwrap();
    // blk records: the network's magic, the size, the block.
    let (mut rest, mut blocks, mut hashes) = (raw.as_slice(), Vec::new(), Vec::new());
    while let Some((record, tail)) = rest.split_at_checked(8) {
        let size = u32::from_le_bytes(record[4..].try_into().unwrap()) as usize;
        let (block, tail) = tail.split_at(size);
        let decoded = dash_consensus::primitives::decode_block(&mut &block[..]).expect("a block");
        hashes.push(decoded.into_value().hash().to_string());
        blocks.push(hex(block));
        rest = tail;
    }
    blocks.remove(0);
    hashes.remove(0);
    RecordedRun {
        args,
        blocks,
        hashes,
    }
}

impl RecordedRun {
    pub fn args(&self) -> Vec<&str> {
        self.args.iter().map(String::as_str).collect()
    }

    pub fn tip(&self) -> usize {
        self.blocks.len()
    }

    /// The block at `height`'s hash, in RPC hex.
    pub fn hash(&self, height: usize) -> &str {
        &self.hashes[height - 1]
    }

    /// The run's asset unlocks: (index, block height, transaction hex), in block order.
    pub fn asset_unlocks(&self) -> Vec<(u64, usize, String)> {
        let mut out = Vec::new();
        for (i, block) in self.blocks.iter().enumerate() {
            let bytes = parse_hex(block);
            let block = dash_consensus::primitives::decode_block(&mut bytes.as_slice()).unwrap();
            for tx in block.into_value().txs() {
                let Some(Ok(decoded)) = tx.payload() else {
                    continue;
                };
                if let dash_primitives::SpecialPayload::AssetUnlock(unlock) = decoded.payload() {
                    let raw = dash_consensus::CoreSerialize::core_serialize(tx.raw());
                    out.push((unlock.index(), i + 1, hex(&raw)));
                }
            }
        }
        out
    }
}
