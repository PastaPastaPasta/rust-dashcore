//! `MemEvoStore`: the in-memory evo store for tests, fuzzing and the L2 harnesses (EV2 §2.10;
//! PLAN M-9c: LQ1a starts on it).

use std::cmp::Reverse;
use std::sync::Arc;

use dash_consensus::{BlockId, ChainAncestry};
use dash_params::LlmqType;
use dash_primitives::{BlockHash, QuorumHash};
use dash_types::Numeric;
use imbl::OrdMap;

use super::{DmlReader, EvoDelta, EvoStore, StoreError};
use crate::creditpool::{CreditBlockData, PoolState};
use crate::mnhf::EhfSignals;
use crate::{MnList, MnListDiffRecord, SameRecord};
use dash_evo::llmq::{
    LlmqCommitment, MinedCommitment, MinedCommitmentReader, MinedFamily, MinedIndexEntry,
    QuorumSnapshot,
};

/// A height-index key: type, family (`None` for the non-rotated index, else the quorum index),
/// mined height newest first, mined block.
type IndexKey = (u8, Option<i16>, Reverse<u32>, BlockHash);

fn family_key(family: MinedFamily) -> Option<i16> {
    match family {
        MinedFamily::NonRotated => None,
        MinedFamily::Rotated {
            quorum_index,
        } => Some(quorum_index),
    }
}

/// One `q_mc` row: a commitment as one block mined it.
#[derive(Clone, Debug, PartialEq)]
struct Mined {
    commitment: Arc<LlmqCommitment>,
    block: BlockHash,
    height: u32,
}

/// Every evo record, in persistent maps keyed as A1 §2.9's column families are (`imbl`, so
/// [`MemEvoStore::apply`] is atomic at O(log n) per row). Nothing is ever erased; a
/// rewrite of an existing key must equal what is stored, as v24's `WriteDerived` requires, and
/// a differing rewrite is [`StoreError::Inconsistent`] (EV2 §3.8). Rotation snapshots are the
/// exception: Core overwrites them unconditionally (LQ1 §3.7.2).
///
/// It implements [`DmlReader`] itself. The ancestry-resolved reads need the block index, so the
/// full [`EvoStore`] is a [`MemEvoView`] that binds the store to one ([`MemEvoStore::view`]), as
/// CH7a's `EvoReadView` binds its DB snapshot to the index.
#[derive(Clone, Debug, Default)]
pub struct MemEvoStore {
    best_block: Option<BlockHash>,
    mn_snapshots: OrdMap<BlockHash, Arc<MnList>>,
    mn_diffs: OrdMap<BlockHash, Arc<MnListDiffRecord>>,
    credit_data: OrdMap<BlockHash, Arc<CreditBlockData>>,
    credit_pools: OrdMap<BlockHash, Arc<PoolState>>,
    mnhf_signals: OrdMap<BlockHash, Arc<EhfSignals>>,
    quorum_snapshots: OrdMap<(u8, BlockHash), Arc<QuorumSnapshot>>,
    /// `q_mc`: per (type, quorum hash), every block that mined it, on any branch.
    mined: OrdMap<(u8, QuorumHash), Vec<Mined>>,
    /// Both `q_mcih` families: the base height.
    index: OrdMap<IndexKey, u32>,
}

/// Inserts `value` at `key`, or checks that an existing value equals it (Core's
/// `WriteDerived`, `evo/evodb.h` v24:159-205).
///
/// The comparison is [`SameRecord`] (shared rows short-circuit on pointer equality): rows as Core
/// serializes them, with operator keys also equal under Core's `operator==`, both on clones. For
/// rows decoded from bytes that is Core's serialized comparison, and checking a rewrite cannot
/// change the stored row.
fn write_derived<K: Ord + Clone, V: SameRecord>(
    map: &mut OrdMap<K, Arc<V>>,
    key: K,
    value: &Arc<V>,
    what: &'static str,
) -> Result<(), StoreError> {
    match map.get(&key) {
        None => {
            map.insert(key, Arc::clone(value));
            Ok(())
        }
        Some(existing) if existing.same_record(value) => Ok(()),
        Some(_) => Err(StoreError::inconsistent(what)),
    }
}

/// Writes a height-index row, whose value is the base height (Core's plain `Write`; a differing
/// rewrite is still an inconsistency here, as the derived rows are).
fn write_index<K: Ord + Clone>(
    map: &mut OrdMap<K, u32>,
    key: K,
    base_height: u32,
) -> Result<(), StoreError> {
    match map.get(&key) {
        None => {
            map.insert(key, base_height);
            Ok(())
        }
        Some(existing) if *existing == base_height => Ok(()),
        Some(_) => Err(StoreError::inconsistent("mined commitment height-index rewrite")),
    }
}

impl MemEvoStore {
    /// An empty store: no evo tip, no records.
    pub fn new() -> Self {
        Self::default()
    }

    /// Writes a connected block's delta and moves the evo tip to it (CH7a's B1 write).
    ///
    /// All or nothing: a failed apply leaves the store unchanged. The delta must be consistent
    /// with itself (one block, one height; a snapshot stamped with its own block). Test store:
    /// it does not check that the delta's parent is the evo tip.
    pub fn apply(&mut self, delta: &EvoDelta) -> Result<(), StoreError> {
        let mut next = self.clone();
        next.write(delta)?;
        *self = next;
        Ok(())
    }

    fn write(&mut self, delta: &EvoDelta) -> Result<(), StoreError> {
        let block = delta.block;
        if let Some(dml) = &delta.dml {
            let height = i64::from(delta.height);
            if dml.block != block
                || i64::from(dml.height) != height
                || i64::from(dml.record.height) != height
            {
                return Err(StoreError::inconsistent("DML delta for another block"));
            }
            write_derived(&mut self.mn_diffs, block, &dml.record, "mn_diff rewrite")?;
            if let Some(snapshot) = &dml.snapshot {
                if *snapshot.block_hash() != block || i64::from(snapshot.height()) != height {
                    return Err(StoreError::inconsistent("mn_snapshot of another block"));
                }
                write_derived(&mut self.mn_snapshots, block, snapshot, "mn_snapshot rewrite")?;
            }
        }
        if let Some(data) = &delta.credit_data {
            write_derived(&mut self.credit_data, block, &Arc::new(*data), "credit data rewrite")?;
        }
        if let Some(pool) = &delta.credit_pool {
            write_derived(&mut self.credit_pools, block, pool, "credit pool rewrite")?;
        }
        if let Some(signals) = &delta.mnhf_signals {
            write_derived(&mut self.mnhf_signals, block, &Arc::new(*signals), "MNHF rewrite")?;
        }
        for record in &delta.mined_commitments {
            if record.base_height > delta.height {
                return Err(StoreError::inconsistent("commitment base above its mining block"));
            }
            let t = record.llmq_type.to_base();
            let rows = self.mined.entry((t, record.quorum_hash)).or_default();
            let mined = Mined {
                commitment: Arc::clone(&record.commitment),
                block,
                height: delta.height,
            };
            match rows.iter().find(|row| row.block == block) {
                Some(row) if *row == mined => {}
                Some(_) => return Err(StoreError::inconsistent("mined commitment rewrite")),
                None => rows.push(mined),
            }
            let key = (t, record.rotation_index, Reverse(delta.height), block);
            write_index(&mut self.index, key, record.base_height)?;
        }
        // Erased mined rows need no write: every read resolves through `at`'s ancestry, which a
        // disconnected block is not in, and a reconnect finds its own row unchanged.
        self.best_block = Some(delta.evo_tip.unwrap_or(block));
        Ok(())
    }

    /// Disconnects the evo tip `block`, making `prev` the tip. Nothing is erased (EV2 §2.10).
    pub fn disconnect(
        &mut self,
        block: &BlockHash,
        prev: Option<BlockHash>,
    ) -> Result<(), StoreError> {
        if self.best_block != Some(*block) {
            return Err(StoreError::inconsistent("disconnect of a block that is not the evo tip"));
        }
        self.best_block = prev;
        Ok(())
    }

    /// `StoreSnapshotForBlock`: an unconditional overwrite of the rotation snapshot row, as
    /// [`dash_evo::effect::Effect::StoreQuorumSnapshot`] and runtime writes apply it (LQ1 §3.7.2).
    pub fn store_quorum_snapshot(
        &mut self,
        llmq_type: LlmqType,
        cycle_base: BlockHash,
        snapshot: QuorumSnapshot,
    ) {
        self.quorum_snapshots.insert((llmq_type.to_base(), cycle_base), Arc::new(snapshot));
    }

    /// The store bound to `chain`, resolving `at` arguments through it.
    pub fn view<'a>(&'a self, chain: &'a dyn ChainAncestry) -> MemEvoView<'a> {
        MemEvoView {
            store: self,
            chain,
        }
    }
}

impl DmlReader for MemEvoStore {
    fn mn_list_snapshot(&self, block: &BlockHash) -> Result<Option<Arc<MnList>>, StoreError> {
        Ok(self.mn_snapshots.get(block).cloned())
    }

    fn mn_list_diff(&self, block: &BlockHash) -> Result<Option<Arc<MnListDiffRecord>>, StoreError> {
        Ok(self.mn_diffs.get(block).cloned())
    }
}

/// A [`MemEvoStore`] bound to a block index: the full [`EvoStore`].
#[derive(Clone, Copy)]
pub struct MemEvoView<'a> {
    store: &'a MemEvoStore,
    chain: &'a dyn ChainAncestry,
}

impl std::fmt::Debug for MemEvoView<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemEvoView").field("store", self.store).finish_non_exhaustive()
    }
}

impl MemEvoView<'_> {
    /// Whether the block `hash` at `height` is `at` or one of its ancestors.
    fn in_ancestry(&self, at: BlockId, height: u32, hash: &BlockHash) -> bool {
        self.chain.ancestor(at, height).is_some_and(|id| self.chain.header(id).hash == *hash)
    }
}

impl DmlReader for MemEvoView<'_> {
    fn mn_list_snapshot(&self, block: &BlockHash) -> Result<Option<Arc<MnList>>, StoreError> {
        self.store.mn_list_snapshot(block)
    }

    fn mn_list_diff(&self, block: &BlockHash) -> Result<Option<Arc<MnListDiffRecord>>, StoreError> {
        self.store.mn_list_diff(block)
    }
}

impl MinedCommitmentReader for MemEvoView<'_> {
    type Error = StoreError;

    fn chain(&self) -> &dyn ChainAncestry {
        self.chain
    }

    fn mined_commitment(
        &self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
        at: BlockId,
    ) -> Result<Option<MinedCommitment>, StoreError> {
        let Some(rows) = self.store.mined.get(&(llmq_type.to_base(), *quorum_hash)) else {
            return Ok(None);
        };
        Ok(rows
            .iter()
            .find(|row| self.in_ancestry(at, row.height, &row.block))
            .map(|row| (Arc::clone(&row.commitment), row.block)))
    }

    // Core: llmq/blockprocessor.cpp:687 CQuorumBlockProcessor::GetMinedCommitmentsUntilBlock
    // Core: llmq/blockprocessor.cpp:731 CQuorumBlockProcessor::GetLastMinedCommitmentsByQuorumIndexUntilBlock
    fn mined_index(
        &self,
        llmq_type: LlmqType,
        family: MinedFamily,
        chain_end: BlockId,
        max_mined_height: u32,
        limit: usize,
    ) -> Result<Vec<MinedIndexEntry>, StoreError> {
        let cap = max_mined_height.min(self.chain.header(chain_end).height);
        if cap == 0 {
            return Ok(Vec::new());
        }
        let (t, f) = (llmq_type.to_base(), family_key(family));
        // Core stops before height 0 (`curKey >= lastKey`).
        let range = (t, f, Reverse(cap), BlockHash::ZERO)..=(t, f, Reverse(1), max_hash());
        Ok(self
            .store
            .index
            .range(range)
            .filter(|((_, _, Reverse(height), block), _)| {
                self.in_ancestry(chain_end, *height, block)
            })
            .take(limit)
            .map(|((_, _, Reverse(mined_height), _), base_height)| MinedIndexEntry {
                mined_height: *mined_height,
                base_height: *base_height,
            })
            .collect())
    }
}

impl EvoStore for MemEvoView<'_> {
    fn best_block(&self) -> Result<Option<BlockHash>, StoreError> {
        Ok(self.store.best_block)
    }

    fn credit_pool(&self, block: &BlockHash) -> Result<Option<Arc<PoolState>>, StoreError> {
        Ok(self.store.credit_pools.get(block).cloned())
    }

    fn credit_data(&self, block: &BlockHash) -> Result<Option<CreditBlockData>, StoreError> {
        Ok(self.store.credit_data.get(block).map(|data| **data))
    }

    fn mnhf_signals(&self, block: &BlockHash) -> Result<Option<EhfSignals>, StoreError> {
        Ok(self.store.mnhf_signals.get(block).map(|signals| **signals))
    }

    fn quorum_snapshot(
        &self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
    ) -> Result<Option<Arc<QuorumSnapshot>>, StoreError> {
        Ok(self.store.quorum_snapshots.get(&(llmq_type.to_base(), *cycle_base)).cloned())
    }
}

/// The largest block hash, the upper bound of a key range.
fn max_hash() -> BlockHash {
    BlockHash::from([0xff; 32])
}
