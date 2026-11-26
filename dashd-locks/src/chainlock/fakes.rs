//! Doubles for the ChainLock signer and first-seen tests.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use dash_consensus::Tx;
use dash_consensus::primitives::{
    Block, BlockHeader, Header, OutPoint, Transaction, TxHash, TxIn, TxType,
};
use dash_primitives::BlockHash;
use parking_lot::Mutex;

use super::reads::{ChainEntry, ChainReads};
use super::signer::{ClReads, ClScheduler, has_chainlock, has_conflicting_chainlock};
use crate::signing::TxSource;

type Entries = HashMap<BlockHash, (ChainEntry, Option<Arc<Block>>)>;
type Task = (Arc<dyn Fn() + Send + Sync>, Duration);

/// A block tree: entries by hash, each block's body unless forgotten, and the active tip.
#[derive(Default)]
pub struct FakeChain {
    entries: Mutex<Entries>,
    tip: Mutex<Option<BlockHash>>,
    nonce: Mutex<u32>,
}

impl FakeChain {
    /// A block on `prev` (`None`: a genesis) at header time `time`; it becomes the tip.
    pub fn mine_on(&self, prev: Option<BlockHash>, txs: Vec<Tx>, time: u32) -> Arc<Block> {
        let nonce = {
            let mut n = self.nonce.lock();
            *n += 1;
            *n
        };
        let base = BlockHeader::default();
        let header = BlockHeader::new(
            0x2000_0000,
            prev.unwrap_or(BlockHash::from([0; 32])),
            base.merkle_root(),
            time,
            base.bits(),
            nonce,
        );
        let block =
            Arc::new(Block::new(Header::new(header), txs.into_iter().map(Arc::new).collect()));
        let height = prev.map_or(0, |p| self.entry(&p).height + 1);
        let entry = ChainEntry {
            hash: block.hash(),
            height,
            prev,
        };
        self.entries.lock().insert(block.hash(), (entry, Some(Arc::clone(&block))));
        *self.tip.lock() = Some(block.hash());
        block
    }

    /// A block on the tip.
    pub fn mine(&self, txs: Vec<Tx>, time: u32) -> Arc<Block> {
        let tip = *self.tip.lock();
        self.mine_on(tip, txs, time)
    }

    pub fn entry(&self, hash: &BlockHash) -> ChainEntry {
        self.entries.lock()[hash].0
    }

    pub fn set_tip(&self, hash: BlockHash) {
        *self.tip.lock() = Some(hash);
    }

    /// Makes reading `hash`'s block fail.
    pub fn forget_body(&self, hash: &BlockHash) {
        self.entries.lock().get_mut(hash).expect("a known block").1 = None;
    }

    /// Removes `hash` from the tree.
    pub fn forget(&self, hash: &BlockHash) {
        self.entries.lock().remove(hash);
    }
}

impl ChainReads for FakeChain {
    fn tip(&self) -> Option<ChainEntry> {
        let tip = (*self.tip.lock())?;
        self.lookup(&tip)
    }

    fn lookup(&self, hash: &BlockHash) -> Option<ChainEntry> {
        self.entries.lock().get(hash).map(|(e, _)| *e)
    }

    fn ancestor_hash(&self, hash: &BlockHash, height: i32) -> Option<BlockHash> {
        let mut at = self.lookup(hash)?;
        while at.height > height {
            at = self.lookup(&at.prev?)?;
        }
        (at.height == height).then_some(at.hash)
    }

    fn read_block(&self, hash: &BlockHash) -> Option<Arc<Block>> {
        self.entries.lock().get(hash).and_then(|(_, b)| b.clone())
    }
}

/// SPORK_19 and the best ChainLock's block over a [`FakeChain`].
pub struct FakeCl {
    pub chain: Arc<FakeChain>,
    pub enabled: Mutex<bool>,
    pub signing_enabled: Mutex<bool>,
    pub best: Mutex<Option<ChainEntry>>,
    /// Best heights read, in order, before the best block's.
    pub heights: Mutex<VecDeque<i32>>,
}

impl FakeCl {
    pub fn new(chain: Arc<FakeChain>) -> Arc<Self> {
        Arc::new(Self {
            chain,
            enabled: Mutex::new(true),
            signing_enabled: Mutex::new(true),
            best: Mutex::new(None),
            heights: Mutex::default(),
        })
    }

    pub fn lock_at(&self, hash: &BlockHash) {
        *self.best.lock() = Some(self.chain.entry(hash));
    }
}

impl ClReads for FakeCl {
    fn enabled(&self) -> bool {
        *self.enabled.lock()
    }

    fn signing_enabled(&self) -> bool {
        *self.signing_enabled.lock()
    }

    fn best_height(&self) -> i32 {
        let best = self.best.lock().map_or(-1, |b| b.height);
        self.heights.lock().pop_front().unwrap_or(best)
    }

    fn has_chainlock(&self, height: i32, hash: &BlockHash) -> bool {
        self.enabled() && has_chainlock(self.best.lock().as_ref(), height, hash, &*self.chain)
    }

    fn has_conflicting_chainlock(&self, height: i32, hash: &BlockHash) -> bool {
        self.enabled()
            && has_conflicting_chainlock(self.best.lock().as_ref(), height, hash, &*self.chain)
    }
}

/// Holds the scheduled task and counts stops.
#[derive(Default)]
pub struct FakeScheduler {
    task: Mutex<Option<Task>>,
    once: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
    pub stops: Mutex<usize>,
}

impl FakeScheduler {
    pub fn every(&self) -> Option<Duration> {
        self.task.lock().as_ref().map(|(_, d)| *d)
    }

    /// Runs the one-shot tasks scheduled so far; returns how many ran.
    pub fn run_once(&self) -> usize {
        let tasks = std::mem::take(&mut *self.once.lock());
        let n = tasks.len();
        tasks.into_iter().for_each(|t| t());
        n
    }

    /// Runs the scheduled task once.
    pub fn run(&self) {
        let task = self.task.lock().as_ref().map(|(t, _)| Arc::clone(t)).expect("a task");
        task();
    }
}

impl ClScheduler for FakeScheduler {
    fn schedule_every(&self, task: Box<dyn Fn() + Send + Sync>, every: Duration) {
        *self.task.lock() = Some((Arc::from(task), every));
    }

    fn schedule(&self, task: Box<dyn FnOnce() + Send>) {
        self.once.lock().push(task);
    }

    fn stop(&self) {
        *self.stops.lock() += 1;
    }
}

/// Transactions by txid: `None` in the mempool, `Some(block)` mined in `block`.
#[derive(Default)]
pub struct FakeTxs(pub Mutex<HashMap<TxHash, Option<BlockHash>>>, pub Mutex<bool>);

impl TxSource for FakeTxs {
    /// Fails while the second field is set.
    fn get_transaction(&self, txid: &TxHash) -> dash_kv::Result<Option<Option<BlockHash>>> {
        if *self.1.lock() {
            return Err(dash_kv::StoreError::engine("test", dash_kv::ErrorKind::Io, "scripted"));
        }
        Ok(self.0.lock().get(txid).copied())
    }
}

/// A version-1 transaction spending output 0 of a txid made from `tag`.
pub fn spend(tag: u32) -> Tx {
    let mut prev = [0; 32];
    prev[..4].copy_from_slice(&tag.to_le_bytes());
    let input = TxIn::new(OutPoint::new(TxHash::from(prev), 0), Default::default(), u32::MAX);
    Tx::new(Transaction::new(1, TxType::new(0), vec![input], vec![], tag, vec![]))
}

pub fn coinbase(tag: u32) -> Tx {
    let input = TxIn::new(OutPoint::new(TxHash::from([0; 32]), u32::MAX), Default::default(), 0);
    let tx = Tx::new(Transaction::new(1, TxType::new(0), vec![input], vec![], tag, vec![]));
    assert!(tx.is_coinbase());
    tx
}

pub fn no_inputs(tag: u32) -> Tx {
    Tx::new(Transaction::new(1, TxType::new(0), vec![], vec![], tag, vec![]))
}
