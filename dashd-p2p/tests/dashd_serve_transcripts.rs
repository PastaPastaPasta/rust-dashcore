//! P4b's transcript equality with dashd v24.0.0-rc.3 (P4b D12): the `serve-*` fixtures under
//! `tests/fixtures/dashd-v24/` (`contrib/node/p4b-transcripts/capture.py`; provenance in the
//! directory's `PROVENANCE.md`) replayed by [`dashd_p2p::sim::transcript`]. dashd mined each
//! fixture's blocks; their `submitblock` records store them in the sim, which then serves
//! `getdata`, `getblocks`, `getblocktxn` and the `filter*` messages as dashd did.

use std::sync::Arc;

use dash_p2p::NetConfig;
use dash_params::{ChainParamsBuilder, NetworkKind, Release};
use dashd_p2p::fingerprint::{IdentityOptions, local_identity, local_services};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::sim::Sim;
use dashd_p2p::sim::transcript::NoHooks;
use dashd_p2p::{Registry, Seams};

/// The dashd arguments a fixture adds to the capture tool's.
#[derive(Default)]
struct Args {
    /// `-peerbloomfilters=0`.
    no_bloom: bool,
    /// `-prune=1`: NODE_NETWORK_LIMITED instead of NODE_NETWORK, and Core's `fPruneMode`.
    pruned: bool,
}

/// A node with dashd's defaults where `sim` departs from them (`-peertimeout` is 60 s), plus
/// the fixture's arguments.
fn sim(args: &Args) -> Sim {
    let p =
        Arc::new(ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().unwrap());
    let opts = IdentityOptions {
        peer_bloom_filters: !args.no_bloom,
        pruned: args.pruned,
        ..IdentityOptions::new()
    };
    let id = local_identity(&p, &opts).unwrap();
    let sim = Sim::with(
        p,
        id,
        ProcessorOptions::default(),
        Registry::new(),
        Seams::default(),
        |cfg: &mut NetConfig| {
            cfg.peer_timeout = 60;
            cfg.local_services = local_services(&opts);
        },
    );
    sim.chain.set_prune_mode(args.pruned);
    sim
}

fn replay(name: &str, args: Args) {
    let path = format!("{}/tests/fixtures/dashd-v24/{name}.txt", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    dashd_p2p::sim::transcript::replay(name, &text, sim(&args), &mut NoHooks);
}

/// `getdata` of blocks, compact blocks (at the tip and past MAX_CMPCTBLOCK_DEPTH), a filtered
/// block with no filter, an unknown block, and several blocks in one message.
#[test]
fn serve_blocks() {
    replay("serve-blocks", Args::default());
}

/// `getblocks` from genesis (500 `inv`s, then the tip's `inv` after the 500th block is fetched),
/// to a `hashStop`, and a 102-hash locator.
#[test]
fn serve_getblocks() {
    replay("serve-getblocks", Args::default());
}

/// BIP37: `merkleblock` with and without a matched transaction, `filterclear`, and the
/// filter size, hash-function and `filteradd` limits.
#[test]
fn serve_filters() {
    replay("serve-filters", Args::default());
}

/// BIP37 over DIP3's special transactions: a filter on a masternode's owner key ID matches its
/// ProRegTx; under BLOOM_UPDATE_ALL the ProTx hash is inserted, so its ProUpRegTx, ProUpServTx
/// and ProUpRevTx match too, and under BLOOM_UPDATE_NONE they don't.
#[test]
fn serve_filters_protx() {
    replay("serve-filters-protx", Args::default());
}

/// `-peerbloomfilters=0`: each `filter*` message disconnects.
#[test]
fn serve_nobloom() {
    replay(
        "serve-nobloom",
        Args {
            no_bloom: true,
            ..Args::default()
        },
    );
}

/// `getblocktxn`: served, past MAX_BLOCKTXN_DEPTH (a `block`), unknown, and out of bounds.
#[test]
fn serve_getblocktxn() {
    replay("serve-getblocktxn", Args::default());
}

/// `-prune=1`: blocks deeper than NODE_NETWORK_LIMITED_MIN_BLOCKS + 2 disconnect.
#[test]
fn serve_limited() {
    replay(
        "serve-limited",
        Args {
            pruned: true,
            ..Args::default()
        },
    );
}
