//! The CoinJoin host traits over V's published view, SP1, the masternode lists and the P2P peer
//! manager. Core citations are v24.0.0.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::sync::Arc;

use dash_chain::{BlockRef, SporkView};
use dash_coinjoin::dstx::DstxChain;
use dash_coinjoin::queue::{MixingMasternode, TipMasternodes};
use dash_consensus::BlockId;
use dash_consensus::ctx::{Deployment, DeploymentPos, DeploymentView as _};
use dash_dml::Dmn;
use dash_evo::types::ProTxHash;
use dash_p2p::rng::FastRandom;
use dash_p2p::{NetCore, OutMsg, PeerId, Service};
use dash_p2p_core::{DsQueue, Inventory};
use dash_primitives::{BlockHash, OutPoint, TxHash};
use dashd_chainstate::chain::{ChainViewHandle, QueryDeployments, no_wait_marked};
use dashd_coinjoin::env::{ChainView, MnLists, PeerSink};
use dashd_coinjoin::server::{RandomSource, ServerClock};
use dashd_masternode::active::worker::{CheckedListForBlock, ListForBlock};
use dashd_masternode::sync::MasternodeSync;
use dashd_masternode::tip::TipMnList;
use dashd_p2p::processing::peerman::Shared;

/// [`ChainView`] over V's published view, SP1's sync state and spork 19. Only the two V24 reads
/// query V, at the call and on the calling thread; the signals callback thread (no-wait marked)
/// calls the others.
#[derive(Clone)]
pub struct NodeChainView {
    view: ChainViewHandle,
    sync: Arc<MasternodeSync>,
    sporks: Arc<dyn SporkView>,
}

impl NodeChainView {
    pub fn new(
        view: ChainViewHandle,
        sync: Arc<MasternodeSync>,
        sporks: Arc<dyn SporkView>,
    ) -> Self {
        Self {
            view,
            sync,
            sporks,
        }
    }

    /// The V24 seam (DEC-1802): `read` on V for `tip`, at Core's read points on msghand. Asserts
    /// on a no-wait thread; a V error (shutdown) reads as false.
    // Core: coinjoin/coinjoin.cpp:144, :255-262; net_processing.cpp:3660, :3668
    fn v24(&self, tip: Anchor, read: fn(&QueryDeployments<'_>, BlockId) -> bool) -> bool {
        assert!(!no_wait_marked(), "a V24 read on a no-wait thread");
        tip.is_some_and(|(t, _)| {
            self.view.query(move |q| read(&q.deployments, t)).recv().unwrap_or(false)
        })
    }
}

const V24: Deployment = Deployment::Versionbits(DeploymentPos::V24);

type Anchor = Option<(BlockId, u32)>;

fn published_tip(view: &ChainViewHandle) -> Anchor {
    let view = view.view();
    view.snap.has_tip().then_some((view.snap.tip.tip, view.snap.tip.tip_height))
}

/// The hash of the floor's ancestor at a height, from the block index.
pub(super) fn floor_ancestor_hash(
    view: &ChainViewHandle,
) -> impl Fn(BlockId, u32) -> Option<BlockHash> + '_ {
    |id, height| view.ancestor(id, height).and_then(|a| view.entry(a)).map(|e| e.hash)
}

impl ChainView for NodeChainView {
    fn tip(&self) -> Option<(i32, BlockHash)> {
        let view = self.view.view();
        if !view.snap.has_tip() {
            return None;
        }
        Some((i32::try_from(view.snap.tip.tip_height).ok()?, view.snap.tip.tip_hash))
    }

    /// `DeploymentActiveAt(*tip, V24)`.
    fn v24_active_at_tip(&self) -> bool {
        self.v24(published_tip(&self.view), |d, tip| d.active_at(tip, V24))
    }

    /// `DeploymentActiveAfter(tip, V24)`.
    fn v24_active_after_tip(&self) -> bool {
        self.v24(published_tip(&self.view), |d, tip| d.active_after(Some(tip), V24))
    }

    fn has_chain_lock(&self, height: i32, hash: &BlockHash) -> bool {
        let (enabled, floor) = (self.sporks.chainlocks_enabled(), self.view.view().snap.cl_floor);
        has_chain_lock(enabled, floor, height, hash, floor_ancestor_hash(&self.view))
    }

    // Core: masternode/sync.h:79
    fn blockchain_synced(&self) -> bool {
        self.sync.is_blockchain_synced()
    }
}

/// Core's `Chainlocks::HasChainLock` over a published `bestChainLockBlockIndex`: above it false,
/// at its height the hash, below it its ancestor's hash.
// Core: chainlock/chainlock.cpp:49-71
pub(super) fn has_chain_lock(
    enabled: bool,
    floor: Option<BlockRef>,
    height: i32,
    hash: &BlockHash,
    ancestor_hash: impl FnOnce(BlockId, u32) -> Option<BlockHash>,
) -> bool {
    let (Some(floor), true) = (floor, enabled) else {
        return false;
    };
    let Ok(height) = u32::try_from(height) else {
        return false;
    };
    match height.cmp(&floor.height) {
        Ordering::Greater => false,
        Ordering::Equal => *hash == floor.hash,
        Ordering::Less => ancestor_hash(floor.id, height) == Some(*hash),
    }
}

fn mixing(dmn: &Dmn) -> MixingMasternode {
    MixingMasternode {
        pro_tx_hash: TxHash::from(*dmn.pro_tx_hash.as_bytes()),
        collateral: dmn.collateral,
        operator_key: dmn.cold.pub_key_operator.get(),
    }
}

fn protx(hash: &TxHash) -> ProTxHash {
    ProTxHash::from(*hash.as_bytes())
}

/// [`MnLists`] over the published tip list and, for [`MnLists::mn_at_depth`], V's
/// `GetListForBlock`, which asserts on a no-wait thread.
pub struct NodeMnLists<L> {
    tip: TipMnList,
    view: ChainViewHandle,
    lists: CheckedListForBlock<L>,
}

impl<L: ListForBlock> NodeMnLists<L> {
    /// `lists` answers `GetListForBlock` (the node's `p2p::active_mn::node_lists`).
    pub fn new(tip: TipMnList, view: ChainViewHandle, lists: L) -> Self {
        Self {
            tip,
            view,
            lists: CheckedListForBlock::new(lists, no_wait_marked),
        }
    }

    // Core: evo/deterministicmns.h:796-799; net_processing.cpp:3675-3682
    pub(super) fn mn_at(&self, at: Anchor, depth: i32, pro: &TxHash) -> Option<MixingMasternode> {
        assert!(!no_wait_marked(), "a masternode list read on a no-wait thread");
        let height = u32::try_from(i64::from(at?.1) - i64::from(depth)).ok()?;
        let block = self.view.ancestor(at?.0, height)?;
        let list = self.lists.list_for_block(block).ok()?;
        list.get(&protx(pro)).map(|dmn| mixing(dmn))
    }
}

// Core: evo/deterministicmns.cpp:941-947 GetListAtChainTip
impl<L: ListForBlock> TipMasternodes for NodeMnLists<L> {
    fn valid_mn(&self, pro_tx_hash: &TxHash) -> Option<MixingMasternode> {
        self.tip.load().list.get_valid(&protx(pro_tx_hash)).map(|dmn| mixing(dmn))
    }

    fn enabled_count(&self) -> i32 {
        i32::try_from(self.tip.load().list.counts().enabled()).unwrap_or(i32::MAX)
    }
}

impl<L: ListForBlock> MnLists for NodeMnLists<L> {
    fn valid_by_collateral(&self, outpoint: &OutPoint) -> Option<MixingMasternode> {
        self.tip.load().list.get_valid_by_collateral(outpoint).map(|dmn| mixing(dmn))
    }

    fn mn_at_depth(&self, depth: i32, pro_tx_hash: &TxHash) -> Option<MixingMasternode> {
        self.mn_at(published_tip(&self.view), depth, pro_tx_hash)
    }
}

/// [`DstxChain`] against one tip, built once per `validate_dstx` call: Core takes
/// `pindex = Tip()` once (np:3652-3656) and walks from it (np:3673-3681).
pub struct NodeDstxChain<'a, L> {
    chain: &'a NodeChainView,
    mns: &'a NodeMnLists<L>,
    tip: Anchor,
}

impl<'a, L: ListForBlock> NodeDstxChain<'a, L> {
    pub fn pin(chain: &'a NodeChainView, mns: &'a NodeMnLists<L>) -> Self {
        Self {
            chain,
            mns,
            tip: published_tip(&chain.view),
        }
    }
}

impl<L: ListForBlock> DstxChain for NodeDstxChain<'_, L> {
    fn tip_height(&self) -> Option<i32> {
        i32::try_from(self.tip?.1).ok()
    }

    fn v24_active_at_tip(&self) -> bool {
        self.chain.v24(self.tip, |d, tip| d.active_at(tip, V24))
    }

    fn v24_active_after_tip(&self) -> bool {
        self.chain.v24(self.tip, |d, tip| d.active_after(Some(tip), V24))
    }

    fn mn_at_depth(&self, depth: i32, pro_tx_hash: &TxHash) -> Option<MixingMasternode> {
        self.mns.mn_at(self.tip, depth, pro_tx_hash)
    }
}

type HeldDsq = Vec<(Arc<Shared>, DsQueue)>;

thread_local! {
    /// The queues [`NodePeerSink::outside_server`] holds on this thread until its call returns.
    static HELD_DSQ: RefCell<Option<HeldDsq>> = const { RefCell::new(None) };
}

/// [`PeerSink`] over the P2P peer manager. Every call takes only peer-manager leaves (the
/// connection table, the object-request tracker, inventory cells, misbehaviour cells) or
/// queues for the actor, except `relay_dsq`, which reads each peer's state: inside a pass the
/// peer manager defers it, and on another thread [`NodePeerSink::outside_server`] holds it until
/// the server's lock is released. None of them calls back into the server.
#[derive(Clone)]
pub struct NodePeerSink {
    shared: Arc<Shared>,
}

impl NodePeerSink {
    pub fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
        }
    }

    /// Runs `f`, which takes the server's lock off the message thread (the scheduler's
    /// `SharedServer::tick`), and relays the queues it produced once `f` has returned. The
    /// message thread holds a peer's state while it waits for the server's lock, so a relay
    /// under that lock could deadlock against it.
    pub fn outside_server<T>(f: impl FnOnce() -> T) -> T {
        let hold = Hold::start();
        let out = f();
        for (shared, dsq) in hold.release() {
            shared.relay_dsq(&dsq);
        }
        out
    }
}

/// The outermost [`NodePeerSink::outside_server`] on this thread; a panic drops its queues.
struct Hold(bool);

impl Hold {
    fn start() -> Self {
        Self(HELD_DSQ.with(|held| {
            let mut held = held.borrow_mut();
            let outer = held.is_none();
            held.get_or_insert_with(Vec::new);
            outer
        }))
    }

    fn release(self) -> HeldDsq {
        let held = if self.0 {
            HELD_DSQ.with(|h| h.borrow_mut().take())
        } else {
            None
        };
        held.unwrap_or_default()
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        if self.0 {
            HELD_DSQ.with(|h| h.borrow_mut().take());
        }
    }
}

impl PeerSink for NodePeerSink {
    // Core: coinjoin/server.cpp:1328
    fn push_to(&mut self, peer: PeerId, msg: OutMsg) {
        self.shared.push_to(peer, msg);
    }

    // Core: coinjoin/server.cpp:1312-1315,1375-1378; net.h:1369-1373
    fn push_to_addr(&mut self, addr: &Service, msg: OutMsg) -> bool {
        self.shared.push_to_addr(addr, msg)
    }

    fn is_connected(&self, addr: &Service) -> bool {
        self.shared.is_connected(addr)
    }

    // Core: net_processing.cpp:6919-6923
    fn misbehaving(&mut self, peer: PeerId, score: i32, reason: &str) {
        self.shared.misbehaving(peer, score, reason);
    }

    // Core: net_processing.cpp:6930-6935
    fn erase_object_request(&mut self, peer: PeerId, inv: Inventory) {
        self.shared.erase_object_request(peer, &inv);
    }

    // Core: net_processing.cpp:6974-6977
    fn forget_object_request(&mut self, inv: Inventory) {
        self.shared.forget_object_request(&inv);
    }

    // Core: net_processing.cpp:6984-6987
    fn relay_inv(&mut self, inv: Inventory) {
        self.shared.relay_inv(&inv);
    }

    /// The peer manager's `PushInv` queues `MSG_TX`/`MSG_DSTX` on the transaction path.
    // Core: net_processing.cpp:6984-6987, :1239-1256
    fn relay_tx_inv(&mut self, inv: Inventory) {
        self.shared.relay_inv(&inv);
    }

    // Core: net_processing.cpp:6999-7002
    fn relay_dsq(&mut self, dsq: &DsQueue) {
        let held = HELD_DSQ.with(|held| {
            held.borrow_mut()
                .as_mut()
                .map(|queues| queues.push((Arc::clone(&self.shared), dsq.clone())))
                .is_some()
        });
        if !held {
            self.shared.relay_dsq(dsq);
        }
    }

    /// Queued for the actor.
    // Core: net_processing.cpp:7004-7007
    fn relay_transaction(&mut self, txid: &TxHash) {
        self.shared.relay_transaction(txid);
    }
}

/// [`ServerClock`] over the network core's mocktime-aware clock and its peers' time offset.
#[derive(Clone)]
pub struct NodeServerClock {
    core: Arc<NetCore>,
}

impl NodeServerClock {
    pub fn new(core: Arc<NetCore>) -> Self {
        Self {
            core,
        }
    }
}

impl ServerClock for NodeServerClock {
    fn now(&self) -> i64 {
        self.core.clock().node_now().0
    }

    fn adjusted_time(&self) -> i64 {
        self.core.adjusted_time()
    }
}

/// Core's `GetRand<int>(n)`: a fresh `FastRandomContext` keyed by `GetRandHash()`, then
/// `randrange(n)`.
// Core: random.h GetRand; random.cpp:695-699
#[derive(Clone, Copy, Debug, Default)]
pub struct NodeRandom;

impl RandomSource for NodeRandom {
    fn below(&mut self, upper: u32) -> u32 {
        let draw = FastRandom::with_seed(dashd_random::get_rand_hash()).randrange(u64::from(upper));
        u32::try_from(draw).expect("a draw below a u32 bound")
    }
}
