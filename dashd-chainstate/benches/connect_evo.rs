//! `connect_evo` (CH7a-5a; CH7 §7.1's evo PF0 rows): V connecting blocks past DIP0003 through the
//! node's connector with `NodeEvo` and EV1's checks.
//!
//! - `evo/dip3-prequorum`: dashd v24.0.0-rc.4's recorded regtest chain (`tests/data/ch7a4b`,
//!   DIP0003 at 48, DIP0024 moved out), blocks 49 to 200: CbTx blocks with an empty list, and six
//!   of `llmq_test`'s mining windows of null commitments (§7.1's "a regtest chain with null
//!   commitments only"). Setup connects 1 to 48; the region is the 152 blocks after.
//! - `evo/mnlist-tip-publish`: dashd's recorded masternode chain (`tests/data/ch7a5`, the same
//!   parameters), blocks 116 to 300, each of whose publications carries a non-empty list: two
//!   registrations, a ProUpServTx, a ProUpRegTx, a ProUpRevTx and a collateral spend, and H7's
//!   notification at each change. A publication is one `Arc` clone of the `imbl` list whatever its
//!   size (D16), so two masternodes stand for many. Setup connects 1 to 115.
//! - `evo/v20-window` (CH7b-3 C10): dashd's recorded `feature_llmq_chainlocks` run
//!   (`test-vectors/statedigest/regtest-v24`, V20 at 200), blocks 200 to 352: CbTxs of version 3
//!   from 200, and step 10 verifying each of the 44 CbTx ChainLocks from 309. Setup connects 1 to
//!   199.
//! - `evo/dip24-cycle` (CH7b-3 C10): the recorded `feature_llmq_rotation` run, the DIP0024 cycle
//!   based at 384 through its mining window (`llmq_test_dip0024`: interval 24, window 12 to 20),
//!   384 to 404: the rotated commitments mined at 396, and step 10 verifying the CbTx ChainLock of
//!   every block. Setup connects 1 to 383.
//!
//! The last two are regtest stand-ins; the mainnet windows come after E-O8-1. No row has a
//! threshold against Core: its counts are deferred (DEC-783).
//!
//! **The node.** `support/evo_node.rs`'s, which never flushes. The clock is the recording's, a
//! day on.
//!
//! The bench builds without the crate's test features, as `connect_chain` does.

// PROCESS.md: benches measure production only, so a test feature turned on here (by a self
// dev-dependency or `--all-features`) fails the build. Clippy's `--all-features` run is exempt.
#[cfg(all(not(clippy), any(feature = "crash", feature = "planted-bugs")))]
compile_error!("benches measure production only: build them without `crash` and `planted-bugs`");

#[path = "support/evo_node.rs"]
mod evo_node;

use std::sync::Arc;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use dash_consensus::primitives::{Block, decode_block};
use dash_params::{ChainOption, ChainParamsBuilder, ChainSetup, Network, Release};

use evo_node::{Flush, Node, decode};

/// dashd's main chain, blocks 1 to 200, one hex per line after `#` header lines.
const MAIN: &str = include_str!("../tests/data/ch7a4b/blocks.txt");
/// The recording's `-mocktime`: its blocks' times start there.
const MOCKTIME: i64 = 1_700_000_000;
/// DIP0003's height on the recorded chain.
const DIP3: usize = 48;
/// dashd's masternode chain, blocks 1 to 300, under the same parameters.
const MN: &str = include_str!("../tests/data/ch7a5/blocks.txt");
/// The masternode chain's first registration.
const FIRST_MN: usize = 116;
/// dashd's recorded runs: the O8 recorder's node0 of each.
const RECORDED: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../test-vectors/statedigest/regtest-v24");
/// V20's height on the recorded ChainLock run.
const V20: usize = 200;
/// The rotation run's DIP0024 cycle base and the end of its mining window.
const DIP24_BASE: usize = 384;
const DIP24_WINDOW_END: usize = 404;

/// The recording's parameters: regtest with `-dip3params=48:48
/// -testactivationheight=dip0024@1000`; `evo_node` runs with `-checkblockindex` off.
fn params() -> Arc<ChainSetup> {
    let p = ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
        .arg(ChainOption::Dip3Params, "48:48")
        .arg(ChainOption::TestActivationHeight, "dip0024@1000")
        .build()
        .expect("regtest");
    Arc::new(p)
}

/// `test`'s node0: regtest at v24 with `extract.json`'s `chain_args` (one a line), its blocks
/// after genesis (element `i` at height `i + 1`), and a clock a day past its tip.
fn recorded(test: &str) -> (Arc<ChainSetup>, Vec<Arc<Block>>, i64) {
    let node0 = std::path::Path::new(RECORDED).join(test).join("node0");
    let extract = std::fs::read_to_string(node0.join("extract.json")).expect("extract.json");
    let mut lines = extract.lines().map(str::trim);
    lines.by_ref().find(|l| *l == "\"chain_args\": [").expect("chain_args");
    let mut builder = ChainParamsBuilder::new(Network::Regtest, Release::V24_0);
    for arg in lines.take_while(|l| !l.starts_with(']')) {
        let arg = arg.trim_end_matches(',').trim_matches('"');
        let (name, value) = arg.split_once('=').unwrap_or((arg, ""));
        let option = ChainOption::from_name(name).unwrap_or_else(|| panic!("{test}: {name}"));
        builder = builder.arg(option, value);
    }
    let file = std::fs::File::open(node0.join("blocks.dat.zst")).expect("the run");
    let raw = zstd::decode_all(file).expect("zstd");
    let (mut rest, mut blocks) = (raw.as_slice(), Vec::new());
    while let Some((record, tail)) = rest.split_at_checked(8) {
        let size = u32::from_le_bytes(record[4..].try_into().expect("four bytes"));
        let (mut bytes, tail) = tail.split_at(usize::try_from(size).expect("a size"));
        blocks.push(Arc::new(decode_block(&mut bytes).expect("a block").into_value()));
        rest = tail;
    }
    blocks.remove(0);
    let now = i64::from(blocks.last().expect("blocks").header().raw().time) + 86_400;
    (Arc::new(builder.build().expect("regtest")), blocks, now)
}

/// A row connecting `measured` on a node that has connected `setup`.
fn connects(
    c: &mut Criterion,
    id: &str,
    p: &Arc<ChainSetup>,
    now: i64,
    blocks: [&[Arc<Block>]; 2],
) {
    let [setup, measured] = blocks;
    c.bench_function(id, |b| {
        b.iter_batched(
            || {
                let node = Node::start(Arc::clone(p), now, Flush::Never);
                node.connect(setup);
                node
            },
            |node| {
                node.connect(measured);
                node
            },
            BatchSize::PerIteration,
        );
    });
}

fn v20_window(c: &mut Criterion) {
    let (p, blocks, now) = recorded("feature_llmq_chainlocks");
    assert_eq!(blocks.len(), 352);
    let (setup, measured) = blocks.split_at(V20 - 1);
    connects(c, "evo/v20-window", &p, now, [setup, measured]);
}

fn dip24_cycle(c: &mut Criterion) {
    let (p, blocks, now) = recorded("feature_llmq_rotation");
    let (setup, rest) = blocks.split_at(DIP24_BASE - 1);
    let measured = &rest[..=DIP24_WINDOW_END - DIP24_BASE];
    connects(c, "evo/dip24-cycle", &p, now, [setup, measured]);
}

fn dip3_prequorum(c: &mut Criterion) {
    let blocks = decode(MAIN);
    assert_eq!(blocks.len(), 200);
    let (setup, measured) = blocks.split_at(DIP3);
    c.bench_function("evo/dip3-prequorum", |b| {
        b.iter_batched(
            || {
                let node = Node::start(params(), MOCKTIME + 86_400, Flush::Never);
                node.connect(setup);
                node
            },
            |node| {
                node.connect(measured);
                node
            },
            BatchSize::PerIteration,
        );
    });
}

fn mnlist_tip_publish(c: &mut Criterion) {
    let blocks = decode(MN);
    assert_eq!(blocks.len(), 300);
    let (setup, measured) = blocks.split_at(FIRST_MN - 1);
    // The region's publications carry the list: from 116 on, it is never empty.
    let node = Node::start(params(), MOCKTIME + 86_400, Flush::Never);
    node.connect(setup);
    assert!(node.v.view().mn_list.is_empty(), "no masternode before 116");
    for block in measured {
        node.connect(std::slice::from_ref(block));
        assert!(!node.v.view().mn_list.is_empty(), "{}: an empty list", block.hash());
    }
    drop(node);
    c.bench_function("evo/mnlist-tip-publish", |b| {
        b.iter_batched(
            || {
                let node = Node::start(params(), MOCKTIME + 86_400, Flush::Never);
                node.connect(setup);
                node
            },
            |node| {
                node.connect(measured);
                node
            },
            BatchSize::PerIteration,
        );
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(10);
    targets = dip3_prequorum, mnlist_tip_publish, v20_window, dip24_cycle
}
criterion_main!(benches);
