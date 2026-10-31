use std::sync::Arc;

use dash_consensus::header::HeaderParams;
use dash_consensus::primitives::{BlockHash, BlockHeader, Transaction, Tx, TxType};
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_primitives::MerkleRoot;

use super::*;

const REGTEST_BITS: u32 = 0x207f_ffff;
/// A target no hash meets in practice: one in 2^232.
const HARD_BITS: u32 = 0x0300_0001;

fn pow() -> PowParams {
    let p = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
    *HeaderParams::from_chain_params(&p).unwrap().pow()
}

fn block(bits: u32, nonce: u32, time: u32) -> Block {
    let cb = Tx::new(Transaction {
        version: 1,
        tx_type: TxType::new(0),
        inputs: Vec::new(),
        outputs: Vec::new(),
        lock_time: time,
        extra_payload: Vec::new(),
    });
    let header = Header::new(BlockHeader {
        version: 0x2000_0000,
        prev_hash: BlockHash::from([3; 32]),
        time,
        bits,
        nonce,
        ..BlockHeader::default()
    });
    Block::new(header, vec![Arc::new(cb)])
}

fn meets(block: &Block) -> bool {
    check_proof_of_work(block.hash().as_bytes(), CompactTarget::new(block.header().bits()), &pow())
}

fn never() -> bool {
    false
}

/// A regtest block whose nonce 0 misses the target, and the first nonce that meets it.
fn needs_grinding() -> (Block, u32) {
    let p = pow();
    (0..)
        .find_map(|time| {
            let mut b = block(REGTEST_BITS, 0, time);
            let mut tries = u64::MAX;
            assert_eq!(grind(&mut b, &p, &mut tries, &never), Grind::Found);
            let nonce = b.header().nonce();
            (nonce > 0).then(|| (block(REGTEST_BITS, 0, time), nonce))
        })
        .unwrap()
}

#[test]
fn found_sets_the_merkle_root_and_spends_one_try_per_nonce_passed() {
    let (mut b, nonce) = needs_grinding();
    assert_eq!(b.header().merkle_root(), MerkleRoot::from([0; 32]));
    let mut tries = 1_000;
    assert_eq!(grind(&mut b, &pow(), &mut tries, &never), Grind::Found);
    assert_eq!(b.header().nonce(), nonce);
    assert_eq!(tries, 1_000 - u64::from(nonce));
    assert_eq!(b.header().merkle_root(), b.merkle().0);
    assert!(meets(&b));
}

/// `max_tries == 0` is checked before the proof: the nonce the last try reached meets the target
/// and still loses, as in Core.
#[test]
fn running_out_on_a_valid_nonce_is_out_of_tries() {
    let (mut b, nonce) = needs_grinding();
    let mut tries = u64::from(nonce);
    assert_eq!(grind(&mut b, &pow(), &mut tries, &never), Grind::OutOfTries);
    assert_eq!((tries, b.header().nonce()), (0, nonce));
    assert!(meets(&b));
}

#[test]
fn no_tries_is_out_of_tries_at_once() {
    let mut b = block(REGTEST_BITS, 0, 1);
    let mut tries = 0;
    assert_eq!(grind(&mut b, &pow(), &mut tries, &never), Grind::OutOfTries);
    assert_eq!(b.header().nonce(), 0);
    assert_eq!(b.header().merkle_root(), b.merkle().0);
}

#[test]
fn the_last_nonce_is_never_tried() {
    let mut b = block(HARD_BITS, u32::MAX - 1, 1);
    let mut tries = 10;
    assert_eq!(grind(&mut b, &pow(), &mut tries, &never), Grind::NonceExhausted);
    assert_eq!((tries, b.header().nonce()), (9, u32::MAX));

    // Even a block whose last nonce meets the target (regtest's limit, nearly every other hash).
    let mut b = block(REGTEST_BITS, u32::MAX, 1);
    let mut tries = 10;
    assert_eq!(grind(&mut b, &pow(), &mut tries, &never), Grind::NonceExhausted);
    assert_eq!(tries, 10);
}

#[test]
fn a_shutdown_stops_the_loop() {
    let mut b = block(HARD_BITS, 0, 1);
    let mut tries = 10;
    assert_eq!(grind(&mut b, &pow(), &mut tries, &|| true), Grind::Shutdown);
    assert_eq!((tries, b.header().nonce()), (10, 0));
}

#[test]
fn out_of_tries_wins_over_a_shutdown() {
    let mut b = block(HARD_BITS, 0, 1);
    let mut tries = 0;
    assert_eq!(grind(&mut b, &pow(), &mut tries, &|| true), Grind::OutOfTries);
}
