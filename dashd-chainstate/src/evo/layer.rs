//! The evo layer (CH7 D22; A1 §2.10.2): the committed blocks' evo rows that are not yet durable,
//! as two merged indexes over the DB, and the window CH2's flush takes from it.
//!
//! Core keeps them in its evodb transactions, flushed with the coins in a full flush
//! (`validation.cpp` v24:3164-3169). Here a commit merges a block's rows into the **active**
//! index; a freeze moves it to the **frozen** one, which B1 writes, and starts an empty active
//! one. CH2 freezes one window at a time, so there are at most two indexes whatever the window's
//! size, and a read costs O(log n) in the window's rows plus at most one DB read (D22).

use std::sync::Arc;

use dash_dml::StoreError;
use dash_primitives::BlockHash;
use dashd_store::{Cf, KvBatch, KvRead};
use imbl::{HashMap, OrdMap};

use super::codec::{DeltaRows, EvoCf, MinedKey, split_mined_key};
use crate::flush::{EvoWindow, StateDelta};

/// The merged rows of a run of committed blocks (D22): later rows and tombstones win, so writing
/// it equals writing the blocks' deltas in order. Cloning it is O(1), which a freeze and a pinned
/// read view rely on.
#[derive(Clone, Debug, Default)]
pub struct EvoIndex {
    /// `mn_diff`, `mn_snapshot`, `credit_pool` (pool and credit data) and `mnhf` rows.
    by_hash: HashMap<(EvoCf, BlockHash), Arc<[u8]>>,
    /// `llmq_mined` and `llmq_mined_h` rows by [`MinedKey`]; `None` is a disconnect's tombstone
    /// (CH7 §4.1.4).
    mined: OrdMap<MinedKey, Option<Arc<[u8]>>>,
    /// The last committed delta's evo best block (D7).
    evo_tip: Option<BlockHash>,
    /// The encoded key and value bytes of every entry, a tombstone counting its key (D18).
    bytes: u64,
}

/// What a level of the evo state holds for a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lookup<'a> {
    /// A row.
    Live(&'a Arc<[u8]>),
    /// A tombstone: the row is erased, whatever the levels below hold.
    Erased,
    /// Nothing here: the levels below answer.
    Absent,
}

fn entry_bytes(key: usize, value: Option<&Arc<[u8]>>) -> u64 {
    (key + value.map_or(0, |v| v.len())) as u64
}

impl EvoIndex {
    /// The by-hash row of `kind` for `block`, if this index has one.
    pub fn get(&self, kind: EvoCf, block: &BlockHash) -> Option<&Arc<[u8]>> {
        self.by_hash.get(&(kind, *block))
    }

    /// The mined row at `key`.
    pub fn mined(&self, key: &[u8]) -> Lookup<'_> {
        match self.mined.get(key) {
            Some(Some(value)) => Lookup::Live(value),
            Some(None) => Lookup::Erased,
            None => Lookup::Absent,
        }
    }

    /// The mined entries with keys starting with `prefix`, from `start` on, in key order;
    /// tombstones as `None`.
    pub fn mined_from<'a>(
        &'a self,
        prefix: &'a [u8],
        start: &[u8],
    ) -> impl Iterator<Item = (&'a [u8], Option<&'a Arc<[u8]>>)> + 'a {
        self.mined
            .range(start.max(prefix).to_vec()..)
            .take_while(move |(key, _)| key.starts_with(prefix))
            .map(|(key, value)| (key.as_slice(), value.as_ref()))
    }

    /// The evo best block of the last committed delta.
    pub fn evo_tip(&self) -> Option<BlockHash> {
        self.evo_tip
    }

    /// The encoded bytes of the index's entries (D18).
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    fn put(&mut self, kind: EvoCf, block: BlockHash, value: Arc<[u8]>) {
        let key = kind.key(&block).as_bytes().len();
        let added = entry_bytes(key, Some(&value));
        if let Some(old) = self.by_hash.insert((kind, block), value) {
            self.bytes -= entry_bytes(key, Some(&old));
        }
        self.bytes += added;
    }

    fn put_mined(&mut self, key: MinedKey, value: Option<Arc<[u8]>>) {
        let len = key.len();
        let added = entry_bytes(len, value.as_ref());
        if let Some(old) = self.mined.insert(key, value) {
            self.bytes -= entry_bytes(len, old.as_ref());
        }
        self.bytes += added;
    }

    /// Writes the index into B1 (CH2 §2.7): a put per live entry, a delete per tombstone.
    pub fn write_b1<B: KvBatch>(&self, batch: &mut B) {
        for ((kind, block), value) in &self.by_hash {
            batch.put(kind.cf(), kind.key(block).as_bytes(), value);
        }
        for (key, value) in &self.mined {
            let (cf, key) = split_mined_key(key);
            match value {
                Some(value) => batch.put(cf.cf(), key, value),
                None => batch.delete(cf.cf(), key),
            }
        }
    }
}

/// A frozen [`EvoIndex`]: CH2's [`StateDelta`], which B1 writes with `meta.evo_tip` (D7).
#[derive(Debug)]
pub struct EvoWindowDelta(EvoIndex);

impl EvoWindowDelta {
    pub fn index(&self) -> &EvoIndex {
        &self.0
    }
}

impl StateDelta for EvoWindowDelta {
    const EVO: bool = true;

    fn write_b1<B: KvBatch>(&self, batch: &mut B) {
        self.0.write_b1(batch);
    }

    fn bytes(&self) -> u64 {
        self.0.bytes
    }
}

/// The evo state above the DB: the active index, and the frozen one while its flush is in flight
/// (D22). V is its only writer (A1 §2.1 rule 4).
#[derive(Debug, Default)]
pub struct EvoLayer {
    active: EvoIndex,
    frozen: Option<EvoIndex>,
    /// The last committed delta's evo tip, kept across takes and releases, else the durable
    /// `meta.evo_tip` the layer resumed from (D37): what CH2's freeze check holds the flush's tip to.
    committed_tip: Option<BlockHash>,
}

/// A read through the levels below the active index: the frozen index, then the DB.
fn read_below<R: KvRead>(
    frozen: Option<&EvoIndex>,
    db: &R,
    level: impl Fn(&EvoIndex) -> Lookup<'_>,
    cf: Cf,
    key: &[u8],
) -> Result<Option<Arc<[u8]>>, StoreError> {
    match frozen.map_or(Lookup::Absent, level) {
        Lookup::Live(value) => Ok(Some(Arc::clone(value))),
        Lookup::Erased => Ok(None),
        Lookup::Absent => Ok(db.get(cf, key).map_err(db_error)?.map(Arc::from)),
    }
}

/// A DB read's failure: a store fault (A1 I5); the guarded engine has tripped the latch.
pub(crate) fn db_error(e: dashd_store::StoreError) -> StoreError {
    StoreError::read(e.to_string())
}

/// Core's `WriteDerived` check (`evo/evodb.h` v24:159-205): a block-derived row may be written
/// again only with the same bytes.
fn check_derived(
    existing: Option<&[u8]>,
    value: &[u8],
    what: &'static str,
) -> Result<(), StoreError> {
    match existing {
        Some(existing) if existing != value => Err(StoreError::inconsistent(what)),
        _ => Ok(()),
    }
}

impl EvoLayer {
    pub fn new() -> Self {
        Self::default()
    }

    /// A layer over a DB whose `meta.evo_tip` is `evo_tip`, so the first freeze after an open is
    /// checked as every later one is (D37).
    pub fn resume(evo_tip: Option<BlockHash>) -> Self {
        Self {
            committed_tip: evo_tip,
            ..Self::default()
        }
    }

    /// `commit_tx`'s merge (D1, D22): `rows` into the active index, all or nothing.
    ///
    /// A block-derived row (by hash, and `llmq_mined`) that meets a row in the layer or `db` must
    /// equal it, as Core's `WriteDerived` reads through its transaction to the DB; the height index
    /// is written plainly, as Core's `Write`. Tombstones replace both rows of each erased
    /// commitment; a later put revives them.
    ///
    /// # Errors
    ///
    /// [`StoreError::Inconsistent`] for a differing rewrite, and a DB read's fault; the layer is
    /// then unchanged.
    pub fn commit<R: KvRead>(&mut self, rows: DeltaRows, db: &R) -> Result<(), StoreError> {
        let mut next = self.active.clone();
        let frozen = self.frozen.as_ref();
        for (kind, block, value) in rows.by_hash {
            let existing = match next.get(kind, &block) {
                Some(row) => Some(Arc::clone(row)),
                None => read_below(
                    frozen,
                    db,
                    |i| i.get(kind, &block).map_or(Lookup::Absent, Lookup::Live),
                    kind.cf(),
                    kind.key(&block).as_bytes(),
                )?,
            };
            check_derived(existing.as_deref(), &value, "block-derived evo row rewrite")?;
            if existing.is_none() {
                next.put(kind, block, value);
            }
        }
        for (key, value) in rows.mined {
            let existing = match next.mined(&key) {
                Lookup::Live(row) => Some(Arc::clone(row)),
                Lookup::Erased => None,
                Lookup::Absent => {
                    let (cf, cf_key) = split_mined_key(&key);
                    read_below(frozen, db, |i| i.mined(&key), cf.cf(), cf_key)?
                }
            };
            check_derived(existing.as_deref(), &value, "mined commitment rewrite")?;
            if existing.is_none() {
                next.put_mined(key, Some(value));
            }
        }
        for (key, value) in rows.index {
            next.put_mined(key, Some(value));
        }
        for key in rows.erased {
            next.put_mined(key, None);
        }
        next.evo_tip = Some(rows.evo_tip);
        self.active = next;
        self.committed_tip = Some(rows.evo_tip);
        Ok(())
    }

    /// The evo best block of the committed state above the DB: the active index's, then the
    /// frozen one's (D7). `None` when every committed row is durable, and `meta.evo_tip` answers.
    pub fn evo_tip(&self) -> Option<BlockHash> {
        self.active.evo_tip.or_else(|| self.frozen.as_ref().and_then(|f| f.evo_tip))
    }

    /// O(1) clones of the active and frozen indexes, for a pinned read view (D22).
    pub fn indexes(&self) -> (EvoIndex, Option<EvoIndex>) {
        (self.active.clone(), self.frozen.clone())
    }
}

impl EvoWindow<EvoWindowDelta> for EvoLayer {
    /// Freezes the active index (an O(1) clone kept for reads until W's release) and starts an
    /// empty one.
    fn take(&mut self) -> EvoWindowDelta {
        debug_assert!(self.frozen.is_none(), "one window in flight");
        let taken = std::mem::take(&mut self.active);
        self.frozen = Some(taken.clone());
        EvoWindowDelta(taken)
    }

    fn bytes(&self) -> u64 {
        self.active.bytes + self.frozen.as_ref().map_or(0, EvoIndex::bytes)
    }

    /// The last committed evo tip, which a release does not forget (D37): a window that skipped a
    /// block's commit fails the freeze check even when every earlier row is durable.
    fn best_block(&self) -> Option<BlockHash> {
        self.committed_tip
    }

    /// B1 is durable: the DB now holds the frozen index's rows.
    fn release(&mut self) {
        self.frozen = None;
    }
}

#[cfg(test)]
#[path = "layer_tests.rs"]
mod tests;
