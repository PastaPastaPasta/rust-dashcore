//! Shape checks outside the deep-window guard's measured region.
#![allow(dead_code)]

use std::sync::Arc;

use dash_consensus::primitives::Block;
use dash_params::LlmqType;
use dash_primitives::{SpecialPayload, TxType};
use dash_types::Numeric;

const CYCLE: usize = 24;
const M_OFFSET: usize = 11;

const TYPES: [LlmqType; 3] =
    [LlmqType::LlmqTest, LlmqType::LlmqTestInstantsend, LlmqType::LlmqTestPlatform];

fn commitment_counts(block: &Block) -> [usize; 3] {
    let mut seen = [0; 3];
    for tx in block.txs().iter().filter(|tx| tx.tx_type() == TxType::QuorumCommitment) {
        let decoded = tx.payload().expect("QC payload").expect("decoded QC");
        assert!(decoded.is_complete(), "complete QC");
        let SpecialPayload::QuorumCommitment(payload) = decoded.payload() else {
            panic!("QC");
        };
        let qc = payload.commitment();
        if !qc.is_null() {
            let t = TYPES.iter().position(|t| *t == qc.llmq_type()).expect("recorded QC type");
            seen[t] += 1;
        }
    }
    seen
}

/// Counts non-null commitments before any blocks are connected.
pub fn non_null_commitments(blocks: &[Arc<Block>]) -> usize {
    blocks.iter().map(|block| commitment_counts(block).iter().sum::<usize>()).sum()
}

/// Every measured block runs the quorum-root check; every mining cycle carries all three types.
pub fn validate_segment(blocks: &[Arc<Block>], first: usize) -> usize {
    let mut total = 0;
    for (i, block) in blocks.iter().enumerate() {
        let height = first + i;
        let coinbase = &block.txs()[0];
        assert_eq!(coinbase.tx_type(), TxType::CoinbaseCommitment, "CbTx at {height}");
        let decoded = coinbase.payload().expect("CbTx payload").expect("decoded CbTx");
        assert!(decoded.is_complete(), "complete CbTx at {height}");
        let SpecialPayload::CoinbaseCommitment(cbtx) = decoded.payload() else {
            panic!("CbTx at {height}");
        };
        assert!(cbtx.version().to_base() >= 2, "CbTx version at {height}");
        assert_eq!(cbtx.height().to_u32() as usize, height, "CbTx height");
        let seen = commitment_counts(block);
        total += seen.iter().sum::<usize>();
        assert_eq!(
            seen,
            if height % CYCLE == M_OFFSET {
                [1; 3]
            } else {
                [0; 3]
            },
            "non-null QCs at {height}"
        );
    }
    assert!(total > 0, "the measured segment must carry non-null commitments");
    total
}

/// The side branch starts after its parent on M and loses before the measured segment.
pub fn fork_parent(
    main: &[Arc<Block>],
    side: &[Arc<Block>],
    segment_start: usize,
    deep_flush: usize,
) -> usize {
    let parent = side.first().expect("side-branch blocks").header().prev_hash();
    let p = main.iter().position(|b| b.hash() == parent).expect("S's parent on M") + 1;
    assert!(p > deep_flush, "the fork must follow the deep flush");
    assert!(p + side.len() < segment_start, "the reorg must finish before the segment");
    for pair in side.windows(2) {
        assert_eq!(pair[1].header().prev_hash(), pair[0].hash(), "S linkage");
    }
    assert_ne!(side[0].hash(), main[p].hash(), "a distinct side branch");
    p
}
