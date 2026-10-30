//! Lowering and activation (SB14 §3, D7), under the writer mutex.
//!
//! - [`Store::lower`] moves the active records at `f..=T` off-chain, in batches of at most
//!   [`LOWER_BATCH`] from the top, each one SQLite commit with the slot below its own batch;
//!   the last is Durable. A pre-sync first makes every height at or below `T` synced, so no slot
//!   names an unsynced height and every copy's parent is stored (S1).
//! - [`Store::activate`] then writes the new records into the files, runs raise steps 1–3, and
//!   returns the batch of the one commit that raises the SQLite slot over them, writes their
//!   runs and drops the copies of re-activated records. The caller adds its own rows and commits
//!   it ([`Store::commit_activation`]).

use dash_chain::HeaderRow;
use dash_kv::{Commit, KvBatch, KvEngine, KvRead};

use super::hdr::io::HdrIo;
use super::hdr::slot::{Committed, Slot};
use super::runs::Runs;
use super::schema::*;
use super::{derived_row, kv, lock, put_runs, Bounds, Error, HdrWriter, Result, Store, END};
use super::{Point, Subscriber, LOWER_BATCH};

/// The height a subscriber commit may name ([`Store::commit_naming`]): any, or, under a raised
/// slot, at most that slot's tip (`Some(None)`: an `empty` slot, none).
#[derive(Clone, Copy, Debug)]
pub(super) struct Cap(Option<Option<u32>>);

impl Cap {
    /// No cap (a lowering's own rewind names heights below its slots).
    pub(super) const NONE: Cap = Cap(None);

    /// `h`, or the cap below it; `None` when nothing may be named.
    pub(super) fn clamp(self, h: u32) -> Option<u32> {
        match self.0 {
            None => Some(h),
            Some(tip) => tip.map(|t| t.min(h)),
        }
    }

    /// Whether the raised slot covers `h`.
    fn covers(self, h: u32) -> bool {
        matches!(self.0, Some(Some(t)) if t >= h)
    }
}

/// A record [`Store::activate`] makes active at the next height.
#[derive(Clone, Debug)]
pub(super) struct Activate {
    pub hash: [u8; 32],
    pub header: [u8; 80],
    /// The id it keeps: an off-chain record's (re-activated, its copy dropped), or one
    /// dash-chain gave. `None`: the next id (D5).
    pub id: Option<u32>,
    /// The record is off-chain now (it has `hdr_hash` and index rows to drop).
    pub stored: bool,
    /// Its own row, when its fields differ from the derived ones (D21): kept as an override.
    pub row: Option<HeaderRow>,
}

/// What [`Store::activate`] leaves for its commit.
pub(super) struct Activation<B> {
    pub batch: B,
    pub slot: Slot,
    /// Off-chain records the commit drops.
    pub dropped: u64,
    /// Override ids the commit adds.
    pub overrides: Vec<u32>,
    /// The runs the batch writes, if any.
    pub runs: Option<Runs>,
}

impl<E: KvEngine, F: HdrIo> Store<E, F> {
    /// Lowers the active chain to `f − 1` (D7 steps 0–1). Nothing to do when `f` is above the
    /// tip. The caller holds the writer mutex and checked `f ≥ start`.
    pub(super) fn lower(&self, hw: &mut HdrWriter<F>, f: u32) -> Result<()> {
        // Any failure part way leaves memory and SQLite apart: the writer stops (D18).
        let done = self.lower_batches(hw, f);
        hw.poison(done)
    }

    fn lower_batches(&self, hw: &mut HdrWriter<F>, f: u32) -> Result<()> {
        let (start, t) = {
            let h = self.hdr();
            match (h.start, h.tip) {
                (Some(s), Some(t)) if f <= t => (s, t),
                _ => return Ok(()),
            }
        };
        debug_assert!(f >= start);
        self.halted()?;
        let db = self.db_create(hw)?;
        // Step 0, the pre-sync (r4 R1): nothing at or below T unsynced.
        if self.hdr().dirty() {
            self.raise_files(hw, false)?;
        }
        let mut top = t;
        let mut n = 0;
        loop {
            let b = top.saturating_sub(LOWER_BATCH - 1).max(f);
            let mut batch = db.batch();
            let mut copied = Vec::with_capacity((top - b + 1) as usize);
            for at in b..=top {
                let row = self.active_row(at)?;
                let id = dash_chain::BlockId(row.id);
                batch.put(HDR, &id_key(id), &encode_row(&row));
                batch.put(HDR_HASH, &row.hash, &hash_value(id, at));
                batch.put(HDR_H, &height_id_key(at, id), &[]);
                batch.put(HDR_PREV, &prev_id_key(&prev_of(&row), id), &[]);
                // The old tip's copy is a leaf, unless an off-chain record names it.
                if n == 0 && at == t && !has_child(db, &row.hash)? {
                    batch.put(HDR_LEAF, &id_key(id), &[]);
                }
                copied.push(row.id);
            }
            let new_tip = b.checked_sub(1).filter(|&n| n >= start);
            let committed = match new_tip {
                Some(n) => {
                    let mut h = self.hdr();
                    Committed::Range {
                        start,
                        tip: n,
                        tip_hash: h.hash_at(n)?,
                        linked: h.linked,
                    }
                }
                None => Committed::Empty,
            };
            let mut runs = self.hdr().runs.clone();
            runs.close(Some(top));
            runs.trim(new_tip);
            put_runs(&mut batch, &runs);
            self.put_next_id(&mut batch, runs.next_id);
            let slot = Self::next_slot(hw, committed);
            batch.put(META, meta_key::HDR_MARKER, &slot.encode());
            let durability = match b == f {
                true => Commit::Durable,
                false => Commit::Buffered,
            };
            self.reach(Point::LowerBatch(n));
            let mut w = self.lease();
            // Under the lease, which subscriber commits hold while they read the bounds, and in
            // every batch: one may have written above f − 1 since the last (Opus-h r1 M1).
            let subs = self.rewind_subs(&mut batch, f)?;
            let _fence = self.fence();
            hw.poison(self.commit(&mut w, batch, durability))?;
            // Published before the lease is released (D16 (3)).
            {
                let mut h = self.hdr();
                h.lower(new_tip)?;
                h.runs = runs;
                h.runs.written();
                h.committed = committed;
                h.offchain += copied.len() as u64;
            }
            {
                let mut o = lock(&self.overrides);
                for id in &copied {
                    o.remove(id);
                }
            }
            self.rows_write().subs = subs;
            hw.sqlite_pending = durability == Commit::Buffered;
            if b == f {
                return Ok(());
            }
            top = b - 1;
            n += 1;
        }
    }

    /// A lowering batch's subscriber rewind: every row above `f − 1`, with its locator. Returns
    /// the subscribers' bounds after it (unchanged after the first batch, unless a subscriber
    /// commit wrote above `f − 1` since).
    fn rewind_subs(&self, batch: &mut E::Batch, f: u32) -> Result<[Bounds; 3]> {
        let mut subs = self.rows_read().subs;
        for s in Subscriber::ALL {
            let bounds = subs[s as usize];
            if bounds.hi.is_none_or(|hi| hi < f) {
                continue;
            }
            batch.delete_range(s.cf(), &be32(f), &END);
            let after = match f.checked_sub(1) {
                Some(keep) => bounds.truncated(keep),
                None => Bounds::default(),
            };
            match self.locator_at(after.hi.map(|hi| hi.min(f.saturating_sub(1))), Cap::NONE)? {
                Some(v) => batch.put(LOCATORS, s.locator_key(), &v),
                None => batch.delete(LOCATORS, s.locator_key()),
            }
            subs[s as usize] = after;
        }
        Ok(subs)
    }

    /// D7 step 2 and the batch of step 3: writes `new` at the tip + 1 (the start, on an empty
    /// store), their runs placed, raise steps 1–3 with no slot; then the batch with the SQLite
    /// slot over them, the runs, the dropped copies and the override rows. The caller holds the
    /// writer mutex, adds its own rows and commits with [`commit_activation`](Self::commit_activation).
    pub(super) fn activate(
        &self,
        hw: &mut HdrWriter<F>,
        at: u32,
        new: &[Activate],
    ) -> Result<Activation<E::Batch>> {
        self.halted()?;
        // Any failure part way leaves memory ahead of SQLite: the writer stops (D18).
        let done = self.activate_records(hw, at, new);
        hw.poison(done)
    }

    fn activate_records(
        &self,
        hw: &mut HdrWriter<F>,
        at: u32,
        new: &[Activate],
    ) -> Result<Activation<E::Batch>> {
        let db = self.db_create(hw)?;
        let mut batch = db.batch();
        let mut dropped = 0;
        let mut overrides = Vec::new();
        {
            let mut h = self.hdr();
            for (i, a) in new.iter().enumerate() {
                let height = at + i as u32;
                let tip = h.tip;
                // The record lands before the runs take it, so they never run past the tip.
                h.append(height, &[(a.hash, a.header)])?;
                let id = match a.id {
                    Some(id) => {
                        h.runs.place(height, id, tip);
                        id
                    }
                    None => h.runs.append(height, tip),
                };
                let key = id_key(dash_chain::BlockId(id));
                if a.stored {
                    batch.delete(HDR_HASH, &a.hash);
                    batch.delete(HDR_H, &height_id_key(height, dash_chain::BlockId(id)));
                    batch.delete(
                        HDR_PREV,
                        &prev_id_key(
                            a.header[4..36].try_into().expect("32"),
                            dash_chain::BlockId(id),
                        ),
                    );
                    batch.delete(HDR_LEAF, &key);
                    dropped += 1;
                }
                match &a.row {
                    Some(row) => {
                        batch.put(
                            HDR,
                            &key,
                            &encode_row(&HeaderRow {
                                id,
                                height,
                                ..*row
                            }),
                        );
                        overrides.push(id);
                    }
                    None if a.stored => batch.delete(HDR, &key),
                    None => {}
                }
            }
        }
        let raised = self.raise_files(hw, false)?;
        let slot = Self::next_slot(hw, raised.committed);
        batch.put(META, meta_key::HDR_MARKER, &slot.encode());
        if let Some(runs) = &raised.runs {
            put_runs(&mut batch, runs);
            self.put_next_id(&mut batch, runs.next_id);
        }
        Ok(Activation {
            batch,
            slot,
            dropped,
            overrides,
            runs: raised.runs,
        })
    }

    /// Commits an [`Activation`] (D7 step 3) and publishes it; `publish` runs under the lease and
    /// the fence once the commit is in (the caller's own published state).
    pub(super) fn commit_activation(
        &self,
        hw: &mut HdrWriter<F>,
        a: Activation<E::Batch>,
        durability: Commit,
        publish: impl FnOnce(&Self),
    ) -> Result<()> {
        let mut w = self.lease();
        let _fence = self.fence();
        hw.poison(self.commit(&mut w, a.batch, durability))?;
        {
            let mut h = self.hdr();
            h.committed = a.slot.committed;
            if let Some(runs) = &a.runs {
                h.runs.written_as(runs);
            }
            h.offchain -= a.dropped;
        }
        {
            let mut o = lock(&self.overrides);
            for id in &a.overrides {
                o.insert(*id);
            }
        }
        publish(self);
        hw.sqlite_pending = durability == Commit::Buffered;
        Ok(())
    }

    /// Whether the committed tip covers `h` (Writes that name headers, §3).
    fn covered(&self, h: u32) -> bool {
        self.hdr().committed.tip().is_some_and(|t| t >= h)
    }

    /// A SQLite commit of rows that may name a header height (a locator, an MN row), D16
    /// (2)–(3). `build` makes the batch, the height it names and what `publish` publishes, under
    /// the lease, so it reads the bounds and the tip a lowering publishes under it (Opus-h r1
    /// M1). With the height covered by the committed tip, the batch commits under the lease
    /// alone. Otherwise it is dropped and built again under the writer mutex, where no lowering
    /// runs: raise steps 1–3 first, then the batch with the SQLite slot, what it names capped at
    /// that slot's tip ([`Cap`]: a fast append may have moved the tip since the raise). The
    /// database is created here if absent (D4).
    pub(super) fn commit_naming<P>(
        &self,
        durability: Commit,
        mut build: impl FnMut(&Self, &E, Cap) -> Result<(E::Batch, Option<u32>, P)>,
        publish: impl FnOnce(&Self, P),
    ) -> Result<()> {
        let mut hw = None;
        let mut slot: Option<(Slot, Option<Runs>)> = None;
        loop {
            if hw.is_none() && self.db().is_none() {
                hw = Some(self.hw());
            }
            let db = match &mut hw {
                Some(hw) => {
                    self.halted()?;
                    self.db_create(hw)?
                }
                None => self.db().expect("checked"),
            };
            self.reach(Point::Naming);
            let mut w = self.lease();
            let cap = Cap(slot.as_ref().map(|(s, _)| s.committed.tip()));
            let (mut batch, names, p) = build(self, db, cap)?;
            if names.is_some_and(|n| !self.covered(n) && !cap.covers(n)) {
                drop(w);
                match &mut hw {
                    None => hw = Some(self.hw()),
                    Some(_) if slot.is_some() => {
                        return Err(Error::Corrupt(format!("{names:?} above the raised slot")));
                    }
                    Some(hw) => {
                        let raised = self.raise_files(hw, false)?;
                        slot = Some((Self::next_slot(hw, raised.committed), raised.runs));
                    }
                }
                continue;
            }
            if let Some((slot, runs)) = &slot {
                batch.put(META, meta_key::HDR_MARKER, &slot.encode());
                if let Some(runs) = runs {
                    put_runs(&mut batch, runs);
                    self.put_next_id(&mut batch, runs.next_id);
                }
            }
            let _fence = self.fence();
            let done = self.commit(&mut w, batch, durability);
            if let (Err(e), Some(hw)) = (&done, &mut hw) {
                if slot.is_some() {
                    hw.latch.set(e);
                }
            }
            done?;
            if let (Some((slot, runs)), Some(hw)) = (slot, &mut hw) {
                let mut h = self.hdr();
                h.committed = slot.committed;
                if let Some(runs) = &runs {
                    h.runs.written_as(runs);
                }
                hw.sqlite_pending = durability == Commit::Buffered;
            }
            publish(self, p);
            return Ok(());
        }
    }

    /// The derived row of the active record at `h`, ignoring any override (D21's comparison).
    pub(super) fn derived_at(&self, h: u32) -> Result<HeaderRow> {
        let mut hd = self.hdr();
        let tip = hd.tip.ok_or_else(|| Error::Corrupt(format!("no active record at {h}")))?;
        let id = hd
            .runs
            .id_at(h, tip)
            .ok_or_else(|| Error::Layout(format!("no run covers active height {h}")))?;
        let r = hd.rec(h)?;
        let start = hd.start.expect("active");
        let skip = match h {
            0 => None,
            _ => Some(super::legacy::skip_height(h))
                .filter(|&s| s >= start)
                .and_then(|s| hd.runs.id_at(s, tip)),
        };
        let (work, time_max) = hd.derived(h)?;
        Ok(derived_row(&r, id, h, skip, work, time_max))
    }
}

/// Whether `db` has an off-chain record naming `hash` as its parent.
pub(super) fn has_child(db: &impl KvRead, hash: &[u8; 32]) -> Result<bool> {
    Ok(kv(db.iter_prefix(HDR_PREV, hash).next().transpose())?.is_some())
}
