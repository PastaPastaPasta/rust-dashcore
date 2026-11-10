//! The header step's `WriteSet` (SB0 §9.5, SB5 §3.7) on SB14's layout: dash-chain's streamed
//! [`StepRow`]s, then its [`StepTail`], under the writer mutex from the step's start (D16).
//!
//! The rows are checked as they arrive, against the store and the step's own earlier rows, with
//! `MemHeaderStore`'s contract: `UNIQUE` hash and id, a status only for a stored record, a leaf
//! only for an accepted record and only once. The tail's `ActiveDiff` is checked as there too
//! (D44: `from_height` is the lowest rewritten height), all before anything is written.
//!
//! The commit is then SB14's step path: a lowering to `from_height − 1` if the diff rewrites
//! active heights (D7, its own batches), the new active records written into the files and
//! synced (raise steps 1–3), and one SQLite commit carrying the slot, the step's off-chain rows,
//! override rows (D21), leaves, `locks`, `meta` and the runs. A dropped or refused step writes
//! nothing; a lowering's batches are each a valid prefix (§3).

use std::collections::HashMap;
use std::sync::MutexGuard;

use dash_chain::{
    ActiveDiff, BlockId, ChainMeta, Durability, HeaderRow, HeaderStepSink, StepRow, StepTail,
};
use dash_kv::{Commit, KvBatch, KvEngine};

use super::hdr::io::HdrIo;
use super::lower::Activate;
use super::schema::*;
use super::{kv, lock, put_meta, put_runs, refused, Bounds, HdrWriter, Result, Store};

/// An open header step: the writer mutex, held until [`commit`](Self::commit) or drop.
pub struct HeaderStep<'s, E: KvEngine, F: HdrIo> {
    store: &'s Store<E, F>,
    hw: MutexGuard<'s, HdrWriter<F>>,
    /// The rows this step inserted or changed, as they will be committed.
    rows: HashMap<BlockId, HeaderRow>,
    /// The ids of the records this step inserted, by hash.
    new_hashes: HashMap<[u8; 32], BlockId>,
    /// `hdr_leaf` membership this step changed.
    leaves: HashMap<BlockId, bool>,
    /// A failed `rows` call: the step is abandoned and refuses its commit.
    failed: bool,
}

impl<E: KvEngine, F: HdrIo> std::fmt::Debug for HeaderStep<'_, E, F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeaderStep")
            .field("rows", &self.rows.len())
            .field("failed", &self.failed)
            .finish_non_exhaustive()
    }
}

impl<E: KvEngine, F: HdrIo> Store<E, F> {
    /// Opens a header step: takes the writer mutex until the step commits or is dropped (SB0
    /// E8). Other writes that lower or carry a slot wait meanwhile; reads and plain appends do
    /// not.
    pub fn begin_step(&self) -> Result<HeaderStep<'_, E, F>> {
        let hw = self.hw();
        self.halted()?;
        Ok(HeaderStep {
            store: self,
            hw,
            rows: HashMap::new(),
            new_hashes: HashMap::new(),
            leaves: HashMap::new(),
            failed: false,
        })
    }
}

/// The new active chain a step's diff walks: `(height, row)` from `from_height` up.
type Walk = Vec<(u32, HeaderRow)>;

impl<E: KvEngine, F: HdrIo> HeaderStep<'_, E, F> {
    /// The row of `id` as this step leaves it.
    fn row(&self, id: BlockId) -> Result<Option<HeaderRow>> {
        match self.rows.get(&id) {
            Some(row) => Ok(Some(*row)),
            None => self.store.row(id),
        }
    }

    /// The id of `hash` as this step leaves it.
    fn id_of(&self, hash: &[u8; 32]) -> Result<Option<BlockId>> {
        match self.new_hashes.get(hash) {
            Some(&id) => Ok(Some(id)),
            None => self.store.id_of(hash),
        }
    }

    fn is_leaf(&self, id: BlockId) -> Result<bool> {
        if let Some(&leaf) = self.leaves.get(&id) {
            return Ok(leaf);
        }
        if let Some(db) = self.store.db() {
            if kv(db.get(HDR_LEAF, &id_key(id)))?.is_some() {
                return Ok(true);
            }
        }
        Ok(self.store.active_leaves()?.contains(&id))
    }

    fn apply(&mut self, row: &StepRow) -> Result<()> {
        match row {
            StepRow::Record(rec) => {
                let row = rec.to_row();
                if self.id_of(&row.hash)?.is_some() || self.row(rec.id)?.is_some() {
                    return Err(refused(format!(
                        "UNIQUE constraint failed inserting {:?}",
                        rec.id
                    )));
                }
                self.new_hashes.insert(row.hash, rec.id);
                self.rows.insert(rec.id, row);
            }
            &StepRow::Status(id, status) => {
                let mut row =
                    self.row(id)?.ok_or_else(|| refused(format!("status for unknown {id:?}")))?;
                row.status = status.bits();
                self.rows.insert(id, row);
            }
            &StepRow::LeafAdd(id) => {
                let row =
                    self.row(id)?.ok_or_else(|| refused(format!("leaf_add of unknown {id:?}")))?;
                if row.kind != 0 {
                    return Err(refused(format!("leaf_add of context record {id:?}")));
                }
                if self.is_leaf(id)? {
                    return Err(refused(format!("UNIQUE constraint failed: hdr_leaf {id:?}")));
                }
                self.leaves.insert(id, true);
            }
            &StepRow::LeafRemove(id) => {
                self.leaves.insert(id, false);
            }
        }
        Ok(())
    }

    /// §3.7's `ActiveDiff`, checked as `MemHeaderStore` checks it: the active top is
    /// `old_height`; the fork, `from_height − 1`, is active; and the walk from `new_tip` passes
    /// accepted records one height apart down to it. The first diff (nothing active yet) walks
    /// to below the lowest accepted record. Returns the walk and the bounds of the active chain
    /// after the step.
    fn check_active(&self, d: ActiveDiff, active: Bounds) -> Result<(Walk, Bounds)> {
        let fork = d.from_height.checked_sub(1);
        let first = match active.hi {
            Some(top) if top != d.old_height => {
                return Err(refused(format!(
                    "active ends at {top}, not old_height {}",
                    d.old_height
                )));
            }
            Some(_) => {
                if fork.is_none_or(|f| active.lo.is_none_or(|lo| f < lo) || f > d.old_height) {
                    return Err(refused(format!("fork height {fork:?} is not active")));
                }
                false
            }
            None if d.old_height != 0 => {
                return Err(refused(format!("nothing active, but old_height {}", d.old_height)));
            }
            None => true,
        };
        if d.new_height >= super::hdr::index::HEIGHT_CAP {
            return Err(refused(format!("new height {} reaches 2^24 (D3)", d.new_height)));
        }
        if d.from_height > d.new_height.saturating_add(1) {
            return Err(refused(format!(
                "from height {} above new height {} + 1",
                d.from_height, d.new_height
            )));
        }
        // `id` names the record at height `h`, and after the walk the one at the fork height.
        let mut id = Some(d.new_tip);
        let mut walk = Vec::new();
        for h in (d.from_height..=d.new_height).rev() {
            let at = id.ok_or_else(|| refused(format!("active walk: no stored record at {h}")))?;
            let row =
                self.row(at)?.ok_or_else(|| refused(format!("active walk: no row {at:?}")))?;
            if row.kind != 0 || row.height != h {
                return Err(refused(format!(
                    "active walk: {at:?} (kind {}, height {}) at height {h}",
                    row.kind, row.height
                )));
            }
            id = self.id_of(&prev_of(&row))?;
            walk.push((h, row));
        }
        walk.reverse();
        let walked = d.from_height <= d.new_height;
        if !first {
            let at_fork = self.store.active_id(fork.expect("checked above"));
            if id != at_fork {
                return Err(refused(format!(
                    "active walk reached {id:?}, not the fork {at_fork:?}"
                )));
            }
            return Ok((
                walk,
                Bounds {
                    lo: active.lo,
                    hi: Some(d.new_height),
                },
            ));
        }
        if let Some(below) = id {
            if self.row(below)?.is_some_and(|r| r.kind == 0) {
                return Err(refused(format!(
                    "the first active walk stops above accepted {below:?}"
                )));
            }
        }
        let bounds = match walked {
            true => Bounds {
                lo: Some(d.from_height),
                hi: Some(d.new_height),
            },
            false => Bounds::default(),
        };
        Ok((walk, bounds))
    }

    /// Commits the step: everything checked first, then the lowering (if the diff rewrites
    /// active heights), the new active records into the files, and one SQLite commit with the
    /// slot and every other row. On a refusal nothing is written and the step is spent.
    pub fn commit(mut self, tail: &StepTail, durability: Durability) -> Result<()> {
        if self.failed {
            return Err(refused("commit of a step whose rows failed"));
        }
        let store = self.store;
        // D18: a latch set since the step began (an append's failure) refuses it too.
        store.halted()?;
        let view = store.view();
        let mut meta: ChainMeta =
            view.meta.ok_or_else(|| refused("header step before any meta row"))?;
        let (walk, active) = match tail.active {
            Some(d) => self.check_active(d, view.active)?,
            None => (Vec::new(), view.active),
        };
        let mut locks = Vec::new();
        if let Some(cl) = tail.cl {
            let floor = match cl.floor {
                Some(id) => {
                    let row = self
                        .row(id)?
                        .ok_or_else(|| refused(format!("ChainLock floor {id:?} not stored")))?;
                    Some((row.height, row.hash))
                }
                None => None,
            };
            locks = lock_rows(&cl, floor).into_iter().collect();
            meta.cl = cl;
        }
        meta.next_id = tail.meta.next_id;
        meta.tip = tail.meta.tip;
        meta.best_header = tail.meta.best_header;
        // The active top after the step is `meta.tip` (MemHeaderStore's closing check).
        if let Some(top) = active.hi {
            let at_top = match walk.last() {
                Some((_, row)) => Some(BlockId(row.id)),
                None => store.active_id(top),
            };
            if at_top != Some(meta.tip) {
                return Err(refused(format!(
                    "meta.tip {:?} is not the active top {at_top:?}",
                    meta.tip
                )));
            }
        }
        let hw = &mut *self.hw;
        // The lowering (D7), when the diff rewrites active heights.
        let lower_from =
            tail.active.map(|d| d.from_height).filter(|&f| view.active.hi.is_some_and(|t| f <= t));
        if let Some(f) = lower_from {
            store.lower(hw, f)?;
        }
        // New records not made active go off-chain: an id issued other than by extension closes
        // the open run (D5 (2)).
        let walked: HashMap<BlockId, u32> = walk.iter().map(|(h, r)| (BlockId(r.id), *h)).collect();
        if self.new_hashes.values().any(|id| !walked.contains_key(id)) {
            let mut h = store.hdr();
            let tip = h.tip;
            h.runs.close(tip);
        }
        let list: Vec<Activate> = walk
            .iter()
            .map(|(_, row)| Activate {
                hash: row.hash,
                header: row.header,
                id: Some(row.id),
                stored: !self.new_hashes.contains_key(&row.hash),
                row: None,
            })
            .collect();
        let from = walk.first().map_or(0, |(h, _)| *h);
        let mut a = store.activate(hw, from, &list)?;
        // D18: the activation moved memory past SQLite (records synced, runs placed, the
        // anchor); a failure from here to its commit poisons the writer.
        let done = (|| -> Result<()> {
            // D21: an active row that differs from the derived one stays as an override.
            let mut gone_overrides = Vec::new();
            for (h, row) in &walk {
                if store.derived_at(*h)? == *row {
                    continue;
                }
                // The anchor's work and `time_max` are its override row's, and the heights above
                // derive from them (open reloads them from the row).
                let at_start = store.hdr().start == Some(*h);
                if at_start {
                    let anchor = (dash_chain::Work::from_bendian(row.chain_work), row.time_max);
                    store.hdr().anchor = Some(anchor);
                }
                a.batch.put(HDR, &id_key(BlockId(row.id)), &encode_row(row));
                a.overrides.push(row.id);
            }
            // The step's other rows: off-chain records (new, or a status change), and status
            // changes on records that stay active.
            for (id, row) in &self.rows {
                if walked.contains_key(id) {
                    continue;
                }
                let active_at = store.active_id(row.height);
                if active_at == Some(*id) {
                    if store.derived_at(row.height)? == *row {
                        a.batch.delete(HDR, &id_key(*id));
                        gone_overrides.push(id.0);
                    } else {
                        a.batch.put(HDR, &id_key(*id), &encode_row(row));
                        a.overrides.push(id.0);
                    }
                    continue;
                }
                a.batch.put(HDR, &id_key(*id), &encode_row(row));
                if self.new_hashes.contains_key(&row.hash) {
                    a.batch.put(HDR_HASH, &row.hash, &hash_value(*id, row.height));
                    a.batch.put(HDR_H, &height_id_key(row.height, *id), &[]);
                    a.batch.put(HDR_PREV, &prev_id_key(&prev_of(row), *id), &[]);
                    store.hdr().offchain += 1;
                }
            }
            let active_ids: Vec<BlockId> = store.active_leaves()?;
            for (&id, &leaf) in &self.leaves {
                match leaf {
                    // The active tip is a leaf implicitly (§2 `leaves_from`).
                    true if active_ids.contains(&id) => {}
                    true => a.batch.put(HDR_LEAF, &id_key(id), &[]),
                    false => a.batch.delete(HDR_LEAF, &id_key(id)),
                }
            }
            for (k, v) in &locks {
                match v {
                    Some(v) => a.batch.put(LOCKS, k, v),
                    None => a.batch.delete(LOCKS, k),
                }
            }
            {
                let mut h = store.hdr();
                let next = meta.next_id.0.max(h.runs.next_id);
                if next != h.runs.next_id {
                    h.runs.next_id = next;
                    h.runs.dirty = true;
                }
                meta.next_id = BlockId(meta.next_id.0.max(h.runs.issued(h.tip)));
                // `meta` names `next_id`: the runs go with it, the base run's first row included
                // (D5 (0)).
                if h.runs.owed() {
                    put_runs(&mut a.batch, &h.runs);
                    a.runs = Some(h.runs.clone());
                }
            }
            put_meta(&mut a.batch, &meta);
            let commit = match durability {
                Durability::Buffered => Commit::Buffered,
                Durability::Durable => Commit::Durable,
            };
            store.commit_activation(hw, a, commit, |s| {
                {
                    let mut o = lock(&s.overrides);
                    for id in &gone_overrides {
                        o.remove(id);
                    }
                }
                s.rows_write().meta = Some(meta);
            })
        })();
        hw.poison(done)
    }
}

impl<E: KvEngine, F: HdrIo> HeaderStepSink for HeaderStep<'_, E, F> {
    fn rows(&mut self, batch: &[StepRow]) -> Result<(), dash_chain::StoreError> {
        if self.failed {
            return Err(refused("rows after a failed batch").into());
        }
        for row in batch {
            if let Err(e) = self.apply(row) {
                self.failed = true;
                return Err(e.into());
            }
        }
        Ok(())
    }
}
