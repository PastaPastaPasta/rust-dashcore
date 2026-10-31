//! `sim`'s chain: a header arena standing in for CH1-node behind [`ChainAccess`]. It starts at
//! the network's genesis (the M2 node before headers sync), accepts any header whose parent it
//! knows (no proof of work or contextual checks), and gives each header one unit of work. Tests
//! move the active tip, the IBD latch and the minimum chain work, and mark hashes invalid.
//!
//! [`SimChain::validating`] judges headers as Core does instead (`AcceptBlockHeader` with
//! dash-consensus's header checks, `GetBlockProof` work, the IBD rule), for comparisons with
//! Core's own node.
//!
//! Blocks (P3 S1): `ProcessBlock` stores a block's data, runs the merkle pass unless the
//! submitter already did (`merkle_checked`), and connects the most-work chain whose data is all
//! present, emitting `BlockChecked` to a [`SimListener`] as V does, before the publication that
//! ends the accept (C7 (a)). Each publication stores the tip cell ([`ChainAccess::snapshot`]),
//! then the `ChainView` ([`ChainAccess::drain_snapshot`]), with a test hook between the two
//! stores (P3 D47). By default V's work runs inline on the caller's thread; after
//! [`SimChain::spawn_v`] it runs on a thread standing in for V, which first waits as C5′ says
//! (DEC-240).

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Weak};
use std::task::{Context, Poll, Waker};

use dash_clock::{Clock, NodeSeconds};
use dash_consensus::block::{Mutation, block_mutation};
use dash_consensus::ctx::{
    ChainAncestry, CheckpointView, ConsensusCtx, Deployment, DeploymentView, HeaderCtx,
    bls_ctx_for_block,
};
use dash_consensus::header::{HeaderParams, check_block_header, contextual_check_block_header};
use dash_consensus::{
    Block, BlockId, BlockValidationError, BlockValidationResult, Header, TxRef, decode_tx,
};
use dash_num::CompactTarget;
use dash_p2p::PeerId;
use dash_params::ChainParams;
use dash_primitives::{BlockHash, BlockHeader, MerkleRoot};
use dash_types::Numeric as _;
use dash_types::codec::BaseCodec;
use parking_lot::{Condvar, Mutex, MutexGuard};

use crate::processing::peer::assert_no_header_cell;
use crate::processing::seams::{
    BlockOutcome, BlockPos, BlockRead, BlockSubmit, BlockWait, ChainAccess, ChainSnapshot,
    EntryFacts, EntryStatus, HeaderBatch, HeadersResult, Interrupted, TipFacts, TreeRead, Validity,
    ViewExpired, ViewToken, Work,
};

/// What a `sim` chain tells the processor, standing for CH1-node's `ValidationInterface`
/// listener (P3 §2.5). `sim` registers its [`Shared`](crate::processing::peerman::Shared).
pub trait SimListener: Send + Sync {
    /// `BlockChecked`, on V's thread, before the publication that ends the scope (C7 (a)).
    fn block_checked(&self, hash: BlockHash, invalid: Option<BlockValidationError>, ibd: bool);
    /// A publication has stored its `ChainView`: the queued `BlockConnected`'s ring.
    fn published(&self);
    /// C5′: wait until no `BlockChecked` is outstanding (DEC-240, D55).
    fn wait_quiescent(&self) -> Result<(), Interrupted>;
    /// C8's `NewPoWValidBlock`, inline in the accept before the block is written: the new
    /// entry, the block, V's in-scope tree and the stamp (P3 §2.5, D50, D101).
    fn new_pow_valid_block(
        &self,
        facts: &EntryFacts,
        block: &Arc<Block>,
        tree: &dyn TreeRead,
        stamp: u64,
    ) {
        let _ = (facts, block, tree, stamp);
    }
    /// The queued `BlockConnected` of one connected block, after the publication, at `now`
    /// (`GetTime()`, the chain's clock; P3 §6.2).
    fn block_connected(&self, now: i64) {
        let _ = now;
    }
}

/// A test hook V's thread runs: before an accept ([`SimChain::set_accept_hook`]), or at
/// `NewPoWValidBlock`'s point with the stamp it would store ([`SimChain::set_announce_hook`]).
pub type AcceptHook = Arc<dyn Fn(&BlockSubmit) + Send + Sync>;
/// See [`AcceptHook`]: the stamp (one more than the last `view_seq`) and the new entry.
pub type AnnounceHook = Arc<dyn Fn(u64, &EntryFacts) + Send + Sync>;

/// The arena and the knobs.
struct Inner {
    entries: Vec<EntryFacts>,
    by_hash: HashMap<BlockHash, BlockId>,
    tip: BlockId,
    best_header: BlockId,
    ibd: bool,
    min_chain_work: Work,
    loading_blocks: bool,
    shutting_down: bool,
    invalid: HashSet<BlockHash>,
    /// The entries' mutable status, shared copy-on-write with the published `ChainView`; write
    /// it through [`Inner::st_mut`].
    st: Arc<Statuses>,
    /// Blocks [`SimChain::mark_bad_block`] fails in `CheckBlock`.
    bad_blocks: HashSet<BlockHash>,
    /// Blocks `ConnectBlock` fails ([`SimChain::mark_bad_connect`]).
    bad_connects: HashSet<BlockHash>,
    epoch: u64,
    /// The tip cell ([`ChainAccess::snapshot`]).
    snap: Arc<ChainSnapshot>,
    batches: Vec<(PeerId, usize)>,
    /// The `ProcessBlock` submits so far.
    submits: Vec<BlockSubmit>,
    /// The stored blocks, which [`ChainAccess::read_block`] returns.
    blocks: HashMap<BlockId, Arc<Block>>,
    /// [`ChainAccess::prune_mode`].
    prune_mode: bool,
    /// Core's `m_have_pruned`: [`SimChain::prune`] has run.
    have_pruned: bool,
}

impl Inner {
    /// `params`' genesis alone, in IBD, with minimum chain work 1; not yet published.
    fn genesis(params: &ChainParams) -> Self {
        let genesis = Header::new(params.genesis.header);
        let entry = EntryFacts {
            id: BlockId(0),
            hash: genesis.hash(),
            prev: None,
            height: 0,
            time: genesis.time(),
            chain_work: Work::ONE,
            header: *genesis.raw(),
        };
        Self {
            entries: vec![entry],
            by_hash: HashMap::from([(entry.hash, entry.id)]),
            tip: entry.id,
            best_header: entry.id,
            ibd: true,
            min_chain_work: Work::ONE,
            loading_blocks: false,
            shutting_down: false,
            invalid: HashSet::new(),
            st: Arc::new(Statuses {
                connected: HashSet::from([entry.id]),
                data: HashMap::from([(entry.id, 1)]),
                txs_valid: HashSet::from([entry.id]),
                failed: HashSet::new(),
                chain_tx: HashSet::from([entry.id]),
                unlinked: HashMap::new(),
                pruned: HashMap::new(),
            }),
            bad_blocks: HashSet::new(),
            bad_connects: HashSet::new(),
            blocks: HashMap::new(),
            prune_mode: false,
            have_pruned: false,
            epoch: 0,
            snap: Arc::new(ChainSnapshot {
                tip: facts(&entry, &entry),
                bls: bls_ctx_for_block(params, 1),
                epoch: 0,
                view: ViewToken::default(),
            }),
            batches: Vec::new(),
            submits: Vec::new(),
        }
    }

    fn entry(&self, id: BlockId) -> &EntryFacts {
        &self.entries[id.0 as usize]
    }

    /// [`Inner::receive`] for `block`'s data, which [`ChainAccess::read_block`] then returns.
    fn store_block(&mut self, id: BlockId, block: &Arc<Block>) {
        self.receive(id, u32::try_from(block.txs().len()).unwrap_or(u32::MAX));
        self.blocks.insert(id, Arc::clone(block));
    }

    /// `ReadBlockFromDisk(block, pos)` and the caller's hash test: the block stored at `pos`,
    /// when it is `id`'s.
    fn block_at(&self, id: BlockId, pos: BlockPos) -> Option<Arc<Block>> {
        let at = BlockId(pos.pos);
        let block = self.blocks.get(&at).filter(|_| pos.file == 0)?;
        (block.hash() == self.entries.get(id.0 as usize)?.hash).then(|| Arc::clone(block))
    }

    /// A failed read: [`BlockRead::Pruned`] by Core's `IsBlockPruned`, else
    /// [`BlockRead::Failed`] (an entry marked as having data with no block stored).
    // Core: node/blockstorage.cpp:443-447 IsBlockPruned
    fn failed_read(&self, id: BlockId) -> BlockRead {
        let st = self.st.status(id);
        if self.have_pruned && !st.have_data && st.n_tx > 0 {
            BlockRead::Pruned
        } else {
            BlockRead::Failed
        }
    }

    /// `ReceivedBlockTransactions`: `BLOCK_HAVE_DATA`, `nTx` and `BLOCK_VALID_TRANSACTIONS` for
    /// `id`, then `nChainTx` for it and, through `m_blocks_unlinked`, every stored descendant
    /// once its parent's is set; a child of a failed parent (not `BLOCK_VALID_TREE`) is never
    /// queued. A no-op for an entry that already has data.
    // Core: validation.cpp:4266-4305 ReceivedBlockTransactions
    fn receive(&mut self, id: BlockId, n_tx: u32) {
        let prev = self.entries[id.0 as usize].prev;
        let st = Arc::make_mut(&mut self.st);
        if st.data.contains_key(&id) {
            return;
        }
        st.data.insert(id, n_tx);
        st.pruned.remove(&id);
        st.txs_valid.insert(id);
        if let Some(p) = prev
            && !st.chain_tx.contains(&p)
        {
            if !st.failed.contains(&p) {
                st.unlinked.entry(p).or_default().push(id);
            }
            return;
        }
        let mut linked = vec![id];
        while let Some(b) = linked.pop() {
            st.chain_tx.insert(b);
            linked.extend(st.unlinked.remove(&b).unwrap_or_default());
        }
    }

    /// The status sets for writing: copies them first if a published view still shares them.
    fn st_mut(&mut self) -> &mut Statuses {
        Arc::make_mut(&mut self.st)
    }

    /// `ActivateBestChain` over the stored data: the most-work entry (first indexed on a tie)
    /// whose chain is all downloaded and has no failed entry above the active chain
    /// (`BLOCK_FAILED_CHILD`), if it has more work than the tip; returns
    /// the entries `ConnectTip` ran on, oldest first, each with its `ConnectBlock` failure. A
    /// failure marks the entry failed and ends the activation at its parent (`InvalidBlockFound`;
    /// no other chain is tried).
    // Core: validation.cpp:3519-3581 FindMostWorkChain, :3724-3856 ActivateBestChain
    fn activate_best_chain(&mut self) -> Vec<(BlockId, Option<BlockValidationError>)> {
        let tip_work = self.entry(self.tip).chain_work;
        let best = self
            .entries
            .iter()
            .filter(|e| e.chain_work > tip_work && self.st.txs_downloaded(e.id))
            .filter(|e| !self.failed_branch(e.id))
            .fold(None::<EntryFacts>, |b, e| match b {
                Some(b) if b.chain_work >= e.chain_work => Some(b),
                _ => Some(*e),
            });
        let Some(best) = best else {
            return Vec::new();
        };
        let mut path = Vec::new();
        let mut at = Some(best.id);
        while let Some(e) = at {
            if self.on_active_chain(e) {
                break;
            }
            path.push(e);
            at = self.entry(e).prev;
        }
        path.reverse();
        let mut steps = Vec::new();
        for &e in &path {
            if self.bad_connects.contains(&self.entry(e).hash) {
                self.st_mut().failed.insert(e);
                let err = BlockValidationError::new(BlockValidationResult::Consensus, "bad-cb-sim");
                steps.push((e, Some(err)));
                return steps;
            }
            self.st_mut().connected.insert(e);
            self.tip = e;
            steps.push((e, None));
        }
        steps
    }

    /// Whether `id` or an ancestor of it off the active chain is failed: Core's
    /// `BLOCK_FAILED_CHILD` marks, which `FindMostWorkChain` drops a candidate for.
    // Core: validation.cpp:3535-3575 FindMostWorkChain
    fn failed_branch(&self, id: BlockId) -> bool {
        let mut at = Some(id);
        while let Some(e) = at
            && !self.on_active_chain(e)
        {
            if self.st.failed.contains(&e) {
                return true;
            }
            at = self.entry(e).prev;
        }
        false
    }

    fn on_active_chain(&self, id: BlockId) -> bool {
        let e = self.entry(id);
        let mut at = *self.entry(self.tip);
        if at.height < e.height {
            return false;
        }
        while at.height > e.height {
            at = *self.entry(at.prev.expect("height > 0"));
        }
        at.id == id
    }

    /// Adds `h` on `prev` with `work` more than it, the best header if it has the most work.
    // Core: node/blockstorage.cpp:85 BlockManager::AddToBlockIndex
    fn append(&mut self, h: &Header, hash: BlockHash, prev: BlockId, work: Work) -> BlockId {
        let p = *self.entry(prev);
        let e = EntryFacts {
            id: BlockId(self.entries.len() as u32),
            hash,
            prev: Some(prev),
            height: p.height + 1,
            time: h.time(),
            chain_work: p.chain_work + work,
            header: *h.raw(),
        };
        self.entries.push(e);
        self.by_hash.insert(hash, e.id);
        if e.chain_work > self.entry(self.best_header).chain_work {
            self.best_header = e.id;
        }
        e.id
    }

    /// The unvalidating chain's rule: the parent must be known; each header adds one unit of
    /// work; [`SimChain::mark_invalid`]'s hashes fail as `high-hash`.
    fn accept_unchecked(
        &mut self,
        h: &Header,
        hash: &BlockHash,
    ) -> Result<BlockId, BlockValidationError> {
        if self.invalid.contains(hash) {
            return Err(BlockValidationError::new(
                BlockValidationResult::InvalidHeader,
                "high-hash",
            ));
        }
        if let Some(&id) = self.by_hash.get(hash) {
            return self.known(id);
        }
        let Some(&prev) = self.by_hash.get(&h.prev_hash()) else {
            return Err(missing_prev());
        };
        Ok(self.append(h, *hash, prev, Work::ONE))
    }

    /// A header already in the index: `BLOCK_CACHED_INVALID` if its block failed.
    // Core: validation.cpp:4606-4615 AcceptBlockHeader
    fn known(&self, id: BlockId) -> Result<BlockId, BlockValidationError> {
        if self.st.failed.contains(&id) {
            return Err(BlockValidationError::new(
                BlockValidationResult::CachedInvalid,
                "duplicate",
            ));
        }
        Ok(id)
    }
}

fn missing_prev() -> BlockValidationError {
    BlockValidationError::new(BlockValidationResult::MissingPrev, "prev-blk-not-found")
}

/// What [`SimChain::validating`] judges headers with.
struct Validator {
    header_params: HeaderParams,
    /// The node clock, `time-too-new`'s and the IBD rule's.
    clock: Arc<dyn Clock>,
    /// `-maxtipage`, in seconds.
    max_tip_age: i64,
}

/// The `ChainView` cell: the snapshot, its `view_seq`, and the entries' status as published
/// with it, which `status` and `valid_scripts` read (P3 §2.4: the last published value).
struct View {
    snap: Arc<ChainSnapshot>,
    seq: u64,
    status: Arc<Statuses>,
}

/// The entries' mutable status (`nStatus`, `nTx`); `EntryFacts` holds the immutable fields.
#[derive(Clone, Debug, Default)]
struct Statuses {
    /// Entries that were ever on the active chain (Core's `BLOCK_VALID_SCRIPTS`, set by
    /// `ConnectBlock`); genesis from the start.
    connected: HashSet<BlockId>,
    /// `BLOCK_HAVE_DATA` entries and their `nTx`.
    data: HashMap<BlockId, u32>,
    /// `BLOCK_VALID_TRANSACTIONS` entries (`ReceivedBlockTransactions`).
    txs_valid: HashSet<BlockId>,
    /// `BLOCK_FAILED_VALID` entries.
    failed: HashSet<BlockId>,
    /// `nChainTx != 0` entries: they and every ancestor have data.
    chain_tx: HashSet<BlockId>,
    /// `m_blocks_unlinked`: entries with data whose parent's `nChainTx` is still 0, by parent.
    unlinked: HashMap<BlockId, Vec<BlockId>>,
    /// Entries [`SimChain::prune`] took the data of, with the `nTx` Core keeps.
    pruned: HashMap<BlockId, u32>,
}

impl Statuses {
    /// `nChainTx != 0`.
    fn txs_downloaded(&self, id: BlockId) -> bool {
        self.chain_tx.contains(&id)
    }

    fn status(&self, id: BlockId) -> EntryStatus {
        let validity = if self.connected.contains(&id) {
            Validity::Scripts
        } else if self.txs_valid.contains(&id) {
            Validity::Transactions
        } else {
            Validity::Tree
        };
        let have_data = self.data.contains_key(&id);
        EntryStatus {
            have_data,
            data_pos: have_data.then_some(SimChain::pos(id)),
            validity,
            failed: self.failed.contains(&id),
            n_tx: self.data.get(&id).or_else(|| self.pruned.get(&id)).copied().unwrap_or(0),
            have_txs_downloaded: self.txs_downloaded(id),
        }
    }
}

/// V's reply to one `ProcessNewBlock`.
type Reply = Result<BlockOutcome, Interrupted>;

/// A reply slot standing for CH1-node's `ActorReply`: `set` stores the value, then wakes.
#[derive(Default)]
struct Slot {
    state: Mutex<(Option<Reply>, Option<Waker>)>,
}

impl Slot {
    fn set(&self, r: Reply) {
        let waker = {
            let mut s = self.state.lock();
            s.0 = Some(r);
            s.1.take()
        };
        if let Some(w) = waker {
            w.wake();
        }
    }
}

struct SlotFuture(Arc<Slot>);

impl Future for SlotFuture {
    type Output = Reply;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut s = self.0.state.lock();
        match s.0.take() {
            Some(r) => Poll::Ready(r),
            None => {
                s.1 = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

/// The test hooks.
#[derive(Default)]
struct Hooks {
    listener: Option<Weak<dyn SimListener>>,
    /// Runs between the tip cell's store and the `ChainView`'s.
    between_stores: Option<Arc<dyn Fn() + Send + Sync>>,
    accept: Option<AcceptHook>,
    announce: Option<AnnounceHook>,
    /// Runs after a new block's write and before its accept's `ActivateBestChain`.
    connect: Option<AnnounceHook>,
    /// Runs on V's thread after each reply is stored.
    reply: Option<Arc<dyn Fn() + Send + Sync>>,
    /// The clock `BlockConnected` reads; none, no `BlockConnected`.
    clock: Option<Arc<dyn Clock>>,
    /// Runs as V starts serving a peer's `ProcessHeaders`, with the peer.
    headers: Option<Arc<dyn Fn(PeerId) + Send + Sync>>,
    /// Runs as the processor makes one of the [`SimRead`]s, before the read.
    read: Option<Arc<dyn Fn(SimRead) + Send + Sync>>,
    /// V's thread, once [`SimChain::spawn_v`] has started it.
    v: Option<mpsc::Sender<(BlockSubmit, Arc<Slot>)>>,
}

/// A [`ChainAccess`] read that [`SimChain::set_read_hook`] observes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimRead {
    /// `LoadingBlocks()`, which the `block` handler reads after its time and before the decode.
    LoadingBlocks,
    /// `nMinimumChainWork`, which `FindNextBlocksToDownload` reads before its requests.
    MinChainWork,
    /// [`ChainAccess::drain_snapshot`], at every drain point; the head of `send_messages` makes
    /// it under the `block_inv` cell.
    DrainSnapshot,
    /// [`ChainAccess::read_block`] or [`ChainAccess::read_block_at`] of the entry: a hook that
    /// prunes it races Core's `ReadBlockFromDisk` (O17 P2P-17).
    ReadBlock(BlockId),
}

/// The `sim` chain.
pub struct SimChain {
    params: Arc<ChainParams>,
    /// Serializes writers, so publications store in the order they were made.
    writer: Mutex<()>,
    inner: Mutex<Inner>,
    view: Mutex<View>,
    published: Condvar,
    hooks: Mutex<Hooks>,
    validator: Option<Validator>,
    /// [`ChainAccess::snapshot`] calls (the two-cell test asserts no drain point makes one).
    snapshot_calls: AtomicU64,
    /// Merkle passes `CheckBlock` ran.
    merkle_passes: AtomicU64,
    /// [`ChainAccess::wait_published`] calls (the `Ahead` tests publish only after one).
    published_waits: AtomicU64,
    /// The status views [`SimChain::expire`] made unreadable, as CH1-node's journal drops a view
    /// after 2^16 status writes (BL-224).
    expired: Mutex<Vec<Arc<Statuses>>>,
}

impl SimChain {
    /// A chain holding only `params`' genesis block, in IBD, with minimum chain work 1.
    pub fn new(params: Arc<ChainParams>) -> Arc<Self> {
        let mut inner = Inner::genesis(&params);
        publish(&params, &mut inner);
        Self::around(params, inner, None)
    }

    fn around(params: Arc<ChainParams>, inner: Inner, validator: Option<Validator>) -> Arc<Self> {
        let view = View {
            snap: Arc::clone(&inner.snap),
            seq: 1,
            status: Arc::clone(&inner.st),
        };
        Arc::new(Self {
            params,
            writer: Mutex::new(()),
            inner: Mutex::new(inner),
            view: Mutex::new(view),
            published: Condvar::new(),
            hooks: Mutex::default(),
            validator,
            snapshot_calls: AtomicU64::new(0),
            merkle_passes: AtomicU64::new(0),
            published_waits: AtomicU64::new(0),
            expired: Mutex::default(),
        })
    }

    /// A chain holding only `params`' genesis block that judges headers as Core does: its
    /// `AcceptBlockHeader` on `clock`, each header's `GetBlockProof` as work, `params`' minimum
    /// chain work, and Core's latched IBD rule with `max_tip_age`. Block validation is not
    /// modelled, so no entry is ever marked failed.
    ///
    /// # Panics
    ///
    /// When `params`' proof-of-work parameters are malformed.
    pub fn validating(
        params: Arc<ChainParams>,
        clock: Arc<dyn Clock>,
        max_tip_age: i64,
    ) -> Arc<Self> {
        let header_params = HeaderParams::from_chain_params(&params).expect("valid pow params");
        let mut inner = Inner::genesis(&params);
        let genesis = &mut inner.entries[0];
        genesis.chain_work = CompactTarget::new(genesis.header.bits).block_proof();
        inner.min_chain_work = params.consensus.minimum_chain_work;
        publish(&params, &mut inner);
        let validator = Validator {
            header_params,
            clock,
            max_tip_age,
        };
        Self::around(params, inner, Some(validator))
    }

    /// Registers the processor's listener (P3 §2.5; Core's `RegisterValidationInterface`).
    pub fn set_listener(&self, listener: Weak<dyn SimListener>) {
        self.hooks.lock().listener = Some(listener);
    }

    /// Runs `hook` with the peer as V starts serving each `ProcessHeaders`, before the accept:
    /// another origin's work lands between the caller's message-start drain and the reply.
    pub fn set_headers_hook(&self, hook: Option<Arc<dyn Fn(PeerId) + Send + Sync>>) {
        self.hooks.lock().headers = hook;
    }

    /// Runs `hook` as the processor reads `loading_blocks` or `min_chain_work`, before the read:
    /// a test advances the clock there to model work between a sampled time and a later
    /// operation (D86).
    pub fn set_read_hook(&self, hook: Option<Arc<dyn Fn(SimRead) + Send + Sync>>) {
        self.hooks.lock().read = hook;
    }

    fn on_read(&self, read: SimRead) {
        let hook = self.hooks.lock().read.clone();
        if let Some(hook) = hook {
            hook(read);
        }
    }

    /// The clock [`SimListener::block_connected`] is called with.
    pub fn set_clock(&self, clock: Arc<dyn Clock>) {
        self.hooks.lock().clock = Some(clock);
    }

    fn listener(&self) -> Option<Arc<dyn SimListener>> {
        self.hooks.lock().listener.as_ref().and_then(Weak::upgrade)
    }

    /// Runs `hook` in every publication between the tip cell's store and the `ChainView`'s
    /// (P3 D47's two cells).
    pub fn set_between_stores(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        self.hooks.lock().between_stores = hook;
    }

    /// Runs `hook` on V's thread before each accept, after C5′'s wait.
    pub fn set_accept_hook(&self, hook: Option<AcceptHook>) {
        self.hooks.lock().accept = hook;
    }

    /// Runs `hook` right after each `NewPoWValidBlock` call (`!IsInitialBlockDownload()` and
    /// the block extends the tip, v:4831) and before the block is written, with neither the
    /// index nor the view lock held (C8), only V's writer: S4-x (6)'s first pause (P3 §6.2).
    /// The hook may read the chain and drain, but must not publish.
    pub fn set_announce_hook(&self, hook: Option<AnnounceHook>) {
        self.hooks.lock().announce = hook;
    }

    /// Runs `hook` after an accept writes a new block and before the accept's
    /// `ActivateBestChain` (Core's first `ConnectTip`, v:3432), with neither the index nor the
    /// view lock held: S4-x (6)'s second pause. The hook may read the chain, but must not
    /// publish.
    pub fn set_connect_hook(&self, hook: Option<AnnounceHook>) {
        self.hooks.lock().connect = hook;
    }

    /// Runs `hook` on V's thread after [`SimChain::spawn_v`]'s thread stores each reply.
    pub fn set_reply_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        self.hooks.lock().reply = hook;
    }

    /// Starts a thread standing in for V: `ProcessBlock` then queues each submit to it, and it
    /// waits as C5′ says before each accept outside IBD (DEC-240, D55). The thread ends when the
    /// chain is dropped.
    pub fn spawn_v(self: &Arc<Self>) {
        let (tx, rx) = mpsc::channel::<(BlockSubmit, Arc<Slot>)>();
        let chain = Arc::downgrade(self);
        std::thread::Builder::new()
            .name("sim-v".into())
            .spawn(move || {
                while let Ok((submit, slot)) = rx.recv() {
                    let Some(chain) = chain.upgrade() else {
                        slot.set(Err(Interrupted));
                        return;
                    };
                    slot.set(chain.serve_block(submit, true));
                    let hook = chain.hooks.lock().reply.clone();
                    if let Some(h) = hook {
                        h();
                    }
                }
            })
            .expect("spawning sim-v");
        self.hooks.lock().v = Some(tx);
    }

    /// `ProcessNewBlock` as V serves it: C5′'s wait when `pre_accept_wait` and outside IBD,
    /// then the accept hook, then [`SimChain::accept_block`].
    fn serve_block(
        &self,
        submit: BlockSubmit,
        pre_accept_wait: bool,
    ) -> Result<BlockOutcome, Interrupted> {
        if self.inner.lock().shutting_down {
            return Err(Interrupted);
        }
        if pre_accept_wait
            && !self.inner.lock().ibd
            && let Some(l) = self.listener()
        {
            l.wait_quiescent()?;
        }
        let hook = self.hooks.lock().accept.clone();
        if let Some(h) = hook {
            h(&submit);
        }
        Ok(self.accept_block(&submit))
    }

    /// V's accept and `ActivateBestChain` for `submit`, then the publication: `CheckBlock`'s
    /// merkle pass unless `merkle_checked`, then [`SimChain::mark_bad_block`]'s failure, then
    /// `AcceptBlock` (the header, then the data unless already held), the announce hook, and the
    /// connects. Each failure and each connect emits `BlockChecked` before the publication
    /// (C1, C7 (a)).
    // Core: validation.cpp:4867-4907 ProcessNewBlock, :4759-4865 AcceptBlock
    pub fn accept_block(&self, submit: &BlockSubmit) -> BlockOutcome {
        let writer = self.writer.lock();
        let mut inner = self.inner.lock();
        inner.submits.push(submit.clone());
        let ibd = inner.ibd;
        let mut signals = Vec::new();
        let mut new_block = false;
        let checked = if submit.merkle_checked {
            Ok(())
        } else {
            self.merkle_passes.fetch_add(1, Ordering::SeqCst);
            match block_mutation(&submit.block) {
                Some(Mutation::MerkleRoot(e)) => Err(e),
                Some(Mutation::Tx64) => {
                    Err(BlockValidationError::new(BlockValidationResult::Mutated, "bad-txns-size"))
                }
                None => Ok(()),
            }
        };
        let checked = checked.and_then(|()| {
            if inner.bad_blocks.contains(&submit.hash) {
                Err(BlockValidationError::new(BlockValidationResult::Consensus, "bad-blk-sim"))
            } else {
                Ok(())
            }
        });
        let accepted = checked.and_then(|()| {
            let header = *submit.block.header();
            match &self.validator {
                Some(v) => self.accept_header(&mut inner, v, &header, &submit.hash),
                None => inner.accept_unchecked(&header, &submit.hash),
            }
        });
        match accepted {
            Err(e) => {
                if let Some(&id) = inner.by_hash.get(&submit.hash)
                    && e.result() != BlockValidationResult::Mutated
                {
                    inner.st_mut().failed.insert(id);
                }
                signals.push((submit.hash, Some(e)));
            }
            Ok(id) if inner.st.data.contains_key(&id) => {}
            Ok(id) => {
                new_block = true;
                let e = *inner.entry(id);
                if !ibd && e.prev == Some(inner.tip) {
                    drop(inner);
                    self.new_pow_valid_block(&e, &submit.block);
                    inner = self.inner.lock();
                }
                inner.store_block(id, &submit.block);
                let hook = self.hooks.lock().connect.clone();
                if let Some(h) = hook {
                    drop(inner);
                    h(self.view.lock().seq + 1, &e);
                    inner = self.inner.lock();
                }
            }
        }
        let steps = inner.activate_best_chain();
        let connected = steps.iter().filter(|(_, e)| e.is_none()).count();
        for (id, invalid) in steps {
            signals.push((inner.entry(id).hash, invalid));
        }
        self.publish_with(writer, inner, signals, ibd);
        // Core queues `BlockConnected` per connected block (v:3432); the listener runs them
        // after the publication, as the scheduler thread would.
        let (listener, clock) = {
            let h = self.hooks.lock();
            (h.listener.as_ref().and_then(Weak::upgrade), h.clock.clone())
        };
        if let (Some(l), Some(c)) = (listener, clock) {
            for _ in 0..connected {
                l.block_connected(c.node_now().0);
            }
        }
        BlockOutcome {
            new_block,
        }
    }

    /// v:4832's `NewPoWValidBlock(e, block)`, with only V's writer held (C8): the listener's
    /// visit against this chain's tree and the stamp, one more than the last `view_seq`; then
    /// the announce hook, which pauses V right after the call and before the write.
    fn new_pow_valid_block(&self, e: &EntryFacts, block: &Arc<Block>) {
        let stamp = self.view.lock().seq + 1;
        if let Some(l) = self.listener() {
            l.new_pow_valid_block(e, block, self, stamp);
        }
        let hook = self.hooks.lock().announce.clone();
        if let Some(h) = hook {
            h(stamp, e);
        }
    }

    /// Fails `hash` in `CheckBlock` (`BLOCK_CONSENSUS`, `bad-blk-sim`), as an invalid block.
    pub fn mark_bad_block(&self, hash: BlockHash) {
        self.inner.lock().bad_blocks.insert(hash);
    }

    /// Fails `hash` in `ConnectBlock` (`BLOCK_CONSENSUS`, `bad-cb-sim`): its `ConnectTip`
    /// emits an invalid `BlockChecked` and the activation stops at its parent.
    pub fn mark_bad_connect(&self, hash: BlockHash) {
        self.inner.lock().bad_connects.insert(hash);
    }

    /// The `ProcessBlock` submits so far.
    pub fn submits(&self) -> Vec<BlockSubmit> {
        self.inner.lock().submits.clone()
    }

    /// How many merkle passes `CheckBlock` ran.
    pub fn merkle_passes(&self) -> u64 {
        self.merkle_passes.load(Ordering::SeqCst)
    }

    /// How many [`ChainAccess::snapshot`] calls were made.
    pub fn snapshot_calls(&self) -> u64 {
        self.snapshot_calls.load(Ordering::SeqCst)
    }

    /// How many [`ChainAccess::wait_published`] calls were made.
    pub fn published_waits(&self) -> u64 {
        self.published_waits.load(Ordering::SeqCst)
    }

    /// The statuses published with `snap` (BL-236), never `inner`'s: between the two stores
    /// `inner` may already show B connected while the view still shows tip A, a state no Core
    /// `cs_main` section can see (P3 §2.4). The genesis snapshot, built before any status
    /// store, reads the current view's.
    fn published_status(&self, snap: &ChainSnapshot) -> Arc<Statuses> {
        snap.view.get::<Statuses>().unwrap_or_else(|| Arc::clone(&self.view.lock().status))
    }

    /// Makes `snap`'s status view unreadable: [`ChainAccess::valid_scripts`] answers
    /// [`ViewExpired`] for it, and for every snapshot published with the same statuses.
    pub fn expire(&self, snap: &ChainSnapshot) {
        let status = self.published_status(snap);
        self.expired.lock().push(status);
    }

    /// Sets or clears `BLOCK_FAILED_VALID` on `id` and publishes, as `invalidateblock` and
    /// `reconsiderblock` do for an entry off the active chain.
    pub fn set_failed(&self, id: BlockId, failed: bool) {
        let writer = self.writer.lock();
        let mut inner = self.inner.lock();
        let st = inner.st_mut();
        if failed {
            st.failed.insert(id);
        } else {
            st.failed.remove(&id);
        }
        self.publish_with(writer, inner, Vec::new(), false);
    }

    /// The current `view_seq`.
    pub fn view_seq(&self) -> u64 {
        self.view.lock().seq
    }

    /// A publication with no change, as V's next scope end publishes after a test stood in for
    /// V's push (C7 (a)).
    pub fn republish(&self) {
        let writer = self.writer.lock();
        let inner = self.inner.lock();
        self.publish_with(writer, inner, Vec::new(), false);
    }

    /// Publishes: stores the tip cell, emits `signals` (stamped by the listener with one more
    /// than the current `view_seq`), runs the between-stores hook, stores the `ChainView`, then
    /// rings. Writers are serialized by `writer`, so stores land in publication order.
    fn publish_with(
        &self,
        writer: MutexGuard<'_, ()>,
        mut inner: MutexGuard<'_, Inner>,
        signals: Vec<(BlockHash, Option<BlockValidationError>)>,
        ibd: bool,
    ) -> Arc<ChainSnapshot> {
        publish(&self.params, &mut inner);
        let snap = Arc::clone(&inner.snap);
        let status = Arc::clone(&inner.st);
        drop(inner);
        let (listener, between) = {
            let h = self.hooks.lock();
            (h.listener.as_ref().and_then(Weak::upgrade), h.between_stores.clone())
        };
        if let Some(l) = &listener {
            for (hash, invalid) in signals {
                l.block_checked(hash, invalid, ibd);
            }
        }
        if let Some(h) = between {
            h();
        }
        {
            let mut view = self.view.lock();
            view.seq += 1;
            view.snap = Arc::clone(&snap);
            view.status = status;
        }
        self.published.notify_all();
        drop(writer);
        if let Some(l) = &listener {
            l.published();
        }
        snap
    }

    /// `n` blocks building on `prev`, `spacing` seconds apart and distinguished by `salt`, each
    /// holding one coinbase whose txid is the header's merkle root. Synthetic `sim` data: the
    /// coinbase is a minimal Core-serialized transaction (one null-prevout input whose script
    /// carries `salt` and the index, one `OP_TRUE` output), not taken from a chain.
    pub fn build_blocks(prev: &EntryFacts, n: usize, spacing: u32, salt: u8) -> Vec<Arc<Block>> {
        let (mut prev_hash, mut time) = (prev.hash, prev.time);
        (0..n)
            .map(|i| {
                time += spacing;
                let coinbase = sim_coinbase(salt, i as u32);
                let h = BlockHeader {
                    version: 0x2000_0000,
                    prev_hash,
                    merkle_root: MerkleRoot::from_lendian(*coinbase.txid().as_bytes()),
                    time,
                    bits: prev.header.bits,
                    nonce: i as u32,
                };
                let block = Block::new(Header::new(h), vec![coinbase]);
                prev_hash = block.hash();
                Arc::new(block)
            })
            .collect()
    }

    /// The chain parameters.
    pub fn params(&self) -> &Arc<ChainParams> {
        &self.params
    }

    /// The genesis entry.
    pub fn genesis(&self) -> EntryFacts {
        self.inner.lock().entries[0]
    }

    /// The entry for `hash`.
    pub fn by_hash(&self, hash: &BlockHash) -> Option<EntryFacts> {
        let inner = self.inner.lock();
        inner.by_hash.get(hash).map(|id| inner.entries[id.0 as usize])
    }

    /// `n` headers building on `prev`, `spacing` seconds apart, distinguished by `salt`.
    pub fn build(prev: &EntryFacts, n: usize, spacing: u32, salt: u8) -> Vec<BlockHeader> {
        let mut out: Vec<BlockHeader> = Vec::with_capacity(n);
        let (mut prev_hash, mut time) = (prev.hash, prev.time);
        for i in 0..n {
            time += spacing;
            let mut root = [salt; 32];
            root[..4].copy_from_slice(&(i as u32).to_le_bytes());
            let h = BlockHeader {
                version: 0x2000_0000,
                prev_hash,
                merkle_root: MerkleRoot::decode(&mut &root[..]).expect("32 bytes"),
                time,
                bits: prev.header.bits,
                nonce: i as u32,
            };
            prev_hash = Header::new(h).hash();
            out.push(h);
        }
        out
    }

    /// `n` headers on `params`' genesis block, `spacing` seconds apart, each with a nonce that
    /// passes Core's header checks (`CheckProofOfWork`), so a [`SimChain::validating`] chain and
    /// Core both accept them.
    ///
    /// # Panics
    ///
    /// When `params`' proof-of-work parameters are malformed.
    pub fn mine(params: &ChainParams, n: usize, spacing: u32) -> Vec<BlockHeader> {
        let hp = HeaderParams::from_chain_params(params).expect("valid pow params");
        let genesis = Header::new(params.genesis.header);
        let (mut prev_hash, mut time) = (genesis.hash(), genesis.time());
        (0..n)
            .map(|i| {
                time += spacing;
                let mut root = [0x5a; 32];
                root[..4].copy_from_slice(&(i as u32).to_le_bytes());
                let mut h = BlockHeader {
                    version: 0x2000_0000,
                    prev_hash,
                    merkle_root: MerkleRoot::decode(&mut &root[..]).expect("32 bytes"),
                    time,
                    bits: params.genesis.header.bits,
                    nonce: 0,
                };
                while check_block_header(&Header::new(h), &hp, true).is_err() {
                    h.nonce += 1;
                }
                prev_hash = Header::new(h).hash();
                h
            })
            .collect()
    }

    /// Accepts `headers` as `ProcessHeaders` would, without a peer; returns the last entry.
    pub fn add(&self, headers: &[BlockHeader]) -> EntryFacts {
        let batch = HeaderBatch::new(headers.iter().map(|h| Header::new(*h)).collect());
        let r = self.accept(batch, None);
        let last = r.last_accepted.expect("the headers connect");
        self.inner.lock().entries[last.0 as usize]
    }

    /// Stores `blocks`' data as V would on a non-P2P path (an import), without connecting, and
    /// publishes it: their headers must connect. Returns the last entry.
    pub fn store_blocks(&self, blocks: &[Arc<Block>]) -> EntryFacts {
        let headers: Vec<BlockHeader> = blocks.iter().map(|b| *b.header().raw()).collect();
        let last = self.add(&headers);
        let writer = self.writer.lock();
        let mut inner = self.inner.lock();
        for b in blocks {
            let id = inner.by_hash[&b.hash()];
            inner.store_block(id, b);
        }
        self.publish_with(writer, inner, Vec::new(), false);
        last
    }

    /// Core's `vHashes` for `UpdatedBlockTip`: from `new` back to (not including) `fork`, newest
    /// first.
    pub fn new_blocks(&self, new: BlockId, fork: Option<BlockId>) -> Vec<BlockHash> {
        let inner = self.inner.lock();
        let mut out = Vec::new();
        let mut at = Some(new);
        while at != fork {
            let Some(e) = at.map(|id| inner.entries[id.0 as usize]) else {
                break;
            };
            out.push(e.hash);
            at = e.prev;
        }
        out
    }

    /// Makes `id` the active tip (Core's `ConnectTip` to it), publishing a new snapshot. Every
    /// entry on the new active chain is then `BLOCK_VALID_SCRIPTS`, and stays so after a later
    /// tip leaves it. It marks data received without storing a block: store a header-only
    /// entry's block (`store_blocks`) first if section 7 may announce it to an HB peer, whose
    /// `read_block` would otherwise find nothing and panic, as Core's assert does (np:6492).
    pub fn set_tip(&self, id: BlockId) {
        let writer = self.writer.lock();
        let mut inner = self.inner.lock();
        inner.tip = id;
        // Up to the first entry an earlier tip already connected.
        let mut at = Some(id);
        while let Some(e) = at {
            if !inner.st_mut().connected.insert(e) {
                break;
            }
            inner.receive(e, 1);
            at = inner.entries[e.0 as usize].prev;
        }
        self.publish_with(writer, inner, Vec::new(), false);
    }

    /// Core's `fPruneMode`, which [`ChainAccess::prune_mode`] reads.
    pub fn set_prune_mode(&self, prune: bool) {
        self.inner.lock().prune_mode = prune;
    }

    /// `PruneOneBlockFile` for `id` alone, then `UnlinkPrunedFiles`: its data and its stored
    /// block go, its `nTx` stays, and `m_have_pruned` is set; published.
    pub fn prune(&self, id: BlockId) {
        self.prune_entry(id, true);
    }

    /// `PruneOneBlockFile` for `id` alone, before `UnlinkPrunedFiles`: the entry loses its data
    /// and keeps its `nTx`, `m_have_pruned` is set, and the stored block stays readable at its
    /// old position (the files go later, in `FlushStateToDisk`); published.
    pub fn prune_index_only(&self, id: BlockId) {
        self.prune_entry(id, false);
    }

    // Core: node/blockstorage.cpp:131-144 PruneOneBlockFile, :582-590 UnlinkPrunedFiles
    fn prune_entry(&self, id: BlockId, unlink: bool) {
        let writer = self.writer.lock();
        let mut inner = self.inner.lock();
        let st = inner.st_mut();
        if let Some(n_tx) = st.data.remove(&id) {
            st.pruned.insert(id, n_tx);
        }
        if unlink {
            inner.blocks.remove(&id);
        }
        inner.have_pruned = true;
        self.publish_with(writer, inner, Vec::new(), false);
    }

    /// The position the sim stores `id`'s block at: file 0, the entry's index.
    fn pos(id: BlockId) -> BlockPos {
        BlockPos {
            file: 0,
            pos: id.0,
        }
    }

    /// Sets the IBD latch.
    pub fn set_ibd(&self, ibd: bool) {
        self.inner.lock().ibd = ibd;
    }

    /// Sets `nMinimumChainWork`.
    pub fn set_min_chain_work(&self, w: Work) {
        self.inner.lock().min_chain_work = w;
    }

    /// Sets `LoadingBlocks()`.
    pub fn set_loading_blocks(&self, loading: bool) {
        self.inner.lock().loading_blocks = loading;
    }

    /// Makes `ProcessHeaders` answer [`Interrupted`], as a chain side that is shutting down.
    pub fn set_shutting_down(&self, down: bool) {
        self.inner.lock().shutting_down = down;
        let _view = self.view.lock();
        self.published.notify_all();
    }

    /// Makes `ProcessHeaders` reject `hash` as `BLOCK_INVALID_HEADER` (`high-hash`).
    pub fn mark_invalid(&self, hash: BlockHash) {
        self.inner.lock().invalid.insert(hash);
    }

    /// The `ProcessHeaders` calls so far: the peer and the batch size.
    pub fn batches(&self) -> Vec<(PeerId, usize)> {
        self.inner.lock().batches.clone()
    }

    /// `ProcessHeaders`: each header in turn until one fails, by Core's `AcceptBlockHeader` on a
    /// validating chain.
    // Core: validation.cpp:4729-4746 ChainstateManager::ProcessNewBlockHeaders
    fn accept(&self, batch: HeaderBatch, from: Option<PeerId>) -> HeadersResult {
        let writer = self.writer.lock();
        let mut inner = self.inner.lock();
        if let Some(peer) = from {
            inner.batches.push((peer, batch.headers.len()));
        }
        let mut last = None;
        let mut invalid = None;
        for (h, hash) in batch.headers.iter().zip(&batch.hashes) {
            let accepted = match &self.validator {
                Some(v) => self.accept_header(&mut inner, v, h, hash),
                None => inner.accept_unchecked(h, hash),
            };
            match accepted {
                Ok(id) => last = Some(id),
                Err(e) => {
                    invalid = Some(e);
                    break;
                }
            }
        }
        let snap = self.publish_with(writer, inner, Vec::new(), false);
        HeadersResult {
            last_accepted: last,
            invalid,
            snap,
        }
    }

    /// Core's `AcceptBlockHeader` without the chainlock state, which this chain never sets.
    // Core: validation.cpp:4598-4726 ChainstateManager::AcceptBlockHeader
    fn accept_header(
        &self,
        inner: &mut Inner,
        v: &Validator,
        h: &Header,
        hash: &BlockHash,
    ) -> Result<BlockId, BlockValidationError> {
        if let Some(&id) = inner.by_hash.get(hash) {
            return inner.known(id);
        }
        check_block_header(h, &v.header_params, true)?;
        let Some(&prev) = inner.by_hash.get(&h.prev_hash()) else {
            return Err(missing_prev());
        };
        let p = *inner.entry(prev);
        let arena = Arena {
            inner,
            params: &self.params,
        };
        let ctx = ConsensusCtx {
            params: &self.params,
            height: p.height + 1,
            prev: Some(prev),
            chain: &arena,
            deployments: &arena,
            checkpoints: &arena,
            bls: bls_ctx_for_block(&self.params, p.height + 1),
            median_time_past: arena.median_time_past(prev),
            header_check_time: v.clock.node_now().0,
        };
        contextual_check_block_header(h, &ctx, &v.header_params)?;
        Ok(inner.append(h, *hash, prev, CompactTarget::new(h.bits()).block_proof()))
    }
}

/// The arena as the header checks read the block index.
struct Arena<'a> {
    inner: &'a Inner,
    params: &'a ChainParams,
}

impl Arena<'_> {
    fn entry(&self, id: BlockId) -> &EntryFacts {
        self.inner.entry(id)
    }

    /// `GetMedianTimePast()`: the median of the times of `id` and its ten predecessors.
    // Core: chain.h:283 CBlockIndex::GetMedianTimePast
    fn median_time_past(&self, id: BlockId) -> i64 {
        let mut times: Vec<i64> = std::iter::successors(Some(id), |&at| self.entry(at).prev)
            .take(11)
            .map(|at| i64::from(self.entry(at).time))
            .collect();
        times.sort_unstable();
        times[times.len() / 2]
    }

    fn active_at_height(&self, height: u32, deployment: Deployment) -> bool {
        match deployment {
            Deployment::Buried(d) => height >= self.params.consensus.deployment_height(d).to_u32(),
            // Core's header checks read buried deployments only.
            Deployment::Versionbits(_) => false,
        }
    }
}

impl ChainAncestry for Arena<'_> {
    fn header(&self, id: BlockId) -> HeaderCtx {
        let e = self.entry(id);
        HeaderCtx {
            height: e.height,
            hash: e.hash,
            time: e.time,
            bits: CompactTarget::new(e.header.bits),
            version: e.header.version,
            mtp: self.median_time_past(id),
            chain_work: e.chain_work,
        }
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        let mut e = self.entry(id);
        if height > e.height {
            return None;
        }
        while e.height > height {
            e = self.entry(e.prev?);
        }
        Some(e.id)
    }

    fn prev(&self, id: BlockId) -> Option<BlockId> {
        self.entry(id).prev
    }
}

impl DeploymentView for Arena<'_> {
    fn active_after(&self, prev: Option<BlockId>, deployment: Deployment) -> bool {
        let height = prev.map_or(0, |p| self.entry(p).height + 1);
        self.active_at_height(height, deployment)
    }

    fn active_at(&self, block: BlockId, deployment: Deployment) -> bool {
        self.active_at_height(self.entry(block).height, deployment)
    }
}

impl CheckpointView for Arena<'_> {
    /// Core's `GetLastCheckpoint`: the highest checkpoint whose block the index holds.
    fn highest_checkpoint_in_index(&self) -> Option<u32> {
        self.params
            .checkpoints
            .iter()
            .filter(|(_, hash)| self.inner.by_hash.contains_key(&BlockHash::from(*hash.as_bytes())))
            .map(|(h, _)| h.to_u32())
            .max()
    }
}

fn facts(tip: &EntryFacts, best: &EntryFacts) -> TipFacts {
    TipFacts {
        tip: tip.id,
        tip_work: tip.chain_work,
        tip_time: tip.time,
        tip_height: tip.height,
        tip_hash: tip.hash,
        best_header: best.id,
        best_header_hash: best.hash,
        best_header_time: best.time,
        best_header_work: best.chain_work,
        best_header_height: best.height,
    }
}

/// A minimal coinbase for [`SimChain::build_blocks`], Core-serialized: version 1, one input
/// spending the null outpoint with script `[1, salt, 4, i]`, one 50-DASH `OP_TRUE` output,
/// lock time 0.
fn sim_coinbase(salt: u8, i: u32) -> TxRef {
    let mut raw = vec![1, 0, 0, 0, 1];
    raw.extend_from_slice(&[0; 32]);
    raw.extend_from_slice(&[0xff; 4]);
    raw.extend_from_slice(&[7, 1, salt, 4]);
    raw.extend_from_slice(&i.to_le_bytes());
    raw.extend_from_slice(&[0xff; 4]);
    raw.push(1);
    raw.extend_from_slice(&5_000_000_000u64.to_le_bytes());
    raw.extend_from_slice(&[1, 0x51]);
    raw.extend_from_slice(&[0; 4]);
    Arc::new(decode_tx(&mut &raw[..]).expect("a valid coinbase").into_value())
}

/// Stores the tip cell.
fn publish(params: &ChainParams, inner: &mut Inner) {
    let tip = inner.entries[inner.tip.0 as usize];
    let best = inner.entries[inner.best_header.0 as usize];
    inner.epoch += 1;
    inner.snap = Arc::new(ChainSnapshot {
        tip: facts(&tip, &best),
        bls: bls_ctx_for_block(params, tip.height + 1),
        epoch: inner.epoch,
        // The statuses the `ChainView` store publishes with this snapshot (BL-236).
        view: ViewToken::new(Arc::clone(&inner.st)),
    });
}

impl TreeRead for SimChain {
    fn lookup(&self, hash: &BlockHash) -> Option<BlockId> {
        self.inner.lock().by_hash.get(hash).copied()
    }

    fn entry(&self, id: BlockId) -> Option<EntryFacts> {
        self.inner.lock().entries.get(id.0 as usize).copied()
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        let inner = self.inner.lock();
        let mut e = *inner.entries.get(id.0 as usize)?;
        if height > e.height {
            return None;
        }
        while e.height > height {
            e = inner.entries[e.prev?.0 as usize];
        }
        Some(e.id)
    }
}

impl ChainAccess for SimChain {
    fn snapshot(&self) -> Arc<ChainSnapshot> {
        self.snapshot_calls.fetch_add(1, Ordering::SeqCst);
        Arc::clone(&self.inner.lock().snap)
    }

    fn valid_scripts(&self, snap: &ChainSnapshot, id: BlockId) -> Result<bool, ViewExpired> {
        let status = self.published_status(snap);
        if self.expired.lock().iter().any(|s| Arc::ptr_eq(s, &status)) {
            return Err(ViewExpired);
        }
        Ok(status.status(id).is_valid(Validity::Scripts))
    }

    // Core: chain.cpp:32-54 LocatorEntries
    fn locator(&self, id: Option<BlockId>) -> Vec<BlockHash> {
        let mut out = Vec::new();
        let mut at = id.and_then(|i| self.entry(i));
        let mut step = 1;
        while let Some(e) = at {
            out.push(e.hash);
            if e.height == 0 {
                break;
            }
            let height = e.height.saturating_sub(step);
            at = self.ancestor(e.id, height).and_then(|i| self.entry(i));
            if out.len() > 10 {
                step *= 2;
            }
        }
        out
    }

    fn loading_blocks(&self) -> bool {
        self.on_read(SimRead::LoadingBlocks);
        self.inner.lock().loading_blocks
    }

    fn min_chain_work(&self) -> Work {
        self.on_read(SimRead::MinChainWork);
        self.inner.lock().min_chain_work
    }

    // Core: validation.cpp:1970-1993 Chainstate::IsInitialBlockDownload (validating chains)
    fn is_initial_block_download(&self, snap: &ChainSnapshot, now: NodeSeconds) -> bool {
        let mut inner = self.inner.lock();
        if !inner.ibd {
            return false;
        }
        let Some(v) = &self.validator else {
            return true;
        };
        if inner.loading_blocks
            || snap.tip.tip_work < inner.min_chain_work
            || i64::from(snap.tip.tip_time) < now.0 - v.max_tip_age
        {
            return true;
        }
        inner.ibd = false;
        false
    }

    fn process_headers(
        &self,
        batch: HeaderBatch,
        from: PeerId,
    ) -> Result<HeadersResult, Interrupted> {
        assert_no_header_cell();
        if self.inner.lock().shutting_down {
            return Err(Interrupted);
        }
        let hook = self.hooks.lock().headers.clone();
        if let Some(h) = hook {
            h(from);
        }
        Ok(self.accept(batch, Some(from)))
    }

    /// Inline by default: V's work runs on the caller's thread and the reply is ready, so no
    /// C5′ wait can run (the caller is the only drainer). After [`SimChain::spawn_v`], queued to
    /// V's thread.
    fn process_block(&self, req: BlockSubmit) -> Result<BlockWait, Interrupted> {
        assert_no_header_cell();
        let v = self.hooks.lock().v.clone();
        match v {
            Some(tx) => {
                let slot = Arc::new(Slot::default());
                tx.send((req, Arc::clone(&slot))).map_err(|_| Interrupted)?;
                Ok(BlockWait(Box::pin(SlotFuture(slot))))
            }
            None => Ok(BlockWait::ready(self.serve_block(req, false))),
        }
    }

    fn status(&self, snap: &ChainSnapshot, id: BlockId) -> Option<EntryStatus> {
        let published = self.published_status(snap);
        let inner = self.inner.lock();
        inner.entries.get(id.0 as usize)?;
        Some(published.status(id))
    }

    fn in_active_chain(&self, snap: &ChainSnapshot, id: BlockId) -> bool {
        self.entry(id).is_some_and(|e| self.ancestor(snap.tip.tip, e.height) == Some(id))
    }

    fn active_at(&self, snap: &ChainSnapshot, height: u32) -> Option<BlockId> {
        self.ancestor(snap.tip.tip, height)
    }

    // Core: chain.cpp:166-181 LastCommonAncestor
    fn last_common_ancestor(&self, a: BlockId, b: BlockId) -> BlockId {
        let inner = self.inner.lock();
        let (mut a, mut b) = (*inner.entry(a), *inner.entry(b));
        while a.height > b.height {
            a = *inner.entry(a.prev.expect("height > 0"));
        }
        while b.height > a.height {
            b = *inner.entry(b.prev.expect("height > 0"));
        }
        while a.id != b.id {
            a = *inner.entry(a.prev.expect("a common genesis"));
            b = *inner.entry(b.prev.expect("a common genesis"));
        }
        a.id
    }

    fn drain_snapshot(&self) -> (Arc<ChainSnapshot>, u64) {
        self.on_read(SimRead::DrainSnapshot);
        let view = self.view.lock();
        (Arc::clone(&view.snap), view.seq)
    }

    fn wait_published(&self, seq: u64) -> Result<(), Interrupted> {
        assert_no_header_cell();
        self.published_waits.fetch_add(1, Ordering::SeqCst);
        let mut view = self.view.lock();
        while view.seq < seq {
            if self.inner.lock().shutting_down {
                return Err(Interrupted);
            }
            self.published.wait(&mut view);
        }
        Ok(())
    }

    /// The stored block at the entry's position now, as `NodeChain` reads it.
    fn read_block(&self, id: BlockId) -> BlockRead {
        self.on_read(SimRead::ReadBlock(id));
        let inner = self.inner.lock();
        let pos = inner.st.status(id).data_pos;
        pos.and_then(|pos| inner.block_at(id, pos))
            .map_or_else(|| inner.failed_read(id), BlockRead::Block)
    }

    /// The block stored at `pos`, when it is `id`'s; else as [`ChainAccess::read_block`].
    fn read_block_at(&self, id: BlockId, pos: BlockPos) -> BlockRead {
        self.on_read(SimRead::ReadBlock(id));
        let inner = self.inner.lock();
        inner.block_at(id, pos).map_or_else(|| inner.failed_read(id), BlockRead::Block)
    }

    fn prune_mode(&self) -> bool {
        self.inner.lock().prune_mode
    }
}
