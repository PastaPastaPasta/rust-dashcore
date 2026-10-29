//! `PeerLogic`: Core's `ProcessMessages`, `ProcessMessage` and `SendMessages` for one peer
//! (P2a §3.1.2, §3.1.4, §3.2), run on the actor thread only.
//!
//! The network-level branches (handshake, ping, address intake, the gates) are SB6's functions,
//! called at Core's points; the processor keeps Core's per-peer and global state and calls the
//! later tasks' seams at the positions Core gives them.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use dash_evo::locks::{ClsigHashSchemes, clsig_hash};
use dash_num::Hash256;
use dash_p2p::gates::{self, Gate};
use dash_p2p::handshake::{
    self, HandshakeHost, LocalIdentity, PeerNet, PeerVersion, VersionInputs,
};
use dash_p2p::policy::{self, Throw};
use dash_p2p::{
    CommandString, ConnectionType, NetCore, OutMsg, PeerHandle, PeerId, Permissions,
    ProtocolVersion, RawMsg, ServiceFlags, Step, Why, addr, ping,
};
use dash_p2p_core::{
    ClSig, GetData, Inv, InvType, Inventory, NotFound, ProtoCtx, QSendRecSigs, SendCmpct, SendDsq,
};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use parking_lot::{Mutex, MutexGuard};

use dash_consensus::BlockValidationError;
use dash_primitives::BlockHash;

use super::blocks::BlockStep;
use super::eviction::OutboundFlags;
use super::misbehavior::{self, MisbehaviorCell};
use super::peer::{BlockInvCell, HeaderCell, HeaderGuard, PeerState, WantsDsq};
use super::peerman::{Ahead, Signals};
use super::registry::{HandlerCx, MessageProcessingResult, Registry, row_of};
use super::relay::{self, InvRelayCell};
use super::seams::{
    AddressRelay, BlockCheckedCx, BlockSync, ChainAccess, ChainLocks, ChainSnapshot, GetDataBatch,
    Interrupted, IsLockLookup, ObjectRequests, OverlayState, SyncCx,
};
use super::serve_blocks::process_get_block_data;
use super::signals::SyncSignal;
use super::solicit::{GetDataResponse, UNREQUESTED_OBJECT_MISBEHAVIOR_SCORE, object_interval_us};
use super::txrequest::TxRequestTracker;

/// Core's `MAX_INV_SZ`.
pub const MAX_INV_SZ: usize = 50_000;
/// The `notfound` bound at v24: `min(MAX_INV_SZ, MAX_PEER_OBJECT_ANNOUNCEMENTS +
/// MAX_BLOCKS_IN_TRANSIT_PER_PEER)` (`np:5752`; DEC-82).
pub const NOTFOUND_LIMIT: usize = MAX_INV_SZ;
/// Core's `CMPCTBLOCKS_VERSION`.
pub const CMPCTBLOCKS_VERSION: u64 = 1;
/// Core's `AVG_ADDRESS_BROADCAST_INTERVAL`: 30 s, in µs.
pub const AVG_ADDRESS_BROADCAST_INTERVAL_US: i64 = 30 * 1_000_000;

/// Core's `MAX_PEER_OBJECT_ANNOUNCEMENTS`: the tracker's cap per peer.
// Core: net_processing.cpp:85-94
pub const MAX_PEER_OBJECT_ANNOUNCEMENTS: usize = 100_000;
/// Core's `MAX_PEER_OBJECT_REQUEST_IN_FLIGHT`.
pub const MAX_PEER_OBJECT_REQUEST_IN_FLIGHT: usize = 100;
/// Core's `NONPREF_PEER_TX_DELAY`: 2 s, in µs.
pub const NONPREF_PEER_TX_DELAY_US: i64 = 2_000_000;
/// Core's `OVERLOADED_PEER_OBJECT_DELAY`: 2 s, in µs.
pub const OVERLOADED_PEER_OBJECT_DELAY_US: i64 = 2_000_000;

/// Core's `DEFAULT_MAX_TIP_AGE`: 6 hours, in seconds.
pub const DEFAULT_MAX_TIP_AGE: i64 = 6 * 60 * 60;

/// The processor's options (ND1 parses them).
#[derive(Clone, Debug)]
pub struct ProcessorOptions {
    /// `-blocksonly` (Core's `m_ignore_incoming_txs`).
    pub blocks_only: bool,
    /// `-maxtipage`, in seconds.
    pub max_tip_age: i64,
}

impl Default for ProcessorOptions {
    fn default() -> Self {
        Self {
            blocks_only: false,
            max_tip_age: DEFAULT_MAX_TIP_AGE,
        }
    }
}

/// One peer's entry in the [`PeerMap`]: its state, and the cells (also in the state) that
/// callers outside the actor use without the state's lock: misbehaviour (Core's
/// `m_misbehavior_mutex`), the block announcements queue (`m_block_inv_mutex`), the header
/// cell (the peer's share of `cs_main`, P3 D50) and the inventory relay
/// (`m_tx_inventory_mutex`, P5a D10).
#[derive(Clone)]
pub struct PeerSlot {
    /// The processor state; only the actor holds it beyond a short read.
    pub state: Arc<Mutex<PeerState>>,
    /// `state.misbehavior`.
    pub misbehavior: MisbehaviorCell,
    /// `state.block_inv`.
    pub block_inv: BlockInvCell,
    /// `state.header_cell`.
    pub header_cell: HeaderCell,
    /// `state.inv_relay`.
    pub inv_relay: InvRelayCell,
}

impl std::fmt::Debug for PeerSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PeerSlot").finish_non_exhaustive()
    }
}

impl PeerSlot {
    /// A slot around `state`.
    pub fn new(state: PeerState) -> Self {
        Self {
            misbehavior: Arc::clone(&state.misbehavior),
            block_inv: Arc::clone(&state.block_inv),
            header_cell: state.header_cell.clone(),
            inv_relay: Arc::clone(&state.inv_relay),
            state: Arc::new(Mutex::new(state)),
        }
    }
}

/// One drain's observation (P3 §3.6 point 3, D47): the `ChainView` it took and that view's
/// `view_seq`, the cutoff it applied. Every region reads `snap`.
#[derive(Clone, Debug)]
pub struct Observation {
    /// The drained snapshot.
    pub snap: Arc<ChainSnapshot>,
    /// Its `view_seq`.
    pub seq: u64,
}

/// A peer's state lock with its header region open (P3 §3.6). Dropping it leaves the region
/// and then unlocks the state, on every path, a panic's included, so no header guard outlives
/// the state lock.
pub(super) struct Region<'a>(MutexGuard<'a, PeerState>);

impl std::ops::Deref for Region<'_> {
    type Target = PeerState;
    fn deref(&self) -> &PeerState {
        &self.0
    }
}

impl std::ops::DerefMut for Region<'_> {
    fn deref_mut(&mut self) -> &mut PeerState {
        &mut self.0
    }
}

impl<'a> Region<'a> {
    /// A region on `st` with no stamp test, for reads that use no snapshot (tests and
    /// diagnostics; P3 D62).
    pub(super) fn peek(mut st: MutexGuard<'a, PeerState>) -> Self {
        let cell = st.header_cell.clone();
        st.enter_region(HeaderGuard::lock(&cell));
        Self(st)
    }

    /// The state lock the region holds, to relock after a release.
    fn mutex(&self) -> &'a Mutex<PeerState> {
        MutexGuard::mutex(&self.0)
    }
}

impl Drop for Region<'_> {
    fn drop(&mut self) {
        self.0.leave_region();
    }
}

/// Every peer's processor state, shared with the network core's callbacks: `initialize` inserts
/// on the dialling task, `finalize` removes, and only the actor locks a [`PeerState`] (one at a
/// time, except while [`AddressRelay::relay`] picks targets), so no two state locks are ever
/// held together otherwise. The misbehaviour, block-inv and inv-relay cells are leaf locks:
/// nothing else is taken under them.
pub type PeerMap = Mutex<BTreeMap<PeerId, PeerSlot>>;

/// The processor's global counters (Core's `PeerManagerImpl` members P2a keeps).
#[derive(Debug)]
pub struct Counters {
    /// `m_num_preferred_download_peers`.
    pub num_preferred_download: i32,
    /// `nSyncStarted`.
    pub sync_started: i32,
    /// `m_outbound_peers_with_protect_from_disconnect`.
    pub outbound_with_protect: i32,
    /// `m_last_block_inv_triggering_headers_sync` (null at start).
    pub last_block_inv_triggering_sync: BlockHash,
    /// `m_next_inv_to_inbounds` in µs (D61).
    pub next_inv_to_inbounds_us: i64,
    /// `hashRecentRejectsChainTip` (null at start, D69).
    pub recent_rejects_chain_tip: BlockHash,
}

impl Default for Counters {
    fn default() -> Self {
        Self {
            num_preferred_download: 0,
            sync_started: 0,
            outbound_with_protect: 0,
            last_block_inv_triggering_sync: BlockHash::ZERO,
            next_inv_to_inbounds_us: 0,
            recent_rejects_chain_tip: BlockHash::ZERO,
        }
    }
}

/// The later tasks' seams at Core's positions (P2a §2.4), each with its M2 default.
pub struct Seams {
    /// P3.
    pub block_sync: Box<dyn BlockSync>,
    /// P5.
    pub objects: Box<dyn ObjectRequests>,
    /// P2a-net.
    pub addr_relay: Box<dyn AddressRelay>,
    /// LK1a (P5a D4, D5).
    pub chainlocks: Box<dyn ChainLocks>,
    /// SP1 and LK1b: the state `AlreadyHave` reads (P5a D20).
    pub overlay: Box<dyn OverlayState>,
    /// LK1b (P4b D9).
    pub islocks: Box<dyn IsLockLookup>,
}

impl Default for Seams {
    fn default() -> Self {
        Self {
            block_sync: Box::new(super::seams::NoBlockSync),
            objects: Box::new(super::seams::NoObjectRequests),
            addr_relay: Box::new(super::seams::NoAddressRelay),
            chainlocks: Box::new(super::seams::NoChainLocks),
            overlay: Box::new(super::seams::StaticOverlayState::default()),
            islocks: Box::new(super::seams::NoIsLocks),
        }
    }
}

/// Core's `CNode::IsBlockRelayOnly()`.
// Core: net.cpp:554-560
pub(super) fn is_block_relay_only(peer: &PeerHandle, blocks_only: bool) -> bool {
    (blocks_only && !peer.has_permission(Permissions::RELAY))
        || peer.conn_type() == ConnectionType::BlockRelay
}

/// Core's `RejectIncomingTxs(peer)`.
// Core: net_processing.cpp:6273-6281 PeerManagerImpl::RejectIncomingTxs
pub fn reject_incoming_txs(peer: &PeerHandle, blocks_only: bool) -> bool {
    matches!(peer.conn_type(), ConnectionType::BlockRelay | ConnectionType::Feeler)
        || (blocks_only && !peer.has_permission(Permissions::RELAY))
}

/// Core's `CanServeBlocks(peer)`.
// Core: net_processing.cpp:1190-1193
pub(super) fn can_serve_blocks(services: ServiceFlags) -> bool {
    services.has(ServiceFlags::NODE_NETWORK) || services.has(ServiceFlags::NODE_NETWORK_LIMITED)
}

/// `PushNodeVersion`'s processor inputs for `peer` (P2a §3.3.4).
pub fn version_inputs(
    core: &NetCore,
    chain: &dyn ChainAccess,
    peer: &PeerHandle,
    our_services: ServiceFlags,
    blocks_only: bool,
) -> VersionInputs {
    VersionInputs {
        our_services,
        best_height: chain.snapshot().tip.tip_height as i32,
        relay: !reject_incoming_txs(peer, blocks_only),
        now: core.clock().node_now().0,
    }
}

/// A message with `payload` encoded as Core serializes it.
pub fn out_msg<T: BaseCodec>(cmd: CommandString, payload: &T) -> OutMsg {
    let mut buf = Vec::new();
    payload.encode(&mut buf);
    OutMsg::new(cmd, buf)
}

/// The handler context of one `getdata` item, on a snapshot taken now.
fn getdata_cx<'a>(
    core: &'a NetCore,
    peer: &'a PeerHandle,
    state: &'a mut PeerState,
    chain: &'a dyn ChainAccess,
    signals: &'a Signals,
    requests: Arc<Mutex<TxRequestTracker>>,
) -> HandlerCx<'a> {
    let snap = chain.snapshot();
    HandlerCx {
        core,
        peer,
        state,
        chain,
        ctx: ProtoCtx::new(peer.common_version(), snap.bls),
        snap,
        now_us: core.clock().node_now_micros(),
        signals,
        requests,
        post: Vec::new(),
    }
}

/// The processor's state at Core's handshake points (SB6's [`HandshakeHost`]): the fields of
/// [`PeerState`] the handshake touches, borrowed apart from [`PeerState::net`], which SB6's
/// functions take alongside it.
struct Host<'a> {
    id: PeerId,
    our_services: ServiceFlags,
    misbehavior: &'a MisbehaviorCell,
    tx_relay: &'a mut bool,
    relay_txs: &'a mut bool,
    preferred_download: &'a mut bool,
    counters: &'a mut Counters,
    blocks_only: bool,
}

impl<'a> Host<'a> {
    /// The host and the peer's `PeerNet`, borrowed from `st` side by side.
    fn split(
        st: &'a mut PeerState,
        counters: &'a mut Counters,
        blocks_only: bool,
    ) -> (Self, &'a mut PeerNet) {
        let PeerState {
            id,
            our_services,
            net,
            misbehavior,
            tx_relay,
            relay_txs,
            preferred_download,
            ..
        } = st;
        let host = Self {
            id: *id,
            our_services: *our_services,
            misbehavior,
            tx_relay,
            relay_txs,
            preferred_download,
            counters,
            blocks_only,
        };
        (host, net)
    }
}

impl HandshakeHost for Host<'_> {
    fn misbehaving(&mut self, score: i32, msg: &'static str) {
        misbehavior::misbehaving(self.id, self.misbehavior, score, msg);
    }

    // Core: net_processing.cpp:4070-4101 (SetTxRelay; txreconciliation is off)
    fn version_recorded(&mut self, peer: &PeerHandle, v: &PeerVersion) {
        if !matches!(peer.conn_type(), ConnectionType::BlockRelay | ConnectionType::Feeler)
            && (v.relay || self.our_services.has(ServiceFlags::NODE_BLOOM))
        {
            *self.tx_relay = true;
            *self.relay_txs = v.relay;
            if v.relay {
                peer.set_relays_txs(true);
            }
        }
    }

    // Core: net_processing.cpp:4106-4111
    fn verack_pushed(&mut self, peer: &PeerHandle) {
        let preferred = (!peer.is_inbound() || peer.has_permission(Permissions::NO_BAN))
            && peer.conn_type() != ConnectionType::AddrFetch
            && can_serve_blocks(peer.version_services());
        *self.preferred_download = preferred;
        self.counters.num_preferred_download += i32::from(preferred);
    }

    fn reject_incoming_txs(&self, peer: &PeerHandle) -> bool {
        reject_incoming_txs(peer, self.blocks_only)
    }
}

/// Core's `PeerManagerImpl` minus the thread: the per-peer and global state and the seams.
pub struct PeerLogic {
    pub(super) core: Arc<NetCore>,
    identity: Arc<LocalIdentity>,
    pub(super) opts: ProcessorOptions,
    pub(super) chain: Arc<dyn ChainAccess>,
    pub(super) peers: Arc<PeerMap>,
    interrupt: Arc<AtomicBool>,
    registry: Registry,
    pub(super) seams: Seams,
    /// `m_object_request`: every object type's announcements and requests (P5a D8).
    /// Shared with the handler contexts, which consume a response on Core's callback (P5a D21).
    pub(super) object_request: Arc<Mutex<TxRequestTracker>>,
    /// ConnMan's stale-tip and extra-block-relay flags, and `m_last_tip_update`.
    pub(super) flags: Arc<OutboundFlags>,
    /// `m_stale_tip_check_time`, in seconds.
    pub(super) stale_tip_check_time: i64,
    /// `m_initial_sync_finished`.
    pub(super) initial_sync_finished: bool,
    /// The global counters.
    pub counters: Counters,
    /// The signal queue, the bells, the announce stamp and `recent` (P3 §2.5).
    pub(super) signals: Arc<Signals>,
    /// How many region starts met `Ahead` and retried (P3 §3.6), for tests.
    pub region_retries: u64,
}

/// A reader of `m_object_request`'s per-peer counts for another thread (SP1's sync tick).
#[derive(Clone)]
pub struct RequestedObjects(Arc<Mutex<TxRequestTracker>>);

impl RequestedObjects {
    /// Core's `PeerGetRequestedObjectCount(id)`: the peer's announcements in the tracker.
    // Core: net_processing.cpp:1696-1700, 7016-7019
    pub fn count(&self, peer: PeerId) -> usize {
        self.0.lock().count(peer)
    }
}

impl PeerLogic {
    /// The object tracker's counts, for SP1's sync tick.
    pub fn requested_objects(&self) -> RequestedObjects {
        RequestedObjects(Arc::clone(&self.object_request))
    }

    /// A processor over `core` and `chain`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        core: Arc<NetCore>,
        identity: Arc<LocalIdentity>,
        opts: ProcessorOptions,
        chain: Arc<dyn ChainAccess>,
        peers: Arc<PeerMap>,
        interrupt: Arc<AtomicBool>,
        registry: Registry,
        mut seams: Seams,
        flags: Arc<OutboundFlags>,
        signals: Arc<Signals>,
    ) -> Self {
        seams.block_sync.bind_source_counter(Arc::clone(&signals.source_len));
        seams.block_sync.bind_stalling_timeout(signals.stalling_timeout.clone());
        seams.block_sync.bind_ignore_incoming_txs(opts.blocks_only);
        // `m_object_request`'s `PriorityComputer`: two `GetRand(0xFFFFFFFFFFFFFFFF)` draws.
        // Core: txrequest.cpp PriorityComputer
        let k0 = core.get_rand(u64::MAX);
        let k1 = core.get_rand(u64::MAX);
        Self {
            core,
            identity,
            opts,
            chain,
            peers,
            interrupt,
            registry,
            seams,
            object_request: Arc::new(Mutex::new(TxRequestTracker::new(k0, k1))),
            flags,
            stale_tip_check_time: 0,
            initial_sync_finished: false,
            counters: Counters::default(),
            signals,
            region_retries: 0,
        }
    }

    /// The network core.
    pub fn core(&self) -> &Arc<NetCore> {
        &self.core
    }

    fn interrupted(&self) -> bool {
        self.interrupt.load(Ordering::SeqCst)
    }

    pub(super) fn state_of(&self, id: PeerId) -> Option<Arc<Mutex<PeerState>>> {
        self.peers.lock().get(&id).map(|s| Arc::clone(&s.state))
    }

    /// A drain (P3 §3.6 point 3): one `ChainView` load, then the queued signals stamped at or
    /// below its `view_seq`, in push order, against its snapshot (D37, D47). Never called with a
    /// peer's state locked: `BlockChecked` locks its source's.
    pub fn drain_sync_signals(&mut self) -> Observation {
        let (snap, seq) = self.chain.drain_snapshot();
        let obs = Observation {
            snap,
            seq,
        };
        self.drain_at(&obs);
        obs
    }

    /// The drain for an observation already taken (the head of `send_messages` takes it under
    /// the `block_inv` cell).
    fn drain_at(&mut self, obs: &Observation) {
        let signals = Arc::clone(&self.signals);
        for s in signals.queue.drain(obs.seq) {
            let SyncSignal::BlockChecked {
                hash,
                invalid,
                ibd,
            } = &*s;
            self.block_checked(hash, invalid.as_ref(), *ibd, &obs.snap);
        }
    }

    /// A region start (P3 §3.6): with `st` locked, takes its header cell for `obs`, running
    /// `read` under the cell before the stamp read. On `Ahead`, releases both, waits for that
    /// publication, drains again and retries; nothing of the region has run. `Err` when the chain
    /// side is shutting down.
    pub(super) fn enter_region<'a, R>(
        &mut self,
        mut st: MutexGuard<'a, PeerState>,
        mut obs: Observation,
        read: impl Fn(&Signals) -> R,
    ) -> Result<(Region<'a>, Observation, R), Interrupted> {
        loop {
            let cell = st.header_cell.clone();
            match self.signals.lock_headers(&cell, obs.seq, &read) {
                Ok((guard, r)) => {
                    st.enter_region(guard);
                    return Ok((Region(st), obs, r));
                }
                Err(Ahead(stamp)) => {
                    let state = MutexGuard::mutex(&st);
                    drop(st);
                    self.region_retries += 1;
                    self.chain.wait_published(stamp)?;
                    obs = self.drain_sync_signals();
                    st = state.lock();
                }
            }
        }
    }

    /// A drain, then a region on `state` for it: how a message, a `Redrain` relock and phase 3
    /// start (P3 §3.6, I0).
    fn open_region<'a>(
        &mut self,
        state: &'a Mutex<PeerState>,
    ) -> Result<(Region<'a>, Observation), Interrupted> {
        let obs = self.drain_sync_signals();
        let (region, obs, ()) = self.enter_region(state.lock(), obs, |_| ())?;
        Ok((region, obs))
    }

    /// The rest of Core's `FinalizeNode` once the state has left the map: the counters, the
    /// seams and AddrMan's `Connected`.
    // Core: net_processing.cpp:1783-1856 PeerManagerImpl::FinalizeNode
    pub fn finalize(&mut self, peer: &PeerHandle, state: &PeerState) {
        let c = &mut self.counters;
        c.sync_started -= i32::from(state.sync_started);
        c.num_preferred_download -= i32::from(state.preferred_download);
        c.outbound_with_protect -= i32::from(state.chain_sync.protect);
        self.seams.block_sync.finalize(state.id);
        self.object_request.lock().disconnected_peer(state.id);
        self.seams.objects.finalize(state.id);
        if peer.successfully_connected()
            && state.misbehavior.lock().score == 0
            && peer.conn_type() != ConnectionType::BlockRelay
            && !peer.is_inbound()
        {
            let now = self.core.clock().node_now().0;
            self.core.addrman().connected(&peer.addr().service, now);
        }
        tracing::debug!(target: "net", "Cleared nodestate for peer={}", state.id);
    }

    /// Core's `ProcessMessages`: the getdata queue, then one message (P2a §3.1.2). Returns
    /// Core's `fMoreWork` for this peer.
    // Core: net_processing.cpp:5872-5947 PeerManagerImpl::ProcessMessages
    pub fn process_messages(&mut self, peer: &PeerHandle) -> bool {
        let Some(state) = self.state_of(peer.id()) else {
            return false;
        };
        // I0: the message's snapshot comes through a drain, before the state lock (P3 §3.6).
        let obs = self.drain_sync_signals();
        let mut st = state.lock();
        if !st.getdata_requests.is_empty() {
            self.process_getdata(peer, &mut st);
        }
        // `ProcessOrphanTx` is P5's (`NoOrphans` at M2).
        let processed_orphan = false;
        if peer.disconnect_requested() {
            return false;
        }
        if processed_orphan {
            return true;
        }
        if !st.getdata_requests.is_empty() {
            return true;
        }
        if peer.pause_send() {
            return false;
        }
        let Some(polled) = self.core.pop(peer) else {
            return false;
        };
        let mut more = polled.more;
        let Ok((region, obs, ())) = self.enter_region(st, obs, |_| ()) else {
            return false;
        };
        match self.run_message(peer, region, &polled.msg, obs, true) {
            Ok(st) => {
                if self.interrupted() {
                    return false;
                }
                if st.is_some_and(|st| !st.getdata_requests.is_empty()) {
                    more = true;
                }
            }
            Err(t) => {
                let line = t.log_line(&polled.msg.cmd, polled.msg.payload.len());
                tracing::debug!(target: "net", "{line}");
            }
        }
        more
    }

    /// One message in `region`: phase 1 (through the gates when `gated`), then the steps it
    /// returns (P3 §2.6). Returns the region the message ended in, `None` when the chain side
    /// shut down meanwhile.
    fn run_message<'a>(
        &mut self,
        peer: &PeerHandle,
        mut region: Region<'a>,
        msg: &RawMsg,
        obs: Observation,
        gated: bool,
    ) -> Result<Option<Region<'a>>, Throw> {
        let state = region.mutex();
        let mut step = if gated {
            self.process_message(peer, &mut region, msg, &obs.snap)?
        } else {
            self.admitted(peer, &mut region, msg, &obs.snap)?
        };
        loop {
            match step {
                BlockStep::Done => return Ok(Some(region)),
                BlockStep::Redrain(after) => {
                    drop(region);
                    let Ok((r, obs)) = self.open_region(state) else {
                        return Ok(None);
                    };
                    region = r;
                    step = self.after_headers(peer, &mut region, after, &obs.snap);
                }
                BlockStep::Process {
                    submit,
                    tail,
                } => {
                    drop(region);
                    let hash = submit.hash;
                    let Ok(outcome) = self.wait_block(submit) else {
                        return Ok(None);
                    };
                    // C1: everything V emitted before its reply, then the tail's region.
                    let Ok((r, obs)) = self.open_region(state) else {
                        return Ok(None);
                    };
                    self.block_tail(peer, &hash, outcome, tail, &obs.snap);
                    return Ok(Some(r));
                }
            }
        }
    }

    /// Core's `ProcessMessage`: the gates, then the command's branch or the extension dispatch
    /// (P2a §3.2.1). `Err` is a read that threw.
    // Core: net_processing.cpp:3918-5831 PeerManagerImpl::ProcessMessage
    fn process_message(
        &mut self,
        peer: &PeerHandle,
        st: &mut PeerState,
        msg: &RawMsg,
        snap: &Arc<ChainSnapshot>,
    ) -> Result<BlockStep, Throw> {
        let cmd = msg.cmd;
        let id = peer.id();
        tracing::debug!(target: "net", "received: {} ({} bytes) peer={id}", cmd, msg.payload.len());

        let now = self.core.clock().node_now().0;
        let reject_txs = reject_incoming_txs(peer, self.opts.blocks_only);
        match gates::admit(peer, &cmd, now, reject_txs) {
            Gate::Admit => {}
            // `gates::admit` has logged Core's line (once, as Core does).
            Gate::RedundantVersion | Gate::BeforeVersion | Gate::BeforeVerack => {
                return Ok(BlockStep::Done);
            }
            Gate::ProbeWithoutMnauth => {
                self.core.disconnect(id, Why::Gate("masternode probe without MNAUTH"));
                return Ok(BlockStep::Done);
            }
            // Core: net_processing.cpp:4381-4386, protocol.cpp:185-213, :335-338 (P5a D23)
            Gate::BlocksOnlyViolation => {
                self.core.disconnect(id, Why::Gate("blocks-only violation"));
                return Ok(BlockStep::Done);
            }
        }

        self.admitted(peer, st, msg, snap)
    }

    /// `ProcessMessage` after the gates: the command's branch, the extension dispatch or the
    /// unknown-command line, for a message the gates admitted, in a region opened after a drain
    /// as [`PeerLogic::process_messages`] opens it. `sim` and the differential harness call it
    /// to drive a branch directly.
    pub fn process_admitted(&mut self, peer: &PeerHandle, msg: &RawMsg) -> Result<(), Throw> {
        let Some(state) = self.state_of(peer.id()) else {
            return Ok(());
        };
        let Ok((region, obs)) = self.open_region(&state) else {
            return Ok(());
        };
        self.run_message(peer, region, msg, obs, false).map(drop)
    }

    fn admitted(
        &mut self,
        peer: &PeerHandle,
        st: &mut PeerState,
        msg: &RawMsg,
        snap: &Arc<ChainSnapshot>,
    ) -> Result<BlockStep, Throw> {
        let cmd = msg.cmd;
        let payload = &msg.payload[..];
        // R8: one snapshot for the message, the message-start drain's (I0); every read's context
        // is built from it now.
        let chain = Arc::clone(&self.chain);
        let core = Arc::clone(&self.core);
        let signals = Arc::clone(&self.signals);
        let snap = Arc::clone(snap);
        let ctx = ProtoCtx::new(peer.common_version(), snap.bls);
        let now_us = core.clock().node_now_micros();
        if !matches!(
            cmd.as_str(),
            "version"
                | "verack"
                | "sendaddrv2"
                | "sendtxrcncl"
                | "ping"
                | "pong"
                | "addr"
                | "addrv2"
        ) {
            let mut cx = HandlerCx {
                core: &core,
                peer,
                state: st,
                chain: &*chain,
                snap,
                ctx,
                now_us,
                signals: &signals,
                requests: Arc::clone(&self.object_request),
                post: Vec::new(),
            };
            let step = self.inline_or_dispatch(&mut cx, &cmd, payload);
            // A handler that throws returns no `MessageProcessingResult` (P5a D13).
            if step.is_err() {
                cx.post.clear();
            }
            self.post_process(&mut cx);
            return step;
        }
        match self.net_branch(&core, &*chain, peer, st, msg, now_us) {
            Step::Throw(t) => Err(t),
            Step::Done | Step::Ignored | Step::Disconnect => Ok(BlockStep::Done),
        }
    }

    /// The branches SB6 implements: the handshake, `ping`/`pong` and address intake, with the
    /// processor's state at Core's points.
    fn net_branch(
        &mut self,
        core: &NetCore,
        chain: &dyn ChainAccess,
        peer: &PeerHandle,
        st: &mut PeerState,
        msg: &RawMsg,
        now_us: i64,
    ) -> Step {
        let payload = &msg.payload[..];
        let id = peer.id();
        let our = st.our_services;
        let (mut host, net) = Host::split(st, &mut self.counters, self.opts.blocks_only);
        match msg.cmd.as_str() {
            "version" => {
                let inputs = version_inputs(core, chain, peer, our, self.opts.blocks_only);
                handshake::on_version(core, peer, &self.identity, &inputs, net, payload, &mut host)
            }
            "verack" => handshake::on_verack(core, peer, &self.identity, net, &mut host),
            "sendaddrv2" => handshake::on_sendaddrv2(core, peer, net),
            // Core: net_processing.cpp:4310-4314 (txreconciliation is off)
            "sendtxrcncl" => {
                tracing::debug!(target: "net", "sendtxrcncl from peer={id} ignored, as our node does not have txreconciliation enabled");
                Step::Done
            }
            "ping" => ping::on_ping(core, peer, payload),
            "pong" => ping::on_pong(peer, &mut net.ping, payload, msg.recv_us),
            _ => {
                let v2 = msg.cmd.as_str() == "addrv2";
                let intake = addr::on_addr(core, peer, &mut net.addr, payload, v2, now_us);
                if let Some((score, why)) = intake.misbehaving {
                    misbehavior::misbehaving(id, host.misbehavior, score, &why);
                }
                if !intake.relay.is_empty() {
                    self.seams.addr_relay.relay(core, &self.peers, id, &intake.relay);
                }
                intake.step.unwrap_or(Step::Done)
            }
        }
    }

    /// The inline branches that read through the handler context, then the extension dispatch
    /// and the unknown-command line.
    fn inline_or_dispatch(
        &mut self,
        cx: &mut HandlerCx<'_>,
        cmd: &CommandString,
        payload: &[u8],
    ) -> Result<BlockStep, Throw> {
        let mut data = payload;
        let id = cx.id();
        match cmd.as_str() {
            // Core: net_processing.cpp:4261-4271
            "sendheaders" => cx.state.prefer_headers = true,
            "sendheaders2" => cx.state.prefer_headers_compressed = true,
            // Core: net_processing.cpp:4273-4288
            "sendcmpct" => {
                // Core reads `sendcmpct_hb` as any non-zero byte (`serialize.h:238-239`); SB6's
                // typed read does too (D39).
                let m: SendCmpct = cx.read(&mut data)?;
                if m.version == CMPCTBLOCKS_VERSION {
                    cx.state.provides_cmpctblocks = true;
                    cx.state.headers_mut().requested_hb_cmpctblocks = m.high_bandwidth;
                    cx.peer.set_bip152_highbandwidth_from(m.high_bandwidth);
                }
            }
            // Core: net_processing.cpp:4485-4497
            "senddsq" => {
                let m: SendDsq = cx.read(&mut data)?;
                cx.state.wants_dsq = if !m.send_dsq.0 {
                    WantsDsq::None
                } else if cx.peer.common_version().0 < ProtocolVersion::DSQ_INV.0 {
                    WantsDsq::All
                } else {
                    WantsDsq::Inv
                };
            }
            // Core: net_processing.cpp:4500-4505
            "qsendrecsigs" => {
                let m: QSendRecSigs = cx.read(&mut data)?;
                cx.state.wants_recsigs = m.send_rec_sigs.0;
            }
            // Core: net_processing.cpp:4507-4514
            "inv" => {
                let m: Inv = cx.read(&mut data)?;
                if m.inventory.len() > MAX_INV_SZ {
                    cx.misbehaving(20, &format!("inv message size = {}", m.inventory.len()));
                    return Ok(BlockStep::Done);
                }
                self.inv_entries(cx, &m.inventory);
            }
            // Core: net_processing.cpp:4613-4634
            "getdata" => {
                let m: GetData = cx.read(&mut data)?;
                let n = m.inventory.len();
                if n > MAX_INV_SZ {
                    cx.misbehaving(20, &format!("getdata message size = {n}"));
                    return Ok(BlockStep::Done);
                }
                tracing::debug!(target: "net", "received getdata ({n} invsz) peer={id}");
                if let Some(first) = m.inventory.first() {
                    tracing::debug!(target: "net", "received getdata for: {first} peer={id}");
                }
                cx.state.getdata_requests.extend(m.inventory);
                let (core, peer) = (cx.core, cx.peer);
                self.process_getdata_with(core, peer, cx.state);
            }
            // Core: net_processing.cpp:5744-5765
            "notfound" => {
                let m: NotFound = cx.read(&mut data)?;
                if m.inventory.len() > NOTFOUND_LIMIT {
                    cx.misbehaving(20, &format!("notfound message size = {}", m.inventory.len()));
                    return Ok(BlockStep::Done);
                }
                // Core takes the tracker's lock once for the message (np:5757).
                let mut tracker = self.object_request.lock();
                for inv in m.inventory.iter().filter(|i| i.inv_type.is_known()) {
                    tracker.received_response(id, inv);
                    self.seams.objects.received_response(id, inv);
                }
            }
            // Core: net_processing.cpp:5389-5424
            "getaddr" => {
                if !cx.peer.is_inbound() {
                    tracing::debug!(target: "net", "Ignoring \"getaddr\" from {} connection. peer={id}", cx.peer.conn_type().as_str());
                } else {
                    self.seams.addr_relay.on_getaddr(cx);
                }
            }
            // Core: net_processing.cpp:5779-5819
            "clsig" => {
                self.extensions_before_clsig(cx, cmd, payload)?;
                self.on_clsig(cx, payload)?;
            }
            // Core: net_processing.cpp:5309-5346
            "headers" | "headers2" => return self.on_headers(cx, cmd, payload),
            // Core: net_processing.cpp:5348-5387
            "block" => return self.on_block(cx, payload),
            // Core: net_processing.cpp:5052-5293
            "cmpctblock" => return self.on_cmpctblock(cx, payload),
            // Core: net_processing.cpp:5295-5307
            "blocktxn" => return self.on_blocktxn(cx, payload),
            _ => {
                if let Some(r) = self.registry.dispatch(cx, cmd, payload) {
                    return r.map(|()| BlockStep::Done);
                }
                debug_assert!(row_of(cmd).is_none(), "{cmd:?} has no branch");
                // Core: net_processing.cpp:5828
                tracing::debug!(target: "net", "Unknown command \"{}\" from peer={id}", sanitize(cmd));
            }
        }
        Ok(BlockStep::Done)
    }

    /// The per-entry part of the `inv` branch, then the block-inv `getheaders` trigger.
    // Core: net_processing.cpp:4516-4608
    fn inv_entries(&mut self, cx: &mut HandlerCx<'_>, invs: &[Inventory]) {
        let id = cx.id();
        let reject_tx_invs = reject_incoming_txs(cx.peer, self.opts.blocks_only);
        let mut best_block = None;
        for inv in invs {
            let Some(inv_cmd) = inv.inv_type.command() else {
                tracing::debug!(target: "net", "got inv of unknown type {}: {} peer={id}", inv.inv_type.to_base(), inv.hash);
                continue;
            };
            if self.interrupted() {
                return;
            }
            if inv.inv_type == InvType::Block {
                if let Some(hash) = self.inv_block_entry(cx, inv) {
                    best_block = Some(hash);
                }
                continue;
            }
            // Core: net_processing.cpp:4547-4551 (P5a D23)
            if reject_tx_invs && policy::violates_blocks_only(&inv_cmd) {
                tracing::debug!(target: "net", "{inv_cmd} ({}) inv sent in violation of protocol, disconnecting peer={id}", inv.hash);
                cx.disconnect("inv sent in violation of protocol");
                return;
            }
            let already_have = self.already_have(cx.core, &cx.snap, inv);
            tracing::debug!(target: "net", "got inv: {inv}  {} peer={id}", if already_have { "have" } else { "new" });
            cx.state.inv_relay.lock().add_known(&inv.hash.to_lendian());
            if already_have {
                continue;
            }
            // Core: net_processing.cpp:4563-4573: ignored up to ADDRV2_PROTO_VERSION (P5a D23)
            if reject_tx_invs && inv.inv_type == InvType::IsdLock {
                if cx.peer.common_version().0 <= ProtocolVersion::ADDRV2.0 {
                    continue;
                }
                tracing::debug!(target: "net", "{inv_cmd} ({}) inv sent in violation of protocol, disconnecting peer={id}", inv.hash);
                cx.disconnect("inv sent in violation of protocol");
                return;
            }
            // Only sporks are requested in IBD.
            let now = cx.core.clock().node_now();
            if inv.inv_type == InvType::Spork || !cx.chain.is_initial_block_download(&cx.snap, now)
            {
                self.add_object_announcement(cx.state, inv, cx.now_us);
                self.seams.objects.add_object_announcement(id, inv, cx.now_us);
            }
        }
        if let Some(best) = best_block {
            self.inv_trigger_getheaders(cx, best);
        }
    }

    /// Core's `AlreadyHave`. The transaction types first reset `m_recent_rejects` when the tip
    /// has moved since the last reset, which redraws the filter's tweak; M2 keeps the draw and
    /// the tip, not the filter, which nothing fills before transaction relay (D69). The rest
    /// follows Core's switch: `MSG_CLSIG` to the ChainLocks seam, governance and `MSG_ISDLOCK`
    /// to their managers' state first, the Dash types to the handlers, and what Core does not
    /// know is had (P5a D20). A type whose manager is not ported is not had: Core's managers
    /// know nothing a node without them would.
    // Core: net_processing.cpp:2351-2366; common/bloom.cpp:385-391 CRollingBloomFilter::reset
    fn already_have(&mut self, core: &NetCore, snap: &ChainSnapshot, inv: &Inventory) -> bool {
        let tx = matches!(inv.inv_type, InvType::Tx | InvType::Dstx | InvType::AssetUnlock);
        let tip = snap.tip.tip_hash;
        if tx && tip != self.counters.recent_rejects_chain_tip {
            self.counters.recent_rejects_chain_tip = tip;
            // `nTweak = GetRand<unsigned int>()`.
            core.get_rand(u64::from(u32::MAX));
        }
        match inv.inv_type {
            // Core: net_processing.cpp:2438-2439
            InvType::ClSig => self.seams.chainlocks.already_have(&inv.hash.to_lendian()),
            // `NetGovernance::AlreadyHave` is `!ConfirmInventoryRequest`, false until governance
            // is valid and mnsync's blockchain is synced.
            // Core: net_processing.cpp:2426-2431; governance/net_governance.cpp:273-282;
            // governance/governance.cpp:612-613
            InvType::GovernanceObject | InvType::GovernanceObjectVote
                if !self.seams.overlay.governance_ready() =>
            {
                true
            }
            // `CInstantSendManager::AlreadyHave` is true while InstantSend is off.
            // Core: net_processing.cpp:2441-2442; instantsend/instantsend.cpp:352-356
            InvType::IsdLock if !self.seams.overlay.instantsend_enabled() => true,
            InvType::Tx
            | InvType::Dstx
            | InvType::AssetUnlock
            | InvType::Spork
            | InvType::GovernanceObject
            | InvType::GovernanceObjectVote
            | InvType::QuorumFinalCommitment
            | InvType::QuorumRecoveredSig
            | InvType::IsdLock
            | InvType::PlatformBan
            | InvType::QuorumContrib
            | InvType::QuorumComplaint
            | InvType::QuorumJustification
            | InvType::QuorumPrematureCommitment
            | InvType::Dsq => self.registry.already_have(inv),
            // "Don't know what it is, just say we already got one": `MSG_FILTERED_BLOCK`,
            // `MSG_CMPCT_BLOCK` (P5a D20).
            // Core: net_processing.cpp:2458-2459
            _ => true,
        }
    }

    /// Core's `AddObjectAnnouncement`: the tracker's parameters for an announcement from `st`.
    /// The non-preferred delay is for `MSG_TX` only and only without a masternode manager, which
    /// a following node never has (P5a D9).
    // Core: net_processing.cpp:1635-1664 PeerManagerImpl::AddObjectAnnouncement
    fn add_object_announcement(&mut self, st: &PeerState, inv: &Inventory, now_us: i64) {
        let mut tracker = self.object_request.lock();
        if tracker.count(st.id) >= MAX_PEER_OBJECT_ANNOUNCEMENTS {
            return;
        }
        let preferred = st.preferred_download;
        let mut delay = 0;
        if inv.inv_type == InvType::Tx && !preferred {
            delay += NONPREF_PEER_TX_DELAY_US;
        }
        if tracker.count_in_flight(st.id) >= MAX_PEER_OBJECT_REQUEST_IN_FLIGHT {
            delay += OVERLOADED_PEER_OBJECT_DELAY_US;
        }
        tracker.received_inv(st.id, inv, preferred, now_us + delay);
    }

    /// Core's `PostProcessMessage` for each result the handler queued, in order (P5a D13).
    // Core: net_processing.cpp:3819-3836 PeerManagerImpl::PostProcessMessage
    fn post_process(&mut self, cx: &mut HandlerCx<'_>) {
        for result in std::mem::take(&mut cx.post) {
            self.apply_result(cx.state, result);
        }
    }

    fn apply_result(&mut self, st: &PeerState, result: MessageProcessingResult) {
        if let Some(e) = result.error {
            misbehavior::misbehaving(st.id, &st.misbehavior, e.score, &e.message);
        }
        if let Some(inv) = result.to_erase {
            self.object_request.lock().received_response(st.id, &inv);
        }
        for inv in &result.inventory {
            // An inv being relayed is available locally: nobody need be asked for it any more.
            self.object_request.lock().forget_tx_hash(inv);
            relay::relay_inv(&self.peers, inv);
        }
    }

    /// Core's extensions, which see every known command before the `CLSIG` branch, each result
    /// applied as it returns (P5a D21). The other commands reach only their owner: an extension
    /// returns at once for a command not its own (np:3841, blockprocessor.cpp:101-103,
    /// evo/mnauth.cpp:57).
    // Core: net_processing.cpp:5779-5792
    fn extensions_before_clsig(
        &mut self,
        cx: &mut HandlerCx<'_>,
        cmd: &CommandString,
        payload: &[u8],
    ) -> Result<(), Throw> {
        for i in 0..self.registry.extension_count() {
            self.registry.dispatch_extension(i, cx, cmd, payload)?;
            self.post_process(cx);
        }
        Ok(())
    }

    /// Core's `CLSIG` branch: nothing while ChainLocks are off; otherwise the read, the hash, the
    /// solicitation check (P2P-12, PRD 23) and LK1a's `ProcessNewChainLock` (P5a D3-D6, D16).
    // Core: net_processing.cpp:5794-5819
    fn on_clsig(&mut self, cx: &mut HandlerCx<'_>, payload: &[u8]) -> Result<(), Throw> {
        if !self.seams.chainlocks.is_enabled() {
            return Ok(());
        }
        let mut data = payload;
        let clsig: ClSig = cx.read(&mut data)?;
        // Core's eager read throws on a signature canonical in neither scheme (quirk 38).
        // Core: bls/bls.h:196
        let Some(hash) = clsig_hash(&clsig, ClsigHashSchemes::both(cx.ctx.bls)) else {
            return Err(Throw::ios_failure("malleable BLS object"));
        };
        let inv = Inventory {
            inv_type: InvType::ClSig,
            hash: Hash256::from_lendian(hash),
        };
        let id = cx.id();
        if cx.consume_getdata_response(&inv) == GetDataResponse::Unrequested {
            tracing::debug!(target: "chainlocks", "CLSIG -- received unrequested CLSIG {}, peer={id}", inv.hash);
            cx.misbehaving(UNREQUESTED_OBJECT_MISBEHAVIOR_SCORE, "unrequested clsig");
            return Ok(());
        }
        let result = self.seams.chainlocks.process_new_chainlock(id, &clsig, &hash);
        cx.post_process(result);
        Ok(())
    }

    fn process_getdata(&mut self, peer: &PeerHandle, st: &mut PeerState) {
        let core = Arc::clone(&self.core);
        self.process_getdata_with(&core, peer, st);
    }

    /// Core's `ProcessGetData`: known non-block types are served by the handlers or answered
    /// `notfound`; one block request per call goes to `ProcessGetBlockData` (P4b), which never
    /// answers `notfound`; an unknown type ends the batch and is dropped on the next call.
    // Core: net_processing.cpp:2935-3111 PeerManagerImpl::ProcessGetData
    fn process_getdata_with(&mut self, core: &NetCore, peer: &PeerHandle, st: &mut PeerState) {
        let signals = Arc::clone(&self.signals);
        let mut not_found = Vec::new();
        let mut taken = 0;
        while let Some(inv) = st.getdata_requests.get(taken).copied() {
            let Some(inv_cmd) = inv.inv_type.command() else {
                break;
            };
            if self.interrupted() {
                return;
            }
            if peer.pause_send() || inv.inv_type.is_gen_blk() {
                break;
            }
            taken += 1;
            if !st.tx_relay && policy::violates_blocks_only(&inv_cmd) {
                continue;
            }
            let chain = Arc::clone(&self.chain);
            let requests = Arc::clone(&self.object_request);
            let mut cx = getdata_cx(core, peer, st, &*chain, &signals, requests);
            let pushed = self.process_getdata_inv(&mut cx, &inv);
            self.post_process(&mut cx);
            if !pushed {
                not_found.push(inv);
            }
        }
        // Only one block item per call, since they're uncommon and can be expensive to process;
        // an unknown type is erased unanswered, and the queue goes on at the next call.
        if let Some(inv) = st.getdata_requests.get(taken).copied()
            && !peer.pause_send()
        {
            taken += 1;
            if inv.inv_type.is_gen_blk() {
                let requests = Arc::clone(&self.object_request);
                let mut cx = getdata_cx(core, peer, st, &*self.chain, &signals, requests);
                process_get_block_data(&mut cx, &inv, &*self.seams.islocks);
            }
        }
        st.getdata_requests.drain(..taken);
        if !not_found.is_empty() {
            let msg = out_msg(
                CommandString::NOTFOUND,
                &NotFound {
                    inventory: not_found,
                },
            );
            core.push(peer, msg);
        }
    }

    /// One non-block entry of `ProcessGetData`: the handlers in Core's order, then `MSG_CLSIG`
    /// from LK1a's store. Whether it pushed.
    // Core: net_processing.cpp:3017-3047
    fn process_getdata_inv(&mut self, cx: &mut HandlerCx<'_>, inv: &Inventory) -> bool {
        if self.registry.process_getdata(cx, inv) {
            return true;
        }
        if inv.inv_type != InvType::ClSig {
            return false;
        }
        match self.seams.chainlocks.chainlock_by_hash(&inv.hash.to_lendian()) {
            Some(clsig) => {
                cx.push(out_msg(CommandString::CLSIG, &clsig));
                true
            }
            None => false,
        }
    }

    /// Core's `SendMessages` section 13: the tracker's requestable announcements, less those
    /// `AlreadyHave` now covers, appended to the batch section 12 started (P5a D8).
    // Core: net_processing.cpp:6841-6892
    fn object_getdata_section(
        &mut self,
        st: &mut PeerState,
        snap: &ChainSnapshot,
        batch: &mut GetDataBatch<'_>,
        now_us: i64,
    ) {
        let id = st.id;
        let mut expired = Vec::new();
        let requestable = self.object_request.lock().get_requestable(id, now_us, &mut expired);
        for (peer, inv) in &expired {
            tracing::debug!(target: "net", "timeout of inflight object {inv} from peer={peer}");
        }
        let core = Arc::clone(&self.core);
        let mut requested = Vec::new();
        let mut already_have = Vec::new();
        for inv in requestable {
            if self.already_have(&core, snap, &inv) {
                already_have.push(inv);
                continue;
            }
            tracing::debug!(target: "net", "Requesting {inv} peer={id}");
            batch.push(inv);
            requested.push(inv);
            st.recent_object_requests.record(&inv, now_us);
        }
        let mut tracker = self.object_request.lock();
        for inv in &requested {
            tracker.requested_tx(id, inv, now_us + object_interval_us(inv.inv_type));
        }
        for inv in &already_have {
            tracker.forget_tx_hash(inv);
        }
    }

    /// Core's `SendMessages`: every section on every pass, in Core's order (P2a §3.1.4;
    /// DEC-56).
    // Core: net_processing.cpp:6300-6899 PeerManagerImpl::SendMessages
    pub fn send_messages(&mut self, peer: &PeerHandle) {
        let id = peer.id();
        let Some(slot) = self.peers.lock().get(&id).cloned() else {
            return;
        };
        // The head (P3 §3.6 point 4, DEC-236, D44): under the `block_inv` cell, the snapshot S
        // and the move of the headers-relay list are one observation; then the drain to S's
        // `view_seq`, and the region, whose `recent` copy is taken under the cell before the
        // stamp read (D58). An `Ahead` retry keeps the moved list.
        let (relay, obs) = {
            #[cfg(feature = "sim")]
            self.signals.on_lock(super::peerman::LockPoint::HeadEntry(id));
            let mut inv = slot.block_inv.lock();
            #[cfg(feature = "sim")]
            self.signals.on_lock(super::peerman::LockPoint::Head(id));
            let (snap, seq) = self.chain.drain_snapshot();
            (
                std::mem::take(&mut inv.headers_relay),
                Observation {
                    snap,
                    seq,
                },
            )
        };
        self.drain_at(&obs);
        let Ok((mut st, obs, recent)) = self.enter_region(slot.state.lock(), obs, Signals::recent)
        else {
            return;
        };
        let snap = obs.snap;
        let core = &*self.core;

        // 1.
        if misbehavior::maybe_discourage_and_disconnect(core, peer, &st) {
            return;
        }
        // 2.
        if !peer.successfully_connected() || peer.disconnect_requested() {
            return;
        }
        let now_us = core.clock().node_now_micros();
        // 3.
        if peer.conn_type() == ConnectionType::AddrFetch
            && now_us - peer.connected() * 1_000_000 > 10 * AVG_ADDRESS_BROADCAST_INTERVAL_US
        {
            tracing::debug!(target: "net", "addrfetch connection timeout; disconnecting peer={id}");
            core.disconnect(id, Why::Processor("addrfetch connection timeout"));
            return;
        }
        // 4.
        ping::maybe_send_ping(core, peer, &mut st.net.ping, now_us);
        if peer.disconnect_requested() {
            return;
        }
        // 5.
        let our_services = st.our_services;
        self.seams.addr_relay.send_section(core, peer, &mut st.net.addr, our_services, now_us);
        // Sections 6-12 run under one `cs_main` in Core: the head's snapshot for all of them.
        // 6.
        let sync_from_peer = self.start_sync_section(peer, &mut st, &snap);
        // 7.
        self.announce_section(peer, &mut st, &snap, &relay, &recent);
        // 8.
        self.inv_section(peer, &mut st, now_us);
        // 9.
        let mut cx = SyncCx::new(&self.core, peer, &mut st, &*self.chain, &snap);
        if self.seams.block_sync.stall_section(&mut cx, now_us) {
            return;
        }
        // 10.
        if self.headers_timeout_section(peer, &mut st, &snap) {
            return;
        }
        // 11. `GetTime()` seconds, mockable.
        let now = self.core.clock().node_now().0;
        self.consider_eviction(peer, &mut st, &snap, now);
        // 12.
        let core = Arc::clone(&self.core);
        let mut batch = GetDataBatch::new(&core, peer);
        let mut cx = SyncCx::new(&core, peer, &mut st, &*self.chain, &snap);
        self.seams.block_sync.getdata_section(&mut cx, &mut batch, sync_from_peer, now_us);
        // 13.
        self.object_getdata_section(&mut st, &snap, &mut batch, now_us);
        self.seams.objects.send_section(&mut batch, now_us);
        batch.finish();
    }

    /// Delivers one drained `BlockChecked` to the block-sync seam at the drain's snapshot (D65,
    /// P3 §3.7).
    fn block_checked(
        &mut self,
        hash: &BlockHash,
        invalid: Option<&BlockValidationError>,
        ibd: bool,
        snap: &ChainSnapshot,
    ) {
        let mut cx = BlockCheckedCx {
            core: &self.core,
            chain: &*self.chain,
            peers: &self.peers,
            snap,
        };
        self.seams.block_sync.block_checked(&mut cx, hash, invalid, ibd);
    }

    /// Core's `NetHandler::Interrupt`.
    pub fn interrupt_handlers(&mut self) {
        self.registry.interrupt();
    }

    /// Core's `NetHandler::Stop`.
    pub fn stop_handlers(&mut self) {
        self.registry.stop();
    }
}

/// Core's `SanitizeString(msg_type)` for a log line.
fn sanitize(cmd: &CommandString) -> String {
    cmd.as_bytes()
        .iter()
        .take_while(|&&b| b != 0)
        .filter(|&&b| b.is_ascii_alphanumeric() || b" .,;-_/:?@()".contains(&b))
        .map(|&b| char::from(b))
        .collect()
}
