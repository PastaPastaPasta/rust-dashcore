//! The generator's world model (O2b §3.2): the block tree it built, the coins each block
//! creates and spends, what the node holds of each block, and the clock. It is updated from
//! **Core's** step after every op (O2b-3 P2), so a corpus resolves to the same ops whatever the
//! second system is.
//!
//! It is advisory: the model picks targets and predicts replies (the generator's expectations)
//! and ties (`ties`); Core's report is authoritative for both.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use dash_oracle_sys::{ChainOp, CheckResult, Checked, RestartKind};

use super::av::Line;
use super::build::{Built, Lock, MATURITY, MAX_FUTURE};
use super::core::SPORK_19;
use super::generate::{CL_SEEN_KEPT, cl_conflict, conflicts_with};
use super::mutate::{Stage, chain_operator};
use super::step::{ReplyClass, Step};

pub type Hash = [u8; 32];
pub type Outpoint = (Hash, u32);

/// A spendable output the model knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Coin {
    pub outpoint: Outpoint,
    pub value: i64,
    pub lock: Lock,
    pub height: u32,
    pub coinbase: bool,
}

impl Coin {
    /// Whether a block at `height` may spend it.
    pub fn mature_at(&self, height: u32) -> bool {
        !self.coinbase || height >= self.height + MATURITY
    }
}

/// What the node holds of a block (as far as the model knows).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Held {
    /// Built, nothing sent (a held block whose header may follow, `extend … hold`).
    Nothing,
    /// The header is in the block index.
    Header,
    /// Header and data are in the block index.
    Data,
    /// Sent, and refused before the index (a header-stage reject or a decode failure).
    Refused,
}

/// The tie class of an index entry: Core breaks a tie of equal work and equal sequence id by
/// address, and only loaded, header-only and unlinked entries share an id (0) (O2b-3 P3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seq {
    /// Present at the last load (restore or RESTART): sequence id 0.
    Loaded,
    /// Its data arrived after the last load, every ancestor's too: a unique id.
    Arrived,
    /// Its data arrived after the last load while an ancestor's was missing: 0 until that
    /// arrives (`ReceivedBlockTransactions` numbers only a block whose parent's chain has its
    /// transactions, then the stored descendants that waited on it).
    Unlinked,
    /// Its data arrived while its parent was marked failed: the walk that numbers waiting
    /// descendants skips it, so it stays 0 until the next load. (The model's failed flag also
    /// covers a ChainLock conflict, where Core does number it: that case errs towards 0.)
    Stranded,
    /// `preciousblock` gave it a unique negative id.
    Precious,
}

/// One block the model knows.
#[derive(Clone, Debug)]
pub struct Block {
    pub hash: Hash,
    pub parent: Option<usize>,
    pub height: u32,
    pub time: u32,
    pub built: Option<Built>,
    pub creates: Vec<Coin>,
    pub spends: Vec<Outpoint>,
    pub held: Held,
    /// Known failed in Core (a reject that marks it, `BlockChecked` invalid, an invalidation,
    /// a conflicting or invalid tip row). A mutated block that Core has not judged is not.
    pub invalid: bool,
    /// The chain operator it was built with ([`super::mutate::chain_operator`]): never presumed
    /// valid.
    pub mutation: Option<usize>,
    pub seq: Seq,
    /// Its index entry as the last full flush or index-only write left it on disk
    /// (`held`, `invalid`): a KILL loses every change since, and every entry indexed since.
    pub durable: Option<(Held, bool)>,
}

impl Block {
    pub fn mutated(&self) -> bool {
        self.mutation.is_some()
    }

    /// Its index entry has changes a KILL loses.
    pub fn dirty(&self) -> bool {
        self.indexed() && self.durable != Some((self.held, self.invalid))
    }

    /// Its serialization (empty for genesis, which is never sent).
    pub fn built_bytes(&self) -> Vec<u8> {
        self.built.as_ref().map(|b| b.bytes.clone()).unwrap_or_default()
    }

    pub fn indexed(&self) -> bool {
        matches!(self.held, Held::Header | Held::Data)
    }

    /// Refused before the index, unless it is indexed already.
    fn refuse(&mut self) {
        if !self.indexed() {
            self.held = Held::Refused;
        }
    }

    /// At least its header is in the index.
    fn ensure_header(&mut self) {
        if !self.indexed() {
            self.held = Held::Header;
        }
    }

    /// Seq-0 in Core's comparator: loaded, header-only, or stored under a chain without data (a
    /// header-only block made precious has a negative id in Core: counting it is conservative, it
    /// only over-avoids).
    pub fn seq_zero(&self) -> bool {
        self.held == Held::Header
            || (self.held == Held::Data
                && matches!(self.seq, Seq::Loaded | Seq::Unlinked | Seq::Stranded))
    }
}

/// The model.
#[derive(Clone, Debug)]
pub struct World {
    pub blocks: Vec<Block>,
    pub by_hash: HashMap<Hash, usize>,
    /// The image's tip: inputs never fork below the image's maturity window.
    pub image_tip: usize,
    /// Core's active tip after the last step.
    pub tip: usize,
    /// Tips of branches the generator made (`fork`), newest last; branch 0 is the active chain.
    pub branches: Vec<usize>,
    pub mocktime: i64,
    /// Core's best ChainLock whose block it has indexed (`bestChainLockBlockIndex`): what
    /// conflicts and enforcement read. A newer lock on a block Core lacks leaves it, until
    /// that block is indexed.
    pub best_cl: Option<usize>,
    /// [`World::best_cl`] when CL_ENFORCE last ran since the last restart.
    pub cl_enforced: Option<usize>,
    /// The height of Core's best recorded ChainLock, its block known or not (-1: none).
    pub cl_height: i32,
    /// The ChainLocks recorded since the last load, as `(height, block hash)`.
    pub cl_seen: BTreeSet<(i32, Hash)>,
    /// Every tip the coins database may have last been written at; `None` once a load leaves
    /// them unknown, until the next write the model follows. Only the KILL's expectation reads
    /// it (DEC-1598, DEC-1602).
    pub coins_tips: Option<Vec<usize>>,
    /// The session is down (a refused load).
    pub down: bool,
    /// Per-input counter that makes every coinbase unique.
    pub tag: u64,
    /// A block failed a consensus check in Core during this input (not a ChainLock conflict,
    /// and not a child of a failed block).
    pub consensus_failure: bool,
    /// The blocks the input's own INVALIDATEs marked.
    pub own_invalidates: BTreeSet<usize>,
    /// The blocks submitted while an ancestor was already marked failed, whatever the reply,
    /// indexed or not: rule 2 scope (DEC-256).
    pub stored_under_failed: BTreeSet<usize>,
    /// S2's prebuilt header line above the image tip ([`super::av`]); `None` on S0 and S1.
    pub line: Option<Line>,
    /// The fan-out shape's outputs ([`super::generate::fan_out`]): the schedule's.
    pub fan_out: usize,
    /// Core's best header (`m_best_header`) as the last live step's post reported it; `None`
    /// when the model does not know that block.
    pub best_header: Option<usize>,
    /// Each block's children, oldest first: the tree [`World::children`] reads.
    kids: Vec<Vec<usize>>,
    /// Spork 19's value: the image's (0, on), then each `SPORK`'s; a load reads the store
    /// (O2b D34).
    pub spork19: i64,
    /// What the last store of `sporks.dat` holds: a clean restart's teardown writes it
    /// (`~CSporkManager`, `spork.cpp:46-50`), a KILL does not.
    pub spork19_stored: i64,
    /// [`World::spork19`] at the last load.
    pub spork19_loaded: i64,
    /// The ChainLock handler's cached `isEnabled` (`handler.h:55`): set by `CheckActiveState`
    /// only, which the session and V run at `CL_RECORD` and `CL_ENFORCE` alone; false after a
    /// load, whose handler is new.
    pub cl_handler_enabled: bool,
    /// While spork 19 was off, a block conflicting with the best lock was indexed, or an
    /// enforcement left it unenforced: the active chain may conflict with the lock once the
    /// spork is back on (OD-L3-128 C-B). A reset or an enforcement that runs closes it.
    pub cl_window_open: bool,
    /// Core's ChainLock conflict marks may differ from what the lock's geometry says: a reset
    /// dropped a lock some indexed block conflicted with, or, while spork 19 was off, a block
    /// conflicting with the best lock was indexed or an enforcement marked nothing with one
    /// indexed (4f2 DESIGN §4.2).
    pub cl_flags_unsure: bool,
    /// The locks a reset dropped while an indexed block conflicted with them, in order: Core's
    /// marks against them outlive the lock (`ResetChainlock` clears no block's status), so the
    /// flag's readers still take their geometry for a possible mark (4f2 DESIGN §4.2, r2).
    pub cl_dropped: Vec<usize>,
}

impl World {
    /// A world holding `blocks` (genesis first, a chain), all loaded with data.
    pub fn from_chain(blocks: Vec<Block>, mocktime: i64) -> World {
        let by_hash = blocks.iter().enumerate().map(|(i, b)| (b.hash, i)).collect();
        let tip = blocks.len() - 1;
        let mut kids = vec![Vec::new(); blocks.len()];
        for (i, b) in blocks.iter().enumerate() {
            if let Some(p) = b.parent {
                kids[p].push(i);
            }
        }
        World {
            blocks,
            by_hash,
            image_tip: tip,
            tip,
            branches: Vec::new(),
            mocktime,
            best_cl: None,
            cl_enforced: None,
            cl_height: -1,
            cl_seen: BTreeSet::new(),
            coins_tips: Some(vec![tip]),
            down: false,
            tag: 0,
            consensus_failure: false,
            own_invalidates: BTreeSet::new(),
            stored_under_failed: BTreeSet::new(),
            line: None,
            fan_out: super::generate::FAN_OUT,
            best_header: Some(tip),
            kids,
            spork19: 0,
            spork19_stored: 0,
            spork19_loaded: 0,
            cl_handler_enabled: false,
            cl_window_open: false,
            cl_flags_unsure: false,
            cl_dropped: Vec::new(),
        }
    }

    pub fn id(&self, hash: &Hash) -> Option<usize> {
        self.by_hash.get(hash).copied()
    }

    pub fn add(&mut self, block: Block) -> usize {
        let id = self.blocks.len();
        self.by_hash.insert(block.hash, id);
        if let Some(p) = block.parent {
            self.kids[p].push(id);
        }
        self.blocks.push(block);
        self.kids.push(Vec::new());
        id
    }

    /// `id`'s ancestors from `id` down to genesis.
    pub fn ancestry(&self, id: usize) -> impl Iterator<Item = usize> + '_ {
        std::iter::successors(Some(id), |&i| self.blocks[i].parent)
    }

    /// `id`'s ancestor at `height`.
    pub fn ancestor(&self, id: usize, height: u32) -> Option<usize> {
        self.ancestry(id).find(|&i| self.blocks[i].height == height)
    }

    pub fn is_ancestor(&self, a: usize, of: usize) -> bool {
        self.ancestor(of, self.blocks[a].height) == Some(a)
    }

    /// Whether a RECONSIDER of `t` reaches a block of [`World::stored_under_failed`]: one in
    /// `t`'s subtree, or in the subtree of an ancestor of `t` marked failed.
    pub fn reconsider_reaches_stored_under_failed(&self, t: usize) -> bool {
        let roots: Vec<usize> =
            self.ancestry(t).filter(|&a| a == t || self.blocks[a].invalid).collect();
        self.stored_under_failed.iter().any(|&s| self.ancestry(s).any(|a| roots.contains(&a)))
    }

    /// Block `id`'s time is more than `MAX_FUTURE` past the mock clock.
    pub(crate) fn too_new(&self, id: usize) -> bool {
        i64::from(self.blocks[id].time) > self.mocktime + MAX_FUTURE
    }

    /// Block `id`'s data is stored. Core numbers it (a unique sequence id) only if every
    /// ancestor's data is stored, and then each stored descendant that waited on it; otherwise
    /// it stays at 0 ([`Seq::Unlinked`], or [`Seq::Stranded`] under a failed parent).
    pub(crate) fn store_data(&mut self, id: usize) {
        if self.blocks[id].held == Held::Data {
            return;
        }
        self.blocks[id].held = Held::Data;
        let linked = self.ancestry(id).all(|a| self.blocks[a].held == Held::Data);
        if !linked {
            let failed_parent = self.blocks[id].parent.is_some_and(|p| self.blocks[p].invalid);
            self.blocks[id].seq = if failed_parent {
                Seq::Stranded
            } else {
                Seq::Unlinked
            };
            return;
        }
        let mut queue = vec![id];
        while let Some(i) = queue.pop() {
            self.blocks[i].seq = Seq::Arrived;
            queue.extend(self.children(i).filter(|&c| {
                self.blocks[c].held == Held::Data && self.blocks[c].seq != Seq::Stranded
            }));
        }
    }

    /// `id`'s children, oldest first.
    pub fn children(&self, id: usize) -> impl Iterator<Item = usize> + '_ {
        self.kids[id].iter().copied()
    }

    /// The active chain from the tip down, `depth` blocks below the tip (clamped to genesis).
    pub fn active_at_depth(&self, depth: u32) -> usize {
        let h = self.blocks[self.tip].height.saturating_sub(depth);
        self.ancestor(self.tip, h).unwrap_or(0)
    }

    /// The coins spendable on top of `id` (its chain's UTXO set), by outpoint.
    pub fn utxos(&self, id: usize) -> BTreeMap<Outpoint, Coin> {
        let path: Vec<usize> = self.ancestry(id).collect();
        let mut set = BTreeMap::new();
        // Creates before spends: a transaction may spend an earlier one's output in its block.
        for &b in path.iter().rev() {
            for c in &self.blocks[b].creates {
                set.insert(c.outpoint, *c);
            }
            for o in &self.blocks[b].spends {
                set.remove(o);
            }
        }
        set
    }

    /// Whether the model presumes `id`'s chain valid: no block on it is known failed, mutated
    /// or refused.
    pub fn presumed_valid(&self, id: usize) -> bool {
        self.ancestry(id).all(|i| {
            let b = &self.blocks[i];
            !b.invalid && !b.mutated() && b.held != Held::Refused
        })
    }

    /// Whether `id` may be marked only as a strict descendant of the input's own INVALIDATE: Core
    /// marks the target and the active blocks it disconnects, never a header-only or unconnected
    /// descendant, so whether Core holds `id` failed is not the model's to say.
    // Core: src/validation.cpp:3929-3941 at v24.0.0
    // Core: src/validation.cpp:4027-4030 at v24.0.0
    pub fn under_own_invalidate(&self, id: usize) -> bool {
        !self.own_invalidates.contains(&id)
            && self.ancestry(id).skip(1).any(|a| self.own_invalidates.contains(&a))
    }

    /// Held blocks on the branch ending at `tip`, lowest first (built, data not sent).
    pub fn undelivered(&self, tip: usize) -> Vec<usize> {
        let mut v: Vec<usize> = self
            .ancestry(tip)
            .take_while(|&i| matches!(self.blocks[i].held, Held::Nothing | Held::Header))
            .collect();
        v.reverse();
        v
    }

    /// Block `id` and every descendant.
    pub(crate) fn subtree(&self, id: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut stack = vec![id];
        while let Some(i) = stack.pop() {
            out.push(i);
            stack.extend(self.children(i));
        }
        out
    }

    /// Spork 19 is active: its value is before the adjusted time, the mock clock with no peers
    /// (`IsSporkActive`, `spork.cpp:163-169`).
    pub fn spork19_active(&self) -> bool {
        self.spork19 < self.mocktime
    }

    /// `CheckActiveState` (`chainlock/handler.cpp:209-222`) as the next `CL_RECORD` or
    /// `CL_ENFORCE` runs it: whether the handler is enabled after it, and whether its off → on
    /// edge resets the ChainLocks first (`ResetChainlock`).
    pub fn cl_check(&self) -> (bool, bool) {
        let on = self.spork19_active();
        (on, on && !self.cl_handler_enabled)
    }

    /// The best lock and the best recorded height as a ChainLock op sees them after its
    /// `CheckActiveState`.
    pub fn cl_after_check(&self) -> (Option<usize>, i32) {
        if self.cl_check().1 {
            (None, -1)
        } else {
            (self.best_cl, self.cl_height)
        }
    }

    /// An indexed block conflicts with the best lock.
    pub(crate) fn indexed_conflicts(&self) -> bool {
        self.best_cl.is_some_and(|cl| self.indexed_conflicts_with(cl))
    }

    /// Lock block `cl` is indexed and an indexed block conflicts with it.
    fn indexed_conflicts_with(&self, cl: usize) -> bool {
        self.blocks[cl].indexed()
            && (0..self.blocks.len())
                .any(|id| self.blocks[id].indexed() && conflicts_with(self, cl, id))
    }

    /// The active chain has a block the best lock conflicts with.
    fn active_conflicts(&self) -> bool {
        self.best_cl.is_some() && self.ancestry(self.tip).any(|a| cl_conflict(self, a))
    }

    /// Spork 19 is back on and the active chain conflicts with a lock it was off for: Core's
    /// 5 s timer (`h.cpp:63-72`) would run `CheckActiveState` and `EnforceBestChainLock` now,
    /// and the generator stands in for it (OD-L3-128 C-B: never without
    /// [`World::cl_window_open`]).
    pub fn cl_window_due(&self) -> bool {
        !self.down && self.cl_window_open && self.spork19_active() && self.active_conflicts()
    }

    /// `ChainlockHandler::CheckActiveState` folded in before a ChainLock op runs.
    fn check_active_state(&mut self) {
        let (on, resets) = self.cl_check();
        if resets {
            if let Some(cl) = self.best_cl
                && self.indexed_conflicts()
            {
                self.cl_flags_unsure = true;
                self.cl_dropped.push(cl);
            }
            self.best_cl = None;
            self.cl_height = -1;
            self.cl_enforced = None;
            self.cl_window_open = false;
        }
        self.cl_handler_enabled = on;
    }

    /// Spork 19's value in Core's post after `op`'s step, as the model predicts it.
    fn spork19_after(&self, op: &ChainOp, class: &ReplyClass) -> i64 {
        match op {
            ChainOp::Spork {
                id: SPORK_19,
                value,
            } if *class == ReplyClass::Ok => *value,
            ChainOp::Restart(RestartKind::Clean) if !self.down => self.spork19,
            ChainOp::Restart(_) => self.spork19_stored,
            _ => self.spork19,
        }
    }

    /// The model's spork 19 and best ChainLock height against Core's post for `op` (4f2 DESIGN
    /// §3.4), before the step is folded in: the model predicts, Core's post only confirms.
    pub fn check_spork_cl(&self, op: &ChainOp, step: &Step) -> Result<(), String> {
        let class = step.class(op);
        if let Some(sporks) = &step.post.sporks {
            let value = self.spork19_after(op, &class);
            let mocktime = match op {
                ChainOp::SetTime(t) => *t,
                _ => self.mocktime,
            };
            let want = (value, value < mocktime);
            let got = sporks.iter().find(|s| s.0 == SPORK_19).map(|s| (s.1, s.2));
            if got != Some(want) {
                return Err(format!("model: spork19 {want:?}, Core {got:?}"));
            }
        }
        let height = match op {
            _ if self.down || step.post.down || self.cl_seen.len() >= CL_SEEN_KEPT => return Ok(()),
            ChainOp::ClRecord {
                height,
                ..
            } if class == ReplyClass::Ok => *height,
            ChainOp::ClRecord {
                ..
            }
            | ChainOp::ClEnforce => self.cl_after_check().1,
            _ => return Ok(()),
        };
        let got = step.post.best_cl.as_ref().map(|cl| cl.as_ref().map_or(-1, |c| c.0));
        if got != Some(height) {
            return Err(format!("model: cl height {height}, Core {got:?}"));
        }
        Ok(())
    }

    /// Fold Core's step for `op` in.
    pub fn observe(&mut self, op: &ChainOp, step: &Step) {
        let class = step.class(op);
        let (before, was_down) = (self.tip, self.down);
        if matches!(op, ChainOp::ClRecord { .. } | ChainOp::ClEnforce)
            && !self.down
            && class != ReplyClass::Down
        {
            self.check_active_state();
        }
        // What a load reads back: a clean restart of an up session flushes its tip at teardown;
        // a KILL, or a restart of a down session, writes nothing. The load's own activation
        // does not flush, so `post.tip` is not the coins database's.
        if *op == ChainOp::Restart(RestartKind::Clean) && !self.down {
            self.coins_tips = Some(vec![self.tip]);
        }
        // The op's own block: its reply decides its state, not its BlockChecked.
        let mut own = None;
        let mut own_header = None;
        match op {
            ChainOp::Block(bytes) => {
                own = block_id(self, bytes);
                if let Some(id) = own {
                    if self.ancestry(id).skip(1).any(|a| self.blocks[a].invalid) {
                        self.stored_under_failed.insert(id);
                    }
                    self.observe_block_reply(id, &class);
                }
            }
            // TestBlockValidity indexes nothing, and its BlockChecked is the proposed block's.
            ChainOp::Proposal(bytes) => own = block_id(self, bytes),
            ChainOp::Header(header) => {
                own_header = self.id(&header_hash(header));
                if let Some(id) = own_header {
                    self.observe_header_reply(id, &class);
                }
            }
            ChainOp::Invalidate(hash) if class == ReplyClass::Ok => {
                if let Some(id) = self.id(hash) {
                    for i in self.subtree(id) {
                        self.blocks[i].invalid = true;
                    }
                    self.own_invalidates.insert(id);
                }
            }
            ChainOp::Reconsider {
                hash,
                ignore_chainlocks,
            } if class == ReplyClass::Ok => {
                if let Some(id) = self.id(hash) {
                    // Conflict marks stay unless it ignores ChainLocks (`ResetBlockFailureFlags`):
                    // a marked block the lock conflicts with is taken for one.
                    let mut reset = self.subtree(id);
                    reset.extend(self.ancestry(id));
                    for a in reset {
                        let conflict = !ignore_chainlocks && cl_conflict(self, a);
                        self.blocks[a].invalid &= conflict;
                    }
                }
            }
            ChainOp::Precious(hash) if class == ReplyClass::Ok => {
                // `PreciousBlock` leaves a block with less work than the tip alone.
                if let Some(id) = self
                    .id(hash)
                    .filter(|&id| self.blocks[id].height >= self.blocks[self.tip].height)
                {
                    self.blocks[id].seq = Seq::Precious;
                }
            }
            ChainOp::SetTime(t) => self.mocktime = *t,
            // `EnforceBestChainLock` returns while the handler is disabled (`h.cpp:273-277`).
            ChainOp::ClEnforce => {
                self.cl_enforced = self.best_cl;
                match self.best_cl {
                    // Nothing is marked, so an indexed block the lock conflicts with (one it was
                    // recorded over while spork 19 was off) stays unmarked.
                    Some(_) if !self.down && !self.cl_handler_enabled => {
                        self.cl_window_open = true;
                        if self.indexed_conflicts() {
                            self.cl_flags_unsure = true;
                        }
                    }
                    Some(cl) => {
                        self.enforce_marks(cl);
                        self.cl_window_open = false;
                    }
                    None => {}
                }
            }
            ChainOp::Spork {
                id: SPORK_19,
                value,
            } if class == ReplyClass::Ok => self.spork19 = *value,
            ChainOp::Restart(kind) => {
                // BL-837: a load keeps no lock, but Core keeps the conflict marks it set, so the
                // held lock is kept as a reset keeps it, read before a KILL loses anything; once,
                // over repeated loads.
                if let Some(cl) = self.best_cl
                    && self.indexed_conflicts()
                {
                    self.cl_flags_unsure = true;
                    if !self.cl_dropped.contains(&cl) {
                        self.cl_dropped.push(cl);
                    }
                }
                self.cl_enforced = None;
                if *kind == RestartKind::Clean && !self.down {
                    self.spork19_stored = self.spork19;
                }
                self.spork19 = self.spork19_stored;
                if *kind == RestartKind::Kill {
                    self.lose_dirty();
                }
                self.loaded();
            }
            _ => {}
        }
        // Indexed unmarked while spork 19 is off, against the best lock (DESIGN §4.2 (b)).
        if !self.spork19_active()
            && let Some(id) = own.or(own_header)
            && self.blocks[id].indexed()
            && cl_conflict(self, id)
        {
            self.cl_window_open = true;
            self.cl_flags_unsure = true;
        }
        self.observe_checked(step, own);
        self.down = step.post.down;
        if let Some(tip) = &step.post.tip
            && let Some(id) = self.id(&tip.1.0)
        {
            self.tip = id;
        }
        if let Some(best) = &step.post.best_header {
            self.best_header = best.as_ref().and_then(|b| self.id(&b.1.0));
        }
        if let ChainOp::ClRecord {
            height,
            hash,
            ..
        } = op
        {
            self.cl_seen.insert((*height, *hash));
        }
        if let Some(cl) = &step.post.best_cl {
            self.cl_height = cl.as_ref().map_or(-1, |c| c.0);
            let best = cl.as_ref().and_then(|c| self.id(&c.1.0));
            let known = best.filter(|&id| self.blocks[id].indexed());
            if known.is_some() || cl.is_none() || matches!(op, ChainOp::Restart(_)) {
                // DEC-1598: a lock replaced by one whose chain does not hold it, while an indexed
                // block conflicts with it, is kept as a reset keeps it.
                if !matches!(op, ChainOp::Restart(_))
                    && let Some(old) = self.best_cl
                    && known.is_none_or(|new| !self.is_ancestor(old, new))
                    && self.indexed_conflicts_with(old)
                    && !self.cl_dropped.contains(&old)
                {
                    self.cl_dropped.push(old);
                    self.cl_flags_unsure = true;
                }
                self.best_cl = known;
            }
        }
        // A reorg while spork 19 is off connects what `ConnectBlock`'s gated check lets through
        // (`v:2527`), whichever op moved the tip: the lock's conflicting blocks stay unmarked.
        if !self.down && !self.spork19_active() && self.active_conflicts() {
            self.cl_window_open = true;
            self.cl_flags_unsure = true;
        }
        self.observe_tips(step);
        // Last: the snapshot holds what this step taught the model, the tips' statuses included.
        let flushed = |c: &BTreeMap<String, u64>| c.values().any(|n| *n > 0);
        let full = flushed(&step.post.flush_delta.full);
        // A restart's flush is its teardown's (above), before the load moved the tip.
        if full && !matches!(op, ChainOp::Restart(_)) {
            self.coins_tips = self.written_tips(before, step);
        }
        // A write after a restart's load: the activation may have flushed anywhere on its way. A
        // clean restart while up loaded the teardown's tip; a KILL, or a restart while down,
        // loaded one of the tips the set holds.
        if let ChainOp::Restart(kind) = op
            && step.post.flushes.as_ref().is_some_and(|f| flushed(&f.full))
        {
            let tips = self.coins_tips.take();
            self.coins_tips = if *kind == RestartKind::Clean && !was_down {
                tips.zip(self.written_tips(before, step)).map(|(mut tips, written)| {
                    tips.extend(written);
                    tips.sort_unstable();
                    tips.dedup();
                    tips
                })
            } else {
                tips.and_then(|tips| self.loaded_tips(tips, step))
            };
        }
        if full || flushed(&step.post.flush_delta.index_only) {
            for b in self.blocks.iter_mut().filter(|b| b.indexed()) {
                b.durable = Some((b.held, b.invalid));
            }
        }
    }

    /// The tips a step with a full write may have written at, from tip `before` to its post's:
    /// every block the active tip passes through ([`World::passed_tips`]).
    fn written_tips(&self, before: usize, step: &Step) -> Option<Vec<usize>> {
        self.passed_tips(before, self.blocks[before].height, step)
    }

    /// A write after a load from one of `tips`: those tips, and every block the active tip
    /// passes through from the lowest of them on the post's chain; unknown when a tip is off the
    /// post's chain.
    fn loaded_tips(&self, mut tips: Vec<usize>, step: &Step) -> Option<Vec<usize>> {
        let post = self.tip;
        if !tips.iter().all(|&t| self.is_ancestor(t, post)) {
            return None;
        }
        let lowest = tips.iter().map(|&t| self.blocks[t].height).min()?;
        tips.extend(self.passed_tips(post, lowest, step)?);
        tips.sort_unstable();
        tips.dedup();
        Some(tips)
    }

    /// Every block the active tip may pass through in a step that starts on `base`'s chain at
    /// height `low` or above and ends at the post's tip, each a tip a full write may leave the
    /// coins database at: the post's chain and each chain the step tried and left, down to their
    /// forks with `base`'s chain, and `base`'s chain down to the lowest of them. A tried chain
    /// reaches a block its `BlockChecked` reports valid, or the parent of one it fails. Unknown
    /// when a chain meets `base`'s nowhere.
    fn passed_tips(&self, base: usize, low: u32, step: &Step) -> Option<Vec<usize>> {
        let tried = step.reply.checked.iter().filter_map(|c| {
            let id = self.id(&c.0.0)?;
            if c.1 == CheckResult::Valid {
                Some(id)
            } else {
                self.blocks[id].parent
            }
        });
        let mut low = low;
        let mut tips = Vec::new();
        for top in std::iter::once(self.tip).chain(tried) {
            let fork = self.ancestry(top).find(|&a| self.is_ancestor(a, base))?;
            low = low.min(self.blocks[fork].height);
            tips.extend(self.ancestry(top).take_while(|&a| a != fork));
            tips.push(fork);
        }
        tips.extend(self.ancestry(base).take_while(|&a| self.blocks[a].height >= low));
        tips.sort_unstable();
        tips.dedup();
        Some(tips)
    }

    /// `EnforceBlock`'s marks: up the lock's chain to the active one, each sibling is marked
    /// conflicting, with the active blocks it disconnects (`MarkConflictingBlock`).
    fn enforce_marks(&mut self, cl: usize) {
        let mut walk = cl;
        while !self.is_ancestor(walk, self.tip) {
            let Some(parent) = self.blocks[walk].parent else {
                return;
            };
            let siblings: Vec<usize> =
                self.children(parent).filter(|&c| c != walk && self.blocks[c].indexed()).collect();
            for s in siblings {
                if self.is_ancestor(s, self.tip) {
                    for a in self.ancestry(self.tip).take_while(|&a| a != s).collect::<Vec<_>>() {
                        self.blocks[a].invalid = true;
                    }
                }
                self.blocks[s].invalid = true;
            }
            walk = parent;
        }
    }

    /// A KILL: every index entry is as the last write left it, and one written since is gone.
    pub fn lose_dirty(&mut self) {
        for b in self.blocks.iter_mut().filter(|b| b.dirty()) {
            (b.held, b.invalid) = b.durable.unwrap_or((Held::Nothing, false));
        }
    }

    /// The best ChainLock is known and has not been enforced since it resolved.
    pub fn cl_due(&self) -> bool {
        !self.down && self.best_cl.is_some() && self.best_cl != self.cl_enforced
    }

    /// What `submitblock` of block `id` said of it.
    fn observe_block_reply(&mut self, id: usize, class: &ReplyClass) {
        let stage = self.blocks[id].mutation.map(|m| chain_operator(m).stage);
        if let ReplyClass::Reject(r) = class {
            self.consensus_failure |= marks_failure(r, stage);
        }
        let b = &mut self.blocks[id];
        let stored = match class {
            ReplyClass::Ok | ReplyClass::Inconclusive => true,
            // A known header marked failed or conflicting: `AcceptBlockHeader` stops, nothing
            // is stored.
            ReplyClass::Duplicate => !b.invalid,
            // An orphan stays deliverable: Core kept nothing of it.
            ReplyClass::Reject(r) if r.starts_with("prev-blk-not-found") => false,
            ReplyClass::Reject(r) if header_stage(r) => {
                b.refuse();
                false
            }
            // A new header's conflict (`AcceptBlockHeader`): indexed conflicting, no data. A
            // known header's is ConnectBlock's, below.
            ReplyClass::Reject(r) if r.starts_with("bad-chainlock") && !b.indexed() => {
                b.ensure_header();
                b.invalid = true;
                false
            }
            ReplyClass::Reject(_) => match stage {
                Some(Stage::Check) => {
                    b.refuse();
                    false
                }
                Some(Stage::Contextual) => {
                    b.ensure_header();
                    b.invalid = true;
                    false
                }
                // ConnectBlock's (or an unplanned) failure: stored and marked.
                _ => {
                    b.invalid = true;
                    true
                }
            },
            ReplyClass::Down => false,
        };
        if stored {
            self.store_data(id);
        }
    }

    /// What `submitheader` of block `id` said of it.
    fn observe_header_reply(&mut self, id: usize, class: &ReplyClass) {
        if let ReplyClass::Reject(r) = class
            && !r.starts_with("Must submit previous header")
        {
            self.consensus_failure |= marks_failure(r, None);
        }
        let b = &mut self.blocks[id];
        match class {
            // A block refused by `CheckBlock` before has a good header.
            ReplyClass::Ok if matches!(b.held, Held::Nothing | Held::Refused) => {
                b.held = Held::Header;
            }
            ReplyClass::Reject(r) if r.starts_with("Must submit previous header") => {}
            ReplyClass::Reject(r) if header_stage(r) => b.refuse(),
            // Indexed, and marked (`bad-chainlock`).
            ReplyClass::Reject(_) => {
                if b.held == Held::Nothing {
                    b.held = Held::Header;
                }
                b.invalid = true;
            }
            _ => {}
        }
    }

    /// What `BlockChecked` said of the blocks the op validated other than its `own` (connected
    /// in an activation: a failure there marks the block).
    fn observe_checked(&mut self, step: &Step, own: Option<usize>) {
        for Checked(hash, result, class, _) in &step.reply.checked {
            let id = self.id(&hash.0);
            // The op's own block's failure is its reply's (above). Of the others, an orphan
            // (`missing_prev`) and a mutated block are not marked (`InvalidBlockFound`), and a
            // ChainLock conflict is not a consensus failure.
            if *result == CheckResult::Invalid
                && id.is_none_or(|i| Some(i) != own)
                && !matches!(class.as_deref(), Some("chainlock" | "missing_prev" | "mutated"))
            {
                self.consensus_failure = true;
            }
            if let Some(id) = id.filter(|&id| Some(id) != own) {
                let b = &mut self.blocks[id];
                match result {
                    CheckResult::Invalid => {
                        b.invalid = true;
                        if b.held == Held::Nothing {
                            b.held = Held::Header;
                        }
                    }
                    CheckResult::Valid => b.invalid = false,
                    CheckResult::Error => {}
                }
            }
        }
    }

    /// A load (restore or RESTART): every stored entry's sequence id is 0 again.
    pub fn loaded(&mut self) {
        self.cl_seen.clear();
        self.spork19_loaded = self.spork19;
        self.cl_handler_enabled = false;
        self.cl_window_open = false;
        for b in &mut self.blocks {
            if b.held == Held::Data {
                b.seq = Seq::Loaded;
            }
        }
    }

    /// `getchaintips`' statuses refresh the model's validity of each tip.
    fn observe_tips(&mut self, step: &Step) {
        let Some(tips) = &step.post.tips else {
            return;
        };
        for row in tips {
            let (Some(hash), Some(status)) = (
                row.get("hash").and_then(|h| h.as_str()),
                row.get("status").and_then(|s| s.as_str()),
            ) else {
                continue;
            };
            let Some(hash) = display_hash(hash) else {
                continue;
            };
            if let Some(id) = self.id(&hash) {
                let b = &mut self.blocks[id];
                match status {
                    "invalid" | "conflicting" => b.invalid = true,
                    "active" | "valid-fork" | "valid-headers" => b.invalid = false,
                    _ => {}
                }
            }
        }
    }
}

/// A block's hash from its serialization (its header's X11).
pub fn block_id(world: &World, bytes: &[u8]) -> Option<usize> {
    let header: &[u8; 80] = bytes.get(..80)?.try_into().ok()?;
    world.id(&header_hash(header))
}

pub fn header_hash(header: &[u8; 80]) -> Hash {
    let mut cursor = &header[..];
    dash_consensus::decode_header(&mut cursor)
        .map(|h| *h.value().hash().as_bytes())
        .unwrap_or([0; 32])
}

/// Display hex (RPC order) to internal order.
pub fn display_hash(hex: &str) -> Option<Hash> {
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().rev().enumerate() {
        *o = u8::from_str_radix(hex.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

/// Rejects decided before the block reaches the index (`CheckBlockHeader`,
/// `ContextualCheckBlockHeader`, `AcceptBlockHeader`'s own checks): nothing is indexed.
/// (`bad-chainlock` is not one: Core indexes the block and marks it conflicting.)
pub fn header_stage(reason: &str) -> bool {
    const HEADER: &[&str] = &[
        "high-hash",
        "bad-diffbits",
        "time-too-old",
        "time-too-new",
        "bad-version",
        "bad-prevblk",
        "prev-blk-not-found",
        "duplicate-invalid",
        "duplicate",
        "checkpoint",
        "bad-fork-prior-to-checkpoint",
        "Block decode failed",
        "Block does not start with a coinbase",
    ];
    HEADER.iter().any(|p| reason.starts_with(p))
}

/// Whether reject `r` of a block (`stage`: its planted failure's) leaves a consensus failure
/// marked in the index: rule 2's "consensus failure". Not a ChainLock conflict
/// (`bad-chainlock`, `bad-prevblk-chainlock`) or its parent's (`bad-prevblk`), and not a reject
/// that indexes nothing: the header's own checks, an orphan, `CheckBlock`'s. A known entry
/// already failed (`duplicate-invalid`) marks nothing new: a consensus failure that marked it
/// counted then.
fn marks_failure(r: &str, stage: Option<Stage>) -> bool {
    !(r.starts_with("bad-chainlock")
        || header_stage(r)
        || matches!(stage, Some(Stage::Pow | Stage::Check)))
}

#[cfg(test)]
mod tests {
    use dash_oracle_sys::{ChainHash, ChainOp, CheckResult, Checked, Post, Reply};
    use serde_json::{Value, json};

    use dash_oracle_sys::RestartKind;

    use super::{Block, Held, Seq, World, header_hash};
    use crate::chain::core::{SPORK_19, SPORK_OFF, image_world};
    use crate::chain::step::Step;

    /// Core's step replying `outcome`, its POST up with the fields of `post` added.
    fn stepped(outcome: Value, post: Value) -> Step {
        let mut full = json!({
            "down": false,
            "flush_delta": {"full": {}, "index_only": {}},
            "tie": [],
        });
        for (k, v) in post.as_object().expect("an object") {
            full[k] = v.clone();
        }
        Step {
            reply: Reply {
                checked: Vec::new(),
                outcome: Ok(outcome),
            },
            records: Vec::new(),
            events: Vec::new(),
            post: serde_json::from_value(full).expect("a post"),
        }
    }

    fn spork_19(value: i64) -> ChainOp {
        ChainOp::Spork {
            id: SPORK_19,
            value,
        }
    }

    /// A BLOCK op for a new block on `parent` (an 80-byte header whose hash the world indexes),
    /// and Core's step for it: an `inconclusive` reply, nothing else.
    fn submitted(w: &mut World, parent: usize, nonce: u8) -> (usize, ChainOp, Step) {
        let mut header = [0u8; 80];
        header[0] = 4;
        header[4..36].copy_from_slice(&w.blocks[parent].hash);
        header[79] = nonce;
        let height = w.blocks[parent].height + 1;
        let id = w.add(Block {
            hash: header_hash(&header),
            parent: Some(parent),
            height,
            time: w.blocks[parent].time + 150,
            built: None,
            creates: Vec::new(),
            spends: Vec::new(),
            held: Held::Header,
            invalid: false,
            mutation: None,
            seq: Seq::Arrived,
            durable: None,
        });
        let post: Post = serde_json::from_value(json!({
            "down": false,
            "flush_delta": {"full": {}, "index_only": {}},
            "tie": [],
        }))
        .expect("a post");
        let step = Step {
            reply: Reply {
                checked: Vec::new(),
                outcome: Ok(Value::String("inconclusive".into())),
            },
            records: Vec::new(),
            events: Vec::new(),
            post,
        };
        (id, ChainOp::Block(header.to_vec()), step)
    }

    /// The child index gives what a scan of every block's parent gives, in the same order: on
    /// the image, on S2's line, and after blocks are added on forks and holds.
    #[test]
    fn children_index_matches_the_scan() {
        let scan = |w: &World, id: usize| -> Vec<usize> {
            w.blocks
                .iter()
                .enumerate()
                .filter(|(_, b)| b.parent == Some(id))
                .map(|(i, _)| i)
                .collect()
        };
        let same = |w: &World| {
            for id in 0..w.blocks.len() {
                assert_eq!(w.children(id).collect::<Vec<_>>(), scan(w, id), "block {id}");
            }
        };
        let mut w = image_world();
        same(&w);
        let below = w.blocks[w.tip].parent.expect("parent");
        for nonce in 1..=3 {
            let (id, ..) = submitted(&mut w, below, nonce);
            submitted(&mut w, id, nonce + 10);
        }
        let tip = w.tip;
        submitted(&mut w, tip, 30);
        same(&w);
        assert_eq!(w.children(below).count(), 4, "the tip and three siblings");
        let mut s2 = crate::chain::av::world();
        same(&s2);
        let tip = s2.tip;
        submitted(&mut s2, tip, 1);
        same(&s2);
        assert_eq!(s2.children(tip).count(), 2, "the line's block and a sibling");
    }

    #[test]
    fn rule_2_scope_records_a_block_submitted_under_a_failed_ancestor() {
        let mut w = image_world();
        let below = w.blocks[w.tip].parent.expect("parent");
        let (good, op, step) = submitted(&mut w, below, 1);
        w.observe(&op, &step);
        assert!(!w.stored_under_failed.contains(&good), "its ancestors are good");
        let (failed, op, step) = submitted(&mut w, below, 2);
        w.observe(&op, &step);
        w.blocks[failed].invalid = true;
        let (child, op, step) = submitted(&mut w, failed, 3);
        let (grandchild, op2, step2) = submitted(&mut w, child, 4);
        w.observe(&op, &step);
        w.observe(&op2, &step2);
        assert!(w.stored_under_failed.contains(&child), "its parent is failed");
        assert!(w.stored_under_failed.contains(&grandchild), "an ancestor is failed");
        assert!(!w.stored_under_failed.contains(&failed), "the failed block itself is not");
    }

    /// 4f2 test 22: spork 19 is active while its value is before the mock clock
    /// (`spork.cpp:163-169`); a KILL loads the last store, a clean restart stores it first, a
    /// restart of a down session stores nothing (O2b D34).
    #[test]
    fn spork_19_is_active_by_cores_rule() {
        let mut w = image_world();
        assert_eq!((w.spork19, w.spork19_active()), (0, true), "the image's spork 19 is on");
        let ok = || stepped(json!(true), json!({}));
        w.observe(&spork_19(SPORK_OFF), &stepped(json!(false), json!({})));
        assert!(w.spork19_active(), "an unsigned update changes nothing");
        w.observe(&spork_19(SPORK_OFF), &ok());
        assert!(!w.spork19_active());
        let clock = w.mocktime;
        w.mocktime = SPORK_OFF + 1;
        assert!(w.spork19_active(), "on once the clock is past the value");
        w.mocktime = clock;
        let restart = |kind| ChainOp::Restart(kind);
        w.observe(&restart(RestartKind::Kill), &stepped(Value::Null, json!({})));
        assert_eq!(w.spork19, 0, "a KILL loads the last store");
        w.observe(&spork_19(SPORK_OFF), &ok());
        w.observe(&restart(RestartKind::Clean), &stepped(Value::Null, json!({})));
        assert_eq!((w.spork19, w.spork19_stored), (SPORK_OFF, SPORK_OFF), "a clean one stores it");
        w.observe(&spork_19(0), &ok());
        w.down = true;
        w.observe(&restart(RestartKind::Clean), &stepped(Value::Null, json!({})));
        assert_eq!(w.spork19, SPORK_OFF, "a down session's teardown stores nothing new");
        assert_eq!(w.spork19_loaded, SPORK_OFF);
    }

    /// 4f2 test 23: an enforcement while the handler is disabled marks nothing
    /// (`chainlock/handler.cpp:273-277`) and leaves the lock's window open; with the handler
    /// enabled, the same enforcement marks the active chain that conflicts with the lock.
    #[test]
    fn an_enforce_while_the_handler_is_off_marks_nothing() {
        let mut w = image_world();
        let below = w.blocks[w.tip].parent.expect("parent");
        let (sibling, op, step) = submitted(&mut w, below, 1);
        w.observe(&op, &step);
        w.best_cl = Some(sibling);
        let tip = w.tip;
        let enforced = stepped(Value::Null, json!({}));
        let mut off = w.clone();
        off.spork19 = SPORK_OFF;
        off.observe(&ChainOp::ClEnforce, &enforced);
        assert!(!off.blocks[tip].invalid, "nothing marked while spork 19 is off");
        assert!(off.cl_window_open && !off.cl_handler_enabled);
        let mut on = w;
        on.cl_handler_enabled = true;
        on.observe(&ChainOp::ClEnforce, &enforced);
        assert!(on.blocks[tip].invalid, "the active block the lock conflicts with is marked");
        assert!(!on.cl_window_open);
    }

    /// 4f2 test 25: the conflict marks become unsure at a reset that drops a lock some indexed
    /// block conflicts with, marked in the model or not, and the lock is remembered
    /// (DESIGN §4.2 (a), r2), when a block the best lock conflicts with is
    /// indexed while spork 19 is off ((b)), and when an enforcement marks nothing while one is
    /// indexed off the active chain (a lock on the tip recorded over it while the spork was off,
    /// (b'); the active chain does not conflict, so only the enforcement's scan sees it); a reset
    /// with nothing conflicting leaves them sure and remembers nothing.
    #[test]
    fn a_reset_over_marks_or_an_unmarked_conflict_makes_the_marks_unsure() {
        let mut clean = image_world();
        clean.best_cl = Some(clean.tip);
        clean.cl_handler_enabled = false;
        clean.check_active_state();
        assert!(clean.best_cl.is_none() && !clean.cl_flags_unsure, "nothing conflicted");
        assert!(clean.cl_dropped.is_empty(), "nothing conflicted");
        let mut w = image_world();
        let below = w.blocks[w.tip].parent.expect("parent");
        let (sibling, op, step) = submitted(&mut w, below, 1);
        w.observe(&op, &step);
        w.best_cl = Some(sibling);
        w.cl_handler_enabled = false;
        for invalid in [false, true] {
            let mut reset = w.clone();
            let tip = reset.tip;
            reset.blocks[tip].invalid = invalid;
            reset.check_active_state();
            let case = format!("(a), the tip invalid {invalid}");
            assert!(reset.best_cl.is_none() && reset.cl_flags_unsure, "{case}");
            assert_eq!(reset.cl_dropped, [sibling], "{case}");
        }
        let mut off = image_world();
        off.best_cl = Some(off.tip);
        off.spork19 = SPORK_OFF;
        let (_, op, step) = submitted(&mut off, below, 2);
        off.observe(&op, &step);
        assert!(off.cl_flags_unsure && off.cl_window_open, "(b)");
        let mut late = image_world();
        let (_, op, step) = submitted(&mut late, below, 3);
        late.observe(&op, &step);
        assert!(!late.cl_flags_unsure, "indexed before the lock");
        late.best_cl = Some(late.tip);
        late.spork19 = SPORK_OFF;
        assert!(!late.active_conflicts(), "the lock is on the active chain");
        late.observe(&ChainOp::ClEnforce, &stepped(Value::Null, json!({})));
        assert!(late.cl_flags_unsure && late.cl_window_open, "recorded while off");
    }

    /// A step with a full write whose post's tip is block `tip`, and `checked` its
    /// `BlockChecked`s; after a load (`restart`), a write since the load.
    fn written(w: &World, tip: usize, checked: &[(usize, CheckResult)], restart: bool) -> Step {
        let hex = |id: usize| -> String {
            w.blocks[id].hash.iter().rev().map(|b| format!("{b:02x}")).collect()
        };
        let none = json!({"full": {}, "index_only": {}});
        let some = json!({"full": {"always": 1}, "index_only": {}});
        let (delta, flushes) = if restart {
            (none.clone(), some)
        } else {
            (some, none)
        };
        let mut step = stepped(
            Value::Null,
            json!({"tip": [w.blocks[tip].height, hex(tip)], "flush_delta": delta, "flushes": flushes}),
        );
        step.reply.checked = checked
            .iter()
            .map(|&(id, r)| {
                let hash: ChainHash = serde_json::from_value(json!(hex(id))).expect("a hash");
                Checked(hash, r, None, None)
            })
            .collect();
        step
    }

    fn tips(w: &World) -> Option<Vec<usize>> {
        w.coins_tips.clone().map(|mut t| {
            t.sort_unstable();
            t
        })
    }

    /// T40 (DEC-1598, DEC-1602; steps applied to a constructed world whose tip is unmarked; no
    /// generator, no oracle): the tips the coins database may have been written at follow each
    /// full write over every block the active tip passed through: what the step disconnected and
    /// connected, each chain it tried and left, and their forks. A write after a load extends
    /// them from the lowest, whichever restart loaded (clean, KILL, or one while down); from a
    /// tip off the post's chain it leaves them unknown.
    #[test]
    fn coins_tips_follow_full_writes_and_disconnects() {
        let w = image_world();
        let tip = w.tip;
        assert_eq!(tips(&w), Some(vec![tip]), "from_chain");
        let below = w.blocks[tip].parent.expect("parent");
        let below2 = w.blocks[below].parent.expect("parent");
        let sorted = |mut v: Vec<usize>| {
            v.sort_unstable();
            Some(v)
        };
        let add = |w: &mut World, n: u8, parent: usize| {
            let (id, op, step) = submitted(w, parent, n);
            w.observe(&op, &step);
            w.blocks[id].held = Held::Data;
            id
        };
        // An INVALIDATE two deep.
        let mut back = w.clone();
        let hash = back.blocks[below].hash;
        back.observe(&ChainOp::Invalidate(hash), &written(&w, below2, &[], false));
        assert_eq!(tips(&back), sorted(vec![below2, below, tip]), "disconnected two");
        // A reorg to a longer sibling.
        let mut reorg = w.clone();
        let s1 = add(&mut reorg, 1, below);
        let s2 = add(&mut reorg, 2, s1);
        let step = written(&reorg, s2, &[], false);
        reorg.observe(&ChainOp::Activate, &step);
        assert_eq!(tips(&reorg), sorted(vec![s2, s1, tip, below]), "reorg");
        // Connecting three.
        let mut up = w.clone();
        let b1 = add(&mut up, 3, tip);
        let b2 = add(&mut up, 4, b1);
        let b3 = add(&mut up, 5, b2);
        let step = written(&up, b3, &[], false);
        up.observe(&ChainOp::Activate, &step);
        assert_eq!(tips(&up), sorted(vec![tip, b1, b2, b3]), "connected");
        // (m) a chain tried and left, beside another chain's tip: its fork and the blocks down to
        // it, and the failed block's parent.
        let mut m = w.clone();
        let x1 = add(&mut m, 6, below2);
        let x2 = add(&mut m, 7, x1);
        let x3 = add(&mut m, 8, x2);
        let y = add(&mut m, 9, tip);
        let checked =
            [(x1, CheckResult::Valid), (x2, CheckResult::Valid), (x3, CheckResult::Invalid)];
        let step = written(&m, y, &checked, false);
        m.observe(&ChainOp::Activate, &step);
        assert_eq!(tips(&m), sorted(vec![tip, y, x1, x2, below, below2]), "(m)");
        // (t) a chain tried and left, back on the same tip.
        let mut t = w.clone();
        let t1 = add(&mut t, 11, below2);
        let t2 = add(&mut t, 12, t1);
        let step = written(&t, tip, &[(t1, CheckResult::Valid), (t2, CheckResult::Invalid)], false);
        t.observe(&ChainOp::Activate, &step);
        assert_eq!(tips(&t), sorted(vec![tip, below, below2, t1]), "(t)");
        // (t) with only the failed block: its parent is the chain tried, here the fork itself.
        let mut failed = w.clone();
        let f1 = add(&mut failed, 15, below2);
        let step = written(&failed, tip, &[(f1, CheckResult::Invalid)], false);
        failed.observe(&ChainOp::Activate, &step);
        assert_eq!(tips(&failed), sorted(vec![tip, below, below2]), "(t), a failed block alone");
        // An index-only write leaves them.
        let mut index = up.clone();
        let mut step = written(&index, b3, &[], false);
        step.post.flush_delta.full.clear();
        step.post.flush_delta.index_only.insert("always".into(), 1);
        index.observe(&ChainOp::Activate, &step);
        assert_eq!(tips(&index), tips(&up), "index only");
        // A clean restart while up: the teardown's tip alone.
        let mut clean = up.clone();
        let step = stepped(Value::Null, json!({}));
        clean.observe(&ChainOp::Restart(RestartKind::Clean), &step);
        assert_eq!(tips(&clean), Some(vec![b3]), "a clean restart");
        // A clean restart while up whose load connects one more, then writes.
        let mut clean = up.clone();
        let c4 = add(&mut clean, 13, b3);
        let step = written(&clean, c4, &[(c4, CheckResult::Valid)], true);
        clean.observe(&ChainOp::Restart(RestartKind::Clean), &step);
        assert_eq!(tips(&clean), sorted(vec![b3, c4]), "a clean restart, then a write");
        // (n) a KILL whose load connects from below every tip, then writes: from the lowest tip,
        // across the gap between them.
        let mut n = up.clone();
        n.coins_tips = Some(vec![b1, b3]);
        let b4 = add(&mut n, 10, b3);
        let step = written(&n, b4, &[(b4, CheckResult::Valid)], true);
        n.observe(&ChainOp::Restart(RestartKind::Kill), &step);
        assert_eq!(tips(&n), sorted(vec![b1, b2, b3, b4]), "(n)");
        // A refused load, then a load that writes: from the lowest tip, as a KILL's, across the
        // gap between them.
        let mut refused = up.clone();
        refused.coins_tips = Some(vec![b1, b3]);
        let d4 = add(&mut refused, 14, b3);
        let step = stepped(Value::Null, json!({"down": true}));
        refused.observe(&ChainOp::Restart(RestartKind::Kill), &step);
        assert!(refused.down, "the refused load");
        assert_eq!(tips(&refused), Some(vec![b1, b3]), "the refused load");
        let step = written(&refused, d4, &[(d4, CheckResult::Valid)], true);
        refused.observe(&ChainOp::Restart(RestartKind::Clean), &step);
        assert_eq!(tips(&refused), sorted(vec![b1, b2, b3, d4]), "a load after a refused one");
        // A KILL's write after a load from a tip off the post's chain: unknown.
        let mut off = reorg.clone();
        let step = written(&off, s2, &[], true);
        off.observe(&ChainOp::Restart(RestartKind::Kill), &step);
        assert_eq!(tips(&off), None, "a KILL from off the post's chain");
        // A KILL with no write after its load keeps them.
        let mut quiet = reorg.clone();
        quiet.observe(&ChainOp::Restart(RestartKind::Kill), &stepped(Value::Null, json!({})));
        assert_eq!(tips(&quiet), tips(&reorg), "no write after the load");
    }

    /// POST `best_cl` naming block `id`.
    fn lock_at(w: &World, id: usize) -> Value {
        let hex: String = w.blocks[id].hash.iter().rev().map(|b| format!("{b:02x}")).collect();
        json!({"best_cl": [w.blocks[id].height, hex]})
    }

    /// T41 (DEC-1598): a post that replaces the best lock with one whose chain does not hold it,
    /// while an indexed block conflicts with it, keeps it in `cl_dropped` and makes the flags
    /// unsure, once (also when the new lock is below it on its own chain, whose chain then does
    /// not hold it); a lock whose chain holds it, no indexed conflict, or a post naming a lock on
    /// a block the model does not hold indexed (the best lock stays) keeps nothing. A restart's
    /// load keeps it once, by BL-837's push (T20).
    #[test]
    fn a_replaced_lock_is_kept_while_an_indexed_block_conflicts_with_it() {
        let mut w = image_world();
        let tip = w.tip;
        let below = w.blocks[tip].parent.expect("parent");
        let (sibling, op, step) = submitted(&mut w, below, 1);
        w.observe(&op, &step);
        let (above, op, step) = submitted(&mut w, tip, 2);
        w.observe(&op, &step);
        w.best_cl = Some(tip);
        let post = |w: &mut World, id: usize| {
            let step = stepped(Value::Null, lock_at(w, id));
            w.observe(&ChainOp::Activate, &step);
        };
        let mut off = w.clone();
        post(&mut off, sibling);
        assert_eq!(off.best_cl, Some(sibling));
        assert_eq!(off.cl_dropped, [tip], "replaced off its chain");
        assert!(off.cl_flags_unsure, "replaced off its chain");
        let mut again = off.clone();
        again.best_cl = Some(tip);
        post(&mut again, sibling);
        assert_eq!(again.cl_dropped, [tip], "already kept");
        let mut on = w.clone();
        post(&mut on, above);
        assert_eq!(on.best_cl, Some(above));
        assert!(on.cl_dropped.is_empty() && !on.cl_flags_unsure, "its chain holds the old lock");
        let mut back = w.clone();
        post(&mut back, below);
        assert_eq!(back.best_cl, Some(below));
        assert_eq!(back.cl_dropped, [tip], "replaced below it, on its own chain");
        assert!(back.cl_flags_unsure, "replaced below it, on its own chain");
        let mut cleared = w.clone();
        let step = stepped(Value::Null, json!({"best_cl": null}));
        cleared.observe(&ChainOp::Activate, &step);
        assert_eq!(cleared.cl_dropped, [tip], "replaced by none");
        assert!(cleared.cl_flags_unsure, "replaced by none");
        let mut loaded = w.clone();
        let step = stepped(Value::Null, lock_at(&loaded, sibling));
        loaded.observe(&ChainOp::Restart(RestartKind::Clean), &step);
        assert_eq!(loaded.best_cl, Some(sibling));
        assert_eq!(loaded.cl_dropped, [tip], "a load keeps it once");

        let mut quiet = image_world();
        let fork = quiet.add(Block {
            hash: [0xf1; 32],
            ..quiet.blocks[tip].clone()
        });
        quiet.blocks[fork].parent = Some(below);
        quiet.blocks[fork].held = Held::Nothing;
        let (high, op, step) = submitted(&mut quiet, fork, 3);
        quiet.observe(&op, &step);
        quiet.best_cl = Some(tip);
        post(&mut quiet, high);
        assert_eq!(quiet.best_cl, Some(high));
        assert!(quiet.cl_dropped.is_empty() && !quiet.cl_flags_unsure, "no indexed conflict");

        for known in [false, true] {
            let mut stays = w.clone();
            let step = if known {
                let unsent = stays.add(Block {
                    hash: [0xf2; 32],
                    held: Held::Nothing,
                    ..stays.blocks[sibling].clone()
                });
                stepped(Value::Null, lock_at(&stays, unsent))
            } else {
                let height = stays.blocks[tip].height + 1;
                stepped(Value::Null, json!({"best_cl": [height, "ee".repeat(32)]}))
            };
            stays.observe(&ChainOp::Activate, &step);
            assert_eq!(stays.best_cl, Some(tip), "known {known}: the best lock stays");
            assert!(stays.cl_dropped.is_empty(), "known {known}");
        }
    }

    /// T20 (BL-837): a restart's load, clean or KILL, on a world whose tip is unmarked keeps the
    /// lock the model held in `cl_dropped` and makes the flags unsure while an indexed block
    /// conflicts with it, whatever lock the post reads and whether spork 19 is on or off at the
    /// load, and once over repeated loads (a refused load, then a restart while down); with no
    /// indexed conflict, nothing.
    #[test]
    fn a_load_with_a_held_lock_over_an_indexed_conflict_drops_it() {
        let mut w = image_world();
        let tip = w.tip;
        let below = w.blocks[tip].parent.expect("parent");
        let mut quiet = w.clone();
        quiet.best_cl = Some(tip);
        let (_, op, step) = submitted(&mut w, below, 1);
        w.observe(&op, &step);
        w.best_cl = Some(tip);
        assert!(w.indexed_conflicts() && !quiet.indexed_conflicts());
        assert!(w.spork19_active(), "on, and off at SPORK_OFF");
        for kind in [RestartKind::Clean, RestartKind::Kill] {
            for spork in [w.spork19, SPORK_OFF] {
                for post in [json!({"best_cl": null}), lock_at(&w, tip)] {
                    let case = format!("{kind:?}, spork 19 at {spork}, {post}");
                    let mut held = w.clone();
                    held.spork19 = spork;
                    held.observe(&ChainOp::Restart(kind), &stepped(Value::Null, post.clone()));
                    assert_eq!(held.cl_dropped, [tip], "{case}");
                    assert!(held.cl_flags_unsure, "{case}");
                    let mut none = quiet.clone();
                    none.spork19 = spork;
                    none.observe(&ChainOp::Restart(kind), &stepped(Value::Null, post));
                    assert!(
                        none.cl_dropped.is_empty() && !none.cl_flags_unsure,
                        "{case}: no conflict"
                    );
                }
            }
        }
        // A refused load leaves the session down and the model's lock as it was; the restart
        // while down that follows keeps it, once.
        for kind in [RestartKind::Clean, RestartKind::Kill] {
            let mut twice = w.clone();
            let refused = stepped(Value::Null, json!({"down": true}));
            twice.observe(&ChainOp::Restart(RestartKind::Clean), &refused);
            assert!(twice.down && twice.best_cl == Some(tip), "the refused load");
            assert_eq!(twice.cl_dropped, [tip], "the refused load");
            twice.observe(&ChainOp::Restart(kind), &stepped(Value::Null, json!({})));
            assert_eq!(twice.cl_dropped, [tip], "{kind:?} after a refused load: kept once");
            assert!(twice.cl_flags_unsure, "{kind:?} after a refused load");
        }
    }

    /// 4f2 test 27 (OD-L3-128 C-B): the window enforcement needs the fact only an inactive spork
    /// sets, [`World::cl_window_open`]; the lock's geometry alone, an active chain conflicting
    /// with an enforced lock while spork 19 is on, is not due.
    #[test]
    fn the_window_enforcement_needs_an_inactive_spork_fact() {
        let mut w = image_world();
        let below = w.blocks[w.tip].parent.expect("parent");
        let (lock, op, step) = submitted(&mut w, below, 4);
        w.observe(&op, &step);
        w.best_cl = Some(lock);
        w.cl_enforced = w.best_cl;
        w.cl_handler_enabled = true;
        assert!(w.spork19_active() && w.active_conflicts(), "the geometry holds");
        assert!(!w.cl_due() && !w.cl_window_due(), "not due on geometry alone");
        w.cl_window_open = true;
        assert!(w.cl_window_due(), "due once the spork was off for it");
    }

    /// 4f2 test 24: the model check refuses a post whose spork 19 or best ChainLock height is
    /// not what the model predicts.
    #[test]
    fn the_model_check_refuses_a_post_that_disagrees() {
        let w = image_world();
        let set = ChainOp::SetTime(w.mocktime);
        let sporks = |value: i64, active: bool| json!({"sporks": [[SPORK_19, value, active]]});
        assert!(w.check_spork_cl(&set, &stepped(Value::Null, sporks(0, true))).is_ok());
        let wrong = w.check_spork_cl(&set, &stepped(Value::Null, sporks(SPORK_OFF, false)));
        assert!(wrong.expect_err("refused").starts_with("model: spork19"));
        let late = ChainOp::SetTime(SPORK_OFF + 1);
        let off = w.check_spork_cl(&spork_19(SPORK_OFF), &stepped(json!(true), sporks(0, true)));
        assert!(off.is_err(), "the update's value is predicted");
        let mut w2 = w.clone();
        w2.spork19 = SPORK_OFF;
        assert!(w2.check_spork_cl(&late, &stepped(Value::Null, sporks(SPORK_OFF, true))).is_ok());
        let tip = &w.blocks[w.tip];
        let mut display = tip.hash;
        display.reverse();
        let hex: String = display.iter().map(|b| format!("{b:02x}")).collect();
        let record = ChainOp::ClRecord {
            height: tip.height as i32,
            hash: tip.hash,
            sig: [0; 96],
        };
        let at = |best: Value| json!({"best_cl": best});
        let accepted = |post| w.check_spork_cl(&record, &stepped(json!("accepted"), post));
        assert!(accepted(at(json!([tip.height, hex]))).is_ok());
        let unmoved = accepted(at(Value::Null));
        assert!(unmoved.expect_err("refused").starts_with("model: cl height"));
    }
}
