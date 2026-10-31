//! `EvoReadView` (A1 §2.10.2; CH7 D17, D22): one coherent read of the evo state, pinned at a
//! point in V's commits, and dash-dml's [`EvoStore`] over it.
//!
//! A read checks the active index, then the frozen one, then the DB snapshot taken with them. Every
//! by-hash row is keyed by block hash and never mutated, so those reads hold across reorgs. The
//! mined rows are the chain's at the pin (undo erases them, CH7 Q1); a read resolves them through
//! the ancestry of the block it names, as [`MinedCommitmentReader`] requires.

use std::sync::Arc;

use dash_consensus::{BlockId, ChainAncestry};
use dash_dml::creditpool::{CreditBlockData, PoolState};
use dash_dml::mnhf::EhfSignals;
use dash_dml::{DmlReader, EvoStore, MnList, MnListDiffRecord, StoreError};
use dash_evo::llmq::{
    MinedCommitment, MinedCommitmentReader, MinedFamily, MinedIndexEntry, QuorumSnapshot,
};
use dash_params::LlmqType;
use dash_primitives::{BlockHash, QuorumHash};
use dashd_store::{DbSchema, KvEngine, KvRead};

use super::codec::{
    self, EvoCf, EvoCodec, MinedKey, decode_index, decode_mined, index_key, index_key_height,
    index_prefix, mined_key, split_mined_key,
};
use super::layer::{EvoIndex, Lookup, db_error};
use crate::store::{EVO_TIP, GuardedSnapshot};

/// The evo state as of one moment of V's: O(1) clones of the active and frozen indexes and a DB
/// snapshot taken with them (D22), bound to the block index the `at` arguments name.
pub struct EvoReadView<'a, E: KvEngine + 'a> {
    chain: &'a dyn ChainAncestry,
    active: EvoIndex,
    frozen: Option<EvoIndex>,
    snapshot: GuardedSnapshot<'a, E>,
    codec: &'a EvoCodec,
    genesis: BlockHash,
}

/// A row as a level holds it: shared from an index, or read from the DB.
enum Row<'r> {
    Index(&'r Arc<[u8]>),
    Db(Vec<u8>),
}

impl std::ops::Deref for Row<'_> {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match self {
            Row::Index(row) => row,
            Row::Db(row) => row,
        }
    }
}

impl<'a, E: KvEngine + 'a> EvoReadView<'a, E> {
    pub(crate) fn new(
        chain: &'a dyn ChainAncestry,
        (active, frozen): (EvoIndex, Option<EvoIndex>),
        snapshot: GuardedSnapshot<'a, E>,
        codec: &'a EvoCodec,
        genesis: BlockHash,
    ) -> Self {
        Self {
            chain,
            active,
            frozen,
            snapshot,
            codec,
            genesis,
        }
    }

    fn indexes(&self) -> impl Iterator<Item = &EvoIndex> {
        std::iter::once(&self.active).chain(&self.frozen)
    }

    /// The by-hash row of `kind` for `block`, decoded.
    fn by_hash<T>(
        &self,
        kind: EvoCf,
        block: &BlockHash,
        decode: impl FnOnce(&[u8]) -> Result<T, StoreError>,
    ) -> Result<Option<T>, StoreError> {
        let row = match self.indexes().find_map(|i| i.get(kind, block)) {
            Some(row) => Row::Index(row),
            None => {
                match self.snapshot.get(kind.cf(), kind.key(block).as_bytes()).map_err(db_error)? {
                    Some(row) => Row::Db(row),
                    None => return Ok(None),
                }
            }
        };
        decode(&row).map(Some)
    }

    /// The live mined row at `key`: the first level that holds the key answers.
    fn mined_row(&self, key: &[u8]) -> Result<Option<Row<'_>>, StoreError> {
        for index in self.indexes() {
            match index.mined(key) {
                Lookup::Live(row) => return Ok(Some(Row::Index(row))),
                Lookup::Erased => return Ok(None),
                Lookup::Absent => {}
            }
        }
        let (cf, cf_key) = split_mined_key(key);
        self.snapshot.get(cf.cf(), cf_key).map(|row| row.map(Row::Db)).map_err(db_error)
    }

    /// The live mined rows with keys starting with `prefix`, from `start` on, in key order, each
    /// handed to `visit` until it returns false: the active index, the frozen one and the DB merged,
    /// the upper level winning a key and its tombstones hiding the rows below (D22: O(k + log n)
    /// for k rows).
    fn mined_scan(
        &self,
        prefix: &[u8],
        start: &[u8],
        mut visit: impl FnMut(&[u8], &[u8]) -> Result<bool, StoreError>,
    ) -> Result<(), StoreError> {
        let (cf, cf_prefix) = split_mined_key(prefix);
        let cf_start = &start[1..];
        let mut levels: Vec<_> =
            self.indexes().map(|i| i.mined_from(prefix, start).peekable()).collect();
        let mut db = self.snapshot.iter_prefix_from(cf.cf(), cf_prefix, cf_start).peekable();
        loop {
            // The smallest key any level holds next, without the family tag.
            let db_head = match db.peek() {
                Some(Ok((key, _))) => Some(key.clone()),
                Some(Err(_)) => {
                    return Err(db_error(db.next().expect("peeked").expect_err("an error")));
                }
                None => None,
            };
            let heads: Vec<Option<&[u8]>> =
                levels.iter_mut().map(|l| l.peek().map(|&(key, _)| &key[1..])).collect();
            let Some(next) = heads.iter().flatten().copied().chain(db_head.as_deref()).min() else {
                return Ok(());
            };
            let next = next.to_vec();
            // The upper level answers; every level at the key moves past it.
            let mut row: Option<Option<Row<'_>>> = None;
            for (level, head) in levels.iter_mut().zip(heads) {
                if head == Some(next.as_slice()) {
                    let (_, value) = level.next().expect("peeked");
                    row.get_or_insert(value.map(Row::Index));
                }
            }
            if db_head.as_deref() == Some(next.as_slice()) {
                let (_, value) = db.next().expect("peeked").map_err(db_error)?;
                row.get_or_insert(Some(Row::Db(value)));
            }
            if let Some(Some(value)) = row {
                let key: MinedKey = [&prefix[..1], &next].concat();
                if !visit(&key, &value)? {
                    return Ok(());
                }
            }
        }
    }

    /// Whether the block `hash` at `height` is `at` or one of its ancestors.
    fn in_ancestry(&self, at: BlockId, height: u32, hash: &BlockHash) -> bool {
        self.chain.ancestor(at, height).is_some_and(|id| self.chain.header(id).hash == *hash)
    }
}

impl<'a, E: KvEngine + 'a> DmlReader for EvoReadView<'a, E>
where
    GuardedSnapshot<'a, E>: Sync,
{
    fn mn_list_snapshot(&self, block: &BlockHash) -> Result<Option<Arc<MnList>>, StoreError> {
        self.by_hash(EvoCf::MnSnapshot, block, |row| self.codec.decode_snapshot(row, block))
    }

    fn mn_list_diff(&self, block: &BlockHash) -> Result<Option<Arc<MnListDiffRecord>>, StoreError> {
        self.by_hash(EvoCf::MnDiff, block, |row| self.codec.decode_diff(row))
    }
}

impl<'a, E: KvEngine + 'a> MinedCommitmentReader for EvoReadView<'a, E>
where
    GuardedSnapshot<'a, E>: Sync,
{
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
        let Some(row) = self.mined_row(&mined_key(llmq_type, quorum_hash))? else {
            return Ok(None);
        };
        let row = decode_mined(&row)?;
        Ok(self.in_ancestry(at, row.height, &row.block).then_some((row.commitment, row.block)))
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
        let mut entries = Vec::new();
        if cap == 0 || limit == 0 {
            return Ok(entries);
        }
        let rotation = match family {
            MinedFamily::NonRotated => None,
            MinedFamily::Rotated {
                quorum_index,
            } => Some(quorum_index),
        };
        let prefix = index_prefix(llmq_type, rotation);
        let start = index_key(llmq_type, rotation, cap);
        self.mined_scan(&prefix, &start, |key, value| {
            let mined_height = index_key_height(key)?;
            // Core stops before height 0 (`curKey >= lastKey`).
            if mined_height == 0 {
                return Ok(false);
            }
            let row = decode_index(value)?;
            // No commit is based above its mining block (`EvoCodec::rows` refuses one), so such a
            // row is corruption (D39). Core asserts only that the base is an ancestor of the chain
            // end (`llmq/blockprocessor.cpp:761-767`, `:804-809`).
            if row.base_height > mined_height {
                return Err(StoreError::read(format!(
                    "llmq_mined_h row mined at {mined_height} names base height {}",
                    row.base_height
                )));
            }
            if self.in_ancestry(chain_end, mined_height, &row.block) {
                entries.push(MinedIndexEntry {
                    mined_height,
                    base_height: row.base_height,
                });
            }
            Ok(entries.len() < limit)
        })?;
        Ok(entries)
    }
}

impl<'a, E: KvEngine + 'a> EvoStore for EvoReadView<'a, E>
where
    GuardedSnapshot<'a, E>: Sync,
{
    // Core: evo/evodb.cpp:120 CEvoDB::VerifyBestBlock
    /// The pin's evo best block: the active index's, the frozen one's, else `meta.evo_tip`. One
    /// naming genesis reads as absent, at every level (D7, D38): Core writes no evo best block for
    /// genesis (`validation.cpp:2588-2592`), so `VerifyBestBlock` fails at a DIP3-active block 1.
    fn best_block(&self) -> Result<Option<BlockHash>, StoreError> {
        let tip = match self.indexes().find_map(EvoIndex::evo_tip) {
            Some(tip) => tip,
            None => match self.snapshot.get(DbSchema::META, EVO_TIP).map_err(db_error)? {
                None => return Ok(None),
                Some(v) => <[u8; 32]>::try_from(v.as_slice())
                    .map(BlockHash::from)
                    .map_err(|_| StoreError::read("meta.evo_tip is not a block hash"))?,
            },
        };
        Ok(Some(tip).filter(|tip| *tip != self.genesis))
    }

    fn credit_pool(&self, block: &BlockHash) -> Result<Option<Arc<PoolState>>, StoreError> {
        self.by_hash(EvoCf::CreditPool, block, |row| codec::decode_pool(row).map(Arc::new))
    }

    fn credit_data(&self, block: &BlockHash) -> Result<Option<CreditBlockData>, StoreError> {
        self.by_hash(EvoCf::CreditData, block, codec::decode_credit_data)
    }

    fn mnhf_signals(&self, block: &BlockHash) -> Result<Option<EhfSignals>, StoreError> {
        self.by_hash(EvoCf::Mnhf, block, codec::decode_mnhf)
    }

    /// An effect row, written straight to the DB (§6.2): the snapshot's.
    fn quorum_snapshot(
        &self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
    ) -> Result<Option<Arc<QuorumSnapshot>>, StoreError> {
        let key = codec::quorum_snapshot_key(llmq_type, cycle_base);
        match self.snapshot.get(codec::cf("llmq_snapshot"), &key).map_err(db_error)? {
            None => Ok(None),
            Some(row) => codec::decode_quorum_snapshot(&row).map(|s| Some(Arc::new(s))),
        }
    }
}
