//! BL-503's transcript equality with dashd v24.0.0-rc.5 (the plan's §4.3): the `fetch-*`
//! fixtures under `tests/fixtures/dashd-v24/` (`contrib/node/bl503-transcripts/capture.py`;
//! provenance in the directory's `PROVENANCE.md`) replayed by [`dashd_p2p::sim::transcript`]
//! against a node running P3's `BlockDownload`. Each `getblockfrompeer` record runs Core's
//! `FetchBlock` through the processor's queue and compares the answer with dashd's, and each
//! `inflight` record compares `getpeerinfo`'s heights in flight.

use std::sync::Arc;

use dash_params::{ChainParamsBuilder, Network, Release};
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::download::BlockDownload;
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::sim::Sim;
use dashd_p2p::sim::transcript::Hooks;
use dashd_p2p::{Registry, Seams};

/// The node downloads blocks, so dashd's block `getdata`s are compared.
struct Downloads;

impl Hooks for Downloads {
    fn downloads_blocks(&self) -> bool {
        true
    }
}

/// A node with P3's `BlockDownload` and dashd's defaults where `sim` departs from them:
/// `-peertimeout` is 60 s.
fn sim() -> Sim {
    let p = Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let id = local_identity(&p, &IdentityOptions::new()).unwrap();
    let seams = Seams {
        block_sync: Box::new(BlockDownload::new()),
        ..Seams::default()
    };
    Sim::with(p, id, ProcessorOptions::default(), Registry::new(), seams, |cfg| {
        cfg.peer_timeout = 60;
    })
}

fn replay(name: &str) {
    let path = format!("{}/tests/fixtures/dashd-v24/{name}.txt", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    dashd_p2p::sim::transcript::replay(name, &text, sim(), &mut Downloads);
}

/// A fork's tip fetched from the peer that announced it: `{}`, one `getdata(MSG_BLOCK)`; the
/// block arrives and nothing is left in flight.
#[test]
fn fetch_fork() {
    replay("fetch-fork");
}

/// The same peer twice (two `getdata`s), then another peer, which takes the only request.
#[test]
fn fetch_twice() {
    replay("fetch-twice");
}

/// A peer before its `version`: "Peer not fully connected", the request kept, and the block
/// download timeout once it is connected.
#[test]
fn fetch_unconnected() {
    replay("fetch-unconnected");
}

/// "Peer does not exist" for −1, the next id and a finalized peer, before the remove-all.
#[test]
fn fetch_nopeer() {
    replay("fetch-nopeer");
}

/// `notfound` clears no block request; the block download timeout disconnects the peer.
#[test]
fn fetch_timeout() {
    replay("fetch-timeout");
}
