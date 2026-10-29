//! A test stand-in for SB5's `write_header_step` (SB4 C9b): stage 3 of an SPV step into a
//! [`MemHeaderStore`], so SB4's full-chain and memory tests (A-4, A-31) can run windowed chains
//! from outside the crate before SB5 merges. It writes the step's new records and overlay marks,
//! then the tail; the `active` rows are SB5's and stay empty.

use super::HeaderChain;
use crate::error::StoreError;
use crate::mem_store::MemHeaderStore;
use crate::residency::Evicting;
use crate::step::{Durability, HeaderStepSink as _, MetaDelta, StepRow, StepTail};
use crate::store::{HeaderRecord, HeaderStore as _, RecordKind};
use dash_consensus::BlockId;

impl HeaderChain<MemHeaderStore, Evicting> {
    /// Stage 3 (§9.1) of the step `accept` and the ChainLock calls since the last
    /// [`records_committed`](HeaderChain::records_committed) built, in one store step. Call it
    /// before `set_active_tip`; the tail records the tip as it was.
    pub fn commit_mem_step(&self, store: &mut MemHeaderStore) -> Result<(), StoreError> {
        store.begin_step();
        let mut rows = Vec::new();
        // A dirty id already stored is a status change: its row is the overlay's.
        for &id in self.dirty.ids() {
            if store.by_id(id)?.is_some() {
                continue;
            }
            let (rec, prev) = self
                .shared
                .arena
                .with(id, |e, entry| {
                    let rec = HeaderRecord {
                        id,
                        hash: e.hash,
                        height: e.height,
                        header: e.header,
                        skip: e.skip,
                        status: entry.group.read_own().status,
                        chain_work: e.chain_work,
                        time_max: e.time_max,
                        kind: match e.context {
                            true => RecordKind::Context,
                            false => RecordKind::Accepted,
                        },
                    };
                    (rec, e.prev)
                })
                .expect("a new entry is resident until its step commits");
            rows.extend(rec.step_rows(prev));
        }
        rows.extend(self.overlay_marks().map(|(id, s)| StepRow::Status(id, s)));
        store.rows(&rows)?;
        let tip = self.private_tip().expect("a tip").0.id;
        let tail = StepTail {
            active: None,
            cl: None,
            meta: MetaDelta {
                next_id: BlockId(self.next_id),
                tip,
                best_header: self.best_header(),
            },
        };
        store.commit_step(&tail, Durability::Buffered)
    }
}
