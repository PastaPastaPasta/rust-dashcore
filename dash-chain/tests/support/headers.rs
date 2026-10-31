//! H-M and H-T (`tests/data/headers/README.md`), read in `accept` batches.

use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

use dash_chain::{FailStop, HeaderBatch, HeadersResult, Services, StaticSporks};
use dash_clock::MockClock;
use dash_consensus::primitives::Header;
use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};
use dash_primitives::BlockHeader;
use dash_types::codec::BaseCodec as _;

/// A `headers` message's worth, as SB14's driver commits after each (SB4 §3.8's B).
pub const BATCH: usize = 2_000;

pub fn params(net: &str) -> Arc<ChainSetup> {
    let network = match net {
        "mainnet" => Network::Main,
        "testnet" => Network::Testnet3,
        _ => panic!("{net}: H-M or H-T"),
    };
    let built = ChainParamsBuilder::new(network, Release::V24_0).build();
    Arc::new(built.expect("Core's parameters"))
}

/// A latch that fails the test.
pub struct Panics;

impl FailStop for Panics {
    fn read_failed(&self, what: &str) {
        panic!("the latch tripped: {what}");
    }

    fn is_tripped(&self) -> bool {
        false
    }
}

/// The replays' services: the clock past every dumped header's time, so none is
/// `time-too-new`, and ChainLocks off.
pub fn services(p: &ChainSetup) -> Services {
    Services {
        clock: Arc::new(MockClock::at(2_000_000_000)),
        sporks: Arc::new(StaticSporks::for_network(p, Some(false))),
        fail: Arc::new(Panics),
    }
}

/// An empty `MemHeaderStore` reset to a genesis-origin chain's Candidate meta (SB0 §9.5).
#[cfg(all(feature = "test-util", not(loom)))]
pub fn genesis_store(p: &ChainSetup) -> dash_chain::MemHeaderStore {
    use dash_chain::{AnchorState, BlockId, CHAIN_SCHEMA, ChainMeta, ClPersisted};
    let store = dash_chain::MemHeaderStore::new();
    store
        .reset(ChainMeta {
            schema: CHAIN_SCHEMA,
            anchor: (Header::new(dash_consensus::params::genesis_block(p).header).hash(), 0),
            anchor_state: AnchorState::Candidate,
            next_id: BlockId(0),
            tip: BlockId(0),
            best_header: BlockId(0),
            sync_seq: 0,
            cl: ClPersisted {
                best: None,
                floor: None,
                enabled_observed: false,
            },
        })
        .expect("reset");
    store
}

/// Asserts the batch was taken whole.
pub fn accepted(r: &HeadersResult) {
    assert!(r.stop.is_none(), "{:?}", r.stop);
}

/// The directory holding `<net>.bin`: `SB4_HEADERS_DIR`, or `tests/data/headers`.
pub fn dir() -> PathBuf {
    match std::env::var_os("SB4_HEADERS_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/headers"),
    }
}

/// The dump's batches from genesis, hashed as they are read.
pub fn batches(net: &str) -> impl Iterator<Item = HeaderBatch> {
    let path = dir().join(format!("{net}.bin"));
    let mut file = File::open(&path).unwrap_or_else(|e| {
        panic!("{}: {e} (run tests/data/headers/fetch.sh, or set SB4_HEADERS_DIR)", path.display())
    });
    // Filled in place: A-31 counts allocations while this runs.
    let mut raw = vec![0u8; 80 * BATCH];
    std::iter::from_fn(move || {
        let mut n = 0;
        while n < raw.len() {
            match file.read(&mut raw[n..]).expect("read") {
                0 => break,
                k => n += k,
            }
        }
        assert_eq!(n % 80, 0, "{}: a partial header", path.display());
        (n > 0).then(|| batch(raw[..n].as_chunks::<80>().0.iter().map(|h| decode(h))))
    })
}

pub fn decode(raw: &[u8]) -> Header {
    Header::new(BlockHeader::decode(&mut &raw[..]).expect("an 80-byte header"))
}

pub fn batch(headers: impl Iterator<Item = Header>) -> HeaderBatch {
    let headers: Vec<Header> = headers.collect();
    HeaderBatch {
        hashes: headers.iter().map(Header::hash).collect(),
        headers,
    }
}
