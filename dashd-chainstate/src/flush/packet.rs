//! What V hands W at a freeze (CH2 §2.7, §4.1).

use std::any::Any;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use dash_chain::{BlockRef, IndexWrite};
use dashd_store::KvBatch;

use super::control::FlushIndex;
use super::machine::block_index_cf;
use super::policy::{FlushCauses, FlushStateMode};
use super::prune::PruneDecision;
use crate::coins::FrozenLayer;

/// The window's evo state, which B1 writes beside the coins' best block (CH7a's
/// [`EvoWindowDelta`](crate::evo::EvoWindowDelta); `()` without evo state).
pub trait StateDelta: Send + 'static {
    /// Whether B1 carries evo rows, so a full flush prints Core's "write evodb cache to disk"
    /// pair around B1's durable write (CH7 D20).
    const EVO: bool = false;

    /// Writes the window's merged entries into B1 (CH7 D22): a put per live entry, a delete per
    /// tombstone.
    fn write_b1<B: KvBatch>(&self, batch: &mut B);

    /// Core's `m_evoDb.GetMemoryUsage()` analogue: `EVO_CRITICAL`'s measure. At least the key and
    /// value bytes [`write_b1`](Self::write_b1) puts: B1's batch is sized from it (BL-511).
    fn bytes(&self) -> u64;
}

impl StateDelta for () {
    fn write_b1<B: KvBatch>(&self, _: &mut B) {}

    fn bytes(&self) -> u64 {
        0
    }
}

/// The presumed-effect ledger (CH2 §4.1): V bumps it once a presumed effect's write has returned,
/// and a freeze captures it, so B1's barrier covers every effect written for the blocks it
/// flushes.
#[derive(Debug, Default)]
pub struct EffectLedger {
    seq: AtomicU64,
}

impl EffectLedger {
    /// Records a presumed effect whose write has returned; its sequence number.
    pub fn record_presumed(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// The last sequence number recorded.
    pub fn current(&self) -> u64 {
        self.seq.load(Ordering::Acquire)
    }
}

/// What V hands W at a freeze. Built on V at a block boundary, immutable afterwards.
pub struct FreezePacket<D: StateDelta> {
    /// The flush's number: one per freeze, increasing.
    pub flush_id: u64,
    /// F, whose tip is the flush's `new`: the coins' best block at the freeze.
    pub coins: Arc<FrozenLayer>,
    /// The previous durable tip: the marker's `old` in a split flush.
    pub old: Option<BlockRef>,
    pub delta: D,
    /// Every dirty block-index record at the freeze, in B1's batch.
    pub index: FrozenIndex,
    /// The [`EffectLedger`] at the freeze.
    pub presumed_effects_seq: u64,
    /// The `seq` of the tip step that made `new` V's tip (CH2 §4.4.1 rule 1; E-b1.2).
    pub tip_seq: u64,
    pub causes: FlushCauses,
    pub mode: FlushStateMode,
    /// Step 0's dry run, kept for step 7 (CH2b b2).
    pub prune: Option<PruneDecision>,
    /// When V decided the flush: `last_write`'s value once B1 has returned (Core sets
    /// `m_last_write` to the decision's `nNow`).
    pub decided_at: Instant,
    /// An RPC copy point's ALWAYS flush (§2.6): step 11's hook runs before the release, and V
    /// takes the RPC's snapshot once it observes the release, before anything else is written
    /// (E-b1.8).
    pub copy_point: bool,
}

impl<D: StateDelta> FreezePacket<D> {
    /// The block the flush makes durable: F's tip.
    pub fn new_tip(&self) -> BlockRef {
        self.coins.tip()
    }

    /// F's release generation: what W reports in `FlushDone`.
    pub fn generation(&self) -> u64 {
        self.coins.generation()
    }

    /// Whether this flush prunes: step 0 selected files (`fFlushForPrune`, `validation.cpp:3020`).
    pub fn flush_for_prune(&self) -> bool {
        self.prune.as_ref().is_some_and(|p| !p.files.is_empty())
    }
}

/// The dirty block-index records at a freeze or an index-only decision, put by V into the
/// engine's batch that B1 starts from (CH2c D40). Taken as they are encoded
/// (`HeaderChain::take_dirty_into`), they never exist apart from that batch, whose bytes are
/// those B1 built from the records before: the records first, in `take_dirty`'s order.
///
/// The batch is the coins' engine's, and W writes it through that same engine: another engine's
/// batch type fails B1, and a RocksDB batch keeps its database open until it is written or
/// dropped.
pub struct FrozenIndex {
    /// The engine's batch, behind `Any`: the packet does not name the engine.
    batch: Box<dyn Any + Send>,
    /// What B1 can come to, its infos aside, when the index could bound its records: B1's guard
    /// checks it in debug builds (BL-511).
    bound: Option<usize>,
    /// The records again, for the planted bugs whose B1 drops or reads some (T19, T20, T21, and
    /// a split T3, T4 or T24's B1 without the evo state).
    #[cfg(feature = "planted-bugs")]
    copy: Vec<IndexWrite>,
}

impl FrozenIndex {
    /// Takes `index`'s dirty records into `batch`, B1's: an empty one, made with the smaller of R
    /// and `bound` when that is set, else with R (BL-511).
    pub fn take<B: KvBatch + 'static, I: FlushIndex + ?Sized>(
        mut batch: B,
        index: &mut I,
        bound: Option<usize>,
    ) -> Self {
        debug_assert!(batch.is_empty(), "a fresh batch");
        #[cfg(feature = "planted-bugs")]
        let mut copy = Vec::new();
        let cf = block_index_cf();
        index.take_dirty_into(&mut |hash, bytes| {
            batch.put(cf, hash.as_bytes(), bytes);
            #[cfg(feature = "planted-bugs")]
            copy.push(IndexWrite {
                hash: *hash,
                bytes: bytes.to_vec(),
            });
        });
        Self {
            batch: Box::new(batch),
            bound,
            #[cfg(feature = "planted-bugs")]
            copy,
        }
    }

    /// The batch, holding the records, its bound if it was sized to one, and with planted bugs
    /// the records' copy (else empty).
    ///
    /// # Panics
    ///
    /// If the batch is not a `B`: the packet was built for another engine.
    pub(super) fn into_parts<B: KvBatch + 'static>(self) -> (B, Option<usize>, Vec<IndexWrite>) {
        let batch = *self.batch.downcast().expect("B1's batch is the engine's");
        #[cfg(feature = "planted-bugs")]
        return (batch, self.bound, self.copy);
        #[cfg(not(feature = "planted-bugs"))]
        (batch, self.bound, Vec::new())
    }
}

impl fmt::Debug for FrozenIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrozenIndex").finish_non_exhaustive()
    }
}
