//! The SPV header step's write set and the `hdr` row codec (SB5 §3.7, §7; SB0 §9.5). SB4's
//! stage 3 streams [`StepRow`]s into a [`HeaderStepSink`] and ends with one [`StepTail`]; SB13's
//! SQLite store and the test-util `MemHeaderStore` execute them. Stage 4 returns a [`TipSummary`].

use std::sync::Arc;

use dash_consensus::BlockId;
use dash_consensus::primitives::header_bytes;
use dash_primitives::{BlockHash, BlockHeader};
use dash_types::Numeric as _;
use dash_types::codec::BaseCodec as _;

use crate::Work;
use crate::chainlock::ClPersisted;
use crate::error::StoreError;
use crate::snapshot::ChainReadSnapshot;
use crate::status::Status;
use crate::store::{HeaderRecord, RecordKind};
use crate::tip::BlockRef;

/// `meta.schema` of the SPV header tables (§7).
pub const CHAIN_SCHEMA: u32 = 3;

/// Whether the anchor may still be replaced (SB0 §4.3.1, DEC-107). `meta.anchor_state` stores the
/// discriminant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum AnchorState {
    /// Chosen during initial sync; a fallback may clear the header tables and replace it.
    Candidate = 0,
    /// Set by one Durable transaction before any MN, lock or subscriber write; never replaced.
    Established = 1,
}

/// A store commit's durability (SB0 §9.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Durability {
    /// Atomic and survives a process kill; a power loss may lose a suffix of these commits.
    Buffered,
    /// Survives power loss, and so does every earlier commit.
    Durable,
}

/// The `meta` table (§7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainMeta {
    pub schema: u32,
    /// The anchor's hash and height.
    pub anchor: (BlockHash, u32),
    pub anchor_state: AnchorState,
    /// The next id to assign; `open` checks it is above every stored id (n4).
    pub next_id: BlockId,
    pub tip: BlockId,
    /// Diagnostic only: `open` recomputes it (D17, E5).
    pub best_header: BlockId,
    /// Incremented by each `sync_wal` that had a newer Buffered commit to cover (SB0 §9.4).
    pub sync_seq: u64,
    pub cl: ClPersisted,
}

/// One row of the header step's write set, streamed in batches (DEC-155 (2)).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StepRow {
    /// A record inserted in this step.
    Record(HeaderRecord),
    /// A status-only change: each overlay mark (SB5-A13).
    Status(BlockId, Status),
    /// `hdr_leaf` gains an accepted record. Adding an existing leaf or a context record is an
    /// error.
    LeafAdd(BlockId),
    /// `hdr_leaf` loses an id; a no-op when the id is not a leaf, as SQL's `DELETE`.
    LeafRemove(BlockId),
}

/// The sink of a step's rows. All of a step's batches run inside one store transaction that the
/// sink opened for the step, and none is visible before the step's commit. An error may come
/// from `rows` (a UNIQUE or I/O failure mid-stream) or from the commit; either abandons the step,
/// whose transaction rolls back, and the caller trips the fail-stop latch.
pub trait HeaderStepSink {
    /// Executes `batch` in the open step transaction (SQLite: one `BEGIN IMMEDIATE … COMMIT` for
    /// the whole step). Called again whenever the producer's buffer fills; never sees a partial
    /// row. Runs synchronously on the header writer's thread (SB0 E8: the leased write
    /// connection).
    fn rows(&mut self, batch: &[StepRow]) -> Result<(), StoreError>;
}

/// The step's O(1) tail, executed after the rows, in field order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StepTail {
    /// The compact `active` change, when the tip moved.
    pub active: Option<ActiveDiff>,
    /// The `locks` rows and the observed spork state, when the step changed them.
    pub cl: Option<ClPersisted>,
    pub meta: MetaDelta,
}

/// The `active` rows after the step: heights `from_height ..= new_height` are rewritten by
/// walking prev from `new_tip` (SQLite: one recursive CTE); heights above `new_height` are
/// deleted. `old_height` is the active height before the step. With rows below, the walk ends
/// at the fork's, `from_height − 1`; a store with none (the first step) gets every accepted
/// height from the anchor, or genesis at 0, up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActiveDiff {
    pub from_height: u32,
    pub new_tip: BlockId,
    pub new_height: u32,
    pub old_height: u32,
}

/// The step's `meta` fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MetaDelta {
    pub next_id: BlockId,
    pub tip: BlockId,
    pub best_header: BlockId,
}

/// One `hdr` row (§7), column for column. SQLite stores the integers as `INTEGER` and the arrays
/// as `BLOB`s.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeaderRow {
    pub hash: [u8; 32],
    pub id: u32,
    pub height: u32,
    pub header: [u8; 80],
    /// NULL when the record has no skip.
    pub skip: Option<u32>,
    /// Core's `nStatus` bits.
    pub status: u32,
    /// Big-endian.
    pub chain_work: [u8; 32],
    pub time_max: u32,
    /// 0 accepted, 1 context.
    pub kind: u8,
}

/// A stored row that is not a record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RowError {
    #[error("hdr.kind {0} is neither 0 (accepted) nor 1 (context)")]
    Kind(u8),
    #[error("hdr.header does not decode")]
    Header,
}

impl HeaderRecord {
    /// The record's `hdr` row (§7, P3).
    pub fn to_row(&self) -> HeaderRow {
        HeaderRow {
            hash: *self.hash.as_bytes(),
            id: self.id.0,
            height: self.height,
            header: header_bytes(&self.header),
            skip: self.skip.map(|s| s.0),
            status: self.status.bits(),
            chain_work: self.chain_work.to_bendian(),
            time_max: self.time_max,
            kind: match self.kind {
                RecordKind::Accepted => 0,
                RecordKind::Context => 1,
            },
        }
    }

    /// The record a `hdr` row holds. The hash is taken as stored, never recomputed (SB5-A2).
    pub fn from_row(row: &HeaderRow) -> Result<HeaderRecord, RowError> {
        let kind = match row.kind {
            0 => RecordKind::Accepted,
            1 => RecordKind::Context,
            k => return Err(RowError::Kind(k)),
        };
        let header = BlockHeader::decode(&mut &row.header[..]).map_err(|_| RowError::Header)?;
        Ok(HeaderRecord {
            id: BlockId(row.id),
            hash: BlockHash::from(row.hash),
            height: row.height,
            header,
            skip: row.skip.map(BlockId),
            status: Status(row.status),
            chain_work: Work::from_bendian(row.chain_work),
            time_max: row.time_max,
            kind,
        })
    }

    /// The rows that insert this new record, with its `hdr_leaf` deltas (§3.7): an accepted
    /// record becomes a leaf and its parent, `prev`, stops being one. The parent's removal is
    /// unconditional, since removing a non-leaf is a no-op.
    pub fn step_rows(self, prev: Option<BlockId>) -> impl Iterator<Item = StepRow> {
        let leaf = (self.kind == RecordKind::Accepted).then_some(self.id);
        let (add, remove) = (leaf.map(StepRow::LeafAdd), leaf.and(prev).map(StepRow::LeafRemove));
        [Some(StepRow::Record(self)), add, remove].into_iter().flatten()
    }
}

/// What SPV stage 4, [`HeaderChain::activate_segmented`](crate::HeaderChain::activate_segmented),
/// did, in O(1): no leg vector outlives its sub-step.
#[derive(Clone, Debug)]
pub struct TipSummary {
    /// The last common ancestor of the old tip and the target.
    pub fork: BlockRef,
    pub disconnected: u32,
    pub connected: u32,
    /// Each a pure disconnect or connect run of at most `segment_events` steps (§4.8, m10).
    pub sub_steps: u32,
    /// The release's snapshot; its `tip_seq` is the last step's.
    pub snap: Arc<ChainReadSnapshot>,
}

#[cfg(all(test, not(loom)))]
mod tests;
