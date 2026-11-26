//! The SPV store as the header chain's [`HeaderStore`]. The chain calls it inline, so every
//! chain call runs in one of the store's blocking jobs ([`StoreHandle::job`]).

use std::future::Future;
use std::sync::Arc;

use dash_chain::{BlockId, ChainMeta, HeaderRecord, HeaderStore, StoreError, Work};
use dash_consensus::Header;
use dash_kv::KvEngine;
use dash_primitives::BlockHash;

use crate::error::{SpvError, StorageError};
use crate::storage::store::hdr::io::HdrIo;
use crate::storage::store::Store;
use crate::storage::SpvStore;

/// A shared handle to one `Store`: the client's `SpvStore`, or a test's store.
pub trait StoreHandle: Clone + Send + Sync + 'static {
    type Engine: KvEngine + 'static;
    type Io: HdrIo;

    fn store(&self) -> &Store<Self::Engine, Self::Io>;

    /// Runs `f` on the blocking pool as one of the store's jobs, where the store counts them. A
    /// job refused, by a stop or by a runtime shutting down, never ran: `ShuttingDown`. No job's
    /// join or abort handle is ever handed out, so a cancelled job only ever means a runtime
    /// shutdown.
    fn job<T: Send + 'static>(
        &self,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> impl Future<Output = Result<T, SpvError>> + Send;
}

impl StoreHandle for SpvStore {
    type Engine = dash_kv_sqlite::SqliteEngine;
    type Io = crate::storage::store::hdr::io::OsIo;

    fn store(&self) -> &Store<Self::Engine, Self::Io> {
        self.inner()
    }

    fn job<T: Send + 'static>(
        &self,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> impl Future<Output = Result<T, SpvError>> + Send {
        let store = self.clone();
        async move { Ok(store.run(move |_| Ok(f())).await?) }
    }
}

impl<E: KvEngine + 'static, F: HdrIo> StoreHandle for Arc<Store<E, F>> {
    type Engine = E;
    type Io = F;

    fn store(&self) -> &Store<E, F> {
        self
    }

    async fn job<T: Send + 'static>(
        &self,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, SpvError> {
        tokio::task::spawn_blocking(f).await.map_err(|e| {
            if e.is_cancelled() {
                SpvError::Storage(StorageError::ShuttingDown)
            } else {
                SpvError::ChannelFailure("store job".into(), e.to_string())
            }
        })
    }
}

/// The chain's store: the handle's `Store`, whose reads re-hash every record they return.
#[derive(Clone)]
pub struct SpvChainStore<H>(pub H);

/// `rec`, if its header hashes to the hash stored with it: no stored pair is served unchecked.
pub(super) fn rehashed(rec: HeaderRecord) -> Result<HeaderRecord, StoreError> {
    let computed = Header::new(rec.header).hash();
    if computed != rec.hash {
        return Err(StoreError::Corrupt(format!(
            "header {:?} at {} is stored as {:?} but hashes to {computed:?}",
            rec.id, rec.height, rec.hash
        )));
    }
    Ok(rec)
}

fn all_rehashed(recs: Vec<HeaderRecord>) -> Result<Vec<HeaderRecord>, StoreError> {
    recs.into_iter().map(rehashed).collect()
}

impl<H: StoreHandle> HeaderStore for SpvChainStore<H> {
    fn by_hash(&self, h: &BlockHash) -> Result<Option<HeaderRecord>, StoreError> {
        self.0.store().by_hash(h)?.map(rehashed).transpose()
    }

    fn by_id(&self, id: BlockId) -> Result<Option<HeaderRecord>, StoreError> {
        self.0.store().by_id(id)?.map(rehashed).transpose()
    }

    fn active_at(&self, height: u32) -> Result<Option<BlockId>, StoreError> {
        self.0.store().active_at(height)
    }

    fn from_height(&self, h: u32) -> Result<Vec<HeaderRecord>, StoreError> {
        all_rehashed(self.0.store().from_height(h)?)
    }

    fn leaves_from(&self, from: BlockId, out: &mut [(BlockId, Work)]) -> Result<usize, StoreError> {
        self.0.store().leaves_from(from, out)
    }

    fn children_from(
        &self,
        prev: &BlockHash,
        from: BlockId,
        out: &mut [BlockId],
    ) -> Result<usize, StoreError> {
        self.0.store().children_from(prev, from, out)
    }

    fn chain_meta(&self) -> Result<Option<ChainMeta>, StoreError> {
        self.0.store().chain_meta()
    }

    fn active_from(&self, lo: u32) -> Result<Vec<(u32, BlockId)>, StoreError> {
        self.0.store().active_from(lo)
    }

    fn walk_down(&self, from: BlockId, n: u32) -> Result<Vec<HeaderRecord>, StoreError> {
        all_rehashed(self.0.store().walk_down(from, n)?)
    }

    fn max_id(&self) -> Result<Option<BlockId>, StoreError> {
        self.0.store().max_id()
    }

    fn min_height(&self) -> Result<Option<u32>, StoreError> {
        self.0.store().min_height()
    }
}
