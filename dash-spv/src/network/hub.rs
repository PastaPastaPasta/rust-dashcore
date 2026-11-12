//! Bounded subscription delivery, independent of the network and request ledger.

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::hash::Hash;
use std::mem;
use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::sync::{Arc, Weak};
use std::time::Duration;

use tokio::sync::{mpsc, Mutex, Notify};
use tokio::time::Instant;

use super::{InboundMessage, MessageType, RequestKey};

/// Message and payload-byte limits for one subscriber.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubCap {
    pub msgs: NonZeroUsize,
    pub bytes: NonZeroUsize,
}

impl SubCap {
    /// The default budget for a subscriber of this kind.
    pub fn for_kind(kind: MessageType) -> Self {
        let (msgs, mib) = match kind {
            MessageType::Headers => (4, 2),
            MessageType::CfHeaders => (8, 1),
            MessageType::CFilter => (2_000, 4),
            MessageType::Block => (8, 8),
            MessageType::MnListDiff | MessageType::QrInfo => (4, 6),
            _ => (256, 1),
        };
        Self {
            msgs: NonZeroUsize::new(msgs).unwrap_or(NonZeroUsize::MIN),
            bytes: NonZeroUsize::new(mib * 1024 * 1024).unwrap_or(NonZeroUsize::MIN),
        }
    }
}

impl From<NonZeroUsize> for SubCap {
    fn from(msgs: NonZeroUsize) -> Self {
        Self {
            msgs,
            bytes: NonZeroUsize::new(1024 * 1024).unwrap_or(NonZeroUsize::MIN),
        }
    }
}

/// A subscription's identity, stable until it is removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SubId(u64);

/// Delivery metadata uses the request ledger's ID and failure types.
#[derive(Debug)]
pub enum Inbound<R, A, F, P = SocketAddr> {
    Message {
        peer: P,
        msg: Arc<InboundMessage>,
        request: Option<(R, A)>,
    },
    RequestFailed {
        id: R,
        key: RequestKey,
        attempts: u8,
        last: F,
    },
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

struct Queued<R, A, F, P> {
    inbound: Inbound<R, A, F, P>,
    bytes: usize,
}

/// One notification for a subscription's current stall episode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubscriberStalled {
    pub kinds: Vec<MessageType>,
}

struct Stall {
    since: Instant,
    reported: bool,
    blocked_bytes: usize,
}

struct Subscriber<R, A, F, P> {
    kinds: Vec<MessageType>,
    cap: SubCap,
    queue: VecDeque<Queued<R, A, F, P>>,
    msgs: usize,
    bytes: usize,
    // A queued failure keeps its reservation until it is received.
    requests: HashMap<R, bool>,
    stall: Option<Stall>,
}

impl<R, A, F, P> Subscriber<R, A, F, P> {
    fn accepts(&self, bytes: usize) -> bool {
        self.msgs == 0
            || (self.msgs < self.cap.msgs.get()
                && bytes <= self.cap.bytes.get().saturating_sub(self.bytes)
                && self.bytes <= self.cap.bytes.get())
    }
}

struct Delivery<R, A, P> {
    peer: P,
    msg: Arc<InboundMessage>,
    bytes: usize,
    request: Option<(R, A)>,
    targets: VecDeque<SubId>,
}

/// A sans-io fanout queue. A caller must stop polling each parked connection.
/// `P` can identify a connection independently of its socket address.
pub struct SubscriptionHub<R, A, F, P = SocketAddr> {
    subscribers: BTreeMap<SubId, Subscriber<R, A, F, P>>,
    parked: BTreeMap<P, Delivery<R, A, P>>,
    parked_order: VecDeque<P>,
    next_sub: u64,
    stall_report: Duration,
    clock: Arc<dyn Fn() -> Instant + Send + Sync>,
}

impl<R: Copy + Eq + Hash, A: Copy, F, P: Copy + Ord> Default for SubscriptionHub<R, A, F, P> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R: Copy + Eq + Hash, A: Copy, F, P: Copy + Ord> SubscriptionHub<R, A, F, P> {
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
            parked: BTreeMap::new(),
            parked_order: VecDeque::new(),
            next_sub: 0,
        }
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
                requests: HashMap::new(),
                stall: None,
            },
        );
        sub
    }

    pub fn offer(
        &mut self,
        peer: P,
        kind: MessageType,
        msg: Arc<InboundMessage>,
        bytes: usize,
        request: Option<(R, A)>,
    ) -> Result<Offer, AlreadyParked<R, A, P>> {
        if self.is_parked(peer) {
            return Err(AlreadyParked {
                peer,
                kind,
                msg,
                bytes,
                request,
            });
        }
        let targets = self
            .subscribers
            .iter()
            .filter_map(|(id, sub)| sub.kinds.contains(&kind).then_some(*id))
            .collect();
        let delivery = Delivery {
            peer,
            msg,
            bytes,
            request,
            targets,
        };
        Ok(self.deliver(delivery))
    }

    fn deliver(&mut self, mut delivery: Delivery<R, A, P>) -> Offer {
        while let Some(id) = delivery.targets.pop_front() {
            let Some(sub) = self.subscribers.get_mut(&id) else {
                continue;
            };
            if !sub.accepts(delivery.bytes) {
                let stall = sub.stall.get_or_insert_with(|| Stall {
                    since: (self.clock)(),
                    reported: false,
                    blocked_bytes: delivery.bytes,
                });
                stall.blocked_bytes = stall.blocked_bytes.max(delivery.bytes);
                delivery.targets.push_front(id);
                self.parked_order.push_back(delivery.peer);
                self.parked.insert(delivery.peer, delivery);
                return Offer::Parked;
            }
            sub.msgs += 1;
            sub.bytes += delivery.bytes;
            if sub.msgs >= sub.cap.msgs.get() || sub.bytes >= sub.cap.bytes.get() {
                sub.stall.get_or_insert_with(|| Stall {
                    since: (self.clock)(),
                    reported: false,
                    blocked_bytes: 0,
                });
            }
            if delivery.targets.is_empty() {
                sub.queue.push_back(Queued {
                    inbound: Inbound::Message {
                        peer: delivery.peer,
                        msg: delivery.msg,
                        request: delivery.request,
                    },
                    bytes: delivery.bytes,
                });
                return Offer::Delivered;
            }
            sub.queue.push_back(Queued {
                inbound: Inbound::Message {
                    peer: delivery.peer,
                    msg: Arc::clone(&delivery.msg),
                    request: delivery.request,
                },
                bytes: delivery.bytes,
            });
        }
        Offer::Delivered
    }

    /// Releases payload accounting. Call `on_space` before polling parked peers again.
    pub fn recv(&mut self, id: SubId) -> Option<Inbound<R, A, F, P>> {
        let sub = self.subscribers.get_mut(&id)?;
        let queued = sub.queue.pop_front()?;
        match &queued.inbound {
            Inbound::Message {
                ..
            } => {
                sub.msgs -= 1;
                sub.bytes -= queued.bytes;
                if sub.stall.as_ref().is_some_and(|stall| sub.accepts(stall.blocked_bytes)) {
                    sub.stall = None;
                }
            }
            Inbound::RequestFailed {
                id,
                ..
            } => {
                sub.requests.remove(id);
            }
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

    pub fn is_parked(&self, peer: P) -> bool {
        self.parked.contains_key(&peer)
    }

    /// Retries pending fanout and returns the connections whose delivery is complete.
    pub fn on_space(&mut self, _sub: SubId) -> Vec<P> {
        let pending = mem::take(&mut self.parked_order);
        let resumed = pending
            .into_iter()
            .filter_map(|peer| {
                let delivery = self.parked.remove(&peer)?;
                (self.deliver(delivery) == Offer::Delivered).then_some(peer)
            })
            .collect();
        self.refresh_stalls();
        resumed
    }

    fn refresh_stalls(&mut self) {
        for (id, sub) in &mut self.subscribers {
            let blocked_bytes = self
                .parked
                .values()
                .filter_map(|delivery| {
                    (delivery.targets.front() == Some(id)).then_some(delivery.bytes)
                })
                .max()
                .unwrap_or(0);
            if sub.accepts(blocked_bytes)
                && sub.msgs < sub.cap.msgs.get()
                && sub.bytes < sub.cap.bytes.get()
            {
                sub.stall = None;
            } else if let Some(stall) = &mut sub.stall {
                stall.blocked_bytes = blocked_bytes;
            }
        }
    }

    pub fn remove(&mut self, sub: SubId) -> Vec<P> {
        self.subscribers.remove(&sub);
        self.on_space(sub)
    }

    /// Discards only the outstanding fanout for this closed connection.
    pub fn disconnect(&mut self, peer: P) {
        self.parked.remove(&peer);
        self.parked_order.retain(|id| *id != peer);
        self.refresh_stalls();
    }

    /// Reserves one terminal notification slot for an open request.
    pub fn reserve_request(&mut self, sub: SubId, id: R) -> bool {
        let Some(sub) = self.subscribers.get_mut(&sub) else {
            return false;
        };
        match sub.requests.entry(id) {
            Entry::Vacant(entry) => {
                entry.insert(false);
                true
            }
            Entry::Occupied(_) => false,
        }
    }

    /// Releases a completed or cancelled request's unused terminal slot.
    pub fn release_request(&mut self, sub: SubId, id: R) {
        if let Some(sub) = self.subscribers.get_mut(&sub) {
            if sub.requests.get(&id) == Some(&false) {
                sub.requests.remove(&id);
            }
        }
    }

    /// Enqueues at most one failure per reservation, even when payload delivery is full.
    pub fn request_failed(
        &mut self,
        sub: SubId,
        id: R,
        key: RequestKey,
        attempts: u8,
        last: F,
    ) -> bool {
        let Some(sub) = self.subscribers.get_mut(&sub) else {
            return false;
        };
        let Some(queued) = sub.requests.get_mut(&id) else {
            return false;
        };
        if *queued {
            return false;
        }
        *queued = true;
        sub.queue.push_back(Queued {
            inbound: Inbound::RequestFailed {
                id,
                key,
                attempts,
                last,
            },
            bytes: 0,
        });
        true
    }
}

struct AsyncState<R, A, F, P> {
    core: SubscriptionHub<R, A, F, P>,
    ready: BTreeMap<SubId, mpsc::Sender<()>>,
    resumed: BTreeSet<P>,
}

impl<R: Copy + Eq + Hash, A: Copy, F, P: Copy + Ord> AsyncState<R, A, F, P> {
    fn maintain(&mut self) {
        let dropped: Vec<_> =
            self.ready.iter().filter_map(|(id, tx)| tx.is_closed().then_some(*id)).collect();
        for id in dropped {
            self.ready.remove(&id);
            self.resumed.extend(self.core.remove(id));
        }
        self.wake();
    }

    fn wake(&self) {
        for (id, tx) in &self.ready {
            if self.core.subscribers.get(id).is_some_and(|sub| !sub.queue.is_empty()) {
                let _ = tx.try_send(());
            }
        }
    }
}

/// A thin async shell. Payloads remain in the bounded core; channels carry only wake tokens.
pub struct AsyncHub<R, A, F, P = SocketAddr> {
    state: Arc<Mutex<AsyncState<R, A, F, P>>>,
    space: Arc<Notify>,
}

impl<R: Copy + Eq + Hash, A: Copy, F, P: Copy + Ord> Default for AsyncHub<R, A, F, P> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R: Copy + Eq + Hash, A: Copy, F, P: Copy + Ord> AsyncHub<R, A, F, P> {
    pub fn new() -> Self {
        Self::with_clock(Duration::from_secs(10), Arc::new(Instant::now))
    }

    pub fn with_clock(
        stall_report: Duration,
        clock: Arc<dyn Fn() -> Instant + Send + Sync>,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(AsyncState {
                core: SubscriptionHub::with_clock(stall_report, clock),
                ready: BTreeMap::new(),
                resumed: BTreeSet::new(),
            })),
            space: Arc::new(Notify::new()),
        }
    }

    pub async fn subscribe(&self, kinds: &[MessageType], cap: SubCap) -> Subscription<R, A, F, P> {
        let mut state = self.state.lock().await;
        state.maintain();
        let sub = state.core.subscribe(kinds, cap);
        let (tx, rx) = mpsc::channel(1);
        state.ready.insert(sub, tx);
        Subscription {
            rx,
            state: Arc::downgrade(&self.state),
            space: Arc::clone(&self.space),
            sub,
        }
    }

    pub async fn offer(
        &self,
        peer: P,
        kind: MessageType,
        msg: Arc<InboundMessage>,
        bytes: usize,
        request: Option<(R, A)>,
    ) -> Result<Offer, AlreadyParked<R, A, P>> {
        let mut state = self.state.lock().await;
        state.maintain();
        let result = state.core.offer(peer, kind, msg, bytes, request);
        state.wake();
        result
    }

    pub async fn reserve_request(&self, sub: SubId, id: R) -> bool {
        let mut state = self.state.lock().await;
        state.maintain();
        state.core.reserve_request(sub, id)
    }

    pub async fn release_request(&self, sub: SubId, id: R) {
        self.state.lock().await.core.release_request(sub, id);
    }

    pub async fn request_failed(
        &self,
        sub: SubId,
        id: R,
        key: RequestKey,
        attempts: u8,
        last: F,
    ) -> bool {
        let mut state = self.state.lock().await;
        state.maintain();
        let result = state.core.request_failed(sub, id, key, attempts, last);
        state.wake();
        result
    }

    /// Wakes the driver when a subscriber receives or drops its receiver.
    pub fn space(&self) -> &Notify {
        &self.space
    }

    pub async fn resumed(&self) -> Vec<P> {
        let mut state = self.state.lock().await;
        state.maintain();
        state.wake();
        mem::take(&mut state.resumed).into_iter().collect()
    }

    pub async fn stalled(&self, now: Instant) -> Vec<SubscriberStalled> {
        let mut state = self.state.lock().await;
        state.maintain();
        state.core.stalled(now)
    }

    pub async fn disconnect(&self, peer: P) {
        let mut state = self.state.lock().await;
        state.core.disconnect(peer);
        state.resumed.retain(|id| *id != peer);
    }
}

/// Receives bounded messages and releases their payload bytes before returning them.
pub struct Subscription<R, A, F, P = SocketAddr> {
    rx: mpsc::Receiver<()>,
    state: Weak<Mutex<AsyncState<R, A, F, P>>>,
    space: Arc<Notify>,
    sub: SubId,
}

impl<R: Copy + Eq + Hash, A: Copy, F, P: Copy + Ord> Subscription<R, A, F, P> {
    pub fn id(&self) -> SubId {
        self.sub
    }

    pub async fn recv(&mut self) -> Option<Inbound<R, A, F, P>> {
        loop {
            {
                let state = self.state.upgrade()?;
                let mut state = state.lock().await;
                if let Some(inbound) = state.core.recv(self.sub) {
                    let resumed = state.core.on_space(self.sub);
                    state.resumed.extend(resumed);
                    state.wake();
                    self.space.notify_one();
                    return Some(inbound);
                }
            }
            self.rx.recv().await?;
        }
    }
}

impl<R, A, F, P> Drop for Subscription<R, A, F, P> {
    fn drop(&mut self) {
        self.rx.close();
        self.space.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::time::{advance, Instant};

    use dashcore::network::message::NetworkMessage;
    use tokio::spawn;
    use tokio::task::yield_now;

    use super::{AsyncHub, Inbound, Offer, SubCap, SubId, SubscriptionHub};
    use crate::network::{InboundMessage, MessageType, RequestKey};

    type Hub = SubscriptionHub<u64, u64, &'static str, u64>;

    fn cap(msgs: usize, bytes: usize) -> SubCap {
        SubCap {
            msgs: NonZeroUsize::new(msgs).unwrap(),
            bytes: NonZeroUsize::new(bytes).unwrap(),
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
        assert!(hub.is_parked(1));
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
        assert!(!hub.is_parked(1));
        assert_eq!(take(&mut hub, slow), (1, 2));
        assert!(hub.recv(slow).is_none());
        assert_eq!(hub.offer(1, MessageType::Inv, msg(4), 10, None).unwrap(), Offer::Delivered);
        assert_eq!(take(&mut hub, fast), (1, 4));
        assert_eq!(take(&mut hub, slow), (1, 4));
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
        assert_eq!(
            hub.offer(1, MessageType::Block, msg(3), usize::MAX, None).unwrap(),
            Offer::Delivered
        );
        assert_eq!(hub.offer(1, MessageType::Block, msg(4), 1, None).unwrap(), Offer::Parked);
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
        hub.disconnect(2);
        assert_eq!(take(&mut hub, second), (2, 3));
        assert!(hub.on_space(second).is_empty());
        assert_eq!(take(&mut hub, second), (2, 4));
        assert!(hub.recv(second).is_none());
        hub.remove(second);
        assert_eq!(hub.offer(3, MessageType::Inv, msg(6), 10, None).unwrap(), Offer::Delivered);
    }

    #[test]
    fn terminal_slots_are_bounded_and_owner_only() {
        let mut hub = Hub::new();
        let owner = hub.subscribe(&[MessageType::Inv], cap(1, 1));
        let other = hub.subscribe(&[MessageType::Inv], cap(1, 1));
        assert!(hub.reserve_request(owner, 1));
        assert!(hub.reserve_request(owner, 2));
        assert!(!hub.reserve_request(owner, 1));
        hub.offer(1, MessageType::Inv, msg(1), 1, None).unwrap();
        hub.offer(1, MessageType::Inv, msg(2), 1, None).unwrap();
        assert!(!hub.request_failed(other, 1, RequestKey::CFilters(0), 3, "timeout"));
        assert!(!hub.request_failed(owner, 3, RequestKey::CFilters(0), 3, "timeout"));
        for id in [1, 2] {
            assert!(hub.request_failed(owner, id, RequestKey::CFilters(0), 3, "timeout"));
            assert!(!hub.request_failed(owner, id, RequestKey::CFilters(0), 3, "timeout"));
        }
        hub.release_request(owner, 1);
        assert!(!hub.reserve_request(owner, 1));
        assert_eq!(take(&mut hub, owner), (1, 1));
        for expected in [1, 2] {
            match hub.recv(owner).unwrap() {
                Inbound::RequestFailed {
                    id,
                    key,
                    attempts,
                    last,
                } => {
                    assert_eq!(id, expected);
                    assert_eq!(key, RequestKey::CFilters(0));
                    assert_eq!(attempts, 3);
                    assert_eq!(last, "timeout");
                }
                _ => panic!("expected terminal failure"),
            }
        }
        assert!(hub.recv(owner).is_none());
        assert_eq!(take(&mut hub, other), (1, 1));
        assert!(hub.recv(other).is_none());
        assert_eq!(hub.on_space(other), vec![1]);
        assert_eq!(take(&mut hub, owner), (1, 2));
        assert_eq!(take(&mut hub, other), (1, 2));
        assert!(hub.reserve_request(owner, 3));
        hub.release_request(owner, 3);
        assert!(!hub.request_failed(owner, 3, RequestKey::CFilters(0), 3, "timeout"));
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
        let mut sub = hub.subscribe(&[MessageType::Inv], cap(1, 1)).await;
        let waiting = spawn(async move {
            let inbound = sub.recv().await;
            (sub, inbound)
        });
        yield_now().await;
        hub.offer(1, MessageType::Inv, msg(0), 1, None).await.unwrap();
        let (mut sub, inbound) = waiting.await.unwrap();
        assert!(matches!(inbound, Some(Inbound::Message { .. })));
        let id = sub.id();
        assert!(hub.reserve_request(id, 1).await);
        hub.offer(1, MessageType::Inv, msg(1), 1, None).await.unwrap();
        hub.offer(1, MessageType::Inv, msg(2), 1, None).await.unwrap();
        assert!(hub.request_failed(id, 1, RequestKey::CFilters(0), 3, "timeout").await);
        assert!(matches!(sub.recv().await, Some(Inbound::Message { .. })));
        hub.space().notified().await;
        assert_eq!(hub.resumed().await, vec![1]);
        assert!(matches!(
            sub.recv().await,
            Some(Inbound::RequestFailed {
                id: 1,
                ..
            })
        ));
        assert!(matches!(sub.recv().await, Some(Inbound::Message { .. })));
        hub.offer(1, MessageType::Inv, msg(3), 1, None).await.unwrap();
        hub.offer(1, MessageType::Inv, msg(4), 1, None).await.unwrap();
        drop(sub);
        hub.space().notified().await;
        assert_eq!(hub.resumed().await, vec![1]);
        let mut sub = hub.subscribe(&[MessageType::Inv], cap(1, 1)).await;
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
        assert!(hub.is_parked(1));
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
        assert!(hub.reserve_request(sub, 1));
        assert!(hub.request_failed(sub, 1, RequestKey::CFilters(0), 3, "timeout"));
        assert_eq!(take(&mut hub, sub), (1, 1));
        assert!(hub.on_space(sub).is_empty());
        assert!(hub.stalled(start + Duration::from_secs(20)).is_empty());
        assert_eq!(take(&mut hub, sub), (1, 2));
        assert_eq!(hub.on_space(sub), vec![1]);
        assert!(matches!(hub.recv(sub), Some(Inbound::RequestFailed { .. })));
        assert_eq!(take(&mut hub, sub), (1, 3));
        assert!(hub.stalled(start + Duration::from_secs(20)).is_empty());
        hub.offer(1, MessageType::Block, msg(4), 10, None).unwrap();
        assert_eq!(hub.stalled(start + Duration::from_secs(10)).len(), 1);
        assert_eq!(take(&mut hub, sub), (1, 4));
        hub.offer(1, MessageType::Block, msg(5), 6, None).unwrap();
        hub.offer(1, MessageType::Block, msg(6), 8, None).unwrap();
        hub.disconnect(1);
        assert!(hub.stalled(start + Duration::from_secs(20)).is_empty());
        hub.remove(sub);
        assert!(hub.stalled(start + Duration::from_secs(20)).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn async_shell_reports_stalls() {
        let hub = AsyncHub::<u64, u64, &'static str, u64>::new();
        let mut sub = hub.subscribe(&[MessageType::Inv], cap(1, 1)).await;
        hub.offer(1, MessageType::Inv, msg(1), 1, None).await.unwrap();
        advance(Duration::from_secs(10)).await;
        assert_eq!(hub.stalled(Instant::now()).await.len(), 1);
        assert!(hub.stalled(Instant::now()).await.is_empty());
        sub.recv().await.unwrap();
        assert!(hub.stalled(Instant::now()).await.is_empty());
    }

    #[test]
    fn default_budgets() {
        for (kind, msgs, mib) in [
            (MessageType::Headers, 4, 2),
            (MessageType::CfHeaders, 8, 1),
            (MessageType::CFilter, 2_000, 4),
            (MessageType::Block, 8, 8),
            (MessageType::MnListDiff, 4, 6),
            (MessageType::QrInfo, 4, 6),
            (MessageType::Inv, 256, 1),
            (MessageType::Tx, 256, 1),
            (MessageType::ChainLock, 256, 1),
            (MessageType::IsDLock, 256, 1),
        ] {
            assert_eq!(SubCap::for_kind(kind), cap(msgs, mib * 1024 * 1024));
        }
        assert_eq!(SubCap::from(NonZeroUsize::new(3).unwrap()), cap(3, 1024 * 1024));
    }
}
