//! Stored-block replay skips contextual block and header checks, as dashd v24.0.0 does.
#![cfg(unix)]

mod common;

use common::mine::*;
use common::node::*;
use common::*;

const HEIGHT: u8 = 20;

#[derive(Clone, Copy, Debug)]
enum Fixture {
    CoinbaseHeight,
    HeaderVersion,
}

impl Fixture {
    fn activation(self, height: u32) -> String {
        let name = match self {
            Self::CoinbaseHeight => "bip34",
            Self::HeaderVersion => "cltv",
        };
        format!("-testactivationheight={name}@{height}")
    }

    fn rejection(self) -> &'static str {
        match self {
            Self::CoinbaseHeight => "bad-cb-height",
            Self::HeaderVersion => "bad-version(0x00000003)",
        }
    }

    fn block(self, prev: &[u8; 32]) -> Mined {
        let (height, version) = match self {
            Self::CoinbaseHeight => (HEIGHT + 1, 4),
            Self::HeaderVersion => (HEIGHT, 3),
        };
        mine_version(
            prev,
            GENESIS_TIME + 60 * u32::from(HEIGHT),
            &[coinbase(height, 50_0000_0000)],
            None,
            true,
            version,
        )
    }
}

#[derive(Debug, PartialEq)]
struct State {
    best: String,
    tips: Vec<Tip>,
    serialized: Vec<(String, String)>,
    muhash: Vec<(String, String)>,
}

fn state(node: &Node, block: &Mined) -> State {
    let best = node.best();
    let tips = node.tips();
    assert_eq!(best, shown(&block.hash), "{}: fixture must be active; tips: {tips:?}", node.name);
    assert_eq!(status_of(&tips, &block.hash), Some("active"), "{}", node.name);
    let serialized = utxo_set(&node.call("gettxoutsetinfo", "[]"));
    let muhash = node.utxo_set();
    assert!(!value_of(&serialized, "hash_serialized_2").trim_matches('"').is_empty());
    assert!(!value_of(&muhash, "muhash").trim_matches('"').is_empty());
    State {
        best,
        tips,
        serialized,
        muhash,
    }
}

fn restart(node: &mut Node, fixture: Fixture, replay: bool, block: &Mined) {
    node.stop();
    node.flags.retain(|flag| !flag.starts_with("-testactivationheight="));
    node.flags.push(fixture.activation(2));
    if replay {
        node.flags.push("-reindex-chainstate".to_owned());
    }
    node.start(0);
    wait_until(&format!("{}: {fixture:?} replay", node.name), || {
        node.best() == shown(&block.hash) || node.new_log().contains(fixture.rejection())
    });
}

fn contextual_skip(fixture: Fixture) {
    let home = RetainedHome::new("ch5-2b-context-");
    let dashd = require_dashd();
    let late = fixture.activation(300);
    let mut core = Node::new("dashd", &dashd, home.path(), &[&late]);
    core.start(0);
    let mut prev = genesis(&core);
    let mut blocks = Vec::new();
    for height in 1..HEIGHT {
        let block = next(&prev, height, 50_0000_0000);
        prev = block.hash;
        blocks.push(block);
    }
    let block = fixture.block(&prev);
    for block in blocks.iter().chain([&block]) {
        assert_eq!(core.submit(block), "null", "dashd: initial submit");
    }
    let before = state(&core, &block);
    restart(&mut core, fixture, true, &block);
    let proven = state(&core, &block);
    assert_eq!(proven, before, "dashd pre-proof: {fixture:?}");
    eprintln!("DASHD PRE-PROOF {fixture:?}: {proven:#?}");
    core.stop();

    // Core's changed-activation replay must be proved before starting rdashd.
    let mut rust = Node::rdashd(home.path(), &[&late], Storage::Live);
    rust.start(0);
    for block in blocks.iter().chain([&block]) {
        assert_eq!(rust.submit(block), "null", "rdashd: initial submit");
    }
    assert_eq!(state(&rust, &block), proven, "rdashd initial state: {fixture:?}");
    restart(&mut rust, fixture, false, &block);
    assert_eq!(state(&rust, &block), proven, "changed activation loads the stored index");
    restart(&mut rust, fixture, true, &block);
    assert_eq!(state(&rust, &block), proven, "rdashd replay: {fixture:?}");
    rust.stop();
}

#[test]
fn reindex_chainstate_skips_coinbase_height_context_as_dashd_v24() {
    contextual_skip(Fixture::CoinbaseHeight);
}

#[test]
fn reindex_chainstate_skips_header_version_context_as_dashd_v24() {
    contextual_skip(Fixture::HeaderVersion);
}
