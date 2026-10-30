//! [`ChainRef`]: the chain the workload builds (ST3a §5.2's `ChainRef`), and the coins and stub
//! evo state at any of its blocks.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use dash_consensus::primitives::BlockHash;
use dashd_store::faulty::Reference;

use super::synth::{GenBlock, generate, generated};
use super::{ChainConfig, ChainOp};
use crate::coins::OutPointKey;

/// The stub evo fold of one block: the previous state, the block, and at the presumed effect's
/// reader height the snapshot it reads (the base block's hash).
pub fn evo_fold(prev: &[u8; 32], block: &BlockHash, snapshot: Option<&[u8]>) -> [u8; 32] {
    let mut buf = [&prev[..], block.as_bytes()].concat();
    if let Some(s) = snapshot {
        buf.extend_from_slice(b"snap");
        buf.extend_from_slice(s);
    }
    dash_consensus::hash::sha256d(&buf)
}

/// The blocks generated so far, by hash, with their branch-0 validity from the configuration.
#[derive(Clone, Default)]
pub struct Blocks {
    pub map: HashMap<BlockHash, Arc<GenBlock>>,
}

impl Blocks {
    /// The block at `height` on `branch` after `parent`.
    pub fn child(
        &mut self,
        cfg: &ChainConfig,
        parent: Option<&BlockHash>,
        height: u32,
        branch: u8,
    ) -> Arc<GenBlock> {
        let p = parent.map(|h| self.map[h].clone());
        let invalid = branch == 0 && cfg.invalid.contains(&height);
        let b = generate(p.as_deref(), height, branch, invalid, cfg.core);
        self.map.entry(b.hash).or_insert_with(|| b.clone());
        b
    }

    pub fn get(&self, h: &BlockHash) -> Option<&Arc<GenBlock>> {
        self.map.get(h)
    }

    /// The hashes from genesis to `tip`, by height.
    pub fn path(&self, tip: &BlockHash) -> Vec<BlockHash> {
        let mut out = Vec::new();
        let mut cur = self.map.get(tip);
        while let Some(b) = cur {
            out.push(b.hash);
            cur = (b.height > 0).then(|| &self.map[&b.parent]);
        }
        out.reverse();
        out
    }
}

/// The UTXO set at the end of `path` (genesis's outputs never enter it).
pub fn coins_along<'a>(
    path: impl IntoIterator<Item = &'a GenBlock>,
) -> BTreeMap<OutPointKey, Vec<u8>> {
    let mut coins = BTreeMap::new();
    for b in path {
        for (k, _) in &b.spends {
            coins.remove(k);
        }
        coins.extend(b.adds.iter().cloned());
    }
    coins
}

/// The reference.
#[derive(Clone)]
pub struct ChainRef {
    cfg: ChainConfig,
    pub blocks: Blocks,
    /// Every block the workload accepted or connected on the current chain.
    headers: Vec<BlockHash>,
    /// How many of `headers` are connected: the active chain.
    connected: usize,
    /// The branch the next accepted block extends: a reorg and `invalidateblock` start a new one.
    branch: u8,
    next_branch: u8,
    /// The blocks the workload itself makes (not R12's lookahead).
    workload: BTreeSet<BlockHash>,
    /// The blocks `invalidateblock` marked failed (their descendants fail with them).
    invalidated: BTreeSet<BlockHash>,
    /// The workload's ops.
    ops: usize,
}

impl ChainRef {
    /// The next block of the workload's current chain, on `branch`.
    fn extend(&mut self, branch: u8) {
        let tip = *self.headers.last().expect("genesis");
        let b = self.blocks.child(&self.cfg, Some(&tip), self.headers.len() as u32, branch);
        self.workload.insert(b.hash);
        self.headers.push(b.hash);
    }

    /// `n` connects, each of a block extended first if none is accepted; the first invalid block
    /// stops them.
    fn connect(&mut self, n: u32, branch: u8) {
        for _ in 0..n {
            if self.connected == self.headers.len() {
                self.extend(branch);
            }
            if !self.blocks.map[&self.headers[self.connected]].valid {
                break;
            }
            self.connected += 1;
        }
    }

    /// The active chain, by height.
    pub fn active(&self) -> &[BlockHash] {
        &self.headers[..self.connected]
    }

    /// How many ops the workload has.
    pub fn ops(&self) -> usize {
        self.ops
    }

    /// V's `branch_from_tip`: the next block extends `tip`'s branch, unless V's index marks that
    /// branch's next block failed (invalidated, or invalid with data, which an activation to
    /// `tip` tried), then a fresh branch.
    fn branch_from(&mut self, tip: &BlockHash) -> u8 {
        let b = self.blocks.map[tip].clone();
        let next = self.blocks.child(&self.cfg, Some(tip), b.height + 1, b.branch);
        if self.failed(&next) {
            self.next_branch += 1;
            self.next_branch - 1
        } else {
            b.branch
        }
    }

    /// Whether V's index may mark `b` failed: invalidated, or invalid with data.
    fn failed(&self, b: &GenBlock) -> bool {
        self.invalidated.contains(&b.hash) || (!b.valid && self.workload.contains(&b.hash))
    }

    pub fn block(&self, h: &BlockHash) -> Option<&Arc<GenBlock>> {
        self.blocks.get(h)
    }

    pub fn height(&self, h: &BlockHash) -> Option<u32> {
        self.blocks.get(h).map(|b| b.height)
    }

    /// Whether `a` is `b` or an ancestor of it.
    pub fn is_ancestor(&self, a: &BlockHash, b: &BlockHash) -> bool {
        let (Some(ha), Some(_)) = (self.height(a), self.height(b)) else {
            return false;
        };
        self.blocks.path(b).get(ha as usize) == Some(a)
    }

    /// The UTXO set at `tip` (`None`: the empty chainstate).
    pub fn coins_at(&self, tip: Option<&BlockHash>) -> BTreeMap<OutPointKey, Vec<u8>> {
        let path = tip.map(|t| self.blocks.path(t)).unwrap_or_default();
        coins_along(path.iter().map(|h| &*self.blocks.map[h]))
    }

    /// The stub evo state at `tip`.
    pub fn evo_at(&self, tip: &BlockHash) -> [u8; 32] {
        let path = self.blocks.path(tip);
        let mut state = [0u8; 32];
        for (h, id) in path.iter().enumerate() {
            let snap = self
                .cfg
                .presumed
                .filter(|(_, reader)| *reader == h as u32)
                .and_then(|(base, _)| path.get(base as usize));
            state = evo_fold(&state, id, snap.map(|s| &s.as_bytes()[..]));
        }
        state
    }

    /// Whether every block from genesis to `tip` is valid.
    pub fn valid_path(&self, tip: &BlockHash) -> bool {
        self.blocks.path(tip).iter().all(|h| self.blocks.map[h].valid)
    }

    /// Whether no block on `tip`'s path is invalid or invalidated, so that V's index may mark
    /// none of them failed (R6 (d)).
    pub fn never_failed(&self, tip: &BlockHash) -> bool {
        self.valid_path(tip) && self.blocks.path(tip).iter().all(|h| !self.invalidated.contains(h))
    }

    /// The reference as the checks of a warranted refusal (DEC-196) query it: `ChainSut::open`
    /// does not get the harness's, and every query those checks make reads only blocks, by hash,
    /// and `cfg`'s presumed effect. The blocks this process generated hold the harness's
    /// reference's, the same by hash, so for every block of the run the view answers as it does;
    /// D-c6-5 bounds what it answers for the blocks of other configurations, which it holds too.
    pub fn checks_view(cfg: &ChainConfig) -> Self {
        let mut r = <Self as Reference>::new(cfg);
        r.blocks.map = generated();
        r
    }
}

impl Reference for ChainRef {
    type Op = ChainOp;
    type Config = ChainConfig;

    fn new(cfg: &ChainConfig) -> Self {
        let mut r = Self {
            cfg: cfg.clone(),
            blocks: Blocks::default(),
            headers: Vec::new(),
            connected: 1,
            branch: 0,
            next_branch: 1,
            workload: BTreeSet::new(),
            invalidated: BTreeSet::new(),
            ops: 0,
        };
        let g = r.blocks.child(cfg, None, 0, 0);
        r.workload.insert(g.hash);
        r.headers.push(g.hash);
        r
    }

    fn apply(&mut self, op: &ChainOp) {
        self.ops += 1;
        let branch = self.branch;
        match op {
            ChainOp::Connect(n) => self.connect(*n, branch),
            ChainOp::Continue => self.connect(1, branch),
            ChainOp::FlushDuring(d) => self.connect(*d, branch),
            ChainOp::FlushRewind(d) => {
                // V disconnects `d` blocks of the branch, dropping the accepted ones; the next
                // connects take the same blocks again.
                self.connected = self.connected.saturating_sub(*d as usize).max(1);
                self.headers.truncate(self.connected);
            }
            ChainOp::Accept(n) => {
                for _ in 0..*n {
                    self.extend(branch);
                }
            }
            ChainOp::Invalidate(h) => {
                // V disconnects below block `h`, which stays failed: the next connects extend a
                // new branch from its parent. Genesis is never invalidated, and a height off the
                // active chain changes nothing.
                let h = (*h as usize).max(1);
                if h < self.connected {
                    self.invalidated.insert(self.headers[h]);
                    self.headers.truncate(h);
                    self.connected = h;
                    self.branch = self.next_branch;
                    self.next_branch += 1;
                }
            }
            ChainOp::Reorg {
                fork,
                len,
            }
            | ChainOp::FlushReorg {
                fork,
                len,
            } => {
                // V disconnects to `fork` (to the tip at most), dropping the accepted blocks.
                self.headers.truncate((*fork as usize + 1).min(self.connected));
                self.connected = self.headers.len();
                let branch = self.next_branch;
                self.branch = branch;
                self.next_branch += 1;
                for _ in 0..*len {
                    self.extend(branch);
                }
                self.connected = self.headers.len();
            }
            ChainOp::Wipe => {
                // V re-activates from nothing: the highest block the workload made whose path is
                // valid and never invalidated, ties to the lowest branch.
                let usable = |h: &BlockHash| {
                    self.blocks
                        .path(h)
                        .iter()
                        .all(|id| self.blocks.map[id].valid && !self.invalidated.contains(id))
                };
                let tip = *self
                    .workload
                    .iter()
                    .filter(|h| usable(h))
                    .max_by_key(|h| {
                        (self.blocks.map[*h].height, Reverse(self.blocks.map[*h].branch))
                    })
                    .expect("genesis");
                self.headers = self.blocks.path(&tip);
                self.connected = self.headers.len();
                self.branch = self.branch_from(&tip);
            }
            ChainOp::Flush
            | ChainOp::IndexWrite
            | ChainOp::TestValidity
            | ChainOp::Corrupt
            | ChainOp::Prune(_)
            | ChainOp::AutoTarget(_)
            | ChainOp::PruneRequest(_)
            | ChainOp::SubSync(_)
            | ChainOp::SubTick
            | ChainOp::SubDisable(_) => {}
        }
        // R12's continuation connects one block past whatever recovery reaches: on the tip's
        // branch, or on a fresh one (V's next unused number, at most the reference's) when that
        // branch's next block may be failed. Generate those after every block the workload knows.
        let known: Vec<(BlockHash, u32, u8)> = self
            .blocks
            .map
            .values()
            .filter(|b| self.workload.contains(&b.hash))
            .map(|b| (b.hash, b.height, b.branch))
            .collect();
        for (h, height, own) in known {
            let next = self.blocks.child(&self.cfg, Some(&h), height + 1, own);
            if self.failed(&next) {
                for branch in 0..=self.next_branch {
                    self.blocks.child(&self.cfg, Some(&h), height + 1, branch);
                }
            }
        }
    }
}
