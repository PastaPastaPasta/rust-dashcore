//! A peer's processor state: Core's `Peer` and `CNodeState` in one struct (P2a §2.3, §3.4.1).
//!
//! The net-level fields (version, services, flags read by the network core) live on SB6's
//! [`dash_p2p::Peer`]; the fields SB6's handshake, ping and addr functions own are embedded as
//! [`PeerNet`]. Nothing outside `processing` mutates this state.

use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::Arc;

use arrayvec::ArrayVec;
use dash_consensus::BlockId;
use dash_p2p::handshake::PeerNet;
use dash_p2p::{PeerId, ServiceFlags};
use dash_p2p_core::Inventory;
use dash_primitives::BlockHash;
use parking_lot::{Condvar, Mutex};

use super::bloom::BloomFilter;
use super::download::HeightsCell;
use super::headers::MAX_BLOCKS_TO_ANNOUNCE;
use super::misbehavior::MisbehaviorCell;
use super::relay::{InvRelay, InvRelayCell};
use super::solicit::RecentObjectRequests;

/// `m_blocks_for_headers_relay`, compressed without changing section 7's result (P3 D44):
/// section 7 announces by headers only while the list holds at most [`MAX_BLOCKS_TO_ANNOUNCE`]
/// hashes, and past that it only reverts to an inv of the last one (np:6412-6414,
/// np:6531-6550), so only the last is kept then. The short form stays inline: the list never
/// allocates.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
pub enum RelayList {
    /// At most [`MAX_BLOCKS_TO_ANNOUNCE`] hashes, in append order.
    Short(ArrayVec<BlockHash, MAX_BLOCKS_TO_ANNOUNCE>),
    /// More were appended; `last` is the newest.
    Overflow {
        /// The last hash appended.
        last: BlockHash,
    },
}

impl Default for RelayList {
    fn default() -> Self {
        Self::Short(ArrayVec::new())
    }
}

impl RelayList {
    /// Core's `push_back`.
    pub fn push(&mut self, hash: BlockHash) {
        match self {
            Self::Short(v) if !v.is_full() => v.push(hash),
            _ => {
                *self = Self::Overflow {
                    last: hash,
                }
            }
        }
    }

    /// Whether nothing is queued.
    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Short(v) if v.is_empty())
    }

    /// The number of hashes Core's vector would hold, capped: [`MAX_BLOCKS_TO_ANNOUNCE`] + 1
    /// stands for every longer list, which section 7 treats alike.
    pub fn len_capped(&self) -> usize {
        match self {
            Self::Short(v) => v.len(),
            Self::Overflow {
                ..
            } => MAX_BLOCKS_TO_ANNOUNCE + 1,
        }
    }

    /// The hashes section 7 reads: all of them while short, else only the last (`back()`).
    pub fn as_slice(&self) -> &[BlockHash] {
        match self {
            Self::Short(v) => v,
            Self::Overflow {
                last,
            } => std::slice::from_ref(last),
        }
    }
}

/// The blocks queued for announcement (Core's `Peer::m_blocks_for_*_relay` under
/// `m_block_inv_mutex`).
#[derive(Debug, Default)]
pub struct BlockInv {
    /// `m_blocks_for_headers_relay`: `UpdatedBlockTip` appends; the head of `send_messages`
    /// moves it out (P3 DEC-236, D44).
    pub headers_relay: RelayList,
    /// `m_blocks_for_inv_relay`: section 7 and `getblocks` append, section 8 drains.
    pub inv_relay: Vec<BlockHash>,
    /// `m_continuation_block`: the last block of a full `getblocks` batch, whose request is
    /// answered with an `inv` of the tip (P4b D10).
    pub continuation: Option<BlockHash>,
}

/// The four `CNodeState` fields that `NewPoWValidBlock` reads or writes (np:2253-2264), in a
/// per-peer leaf cell (P3 D50). It is the peer's share of `cs_main`: V's visit takes it on V's
/// thread, and the actor holds it wherever `msghand` holds `cs_main` for the peer (P3 §3.6).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PeerHeaders {
    /// `m_requested_hb_cmpctblocks`.
    pub requested_hb_cmpctblocks: bool,
    /// `pindexBestKnownBlock`.
    pub best_known_block: Option<BlockId>,
    /// `hashLastUnknownBlock`.
    pub last_unknown_block: Option<BlockHash>,
    /// `pindexBestHeaderSent`.
    pub best_header_sent: Option<BlockId>,
}

/// [`PeerHeaders`] under its leaf lock. The peer map's `PeerSlot` and the peer's state hold the
/// same cell. Every reader but V takes it through `Shared::lock_headers`, which checks the
/// announce stamp (P3 D56), except the reads that use no snapshot: [`HeaderCell::peek`] and the
/// peek regions of `Shared::with_state` (tests and diagnostics; P3 D62).
///
/// The lock is a token: a [`HeaderGuard`] takes the value out of the cell and puts it back when
/// it drops, and a taker waits while the value is out. The guard owns the value and an `Arc` of
/// the cell, so it is `Send` and can sit in the peer's state, which the peer map shares across
/// threads, without `parking_lot`'s `send_guard` (which would make every guard in the workspace
/// `Send`; P3 D61).
#[derive(Clone)]
pub struct HeaderCell(Arc<CellInner>);

impl std::fmt::Debug for HeaderCell {
    /// The value when free, `<held>` while a guard has it (the slot is then empty, which is not
    /// an empty cell).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0.slot.try_lock().as_deref() {
            Some(Some(h)) => f.debug_tuple("HeaderCell").field(h).finish(),
            Some(None) => f.write_str("HeaderCell(<held>)"),
            None => f.write_str("HeaderCell(<locked>)"),
        }
    }
}

struct CellInner {
    /// `None` while a guard holds the value.
    slot: Mutex<Option<PeerHeaders>>,
    returned: Condvar,
}

impl Default for HeaderCell {
    fn default() -> Self {
        Self(Arc::new(CellInner {
            slot: Mutex::new(Some(PeerHeaders::default())),
            returned: Condvar::new(),
        }))
    }
}

impl HeaderCell {
    /// Waits until no guard holds the value, then runs `f` on the slot.
    fn when_free<R>(&self, f: impl FnOnce(&mut Option<PeerHeaders>) -> R) -> R {
        let mut slot = self.0.slot.lock();
        while slot.is_none() {
            self.0.returned.wait(&mut slot);
        }
        f(&mut slot)
    }

    /// A copy of the fields, taken as a lock would take them, with no stamp check: tests and
    /// diagnostics only.
    pub fn peek(&self) -> PeerHeaders {
        self.when_free(|s| s.expect("free"))
    }
}

thread_local! {
    /// How many header-cell guards this thread holds (P3 §3.6, R12).
    static HEADER_GUARDS: Cell<u32> = const { Cell::new(0) };
}

/// Panics if this thread holds a header-cell guard. Every call that sends to V checks it first:
/// V's `NewPoWValidBlock` visit may wait for a cell, so a holder that waited on V would deadlock
/// (P3 §3.8.1, R12). On in every build.
#[track_caller]
pub fn assert_no_header_cell() {
    let held = HEADER_GUARDS.with(Cell::get);
    assert_eq!(held, 0, "a header cell is held across a call that waits on V (P3 R12)");
}

/// A header-cell guard, holding the cell's value, counted in this thread's
/// [`assert_no_header_cell`] tally.
pub struct HeaderGuard {
    cell: HeaderCell,
    headers: PeerHeaders,
}

impl HeaderGuard {
    /// Locks `cell` (blocking; the caller checks the announce stamp afterwards).
    pub(crate) fn lock(cell: &HeaderCell) -> Self {
        let headers = cell.when_free(|s| s.take().expect("free"));
        HEADER_GUARDS.with(|n| n.set(n.get() + 1));
        Self {
            cell: cell.clone(),
            headers,
        }
    }
}

impl Drop for HeaderGuard {
    fn drop(&mut self) {
        *self.cell.0.slot.lock() = Some(self.headers);
        // Lockers and peekers wait on the same condition.
        self.cell.0.returned.notify_all();
        HEADER_GUARDS.with(|n| n.set(n.get() - 1));
    }
}

impl std::fmt::Debug for HeaderGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("HeaderGuard").field(&self.headers).finish()
    }
}

impl std::ops::Deref for HeaderGuard {
    type Target = PeerHeaders;
    fn deref(&self) -> &PeerHeaders {
        &self.headers
    }
}

impl std::ops::DerefMut for HeaderGuard {
    fn deref_mut(&mut self) -> &mut PeerHeaders {
        &mut self.headers
    }
}

/// [`BlockInv`] under its own leaf lock, so the chain side's `UpdatedBlockTip` queues an
/// announcement without the peer's state lock, which the actor may hold while it waits on the
/// chain.
pub type BlockInvCell = Arc<Mutex<BlockInv>>;

/// Core's `m_wants_dsq` (`senddsq`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WantsDsq {
    /// No `senddsq`, or `senddsq(false)`.
    #[default]
    None,
    /// Every DSQ as a message (common version below `DSQ_INV_VERSION`).
    All,
    /// DSQ announced by inv.
    Inv,
}

/// Core's `CNodeState::ChainSyncTimeoutState` (`ConsiderEviction`, P2a §3.9.1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChainSyncTimeoutState {
    /// `m_timeout` in seconds; 0 is unset.
    pub timeout: i64,
    /// `m_work_header`.
    pub work_header: Option<BlockId>,
    /// `m_sent_getheaders`.
    pub sent_getheaders: bool,
    /// `m_protect`.
    pub protect: bool,
}

/// One peer's processor state (Core's `Peer` ∪ `CNodeState`).
#[derive(Debug)]
pub struct PeerState {
    /// `m_id`.
    pub id: PeerId,
    /// `CNodeState::m_is_inbound`, kept until `FinalizeNode` (the HB list reads it for a peer
    /// that has left the connection table, P3 §3.5.2).
    pub inbound: bool,
    /// `m_our_services`: the services we offer this peer (`NODE_BLOOM` added for inbound peers
    /// with the bloom-filter permission).
    pub our_services: ServiceFlags,
    /// The fields SB6's handshake, ping and addr functions own.
    pub net: PeerNet,

    // ---- misbehaviour (Peer::m_misbehavior_*)
    /// The misbehaviour score and flag, under their own lock. The peer map's `PeerSlot` holds
    /// the same cell, so it is never replaced (read it through `Shared::misbehavior`).
    pub(crate) misbehavior: MisbehaviorCell,

    // ---- negotiation (CNodeState / Peer)
    /// `fPreferHeaders`.
    pub prefer_headers: bool,
    /// `fPreferHeadersCompressed`.
    pub prefer_headers_compressed: bool,
    /// `m_provides_cmpctblocks`.
    pub provides_cmpctblocks: bool,
    /// `m_wants_dsq`.
    pub wants_dsq: WantsDsq,
    /// `m_wants_recsigs`.
    pub wants_recsigs: bool,
    /// `fPreferredDownload`.
    pub preferred_download: bool,
    /// Core's `m_tx_relay` exists (`SetTxRelay`); P5 fills it.
    pub tx_relay: bool,
    /// `m_tx_relay->m_relay_txs`.
    pub relay_txs: bool,
    /// `m_tx_relay->m_bloom_filter`: set only on a peer with a tx relay (P4b D7).
    pub bloom: Option<BloomFilter>,
    /// `m_send_mempool` (the `mempool` stub).
    pub send_mempool: bool,

    // ---- headers sync (P2a §3.4.1)
    /// `fSyncStarted`.
    pub sync_started: bool,
    /// `m_headers_sync_timeout` in µs; `i64::MAX` is disarmed for good, 0 unset.
    pub headers_sync_timeout_us: i64,
    /// `m_last_getheaders_timestamp` in µs (`NodeClock`); 0 when none outstanding.
    pub last_getheaders_us: i64,
    /// `TxRelay::m_next_inv_send_time` in µs (D61).
    pub next_inv_send_us: i64,
    /// `nUnconnectingHeaders`.
    pub unconnecting_headers: i32,
    /// `m_inv_triggered_getheaders_before_sync`.
    pub inv_triggered_getheaders_before_sync: bool,
    /// `m_last_block_announcement` in seconds.
    pub last_block_announcement: i64,
    /// `m_chain_sync`.
    pub chain_sync: ChainSyncTimeoutState,
    /// `pindexLastCommonBlock` (P3 maintains it; `getpeerinfo`'s `common_height`).
    pub last_common_block: Option<BlockId>,
    /// The block announcements queue; the peer map's `PeerSlot` holds the same cell.
    pub(crate) block_inv: BlockInvCell,
    /// The known-inventory filter and `vInventoryOtherToSend`; the peer map's `PeerSlot` holds
    /// the same cell (P5a D10).
    pub(crate) inv_relay: InvRelayCell,
    /// The header cell; the peer map's `PeerSlot` holds the same cell (P3 D50).
    pub(crate) header_cell: HeaderCell,
    /// `vHeightInFlight`'s mirror, which the peer's block-download entry writes (P3 §2.7).
    pub heights_in_flight: HeightsCell,
    /// The header cell's guard while the actor is in one of this peer's regions (P3 §3.6):
    /// message handling, and the sections of `send_messages`. [`PeerState::headers`] reads
    /// through it.
    headers: Option<HeaderGuard>,

    /// `m_getdata_requests`: queued by `getdata`, served by `ProcessGetData` (P4/P5).
    pub getdata_requests: VecDeque<Inventory>,

    /// `m_getsporks_recvd` and `m_getsporks_last_response`: the sorted hashes of the active
    /// sporks the last `getsporks` reply sent, `None` before the first (SP0's handler).
    pub getsporks_last_response: Option<Vec<[u8; 32]>>,

    /// `m_recent_object_requests`: the GETDATA-only objects we asked this peer for (P4a D7).
    pub recent_object_requests: RecentObjectRequests,
}

impl PeerState {
    /// `(m_blocks_for_inv_relay, m_blocks_for_headers_relay)`, copied under their lock.
    pub fn blocks_for_relay(&self) -> (Vec<BlockHash>, RelayList) {
        let inv = self.block_inv.lock();
        (inv.inv_relay.clone(), inv.headers_relay.clone())
    }

    /// The header fields, inside one of this peer's regions.
    ///
    /// # Panics
    ///
    /// Outside a region: the cell is not held, so reading it here would race V's visit.
    #[track_caller]
    pub fn headers(&self) -> &PeerHeaders {
        self.headers.as_deref().expect("header cell not held (outside a region, P3 §3.6)")
    }

    /// The header fields for writing, inside one of this peer's regions.
    ///
    /// # Panics
    ///
    /// Outside a region, as [`PeerState::headers`].
    #[track_caller]
    pub fn headers_mut(&mut self) -> &mut PeerHeaders {
        self.headers.as_deref_mut().expect("header cell not held (outside a region, P3 §3.6)")
    }

    /// Starts a region with `guard`, the cell `Shared::lock_headers` locked.
    pub(crate) fn enter_region(&mut self, guard: HeaderGuard) {
        debug_assert!(self.headers.is_none(), "a region is already open");
        self.headers = Some(guard);
    }

    /// Ends the region: unlocks the header cell. A no-op outside a region.
    pub(crate) fn leave_region(&mut self) {
        self.headers = None;
    }

    /// The known-inventory filter's tweak, `InitializeNode`'s draw for it (P5a D10).
    pub fn set_known_filter_tweak(&mut self, tweak: u32) {
        self.inv_relay = Arc::new(Mutex::new(InvRelay::new(tweak)));
    }

    /// `(m_misbehavior_score, m_should_discourage)`.
    pub fn misbehavior(&self) -> (i32, bool) {
        let m = self.misbehavior.lock();
        (m.score, m.should_discourage)
    }

    /// A fresh state offering `our_services`, created at `now_us` (Core's `InitializeNode`:
    /// the `CNodeState` and `Peer` constructors, and the address token bucket's first stamp).
    /// Its known-inventory filter's tweak is 0 until [`PeerState::set_known_filter_tweak`].
    pub fn new(id: PeerId, our_services: ServiceFlags, now_us: i64) -> Self {
        Self {
            id,
            inbound: false,
            our_services,
            net: PeerNet::new(now_us),
            misbehavior: MisbehaviorCell::default(),
            prefer_headers: false,
            prefer_headers_compressed: false,
            provides_cmpctblocks: false,
            wants_dsq: WantsDsq::None,
            wants_recsigs: false,
            preferred_download: false,
            tx_relay: false,
            relay_txs: false,
            bloom: None,
            send_mempool: false,
            sync_started: false,
            headers_sync_timeout_us: 0,
            last_getheaders_us: 0,
            next_inv_send_us: 0,
            unconnecting_headers: 0,
            inv_triggered_getheaders_before_sync: false,
            last_block_announcement: 0,
            chain_sync: ChainSyncTimeoutState::default(),
            last_common_block: None,
            block_inv: BlockInvCell::default(),
            inv_relay: Arc::new(Mutex::new(InvRelay::new(0))),
            header_cell: HeaderCell::default(),
            heights_in_flight: HeightsCell::default(),
            headers: None,
            getdata_requests: VecDeque::new(),
            getsporks_last_response: None,
            recent_object_requests: RecentObjectRequests::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use dash_types::Numeric as _;
    use proptest::prelude::*;

    use super::*;

    /// What section 7 reads from `m_blocks_for_headers_relay` (np:6412-6414, np:6420-6550):
    /// `size() > 1`, `size() > MAX_BLOCKS_TO_ANNOUNCE`, `empty()`, `back()`, and the whole
    /// vector while it has at most `MAX_BLOCKS_TO_ANNOUNCE` hashes.
    fn projection(
        len: usize,
        hashes: &[BlockHash],
    ) -> (bool, bool, bool, Option<BlockHash>, Option<Vec<BlockHash>>) {
        let short = len <= MAX_BLOCKS_TO_ANNOUNCE;
        (
            len > 1,
            len > MAX_BLOCKS_TO_ANNOUNCE,
            len == 0,
            hashes.last().copied(),
            short.then(|| hashes.to_vec()),
        )
    }

    proptest! {
        #[test]
        fn section_7_reads_the_same_from_the_list_as_from_cores_vector(
            appends in proptest::collection::vec(any::<[u8; 32]>(), 0..3 * MAX_BLOCKS_TO_ANNOUNCE),
        ) {
            let mut list = RelayList::default();
            let mut core = Vec::new();
            for h in appends {
                let h = BlockHash::from_lendian(h);
                list.push(h);
                core.push(h);
                prop_assert_eq!(
                    projection(list.len_capped(), list.as_slice()),
                    projection(core.len(), &core),
                );
                prop_assert!(!list.is_empty());
            }
        }
    }
}
