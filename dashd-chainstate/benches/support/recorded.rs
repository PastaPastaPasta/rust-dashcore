//! Chain parameters and blocks from the recorders used by the evo benches.
#![allow(dead_code)]

use std::path::Path;
use std::sync::Arc;

use dash_consensus::primitives::{Block, decode_block};
use dash_params::{ChainOption, ChainParamsBuilder, ChainSetup, Network, Release};

pub fn params<'a>(args: impl IntoIterator<Item = &'a str>) -> Arc<ChainSetup> {
    let mut builder = ChainParamsBuilder::new(Network::Regtest, Release::V24_0);
    for arg in args {
        let (name, value) = arg.split_once('=').unwrap_or((arg, ""));
        if name == "-mocktime" {
            value.parse::<i64>().expect("the precursor's mock clock");
            continue;
        }
        let option = ChainOption::from_name(name).unwrap_or_else(|| panic!("chain option {name}"));
        builder = builder.arg(option, value);
    }
    Arc::new(builder.build().expect("recorded regtest parameters"))
}

/// The text recorder's complete chain arguments, after `# dashd -regtest`.
pub fn header_args(text: &str) -> &str {
    let mut headers = text.lines().filter_map(|line| line.strip_prefix("# dashd -regtest "));
    let args = headers.next().expect("the recording's chain arguments");
    assert!(headers.next().is_none(), "one chain-arguments header");
    args
}

/// The O8 recorder's node0, excluding genesis, and a clock a day past its tip.
pub fn recorded(node0: &Path) -> (Arc<ChainSetup>, Vec<Arc<Block>>, i64) {
    let extract: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(node0.join("extract.json")).expect("extract.json"),
    )
    .expect("extract JSON");
    let args = extract["chain_args"].as_array().expect("chain_args");
    let p = params(args.iter().map(|arg| arg.as_str().expect("a chain argument")));
    let raw = zstd::decode_all(
        std::fs::File::open(node0.join("blocks.dat.zst")).expect("the recorded blocks"),
    )
    .expect("zstd");
    let (mut rest, mut blocks) = (raw.as_slice(), Vec::new());
    while !rest.is_empty() {
        let (record, tail) = rest.split_at_checked(8).expect("loadblock framing");
        let size = u32::from_le_bytes(record[4..].try_into().expect("four bytes"));
        let (mut bytes, tail) = tail.split_at_checked(size as usize).expect("a framed block");
        blocks.push(Arc::new(decode_block(&mut bytes).expect("a block").into_value()));
        assert!(bytes.is_empty(), "one block per record");
        rest = tail;
    }
    blocks.remove(0);
    let now = i64::from(blocks.last().expect("blocks").header().raw().time()) + 86_400;
    (p, blocks, now)
}
