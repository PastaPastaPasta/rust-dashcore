//! MN0-2b-1's dashd differential (CP OD-CP-33 C-1, D-2): block proposals past DIP0003 run on
//! Core's dummy index. Each proposal's bytes go to dashd v24.0.0's `getblocktemplate`
//! proposal mode (`rpc/mining.cpp:697-724`) and to V's (`ChainHandle::propose_block`), before the
//! block is submitted to both. dashd's answer (null, or the reject reason) is the verdict V must
//! give.
//!
//! Back to back: at each height, dashd's block, then mutations of it that dashd rejects (a CbTx
//! height or quorum root, a commitment dropped or its height changed, an MNHF signal), then the
//! block again. These are proposals of different blocks on the same parent, and after the
//! submit, on the next parent. The dummy's id is reused by every one, so a cache that kept an
//! answer for one would give it to the next: after each proposal V's versionbits cache, the one
//! keyed by block, holds nothing for the dummy, and on CH7a-4b's chain both nodes agree on the
//! tip and the deployments after each submit.
//!
//! The chains:
//! - CH7a-4b's recorded chain to 200 (DIP0003 at 48, DIP0024 moved out), whose mining windows
//!   carry null commitments;
//! - default regtest past DIP0003 (432) to 460, mined by a dashd for the test, where DIP0024
//!   rotates `llmq_test_dip0024` with cycle bases 432 and 456.
//!
//! Needs dashd v24.0.0 (see `v24`).

mod v24;

use std::sync::Arc;

use dash_chain::DUMMY_ID;
use dash_consensus::primitives::{Header, Transaction, core_serialize, decode_block};
use dash_consensus::tx::{TRANSACTION_MNHF_SIGNAL, TRANSACTION_QUORUM_COMMITMENT};
use dash_consensus::{Block, Tx};
use dash_primitives::TxType;
use dashd_chainstate::chain::BlockState;
use serde_json::{Value, json};

use v24::{Dashd, Harness, blocks, dashd_binary, hex};

/// dashd's CH7a-4b chain, blocks 1 to 200.
const CH7A4B: &str = include_str!("../../dashd-chainstate/tests/data/ch7a4b/blocks.txt");
/// The recordings' coinbase address.
const ADDRESS: &str = "ycwedq2f3sz2Yf9JqZsBCQPxp18WU3Hp4J";

/// `BIP22ValidationResult` of a state: null when valid, else the reject reason.
fn bip22(state: &BlockState) -> Value {
    match state {
        BlockState::Valid => Value::Null,
        BlockState::Invalid(e) => json!(e.reject_reason()),
        BlockState::Error(msg) => json!(msg),
    }
}

/// Both nodes' proposal answers on `block`'s bytes: dashd's, then V's.
fn propose_both(h: &Harness, block: &Block) -> [Value; 2] {
    let data = hex(&core_serialize(block));
    let core = h.dashd().call("getblocktemplate", json!([{"mode": "proposal", "data": data}]));
    // V decodes the bytes dashd got.
    let bytes = core_serialize(block);
    let decoded = Arc::new(decode_block(&mut &bytes[..]).expect("a block").into_value());
    let chain = h.sut.chain().expect("V runs");
    let ours = match chain.propose_block(decoded).recv().expect("V serves") {
        Ok(state) => bip22(&state),
        Err(not_tested) => json!(not_tested.to_string()),
    };
    // C-1: the dummy's id never reaches the one cache keyed by block (`m_versionbitscache`).
    let kept = chain
        .query(|q| q.deployments.versionbits_cache_holds(Some(DUMMY_ID)))
        .recv()
        .expect("V serves");
    assert!(!kept, "the versionbits cache holds a state after the dummy");
    [core, ours]
}

/// `block` with `txs`, its merkle root recomputed (proposal mode checks it, not the PoW).
fn with_txs(block: &Block, txs: Vec<Arc<Tx>>) -> Block {
    let mut raw = *block.header().raw();
    raw.set_merkle_root(Block::new(Header::new(raw), txs.clone()).merkle().0);
    Block::new(Header::new(raw), txs)
}

/// `block` with its transaction `n`'s payload edited by `edit`.
fn edit_payload(block: &Block, n: usize, edit: impl FnOnce(&mut Vec<u8>)) -> Block {
    let mut txs = block.txs().to_vec();
    let mut tx = txs[n].to_mutable();
    {
        let mut extra_payload = tx.extra_payload().clone();
        edit(&mut extra_payload);
        tx = dash_primitives::Transaction::new(
            tx.version(),
            tx.tx_type(),
            tx.inputs().clone(),
            tx.outputs().clone(),
            tx.lock_time(),
            extra_payload,
        );
    }
    txs[n] = Arc::new(Tx::new(tx));
    with_txs(block, txs)
}

/// Adds one to the little-endian `u32` at `at` (a CbTx's or a commitment's `nHeight`).
fn bump_u32(bytes: &mut [u8], at: usize) {
    let v = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    bytes[at..at + 4].copy_from_slice(&v.wrapping_add(1).to_le_bytes());
}

/// An MNHF signal transaction (`evo/mnhftx.h`: version 1, bit 10, an unknown quorum, a zero
/// signature), with no inputs or outputs, as Core's special transactions of this type are.
fn mnhf_signal() -> Arc<Tx> {
    let mut payload = vec![1, 10];
    payload.extend([0x11; 32]);
    payload.extend([0; 96]);
    Arc::new(Tx::new(Transaction::new(
        3,
        TxType::new(TRANSACTION_MNHF_SIGNAL),
        vec![],
        vec![],
        0,
        payload,
    )))
}

/// The mutations of `block` at `height` this test proposes, by name. Each must be one dashd
/// rejects.
fn mutations(block: &Block) -> Vec<(&'static str, Block)> {
    let mut out = vec![
        ("cbtx height", edit_payload(block, 0, |p| bump_u32(p, 2))),
        ("cbtx quorum root", edit_payload(block, 0, |p| p[38] ^= 1)),
    ];
    let qc = TxType::new(TRANSACTION_QUORUM_COMMITMENT);
    if let Some(n) = block.txs().iter().position(|tx| tx.tx_type() == qc) {
        let mut dropped = block.txs().to_vec();
        dropped.remove(n);
        out.push(("commitment dropped", with_txs(block, dropped)));
        out.push(("commitment height", edit_payload(block, n, |p| bump_u32(p, 2))));
    }
    let mut signalled = block.txs().to_vec();
    signalled.push(mnhf_signal());
    out.push(("mnhf signal", with_txs(block, signalled)));
    out
}

/// At `height`, before the submit: `block`, each mutation, then `block` again, on both nodes.
fn back_to_back(h: &Harness, block: &Block, height: usize) {
    let [core, ours] = propose_both(h, block);
    assert_eq!(core, Value::Null, "dashd's own block at {height}");
    assert_eq!(ours, core, "the proposal of {height}");
    for (name, mutated) in mutations(block) {
        let [core, ours] = propose_both(h, &mutated);
        assert_ne!(core, Value::Null, "dashd rejects the {name} at {height}");
        assert_eq!(ours, core, "the {name} at {height}");
    }
    let [core, ours] = propose_both(h, block);
    assert_eq!((&core, &ours), (&Value::Null, &Value::Null), "{height} again");
}

/// CH7a-4b's chain: below DIP0003 dashd's blocks only; from DIP0003's block 48 back to back.
#[test]
fn proposals_back_to_back_on_the_dip3_chain() {
    let Some(h) = Harness::start() else {
        return;
    };
    for (i, b) in blocks(CH7A4B).iter().enumerate() {
        let height = i + 1;
        if height >= 48 {
            back_to_back(&h, b, height);
        } else {
            let [core, ours] = propose_both(&h, b);
            assert_eq!((&core, &ours), (&Value::Null, &Value::Null), "{height}");
        }
        h.submit(b, height);
        // The proposals left both nodes' state as the submit found it: tips and deployments.
        h.assert_agree_on_tip(&b.hash(), height);
    }
}

/// The height default regtest's chain is mined to: past DIP0003 (432) and the next
/// `llmq_test_dip0024` cycle base (456).
const ROTATED_TO: usize = 460;

/// Default regtest (DIP0024 active from 1, DIP0003 at 432): a dashd mines the chain, and each
/// block is proposed on a fresh dashd and V before the submit, back to back from 432.
#[test]
fn proposals_back_to_back_through_rotated_cycle_bases() {
    let Some(binary) = dashd_binary() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let miner = Dashd::start_with(&binary, &dir.path().join("miner"), &[]);
    let hashes = miner.call("generatetoaddress", json!([ROTATED_TO, ADDRESS]));
    let mined: Vec<Arc<Block>> = hashes
        .as_array()
        .expect("the hashes")
        .iter()
        .map(|hash| {
            let text = miner.call("getblock", json!([hash, 0]));
            let text = text.as_str().expect("hex");
            let bytes: Vec<u8> = (0..text.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
                .collect();
            Arc::new(decode_block(&mut &bytes[..]).expect("a block").into_value())
        })
        .collect();
    drop(miner);
    let Some(h) = Harness::start_with(&[]) else {
        return;
    };
    for (i, b) in mined.iter().enumerate() {
        let height = i + 1;
        if height >= 432 {
            back_to_back(&h, b, height);
        } else {
            let [core, ours] = propose_both(&h, b);
            assert_eq!((&core, &ours), (&Value::Null, &Value::Null), "{height}");
        }
        h.submit(b, height);
    }
}
