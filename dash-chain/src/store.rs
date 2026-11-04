//! The SPV cold tier seam (SB0 §4.1, §9.5; SB4 §4). SB13 writes the SQLite store; SB4 has the
//! in-memory test double.

use dash_consensus::BlockId;
use dash_primitives::{BlockHash, BlockHeader};

use crate::Work;
use crate::error::StoreError;
use crate::status::Status;
use crate::step::ChainMeta;

/// What a header record is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RecordKind {
    /// An accepted header.
    Accepted,
    /// One of the 24 anchor-pack headers below the anchor: context for the rules only, never
    /// accepted (SB0 §4.1.1).
    Context,
}

/// A committed header with its index state, as the cold tier stores it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeaderRecord {
    pub id: BlockId,
    /// `X11(header)`, stored so that a cold read never re-hashes (SB5-A2, DEC-151).
    pub hash: BlockHash,
    pub height: u32,
    /// The 80-byte header; carries `prev_hash`.
    pub header: BlockHeader,
    /// Core's `pskip`.
    pub skip: Option<BlockId>,
    pub status: Status,
    pub chain_work: Work,
    /// Core's `nTimeMax`, needed when the record is a cold prev (SB4 §5.4, D35).
    pub time_max: u32,
    pub kind: RecordKind,
}

/// The SPV cold tier (SB0 §4.1). The node's store is [`NoStore`], whose methods are never
/// called. A row that does not decode is [`StoreError::Corrupt`]; a failed read or write is
/// [`StoreError::Io`].
pub trait HeaderStore: Send + Sync + 'static {
    fn by_hash(&self, h: &BlockHash) -> Result<Option<HeaderRecord>, StoreError>;
    fn by_id(&self, id: BlockId) -> Result<Option<HeaderRecord>, StoreError>;
    /// The live projection of the active chain (SB0 §4.4).
    fn active_at(&self, height: u32) -> Result<Option<BlockId>, StoreError>;
    /// Every branch from height `h` up; used by `open`.
    #[expect(clippy::wrong_self_convention, reason = "SB0 §4.1's name: a query, not a conversion")]
    fn from_height(&self, h: u32) -> Result<Vec<HeaderRecord>, StoreError>;
    /// Up to `out.len()` of the records with no accepted child (DEC-123 Q7), as
    /// `(id, chain_work)`, ids from `from` up in id order; the count written. The chain reads on
    /// from past the last id until a page is empty, so a short page is not the end. It pages
    /// through a fixed buffer, so a wide tree costs a step no memory (SB5 §6.2).
    fn leaves_from(&self, from: BlockId, out: &mut [(BlockId, Work)]) -> Result<usize, StoreError>;
    /// Up to `out.len()` of the ids of the records whose header's `prev_hash` is `prev`, from the
    /// same `prev` index (SB4 §3.7), from `from` up in id order; the count written. As for
    /// `leaves_from`, the end is an empty page.
    fn children_from(
        &self,
        prev: &BlockHash,
        from: BlockId,
        out: &mut [BlockId],
    ) -> Result<usize, StoreError>;
    /// The `meta` row (SB5 §7).
    fn chain_meta(&self) -> Result<Option<ChainMeta>, StoreError>;
    /// The `active` rows at heights `lo` and up, ascending (SB5 §3.7).
    fn active_from(&self, lo: u32) -> Result<Vec<(u32, BlockId)>, StoreError>;
    /// `n` records down the prev links from `from`, `from` first; fewer when a prev is not
    /// stored, none when `from` is not (SB5 §3.7). The default reads `from` by id, then each
    /// parent by hash; SB13 answers it with one recursive CTE.
    fn walk_down(&self, from: BlockId, n: u32) -> Result<Vec<HeaderRecord>, StoreError> {
        let mut out = Vec::new();
        let mut next = self.by_id(from)?;
        while let Some(rec) = next.filter(|_| out.len() < n as usize) {
            next = match out.len() + 1 < n as usize {
                true => self.by_hash(&rec.header.prev_hash)?,
                false => None,
            };
            out.push(rec);
        }
        Ok(out)
    }
    /// The largest stored id (SB5 n4).
    fn max_id(&self) -> Result<Option<BlockId>, StoreError>;
}

/// The node's store: the node keeps every entry resident and never reads a cold tier.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoStore;

impl HeaderStore for NoStore {
    fn by_hash(&self, _: &BlockHash) -> Result<Option<HeaderRecord>, StoreError> {
        Ok(None)
    }

    fn by_id(&self, _: BlockId) -> Result<Option<HeaderRecord>, StoreError> {
        Ok(None)
    }

    fn active_at(&self, _: u32) -> Result<Option<BlockId>, StoreError> {
        Ok(None)
    }

    fn from_height(&self, _: u32) -> Result<Vec<HeaderRecord>, StoreError> {
        Ok(Vec::new())
    }

    fn leaves_from(&self, _: BlockId, _: &mut [(BlockId, Work)]) -> Result<usize, StoreError> {
        Ok(0)
    }

    fn children_from(
        &self,
        _: &BlockHash,
        _: BlockId,
        _: &mut [BlockId],
    ) -> Result<usize, StoreError> {
        Ok(0)
    }

    fn chain_meta(&self) -> Result<Option<ChainMeta>, StoreError> {
        Ok(None)
    }

    fn active_from(&self, _: u32) -> Result<Vec<(u32, BlockId)>, StoreError> {
        Ok(Vec::new())
    }

    fn max_id(&self) -> Result<Option<BlockId>, StoreError> {
        Ok(None)
    }
}
