//! The generator's world model (O2b §3.2): the block tree it built, the coins each block
//! creates and spends, what the node holds of each block, and the clock. It is updated from
//! **Core's** step after every op (O2b-3 P2), so a corpus resolves to the same ops whatever the
//! second system is.
//!
//! It is advisory: the model picks targets and predicts replies (the generator's expectations)
//! and ties (`ties`); Core's report is authoritative for both.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use dash_oracle_sys::{ChainOp, CheckResult, Checked, RestartKind};

use super::build::{Built, Lock, MATURITY, MAX_FUTURE};
use super::generate::cl_conflict;
use super::mutate::{OPERATORS, Stage};
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
    /// The mutation operator it was built with ([`super::mutate::OPERATORS`]): never presumed
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
    /// The active tip the coins database last had written: a KILL's load starts from it.
    pub coins_tip: usize,
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
}

impl World {
    /// A world holding `blocks` (genesis first, a chain), all loaded with data.
    pub fn from_chain(blocks: Vec<Block>, mocktime: i64) -> World {
        let by_hash = blocks.iter().enumerate().map(|(i, b)| (b.hash, i)).collect();
        let tip = blocks.len() - 1;
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
            coins_tip: tip,
            down: false,
            tag: 0,
            consensus_failure: false,
            own_invalidates: BTreeSet::new(),
            stored_under_failed: BTreeSet::new(),
        }
    }

    pub fn id(&self, hash: &Hash) -> Option<usize> {
        self.by_hash.get(hash).copied()
    }

    pub fn add(&mut self, block: Block) -> usize {
        let id = self.blocks.len();
        self.by_hash.insert(block.hash, id);
        self.blocks.push(block);
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

    pub fn children(&self, id: usize) -> impl Iterator<Item = usize> + '_ {
        self.blocks.iter().enumerate().filter(move |(_, b)| b.parent == Some(id)).map(|(i, _)| i)
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

    /// Fold Core's step for `op` in.
    pub fn observe(&mut self, op: &ChainOp, step: &Step) {
        let class = step.class(op);
        // What a load reads back: a clean restart of an up session flushes its tip at teardown;
        // a KILL, or a restart of a down session, writes nothing. The load's own activation
        // does not flush, so `post.tip` is not the coins database's.
        if *op == ChainOp::Restart(RestartKind::Clean) && !self.down {
            self.coins_tip = self.tip;
        }
        // The op's own block: its reply decides its state, not its BlockChecked.
        let mut own = None;
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
                if let Some(id) = self.id(&header_hash(header)) {
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
            ChainOp::ClEnforce => {
                self.cl_enforced = self.best_cl;
                if let Some(cl) = self.best_cl {
                    self.enforce_marks(cl);
                }
            }
            ChainOp::Restart(kind) => {
                self.cl_enforced = None;
                if *kind == RestartKind::Kill {
                    self.lose_dirty();
                }
                self.loaded();
            }
            _ => {}
        }
        self.observe_checked(step, own);
        self.down = step.post.down;
        if let Some(tip) = &step.post.tip
            && let Some(id) = self.id(&tip.1.0)
        {
            self.tip = id;
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
                self.best_cl = known;
            }
        }
        self.observe_tips(step);
        // Last: the snapshot holds what this step taught the model, the tips' statuses included.
        let flushed = |c: &BTreeMap<String, u64>| c.values().any(|n| *n > 0);
        let full = flushed(&step.post.flush_delta.full);
        // A restart's flush is its teardown's (above), before the load moved the tip.
        if full && !matches!(op, ChainOp::Restart(_)) {
            self.coins_tip = self.tip;
        }
        if full || flushed(&step.post.flush_delta.index_only) {
            for b in self.blocks.iter_mut().filter(|b| b.indexed()) {
                b.durable = Some((b.held, b.invalid));
            }
        }
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
        let stage = self.blocks[id].mutation.map(|m| OPERATORS[m].stage);
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
    use dash_oracle_sys::{ChainOp, Post, Reply};
    use serde_json::{Value, json};

    use super::{Block, Held, Seq, World, header_hash};
    use crate::chain::core::image_world;
    use crate::chain::step::Step;

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
}
