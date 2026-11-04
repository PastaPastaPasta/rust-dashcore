//! Special transactions for H4's rows on regtest with DIP0003 at [`DIP3`]: well-formed ones of
//! the types that read quorums, and the malformed shapes of their payloads.

use std::sync::Arc;

use dash_consensus::Tx;
use dash_consensus::primitives::canonical::{SPECIAL_VERSION, TRANSACTION_ASSET_UNLOCK};
use dash_consensus::primitives::{Transaction, TxType, write_commitment};
use dash_consensus::tx::{TRANSACTION_MNHF_SIGNAL, TRANSACTION_QUORUM_COMMITMENT};
use dash_dml::blockprocessor::pool::null_commitment;
use dash_params::LlmqType;
use dash_params::{ChainOption, ChainParamsBuilder, ChainSetup, Network, Release};
use dash_primitives::{QuorumHash, QuorumVvecHash};

/// DIP0003 on the regtest chains below (`-dip3params`), so a short chain crosses it.
pub(super) const DIP3: u32 = 30;

/// Regtest with DIP0003 at [`DIP3`] and the `-testactivationheight` settings `moved`.
pub(super) fn regtest(moved: &[&str]) -> ChainSetup {
    let mut b = ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
        .arg(ChainOption::Dip3Params, format!("{DIP3}:{DIP3}"));
    for m in moved {
        b = b.arg(ChainOption::TestActivationHeight, *m);
    }
    b.build().expect("regtest")
}

pub(super) fn special(tx_type: u16, extra_payload: Vec<u8>) -> Arc<Tx> {
    Arc::new(Tx::new(Transaction {
        version: SPECIAL_VERSION,
        tx_type: TxType::new(tx_type),
        inputs: vec![],
        outputs: vec![],
        lock_time: 0,
        extra_payload,
    }))
}

/// A quorum-commitment transaction for `llmq_test` at `height`, for the quorum `[7; 32]`, which
/// names no block: null, or with a non-null verification-vector hash.
pub(super) fn qc(params: &ChainSetup, height: u32, null: bool) -> Arc<Tx> {
    let llmq = params.consensus.llmq(LlmqType::LlmqTest).expect("llmq_test");
    let mut c = null_commitment(LlmqType::LlmqTest, llmq, QuorumHash::from([7; 32]), 1, 0);
    if !null {
        c.quorum_vvec_hash = QuorumVvecHash::from([1; 32]);
    }
    let mut p = Vec::new();
    p.extend(1u16.to_le_bytes());
    p.extend(height.to_le_bytes());
    write_commitment(&c, &mut p);
    special(TRANSACTION_QUORUM_COMMITMENT, p)
}

/// An MNHF signal: version, bit, quorum hash `[7; 32]`, signature (`CMNHFTx`).
pub(super) fn mnhf() -> Arc<Tx> {
    let mut p = vec![1, 10];
    p.extend([7; 32]);
    p.extend([0; 96]);
    special(TRANSACTION_MNHF_SIGNAL, p)
}

/// An asset unlock: version, index, fee, requested height, quorum hash `[7; 32]`, signature.
pub(super) fn asset_unlock() -> Arc<Tx> {
    let mut p = vec![1];
    p.extend(0u64.to_le_bytes());
    p.extend(0u32.to_le_bytes());
    p.extend(DIP3.to_le_bytes());
    p.extend([7; 32]);
    p.extend([0; 96]);
    special(TRANSACTION_ASSET_UNLOCK, p)
}

/// `tx` with one more payload byte: the decode is not complete (`GetTxPayload`'s "stream
/// empty").
pub(super) fn trailing(tx: &Tx) -> Arc<Tx> {
    let mut raw = tx.to_mutable();
    raw.extra_payload.push(0);
    Arc::new(Tx::new(raw))
}

/// `tx` with its payload cut short: the decode fails.
pub(super) fn truncated(tx: &Tx) -> Arc<Tx> {
    let mut raw = tx.to_mutable();
    raw.extra_payload.truncate(3);
    Arc::new(Tx::new(raw))
}

#[test]
fn the_fixtures_decode_completely() {
    let params = regtest(&[]);
    let h = DIP3 + 3;
    for tx in [qc(&params, h, true), qc(&params, h, false), mnhf(), asset_unlock()] {
        let decoded = tx.payload().expect("special").expect("decodes");
        assert!(decoded.is_complete(), "{:?}", decoded.payload);
    }
    for tx in [trailing(&mnhf()), trailing(&qc(&params, h, true))] {
        assert!(!tx.payload().expect("special").expect("decodes").is_complete());
    }
    assert!(truncated(&qc(&params, h, false)).payload().expect("special").is_err());
}
