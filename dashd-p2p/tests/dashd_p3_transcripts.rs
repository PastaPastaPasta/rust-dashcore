//! P3's transcript equality with dashd v24.0.0-rc.3 (P3 §6.3 items 1–5): the `blocks-*`
//! and `cmpct-*` fixtures under `tests/fixtures/dashd-v24/`
//! (`contrib/node/p3-transcripts/capture.py`; provenance in the directory's `PROVENANCE.md`)
//! replayed by [`dashd_p2p::sim::transcript`] against a node running P3's `BlockDownload`, so
//! dashd's block `getdata`s are compared, not dropped ([`Hooks::downloads_blocks`]).

use std::sync::Arc;

use dash_p2p_core::{CmpctBlock, ShortTxId};
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_primitives::BlockHeader;
use dash_types::codec::BaseCodec;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::download::BlockDownload;
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::sim::Sim;
use dashd_p2p::sim::transcript::{Hooks, unhex};
use dashd_p2p::{Registry, Seams};

/// The node downloads blocks, and `cmpctblock`s are generated from `crowded:HEADER:N:CROWD`.
#[derive(Default)]
struct Downloads;

impl Hooks for Downloads {
    fn downloads_blocks(&self) -> bool {
        true
    }

    fn generate(&mut self, cmd: &str, spec: &str) -> Option<Vec<u8>> {
        let f: Vec<&str> = spec.strip_prefix("crowded:")?.split(':').collect();
        assert_eq!(cmd, "cmpctblock");
        let header = BlockHeader::decode(&mut &unhex(f[0])[..]).expect("an 80-byte header");
        let (n, crowd, buckets) =
            (f[1].parse().unwrap(), f[2].parse().unwrap(), f[3].parse().unwrap());
        let msg = CmpctBlock {
            header,
            nonce: 0,
            short_ids: crowded(n, crowd, buckets).into_iter().map(ShortTxId::new).collect(),
            prefilled: Vec::new(),
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf);
        Some(buf)
    }
}

/// `n` distinct short IDs, the first `crowd` in bucket 1 of a libstdc++ map of `b` buckets, the
/// rest one per other bucket (the capture tool's `crowded`). `b` comes from the fixture, not from
/// [`bucket_count`](dashd_p2p::processing::compact::bucket_count), which the replay tests.
fn crowded(n: usize, crowd: usize, b: u64) -> Vec<u64> {
    let mut ids: Vec<u64> = (0..crowd as u64).map(|k| 1 + k * b).collect();
    ids.extend((0..b).filter(|&x| x != 1).take(n - crowd));
    ids
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

fn replay(name: &str, mut hooks: Downloads) {
    let path = format!("{}/tests/fixtures/dashd-v24/{name}.txt", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    dashd_p2p::sim::transcript::replay(name, &text, sim(), &mut hooks);
}

/// Item 1: one peer serves 1100 blocks; the `getdata` sizes (16, then one per block) and order.
#[test]
fn blocks_1100() {
    replay("blocks-1100", Downloads);
}

/// Item 2: two peers, peer 1 withholding block 1: the window fills, "Stall started", no
/// disconnect at +2 s, the disconnect at +3 s, and block 1 re-requested from peer 2.
#[test]
fn blocks_stall() {
    replay("blocks-stall", Downloads);
}

/// Item 3: a lone direct fetch from a peer that sent `sendcmpct(0, 1)` asks
/// `getdata(MSG_CMPCT_BLOCK)`, and the `cmpctblock` with the coinbase prefilled connects the
/// block. Then item 4 (S4): the valid block outside IBD, with nothing else in flight, makes its
/// source a high-bandwidth peer: `sendcmpct(1, 1)` (`MaybeSetPeerAsAnnouncingHeaderAndIDs`).
#[test]
fn cmpct_swap() {
    replay("cmpct-swap", Downloads);
}

/// Item 4's announcement half: a high-bandwidth peer that has the tip gets the RPC block's
/// `cmpctblock` from `NewPoWValidBlock` (nonce and short IDs masked), and nothing else.
#[test]
fn cmpct_announce() {
    replay("cmpct-announce", Downloads);
}

/// Item 5: for every n next to a change of libstdc++'s bucket count, 13 short IDs in one bucket
/// are `READ_STATUS_FAILED` (`getdata(MSG_BLOCK)`) and 12 are not (`getblocktxn`), each from a
/// fresh inbound peer.
#[test]
fn cmpct_buckets() {
    replay("cmpct-buckets", Downloads);
}
