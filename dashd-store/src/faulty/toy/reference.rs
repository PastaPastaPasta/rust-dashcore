//! The toy's reference model (§5.2 `ChainRef`): the chain the workload builds, and the coins,
//! state and subscriber digests at any of its blocks.
//!
//! The toy and the reference generate blocks the same way ([`Gen`]), so a block's bytes, id and
//! validity are a function of (parent, height, branch).

use std::collections::BTreeMap;

use bitcoin_hashes::{Hash as _, HashEngine, sha256d};

use crate::faulty::toy::block::{NULL, block_bytes, block_id, undo_bytes};
use crate::faulty::toy::{IndexEntry, STATUS_DATA, STATUS_FAILED, ToyBug, ToyConfig, ToyOp};
use crate::flatfile::{CoreBytes, CoreUndoBytes};
use dash_kv::faulty::Hash;
use dash_kv::faulty::Reference;

/// One generated block.
#[derive(Debug, Clone)]
pub struct GenBlock {
    pub id: Hash,
    pub parent: Hash,
    pub height: u32,
    pub branch: u8,
    pub bytes: CoreBytes,
    pub undo: CoreUndoBytes,
    pub valid: bool,
}

/// The deterministic block generator both the toy and its reference use.
#[derive(Debug, Clone, Default)]
pub struct Gen {
    pub blocks: BTreeMap<Hash, GenBlock>,
}

impl Gen {
    /// The block at `height` on `branch` after `parent` (generated once).
    pub fn block(&mut self, cfg: &ToyConfig, parent: &Hash, height: u32, branch: u8) -> GenBlock {
        let bytes = block_bytes(parent, height, branch, cfg.block_size as usize);
        let id = block_id(bytes.as_slice());
        self.blocks
            .entry(id)
            .or_insert_with(|| GenBlock {
                id,
                parent: *parent,
                height,
                branch,
                undo: undo_bytes(&id, cfg.undo_size as usize),
                bytes,
                valid: !(branch == 0 && cfg.invalid.contains(&height)),
            })
            .clone()
    }

    pub fn get(&self, id: &Hash) -> Option<&GenBlock> {
        self.blocks.get(id)
    }

    /// The ids from genesis to `tip`, by height.
    pub fn path(&self, tip: &Hash) -> Vec<Hash> {
        let mut out = Vec::new();
        let mut cur = *tip;
        while cur != NULL {
            let b = &self.blocks[&cur];
            out.push(cur);
            cur = b.parent;
        }
        out.reverse();
        out
    }
}

pub(crate) fn sha(parts: &[&[u8]]) -> Hash {
    let mut e = sha256d::Hash::engine();
    for p in parts {
        e.input(p);
    }
    sha256d::Hash::from_engine(e).to_byte_array()
}

/// The coins digest of a coins table: `(height, id)` pairs in height order.
pub fn coins_digest<'a>(entries: impl Iterator<Item = (u32, &'a Hash)>) -> Hash {
    let mut e = sha256d::Hash::engine();
    for (h, id) in entries {
        e.input(&h.to_be_bytes());
        e.input(id);
    }
    sha256d::Hash::from_engine(e).to_byte_array()
}

/// The evo fold of one block: the previous state, the block, and (at the presumed effect's
/// reader height) the rotation snapshot it reads.
pub fn evo_fold(prev: &Hash, id: &Hash, snapshot: Option<&Hash>) -> Hash {
    match snapshot {
        Some(s) => sha(&[prev, id, b"snap", s]),
        None => sha(&[prev, id]),
    }
}

/// The script-check decision for connecting a block (Core's assumevalid): skipped for the first
/// branch's blocks up to the configured height, full otherwise.
pub fn scripts(cfg: &ToyConfig, b: &GenBlock) -> dash_kv::faulty::ScriptCheck {
    use dash_kv::faulty::ScriptCheck;
    match cfg.assumevalid {
        Some(av) if b.branch == 0 && b.height <= av => ScriptCheck::Skipped,
        _ => ScriptCheck::Full,
    }
}

/// The reference.
pub struct ToyRef {
    pub cfg: ToyConfig,
    pub blocks: Gen,
    /// The active chain after the whole workload.
    pub active: Vec<Hash>,
    /// Every block the workload accepted or connected on the current chain.
    pub headers: Vec<Hash>,
    connected: usize,
    next_branch: u8,
    /// The blocks the workload itself makes (not R12's lookahead).
    workload: std::collections::BTreeSet<Hash>,
}

impl ToyRef {
    pub fn block(&self, id: &Hash) -> Option<&GenBlock> {
        self.blocks.get(id)
    }

    /// The coins digest at `tip` (`None`: the empty chain).
    pub fn coins_digest_at(&self, tip: Option<&Hash>) -> Hash {
        let path = tip.map(|t| self.blocks.path(t)).unwrap_or_default();
        coins_digest(path.iter().enumerate().map(|(h, id)| (h as u32, id)))
    }

    /// The evo state at `tip`.
    pub fn state_at(&self, tip: Option<&Hash>) -> Hash {
        let path = tip.map(|t| self.blocks.path(t)).unwrap_or_default();
        let mut state = [0u8; 32];
        for (h, id) in path.iter().enumerate() {
            let snap = (Some(h as u32) == self.cfg.presumed_reader)
                .then(|| self.cfg.presumed_base.and_then(|b| path.get(b as usize)))
                .flatten();
            state = evo_fold(&state, id, snap);
        }
        state
    }

    /// The subscriber's data digest at `tip`: one record per height.
    pub fn subscriber_digest_at(&self, tip: Option<&Hash>) -> Hash {
        self.coins_digest_at(tip)
    }

    /// The script-check decision a connect of `id` must report (R6).
    pub fn scripts(&self, id: &Hash) -> Option<dash_kv::faulty::ScriptCheck> {
        self.blocks.get(id).map(|b| scripts(&self.cfg, b))
    }

    pub fn height(&self, id: &Hash) -> Option<u32> {
        self.blocks.get(id).map(|b| b.height)
    }

    /// Whether `a` is `b` or an ancestor of it.
    pub fn is_ancestor(&self, a: &Hash, b: &Hash) -> bool {
        let (Some(ha), Some(_)) = (self.height(a), self.height(b)) else {
            return false;
        };
        self.blocks.path(b).get(ha as usize) == Some(a)
    }

    /// Core's best valid tip over an index (R6(e)): the highest block whose whole path has data,
    /// is valid, and is not marked failed; ties go to the first branch.
    pub fn best_valid(&self, index: &[IndexEntry]) -> Option<Hash> {
        let have: BTreeMap<Hash, &IndexEntry> = index.iter().map(|e| (e.hash, e)).collect();
        let ok = |id: &Hash| {
            have.get(id)
                .is_some_and(|e| e.status & STATUS_DATA != 0 && e.status & STATUS_FAILED == 0)
                && self.blocks.get(id).is_some_and(|b| b.valid)
        };
        index
            .iter()
            .filter(|e| self.blocks.path(&e.hash).iter().all(ok))
            .max_by_key(|e| {
                (e.height, std::cmp::Reverse(self.blocks.get(&e.hash).map_or(0, |b| b.branch)))
            })
            .map(|e| e.hash)
    }
}

impl Reference for ToyRef {
    type Op = ToyOp;
    type Config = ToyConfig;

    fn new(cfg: &ToyConfig) -> Self {
        let mut r = Self {
            cfg: cfg.clone(),
            blocks: Gen::default(),
            active: Vec::new(),
            headers: Vec::new(),
            connected: 1,
            next_branch: 1,
            workload: Default::default(),
        };
        let g = r.blocks.block(cfg, &NULL, 0, 0);
        r.workload.insert(g.id);
        r.headers.push(g.id);
        r.active.push(g.id);
        r
    }

    fn apply(&mut self, op: &ToyOp) {
        let cfg = self.cfg.clone();
        let branch = self.blocks.get(self.headers.last().expect("genesis")).map_or(0, |b| b.branch);
        let extend = |r: &mut Self, branch: u8| {
            let tip = *r.headers.last().expect("genesis");
            let b = r.blocks.block(&cfg, &tip, r.headers.len() as u32, branch);
            r.workload.insert(b.id);
            r.headers.push(b.id);
        };
        match op {
            ToyOp::Connect(n) | ToyOp::Import(n) => {
                for _ in 0..*n {
                    if self.connected == self.headers.len() {
                        extend(self, branch);
                    }
                    if !self.blocks.blocks[&self.headers[self.connected]].valid
                        && self.cfg.bug != Some(ToyBug::T21)
                    {
                        break;
                    }
                    self.connected += 1;
                }
            }
            ToyOp::Accept(n) => {
                for _ in 0..*n {
                    extend(self, branch);
                }
            }
            ToyOp::FlushAt {
                during,
                ..
            } => {
                // V connects `during` blocks while the flush runs.
                for _ in 0..*during {
                    if self.connected == self.headers.len() {
                        extend(self, branch);
                    }
                    self.connected += 1;
                }
            }
            ToyOp::Invalidate(h) => {
                // V disconnects below block `h`; the next connect regenerates it.
                let h = (*h as usize).min(self.connected);
                self.headers.truncate(h);
                self.connected = h;
            }
            ToyOp::Reorg {
                fork,
                len,
            } => {
                self.headers.truncate(*fork as usize + 1);
                let branch = self.next_branch;
                self.next_branch += 1;
                for _ in 0..*len {
                    extend(self, branch);
                }
                self.connected = self.headers.len();
            }
            _ => {}
        }
        self.active = self.headers[..self.connected].to_vec();
        // R12's continuation connects one block past whatever recovery reaches: generate the
        // next block after every block the workload knows.
        let known: Vec<(Hash, u32, u8)> = self
            .blocks
            .blocks
            .values()
            .filter(|b| self.workload.contains(&b.id))
            .map(|b| (b.id, b.height, b.branch))
            .collect();
        for (id, h, branch) in known {
            self.blocks.block(&cfg, &id, h + 1, branch);
        }
    }
}
