//! SPV's `RuleContext` (SB4 §8): the entries the header rules may read for the next header, as
//! one branch's ids cached across consecutive accepts (D23).

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};

use dash_consensus::pow::PowParams;
use dash_consensus::{BlockId, ChainAncestry, HeaderCtx};

use super::header_ctx;
use crate::arena::Arena;
use crate::entry::MTP_UNKNOWN;
use crate::error::ChainError;
use crate::residency::Residency;
use crate::walk;

/// DGW's reads: the parent and its 23 predecessors, plus one spare (§8.1).
const DGW_ENVELOPE: i64 = 25;

/// `GetNextWorkRequired`'s read envelope R for the header at `next_height` (§8.1): the number of
/// entries, `prev` and its ancestors, that its era's retarget reads, and never fewer than the
/// eleven of the median time past. A branch that reads nothing (devnet's minimum-difficulty
/// prefix, `fPowNoRetargeting` after KGW, BTC between retargets without min-difficulty) reads
/// `prev` at most.
// Core: pow.cpp:170-212 GetNextWorkRequired; :134-167 GetNextWorkRequiredBTC; :15-79
// KimotoGravityWell; :81-133 DarkGravityWave
pub(crate) fn read_envelope(pow: &PowParams, next_height: u32) -> u32 {
    let c = pow.config();
    let next = i64::from(next_height);
    let era = if next - 1 < i64::from(c.minimum_difficulty_blocks) {
        0
    } else if next < i64::from(c.kgw_height) {
        // Reachable only where PowParams has checked the interval is not 0.
        let interval = (c.target_timespan / c.target_spacing).max(1);
        match next % interval == 0 || c.allow_min_difficulty_blocks {
            // The retarget's first block, or the min-difficulty walk back to the interval start.
            true => interval,
            false => 1,
        }
    } else if c.no_retargeting {
        1
    } else if next < i64::from(c.dgw_height) {
        // PastBlocksMax entries and the guard read that ends the loop.
        7 * c.target_timespan / c.target_spacing + 1
    } else {
        DGW_ENVELOPE
    };
    u32::try_from(era.max(walk::MEDIAN_TIME_SPAN as i64)).unwrap_or(u32::MAX)
}

/// The writer's cached context (§8.2, D23): the ids of one branch at heights `lo ..= top`,
/// lowest first, and owned copies of those that are not resident. It stores ids, never
/// borrows, so it cannot dangle; an id evicted since it was cached is copied at the next
/// refresh after the eviction's `residency_epoch` bump (§8.2 step 6).
#[derive(Default)]
pub(crate) struct RuleCache {
    lo: u32,
    path: VecDeque<BlockId>,
    /// The path's cold entries; the top's carries its `mtp` (§8.3).
    cold: HashMap<BlockId, HeaderCtx>,
    /// The `residency_epoch` at which `cold` was last completed.
    epoch: u64,
    /// The chain's floor: the lowest held entry, which has no parent (§8.4).
    floor: u32,
    /// A forced envelope (A-9's negative control).
    #[cfg(test)]
    pub(crate) envelope: Option<u32>,
}

/// One entry as `refresh` reads it: its `pprev`, and a copy when it is cold.
pub(crate) struct PathEntry {
    pub(crate) prev: Option<BlockId>,
    pub(crate) cold: Option<HeaderCtx>,
}

impl RuleCache {
    /// The envelope the next header on a parent at `prev_height` reads.
    pub(crate) fn envelope(&self, pow: &PowParams, prev_height: u32) -> u32 {
        #[cfg(test)]
        if let Some(r) = self.envelope {
            return r;
        }
        read_envelope(pow, prev_height + 1)
    }

    /// Makes the context `prev`'s branch from `max(prev_height + 1 − envelope, floor)` up to
    /// `prev` (§8.2 steps 1–6): extend or cut the top when the cached branch leads to `prev`,
    /// otherwise rebuild, then grow or trim the bottom, and copy the cold entries. `time` is a
    /// resident entry's `nTime`, for a cold top's `mtp`.
    pub(crate) fn refresh(
        &mut self,
        prev: BlockId,
        prev_height: u32,
        envelope: u32,
        (floor, epoch): (u32, u64),
        mut link: impl FnMut(BlockId) -> Result<PathEntry, ChainError>,
        time: impl Fn(BlockId) -> Option<u32>,
    ) -> Result<(), ChainError> {
        let want = (prev_height + 1).saturating_sub(envelope).max(floor);
        self.floor = floor;
        if !self.align_top(prev, prev_height, envelope, &mut link)? {
            self.path.clear();
            self.cold.clear();
            let top = link(prev)?;
            self.keep_cold(prev, top.cold);
            self.path.push_back(prev);
            self.lo = prev_height;
        }
        if self.lo > want {
            let mut front = link(*self.path.front().expect("the path holds prev"))?;
            while self.lo > want {
                let below = front
                    .prev
                    .ok_or(ChainError::Internal("rule context: no parent above floor"))?;
                front = link(below)?;
                self.keep_cold(below, front.cold.take());
                self.path.push_front(below);
                self.lo -= 1;
            }
        }
        while self.lo < want {
            let id = self.path.pop_front().expect("lo < want <= top");
            self.cold.remove(&id);
            self.lo += 1;
        }
        if self.epoch != epoch {
            for i in 0..self.path.len() {
                let id = self.path[i];
                if !self.cold.contains_key(&id) {
                    self.keep_cold(id, link(id)?.cold);
                }
            }
            self.epoch = epoch;
        }
        self.top_mtp(time);
        Ok(())
    }

    fn keep_cold(&mut self, id: BlockId, cold: Option<HeaderCtx>) {
        if let Some(h) = cold {
            self.cold.insert(id, h);
        }
    }

    /// A cold top's `mtp` from the path's times (§8.3): the path holds at least eleven entries
    /// unless it starts at the floor, where genesis ends the times as in Core and the pack's
    /// lowest ten leave it unknown.
    fn top_mtp(&mut self, time: impl Fn(BlockId) -> Option<u32>) {
        let top = *self.path.back().expect("the path holds prev");
        if self.cold.get(&top).is_none_or(|h| h.mtp != MTP_UNKNOWN) {
            return;
        }
        let n = self.path.len().min(walk::MEDIAN_TIME_SPAN);
        if n < walk::MEDIAN_TIME_SPAN && self.lo > 0 {
            return;
        }
        let time_of = |id: &BlockId| match self.cold.get(id) {
            Some(h) => h.time,
            None => time(*id).expect("a path entry is resident or cold"),
        };
        let mtp = walk::median_time_past(self.path.iter().rev().take(n).map(time_of));
        self.cold.get_mut(&top).expect("checked above").mtp = mtp;
    }

    /// Moves the cached top to `prev` when `prev` is on the cached branch (the top is cut back
    /// to it) or the cached top is an ancestor of `prev` at most `envelope` below it (the ids
    /// between are pushed). False when neither holds: the caller rebuilds.
    fn align_top(
        &mut self,
        prev: BlockId,
        prev_height: u32,
        envelope: u32,
        link: &mut impl FnMut(BlockId) -> Result<PathEntry, ChainError>,
    ) -> Result<bool, ChainError> {
        let Some(top_height) = self.top_height() else {
            return Ok(false);
        };
        if prev_height <= top_height {
            if self.at(prev_height) != Some(prev) {
                return Ok(false);
            }
            for id in self.path.drain((prev_height - self.lo) as usize + 1..) {
                self.cold.remove(&id);
            }
            return Ok(true);
        }
        if prev_height - top_height > envelope {
            return Ok(false);
        }
        let mut above = Vec::with_capacity((prev_height - top_height) as usize);
        let mut walk = Some(prev);
        for _ in top_height..prev_height {
            let id = walk.ok_or(ChainError::Internal("rule context: no parent above floor"))?;
            let e = link(id)?;
            above.push((id, e.cold));
            walk = e.prev;
        }
        if walk != self.path.back().copied() {
            return Ok(false);
        }
        for (id, cold) in above.into_iter().rev() {
            self.keep_cold(id, cold);
            self.path.push_back(id);
        }
        Ok(true)
    }

    /// The path's lowest height and its ids.
    #[cfg(all(test, not(loom)))]
    pub(crate) fn path(&self) -> (u32, Vec<BlockId>) {
        (self.lo, self.path.iter().copied().collect())
    }

    fn top_height(&self) -> Option<u32> {
        (!self.path.is_empty()).then(|| self.lo + self.path.len() as u32 - 1)
    }

    /// The id at `height`, when the path holds that height.
    fn at(&self, height: u32) -> Option<BlockId> {
        height.checked_sub(self.lo).and_then(|i| self.path.get(i as usize)).copied()
    }
}

/// The rules' view of a [`RuleCache`] (§8.2): `ChainAncestry` over the path's entries only. A
/// read outside the path sets a flag and answers a fixed dummy (debug builds panic first); the
/// caller turns the flag into `ChainError::Internal`.
pub(crate) struct RuleContext<'a, R: Residency> {
    arena: &'a Arena<R>,
    cache: &'a RuleCache,
    outside: AtomicBool,
}

impl<'a, R: Residency> RuleContext<'a, R> {
    pub(crate) fn new(arena: &'a Arena<R>, cache: &'a RuleCache) -> RuleContext<'a, R> {
        debug_assert!(!cache.path.is_empty(), "a refreshed context");
        RuleContext {
            arena,
            cache,
            outside: AtomicBool::new(false),
        }
    }

    /// Whether a rule read outside the path.
    pub(crate) fn read_outside(&self) -> bool {
        self.outside.load(Ordering::Relaxed)
    }

    #[inline]
    fn header_of(&self, id: BlockId) -> Option<HeaderCtx> {
        self.arena.imm(id, header_ctx).or_else(|| self.cache.cold.get(&id).copied())
    }

    /// `id`'s height when the path holds `id`.
    #[inline(always)]
    fn height_on_path(&self, id: BlockId) -> Option<u32> {
        let height = self
            .arena
            .imm(id, |e| e.height)
            .or_else(|| self.cache.cold.get(&id).map(|h| h.height))?;
        (self.cache.at(height) == Some(id)).then_some(height)
    }

    fn outside<T>(&self, what: &str, id: BlockId, dummy: T) -> T {
        debug_assert!(false, "rule read outside its envelope: {what} of {id:?}");
        self.outside.store(true, Ordering::Relaxed);
        dummy
    }

    fn dummy(&self) -> HeaderCtx {
        let top = *self.cache.path.back().expect("a refreshed context");
        self.header_of(top).expect("the path's top is resident or cold")
    }
}

impl<R: Residency> ChainAncestry for RuleContext<'_, R> {
    fn header(&self, id: BlockId) -> HeaderCtx {
        match self.height_on_path(id).and_then(|_| self.header_of(id)) {
            Some(h) => h,
            None => self.outside("header", id, self.dummy()),
        }
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        let Some(top) = self.height_on_path(id) else {
            return self.outside("ancestor", id, None);
        };
        if height > top {
            return None;
        }
        match self.cache.at(height) {
            Some(a) => Some(a),
            None => self.outside("ancestor", id, None),
        }
    }

    fn prev(&self, id: BlockId) -> Option<BlockId> {
        let Some(height) = self.height_on_path(id) else {
            return self.outside("prev", id, None);
        };
        // The lowest held entry (genesis, the pack's lowest) has none; any other entry's parent
        // must be on the path.
        if height == self.cache.floor {
            return None;
        }
        match self.cache.at(height - 1) {
            Some(p) => {
                debug_assert!(
                    self.arena.imm(id, |e| e.prev).is_none_or(|prev| prev == Some(p)),
                    "the path follows pprev"
                );
                Some(p)
            }
            None => self.outside("prev", id, None),
        }
    }
}

#[cfg(all(test, not(loom)))]
mod tests;
