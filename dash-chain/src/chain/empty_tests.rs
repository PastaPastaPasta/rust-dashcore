//! `new_empty`: a node chain with no entry, whose first entry is the genesis header
//! (`node/chainstate.cpp:210`, `validation.cpp:4617`, `:4712`).

use dash_consensus::pow::check_proof_of_work;
use dash_params::Network;

use super::*;
use crate::testing::{self, params, services};

fn empty_node() -> HeaderChain {
    let p = params(Network::Regtest);
    let svc = services(&p);
    HeaderChain::new_empty(p.clone(), ChainConfig::node(&p), svc, NoStore).expect("node mode")
}

fn genesis_header(chain: &HeaderChain) -> Header {
    Header::new(dash_consensus::params::genesis_block(&chain.params).header())
}

/// A mined regtest header on `prev`.
fn mined(chain: &HeaderChain, prev: BlockHash, time: u32) -> Header {
    let mut raw = testing::header(prev, time, 0);
    loop {
        let h = Header::new(raw);
        if check_proof_of_work(h.hash().as_bytes(), CompactTarget::new(raw.bits()), chain.hp.pow())
        {
            return h;
        }
        raw.set_nonce(raw.nonce() + 1);
    }
}

/// The empty chain publishes a snapshot with no tip and placeholder facts, and its readers and
/// `CheckBlockIndex` need no entry; none of them reads the root (`genesis()` asserts there is
/// one).
#[test]
fn an_empty_node_chain_serves_readers_with_no_entry() {
    let chain = empty_node();
    let snap = chain.snapshot();
    assert!(!snap.has_tip());
    assert_eq!(snap.tip_seq, 0);
    assert_eq!((snap.tip.tip, snap.tip.best_header), (crate::DUMMY_ID, crate::DUMMY_ID));
    assert_eq!((snap.tip.tip_height, snap.tip.best_header_height), (0, 0));
    assert_eq!(snap.tip.best_header_work, Work::ZERO);
    assert_eq!(chain.reader().snapshot().epoch, snap.epoch);
    assert_eq!(chain.reader().lookup(&genesis_header(&chain).hash()), None);
    assert!(chain.private_tip().is_none());
    assert_eq!(chain.check_index(), Ok(()), "Core's rule with no active chain");
}

#[test]
fn new_empty_is_node_only() {
    let p = params(Network::Regtest);
    let svc = services(&p);
    let spv =
        HeaderChain::<NoStore>::new_empty(p.clone(), ChainConfig::spv(&p, 10_000), svc, NoStore);
    assert!(matches!(spv, Err(ChainError::Config(ConfigError::WrongMode("new_empty")))));
}

/// The genesis header is the first root: `Accepted`, the best header, and, once published, the
/// snapshot's best header, still with no tip until the first `connect_tip`. A second accept
/// returns the same entry.
#[test]
fn the_genesis_header_is_the_first_root_of_an_empty_node_chain() {
    let mut chain = empty_node();
    let genesis = genesis_header(&chain);
    let one = chain.accept_header(&genesis).expect("accept_header").one;
    let AcceptOne::Accepted(id) = one else {
        panic!("{one:?}")
    };
    assert_eq!((id, chain.root, chain.next_id), (BlockId(0), id, 1));
    assert_eq!(chain.best_header(), id);
    let e = chain.reader().entry(id).expect("resident");
    assert_eq!((e.height, e.prev, e.status.bits()), (0, None, Status::VALID_TREE));
    chain.publish_status();
    let snap = chain.snapshot();
    assert!(!snap.has_tip());
    assert_eq!((snap.tip.tip, snap.tip.best_header), (id, id));
    assert_eq!(snap.tip.best_header_hash, genesis.hash());
    assert_eq!(chain.check_index(), Ok(()));

    assert_eq!(chain.accept_header(&genesis).expect("again").one, AcceptOne::Accepted(id));
    assert_eq!(chain.next_id, 1, "no second root");

    chain.connect_tip(id).expect("the first ABC connects genesis");
    let snap = chain.snapshot();
    assert!(snap.has_tip());
    assert_eq!((snap.tip.tip, snap.tip.best_header, snap.tip_seq), (id, id, 1));

    let child = mined(&chain, genesis.hash(), genesis.raw().time() + 150);
    let one = chain.accept_header(&child).expect("accept_header").one;
    assert!(matches!(one, AcceptOne::Accepted(BlockId(1))), "{one:?}");
}

/// A header that is not genesis finds no parent on the empty chain.
#[test]
fn a_child_before_genesis_has_no_parent_on_an_empty_chain() {
    let mut chain = empty_node();
    let child = mined(&chain, genesis_header(&chain).hash(), 1_417_713_500);
    let one = chain.accept_header(&child).expect("accept_header").one;
    assert!(
        matches!(&one, AcceptOne::Invalid(e) if e.reject_reason() == "prev-blk-not-found"),
        "{one:?}"
    );
    assert_eq!(chain.next_id, 0);
}

/// Only an empty node chain takes genesis as a root: in SPV, or with an entry already held,
/// a genesis header it does not hold stays `BelowAnchor`.
#[test]
fn genesis_is_below_the_anchor_unless_the_node_chain_is_empty() {
    let p = params(Network::Regtest);
    let hp = HeaderParams::from_chain_params(&p).expect("regtest");
    let spv_cfg = ChainConfig::spv(&p, 10_000);
    let mut spv = HeaderChain::<NoStore>::empty(p.clone(), spv_cfg, services(&p), NoStore, hp, 0);
    let genesis = genesis_header(&spv);
    assert_eq!(spv.accept_header(&genesis).expect("SPV").one, AcceptOne::BelowAnchor);
    assert_eq!(spv.next_id, 0);

    let mut node = empty_node();
    let other = mined(&node, BlockHash::from([3; 32]), 1_417_713_500);
    node.add_new(other.raw(), &other.hash(), None, Status(Status::VALID_TREE)).expect("a root");
    assert_eq!(node.accept_header(&genesis).expect("node").one, AcceptOne::BelowAnchor);
    assert_eq!(node.next_id, 1);
}
