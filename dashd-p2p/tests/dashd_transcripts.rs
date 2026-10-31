//! Transcript equality with dashd v24.0.0-rc.3 (P2a §6.1, the phase E acceptance): each fixture
//! under `tests/fixtures/dashd-v24/` (`contrib/node/p2a-transcripts/capture.py`; provenance in the
//! directory's `PROVENANCE.md`) replayed by [`dashd_p2p::sim::transcript`], whose docs list the
//! masks and the messages it drops (later tasks' sends).

use std::sync::Arc;

use dash_params::{ChainParamsBuilder, Network, Release};
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::sim::Sim;
use dashd_p2p::sim::transcript::NoHooks;
use dashd_p2p::{Registry, Seams};

/// A node with dashd's defaults where `sim` departs from them: `-peertimeout` is 60 s.
fn sim(opts: ProcessorOptions) -> Sim {
    let p = Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let id = local_identity(&p, &IdentityOptions::new()).unwrap();
    Sim::with(p, id, opts, Registry::new(), Seams::default(), |cfg| {
        cfg.peer_timeout = 60;
    })
}

fn replay_with(name: &str, opts: ProcessorOptions) {
    let path = format!("{}/tests/fixtures/dashd-v24/{name}.txt", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    dashd_p2p::sim::transcript::replay(name, &text, sim(opts), &mut NoHooks);
}

fn replay(name: &str) {
    replay_with(name, ProcessorOptions::default());
}

/// `-blocksonly=1` (P5a D23): relay 0 in `version`, no `senddsq`, and the `tx` and `tx`-inv
/// disconnects.
#[test]
fn blocksonly_violations() {
    let opts = ProcessorOptions {
        blocks_only: true,
        ..ProcessorOptions::default()
    };
    replay_with("blocksonly-violations", opts);
}

macro_rules! transcripts {
    ($($test:ident => $file:literal,)*) => {
        $(
            #[test]
            fn $test() {
                replay($file);
            }
        )*
    };
}

transcripts! {
    handshake_inbound => "handshake-inbound",
    handshake_outbound_full_relay => "handshake-outbound-full-relay",
    handshake_block_relay_only => "handshake-block-relay-only",
    handshake_addr_fetch => "handshake-addr-fetch",
    handshake_feeler => "handshake-feeler",
    handshake_manual => "handshake-manual",
    headers_2000 => "headers-2000",
    headers_2000_bit11 => "headers-2000-bit11",
    headers2_8000 => "headers2-8000",
    announce => "announce",
    evict_slow_chain => "evict-slow-chain",
}
