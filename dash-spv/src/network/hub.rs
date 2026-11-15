//! Bounded subscription delivery, independent of the network and request ledger.

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::mem;
use std::num::NonZeroUsize;
use std::sync::{Arc, Weak};
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::{mpsc, Notify};
use tokio::time::Instant;

use super::{InboundMessage, MessageType};

/// Message, payload-byte and request-credit limits for one subscriber.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubCap {
    pub msgs: NonZeroUsize,
    pub bytes: NonZeroUsize,
    /// Open requests plus unread terminal failures.
    pub requests: NonZeroUsize,
}

impl SubCap {
    /// The default budget for a subscriber of this kind.
    pub fn for_kind(kind: MessageType) -> Self {
        let (msgs, mib, requests) = match kind {
            MessageType::Headers => (4, 2, 4),
            MessageType::CfHeaders => (8, 1, 16),
            MessageType::CFilter => (2_000, 4, 64),
            MessageType::Block => (8, 8, 32),
            MessageType::MnListDiff | MessageType::QrInfo => (4, 6, 8),
            _ => (256, 1, 16),
        };
        Self {
            msgs: nonzero(msgs),
            bytes: nonzero(mib * 1024 * 1024),
            requests: nonzero(requests),
        }
    }
}

impl From<NonZeroUsize> for SubCap {
    fn from(msgs: NonZeroUsize) -> Self {
        Self {
            msgs,
            bytes: nonzero(1024 * 1024),
            requests: nonzero(16),
        }
    }
}

fn nonzero(n: usize) -> NonZeroUsize {
    NonZeroUsize::new(n).unwrap_or(NonZeroUsize::MIN)
}

/// A connection's identity, never reused within a process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConnId(pub u64);

/// A connection token carried by deliveries. The hub keys parked deliveries by its `ConnId`.
pub trait Conn {
    fn key(&self) -> ConnId;
}

/// A subscription's identity, stable until it is removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SubId(u64);

/// Delivery metadata uses the request ledger's ID and failure types.
#[derive(Debug)]
pub enum Inbound<R, A, F, P> {
    Message {
        peer: P,
        msg: Arc<InboundMessage>,
        request: Option<(R, A)>,
    },
    /// A request's terminal failure. It holds the request's credit until it is received.
    RequestFailed(F),
    /// A credit is free again after `acquire_or_want` found none.
    CreditAvailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offer {
    Delivered,
    Parked,
}

/// An offer to an already parked peer. The caller retains the undelivered input.
#[derive(Debug)]
pub struct AlreadyParked<R, A, P> {
    pub peer: P,
    pub kind: MessageType,
    pub msg: Arc<InboundMessage>,
    pub bytes: usize,
    pub request: Option<(R, A)>,
}

/// The default global budget for retained message bytes, queued or parked.
pub const HUB_BYTES: usize = 24 * 1024 * 1024;

struct Queued<R, A, F, P> {
    inbound: Inbound<R, A, F, P>,
    // Messages only: their size and their global charge.
    charge: Option<(usize, u64)>,
}

/// One notification for a subscription's current stall episode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubscriberStalled {
    pub kinds: Vec<MessageType>,
}

struct Stall {
    since: Instant,
    reported: bool,
}

struct Subscriber<R, A, F, P> {
    kinds: Vec<MessageType>,
    cap: SubCap,
    queue: VecDeque<Queued<R, A, F, P>>,
    msgs: usize,
    bytes: usize,
    // Credits held by open requests and by queued failures.
    credits: usize,
    failures: usize,
    wants_credit: bool,
    marker_queued: bool,
    stall: Option<Stall>,
}

impl<R, A, F, P> Subscriber<R, A, F, P> {
    /// Whether a message of `bytes` fits. An oversized one fits only when no message is queued.
    fn fits(&self, bytes: usize) -> bool {
        self.msgs < self.cap.msgs.get()
            && (self.bytes.saturating_add(bytes) <= self.cap.bytes.get() || self.msgs == 0)
    }

    /// Returns one credit and queues `CreditAvailable` if one was wanted and none is queued.
    fn release(&mut self) {
        self.credits = self.credits.saturating_sub(1);
        if mem::take(&mut self.wants_credit) && !self.marker_queued {
            self.marker_queued = true;
            self.queue.push_back(Queued {
                inbound: Inbound::CreditAvailable,
                charge: None,
            });
        }
    }
}

struct Delivery<R, A, P> {
    peer: P,
    msg: Arc<InboundMessage>,
    bytes: usize,
    charge: u64,
    request: Option<(R, A)>,
    targets: VecDeque<SubId>,
    closed: bool,
}

/// One message's share of the global budget, held by each queued copy and its parked delivery.
struct Charge {
    bytes: usize,
    refs: usize,
}

/// A sans-io fanout queue. A caller must stop polling each parked connection.
pub struct SubscriptionHub<R, A, F, P> {
    subscribers: BTreeMap<SubId, Subscriber<R, A, F, P>>,
    // In arrival order. A closed connection's delivery stays until its targets take it.
    parked: VecDeque<Delivery<R, A, P>>,
    charges: HashMap<u64, Charge>,
    next_charge: u64,
    used: usize,
    hub_bytes: usize,
    space_wake: bool,
    next_sub: u64,
    stall_report: Duration,
    clock: Arc<dyn Fn() -> Instant + Send + Sync>,
}

impl<R: Clone, A: Clone, F, P: Clone + Conn> Default for SubscriptionHub<R, A, F, P> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R: Clone, A: Clone, F, P: Clone + Conn> SubscriptionHub<R, A, F, P> {
    pub fn new() -> Self {
        Self::with_clock(Duration::from_secs(10), Arc::new(Instant::now))
    }

    pub fn with_clock(
        stall_report: Duration,
        clock: Arc<dyn Fn() -> Instant + Send + Sync>,
    ) -> Self {
        Self {
            stall_report,
            clock,
            subscribers: BTreeMap::new(),
            parked: VecDeque::new(),
            charges: HashMap::new(),
            next_charge: 0,
            used: 0,
            hub_bytes: HUB_BYTES,
            space_wake: false,
            next_sub: 0,
        }
    }

    /// Replaces the global budget for retained message bytes.
    pub fn with_hub_bytes(mut self, hub_bytes: usize) -> Self {
        self.hub_bytes = hub_bytes;
        self
    }

    pub fn subscribe(&mut self, kinds: &[MessageType], cap: SubCap) -> SubId {
        let sub = SubId(self.next_sub);
        self.next_sub += 1;
        let mut unique = Vec::new();
        for kind in kinds {
            if !unique.contains(kind) {
                unique.push(*kind);
            }
        }
        self.subscribers.insert(
            sub,
            Subscriber {
                kinds: unique,
                cap,
                queue: VecDeque::new(),
                msgs: 0,
                bytes: 0,
                credits: 0,
                failures: 0,
                wants_credit: false,
                marker_queued: false,
                stall: None,
            },
        );
        sub
    }

    /// Fans a message out, `bytes` being its retained decoded size.
    pub fn offer(
        &mut self,
        peer: P,
        kind: MessageType,
        msg: Arc<InboundMessage>,
        bytes: usize,
        request: Option<(R, A)>,
    ) -> Result<Offer, AlreadyParked<R, A, P>> {
        if self.is_parked(peer.key()) {
            return Err(AlreadyParked {
                peer,
                kind,
                msg,
                bytes,
                request,
            });
        }
        let targets: VecDeque<_> = self
            .subscribers
            .iter()
            .filter_map(|(id, sub)| sub.kinds.contains(&kind).then_some(*id))
            .collect();
        if targets.is_empty() {
            return Ok(Offer::Delivered);
        }
        let charge = self.next_charge;
        self.next_charge += 1;
        self.charges.insert(
            charge,
            Charge {
                bytes,
                refs: 1,
            },
        );
        self.used += bytes;
        let delivery = Delivery {
            peer,
            msg,
            bytes,
            charge,
            request,
            targets,
            closed: false,
        };
        let offer = self.deliver(delivery);
        self.refresh_stalls();
        Ok(offer)
    }

    fn deliver(&mut self, mut delivery: Delivery<R, A, P>) -> Offer {
        while let Some(id) = delivery.targets.pop_front() {
            // An earlier delivery blocked on this subscription goes first, so none starves.
            let behind = self.parked.iter().any(|parked| parked.targets.front() == Some(&id));
            let Some(sub) = self.subscribers.get_mut(&id) else {
                continue;
            };
            if behind || !sub.fits(delivery.bytes) {
                delivery.targets.push_front(id);
                self.parked.push_back(delivery);
                return Offer::Parked;
            }
            sub.msgs += 1;
            sub.bytes += delivery.bytes;
            sub.queue.push_back(Queued {
                inbound: Inbound::Message {
                    peer: delivery.peer.clone(),
                    msg: Arc::clone(&delivery.msg),
                    request: delivery.request.clone(),
                },
                charge: Some((delivery.bytes, delivery.charge)),
            });
            if let Some(charge) = self.charges.get_mut(&delivery.charge) {
                charge.refs += 1;
            }
        }
        self.unref(delivery.charge);
        Offer::Delivered
    }

    fn unref(&mut self, id: u64) {
        let Entry::Occupied(mut entry) = self.charges.entry(id) else {
            return;
        };
        entry.get_mut().refs -= 1;
        if entry.get().refs > 0 {
            return;
        }
        let bytes = entry.remove().bytes;
        let was_full = !self.may_poll();
        self.used -= bytes;
        if was_full && self.may_poll() {
            self.space_wake = true;
        }
    }

    /// Whether the driver may poll another message: the global budget is not used up.
    pub fn may_poll(&self) -> bool {
        self.used < self.hub_bytes
    }

    /// Whether global use fell below the budget since the last call.
    fn take_space_wake(&mut self) -> bool {
        mem::take(&mut self.space_wake)
    }

    /// Releases payload accounting. Call `on_space` before polling parked peers again.
    pub fn recv(&mut self, id: SubId) -> Option<Inbound<R, A, F, P>> {
        let sub = self.subscribers.get_mut(&id)?;
        let queued = sub.queue.pop_front()?;
        match queued.inbound {
            Inbound::RequestFailed(_) => {
                sub.failures -= 1;
                sub.release();
            }
            Inbound::CreditAvailable => sub.marker_queued = false,
            Inbound::Message {
                ..
            } => {}
        }
        if let Some((bytes, charge)) = queued.charge {
            sub.msgs -= 1;
            sub.bytes -= bytes;
            self.unref(charge);
        }
        Some(queued.inbound)
    }

    pub fn stalled(&mut self, now: Instant) -> Vec<SubscriberStalled> {
        self.subscribers
            .values_mut()
            .filter_map(|sub| {
                let stall = sub.stall.as_mut()?;
                if !stall.reported
                    && now.saturating_duration_since(stall.since) >= self.stall_report
                {
                    stall.reported = true;
                    Some(SubscriberStalled {
                        kinds: sub.kinds.clone(),
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    /// Whether an open connection's delivery is parked. A closed connection is never parked.
    pub fn is_parked(&self, conn: ConnId) -> bool {
        self.parked.iter().any(|delivery| !delivery.closed && delivery.peer.key() == conn)
    }

    /// Retries pending fanout and returns the open connections whose delivery is complete.
    pub fn on_space(&mut self, _sub: SubId) -> Vec<P> {
        let pending = mem::take(&mut self.parked);
        let mut resumed = Vec::new();
        for delivery in pending {
            let resume = (!delivery.closed).then(|| delivery.peer.clone());
            if self.deliver(delivery) == Offer::Delivered {
                resumed.extend(resume);
            }
        }
        self.refresh_stalls();
        resumed
    }

    /// A subscription is stalled while a parked delivery waits on it.
    fn refresh_stalls(&mut self) {
        for (id, sub) in &mut self.subscribers {
            if self.parked.iter().any(|delivery| delivery.targets.front() == Some(id)) {
                sub.stall.get_or_insert_with(|| Stall {
                    since: (self.clock)(),
                    reported: false,
                });
            } else {
                sub.stall = None;
            }
        }
    }

    pub fn remove(&mut self, sub: SubId) -> Vec<P> {
        if let Some(removed) = self.subscribers.remove(&sub) {
            for queued in removed.queue {
                if let Some((_, charge)) = queued.charge {
                    self.unref(charge);
                }
            }
        }
        self.on_space(sub)
    }

    /// Marks the connection closed. Its parked delivery still reaches every remaining target.
    pub fn disconnect(&mut self, conn: ConnId) {
        for delivery in &mut self.parked {
            if delivery.peer.key() == conn {
                delivery.closed = true;
            }
        }
    }

    /// Takes a request credit, or records that the subscription wants one.
    /// A later `release` then queues one `CreditAvailable`. A removed subscription gets nothing.
    pub fn acquire_or_want(&mut self, sub: SubId) -> bool {
        let Some(sub) = self.subscribers.get_mut(&sub) else {
            return false;
        };
        if sub.credits < sub.cap.requests.get() {
            sub.credits += 1;
            true
        } else {
            sub.wants_credit = true;
            false
        }
    }

    /// Returns a request's credit when it ends without a terminal failure.
    pub fn release(&mut self, sub: SubId) {
        if let Some(sub) = self.subscribers.get_mut(&sub) {
            if sub.credits > sub.failures {
                sub.release();
            }
        }
    }

    /// Queues a request's terminal failure, which keeps the request's credit until received.
    /// Returns false when the subscription is gone or holds no credit for another failure.
    pub fn failed(&mut self, sub: SubId, failure: F) -> bool {
        let Some(sub) = self.subscribers.get_mut(&sub) else {
            return false;
        };
        if sub.failures >= sub.credits {
            return false;
        }
        sub.failures += 1;
        sub.queue.push_back(Queued {
            inbound: Inbound::RequestFailed(failure),
            charge: None,
        });
        true
    }
}

struct Shared<R, A, F, P> {
    core: SubscriptionHub<R, A, F, P>,
    ready: BTreeMap<SubId, mpsc::Sender<()>>,
    resumed: BTreeMap<ConnId, P>,
}

impl<R: Clone, A: Clone, F, P: Clone + Conn> Shared<R, A, F, P> {
    /// Records resumed connections and returns whether the driver has new space to act on.
    fn resume(&mut self, peers: Vec<P>) -> bool {
        let space_wake = self.core.take_space_wake();
        let space = space_wake || !peers.is_empty();
        self.resumed.extend(peers.into_iter().map(|peer| (peer.key(), peer)));
        space
    }

    /// Sends a token to every subscription with queued items. Called under the lock, so a
    /// receiver that found its queue empty under the same lock always finds the token.
    fn wake(&self) {
        for (id, tx) in &self.ready {
            if self.core.subscribers.get(id).is_some_and(|sub| !sub.queue.is_empty()) {
                let _ = tx.try_send(());
            }
        }
    }
}

struct Inner<R, A, F, P> {
    shared: Mutex<Shared<R, A, F, P>>,
    space: Notify,
}

/// A thin async shell. Payloads remain in the bounded core; channels carry only wake tokens.
/// The lock is never held across an await.
pub struct AsyncHub<R, A, F, P> {
    inner: Arc<Inner<R, A, F, P>>,
}

impl<R: Clone, A: Clone, F, P: Clone + Conn> Default for AsyncHub<R, A, F, P> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R: Clone, A: Clone, F, P: Clone + Conn> AsyncHub<R, A, F, P> {
    pub fn new() -> Self {
        Self::from_core(SubscriptionHub::new())
    }

    pub fn from_core(core: SubscriptionHub<R, A, F, P>) -> Self {
        Self {
            inner: Arc::new(Inner {
                shared: Mutex::new(Shared {
                    core,
                    ready: BTreeMap::new(),
                    resumed: BTreeMap::new(),
                }),
                space: Notify::new(),
            }),
        }
    }

    pub fn subscribe(&self, kinds: &[MessageType], cap: SubCap) -> Subscription<R, A, F, P> {
        let mut shared = self.inner.shared.lock();
        let sub = shared.core.subscribe(kinds, cap);
        let (tx, rx) = mpsc::channel(1);
        shared.ready.insert(sub, tx);
        Subscription {
            rx,
            inner: Arc::downgrade(&self.inner),
            sub,
        }
    }

    pub fn offer(
        &self,
        peer: P,
        kind: MessageType,
        msg: Arc<InboundMessage>,
        bytes: usize,
        request: Option<(R, A)>,
    ) -> Result<Offer, AlreadyParked<R, A, P>> {
        let mut shared = self.inner.shared.lock();
        let result = shared.core.offer(peer, kind, msg, bytes, request);
        shared.wake();
        result
    }

    pub fn acquire_or_want(&self, sub: SubId) -> bool {
        self.inner.shared.lock().core.acquire_or_want(sub)
    }

    pub fn release(&self, sub: SubId) {
        let mut shared = self.inner.shared.lock();
        shared.core.release(sub);
        shared.wake();
    }

    pub fn failed(&self, sub: SubId, failure: F) -> bool {
        let mut shared = self.inner.shared.lock();
        let result = shared.core.failed(sub, failure);
        shared.wake();
        result
    }

    /// Whether the driver may poll another message under the global budget.
    pub fn may_poll(&self) -> bool {
        self.inner.shared.lock().core.may_poll()
    }

    /// Wakes the driver when a parked connection resumes, global use falls below the budget,
    /// or a subscription is removed.
    pub fn space(&self) -> &Notify {
        &self.inner.space
    }

    pub fn resumed(&self) -> Vec<P> {
        mem::take(&mut self.inner.shared.lock().resumed).into_values().collect()
    }

    pub fn stalled(&self, now: Instant) -> Vec<SubscriberStalled> {
        self.inner.shared.lock().core.stalled(now)
    }

    pub fn disconnect(&self, conn: ConnId) {
        let mut shared = self.inner.shared.lock();
        shared.core.disconnect(conn);
        shared.resumed.remove(&conn);
    }
}

/// Receives bounded messages and releases their payload bytes before returning them.
/// Dropping it removes the subscription.
pub struct Subscription<R: Clone, A: Clone, F, P: Clone + Conn> {
    rx: mpsc::Receiver<()>,
    inner: Weak<Inner<R, A, F, P>>,
    sub: SubId,
}

impl<R: Clone, A: Clone, F, P: Clone + Conn> Subscription<R, A, F, P> {
    pub fn id(&self) -> SubId {
        self.sub
    }

    /// Returns the next item, or `None` once the hub is gone.
    pub async fn recv(&mut self) -> Option<Inbound<R, A, F, P>> {
        loop {
            {
                let inner = self.inner.upgrade()?;
                let mut shared = inner.shared.lock();
                if let Some(inbound) = shared.core.recv(self.sub) {
                    let resumed = shared.core.on_space(self.sub);
                    let space = shared.resume(resumed);
                    shared.wake();
                    drop(shared);
                    if space {
                        inner.space.notify_one();
                    }
                    return Some(inbound);
                }
            }
            self.rx.recv().await?;
        }
    }
}

impl<R: Clone, A: Clone, F, P: Clone + Conn> Drop for Subscription<R, A, F, P> {
    fn drop(&mut self) {
        self.rx.close();
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let mut shared = inner.shared.lock();
        shared.ready.remove(&self.sub);
        let resumed = shared.core.remove(self.sub);
        shared.resume(resumed);
        shared.wake();
        drop(shared);
        inner.space.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::time::{advance, timeout, Instant};

    use dashcore::network::message::NetworkMessage;
    use tokio::task::yield_now;
    use tokio::{pin, spawn};

    use super::{
        AsyncHub, Conn, ConnId, Inbound, Offer, SubCap, SubId, SubscriptionHub, HUB_BYTES,
    };
    use crate::network::{InboundMessage, MessageType};

    type Hub = SubscriptionHub<u64, u64, &'static str, u64>;

    impl Conn for u64 {
        fn key(&self) -> ConnId {
            ConnId(*self)
        }
    }

    /// A token that is `Clone` but not `Copy`, with a display address shared across connections.
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Peer {
        addr: &'static str,
        conn: u64,
    }

    impl Conn for Peer {
        fn key(&self) -> ConnId {
            ConnId(self.conn)
        }
    }

    fn cap(msgs: usize, bytes: usize) -> SubCap {
        SubCap {
            msgs: NonZeroUsize::new(msgs).unwrap(),
            bytes: NonZeroUsize::new(bytes).unwrap(),
            requests: NonZeroUsize::new(16).unwrap(),
        }
    }

    fn msg(n: u64) -> Arc<InboundMessage> {
        Arc::new(InboundMessage::new(NetworkMessage::Ping(n)))
    }

    fn take(hub: &mut Hub, sub: SubId) -> (u64, u64) {
        match hub.recv(sub).unwrap() {
            Inbound::Message {
                peer,
                msg,
                ..
            } => match **msg {
                NetworkMessage::Ping(n) => (peer, n),
                _ => panic!("wrong payload"),
            },
            _ => panic!("wrong delivery"),
        }
    }

    #[test]
    fn fanout_waits_for_the_full_target() {
        let mut hub = Hub::new();
        let fast = hub.subscribe(&[MessageType::Inv, MessageType::Inv], cap(4, 100));
        let slow = hub.subscribe(&[MessageType::Inv], cap(1, 100));
        let other = hub.subscribe(&[MessageType::Tx], cap(1, 100));
        let first = msg(1);
        assert_eq!(
            hub.offer(1, MessageType::Inv, first.clone(), 10, Some((7, 8))).unwrap(),
            Offer::Delivered
        );
        assert_eq!(Arc::strong_count(&first), 3);
        assert_eq!(hub.offer(1, MessageType::Inv, msg(2), 10, None).unwrap(), Offer::Parked);
        assert!(hub.is_parked(ConnId(1)));
        assert_eq!(hub.offer(2, MessageType::Tx, msg(3), 10, None).unwrap(), Offer::Delivered);
        assert_eq!(take(&mut hub, other), (2, 3));
        match hub.recv(fast).unwrap() {
            Inbound::Message {
                msg,
                request,
                ..
            } => {
                assert!(Arc::ptr_eq(&msg, &first));
                assert_eq!(request, Some((7, 8)));
            }
            _ => panic!("wrong delivery"),
        }
        assert_eq!(take(&mut hub, fast), (1, 2));
        assert!(hub.recv(fast).is_none());
        assert_eq!(take(&mut hub, slow), (1, 1));
        assert_eq!(hub.on_space(slow), vec![1]);
        assert!(!hub.is_parked(ConnId(1)));
        assert_eq!(take(&mut hub, slow), (1, 2));
        assert!(hub.recv(slow).is_none());
        assert_eq!(hub.offer(1, MessageType::Inv, msg(4), 10, None).unwrap(), Offer::Delivered);
        assert_eq!(take(&mut hub, fast), (1, 4));
        assert_eq!(take(&mut hub, slow), (1, 4));
    }

    #[test]
    fn duplicate_kind_delivers_once() {
        let start = Instant::now();
        let mut hub = Hub::with_clock(Duration::from_secs(10), Arc::new(move || start));
        let sub = hub.subscribe(
            &[MessageType::NotFound, MessageType::Inv, MessageType::NotFound],
            cap(1, 100),
        );
        let first = msg(1);
        hub.offer(1, MessageType::NotFound, first.clone(), 1, None).unwrap();
        assert_eq!(Arc::strong_count(&first), 2);
        assert_eq!(hub.offer(1, MessageType::NotFound, msg(2), 1, None).unwrap(), Offer::Parked);
        let stalled = hub.stalled(start + Duration::from_secs(10));
        assert_eq!(stalled.len(), 1);
        assert_eq!(stalled[0].kinds, vec![MessageType::NotFound, MessageType::Inv]);
        assert_eq!(take(&mut hub, sub), (1, 1));
        assert_eq!(hub.on_space(sub), vec![1]);
        assert_eq!(take(&mut hub, sub), (1, 2));
        assert!(hub.recv(sub).is_none());
        assert_eq!(hub.used, 0);
    }

    #[test]
    fn parked_peers_resume_in_arrival_order() {
        let mut hub = Hub::new();
        let sub = hub.subscribe(&[MessageType::Inv], cap(1, 1));
        hub.offer(0, MessageType::Inv, msg(0), 1, None).unwrap();
        hub.offer(2, MessageType::Inv, msg(2), 1, None).unwrap();
        hub.offer(1, MessageType::Inv, msg(1), 1, None).unwrap();
        assert_eq!(take(&mut hub, sub), (0, 0));
        assert_eq!(hub.on_space(sub), vec![2]);
        hub.offer(2, MessageType::Inv, msg(3), 1, None).unwrap();
        assert_eq!(take(&mut hub, sub), (2, 2));
        assert_eq!(hub.on_space(sub), vec![1]);
        assert_eq!(take(&mut hub, sub), (1, 1));
        assert_eq!(hub.on_space(sub), vec![2]);
        assert_eq!(take(&mut hub, sub), (2, 3));
        assert!(hub.recv(sub).is_none());
    }

    #[test]
    fn byte_bound_and_oversized_message() {
        let mut hub = Hub::new();
        let sub = hub.subscribe(&[MessageType::Block], cap(4, 10));
        assert_eq!(hub.offer(1, MessageType::Block, msg(1), 6, None).unwrap(), Offer::Delivered);
        assert_eq!(hub.offer(1, MessageType::Block, msg(2), 5, None).unwrap(), Offer::Parked);
        assert_eq!(take(&mut hub, sub), (1, 1));
        assert_eq!(hub.on_space(sub), vec![1]);
        assert_eq!(take(&mut hub, sub), (1, 2));
        assert!(hub.acquire_or_want(sub));
        assert!(hub.failed(sub, "timeout"));
        assert_eq!(
            hub.offer(1, MessageType::Block, msg(3), 1_000_000, None).unwrap(),
            Offer::Delivered
        );
        assert_eq!(hub.offer(1, MessageType::Block, msg(4), 1, None).unwrap(), Offer::Parked);
        assert!(matches!(hub.recv(sub), Some(Inbound::RequestFailed { .. })));
        assert_eq!(take(&mut hub, sub), (1, 3));
        assert_eq!(hub.on_space(sub), vec![1]);
        assert_eq!(take(&mut hub, sub), (1, 4));
        assert_eq!(hub.offer(1, MessageType::Block, msg(5), 6, None).unwrap(), Offer::Delivered);
        assert_eq!(hub.offer(1, MessageType::Block, msg(6), 4, None).unwrap(), Offer::Delivered);
        assert_eq!(hub.offer(1, MessageType::Block, msg(7), 1, None).unwrap(), Offer::Parked);
    }

    #[test]
    fn dropped_targets_and_closed_connections() {
        let mut hub = Hub::new();
        let first = hub.subscribe(&[MessageType::Inv], cap(1, 100));
        let second = hub.subscribe(&[MessageType::Inv], cap(2, 100));
        hub.offer(1, MessageType::Inv, msg(1), 10, None).unwrap();
        hub.offer(1, MessageType::Inv, msg(2), 10, None).unwrap();
        assert_eq!(hub.remove(first), vec![1]);
        assert_eq!(take(&mut hub, second), (1, 1));
        assert_eq!(take(&mut hub, second), (1, 2));
        hub.offer(2, MessageType::Inv, msg(3), 10, None).unwrap();
        hub.offer(2, MessageType::Inv, msg(4), 10, None).unwrap();
        hub.offer(2, MessageType::Inv, msg(5), 10, None).unwrap();
        hub.disconnect(ConnId(2));
        assert!(!hub.is_parked(ConnId(2)));
        assert_eq!(take(&mut hub, second), (2, 3));
        assert!(hub.on_space(second).is_empty());
        assert_eq!(take(&mut hub, second), (2, 4));
        assert_eq!(take(&mut hub, second), (2, 5));
        assert!(hub.recv(second).is_none());
        hub.remove(second);
        assert_eq!(hub.offer(3, MessageType::Inv, msg(6), 10, None).unwrap(), Offer::Delivered);
    }

    #[test]
    fn close_while_parked_fanout() {
        let mut hub = Hub::new();
        let fast = hub.subscribe(&[MessageType::Inv], cap(4, 100));
        let slow = hub.subscribe(&[MessageType::Inv], cap(1, 100));
        let gone = hub.subscribe(&[MessageType::Inv], cap(1, 100));
        hub.offer(1, MessageType::Inv, msg(1), 1, None).unwrap();
        assert_eq!(hub.offer(1, MessageType::Inv, msg(2), 1, None).unwrap(), Offer::Parked);
        hub.disconnect(ConnId(1));
        assert!(!hub.is_parked(ConnId(1)));
        assert!(hub.remove(gone).is_empty());
        assert_eq!(take(&mut hub, slow), (1, 1));
        assert!(hub.on_space(slow).is_empty());
        assert_eq!(take(&mut hub, slow), (1, 2));
        assert!(hub.recv(slow).is_none());
        assert_eq!(take(&mut hub, fast), (1, 1));
        assert_eq!(take(&mut hub, fast), (1, 2));
        assert!(hub.recv(fast).is_none());
        assert_eq!(hub.offer(1, MessageType::Inv, msg(3), 1, None).unwrap(), Offer::Delivered);
    }

    #[test]
    fn close_while_parked_tagged() {
        let mut hub = Hub::new();
        let owner = hub.subscribe(&[MessageType::Inv], cap(1, 100));
        hub.offer(1, MessageType::Inv, msg(1), 1, None).unwrap();
        let tagged = msg(2);
        assert_eq!(
            hub.offer(1, MessageType::Inv, tagged.clone(), 1, Some((7, 8))).unwrap(),
            Offer::Parked
        );
        hub.disconnect(ConnId(1));
        assert_eq!(take(&mut hub, owner), (1, 1));
        assert!(hub.on_space(owner).is_empty());
        match hub.recv(owner).unwrap() {
            Inbound::Message {
                peer,
                msg,
                request,
            } => {
                assert_eq!(peer, 1);
                assert!(Arc::ptr_eq(&msg, &tagged));
                assert_eq!(request, Some((7, 8)));
            }
            _ => panic!("wrong delivery"),
        }
        assert!(hub.recv(owner).is_none());
    }

    fn credits(requests: usize) -> SubCap {
        SubCap {
            requests: NonZeroUsize::new(requests).unwrap(),
            ..cap(1, 1)
        }
    }

    fn markers(hub: &mut Hub, sub: SubId) -> usize {
        let mut n = 0;
        while let Some(inbound) = hub.recv(sub) {
            assert!(matches!(inbound, Inbound::CreditAvailable));
            n += 1;
        }
        n
    }

    #[test]
    fn issue_fail_without_recv_is_bounded() {
        let mut hub = Hub::new();
        let owner = hub.subscribe(&[MessageType::Inv], credits(2));
        let other = hub.subscribe(&[MessageType::Inv], credits(1));
        assert!(hub.acquire_or_want(owner));
        assert!(hub.acquire_or_want(owner));
        assert!(!hub.acquire_or_want(owner));
        assert!(hub.acquire_or_want(other));
        hub.offer(1, MessageType::Inv, msg(1), 1, None).unwrap();
        assert!(hub.failed(owner, "first"));
        assert!(hub.failed(owner, "second"));
        assert!(!hub.failed(owner, "third"));
        hub.release(owner);
        assert!(!hub.acquire_or_want(owner));
        assert!(!hub.failed(owner, "fourth"));
        assert_eq!(take(&mut hub, owner), (1, 1));
        for expected in ["first", "second"] {
            assert!(
                matches!(hub.recv(owner), Some(Inbound::RequestFailed(last)) if last == expected)
            );
        }
        assert!(matches!(hub.recv(owner), Some(Inbound::CreditAvailable)));
        assert!(hub.recv(owner).is_none());
        assert_eq!(take(&mut hub, other), (1, 1));
        let gone = hub.subscribe(&[MessageType::Inv], credits(1));
        hub.remove(gone);
        assert!(!hub.acquire_or_want(gone));
        assert!(!hub.failed(gone, "timeout"));
    }

    #[test]
    fn credits_return_on_answer_cancel_recv() {
        let mut hub = Hub::new();
        let sub = hub.subscribe(&[MessageType::Inv], credits(3));
        for _ in 0..3 {
            assert!(hub.acquire_or_want(sub));
        }
        assert!(!hub.acquire_or_want(sub));
        // An answered request and a cancelled one each return their credit.
        hub.release(sub);
        hub.release(sub);
        assert!(hub.failed(sub, "timeout"));
        assert!(matches!(hub.recv(sub), Some(Inbound::CreditAvailable)));
        assert!(hub.acquire_or_want(sub));
        assert!(hub.acquire_or_want(sub));
        assert!(!hub.acquire_or_want(sub));
        assert!(matches!(hub.recv(sub), Some(Inbound::RequestFailed("timeout"))));
        assert!(matches!(hub.recv(sub), Some(Inbound::CreditAvailable)));
        assert!(hub.acquire_or_want(sub));
        assert!(!hub.acquire_or_want(sub));
        hub.remove(sub);
        assert!(!hub.acquire_or_want(sub));
    }

    #[test]
    fn credit_available_after_no_credit() {
        let mut hub = Hub::new();
        let sub = hub.subscribe(&[MessageType::Inv], credits(1));
        assert!(hub.acquire_or_want(sub));
        hub.release(sub);
        assert!(hub.recv(sub).is_none());
        assert!(hub.acquire_or_want(sub));
        assert!(!hub.acquire_or_want(sub));
        hub.release(sub);
        assert_eq!(markers(&mut hub, sub), 1);
        assert!(hub.acquire_or_want(sub));
    }

    #[test]
    fn acquire_or_want_then_release_queues_marker() {
        let mut hub = Hub::new();
        let sub = hub.subscribe(&[MessageType::Inv], credits(1));
        assert!(hub.acquire_or_want(sub));
        assert!(!hub.acquire_or_want(sub));
        assert!(hub.recv(sub).is_none());
        hub.release(sub);
        assert!(matches!(hub.recv(sub), Some(Inbound::CreditAvailable)));
        assert!(hub.recv(sub).is_none());
    }

    #[test]
    fn release_in_no_credit_window_queues_one_marker() {
        let mut hub = Hub::new();
        let sub = hub.subscribe(&[MessageType::Inv], credits(2));
        assert!(hub.acquire_or_want(sub));
        assert!(hub.acquire_or_want(sub));
        // Both credits free after the caller saw none, before it acts on that answer.
        assert!(!hub.acquire_or_want(sub));
        hub.release(sub);
        hub.release(sub);
        assert_eq!(markers(&mut hub, sub), 1);
        assert!(hub.acquire_or_want(sub));
        assert!(hub.acquire_or_want(sub));
        assert!(!hub.acquire_or_want(sub));
    }

    #[test]
    fn repeated_defer_release_keeps_one_marker() {
        let mut hub = Hub::new();
        let sub = hub.subscribe(&[MessageType::Inv], credits(1));
        assert!(hub.acquire_or_want(sub));
        for _ in 0..5 {
            assert!(!hub.acquire_or_want(sub));
            hub.release(sub);
            assert!(hub.acquire_or_want(sub));
        }
        assert!(!hub.acquire_or_want(sub));
        hub.release(sub);
        assert_eq!(markers(&mut hub, sub), 1);
    }

    #[test]
    fn recv_clears_marker_queued() {
        let mut hub = Hub::new();
        let sub = hub.subscribe(&[MessageType::Inv], credits(1));
        assert!(hub.acquire_or_want(sub));
        assert!(!hub.acquire_or_want(sub));
        hub.release(sub);
        assert_eq!(markers(&mut hub, sub), 1);
        assert!(hub.acquire_or_want(sub));
        assert!(!hub.acquire_or_want(sub));
        hub.release(sub);
        assert_eq!(markers(&mut hub, sub), 1);
    }

    #[test]
    fn markers_and_failures_skip_the_message_bound() {
        let mut hub = Hub::new();
        let sub = hub.subscribe(&[MessageType::Inv], credits(2));
        hub.offer(1, MessageType::Inv, msg(1), 1, None).unwrap();
        assert!(hub.acquire_or_want(sub));
        assert!(hub.acquire_or_want(sub));
        assert!(!hub.acquire_or_want(sub));
        hub.release(sub);
        assert!(hub.failed(sub, "timeout"));
        assert_eq!(take(&mut hub, sub), (1, 1));
        assert!(matches!(hub.recv(sub), Some(Inbound::CreditAvailable)));
        assert!(matches!(hub.recv(sub), Some(Inbound::RequestFailed("timeout"))));
        assert!(hub.recv(sub).is_none());
        assert!(hub.acquire_or_want(sub));
        assert!(hub.acquire_or_want(sub));
    }

    #[test]
    fn parked_deliveries_are_keyed_by_connection() {
        let mut hub = SubscriptionHub::<u64, u64, &'static str, Peer>::new();
        let sub = hub.subscribe(&[MessageType::Inv], cap(1, 100));
        let old = Peer {
            addr: "a",
            conn: 1,
        };
        let new = Peer {
            addr: "a",
            conn: 2,
        };
        assert_eq!(old.addr, new.addr);
        hub.offer(old.clone(), MessageType::Inv, msg(1), 1, None).unwrap();
        assert_eq!(
            hub.offer(old.clone(), MessageType::Inv, msg(2), 1, None).unwrap(),
            Offer::Parked
        );
        assert!(hub.is_parked(ConnId(1)));
        assert!(!hub.is_parked(ConnId(2)));
        assert!(hub.offer(old.clone(), MessageType::Inv, msg(3), 1, None).is_err());
        assert_eq!(
            hub.offer(new.clone(), MessageType::Inv, msg(4), 1, None).unwrap(),
            Offer::Parked
        );
        hub.disconnect(ConnId(1));
        assert!(hub.is_parked(ConnId(2)));
        let peers: Vec<_> = (0..3)
            .map(|_| {
                let peer = match hub.recv(sub).unwrap() {
                    Inbound::Message {
                        peer,
                        ..
                    } => peer,
                    _ => panic!("wrong delivery"),
                };
                hub.on_space(sub);
                peer
            })
            .collect();
        assert_eq!(peers, vec![old.clone(), old, new]);
        assert!(!hub.is_parked(ConnId(2)));
    }

    #[test]
    fn already_parked_offer_returns_the_input() {
        let mut hub = Hub::new();
        let sub = hub.subscribe(&[MessageType::Inv], cap(1, 1));
        hub.offer(1, MessageType::Inv, msg(1), 1, None).unwrap();
        hub.offer(1, MessageType::Inv, msg(2), 1, None).unwrap();
        let third = msg(3);
        let rejected = hub.offer(1, MessageType::Inv, third.clone(), 1, Some((4, 5))).unwrap_err();
        assert!(Arc::ptr_eq(&rejected.msg, &third));
        assert_eq!(rejected.request, Some((4, 5)));
        assert_eq!(take(&mut hub, sub), (1, 1));
        assert_eq!(hub.on_space(sub), vec![1]);
        assert_eq!(take(&mut hub, sub), (1, 2));
        assert!(hub.recv(sub).is_none());
    }

    #[tokio::test]
    async fn async_recv_releases_space_and_drop_resumes_peers() {
        let hub = AsyncHub::<u64, u64, &'static str, u64>::new();
        let mut sub = hub.subscribe(&[MessageType::Inv], cap(1, 1));
        let waiting = spawn(async move {
            let inbound = sub.recv().await;
            (sub, inbound)
        });
        yield_now().await;
        hub.offer(1, MessageType::Inv, msg(0), 1, None).unwrap();
        let (mut sub, inbound) = waiting.await.unwrap();
        assert!(matches!(inbound, Some(Inbound::Message { .. })));
        let id = sub.id();
        assert!(hub.acquire_or_want(id));
        hub.offer(1, MessageType::Inv, msg(1), 1, None).unwrap();
        hub.offer(1, MessageType::Inv, msg(2), 1, None).unwrap();
        assert!(hub.failed(id, "timeout"));
        assert!(matches!(sub.recv().await, Some(Inbound::Message { .. })));
        timeout(Duration::from_secs(60), hub.space().notified()).await.unwrap();
        assert_eq!(hub.resumed(), vec![1]);
        assert!(matches!(sub.recv().await, Some(Inbound::RequestFailed("timeout"))));
        assert!(matches!(sub.recv().await, Some(Inbound::Message { .. })));
        hub.offer(1, MessageType::Inv, msg(3), 1, None).unwrap();
        hub.offer(1, MessageType::Inv, msg(4), 1, None).unwrap();
        drop(sub);
        timeout(Duration::from_secs(60), hub.space().notified()).await.unwrap();
        assert_eq!(hub.resumed(), vec![1]);
        let mut sub = hub.subscribe(&[MessageType::Inv], cap(1, 1));
        drop(hub);
        assert!(sub.recv().await.is_none());
    }

    #[test]
    fn stalled_subscriber_parks_the_peer() {
        let start = Instant::now();
        let mut hub = Hub::with_clock(Duration::from_secs(10), Arc::new(move || start));
        let sub = hub.subscribe(&[MessageType::Inv], cap(1, 10));
        hub.offer(1, MessageType::Inv, msg(1), 1, None).unwrap();
        hub.offer(1, MessageType::Inv, msg(2), 1, None).unwrap();
        assert!(hub.is_parked(ConnId(1)));
        assert!(hub.stalled(start + Duration::from_secs(9)).is_empty());
        let events = hub.stalled(start + Duration::from_secs(10));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kinds, vec![MessageType::Inv]);
        assert!(hub.stalled(start + Duration::from_secs(100)).is_empty());
        assert_eq!(take(&mut hub, sub), (1, 1));
        assert_eq!(hub.on_space(sub), vec![1]);
        assert_eq!(take(&mut hub, sub), (1, 2));
        assert!(hub.stalled(start + Duration::from_secs(100)).is_empty());
        hub.offer(1, MessageType::Inv, msg(3), 1, None).unwrap();
        assert!(hub.stalled(start + Duration::from_secs(100)).is_empty());
        hub.offer(1, MessageType::Inv, msg(4), 1, None).unwrap();
        assert_eq!(hub.stalled(start + Duration::from_secs(10)).len(), 1);
        assert!(hub.stalled(start + Duration::from_secs(100)).is_empty());
    }

    #[test]
    fn byte_stall_survives_insufficient_space_and_failure_delivery() {
        let start = Instant::now();
        let mut hub = Hub::with_clock(Duration::from_secs(10), Arc::new(move || start));
        let sub = hub.subscribe(&[MessageType::Block], cap(8, 10));
        hub.offer(1, MessageType::Block, msg(1), 2, None).unwrap();
        hub.offer(1, MessageType::Block, msg(2), 6, None).unwrap();
        hub.offer(1, MessageType::Block, msg(3), 9, None).unwrap();
        assert_eq!(hub.stalled(start + Duration::from_secs(10)).len(), 1);
        assert!(hub.acquire_or_want(sub));
        assert!(hub.failed(sub, "timeout"));
        assert_eq!(take(&mut hub, sub), (1, 1));
        assert!(hub.on_space(sub).is_empty());
        assert!(hub.stalled(start + Duration::from_secs(20)).is_empty());
        assert_eq!(take(&mut hub, sub), (1, 2));
        assert_eq!(hub.on_space(sub), vec![1]);
        assert!(matches!(hub.recv(sub), Some(Inbound::RequestFailed { .. })));
        assert_eq!(take(&mut hub, sub), (1, 3));
        assert!(hub.stalled(start + Duration::from_secs(20)).is_empty());
        assert_eq!(hub.offer(1, MessageType::Block, msg(4), 10, None).unwrap(), Offer::Delivered);
        assert!(hub.stalled(start + Duration::from_secs(20)).is_empty());
        assert_eq!(take(&mut hub, sub), (1, 4));
        hub.offer(1, MessageType::Block, msg(5), 6, None).unwrap();
        hub.offer(1, MessageType::Block, msg(6), 8, None).unwrap();
        hub.disconnect(ConnId(1));
        assert_eq!(hub.stalled(start + Duration::from_secs(20)).len(), 1);
        hub.remove(sub);
        assert!(hub.stalled(start + Duration::from_secs(20)).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn async_shell_reports_stalls() {
        let hub = AsyncHub::<u64, u64, &'static str, u64>::new();
        let mut sub = hub.subscribe(&[MessageType::Inv], cap(1, 1));
        hub.offer(1, MessageType::Inv, msg(1), 1, None).unwrap();
        hub.offer(1, MessageType::Inv, msg(2), 1, None).unwrap();
        advance(Duration::from_secs(10)).await;
        assert_eq!(hub.stalled(Instant::now()).len(), 1);
        assert!(hub.stalled(Instant::now()).is_empty());
        sub.recv().await.unwrap();
        advance(Duration::from_secs(10)).await;
        assert!(hub.stalled(Instant::now()).is_empty());
    }

    #[test]
    fn blocked_head_starts_stall() {
        let start = Instant::now();
        let mut hub = Hub::with_clock(Duration::from_secs(10), Arc::new(move || start));
        let sub = hub.subscribe(&[MessageType::Inv], cap(1, 10));
        hub.offer(1, MessageType::Inv, msg(1), 10, None).unwrap();
        assert!(hub.stalled(start + Duration::from_secs(100)).is_empty());
        hub.offer(1, MessageType::Inv, msg(2), 1, None).unwrap();
        assert_eq!(hub.stalled(start + Duration::from_secs(10)).len(), 1);
        assert_eq!(take(&mut hub, sub), (1, 1));
        assert!(hub.stalled(start + Duration::from_secs(100)).is_empty());
        assert_eq!(hub.on_space(sub), vec![1]);
        hub.offer(2, MessageType::Inv, msg(3), 1, None).unwrap();
        assert_eq!(hub.stalled(start + Duration::from_secs(10)).len(), 1);
    }

    #[test]
    fn blocked_head_is_not_overtaken() {
        let mut hub = Hub::new();
        let sub = hub.subscribe(&[MessageType::Inv], cap(4, 10));
        hub.offer(1, MessageType::Inv, msg(1), 9, None).unwrap();
        assert_eq!(hub.offer(1, MessageType::Inv, msg(2), 10, None).unwrap(), Offer::Parked);
        assert_eq!(hub.offer(2, MessageType::Inv, msg(3), 1, None).unwrap(), Offer::Parked);
        assert_eq!(take(&mut hub, sub), (1, 1));
        assert_eq!(hub.on_space(sub), vec![1]);
        assert_eq!(take(&mut hub, sub), (1, 2));
        assert_eq!(hub.on_space(sub), vec![2]);
        assert_eq!(take(&mut hub, sub), (2, 3));
        assert!(hub.recv(sub).is_none());
    }

    #[test]
    fn byte_only_saturation() {
        let mut hub = Hub::new();
        let sub = hub.subscribe(&[MessageType::Block], cap(100, 10));
        for n in 0..5 {
            assert_eq!(
                hub.offer(1, MessageType::Block, msg(n), 2, None).unwrap(),
                Offer::Delivered
            );
        }
        assert_eq!(hub.offer(1, MessageType::Block, msg(5), 1, None).unwrap(), Offer::Parked);
        assert_eq!(take(&mut hub, sub), (1, 0));
        assert_eq!(hub.on_space(sub), vec![1]);
        assert_eq!(hub.used, 9);
    }

    #[test]
    fn oversize_admitted_only_when_empty() {
        let mut hub = Hub::new();
        let sub = hub.subscribe(&[MessageType::Block], cap(4, 10));
        assert_eq!(hub.offer(1, MessageType::Block, msg(1), 11, None).unwrap(), Offer::Delivered);
        assert_eq!(hub.offer(2, MessageType::Block, msg(2), 1, None).unwrap(), Offer::Parked);
        assert_eq!(take(&mut hub, sub), (1, 1));
        assert_eq!(hub.on_space(sub), vec![2]);
        assert_eq!(hub.offer(3, MessageType::Block, msg(3), 11, None).unwrap(), Offer::Parked);
        assert_eq!(take(&mut hub, sub), (2, 2));
        assert_eq!(hub.on_space(sub), vec![3]);
        assert_eq!(take(&mut hub, sub), (3, 3));
    }

    #[test]
    fn max_size_parked_heads_within_global_budget() {
        const MAX: usize = 6;
        let mut hub = Hub::new().with_hub_bytes(20);
        let slow = hub.subscribe(&[MessageType::Block], cap(1, MAX));
        let mut conn = 0;
        while hub.may_poll() {
            hub.offer(conn, MessageType::Block, msg(conn), MAX, None).unwrap();
            conn += 1;
        }
        assert!(conn > 1);
        assert!(hub.used <= 20 + MAX);
        assert_eq!(hub.used, conn as usize * MAX);
        assert!((1..conn).all(|parked| hub.is_parked(ConnId(parked))));
        assert_eq!(take(&mut hub, slow), (0, 0));
        assert_eq!(hub.on_space(slow), vec![1]);
        assert!(hub.used <= 20 + MAX);
    }

    #[test]
    fn global_budget_charges_each_message_once() {
        let mut hub = Hub::new().with_hub_bytes(10);
        let fast = hub.subscribe(&[MessageType::Inv], cap(4, 100));
        let slow = hub.subscribe(&[MessageType::Inv], cap(1, 100));
        hub.offer(1, MessageType::Inv, msg(1), 4, None).unwrap();
        assert_eq!(hub.used, 4);
        assert_eq!(hub.offer(1, MessageType::Inv, msg(2), 4, None).unwrap(), Offer::Parked);
        assert_eq!(hub.used, 8);
        assert!(hub.may_poll());
        assert_eq!(hub.offer(2, MessageType::Inv, msg(3), 4, None).unwrap(), Offer::Parked);
        assert_eq!(hub.used, 12);
        assert!(!hub.may_poll());
        for n in 1..=3 {
            assert_eq!(take(&mut hub, fast).1, n);
        }
        assert_eq!(hub.used, 12);
        assert!(!hub.take_space_wake());
        assert_eq!(take(&mut hub, slow), (1, 1));
        assert_eq!(hub.used, 8);
        assert!(hub.may_poll());
        assert!(hub.take_space_wake());
        assert!(!hub.take_space_wake());
        assert_eq!(hub.on_space(slow), vec![1]);
        assert_eq!(hub.used, 8);
        assert_eq!(take(&mut hub, slow), (1, 2));
        assert_eq!(hub.on_space(slow), vec![2]);
        assert_eq!(take(&mut hub, slow), (2, 3));
        assert_eq!(hub.used, 0);
        assert!(!hub.take_space_wake());
        assert_eq!(hub.offer(3, MessageType::Tx, msg(4), 100, None).unwrap(), Offer::Delivered);
        assert_eq!(hub.used, 0);
    }

    #[test]
    fn removed_subscriber_and_closed_connection_release_charges() {
        let mut hub = Hub::new().with_hub_bytes(8);
        let kept = hub.subscribe(&[MessageType::Inv], cap(1, 100));
        let dropped = hub.subscribe(&[MessageType::Inv], cap(4, 100));
        hub.offer(1, MessageType::Inv, msg(1), 4, None).unwrap();
        hub.offer(1, MessageType::Inv, msg(2), 4, None).unwrap();
        hub.disconnect(ConnId(1));
        assert_eq!(hub.used, 8);
        assert!(hub.remove(dropped).is_empty());
        assert_eq!(hub.used, 8);
        assert_eq!(take(&mut hub, kept), (1, 1));
        assert_eq!(hub.used, 4);
        assert!(hub.take_space_wake());
        assert!(hub.on_space(kept).is_empty());
        assert_eq!(hub.used, 4);
        assert!(hub.remove(kept).is_empty());
        assert_eq!(hub.used, 0);
    }

    #[tokio::test]
    async fn async_shell_wakes_the_driver_when_budget_frees() {
        let hub = AsyncHub::<u64, u64, &'static str, u64>::new();
        let mut sub = hub.subscribe(&[MessageType::Inv], cap(4, 100));
        hub.offer(1, MessageType::Inv, msg(1), HUB_BYTES, None).unwrap();
        assert!(!hub.may_poll());
        sub.recv().await.unwrap();
        assert!(hub.may_poll());
        timeout(Duration::from_secs(60), hub.space().notified()).await.unwrap();
    }

    #[tokio::test]
    async fn dropping_a_subscription_removes_it_and_wakes_the_driver() {
        let hub = AsyncHub::<u64, u64, &'static str, u64>::new();
        let kept = hub.subscribe(&[MessageType::Inv], cap(4, 100));
        let dropped = hub.subscribe(&[MessageType::Inv], cap(1, 100));
        hub.offer(1, MessageType::Inv, msg(1), 1, None).unwrap();
        assert_eq!(hub.offer(1, MessageType::Inv, msg(2), 1, None).unwrap(), Offer::Parked);
        let id = dropped.id();
        drop(dropped);
        assert_eq!(hub.resumed(), vec![1]);
        assert!(!hub.acquire_or_want(id));
        timeout(Duration::from_secs(60), hub.space().notified()).await.unwrap();
        drop(kept);
        timeout(Duration::from_secs(60), hub.space().notified()).await.unwrap();
        assert_eq!(hub.offer(2, MessageType::Inv, msg(3), 1, None).unwrap(), Offer::Delivered);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_lost_wake_between_concurrent_offer_and_recv() {
        const N: u64 = 500;
        let hub = Arc::new(AsyncHub::<u64, u64, &'static str, u64>::new());
        let mut sub = hub.subscribe(&[MessageType::Inv], cap(1, 100));
        let consumer = spawn(async move {
            for expected in 0..N {
                match sub.recv().await {
                    Some(Inbound::Message {
                        msg,
                        ..
                    }) => assert!(matches!(**msg, NetworkMessage::Ping(n) if n == expected)),
                    _ => panic!("wrong delivery"),
                }
            }
        });
        let driver = Arc::clone(&hub);
        let producer = spawn(async move {
            for n in 0..N {
                if driver.offer(1, MessageType::Inv, msg(n), 1, None).unwrap() == Offer::Delivered {
                    continue;
                }
                loop {
                    let space = driver.space().notified();
                    pin!(space);
                    space.as_mut().enable();
                    if driver.resumed() == vec![1] {
                        break;
                    }
                    space.await;
                }
            }
        });
        timeout(Duration::from_secs(120), async {
            producer.await.unwrap();
            consumer.await.unwrap();
        })
        .await
        .unwrap();
    }

    #[test]
    fn default_budgets() {
        for (kind, msgs, mib, requests) in [
            (MessageType::Headers, 4, 2, 4),
            (MessageType::CfHeaders, 8, 1, 16),
            (MessageType::CFilter, 2_000, 4, 64),
            (MessageType::Block, 8, 8, 32),
            (MessageType::MnListDiff, 4, 6, 8),
            (MessageType::QrInfo, 4, 6, 8),
            (MessageType::Inv, 256, 1, 16),
            (MessageType::Tx, 256, 1, 16),
            (MessageType::ChainLock, 256, 1, 16),
            (MessageType::IsDLock, 256, 1, 16),
        ] {
            let expected = SubCap {
                requests: NonZeroUsize::new(requests).unwrap(),
                ..cap(msgs, mib * 1024 * 1024)
            };
            assert_eq!(SubCap::for_kind(kind), expected);
        }
        assert_eq!(SubCap::from(NonZeroUsize::new(3).unwrap()), cap(3, 1024 * 1024));
    }
}
