//! What a subscriber's data object computes (CH2 §2.9's state contract): its rows by class, its
//! locator-committed state, and its rewind. The driver writes every row; the data object only
//! computes them.

use std::sync::Arc;

use dash_chain::{BlockRef, Needs};
use dash_consensus::Block;
use dashd_store::{Cf, KvEngine};

use super::SubscriberError;
use crate::store::{GuardedEngine, GuardedRead};

/// Row classes, declared once (§2.9).
#[derive(Debug)]
pub struct RowSchema {
    /// Keys start with the block's big-endian height. One row per block on the indexed chain,
    /// written per block (Buffered) as processed, so a row is readable when `best_block()`
    /// reports its block. Blind-deleted by range above L.
    pub height_cfs: &'static [Cf],
    /// Hash-keyed copies of blocks that left the indexed chain (Core's
    /// `CopyHeightIndexToHashIndex`), written at the disconnect after `sync_files`. Never deleted.
    pub archive_cfs: &'static [Cf],
    /// Fixed keys written only in locator batches (coinstatsindex `DB_MUHASH`).
    pub state_keys: &'static [(Cf, &'static [u8])],
    /// Keys that are not height-prefixed, mutated per block and undone on disconnect (IX2).
    /// Written only in locator batches; rows for (L, p] live in the journal.
    pub keyed_cfs: &'static [Cf],
    /// Idempotent overwrites, never deleted (txindex, DEC-77 Q6). Written per block (Buffered).
    pub inexact_cfs: &'static [Cf],
    /// The subscriber's own flat files, with a monotone write position.
    pub has_files: bool,
    /// The driver's locator key (Core's `DB_BEST_BLOCK`).
    pub locator: (Cf, &'static [u8]),
}

impl RowSchema {
    /// Only inexact rows: no journal, no `D_max` (§2.9 items 2 and 6).
    pub fn inexact_only(&self) -> bool {
        self.height_cfs.is_empty()
            && self.keyed_cfs.is_empty()
            && self.state_keys.is_empty()
            && !self.has_files
    }

    pub(crate) fn class(&self, cf: Cf) -> RowClass {
        if self.height_cfs.contains(&cf) {
            RowClass::Height
        } else if self.keyed_cfs.contains(&cf) {
            RowClass::Keyed
        } else if self.inexact_cfs.contains(&cf) {
            RowClass::Inexact
        } else if self.archive_cfs.contains(&cf) {
            RowClass::Archive
        } else {
            RowClass::State
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowClass {
    Height,
    Archive,
    State,
    Keyed,
    Inexact,
}

/// A key and its value, as a prefix scan returns them.
pub type KeyValue = (Vec<u8>, Vec<u8>);

/// A state row as open reads it: its column family, key and value (`None` if absent).
pub type StateRead = (Cf, Vec<u8>, Option<Vec<u8>>);

/// A blind delete's range `[start, end)` in a column family.
pub type CfRange = (Cf, Vec<u8>, Vec<u8>);

/// One row write. Keyed rows may delete; the other classes only put.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    Put(Cf, Vec<u8>, Vec<u8>),
    Delete(Cf, Vec<u8>),
}

impl Row {
    pub fn cf(&self) -> Cf {
        match self {
            Row::Put(cf, ..) | Row::Delete(cf, _) => *cf,
        }
    }

    pub fn key(&self) -> &[u8] {
        match self {
            Row::Put(_, k, _) | Row::Delete(_, k) => k,
        }
    }

    /// Owned capacity (§2.9 item 2, G-R4-n1).
    pub fn bytes(&self) -> u64 {
        let owned = match self {
            Row::Put(_, k, v) => k.capacity() + v.capacity(),
            Row::Delete(_, k) => k.capacity(),
        };
        (owned + size_of::<Row>()) as u64
    }

    pub(crate) fn put_into<B: dashd_store::KvBatch>(&self, b: &mut B) {
        match self {
            Row::Put(cf, k, v) => b.put(*cf, k, v),
            Row::Delete(cf, k) => b.delete(*cf, k),
        }
    }
}

/// One block's height, keyed and inexact rows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockRows {
    pub rows: Vec<Row>,
}

/// A block as `connect` sees it: its data and undo, read only if the subscriber needs them (F8).
pub struct ConnectedBlock<'a> {
    pub block: BlockRef,
    pub data: Option<&'a Block>,
    pub undo: Option<&'a [u8]>,
}

/// A block as `stage_disconnected` sees it.
pub struct DisconnectedBlock<'a> {
    pub block: BlockRef,
    pub data: Option<&'a Block>,
    pub undo: Option<&'a [u8]>,
}

/// Block and undo data for delivered events (F8): the node's block files.
pub trait BlockSource: Send + Sync {
    /// # Errors
    ///
    /// [`SubscriberError::Blocks`] when the data is not readable, which F8 rules out for a
    /// delivered block.
    fn block(&self, b: &BlockRef) -> Result<Arc<Block>, SubscriberError>;

    /// The block's serialized undo (`CBlockUndo`).
    ///
    /// # Errors
    ///
    /// As [`block`](Self::block).
    fn undo(&self, b: &BlockRef) -> Result<Vec<u8>, SubscriberError>;
}

/// A source with no data: subscribers that need neither blocks nor undo.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoBlocks;

impl BlockSource for NoBlocks {
    fn block(&self, b: &BlockRef) -> Result<Arc<Block>, SubscriberError> {
        Err(SubscriberError::Blocks(format!("no block data for {}", b.hash)))
    }

    fn undo(&self, b: &BlockRef) -> Result<Vec<u8>, SubscriberError> {
        Err(SubscriberError::Blocks(format!("no undo data for {}", b.hash)))
    }
}

/// The data object (§2.9's `SubscriberData`). `connect` and `stage_disconnected` are pure: the
/// driver may compute an entry, not admit it, and compute it again later.
pub trait SubscriberData: Send {
    type E: KvEngine;
    /// Core's locator-committed state (coinstatsindex: MuHash and totals; blockfilterindex: the
    /// next file position), with any resource high-water mark. An inexact-only subscriber keeps
    /// one state, not one per block, so its state must not depend on the block.
    type State: Clone + Send;

    fn name(&self) -> &'static str;
    fn needs(&self) -> Needs;
    /// Awaited by step 6 and the gate, and counted in the prune locks.
    fn mandatory(&self) -> bool {
        true
    }
    fn start_height(&self) -> u32;
    fn schema(&self) -> &'static RowSchema;
    fn initial_state(&self) -> Self::State;

    /// b's rows and the state at b, from the state at b's parent (Core's `CustomAppend`).
    ///
    /// # Errors
    ///
    /// The data object's failure.
    fn connect(
        &self,
        b: &ConnectedBlock<'_>,
        at_parent: &Self::State,
    ) -> Result<(BlockRows, Self::State), SubscriberError>;

    /// The rows that undo b's keyed rows, and the state at b's parent, from b, its undo, the
    /// state at b and barrier-covered reads at heights ≤ h(L) (Core's `CustomRewind`). Never reads
    /// a key above L.
    ///
    /// # Errors
    ///
    /// The data object's failure, or a read's.
    fn stage_disconnected(
        &self,
        b: &DisconnectedBlock<'_>,
        at_b: &Self::State,
        below: &DurableBelow<'_, Self::E>,
    ) -> Result<(Vec<Row>, Self::State), SubscriberError>;

    /// Core's `CopyHeightIndexToHashIndex`: the archive rows for a block's height rows.
    fn archive(&self, height_rows: &[Row]) -> Vec<Row>;

    /// Resource high-water marks are monotone for the process (O-R2-M2): every state the
    /// driver moves to keeps `latest`'s.
    fn with_high_water(&self, at: &Self::State, _latest: &Self::State) -> Self::State {
        at.clone()
    }

    /// The high-water floor at open: `max(state's, file_len)` (§2.9 item 3).
    fn with_file_floor(&self, at: &Self::State, _file_len: u64) -> Self::State {
        at.clone()
    }

    /// The state's rows, keys ⊆ `schema().state_keys`.
    fn encode_state(&self, s: &Self::State) -> Vec<Row>;

    /// The state from its rows as read at open; may read the height row at L (Core's
    /// coinstatsindex `CustomInit`).
    ///
    /// # Errors
    ///
    /// The rows do not decode, or a read failed.
    fn decode_state(
        &self,
        rows: &[StateRead],
        at_l: &DurableBelow<'_, Self::E>,
    ) -> Result<Self::State, SubscriberError>;

    /// The blind-delete ranges `[start, end)` of heights `(lo_exclusive, hi_inclusive]`.
    fn height_ranges(&self, lo_exclusive: u32, hi_inclusive: u32) -> Vec<CfRange>;

    /// A bound on one journal entry's bytes for any consensus-valid block (G-R4-n1); 0 for an
    /// inexact-only subscriber.
    fn max_entry_bytes(&self) -> u64;

    /// The state's owned capacity, counted in its journal entry.
    fn state_bytes(&self, _s: &Self::State) -> u64 {
        size_of::<Self::State>() as u64
    }

    /// Syncs the subscriber's flat files, before an archive row or a locator references them.
    ///
    /// # Errors
    ///
    /// The sync failed.
    fn sync_files(&mut self) -> Result<(), SubscriberError> {
        Ok(())
    }

    /// The flat files' length at open.
    ///
    /// # Errors
    ///
    /// The length could not be read.
    fn file_len(&self) -> Result<u64, SubscriberError> {
        Ok(0)
    }
}

/// Barrier-covered reads only: rows at heights ≤ h(L) (§2.9).
pub struct DurableBelow<'a, E: KvEngine> {
    pub(crate) engine: &'a GuardedEngine<E>,
    pub(crate) max_height: u32,
    pub(crate) schema: &'static RowSchema,
}

/// A height row's key: the big-endian height, then the rest.
pub fn height_key(height: u32, rest: &[u8]) -> Vec<u8> {
    let mut k = Vec::with_capacity(4 + rest.len());
    k.extend_from_slice(&height.to_be_bytes());
    k.extend_from_slice(rest);
    k
}

impl<E: KvEngine> DurableBelow<'_, E> {
    fn check(&self, cf: Cf, height: u32) -> Result<(), SubscriberError> {
        if height > self.max_height || !self.schema.height_cfs.contains(&cf) {
            return Err(SubscriberError::Data(format!(
                "a read above L (height {height} > {}) or outside the height rows",
                self.max_height
            )));
        }
        Ok(())
    }

    /// The height row at `height` with key suffix `key_rest`.
    ///
    /// # Errors
    ///
    /// A read above L, or the store's.
    pub fn height_row(
        &self,
        cf: Cf,
        height: u32,
        key_rest: &[u8],
    ) -> Result<Option<Vec<u8>>, SubscriberError> {
        self.check(cf, height)?;
        self.engine.read(cf, &height_key(height, key_rest)).map_err(|_| self.fatal())
    }

    /// Every row at `height` (n4: archiving (F, L]).
    ///
    /// # Errors
    ///
    /// As [`height_row`](Self::height_row).
    pub fn height_rows(&self, cf: Cf, height: u32) -> Result<Vec<KeyValue>, SubscriberError> {
        self.check(cf, height)?;
        dashd_store::KvRead::iter_prefix(self.engine, cf, &height.to_be_bytes())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| self.fatal())
    }

    /// A point read in a keyed column family at or below L: the DB's row, which the journal
    /// does not shadow at a rewind (the journal is empty there).
    ///
    /// # Errors
    ///
    /// The store's.
    pub fn keyed(&self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>, SubscriberError> {
        if !self.schema.keyed_cfs.contains(&cf) {
            return Err(SubscriberError::Data("a keyed read outside the keyed rows".into()));
        }
        self.engine.read(cf, key).map_err(|_| self.fatal())
    }

    /// A failed read tripped the latch.
    fn fatal(&self) -> SubscriberError {
        match self.engine.latch().check() {
            Err(f) => SubscriberError::Fatal(f),
            Ok(()) => SubscriberError::Data("a subscriber read failed".into()),
        }
    }
}
