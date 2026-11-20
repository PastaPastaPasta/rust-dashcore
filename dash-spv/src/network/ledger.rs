use std::collections::{BTreeMap, HashMap, HashSet};
use std::mem;
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant as SteadyInstant};

use parking_lot::Mutex as LedgerMutex;

pub use dash_clock::Clock;
use dashcore::network::message::NetworkMessage;
use dashcore::network::message_blockdata::Inventory;
use dashcore::network::message_filter::GetCFilters;
use dashcore::BlockHash;
use tokio::sync::{Mutex, Notify};
use tokio::time::Instant;

pub use crate::network::costs::{ChargeId, ResponseCosts};
use crate::network::peer::PeerHandle;
pub use crate::network::peer::PeerId;

const KICKING_ATTEMPTS: u32 = 2;
const HEDGE_AFTER: Duration = Duration::from_secs(2);
const MIN_IN_FLIGHT_PER_CLASS: usize = 4;
const MSG_CLASSES: usize = 5;
const DRAIN_PRIORITY: [Priority; MSG_CLASSES] =
    [Priority::Other, Priority::Blocks, Priority::CFilters, Priority::CfHeaders, Priority::Headers];

/// De-dup, timeout and retry key of a request.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum RequestKey {
    /// Locator's first hash.
    Headers(BlockHash),
    /// Stop hash.
    CfHeaders(BlockHash),
    /// Start height.
    CFilters(u32),
    MnListDiff(BlockHash),
    Block(BlockHash),
}

pub fn request_keys(msg: &NetworkMessage) -> Vec<RequestKey> {
    match msg {
        NetworkMessage::GetHeaders(m) | NetworkMessage::GetHeaders2(m) => {
            m.locator_hashes.first().map(|h| RequestKey::Headers(*h)).into_iter().collect()
        }
        NetworkMessage::GetCFHeaders(m) => vec![RequestKey::CfHeaders(m.stop_hash)],
        NetworkMessage::GetCFilters(m) => vec![RequestKey::CFilters(m.start_height)],
        NetworkMessage::GetMnListD(m) => vec![RequestKey::MnListDiff(m.block_hash)],
        NetworkMessage::GetData(inv) => inv
            .iter()
            .filter_map(|i| match i {
                Inventory::Block(h) => Some(RequestKey::Block(*h)),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Identifies the subscription which owns a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SubId(pub u64);

/// Identifies a logical request until its owner releases or cancels it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestId(pub u64);

/// Identifies one send to one connection, including a hedge send.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AttemptId(pub u64);

/// A member of a request the ledger tracks, and the key its response is matched by.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum LedgerKey {
    /// Locator's first hash.
    Headers(BlockHash),
    /// Stop hash.
    CfHeaders(BlockHash),
    /// Start height.
    CFilters(u32),
    MnListDiff {
        base: BlockHash,
        block: BlockHash,
    },
    /// The requested tip block.
    QrInfo(BlockHash),
    Block(BlockHash),
}

/// Returns the members of a request the ledger tracks, one per expected reply. A request
/// with no key is `Keyless`; a `getdata` naming anything but blocks is `Unsupported`, and
/// so is one naming a filtered or compact block, whose reply the ledger cannot match.
pub fn ledger_keys(msg: &NetworkMessage) -> Result<Vec<LedgerKey>, RequestError> {
    match msg {
        NetworkMessage::GetHeaders(m) | NetworkMessage::GetHeaders2(m) => m
            .locator_hashes
            .first()
            .map(|h| vec![LedgerKey::Headers(*h)])
            .ok_or(RequestError::Keyless),
        NetworkMessage::GetCFHeaders(m) => Ok(vec![LedgerKey::CfHeaders(m.stop_hash)]),
        NetworkMessage::GetCFilters(m) => Ok(vec![LedgerKey::CFilters(m.start_height)]),
        NetworkMessage::GetMnListD(m) => Ok(vec![LedgerKey::MnListDiff {
            base: m.base_block_hash,
            block: m.block_hash,
        }]),
        NetworkMessage::GetQRInfo(m) => Ok(vec![LedgerKey::QrInfo(m.block_request_hash)]),
        NetworkMessage::GetData(inv) => {
            let tracked = inv.iter().filter(|i| matches!(i, Inventory::Block(_))).count();
            let filtered = inv
                .iter()
                .any(|i| matches!(i, Inventory::FilteredBlock(_) | Inventory::CompactBlock(_)));
            if filtered || (tracked > 0 && tracked < inv.len()) {
                return Err(RequestError::Unsupported);
            }
            if tracked == 0 {
                return Err(RequestError::Keyless);
            }
            Ok(inv
                .iter()
                .filter_map(|i| match i {
                    Inventory::Block(h) => Some(LedgerKey::Block(*h)),
                    _ => None,
                })
                .collect())
        }
        _ => Err(RequestError::Keyless),
    }
}

/// Whether the ledger admits `msg`: exactly the requests `ledger_keys` keys.
#[cfg_attr(not(test), expect(dead_code, reason = "the router drives the ledger"))]
pub fn ledger_tracked_kind(msg: &NetworkMessage) -> bool {
    ledger_keys(msg).is_ok()
}

/// Whether the untracked send paths must refuse `msg`, because a reply to it could settle
/// a ledger slot. A superset of `ledger_tracked_kind`.
#[cfg_attr(not(test), expect(dead_code, reason = "the router drives the ledger"))]
pub fn untracked_refuses(msg: &NetworkMessage) -> bool {
    ledger_tracked_kind(msg) || is_getheaders_any(msg) || getdata_names_a_block(msg)
}

fn is_getheaders_any(msg: &NetworkMessage) -> bool {
    matches!(msg, NetworkMessage::GetHeaders(_) | NetworkMessage::GetHeaders2(_))
}

/// Core may answer a compact-block `getdata` with a full `block`, which is a ledger key.
fn getdata_names_a_block(msg: &NetworkMessage) -> bool {
    matches!(msg, NetworkMessage::GetData(inv) if inv.iter().any(|i| matches!(i, Inventory::Block(_) | Inventory::CompactBlock(_))))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailReason {
    Timeout,
    Disconnected,
    WriteFailed,
    NotFound,
    /// The stop filter arrived before the declared number of filters.
    Incomplete,
}

/// Why the ledger did not admit a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RequestError {
    #[error("the request has no key the ledger tracks")]
    Keyless,
    #[error(
        "the request mixes tracked and untracked inventory, asks for filtered blocks, or is a getcfilters without a filter count"
    )]
    Unsupported,
    #[error("no request credit is free; a `CreditAvailable` follows a release")]
    NoCredit,
    #[error("another subscription holds these keys")]
    KeyInUse,
    #[error("the request overlaps the keys of a request in play")]
    Overlap,
    #[error("the request names more than {MAX_MEMBERS} members")]
    TooManyMembers,
}

/// The owner-side credit pool the ledger admits requests against. It is never called with
/// the ledger lock held.
pub trait Credits: Send + Sync {
    /// Takes a credit if one is free; otherwise records that `owner` wants one. One step.
    fn acquire_or_want(&self, owner: SubId) -> bool;
    fn release(&self, owner: SubId);
    /// Hands a terminal failure to its owner; the credit returns when the owner reads it.
    fn failed(&self, owner: SubId, failure: RequestFailed);
}

enum CreditOp {
    Release(SubId),
    Failed(SubId, RequestFailed),
}

/// How a request's key set relates to the requests in play.
enum Admission {
    New,
    Existing(RequestId),
    Refused(RequestError),
}

/// Test points inside `request`.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct LedgerHooks {
    /// Runs after `acquire_or_want` refused `owner` and before `request` returns `NoCredit`.
    pub after_no_credit: Option<Box<dyn Fn(SubId) + Send + Sync>>,
}

/// Marks a response that answers a live request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tag {
    pub id: RequestId,
    pub attempt: AttemptId,
    pub member: LedgerKey,
    /// Whether this response leaves no member of the request pending.
    pub last: bool,
}

/// The one terminal outcome of a request that some member could not get.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestFailed {
    pub id: RequestId,
    pub failed: Vec<(LedgerKey, FailReason)>,
    pub arrived: Vec<LedgerKey>,
    pub sends: u8,
    pub last: FailReason,
}

/// What a response matched on its connection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Matched {
    /// Set when the response answers a pending member of a live request.
    pub tag: Option<Tag>,
    /// The record the response settles.
    pub settle: Option<ChargeId>,
    /// The record a stream filter's bytes accrue to.
    pub accrue: Option<ChargeId>,
}

/// Whether `send_if_live` pushed the request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendOutcome {
    Sent,
    /// The slot was resolved before the send; nothing was pushed.
    NotLive,
}

/// The latest deadline among a connection's sent, unresolved slots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Horizon {
    None,
    /// The connection is read-blocked, so its silence is not the peer's.
    Blocked,
    At(SteadyInstant),
}

/// What `expire` found past its deadline.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Expired {
    /// Connections blamed for an unanswered first or second send.
    pub culprits: HashSet<PeerId>,
    /// Connections that left an orphaned request unanswered past its deadline.
    pub unanswered: HashSet<PeerId>,
}

/// The connection view needed by both the legacy network and the future router.
pub trait RoutePeer {
    fn id(&self) -> PeerId;
    /// Whether SPV cannot process this connection's input for a local reason.
    fn blocked(&self) -> bool {
        false
    }
    fn in_flight_bytes(&self) -> u64;
    fn cap(&self) -> u64;
    fn serving_rate(&self) -> u64;
    fn queued_secs(&self) -> Option<f64> {
        match (self.in_flight_bytes(), self.serving_rate()) {
            (0, _) => Some(0.0),
            (_, 0) => None,
            (bytes, rate) => Some(bytes as f64 / rate as f64),
        }
    }
}

impl RoutePeer for PeerHandle {
    fn id(&self) -> PeerId {
        self.id()
    }
    fn in_flight_bytes(&self) -> u64 {
        self.in_flight_bytes()
    }
    fn cap(&self) -> u64 {
        self.cap()
    }
    fn serving_rate(&self) -> u64 {
        self.serving_rate()
    }
}

/// Sends of one request, counting the first, each retry and each hedge.
pub const MAX_SENDS: u8 = 3;
/// Most members one request may name.
pub const MAX_MEMBERS: usize = 16;
/// A `qrinfo` reply takes long to build, so each send of one waits longer than the last.
const QRINFO_TIMEOUTS: [Duration; 3] =
    [Duration::from_secs(10), Duration::from_secs(30), Duration::from_secs(60)];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlotState {
    Live,
    /// The request left or another send won; the slot only absorbs its own reply.
    Orphaned,
    /// The send timed out; the slot absorbs its late positional reply and expires silently.
    TimedOut,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Reserved,
    /// Carries the connection's send order.
    Sent(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlotKind {
    Headers,
    CFilters,
    QrInfo,
    Other,
}

struct SlotMember {
    key: LedgerKey,
    charge: Option<ChargeId>,
}

struct Stream {
    stop: BlockHash,
    declared: Option<u32>,
    received: u32,
}

/// One send to one connection, holding only its unresolved members.
struct Slot {
    attempt: AttemptId,
    id: RequestId,
    kind: SlotKind,
    state: SlotState,
    phase: Phase,
    members: Vec<SlotMember>,
    stream: Option<Stream>,
    timeout: Duration,
    /// When the slot was reserved, then when it was pushed.
    sent_at: SteadyInstant,
    deadline: SteadyInstant,
    last_progress: SteadyInstant,
    index: u8,
    hedge: bool,
    reserved_bytes: u64,
}

impl Slot {
    fn holds(&self, key: &LedgerKey) -> bool {
        self.members.iter().any(|m| &m.key == key)
    }

    fn live(&self) -> bool {
        self.state == SlotState::Live
    }
}

/// A local reason SPV cannot process a connection's input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(test), expect(dead_code, reason = "the router drives the ledger"))]
pub enum BlockReason {
    /// The hub parked this connection's next message.
    Park,
    /// The hub's global budget is exhausted.
    Budget,
}

impl BlockReason {
    fn bit(self) -> u8 {
        match self {
            BlockReason::Park => 1,
            BlockReason::Budget => 2,
        }
    }
}

#[derive(Default)]
struct Conn {
    slots: Vec<Slot>,
    next_seq: u64,
    reasons: u8,
    blocked_since: Option<SteadyInstant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MemberState {
    Pending,
    Arrived,
    Failed(FailReason),
}

struct Member {
    key: LedgerKey,
    state: MemberState,
    notfound_from: Option<PeerId>,
}

#[derive(PartialEq, Eq)]
enum LedgerState {
    Queued,
    OnWire,
    Arrived,
}

struct LedgerEntry {
    owner: SubId,
    members: Vec<Member>,
    msg: NetworkMessage,
    height: u32,
    class: Priority,
    state: LedgerState,
    sends: u8,
    tried: HashSet<PeerId>,
    filters: Option<u32>,
    hedged: bool,
    answered: bool,
    sequence: u64,
}

impl LedgerEntry {
    fn pending(&self) -> impl Iterator<Item = &LedgerKey> {
        self.members.iter().filter(|m| m.state == MemberState::Pending).map(|m| &m.key)
    }

    fn is_pending(&self, key: &LedgerKey) -> bool {
        self.members.iter().any(|m| &m.key == key && m.state == MemberState::Pending)
    }

    /// Whether `peer` said `notfound` for a member this request still needs.
    fn refused_by(&self, peer: PeerId) -> bool {
        self.members
            .iter()
            .any(|m| m.state == MemberState::Pending && m.notfound_from == Some(peer))
    }
}

/// Returns `msg` naming only `keys`. A range or a single-member request is sent whole.
fn message_for(msg: &NetworkMessage, keys: &[LedgerKey]) -> NetworkMessage {
    match msg {
        NetworkMessage::GetData(inv) => NetworkMessage::GetData(
            inv.iter()
                .filter(|item| match item {
                    Inventory::Block(hash) => keys.contains(&LedgerKey::Block(*hash)),
                    _ => true,
                })
                .copied()
                .collect(),
        ),
        msg => msg.clone(),
    }
}

#[derive(Default)]
struct LedgerInner {
    next_request: u64,
    next_attempt: u64,
    next_sequence: u64,
    entries: HashMap<RequestId, LedgerEntry>,
    by_key: HashMap<LedgerKey, RequestId>,
    conns: HashMap<PeerId, Conn>,
    released: Vec<(PeerId, ChargeId)>,
    credit_ops: Vec<CreditOp>,
    /// Reasons that block every connection.
    all_reasons: u8,
}

impl LedgerInner {
    fn blocked(&self, peer: PeerId) -> bool {
        self.all_reasons != 0 || self.conns.get(&peer).is_some_and(|conn| conn.reasons != 0)
    }

    /// Applies a change of block reasons to one connection. Leaving the blocked state
    /// moves every unresolved deadline later by the time spent blocked since its send.
    fn reblock(&mut self, peer: PeerId, change: impl FnOnce(&mut u8), now: SteadyInstant) {
        let all = self.all_reasons;
        let conn = self.conns.entry(peer).or_default();
        let was = conn.reasons | all != 0;
        change(&mut conn.reasons);
        let is = conn.reasons | all != 0;
        shift_on_change(conn, was, is, now);
    }

    fn reblock_all(&mut self, change: impl FnOnce(&mut u8), now: SteadyInstant) {
        let was_all = self.all_reasons;
        change(&mut self.all_reasons);
        let is_all = self.all_reasons;
        for conn in self.conns.values_mut() {
            shift_on_change(conn, conn.reasons | was_all != 0, conn.reasons | is_all != 0, now);
        }
    }

    /// Classifies a whole key set against the keys live requests hold.
    fn classify(&self, owner: SubId, keys: &[LedgerKey]) -> Admission {
        let holders: HashSet<_> = keys.iter().filter_map(|key| self.by_key.get(key)).collect();
        let Some(&&id) = holders.iter().next() else {
            return Admission::New;
        };
        let Some(entry) = self.entries.get(&id) else {
            return Admission::Refused(RequestError::Overlap);
        };
        let held = self.by_key.values().filter(|holder| **holder == id).count();
        if holders.len() > 1
            || held != keys.len()
            || keys.iter().any(|k| !self.by_key.contains_key(k))
        {
            return Admission::Refused(RequestError::Overlap);
        }
        if entry.owner == owner {
            Admission::Existing(id)
        } else {
            Admission::Refused(RequestError::KeyInUse)
        }
    }

    fn slot(&self, attempt: AttemptId) -> Option<(PeerId, &Slot)> {
        self.conns.iter().find_map(|(peer, conn)| {
            conn.slots.iter().find(|slot| slot.attempt == attempt).map(|slot| (*peer, slot))
        })
    }

    fn slot_mut(&mut self, attempt: AttemptId) -> Option<&mut Slot> {
        self.conns
            .values_mut()
            .find_map(|conn| conn.slots.iter_mut().find(|slot| slot.attempt == attempt))
    }

    fn live_slots(&self, id: RequestId) -> impl Iterator<Item = (PeerId, &Slot)> {
        self.conns.iter().flat_map(move |(peer, conn)| {
            conn.slots.iter().filter(move |slot| slot.id == id && slot.live()).map(|s| (*peer, s))
        })
    }

    /// Whether `peer` holds an unresolved slot naming any of `keys`.
    fn holds_any<'a>(&self, peer: PeerId, mut keys: impl Iterator<Item = &'a LedgerKey>) -> bool {
        self.conns
            .get(&peer)
            .is_some_and(|conn| keys.any(|key| conn.slots.iter().any(|slot| slot.holds(key))))
    }

    fn reserved_bytes(&self, peer: PeerId) -> u64 {
        self.conns.get(&peer).map_or(0, |conn| {
            conn.slots.iter().fold(0u64, |sum, slot| sum.saturating_add(slot.reserved_bytes))
        })
    }

    /// Removes the slot for `attempt` and releases the charges it still holds.
    fn take_slot(&mut self, attempt: AttemptId) -> Option<(PeerId, Slot)> {
        let (peer, pos) = self.conns.iter().find_map(|(peer, conn)| {
            conn.slots.iter().position(|slot| slot.attempt == attempt).map(|pos| (*peer, pos))
        })?;
        let slot = self.conns.get_mut(&peer)?.slots.remove(pos);
        self.release_slot(peer, &slot);
        Some((peer, slot))
    }

    fn release_slot(&mut self, peer: PeerId, slot: &Slot) {
        self.released.extend(slot.members.iter().filter_map(|m| m.charge.map(|id| (peer, id))));
    }
}

/// Pure request scheduling and response matching. All state transitions use one mutex.
/// A send is reserved by `route` or `hedge`, gets its charge records with `attach` and
/// reaches the wire only inside `Reservation::send_if_live`.
#[cfg_attr(not(test), expect(dead_code, reason = "the router drives the ledger"))]
pub struct RequestLedger {
    inner: LedgerMutex<LedgerInner>,
    clock: Arc<dyn Clock>,
    credits: Arc<dyn Credits>,
    #[cfg(test)]
    hooks: LedgerMutex<LedgerHooks>,
}

/// A reserved send with its charges attached. Dropping it unsent withdraws the send.
#[must_use]
pub struct Reservation<'a> {
    ledger: &'a RequestLedger,
    attempt: Option<AttemptId>,
}

#[cfg_attr(not(test), expect(dead_code, reason = "the router drives the ledger"))]
impl Reservation<'_> {
    /// Pushes the request if its slot is still unresolved, under the ledger lock. `push`
    /// gets the members still unresolved and the message naming them. The send's deadline
    /// starts here.
    pub fn send_if_live(
        mut self,
        timeout: Duration,
        push: impl FnOnce(&[LedgerKey], &NetworkMessage),
    ) -> SendOutcome {
        let Some(attempt) = self.attempt.take() else {
            return SendOutcome::NotLive;
        };
        self.ledger.send_if_live(attempt, timeout, push)
    }

    /// Withdraws the send after a refused push.
    pub fn unreserve(mut self) {
        if let Some(attempt) = self.attempt.take() {
            self.ledger.unreserve(attempt);
        }
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if let Some(attempt) = self.attempt.take() {
            self.ledger.unreserve(attempt);
        }
    }
}

#[cfg_attr(not(test), expect(dead_code, reason = "the router drives the ledger"))]
impl RequestLedger {
    pub fn new(clock: Arc<dyn Clock>, credits: Arc<dyn Credits>) -> Self {
        Self {
            inner: LedgerMutex::new(LedgerInner::default()),
            clock,
            credits,
            #[cfg(test)]
            hooks: LedgerMutex::new(LedgerHooks::default()),
        }
    }

    #[cfg(test)]
    pub(crate) fn set_hooks(&self, hooks: LedgerHooks) {
        *self.hooks.lock() = hooks;
    }

    /// Runs `step` under the ledger lock, then makes the credit calls it queued.
    fn locked<R>(&self, step: impl FnOnce(&mut LedgerInner) -> R) -> R {
        let (out, ops) = {
            let mut inner = self.inner.lock();
            let out = step(&mut inner);
            (out, mem::take(&mut inner.credit_ops))
        };
        for op in ops {
            match op {
                CreditOp::Release(owner) => self.credits.release(owner),
                CreditOp::Failed(owner, failure) => self.credits.failed(owner, failure),
            }
        }
        out
    }

    /// Admits a request against `owner`'s credits. The same key set from the same owner
    /// returns the request in play and takes no credit; another owner's identical set is
    /// `KeyInUse`; any partial overlap is `Overlap`. A rejected admission returns its credit.
    /// A `getcfilters` is `Unsupported` here, because only `request_filters` gives its count.
    pub fn request(
        &self,
        owner: SubId,
        height: u32,
        msg: NetworkMessage,
        class: Priority,
    ) -> Result<RequestId, RequestError> {
        if matches!(msg, NetworkMessage::GetCFilters(_)) {
            return Err(RequestError::Unsupported);
        }
        self.request_sized(owner, height, msg, class, None)
    }

    pub fn request_filters(
        &self,
        owner: SubId,
        request: GetCFilters,
        filters: u32,
    ) -> Result<RequestId, RequestError> {
        self.request_sized(
            owner,
            request.start_height,
            NetworkMessage::GetCFilters(request),
            Priority::CFilters,
            Some(filters),
        )
    }

    fn request_sized(
        &self,
        owner: SubId,
        height: u32,
        msg: NetworkMessage,
        class: Priority,
        filters: Option<u32>,
    ) -> Result<RequestId, RequestError> {
        let msg = match msg {
            NetworkMessage::GetData(inv) => {
                let mut seen = HashSet::new();
                NetworkMessage::GetData(inv.into_iter().filter(|item| seen.insert(*item)).collect())
            }
            msg => msg,
        };
        let keys = ledger_keys(&msg)?;
        if keys.len() > MAX_MEMBERS {
            return Err(RequestError::TooManyMembers);
        }
        match self.inner.lock().classify(owner, &keys) {
            Admission::New => {}
            Admission::Existing(id) => return Ok(id),
            Admission::Refused(error) => return Err(error),
        }
        if !self.credits.acquire_or_want(owner) {
            #[cfg(test)]
            if let Some(hook) = &self.hooks.lock().after_no_credit {
                hook(owner);
            }
            return Err(RequestError::NoCredit);
        }
        let admitted = {
            let mut inner = self.inner.lock();
            match inner.classify(owner, &keys) {
                Admission::New => {
                    Ok(insert_entry(&mut inner, owner, height, msg, class, filters, keys))
                }
                Admission::Existing(id) => Err(Ok(id)),
                Admission::Refused(error) => Err(Err(error)),
            }
        };
        admitted.or_else(|rejected| {
            self.credits.release(owner);
            rejected
        })
    }

    pub fn len(&self) -> usize {
        self.inner.lock().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().entries.is_empty()
    }

    /// Resolves every slot, drops every request, returning its credit, and unblocks every
    /// connection. The released charges stay in `take_released`.
    pub fn clear(&self) {
        self.locked(|inner| {
            inner.all_reasons = 0;
            let conns = mem::take(&mut inner.conns);
            for (peer, conn) in conns {
                for slot in &conn.slots {
                    inner.release_slot(peer, slot);
                }
            }
            let owners: Vec<_> = inner.entries.drain().map(|(_, entry)| entry.owner).collect();
            inner.credit_ops.extend(owners.into_iter().map(CreditOp::Release));
            inner.by_key.clear();
        })
    }

    /// Reserves sends of queued requests. A reserved send is not on the wire: it matches
    /// no response and never expires until `send_if_live` pushes it. A key is never routed
    /// to a connection that holds an unresolved slot naming it.
    pub fn route<P: RoutePeer>(
        &self,
        peers: &[P],
        global_cap: u64,
        costs: &ResponseCosts,
    ) -> Vec<(PeerId, AttemptId, NetworkMessage)> {
        let now = self.clock.steady();
        let mut inner = self.inner.lock();
        let mut reserved: HashMap<PeerId, u64> =
            peers.iter().map(|peer| (peer.id(), inner.reserved_bytes(peer.id()))).collect();
        let mut guaranteed = [MIN_IN_FLIGHT_PER_CLASS; MSG_CLASSES];
        for entry in inner.entries.values() {
            if entry.state == LedgerState::OnWire {
                guaranteed[entry.class as usize] =
                    guaranteed[entry.class as usize].saturating_sub(1);
            }
        }
        let total = peers.iter().fold(0u64, |sum, peer| {
            sum.saturating_add(peer.in_flight_bytes())
                .saturating_add(*reserved.get(&peer.id()).unwrap_or(&0))
        });
        let mut budget = global_cap.saturating_sub(total);
        let mut queued: Vec<_> = inner
            .entries
            .iter()
            .filter(|(_, entry)| entry.state == LedgerState::Queued)
            .map(|(id, entry)| (*id, entry.class, entry.height, entry.sequence))
            .collect();
        queued.sort_by_key(|(_, class, height, sequence)| (*class as usize, *height, *sequence));
        // Preserve #902's per-class floor before the strict-priority drain.
        let mut preferred = Vec::new();
        let mut rest = Vec::new();
        for (id, class, _, _) in queued {
            if guaranteed[class as usize] > 0 {
                guaranteed[class as usize] -= 1;
                preferred.push(id);
            } else {
                rest.push(id);
            }
        }
        preferred.extend(rest);
        let mut out = Vec::new();
        for id in preferred {
            if budget == 0 {
                break;
            }
            let Some(entry) = inner.entries.get(&id) else {
                continue;
            };
            let open: Vec<_> = peers
                .iter()
                .filter(|peer| {
                    !peer.blocked()
                        && !inner.blocked(peer.id())
                        && !entry.refused_by(peer.id())
                        && !inner.holds_any(peer.id(), entry.pending())
                        && peer
                            .in_flight_bytes()
                            .saturating_add(*reserved.get(&peer.id()).unwrap_or(&0))
                            < peer.cap()
                })
                .collect();
            let fresh = open.iter().copied().filter(|peer| !entry.tried.contains(&peer.id()));
            let peer = choose_route_peer(fresh, entry.class, &reserved)
                .or_else(|| choose_route_peer(open.into_iter(), entry.class, &reserved));
            let Some(peer) = peer else {
                continue;
            };
            let keys: Vec<_> = entry.pending().cloned().collect();
            let msg = message_for(&entry.msg, &keys);
            let cost = costs.cost(&msg);
            if let Some(attempt) = reserve_slot(&mut inner, id, peer.id(), keys, cost, now, false) {
                out.push((peer.id(), attempt, msg));
            }
            let bytes = reserved.entry(peer.id()).or_default();
            *bytes = bytes.saturating_add(cost);
            budget = budget.saturating_sub(cost);
        }
        out
    }

    /// Stores each charge on the member with its key, if the slot is still unresolved.
    /// Charges with no unresolved member, or for a resolved slot, go to `take_released`.
    /// Returns `None` when the slot is resolved.
    pub fn attach(
        &self,
        attempt: AttemptId,
        conn: PeerId,
        charges: Vec<(LedgerKey, ChargeId)>,
    ) -> Option<Reservation<'_>> {
        let mut inner = self.inner.lock();
        let inner = &mut *inner;
        let slot = inner.conns.get_mut(&conn).and_then(|c| {
            c.slots.iter_mut().find(|s| s.attempt == attempt && s.phase == Phase::Reserved)
        });
        let Some(slot) = slot else {
            inner.released.extend(charges.into_iter().map(|(_, id)| (conn, id)));
            return None;
        };
        slot.reserved_bytes = 0;
        for (key, id) in charges {
            match slot.members.iter_mut().find(|m| m.key == key && m.charge.is_none()) {
                Some(member) => member.charge = Some(id),
                None => inner.released.push((conn, id)),
            }
        }
        Some(Reservation {
            ledger: self,
            attempt: Some(attempt),
        })
    }

    fn send_if_live(
        &self,
        attempt: AttemptId,
        timeout: Duration,
        push: impl FnOnce(&[LedgerKey], &NetworkMessage),
    ) -> SendOutcome {
        let now = self.clock.steady();
        let mut inner = self.inner.lock();
        let inner = &mut *inner;
        let Some((peer, slot)) = inner.slot(attempt) else {
            return SendOutcome::NotLive;
        };
        if slot.phase != Phase::Reserved || slot.members.is_empty() {
            return SendOutcome::NotLive;
        }
        let Some(entry) = inner.entries.get(&slot.id) else {
            return SendOutcome::NotLive;
        };
        let keys: Vec<_> = slot.members.iter().map(|m| m.key.clone()).collect();
        push(&keys, &message_for(&entry.msg, &keys));
        let Some(conn) = inner.conns.get_mut(&peer) else {
            return SendOutcome::NotLive;
        };
        let seq = conn.next_seq;
        conn.next_seq = conn.next_seq.wrapping_add(1);
        if let Some(slot) = conn.slots.iter_mut().find(|slot| slot.attempt == attempt) {
            let timeout = match slot.kind {
                SlotKind::QrInfo => {
                    QRINFO_TIMEOUTS[usize::from(slot.index).min(QRINFO_TIMEOUTS.len() - 1)]
                }
                _ => timeout,
            };
            slot.phase = Phase::Sent(seq);
            slot.timeout = timeout;
            slot.sent_at = now;
            slot.deadline = now + timeout;
            slot.last_progress = now;
        }
        SendOutcome::Sent
    }

    /// Withdraws a reserved send that never reached the wire: it no longer counts toward
    /// `MAX_SENDS` and its charges are released. A no-op once the slot is sent or resolved.
    pub fn unreserve(&self, attempt: AttemptId) {
        let mut inner = self.inner.lock();
        if inner.slot(attempt).is_some_and(|(_, slot)| slot.phase == Phase::Reserved) {
            withdraw(&mut inner, attempt);
        }
    }

    /// Ends a sent request whose write failed.
    pub fn unmark_sent(&self, attempt: AttemptId) {
        self.end_attempt(attempt, FailReason::WriteFailed);
    }

    pub fn end_attempt(&self, attempt: AttemptId, reason: FailReason) {
        self.locked(|inner| end_send(inner, attempt, reason));
    }

    /// Resolves every slot on a finalized connection.
    pub fn disconnected(&self, peer: PeerId) {
        self.locked(|inner| {
            let attempts: Vec<_> = inner
                .conns
                .get(&peer)
                .map(|conn| conn.slots.iter().map(|slot| slot.attempt).collect())
                .unwrap_or_default();
            for attempt in attempts {
                end_send(inner, attempt, FailReason::Disconnected);
            }
            inner.conns.remove(&peer);
        })
    }

    /// Marks `conn` read-blocked for `reason`. While blocked its slots do not expire, its
    /// sends are not hedged and nothing new is routed to it.
    pub fn block(&self, conn: PeerId, reason: BlockReason, now: SteadyInstant) {
        self.inner.lock().reblock(conn, |reasons| *reasons |= reason.bit(), now);
    }

    pub fn unblock(&self, conn: PeerId, reason: BlockReason, now: SteadyInstant) {
        self.inner.lock().reblock(conn, |reasons| *reasons &= !reason.bit(), now);
    }

    /// Marks every connection read-blocked for `reason`, as when the hub's budget runs out.
    pub fn block_all(&self, reason: BlockReason, now: SteadyInstant) {
        self.inner.lock().reblock_all(|reasons| *reasons |= reason.bit(), now);
    }

    pub fn unblock_all(&self, reason: BlockReason, now: SteadyInstant) {
        self.inner.lock().reblock_all(|reasons| *reasons &= !reason.bit(), now);
    }

    /// Returns the latest deadline of every sent, unresolved slot on `conn`, live and
    /// orphaned, or `Blocked` while `conn` is read-blocked. A reserved slot has no deadline
    /// yet.
    pub fn deadline_horizon(&self, conn: PeerId) -> Horizon {
        let inner = self.inner.lock();
        if inner.blocked(conn) {
            return Horizon::Blocked;
        }
        inner
            .conns
            .get(&conn)
            .and_then(|c| {
                c.slots
                    .iter()
                    .filter(|slot| slot.phase != Phase::Reserved)
                    .map(|slot| slot.deadline)
                    .max()
            })
            .map_or(Horizon::None, Horizon::At)
    }

    /// Ends every sent slot past its deadline. A timed-out `getheaders` or `getcfilters`
    /// send stays as an orphan, because its late reply is matched by position; that orphan
    /// later expires silently. Only the first two non-hedge sends blame their connection,
    /// as in #902. Any other orphan past its deadline marks its connection for
    /// disconnection under SPV's non-service policy.
    pub fn expire(&self) -> Expired {
        let now = self.clock.steady();
        self.locked(|inner| expire(inner, now))
    }

    /// Reserves one extra send of the lowest stalled block. A send not yet pushed is not
    /// stalled. A hedge counts toward `MAX_SENDS` and is reserved only while a send remains.
    pub fn hedge<P: RoutePeer>(
        &self,
        peers: &[P],
        costs: &ResponseCosts,
    ) -> Option<(PeerId, AttemptId, NetworkMessage)> {
        let now = self.clock.steady();
        let mut inner = self.inner.lock();
        let (id, entry) = inner
            .entries
            .iter()
            .filter(|(_, entry)| {
                entry.state == LedgerState::OnWire
                    && entry.pending().any(|key| matches!(key, LedgerKey::Block(_)))
            })
            .min_by_key(|(_, entry)| (entry.height, entry.sequence))?;
        let id = *id;
        let blocked = |peer: PeerId| {
            inner.blocked(peer) || peers.iter().any(|p| p.id() == peer && p.blocked())
        };
        if entry.hedged
            || entry.sends >= MAX_SENDS
            || inner.live_slots(id).any(|(on, slot)| {
                blocked(on)
                    || slot.phase == Phase::Reserved
                    || now.duration_since(slot.last_progress) < HEDGE_AFTER
            })
        {
            return None;
        }
        let peer = peers
            .iter()
            .filter(|peer| {
                !blocked(peer.id())
                    && peer.in_flight_bytes().saturating_add(inner.reserved_bytes(peer.id()))
                        < peer.cap()
                    && !entry.refused_by(peer.id())
                    && !inner.live_slots(id).any(|(on, _)| on == peer.id())
                    && !inner.holds_any(peer.id(), entry.pending())
            })
            .max_by_key(|peer| {
                peer.cap()
                    .saturating_sub(peer.in_flight_bytes())
                    .saturating_sub(inner.reserved_bytes(peer.id()))
            })?;
        let keys: Vec<_> = entry.pending().cloned().collect();
        let msg = message_for(&entry.msg, &keys);
        let attempt = reserve_slot(&mut inner, id, peer.id(), keys, costs.cost(&msg), now, true)?;
        Some((peer.id(), attempt, msg))
    }

    /// Matches a response to the oldest sent slot on `peer` that it answers, in send
    /// order. Reserved slots match nothing. A live slot's pending member is tagged; an
    /// orphan absorbs its reply untagged. Each send counts its own filters, and a range
    /// ends at its stop filter: complete with the declared count, else that send ends
    /// as `Incomplete`. Returns `None` when no slot matches.
    pub fn response(&self, peer: PeerId, msg: &NetworkMessage) -> Option<Matched> {
        let now = self.clock.steady();
        self.locked(|inner| respond(inner, peer, msg, now))
    }

    /// Releases the keys of the members that arrived. The request leaves once no member is
    /// pending; until then its pending members stay live.
    pub fn request_answered(&self, id: RequestId) -> bool {
        self.locked(|inner| answer(inner, id))
    }

    /// Drops a request and returns its credit. Its sent slots stay as orphans until
    /// answered or expired.
    pub fn cancel(&self, id: RequestId) -> bool {
        self.locked(|inner| cancel(inner, id))
    }

    /// Cancels every request `owner` holds, as when its subscription is dropped.
    pub fn cancel_owner(&self, owner: SubId) {
        self.locked(|inner| {
            let ids: Vec<_> =
                inner.entries.iter().filter(|(_, e)| e.owner == owner).map(|(id, _)| *id).collect();
            for id in ids {
                cancel(inner, id);
            }
        })
    }

    /// Returns the charges every resolution without a response released, once.
    pub fn take_released(&self) -> Vec<(PeerId, ChargeId)> {
        mem::take(&mut self.inner.lock().released)
    }

    /// Settles the members of `peer`'s sends that a `notfound` names and returns their
    /// records. A send ends once none of its members is pending. A member refused by a
    /// second connection fails, however many sends remain.
    pub fn not_found(&self, peer: PeerId, inv: &[Inventory]) -> Vec<ChargeId> {
        self.locked(|inner| not_found(inner, peer, inv))
    }
}

fn not_found(inner: &mut LedgerInner, peer: PeerId, inv: &[Inventory]) -> Vec<ChargeId> {
    let mut settled = Vec::new();
    let mut touched = Vec::new();
    for item in inv {
        let Inventory::Block(hash) = item else {
            continue;
        };
        let key = LedgerKey::Block(*hash);
        let Some(attempt) = keyed_slot(inner, peer, &key) else {
            continue;
        };
        let Some(slot) = inner.slot_mut(attempt) else {
            continue;
        };
        let (id, live) = (slot.id, slot.live());
        if let Some(pos) = slot.members.iter().position(|m| m.key == key) {
            settled.extend(slot.members.remove(pos).charge);
        }
        if slot.members.is_empty() {
            inner.take_slot(attempt);
        }
        if !live {
            continue;
        }
        let Some(entry) = inner.entries.get_mut(&id) else {
            continue;
        };
        let Some(member) =
            entry.members.iter_mut().find(|m| m.key == key && m.state == MemberState::Pending)
        else {
            continue;
        };
        match member.notfound_from {
            Some(other) if other != peer => {
                member.state = MemberState::Failed(FailReason::NotFound);
                member_resolved(inner, id, &key);
            }
            _ => member.notfound_from = Some(peer),
        }
        if !touched.contains(&(attempt, id)) {
            touched.push((attempt, id));
        }
    }
    for (attempt, id) in touched {
        if inner.slot(attempt).is_none() {
            after_send_end(inner, id, FailReason::NotFound);
        }
        end_drained_sends(inner, id);
    }
    settled
}

/// Ends each sent, live send of `id` with no pending member left. Only a `notfound` takes
/// a pending member off a send, so that is the reason the send ends.
fn end_drained_sends(inner: &mut LedgerInner, id: RequestId) {
    let Some(entry) = inner.entries.get(&id) else {
        return;
    };
    let drained: Vec<_> = inner
        .live_slots(id)
        .filter(|(_, slot)| {
            matches!(slot.phase, Phase::Sent(_))
                && !slot.members.iter().any(|m| entry.is_pending(&m.key))
        })
        .map(|(_, slot)| slot.attempt)
        .collect();
    for attempt in drained {
        end_send(inner, attempt, FailReason::NotFound);
    }
}

fn expire(inner: &mut LedgerInner, now: SteadyInstant) -> Expired {
    let mut expired = Expired::default();
    let mut attempts = Vec::new();
    for (peer, conn) in &inner.conns {
        if inner.blocked(*peer) {
            continue;
        }
        for slot in &conn.slots {
            if slot.phase == Phase::Reserved || now <= slot.deadline {
                continue;
            }
            match slot.state {
                SlotState::Orphaned => {
                    expired.unanswered.insert(*peer);
                }
                SlotState::Live if slot.index < KICKING_ATTEMPTS as u8 && !slot.hedge => {
                    expired.culprits.insert(*peer);
                }
                SlotState::Live | SlotState::TimedOut => {}
            }
            let positional = matches!(slot.kind, SlotKind::Headers | SlotKind::CFilters);
            attempts.push((slot.attempt, slot.live() && positional));
        }
    }
    for (attempt, keep) in attempts {
        if keep {
            orphan_on_timeout(inner, attempt, now);
        } else {
            end_send(inner, attempt, FailReason::Timeout);
        }
    }
    expired
}

/// Ends a timed-out send but keeps its slot as an orphan with a fresh deadline. A late
/// positional reply then lands on its own slot, not on the next one on the connection.
/// The timeout already applied #902's blame limit, so the orphan's expiry blames nobody.
fn orphan_on_timeout(inner: &mut LedgerInner, attempt: AttemptId, now: SteadyInstant) {
    let Some(slot) = inner.slot_mut(attempt) else {
        return;
    };
    slot.state = SlotState::TimedOut;
    slot.deadline = now + slot.timeout;
    slot.last_progress = now;
    let id = slot.id;
    after_send_end(inner, id, FailReason::Timeout);
}

fn respond(
    inner: &mut LedgerInner,
    peer: PeerId,
    msg: &NetworkMessage,
    now: SteadyInstant,
) -> Option<Matched> {
    if answers_in_order(msg) {
        refresh_live_sends(inner, peer, now);
    }
    let attempt = match_slot(inner, peer, msg)?;

    let (id, live, first) = {
        let (_, slot) = inner.slot(attempt)?;
        (slot.id, slot.live(), slot.members.first()?.key.clone())
    };
    let NetworkMessage::CFilter(filter) = msg else {
        let key = response_key(msg).unwrap_or(first);
        return Some(resolve_on_slot(inner, attempt, &key));
    };
    let pending = live && inner.entries.get(&id).is_some_and(|e| e.is_pending(&first));
    let slot = inner.slot_mut(attempt)?;
    let charge = slot.members.first().and_then(|m| m.charge);
    let stream = slot.stream.as_mut()?;
    stream.received = stream.received.saturating_add(1);
    let (stop, complete) = (
        filter.block_hash == stream.stop,
        stream.declared.is_none_or(|declared| declared == stream.received),
    );
    let tag = pending.then(|| Tag {
        id,
        attempt,
        member: first.clone(),
        last: false,
    });
    if !stop {
        return Some(Matched {
            tag,
            settle: None,
            accrue: charge,
        });
    }
    if !complete {
        if let Some(member) = slot.members.first_mut() {
            member.charge = None;
        }
        end_send(inner, attempt, FailReason::Incomplete);
        return Some(Matched {
            tag,
            settle: charge,
            accrue: None,
        });
    }
    Some(resolve_on_slot(inner, attempt, &first))
}

/// Whether `msg` is an answer that shows its peer working through its request queue.
fn answers_in_order(msg: &NetworkMessage) -> bool {
    matches!(
        msg,
        NetworkMessage::CFilter(_)
            | NetworkMessage::CFHeaders(_)
            | NetworkMessage::Block(_)
            | NetworkMessage::Headers(_)
            | NetworkMessage::Headers2(_)
    )
}

/// Peers answer in order, so an answer from `peer` keeps every live send it has queued
/// from being judged stalled behind it, as #902's `Broker::progress` does. Reserved slots
/// are not on the wire, and an orphan keeps its own deadline.
fn refresh_live_sends(inner: &mut LedgerInner, peer: PeerId, now: SteadyInstant) {
    let Some(conn) = inner.conns.get_mut(&peer) else {
        return;
    };
    for slot in conn.slots.iter_mut().filter(|slot| slot.live()) {
        if let Phase::Sent(_) = slot.phase {
            slot.last_progress = now;
            slot.deadline = slot.deadline.max(now + slot.timeout);
        }
    }
}

fn answer(inner: &mut LedgerInner, id: RequestId) -> bool {
    let Some(entry) = inner.entries.get_mut(&id) else {
        return false;
    };
    if entry.pending().next().is_none() {
        let owner = entry.owner;
        leave(inner, id);
        inner.credit_ops.push(CreditOp::Release(owner));
        return true;
    }

    entry.answered = true;
    let arrived: Vec<_> = entry
        .members
        .iter()
        .filter(|m| m.state == MemberState::Arrived)
        .map(|m| m.key.clone())
        .collect();
    for key in arrived {
        if inner.by_key.get(&key) == Some(&id) {
            inner.by_key.remove(&key);
        }
    }
    true
}

fn cancel(inner: &mut LedgerInner, id: RequestId) -> bool {
    let Some(entry) = leave(inner, id) else {
        return false;
    };
    inner.credit_ops.push(CreditOp::Release(entry.owner));
    true
}

fn shift_on_change(conn: &mut Conn, was: bool, is: bool, now: SteadyInstant) {
    if !was && is {
        conn.blocked_since = Some(now);
    }
    if was && !is {
        if let Some(since) = conn.blocked_since.take() {
            for slot in &mut conn.slots {
                let blocked = now.saturating_duration_since(since.max(slot.sent_at));
                slot.deadline += blocked;
                slot.last_progress += blocked;
            }
        }
    }
}

fn insert_entry(
    inner: &mut LedgerInner,
    owner: SubId,
    height: u32,
    msg: NetworkMessage,
    class: Priority,
    filters: Option<u32>,
    keys: Vec<LedgerKey>,
) -> RequestId {
    let id = RequestId(inner.next_request);
    inner.next_request = inner.next_request.wrapping_add(1);
    let sequence = inner.next_sequence;
    inner.next_sequence = inner.next_sequence.wrapping_add(1);
    for key in &keys {
        inner.by_key.insert(key.clone(), id);
    }
    inner.entries.insert(
        id,
        LedgerEntry {
            owner,
            members: keys
                .into_iter()
                .map(|key| Member {
                    key,
                    state: MemberState::Pending,
                    notfound_from: None,
                })
                .collect(),
            msg,
            height,
            class,
            state: LedgerState::Queued,
            sends: 0,
            tried: HashSet::new(),
            filters,
            hedged: false,
            answered: false,
            sequence,
        },
    );
    id
}

fn reserve_slot(
    inner: &mut LedgerInner,
    id: RequestId,
    peer: PeerId,
    keys: Vec<LedgerKey>,
    cost: u64,
    now: SteadyInstant,
    hedge: bool,
) -> Option<AttemptId> {
    let entry = inner.entries.get_mut(&id)?;
    let attempt = AttemptId(inner.next_attempt);
    inner.next_attempt = inner.next_attempt.wrapping_add(1);
    entry.tried.insert(peer);
    entry.state = LedgerState::OnWire;
    entry.hedged |= hedge;
    let index = entry.sends;
    entry.sends = entry.sends.saturating_add(1);
    let (kind, stream) = match &entry.msg {
        NetworkMessage::GetHeaders(_) | NetworkMessage::GetHeaders2(_) => (SlotKind::Headers, None),
        NetworkMessage::GetQRInfo(_) => (SlotKind::QrInfo, None),
        NetworkMessage::GetCFilters(request) => (
            SlotKind::CFilters,
            Some(Stream {
                stop: request.stop_hash,
                declared: entry.filters,
                received: 0,
            }),
        ),
        _ => (SlotKind::Other, None),
    };
    inner.conns.entry(peer).or_default().slots.push(Slot {
        attempt,
        id,
        kind,
        state: SlotState::Live,
        phase: Phase::Reserved,
        members: keys
            .into_iter()
            .map(|key| SlotMember {
                key,
                charge: None,
            })
            .collect(),
        stream,
        timeout: Duration::ZERO,
        sent_at: now,
        deadline: now,
        last_progress: now,
        index,
        hedge,
        reserved_bytes: cost,
    });
    Some(attempt)
}

/// Returns the oldest sent slot on `peer` holding `key`, in send order.
fn keyed_slot(inner: &LedgerInner, peer: PeerId, key: &LedgerKey) -> Option<AttemptId> {
    oldest_sent(inner, peer, |slot| slot.holds(key))
}

fn oldest_sent(
    inner: &LedgerInner,
    peer: PeerId,
    matches: impl Fn(&Slot) -> bool,
) -> Option<AttemptId> {
    inner
        .conns
        .get(&peer)?
        .slots
        .iter()
        .filter_map(|slot| match slot.phase {
            Phase::Sent(seq) if matches(slot) => Some((seq, slot.attempt)),
            _ => None,
        })
        .min_by_key(|(seq, _)| *seq)
        .map(|(_, attempt)| attempt)
}

fn response_key(msg: &NetworkMessage) -> Option<LedgerKey> {
    match msg {
        NetworkMessage::Block(block) => Some(LedgerKey::Block(block.block_hash())),
        NetworkMessage::CFHeaders(headers) => Some(LedgerKey::CfHeaders(headers.stop_hash)),
        NetworkMessage::MnListDiff(diff) => Some(LedgerKey::MnListDiff {
            base: diff.base_block_hash,
            block: diff.block_hash,
        }),
        NetworkMessage::Headers(headers) => {
            headers.first().map(|header| LedgerKey::Headers(header.prev_blockhash))
        }
        NetworkMessage::QRInfo(info) => Some(LedgerKey::QrInfo(info.mn_list_diff_tip.block_hash)),
        _ => None,
    }
}

fn match_slot(inner: &LedgerInner, peer: PeerId, msg: &NetworkMessage) -> Option<AttemptId> {
    if let Some(key) = response_key(msg) {
        return keyed_slot(inner, peer, &key);
    }
    match msg {
        NetworkMessage::Headers(_) => {
            oldest_sent(inner, peer, |slot| slot.kind == SlotKind::Headers)
        }
        NetworkMessage::CFilter(_) => {
            oldest_sent(inner, peer, |slot| slot.kind == SlotKind::CFilters)
        }
        _ => None,
    }
}

/// Settles `key` on its slot. A live slot's pending member arrives and is tagged.
fn resolve_on_slot(inner: &mut LedgerInner, attempt: AttemptId, key: &LedgerKey) -> Matched {
    let Some(slot) = inner.slot_mut(attempt) else {
        return Matched::default();
    };
    let (id, live) = (slot.id, slot.live());
    let settle = slot
        .members
        .iter()
        .position(|m| &m.key == key)
        .and_then(|pos| slot.members.remove(pos).charge);
    let emptied = slot.members.is_empty();
    if emptied {
        inner.take_slot(attempt);
    }
    let mut matched = Matched {
        tag: None,
        settle,
        accrue: None,
    };
    if !live {
        return matched;
    }
    let member = inner.entries.get_mut(&id).and_then(|entry| {
        let member =
            entry.members.iter_mut().find(|m| &m.key == key && m.state == MemberState::Pending)?;
        member.state = MemberState::Arrived;
        Some(entry.pending().next().is_none())
    });
    if let Some(last) = member {
        matched.tag = Some(Tag {
            id,
            attempt,
            member: key.clone(),
            last,
        });
        member_resolved(inner, id, key);
        if last {
            settle_entry(inner, id, None);
            return matched;
        }
    }
    if emptied {
        after_send_end(inner, id, FailReason::NotFound);
    }
    end_drained_sends(inner, id);
    matched
}

/// Drops a member that is no longer pending from the request's reserved slots, which
/// have not reached the wire. A reserved slot left with no member is withdrawn.
fn member_resolved(inner: &mut LedgerInner, id: RequestId, key: &LedgerKey) {
    let mut emptied = Vec::new();
    for (peer, conn) in inner.conns.iter_mut() {
        for slot in conn.slots.iter_mut() {
            if slot.id != id || !slot.live() || slot.phase != Phase::Reserved {
                continue;
            }
            if let Some(pos) = slot.members.iter().position(|m| &m.key == key) {
                if let Some(charge) = slot.members.remove(pos).charge {
                    inner.released.push((*peer, charge));
                }
            }
            if slot.members.is_empty() {
                emptied.push(slot.attempt);
            }
        }
    }
    for attempt in emptied {
        withdraw(inner, attempt);
    }
}

/// Removes a reserved slot as if it was never made.
fn withdraw(inner: &mut LedgerInner, attempt: AttemptId) {
    let Some((_, slot)) = inner.take_slot(attempt) else {
        return;
    };
    if !slot.live() {
        return;
    }
    let live_left = inner.live_slots(slot.id).next().is_some();
    let Some(entry) = inner.entries.get_mut(&slot.id) else {
        return;
    };
    entry.sends = entry.sends.saturating_sub(1);
    if slot.hedge {
        entry.hedged = false;
    }
    if entry.state == LedgerState::OnWire && !live_left {
        entry.state = LedgerState::Queued;
    }
}

/// Resolves a slot without a response for its remaining members. A live slot's send ends.
fn end_send(inner: &mut LedgerInner, attempt: AttemptId, reason: FailReason) {
    let Some((_, slot)) = inner.take_slot(attempt) else {
        return;
    };
    if slot.live() {
        after_send_end(inner, slot.id, reason);
    }
}

fn after_send_end(inner: &mut LedgerInner, id: RequestId, reason: FailReason) {
    let live_left = inner.live_slots(id).next().is_some();
    let Some(entry) = inner.entries.get_mut(&id) else {
        return;
    };
    if entry.pending().next().is_none() {
        settle_entry(inner, id, Some(reason));
        return;
    }
    // Another live send can still answer, so nothing is requeued until the last one ends.
    if live_left {
        return;
    }
    if entry.sends >= MAX_SENDS {
        for member in entry.members.iter_mut().filter(|m| m.state == MemberState::Pending) {
            member.state = MemberState::Failed(reason);
        }
        settle_entry(inner, id, Some(reason));
    } else {
        entry.state = LedgerState::Queued;
    }
}

/// Ends a request with no pending member: a failure if some member failed, else arrived.
/// An arrived request the owner already answered leaves. `ended` is the reason of the send
/// end that settled it; when an arrival settled it, the failed members give the reason.
fn settle_entry(inner: &mut LedgerInner, id: RequestId, ended: Option<FailReason>) {
    let Some(entry) = inner.entries.get_mut(&id) else {
        return;
    };
    let failed = entry.members.iter().find_map(|m| match m.state {
        MemberState::Failed(reason) => Some(reason),
        _ => None,
    });
    let Some(failed) = failed else {
        entry.state = LedgerState::Arrived;
        let answered = entry.answered;
        retire_slots(inner, id);
        if answered {
            let owner = entry_owner(inner, id);
            leave(inner, id);
            inner.credit_ops.extend(owner.map(CreditOp::Release));
        }
        return;
    };
    let Some(entry) = leave(inner, id) else {
        return;
    };
    let mut failure = RequestFailed {
        id,
        failed: Vec::new(),
        arrived: Vec::new(),
        sends: entry.sends,
        last: ended.unwrap_or(failed),
    };
    for member in entry.members {
        match member.state {
            MemberState::Failed(reason) => failure.failed.push((member.key, reason)),
            MemberState::Arrived => failure.arrived.push(member.key),
            MemberState::Pending => {}
        }
    }
    inner.credit_ops.push(CreditOp::Failed(entry.owner, failure));
}

/// Orphans the request's sent live slots and removes its reserved ones.
fn retire_slots(inner: &mut LedgerInner, id: RequestId) {
    let mut reserved = Vec::new();
    for conn in inner.conns.values_mut() {
        for slot in conn.slots.iter_mut().filter(|slot| slot.id == id && slot.live()) {
            match slot.phase {
                Phase::Reserved => reserved.push(slot.attempt),
                Phase::Sent(_) => slot.state = SlotState::Orphaned,
            }
        }
    }
    for attempt in reserved {
        inner.take_slot(attempt);
    }
}

fn entry_owner(inner: &LedgerInner, id: RequestId) -> Option<SubId> {
    inner.entries.get(&id).map(|entry| entry.owner)
}

fn leave(inner: &mut LedgerInner, id: RequestId) -> Option<LedgerEntry> {
    retire_slots(inner, id);
    let entry = inner.entries.remove(&id)?;
    for member in &entry.members {
        if inner.by_key.get(&member.key) == Some(&id) {
            inner.by_key.remove(&member.key);
        }
    }
    Some(entry)
}

fn choose_route_peer<'a, P: RoutePeer>(
    peers: impl Iterator<Item = &'a P> + Clone,
    class: Priority,
    reserved: &HashMap<PeerId, u64>,
) -> Option<&'a P> {
    if matches!(class, Priority::Headers | Priority::Blocks) {
        let waits: Option<Vec<_>> = peers
            .clone()
            .map(|peer| {
                let queued = peer.queued_secs()?;
                let bytes = *reserved.get(&peer.id()).unwrap_or(&0);
                if bytes > 0 && peer.serving_rate() == 0 {
                    return None;
                }
                Some((
                    queued
                        + if bytes == 0 {
                            0.0
                        } else {
                            bytes as f64 / peer.serving_rate() as f64
                        },
                    peer,
                ))
            })
            .collect();
        if let Some(waits) = waits {
            return waits.into_iter().min_by(|a, b| a.0.total_cmp(&b.0)).map(|(_, peer)| peer);
        }
    }
    peers.min_by(|a, b| {
        let a_bytes = a.in_flight_bytes().saturating_add(*reserved.get(&a.id()).unwrap_or(&0));
        let b_bytes = b.in_flight_bytes().saturating_add(*reserved.get(&b.id()).unwrap_or(&0));
        (a_bytes as u128 * b.cap() as u128).cmp(&(b_bytes as u128 * a.cap() as u128))
    })
}

enum ReqState {
    Queued(Tries),
    OnWire(Box<OnWire>),
    /// The block was received and waits for its manager to release the key.
    Arrived,
}

/// What a key's sends so far count for, kept across retries.
#[derive(Clone, Copy, Debug, Default)]
struct Tries {
    /// Sends that can blame their peer.
    attempts: u32,
    /// Whether the key already had its one retry without blame after being answered.
    forgiven: bool,
    /// The filters its batch holds, for a filter range declared with its size.
    filters: Option<u32>,
}

struct OnWire {
    /// The connection, not the address: a late close of an old connection must not
    /// pull back the work of a new one to the same address.
    peer: PeerId,
    /// Refreshed by every piece of a streaming response.
    last_progress: Instant,
    msg: NetworkMessage,
    height: u32,
    hedge: Option<PeerId>,
    tries: Tries,
    /// Send order: a peer answers its requests in this order.
    seq: u64,
    /// Filters its carrier sent for it so far.
    filters_received: u32,
    /// Whether its stop filter arrived.
    ended: bool,
}

impl OnWire {
    /// Every filter of its batch arrived, so it waits for its manager, not for the peer.
    fn answered(&self) -> bool {
        self.ended && self.tries.filters == Some(self.filters_received)
    }
}

/// The request queue plus the registry of every keyed request in play, queued, on the
/// wire or arrived. A key stays registered until released, so re-declaring it is a no-op.
pub(crate) struct Broker {
    queue: MsgQueue,
    requests: Mutex<HashMap<RequestKey, ReqState>>,
    sends: AtomicU64,
}

impl Broker {
    pub fn new() -> Self {
        Self {
            queue: MsgQueue::new(),
            requests: Mutex::new(HashMap::new()),
            sends: AtomicU64::new(0),
        }
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn wake(&self) {
        self.queue.notify.notify_one();
    }

    pub async fn notified(&self) {
        self.queue.notify.notified().await
    }

    pub async fn enqueue(&self, height: u32, msg: NetworkMessage) {
        self.enqueue_sized(height, msg, None).await;
    }

    /// Enqueues the `getcfilters` of a batch of `filters` filters, so its range counts
    /// as answered once all of them arrive.
    pub async fn enqueue_filters(&self, height: u32, msg: NetworkMessage, filters: u32) {
        self.enqueue_sized(height, msg, Some(filters)).await;
    }

    async fn enqueue_sized(&self, height: u32, msg: NetworkMessage, filters: Option<u32>) {
        let keys = request_keys(&msg);
        if !keys.is_empty() {
            let mut reqs = self.requests.lock().await;
            if keys.iter().any(|k| reqs.contains_key(k)) {
                return;
            }
            for key in keys {
                reqs.insert(
                    key,
                    ReqState::Queued(Tries {
                        filters,
                        ..Tries::default()
                    }),
                );
            }
        }
        self.queue.push(height, msg).await;
    }

    /// Unregisters `key`. If its request was on the wire, wakes the router to send more.
    /// If it was still queued, drops it unless another of its keys is still registered,
    /// since sent later it would go out untracked or under a new request for the same key.
    pub async fn release(&self, key: &RequestKey) {
        let mut reqs = self.requests.lock().await;
        match reqs.remove(key) {
            Some(ReqState::OnWire(_)) => {
                drop(reqs);
                self.wake();
            }
            Some(ReqState::Queued(_)) => self.drop_queued(key, &reqs).await,
            Some(ReqState::Arrived) | None => {}
        }
    }

    /// Drops the queued messages carrying `key` that no other registered key still needs.
    async fn drop_queued(&self, key: &RequestKey, reqs: &HashMap<RequestKey, ReqState>) {
        self.queue
            .remove(|msg| {
                let keys = request_keys(msg);
                keys.contains(key) && !keys.iter().any(|k| reqs.contains_key(k))
            })
            .await;
    }

    pub async fn clear(&self) {
        self.requests.lock().await.clear();
        self.queue.pop_n(self.queue.len()).await;
    }

    /// Puts the matching requests back in the queue. An answered range that has not had
    /// its free retry costs no attempt, and with `forgive` this is that retry.
    async fn requeue(
        &self,
        matches: impl Fn(&RequestKey, &OnWire) -> bool,
        forgive: bool,
    ) -> usize {
        let retried: Vec<(u32, NetworkMessage)> = {
            let mut reqs = self.requests.lock().await;
            let keys: Vec<RequestKey> = reqs
                .iter()
                .filter_map(|(k, s)| match s {
                    ReqState::OnWire(o) if matches(k, o) => Some(k.clone()),
                    _ => None,
                })
                .collect();
            keys.into_iter()
                .filter_map(|key| match reqs.remove(&key) {
                    Some(ReqState::OnWire(o)) => {
                        // Until an answered range has had its free retry, the wait was
                        // the client's, so its send costs no attempt.
                        let mut tries = o.tries;
                        if o.answered() && !tries.forgiven {
                            tries.attempts = tries.attempts.saturating_sub(1);
                            tries.forgiven = forgive;
                        }
                        reqs.insert(key, ReqState::Queued(tries));
                        Some((o.height, o.msg))
                    }
                    _ => None,
                })
                .collect()
        };
        let n = retried.len();
        self.queue.push_front_all(retried).await;
        n
    }

    pub async fn requeue_from(&self, gone: &HashSet<PeerId>) -> usize {
        self.requeue(|_, o| gone.contains(&o.peer), false).await
    }

    pub async fn not_found(&self, peer: PeerId, inv: &[Inventory]) {
        let keys: HashSet<RequestKey> = inv
            .iter()
            .filter_map(|i| match i {
                Inventory::Block(hash) => Some(RequestKey::Block(*hash)),
                _ => None,
            })
            .collect();
        if !keys.is_empty() {
            self.requeue(|k, o| o.peer == peer && keys.contains(k), false).await;
        }
    }

    /// Settled on arrival, not when processed: blocks wait in height order, and that
    /// wait is not the peer's. The first copy of a hedged block settles it. A request
    /// put back in the queue is settled by a copy from any peer, and its message dropped.
    /// The key stays registered until released, so a re-declare before then sends nothing.
    pub async fn block_arrived(&self, peer: PeerId, hash: BlockHash) {
        let key = RequestKey::Block(hash);
        let mut reqs = self.requests.lock().await;
        match reqs.get(&key) {
            Some(ReqState::OnWire(o)) if o.peer == peer || o.hedge == Some(peer) => {}
            Some(ReqState::Queued(_)) => {
                reqs.remove(&key);
                self.drop_queued(&key, &reqs).await;
            }
            _ => return,
        }
        reqs.insert(key, ReqState::Arrived);
        drop(reqs);
        self.wake();
    }

    /// Counts a filter from `peer` toward the oldest filter range it has not finished, and
    /// ends that range when `hash` is its stop hash. A range that ended with its whole
    /// batch is answered: it then waits for the filter manager's scan, not for the peer.
    pub async fn filter_arrived(&self, peer: PeerId, hash: BlockHash) {
        let mut reqs = self.requests.lock().await;
        let oldest = reqs
            .values_mut()
            .filter_map(|state| match state {
                ReqState::OnWire(o)
                    if o.peer == peer
                        && !o.ended
                        && matches!(o.msg, NetworkMessage::GetCFilters(_)) =>
                {
                    Some(o)
                }
                _ => None,
            })
            .min_by_key(|o| o.seq);
        if let Some(o) = oldest {
            o.filters_received += 1;
            o.ended = matches!(&o.msg, NetworkMessage::GetCFilters(m) if m.stop_hash == hash);
        }
    }

    /// Peers answer in order, so any answer is progress on all of its requests.
    pub async fn progress(&self, peer: PeerId) {
        let now = Instant::now();
        for state in self.requests.lock().await.values_mut() {
            if let ReqState::OnWire(o) = state {
                if o.peer == peer {
                    o.last_progress = now;
                }
            }
        }
    }

    /// Returns the peers to kick for a stalled request. A request that already stalled
    /// [`KICKING_ATTEMPTS`] times, or was answered and has not had its free retry yet, is
    /// retried without blaming its peer.
    pub async fn expire(&self, timeout: impl Fn(&PeerId) -> Duration) -> HashSet<PeerId> {
        let now = Instant::now();
        let mut culprits = HashSet::new();
        let mut unanswerable = HashSet::new();
        for (key, state) in self.requests.lock().await.iter() {
            let ReqState::OnWire(o) = state else {
                continue;
            };
            if now.duration_since(o.last_progress) <= timeout(&o.peer) {
                continue;
            }
            if o.answered() && !o.tries.forgiven {
                tracing::debug!(
                    target: "dash_spv::network",
                    "{key:?} answered by {} but not released in time, retrying",
                    o.peer,
                );
                unanswerable.insert(key.clone());
            } else if o.tries.attempts < KICKING_ATTEMPTS {
                culprits.insert(o.peer);
            } else {
                tracing::debug!(
                    target: "dash_spv::network",
                    "{key:?} unanswered after {} attempts, retrying without kicking {}",
                    o.tries.attempts,
                    o.peer,
                );
                unanswerable.insert(key.clone());
            }
        }
        if !unanswerable.is_empty() {
            self.requeue(|k, _| unanswerable.contains(k), true).await;
        }
        culprits
    }

    /// Puts the registered keys of `msg` on the wire to `peer`, and returns each one's
    /// prior state with its new send number.
    async fn mark_sent(
        &self,
        msg: &NetworkMessage,
        height: u32,
        peer: PeerId,
    ) -> Vec<(RequestKey, ReqState, u64)> {
        let mut reqs = self.requests.lock().await;
        let mut prior = Vec::new();
        for key in request_keys(msg) {
            let Some(slot) = reqs.get_mut(&key) else {
                continue;
            };
            let tries = match slot {
                ReqState::Queued(tries) => *tries,
                ReqState::OnWire(o) => o.tries,
                // The block of a requeued request can arrive while it is resent.
                ReqState::Arrived => continue,
            };
            let seq = self.sends.fetch_add(1, Ordering::Relaxed);
            let on_wire = ReqState::OnWire(Box::new(OnWire {
                peer,
                msg: msg.clone(),
                height,
                last_progress: Instant::now(),
                hedge: None,
                tries: Tries {
                    attempts: tries.attempts + 1,
                    ..tries
                },
                seq,
                filters_received: 0,
                ended: false,
            }));
            prior.push((key, std::mem::replace(slot, on_wire), seq));
        }
        prior
    }

    /// Restores the keys of a failed send that nothing else took over since. Returns
    /// whether its message still needs sending.
    async fn unmark_sent(&self, prior: Vec<(RequestKey, ReqState, u64)>) -> bool {
        if prior.is_empty() {
            return true;
        }
        let mut reqs = self.requests.lock().await;
        let mut restored = false;
        for (key, state, seq) in prior {
            if let Some(slot) = reqs.get_mut(&key) {
                if matches!(slot, ReqState::OnWire(o) if o.seq == seq) {
                    *slot = state;
                    restored = true;
                }
            }
        }
        restored
    }

    /// Sends what fits in both the peers' and the global byte caps, each message to
    /// the peer [`choose_peer`] picks.
    pub async fn route(
        &self,
        peers: &[PeerHandle],
        global_cap: u64,
        costs: &ResponseCosts,
    ) -> usize {
        if peers.is_empty() {
            return 0;
        }
        let total_in_flight: u64 = peers.iter().map(PeerHandle::in_flight_bytes).sum();
        let per_peer_room: u64 =
            peers.iter().map(|p| p.cap().saturating_sub(p.in_flight_bytes())).sum();
        let budget = per_peer_room.min(global_cap.saturating_sub(total_in_flight));
        if budget == 0 {
            return 0;
        }

        // A class with almost nothing on the wire goes first, so strict priority
        // never parks a stage.
        let mut guaranteed = [MIN_IN_FLIGHT_PER_CLASS; MSG_CLASSES];
        for state in self.requests.lock().await.values() {
            if let ReqState::OnWire(o) = state {
                let slot = &mut guaranteed[classify(&o.msg) as usize];
                *slot = slot.saturating_sub(1);
            }
        }
        let msgs = self.queue.pop_guaranteed(budget, &guaranteed, |m| costs.cost(m)).await;
        let mut sent = 0;
        // Popped but unsent goes back: its key is registered, so a dropped message
        // would stall its pipeline.
        let mut unsent: Vec<(u32, NetworkMessage)> = Vec::new();
        let mut written: Vec<(RequestKey, u64)> = Vec::new();
        let mut msgs = msgs.into_iter();
        for (height, msg) in msgs.by_ref() {
            let open = peers.iter().filter(|p| p.in_flight_bytes() < p.cap());
            let Some(peer) = choose_peer(open, classify(&msg)) else {
                unsent.push((height, msg));
                break;
            };
            // Marked before the write, so an answer that arrives during it is counted.
            let prior = self.mark_sent(&msg, height, peer.id()).await;
            if peer.send(&msg).await.is_ok() {
                sent += 1;
                written.extend(prior.into_iter().map(|(key, _, seq)| (key, seq)));
            } else {
                tracing::warn!(target: "dash_spv::network", "router: send to {} failed", peer.addr());
                if self.unmark_sent(prior).await {
                    unsent.push((height, msg));
                }
            }
        }
        unsent.extend(msgs);
        self.queue.push_front_all(unsent).await;

        if !written.is_empty() {
            let now = Instant::now();
            let mut reqs = self.requests.lock().await;
            for (key, seq) in written {
                if let Some(ReqState::OnWire(o)) = reqs.get_mut(&key) {
                    if o.seq == seq {
                        o.last_progress = now;
                    }
                }
            }
        }

        if sent > 0 {
            tracing::debug!(
                target: "dash_spv::network",
                "router: sent {} | peers={} queue={}",
                sent,
                peers.len(),
                self.queue.len(),
            );
        }
        sent
    }

    /// Endgame: asks a second peer for the lowest block on the wire once its peer delivered
    /// nothing for [`HEDGE_AFTER`], and returns its height. Blocks are processed in height
    /// order, so only the lowest one holds up progress and a second copy of any other is wasted.
    pub async fn hedge(&self, peers: &[PeerHandle]) -> Option<u32> {
        let (key, msg, height, peer) = {
            let reqs = self.requests.lock().await;
            let (key, o) = reqs
                .iter()
                .filter_map(|(key, state)| match state {
                    ReqState::OnWire(o) if matches!(key, RequestKey::Block(_)) => {
                        Some((key, o.as_ref()))
                    }
                    _ => None,
                })
                .min_by_key(|(_, o)| o.height)?;
            if o.hedge.is_some() || o.last_progress.elapsed() < HEDGE_AFTER {
                return None;
            }
            let peer = peers
                .iter()
                .filter(|p| p.id() != o.peer && p.in_flight_bytes() < p.cap())
                .max_by_key(|p| p.cap().saturating_sub(p.in_flight_bytes()))?;
            (key.clone(), o.msg.clone(), o.height, peer.clone())
        };
        peer.send(&msg).await.ok()?;
        match self.requests.lock().await.get_mut(&key) {
            Some(ReqState::OnWire(o)) if o.hedge.is_none() => {
                o.hedge = Some(peer.id());
                Some(height)
            }
            _ => None,
        }
    }

    #[cfg(test)]
    pub async fn mark_on_wire(&self, msg: &NetworkMessage, peer: PeerId) {
        self.mark_sized_on_wire(msg, peer, None).await;
    }

    /// A key that is already queued keeps its queued tries, size included.
    #[cfg(test)]
    pub async fn mark_sized_on_wire(
        &self,
        msg: &NetworkMessage,
        peer: PeerId,
        filters: Option<u32>,
    ) {
        let mut reqs = self.requests.lock().await;
        for key in request_keys(msg) {
            let tries = match reqs.get(&key) {
                Some(ReqState::Queued(tries)) => *tries,
                _ => Tries {
                    filters,
                    ..Tries::default()
                },
            };
            let on_wire = OnWire {
                peer,
                last_progress: Instant::now(),
                msg: msg.clone(),
                height: 0,
                hedge: None,
                tries: Tries {
                    attempts: tries.attempts + 1,
                    ..tries
                },
                seq: self.sends.fetch_add(1, Ordering::Relaxed),
                filters_received: 0,
                ended: false,
            };
            reqs.insert(key, ReqState::OnWire(Box::new(on_wire)));
        }
    }

    #[cfg(test)]
    pub async fn queued_attempts(&self, key: &RequestKey) -> Option<u32> {
        match self.requests.lock().await.get(key) {
            Some(ReqState::Queued(tries)) => Some(tries.attempts),
            _ => None,
        }
    }

    #[cfg(test)]
    pub async fn state_of(&self, key: &RequestKey) -> Option<&'static str> {
        self.requests.lock().await.get(key).map(|s| match s {
            ReqState::Queued(_) => "queued",
            ReqState::OnWire(_) => "on_wire",
            ReqState::Arrived => "arrived",
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Priority {
    Other,
    Blocks,
    CFilters,
    CfHeaders,
    Headers,
}

type MsgClass = Priority;

pub fn classify(msg: &NetworkMessage) -> Priority {
    match msg {
        NetworkMessage::GetData(inv)
            if !inv.is_empty() && inv.iter().all(|i| matches!(i, Inventory::Block(_))) =>
        {
            MsgClass::Blocks
        }
        NetworkMessage::GetCFilters(_) => MsgClass::CFilters,
        NetworkMessage::GetCFHeaders(_) => MsgClass::CfHeaders,
        NetworkMessage::GetHeaders(_) | NetworkMessage::GetHeaders2(_) => MsgClass::Headers,
        _ => MsgClass::Other,
    }
}

/// Picks the peer among `open` for a message of `class`. A header or block request goes to
/// the peer that will answer it soonest, since sync waits on each of them in height order.
/// Bulk filter traffic goes to the peer least loaded relative to its own cap, and so does
/// any request while a busy peer has no serving rate yet.
fn choose_peer<'a>(
    open: impl Iterator<Item = &'a PeerHandle> + Clone,
    class: MsgClass,
) -> Option<&'a PeerHandle> {
    choose_route_peer(open, class, &HashMap::new())
}

/// `(height, seq)`: lowest height first, then FIFO; front inserts take negative seqs.
type QueueKey = (u32, i64);

/// One queue per class, drained by strict [`DRAIN_PRIORITY`].
struct MsgQueue {
    other: Mutex<BTreeMap<QueueKey, NetworkMessage>>,
    blocks: Mutex<BTreeMap<QueueKey, NetworkMessage>>,
    cfilters: Mutex<BTreeMap<QueueKey, NetworkMessage>>,
    cfheaders: Mutex<BTreeMap<QueueKey, NetworkMessage>>,
    headers: Mutex<BTreeMap<QueueKey, NetworkMessage>>,
    back_seq: AtomicI64,
    front_seq: AtomicI64,
    len: AtomicUsize,
    notify: Notify,
}

impl MsgQueue {
    fn new() -> Self {
        Self {
            other: Mutex::new(BTreeMap::new()),
            blocks: Mutex::new(BTreeMap::new()),
            cfilters: Mutex::new(BTreeMap::new()),
            cfheaders: Mutex::new(BTreeMap::new()),
            headers: Mutex::new(BTreeMap::new()),
            back_seq: AtomicI64::new(0),
            front_seq: AtomicI64::new(-1),
            len: AtomicUsize::new(0),
            notify: Notify::new(),
        }
    }

    fn len(&self) -> usize {
        self.len.load(Ordering::SeqCst)
    }

    fn queue(&self, class: MsgClass) -> &Mutex<BTreeMap<QueueKey, NetworkMessage>> {
        match class {
            MsgClass::Other => &self.other,
            MsgClass::Blocks => &self.blocks,
            MsgClass::CFilters => &self.cfilters,
            MsgClass::CfHeaders => &self.cfheaders,
            MsgClass::Headers => &self.headers,
        }
    }

    async fn pop_n(&self, n: usize) -> Vec<(u32, NetworkMessage)> {
        self.pop_guaranteed(n as u64, &[0; MSG_CLASSES], |_| 1).await
    }

    /// Pops by `cost` until `budget` (the last may overshoot), first up to
    /// `guaranteed[class]` from each class.
    async fn pop_guaranteed(
        &self,
        budget: u64,
        guaranteed: &[usize; MSG_CLASSES],
        cost: impl Fn(&NetworkMessage) -> u64,
    ) -> Vec<(u32, NetworkMessage)> {
        let mut out: Vec<(u32, NetworkMessage)> = Vec::new();
        let mut spent = 0u64;
        for class in DRAIN_PRIORITY {
            let mut q = self.queue(class).lock().await;
            let mut take = guaranteed[class as usize];
            while take > 0 && spent < budget {
                let Some(((height, _), msg)) = q.pop_first() else {
                    break;
                };
                spent += cost(&msg);
                out.push((height, msg));
                take -= 1;
            }
        }
        for class in DRAIN_PRIORITY {
            let mut q = self.queue(class).lock().await;
            while spent < budget {
                let Some(((height, _), msg)) = q.pop_first() else {
                    break;
                };
                spent += cost(&msg);
                out.push((height, msg));
            }
        }
        self.len.fetch_sub(out.len(), Ordering::SeqCst);
        out
    }

    async fn remove(&self, matches: impl Fn(&NetworkMessage) -> bool) {
        let mut removed = 0;
        for class in DRAIN_PRIORITY {
            let mut q = self.queue(class).lock().await;
            let before = q.len();
            q.retain(|_, msg| !matches(msg));
            removed += before - q.len();
        }
        self.len.fetch_sub(removed, Ordering::SeqCst);
    }

    async fn push(&self, height: u32, msg: NetworkMessage) {
        let key = (height, self.back_seq.fetch_add(1, Ordering::SeqCst));
        self.queue(classify(&msg)).lock().await.insert(key, msg);
        self.len.fetch_add(1, Ordering::SeqCst);
        self.notify.notify_one();
    }

    async fn push_front_all(&self, msgs: Vec<(u32, NetworkMessage)>) {
        if msgs.is_empty() {
            return;
        }
        let n = msgs.len();
        for (height, msg) in msgs.into_iter().rev() {
            let key = (height, self.front_seq.fetch_sub(1, Ordering::SeqCst));
            self.queue(classify(&msg)).lock().await.insert(key, msg);
        }
        self.len.fetch_add(n, Ordering::SeqCst);
        self.notify.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::costs::Ledger as CostLedger;
    use crate::network::peer::ConnectedPeer;
    use crate::test_utils::test_socket_address;
    use dash_clock::NodeSeconds;
    use dashcore::hash_types::FilterHeader;
    use dashcore::hash_types::{ChainLockHash, InstantSendLockHash};
    use dashcore::network::message_blockdata::GetHeadersMessage;
    use dashcore::network::message_bloom::BloomFlags;
    use dashcore::network::message_bloom::FilterLoad;
    use dashcore::network::message_filter::{CFHeaders, CFilter, GetCFHeaders, GetCFilters};
    use dashcore::network::message_qrinfo::{GetQRInfo, QRInfo};
    use dashcore::network::message_sml::{GetMnListDiff, MnListDiff};
    use dashcore::{Block, Header, Transaction, Txid};
    use std::cell::RefCell;
    use std::ops::Deref;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use test_case::test_case;

    #[derive(Clone)]
    struct ManualClock(Arc<LedgerMutex<SteadyInstant>>);

    impl ManualClock {
        fn new() -> Self {
            Self(Arc::new(LedgerMutex::new(SteadyInstant::now())))
        }
        fn advance(&self, duration: Duration) {
            let mut now = self.0.lock();
            *now += duration;
        }
    }

    impl Clock for ManualClock {
        fn node_now(&self) -> NodeSeconds {
            NodeSeconds(0)
        }
        fn steady(&self) -> SteadyInstant {
            *self.0.lock()
        }
    }

    struct TestRoutePeer {
        id: PeerId,
        bytes: u64,
        cap: u64,
        rate: u64,
        blocked: bool,
    }

    impl TestRoutePeer {
        fn new(n: u8) -> Self {
            Self {
                id: conn(n, 0),
                bytes: 0,
                cap: 1 << 30,
                rate: 0,
                blocked: false,
            }
        }
    }

    impl RoutePeer for TestRoutePeer {
        fn id(&self) -> PeerId {
            self.id
        }
        fn blocked(&self) -> bool {
            self.blocked
        }
        fn in_flight_bytes(&self) -> u64 {
            self.bytes
        }
        fn cap(&self) -> u64 {
            self.cap
        }
        fn serving_rate(&self) -> u64 {
            self.rate
        }
    }

    #[derive(Default)]
    struct CreditState {
        free: usize,
        held: HashMap<SubId, usize>,
        wants: HashSet<SubId>,
        marker_queued: HashSet<SubId>,
        markers: HashMap<SubId, usize>,
        failures: Vec<(SubId, RequestFailed)>,
    }

    /// The hub's credit contract under one mutex: a want is recorded in the same step as a
    /// refused acquire, and a release queues at most one marker per wanting owner.
    #[derive(Default)]
    struct TestCredits {
        state: LedgerMutex<CreditState>,
        /// Runs inside the next successful `acquire_or_want`, without the credit lock.
        on_acquire: LedgerMutex<Option<Box<dyn FnOnce() + Send>>>,
    }

    impl TestCredits {
        fn new(free: usize) -> Arc<Self> {
            let credits = Self::default();
            credits.state.lock().free = free;
            Arc::new(credits)
        }

        fn free(&self) -> usize {
            self.state.lock().free
        }

        fn held(&self, owner: SubId) -> usize {
            self.state.lock().held.get(&owner).copied().unwrap_or(0)
        }

        fn markers(&self, owner: SubId) -> usize {
            self.state.lock().markers.get(&owner).copied().unwrap_or(0)
        }

        /// Reads the owner's queued marker, as `recv` does.
        fn recv_marker(&self, owner: SubId) {
            let mut state = self.state.lock();
            state.marker_queued.remove(&owner);
            state.markers.remove(&owner);
        }

        /// Reads the owner's failures, as `recv` does, which returns their credits.
        fn take_failures(&self, owner: SubId) -> Vec<RequestFailed> {
            let failures: Vec<_> = {
                let mut state = self.state.lock();
                let (mine, rest) =
                    mem::take(&mut state.failures).into_iter().partition(|(o, _)| *o == owner);
                state.failures = rest;
                mine
            };
            for _ in &failures {
                self.release(owner);
            }
            failures.into_iter().map(|(_, f)| f).collect()
        }
    }

    impl Credits for TestCredits {
        fn acquire_or_want(&self, owner: SubId) -> bool {
            let acquired = {
                let mut state = self.state.lock();
                if state.free == 0 {
                    state.wants.insert(owner);
                    false
                } else {
                    state.free -= 1;
                    *state.held.entry(owner).or_default() += 1;
                    true
                }
            };
            let hook = if acquired {
                self.on_acquire.lock().take()
            } else {
                None
            };
            if let Some(hook) = hook {
                hook();
            }
            acquired
        }

        fn release(&self, owner: SubId) {
            let mut state = self.state.lock();
            state.free += 1;
            if let Some(held) = state.held.get_mut(&owner) {
                *held -= 1;
            }
            for wanting in mem::take(&mut state.wants) {
                if state.marker_queued.insert(wanting) {
                    *state.markers.entry(wanting).or_default() += 1;
                }
            }
        }

        fn failed(&self, owner: SubId, failure: RequestFailed) {
            self.state.lock().failures.push((owner, failure));
        }
    }

    /// A ledger over a `TestCredits` pool.
    struct TestLedger {
        ledger: RequestLedger,
        credits: Arc<TestCredits>,
    }

    impl Deref for TestLedger {
        type Target = RequestLedger;

        fn deref(&self) -> &RequestLedger {
            &self.ledger
        }
    }

    impl TestLedger {
        fn take_failures(&self, owner: SubId) -> Vec<RequestFailed> {
            self.credits.take_failures(owner)
        }
    }

    fn ledger_with(free: usize) -> (TestLedger, ManualClock) {
        let clock = ManualClock::new();
        let credits = TestCredits::new(free);
        let ledger = RequestLedger::new(Arc::new(clock.clone()), credits.clone());
        (
            TestLedger {
                ledger,
                credits,
            },
            clock,
        )
    }

    fn pure_ledger() -> (TestLedger, ManualClock) {
        ledger_with(1 << 20)
    }

    const TIMEOUT: Duration = Duration::from_secs(10);

    /// Reserves one send without pushing it.
    fn reserve_one(
        ledger: &RequestLedger,
        peers: &[TestRoutePeer],
    ) -> (PeerId, AttemptId, NetworkMessage) {
        let mut routes = ledger.route(peers, u64::MAX, &ResponseCosts::new());
        assert_eq!(routes.len(), 1);
        routes.remove(0)
    }

    /// Pushes a reserved send with no charge records and returns whether it went out.
    fn send(ledger: &RequestLedger, peer: PeerId, attempt: AttemptId) -> bool {
        ledger.attach(attempt, peer, Vec::new()).is_some_and(|reservation| {
            reservation.send_if_live(TIMEOUT, |_, _| {}) == SendOutcome::Sent
        })
    }

    fn send_all(ledger: &RequestLedger, routes: &[(PeerId, AttemptId, NetworkMessage)]) {
        for (peer, attempt, _) in routes {
            assert!(send(ledger, *peer, *attempt));
        }
    }

    /// Reserves and pushes one send.
    fn route_one(
        ledger: &RequestLedger,
        peers: &[TestRoutePeer],
    ) -> (PeerId, AttemptId, NetworkMessage) {
        let route = reserve_one(ledger, peers);
        assert!(send(ledger, route.0, route.1));
        route
    }

    fn hedge_one(
        ledger: &RequestLedger,
        peers: &[TestRoutePeer],
    ) -> Option<(PeerId, AttemptId, NetworkMessage)> {
        let route = ledger.hedge(peers, &ResponseCosts::new())?;
        assert!(send(ledger, route.0, route.1));
        Some(route)
    }

    fn culprits(ledger: &RequestLedger) -> HashSet<PeerId> {
        ledger.expire().culprits
    }

    fn req(
        ledger: &RequestLedger,
        owner: SubId,
        height: u32,
        msg: NetworkMessage,
        class: Priority,
    ) -> RequestId {
        ledger.request(owner, height, msg, class).unwrap()
    }

    fn req_filters(
        ledger: &RequestLedger,
        owner: SubId,
        request: GetCFilters,
        filters: u32,
    ) -> RequestId {
        ledger.request_filters(owner, request, filters).unwrap()
    }

    /// Returns the request and send a response is tagged with.
    fn hit(
        ledger: &RequestLedger,
        peer: PeerId,
        msg: &NetworkMessage,
    ) -> Option<(RequestId, AttemptId)> {
        tag_of(ledger, peer, msg).map(|tag| (tag.id, tag.attempt))
    }

    fn tag_of(ledger: &RequestLedger, peer: PeerId, msg: &NetworkMessage) -> Option<Tag> {
        ledger.response(peer, msg)?.tag
    }

    fn filter_reply(hash: u32) -> NetworkMessage {
        NetworkMessage::CFilter(CFilter {
            filter_type: 0,
            block_hash: BlockHash::dummy(hash),
            filter: vec![0],
        })
    }

    #[test]
    fn request_ids_dedup_until_answered_and_attempt_ids_change_on_retry() {
        let (ledger, _) = pure_ledger();
        let owner = SubId(7);
        let id = req(&ledger, owner, 0, get_headers(1), Priority::Headers);
        assert_eq!(req(&ledger, owner, 99, get_headers(1), Priority::Other), id);
        let peers = [TestRoutePeer::new(1), TestRoutePeer::new(2)];
        let (first, attempt, _) = route_one(&ledger, &peers);
        assert_eq!(req(&ledger, owner, 0, get_headers(1), Priority::Headers), id);
        ledger.disconnected(first);
        let (second, retry, _) = reserve_one(&ledger, &peers);
        assert_ne!(first, second);
        assert_ne!(attempt, retry);
        assert!(!send(&ledger, first, attempt));
        assert!(send(&ledger, second, retry));
        assert_eq!(hit(&ledger, second, &NetworkMessage::Headers(vec![])), Some((id, retry)));
        assert_eq!(req(&ledger, owner, 0, get_headers(1), Priority::Headers), id);
        assert!(ledger.request_answered(id));
        assert_ne!(req(&ledger, owner, 0, get_headers(1), Priority::Headers), id);
    }

    #[test]
    fn route_reservations_bound_each_peer_and_the_global_budget() {
        let (ledger, _) = pure_ledger();
        let costs = ResponseCosts::new();
        let cost = costs.cost(&get_cfilters(0));
        let mut peers = [TestRoutePeer::new(1), TestRoutePeer::new(2)];
        for peer in &mut peers {
            peer.cap = cost;
        }
        for n in 0..4 {
            req_filters(&ledger, SubId(1), range(n, 9), 1);
        }
        let routes = ledger.route(&peers, cost, &costs);
        assert_eq!(routes.len(), 1);
        assert!(
            ledger.route(&peers, cost, &costs).is_empty(),
            "a pending write uses global capacity"
        );
        let more = ledger.route(&peers, cost * 2, &costs);
        assert_eq!(more.len(), 1);
        assert_ne!(routes[0].0, more[0].0, "a pending write uses its connection's capacity");
        assert!(ledger.route(&peers, u64::MAX, &costs).is_empty());
        assert!(ledger.attach(routes[0].1, routes[0].0, Vec::new()).is_some());
        peers[0].bytes = cost;
        assert!(ledger.route(&peers, u64::MAX, &costs).is_empty());
    }

    #[test]
    fn pure_routes_keep_class_floors_priority_height_and_fifo() {
        let (ledger, _) = pure_ledger();
        let costs = ResponseCosts::new();
        for n in (0..6).rev() {
            req(&ledger, SubId(1), n, get_headers(n), Priority::Headers);
        }
        req(&ledger, SubId(1), 10, get_cfheaders(10), Priority::Other);
        let routes = ledger.route(&[TestRoutePeer::new(1)], u64::MAX, &costs);
        assert_eq!(routes.len(), 7);
        assert_eq!(routes[0].2, get_cfheaders(10), "the explicit class controls routing");
        for n in 0..6 {
            assert_eq!(routes[n + 1].2, get_headers(n as u32));
        }
        assert!(ledger.route(&[TestRoutePeer::new(1)], u64::MAX, &costs).is_empty());
    }

    #[test]
    fn response_tags_cover_blocks_cfheaders_mnlistdiff_and_headers() {
        let (ledger, _) = pure_ledger();
        let peers = [TestRoutePeer::new(1)];
        let block = Block::dummy(1, vec![]);
        let mut header = Header::dummy(3);
        header.prev_blockhash = BlockHash::dummy(8);
        let diff = MnListDiff::dummy_between(1, 4);
        let cases = [
            (
                NetworkMessage::GetData(vec![Inventory::Block(block.block_hash())]),
                NetworkMessage::Block(block),
            ),
            (
                get_cfheaders(2),
                NetworkMessage::CFHeaders(CFHeaders {
                    filter_type: 0,
                    stop_hash: BlockHash::dummy(2),
                    previous_filter_header: FilterHeader::all_zeros(),
                    filter_hashes: vec![],
                }),
            ),
            (get_mnlistdiff(1), NetworkMessage::MnListDiff(Box::new(diff))),
            (get_headers(8), NetworkMessage::Headers(vec![header])),
        ];
        for (request, response) in cases {
            let id = req(&ledger, SubId(1), 0, request.clone(), classify(&request));
            let (peer, attempt, _) = route_one(&ledger, &peers);
            assert_eq!(hit(&ledger, conn(2, 0), &response), None);
            assert_eq!(hit(&ledger, peer, &response), Some((id, attempt)));
            assert_eq!(hit(&ledger, peer, &response), None);
            assert!(ledger.request_answered(id));
        }
        assert!(ledger.is_empty());
    }

    #[test]
    fn empty_headers_match_the_connections_oldest_attempt_in_send_order() {
        let (ledger, _) = pure_ledger();
        let peer = TestRoutePeer::new(1);
        let first = req(&ledger, SubId(1), 20, get_headers(1), Priority::Headers);
        let second = req(&ledger, SubId(1), 10, get_headers(2), Priority::Headers);
        let routes = ledger.route(&[peer], u64::MAX, &ResponseCosts::new());
        assert_eq!(routes.len(), 2);
        send_all(&ledger, &routes);
        let empty = NetworkMessage::Headers(vec![]);
        assert_eq!(
            hit(&ledger, conn(1, 1), &empty),
            None,
            "replacement connections cannot answer old attempts"
        );
        assert_eq!(hit(&ledger, conn(1, 0), &empty), Some((second, routes[0].1)));
        assert_eq!(hit(&ledger, conn(1, 0), &empty), Some((first, routes[1].1)));
        assert_eq!(hit(&ledger, conn(1, 0), &empty), None);
    }

    #[test]
    fn streaming_filters_match_the_oldest_range_and_complete_only_the_whole_batch() {
        let (ledger, _) = pure_ledger();
        let owner = SubId(1);
        let second = req_filters(
            &ledger,
            owner,
            GetCFilters {
                filter_type: 0,
                start_height: 2,
                stop_hash: BlockHash::dummy(3),
            },
            2,
        );
        let first = req_filters(
            &ledger,
            owner,
            GetCFilters {
                filter_type: 0,
                start_height: 0,
                stop_hash: BlockHash::dummy(1),
            },
            2,
        );
        let routes = ledger.route(&[TestRoutePeer::new(1)], u64::MAX, &ResponseCosts::new());
        assert_eq!(routes.len(), 2);
        send_all(&ledger, &routes);
        assert_eq!(hit(&ledger, conn(2, 0), &filter_reply(0)), None);
        for n in 0..4 {
            let (id, attempt) = if n < 2 {
                (first, routes[0].1)
            } else {
                (second, routes[1].1)
            };
            assert_eq!(hit(&ledger, conn(1, 0), &filter_reply(n)), Some((id, attempt)));
        }
        assert_eq!(hit(&ledger, conn(1, 0), &filter_reply(3)), None);
        assert_eq!(req_filters(&ledger, owner, range(0, 1), 2), first);
    }

    #[test]
    fn multiple_block_keys_complete_individually_under_one_request_id() {
        let (ledger, _) = pure_ledger();
        let first = Block::dummy(1, vec![]);
        let second = Block::dummy(2, vec![]);
        let msg = NetworkMessage::GetData(vec![
            Inventory::Block(first.block_hash()),
            Inventory::Block(second.block_hash()),
        ]);
        let id = req(&ledger, SubId(1), 0, msg.clone(), Priority::Blocks);
        let (peer, attempt, _) = route_one(&ledger, &[TestRoutePeer::new(1)]);
        assert_eq!(hit(&ledger, peer, &NetworkMessage::Block(first.clone())), Some((id, attempt)));
        assert_eq!(hit(&ledger, peer, &NetworkMessage::Block(first)), None);
        assert_eq!(req(&ledger, SubId(1), 0, msg, Priority::Blocks), id);
        assert_eq!(hit(&ledger, peer, &NetworkMessage::Block(second)), Some((id, attempt)));
        assert!(ledger.request_answered(id));
        assert!(ledger.is_empty());
    }

    #[test]
    fn hedge_has_a_distinct_attempt_id_and_the_first_response_wins() {
        let (ledger, clock) = pure_ledger();
        let block = Block::dummy(1, vec![]);
        let msg = NetworkMessage::GetData(vec![Inventory::Block(block.block_hash())]);
        let id = req(&ledger, SubId(1), 0, msg, Priority::Blocks);
        let peers = [TestRoutePeer::new(1), TestRoutePeer::new(2)];
        let (peer, first, _) = route_one(&ledger, &peers[..1]);
        assert!(ledger.hedge(&peers, &ResponseCosts::new()).is_none());
        clock.advance(HEDGE_AFTER);
        let (other, hedge, _) =
            hedge_one(&ledger, &peers).expect("stalled lowest block can be hedged");
        assert_ne!(peer, other);
        assert_ne!(first, hedge);
        assert!(ledger.hedge(&peers, &ResponseCosts::new()).is_none());
        assert_eq!(hit(&ledger, other, &NetworkMessage::Block(block.clone())), Some((id, hedge)));
        assert_eq!(hit(&ledger, peer, &NetworkMessage::Block(block)), None);
    }

    fn get_blocks(hashes: &[u32]) -> NetworkMessage {
        NetworkMessage::GetData(hashes.iter().map(|n| block(*n)).collect())
    }

    #[test]
    fn duplicate_at_exhausted_credit_returns_id() {
        let (ledger, _) = ledger_with(1);
        let id = req(&ledger, SubId(1), 0, get_blocks(&[1, 2]), Priority::Blocks);
        assert_eq!(ledger.credits.free(), 0);
        assert_eq!(ledger.request(SubId(1), 5, get_blocks(&[1, 2]), Priority::Other), Ok(id));
        assert_eq!(
            ledger.request(SubId(1), 0, get_blocks(&[3]), Priority::Blocks),
            Err(RequestError::NoCredit)
        );
        assert_eq!(ledger.credits.held(SubId(1)), 1);
    }

    #[test]
    fn same_owner_partial_overlap_is_overlap() {
        let (ledger, _) = ledger_with(8);
        req(&ledger, SubId(1), 0, get_blocks(&[1, 2]), Priority::Blocks);
        for keys in [&[1][..], &[1, 3], &[1, 2, 3], &[2, 1, 3]] {
            assert_eq!(
                ledger.request(SubId(1), 0, get_blocks(keys), Priority::Blocks),
                Err(RequestError::Overlap),
                "{keys:?}"
            );
        }
        assert_eq!(ledger.credits.free(), 7);
    }

    #[test]
    fn mixed_owner_overlap_is_overlap() {
        let (ledger, _) = ledger_with(8);
        req(&ledger, SubId(1), 0, get_blocks(&[1]), Priority::Blocks);
        req(&ledger, SubId(2), 0, get_blocks(&[2]), Priority::Blocks);
        for (owner, keys) in [(SubId(2), &[1, 3][..]), (SubId(3), &[1, 2]), (SubId(1), &[1, 2])] {
            assert_eq!(
                ledger.request(owner, 0, get_blocks(keys), Priority::Blocks),
                Err(RequestError::Overlap),
                "{owner:?} {keys:?}"
            );
        }
        assert_eq!(ledger.credits.free(), 6);
    }

    #[test]
    fn other_owner_identical_is_key_in_use() {
        let (ledger, _) = ledger_with(8);
        req(&ledger, SubId(1), 0, get_headers(1), Priority::Headers);
        assert_eq!(
            ledger.request(SubId(2), 0, get_headers(1), Priority::Headers),
            Err(RequestError::KeyInUse)
        );
        assert_eq!(ledger.credits.held(SubId(2)), 0);
    }

    #[test]
    fn rejected_admission_returns_credit() {
        let (ledger, _) = ledger_with(2);
        req(&ledger, SubId(1), 0, get_blocks(&[1, 2]), Priority::Blocks);
        for _ in 0..4 {
            let _ = ledger.request(SubId(1), 0, get_blocks(&[2, 3]), Priority::Blocks);
            let _ = ledger.request(SubId(2), 0, get_blocks(&[1, 2]), Priority::Blocks);
            let too_many = NetworkMessage::GetData((10..27).map(block).collect());
            let _ = ledger.request(SubId(1), 0, too_many, Priority::Blocks);
        }
        assert_eq!(ledger.credits.free(), 1);
    }

    #[test]
    fn racing_admission_rolls_back() {
        let clock = ManualClock::new();
        let credits = TestCredits::new(4);
        let ledger = Arc::new(RequestLedger::new(Arc::new(clock), credits.clone()));
        let racer = ledger.clone();
        *credits.on_acquire.lock() = Some(Box::new(move || {
            racer.request(SubId(2), 0, get_headers(1), Priority::Headers).unwrap();
        }));
        assert_eq!(
            ledger.request(SubId(1), 0, get_headers(1), Priority::Headers),
            Err(RequestError::KeyInUse)
        );
        assert_eq!(credits.free(), 3, "only the racing request holds a credit");
        assert_eq!(credits.held(SubId(1)), 0);
        assert_eq!(ledger.len(), 1);

        let racer = ledger.clone();
        *credits.on_acquire.lock() = Some(Box::new(move || {
            racer.request(SubId(1), 0, get_headers(2), Priority::Headers).unwrap();
        }));
        let first = ledger.request(SubId(1), 0, get_headers(2), Priority::Headers).unwrap();
        assert_eq!(credits.free(), 2, "a same-owner race returns the winner's id and one credit");
        assert_eq!(ledger.request(SubId(1), 0, get_headers(2), Priority::Headers), Ok(first));
    }

    #[test]
    fn no_credit_window_release_wakes_once() {
        let clock = ManualClock::new();
        let credits = TestCredits::new(1);
        let ledger = Arc::new(RequestLedger::new(Arc::new(clock), credits.clone()));
        let held = ledger.request(SubId(1), 0, get_headers(1), Priority::Headers).unwrap();
        let window = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));
        let (w, r) = (window.clone(), resume.clone());
        ledger.set_hooks(LedgerHooks {
            after_no_credit: Some(Box::new(move |_| {
                w.wait();
                r.wait();
            })),
        });
        let requester = ledger.clone();
        let deferred = thread::spawn(move || {
            requester.request(SubId(2), 0, get_headers(2), Priority::Headers)
        });
        window.wait();
        assert!(ledger.cancel(held));
        resume.wait();
        assert_eq!(deferred.join().unwrap(), Err(RequestError::NoCredit));
        assert_eq!(credits.markers(SubId(2)), 1, "the release in the window woke the owner once");
    }

    #[test]
    fn repeated_defer_release_one_marker() {
        let (ledger, _) = ledger_with(1);
        for n in 0..5 {
            let id = req(&ledger, SubId(1), 0, get_headers(n), Priority::Headers);
            assert_eq!(
                ledger.request(SubId(2), 0, get_headers(100), Priority::Headers),
                Err(RequestError::NoCredit)
            );
            assert!(ledger.cancel(id));
            assert_eq!(ledger.credits.markers(SubId(2)), 1);
        }
        ledger.credits.recv_marker(SubId(2));
        let id = req(&ledger, SubId(1), 0, get_headers(9), Priority::Headers);
        assert_eq!(
            ledger.request(SubId(2), 0, get_headers(100), Priority::Headers),
            Err(RequestError::NoCredit)
        );
        ledger.cancel(id);
        assert_eq!(ledger.credits.markers(SubId(2)), 1, "a later want and release queue a new one");
    }

    #[test]
    fn credits_return_on_every_exit() {
        let (ledger, clock) = ledger_with(8);
        let peers = [TestRoutePeer::new(1), TestRoutePeer::new(2)];
        let answered = req(&ledger, SubId(1), 0, get_headers(1), Priority::Headers);
        let (peer, _, _) = route_one(&ledger, &peers);
        assert!(tag_of(&ledger, peer, &NetworkMessage::Headers(vec![])).unwrap().last);
        assert_eq!(ledger.credits.held(SubId(1)), 1, "held until the owner answers");
        ledger.request_answered(answered);
        assert_eq!(ledger.credits.held(SubId(1)), 0);

        let (blocks, msg) = dummy_blocks(2);
        let late = req(&ledger, SubId(1), 0, msg, Priority::Blocks);
        let (peer, _, _) = route_one(&ledger, &peers);
        tag_of(&ledger, peer, &NetworkMessage::Block(blocks[0].clone()));
        ledger.request_answered(late);
        assert_eq!(ledger.credits.held(SubId(1)), 1, "a member is still pending");
        tag_of(&ledger, peer, &NetworkMessage::Block(blocks[1].clone()));
        assert_eq!(ledger.credits.held(SubId(1)), 0, "the last member's arrival returns it");

        let cancelled = req(&ledger, SubId(1), 0, get_headers(3), Priority::Headers);
        ledger.cancel(cancelled);
        assert_eq!(ledger.credits.held(SubId(1)), 0);

        let failed = req(&ledger, SubId(1), 0, get_headers(4), Priority::Headers);
        for _ in 0..3 {
            route_one(&ledger, &peers);
            clock.advance(TIMEOUT * 2);
            ledger.expire();
        }
        assert_eq!(ledger.credits.held(SubId(1)), 1, "held while the failure is unread");
        assert_eq!(ledger.take_failures(SubId(1))[0].id, failed);
        assert_eq!(ledger.credits.held(SubId(1)), 0);
        assert!(ledger.is_empty());
        assert_eq!(ledger.credits.free(), 8);
    }

    #[test]
    fn owner_gone_cancels_every_entry() {
        let (ledger, _) = ledger_with(8);
        for n in 0..3 {
            req(&ledger, SubId(1), 0, get_headers(n), Priority::Headers);
        }
        let kept = req(&ledger, SubId(2), 0, get_headers(9), Priority::Headers);
        let routes = ledger.route(&[TestRoutePeer::new(1)], u64::MAX, &ResponseCosts::new());
        send_all(&ledger, &routes);
        ledger.cancel_owner(SubId(1));
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger.credits.held(SubId(1)), 0);
        assert_eq!(ledger.request(SubId(2), 0, get_headers(9), Priority::Headers), Ok(kept));
    }

    #[test]
    fn three_attempts_then_one_failure() {
        for peer_count in [1, 3] {
            let (ledger, clock) = pure_ledger();
            let owner = SubId(7);
            let id = req(&ledger, owner, 0, get_headers(1), Priority::Headers);
            let peers: Vec<_> = (1..=peer_count).map(TestRoutePeer::new).collect();
            let mut tried = HashSet::new();
            for n in 0..3 {
                let (peer, _, _) = route_one(&ledger, &peers);
                tried.insert(peer);
                clock.advance(Duration::from_secs(11));
                let culprits = culprits(&ledger);
                assert_eq!(
                    culprits,
                    if n < 2 {
                        HashSet::from([peer])
                    } else {
                        HashSet::new()
                    }
                );
                if peer_count == 1 {
                    assert!(
                        ledger.route(&peers, u64::MAX, &ResponseCosts::new()).is_empty(),
                        "the timed-out send holds its key until its orphan expires"
                    );
                    clock.advance(Duration::from_secs(11));
                    assert_eq!(ledger.expire(), Expired::default(), "the orphan expires silently");
                }
                assert!(ledger.take_failures(SubId(8)).is_empty());
                if n < 2 {
                    assert!(ledger.take_failures(owner).is_empty());
                }
            }
            assert_eq!(tried.len(), peer_count as usize, "fresh connections are preferred");
            assert_eq!(
                ledger.take_failures(owner),
                vec![RequestFailed {
                    id,
                    failed: vec![(LedgerKey::Headers(BlockHash::dummy(1)), FailReason::Timeout)],
                    arrived: vec![],
                    sends: 3,
                    last: FailReason::Timeout
                }]
            );
            assert!(ledger.is_empty());
            clock.advance(Duration::from_secs(60));
            ledger.expire();
            for peer in &peers {
                ledger.disconnected(peer.id());
            }
            assert!(ledger.take_failures(owner).is_empty());
            assert!(ledger.route(&peers, u64::MAX, &ResponseCosts::new()).is_empty());
            assert_ne!(req(&ledger, owner, 0, get_headers(1), Priority::Headers), id);
        }
    }

    #[test]
    fn disconnects_and_failed_writes_share_the_attempt_cap() {
        let (ledger, _) = pure_ledger();
        let owner = SubId(7);
        let id = req(&ledger, owner, 0, get_headers(1), Priority::Headers);
        let peers = [TestRoutePeer::new(1)];
        let (peer, first, _) = route_one(&ledger, &peers);
        ledger.disconnected(conn(1, 1));
        assert!(ledger.route(&peers, u64::MAX, &ResponseCosts::new()).is_empty());
        ledger.disconnected(peer);
        ledger.disconnected(peer);
        let (_, second, _) = route_one(&ledger, &peers);
        ledger.unmark_sent(first);
        ledger.unmark_sent(second);
        ledger.unmark_sent(second);
        let (_, third, _) = route_one(&ledger, &peers);
        ledger.unmark_sent(third);
        assert_eq!(
            ledger.take_failures(owner),
            vec![RequestFailed {
                id,
                failed: vec![(LedgerKey::Headers(BlockHash::dummy(1)), FailReason::WriteFailed)],
                arrived: vec![],
                sends: 3,
                last: FailReason::WriteFailed
            }]
        );
        assert!(ledger.take_failures(owner).is_empty());
    }

    #[test]
    fn a_hedge_counts_toward_the_send_cap() {
        let (ledger, clock) = pure_ledger();
        let owner = SubId(7);
        let id = req(&ledger, owner, 0, NetworkMessage::GetData(vec![block(1)]), Priority::Blocks);
        let peers = [TestRoutePeer::new(1), TestRoutePeer::new(2)];
        let (peer, first, _) = route_one(&ledger, &peers[..1]);
        clock.advance(HEDGE_AFTER);
        let (other, hedge, _) = ledger.hedge(&peers, &ResponseCosts::new()).unwrap();
        ledger.disconnected(peer);
        assert!(ledger.route(&peers, u64::MAX, &ResponseCosts::new()).is_empty());
        ledger.disconnected(other);
        assert_ne!(first, hedge);
        let (_, attempt, _) = route_one(&ledger, &peers);
        assert!(ledger.take_failures(owner).is_empty());
        ledger.unmark_sent(attempt);
        assert!(ledger.route(&peers, u64::MAX, &ResponseCosts::new()).is_empty());
        assert_eq!(
            ledger.take_failures(owner),
            vec![RequestFailed {
                id,
                failed: vec![(LedgerKey::Block(BlockHash::dummy(1)), FailReason::WriteFailed)],
                arrived: vec![],
                sends: 3,
                last: FailReason::WriteFailed
            }]
        );
    }

    #[test]
    fn an_answered_range_is_never_resent() {
        let (ledger, clock) = pure_ledger();
        let owner = SubId(1);
        let id = req_filters(
            &ledger,
            owner,
            GetCFilters {
                filter_type: 0,
                start_height: 0,
                stop_hash: BlockHash::dummy(1),
            },
            2,
        );
        let peers = [TestRoutePeer::new(1)];
        let (peer, attempt, _) = route_one(&ledger, &peers);
        hit(&ledger, peer, &filter_reply(0));
        assert_eq!(hit(&ledger, peer, &filter_reply(1)), Some((id, attempt)));
        clock.advance(Duration::from_secs(60));
        assert!(culprits(&ledger).is_empty());
        assert!(ledger.route(&peers, u64::MAX, &ResponseCosts::new()).is_empty());
        assert!(ledger.take_failures(owner).is_empty());
    }

    #[test]
    fn a_withdrawn_reservation_is_not_a_send() {
        let (ledger, _) = pure_ledger();
        let owner = SubId(1);
        let id = req(&ledger, owner, 0, get_headers(1), Priority::Headers);
        let peers = [TestRoutePeer::new(1)];
        for _ in 0..5 {
            let (_, attempt, _) = reserve_one(&ledger, &peers);
            ledger.unreserve(attempt);
            ledger.unreserve(attempt);
        }
        for _ in 0..3 {
            let (_, attempt, _) = route_one(&ledger, &peers);
            assert!(ledger.take_failures(owner).is_empty());
            ledger.unmark_sent(attempt);
        }
        assert_eq!(
            ledger.take_failures(owner),
            vec![RequestFailed {
                id,
                failed: vec![(LedgerKey::Headers(BlockHash::dummy(1)), FailReason::WriteFailed)],
                arrived: vec![],
                sends: 3,
                last: FailReason::WriteFailed
            }]
        );
    }

    #[test]
    fn unreserve_after_the_send_ended_keeps_it_counted() {
        let (ledger, _) = pure_ledger();
        let owner = SubId(1);
        req(&ledger, owner, 0, get_headers(1), Priority::Headers);
        let peers = [TestRoutePeer::new(1)];
        let (peer, first, _) = route_one(&ledger, &peers);
        ledger.disconnected(peer);
        ledger.unreserve(first);
        let (_, second, _) = route_one(&ledger, &peers);
        ledger.unreserve(second);
        ledger.unmark_sent(second);
        let (_, third, _) = route_one(&ledger, &peers);
        ledger.unmark_sent(third);
        assert_eq!(ledger.take_failures(owner)[0].sends, 3);
    }

    #[test]
    fn a_withdrawn_hedge_can_be_reserved_again() {
        let (ledger, clock) = pure_ledger();
        req(&ledger, SubId(1), 0, NetworkMessage::GetData(vec![block(1)]), Priority::Blocks);
        let peers = [TestRoutePeer::new(1), TestRoutePeer::new(2)];
        route_one(&ledger, &peers[..1]);
        clock.advance(HEDGE_AFTER);
        let (_, hedge, _) = ledger.hedge(&peers, &ResponseCosts::new()).unwrap();
        ledger.unreserve(hedge);
        let (_, again, _) = ledger.hedge(&peers, &ResponseCosts::new()).unwrap();
        assert_ne!(hedge, again);
        assert!(ledger.hedge(&peers, &ResponseCosts::new()).is_none());
    }

    #[test]
    fn stop_filter_with_wrong_count_ends_send() {
        let (ledger, clock) = pure_ledger();
        let owner = SubId(1);
        let id = req_filters(&ledger, owner, range(0, 1), 2);
        let peers = [TestRoutePeer::new(1)];
        for n in 0..3 {
            let (peer, attempt, _) = route_one(&ledger, &peers);
            let tag = tag_of(&ledger, peer, &filter_reply(1)).unwrap();
            assert_eq!((tag.id, tag.attempt, tag.last), (id, attempt, false));
            assert_eq!(hit(&ledger, peer, &filter_reply(0)), None, "the send has ended");
            clock.advance(Duration::from_secs(11));
            assert!(culprits(&ledger).is_empty());
            if n < 2 {
                assert!(ledger.take_failures(owner).is_empty());
            }
        }
        assert_eq!(
            ledger.take_failures(owner),
            vec![RequestFailed {
                id,
                failed: vec![(LedgerKey::CFilters(0), FailReason::Incomplete)],
                arrived: vec![],
                sends: 3,
                last: FailReason::Incomplete
            }]
        );
    }

    #[test]
    fn partial_primary_then_complete_retry() {
        let (ledger, clock) = pure_ledger();
        let id = req_filters(&ledger, SubId(1), range(0, 2), 3);
        let peers = [TestRoutePeer::new(1), TestRoutePeer::new(2)];
        let (primary, first, _) = route_one(&ledger, &peers[..1]);
        assert_eq!(hit(&ledger, primary, &filter_reply(0)), Some((id, first)));
        clock.advance(Duration::from_secs(11));
        assert_eq!(culprits(&ledger), HashSet::from([primary]));
        let (retry_peer, retry, msg) = route_one(&ledger, &peers);
        assert_ne!(retry_peer, primary);
        assert_eq!(msg, NetworkMessage::GetCFilters(range(0, 2)), "a range is re-sent whole");
        for n in 0..2 {
            let tag = tag_of(&ledger, retry_peer, &filter_reply(n)).unwrap();
            assert_eq!((tag.attempt, tag.last), (retry, false));
        }
        let tag = tag_of(&ledger, retry_peer, &filter_reply(2)).unwrap();
        assert_eq!(
            tag,
            Tag {
                id,
                attempt: retry,
                member: LedgerKey::CFilters(0),
                last: true
            },
            "the retry's own three filters complete the range"
        );
    }

    #[test]
    fn a_timed_out_headers_send_absorbs_its_late_empty_reply() {
        let (ledger, clock) = pure_ledger();
        let peers = [TestRoutePeer::new(1)];
        let first = req(&ledger, SubId(1), 0, get_headers(1), Priority::Headers);
        let second = req(&ledger, SubId(1), 1, get_headers(2), Priority::Headers);
        let routes = ledger.route(&peers, u64::MAX, &ResponseCosts::new());
        assert_eq!(routes.len(), 2);
        let ((peer, timed_out, msg), (_, later, _)) = (&routes[0], &routes[1]);
        assert_eq!(msg, &get_headers(1));
        assert!(send(&ledger, *peer, *timed_out));
        clock.advance(Duration::from_secs(5));
        assert!(send(&ledger, *peer, *later));
        clock.advance(Duration::from_secs(6));
        assert_eq!(culprits(&ledger), HashSet::from([*peer]));
        assert!(
            ledger.route(&peers, u64::MAX, &ResponseCosts::new()).is_empty(),
            "the orphan still holds its key"
        );
        let empty = NetworkMessage::Headers(vec![]);
        assert_eq!(ledger.response(*peer, &empty), Some(Matched::default()), "absorbed");
        assert_eq!(
            tag_of(&ledger, *peer, &empty),
            Some(Tag {
                id: second,
                attempt: *later,
                member: LedgerKey::Headers(BlockHash::dummy(2)),
                last: true
            })
        );
        let (retry_peer, _, msg) = reserve_one(&ledger, &peers);
        assert_eq!((retry_peer, msg), (*peer, get_headers(1)));
        assert_eq!(req(&ledger, SubId(1), 0, get_headers(1), Priority::Headers), first);
    }

    #[test]
    fn a_timed_out_filter_stream_absorbs_its_late_tail() {
        let (ledger, clock) = pure_ledger();
        let peers = [TestRoutePeer::new(1)];
        let first = req_filters(&ledger, SubId(1), range(0, 1), 2);
        let second = req_filters(&ledger, SubId(1), range(2, 3), 2);
        let routes = ledger.route(&peers, u64::MAX, &ResponseCosts::new());
        assert_eq!(routes.len(), 2);
        let ((peer, timed_out, msg), (_, later, _)) = (&routes[0], &routes[1]);
        assert_eq!(msg, &NetworkMessage::GetCFilters(range(0, 1)));
        assert!(send(&ledger, *peer, *timed_out));
        assert_eq!(hit(&ledger, *peer, &filter_reply(0)), Some((first, *timed_out)));
        clock.advance(Duration::from_secs(5));
        assert!(send(&ledger, *peer, *later));
        clock.advance(Duration::from_secs(6));
        assert_eq!(culprits(&ledger), HashSet::from([*peer]));
        assert_eq!(
            ledger.response(*peer, &filter_reply(1)),
            Some(Matched::default()),
            "the late stop filter ends the orphan's own stream"
        );
        assert_eq!(hit(&ledger, *peer, &filter_reply(2)), Some((second, *later)));
        assert_eq!(
            tag_of(&ledger, *peer, &filter_reply(3)),
            Some(Tag {
                id: second,
                attempt: *later,
                member: LedgerKey::CFilters(2),
                last: true
            }),
            "the later range counts only its own filters"
        );
    }

    #[test]
    fn a_timeout_orphan_expires_silently_and_a_cancel_orphan_does_not() {
        for kind in [get_headers(1), NetworkMessage::GetCFilters(range(0, 1))] {
            let (ledger, clock) = pure_ledger();
            let peers = [TestRoutePeer::new(1), TestRoutePeer::new(2), TestRoutePeer::new(3)];
            let admit = || match &kind {
                NetworkMessage::GetCFilters(range) => {
                    req_filters(&ledger, SubId(1), range.clone(), 2)
                }
                msg => req(&ledger, SubId(1), 0, msg.clone(), Priority::Headers),
            };
            let id = admit();
            let mut sent_on = Vec::new();
            for n in 0..3 {
                sent_on.push(route_one(&ledger, &peers).0);
                clock.advance(TIMEOUT + Duration::from_secs(1));
                let expired = ledger.expire();
                assert!(expired.unanswered.is_empty(), "{kind:?}: send {n}");
                assert_eq!(expired.culprits.len(), usize::from(n < 2), "{kind:?}: send {n}");
            }
            assert_eq!(ledger.take_failures(SubId(1)).len(), 1);
            clock.advance(TIMEOUT + Duration::from_secs(1));
            assert_eq!(
                ledger.expire(),
                Expired::default(),
                "{kind:?}: no orphan of a timeout is unanswered, the third send's included"
            );
            for peer in &sent_on {
                assert_eq!(ledger.deadline_horizon(*peer), Horizon::None, "{kind:?}: released");
            }
            let id2 = admit();
            assert_ne!(id, id2);
            let (peer, _, _) = route_one(&ledger, &peers);
            ledger.cancel(id2);
            clock.advance(TIMEOUT + Duration::from_secs(1));
            assert_eq!(
                ledger.expire().unanswered,
                HashSet::from([peer]),
                "{kind:?}: a cancel orphan keeps the rule"
            );
        }
    }

    #[test]
    fn a_skipped_range_expires_on_time_while_later_filters_arrive() {
        let (ledger, clock) = pure_ledger();
        let peers = [TestRoutePeer::new(1)];
        let skipped = req_filters(&ledger, SubId(1), range(0, 1), 2);
        let (peer, _, _) = route_one(&ledger, &peers);
        clock.advance(TIMEOUT + Duration::from_secs(1));
        assert_eq!(culprits(&ledger), HashSet::from([peer]));
        assert!(ledger.cancel(skipped), "the owner gives up; the orphan stays");
        for n in [2, 3] {
            clock.advance(Duration::from_secs(4));
            assert_eq!(
                ledger.response(peer, &filter_reply(n)),
                Some(Matched::default()),
                "the orphan absorbs a later range's filter {n}"
            );
        }
        clock.advance(Duration::from_secs(3));
        assert_eq!(ledger.expire(), Expired::default(), "the orphan expired at its deadline");
        let later = req_filters(&ledger, SubId(1), range(2, 3), 2);
        let (_, attempt, _) = route_one(&ledger, &peers);
        assert_eq!(hit(&ledger, peer, &filter_reply(2)), Some((later, attempt)));
        assert!(tag_of(&ledger, peer, &filter_reply(3)).unwrap().last, "the later range completes");
    }

    #[test]
    fn a_range_pipelined_behind_a_flowing_stream_does_not_expire() {
        let (ledger, clock) = pure_ledger();
        let peers = [TestRoutePeer::new(1)];
        let first = req_filters(&ledger, SubId(1), range(0, 6), 7);
        let second = req_filters(&ledger, SubId(1), range(7, 8), 2);
        let routes = ledger.route(&peers, u64::MAX, &ResponseCosts::new());
        assert_eq!(routes.len(), 2);
        send_all(&ledger, &routes);
        let ((peer, streaming, _), (_, behind, _)) = (&routes[0], &routes[1]);
        for n in 0..7 {
            clock.advance(Duration::from_secs(5));
            assert!(culprits(&ledger).is_empty(), "the queued range is alive at filter {n}");
            assert_eq!(hit(&ledger, *peer, &filter_reply(n)), Some((first, *streaming)));
        }
        clock.advance(Duration::from_secs(5));
        assert!(culprits(&ledger).is_empty());
        assert_eq!(hit(&ledger, *peer, &filter_reply(7)), Some((second, *behind)));
        clock.advance(TIMEOUT + Duration::from_secs(1));
        assert_eq!(culprits(&ledger), HashSet::from([*peer]), "a silent peer still expires");
    }

    #[test]
    fn a_block_batch_still_receiving_blocks_does_not_expire() {
        let (ledger, clock) = pure_ledger();
        let (blocks, msg) = dummy_blocks(MAX_MEMBERS as u32);
        let id = req(&ledger, SubId(1), 0, msg, Priority::Blocks);
        req(&ledger, SubId(1), 1, get_headers(1), Priority::Headers);
        let peers = [TestRoutePeer::new(1), TestRoutePeer::new(2)];
        let routes = ledger.route(&peers, u64::MAX, &ResponseCosts::new());
        assert_eq!(routes.len(), 2);
        assert_ne!(routes[0].0, routes[1].0);
        send_all(&ledger, &routes);
        let batch = routes.iter().find(|r| matches!(r.2, NetworkMessage::GetData(_))).unwrap();
        let silent = routes.iter().find(|r| matches!(r.2, NetworkMessage::GetHeaders(_))).unwrap();
        for (n, block) in blocks.iter().enumerate() {
            clock.advance(Duration::from_secs(5));
            let culprits = culprits(&ledger);
            assert!(!culprits.contains(&batch.0), "the batch is alive at block {n}");
            if n == 2 {
                assert_eq!(
                    culprits,
                    HashSet::from([silent.0]),
                    "another peer's answers do not count"
                );
            }
            let tag = tag_of(&ledger, batch.0, &NetworkMessage::Block(block.clone())).unwrap();
            assert_eq!((tag.id, tag.attempt, tag.last), (id, batch.1, n + 1 == blocks.len()));
        }
    }

    #[test]
    fn the_lowest_block_is_not_hedged_while_its_peer_delivers_other_answers() {
        let (ledger, clock) = pure_ledger();
        let (blocks, msg) = dummy_blocks(3);
        let lowest = NetworkMessage::GetData(vec![block(90)]);
        req(&ledger, SubId(1), 0, lowest.clone(), Priority::Blocks);
        req(&ledger, SubId(1), 1, msg, Priority::Blocks);
        let peers = [TestRoutePeer::new(1), TestRoutePeer::new(2)];
        let routes = ledger.route(&peers[..1], u64::MAX, &ResponseCosts::new());
        assert_eq!(routes.len(), 2);
        send_all(&ledger, &routes);
        let peer = routes[0].0;
        for block in &blocks {
            clock.advance(HEDGE_AFTER);
            assert!(ledger.response(peer, &NetworkMessage::Block(block.clone())).is_some());
            assert!(
                ledger.hedge(&peers, &ResponseCosts::new()).is_none(),
                "the peer is delivering"
            );
        }
        clock.advance(HEDGE_AFTER);
        let (hedged_on, _, msg) = ledger.hedge(&peers, &ResponseCosts::new()).unwrap();
        assert_eq!((hedged_on, msg), (peers[1].id(), lowest), "a stalled peer is hedged");
    }

    #[test]
    fn late_primary_filters_interleaved_with_live_retry() {
        let (ledger, clock) = pure_ledger();
        let id = req_filters(&ledger, SubId(1), range(0, 2), 3);
        let peers = [TestRoutePeer::new(1), TestRoutePeer::new(2)];
        let (primary, _, _) = route_one(&ledger, &peers[..1]);
        hit(&ledger, primary, &filter_reply(0));
        clock.advance(Duration::from_secs(11));
        culprits(&ledger);
        let (retry_peer, retry, _) = route_one(&ledger, &peers[1..]);
        for n in 0..3 {
            assert_eq!(hit(&ledger, primary, &filter_reply(n)), None, "late filters are untagged");
            let tag = tag_of(&ledger, retry_peer, &filter_reply(n)).unwrap();
            assert_eq!((tag.id, tag.attempt, tag.last), (id, retry, n == 2));
        }
    }

    #[test]
    fn partial_range_then_disconnect() {
        let (ledger, _) = pure_ledger();
        let owner = SubId(1);
        let id = req_filters(&ledger, owner, range(0, 1), 2);
        let peers = [TestRoutePeer::new(1), TestRoutePeer::new(2)];
        let (primary, _, _) = route_one(&ledger, &peers[..1]);
        hit(&ledger, primary, &filter_reply(0));
        ledger.disconnected(primary);
        let (retry_peer, retry, _) = route_one(&ledger, &peers);
        assert_eq!(hit(&ledger, retry_peer, &filter_reply(0)), Some((id, retry)));
        assert!(tag_of(&ledger, retry_peer, &filter_reply(1)).unwrap().last);
        assert!(ledger.take_failures(owner).is_empty());
        assert!(ledger.request_answered(id));
        assert!(ledger.is_empty());
    }

    #[test]
    fn members_resolve_one_by_one_and_a_retry_names_only_the_pending_ones() {
        let (ledger, _) = pure_ledger();
        let owner = SubId(1);
        let blocks: Vec<_> = (1..=3).map(|n| Block::dummy(n, vec![])).collect();
        let inv: Vec<_> = blocks.iter().map(|b| Inventory::Block(b.block_hash())).collect();
        let id = req(&ledger, owner, 0, NetworkMessage::GetData(inv.clone()), Priority::Blocks);
        let peers = [TestRoutePeer::new(1), TestRoutePeer::new(2)];
        let (peer, attempt, _) = route_one(&ledger, &peers[..1]);
        let tag = tag_of(&ledger, peer, &NetworkMessage::Block(blocks[1].clone())).unwrap();
        assert_eq!(
            tag,
            Tag {
                id,
                attempt,
                member: LedgerKey::Block(blocks[1].block_hash()),
                last: false
            }
        );
        ledger.disconnected(peer);
        let (other, retry, msg) = route_one(&ledger, &peers);
        assert_eq!(msg, NetworkMessage::GetData(vec![inv[0], inv[2]]));
        assert!(!tag_of(&ledger, other, &NetworkMessage::Block(blocks[0].clone())).unwrap().last);
        assert_eq!(hit(&ledger, other, &NetworkMessage::Block(blocks[1].clone())), None);
        let tag = tag_of(&ledger, other, &NetworkMessage::Block(blocks[2].clone())).unwrap();
        assert_eq!((tag.attempt, tag.last), (retry, true));
    }

    #[test]
    fn request_answered_releases_only_arrived_members() {
        let (ledger, _) = pure_ledger();
        let owner = SubId(1);
        let first = Block::dummy(1, vec![]);
        let second = Block::dummy(2, vec![]);
        let both = NetworkMessage::GetData(vec![
            Inventory::Block(first.block_hash()),
            Inventory::Block(second.block_hash()),
        ]);
        let id = req(&ledger, owner, 0, both, Priority::Blocks);
        let peers = [TestRoutePeer::new(1)];
        let (peer, attempt, _) = route_one(&ledger, &peers);
        hit(&ledger, peer, &NetworkMessage::Block(first.clone()));
        assert!(ledger.request_answered(id));
        let only_first = NetworkMessage::GetData(vec![Inventory::Block(first.block_hash())]);
        let only_second = NetworkMessage::GetData(vec![Inventory::Block(second.block_hash())]);
        assert_ne!(req(&ledger, owner, 0, only_first, Priority::Blocks), id);
        assert_eq!(req(&ledger, owner, 0, only_second, Priority::Blocks), id);
        assert_eq!(ledger.len(), 2);
        let tag = tag_of(&ledger, peer, &NetworkMessage::Block(second)).unwrap();
        assert_eq!((tag.id, tag.attempt, tag.last), (id, attempt, true));
        assert_eq!(ledger.len(), 1, "an answered request leaves with its last member");
    }

    #[test]
    fn a_member_refused_by_two_connections_fails_and_the_failure_lists_both_sets() {
        let (ledger, _) = pure_ledger();
        let owner = SubId(1);
        let found = Block::dummy(1, vec![]);
        let id = req(
            &ledger,
            owner,
            0,
            NetworkMessage::GetData(vec![Inventory::Block(found.block_hash()), block(9)]),
            Priority::Blocks,
        );
        let peers = [TestRoutePeer::new(1), TestRoutePeer::new(2)];
        let (first, _, _) = route_one(&ledger, &peers);
        hit(&ledger, first, &NetworkMessage::Block(found.clone()));
        ledger.not_found(first, &[block(9)]);
        assert!(ledger.take_failures(owner).is_empty());
        let (second, _, msg) = route_one(&ledger, &peers);
        assert_ne!(first, second, "never the connection that said notfound");
        assert_eq!(msg, NetworkMessage::GetData(vec![block(9)]));
        ledger.not_found(second, &[block(9)]);
        assert_eq!(
            ledger.take_failures(owner),
            vec![RequestFailed {
                id,
                failed: vec![(LedgerKey::Block(BlockHash::dummy(9)), FailReason::NotFound)],
                arrived: vec![LedgerKey::Block(found.block_hash())],
                sends: 2,
                last: FailReason::NotFound
            }]
        );
        assert!(ledger.is_empty());
    }

    #[test]
    fn an_arrival_that_ends_a_failed_request_reports_the_failed_members_reason() {
        let (ledger, clock) = pure_ledger();
        let owner = SubId(1);
        let found = Block::dummy(1, vec![]);
        let id = req(
            &ledger,
            owner,
            0,
            NetworkMessage::GetData(vec![Inventory::Block(found.block_hash()), block(9)]),
            Priority::Blocks,
        );
        let peers = [TestRoutePeer::new(1), TestRoutePeer::new(2)];
        let (first, _, _) = route_one(&ledger, &peers[..1]);
        clock.advance(HEDGE_AFTER);
        let (second, _, _) = hedge_one(&ledger, &peers).unwrap();
        ledger.not_found(first, &[block(9)]);
        ledger.not_found(second, &[block(9)]);
        assert!(ledger.take_failures(owner).is_empty(), "a member is still pending");
        assert!(ledger.route(&peers, u64::MAX, &ResponseCosts::new()).is_empty());
        assert!(tag_of(&ledger, first, &NetworkMessage::Block(found.clone())).unwrap().last);
        assert_eq!(
            ledger.take_failures(owner),
            vec![RequestFailed {
                id,
                failed: vec![(LedgerKey::Block(BlockHash::dummy(9)), FailReason::NotFound)],
                arrived: vec![LedgerKey::Block(found.block_hash())],
                sends: 2,
                last: FailReason::NotFound
            }]
        );
    }

    #[test]
    fn a_getdata_is_stored_and_pushed_without_duplicate_invs() {
        let (ledger, _) = pure_ledger();
        let request = NetworkMessage::GetData(vec![block(1), block(2), block(1), block(2)]);
        req(&ledger, SubId(1), 0, request, Priority::Blocks);
        let (peer, attempt, msg) = reserve_one(&ledger, &[TestRoutePeer::new(1)]);
        let deduplicated = NetworkMessage::GetData(vec![block(1), block(2)]);
        assert_eq!(msg, deduplicated);
        let mut pushed = None;
        let reservation = ledger.attach(attempt, peer, Vec::new()).unwrap();
        reservation.send_if_live(TIMEOUT, |_, msg| pushed = Some(msg.clone()));
        assert_eq!(pushed, Some(deduplicated));
    }

    #[test]
    fn a_request_names_at_most_max_members() {
        let (ledger, _) = pure_ledger();
        let inv = |n: u32| NetworkMessage::GetData((0..n).map(block).collect());
        assert_eq!(
            ledger.request(SubId(1), 0, inv(MAX_MEMBERS as u32 + 1), Priority::Blocks),
            Err(RequestError::TooManyMembers)
        );
        assert!(ledger.is_empty());
        assert!(ledger.request(SubId(1), 0, inv(MAX_MEMBERS as u32), Priority::Blocks).is_ok());
    }

    #[test]
    fn a_deadline_starts_at_the_send_and_each_filter_refreshes_it() {
        let (ledger, clock) = pure_ledger();
        req(&ledger, SubId(1), 0, get_headers(1), Priority::Headers);
        req_filters(&ledger, SubId(1), range(0, 2), 3);
        let peers = [TestRoutePeer::new(1), TestRoutePeer::new(2)];
        let routes = ledger.route(&peers, u64::MAX, &ResponseCosts::new());
        assert_eq!(routes.len(), 2);
        clock.advance(Duration::from_secs(9));
        send_all(&ledger, &routes);
        clock.advance(Duration::from_secs(9));
        let stream = routes.iter().find(|r| matches!(r.2, NetworkMessage::GetCFilters(_))).unwrap();
        assert!(tag_of(&ledger, stream.0, &filter_reply(0)).is_some());
        assert!(culprits(&ledger).is_empty());
        clock.advance(Duration::from_secs(2));
        let headers = routes.iter().find(|r| matches!(r.2, NetworkMessage::GetHeaders(_))).unwrap();
        assert_eq!(culprits(&ledger), HashSet::from([headers.0]));
        clock.advance(Duration::from_secs(9));
        assert_eq!(culprits(&ledger), HashSet::from([stream.0]));
    }

    #[test]
    fn late_response_after_cancel_is_untagged() {
        let (ledger, clock) = pure_ledger();
        let owner = SubId(1);
        let block = Block::dummy(1, vec![]);
        let request = NetworkMessage::GetData(vec![Inventory::Block(block.block_hash())]);
        let id = req(&ledger, owner, 0, request.clone(), Priority::Blocks);
        let peers = [TestRoutePeer::new(1), TestRoutePeer::new(2)];
        let (peer, attempt, _) = route_one(&ledger, &peers[..1]);
        clock.advance(HEDGE_AFTER);
        let (other, hedge, _) = hedge_one(&ledger, &peers).unwrap();
        assert!(ledger.cancel(id));
        assert!(!ledger.cancel(id));
        let response = NetworkMessage::Block(block);
        for peer in [peer, other, conn(3, 0)] {
            assert_eq!(hit(&ledger, peer, &response), None);
        }
        assert!(!send(&ledger, peer, attempt));
        assert!(!send(&ledger, other, hedge));
        ledger.unmark_sent(attempt);
        ledger.disconnected(peer);
        clock.advance(Duration::from_secs(60));
        assert_eq!(ledger.expire(), Expired::default());
        assert!(ledger.is_empty());
        assert!(ledger.take_failures(owner).is_empty());
        assert!(ledger.route(&peers, u64::MAX, &ResponseCosts::new()).is_empty());
        assert_ne!(req(&ledger, owner, 0, request, Priority::Blocks), id);
    }

    #[test]
    fn late_response_after_completion_is_untagged() {
        let (ledger, clock) = pure_ledger();
        let owner = SubId(1);
        let id = req(&ledger, owner, 0, get_headers(1), Priority::Headers);
        let peers = [TestRoutePeer::new(1)];
        let (peer, attempt, _) = route_one(&ledger, &peers);
        let response = NetworkMessage::Headers(vec![]);
        assert_eq!(hit(&ledger, peer, &response), Some((id, attempt)));
        assert_eq!(hit(&ledger, peer, &response), None);
        ledger.unmark_sent(attempt);
        assert!(ledger.request_answered(id));
        assert_eq!(hit(&ledger, peer, &response), None);
        clock.advance(Duration::from_secs(60));
        assert_eq!(ledger.expire(), Expired::default());
        assert!(ledger.is_empty());
        assert!(ledger.take_failures(owner).is_empty());
    }

    #[test]
    fn clear_while_budget_blocked_leaves_no_connection_blocked() {
        let (ledger, clock) = pure_ledger();
        let peers = [TestRoutePeer::new(1)];
        ledger.block_all(BlockReason::Budget, clock.steady());
        ledger.block(peers[0].id(), BlockReason::Park, clock.steady());
        ledger.clear();
        assert_eq!(ledger.deadline_horizon(peers[0].id()), Horizon::None);
        req(&ledger, SubId(1), 0, get_headers(1), Priority::Headers);
        let (peer, _, _) = route_one(&ledger, &peers);
        clock.advance(TIMEOUT + Duration::from_secs(1));
        assert_eq!(culprits(&ledger), HashSet::from([peer]), "expire reaches the connection");
    }

    #[test]
    fn cancel_queued_and_clear_remove_every_key_and_pending_write() {
        let (ledger, _) = pure_ledger();
        let owner = SubId(1);
        let request = NetworkMessage::GetData(vec![block(1), block(2)]);
        let id = req(&ledger, owner, 0, request.clone(), Priority::Blocks);
        assert!(ledger.cancel(id));
        assert!(ledger.route(&[TestRoutePeer::new(1)], u64::MAX, &ResponseCosts::new()).is_empty());
        let fresh = req(&ledger, owner, 0, request, Priority::Blocks);
        assert_ne!(fresh, id);
        let (peer, attempt, _) = reserve_one(&ledger, &[TestRoutePeer::new(1)]);
        ledger.clear();
        assert!(ledger.is_empty());
        assert!(ledger.attach(attempt, peer, Vec::new()).is_none());
        assert!(!ledger.request_answered(fresh));
        let first =
            req(&ledger, owner, 0, NetworkMessage::GetData(vec![block(1)]), Priority::Blocks);
        let second =
            req(&ledger, owner, 0, NetworkMessage::GetData(vec![block(2)]), Priority::Blocks);
        assert_ne!(first, second);
        assert_ne!(fresh, first);
    }

    #[test]
    fn a_response_from_an_ended_send_is_untagged() {
        for retry_first in [false, true] {
            let (ledger, _) = pure_ledger();
            let owner = SubId(1);
            let block = Block::dummy(1, vec![]);
            let id = req(
                &ledger,
                owner,
                0,
                NetworkMessage::GetData(vec![Inventory::Block(block.block_hash())]),
                Priority::Blocks,
            );
            let peers = [TestRoutePeer::new(1), TestRoutePeer::new(2)];
            let (peer, _, _) = route_one(&ledger, &peers[..1]);
            ledger.disconnected(peer);
            let retry = retry_first.then(|| route_one(&ledger, &peers));
            let response = NetworkMessage::Block(block);
            assert_eq!(hit(&ledger, peer, &response), None);
            let (other, new_attempt, _) = retry.unwrap_or_else(|| route_one(&ledger, &peers));
            assert_eq!(hit(&ledger, other, &response), Some((id, new_attempt)));
            assert!(ledger.take_failures(owner).is_empty());
        }
    }

    #[test]
    fn notfound_reroutes_once() {
        let (ledger, _) = pure_ledger();
        let owner = SubId(1);
        let request = NetworkMessage::GetData(vec![block(1)]);
        let id = req(&ledger, owner, 0, request, Priority::Blocks);
        let peers = [TestRoutePeer::new(1), TestRoutePeer::new(2), TestRoutePeer::new(3)];
        let (first, attempt, _) = route_one(&ledger, &peers);
        ledger.not_found(conn(9, 0), &[block(1)]);
        ledger.not_found(first, &[block(2), tx(1)]);
        assert!(ledger.route(&peers, u64::MAX, &ResponseCosts::new()).is_empty());
        ledger.not_found(first, &[block(1), block(1)]);
        assert!(ledger.take_failures(owner).is_empty());
        assert!(!send(&ledger, first, attempt));
        let (second, retry, _) = route_one(&ledger, &peers);
        assert_ne!(first, second);
        ledger.not_found(first, &[block(1)]);
        ledger.not_found(second, &[block(1)]);
        assert_eq!(
            ledger.take_failures(owner),
            vec![RequestFailed {
                id,
                failed: vec![(LedgerKey::Block(BlockHash::dummy(1)), FailReason::NotFound)],
                arrived: vec![],
                sends: 2,
                last: FailReason::NotFound
            }]
        );
        ledger.unmark_sent(retry);
        ledger.not_found(second, &[block(1)]);
        assert!(ledger.take_failures(owner).is_empty());
        assert!(ledger.is_empty());
        assert!(ledger.route(&peers, u64::MAX, &ResponseCosts::new()).is_empty());
    }

    #[test]
    fn a_partial_notfound_settles_only_the_members_it_names() {
        let (ledger, _) = pure_ledger();
        let owner = SubId(1);
        let blocks: Vec<_> = (1..=4).map(|n| Block::dummy(n, vec![])).collect();
        let id =
            req(&ledger, owner, 0, getdata(&blocks.iter().collect::<Vec<_>>()), Priority::Blocks);
        let peers = [TestRoutePeer::new(1), TestRoutePeer::new(2)];
        let (first, attempt, _) = route_one(&ledger, &peers);
        let named = |n: usize| Inventory::Block(blocks[n].block_hash());
        ledger.not_found(first, &[named(0), named(1)]);
        assert!(
            ledger.route(&peers, u64::MAX, &ResponseCosts::new()).is_empty(),
            "the send stays live while its other members are pending"
        );
        let tag = tag_of(&ledger, first, &NetworkMessage::Block(blocks[2].clone())).unwrap();
        assert_eq!((tag.id, tag.attempt, tag.last), (id, attempt, false));
        assert!(ledger.route(&peers, u64::MAX, &ResponseCosts::new()).is_empty());
        let tag = tag_of(&ledger, first, &NetworkMessage::Block(blocks[3].clone())).unwrap();
        assert_eq!((tag.id, tag.attempt, tag.last), (id, attempt, false));
        let (second, _, msg) = route_one(&ledger, &peers);
        assert_eq!(
            (second, msg),
            (peers[1].id(), NetworkMessage::GetData(vec![named(0), named(1)])),
            "the send ended with its last pending member, and the named ones reroute once"
        );
        assert!(ledger.response(first, &NetworkMessage::Block(blocks[0].clone())).is_none());
        assert!(tag_of(&ledger, second, &NetworkMessage::Block(blocks[0].clone())).is_some());
        assert!(tag_of(&ledger, second, &NetworkMessage::Block(blocks[1].clone())).unwrap().last);
        assert!(ledger.take_failures(owner).is_empty());
    }

    #[test]
    fn a_send_a_notfound_drained_ends_when_its_last_member_arrives_elsewhere() {
        let (ledger, clock) = pure_ledger();
        let owner = SubId(1);
        let (blocks, msg) = dummy_blocks(2);
        let id = req(&ledger, owner, 0, msg, Priority::Blocks);
        let peers = [TestRoutePeer::new(1), TestRoutePeer::new(2)];
        let (first, _, _) = route_one(&ledger, &peers[..1]);
        clock.advance(HEDGE_AFTER);
        let (second, hedge, _) = hedge_one(&ledger, &peers).unwrap();
        ledger.not_found(first, &[Inventory::Block(blocks[0].block_hash())]);
        let tag = tag_of(&ledger, second, &NetworkMessage::Block(blocks[1].clone())).unwrap();
        assert_eq!((tag.id, tag.attempt, tag.last), (id, hedge, false));
        assert!(
            ledger.response(first, &NetworkMessage::Block(blocks[1].clone())).is_none(),
            "the first send ended once none of its members was pending"
        );
        assert!(
            ledger.route(&peers, u64::MAX, &ResponseCosts::new()).is_empty(),
            "the hedge is live"
        );
        assert!(tag_of(&ledger, second, &NetworkMessage::Block(blocks[0].clone())).unwrap().last);
        assert!(ledger.take_failures(owner).is_empty());
    }

    #[test]
    fn a_notfound_naming_every_member_ends_the_send_once() {
        let (ledger, _) = pure_ledger();
        let owner = SubId(1);
        let id = req(
            &ledger,
            owner,
            0,
            NetworkMessage::GetData(vec![block(1), block(2)]),
            Priority::Blocks,
        );
        let peers = [TestRoutePeer::new(1), TestRoutePeer::new(2)];
        let (first, _, _) = route_one(&ledger, &peers);
        ledger.not_found(first, &[block(1), block(2), block(1)]);
        let (second, _, msg) = route_one(&ledger, &peers);
        assert_eq!(
            (second, msg),
            (peers[1].id(), NetworkMessage::GetData(vec![block(1), block(2)]))
        );
        assert!(ledger.route(&peers, u64::MAX, &ResponseCosts::new()).is_empty());
        ledger.not_found(second, &[block(2), block(1)]);
        assert_eq!(
            ledger.take_failures(owner),
            vec![RequestFailed {
                id,
                failed: vec![
                    (LedgerKey::Block(BlockHash::dummy(1)), FailReason::NotFound),
                    (LedgerKey::Block(BlockHash::dummy(2)), FailReason::NotFound)
                ],
                arrived: vec![],
                sends: 2,
                last: FailReason::NotFound
            }],
            "one failure for the whole request"
        );
        assert!(ledger.is_empty());
    }

    #[test]
    fn notfound_waits_for_a_different_connection() {
        let (ledger, _) = pure_ledger();
        req(&ledger, SubId(1), 0, NetworkMessage::GetData(vec![block(1)]), Priority::Blocks);
        let peers = [TestRoutePeer::new(1)];
        let (peer, _, _) = route_one(&ledger, &peers);
        ledger.not_found(peer, &[block(1)]);
        assert!(ledger.route(&peers, u64::MAX, &ResponseCosts::new()).is_empty());
        let replacement = TestRoutePeer {
            id: conn(1, 1),
            ..TestRoutePeer::new(1)
        };
        assert_eq!(route_one(&ledger, &[replacement]).0, conn(1, 1));
    }

    #[test]
    fn notfound_does_not_revive_arrived_or_cancelled_requests() {
        let (ledger, _) = pure_ledger();
        let owner = SubId(1);
        let block = Block::dummy(1, vec![]);
        let hash = block.block_hash();
        let request = NetworkMessage::GetData(vec![Inventory::Block(hash)]);
        let id = req(&ledger, owner, 0, request.clone(), Priority::Blocks);
        let peers = [TestRoutePeer::new(1)];
        let (peer, attempt, _) = route_one(&ledger, &peers);
        assert_eq!(hit(&ledger, peer, &NetworkMessage::Block(block)), Some((id, attempt)));
        ledger.not_found(peer, &[Inventory::Block(hash)]);
        assert!(ledger.route(&peers, u64::MAX, &ResponseCosts::new()).is_empty());
        assert!(ledger.take_failures(owner).is_empty());
        ledger.cancel(id);
        ledger.not_found(peer, &[Inventory::Block(hash)]);
        assert!(ledger.is_empty());
        assert_ne!(req(&ledger, owner, 0, request, Priority::Blocks), id);
    }

    #[test]
    fn hedge_respects_pending_write_capacity() {
        let (ledger, clock) = pure_ledger();
        let costs = ResponseCosts::new();
        req(&ledger, SubId(1), 1, NetworkMessage::GetData(vec![block(1)]), Priority::Blocks);
        let mut peers = [TestRoutePeer::new(1), TestRoutePeer::new(2)];
        route_one(&ledger, &peers[..1]);
        req(&ledger, SubId(1), 2, NetworkMessage::GetData(vec![block(2)]), Priority::Blocks);
        peers[1].cap = costs.cost(&NetworkMessage::GetData(vec![block(2)]));
        reserve_one(&ledger, &peers[1..]);
        clock.advance(HEDGE_AFTER);
        assert!(ledger.hedge(&peers, &costs).is_none());
    }

    #[test]
    fn a_reserved_primary_is_not_hedged() {
        let (ledger, clock) = pure_ledger();
        let peers = [TestRoutePeer::new(1), TestRoutePeer::new(2)];
        req(&ledger, SubId(1), 1, NetworkMessage::GetData(vec![block(1)]), Priority::Blocks);
        let (peer, attempt, _) = reserve_one(&ledger, &peers[..1]);
        clock.advance(HEDGE_AFTER * 2);
        assert!(ledger.hedge(&peers, &ResponseCosts::new()).is_none(), "the primary is unsent");
        assert!(send(&ledger, peer, attempt));
        clock.advance(HEDGE_AFTER);
        assert!(hedge_one(&ledger, &peers).is_some(), "a sent primary stalls");
    }

    #[test]
    fn a_notfound_reroute_can_retry_timeouts_on_its_remaining_connection() {
        let (ledger, _) = pure_ledger();
        let owner = SubId(1);
        let id = req(&ledger, owner, 0, NetworkMessage::GetData(vec![block(1)]), Priority::Blocks);
        let peers = [TestRoutePeer::new(1), TestRoutePeer::new(2)];
        let (first, _, _) = route_one(&ledger, &peers);
        ledger.not_found(first, &[block(1)]);
        let (second, attempt, _) = route_one(&ledger, &peers);
        assert_ne!(first, second);
        ledger.unmark_sent(attempt);
        let (third, attempt, _) = route_one(&ledger, &peers);
        assert_eq!(second, third);
        ledger.unmark_sent(attempt);
        assert_eq!(
            ledger.take_failures(owner),
            vec![RequestFailed {
                id,
                failed: vec![(LedgerKey::Block(BlockHash::dummy(1)), FailReason::WriteFailed)],
                arrived: vec![],
                sends: 3,
                last: FailReason::WriteFailed
            }]
        );
    }

    #[test]
    fn two_notfound_responses_to_a_hedged_request_are_terminal() {
        let (ledger, clock) = pure_ledger();
        let owner = SubId(1);
        req(&ledger, owner, 0, NetworkMessage::GetData(vec![block(1)]), Priority::Blocks);
        let peers = [TestRoutePeer::new(1), TestRoutePeer::new(2)];
        let (first, _, _) = route_one(&ledger, &peers[..1]);
        clock.advance(HEDGE_AFTER);
        let (second, _, _) = hedge_one(&ledger, &peers).unwrap();
        ledger.not_found(first, &[block(1)]);
        ledger.not_found(second, &[block(1)]);
        let failures = ledger.take_failures(owner);
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].last, FailReason::NotFound);
        assert!(ledger.is_empty());
    }

    #[test]
    fn a_failed_hedge_is_not_replaced_and_no_hedge_exceeds_the_cap() {
        let (ledger, clock) = pure_ledger();
        req(&ledger, SubId(1), 0, NetworkMessage::GetData(vec![block(1)]), Priority::Blocks);
        req(&ledger, SubId(1), 1, NetworkMessage::GetData(vec![block(2)]), Priority::Blocks);
        let peers = [TestRoutePeer::new(1), TestRoutePeer::new(2), TestRoutePeer::new(3)];
        let routes = ledger.route(&peers[..1], u64::MAX, &ResponseCosts::new());
        assert_eq!(routes.len(), 2);
        send_all(&ledger, &routes);
        clock.advance(HEDGE_AFTER);
        let (hedge, _, msg) = ledger.hedge(&peers, &ResponseCosts::new()).unwrap();
        assert_eq!(msg, NetworkMessage::GetData(vec![block(1)]));
        ledger.disconnected(hedge);
        assert!(ledger.hedge(&peers, &ResponseCosts::new()).is_none());
        ledger.unmark_sent(routes[0].1);
        route_one(&ledger, &peers);
        clock.advance(HEDGE_AFTER);
        assert!(ledger.hedge(&peers, &ResponseCosts::new()).is_none(), "three sends were made");
    }

    /// A ledger with one cost ledger per connection, counting how often each charge record
    /// is settled or released and recording every pushed message.
    struct Rig {
        ledger: TestLedger,
        clock: ManualClock,
        books: RefCell<HashMap<PeerId, Arc<CostLedger>>>,
        charged: RefCell<Vec<(PeerId, ChargeId)>>,
        seen: RefCell<HashMap<(PeerId, ChargeId), u32>>,
        pushed: RefCell<Vec<(PeerId, NetworkMessage)>>,
    }

    impl Rig {
        fn new() -> Self {
            let (ledger, clock) = pure_ledger();
            Self {
                ledger,
                clock,
                books: RefCell::default(),
                charged: RefCell::default(),
                seen: RefCell::default(),
                pushed: RefCell::default(),
            }
        }

        fn book(&self, peer: PeerId) -> Arc<CostLedger> {
            self.books
                .borrow_mut()
                .entry(peer)
                .or_insert_with(|| Arc::new(CostLedger::new(Arc::new(ResponseCosts::new()))))
                .clone()
        }

        fn in_flight(&self, peer: PeerId) -> u64 {
            self.book(peer).in_flight_bytes()
        }

        /// Admits `msg`; a `getcfilters` declares two filters.
        fn request(&self, msg: NetworkMessage) -> RequestId {
            if let NetworkMessage::GetCFilters(range) = msg {
                return req_filters(&self.ledger, SubId(1), range, 2);
            }
            let class = classify(&msg);
            req(&self.ledger, SubId(1), 0, msg, class)
        }

        fn reserve_on(&self, n: u8) -> (PeerId, AttemptId, NetworkMessage) {
            reserve_one(&self.ledger, &[TestRoutePeer::new(n)])
        }

        fn charge(&self, peer: PeerId, msg: &NetworkMessage) -> Vec<(LedgerKey, ChargeId)> {
            let charges = self.book(peer).charge_tracked(msg);
            self.charged.borrow_mut().extend(charges.iter().map(|(_, id)| (peer, *id)));
            charges
        }

        fn send(&self, route: &(PeerId, AttemptId, NetworkMessage)) -> SendOutcome {
            self.send_with(route, |_| {}, |_| {})
        }

        /// Charges, attaches and pushes a reserved send, running `after_charge` and
        /// `after_attach` at those points of the router's send order.
        fn send_with(
            &self,
            (peer, attempt, msg): &(PeerId, AttemptId, NetworkMessage),
            after_charge: impl FnOnce(&Rig),
            after_attach: impl FnOnce(&Rig),
        ) -> SendOutcome {
            let charges = self.charge(*peer, msg);
            after_charge(self);
            let reservation = self.ledger.attach(*attempt, *peer, charges);
            self.drain();
            let Some(reservation) = reservation else {
                return SendOutcome::NotLive;
            };
            after_attach(self);
            let outcome = reservation.send_if_live(TIMEOUT, |_, msg| {
                self.pushed.borrow_mut().push((*peer, msg.clone()));
            });
            self.drain();
            outcome
        }

        fn count(&self, peer: PeerId, id: ChargeId) {
            *self.seen.borrow_mut().entry((peer, id)).or_default() += 1;
        }

        fn deliver(&self, peer: PeerId, msg: &NetworkMessage) -> Option<Matched> {
            let matched = self.ledger.response(peer, msg);
            let book = self.book(peer);
            match &matched {
                Some(m) => {
                    if let Some(id) = m.accrue {
                        book.add_bytes(id, 10);
                    }
                    if let Some(id) = m.settle {
                        self.count(peer, id);
                        book.settle_id(id, 10);
                    }
                }
                None => book.settle_untracked(msg, 10),
            }
            self.drain();
            matched
        }

        fn not_found(&self, peer: PeerId, inv: &[Inventory]) {
            for id in self.ledger.not_found(peer, inv) {
                self.count(peer, id);
                self.book(peer).settle_id(id, 0);
            }
            self.drain();
        }

        fn drain(&self) {
            for (peer, id) in self.ledger.take_released() {
                self.count(peer, id);
                self.book(peer).release_ids(&[id]);
            }
        }

        /// Every record charged so far was settled or released exactly once.
        fn assert_once(&self) {
            let seen = self.seen.borrow();
            for key in self.charged.borrow().iter() {
                assert_eq!(seen.get(key), Some(&1), "record {key:?}");
            }
            assert_eq!(seen.len(), self.charged.borrow().len());
        }

        fn pushed_to(&self, peer: PeerId) -> Vec<NetworkMessage> {
            self.pushed
                .borrow()
                .iter()
                .filter(|(p, _)| *p == peer)
                .map(|(_, m)| m.clone())
                .collect()
        }
    }

    fn dummy_blocks(n: u32) -> (Vec<Block>, NetworkMessage) {
        let blocks: Vec<_> = (1..=n).map(|h| Block::dummy(h, vec![])).collect();
        let inv = blocks.iter().map(|b| Inventory::Block(b.block_hash())).collect();
        (blocks, NetworkMessage::GetData(inv))
    }

    fn getdata(blocks: &[&Block]) -> NetworkMessage {
        NetworkMessage::GetData(blocks.iter().map(|b| Inventory::Block(b.block_hash())).collect())
    }

    /// An untracked request charged on `peer`, which no ledger step may touch.
    fn charge_other(rig: &Rig, peer: PeerId) -> u64 {
        rig.book(peer).charge_untracked(&NetworkMessage::GetData(vec![tx(5)]));
        rig.in_flight(peer)
    }

    /// Sends a block request on connection 2 and reserves its hedge on connection 1.
    fn primary_and_reserved_hedge(
        rig: &Rig,
        msg: NetworkMessage,
    ) -> [(PeerId, AttemptId, NetworkMessage); 2] {
        rig.request(msg);
        let primary = rig.reserve_on(2);
        assert_eq!(rig.send(&primary), SendOutcome::Sent);
        rig.clock.advance(HEDGE_AFTER);
        let peers = [TestRoutePeer::new(1), TestRoutePeer::new(2)];
        let hedge = rig.ledger.hedge(&peers, &ResponseCosts::new()).unwrap();
        assert_eq!(hedge.0, conn(1, 0));
        [primary, hedge]
    }

    #[test]
    fn a_key_is_never_routed_to_a_connection_holding_it() {
        let rig = Rig::new();
        let id = rig.request(get_headers(1));
        let first = rig.reserve_on(1);
        rig.send(&first);
        assert!(rig.ledger.cancel(id));
        rig.request(get_headers(1));
        assert!(rig
            .ledger
            .route(&[TestRoutePeer::new(1)], u64::MAX, &ResponseCosts::new())
            .is_empty());
        let peers = [TestRoutePeer::new(1), TestRoutePeer::new(2)];
        assert_eq!(reserve_one(&rig.ledger, &peers).0, conn(2, 0));
    }

    #[test]
    fn partial_batch_then_disconnect() {
        let rig = Rig::new();
        let (blocks, msg) = dummy_blocks(2);
        rig.request(msg);
        let route = rig.reserve_on(1);
        let other = charge_other(&rig, route.0);
        rig.send(&route);
        assert!(rig
            .deliver(route.0, &NetworkMessage::Block(blocks[0].clone()))
            .unwrap()
            .tag
            .is_some());
        rig.ledger.disconnected(route.0);
        rig.drain();
        rig.assert_once();
        assert_eq!(rig.in_flight(route.0), other, "only the remaining record is released");
    }

    #[test]
    fn partial_batch_then_cancel_then_orphan_expiry() {
        let rig = Rig::new();
        let (blocks, msg) = dummy_blocks(2);
        let id = rig.request(msg);
        let route = rig.reserve_on(1);
        let other = charge_other(&rig, route.0);
        rig.send(&route);
        rig.deliver(route.0, &NetworkMessage::Block(blocks[0].clone()));
        assert!(rig.ledger.cancel(id));
        rig.drain();
        assert!(rig.in_flight(route.0) > other, "the orphan keeps its record until it resolves");
        rig.clock.advance(TIMEOUT + Duration::from_secs(1));
        assert_eq!(
            rig.ledger.expire(),
            Expired {
                culprits: HashSet::new(),
                unanswered: HashSet::from([route.0])
            }
        );
        rig.drain();
        rig.assert_once();
        assert_eq!(rig.in_flight(route.0), other);
    }

    #[test]
    fn attach_after_disconnect_releases_ids() {
        let rig = Rig::new();
        rig.request(get_headers(1));
        let route = rig.reserve_on(1);
        let other = charge_other(&rig, route.0);
        let outcome = rig.send_with(&route, |rig| rig.ledger.disconnected(route.0), |_| {});
        assert_eq!(outcome, SendOutcome::NotLive);
        assert!(rig.pushed_to(route.0).is_empty());
        rig.assert_once();
        assert_eq!(rig.in_flight(route.0), other);
    }

    #[test]
    fn send_if_live_after_resolution_is_not_live() {
        let rig = Rig::new();
        rig.request(get_headers(1));
        let route = rig.reserve_on(1);
        let outcome = rig.send_with(&route, |_| {}, |rig| rig.ledger.disconnected(route.0));
        assert_eq!(outcome, SendOutcome::NotLive);
        assert!(rig.pushed_to(route.0).is_empty());
        rig.assert_once();
        assert_eq!(rig.in_flight(route.0), 0);
    }

    #[test]
    fn unreserve_after_disconnect_is_noop() {
        let rig = Rig::new();
        rig.request(get_headers(1));
        let peers = [TestRoutePeer::new(1)];
        let (peer, attempt, msg) = reserve_one(&rig.ledger, &peers);
        let reservation = rig.ledger.attach(attempt, peer, rig.charge(peer, &msg)).unwrap();
        rig.ledger.disconnected(peer);
        reservation.unreserve();
        rig.drain();
        rig.assert_once();
        for _ in 0..2 {
            let (_, attempt, _) = route_one(&rig.ledger, &peers);
            rig.ledger.unmark_sent(attempt);
        }
        assert_eq!(rig.ledger.take_failures(SubId(1))[0].sends, 3, "the disconnected send counts");
    }

    #[test]
    fn unreserve_releases_attached_ids() {
        let rig = Rig::new();
        rig.request(get_headers(1));
        let peers = [TestRoutePeer::new(1)];
        let (peer, attempt, msg) = reserve_one(&rig.ledger, &peers);
        let other = charge_other(&rig, peer);
        let reservation = rig.ledger.attach(attempt, peer, rig.charge(peer, &msg)).unwrap();
        reservation.unreserve();
        rig.drain();
        rig.assert_once();
        assert_eq!(rig.in_flight(peer), other);
        for _ in 0..3 {
            let (_, attempt, _) = route_one(&rig.ledger, &peers);
            rig.ledger.unmark_sent(attempt);
        }
        assert_eq!(
            rig.ledger.take_failures(SubId(1))[0].sends,
            3,
            "the withdrawn send did not count"
        );
    }

    #[test]
    fn dropped_reservation_unreserves() {
        let rig = Rig::new();
        let id = rig.request(get_headers(1));
        let peers = [TestRoutePeer::new(1)];
        let (peer, attempt, msg) = reserve_one(&rig.ledger, &peers);
        drop(rig.ledger.attach(attempt, peer, rig.charge(peer, &msg)));
        rig.drain();
        rig.assert_once();
        assert_eq!(rig.in_flight(peer), 0);
        let (_, retry, _) = route_one(&rig.ledger, &peers);
        assert_ne!(retry, attempt, "the request was queued again");
        assert_eq!(hit(&rig.ledger, peer, &NetworkMessage::Headers(vec![])), Some((id, retry)));

        let rig = Rig::new();
        rig.request(get_headers(1));
        let (peer, attempt, msg) = reserve_one(&rig.ledger, &peers);
        drop(rig.ledger.attach(attempt, peer, rig.charge(peer, &msg)));
        for _ in 0..3 {
            let (_, attempt, _) = route_one(&rig.ledger, &peers);
            rig.ledger.unmark_sent(attempt);
        }
        assert_eq!(
            rig.ledger.take_failures(SubId(1))[0].sends,
            3,
            "the dropped send did not count"
        );
    }

    #[test]
    fn response_for_one_member_before_attach() {
        let rig = Rig::new();
        let (blocks, msg) = dummy_blocks(2);
        let [primary, hedge] = primary_and_reserved_hedge(&rig, msg);
        let other = charge_other(&rig, hedge.0);
        let outcome = rig.send_with(
            &hedge,
            |rig| {
                rig.deliver(primary.0, &NetworkMessage::Block(blocks[0].clone()));
            },
            |_| {},
        );
        assert_eq!(outcome, SendOutcome::Sent);
        assert_eq!(rig.pushed_to(hedge.0), vec![getdata(&[&blocks[1]])]);
        let second = rig.deliver(hedge.0, &NetworkMessage::Block(blocks[1].clone())).unwrap();
        assert!(second.settle.is_some() && second.tag.unwrap().last);
        rig.ledger.disconnected(primary.0);
        rig.drain();
        rig.assert_once();
        assert_eq!(rig.in_flight(hedge.0), other);
    }

    #[test]
    fn responses_for_every_member_before_attach() {
        let rig = Rig::new();
        let (blocks, msg) = dummy_blocks(2);
        let [primary, hedge] = primary_and_reserved_hedge(&rig, msg);
        let outcome = rig.send_with(
            &hedge,
            |rig| {
                for block in &blocks {
                    rig.deliver(primary.0, &NetworkMessage::Block(block.clone()));
                }
            },
            |_| {},
        );
        assert_eq!(outcome, SendOutcome::NotLive);
        assert!(rig.pushed_to(hedge.0).is_empty());
        rig.assert_once();
        assert_eq!(rig.in_flight(hedge.0), 0);
        assert_eq!(rig.in_flight(primary.0), 0);
    }

    #[test]
    fn attach_by_key_not_position() {
        let rig = Rig::new();
        let (blocks, msg) = dummy_blocks(2);
        rig.request(msg.clone());
        let (peer, attempt, _) = rig.reserve_on(1);
        let mut charges = rig.charge(peer, &msg);
        charges.reverse();
        let reservation = rig.ledger.attach(attempt, peer, charges.clone()).unwrap();
        assert_eq!(reservation.send_if_live(TIMEOUT, |_, _| {}), SendOutcome::Sent);
        for block in &blocks {
            let settled = rig.deliver(peer, &NetworkMessage::Block(block.clone())).unwrap().settle;
            let expected = charges.iter().find(|(k, _)| *k == LedgerKey::Block(block.block_hash()));
            assert_eq!(settled, expected.map(|(_, id)| *id));
        }
        rig.assert_once();
    }

    #[test]
    fn attach_sends_only_unresolved_members() {
        let rig = Rig::new();
        let (blocks, msg) = dummy_blocks(3);
        let [primary, hedge] = primary_and_reserved_hedge(&rig, msg);
        rig.send_with(
            &hedge,
            |rig| {
                rig.deliver(primary.0, &NetworkMessage::Block(blocks[1].clone()));
            },
            |_| {},
        );
        assert_eq!(rig.pushed_to(hedge.0), vec![getdata(&[&blocks[0], &blocks[2]])]);
    }

    #[test]
    fn partial_block_resolution_rebuilds_at_send() {
        let rig = Rig::new();
        let (blocks, msg) = dummy_blocks(2);
        let [primary, hedge] = primary_and_reserved_hedge(&rig, msg);
        let outcome = rig.send_with(
            &hedge,
            |_| {},
            |rig| {
                rig.deliver(primary.0, &NetworkMessage::Block(blocks[0].clone()));
            },
        );
        assert_eq!(outcome, SendOutcome::Sent);
        assert_eq!(rig.pushed_to(hedge.0), vec![getdata(&[&blocks[1]])]);
        for block in &blocks {
            rig.deliver(primary.0, &NetworkMessage::Block(block.clone()));
            rig.deliver(hedge.0, &NetworkMessage::Block(block.clone()));
        }
        rig.assert_once();
        assert_eq!(rig.in_flight(hedge.0), 0);
    }

    #[derive(Clone, Copy, Debug)]
    enum Resolution {
        Finalize,
        Clear,
        Cancel,
        Elsewhere,
    }

    /// Resolves a reserved send between `attach` and `send_if_live` and checks that
    /// nothing reaches the wire and every record leaves once.
    fn resolved_before_send(request: NetworkMessage, resolution: Resolution, replies: &[Block]) {
        let rig = Rig::new();
        let (route, outside, id) = if matches!(resolution, Resolution::Elsewhere) {
            let [primary, hedge] = primary_and_reserved_hedge(&rig, request.clone());
            (hedge, Some(primary), None)
        } else {
            let id = rig.request(request.clone());
            (rig.reserve_on(1), None, Some(id))
        };
        let outcome = rig.send_with(
            &route,
            |_| {},
            |rig| match resolution {
                Resolution::Finalize => rig.ledger.disconnected(route.0),
                Resolution::Clear => rig.ledger.clear(),
                Resolution::Cancel => assert!(rig.ledger.cancel(id.unwrap())),
                Resolution::Elsewhere => {
                    for block in replies {
                        rig.deliver(
                            outside.as_ref().unwrap().0,
                            &NetworkMessage::Block(block.clone()),
                        );
                    }
                }
            },
        );
        assert_eq!(outcome, SendOutcome::NotLive, "{request:?} {resolution:?}");
        assert!(rig.pushed_to(route.0).is_empty());
        rig.assert_once();
        assert_eq!(rig.in_flight(route.0), 0);
    }

    #[test]
    fn resolved_before_send_is_not_pushed() {
        let (blocks1, one) = dummy_blocks(1);
        let (blocks2, two) = dummy_blocks(2);
        for request in [
            get_headers(1),
            NetworkMessage::GetCFilters(range(0, 1)),
            get_cfheaders(2),
            get_mnlistdiff(1),
            get_qrinfo(5),
            one.clone(),
            two.clone(),
        ] {
            for resolution in [Resolution::Finalize, Resolution::Clear, Resolution::Cancel] {
                resolved_before_send(request.clone(), resolution, &[]);
            }
        }
        resolved_before_send(one, Resolution::Elsewhere, &blocks1);
        resolved_before_send(two, Resolution::Elsewhere, &blocks2);
    }

    #[test]
    fn finalize_before_send_is_not_pushed() {
        resolved_before_send(get_headers(1), Resolution::Finalize, &[]);
    }

    #[test]
    fn clear_before_send_is_not_pushed() {
        resolved_before_send(get_headers(1), Resolution::Clear, &[]);
    }

    #[test]
    fn response_after_send_is_a_normal_reply() {
        let rig = Rig::new();
        let first = rig.request(get_headers(1));
        let s = rig.reserve_on(1);
        assert_eq!(rig.send(&s), SendOutcome::Sent);
        let tag = rig.deliver(s.0, &NetworkMessage::Headers(vec![])).unwrap().tag.unwrap();
        assert_eq!((tag.id, tag.attempt, tag.last), (first, s.1, true));
        let second = rig.request(get_headers(2));
        let t = rig.reserve_on(1);
        rig.send(&t);
        let tag = rig.deliver(t.0, &NetworkMessage::Headers(vec![])).unwrap().tag.unwrap();
        assert_eq!((tag.id, tag.attempt), (second, t.1));
        rig.assert_once();
    }

    #[test]
    fn reserved_slots_do_not_expire() {
        let rig = Rig::new();
        let first = rig.request(get_headers(1));
        let route = rig.reserve_on(1);
        let reservation = rig.ledger.attach(route.1, route.0, Vec::new()).unwrap();
        rig.clock.advance(TIMEOUT * 3);
        assert_eq!(rig.ledger.expire(), Expired::default());
        assert_eq!(reservation.send_if_live(TIMEOUT, |_, _| {}), SendOutcome::Sent);
        rig.clock.advance(TIMEOUT);
        assert_eq!(rig.ledger.expire(), Expired::default());
        rig.clock.advance(Duration::from_secs(1));
        assert_eq!(rig.ledger.expire().culprits, HashSet::from([route.0]));
        assert!(rig.ledger.cancel(first));
        let late = NetworkMessage::Headers(vec![]);
        assert_eq!(rig.ledger.response(route.0, &late), Some(Matched::default()));

        let id = rig.request(get_headers(2));
        let route = rig.reserve_on(1);
        let outcome = rig.send_with(
            &route,
            |_| {},
            |rig| {
                rig.clock.advance(TIMEOUT * 3);
                assert!(rig.ledger.cancel(id));
            },
        );
        assert_eq!(outcome, SendOutcome::NotLive);
        assert_eq!(
            rig.ledger.expire(),
            Expired::default(),
            "the cancel removed it, nobody is blamed"
        );
    }

    #[test]
    fn cancel_removes_reserved_slot() {
        let reroutes_to_one = |rig: &Rig, msg: NetworkMessage| {
            rig.request(msg);
            assert_eq!(rig.reserve_on(1).0, conn(1, 0), "connection 1 no longer holds the key");
        };
        let rig = Rig::new();
        let id = rig.request(get_headers(1));
        let route = rig.reserve_on(1);
        let outcome = rig.send_with(&route, |_| {}, |rig| assert!(rig.ledger.cancel(id)));
        assert_eq!(outcome, SendOutcome::NotLive);
        rig.assert_once();
        reroutes_to_one(&rig, get_headers(1));

        let rig = Rig::new();
        let (blocks, msg) = dummy_blocks(1);
        let [primary, hedge] = primary_and_reserved_hedge(&rig, msg.clone());
        let outcome = rig.send_with(
            &hedge,
            |_| {},
            |rig| {
                let tag = rig.deliver(primary.0, &NetworkMessage::Block(blocks[0].clone()));
                assert!(rig.ledger.request_answered(tag.unwrap().tag.unwrap().id));
            },
        );
        assert_eq!(outcome, SendOutcome::NotLive, "a losing hedge that was never sent");
        rig.assert_once();
        reroutes_to_one(&rig, msg);

        let rig = Rig::new();
        let (blocks, msg) = dummy_blocks(1);
        let hash = blocks[0].block_hash();
        rig.request(msg.clone());
        let first = rig.reserve_on(3);
        rig.send(&first);
        rig.not_found(first.0, &[Inventory::Block(hash)]);
        let second = reserve_one(&rig.ledger, &[TestRoutePeer::new(2)]);
        rig.send(&second);
        rig.clock.advance(HEDGE_AFTER);
        let peers = [TestRoutePeer::new(1), TestRoutePeer::new(2)];
        let hedge = rig.ledger.hedge(&peers, &ResponseCosts::new()).unwrap();
        let outcome = rig.send_with(
            &hedge,
            |_| {},
            |rig| {
                rig.not_found(second.0, &[Inventory::Block(hash)]);
            },
        );
        assert_eq!(outcome, SendOutcome::NotLive, "a terminal failure removes the reservation");
        assert_eq!(rig.ledger.take_failures(SubId(1)).len(), 1);
        rig.assert_once();
        reroutes_to_one(&rig, msg);
    }

    #[test]
    fn cancel_orphans_sent_slot() {
        for answered in [true, false] {
            let rig = Rig::new();
            let id = rig.request(get_headers(1));
            let route = rig.reserve_on(1);
            rig.send(&route);
            assert!(rig.ledger.cancel(id));
            assert!(rig
                .ledger
                .route(&[TestRoutePeer::new(1)], u64::MAX, &ResponseCosts::new())
                .is_empty());
            rig.request(get_headers(1));
            assert!(
                rig.ledger
                    .route(&[TestRoutePeer::new(1)], u64::MAX, &ResponseCosts::new())
                    .is_empty(),
                "the orphan still holds its key"
            );
            if answered {
                let matched = rig.deliver(route.0, &NetworkMessage::Headers(vec![])).unwrap();
                assert!(matched.tag.is_none() && matched.settle.is_some());
                assert_eq!(rig.reserve_on(1).0, route.0);
            } else {
                rig.clock.advance(TIMEOUT + Duration::from_secs(1));
                assert_eq!(rig.ledger.expire().unanswered, HashSet::from([route.0]));
            }
            rig.drain();
            rig.assert_once();
        }
    }

    #[test]
    fn orphan_answered_within_deadline_keeps_conn() {
        let rig = Rig::new();
        let (blocks, msg) = dummy_blocks(1);
        let id = rig.request(msg);
        let route = rig.reserve_on(1);
        rig.send(&route);
        rig.ledger.cancel(id);
        rig.clock.advance(TIMEOUT);
        assert!(rig.deliver(route.0, &NetworkMessage::Block(blocks[0].clone())).is_some());
        rig.clock.advance(TIMEOUT * 2);
        assert_eq!(rig.ledger.expire(), Expired::default());
        rig.assert_once();
    }

    #[test]
    fn orphan_expiry_disconnects() {
        let rig = Rig::new();
        let id = rig.request(get_headers(1));
        let route = rig.reserve_on(1);
        rig.send(&route);
        rig.ledger.cancel(id);
        rig.clock.advance(TIMEOUT + Duration::from_secs(1));
        let expired = rig.ledger.expire();
        assert_eq!(expired.unanswered, HashSet::from([route.0]));
        assert!(expired.culprits.is_empty(), "an orphan is not a culprit");
        rig.drain();
        rig.assert_once();
        assert_eq!(rig.in_flight(route.0), 0);
    }

    #[test]
    fn reserved_slot_matches_nothing() {
        let (blocks, getdata_ab) = dummy_blocks(2);
        let filters = NetworkMessage::GetCFilters(range(0, 1));
        for request in [getdata_ab, get_headers(1), filters] {
            let rig = Rig::new();
            let declared = matches!(request, NetworkMessage::GetCFilters(_));
            let id = if declared {
                req_filters(&rig.ledger, SubId(1), range(0, 1), 2)
            } else {
                rig.request(request.clone())
            };
            let route = rig.reserve_on(1);
            let outcome = rig.send_with(
                &route,
                |_| {},
                |rig| {
                    let strays = [
                        NetworkMessage::Block(blocks[0].clone()),
                        NetworkMessage::Headers(vec![]),
                        filter_reply(0),
                        filter_reply(7),
                    ];
                    for stray in &strays {
                        assert_eq!(rig.ledger.response(route.0, stray), None);
                    }
                    rig.not_found(route.0, &[Inventory::Block(blocks[0].block_hash())]);
                },
            );
            assert_eq!(outcome, SendOutcome::Sent);
            assert_eq!(rig.pushed_to(route.0), vec![request.clone()], "nothing was resolved");
            let replies = match &request {
                NetworkMessage::GetData(_) => {
                    blocks.iter().map(|b| NetworkMessage::Block(b.clone())).collect()
                }
                NetworkMessage::GetCFilters(_) => vec![filter_reply(0), filter_reply(1)],
                _ => vec![NetworkMessage::Headers(vec![])],
            };
            let mut last = None;
            for reply in &replies {
                last = rig.deliver(route.0, reply).unwrap().tag;
                assert_eq!(last.as_ref().map(|t| t.id), Some(id));
            }
            assert!(last.unwrap().last);
            rig.assert_once();
        }
    }

    #[test]
    fn fifo_is_send_order() {
        for filters in [false, true] {
            let rig = Rig::new();
            let (s_id, t_id) = if filters {
                (
                    req_filters(&rig.ledger, SubId(1), range(0, 0), 1),
                    req_filters(&rig.ledger, SubId(1), range(1, 1), 1),
                )
            } else {
                (rig.request(get_headers(1)), rig.request(get_headers(2)))
            };
            let routes =
                rig.ledger.route(&[TestRoutePeer::new(1)], u64::MAX, &ResponseCosts::new());
            assert_eq!(routes.len(), 2);
            let (s, t) = (&routes[0], &routes[1]);
            let reply = |id: u32| {
                if filters {
                    filter_reply(id)
                } else {
                    NetworkMessage::Headers(vec![])
                }
            };
            let outcome =
                rig.send_with(s, |_| {}, |rig| assert_eq!(rig.send(t), SendOutcome::Sent));
            assert_eq!(outcome, SendOutcome::Sent);
            let tag = rig.deliver(t.0, &reply(1)).unwrap().tag.unwrap();
            assert_eq!(tag.id, t_id, "the first reply goes to the slot sent first");
            assert_eq!(rig.deliver(s.0, &reply(0)).unwrap().tag.unwrap().id, s_id);
            rig.assert_once();
        }
    }

    #[test]
    fn every_path_settles_or_releases_once() {
        let rig = Rig::new();
        let peers = |n: u8| [TestRoutePeer::new(n)];
        let mut n = 0;
        let mut next_conn = || {
            n += 1;
            n
        };
        let (blocks, _) = dummy_blocks(9);
        let mut checks = Vec::new();
        let mut check = |rig: &Rig, peer: PeerId, base: u64| {
            rig.drain();
            checks.push((peer, base));
        };

        // A response, after the push.
        let c = next_conn();
        rig.request(getdata(&[&blocks[0]]));
        let route = reserve_one(&rig.ledger, &peers(c));
        let base = charge_other(&rig, route.0);
        rig.send(&route);
        rig.deliver(route.0, &NetworkMessage::Block(blocks[0].clone()));
        check(&rig, route.0, base);

        // A notfound, after the push.
        let c = next_conn();
        let id = rig.request(getdata(&[&blocks[1]]));
        let route = reserve_one(&rig.ledger, &peers(c));
        let base = charge_other(&rig, route.0);
        rig.send(&route);
        rig.not_found(route.0, &[Inventory::Block(blocks[1].block_hash())]);
        rig.ledger.cancel(id);
        check(&rig, route.0, base);

        // A stop filter with the wrong count.
        let c = next_conn();
        let id = req_filters(&rig.ledger, SubId(1), range(0, 1), 2);
        let route = reserve_one(&rig.ledger, &peers(c));
        let base = charge_other(&rig, route.0);
        rig.send(&route);
        rig.deliver(route.0, &filter_reply(1));
        rig.ledger.cancel(id);
        check(&rig, route.0, base);

        // Finalize after the push.
        let c = next_conn();
        let id = rig.request(getdata(&[&blocks[2], &blocks[3]]));
        let route = reserve_one(&rig.ledger, &peers(c));
        let base = charge_other(&rig, route.0);
        rig.send(&route);
        rig.ledger.disconnected(route.0);
        rig.ledger.cancel(id);
        check(&rig, route.0, base);

        // A stale attach.
        let c = next_conn();
        let id = rig.request(get_headers(7));
        let route = reserve_one(&rig.ledger, &peers(c));
        let base = charge_other(&rig, route.0);
        rig.send_with(&route, |rig| rig.ledger.disconnected(route.0), |_| {});
        rig.ledger.cancel(id);
        check(&rig, route.0, base);

        // A keyed live-slot expiry.
        let c = next_conn();
        let id = rig.request(getdata(&[&blocks[8]]));
        let route = reserve_one(&rig.ledger, &peers(c));
        let base = charge_other(&rig, route.0);
        rig.send(&route);
        rig.clock.advance(TIMEOUT + Duration::from_secs(1));
        rig.ledger.expire();
        assert_eq!(rig.ledger.deadline_horizon(route.0), Horizon::None, "no orphan");
        rig.ledger.cancel(id);
        check(&rig, route.0, base);

        // A positional timeout, then its orphan's expiry; a cancel orphan's expiry.
        for cancelled in [false, true] {
            let c = next_conn();
            let id = rig.request(get_headers(10 + c as u32));
            let route = reserve_one(&rig.ledger, &peers(c));
            let base = charge_other(&rig, route.0);
            rig.send(&route);
            if cancelled {
                rig.ledger.cancel(id);
            } else {
                rig.clock.advance(TIMEOUT + Duration::from_secs(1));
                rig.ledger.expire();
                rig.ledger.cancel(id);
            }
            rig.clock.advance(TIMEOUT + Duration::from_secs(1));
            rig.ledger.expire();
            check(&rig, route.0, base);
        }

        // A losing hedge, answered late.
        let c = next_conn();
        rig.request(getdata(&[&blocks[4]]));
        let primary = reserve_one(&rig.ledger, &peers(c));
        rig.send(&primary);
        rig.clock.advance(HEDGE_AFTER);
        let h = next_conn();
        let both = [TestRoutePeer::new(c), TestRoutePeer::new(h)];
        let hedge = rig.ledger.hedge(&both, &ResponseCosts::new()).unwrap();
        let base = charge_other(&rig, hedge.0);
        rig.send(&hedge);
        rig.deliver(primary.0, &NetworkMessage::Block(blocks[4].clone()));
        rig.deliver(hedge.0, &NetworkMessage::Block(blocks[4].clone()));
        check(&rig, primary.0, 0);
        check(&rig, hedge.0, base);

        // A gate refusal.
        let c = next_conn();
        let id = rig.request(getdata(&[&blocks[5]]));
        let (peer, attempt, msg) = reserve_one(&rig.ledger, &peers(c));
        let base = charge_other(&rig, peer);
        rig.ledger.attach(attempt, peer, rig.charge(peer, &msg)).unwrap().unreserve();
        rig.ledger.cancel(id);
        check(&rig, peer, base);

        // The connection gone before the push.
        let c = next_conn();
        let id = rig.request(getdata(&[&blocks[6]]));
        let route = reserve_one(&rig.ledger, &peers(c));
        let base = charge_other(&rig, route.0);
        rig.send_with(&route, |_| {}, |rig| rig.ledger.disconnected(route.0));
        rig.ledger.cancel(id);
        check(&rig, route.0, base);

        // `clear` with a sent and a reserved slot.
        let c = next_conn();
        rig.request(getdata(&[&blocks[7]]));
        rig.request(get_headers(99));
        let routes = rig.ledger.route(&peers(c), u64::MAX, &ResponseCosts::new());
        let base = charge_other(&rig, routes[0].0);
        rig.send(&routes[0]);
        let reservation =
            rig.ledger.attach(routes[1].1, routes[1].0, rig.charge(routes[1].0, &routes[1].2));
        rig.ledger.clear();
        drop(reservation);
        check(&rig, routes[0].0, base);

        rig.assert_once();
        for (peer, base) in checks {
            assert_eq!(rig.in_flight(peer), base, "{peer:?}");
        }
    }

    fn get_qrinfo(tip: u8) -> NetworkMessage {
        NetworkMessage::GetQRInfo(GetQRInfo {
            base_block_hashes: vec![],
            block_request_hash: QRInfo::dummy(tip).mn_list_diff_tip.block_hash,
            extra_share: false,
        })
    }

    fn get_headers_with(locator: Vec<BlockHash>) -> GetHeadersMessage {
        GetHeadersMessage::new(locator, BlockHash::dummy(0))
    }

    fn filtered(n: u32) -> Inventory {
        Inventory::FilteredBlock(BlockHash::dummy(n))
    }

    fn compact(n: u32) -> Inventory {
        Inventory::CompactBlock(BlockHash::dummy(n))
    }

    #[test_case(get_headers(1), Ok(vec![LedgerKey::Headers(BlockHash::dummy(1))]), true, None; "getheaders")]
    #[test_case(NetworkMessage::GetHeaders2(get_headers_with(vec![BlockHash::dummy(1)])), Ok(vec![LedgerKey::Headers(BlockHash::dummy(1))]), true, None; "getheaders2")]
    #[test_case(NetworkMessage::GetHeaders(get_headers_with(vec![])), Err(RequestError::Keyless), true, None; "getheaders with an empty locator")]
    #[test_case(NetworkMessage::GetHeaders2(get_headers_with(vec![])), Err(RequestError::Keyless), true, None; "getheaders2 with an empty locator")]
    #[test_case(get_cfheaders(2), Ok(vec![LedgerKey::CfHeaders(BlockHash::dummy(2))]), true, None; "getcfheaders")]
    #[test_case(get_cfilters(100), Ok(vec![LedgerKey::CFilters(100)]), true, Some(RequestError::Unsupported); "getcfilters")]
    #[test_case(get_mnlistdiff(3), Ok(vec![LedgerKey::MnListDiff { base: BlockHash::dummy(3), block: BlockHash::dummy(4) }]), true, None; "getmnlistd")]
    #[test_case(get_qrinfo(5), Ok(vec![LedgerKey::QrInfo(QRInfo::dummy(5).mn_list_diff_tip.block_hash)]), true, None; "getqrinfo")]
    #[test_case(get_blocks(&[1, 2]), Ok(vec![LedgerKey::Block(BlockHash::dummy(1)), LedgerKey::Block(BlockHash::dummy(2))]), true, None; "getdata of blocks")]
    #[test_case(NetworkMessage::GetData(vec![block(1), tx(7)]), Err(RequestError::Unsupported), true, None; "getdata mixing a block and a tx")]
    #[test_case(NetworkMessage::GetData(vec![block(1), filtered(2)]), Err(RequestError::Unsupported), true, None; "getdata mixing a block and a filtered block")]
    #[test_case(NetworkMessage::GetData(vec![filtered(1)]), Err(RequestError::Unsupported), false, None; "getdata of a filtered block")]
    #[test_case(NetworkMessage::GetData(vec![compact(1)]), Err(RequestError::Unsupported), true, None; "getdata of a compact block")]
    #[test_case(NetworkMessage::GetData(vec![block(1), compact(2)]), Err(RequestError::Unsupported), true, None; "getdata mixing a block and a compact block")]
    #[test_case(NetworkMessage::GetData(vec![]), Err(RequestError::Keyless), false, None; "empty getdata")]
    #[test_case(NetworkMessage::GetData(vec![tx(1)]), Err(RequestError::Keyless), false, None; "getdata of a tx")]
    #[test_case(NetworkMessage::GetData(vec![Inventory::InstantSendLock(InstantSendLockHash::from_byte_array([1; 32]))]), Err(RequestError::Keyless), false, None; "getdata of an islock")]
    #[test_case(NetworkMessage::GetData(vec![Inventory::ChainLock(ChainLockHash::from_byte_array([1; 32]))]), Err(RequestError::Keyless), false, None; "getdata of a clsig")]
    #[test_case(NetworkMessage::FilterLoad(FilterLoad { filter: vec![], hash_funcs: 1, tweak: 0, flags: BloomFlags::None }), Err(RequestError::Keyless), false, None; "filterload")]
    #[test_case(NetworkMessage::FilterClear, Err(RequestError::Keyless), false, None; "filterclear")]
    #[test_case(NetworkMessage::MemPool, Err(RequestError::Keyless), false, None; "mempool")]
    #[test_case(NetworkMessage::Tx(Box::new(Transaction::dummy_empty())), Err(RequestError::Keyless), false, None; "tx")]
    fn tracked_kind_and_untracked_refusal_tables(
        msg: NetworkMessage,
        keys: Result<Vec<LedgerKey>, RequestError>,
        refused: bool,
        request_refusal: Option<RequestError>,
    ) {
        assert_eq!(ledger_keys(&msg), keys);
        assert_eq!(ledger_tracked_kind(&msg), keys.is_ok());
        assert_eq!(untracked_refuses(&msg), refused);
        let (ledger, _) = pure_ledger();
        let class = classify(&msg);
        assert_eq!(ledger.request(SubId(1), 0, msg, class).err(), request_refusal.or(keys.err()));
    }

    #[test]
    fn keyless_and_unsupported_requests_take_no_credit() {
        let (ledger, _) = ledger_with(1);
        for msg in [NetworkMessage::MemPool, NetworkMessage::GetData(vec![block(1), tx(1)])] {
            assert!(ledger.request(SubId(1), 0, msg, Priority::Other).is_err());
        }
        assert_eq!(ledger.credits.free(), 1);
        assert!(ledger.is_empty());
    }

    #[test]
    fn qrinfo_deadlines_follow_the_send_index() {
        let (ledger, clock) = pure_ledger();
        let reply = NetworkMessage::QRInfo(Box::new(QRInfo::dummy(5)));
        let id = req(&ledger, SubId(1), 0, get_qrinfo(5), Priority::Other);
        let peers = [TestRoutePeer::new(1), TestRoutePeer::new(2), TestRoutePeer::new(3)];
        for secs in [10, 30] {
            let (peer, _, _) = route_one(&ledger, &peers);
            clock.advance(Duration::from_secs(secs));
            assert!(ledger.expire().culprits.is_empty(), "within {secs}s");
            clock.advance(Duration::from_secs(1));
            assert_eq!(ledger.expire().culprits, HashSet::from([peer]));
        }
        let (peer, attempt, _) = route_one(&ledger, &peers);
        clock.advance(Duration::from_secs(59));
        assert_eq!(ledger.expire(), Expired::default());
        assert_eq!(
            hit(&ledger, peer, &reply),
            Some((id, attempt)),
            "a slow third reply still counts"
        );
    }

    #[test]
    fn deadline_horizon_is_the_latest_unresolved_deadline() {
        let (ledger, clock) = pure_ledger();
        let peers = [TestRoutePeer::new(1)];
        let peer = peers[0].id();
        assert_eq!(ledger.deadline_horizon(peer), Horizon::None);
        let first = req(&ledger, SubId(1), 0, get_headers(1), Priority::Headers);
        let start = clock.steady();
        route_one(&ledger, &peers);
        clock.advance(Duration::from_secs(5));
        req(&ledger, SubId(1), 0, get_headers(2), Priority::Headers);
        let (_, attempt, _) = reserve_one(&ledger, &peers);
        assert_eq!(
            ledger.deadline_horizon(peer),
            Horizon::At(start + TIMEOUT),
            "a reservation has none"
        );
        assert!(send(&ledger, peer, attempt));
        assert_eq!(
            ledger.deadline_horizon(peer),
            Horizon::At(start + Duration::from_secs(5) + TIMEOUT)
        );
        ledger.cancel(first);
        tag_of(&ledger, peer, &NetworkMessage::Headers(vec![]));
        assert_eq!(
            ledger.deadline_horizon(peer),
            Horizon::At(start + Duration::from_secs(5) + TIMEOUT),
            "the orphan answered, the live slot remains"
        );
        tag_of(&ledger, peer, &NetworkMessage::Headers(vec![]));
        assert_eq!(ledger.deadline_horizon(peer), Horizon::None);
    }

    #[test]
    fn park_suspends_expiry_and_shifts_deadlines() {
        let (ledger, clock) = pure_ledger();
        req(&ledger, SubId(1), 0, get_headers(1), Priority::Headers);
        let (peer, _, _) = route_one(&ledger, &[TestRoutePeer::new(1)]);
        let sent = clock.steady();
        clock.advance(Duration::from_secs(5));
        ledger.block(peer, BlockReason::Park, clock.steady());
        clock.advance(Duration::from_secs(95));
        assert_eq!(ledger.expire(), Expired::default());
        assert_eq!(ledger.deadline_horizon(peer), Horizon::Blocked);
        ledger.unblock(peer, BlockReason::Park, clock.steady());
        assert_eq!(
            ledger.deadline_horizon(peer),
            Horizon::At(sent + TIMEOUT + Duration::from_secs(95))
        );
        clock.advance(Duration::from_secs(5));
        assert_eq!(ledger.expire(), Expired::default());
        clock.advance(Duration::from_secs(1));
        assert_eq!(ledger.expire().culprits, HashSet::from([peer]));
    }

    #[test]
    fn a_send_during_a_park_shifts_only_by_the_blocked_time_after_it() {
        let (ledger, clock) = pure_ledger();
        req(&ledger, SubId(1), 0, get_headers(1), Priority::Headers);
        req(&ledger, SubId(1), 1, get_headers(2), Priority::Headers);
        let routes = ledger.route(&[TestRoutePeer::new(1)], u64::MAX, &ResponseCosts::new());
        assert_eq!(routes.len(), 2);
        let ((peer, before, _), (_, during, _)) = (&routes[0], &routes[1]);
        let start = clock.steady();
        let at = |secs| start + Duration::from_secs(secs);
        assert!(send(&ledger, *peer, *before));
        ledger.block(*peer, BlockReason::Park, at(1));
        clock.advance(Duration::from_secs(599));
        assert!(send(&ledger, *peer, *during), "a reservation made before the park is sent");
        clock.advance(Duration::from_secs(1));
        ledger.unblock(*peer, BlockReason::Park, at(600));
        assert_eq!(
            ledger.deadline_horizon(*peer),
            Horizon::At(at(599) + TIMEOUT + Duration::from_secs(1)),
            "one blocked second after its send"
        );
        clock.advance(Duration::from_secs(9));
        assert!(culprits(&ledger).is_empty());
        clock.advance(Duration::from_secs(1));
        assert_eq!(culprits(&ledger), HashSet::from([*peer]), "the earlier send moved by 599 s");
        clock.advance(Duration::from_secs(1));
        assert_eq!(culprits(&ledger), HashSet::from([*peer]), "the send during the park");
    }

    #[test]
    fn budget_blocks_every_connection_orphans_included() {
        let (ledger, clock) = pure_ledger();
        let orphaned = req(&ledger, SubId(1), 0, get_headers(1), Priority::Headers);
        let (first, _, _) = route_one(&ledger, &[TestRoutePeer::new(1)]);
        req(&ledger, SubId(1), 0, get_headers(2), Priority::Headers);
        let (second, _, _) = route_one(&ledger, &[TestRoutePeer::new(2)]);
        ledger.cancel(orphaned);
        ledger.block_all(BlockReason::Budget, clock.steady());
        clock.advance(Duration::from_secs(600));
        assert_eq!(ledger.expire(), Expired::default());
        for peer in [first, second, conn(3, 0)] {
            assert_eq!(ledger.deadline_horizon(peer), Horizon::Blocked);
        }
        ledger.unblock_all(BlockReason::Budget, clock.steady());
        assert_eq!(ledger.expire(), Expired::default(), "every deadline moved by the blocked time");
        clock.advance(TIMEOUT + Duration::from_secs(1));
        let expired = ledger.expire();
        assert_eq!(expired.unanswered, HashSet::from([first]));
        assert_eq!(expired.culprits, HashSet::from([second]));
    }

    #[test]
    fn overlapping_reasons_shift_once() {
        let (ledger, clock) = pure_ledger();
        req(&ledger, SubId(1), 0, get_headers(1), Priority::Headers);
        let (peer, _, _) = route_one(&ledger, &[TestRoutePeer::new(1)]);
        let sent = clock.steady();
        let at = |secs| sent + Duration::from_secs(secs);
        ledger.block(peer, BlockReason::Park, at(1));
        ledger.block_all(BlockReason::Budget, at(3));
        ledger.unblock(peer, BlockReason::Park, at(5));
        assert_eq!(ledger.deadline_horizon(peer), Horizon::Blocked, "the budget still blocks it");
        ledger.unblock_all(BlockReason::Budget, at(9));
        assert_eq!(
            ledger.deadline_horizon(peer),
            Horizon::At(sent + TIMEOUT + Duration::from_secs(8))
        );
    }

    #[test]
    fn read_blocked_connections_get_no_route_and_no_hedge() {
        let (ledger, clock) = pure_ledger();
        let mut peers = [TestRoutePeer::new(1), TestRoutePeer::new(2)];
        ledger.block(peers[0].id(), BlockReason::Park, clock.steady());
        req(&ledger, SubId(1), 0, get_blocks(&[1]), Priority::Blocks);
        let (primary, _, _) = route_one(&ledger, &peers);
        assert_eq!(primary, peers[1].id(), "nothing new to a parked connection");
        clock.advance(HEDGE_AFTER * 4);
        assert!(ledger.hedge(&peers, &ResponseCosts::new()).is_none(), "no hedge onto it");
        ledger.unblock(peers[0].id(), BlockReason::Park, clock.steady());
        peers[1].blocked = true;
        assert!(ledger.hedge(&peers, &ResponseCosts::new()).is_none(), "no hedge of a send on it");
        peers[1].blocked = false;
        ledger.block(primary, BlockReason::Park, clock.steady());
        assert!(ledger.hedge(&peers, &ResponseCosts::new()).is_none());
        ledger.unblock(primary, BlockReason::Park, clock.steady());
        assert_eq!(ledger.hedge(&peers, &ResponseCosts::new()).unwrap().0, peers[0].id());
        req(&ledger, SubId(1), 0, get_headers(1), Priority::Headers);
        peers[0].blocked = true;
        assert_eq!(route_one(&ledger, &peers).0, peers[1].id());
    }

    #[test]
    fn cancel_orphan_across_long_park() {
        let (ledger, clock) = pure_ledger();
        let id = req(&ledger, SubId(1), 0, get_headers(1), Priority::Headers);
        let (peer, _, _) = route_one(&ledger, &[TestRoutePeer::new(1)]);
        ledger.cancel(id);
        ledger.block(peer, BlockReason::Park, clock.steady());
        clock.advance(Duration::from_secs(3600));
        assert_eq!(ledger.expire(), Expired::default(), "the orphan's silence is ours");
        ledger.unblock(peer, BlockReason::Park, clock.steady());
        assert!(ledger.response(peer, &NetworkMessage::Headers(vec![])).is_some());
        clock.advance(TIMEOUT * 2);
        assert_eq!(ledger.expire(), Expired::default());
    }

    #[test]
    fn hedge_orphan_across_long_park() {
        let (ledger, clock) = pure_ledger();
        let block = Block::dummy(1, vec![]);
        req(&ledger, SubId(1), 0, getdata(&[&block]), Priority::Blocks);
        let peers = [TestRoutePeer::new(1), TestRoutePeer::new(2)];
        let (primary, _, _) = route_one(&ledger, &peers[..1]);
        clock.advance(HEDGE_AFTER);
        let (hedge, _, _) = hedge_one(&ledger, &peers).unwrap();
        assert!(tag_of(&ledger, hedge, &NetworkMessage::Block(block.clone())).unwrap().last);
        ledger.block(primary, BlockReason::Park, clock.steady());
        clock.advance(Duration::from_secs(3600));
        assert_eq!(ledger.expire(), Expired::default());
        ledger.unblock(primary, BlockReason::Park, clock.steady());
        clock.advance(TIMEOUT);
        assert_eq!(
            ledger.expire().unanswered,
            HashSet::from([primary]),
            "silence after the park counts"
        );
    }

    /// Requests the filters of blocks `start` to `stop`.
    fn range(start: u32, stop: u32) -> GetCFilters {
        GetCFilters {
            filter_type: 0,
            start_height: start,
            stop_hash: BlockHash::dummy(stop),
        }
    }

    fn get_headers(locator: u32) -> NetworkMessage {
        NetworkMessage::GetHeaders(GetHeadersMessage::new(
            vec![BlockHash::dummy(locator)],
            BlockHash::dummy(0),
        ))
    }

    fn get_cfilters(start: u32) -> NetworkMessage {
        NetworkMessage::GetCFilters(GetCFilters {
            filter_type: 0,
            start_height: start,
            stop_hash: BlockHash::dummy(9),
        })
    }

    /// Requests the filters of blocks `start` and `start + 1`.
    fn two_filter_range(start: u32) -> NetworkMessage {
        NetworkMessage::GetCFilters(GetCFilters {
            filter_type: 0,
            start_height: start,
            stop_hash: BlockHash::dummy(start + 1),
        })
    }

    fn get_cfheaders(stop: u32) -> NetworkMessage {
        NetworkMessage::GetCFHeaders(GetCFHeaders {
            filter_type: 0,
            start_height: 0,
            stop_hash: BlockHash::dummy(stop),
        })
    }

    /// Requests the diff from block `base` to block 4.
    fn get_mnlistdiff(base: u32) -> NetworkMessage {
        NetworkMessage::GetMnListD(GetMnListDiff {
            base_block_hash: BlockHash::dummy(base),
            block_hash: BlockHash::dummy(4),
        })
    }

    fn block(n: u32) -> Inventory {
        Inventory::Block(BlockHash::dummy(n))
    }

    fn tx(n: u8) -> Inventory {
        Inventory::Transaction(Txid::from_byte_array([n; 32]))
    }

    fn conn(addr: u8, n: u64) -> PeerId {
        PeerId::for_test(test_socket_address(addr), n)
    }

    fn start_heights(msgs: &[(u32, NetworkMessage)]) -> Vec<u32> {
        msgs.iter()
            .map(|(_, m)| match m {
                NetworkMessage::GetCFilters(g) => g.start_height,
                other => panic!("unexpected {other:?}"),
            })
            .collect()
    }

    #[test_case(get_headers(1) => vec![RequestKey::Headers(BlockHash::dummy(1))]; "getheaders")]
    #[test_case(get_cfheaders(2) => vec![RequestKey::CfHeaders(BlockHash::dummy(2))]; "getcfheaders")]
    #[test_case(get_cfilters(100) => vec![RequestKey::CFilters(100)]; "getcfilters")]
    #[test_case(get_mnlistdiff(3) => vec![RequestKey::MnListDiff(BlockHash::dummy(4))]; "getmnlistd")]
    #[test_case(NetworkMessage::GetData(vec![block(1), tx(7), block(2)])
        => vec![RequestKey::Block(BlockHash::dummy(1)), RequestKey::Block(BlockHash::dummy(2))];
        "getdata splits per block and ignores other inventory")]
    #[test_case(NetworkMessage::GetData(vec![tx(1)]) => Vec::<RequestKey>::new(); "getdata tx")]
    #[test_case(NetworkMessage::MemPool => Vec::<RequestKey>::new(); "mempool")]
    fn request_keys_identify_tracked_requests(msg: NetworkMessage) -> Vec<RequestKey> {
        request_keys(&msg)
    }

    #[test_case(conn(1, 0), conn(2, 0), conn(1, 0), &[0]; "another peer's work is untouched")]
    #[test_case(conn(1, 0), conn(1, 1), conn(1, 0), &[0]; "the closed connection gives its work back")]
    #[test_case(conn(1, 1), conn(2, 0), conn(1, 0), &[]; "a late close leaves the replacement's work")]
    #[tokio::test]
    async fn losing_a_connection_requeues_only_its_requests(
        carrier_of_0: PeerId,
        carrier_of_1000: PeerId,
        gone: PeerId,
        expected: &[u32],
    ) {
        let broker = Broker::new();
        broker.mark_on_wire(&get_cfilters(0), carrier_of_0).await;
        broker.mark_on_wire(&get_cfilters(1000), carrier_of_1000).await;

        broker.requeue_from(&HashSet::from([gone])).await;

        assert_eq!(start_heights(&broker.queue.pop_n(2).await), expected);
        for height in [0, 1000] {
            let want = if expected.contains(&height) {
                "queued"
            } else {
                "on_wire"
            };
            assert_eq!(broker.state_of(&RequestKey::CFilters(height)).await, Some(want));
        }
    }

    #[tokio::test]
    async fn a_requeued_request_dedups_until_its_response_arrives() {
        let broker = Broker::new();
        let peer = conn(1, 0);
        broker.mark_on_wire(&get_cfilters(0), peer).await;
        broker.requeue_from(&HashSet::from([peer])).await;
        broker.queue.pop_n(1).await;
        assert_eq!(broker.state_of(&RequestKey::CFilters(0)).await, Some("queued"));

        broker.enqueue(0, get_cfilters(0)).await;
        assert_eq!(broker.len(), 0);

        broker.release(&RequestKey::CFilters(0)).await;
        assert_eq!(broker.state_of(&RequestKey::CFilters(0)).await, None);
        broker.enqueue(0, get_cfilters(0)).await;
        assert_eq!(broker.len(), 1);
    }

    /// A block at a slow peer gets a second copy only once it is the lowest on the wire and
    /// its peer stopped delivering.
    #[tokio::test(start_paused = true)]
    async fn hedge_asks_another_peer_only_for_the_lowest_stalled_block() {
        let broker = Broker::new();
        let costs = ResponseCosts::new();
        let peers = [
            ConnectedPeer::for_test(conn(1, 0)).await.handle(),
            ConnectedPeer::for_test(conn(2, 0)).await.handle(),
        ];
        let (slow, idle) = (&peers[..1], peers[1].id());
        let get_block = |n: u32| NetworkMessage::GetData(vec![block(n)]);
        let hedged_blocks = || async {
            let mut hedged: Vec<u32> = broker
                .requests
                .lock()
                .await
                .values()
                .filter_map(|state| match state {
                    ReqState::OnWire(o) if o.hedge == Some(idle) => Some(o.height),
                    _ => None,
                })
                .collect();
            hedged.sort();
            hedged
        };

        for n in [11, 12] {
            broker.enqueue(n, get_block(n)).await;
        }
        assert_eq!(broker.route(slow, u64::MAX, &costs).await, 2);
        tokio::time::advance(HEDGE_AFTER).await;
        broker.enqueue(10, get_block(10)).await;
        assert_eq!(broker.route(slow, u64::MAX, &costs).await, 1);
        assert_eq!(broker.hedge(&peers).await, None, "the lowest block has not waited long enough");

        tokio::time::advance(HEDGE_AFTER / 2).await;
        broker.progress(slow[0].id()).await;
        tokio::time::advance(HEDGE_AFTER / 2).await;
        assert_eq!(broker.hedge(&peers).await, None, "the slow peer is still delivering");

        tokio::time::advance(HEDGE_AFTER / 2).await;
        assert_eq!(broker.hedge(&peers).await, Some(10));
        assert_eq!(broker.hedge(&peers).await, None, "later blocks wait for the lowest");
        assert_eq!(hedged_blocks().await, [10]);

        broker.block_arrived(idle, BlockHash::dummy(10)).await;
        assert_eq!(broker.hedge(&peers).await, Some(11));
        assert_eq!(hedged_blocks().await, [11]);
        assert_eq!(broker.hedge(slow).await, None, "no other peer to ask");
    }

    #[tokio::test]
    async fn an_arrived_block_dedups_until_released() {
        let broker = Broker::new();
        let id = conn(1, 0);
        let getdata = NetworkMessage::GetData(vec![block(1)]);
        let key = RequestKey::Block(BlockHash::dummy(1));

        broker.enqueue(0, getdata.clone()).await;
        assert_eq!(broker.queue.pop_n(1).await.len(), 1);
        broker.mark_on_wire(&getdata, id).await;
        broker.block_arrived(conn(2, 0), BlockHash::dummy(1)).await;
        assert_eq!(broker.state_of(&key).await, Some("on_wire"), "only its carrier settles it");
        broker.block_arrived(id, BlockHash::dummy(1)).await;
        assert_eq!(broker.state_of(&key).await, Some("arrived"));

        broker.enqueue(0, getdata.clone()).await;
        assert_eq!(broker.len(), 0, "a re-declare before the release must not fetch it again");
        assert_eq!(broker.requeue_from(&HashSet::from([id])).await, 0);
        assert!(broker.expire(|_| Duration::ZERO).await.is_empty());
        assert_eq!(broker.state_of(&key).await, Some("arrived"));

        broker.release(&key).await;
        assert_eq!(broker.state_of(&key).await, None);
        broker.enqueue(0, getdata).await;
        assert_eq!(broker.len(), 1);
    }

    #[tokio::test]
    async fn an_answered_filter_range_is_retried_without_blaming_its_peer() {
        let broker = Broker::new();
        let id = conn(1, 0);
        broker.mark_sized_on_wire(&get_cfilters(0), id, Some(1)).await;
        broker.mark_sized_on_wire(&get_cfilters(1000), conn(2, 0), Some(1)).await;

        broker.filter_arrived(id, BlockHash::dummy(8)).await;
        broker.filter_arrived(conn(3, 0), BlockHash::dummy(9)).await;
        assert_eq!(
            broker.expire(|_| Duration::ZERO).await,
            HashSet::from([id, conn(2, 0)]),
            "only the carrier's last filter answers a range"
        );

        broker.mark_sized_on_wire(&get_cfilters(0), id, Some(1)).await;
        broker.filter_arrived(id, BlockHash::dummy(9)).await;
        assert_eq!(broker.expire(|_| Duration::ZERO).await, HashSet::from([conn(2, 0)]));
        assert_eq!(broker.queued_attempts(&RequestKey::CFilters(0)).await, Some(0));
        assert_eq!(broker.state_of(&RequestKey::CFilters(1000)).await, Some("on_wire"));
    }

    #[tokio::test]
    async fn a_range_answered_with_only_its_stop_filter_blames_its_peer() {
        let broker = Broker::new();
        let id = conn(1, 0);
        broker.mark_sized_on_wire(&get_cfilters(0), id, Some(3)).await;

        broker.filter_arrived(id, BlockHash::dummy(9)).await;
        assert_eq!(broker.expire(|_| Duration::ZERO).await, HashSet::from([id]));
    }

    #[tokio::test]
    async fn an_answered_filter_range_is_retried_for_free_only_once() {
        let broker = Broker::new();
        let id = conn(1, 0);
        for culprits in [HashSet::new(), HashSet::from([id])] {
            broker.mark_sized_on_wire(&get_cfilters(0), id, Some(1)).await;
            broker.filter_arrived(id, BlockHash::dummy(9)).await;
            assert_eq!(broker.expire(|_| Duration::ZERO).await, culprits);
        }
    }

    #[tokio::test]
    async fn a_peers_filters_count_toward_its_oldest_unfinished_range() {
        let broker = Broker::new();
        let id = conn(1, 0);
        broker.mark_sized_on_wire(&two_filter_range(0), id, Some(2)).await;
        broker.mark_sized_on_wire(&two_filter_range(2), id, Some(2)).await;

        for n in 0..4 {
            broker.filter_arrived(id, BlockHash::dummy(n)).await;
        }
        assert_eq!(broker.expire(|_| Duration::ZERO).await, HashSet::new());
        assert_eq!(broker.queued_attempts(&RequestKey::CFilters(0)).await, Some(0));
        assert_eq!(broker.queued_attempts(&RequestKey::CFilters(2)).await, Some(0));
    }

    #[tokio::test]
    async fn routed_ranges_are_answered_by_their_batches_in_send_order() {
        let peer = ConnectedPeer::for_test(conn(1, 0)).await;
        peer.set_cap(u64::MAX / 2);
        let broker = Broker::new();
        let starts: Vec<u32> = (0..12).step_by(2).collect();
        for &start in &starts {
            broker.enqueue_filters(start, two_filter_range(start), 2).await;
        }
        let routed = broker.route(&[peer.handle()], u64::MAX, &ResponseCosts::new()).await;
        assert_eq!(routed, starts.len());

        for n in 0..12 {
            broker.filter_arrived(conn(1, 0), BlockHash::dummy(n)).await;
        }
        assert_eq!(broker.expire(|_| Duration::ZERO).await, HashSet::new());
        for start in starts {
            assert_eq!(broker.queued_attempts(&RequestKey::CFilters(start)).await, Some(0));
        }
    }

    /// Routes `broker`'s queue to `handle` on its own task, so a test can act mid-write.
    fn spawn_route(broker: &Arc<Broker>, handle: &PeerHandle) -> tokio::task::JoinHandle<usize> {
        let (broker, handle) = (broker.clone(), handle.clone());
        tokio::spawn(async move { broker.route(&[handle], u64::MAX, &ResponseCosts::new()).await })
    }

    /// Yields until the router has charged its write to `handle`.
    async fn until_writing(handle: &PeerHandle) {
        for _ in 0..1000 {
            if handle.in_flight_bytes() > 0 {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("the router never started its write");
    }

    #[tokio::test]
    async fn a_filter_that_arrives_while_its_request_is_written_is_counted() {
        let peer = ConnectedPeer::for_test(conn(1, 0)).await;
        let handle = peer.handle();
        let broker = Arc::new(Broker::new());
        broker.enqueue_filters(0, two_filter_range(0), 1).await;

        let writes = handle.hold_writes().await;
        let router = spawn_route(&broker, &handle);
        until_writing(&handle).await;
        broker.filter_arrived(conn(1, 0), BlockHash::dummy(1)).await;
        drop(writes);
        assert_eq!(router.await.expect("router task"), 1);

        assert_eq!(broker.expire(|_| Duration::ZERO).await, HashSet::new());
        assert_eq!(broker.queued_attempts(&RequestKey::CFilters(0)).await, Some(0));
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_write_restores_its_range() {
        let peer = ConnectedPeer::for_test(conn(1, 0)).await;
        let handle = peer.handle();
        let broker = Arc::new(Broker::new());
        broker.enqueue_filters(0, two_filter_range(0), 2).await;

        let writes = handle.hold_writes().await;
        assert_eq!(spawn_route(&broker, &handle).await.expect("router task"), 0);
        drop(writes);
        assert_eq!(broker.queued_attempts(&RequestKey::CFilters(0)).await, Some(0));
        assert_eq!(broker.len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_write_requeued_meanwhile_is_queued_once() {
        let peer = ConnectedPeer::for_test(conn(1, 0)).await;
        let handle = peer.handle();
        let broker = Arc::new(Broker::new());
        broker.enqueue_filters(0, two_filter_range(0), 2).await;

        let writes = handle.hold_writes().await;
        let router = spawn_route(&broker, &handle);
        until_writing(&handle).await;
        assert_eq!(broker.requeue_from(&HashSet::from([conn(1, 0)])).await, 1);
        assert_eq!(router.await.expect("router task"), 0);
        drop(writes);
        assert_eq!(broker.len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_write_resent_meanwhile_keeps_the_resend() {
        let peer = ConnectedPeer::for_test(conn(1, 0)).await;
        let handle = peer.handle();
        let broker = Arc::new(Broker::new());
        broker.enqueue_filters(0, two_filter_range(0), 2).await;

        let writes = handle.hold_writes().await;
        let router = spawn_route(&broker, &handle);
        until_writing(&handle).await;
        broker.requeue_from(&HashSet::from([conn(1, 0)])).await;
        broker.mark_sized_on_wire(&two_filter_range(0), conn(2, 0), Some(2)).await;
        assert_eq!(router.await.expect("router task"), 0);
        drop(writes);
        assert_eq!(broker.state_of(&RequestKey::CFilters(0)).await, Some("on_wire"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_keyless_write_is_requeued() {
        let peer = ConnectedPeer::for_test(conn(1, 0)).await;
        let handle = peer.handle();
        let broker = Arc::new(Broker::new());
        broker.enqueue(0, NetworkMessage::GetAddr).await;

        let writes = handle.hold_writes().await;
        assert_eq!(spawn_route(&broker, &handle).await.expect("router task"), 0);
        drop(writes);
        assert_eq!(broker.len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_request_is_timed_from_the_end_of_its_write() {
        let peer = ConnectedPeer::for_test(conn(1, 0)).await;
        let handle = peer.handle();
        let broker = Arc::new(Broker::new());
        broker.enqueue_filters(0, two_filter_range(0), 2).await;

        let writes = handle.hold_writes().await;
        let router = spawn_route(&broker, &handle);
        until_writing(&handle).await;
        tokio::time::advance(Duration::from_secs(4)).await;
        drop(writes);
        assert_eq!(router.await.expect("router task"), 1);
        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(broker.expire(|_| Duration::from_secs(6)).await, HashSet::new());
    }

    #[tokio::test]
    async fn an_answered_range_keeps_its_free_retry_when_its_carrier_drops() {
        let broker = Broker::new();
        let (first, second) = (conn(1, 0), conn(2, 0));
        broker.mark_sized_on_wire(&get_cfilters(0), first, Some(1)).await;
        broker.filter_arrived(first, BlockHash::dummy(9)).await;
        assert_eq!(broker.requeue_from(&HashSet::from([first])).await, 1);

        broker.mark_sized_on_wire(&get_cfilters(0), second, Some(1)).await;
        broker.filter_arrived(second, BlockHash::dummy(9)).await;
        assert_eq!(broker.expire(|_| Duration::ZERO).await, HashSet::new());
        assert_eq!(broker.queued_attempts(&RequestKey::CFilters(0)).await, Some(0));
    }

    #[tokio::test]
    async fn a_requeued_block_is_settled_by_a_copy_from_any_peer() {
        let broker = Broker::new();
        let carrier = conn(1, 0);
        let getdata = NetworkMessage::GetData(vec![block(1)]);
        let key = RequestKey::Block(BlockHash::dummy(1));

        broker.mark_on_wire(&getdata, carrier).await;
        assert_eq!(broker.requeue_from(&HashSet::from([carrier])).await, 1);
        assert_eq!(broker.state_of(&key).await, Some("queued"));

        broker.block_arrived(conn(2, 0), BlockHash::dummy(1)).await;
        assert_eq!(broker.state_of(&key).await, Some("arrived"));
        assert_eq!(broker.len(), 0, "the queued getdata must not go out after the block arrived");

        broker.enqueue(0, getdata).await;
        assert_eq!(broker.len(), 0);
    }

    #[test_case(get_mnlistdiff(3), vec![RequestKey::MnListDiff(BlockHash::dummy(4))], 0; "its only key")]
    #[test_case(NetworkMessage::GetData(vec![block(1), block(2)]), vec![RequestKey::Block(BlockHash::dummy(1))], 1; "one of its keys")]
    #[test_case(NetworkMessage::GetData(vec![block(1), block(2)]), vec![RequestKey::Block(BlockHash::dummy(1)), RequestKey::Block(BlockHash::dummy(2))], 0; "all of its keys")]
    #[tokio::test]
    async fn a_released_request_is_dropped_once_none_of_its_keys_is_in_play(
        msg: NetworkMessage,
        released: Vec<RequestKey>,
        queued: usize,
    ) {
        let broker = Broker::new();
        broker.enqueue(0, msg).await;
        broker.enqueue(0, get_cfilters(0)).await;
        for key in &released {
            broker.release(key).await;
        }
        assert_eq!(broker.len(), queued + 1, "the other request stays queued");
        assert_eq!(broker.queue.pop_n(broker.len()).await.len(), queued + 1);
    }

    #[tokio::test]
    async fn a_request_released_and_declared_from_another_base_replaces_the_queued_one() {
        let broker = Broker::new();
        broker.enqueue(0, get_mnlistdiff(3)).await;
        broker.release(&RequestKey::MnListDiff(BlockHash::dummy(4))).await;
        broker.enqueue(0, get_mnlistdiff(5)).await;

        let queued: Vec<NetworkMessage> =
            broker.queue.pop_n(broker.len()).await.into_iter().map(|(_, msg)| msg).collect();
        assert_eq!(queued, vec![get_mnlistdiff(5)]);
    }

    #[test_case(vec![get_cfilters(0), get_cfilters(0), get_cfilters(1000)], 2, 2; "a request in play is dropped")]
    #[test_case(vec![NetworkMessage::MemPool, NetworkMessage::MemPool], 2, 0; "untracked traffic is never deduplicated")]
    #[tokio::test]
    async fn enqueue_dedups_by_request_key(
        msgs: Vec<NetworkMessage>,
        queued: usize,
        tracked: usize,
    ) {
        let broker = Broker::new();
        for msg in msgs {
            broker.enqueue(0, msg).await;
        }
        assert_eq!(broker.len(), queued);
        assert_eq!(broker.requests.lock().await.len(), tracked);
    }

    /// `slow` holds 1 MB at 100 KB/s, a 10 s queue at a tenth of its cap. `fast` holds
    /// 256 KB at `fast_rate`, half its cap. Returns which of them `msg` is routed to.
    #[test_case(get_headers(1), 1 << 20 => "fast"; "a header request goes to the shortest queue")]
    #[test_case(get_headers(1), 0 => "slow"; "a header request falls back to the cap ratio while a busy peer is unmeasured")]
    #[test_case(NetworkMessage::GetData(vec![block(1)]), 1 << 20 => "fast"; "a block request goes to the shortest queue")]
    #[test_case(NetworkMessage::GetData(vec![block(1)]), 0 => "slow"; "a block request falls back to the cap ratio while a busy peer is unmeasured")]
    #[test_case(get_cfilters(0), 1 << 20 => "slow"; "bulk requests keep the cap ratio")]
    #[tokio::test]
    async fn route_picks_the_peer_by_class(msg: NetworkMessage, fast_rate: u64) -> &'static str {
        let costs = ResponseCosts::new();
        let slow = ConnectedPeer::for_test(conn(1, 0)).await;
        let fast = ConnectedPeer::for_test(conn(2, 0)).await;
        for start in [0, 1000, 2000, 3000] {
            slow.handle().send(&get_cfilters(start)).await.expect("loopback send");
        }
        fast.handle().send(&get_cfilters(4000)).await.expect("loopback send");
        slow.set_cap(slow.in_flight_bytes() * 10);
        fast.set_cap(fast.in_flight_bytes() * 2);
        slow.set_serving_rate(100 << 10);
        fast.set_serving_rate(fast_rate);
        let before = (slow.in_flight_bytes(), fast.in_flight_bytes());

        let broker = Broker::new();
        broker.enqueue(0, msg).await;
        assert_eq!(broker.route(&[slow.handle(), fast.handle()], u64::MAX, &costs).await, 1);

        match (slow.in_flight_bytes() > before.0, fast.in_flight_bytes() > before.1) {
            (true, false) => "slow",
            (false, true) => "fast",
            other => panic!("routed to {other:?}"),
        }
    }

    #[test_case(get_headers(1) => MsgClass::Headers)]
    #[test_case(get_cfheaders(1) => MsgClass::CfHeaders)]
    #[test_case(get_cfilters(0) => MsgClass::CFilters)]
    #[test_case(NetworkMessage::GetData(vec![block(1)]) => MsgClass::Blocks; "getdata blocks")]
    #[test_case(NetworkMessage::GetData(vec![tx(1)]) => MsgClass::Other; "getdata tx is control traffic")]
    #[test_case(NetworkMessage::MemPool => MsgClass::Other)]
    fn classify_cases(msg: NetworkMessage) -> MsgClass {
        classify(&msg)
    }

    #[tokio::test]
    async fn queue_drains_in_strict_priority_order() {
        let q = MsgQueue::new();
        q.push(0, get_cfilters(0)).await;
        q.push(0, NetworkMessage::GetData(vec![block(1)])).await;
        q.push(0, get_cfheaders(1)).await;
        q.push(0, get_headers(1)).await;
        q.push(0, NetworkMessage::MemPool).await;

        let drained: Vec<MsgClass> = q.pop_n(5).await.iter().map(|(_, m)| classify(m)).collect();
        assert_eq!(
            drained,
            [
                MsgClass::Other,
                MsgClass::Blocks,
                MsgClass::CFilters,
                MsgClass::CfHeaders,
                MsgClass::Headers
            ]
        );
        assert_eq!(q.len(), 0);
    }

    #[tokio::test]
    async fn pop_n_respects_its_budget_and_unsent_messages_go_back_to_the_front() {
        let q = MsgQueue::new();
        for start in [0, 1000, 2000] {
            q.push(start, get_cfilters(start)).await;
        }

        let popped = q.pop_n(1).await;
        assert_eq!(start_heights(&popped), [0]);
        assert!(q.pop_n(0).await.is_empty());
        assert_eq!(q.len(), 2);

        q.push_front_all(popped).await;
        assert_eq!(start_heights(&q.pop_n(3).await), [0, 1000, 2000]);
    }
}
