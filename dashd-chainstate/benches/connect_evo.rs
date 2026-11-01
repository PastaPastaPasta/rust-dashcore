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
    targets = dip3_prequorum, mnlist_tip_publish
}
criterion_main!(benches);
