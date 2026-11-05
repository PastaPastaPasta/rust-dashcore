//! Fixtures for the crate's unit tests.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use dash_clock::MockClock;
use dash_consensus::primitives::Header;
use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};
use dash_primitives::{BlockHash, BlockHeader};

use crate::batch::HeaderBatch;
use crate::config::{Services, StaticSporks};
use crate::error::FailStop;

pub(crate) fn params(network: Network) -> Arc<ChainSetup> {
    Arc::new(ChainParamsBuilder::new(network, Release::V24_0).build().expect("Core's parameters"))
}

/// A latch that records whether it tripped.
#[derive(Default)]
pub(crate) struct Latch(AtomicBool);

impl FailStop for Latch {
    fn read_failed(&self, _what: &str) {
        self.0.store(true, Ordering::SeqCst);
    }

    fn is_tripped(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

pub(crate) fn services(params: &ChainSetup) -> Services {
    services_at(params, 1_700_000_000)
}

/// [`services`] on a mock clock at `now`.
pub(crate) fn services_at(params: &ChainSetup, now: i64) -> Services {
    services_with(params, Arc::new(MockClock::at(now)))
}

/// [`services`] on `clock`, for a test that moves it.
pub(crate) fn services_with(params: &ChainSetup, clock: Arc<MockClock>) -> Services {
    Services {
        clock,
        sporks: Arc::new(StaticSporks::for_network(params, None)),
        fail: Arc::new(Latch::default()),
    }
}

/// A unique hash standing in for X11 where a test needs no proof of work: `add_to_block_index`
/// takes the hash as given.
pub(crate) fn fake_hash(k: u64) -> BlockHash {
    let mut b = [0u8; 32];
    // Spread the bits so the index's ReadLE64 hash is not sequential.
    b[..8].copy_from_slice(&k.wrapping_mul(0x9e37_79b9_7f4a_7c15).to_le_bytes());
    b[8..16].copy_from_slice(&k.to_le_bytes());
    b[31] = 0x5b;
    BlockHash::from(b)
}

/// A header on `prev` with regtest's `nBits`.
pub(crate) fn header(prev: BlockHash, time: u32, nonce: u32) -> BlockHeader {
    BlockHeader {
        version: 0x2000_0000,
        prev_hash: prev,
        time,
        bits: 0x207f_ffff,
        nonce,
        ..BlockHeader::default()
    }
}

/// One batch of `headers`, each with its hash.
pub(crate) fn batch(headers: &[Header]) -> HeaderBatch {
    HeaderBatch {
        hashes: headers.iter().map(Header::hash).collect(),
        headers: headers.to_vec(),
    }
}
