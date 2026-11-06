//! The evo tests' fixtures: engines, commitments, deltas and B1.

use std::sync::Arc;

use dash_consensus::{BlockId, ChainAncestry};
use dash_dml::codec::ContextFreeNetInfo;
use dash_dml::creditpool::CreditBlockData;
use dash_dml::store::{MinedCommitmentRecord, MinedErase};
use dash_dml::testkit::MemChain;
use dash_dml::{DmlDelta, EvoDelta, MnList, MnListDiffRecord};
use dash_evo::llmq::LlmqCommitment;
use dash_params::LlmqType;
use dash_primitives::{BlockHash, QuorumHash};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use dashd_store::{Commit, KvEngine, MemoryEngine};

use super::{EvoCodec, EvoLayer, EvoWindowDelta, RocksEvoStore};
use crate::flush::{EvoWindow, StateDelta};
use crate::store::{CHAINSTATE_SCHEMA, GlobalFailStop, GuardedEngine, Meta, RecordingSink};

/// The codec over base-sdk's context-free network-info check.
pub fn codec() -> EvoCodec {
    EvoCodec::new(Arc::new(ContextFreeNetInfo))
}

pub fn guarded<E: KvEngine>(engine: E) -> Arc<GuardedEngine<E>> {
    Arc::new(GuardedEngine::new(engine, GlobalFailStop::new(), Arc::new(RecordingSink::default())))
}

/// Every [`MemChain`]'s genesis hash.
pub fn genesis() -> BlockHash {
    let chain = MemChain::new();
    chain.hash(chain.genesis())
}

/// A store over a fresh in-memory chainstate.
pub fn memory_store() -> RocksEvoStore<MemoryEngine> {
    RocksEvoStore::new(guarded(MemoryEngine::new(&CHAINSTATE_SCHEMA)), codec(), genesis())
}

/// A store over a fresh RocksDB chainstate in `dir`.
#[cfg(feature = "rocksdb")]
pub fn rocks_store(dir: &std::path::Path) -> RocksEvoStore<dashd_store::RocksEngine> {
    let (engine, _) =
        dashd_store::RocksEngine::open(dir, &CHAINSTATE_SCHEMA, &Default::default()).unwrap();
    RocksEvoStore::new(guarded(engine), codec(), genesis())
}

/// A null-bodied commitment of `llmq_type` for `quorum_hash`, decoded from Core's layout
/// (version, type, quorum hash, the quorum index from version 2, two empty bit sets, a zero key,
/// vvec hash and signatures).
pub fn commitment(llmq_type: LlmqType, quorum_hash: QuorumHash, version: u16) -> LlmqCommitment {
    let mut bytes = version.to_le_bytes().to_vec();
    bytes.push(llmq_type.to_base());
    bytes.extend_from_slice(quorum_hash.as_bytes());
    if version == 2 || version == 4 {
        bytes.extend_from_slice(&0i16.to_le_bytes());
    }
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&[0; 48 + 32 + 96 + 96]);
    let mut input = &bytes[..];
    let qc = LlmqCommitment::decode(&mut input).expect("a well-formed commitment");
    assert!(input.is_empty());
    qc
}

pub fn quorum_hash(hash: &BlockHash) -> QuorumHash {
    QuorumHash::from(*hash.as_bytes())
}

/// The empty delta of `block`, staging it as the evo tip.
pub fn delta(chain: &MemChain, block: BlockId) -> EvoDelta {
    let hash = chain.hash(block);
    EvoDelta {
        block: hash,
        height: chain.header(block).height,
        dml: None,
        mined_commitments: Vec::new(),
        credit_data: None,
        credit_pool: None,
        mnhf_signals: None,
        erased_mined: Vec::new(),
        evo_tip: Some(hash),
    }
}

/// A commitment record of `base`, mined in the block of `delta`.
pub fn mined(
    chain: &MemChain,
    llmq_type: LlmqType,
    base: BlockId,
    rotation_index: Option<i16>,
) -> MinedCommitmentRecord {
    let quorum_hash = quorum_hash(&chain.hash(base));
    let version = if rotation_index.is_some() {
        2
    } else {
        1
    };
    MinedCommitmentRecord {
        llmq_type,
        quorum_hash,
        base_height: chain.header(base).height,
        rotation_index,
        commitment: Arc::new(commitment(llmq_type, quorum_hash, version)),
    }
}

/// The disconnect delta of `block`: tombstones for `mined`, the evo tip back at the parent.
pub fn undo(chain: &MemChain, block: BlockId, mined: &[MinedCommitmentRecord]) -> EvoDelta {
    let mut d = delta(chain, block);
    d.erased_mined = mined
        .iter()
        .map(|r| MinedErase {
            llmq_type: r.llmq_type,
            quorum_hash: r.quorum_hash,
            mined_height: d.height,
            rotation_index: r.rotation_index,
        })
        .collect();
    d.evo_tip = chain.prev(block).map(|p| chain.hash(p));
    d
}

/// The DML delta of a block whose list is `next`, after `prev`, with a snapshot if asked.
pub fn dml(prev: &MnList, next: &MnList, snapshot: bool) -> DmlDelta {
    DmlDelta {
        block: *next.block_hash(),
        height: next.height(),
        record: Arc::new(MnListDiffRecord::build(prev, next)),
        snapshot: snapshot.then(|| Arc::new(next.clone())),
    }
}

/// B1 as CH2's flush writes it: `window` and `meta.evo_tip`, Durable.
pub fn write_b1<E: KvEngine>(
    store: &RocksEvoStore<E>,
    window: &EvoWindowDelta,
    evo_tip: Option<BlockHash>,
) {
    let mut b = store.db().batch();
    window.write_b1(&mut b);
    if let Some(tip) = evo_tip {
        Meta::put_evo_tip(&mut b, &tip);
    }
    store.db().write(b, Commit::Durable).unwrap();
}

/// A freeze, B1 with the evo tip taken at it, and W's release.
pub fn flush<E: KvEngine>(store: &RocksEvoStore<E>, layer: &mut EvoLayer) {
    let tip = layer.evo_tip();
    let window = layer.take();
    write_b1(store, &window, tip);
    layer.release();
}

/// A credit-data row with `balance` and nothing unlocked.
pub fn credit_data(balance: i64) -> CreditBlockData {
    CreditBlockData {
        balance,
        unlocked: 0,
    }
}

/// The bytes of a CbTx of `version` at `height` with the given roots, a null ChainLock and no
/// balance (`cbtx.h:54-67`): the fields its version has.
pub fn cbtx_bytes(version: u16, height: u32, mn_root: u8, quorum_root: u8) -> Vec<u8> {
    let mut bytes = [&version.to_le_bytes()[..], &height.to_le_bytes(), &[mn_root; 32]].concat();
    if version >= 2 {
        bytes.extend([quorum_root; 32]);
    }
    if version >= 3 {
        bytes.push(0);
        bytes.extend([0; 96]);
        bytes.extend(0i64.to_le_bytes());
    }
    if version >= 4 {
        bytes.extend([0; 32]);
    }
    bytes
}
