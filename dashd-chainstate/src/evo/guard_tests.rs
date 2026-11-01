use std::sync::Arc;

use dash_consensus::Tx;
use dash_consensus::deployments::{Deployments, NoEhfSignals, VersionBitsCache};
use dash_consensus::primitives::canonical::{SPECIAL_VERSION, TRANSACTION_COINBASE};
use dash_consensus::primitives::{Transaction, TxType, write_commitment};
use dash_dml::StoreError;
use dash_dml::blockprocessor::pool::null_commitment;
use dash_dml::testkit::MemChain;
use dash_evo::BlsCtx;
use dash_evo::llmq::{CompleteChain, PrecomputeMode, QuorumPrecompute};
use dash_params::LlmqType;
use dash_params::{ChainOption, ChainParamsBuilder, ChainSetup, Network, Release};
use dash_primitives::{QuorumHash, QuorumVvecHash};

use super::*;
use crate::evo::not_built::NoMembers;

/// DIP0003 on the regtest chains below (`-dip3params`), so a short chain crosses it.
pub(crate) const DIP3: u32 = 30;

/// Regtest with DIP0003 at [`DIP3`] and the `-testactivationheight` settings `moved`.
pub(crate) fn regtest(moved: &[&str]) -> ChainSetup {
    let mut b = ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
        .arg(ChainOption::Dip3Params, format!("{DIP3}:{DIP3}"));
    for m in moved {
        b = b.arg(ChainOption::TestActivationHeight, *m);
    }
    b.build().expect("regtest")
}

/// Runs `f` over `params`' deployments on a chain `0 ..= tip`, where `BlockId(h)` is height `h`.
fn with_ctx<R>(params: &ChainSetup, tip: u32, f: impl FnOnce(&LlmqCtx<'_, StoreError>) -> R) -> R {
    let mut chain = MemChain::new();
    chain.extend(chain.genesis(), tip);
    let cache = VersionBitsCache::new();
    let deployments = Deployments::new(params, &chain, &NoEhfSignals, &cache);
    let complete = CompleteChain::<StoreError>::new(&chain);
    let llmq = LlmqCtx::new(params, &complete, &deployments, BlsCtx::BASIC).expect("in domain");
    f(&llmq)
}

/// G1q for the block at `height` (on a straight chain, parent `height - 1`).
fn arm(llmq: &LlmqCtx<'_, StoreError>, txs: &[Arc<Tx>], height: u32) -> Option<G1qArm> {
    g1q(txs, llmq, BlockId(height - 1), height).expect("no chain fault")
}

pub(crate) fn special(tx_type: u16, extra_payload: Vec<u8>) -> Arc<Tx> {
    Arc::new(Tx::new(Transaction {
        version: SPECIAL_VERSION,
        tx_type: TxType::new(tx_type),
        inputs: vec![],
        outputs: vec![],
        lock_time: 0,
        extra_payload,
    }))
}

/// A quorum-commitment transaction for `llmq_test` at `height`: null, or with a non-null
/// verification-vector hash.
pub(crate) fn qc(params: &ChainSetup, height: u32, null: bool) -> Arc<Tx> {
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

/// An MNHF signal: version, bit, quorum hash, signature (`CMNHFTx`).
pub(crate) fn mnhf() -> Arc<Tx> {
    let mut p = vec![1, 10];
    p.extend([7; 32]);
    p.extend([0; 96]);
    special(TRANSACTION_MNHF_SIGNAL, p)
}

/// An asset unlock: version, index, fee, requested height, quorum hash, signature.
pub(crate) fn asset_unlock() -> Arc<Tx> {
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
pub(crate) fn trailing(tx: &Tx) -> Arc<Tx> {
    let mut raw = tx.to_mutable();
    raw.extra_payload.push(0);
    Arc::new(Tx::new(raw))
}

/// `tx` with its payload cut short: the decode fails.
pub(crate) fn truncated(tx: &Tx) -> Arc<Tx> {
    let mut raw = tx.to_mutable();
    raw.extra_payload.truncate(3);
    Arc::new(Tx::new(raw))
}

pub(crate) fn plain() -> Arc<Tx> {
    Arc::new(Tx::new(Transaction {
        version: 1,
        tx_type: TxType::new(0),
        inputs: vec![],
        outputs: vec![],
        lock_time: 0,
        extra_payload: vec![],
    }))
}

/// A height above DIP3 that is no cycle base of a rotated type.
const H: u32 = DIP3 + 3;

#[test]
fn the_fixtures_decode_completely() {
    let params = regtest(&[]);
    for tx in [qc(&params, H, true), qc(&params, H, false), mnhf(), asset_unlock()] {
        let decoded = tx.payload().expect("special").expect("decodes");
        assert!(decoded.is_complete(), "{:?}", decoded.payload);
    }
    for tx in [trailing(&mnhf()), trailing(&qc(&params, H, true))] {
        assert!(!tx.payload().expect("special").expect("decodes").is_complete());
    }
    assert!(truncated(&qc(&params, H, false)).payload().expect("special").is_err());
}

/// D3 (a), (b): a non-null commitment, an MNHF signal or an asset unlock in a well-formed block
/// holds its arm; a block of null commitments only holds none.
#[test]
fn each_quorum_dependent_transaction_holds_its_arm() {
    let params = regtest(&[]);
    with_ctx(&params, H, |llmq| {
        assert_eq!(arm(llmq, &[plain()], H), None);
        assert_eq!(arm(llmq, &[plain(), qc(&params, H, true), qc(&params, H, true)], H), None);
        assert_eq!(
            arm(llmq, &[plain(), qc(&params, H, true), qc(&params, H, false)], H),
            Some(G1qArm::Commitment)
        );
        assert_eq!(arm(llmq, &[plain(), mnhf()], H), Some(G1qArm::QuorumSigned));
        assert_eq!(arm(llmq, &[plain(), asset_unlock()], H), Some(G1qArm::QuorumSigned));
        // The first arm in block order is reported.
        assert_eq!(
            arm(llmq, &[plain(), mnhf(), qc(&params, H, false)], H),
            Some(G1qArm::QuorumSigned)
        );
    });
}

/// D3 decode first: one special payload that does not decode completely, anywhere in the block,
/// bypasses every arm, so H4 returns Core's reject.
#[test]
fn a_payload_that_does_not_decode_bypasses_every_arm() {
    let params = regtest(&[]);
    with_ctx(&params, 2 * 24, |llmq| {
        let non_null = qc(&params, H, false);
        let bad_qc = truncated(&non_null);
        let bad_cbtx = special(TRANSACTION_COINBASE, vec![1]);
        let bad_protx = special(1, vec![1, 0]);
        for bad in [bad_qc, trailing(&non_null), trailing(&mnhf()), bad_cbtx, bad_protx] {
            assert_eq!(arm(llmq, &[plain(), bad.clone()], H), None);
            assert_eq!(arm(llmq, &[plain(), bad.clone(), non_null.clone()], H), None);
            assert_eq!(arm(llmq, &[plain(), non_null.clone(), bad.clone()], H), None);
            assert_eq!(arm(llmq, &[plain(), asset_unlock(), bad.clone()], H), None);
            // Arm (c) too: 48 is a rotation cycle base.
            assert_eq!(arm(llmq, &[plain(), bad.clone()], 48), None);
        }
    });
}

/// D3: below DIP0003 G1q holds nowhere, a cycle base or a non-null commitment included.
#[test]
fn below_dip3_no_arm_holds() {
    let params = regtest(&[]);
    with_ctx(&params, DIP3, |llmq| {
        assert_eq!(arm(llmq, &[plain(), qc(&params, 5, false), mnhf()], 5), None);
        assert_eq!(arm(llmq, &[plain()], 24), None);
        assert_eq!(arm(llmq, &[plain(), qc(&params, DIP3, false)], DIP3), Some(G1qArm::Commitment));
    });
}

/// D3 (c) is `NoMembers::precompute`'s refusal, from DIP0003 on, read from the parent alone:
/// on default regtest (DIP0024 from height 1) every 24th block; with DIP0024 moved to 61 the
/// first is 72, whose parent is past it. On devnet the rotating `llmq_devnet_dip0024` runs every
/// 48 blocks beside non-rotating types every 24, so only its bases hold (Opus r1 n-2).
#[test]
fn arm_c_is_the_precompute_refusal_from_dip3_on() {
    let devnet = ChainParamsBuilder::new(Network::Devnet, Release::V24_0)
        .devnet_name("g1q")
        .build()
        .expect("devnet");
    let cases = [
        ("regtest", regtest(&[]), DIP3, 48, 24),
        ("dip0024@61", regtest(&["dip0024@61"]), DIP3, 72, 24),
        ("devnet", devnet, 2, 48, 48),
    ];
    for (name, params, dip3, first, interval) in cases {
        with_ctx(&params, 150, |llmq| {
            let stub = NoMembers {
                llmq: *llmq,
            };
            let mut bases = Vec::new();
            for h in dip3..=150 {
                let refused =
                    stub.precompute(BlockId(h), PrecomputeMode::Connect, &mut Vec::new()).is_err();
                let holds = arm(llmq, &[plain()], h) == Some(G1qArm::CycleBase);
                assert_eq!(holds, refused, "{name} {h}");
                if holds {
                    bases.push(h);
                }
            }
            assert_eq!(bases.first(), Some(&first), "{name}");
            assert!(bases.iter().all(|h| h % interval == 0), "{name} {bases:?}");
        });
    }
}

type Hooks = crate::evo::node::NodeEvo<dashd_store::MemoryEngine, crate::evo::ev1::Ev1NotBuilt>;

/// D3 at startup: `NodeEvo` refuses a stored block where G1q holds, reading its data when held
/// (arms (a), (b)) and its height and parent alone when not (arm (c)); at genesis, below DIP0003
/// and on a block of null commitments it refuses nothing.
#[test]
fn node_evo_refuses_a_stored_block_where_g1q_holds() {
    use crate::connect::{BlockAt, EvoHooks};
    use dash_consensus::Block;
    use dash_consensus::primitives::{BlockHeader, Header};

    let params = regtest(&[]);
    let mut chain = MemChain::new();
    chain.extend(chain.genesis(), 60);
    let cache = VersionBitsCache::new();
    let deployments = Deployments::new(&params, &chain, &NoEhfSignals, &cache);
    let at = |h: u32| BlockAt {
        hash: chain.hash(BlockId(h)),
        height: h,
        prev: h.checked_sub(1).map(BlockId),
        id: Some(BlockId(h)),
    };
    let block = |txs: Vec<Arc<Tx>>| Block::new(Header::new(BlockHeader::default()), txs);
    let refusal = |h, b: Option<&Block>| {
        <Hooks as EvoHooks>::startup_refusal(&params, &chain, &deployments, &at(h), b)
            .expect("no fault")
    };
    let non_null = block(vec![plain(), qc(&params, H, false)]);
    assert_eq!(refusal(H, Some(&non_null)), Some(QUORUM_VALIDATION));
    assert_eq!(refusal(H, Some(&block(vec![plain(), mnhf()]))), Some(QUORUM_VALIDATION));
    assert_eq!(refusal(H, Some(&block(vec![plain(), qc(&params, H, true)]))), None);
    assert_eq!(refusal(H, None), None);
    assert_eq!(refusal(48, None), Some(QUORUM_VALIDATION));
    assert_eq!(refusal(48, Some(&block(vec![plain()]))), Some(QUORUM_VALIDATION));
    assert_eq!(refusal(5, Some(&non_null)), None);
    assert_eq!(refusal(0, None), None);
}
