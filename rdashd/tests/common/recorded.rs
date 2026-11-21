//! The O8 recorder's dashd runs under `test-vectors/statedigest/regtest-v24`, read for
//! `submitblock`: DIFF-RPC's replays (`tests/rpc_diff.rs`), the dbcrash replay of Core-made
//! special transactions (`tests/dbcrash.rs`) and the ChainLock restart (`tests/cl_restart.rs`).

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

thread_local! {
    static LAST_RUN: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// The name of the run this thread loaded last, if any: the chain a pair started next replays.
pub fn last_run() -> Option<String> {
    LAST_RUN.with_borrow(Clone::clone)
}

pub fn recorded_run(name: &str) -> RecordedRun {
    LAST_RUN.set(Some(name.to_owned()));
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
    let (mut rest, mut blocks) = (raw.as_slice(), Vec::new());
    while let Some((record, tail)) = rest.split_at_checked(8) {
        let size = u32::from_le_bytes(record[4..].try_into().unwrap()) as usize;
        let (block, tail) = tail.split_at(size);
        blocks.push(block.to_vec());
        rest = tail;
    }
    blocks.remove(0);
    RecordedRun::new(args, blocks)
}

/// A `chain.txt` run under `dashd-chainstate/tests/data` (`path` below it): `#` lines, the second
/// `# dashd -regtest <chain args>`, then each block's hex from height 1. `name` labels it.
pub fn chain_txt_run(name: &str, path: &str) -> RecordedRun {
    LAST_RUN.set(Some(name.to_owned()));
    let text = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../dashd-chainstate/tests/data").join(path),
    )
    .unwrap();
    let args = text
        .lines()
        .find_map(|l| l.strip_prefix("# dashd -regtest "))
        .expect("the chain's dashd line")
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let blocks = text.lines().filter(|l| !l.starts_with('#') && !l.is_empty()).map(parse_hex);
    RecordedRun::new(args, blocks.collect())
}

impl RecordedRun {
    fn new(args: Vec<String>, raw: Vec<Vec<u8>>) -> RecordedRun {
        let hashes = raw
            .iter()
            .map(|block| {
                let decoded = dash_consensus::primitives::decode_block(&mut &block[..]);
                decoded.expect("a block").into_value().hash().to_string()
            })
            .collect();
        RecordedRun {
            args,
            blocks: raw.iter().map(|b| hex(b)).collect(),
            hashes,
        }
    }

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
