//! [`ChainSut`]: CH2's chainstate profile on ST3a's harness (CH2 §6.4), the toy's
//! (`dashd_store::faulty::toy`) protocol replaced by CH2's real code.
//!
//! - V: the [`CoinsCache`] and a [`BlockOverlay`] per block (a mini connect: `HaveInputs`, the
//!   spends and adds, no scripts), the undo data and index entries, the stub evo fold, and the
//!   effects. Its flush side is [`FlushControl`] over the [`InlineDriver`], rebuilt for each op,
//!   so each of W's store calls is a world event.
//! - Startup: [`ChainstateOpen`], the block index loaded into an arena, [`Recovery`] stepped to
//!   its end, then activation (Core's first `ActivateBestChain`) reading blocks and undo data from
//!   the block files.
//! - The checks after a reopen are the toy's (ST3a §5.1): R1–R6, R9, R13, and R2 on the booted
//!   stores; FA2 and FA7 on the forward run.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dash_chain::{
    BlockIndexRecord, BlockRef, FlatFilePos as IndexPos, IndexAccess, IndexWrite, Status,
};
use dash_consensus::BlockId;
use dash_consensus::coins::BlockUndoRef;
use dash_consensus::primitives::{BlockHash, BlockHeader};
use dashd_store::faulty::InvariantId::{
    self, FA2, Model, R0, R1, R2, R3, R4, R5, R6, R9, R11a, R11b, R11c, R13,
};
use dashd_store::faulty::{
    Abort, FatalCause, FaultyEngine, Hash, Mark, RefExtractor, ScriptCheck, Sut, Trace, TraceEntry,
    Violation, World, allowed_tips,
};
use dashd_store::flatfile::{BlockFileInfo, FlatFileError, FlatFilePos};
use dashd_store::{Commit, KvBatch, KvEngine as _, KvRead, StoreError};

use super::fa;
use super::real::{RealConnect, fatal_abort};
use super::reference::{Blocks, ChainRef, evo_fold};
use super::stores::{
    Ctl, Db, Engines, Files, K_STATE, LOAD_FAILED, OPEN_FAILED, Refusal, Stores, StubDelta,
    StubEvo, cache_at, cf, crash_roll, file_infos, io_failure,
};
use super::subs::{SUB_NAME, SharedIndex, SubMode, Subs};
use super::synth::{GenBlock, RECORD_HEADER, T0};
use super::world::{WorldObserver, hash, read_failed};
use super::{
    BLOCK_READ_FAILED, BLOCK_WRITE_FAILED, BLOCKS_DIR, CHAIN_STORE, ChainConfig, ChainOp,
    ChainRefs, DISCONNECT_FAILED, EFFECT_WRITTEN, INVALID_TIP, PATH_ACCEPT, PATH_CONNECT,
    PATH_DISCONNECT, PATH_LOAD, PATH_LOAD_INDEX, UNDO_WRITE_FAILED, WIPE_BEGUN, common_prefix,
    mini, stores,
};
use crate::chain::{ConnectOutcome, DisconnectResult};
use crate::coins::{BlockOverlay, CoinsCache, CoinsTable, OutPointKey};
use crate::flush::{
    FilesPruneHost, FlushCx, FlushIndex, FlushStateMode, FlushStep, LogCat, NoPrune, PruneHost,
    arm_and_take_without_tip, log,
};
use crate::planted::planted;
use crate::store::{CHAINSTATE_SCHEMA, Fatal, wipe_chainstate};

/// The presumed effect's key in `llmq_snapshot`.
const K_SNAP: &[u8] = b"snap";
/// The independent effects' keys, `i:<height>`, in `llmq_snapshot` beside [`K_SNAP`]: the one
/// effect column family (CH7 Q1).
const K_EFFECT: &str = "i:";

// Core: chainparams.cpp:875 (regtest's `nPruneAfterHeight` under `-fastprune`)
const PRUNE_AFTER_HEIGHT: u64 = 100;

/// A bound on the subscriber's catch-up at a boot.
const SYNC_STEPS: u32 = 100_000;

// FA7's marks (CH2 §6.4): the harness has no verdict, event or status marks, so V logs them.
/// A block's verdict, as a peer or `submitblock` would see it.
pub(super) const VERDICT: &str = "verdict ";
/// A block's validity status changed in memory.
pub(super) const STATUS: &str = "status ";
/// `K`'s corruption, written.
pub(super) const CORRUPTED: &str = "corrupted coin of ";
/// A connect whose fetch reads `K`'s coin from the database (not cached): FA7 starts here, as at
/// an injected read fault.
pub(super) const CORRUPT_READ: &str = "corrupt coin read by ";

/// One block-index entry, as V holds it and `block_index` stores it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub hash: BlockHash,
    pub prev: Option<BlockId>,
    pub height: u32,
    pub status: Status,
    pub n_tx: u32,
    pub file: Option<i32>,
    pub data_pos: u32,
    pub undo_pos: u32,
    pub header: BlockHeader,
}

impl Entry {
    fn record(&self) -> BlockIndexRecord {
        BlockIndexRecord {
            height: self.height,
            status: self.status,
            n_tx: self.n_tx,
            file: self.file,
            data_pos: self.status.has(Status::HAVE_DATA).then_some(self.data_pos),
            undo_pos: self.status.has(Status::HAVE_UNDO).then_some(self.undo_pos),
            header: self.header,
        }
    }

    fn has(&self, mask: u32) -> bool {
        self.status.has(mask)
    }

    fn data(&self) -> FlatFilePos {
        FlatFilePos {
            file: self.file.unwrap_or(0) as u32,
            pos: self.data_pos,
        }
    }

    fn undo(&self) -> FlatFilePos {
        FlatFilePos {
            file: self.file.unwrap_or(0) as u32,
            pos: self.undo_pos,
        }
    }
}

/// The block index as an arena (CH1-node's would be the `HeaderChain`).
#[derive(Clone, Default)]
pub(crate) struct Arena {
    entries: Vec<Entry>,
    by_hash: HashMap<BlockHash, BlockId>,
    /// The structure the subscriber's hub and driver read, kept as entries are pushed.
    mirror: Option<SharedIndex>,
}

impl Arena {
    fn get(&self, id: BlockId) -> Option<&Entry> {
        self.entries.get(id.0 as usize)
    }

    fn by_hash(&self, h: &BlockHash) -> Option<&Entry> {
        self.by_hash.get(h).and_then(|id| self.get(*id))
    }

    fn by_hash_mut(&mut self, h: &BlockHash) -> &mut Entry {
        let id = self.by_hash[h];
        &mut self.entries[id.0 as usize]
    }

    pub(super) fn block_ref(&self, h: &BlockHash) -> BlockRef {
        let id = self.by_hash[h];
        BlockRef {
            id,
            hash: *h,
            height: self.entries[id.0 as usize].height,
        }
    }

    pub(super) fn push(&mut self, e: Entry) -> BlockId {
        let id = BlockId(self.entries.len() as u32);
        self.by_hash.insert(e.hash, id);
        if let Some(m) = &self.mirror {
            m.push(id, e.hash, e.prev, e.height, e.has(Status::HAVE_DATA));
        }
        self.entries.push(e);
        id
    }

    /// A mirror of the entries, kept from now on.
    pub(super) fn mirror(&mut self) -> SharedIndex {
        let m = SharedIndex::default();
        for (i, e) in self.entries.iter().enumerate() {
            m.push(BlockId(i as u32), e.hash, e.prev, e.height, e.has(Status::HAVE_DATA));
        }
        self.mirror = Some(m.clone());
        m
    }

    /// The hashes from genesis to `tip`.
    pub(super) fn path(&self, tip: Option<&BlockHash>) -> Vec<BlockHash> {
        let mut out = Vec::new();
        let mut cur = tip.and_then(|t| self.by_hash.get(t)).copied();
        while let Some(id) = cur {
            let e = &self.entries[id.0 as usize];
            out.push(e.hash);
            cur = e.prev;
        }
        out.reverse();
        out
    }

    /// The index records from genesis to `h`, as `block_index` stores them: what the real connect
    /// and disconnect load into their header chain.
    fn records_to(&self, h: &BlockHash) -> Vec<(BlockHash, Vec<u8>)> {
        let path = self.path(Some(h)).into_iter();
        path.map(|h| (h, self.by_hash(&h).expect("indexed").record().to_bytes())).collect()
    }

    /// Loads every `block_index` record of `db`, parents first; and the records it skips (one
    /// that does not decode, or whose parent is not indexed), which R5 reports.
    fn load(db: &impl KvRead) -> Result<(Self, Vec<String>), StoreError> {
        let (mut recs, mut skipped) = (Vec::new(), Vec::new());
        for r in db.iter_prefix(cf("block_index"), b"") {
            let (k, v) = r?;
            match (<[u8; 32]>::try_from(k.as_slice()), BlockIndexRecord::decode(&v)) {
                (Ok(key), Ok(rec)) => recs.push((BlockHash::from(key), rec)),
                _ => skipped.push(format!("the record at {k:x?} does not decode")),
            }
        }
        recs.sort_by_key(|(_, r)| r.height);
        let mut arena = Self::default();
        for (hash, r) in recs {
            let prev = arena.by_hash.get(&r.header.prev_hash).copied();
            if r.height > 0 && prev.is_none() {
                skipped.push(format!("block {hash:?} at {} has no indexed parent", r.height));
                continue;
            }
            arena.push(Entry {
                hash,
                prev,
                height: r.height,
                status: r.status,
                n_tx: r.n_tx,
                file: r.file,
                data_pos: r.data_pos.unwrap_or(0),
                undo_pos: r.undo_pos.unwrap_or(0),
                header: r.header,
            });
        }
        Ok((arena, skipped))
    }
}

/// The forward assertions' heights and ancestry, over V's arena.
impl fa::Chain for Arena {
    fn height(&self, h: &Hash) -> Option<u32> {
        self.by_hash(&BlockHash::from(*h)).map(|e| e.height)
    }

    fn lca(&self, a: &Hash, b: &Hash) -> Option<u32> {
        let (a, b) =
            (self.by_hash.get(&BlockHash::from(*a))?, self.by_hash.get(&BlockHash::from(*b))?);
        IndexAccess::height(self, IndexAccess::lca(self, *a, *b)?)
    }
}

impl IndexAccess for Arena {
    fn lookup(&self, h: &BlockHash) -> Option<BlockId> {
        self.by_hash.get(h).copied()
    }

    fn hash(&self, id: BlockId) -> Option<BlockHash> {
        self.get(id).map(|e| e.hash)
    }

    fn ancestor(&self, mut id: BlockId, height: u32) -> Option<BlockId> {
        while self.get(id)?.height > height {
            id = self.get(id)?.prev?;
        }
        (self.get(id)?.height == height).then_some(id)
    }

    fn lca(&self, a: BlockId, b: BlockId) -> Option<BlockId> {
        let h = self.get(a)?.height.min(self.get(b)?.height);
        let (mut a, mut b) = (self.ancestor(a, h)?, self.ancestor(b, h)?);
        while a != b {
            (a, b) = (self.get(a)?.prev?, self.get(b)?.prev?);
        }
        Some(a)
    }

    fn height(&self, id: BlockId) -> Option<u32> {
        self.get(id).map(|e| e.height)
    }

    fn have_data_pos(&self, id: BlockId) -> Option<IndexPos> {
        self.get(id).filter(|e| e.has(Status::HAVE_DATA)).map(|e| IndexPos {
            file: e.file.unwrap_or(0),
            pos: e.data_pos,
        })
    }

    fn undo_pos(&self, id: BlockId) -> Option<IndexPos> {
        self.get(id).filter(|e| e.has(Status::HAVE_UNDO)).map(|e| IndexPos {
            file: e.file.unwrap_or(0),
            pos: e.undo_pos,
        })
    }

    fn status(&self, id: BlockId) -> Option<Status> {
        self.get(id).map(|e| e.status)
    }
}

/// V's state.
struct VState {
    blocks: Blocks,
    cache: CoinsCache<FaultyEngine>,
    index: Arena,
    dirty: BTreeSet<BlockHash>,
    /// The active chain, by height.
    active: Vec<BlockHash>,
    /// Accepted blocks beyond the active tip on the current branch.
    headers: Vec<BlockHash>,
    /// The stub evo state after each active height.
    evo: Vec<[u8; 32]>,
    branch: u8,
    next_branch: u8,
    /// The last tip step's `seq`.
    seq: u64,
    /// V's policy clock, past `Stores::t0`.
    clock: Duration,
    /// What recovery found, for R5 and R6.
    found: Found,
    /// The first `F d`, `D d` or `R fork:len` that moved V with no flush of W's frozen past its
    /// files barrier (a precondition).
    idle_flush: Option<ChainOp>,
    /// `K`'s coin, until a connect spends it (none if the next flush rewrites it).
    corrupt: Option<OutPointKey>,
    /// Whether a connect read `K`'s coin (FA7's start, tying the op that met it).
    corrupt_read: bool,
    /// `-prune`'s automatic target (`A t` sets it), and `pruneblockchain`'s height for the call
    /// the `p` op makes.
    prune_target: u64,
    manual_prune: Option<u32>,
    /// Core mode's connect (`connect=core`); the mini connect without it.
    real: Option<RealConnect>,
}

/// What recovery found, for R5 and R6.
struct Found {
    /// R5's E: the index and file infos recovery found, before it wrote anything.
    e_index: Vec<Entry>,
    e_files: BTreeMap<u32, BlockFileInfo>,
    /// The blocks recovery replayed coins-only, and the booted marker `(new, old)` (R6 (b)).
    replayed: Vec<BlockHash>,
    booted_head: Option<(BlockHash, Option<BlockHash>)>,
    /// The tip recovery reached, before activation (R6 (c)).
    tip: Option<BlockHash>,
}

impl Found {
    /// Whether E marks `t` or an ancestor failed (a load marks a failed block's descendants).
    fn failed_in_e(&self, t: &BlockHash) -> bool {
        let mut at = self.e_index.iter().position(|e| e.hash == *t);
        while let Some(e) = at.map(|i| &self.e_index[i]) {
            if e.has(Status::FAILED_MASK) {
                return true;
            }
            at = e.prev.map(|p| p.0 as usize);
        }
        false
    }

    /// DEC-196's case on E: when E marks the loaded tip failed, its height and the highest
    /// candidate's (what a load knows as valid, before a connect finds an invalid block).
    fn failed_tip(&self) -> Option<(u32, Option<u32>)> {
        let tip = self.e_index.iter().find(|e| Some(e.hash) == self.tip)?;
        let ok = candidates(&self.e_index, &chain_of(&self.e_index, self.tip.as_ref()));
        let best = self.e_index.iter().zip(ok).filter(|(_, ok)| *ok).map(|(e, _)| e.height).max();
        self.failed_in_e(&tip.hash).then_some((tip.height, best))
    }

    /// V must refuse to start: the loaded tip, failed in E, strictly outworks every candidate.
    fn must_refuse(&self) -> bool {
        self.failed_tip().is_some_and(|(h, best)| best.is_none_or(|b| h > b))
    }

    /// V may refuse to start: as [`Self::must_refuse`], or at equal work.
    fn may_refuse(&self) -> bool {
        self.failed_tip().is_some_and(|(h, best)| best.is_none_or(|b| h >= b))
    }
}

/// V's index as the flush sees it.
struct ArenaIndex<'a> {
    index: &'a mut Arena,
    dirty: &'a mut BTreeSet<BlockHash>,
}

impl FlushIndex for ArenaIndex<'_> {
    /// The dirty index records, as `HeaderChain::take_dirty_into` hands them to a freeze.
    fn take_dirty_into(&mut self, put: &mut dyn FnMut(&BlockHash, &[u8])) {
        for e in std::mem::take(self.dirty).iter().filter_map(|h| self.index.by_hash(h)) {
            put(&e.hash, &e.record().to_bytes());
        }
    }

    fn dirty_bound(&mut self) -> Option<(usize, usize)> {
        crate::flush::stored_records(self.dirty.len())
    }

    /// V's `NodeBlockIndex::prune_clear` on the arena (it has no unlinked pairs): every entry in
    /// `files` loses its data, undo and positions, and stays dirty.
    fn prune_clear(&mut self, files: &[u32]) -> Result<Vec<IndexWrite>, String> {
        let mut cleared = Vec::new();
        for (i, e) in self.index.entries.iter_mut().enumerate() {
            if e.file.is_some_and(|f| u32::try_from(f).is_ok_and(|f| files.contains(&f))) {
                if let Some(m) = &self.index.mirror {
                    m.clear_data(BlockId(i as u32));
                }
                e.status = Status(e.status.0 & !(Status::HAVE_DATA | Status::HAVE_UNDO));
                e.file = None;
                e.data_pos = 0;
                e.undo_pos = 0;
                self.dirty.insert(e.hash);
                cleared.push(IndexWrite {
                    hash: e.hash,
                    bytes: e.record().to_bytes(),
                });
            }
        }
        Ok(cleared)
    }
}

impl VState {
    fn tip(&self) -> Option<BlockHash> {
        self.active.last().copied()
    }

    fn tip_ref(&self) -> BlockRef {
        self.index.block_ref(self.active.last().expect("a connected genesis"))
    }

    fn now(&self, s: &Stores) -> Instant {
        s.t0 + self.clock
    }

    /// Runs `f` with V's flush view of this state.
    fn with_cx<R>(
        &mut self,
        s: &Stores,
        scripted: bool,
        f: impl FnOnce(&mut FlushCx<'_, FaultyEngine, StubDelta>) -> R,
    ) -> R {
        let (now, tip, last_seq) = (self.now(s), self.tip_ref(), self.seq);
        let state = self.evo.last().copied().unwrap_or_default();
        let Self {
            cache,
            index,
            dirty,
            prune_target,
            manual_prune,
            ..
        } = self;
        let mut index = ArenaIndex {
            index,
            dirty,
        };
        let mut evo = StubEvo {
            state: Some(state),
            tip: Some(tip.hash),
        };
        let (mut no_prune, mut files_prune);
        let prune: &mut dyn PruneHost = if s.cfg.prune.is_some() {
            files_prune = FilesPruneHost {
                files: s.files.clone(),
                target: *prune_target,
                prune_after_height: s.cfg.prune_after.unwrap_or(PRUNE_AFTER_HEIGHT),
            };
            &mut files_prune
        } else {
            no_prune = NoPrune;
            &mut no_prune
        };
        let mut cx = FlushCx {
            now,
            coins: cache,
            mempool_usage: 0,
            tip,
            last_seq,
            index: &mut index,
            evo: &mut evo,
            prune,
            manual_prune_height: *manual_prune,
            reindex: false,
            is_ibd: false,
            left_ibd: false,
            scripted,
        };
        f(&mut cx)
    }

    /// Runs W until the job in flight is released and W has nothing more to run, servicing its
    /// events: step 12's unlink follows the release (Core: validation.cpp:3069-3074), and the
    /// inline W is dropped with this call.
    fn finish(&mut self, s: &Stores, ctl: &mut Ctl<'_>) -> Result<(), Fatal> {
        loop {
            let ran = ctl.driver().step();
            self.with_cx(s, false, |cx| ctl.poll(cx))?;
            if ran.is_none() {
                assert!(!ctl.in_flight(), "W has nothing to run with a job in flight");
                return Ok(());
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // V: accept, connect, disconnect.

    /// Core's `AcceptBlock`: the block's data, and its index entry.
    fn accept(&mut self, s: &Stores, g: &GenBlock) -> Result<(), Abort> {
        if self.index.by_hash(&g.hash).is_some_and(|e| e.has(Status::HAVE_DATA)) {
            return Ok(());
        }
        let time = u64::from(g.block.header().raw().time);
        s.enter(PATH_ACCEPT);
        let pos = s
            .files
            .mutate(|f| f.save_block(&g.bytes, g.height, time, None))
            .map_err(|e| s.files_failed(&e, "save_block", BLOCK_WRITE_FAILED))?;
        s.leave();
        let prev = (g.height > 0).then(|| self.index.by_hash[&g.parent]);
        let entry = Entry {
            hash: g.hash,
            prev,
            height: g.height,
            // Core's `ReceivedBlockTransactions` raises it to `VALID_TRANSACTIONS`; the mini
            // connect's model keeps CH2's `VALID_TREE`.
            status: Status(Status::HAVE_DATA | self.accepted_validity()),
            n_tx: g.block.txs().len() as u32,
            file: Some(pos.file as i32),
            data_pos: pos.pos,
            undo_pos: 0,
            header: *g.block.header().raw(),
        };
        match self.index.by_hash.get(&g.hash) {
            Some(id) => self.index.entries[id.0 as usize] = entry,
            None => {
                self.index.push(entry);
            }
        }
        self.dirty.insert(g.hash);
        Ok(())
    }

    /// The next block of the current branch, generated and accepted.
    fn accept_next(&mut self, s: &Stores) -> Result<BlockHash, Abort> {
        let parent = self.headers.last().or(self.active.last()).copied();
        let height = (self.active.len() + self.headers.len()) as u32;
        let g = self.blocks.child(&s.cfg, parent.as_ref(), height, self.branch);
        self.accept(s, &g)?;
        self.headers.push(g.hash);
        Ok(g.hash)
    }

    /// Core's `ConnectTip` of an accepted block on the tip: `Ok(false)` if it is invalid (marked
    /// `FAILED_VALID`).
    fn connect_block(&mut self, s: &Stores, g: &GenBlock) -> Result<bool, Abort> {
        s.world.mark(Mark::ConnectBegin);
        // A spend consumes `K`'s coin; only one the cache does not serve reads the bad bytes.
        if self
            .corrupt
            .take_if(|k| g.spends.iter().any(|(spent, _)| spent == k))
            .is_some_and(|k| !self.cache.have_in_cache(&k))
        {
            self.corrupt_read = true;
            s.world.mark(Mark::Log {
                line: format!("{CORRUPT_READ}{}", g.height),
            });
        }
        let best = self.index.block_ref(&g.hash);
        let mut ov = BlockOverlay::new(&mut self.cache);
        // The verdict, and the real connect's status and undo position for the block.
        let (verdict, written) = if let Some(real) = &mut self.real {
            match real.connect(s, self.index.records_to(&g.hash), &mut ov, g) {
                Ok((ConnectOutcome::Connected, status, undo_pos)) => {
                    (Ok(()), Some((status, undo_pos)))
                }
                Ok((ConnectOutcome::Invalid(err), ..)) => {
                    (Err(err.reject_reason().to_owned()), None)
                }
                // Reported through the core's abort path.
                Ok((ConnectOutcome::SystemError(msg), ..)) => {
                    return Err(Abort::new(s.obs.aborted().unwrap_or(msg)));
                }
                Err(f) => return Err(fatal_abort(s, f)),
            }
        } else {
            let valid = mini::connect(&mut ov, g)?;
            (valid.then_some(()).ok_or("bad-txns-inputs-missingorspent".into()), None)
        };
        if let Err(reason) = verdict {
            drop(ov);
            self.fail(s, &g.hash, Status::FAILED_VALID);
            s.world.mark(Mark::Log {
                line: format!("{VERDICT}{} {reason}", g.height),
            });
            s.world.mark(Mark::ConnectEnd);
            return Ok(false);
        }
        let (status, undo_pos) = match written {
            Some((status, undo_pos)) => (status, undo_pos),
            None => Self::write_undo(&self.index, s, g)?,
        };
        let e = self.index.by_hash_mut(&g.hash);
        e.status = status;
        e.undo_pos = undo_pos;
        self.dirty.insert(g.hash);
        ov.commit(best).expect("a block's view merges");
        // The stub evo fold, which reads the presumed effect at its reader height.
        let snap = match s.cfg.presumed {
            Some((_, reader)) if reader == g.height => {
                s.db.get(cf("llmq_snapshot"), K_SNAP).map_err(read_failed)?
            }
            _ => None,
        };
        let prev = self.evo.last().copied().unwrap_or_default();
        self.evo.push(evo_fold(&prev, &g.hash, snap.as_deref()));
        self.active.push(g.hash);
        self.seq += 1;
        s.world.mark(Mark::Connected {
            block: hash(&g.hash),
            scripts: ScriptCheck::Full,
            applied: true,
        });
        s.world.mark(Mark::ConnectEnd);
        // Effects, Buffered as soon as the block is connected (A1 §2.10.1).
        if s.cfg.effects.contains(&g.height) {
            let key = format!("{K_EFFECT}{}", g.height);
            let mut b = s.db.batch();
            b.put(cf("llmq_snapshot"), key.as_bytes(), g.hash.as_bytes());
            s.db.write(b, Commit::Buffered).map_err(|e| s.write_failed(&e))?;
            s.world.mark(Mark::Log {
                line: format!("{EFFECT_WRITTEN}{key}"),
            });
        }
        if s.cfg.presumed.is_some_and(|(base, _)| base == g.height) {
            let mut b = s.db.batch();
            b.put(cf("llmq_snapshot"), K_SNAP, g.hash.as_bytes());
            s.db.write(b, Commit::Buffered).map_err(|e| s.write_failed(&e))?;
            s.ledger.record_presumed();
        }
        if let Some(sub) = &s.sub {
            sub.feeds.connect(best);
        }
        self.keep_up(s)?;
        Ok(true)
    }

    /// The subscriber after V's tip step: it processes to its backlog; at its journal or `D_max`
    /// limit (rule 3) it posts `FlushDue`, which V answers here unless a flush is in flight around
    /// this step (the flush answers it).
    fn keep_up(&mut self, s: &Stores) -> Result<(), Abort> {
        let Some(sub) = &s.sub else {
            return Ok(());
        };
        sub.keep_up().map_err(|e| sub.fail(&e))?;
        if !s.in_ctl() && sub.hub.flush_due() {
            s.with_ctl(|ctl| {
                self.with_cx(s, false, |cx| ctl.answer_due(cx))?;
                self.finish(s, ctl)
            })?;
            sub.keep_up().map_err(|e| sub.fail(&e))?;
        }
        Ok(())
    }

    /// The mini connect's `WriteUndoDataForBlock`, in `ConnectBlock` before the view is flushed:
    /// only for a block without undo data (one re-connected after a crash keeps its record).
    /// The block's status and undo position after it, its validity raised to `VALID_SCRIPTS`.
    fn write_undo(index: &Arena, s: &Stores, g: &GenBlock) -> Result<(Status, u32), Abort> {
        let e = index.by_hash(&g.hash).expect("an accepted block");
        let mut status = e.status.0;
        let mut undo_pos = e.undo_pos;
        if g.height > 0 && !e.has(Status::HAVE_UNDO) {
            let file = e.file.unwrap_or(0) as u32;
            let at = s
                .files
                .mutate(|f| f.write_undo(&g.undo, file, g.height, g.parent.as_bytes()))
                .map_err(|e| s.files_failed(&e, "write_undo", UNDO_WRITE_FAILED))?;
            undo_pos = at.pos;
            status |= Status::HAVE_UNDO;
        }
        status = (status & !Status::VALID_MASK) | Status::VALID_SCRIPTS;
        Ok((Status(status), undo_pos))
    }

    /// The validity an accepted block's entry starts with.
    fn accepted_validity(&self) -> u32 {
        if self.real.is_some() {
            Status::VALID_TRANSACTIONS
        } else {
            Status::VALID_TREE
        }
    }

    /// Connects the first accepted block: `Ok(false)` if it is invalid.
    fn connect_next(&mut self, s: &Stores) -> Result<bool, Abort> {
        let h = self.headers.remove(0);
        let g = self.blocks.get(&h).expect("generated").clone();
        s.enter(PATH_CONNECT);
        let ok = self.connect_block(s, &g)?;
        s.leave();
        if !ok {
            self.headers.clear();
        }
        Ok(ok)
    }

    fn connect(&mut self, s: &Stores, n: u32) -> Result<(), Abort> {
        for _ in 0..n {
            if self.headers.is_empty() {
                self.accept_next(s)?;
            }
            if !self.connect_next(s)? {
                break;
            }
        }
        Ok(())
    }

    /// Core's `DisconnectTip`: the tip's undo data from the block files, then the coins. In core
    /// mode the real `DisconnectBlock` reads and applies them; anything but `DISCONNECT_OK` is
    /// `ActivateBestChainStep`'s stop (validation.cpp:3621). An `x` stops V there too, where
    /// Core's `InvalidateBlock` returns false without `AbortNode` (validation.cpp:3959-3966); no
    /// core-mode scenario invalidates.
    fn disconnect_tip(&mut self, s: &Stores) -> Result<(), Abort> {
        let h = self.active.pop().expect("a block above genesis");
        let g = self.blocks.get(&h).expect("generated").clone();
        let parent = self.index.block_ref(&g.parent);
        s.enter(PATH_DISCONNECT);
        if let Some(real) = &mut self.real {
            let mut ov = BlockOverlay::new(&mut self.cache);
            match real.disconnect(s, self.index.records_to(&h), &mut ov, &g) {
                Ok(DisconnectResult::Ok) => ov.commit(parent).expect("a block's view merges"),
                Ok(DisconnectResult::Unclean | DisconnectResult::Failed) => {
                    return Err(s.stop(None, DISCONNECT_FAILED));
                }
                Err(f) => return Err(fatal_abort(s, f)),
            }
        } else {
            let e = self.index.by_hash(&h).expect("an indexed block");
            let bytes = s
                .read_undo(e.undo(), &g.parent)
                .map_err(|err| s.read_failed(&err, DISCONNECT_FAILED))?;
            let undo = BlockUndoRef::decode(&mut &bytes[..])
                .map_err(|_| s.stop(None, DISCONNECT_FAILED))?;
            mini::disconnect(BlockOverlay::new(&mut self.cache), &g, &undo, parent)?;
        }
        s.leave();
        self.evo.pop();
        self.seq += 1;
        if let Some(sub) = &s.sub {
            // Core: validation.cpp:3292-3301, the prune locks above the new tip move back.
            for (name, height) in sub.hub.on_disconnect_tip(g.height) {
                s.world.mark(Mark::Log {
                    line: format!("{name} prune lock moved back to {height}"),
                });
            }
            sub.feeds.disconnect();
        }
        self.keep_up(s)
    }

    fn disconnect_to(&mut self, s: &Stores, height: u32) -> Result<(), Abort> {
        self.headers.clear();
        while self.active.len() as u32 > height + 1 {
            self.disconnect_tip(s)?;
        }
        Ok(())
    }

    fn reorg(&mut self, s: &Stores, fork: u32, len: u32) -> Result<(), Abort> {
        self.disconnect_to(s, fork)?;
        self.branch = self.next_branch;
        self.next_branch += 1;
        for _ in 0..len {
            self.accept_next(s)?;
            if !self.connect_next(s)? {
                break;
            }
        }
        Ok(())
    }

    /// `invalidateblock h`: disconnect to `h − 1`, mark block `h` failed and its descendants
    /// failed children.
    fn invalidate(&mut self, s: &Stores, h: u32) -> Result<(), Abort> {
        let failed: Vec<BlockHash> = self.active.iter().skip(h.max(1) as usize).copied().collect();
        if failed.is_empty() {
            return Ok(());
        }
        self.disconnect_to(s, h.max(1) - 1)?;
        for (i, id) in failed.iter().enumerate() {
            let bit = if i == 0 {
                Status::FAILED_VALID
            } else {
                Status::FAILED_CHILD
            };
            self.fail(s, id, bit);
        }
        self.branch = self.next_branch;
        self.next_branch += 1;
        Ok(())
    }

    /// Marks `id` failed with `bit`, and logs the status change (FA7).
    fn fail(&mut self, s: &Stores, id: &BlockHash, bit: u32) {
        let e = self.index.by_hash_mut(id);
        e.status = Status(e.status.0 | bit);
        s.world.mark(Mark::Log {
            line: format!("{STATUS}{} {:#x}", e.height, e.status.0),
        });
        self.dirty.insert(*id);
    }

    /// `K`: the tip's coinbase coin overwritten past the guard with bytes that do not decode.
    fn corrupt(&mut self, s: &Stores) -> Result<(), Abort> {
        let Some(tip) = self.tip() else {
            return Ok(());
        };
        let g = self.blocks.get(&tip).expect("generated");
        let (k, _) = g.coinbase_out();
        let mut b = s.raw.batch();
        CoinsTable::default().put(&mut b, &k, &[0xff]);
        s.raw.write(b, Commit::Durable).map_err(|e| s.write_failed(&e))?;
        s.world.mark(Mark::Log {
            line: format!("{CORRUPTED}{}", g.height),
        });
        // A dirty cached coin is rewritten by the next flush, so no read meets the bad bytes.
        self.corrupt = (!self.cache.cached(&k).is_some_and(|e| e.flags.is_dirty())).then_some(k);
        Ok(())
    }

    // -----------------------------------------------------------------------------------------
    // The flush side.

    /// `c n` in prune mode: each connect followed by Core's IF_NEEDED call site, so an armed
    /// automatic check is decided there (D-b2-2).
    fn connect_at_sites(&mut self, s: &Stores, n: u32) -> Result<(), Abort> {
        for _ in 0..n {
            if self.headers.is_empty() {
                self.accept_next(s)?;
            }
            if !self.connect_next(s)? {
                break;
            }
            s.with_ctl(|ctl| {
                self.with_cx(s, false, |cx| ctl.flush(FlushStateMode::IfNeeded, cx))?;
                self.finish(s, ctl)
            })?;
        }
        Ok(())
    }

    /// `pruneblockchain h`: Core's `FlushStateToDisk(NONE, h)`, waited for.
    fn prune(&mut self, s: &Stores, height: u32) -> Result<(), Abort> {
        self.manual_prune = Some(height);
        let r = s.with_ctl(|ctl| self.with_cx(s, false, |cx| ctl.flush(FlushStateMode::None, cx)));
        self.manual_prune = None;
        r
    }

    /// Core's init step 10 (`PruneAndFlush`, init.cpp:2344-2350, validation.cpp:3141-3148): in
    /// prune mode a prune check on the recovered tip, after the indexes took their locks (step
    /// 8) and before `ThreadImport`'s activation, as the node's: `FlushControl::prune_and_flush`
    /// arms the check and unlinks before its release (b4 D57). With no tip the node's call arms
    /// the check and its flush takes it, selecting nothing (`blockstorage.cpp:191-193`); CRASH
    /// has no flush to run there, so it arms and takes the check alone.
    fn prune_and_flush(&mut self, s: &Stores) -> Result<(), Abort> {
        if s.cfg.prune.is_none() {
            return Ok(());
        }
        if self.active.is_empty() {
            let mut host = FilesPruneHost {
                files: s.files.clone(),
                target: self.prune_target,
                prune_after_height: s.cfg.prune_after.unwrap_or(PRUNE_AFTER_HEIGHT),
            };
            arm_and_take_without_tip(&mut host, &*s.obs);
            return Ok(());
        }
        s.with_ctl(|ctl| {
            self.with_cx(s, false, |cx| ctl.prune_and_flush(cx))?;
            self.finish(s, ctl)
        })
    }

    /// `A t`: the automatic target, and a check armed for the next call site.
    fn auto_target(&mut self, s: &Stores, target: u64) {
        self.prune_target = target;
        // After a latched failure nothing is pruned, armed or not.
        let _ = s.files.mutate(|f| {
            f.rearm_check_for_pruning();
            Ok(())
        });
    }

    /// An ALWAYS flush, waited for (the ALWAYS shortcut writes the index alone), with a `q`
    /// height's manual prune check.
    fn flush(&mut self, s: &Stores) -> Result<(), Abort> {
        let r =
            s.with_ctl(|ctl| self.with_cx(s, false, |cx| ctl.flush(FlushStateMode::Always, cx)));
        self.manual_prune = None;
        r
    }

    /// The periodic index-only write (Core's `fPeriodicWrite`): a PERIODIC call an hour after
    /// the policy's first.
    fn index_write(&mut self, s: &Stores) -> Result<(), Abort> {
        s.with_ctl(|ctl| {
            self.with_cx(s, false, |cx| ctl.flush(FlushStateMode::Periodic, cx))?;
            self.clock += crate::flush::DATABASE_WRITE_INTERVAL + Duration::from_secs(1);
            self.with_cx(s, false, |cx| ctl.flush(FlushStateMode::Periodic, cx))?;
            self.finish(s, ctl)
        })
    }

    /// A scripted flush V does not wait for: once W's files barrier has returned, V connects `d`
    /// blocks (`F d`), disconnects them (`D d`) or reorgs (`R fork:len`), then waits for the
    /// release.
    fn flush_during(&mut self, s: &Stores, op: ChainOp) -> Result<(), Abort> {
        s.with_ctl(|ctl| {
            self.with_cx(s, true, |cx| ctl.flush(FlushStateMode::IfNeeded, cx))?;
            let mut frozen = false;
            while ctl.in_flight() {
                match ctl.driver().step() {
                    Some(FlushStep::Files) => {
                        frozen = true;
                        break;
                    }
                    None => break,
                    Some(_) => {}
                }
            }
            if !frozen {
                self.idle_flush.get_or_insert(op);
            }
            let moved = match op {
                ChainOp::FlushRewind(d) => {
                    let height = (self.active.len() as u32).saturating_sub(d + 1);
                    self.disconnect_to(s, height)
                }
                ChainOp::FlushReorg {
                    fork,
                    len,
                } => self.reorg(s, fork, len),
                ChainOp::FlushDuring(d) => self.connect(s, d),
                _ => unreachable!("`{op}` does not move V during a flush"),
            };
            if let Err(a) = moved {
                return Ok(Err(a));
            }
            self.finish(s, ctl)?;
            Ok(Ok(()))
        })?
    }

    // -----------------------------------------------------------------------------------------
    // Startup.

    /// Core's first `ActivateBestChain` after recovery: reorg to the most-work candidate, a block
    /// whose whole path has data and no failed block (ties keep the active chain, then the first
    /// branch), reading each block and undo record from the block files; an invalid block on the
    /// way is marked failed and the choice is made again. A loaded active chain with a failed
    /// block (an index written after `invalidateblock`, the coins before it) is never kept
    /// (DEC-196): V refuses to start while it strictly outworks every
    /// candidate, and otherwise reorgs off it, at equal work too.
    fn activate(&mut self, s: &Stores) -> Result<(), Abort> {
        loop {
            let active: BTreeSet<BlockHash> = self.active.iter().copied().collect();
            let ok = candidates(&self.index.entries, &active);
            let target = self
                .index
                .entries
                .iter()
                .zip(&ok)
                .filter(|(_, ok)| **ok)
                .map(|(e, _)| e)
                .max_by_key(|e| {
                    let branch = e.header.time.saturating_sub(T0);
                    (e.height, active.contains(&e.hash), std::cmp::Reverse(branch))
                })
                .map(|e| (e.hash, e.height));
            let failed = self.active.iter().any(|id| self.is_failed(id));
            if failed && target.is_none_or(|(_, h)| self.active.len() as u32 > h + 1) {
                s.world.mark(Mark::Log {
                    line: INVALID_TIP.into(),
                });
                return Err(Abort::new(INVALID_TIP));
            }
            let Some((target, height)) = target else {
                return Ok(());
            };
            if (!failed && self.active.len() as u32 > height) || self.tip() == Some(target) {
                return Ok(());
            }
            let path = self.index.path(Some(&target));
            let shared = common_prefix(&self.active, &path);
            while self.active.len() > shared {
                self.disconnect_tip(s)?;
            }
            self.headers.clear();
            let mut connected = true;
            for id in &path[shared..] {
                let e = self.index.by_hash(id).expect("indexed").clone();
                s.enter(PATH_CONNECT);
                let bytes =
                    s.read_block(e.data()).map_err(|err| s.read_failed(&err, BLOCK_READ_FAILED))?;
                let g = self.learn(s, &e).filter(|g| g.bytes.as_slice() == bytes.as_slice());
                let Some(g) = g else {
                    return Err(s.stop(None, BLOCK_READ_FAILED));
                };
                let next = self.connect_block(s, &g)?;
                s.leave();
                if !next {
                    connected = false;
                    break;
                }
            }
            if connected && self.tip() == Some(target) {
                return Ok(());
            }
        }
    }

    fn is_failed(&self, id: &BlockHash) -> bool {
        self.index.by_hash(id).is_some_and(|e| e.has(Status::FAILED_MASK))
    }

    /// After an activation: the next block extends the tip's branch, unless the index marks that
    /// branch's next block failed (Core rejects a known-invalid block), then a fresh branch. The
    /// reference's `Wipe` makes the same choice.
    fn branch_from_tip(&mut self, s: &Stores) {
        let Some(tip) = self.active.last().and_then(|t| self.blocks.get(t)).cloned() else {
            self.branch = 0;
            return;
        };
        let next = self.blocks.child(&s.cfg, Some(&tip.hash), tip.height + 1, tip.branch);
        self.branch = if self.is_failed(&next.hash) {
            self.next_branch += 1;
            self.next_branch - 1
        } else {
            tip.branch
        };
    }

    /// The generated block an index entry names (its header's time names its branch).
    fn learn(&mut self, s: &Stores, e: &Entry) -> Option<Arc<GenBlock>> {
        if let Some(g) = self.blocks.get(&e.hash) {
            return Some(g.clone());
        }
        // Genesis is branch 0's (core mode's is regtest's, from before `T0`).
        let branch = match e.prev {
            None => 0,
            Some(_) => u8::try_from(e.header.time.checked_sub(T0)?).ok()?,
        };
        let parent = match e.prev {
            Some(p) => {
                let pe = self.index.get(p)?.clone();
                Some(self.learn(s, &pe)?.hash)
            }
            None => None,
        };
        let g = self.blocks.child(&s.cfg, parent.as_ref(), e.height, branch);
        (g.hash == e.hash).then_some(g)
    }
}

/// The chainstate profile's system under test.
pub struct ChainSut {
    s: Stores,
    v: VState,
    /// The ops applied, and the first that refused (a precondition).
    ran: usize,
    refused: Option<Refusal>,
    /// The op in which the forward run's first failed read appeared or a connect read `K`'s coin:
    /// FA7's op that met its start (a fault V tolerates, as an `Allocate`'s, does not count).
    met: Option<usize>,
}

/// What recovery left for V.
struct Recovered {
    files: Arc<stores::Files>,
    index: Arena,
    tip: Option<BlockRef>,
    found: Found,
}

fn recover(world: &World, cfg: &ChainConfig, engines: &Engines) -> Result<Recovered, Abort> {
    let (db, obs): (&Db, _) = (engines.db(), engines.obs());
    let opened = stores::open(world, engines, cfg)?;
    // CH1-node's load of the block index, and R5's E (before recovery's first write; the open
    // writes only to resume a wipe, which keeps both column families).
    let read = |_: StoreError| {
        obs.abort(Some("read"), OPEN_FAILED);
        Abort::new(OPEN_FAILED)
    };
    let (index, _) = Arena::load(db).map_err(read)?;
    let e_files = file_infos(db).map_err(read)?;
    blk_files_present(world, cfg, obs, opened.files(), &index, &e_files)?;
    let booted_head = opened.meta().head_blocks;
    // Core: validation.cpp:5473 (375c64d2317), CH1-node's step 7 (E-b1.3): once the index is
    // loaded, the files a durable prune left are removed, best-effort, by the node load's own
    // scan.
    if opened.meta().pruned_files {
        world.mark(Mark::BestEffortBegin);
        let scanned = crate::store::scan_already_pruned_files(opened.files(), true);
        world.mark(Mark::BestEffortEnd);
        // Only a refusal after an earlier failure, which has been reported.
        scanned.map_err(|_| Abort::new(OPEN_FAILED))?;
    }
    // The first recovery only: a nested crash's recovery completes.
    let first = world.epoch() == 1 && !planted!(cfg.planted, NoRoll);
    let crash = crash_roll(cfg.crash_replay.filter(|_| first));
    let rec = stores::recover(world, opened, &index, cfg.sub_batch.unwrap_or(u64::MAX), crash)?;
    Ok(Recovered {
        files: rec.files,
        found: Found {
            e_index: index.entries.clone(),
            e_files,
            replayed: rec.replayed,
            booted_head,
            tip: rec.tip.map(|t| t.hash),
        },
        tip: rec.tip,
        index,
    })
}

/// CH1-node's check of the loaded index (BL-127, `chain::load`'s `block_files_present`): every
/// blk file a durable `HAVE_DATA` entry names opens, in ascending file order, stopping at the
/// first that does not, before the prune scan. Rev files are not checked; their undo read fails
/// later. A file that does not open is Core's [`LOAD_FAILED`]. An I/O error is a fatal read in
/// its own path, [`PATH_LOAD_INDEX`] (DEC-148), which the cause sanctions only there. A missing
/// file is a refusal with no fatal error: as DEC-196's in `open`, one that is not warranted
/// becomes an abort no table sanctions (R0). It is warranted only when a crash lost the file
/// after a failed directory sync (DEC-212) and E's records pass R1 with that loss entitled, in
/// checking mode against [`ChainRef::checks_view`]; a violation is named in the abort. Only R1:
/// no recovery has run, so DEC-196's other checks have nothing of V's to judge, and R2 and D1 run
/// on the booted stores anyway. `NoBlkChk` skips the check.
// Core: node/blockstorage.cpp:377-390 (LoadBlockIndexDB), validation.cpp:5319-5322,
// node/chainstate.cpp:183-185, flatfile.cpp:45
fn blk_files_present(
    world: &World,
    cfg: &ChainConfig,
    obs: &WorldObserver,
    files: &Files,
    index: &Arena,
    infos: &BTreeMap<u32, BlockFileInfo>,
) -> Result<(), Abort> {
    if planted!(cfg.planted, NoBlkChk) {
        return Ok(());
    }
    let held: BTreeSet<i32> =
        index.entries.iter().filter(|e| e.has(Status::HAVE_DATA)).filter_map(|e| e.file).collect();
    let reader = files.read(|f| f.reader());
    let refused = |why: String| Abort::new(format!("{LOAD_FAILED} (BL-127: {why})"));
    world.mark(Mark::PathBegin {
        path: PATH_LOAD_INDEX,
    });
    for file in held {
        // Core's null position opens nothing.
        let n = u32::try_from(file).map_err(|_| refused(format!("blk file {file}")))?;
        let Err(e) = reader.open_block_file(n) else {
            continue;
        };
        if let Some(path) = e.open_failed() {
            log(obs, LogCat::Always, format!("Unable to open file {}", path.display()));
        }
        if io_failure(&e) {
            obs.abort(None, LOAD_FAILED);
            return Err(Abort::new(LOAD_FAILED));
        }
        let path = format!("{BLOCKS_DIR}/blk{n:05}.dat");
        if !world.lost_to_failed_dir_sync(std::path::Path::new(&path)) {
            return Err(refused(format!("{path} is missing, not lost to a failed directory sync")));
        }
        let refm = ChainRef::checks_view(cfg);
        let broke =
            world.checking(|| check_records(world, files, &refm.blocks, index, infos, true));
        if !broke.is_empty() {
            return Err(refused(format!("the booted stores fail {broke:?}")));
        }
        return Err(Abort::new(LOAD_FAILED));
    }
    world.mark(Mark::PathEnd);
    Ok(())
}

/// The planted probes on what recovery left, before activation (Sol r1, r2), each Durable:
/// `LostEffect` deletes every effect record, `LostCoin` the first coin. `RefuseCoin`,
/// `RefuseHead` and `RefuseEvo` act only when E requires DEC-196's refusal (`refusing`): they
/// delete the first coin, write a `head_blocks` marker naming the tip, or delete `evo_tip`.
fn plant_damage(s: &Stores, refusing: bool) -> Result<(), Abort> {
    use crate::store::{EVO_TIP, HEAD_BLOCKS, TIP};
    let meta = dashd_store::DbSchema::META;
    let keys = |name: &str, prefix: &[u8]| -> Vec<Vec<u8>> {
        (s.raw.iter_prefix(cf(name), prefix)).map(|r| r.expect("a planted read").0).collect()
    };
    let (mut b, mut damaged) = (s.db.batch(), false);
    if planted!(s.cfg.planted, LostEffect) {
        for k in keys("llmq_snapshot", K_EFFECT.as_bytes()) {
            b.delete(cf("llmq_snapshot"), &k);
            damaged = true;
        }
    }
    let lose_coin =
        planted!(s.cfg.planted, LostCoin) || (refusing && planted!(s.cfg.planted, RefuseCoin));
    if lose_coin && let Some(k) = keys("coins", b"").first() {
        b.delete(cf("coins"), k);
        damaged = true;
    }
    if refusing
        && planted!(s.cfg.planted, RefuseHead)
        && let Some(tip) = s.raw.get(meta, TIP).expect("a planted read")
    {
        b.put(meta, HEAD_BLOCKS, &[&tip[..], &[0; 32]].concat());
        damaged = true;
    }
    if refusing && planted!(s.cfg.planted, RefuseEvo) {
        b.delete(meta, EVO_TIP);
        damaged = true;
    }
    if damaged {
        s.db.write(b, Commit::Durable).map_err(|e| s.write_failed(&e))?;
    }
    Ok(())
}

impl ChainSut {
    /// The active tip's height and hash (CH3a a3's directed C11 test).
    pub fn tip(&self) -> Option<(u32, BlockHash)> {
        let tip = self.v.tip()?;
        Some((self.v.active.len() as u32 - 1, tip))
    }

    /// E's entries at `height`, the block index recovery found durable before it wrote anything:
    /// each block's hash, its parent's and its status bits (CH3a a3's directed C11 test).
    pub fn durable_at(&self, height: u32) -> Vec<(BlockHash, BlockHash, u32)> {
        let at = self.v.found.e_index.iter().filter(|e| e.height == height);
        at.map(|e| (e.hash, e.header.prev_hash, e.status.0)).collect()
    }

    fn meta_hash(&self, key: &[u8]) -> Option<BlockHash> {
        let v = self.s.raw.get(dashd_store::DbSchema::META, key).ok().flatten()?;
        <[u8; 32]>::try_from(v.as_slice()).ok().map(BlockHash::from)
    }

    fn evo_state(&self) -> Option<Vec<u8>> {
        self.s.raw.get(cf("mn_diff"), K_STATE).ok().flatten()
    }

    /// One op on V (and W).
    fn run(&mut self, op: &ChainOp) -> Result<(), Abort> {
        if let Some(m) = self.s.obs.aborted() {
            return Err(Abort::new(m));
        }
        let (s, v) = (&self.s, &mut self.v);
        match op {
            ChainOp::Connect(n) if s.cfg.prune.is_some() => v.connect_at_sites(s, *n),
            ChainOp::Connect(n) => v.connect(s, *n),
            ChainOp::Prune(h) => v.prune(s, *h),
            ChainOp::AutoTarget(t) => {
                v.auto_target(s, *t);
                Ok(())
            }
            ChainOp::Accept(n) => {
                for _ in 0..*n {
                    v.accept_next(s)?;
                }
                Ok(())
            }
            ChainOp::Flush => v.flush(s),
            ChainOp::IndexWrite => v.index_write(s),
            ChainOp::Reorg {
                fork,
                len,
            } => v.reorg(s, *fork, *len),
            ChainOp::TestValidity => {
                if let Some(&block) = v.headers.first() {
                    s.world.mark(Mark::Connected {
                        block: hash(&block),
                        scripts: ScriptCheck::Full,
                        applied: false,
                    });
                }
                Ok(())
            }
            ChainOp::Invalidate(h) => v.invalidate(s, *h),
            ChainOp::Wipe => self.wipe(),
            ChainOp::Continue => {
                v.connect(s, 1)?;
                v.flush(s)
            }
            ChainOp::FlushDuring(_)
            | ChainOp::FlushRewind(_)
            | ChainOp::FlushReorg {
                ..
            } => v.flush_during(s, *op),
            ChainOp::Corrupt => v.corrupt(s),
            ChainOp::PruneRequest(h) => {
                v.manual_prune = Some(*h);
                Ok(())
            }
            ChainOp::SubSync(n) => self.sub_sync(*n),
            ChainOp::SubTick => match &s.sub {
                Some(sub) => sub.tick().map_err(|e| sub.fail(&e)),
                None => Ok(()),
            },
            ChainOp::SubDisable(prune) => {
                let mut after = || prune.map_or(Ok(()), |h| v.prune(s, h));
                match &s.sub {
                    Some(sub) => sub.disable(&mut after),
                    None => after(),
                }
            }
        }
    }

    /// `-reindex-chainstate`'s wipe (Core's `LoadChainstate`), then the reconnect of the best
    /// chain from the block files, as CH5's replay will do it.
    fn wipe(&mut self) -> Result<(), Abort> {
        let (s, v) = (&self.s, &mut self.v);
        s.world.mark(Mark::Log {
            line: WIPE_BEGUN.into(),
        });
        s.enter(PATH_LOAD);
        wipe_chainstate(&s.db).map_err(|e| {
            s.failed(e.kind() == Some(dashd_store::ErrorKind::ReadOnly), "write", OPEN_FAILED)
        })?;
        s.leave();
        v.cache = cache_at(&s.db, None);
        v.active.clear();
        v.headers.clear();
        v.evo.clear();
        v.activate(s)?;
        v.branch_from_tip(s);
        Ok(())
    }
}

impl Sut for ChainSut {
    type Config = ChainConfig;
    type Op = ChainOp;
    type Ref = ChainRef;

    const NAME: &'static str = "chainsut";

    fn open(world: &World, cfg: &ChainConfig) -> Result<Self, Abort> {
        let engines = Engines::new(world, cfg);
        let mut rec = recover(world, cfg, &engines)?;
        let g = rec.tip;
        // The subscriber's producer over the recovered chain, before activation moves it.
        let sub = cfg.index.map(|c| {
            let index = rec.index.mirror();
            let active = rec.index.path(rec.found.tip.as_ref());
            let active = active.iter().map(|h| rec.index.block_ref(h)).collect();
            let sub = Subs::new(world, c, engines.db(), engines.obs_arc(), index, active);
            if let Some(g) = rec.tip {
                sub.hub.recovered(g);
            }
            sub
        });
        let s = Stores::new(world, cfg, engines, rec.files, sub);
        plant_damage(&s, rec.found.must_refuse())?;
        let mut v = VState {
            blocks: Blocks::default(),
            cache: cache_at(&s.db, rec.tip),
            active: rec.index.path(rec.found.tip.as_ref()),
            index: rec.index,
            dirty: BTreeSet::new(),
            headers: Vec::new(),
            evo: Vec::new(),
            branch: 0,
            next_branch: 1,
            seq: 0,
            clock: Duration::ZERO,
            found: rec.found,
            idle_flush: None,
            corrupt: None,
            corrupt_read: false,
            prune_target: cfg.prune.unwrap_or(0),
            manual_prune: None,
            real: cfg.core.then(|| RealConnect::new(&s)).transpose()?,
        };
        // Learn every indexed block (a reorg branch's number is never reused).
        let entries = v.index.entries.clone();
        for e in &entries {
            if let Some(g) = v.learn(&s, e) {
                v.next_branch = v.next_branch.max(g.branch + 1);
            }
        }
        // The evo states along the recovered chain: EvoDB's undo would restore them on a
        // disconnect (I34).
        let mut state = [0u8; 32];
        for (h, id) in v.active.clone().iter().enumerate() {
            let snap = s
                .cfg
                .presumed
                .filter(|(_, reader)| *reader == h as u32)
                .and_then(|(base, _)| v.active.get(base as usize).copied());
            state = evo_fold(&state, id, snap.as_ref().map(|s| &s.as_bytes()[..]));
            v.evo.push(state);
        }
        // Core starts its indexes, which read their locators and take their prune locks, before
        // `ThreadImport`'s first `ActivateBestChain` (init.cpp step 8). T28: after activation.
        let late = planted!(cfg.planted, T28);
        if !late && let Some(sub) = &s.sub {
            sub.open(world, g).map_err(|e| sub.fail(&e))?;
        }
        v.prune_and_flush(&s)?;
        // R6 (e) on a refusal (DEC-196): the harness judges an abort by its message alone, so a
        // refusal E does not warrant is turned into one no table sanctions (R0). The harness checks
        // only an open that succeeds, so a warranted refusal runs every check of `check` but
        // activation's outcome, against the reference's view, and a failure does the same.
        if let Err(a) = v.activate(&s) {
            if a.message != INVALID_TIP {
                return Err(a);
            }
            if !v.found.may_refuse() {
                let msg = format!("{INVALID_TIP} (DEC-196: E does not warrant it)");
                return Err(Abort::new(msg));
            }
            let sut = Self {
                s,
                v,
                ran: 0,
                refused: None,
                met: None,
            };
            // As the harness checks: no recovery fault is taken by the checker's reads.
            let refm = ChainRef::checks_view(cfg);
            let broke = world.checking(|| sut.judge(world, &refm, &world.trace(), false));
            if !broke.is_empty() {
                return Err(Abort::new(format!(
                    "{INVALID_TIP} (DEC-196: the recovered stores fail {broke:?})"
                )));
            }
            return Err(a);
        }
        if v.active.is_empty() {
            // A fresh chain: genesis.
            v.connect(&s, 1)?;
        }
        // The next block extends the tip activation reached (the most-work chain, which a forced
        // reorg to a shorter branch may have left).
        v.branch_from_tip(&s);
        let mut sut = Self {
            s,
            v,
            ran: 0,
            refused: None,
            met: None,
        };
        sut.open_subscriber(world, g, late)?;
        Ok(sut)
    }

    fn apply(&mut self, op: &ChainOp) -> Result<(), Abort> {
        self.ran += 1;
        let r = self.run(op);
        if let Err(a) = &r {
            self.refused.get_or_insert_with(|| (self.ran, a.message.clone()));
        }
        if self.met.is_none() && (self.v.corrupt_read || self.read_failed_yet()) {
            self.met = Some(self.ran);
        }
        r
    }

    /// Without a fault: every op returned (or `-dbcrashratio`'s roll in W ended the last), each
    /// `F d`, `D d` and `R fork:len` moved V during a flush in flight, and V's active chain is the
    /// reference's.
    fn preconditions(&self, _: &World, refm: &ChainRef, trace: &Trace) -> Result<(), String> {
        if let Some(op) = self.v.idle_flush {
            return Err(format!("`{op}`: no flush was in flight to move V during"));
        }
        stores::forward_preconditions(
            &self.s.cfg,
            self.refused.as_ref(),
            &self.v.active,
            refm,
            trace,
        )
    }

    /// FA2 on every forward run; FA7 (CH2 §6.4) on one with a read fault or a corrupt coin.
    fn forward_check(&self, _: &World, _: &ChainRef, trace: &Trace) -> Vec<Violation> {
        let mut out = fa2(trace);
        if let Some(index) = self.s.cfg.index {
            out.extend(fa::forward(index, SUB_NAME, &self.v.index, trace));
        }
        let tripped = self.s.db.latch().tripped().is_some();
        out.extend(stores::fa7(trace, self.refused.as_ref(), self.met, tripped));
        out
    }

    fn check(&self, world: &World, refm: &ChainRef, trace: &Trace) -> Result<(), Vec<Violation>> {
        let out = self.judge(world, refm, trace, true);
        if out.is_empty() {
            Ok(())
        } else {
            Err(out)
        }
    }

    /// R2 on the booted stores, before recovery: a marker names the evo tip, or with no marker the
    /// tip is the evo tip.
    fn store_check(world: &World, _: &ChainConfig) -> Vec<Violation> {
        use crate::store::{EVO_TIP, HEAD_BLOCKS, TIP};
        let chain = world.engine(CHAIN_STORE, &CHAINSTATE_SCHEMA);
        let read = |k: &[u8]| chain.get(dashd_store::DbSchema::META, k);
        let (Ok(evo_tip), Ok(head), Ok(tip)) = (read(EVO_TIP), read(HEAD_BLOCKS), read(TIP)) else {
            return vec![Violation::new(InvariantId::Model, "a store-level check's read failed")];
        };
        let ok = match head {
            Some(head) => evo_tip.as_deref() == head.get(..32),
            None => tip == evo_tip,
        };
        if ok {
            Vec::new()
        } else {
            vec![Violation::new(
                InvariantId::R2,
                "the booted markers disagree: evo is not at the tip or the marker's new",
            )]
        }
    }

    fn continue_after_recovery(&mut self) -> Option<ChainOp> {
        Some(ChainOp::Continue)
    }

    fn has_continuation() -> bool {
        true
    }

    fn extractor(_: &ChainConfig) -> Option<Arc<dyn RefExtractor>> {
        Some(Arc::new(ChainRefs))
    }

    /// `stores::sanctioned_after`'s table.
    fn sanctioned_after(cfg: &ChainConfig, message: &str, cause: &FatalCause) -> bool {
        stores::sanctioned_after(cfg, message, cause)
    }

    fn sanctioned(cfg: &ChainConfig, message: &str) -> bool {
        stores::sanctioned(cfg, message)
    }

    fn projection(&self) -> Option<[u8; 32]> {
        Some(self.s.projection(self.v.tip().as_ref(), || self.evo_state()))
    }
}

impl ChainSut {
    /// The subscriber's boot after activation (§2.9 item 7): its open (here only if `late`, T28),
    /// R11b on the blocks its catch-up needs, then the catch-up to V's tip on a restart, or from
    /// genesis for a Synced subscriber's first boot (a Syncing one moves only by `s` and `k`). A
    /// step blocked on its journal or its commit cap is answered with a flush, as V answers
    /// `FlushDue`.
    fn open_subscriber(
        &mut self,
        world: &World,
        g: Option<BlockRef>,
        late: bool,
    ) -> Result<(), Abort> {
        let Some(sub) = &self.s.sub else {
            return Ok(());
        };
        if late {
            sub.open(world, g).map_err(|e| sub.fail(&e))?;
        }
        let tip = self.v.tip_ref();
        let l = sub.boot().l;
        if let Some(l) = l {
            let (undo, data) = self.unreadable(l, tip);
            let mut boot = sub.boot();
            boot.unreadable = undo.into_iter().chain(data.iter().cloned()).collect();
            boot.unreadable_data = data;
        }
        let first = matches!(sub.cfg.mode, SubMode::Synced { .. });
        if world.epoch() == 0 && !first {
            return Ok(());
        }
        self.catch_up(SYNC_STEPS)?;
        if let Some(sub) = &self.s.sub {
            sub.check_caught_up(&self.v.tip_ref());
        }
        Ok(())
    }

    /// R11b: the blocks from L down to its fork with V's tip have undo, and those from the fork
    /// up to the tip have data; the missing ones, undo then data.
    fn unreadable(&self, l: BlockRef, tip: BlockRef) -> (Vec<String>, Vec<String>) {
        let idx = &self.v.index;
        let Some(fork) = idx.lca(l.id, tip.id) else {
            return (vec![format!("L {} has no fork with V's tip", l.height)], Vec::new());
        };
        let fork_height = idx.height(fork).unwrap_or(0);
        let missing = |from: BlockId, need: u32, what: &str| {
            let mut out = Vec::new();
            let mut cur = Some(from);
            while let Some(e) = cur.and_then(|id| idx.get(id)).filter(|e| e.height > fork_height) {
                if !e.has(need) {
                    out.push(format!("block {} has no {what}", e.height));
                }
                cur = e.prev;
            }
            out
        };
        (missing(l.id, Status::HAVE_UNDO, "undo"), missing(tip.id, Status::HAVE_DATA, "data"))
    }

    /// Steps the subscriber until it is idle, at most `max` steps, answering a blocked step with
    /// an ALWAYS flush.
    fn catch_up(&mut self, max: u32) -> Result<(), Abort> {
        for _ in 0..max {
            let Some(sub) = &self.s.sub else {
                return Ok(());
            };
            let mut blocked = false;
            let moved = sub.step(&mut blocked).map_err(|e| sub.fail(&e))?;
            if blocked {
                self.v.flush(&self.s)?;
            } else if !moved {
                return Ok(());
            }
        }
        Err(Abort::new(format!("the subscriber's catch-up took over {max} steps")))
    }

    /// `s n`: n of a Syncing subscriber's steps, each blocked one answered with a flush.
    fn sub_sync(&mut self, n: u32) -> Result<(), Abort> {
        for _ in 0..n {
            let Some(sub) = &self.s.sub else {
                return Ok(());
            };
            let mut blocked = false;
            sub.step(&mut blocked).map_err(|e| sub.fail(&e))?;
            if blocked {
                self.v.flush(&self.s)?;
            }
        }
        Ok(())
    }

    /// R11a–c on the subscriber's boot: L not a strict descendant of G; the blocks its catch-up
    /// reads readable; its database at L after the blind rewind, and its reads after the catch-up.
    fn check_subscriber(&self) -> Vec<Violation> {
        let Some(sub) = &self.s.sub else {
            return Vec::new();
        };
        let boot = sub.boot();
        let mut out = Vec::new();
        if let Some(l) = boot.unknown {
            out.push(Violation::new(
                R11a,
                format!("the subscriber's L {l} is ahead of every block the index kept"),
            ));
        }
        if let Some(l) = boot.l {
            let ahead = match boot.g {
                None => true,
                Some(g) => {
                    l.height > g.height && self.v.index.ancestor(l.id, g.height) == Some(g.id)
                }
            };
            if ahead {
                out.push(Violation::new(
                    R11a,
                    format!(
                        "the subscriber's L at {} is ahead of the chainstate's {:?}",
                        l.height,
                        boot.g.map(|g| g.height)
                    ),
                ));
            }
        }
        // R11b holds for an index enabled in every epoch since its locator was written; one
        // disabled since must refuse instead (S11c, sanctioned), so it must not open over pruned
        // data (Core checks the data, not the undo).
        if boot.disabled && boot.refused.is_none() && !boot.unreadable_data.is_empty() {
            out.push(Violation::new(
                R11b,
                format!("a disabled index opened over pruned data: {:?}", boot.unreadable_data),
            ));
        }
        if !boot.disabled {
            if !boot.unreadable.is_empty() {
                out.push(Violation::new(
                    R11b,
                    format!("the catch-up's blocks: {:?}", boot.unreadable),
                ));
            }
            if let Some(m) = &boot.refused {
                out.push(Violation::new(R11b, format!("the subscriber refused to open: {m}")));
                out.push(Violation::new(
                    R0,
                    "the subscriber refused to start: its catch-up path is pruned",
                ));
            }
        }
        for e in boot.rewound.iter().chain(&boot.caught_up) {
            out.push(Violation::new(R11c, e.clone()));
        }
        out
    }

    /// Every check on what recovery and this epoch left. Without `opened`, on a warranted refusal
    /// (DEC-196), activation's outcome is not judged: `may_refuse` judged the refusal.
    fn judge(&self, world: &World, refm: &ChainRef, trace: &Trace, opened: bool) -> Vec<Violation> {
        let epoch = world.epoch();
        let mut out = self.check_tip(refm, trace);
        out.extend(self.check_stores(&refm.blocks, trace));
        let connected_now = connected_blocks(trace, Some(epoch));
        out.extend(self.check_flushed_were_connected(refm, trace));
        out.extend(self.check_replayed(refm, &connected_blocks(trace, None)));
        out.extend(self.check_activation(refm, &connected_now, opened));
        out.extend(self.effects(trace));
        out.extend(self.check_replay_crash_came_up(trace, epoch));
        out.extend(self.check_pruned_gone(world, trace, epoch));
        out.extend(self.check_subscriber());
        out
    }

    /// R2 on the recovered stores, R3 and R4: the tip is allowed, and the coins and the evo state
    /// there are the reference's.
    fn check_tip(&self, refm: &ChainRef, trace: &Trace) -> Vec<Violation> {
        let mut out = Vec::new();
        let tip = self.meta_hash(crate::store::TIP);
        let head = self.s.raw.get(dashd_store::DbSchema::META, crate::store::HEAD_BLOCKS);
        if head.ok().flatten().is_some() {
            out.push(Violation::new(R2, "head_blocks present after recovery"));
        }
        if self.meta_hash(crate::store::EVO_TIP) != tip {
            out.push(Violation::new(R2, "evo_tip differs from tip"));
        }
        // From a wipe on, the empty chainstate is allowed too (C12).
        let (t_ack, mut allowed) = allowed_tips(trace);
        if wipe_since_last_durable(trace) {
            allowed.push(None);
        }
        if !allowed.contains(&tip.as_ref().map(hash)) {
            let at = tip.and_then(|t| refm.height(&t));
            out.push(Violation::new(R3, format!("recovered tip {at:?} is not allowed")));
            if let (Some(a), Some(t)) = (t_ack, tip)
                && refm.is_ancestor(&t, &BlockHash::from(a))
                && hash(&t) != a
            {
                out.push(Violation::new(R4, "recovered behind the acknowledged flush"));
            }
            if tip.is_none() && t_ack.is_some() {
                out.push(Violation::new(R4, "recovered nothing after an acknowledged flush"));
            }
        }
        let known = tip.filter(|t| refm.block(t).is_some());
        if let Some(t) = known
            && !refm.valid_path(&t)
        {
            let at = refm.height(&t);
            out.push(Violation::new(
                R3,
                format!("recovered tip {at:?} is on a chain with an invalid block"),
            ));
        }
        if self.s.coins() != refm.coins_at(tip.as_ref()) {
            out.push(Violation::new(R3, "coins differ from the reference at the recovered tip"));
        }
        if let Some(t) = known
            && self.evo_state().as_deref() != Some(&refm.evo_at(&t)[..])
        {
            out.push(Violation::new(
                R3,
                "evo state differs from the reference at the recovered tip",
            ));
        }
        out
    }

    /// Whether the forward run's trace holds a failed read; the trace is cloned only once a fault
    /// has fired.
    fn read_failed_yet(&self) -> bool {
        self.s.world.fault_fired()
            && (self.s.world.trace().entries.iter())
                .any(|e| matches!(e, TraceEntry::ReadFailed { .. }))
    }

    /// R1 and R5 on the stores recovery and this epoch left: the checks that need neither the
    /// reference's chain nor R12's continuation (`blocks` resolves the index's blocks).
    fn check_stores(&self, blocks: &Blocks, trace: &Trace) -> Vec<Violation> {
        let (durable, skipped) = Arena::load(&self.s.raw).unwrap_or_default();
        let mut out: Vec<_> = skipped
            .into_iter()
            .map(|w| Violation::new(R5, format!("durable index: {w}")))
            .collect();
        let infos = file_infos(&self.s.raw).expect("a check's read");
        let connected_now = connected_blocks(trace, Some(self.s.world.epoch()));
        out.extend(check_records(&self.s.world, &self.s.files, blocks, &durable, &infos, false));
        out.extend(self.check_kept(blocks, trace, &durable, &infos, &connected_now));
        out
    }

    /// R5: every `block_files` and `block_index` record of E survives recovery with E's value,
    /// except entitled changes: growth of files this epoch wrote, undo and validity added by this
    /// epoch's connects, the failed marks of invalid blocks, and what this epoch's prunes cleared.
    fn check_kept(
        &self,
        blocks: &Blocks,
        trace: &Trace,
        durable: &Arena,
        infos: &BTreeMap<u32, BlockFileInfo>,
        connected_now: &BTreeSet<Hash>,
    ) -> Vec<Violation> {
        let mut out = Vec::new();
        let written = files_written(trace, self.s.world.epoch());
        let top = connected_now
            .iter()
            .filter_map(|b| blocks.get(&BlockHash::from(*b)))
            .map(|b| b.height)
            .max();
        let pruned = pruned_files(trace, self.s.world.epoch());
        for (n, &was) in &self.v.found.e_files {
            let now = infos.get(n).copied();
            let emptied = pruned.contains(n) && now == Some(BlockFileInfo::default());
            if !emptied && !file_record_kept(was, now, written.contains(n), top) {
                let msg = format!("file record {n} changed by recovery: {now:?}, E had {was:?}");
                out.push(Violation::new(R5, msg));
            }
        }
        for e in &self.v.found.e_index {
            let msg = match durable.by_hash(&e.hash) {
                None => format!("index entry {} lost by recovery", e.height),
                Some(now) => {
                    let connected = connected_now.contains(&hash(&e.hash));
                    let invalid = blocks.get(&e.hash).is_some_and(|b| !b.valid);
                    let cleared = e.file.is_some_and(|f| pruned.contains(&(f as u32)))
                        && now.file.is_none()
                        && !now.has(Status::HAVE_DATA | Status::HAVE_UNDO);
                    let now = &if cleared {
                        // The prune's clear, undone: the rest is judged as for any entry.
                        Entry {
                            status: Status(
                                now.status.0
                                    | (e.status.0 & (Status::HAVE_DATA | Status::HAVE_UNDO)),
                            ),
                            file: e.file,
                            data_pos: e.data_pos,
                            undo_pos: e.undo_pos,
                            ..now.clone()
                        }
                    } else {
                        now.clone()
                    };
                    if index_entry_kept(e, now, connected, invalid) {
                        continue;
                    }
                    format!("index entry {} changed by recovery", e.height)
                }
            };
            out.push(Violation::new(R5, msg));
        }
        out
    }

    /// R6 (a): every block on a flushed transition's connect leg was connected (applied) before its
    /// flush began.
    fn check_flushed_were_connected(&self, refm: &ChainRef, trace: &Trace) -> Vec<Violation> {
        let mut out = Vec::new();
        let mut applied: BTreeSet<Hash> = BTreeSet::new();
        for m in trace.marks() {
            match m {
                Mark::Connected {
                    block,
                    applied: true,
                    ..
                } => {
                    applied.insert(*block);
                }
                Mark::FlushBegin {
                    old,
                    new,
                    ..
                } => {
                    let np = refm.blocks.path(&BlockHash::from(*new));
                    let op = old.map(|o| refm.blocks.path(&BlockHash::from(o))).unwrap_or_default();
                    let fork = common_prefix(&np, &op);
                    if let Some(b) = np[fork..].iter().find(|b| !applied.contains(&hash(b))) {
                        let at = refm.height(b);
                        let msg =
                            format!("flushed block {at:?} was never connected before its flush");
                        out.push(Violation::new(R6, msg));
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// R6 (b): the replay connected exactly the booted marker's `fork → new` path (with `old` null,
    /// from height 1, Core: `validation.cpp:5288-5289`), every block of it connected before.
    fn check_replayed(&self, refm: &ChainRef, applied: &BTreeSet<Hash>) -> Vec<Violation> {
        let mut out = Vec::new();
        let expected: Vec<BlockHash> = match self.v.found.booted_head {
            Some((new, old)) if refm.block(&new).is_some() => {
                let np = refm.blocks.path(&new);
                let op = old.filter(|o| refm.block(o).is_some()).map(|o| refm.blocks.path(&o));
                let fork = op.map_or(1, |op| common_prefix(&np, &op));
                np[fork..].to_vec()
            }
            _ => Vec::new(),
        };
        let replayed = &self.v.found.replayed;
        if *replayed != expected {
            let msg = format!(
                "the replay connected {} blocks, the marker's path has {}",
                replayed.len(),
                expected.len()
            );
            out.push(Violation::new(R6, msg));
        }
        let unapplied = replayed.iter().filter(|id| !applied.contains(&hash(id)));
        out.extend(
            unapplied.map(|_| Violation::new(R6, "a replayed block was never connected before")),
        );
        out
    }

    /// R6 (c): every activation connect has an applied mark in this epoch, and no invalid block is
    /// marked valid; and if V `opened`, every block above genesis on V's active chain is marked
    /// connected (`VALID_SCRIPTS`, with its data and undo unless its file was pruned); R6 (d): no
    /// block whose path is valid and never invalidated is marked failed (a warranted refusal's
    /// [`ChainRef::checks_view`] knows no `invalidateblock`); and R6 (e)
    /// ([`Self::check_outcome`]).
    fn check_activation(
        &self,
        refm: &ChainRef,
        connected_now: &BTreeSet<Hash>,
        opened: bool,
    ) -> Vec<Violation> {
        let (v, mut out) = (&self.v, Vec::new());
        let base: BTreeSet<BlockHash> = v.index.path(v.found.tip.as_ref()).into_iter().collect();
        for id in v.active.iter().filter(|id| !base.contains(*id)) {
            if !connected_now.contains(&hash(id)) {
                let msg = format!(
                    "activation connect {:?} has no applied mark in recovery",
                    refm.height(id)
                );
                out.push(Violation::new(R6, msg));
            }
        }
        if opened {
            out.extend(self.check_outcome(refm));
            for h in v.active.iter().skip(1) {
                let e = v.index.by_hash(h).expect("an active block is indexed");
                let kept = e.file.is_some();
                if (e.status.0 & Status::VALID_MASK) != Status::VALID_SCRIPTS
                    || e.has(Status::HAVE_DATA) != kept
                    || e.has(Status::HAVE_UNDO) != kept
                {
                    let msg = format!("active block {} is marked {:#x}", e.height, e.status.0);
                    out.push(Violation::new(R6, msg));
                }
            }
        }
        // Anything past the mode's accepted validity marks a block valid (CH3a D93): past
        // `VALID_TREE` in mini mode, past Core's `ReceivedBlockTransactions` `VALID_TRANSACTIONS`
        // in core mode.
        for e in &v.index.entries {
            let Some(b) = refm.block(&e.hash) else {
                continue;
            };
            if !b.valid && (e.status.0 & Status::VALID_MASK) > v.accepted_validity() {
                out.push(Violation::new(R6, "an invalid block is marked valid"));
            }
            if opened && e.has(Status::FAILED_MASK) && refm.never_failed(&e.hash) {
                let msg = format!("block {} is marked failed on a valid path", b.height);
                out.push(Violation::new(R6, msg));
            }
        }
        out
    }

    /// R6 (e) on an open: the final tip is at the height of E's best valid tip (at equal work a
    /// valid tip stays) and not failed in E, and E's loaded tip, when failed, does not strictly
    /// outwork every candidate (DEC-196).
    fn check_outcome(&self, refm: &ChainRef) -> Vec<Violation> {
        let (v, mut out) = (&self.v, Vec::new());
        if v.found.must_refuse() {
            let msg = format!(
                "V started, but the loaded tip, failed in E, outworks every candidate: {:?} \
                 (DEC-196)",
                v.found.failed_tip()
            );
            out.push(Violation::new(R6, msg));
        } else if let Some(best) =
            best_valid(refm, &v.found.e_index, &chain_of(&v.found.e_index, v.found.tip.as_ref()))
        {
            let (tip_h, best_h) = (v.tip().and_then(|t| refm.height(&t)), refm.height(&best));
            if tip_h != best_h {
                let msg = format!(
                    "the final tip is at {tip_h:?}, the best valid tip of E is at {best_h:?}"
                );
                out.push(Violation::new(R6, msg));
            }
        }
        if v.tip().is_some_and(|t| v.found.failed_in_e(&t)) {
            out.push(Violation::new(R6, "the final tip is failed in E (DEC-196)"));
        }
        out
    }

    /// R9 (ST3a §5.1): once a prune is durable, no pruned file (a durable info of size 0 before
    /// the durable last file) is on disk after the startup scan, unless this epoch failed to
    /// remove it, best-effort: the next startup's scan retries it.
    fn check_pruned_gone(&self, world: &World, trace: &Trace, epoch: u32) -> Vec<Violation> {
        use dashd_store::faulty::OpKind;
        use dashd_store::flatfile::FlatFs as _;
        let read = |k: &[u8]| self.s.raw.get(dashd_store::DbSchema::META, k);
        let (Ok(pruned), Ok(last)) =
            (read(crate::store::PRUNED_FILES), read(crate::store::LAST_FILE))
        else {
            return vec![Violation::new(Model, "a check's read failed")];
        };
        if pruned.as_deref() != Some(&[1]) {
            return Vec::new();
        }
        let last = last.and_then(|v| <[u8; 4]>::try_from(v).ok()).map_or(0, u32::from_be_bytes);
        let failed: BTreeSet<&str> = (trace.entries.iter())
            .filter_map(|e| match e {
                TraceEntry::Event {
                    id,
                    op: OpKind::Remove,
                    failed: true,
                    detail,
                    ..
                } if id.epoch == epoch => Some(detail.as_str()),
                _ => None,
            })
            .collect();
        let infos = file_infos(&self.s.raw).expect("a check's read");
        let fs = world.fs();
        let mut out = Vec::new();
        for (n, _) in infos.range(..last).filter(|(_, i)| i.size == 0) {
            for prefix in ["blk", "rev"] {
                let path = format!("{BLOCKS_DIR}/{prefix}{n:05}.dat");
                let present = fs.len(std::path::Path::new(&path)).ok().flatten().is_some();
                if present && !failed.contains(path.as_str()) {
                    let msg = format!("pruned {prefix}{n:05}.dat is still on disk after the scan");
                    out.push(Violation::new(R9, msg));
                }
            }
        }
        out
    }

    /// C9's replay half is not vacuous: a first recovery that opened wrote at most `k` partial
    /// sub-batches and its final batch, or the roll would have come up.
    fn check_replay_crash_came_up(&self, trace: &Trace, epoch: u32) -> Option<Violation> {
        use dashd_store::faulty::OpKind;
        let k = self.s.cfg.crash_replay.filter(|_| epoch == 1)?;
        let appends = trace
            .entries
            .iter()
            .filter(|e| {
                matches!(e, TraceEntry::Event { id, store, op: OpKind::Append(_), failed: false, .. }
                    if id.epoch == 1 && store == CHAIN_STORE)
            })
            .count();
        (appends > k as usize + 1).then(|| {
            let msg = format!(
                "-dbcrashratio's roll at {k} did not come up in a replay of {appends} batches"
            );
            Violation::new(Model, msg)
        })
    }
}

/// R1: every entry of the durable index `durable` has its data and undo read back as the
/// reference's, within the recorded sizes. A file a crash lost after a failed directory sync is
/// entitled (DEC-212): a rev file always, as the load checks none; a blk file only on the load's
/// refusal (`refusing`), since an open that succeeds found every blk file E's index names
/// (BL-127) and nothing loses one while the epoch runs.
fn check_records(
    world: &World,
    files: &Files,
    blocks: &Blocks,
    durable: &Arena,
    infos: &BTreeMap<u32, BlockFileInfo>,
    refusing: bool,
) -> Vec<Violation> {
    let mut out = Vec::new();
    let lost = |prefix: &str, file: Option<i32>| {
        file.is_some_and(|f| {
            let path = format!("{BLOCKS_DIR}/{prefix}{f:05}.dat");
            world.lost_to_failed_dir_sync(std::path::Path::new(&path))
        })
    };
    let reader = files.read(|f| f.reader());
    for e in &durable.entries {
        let Some(want) = blocks.get(&e.hash) else {
            out.push(Violation::new(R1, "an index entry for an unknown block"));
            continue;
        };
        let info = e.file.and_then(|f| infos.get(&(f as u32))).copied().unwrap_or_default();
        if e.has(Status::HAVE_DATA) {
            let mut bytes = Vec::new();
            let read = reader.read_block(e.data(), &mut bytes).map(|()| bytes);
            let at = (e.data_pos, want.data_len(), info.size);
            let want = (want.bytes.as_slice(), refusing && lost("blk", e.file));
            out.extend(check_record("block", e.height, at, read, want));
        }
        if e.has(Status::HAVE_UNDO) {
            let mut bytes = Vec::new();
            let read =
                reader.read_undo(e.undo(), want.parent.as_bytes(), &mut bytes).map(|()| bytes);
            let at = (e.undo_pos, want.undo_len(), info.undo_size);
            let want = (want.undo.as_slice(), lost("rev", e.file));
            out.extend(check_record("undo", e.height, at, read, want));
        }
    }
    out
}

/// R1 for one record at (`pos`, `len`, the file's recorded `size`): it lies within the file's
/// recorded size, and a read that succeeds returns the generated bytes `want`. An unreadable
/// record is unavailable, not lost (DEC-148): D1 judges whether its bytes are durable. A file
/// `lost` by a crash after a failed directory sync reads nothing, as in Core (DEC-212).
fn check_record(
    what: &str,
    height: u32,
    (pos, len, size): (u32, u64, u32),
    read: Result<Vec<u8>, FlatFileError>,
    (want, lost): (&[u8], bool),
) -> Vec<Violation> {
    let mut out = Vec::new();
    if pos < RECORD_HEADER || u64::from(pos - RECORD_HEADER) + len > u64::from(size) {
        out.push(Violation::new(
            R1,
            format!("{what} {height} lies outside its file's recorded size"),
        ));
    }
    match read {
        Ok(b) if b == want => {}
        Err(err) if io_failure(&err) => {}
        Err(_) if lost => {}
        _ => out.push(Violation::new(R1, format!("{what} {height} does not read back"))),
    }
    out
}

/// R5 for one `block_files` record of E (`was`): unchanged, or grown only by a file this epoch
/// wrote (`written`), within the heights this epoch connected (`top`).
fn file_record_kept(
    was: BlockFileInfo,
    now: Option<BlockFileInfo>,
    written: bool,
    top: Option<u32>,
) -> bool {
    let Some(now) = now else {
        return false;
    };
    now == was
        || written
            && now.size >= was.size
            && now.undo_size >= was.undo_size
            && now.blocks >= was.blocks
            && (was.size == 0
                || (now.height_first == was.height_first && now.time_first == was.time_first))
            && now.height_last >= was.height_last
            && now.time_last >= was.time_last
            && (now.height_last == was.height_last || top.is_some_and(|t| now.height_last <= t))
}

/// R5 for one `block_index` entry of E (`was`): its immutable fields and data position survive,
/// and its flags, undo position and validity change only as this epoch's connects (`connected`)
/// and an invalid block (`invalid`) entitle.
fn index_entry_kept(was: &Entry, now: &Entry, connected: bool, invalid: bool) -> bool {
    let immutable = now.height == was.height && now.header == was.header && now.file == was.file;
    let mut entitled = 0;
    if connected {
        entitled |= Status::HAVE_UNDO | Status::VALID_MASK;
    }
    if invalid {
        entitled |= Status::FAILED_MASK;
    }
    let flags = |s: Status| s.0 & !Status::VALID_MASK;
    let undo_ok = now.undo_pos == was.undo_pos
        || (!was.has(Status::HAVE_UNDO) && connected && now.has(Status::HAVE_UNDO));
    let validity_ok =
        (now.status.0 & Status::VALID_MASK) >= (was.status.0 & Status::VALID_MASK) || connected;
    immutable
        && now.data_pos == was.data_pos
        && undo_ok
        && flags(now.status) & flags(was.status) == flags(was.status)
        && flags(now.status) & !flags(was.status) & !entitled == 0
        && validity_ok
}

/// FA2 (ST3a §5.1; CH2 §4.2 step 7): each selection is within its decision's D, and re-arms the
/// automatic check iff it leaves the target unmet where D had met it.
fn fa2(trace: &Trace) -> Vec<Violation> {
    use dashd_store::faulty::PruneDue;
    let mut out = Vec::new();
    // The last decision: (D, automatic, met_target).
    let mut decision: Option<(&[u32], bool, bool)> = None;
    for m in trace.marks() {
        match m {
            Mark::PruneDecision {
                due,
                dry_run,
                met_target,
                ..
            } => decision = Some((dry_run.as_slice(), *due == PruneDue::Automatic, *met_target)),
            Mark::PruneSelected {
                files,
                rearmed,
                target_unmet,
                ..
            } => {
                let Some((d, auto, met)) = decision else {
                    out.push(Violation::new(FA2, "a selection without a decision"));
                    continue;
                };
                if !files.iter().all(|f| d.contains(f)) {
                    let msg = format!("selection {files:?} is not within D = {d:?}");
                    out.push(Violation::new(FA2, msg));
                }
                if *rearmed != (auto && met && *target_unmet) {
                    let msg = format!(
                        "re-armed {rearmed} after a selection that left the target unmet: \
                         {target_unmet} (D met it: {met})"
                    );
                    out.push(Violation::new(FA2, msg));
                }
            }
            _ => {}
        }
    }
    out
}

/// The files `epoch`'s prunes selected.
fn pruned_files(trace: &Trace, epoch: u32) -> BTreeSet<u32> {
    trace
        .marks_with_epoch()
        .filter_map(|(e, m)| match m {
            Mark::PruneSelected {
                files,
                ..
            } if e == epoch => Some(files.iter().copied()),
            _ => None,
        })
        .flatten()
        .collect()
}

/// The blocks connected (applied) in `trace`, or only in `epoch`.
fn connected_blocks(trace: &Trace, epoch: Option<u32>) -> BTreeSet<Hash> {
    trace
        .marks_with_epoch()
        .filter_map(|(e, m)| match m {
            Mark::Connected {
                block,
                applied: true,
                ..
            } if epoch.is_none_or(|x| x == e) => Some(*block),
            _ => None,
        })
        .collect()
}

impl ChainSut {
    /// R13: an independent effect whose batch a completed chainstate barrier covered survives.
    fn effects(&self, trace: &Trace) -> Vec<Violation> {
        use dashd_store::faulty::{CrashKind, OpKind};
        let mut pending: Vec<String> = Vec::new();
        let mut covered: BTreeSet<String> = BTreeSet::new();
        for e in &trace.entries {
            match e {
                TraceEntry::Mark {
                    mark: Mark::Log {
                        line,
                    },
                    ..
                } => {
                    if let Some(k) = line.strip_prefix(EFFECT_WRITTEN) {
                        pending.push(k.to_string());
                    }
                    // A wipe deletes every effect.
                    if line == WIPE_BEGUN {
                        pending.clear();
                        covered.clear();
                    }
                }
                TraceEntry::Event {
                    store,
                    op: OpKind::Barrier | OpKind::SyncWal,
                    failed: false,
                    ..
                } if store == CHAIN_STORE => covered.extend(pending.drain(..)),
                TraceEntry::Crash {
                    kind: CrashKind::PowerLoss,
                    ..
                } => pending.clear(),
                _ => {}
            }
        }
        covered
            .into_iter()
            .filter(|k| self.s.raw.get(cf("llmq_snapshot"), k.as_bytes()).ok().flatten().is_none())
            .map(|k| {
                Violation::new(
                    R13,
                    format!("effect {k}, committed before a completed barrier, is lost"),
                )
            })
            .collect()
    }
}

/// Whether a wipe began after the last flush whose B1 returned.
fn wipe_since_last_durable(trace: &Trace) -> bool {
    let mut wiped = false;
    for m in trace.marks() {
        match m {
            Mark::Durable {
                ..
            } => wiped = false,
            Mark::Log {
                line,
            } if line == WIPE_BEGUN => wiped = true,
            _ => {}
        }
    }
    wiped
}

// Core: validation.cpp FindMostWorkChain (`fMissingData` only off `m_chain`)
/// Which entries of an index are candidates for activation: a block whose whole path has no
/// failed mark and has data off `chain`, the active chain (the entries come parents first). A
/// pruned block on the active chain is never connected, so it needs none.
fn candidates(index: &[Entry], chain: &BTreeSet<BlockHash>) -> Vec<bool> {
    let mut ok: Vec<bool> = Vec::with_capacity(index.len());
    for e in index {
        let parent_ok = e.prev.is_none_or(|p| ok[p.0 as usize]);
        let data = e.has(Status::HAVE_DATA) || chain.contains(&e.hash);
        ok.push(parent_ok && data && !e.has(Status::FAILED_MASK));
    }
    ok
}

/// The hashes on `tip`'s path in `index` (an arena's entries).
fn chain_of(index: &[Entry], tip: Option<&BlockHash>) -> BTreeSet<BlockHash> {
    let mut out = BTreeSet::new();
    let mut at = tip.and_then(|t| index.iter().position(|e| e.hash == *t));
    while let Some(e) = at.map(|i| &index[i]) {
        out.insert(e.hash);
        at = e.prev.map(|p| p.0 as usize);
    }
    out
}

/// Core's best valid tip over an index (R6 (e)): the highest candidate ([`candidates`]) whose
/// whole path is valid; ties go to the lowest branch.
fn best_valid(refm: &ChainRef, index: &[Entry], chain: &BTreeSet<BlockHash>) -> Option<BlockHash> {
    let mut ok: Vec<bool> = Vec::with_capacity(index.len());
    for (e, candidate) in index.iter().zip(candidates(index, chain)) {
        let parent_ok = e.prev.is_none_or(|p| ok[p.0 as usize]);
        ok.push(parent_ok && candidate && refm.block(&e.hash).is_some_and(|b| b.valid));
    }
    index
        .iter()
        .zip(ok)
        .filter(|(_, ok)| *ok)
        .map(|(e, _)| e)
        .max_by_key(|e| (e.height, std::cmp::Reverse(refm.block(&e.hash).map_or(0, |b| b.branch))))
        .map(|e| e.hash)
}

/// The blk and rev file numbers `epoch` wrote.
fn files_written(trace: &Trace, epoch: u32) -> BTreeSet<u32> {
    use dashd_store::faulty::OpKind;
    trace
        .entries
        .iter()
        .filter_map(|e| match e {
            TraceEntry::Event {
                id,
                op: OpKind::WriteAt,
                detail,
                ..
            } if id.epoch == epoch => {
                let name = detail.rsplit('/').next()?;
                let digits = name.strip_prefix("blk").or_else(|| name.strip_prefix("rev"))?;
                digits.split('.').next()?.parse().ok()
            }
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Entry `n` at `height` on entry `prev`, with its block's data or without it (pruned).
    fn entry(n: u8, prev: Option<u32>, height: u32, data: bool) -> Entry {
        let have = if data {
            Status::HAVE_DATA
        } else {
            0
        };
        Entry {
            hash: BlockHash::from([n; 32]),
            prev: prev.map(BlockId),
            height,
            status: Status(Status::VALID_TREE | have),
            n_tx: 1,
            file: data.then_some(0),
            data_pos: 0,
            undo_pos: 0,
            header: BlockHeader::default(),
        }
    }

    /// D-b2-4, which the CRASH runs cannot see, as the SUT's activation and R6 (e) share the
    /// rule: a pruned block on the active chain stays on a candidate's path; off it, a block
    /// with no data is no candidate, and nor is a block above it.
    #[test]
    fn candidates_need_data_only_off_the_active_chain() {
        // 0 ← 1 ← 2, the active chain, 0 and 1 pruned; 3 accepted on 2; 4 a pruned side branch
        // on 1, and 5 on 4 with data; 6 with data on 1.
        let index = [
            entry(0, None, 0, false),
            entry(1, Some(0), 1, false),
            entry(2, Some(1), 2, true),
            entry(3, Some(2), 3, true),
            entry(4, Some(1), 2, false),
            entry(5, Some(4), 3, true),
            entry(6, Some(1), 2, true),
        ];
        let chain = chain_of(&index, Some(&index[2].hash));
        assert_eq!(candidates(&index, &chain), [true, true, true, true, false, false, true]);
        // With no active chain a pruned block is never a candidate, nor anything above it.
        assert!(candidates(&index, &BTreeSet::new()).iter().all(|c| !c));
    }
}
