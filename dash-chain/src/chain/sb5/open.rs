//! SPV `open` (SB5 §8): the chain from its committed records, the window resident at the stable
//! ids (A3), and the active chain, `best_header` (E5) and the checkpoint cache rebuilt.

use std::cmp::Reverse;
use std::sync::Arc;

use dash_consensus::BlockId;
use dash_consensus::header::HeaderParams;
use dash_params::ChainSetup;
use dash_primitives::BlockHash;
use dash_types::Numeric as _;

use crate::Work;
use crate::active::ActiveChain;
use crate::anchor::CONTEXT_ENTRIES;
use crate::arena;
use crate::chain::{
    HeadFacts, HeaderChain, MutableGroup, PAGE, check_config, mtp_or_unknown, next_page,
};
use crate::cold::ColdWalk as _;
use crate::config::{ChainConfig, ChainMode, Services};
use crate::entry::EntryImm;
use crate::error::{ChainError, ConfigError, FailStop, StoreError};
use crate::open::{Corruption, OpenError};
use crate::residency::Evicting;
use crate::shared::{Node, Shared};
use crate::status::Status;
use crate::step::{CHAIN_SCHEMA, ChainMeta};
use crate::store::{HeaderRecord, HeaderStore, RecordKind};
use crate::tip::BlockRef;
use crate::walk;

impl<S: HeaderStore> HeaderChain<S, Evicting> {
    /// SPV reopen (§8): the chain the store's last commit holds, its window resident at the
    /// stored ids. `best_header` is recomputed over the stored leaves (E5); `meta.best_header`
    /// is diagnostic. Every `StoreError` and `Corruption` trips the latch.
    pub fn open(
        params: Arc<ChainSetup>,
        cfg: ChainConfig,
        svc: Services,
        store: S,
    ) -> Result<Self, OpenError> {
        check_config(&cfg)?;
        if cfg.mode != ChainMode::Spv {
            return Err(ConfigError::WrongMode("open").into());
        }
        let hp = HeaderParams::from_chain_params(&params).map_err(ConfigError::Pow)?;
        let meta = store.chain_meta().map_err(|e| storage(&*svc.fail, e))?;
        let meta = meta.ok_or(OpenError::Empty)?;
        if meta.schema != CHAIN_SCHEMA {
            return Err(OpenError::Schema(meta.schema, CHAIN_SCHEMA));
        }
        // An anchor is genesis at 0 or a pack's checkpoint above DGW's window (§8.4).
        let genesis = meta.anchor.0.as_bytes() == params.consensus.hash_genesis_block.as_bytes();
        let anchor = meta.anchor.1;
        if genesis != (anchor == 0) || (1..CONTEXT_ENTRIES).contains(&anchor) {
            return Err(corrupt(&*svc.fail, Corruption::Anchor(anchor)));
        }
        let mut chain = HeaderChain::empty(params, cfg, svc, store, hp, anchor);
        chain.restore(&meta)?;
        Ok(chain)
    }

    /// §8 steps 1–9 on an empty chain.
    fn restore(&mut self, meta: &ChainMeta) -> Result<(), OpenError> {
        let shared = Arc::clone(&self.shared);
        let fail = &*shared.fail;
        let st = |e| storage(fail, e);
        let bad = |c| corrupt(fail, c);

        // 1. Meta.
        if let Some(max) = shared.store.max_id().map_err(st)?
            && max.0 >= meta.next_id.0
        {
            return Err(bad(Corruption::NextIdNotAbove(max)));
        }
        self.next_id = meta.next_id.0;
        shared.issue(self.next_id);

        // 2. Records: the window, from `tip − w`.
        let tip = shared.store.by_id(meta.tip).map_err(st)?;
        let tip = tip.ok_or_else(|| bad(Corruption::MissingRecord(meta.tip)))?;
        if tip.kind == RecordKind::Context || tip.status.has(Status::FAILED_MASK) {
            return Err(bad(Corruption::TipNotLeafPath(tip.id)));
        }
        if tip.height < shared.anchor {
            return Err(bad(Corruption::ActiveMismatch {
                height: tip.height,
            }));
        }
        let w = self.cfg.window.expect("SPV is windowed");
        let base = tip.height.saturating_sub(w).max(self.floor);
        let mut recs = shared.store.from_height(base).map_err(st)?;
        recs.sort_unstable_by_key(|r| (r.height, r.id));

        // 3 and 5. The arena at the stored ids, parents first, each with its median time past;
        // then the skips, once every window entry is set.
        for rec in &recs {
            self.load_record(rec, base)?;
        }
        if shared.arena.imm(tip.id, |_| ()).is_none() {
            return Err(bad(Corruption::MissingRecord(tip.id)));
        }
        for rec in &recs {
            self.check_skip(rec)?;
        }

        // 4. The active chain.
        let active = self.restore_active(&tip, base)?;

        // 6. The checkpoint cache (SB4 §11.4): one lookup per checkpoint, highest first.
        let mut checkpoints: Vec<(u32, BlockHash)> =
            self.checkpoints.iter().map(|(&hash, &key)| (key, hash)).collect();
        checkpoints.sort_unstable_by_key(|&(key, _)| Reverse(key));
        for (key, hash) in checkpoints {
            if let Some(id) = shared.try_lookup(&hash)? {
                self.last_checkpoint = Some((key, shared.node(id)?.height));
                break;
            }
        }

        // 7. `best_header` (E5), and the cold leaves' work.
        let tip_facts = facts(&shared.node(tip.id)?, tip.id);
        let best = self.best_over_leaves()?.filter(|b| b.work >= tip_facts.work);
        let best = best.ok_or_else(|| bad(Corruption::TipNotLeafPath(tip.id)))?;

        // 9. Tip state, as `init_active_tip` publishes it, without its cold walk to the floor;
        // then step 1's ChainLock rows and observed flag, in a status snapshot (SB4 D-C6-3).
        self.active = active;
        self.tip = Some(tip_facts);
        self.best_header = Some(best);
        self.last_seq = 0;
        self.set_tip_cell(tip_facts.at);
        self.restore_cl_state(meta.cl).map_err(|e| match e {
            ChainError::UnknownBlock(id) => corrupt(fail, Corruption::ClFloor(id)),
            e => chain_error(fail, e),
        })?;

        // 8, after 9 since the cursor is the published tip: `fork_target` from it is the tip,
        // which the last committed step activated. Checked under debug; a release mismatch keeps
        // the tip.
        let target = self.fork_target(&self.virtual_tip()).map_err(|e| chain_error(fail, e))?;
        debug_assert_eq!(target.id, tip.id, "the reopened tip is the fork target");
        if target.id != tip.id {
            let (tip, target) = (tip.id, target.id);
            tracing::warn!(?tip, ?target, "open: the fork target is not the stored tip");
        }
        // 10. SB4's `check_index` holds every entry resident, so it does not run here.
        Ok(())
    }

    /// Sets one window record, its parent already set or, at the window's `base`, cold (§8
    /// steps 3 and 5): its resident index, leaf and chunk accounts, and the median time past
    /// from its ancestors.
    fn load_record(&mut self, rec: &HeaderRecord, base: u32) -> Result<(), OpenError> {
        let shared = &*self.shared;
        let prev = match rec.height > self.floor {
            false => None,
            true => {
                let p = shared.try_lookup(&rec.header.prev_hash)?;
                let resident = p.and_then(|p| shared.arena.imm(p, |e| e.height));
                let at_below = match (resident, rec.height == base) {
                    (Some(h), _) => Some(h),
                    (None, true) => p.map(|p| shared.node(p)).transpose()?.map(|n| n.height),
                    (None, false) => None,
                };
                if at_below != Some(rec.height - 1) {
                    return Err(corrupt(&*shared.fail, Corruption::MissingParent(rec.id)));
                }
                p
            }
        };
        let mut times = vec![rec.header.time];
        let mut cur = prev;
        while let Some(id) = cur.filter(|_| times.len() < walk::MEDIAN_TIME_SPAN) {
            let n = shared.node(id)?;
            times.push(n.header.time);
            cur = n.prev;
        }
        let imm = EntryImm {
            hash: rec.hash,
            header: rec.header,
            prev,
            skip: rec.skip,
            height: rec.height,
            chain_work: rec.chain_work,
            mtp: mtp_or_unknown(&times, rec.height),
            time_max: rec.time_max,
            context: rec.kind == RecordKind::Context,
        };
        let group = MutableGroup {
            status: rec.status,
            ..MutableGroup::default()
        };
        crate::fault::residency(|| {
            shared.arena.set(rec.id, imm, &group);
            shared.index.insert(&shared.arena, rec.id);
            self.resident.insert((rec.height, rec.id));
            *self.chunk_live.entry(arena::chunk_of(rec.id)).or_default() += 1;
        });
        if let Some(p) = prev
            && let Some(work) = shared.arena.imm(p, |e| e.chain_work)
        {
            self.leaves.remove(&(work, Reverse(p)));
        }
        self.leaves.insert((rec.chain_work, Reverse(rec.id)));
        Ok(())
    }

    /// §8 step 3's skip check, after the window is set: a skip at `GetSkipHeight`, an issued
    /// id, and a target at that height, resident or cold. The in-memory walks panic on a resident
    /// link off its height (as Core asserts); a cold one is read so that a bad skip is refused
    /// here as corruption, not met later by a cold walk as a storage error. Only skips that jump
    /// below the window base are cold: a few dozen reads at w = 10,000.
    fn check_skip(&self, rec: &HeaderRecord) -> Result<(), OpenError> {
        let shared = &*self.shared;
        let skip_height = walk::skip_height(rec.height);
        let has_prev = shared.arena.imm(rec.id, |e| e.prev.is_some()).expect("loaded");
        let ok = match (has_prev && skip_height >= self.floor, rec.skip) {
            (false, None) => true,
            (true, Some(s)) if s.0 < self.next_id => match shared.arena.imm(s, |e| e.height) {
                Some(h) => h == skip_height,
                None => {
                    let target = shared.store.by_id(s).map_err(|e| storage(&*shared.fail, e))?;
                    target.is_some_and(|t| t.id == s && t.height == skip_height)
                }
            },
            _ => false,
        };
        match ok {
            true => Ok(()),
            false => Err(corrupt(&*shared.fail, Corruption::SkipHeight(rec.id))),
        }
    }

    /// §8 step 4: the tip's ancestry, from the floor. The window is walked over the loaded
    /// entries and cross-checked against the `active` rows (D18); the heights below the window
    /// come from the rows, linked to the window's lowest entry, and the context heights below the
    /// lowest row from one `walk_down`. The chain is then windowed at `base`, as the writer
    /// leaves it after every step (DEC-174): the rows below are checked, not kept.
    fn restore_active(&self, tip: &HeaderRecord, base: u32) -> Result<ActiveChain, OpenError> {
        let shared = &*self.shared;
        let mismatch = |height| {
            corrupt(
                &*shared.fail,
                Corruption::ActiveMismatch {
                    height,
                },
            )
        };
        let rows = shared.store.active_from(self.floor).map_err(|e| storage(&*shared.fail, e))?;
        let lo = rows.first().map_or(tip.height, |r| r.0);
        if lo < self.floor {
            return Err(mismatch(lo));
        }
        if let Some((_, h)) = rows.iter().zip(lo..=u32::MAX).find(|(r, h)| r.0 != *h) {
            return Err(mismatch(h.min(tip.height)));
        }
        if rows.last() != Some(&(tip.height, tip.id)) {
            return Err(mismatch(tip.height));
        }
        let row = |h: u32| h.checked_sub(lo).and_then(|i| rows.get(i as usize)).map(|r| r.1);

        // The window, walked down from the tip.
        let mut window = Vec::with_capacity((tip.height - base + 1) as usize);
        let mut at = Some(tip.id);
        for h in (base..=tip.height).rev() {
            let id = at.ok_or_else(|| mismatch(h))?;
            let (context, prev) =
                shared.arena.imm(id, |e| (e.context, e.prev)).ok_or_else(|| mismatch(h))?;
            if !context && row(h) != Some(id) {
                return Err(mismatch(h));
            }
            window.push(id);
            at = prev;
        }
        window.reverse();

        // Below the window: the rows down to the lowest accepted height, then its context.
        let ctx_top = lo.min(base);
        if base > lo && at != row(base - 1) {
            return Err(mismatch(base - 1));
        }
        let context: Vec<BlockId> = match row(ctx_top).or_else(|| window.first().copied()) {
            Some(top) if ctx_top > self.floor => {
                let recs = shared.walk_down(top, ctx_top - self.floor + 1)?;
                if recs[0].height != ctx_top {
                    return Err(mismatch(ctx_top));
                }
                recs.into_iter().skip(1).rev().map(|r| r.id).collect()
            }
            _ => Vec::new(),
        };
        let below = (ctx_top..base).map(|h| row(h).expect("a dense row below the window"));
        let mut active = ActiveChain::starting_at(self.shared.anchor);
        for id in context.into_iter().chain(below).chain(window) {
            active.push(id);
        }
        active.set_base(base);
        Ok(active)
    }

    /// E5 (§8 step 7, D17): the most-work `IsValid(VALID_TREE)` entry, a lower id winning a tie
    /// (D9). Each stored leaf stands for its path: itself, or for a FAILED leaf its highest
    /// non-FAILED ancestor. CONFLICT is not FAILED, so a CONFLICT leaf is eligible. Also folds
    /// the cold leaves into `max_cold_leaf_work`.
    fn best_over_leaves(&mut self) -> Result<Option<HeadFacts>, OpenError> {
        let shared = &*self.shared;
        let mut best: Option<HeadFacts> = None;
        let (mut page, mut from) = ([(BlockId(0), Work::ZERO); PAGE], Some(BlockId(0)));
        while let Some(lo) = from {
            let n = shared.cold_leaves(lo, &mut page)?;
            from = next_page(&page[..n], |&(id, _)| id);
            for &(leaf, work) in &page[..n] {
                if shared.arena.imm(leaf, |_| ()).is_none() {
                    let bound = self.max_cold_leaf_work.get_mut();
                    *bound = (*bound).max(work);
                }
                if let Some(f) = Self::leaf_facts(shared, leaf)?
                    && best.is_none_or(|b| (b.work, Reverse(b.at.id)) < (f.work, Reverse(f.at.id)))
                {
                    best = Some(f);
                }
            }
        }
        Ok(best)
    }

    /// One stored leaf's path for E5: the leaf, or for a FAILED leaf its highest non-FAILED
    /// ancestor; `None` when that is a context record or FAILED.
    fn leaf_facts(
        shared: &Shared<S, Evicting>,
        leaf: BlockId,
    ) -> Result<Option<HeadFacts>, OpenError> {
        let (mut id, mut n) = (leaf, shared.node(leaf)?);
        while n.status.has(Status::FAILED_MASK)
            && !n.context
            && let Some(p) = n.prev
        {
            // A parent off the height below could close a cycle of FAILED records.
            let parent = shared.node(p)?;
            if parent.height.checked_add(1) != Some(n.height) {
                return Err(corrupt(&*shared.fail, Corruption::MissingParent(id)));
            }
            (id, n) = (p, parent);
        }
        if n.context || n.status.has(Status::FAILED_MASK) {
            return Ok(None);
        }
        Ok(Some(facts(&n, id)))
    }
}

/// A direct store read's failure: the latch trips. A contradiction the store reports is
/// `Corrupt`, any other failure `Storage`.
fn storage(fail: &dyn FailStop, e: StoreError) -> OpenError {
    fail.read_failed(&e.to_string());
    e.into()
}

/// A `ChainError` from a chain call in `open` that names no corruption of its own: a cold
/// read's `Storage` (the read tripped the latch), mapped as [`storage`] maps it, else a bug,
/// which trips it.
fn chain_error(fail: &dyn FailStop, e: ChainError) -> OpenError {
    match e {
        ChainError::Storage(e) => e.into(),
        e => {
            fail.read_failed(&e.to_string());
            OpenError::Internal(e)
        }
    }
}

/// A contradiction in the store: the latch trips.
fn corrupt(fail: &dyn FailStop, c: Corruption) -> OpenError {
    fail.read_failed(&c.to_string());
    OpenError::Corrupt(c)
}

fn facts(n: &Node, id: BlockId) -> HeadFacts {
    HeadFacts {
        at: BlockRef {
            id,
            hash: n.hash,
            height: n.height,
        },
        work: n.chain_work,
        time: n.header.time,
    }
}

#[cfg(all(test, not(loom)))]
mod tests;
