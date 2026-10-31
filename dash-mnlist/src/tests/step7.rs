//! Steps 1–7 over a real header chain: SB4's `HeaderChain` (SPV mode) holding the capture's
//! headers is the engine's `MnChain`, and a ChainLock step 6 verified is applied by the host's
//! `apply_chainlock` (SB0 §5.2 step 7).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use dash_chain::{
    ChainConfig, ChainLockRef, ClOutcome, FailStop, HeaderBatch, HeaderChain, NoStore, Services,
    StaticSporks,
};
use dash_clock::MockClock;
use dash_consensus::primitives::Header;
use dash_params::ChainSetup;
use dash_primitives::BlockHeader;

use super::*;

#[derive(Default)]
struct Latch(AtomicBool);

impl FailStop for Latch {
    fn read_failed(&self, _: &str) {
        self.0.store(true, Ordering::SeqCst);
    }

    fn is_tripped(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// The capture's headers, genesis first.
fn raw_headers() -> Vec<BlockHeader> {
    fixture()["headers"].as_array().expect("headers").iter().map(decode).collect()
}

/// The services for a chain over `raw`, its clock just past their newest time.
pub(super) fn services(params: &ChainSetup, raw: &[BlockHeader]) -> Services {
    let now = raw.iter().map(|h| i64::from(h.time)).max().expect("headers") + 600;
    Services {
        clock: Arc::new(MockClock::at(now)),
        sporks: Arc::new(StaticSporks::for_network(params, Some(true))),
        fail: Arc::new(Latch::default()),
    }
}

/// An SPV header chain from genesis holding the capture's headers, its tip the capture's.
fn header_chain() -> HeaderChain {
    let params = config().params_arc().clone();
    let raw = raw_headers();
    let svc = services(&params, &raw);
    let cfg = ChainConfig::spv(&params, 10_000);
    let mut chain = HeaderChain::new_genesis(params, cfg, svc, NoStore).expect("a configuration");
    let headers: Vec<Header> = raw[1..].iter().map(|h| Header::new(*h)).collect();
    let batch = HeaderBatch {
        hashes: headers.iter().map(Header::hash).collect(),
        headers,
    };
    assert_eq!(chain.accept(batch, None).expect("Core's headers").stop, None);
    let tip = chain.best_header();
    chain.tip_plan(tip).expect("a plan");
    chain.set_active_tip(tip).expect("the tip");
    chain.records_committed();
    chain
}

/// `MnChain::anchor` (D25): 0 from genesis (anchored: `testnet`).
#[test]
fn a_genesis_header_chains_anchor_is_zero() {
    assert_eq!(MnChain::anchor(&header_chain()), 0);
}

#[test]
fn a_verified_chainlock_sets_the_header_chains_floor() {
    let mut chain = header_chain();
    let tip = MnChain::active_tip(&chain).expect("a tip");
    assert_eq!((tip.height, tip.hash), (311, FixtureChain::full().hash(311)));

    // Steps 1–5 over the header chain's reader.
    let mut engine = applied(&chain, FIRST, FIRST + 1..=tip.height);
    apply_cycles(&mut engine, &chain);
    for t in [CHAIN_LOCKS, ROTATED] {
        for q in core_quorums(t) {
            let state = state(&engine, t, q.hash);
            assert!(matches!(state, QuorumState::Verified(_)), "{t} {}: {state:?}", q.height);
        }
    }

    // Step 6, then step 7: the lock is the chain's floor.
    let (cl, _) = clsigs().into_iter().find(|(cl, _)| cl.height == 311).expect("the tip's lock");
    let verified = engine.verify_chainlock(&cl, &chain, config()).expect("Core's verdict");
    let lock = ChainLockRef {
        height: u32::try_from(verified.height).expect("a height"),
        hash: verified.block_hash,
    };
    let mut vt = chain.virtual_tip();
    let outcome = chain.apply_chainlock(lock, &mut vt).expect("applies");
    assert!(matches!(outcome, ClOutcome::Floor { id, .. } if id == tip.id), "{outcome:?}");
    assert_eq!(chain.cl_floor(), Some(tip.id));
}
