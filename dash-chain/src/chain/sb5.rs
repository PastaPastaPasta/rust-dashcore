//! SB5's half of the writer (SB5 §3.7, §8): the SPV header step's write set (stage 3), and its
//! tests over SB4's SPV chain.

use dash_consensus::BlockId;

use super::HeaderChain;
use crate::config::ChainMode;
use crate::error::{ChainError, StoreError};
use crate::residency::Residency;
use crate::step::{ActiveDiff, HeaderStepSink, MetaDelta, StepRow, StepTail};
use crate::store::{HeaderRecord, HeaderStore, RecordKind};
use crate::tip::BlockRef;

impl<S: HeaderStore, R: Residency> HeaderChain<S, R> {
    /// SPV stage 3 (SB4 §9.1): streams the step's write set into `sink`, in batches of at most
    /// `write_batch` rows from one buffer, inside the store transaction the sink holds open, and
    /// returns the O(1) tail (§3.7). The rows are each new record's (with its `hdr_leaf`
    /// deltas), then each overlay mark's status. `target` is where stage 4 moves the tip,
    /// `None` when it stays; the `active` change is `tip_target`'s, so no plan is built.
    ///
    /// The new records are the dirty ids above the store's largest, which must be every id
    /// issued since: SPV's other dirty ids are marks on stored records, whose rows are the
    /// overlay's (SB5-A13). The dirty set and the overlay are read, not drained; stage 5 drains
    /// them after the commit. A failed probe or write trips the latch; a failed commit is the
    /// caller's to trip.
    pub fn write_header_step(
        &mut self,
        target: Option<BlockRef>,
        sink: &mut dyn HeaderStepSink,
    ) -> Result<StepTail, ChainError> {
        self.require_mode(ChainMode::Spv, "write_header_step")?;
        self.refuse_in_hold("write_header_step inside a hold")?;
        let tip = self.private_tip().ok_or(ChainError::Internal("write_header_step: no tip"))?.0;
        let fail = |e: StoreError| {
            self.shared.fail.read_failed(&e.to_string());
            e
        };
        let stored = self.shared.store.max_id().map_err(fail)?;
        let first_new = stored.map_or(0, |m| m.0 + 1);
        let new = self.dirty.ids().iter().filter(|id| id.0 >= first_new);
        if self.next_id < first_new || new.clone().count() != (self.next_id - first_new) as usize {
            return Err(ChainError::Internal("write_header_step: the new ids are not dense"));
        }
        let active = self.step_active(tip, target, stored.is_none())?;
        let cap = self.cfg.feeds.write_batch as usize;
        let mut out = RowBuffer {
            rows: Vec::with_capacity(cap),
            cap,
            sink,
        };
        for &id in new {
            let (rec, prev) = self.new_record(id)?;
            for row in rec.step_rows(prev) {
                out.push(row).map_err(fail)?;
            }
        }
        for (id, status) in self.overlay_marks() {
            out.push(StepRow::Status(id, status)).map_err(fail)?;
        }
        out.flush().map_err(fail)?;
        Ok(StepTail {
            active,
            // The ChainLock rows and observed spork state, written with every step: O(1), and
            // the writer keeps no committed copy to compare against.
            cl: Some(self.cl_state()),
            meta: MetaDelta {
                next_id: BlockId(self.next_id),
                tip: target.map_or(tip.id, |t| t.id),
                best_header: self.best_header(),
            },
        })
    }

    /// The `active` change stage 4 makes, from `tip_target` (O(1), SB5-A11 (c)): `None` when
    /// the tip stays. A fresh store's first step writes every accepted active height up to the
    /// new tip, from the anchor or genesis (context records are never `active` rows, D44).
    fn step_active(
        &self,
        tip: BlockRef,
        target: Option<BlockRef>,
        fresh: bool,
    ) -> Result<Option<ActiveDiff>, ChainError> {
        let end = target.unwrap_or(tip);
        if let Some(p) = self.pending_plan.lock().as_ref() {
            debug_assert_eq!(p.connect.last().unwrap_or(&p.fork).id, end.id, "stage 1's plan");
        }
        if end.id == tip.id && !fresh {
            return Ok(None);
        }
        let t = self.tip_target(end.id)?;
        if t.fork.height + t.c != end.height {
            return Err(ChainError::Internal("write_header_step: the target's height"));
        }
        let from_height = match fresh {
            false => t.fork.height + 1,
            true => {
                let accepted = |h| self.shared.arena.imm(self.active.get(h)?, |e| !e.context);
                (self.floor..=tip.height)
                    .find(|&h| accepted(h) == Some(true))
                    .ok_or(ChainError::Internal("write_header_step: no accepted active entry"))?
            }
        };
        Ok(Some(ActiveDiff {
            from_height,
            new_tip: end.id,
            new_height: end.height,
            old_height: if fresh {
                0
            } else {
                tip.height
            },
        }))
    }

    /// A new entry's record and its `prev`: new entries stay resident until their step
    /// commits.
    fn new_record(&self, id: BlockId) -> Result<(HeaderRecord, Option<BlockId>), ChainError> {
        self.shared
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
            .ok_or(ChainError::Internal("write_header_step: a new entry is not resident"))
    }
}

/// The step's one row buffer: full batches go to the sink as they fill.
struct RowBuffer<'s> {
    rows: Vec<StepRow>,
    cap: usize,
    sink: &'s mut dyn HeaderStepSink,
}

impl RowBuffer<'_> {
    fn push(&mut self, row: StepRow) -> Result<(), StoreError> {
        self.rows.push(row);
        match self.rows.len() == self.cap {
            true => self.flush(),
            false => Ok(()),
        }
    }

    fn flush(&mut self) -> Result<(), StoreError> {
        if !self.rows.is_empty() {
            self.sink.rows(&self.rows)?;
            self.rows.clear();
        }
        Ok(())
    }
}

mod hold;
pub(super) use hold::HoldGuard;
mod open;
mod segmented;

#[cfg(all(test, not(loom)))]
mod a23_tests;
#[cfg(all(test, not(loom)))]
mod cold_tests;
#[cfg(all(test, not(loom)))]
mod enforce_feed_tests;
#[cfg(all(test, not(loom)))]
mod f8_tests;
#[cfg(all(test, not(loom)))]
mod hold_tests;
#[cfg(all(test, not(loom)))]
mod hold_v_tests;
#[cfg(all(test, not(loom)))]
mod lo9_tests;
#[cfg(all(test, not(loom)))]
mod p2_tests;
#[cfg(all(test, not(loom)))]
mod paged_tests;
#[cfg(all(test, not(loom)))]
mod segmented_tests;
#[cfg(all(test, not(loom)))]
mod step_tests;
#[cfg(all(test, not(loom)))]
mod w_tests;

#[cfg(all(feature = "test-util", not(loom)))]
impl<S: HeaderStore, R: Residency> HeaderChain<S, R> {
    /// §6.2's fixed term, for A-F12: one release's publication with both of its containers new.
    /// The status facts are reset first, and the old snapshot and facts stay held, as readers
    /// may hold them; `measure` runs the publication alone.
    pub(crate) fn republish(&self, measure: impl FnOnce(&mut dyn FnMut())) {
        use crate::feed::Fanout;
        use crate::status_feed::StatusFacts;
        let cell = self.shared.readers.tip();
        let mut st = cell.state.lock();
        st.feeds.status = crate::feed::StatusKey::UNSET;
        cell.shared.status.publish(StatusFacts::default());
        let _held = (cell.published.load_full(), cell.shared.status.facts());
        measure(&mut || {
            self.publish_locked(&mut st, Fanout::None);
        });
    }
}
