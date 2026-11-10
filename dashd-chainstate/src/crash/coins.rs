//! [`CoinsSut`]: CH2's coins profile on ST3a's harness (CH2 §6.4): [`CoinsCache`], W through
//! [`FlushControl`] and the [`InlineDriver`], and [`Recovery`](crate::recovery::Recovery) over the
//! chainstate `FaultyEngine`, with no block data and no evo state.
//!
//! Without block files a marker cannot be replayed, so every flush is one batch (S unbounded) and
//! no marker is ever durable. V connects [`ChainSut`](super::ChainSut)'s synthetic blocks with the
//! same mini connect, and disconnects them from the undo data it generated. Recovery's
//! `IndexAccess` is the generated chain below the durable tip.

use std::sync::Arc;

use dash_chain::{BlockRef, IndexAccess as _, IndexWrite, Status};
use dash_consensus::coins::BlockUndoRef;
use dash_consensus::primitives::BlockHash;
use dashd_store::faulty::InvariantId::{R2, R3};
use dashd_store::faulty::{
    Abort, FatalCause, FaultyEngine, Mark, RefExtractor, ScriptCheck, Sut, Trace, Violation, World,
    allowed_tips,
};
use dashd_store::{DbSchema, KvRead};

use super::chain::{Arena, Entry};
use super::reference::{Blocks, ChainRef};
use super::stores::{Engines, Refusal, Stores, StubEvo, cache_at, crash_roll};
use super::synth::{GenBlock, lookup};
use super::world::hash;
use super::{ChainConfig, ChainOp, ChainRefs, PATH_CONNECT, PATH_DISCONNECT, mini, stores};
use crate::coins::{BlockOverlay, CoinsCache};
use crate::flush::{FlushCx, FlushStateMode, NoPrune};
use crate::store::{HEAD_BLOCKS, TIP};

/// The generated chain from genesis to `tip`, as the index recovery reads.
fn arena_to(tip: Option<&BlockHash>) -> Arena {
    let mut path = Vec::new();
    let mut cur = tip.and_then(lookup);
    while let Some(g) = cur {
        cur = (g.height > 0).then(|| lookup(&g.parent)).flatten();
        path.push(g);
    }
    let mut arena = Arena::default();
    for g in path.iter().rev() {
        index(&mut arena, g);
    }
    arena
}

/// `g`'s index entry, valid and with data (no files hold it: nothing reads it).
fn index(arena: &mut Arena, g: &GenBlock) {
    if arena.lookup(&g.hash).is_some() {
        return;
    }
    let prev = (g.height > 0).then(|| arena.block_ref(&g.parent).id);
    arena.push(Entry {
        hash: g.hash,
        prev,
        height: g.height,
        status: Status(Status::HAVE_DATA | Status::VALID_SCRIPTS),
        n_tx: g.block.txs().len() as u32,
        file: None,
        data_pos: 0,
        undo_pos: 0,
        header: *g.block.header().raw(),
    });
}

/// The coins profile's system under test.
pub struct CoinsSut {
    s: Stores,
    blocks: Blocks,
    cache: CoinsCache<FaultyEngine>,
    index: Arena,
    active: Vec<BlockHash>,
    branch: u8,
    next_branch: u8,
    seq: u64,
    /// The ops applied, and the first that refused (a precondition).
    ran: usize,
    refused: Option<Refusal>,
}

impl CoinsSut {
    fn tip_ref(&self) -> BlockRef {
        self.index.block_ref(self.active.last().expect("a connected genesis"))
    }

    fn connect_block(&mut self, g: &GenBlock) -> Result<(), Abort> {
        index(&mut self.index, g);
        self.s.world.mark(Mark::ConnectBegin);
        let best = self.index.block_ref(&g.hash);
        let mut ov = BlockOverlay::new(&mut self.cache);
        assert!(mini::connect(&mut ov, g)?, "the coins profile's blocks are valid");
        ov.commit(best).expect("a block's view merges");
        self.active.push(g.hash);
        self.seq += 1;
        self.s.world.mark(Mark::Connected {
            block: hash(&g.hash),
            scripts: ScriptCheck::Full,
            applied: true,
        });
        self.s.world.mark(Mark::ConnectEnd);
        Ok(())
    }

    fn connect(&mut self, n: u32) -> Result<(), Abort> {
        for _ in 0..n {
            let parent = self.active.last().copied();
            let height = self.active.len() as u32;
            let g = self.blocks.child(&self.s.cfg, parent.as_ref(), height, self.branch);
            self.s.enter(PATH_CONNECT);
            self.connect_block(&g)?;
            self.s.leave();
        }
        Ok(())
    }

    /// The tip's disconnect from the undo data it was generated with.
    fn disconnect_tip(&mut self) -> Result<(), Abort> {
        let h = self.active.pop().expect("a block above genesis");
        let g = self.blocks.get(&h).expect("generated").clone();
        let undo = BlockUndoRef::decode(&mut g.undo.as_slice()).expect("generated undo decodes");
        let parent = self.index.block_ref(&g.parent);
        self.s.enter(PATH_DISCONNECT);
        mini::disconnect(BlockOverlay::new(&mut self.cache), &g, &undo, parent)?;
        self.s.leave();
        self.seq += 1;
        Ok(())
    }

    fn reorg(&mut self, fork: u32, len: u32) -> Result<(), Abort> {
        while self.active.len() as u32 > fork + 1 {
            self.disconnect_tip()?;
        }
        self.branch = self.next_branch;
        self.next_branch += 1;
        self.connect(len)
    }

    /// One op on V (and W).
    fn run(&mut self, op: &ChainOp) -> Result<(), Abort> {
        if let Some(m) = self.s.obs.aborted() {
            return Err(Abort::new(m));
        }
        match op {
            ChainOp::Connect(n) => self.connect(*n),
            ChainOp::Flush => self.flush(),
            ChainOp::Reorg {
                fork,
                len,
            } => self.reorg(*fork, *len),
            ChainOp::Continue => {
                self.connect(1)?;
                self.flush()
            }
            op => panic!("`{op}` is not in the coins profile"),
        }
    }

    /// An ALWAYS flush, one batch.
    fn flush(&mut self) -> Result<(), Abort> {
        let (tip, last_seq, coins) = (self.tip_ref(), self.seq, &mut self.cache);
        self.s.with_ctl(|ctl| {
            let mut cx = FlushCx {
                now: self.s.t0,
                coins,
                mempool_usage: 0,
                tip,
                last_seq,
                index: &mut Vec::<IndexWrite>::new(),
                evo: &mut StubEvo::default(),
                prune: &mut NoPrune,
                manual_prune_height: None,
                reindex: false,
                is_ibd: false,
                left_ibd: false,
                scripted: false,
            };
            ctl.flush(FlushStateMode::Always, &mut cx)
        })
    }
}

impl Sut for CoinsSut {
    type Config = ChainConfig;
    type Op = ChainOp;
    type Ref = ChainRef;

    const NAME: &'static str = "coinssut";

    fn open(world: &World, cfg: &ChainConfig) -> Result<Self, Abort> {
        assert_eq!(
            *cfg,
            ChainConfig::default(),
            "the coins profile has one-batch flushes and no invalid blocks, effects or crash roll"
        );
        let engines = Engines::new(world, cfg);
        let opened = stores::open(world, &engines, cfg)?;
        let index = arena_to(opened.meta().tip.as_ref());
        let rec = stores::recover(world, opened, &index, u64::MAX, crash_roll(None))?;
        let cache = cache_at(engines.db(), rec.tip);
        let mut blocks = Blocks::default();
        let active = index.path(rec.tip.as_ref().map(|t| &t.hash));
        for h in &active {
            let g = lookup(h).expect("generated");
            blocks.map.insert(g.hash, g);
        }
        let branch = active.last().map_or(0, |t| blocks.map[t].branch);
        let mut sut = Self {
            s: Stores::new(world, cfg, engines, rec.files, None),
            blocks,
            cache,
            index,
            active,
            branch,
            // As the reference's: only the forward run reorgs, from an empty chainstate.
            next_branch: 1,
            seq: 0,
            ran: 0,
            refused: None,
        };
        if sut.active.is_empty() {
            sut.connect(1)?;
        }
        Ok(sut)
    }

    fn apply(&mut self, op: &ChainOp) -> Result<(), Abort> {
        self.ran += 1;
        let r = self.run(op);
        if let Err(a) = &r {
            self.refused.get_or_insert_with(|| (self.ran, a.message.clone()));
        }
        r
    }

    /// Without a fault: every op returned, and V's active chain is the reference's.
    fn preconditions(&self, _: &World, refm: &ChainRef, trace: &Trace) -> Result<(), String> {
        stores::forward_preconditions(&self.s.cfg, self.refused.as_ref(), &self.active, refm, trace)
    }

    fn check(&self, _: &World, refm: &ChainRef, trace: &Trace) -> Result<(), Vec<Violation>> {
        let mut v = Vec::new();
        let meta = |k: &[u8]| self.s.raw.get(DbSchema::META, k).ok().flatten();
        if meta(HEAD_BLOCKS).is_some() {
            v.push(Violation::new(R2, "head_blocks present in the coins profile"));
        }
        let tip = meta(TIP).and_then(|t| <[u8; 32]>::try_from(t.as_slice()).ok());
        let (_, allowed) = allowed_tips(trace);
        if !allowed.contains(&tip) {
            v.push(Violation::new(R3, "the recovered tip is not allowed"));
        }
        let tip = tip.map(BlockHash::from);
        if self.s.coins() != refm.coins_at(tip.as_ref()) {
            v.push(Violation::new(R3, "coins differ from the reference at the recovered tip"));
        }
        if v.is_empty() {
            Ok(())
        } else {
            Err(v)
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

    /// `stores::sanctioned_after`'s table, which no block-file row reaches here.
    fn sanctioned_after(cfg: &ChainConfig, message: &str, cause: &FatalCause) -> bool {
        stores::sanctioned_after(cfg, message, cause)
    }

    fn sanctioned(cfg: &ChainConfig, message: &str) -> bool {
        stores::sanctioned(cfg, message)
    }

    fn projection(&self) -> Option<[u8; 32]> {
        Some(self.s.projection(self.active.last(), || None))
    }
}
