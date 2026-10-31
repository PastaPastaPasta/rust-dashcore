//! `RocksEvoStore` (CH7 §2.1): the evo column families of the chainstate database, under the evo
//! layer. V is their only writer (A1 §2.1 rule 4): the transactional rows through the layer and
//! B1, the effect rows (`llmq_snapshot`) straight to the DB (§6.2).

use std::sync::Arc;

use dash_consensus::ChainAncestry;
use dash_dml::{EvoDelta, StoreError};
use dash_evo::llmq::QuorumSnapshot;
use dash_primitives::{BlockHash, LlmqType};
use dashd_store::{Commit, KvBatch, KvEngine};

use super::codec::{self, EvoCodec};
use super::layer::{EvoLayer, db_error};
use super::view::EvoReadView;
use crate::store::{CoinsReadError, GuardedEngine, Meta};

/// The evo store over the chainstate database. Its reads go through an [`EvoReadView`], which
/// pins the layer's indexes with a DB snapshot.
pub struct RocksEvoStore<E: KvEngine> {
    db: Arc<GuardedEngine<E>>,
    codec: EvoCodec,
    /// The network's genesis block, whose evo best block reads as absent (D7).
    genesis: BlockHash,
}

impl<E: KvEngine> RocksEvoStore<E> {
    pub fn new(db: Arc<GuardedEngine<E>>, codec: EvoCodec, genesis: BlockHash) -> Self {
        Self {
            db,
            codec,
            genesis,
        }
    }

    pub fn db(&self) -> &GuardedEngine<E> {
        &self.db
    }

    pub fn codec(&self) -> &EvoCodec {
        &self.codec
    }

    /// `commit_tx`'s merge (D1, D22): `delta`'s rows into `layer`'s active index, checked against
    /// the layer and the DB as Core's `WriteDerived` checks them.
    ///
    /// # Errors
    ///
    /// [`StoreError::Inconsistent`] for a delta that disagrees with itself or a differing rewrite,
    /// and a DB read's fault; `layer` is then unchanged.
    pub fn commit(&self, layer: &mut EvoLayer, delta: &EvoDelta) -> Result<(), StoreError> {
        layer.commit(self.codec.rows(delta)?, &*self.db)
    }

    /// The layer an open starts from (D37): over the raw durable `meta.evo_tip`, read now, so the
    /// first freeze is checked as every later one. Call it after recovery and any wipe: a layer
    /// resumed before a wipe would refuse the flush at genesis that follows. The tip is the row
    /// as written, a genesis one included; only reads normalise it (D38).
    ///
    /// # Errors
    ///
    /// The meta row's read fault or corruption.
    pub fn resume_layer(&self) -> Result<EvoLayer, CoinsReadError> {
        Ok(EvoLayer::resume(Meta::read(&*self.db)?.evo_tip))
    }

    /// The evo state as of now, read through `chain` (D17, D22).
    pub fn view<'a>(
        &'a self,
        layer: &EvoLayer,
        chain: &'a dyn ChainAncestry,
    ) -> EvoReadView<'a, E> {
        EvoReadView::new(chain, layer.indexes(), self.db.snapshot(), &self.codec, self.genesis)
    }

    // Core: llmq/snapshot.cpp:322 CQuorumSnapshotManager::StoreSnapshotForBlock
    /// `StoreSnapshotForBlock`: the rotation snapshot row, overwritten unconditionally and never
    /// erased (LQ1 §3.7.2), as a Buffered single-row batch: a presumed effect, whose WAL sync the
    /// next B1 waits for (§6.2, DEC-202).
    ///
    /// # Errors
    ///
    /// The write's fault; the guarded engine has tripped the latch.
    pub fn store_quorum_snapshot(
        &self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
        snapshot: &QuorumSnapshot,
    ) -> Result<(), StoreError> {
        let mut b = self.db.batch();
        b.put(
            codec::cf("llmq_snapshot"),
            &codec::quorum_snapshot_key(llmq_type, cycle_base),
            &codec::encode_quorum_snapshot(snapshot),
        );
        self.db.write(b, Commit::Buffered).map_err(db_error)
    }
}
