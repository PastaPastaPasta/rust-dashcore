//! `evo-index-depth` (CH7 §7.1, D22): the evo layer's reads as its window grows. A growth guard,
//! not a Core-parity PF0 row.
//!
//! `evo_index_depth <entries> <reads>` commits blocks through [`RocksEvoStore::commit`] until the
//! active [`EvoIndex`]'s `mined` map holds `entries` entries, every eighth block undone so that its
//! two rows are tombstones, then runs `reads` rounds of three reads over a fixed, seeded read set:
//! an `llmq_mined` point read, a by-hash point read (the block's credit data) and an `llmq_mined_h`
//! range read of up to [`RANGE`] entries from a mining height down, as
//! `GetMinedAndActiveCommitmentsUntilBlock` reads (`bp:731-773`). The setup depends on `entries`
//! alone, so `instructions:u` of a run minus the same run with 0 reads is the reads' cost;
//! `contrib/node/evo-index-depth.sh` takes the median of 3 fresh processes at 1,000 and 100,000
//! entries and passes when the larger costs at most 2 × the smaller (O(log n) predicts about
//! 1.7 ×; a walk over the window would be about 100 ×).
//!
//! `cargo test` runs this target with no arguments, which does nothing.

// PROCESS.md: benches measure production only, so a test feature turned on here (by a self
// dev-dependency or `--all-features`) fails the build. Clippy's `--all-features` run is exempt.
#[cfg(all(not(clippy), any(feature = "crash", feature = "planted-bugs")))]
compile_error!("benches measure production only: build them without `crash` and `planted-bugs`");

use std::hint::black_box;
use std::sync::Arc;

use dash_dml::EvoDelta;
use dash_dml::codec::ContextFreeNetInfo;
use dash_dml::creditpool::CreditBlockData;
use dash_dml::store::{MinedCommitmentRecord, MinedErase};
use dash_evo::llmq::LlmqCommitment;
use dash_primitives::{BlockHash, LlmqType, QuorumHash};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use dashd_chainstate::evo::codec::{index_key, index_prefix, mined_key};
use dashd_chainstate::evo::{EvoCf, EvoCodec, EvoIndex, EvoLayer, Lookup, RocksEvoStore};
use dashd_chainstate::store::{CHAINSTATE_SCHEMA, GlobalFailStop, GuardedEngine, RecordingSink};
use dashd_store::MemoryEngine;

/// The non-rotated types each block mines one commitment of: four `llmq_mined` and four
/// `llmq_mined_h` entries a block.
const TYPES: [LlmqType; 4] =
    [LlmqType::Llmq50_60, LlmqType::Llmq400_60, LlmqType::Llmq400_85, LlmqType::Llmq100_67];

/// The most entries a range read takes.
const RANGE: usize = 8;

/// Every `UNDO`-th block is disconnected after it connects, leaving tombstones.
const UNDO: u32 = 8;

/// A block hash unique to `height` (and `salt`, for the quorum bases).
fn hash(height: u32, salt: u8) -> BlockHash {
    let mut bytes = [salt; 32];
    bytes[..4].copy_from_slice(&height.to_le_bytes());
    BlockHash::from(bytes)
}

/// A null-bodied commitment of `llmq_type` for `quorum_hash` in Core's version-1 layout.
fn commitment(llmq_type: LlmqType, quorum_hash: QuorumHash) -> LlmqCommitment {
    let mut bytes = 1u16.to_le_bytes().to_vec();
    bytes.push(llmq_type.to_base());
    bytes.extend_from_slice(quorum_hash.as_bytes());
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&[0; 48 + 32 + 96 + 96]);
    LlmqCommitment::decode(&mut &bytes[..]).expect("a well-formed commitment")
}

fn record(llmq_type: LlmqType, height: u32) -> MinedCommitmentRecord {
    let quorum_hash = QuorumHash::from(*hash(height, 0xb0).as_bytes());
    MinedCommitmentRecord {
        llmq_type,
        quorum_hash,
        base_height: height - 1,
        rotation_index: None,
        commitment: Arc::new(commitment(llmq_type, quorum_hash)),
    }
}

fn delta(height: u32) -> EvoDelta {
    EvoDelta {
        block: hash(height, 0),
        height,
        dml: None,
        mined_commitments: Vec::new(),
        credit_data: None,
        credit_pool: None,
        mnhf_signals: None,
        erased_mined: Vec::new(),
        evo_tip: Some(hash(height, 0)),
    }
}

/// The active index after blocks 1.. until it holds `entries` mined entries; the last height.
fn build(entries: usize) -> (EvoIndex, u32) {
    let db = Arc::new(GuardedEngine::new(
        MemoryEngine::new(&CHAINSTATE_SCHEMA),
        GlobalFailStop::new(),
        Arc::new(RecordingSink::default()),
    ));
    let store = RocksEvoStore::new(db, EvoCodec::new(Arc::new(ContextFreeNetInfo)), hash(0, 0));
    let mut layer = EvoLayer::new();
    let blocks = entries.div_ceil(2 * TYPES.len()) as u32;
    for height in 1..=blocks {
        let mut d = delta(height);
        d.mined_commitments = TYPES.iter().map(|&t| record(t, height)).collect();
        d.credit_data = Some(CreditBlockData {
            balance: i64::from(height),
            unlocked: 0,
        });
        store.commit(&mut layer, &d).expect("a connect");
        if height.is_multiple_of(UNDO) {
            let mut undo = delta(height);
            undo.erased_mined = d
                .mined_commitments
                .iter()
                .map(|r| MinedErase {
                    llmq_type: r.llmq_type,
                    quorum_hash: r.quorum_hash,
                    mined_height: height,
                    rotation_index: None,
                })
                .collect();
            undo.evo_tip = Some(hash(height - 1, 0));
            store.commit(&mut layer, &undo).expect("a disconnect");
        }
    }
    (layer.indexes().0, blocks)
}

/// xorshift64*: the fixed read set.
fn next(state: &mut u64) -> u64 {
    *state ^= *state >> 12;
    *state ^= *state << 25;
    *state ^= *state >> 27;
    state.wrapping_mul(0x2545_f491_4f6c_dd1d)
}

/// `reads` rounds of the three reads; returns what they found, so nothing is optimised away.
fn reads(index: &EvoIndex, blocks: u32, reads: u64) -> u64 {
    let mut state = 0x9e37_79b9_7f4a_7c15;
    let mut found = 0;
    for _ in 0..reads {
        let height = 1 + (next(&mut state) % u64::from(blocks)) as u32;
        let llmq_type = TYPES[(next(&mut state) % TYPES.len() as u64) as usize];
        let key = mined_key(llmq_type, &QuorumHash::from(*hash(height, 0xb0).as_bytes()));
        found += u64::from(!matches!(black_box(index.mined(&key)), Lookup::Absent));
        found += u64::from(black_box(index.get(EvoCf::CreditData, &hash(height, 0))).is_some());
        let prefix = index_prefix(llmq_type, None);
        let start = index_key(llmq_type, None, height);
        found += index
            .mined_from(&prefix, &start)
            .take(RANGE)
            .map(|e| black_box(e).1.is_some() as u64)
            .sum::<u64>();
    }
    found
}

fn main() {
    let args: Vec<u64> = std::env::args().skip(1).map(|a| a.parse().expect("a number")).collect();
    let [entries, n] = args[..] else {
        return;
    };
    let (index, blocks) = build(entries as usize);
    let found = reads(&index, blocks, n);
    println!("entries {} blocks {blocks} bytes {} reads {n} found {found}", entries, index.bytes());
}
