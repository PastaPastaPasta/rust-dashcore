//! A row store for the cold-cache tests: rows by id, a read counter and an injected read error.

use std::collections::HashMap;

use dash_consensus::BlockId;
use dash_primitives::{BlockHash, BlockHeader};

use crate::Work;
use crate::error::StoreError;
use crate::status::Status;
use crate::step::ChainMeta;
use crate::store::{HeaderRecord, HeaderStore, RecordKind};
use crate::sync::{AtomicBool, AtomicU64, Mutex, Ordering};

pub(super) fn record(k: u32, status: u32) -> HeaderRecord {
    let mut h = [0u8; 32];
    h[..4].copy_from_slice(&k.to_le_bytes());
    HeaderRecord {
        id: BlockId(k),
        hash: BlockHash::from(h),
        height: k,
        header: BlockHeader::default(),
        skip: None,
        status: Status(status),
        chain_work: Work::from(u64::from(k)),
        time_max: k,
        kind: RecordKind::Accepted,
    }
}

#[derive(Default)]
pub(super) struct RowStore {
    rows: Mutex<HashMap<BlockId, HeaderRecord>>,
    pub(super) reads: AtomicU64,
    pub(super) fail: AtomicBool,
}

impl RowStore {
    pub(super) fn with(rows: impl IntoIterator<Item = HeaderRecord>) -> RowStore {
        let s = RowStore::default();
        for r in rows {
            s.commit(r);
        }
        s
    }

    /// A committed row write (SB13's transaction).
    pub(super) fn commit(&self, r: HeaderRecord) {
        self.rows.lock().insert(r.id, r);
    }

    fn read(&self) -> Result<(), StoreError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        match self.fail.load(Ordering::SeqCst) {
            true => Err(StoreError::Io("injected".into())),
            false => Ok(()),
        }
    }
}

impl HeaderStore for RowStore {
    fn by_hash(&self, h: &BlockHash) -> Result<Option<HeaderRecord>, StoreError> {
        self.read()?;
        Ok(self.rows.lock().values().find(|r| r.hash == *h).cloned())
    }

    fn by_id(&self, id: BlockId) -> Result<Option<HeaderRecord>, StoreError> {
        self.read()?;
        Ok(self.rows.lock().get(&id).cloned())
    }

    fn active_at(&self, _: u32) -> Result<Option<BlockId>, StoreError> {
        unimplemented!("the cache reads rows only")
    }

    fn from_height(&self, _: u32) -> Result<Vec<HeaderRecord>, StoreError> {
        unimplemented!("the cache reads rows only")
    }

    fn leaves_from(&self, _: BlockId, _: &mut [(BlockId, Work)]) -> Result<usize, StoreError> {
        unimplemented!("the cache reads rows only")
    }

    fn children_from(
        &self,
        _: &BlockHash,
        _: BlockId,
        _: &mut [BlockId],
    ) -> Result<usize, StoreError> {
        unimplemented!("the cache reads rows only")
    }

    fn chain_meta(&self) -> Result<Option<ChainMeta>, StoreError> {
        unimplemented!("the cache reads rows only")
    }

    fn active_from(&self, _: u32) -> Result<Vec<(u32, BlockId)>, StoreError> {
        unimplemented!("the cache reads rows only")
    }

    fn max_id(&self) -> Result<Option<BlockId>, StoreError> {
        unimplemented!("the cache reads rows only")
    }
}
