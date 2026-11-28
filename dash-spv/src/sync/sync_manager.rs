use crate::error::SyncResult;
use crate::network::{InboundMessage, MessageType, NetworkEvent, NetworkManager};
use crate::sync::{
    BlockHeadersProgress, BlocksProgress, ChainLockProgress, FilterHeadersProgress,
    FiltersProgress, InstantSendProgress, ManagerIdentifier, MasternodesProgress, MempoolProgress,
    SyncEvent, SyncState,
};
use async_trait::async_trait;
use std::net::SocketAddr;
use std::sync::Arc;

use crate::SyncError;

/// Contains a trait for event-driven sync managers.
///
/// Each manager is responsible for a specific sync task (headers, filters, blocks, etc.)
/// and communicates with other managers via events. Managers progress independently and
/// catch up to each other as events flow between them.
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::watch;
use tokio::time::interval;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, PartialEq)]
pub enum SyncManagerProgress {
    BlockHeaders(BlockHeadersProgress),
    FilterHeaders(FilterHeadersProgress),
    Filters(FiltersProgress),
    Blocks(BlocksProgress),
    Masternodes(MasternodesProgress),
    ChainLock(ChainLockProgress),
    InstantSend(InstantSendProgress),
    Mempool(MempoolProgress),
}

pub type Inbound = (SocketAddr, Arc<InboundMessage>);

pub struct SyncManagerTaskContext {
    pub(super) message_receiver: UnboundedReceiver<Inbound>,
    pub(super) sync_event_sender: broadcast::Sender<SyncEvent>,
    pub(super) network_event_receiver: broadcast::Receiver<NetworkEvent>,
    pub(super) network: Arc<dyn NetworkManager>,
    pub(super) shutdown: CancellationToken,
    pub(super) progress_sender: watch::Sender<SyncManagerProgress>,
}

impl SyncManagerTaskContext {
    pub(super) fn emit_sync_event(&self, event: SyncEvent) {
        let _ = self.sync_event_sender.send(event);
    }
    pub(super) fn emit_sync_events(&self, events: impl IntoIterator<Item = SyncEvent>) {
        for event in events {
            self.emit_sync_event(event);
        }
    }
}

impl std::fmt::Display for NetworkEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetworkEvent::PeersUpdated {
                addresses,
                ..
            } => write!(f, "PeersUpdated({} peers)", addresses.len()),
            NetworkEvent::PeerConnected(addr) => write!(f, "PeerConnected({addr})"),
            NetworkEvent::PeerDisconnected(addr) => write!(f, "PeerDisconnected({addr})"),
        }
    }
}

/// The default [`SyncManager::handle_network_event`] body, so an override can delegate to it.
pub(super) async fn default_handle_network_event<M: SyncManager + ?Sized>(
    manager: &mut M,
    event: &NetworkEvent,
    network: &Arc<dyn NetworkManager>,
) -> SyncResult<Vec<SyncEvent>> {
    // `PeersUpdated` kicks off the initial requests: the network manager connects in
    // `start`, after every manager has subscribed, so it is the first thing we hear.
    if let NetworkEvent::PeersUpdated {
        addresses,
        ..
    } = event
    {
        if addresses.is_empty() {
            tracing::info!("{} - no peers available, stopping sync", manager.identifier());
            manager.stop_sync();
            return Ok(vec![]);
        }
        manager.update_target_height(network.tip());
        if manager.state() == SyncState::WaitingForConnections {
            tracing::info!("{} - peers available, starting sync", manager.identifier());
            return manager.start_sync(network).await;
        }
    }
    Ok(vec![])
}

/// Guard that verifies a manager has not already been started.
pub(super) fn ensure_not_started(
    state: SyncState,
    identifier: ManagerIdentifier,
) -> SyncResult<()> {
    if state != SyncState::WaitingForConnections {
        tracing::warn!("{} sync already started.", identifier);
        return Err(SyncError::SyncInProgress(identifier));
    }
    Ok(())
}

#[async_trait]
pub trait SyncManager: Send + Sync + std::fmt::Debug {
    /// Get the unique identifier for this manager.
    fn identifier(&self) -> ManagerIdentifier;

    /// Get the manager's sync state.
    fn state(&self) -> SyncState;

    /// Update the manager's sync state.
    fn set_state(&mut self, state: SyncState);

    /// Update the target height for this manager.
    fn update_target_height(&mut self, _height: u32) {}

    /// Message types this manager subscribes to for topic-based routing.
    ///
    /// The network manager uses this to route only relevant messages to each
    /// manager's task via topic-based filtering.
    fn wanted_message_types(&self) -> &'static [MessageType];

    /// Start the sync process.
    ///
    /// Called after initialization to trigger the initial sync requests.
    /// For example, BlockHeadersManager sends its first getheaders request here.
    /// The default implementation is for reactive managers that just wait for events.
    async fn start_sync(
        &mut self,
        _network: &Arc<dyn NetworkManager>,
    ) -> SyncResult<Vec<SyncEvent>> {
        ensure_not_started(self.state(), self.identifier())?;
        self.set_state(SyncState::WaitForEvents);
        Ok(vec![SyncEvent::SyncStart {
            identifier: self.identifier(),
        }])
    }

    /// Stop the internal processing.
    /// Called when the network manager loses its peers.
    fn stop_sync(&mut self) {
        self.set_state(SyncState::WaitingForConnections);
        self.on_disconnect();
    }

    /// Drop peer-bound in-flight state on disconnect.
    ///
    /// Each manager keeps as much progress as it can across a disconnect, and
    /// only invalidates state that was tied to the now-dead peer. Anything
    /// derivable from durable storage (block headers, filter headers, the
    /// masternode engine) or from preserved per-batch bookkeeping should
    /// survive so reconnect resumes instead of restarting.
    fn on_disconnect(&mut self);

    /// Handle an incoming network message.
    ///
    /// Returns events to emit to other managers.
    async fn handle_message(
        &mut self,
        peer: SocketAddr,
        msg: InboundMessage,
        network: &Arc<dyn NetworkManager>,
    ) -> SyncResult<Vec<SyncEvent>>;

    /// Handle a sync event from another manager.
    ///
    /// This is how managers learn about progress from other managers.
    /// For example, `FilterHeadersManager` subscribes to `BlockHeadersStored`
    /// events to know when new headers are available.
    async fn handle_sync_event(
        &mut self,
        event: &SyncEvent,
        network: &Arc<dyn NetworkManager>,
    ) -> SyncResult<Vec<SyncEvent>>;

    /// Periodic tick (every 100ms) for time-based work such as expiry and retry
    /// schedules the broker does not own.
    async fn tick(&mut self, _network: &Arc<dyn NetworkManager>) -> SyncResult<Vec<SyncEvent>> {
        Ok(vec![])
    }

    /// Handle a network event (peer connection changes).
    ///
    /// Default implementation handles state transitions for WaitingForConnections.
    /// Managers can override to customize behavior.
    async fn handle_network_event(
        &mut self,
        event: &NetworkEvent,
        network: &Arc<dyn NetworkManager>,
    ) -> SyncResult<Vec<SyncEvent>> {
        default_handle_network_event(self, event, network).await
    }

    /// Retrieves the current progress of the Manager.
    fn progress(&self) -> SyncManagerProgress;

    /// The peers this manager keeps state for, or `None` if it keeps no per-peer state. After a
    /// lag on the network-event bus, the task replays the difference between these and the
    /// connected peers through [`Self::handle_network_event`].
    fn tracked_peers(&self) -> Option<Vec<SocketAddr>> {
        None
    }

    /// Called with the losses of every peer ([`NetworkManager::disconnects`]) the task has
    /// handled: at its start, before it handles each `PeersUpdated` with no peers, and after a
    /// lag's replayed loss. Each handled loss adds one, never past the network's count, so a loss
    /// whose events are still queued is not counted as handled.
    fn losses_handled(&mut self, _count: u64) {}

    fn try_emit_progress(
        &self,
        progress_before: SyncManagerProgress,
        progress_sender: &watch::Sender<SyncManagerProgress>,
    ) {
        let progress_now = self.progress();
        if progress_now != progress_before {
            let _ = progress_sender.send(progress_now);
        }
    }

    /// Run the manager task, processing messages, events, and periodic ticks.
    ///
    /// This consumes the manager and runs until shutdown is signaled.
    async fn run(mut self, mut context: SyncManagerTaskContext) -> SyncResult<ManagerIdentifier>
    where
        Self: Sized,
    {
        let identifier = self.identifier();
        tracing::info!("{} task starting", identifier);

        let mut sync_event_receiver = context.sync_event_sender.subscribe();
        let mut handled = context.network.disconnects();
        self.losses_handled(handled);

        // Tick interval for periodic housekeeping
        let mut tick_interval = interval(Duration::from_millis(100));

        tracing::info!("{} task entering main loop", identifier);

        loop {
            tokio::select! {
                _ = context.shutdown.cancelled() => {
                    tracing::info!("{} task received shutdown signal", identifier);
                    break;
                }
                // Process incoming network messages
                Some((peer, message)) = context.message_receiver.recv() => {
                    tracing::trace!("{} received message: {}", identifier, message.cmd());
                    // The last subscriber holds the sole reference, so single-subscriber
                    // messages are moved without copying; shared ones (`inv`) are cloned.
                    let message =
                        Arc::try_unwrap(message).unwrap_or_else(|shared| (*shared).clone());
                    let progress_before = self.progress();
                    match self.handle_message(peer, message, &context.network).await {
                        Ok(events) => {
                            if !events.is_empty() {
                                for event in &events {
                                    tracing::debug!("{} emitting: {}", identifier, event);
                                }
                                context.emit_sync_events(events);
                            }
                            self.try_emit_progress(progress_before, &context.progress_sender);
                        }
                        Err(e) => {
                            tracing::error!("{} error handling message: {}", identifier, e);
                            let error_event = SyncEvent::ManagerError {
                                manager: identifier,
                                error: e.to_string(),
                            };
                            context.emit_sync_event(error_event);
                        }
                    }
                    // The select is unbiased, but with messages always waiting it lands on
                    // them about four times in five, so a manager that emits an event per
                    // message (blocks: `BlockProcessed`) falls behind its own events until
                    // the bus overflows. Handle what queued meanwhile before the next message.
                    if !drain_sync_events(&mut self, &mut sync_event_receiver, &context).await {
                        break;
                    }
                }
                // Process events from other managers
                result = sync_event_receiver.recv() => {
                    if !on_sync_event(&mut self, result, &context).await {
                        break;
                    }
                }
                // Process network events
                result = context.network_event_receiver.recv() => {
                    match result {
                        Ok(event) => {
                            if matches!(&event, NetworkEvent::PeersUpdated { addresses, .. } if addresses.is_empty()) {
                                handled = (handled + 1).min(context.network.disconnects());
                                self.losses_handled(handled);
                            }
                            on_network_event(&mut self, &event, &context).await;
                        }
                        // A lag while shutting down loses nothing that matters.
                        Err(broadcast::error::RecvError::Lagged(_))
                            if context.shutdown.is_cancelled() =>
                        {
                            break;
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("{} lagged network events, skipped {}, re-reading the peers", identifier, n);
                            context.network_event_receiver = context.network_event_receiver.resubscribe();
                            handled = on_network_lag(&mut self, handled, &context).await;
                        }
                        Err(error) => {
                            tracing::error!("{} network event error: {}", identifier, error);
                            break;
                        }
                    }
                }
                // Periodic tick for timeouts and housekeeping
                _ = tick_interval.tick() => {
                    let progress_before = self.progress();
                    match self.tick(&context.network).await {
                        Ok(events) => {
                            if !events.is_empty() {
                                context.emit_sync_events(events);
                            }
                            self.try_emit_progress(progress_before, &context.progress_sender);
                        }
                        Err(e) => {
                            tracing::error!("{} tick error: {}", identifier, e);
                        }
                    }
                }
            }
        }

        tracing::info!("{} task exiting", identifier);
        Ok(identifier)
    }
}

/// Handles one network event and emits what it returns.
async fn on_network_event<M: SyncManager>(
    manager: &mut M,
    event: &NetworkEvent,
    context: &SyncManagerTaskContext,
) {
    let identifier = manager.identifier();
    tracing::trace!("{} received network event: {}", identifier, event);
    let progress_before = manager.progress();
    match manager.handle_network_event(event, &context.network).await {
        Ok(events) => {
            for e in &events {
                tracing::debug!("{} emitting: {}", identifier, e);
            }
            context.emit_sync_events(events);
            manager.try_emit_progress(progress_before, &context.progress_sender);
        }
        Err(e) => {
            tracing::error!("{} error handling network event: {}", identifier, e);
        }
    }
}

/// Answers a lag on the network-event bus with a re-read, never an error (DEC-1946), and
/// returns the losses of every peer handled after it. The caller has resubscribed the receiver
/// first, so no event the re-read already covers is handled after it: a retained empty
/// `PeersUpdated` of a counted loss can't count a newer loss as handled (DEC-2010).
///
/// A count moved past `handled` replays the observed loss (DEC-1947 C, DEC-2007): `PeersUpdated`
/// with no peers, whose `stop_sync` clears every peer's state, then a `PeerConnected` for each
/// connected peer. Otherwise a manager's tracked peers that are gone and the connected ones it
/// does not track are replayed as `PeerDisconnected` and `PeerConnected`. Either way
/// `PeersUpdated` with the connected peers, when there is any, ends the replay: the resubscribe
/// dropped the bus's latest one, and a manager waiting for connections restarts on it.
///
/// One window remains, DEC-1946's accepted lag: a loss counted before the re-read whose events
/// are sent after the resubscribe, a few instructions inside `PeerSet::announce`. Its empty
/// `PeersUpdated` then counts the next loss, if one is counted by then, as handled.
async fn on_network_lag<M: SyncManager>(
    manager: &mut M,
    handled: u64,
    context: &SyncManagerTaskContext,
) -> u64 {
    let now = context.network.disconnects();
    let peers = context.network.connected_peers().await;
    let best_height = context.network.tip();
    let mut replay = Vec::new();
    if now == handled {
        if let Some(tracked) = manager.tracked_peers() {
            let gone = tracked.iter().filter(|a| !peers.contains(a));
            replay.extend(gone.map(|&a| NetworkEvent::PeerDisconnected(a)));
            let new = peers.iter().filter(|a| !tracked.contains(a));
            replay.extend(new.map(|&a| NetworkEvent::PeerConnected(a)));
        }
    } else {
        tracing::info!(
            "{} missed a loss of every peer (disconnects {} -> {}), {} peers now",
            manager.identifier(),
            handled,
            now,
            peers.len()
        );
        let lost = NetworkEvent::PeersUpdated {
            addresses: Vec::new(),
            best_height,
        };
        on_network_event(manager, &lost, context).await;
        manager.losses_handled(now);
        replay.extend(peers.iter().map(|&a| NetworkEvent::PeerConnected(a)));
    }
    if !peers.is_empty() {
        replay.push(NetworkEvent::PeersUpdated {
            addresses: peers,
            best_height,
        });
    }
    for event in &replay {
        on_network_event(manager, event, context).await;
    }
    now
}

/// Handles one result from a manager's sync-event receiver. Returns `false` when the
/// task should stop: the bus is closed.
async fn on_sync_event<M: SyncManager>(
    manager: &mut M,
    result: Result<SyncEvent, broadcast::error::RecvError>,
    context: &SyncManagerTaskContext,
) -> bool {
    let identifier = manager.identifier();
    match result {
        Ok(event) => {
            tracing::trace!("{} received event: {}", identifier, event);
            let progress_before = manager.progress();
            match manager.handle_sync_event(&event, &context.network).await {
                Ok(events) => {
                    if !events.is_empty() {
                        for e in &events {
                            tracing::trace!("{} emitting: {}", identifier, e);
                        }
                        context.emit_sync_events(events);
                    }
                    manager.try_emit_progress(progress_before, &context.progress_sender);
                }
                Err(e) => {
                    tracing::error!("{} error handling event: {}", identifier, e);
                }
            }
        }
        Err(broadcast::error::RecvError::Lagged(n)) => {
            // Keep running: a dead manager is a stalled sync. Skipped events
            // are not recovered, so the bus is sized not to overflow.
            tracing::warn!("{} lagged sync events, skipped {}", identifier, n);
        }
        Err(error) => {
            tracing::error!("{} sync event error: {}", identifier, error);
            return false;
        }
    }
    true
}

/// Handles the sync events already queued for this manager, without waiting. Only as
/// many as were queued when it starts, so a flood of events can't hold off messages.
/// Returns `false` when the task should stop.
async fn drain_sync_events<M: SyncManager>(
    manager: &mut M,
    receiver: &mut broadcast::Receiver<SyncEvent>,
    context: &SyncManagerTaskContext,
) -> bool {
    use broadcast::error::{RecvError, TryRecvError};
    for _ in 0..receiver.len() {
        // Leave the shutdown to the select.
        if context.shutdown.is_cancelled() {
            return true;
        }
        let result = match receiver.try_recv() {
            Ok(event) => Ok(event),
            Err(TryRecvError::Empty) => return true,
            Err(TryRecvError::Lagged(n)) => Err(RecvError::Lagged(n)),
            Err(TryRecvError::Closed) => Err(RecvError::Closed),
        };
        if !on_sync_event(manager, result, context).await {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::BlockHeadersProgress;
    use crate::test_utils::MockNetworkManager;
    use dashcore::network::message::NetworkMessage;
    use std::sync::atomic::{AtomicU32, Ordering};
    use tokio::sync::mpsc;

    /// Mock manager for testing the task runner.
    struct MockManager {
        identifier: ManagerIdentifier,
        state: SyncState,
        tick_count: Arc<AtomicU32>,
        /// Emit one event per message, as the blocks manager emits `BlockProcessed`.
        echo: bool,
        events_seen: Arc<AtomicU32>,
        /// The network events it handled, replayed ones included.
        network_events: Arc<parking_lot::Mutex<Vec<NetworkEvent>>>,
        stops: Arc<AtomicU32>,
        starts: Arc<AtomicU32>,
        tracked: Option<Vec<SocketAddr>>,
        /// Holds each network event, once recorded, until it gets a permit.
        gate: Option<Arc<tokio::sync::Semaphore>>,
    }

    impl std::fmt::Debug for MockManager {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("MockManager").field("identifier", &self.identifier).finish()
        }
    }

    #[async_trait]
    impl SyncManager for MockManager {
        fn identifier(&self) -> ManagerIdentifier {
            self.identifier
        }

        fn state(&self) -> SyncState {
            self.state
        }

        fn set_state(&mut self, state: SyncState) {
            self.state = state;
        }

        fn wanted_message_types(&self) -> &'static [MessageType] {
            &[]
        }

        fn on_disconnect(&mut self) {
            self.stops.fetch_add(1, Ordering::Relaxed);
        }

        async fn start_sync(
            &mut self,
            _network: &Arc<dyn NetworkManager>,
        ) -> SyncResult<Vec<SyncEvent>> {
            ensure_not_started(self.state, self.identifier)?;
            self.starts.fetch_add(1, Ordering::Relaxed);
            self.state = SyncState::WaitForEvents;
            Ok(vec![])
        }

        async fn handle_message(
            &mut self,
            _peer: SocketAddr,
            _msg: InboundMessage,
            _network: &Arc<dyn NetworkManager>,
        ) -> SyncResult<Vec<SyncEvent>> {
            if !self.echo {
                return Ok(vec![]);
            }
            Ok(vec![SyncEvent::SyncStart {
                identifier: self.identifier,
            }])
        }

        async fn handle_sync_event(
            &mut self,
            _event: &SyncEvent,
            _network: &Arc<dyn NetworkManager>,
        ) -> SyncResult<Vec<SyncEvent>> {
            self.events_seen.fetch_add(1, Ordering::Relaxed);
            Ok(vec![])
        }

        async fn tick(&mut self, _network: &Arc<dyn NetworkManager>) -> SyncResult<Vec<SyncEvent>> {
            self.tick_count.fetch_add(1, Ordering::Relaxed);
            Ok(vec![])
        }

        fn progress(&self) -> SyncManagerProgress {
            let mut progress = BlockHeadersProgress::default();
            progress.set_state(self.state);
            SyncManagerProgress::BlockHeaders(progress)
        }

        async fn handle_network_event(
            &mut self,
            event: &NetworkEvent,
            network: &Arc<dyn NetworkManager>,
        ) -> SyncResult<Vec<SyncEvent>> {
            self.network_events.lock().push(event.clone());
            if let Some(gate) = &self.gate {
                gate.acquire().await.expect("the gate stays open").forget();
            }
            default_handle_network_event(self, event, network).await
        }

        fn tracked_peers(&self) -> Option<Vec<SocketAddr>> {
            self.tracked.clone()
        }
    }

    #[tokio::test]
    async fn test_manager_task_shutdown() {
        let tick_count = Arc::new(AtomicU32::new(0));

        let manager = MockManager {
            tick_count: tick_count.clone(),
            ..mock_manager()
        };

        let (_msg_tx, message_receiver) = mpsc::unbounded_channel();
        let sync_event_sender = broadcast::Sender::<SyncEvent>::new(100);
        let network_event_sender = broadcast::Sender::<NetworkEvent>::new(100);
        let network: Arc<dyn NetworkManager> = Arc::new(MockNetworkManager::new());
        let shutdown = CancellationToken::new();
        let (progress_sender, _progress_rx) = watch::channel(manager.progress());

        let context = SyncManagerTaskContext {
            message_receiver,
            sync_event_sender,
            network_event_receiver: network_event_sender.subscribe(),
            network,
            shutdown: shutdown.clone(),
            progress_sender,
        };

        let handle = tokio::spawn(async move { manager.run(context).await });

        // Let the 100ms tick fire a few times.
        tokio::time::sleep(Duration::from_millis(250)).await;
        shutdown.cancel();

        let result = handle.await.unwrap();
        assert_eq!(result.unwrap(), ManagerIdentifier::BlockHeader);
        assert!(
            tick_count.load(Ordering::Relaxed) >= 2,
            "the run loop should have ticked while alive, got {}",
            tick_count.load(Ordering::Relaxed)
        );
    }

    fn mock_manager() -> MockManager {
        MockManager {
            identifier: ManagerIdentifier::BlockHeader,
            state: SyncState::WaitForEvents,
            tick_count: Arc::new(AtomicU32::new(0)),
            echo: false,
            events_seen: Arc::new(AtomicU32::new(0)),
            network_events: Arc::default(),
            stops: Arc::new(AtomicU32::new(0)),
            starts: Arc::new(AtomicU32::new(0)),
            tracked: None,
            gate: None,
        }
    }

    /// A manager that emits an event for each message, with a queue of messages always
    /// waiting, sees every one of its own events: it handles them between messages rather
    /// than leaving them to the select, which favours the waiting messages until the bus
    /// overflows.
    #[tokio::test]
    async fn test_own_events_are_not_lost_behind_waiting_messages() {
        const MESSAGES: u32 = 200;
        let manager = MockManager {
            echo: true,
            ..mock_manager()
        };
        let events_seen = manager.events_seen.clone();
        let (msg_tx, message_receiver) = mpsc::unbounded_channel();
        for _ in 0..MESSAGES {
            let msg = Arc::new(InboundMessage::from(NetworkMessage::Verack));
            msg_tx.send((crate::test_utils::test_socket_address(1), msg)).unwrap();
        }
        // Far smaller than the backlog an undrained receiver builds over 200 messages.
        let sync_event_sender = broadcast::Sender::<SyncEvent>::new(8);
        let network_event_sender = broadcast::Sender::<NetworkEvent>::new(8);
        let shutdown = CancellationToken::new();
        let (progress_sender, _progress_rx) = watch::channel(manager.progress());
        let context = SyncManagerTaskContext {
            message_receiver,
            sync_event_sender,
            network_event_receiver: network_event_sender.subscribe(),
            network: Arc::new(MockNetworkManager::new()),
            shutdown: shutdown.clone(),
            progress_sender,
        };
        let handle = tokio::spawn(async move { manager.run(context).await });

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while events_seen.load(Ordering::Relaxed) < MESSAGES
            && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        shutdown.cancel();
        handle.await.unwrap().unwrap();
        assert_eq!(events_seen.load(Ordering::Relaxed), MESSAGES, "own events were skipped");
    }

    /// A context on `network` whose network-event channel holds `capacity` events, plus the
    /// shutdown token and the channel's sender (dropping it closes the channel, which ends the
    /// task).
    fn context_on(
        manager: &MockManager,
        capacity: usize,
        network: Arc<MockNetworkManager>,
    ) -> (SyncManagerTaskContext, CancellationToken, broadcast::Sender<NetworkEvent>) {
        let (_, message_receiver) = mpsc::unbounded_channel();
        let network_event_sender = broadcast::Sender::<NetworkEvent>::new(capacity);
        let shutdown = CancellationToken::new();
        let (progress_sender, _progress_rx) = watch::channel(manager.progress());
        let context = SyncManagerTaskContext {
            message_receiver,
            sync_event_sender: broadcast::Sender::<SyncEvent>::new(100),
            network_event_receiver: network_event_sender.subscribe(),
            network,
            shutdown: shutdown.clone(),
            progress_sender,
        };
        (context, shutdown, network_event_sender)
    }

    /// A context whose network-event receiver has already lagged behind a capacity-1
    /// channel, plus the shutdown token and the channel's sender.
    fn lagged_context(
        manager: &MockManager,
    ) -> (SyncManagerTaskContext, CancellationToken, broadcast::Sender<NetworkEvent>) {
        let (context, shutdown, sender) =
            context_on(manager, 1, Arc::new(MockNetworkManager::new()));
        for _ in 0..2 {
            sender
                .send(NetworkEvent::PeerConnected(crate::test_utils::test_socket_address(1)))
                .unwrap();
        }
        (context, shutdown, sender)
    }

    /// Polls `done` every 10 ms for up to 5 s.
    async fn eventually(mut done: impl FnMut() -> bool) -> bool {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !done() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        done()
    }

    /// PLAN-r0 §4.6, DEC-1946/1947: a lag across the loss of every peer ends nothing. The
    /// resubscribe drops what the skip left, so only the re-read of the moved disconnect count
    /// can stop the manager: its `PeersUpdated` with no peers runs `stop_sync`. A later lag with
    /// no loss and no peer replays nothing.
    #[tokio::test]
    async fn lagged_network_events_are_not_fatal() {
        let manager = mock_manager();
        let (stops, seen, ticks) =
            (manager.stops.clone(), manager.network_events.clone(), manager.tick_count.clone());
        let network = Arc::new(MockNetworkManager::new());
        let (context, shutdown, sender) = context_on(&manager, 1, network.clone());
        let handle = tokio::spawn(manager.run(context));
        assert!(eventually(|| ticks.load(Ordering::Relaxed) > 0).await, "the task runs");

        let peer = crate::test_utils::test_socket_address(1);
        network.set_connected(0);
        sender.send(updated(&[])).unwrap();
        sender.send(NetworkEvent::PeerDisconnected(peer)).unwrap();
        assert!(eventually(|| stops.load(Ordering::Relaxed) == 1).await, "the lag stops it");
        assert!(!handle.is_finished(), "the task must survive the lag");

        sender.send(NetworkEvent::PeerConnected(peer)).unwrap();
        sender.send(NetworkEvent::PeerConnected(peer)).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        sender.send(NetworkEvent::PeerDisconnected(peer)).unwrap();
        assert!(eventually(|| seen.lock().len() == 2).await, "{:?}", seen.lock());
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(*seen.lock(), [updated(&[]), NetworkEvent::PeerDisconnected(peer)]);
        assert_eq!(stops.load(Ordering::Relaxed), 1, "no loss, no second stop");
        assert!(!handle.is_finished());
        shutdown.cancel();
        assert_eq!(handle.await.unwrap().unwrap(), ManagerIdentifier::BlockHeader);
    }

    fn updated(peers: &[SocketAddr]) -> NetworkEvent {
        NetworkEvent::PeersUpdated {
            addresses: peers.to_vec(),
            best_height: 0,
        }
    }

    /// DEC-2007 M1: a `Synced` manager lagged across a loss of every peer and a reconnect runs
    /// the loss's stop, then restarts on the peer that is back: one stop, one start. The
    /// resubscribe drops the reconnect's `PeersUpdated` that the skip left.
    #[tokio::test]
    async fn a_lag_across_a_loss_and_a_reconnect_stops_and_restarts_once() {
        let manager = MockManager {
            state: SyncState::Synced,
            ..mock_manager()
        };
        let (stops, starts, seen, ticks) = (
            manager.stops.clone(),
            manager.starts.clone(),
            manager.network_events.clone(),
            manager.tick_count.clone(),
        );
        let network = Arc::new(MockNetworkManager::new());
        let (context, shutdown, sender) = context_on(&manager, 1, network.clone());
        let handle = tokio::spawn(manager.run(context));
        assert!(eventually(|| ticks.load(Ordering::Relaxed) > 0).await, "the task runs");

        let peer = crate::test_utils::test_socket_address(1);
        network.set_connected(0);
        network.set_connected(1);
        for event in [
            NetworkEvent::PeerDisconnected(peer),
            updated(&[]),
            NetworkEvent::PeerConnected(peer),
            updated(&[peer]),
        ] {
            sender.send(event).unwrap();
        }
        assert!(eventually(|| seen.lock().len() == 3).await, "{:?}", seen.lock());
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert_eq!(
            *seen.lock(),
            [updated(&[]), NetworkEvent::PeerConnected(peer), updated(&[peer])]
        );
        assert_eq!((stops.load(Ordering::Relaxed), starts.load(Ordering::Relaxed)), (1, 1));
        shutdown.cancel();
        assert_eq!(handle.await.unwrap().unwrap(), ManagerIdentifier::BlockHeader);
    }

    /// DEC-2007 m1: loss 1, a reconnect and loss 2 are counted before the task handles loss 1;
    /// a lag then skips loss 2's events. Handling loss 1 counts one loss handled, not the
    /// network's two, so the lag replays loss 2.
    #[tokio::test]
    async fn a_loss_counted_behind_a_handled_one_is_not_missed() {
        let manager = mock_manager();
        let (stops, seen, ticks) =
            (manager.stops.clone(), manager.network_events.clone(), manager.tick_count.clone());
        let network = Arc::new(MockNetworkManager::new());
        let (context, shutdown, sender) = context_on(&manager, 1, network.clone());
        let handle = tokio::spawn(manager.run(context));
        assert!(eventually(|| ticks.load(Ordering::Relaxed) > 0).await, "the task runs");

        let peer = crate::test_utils::test_socket_address(1);
        network.set_connected(0);
        network.set_connected(1);
        network.set_connected(0);
        sender.send(updated(&[])).unwrap();
        assert!(eventually(|| stops.load(Ordering::Relaxed) == 1).await, "loss 1 is handled");
        sender.send(updated(&[peer])).unwrap();
        sender.send(NetworkEvent::PeerDisconnected(peer)).unwrap();
        assert!(eventually(|| stops.load(Ordering::Relaxed) == 2).await, "loss 2 is replayed");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(*seen.lock(), [updated(&[]), updated(&[])]);
        shutdown.cancel();
        assert_eq!(handle.await.unwrap().unwrap(), ManagerIdentifier::BlockHeader);
    }

    /// DEC-2007 n1, DEC-2010: a loss counted before a lag's re-read whose empty `PeersUpdated`
    /// is sent after the resubscribe (the window inside `PeerSet::announce`) is replayed and
    /// then handled live, which only stops the manager again. The reconnect then starts it once,
    /// and a later lag with no loss replays only the snapshot, which starts nothing more.
    #[tokio::test]
    async fn a_loss_replayed_then_handled_live_is_harmless() {
        let manager = mock_manager();
        let (stops, starts, seen, ticks) = (
            manager.stops.clone(),
            manager.starts.clone(),
            manager.network_events.clone(),
            manager.tick_count.clone(),
        );
        let network = Arc::new(MockNetworkManager::new());
        let (context, shutdown, sender) = context_on(&manager, 1, network.clone());
        let handle = tokio::spawn(manager.run(context));
        assert!(eventually(|| ticks.load(Ordering::Relaxed) > 0).await, "the task runs");

        let (peer, other) =
            (crate::test_utils::test_socket_address(1), crate::test_utils::test_socket_address(2));
        network.set_connected(0);
        sender.send(NetworkEvent::PeerConnected(other)).unwrap();
        sender.send(NetworkEvent::PeerConnected(other)).unwrap();
        assert!(eventually(|| stops.load(Ordering::Relaxed) == 1).await, "the lag replays it");
        sender.send(updated(&[])).unwrap();
        assert!(eventually(|| stops.load(Ordering::Relaxed) == 2).await, "{:?}", seen.lock());
        network.set_connected(1);
        sender.send(updated(&[peer])).unwrap();
        assert!(eventually(|| starts.load(Ordering::Relaxed) == 1).await);
        sender.send(NetworkEvent::PeerConnected(peer)).unwrap();
        sender.send(NetworkEvent::PeerConnected(peer)).unwrap();
        assert!(eventually(|| seen.lock().len() == 4).await, "{:?}", seen.lock());
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert_eq!(*seen.lock(), [updated(&[]), updated(&[]), updated(&[peer]), updated(&[peer])]);
        assert_eq!((stops.load(Ordering::Relaxed), starts.load(Ordering::Relaxed)), (2, 1));
        assert!(!handle.is_finished());
        shutdown.cancel();
        assert_eq!(handle.await.unwrap().unwrap(), ManagerIdentifier::BlockHeader);
    }

    /// DEC-2010, the r2 probe's schedule: loss 1 is counted and a lag keeps its empty
    /// `PeersUpdated`; loss 2 is counted while the lag's replay is held; the intermediate
    /// reconnect is handled live; then loss 2's events and the final reconnect arrive, and a
    /// second lag skips loss 2's. The resubscribe drops loss 1's retained event, so it can't
    /// count loss 2 as handled: the second lag replays loss 2. Each loss stops the manager
    /// once and each reconnect starts it once.
    #[tokio::test]
    async fn a_loss_counted_during_a_lag_replay_is_not_missed_on_a_second_lag() {
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let manager = MockManager {
            gate: Some(gate.clone()),
            ..mock_manager()
        };
        let (stops, starts, seen, ticks) = (
            manager.stops.clone(),
            manager.starts.clone(),
            manager.network_events.clone(),
            manager.tick_count.clone(),
        );
        let network = Arc::new(MockNetworkManager::new());
        let (context, shutdown, sender) = context_on(&manager, 2, network.clone());
        let handle = tokio::spawn(manager.run(context));
        assert!(eventually(|| ticks.load(Ordering::Relaxed) > 0).await, "the task runs");

        let peer = crate::test_utils::test_socket_address(1);
        network.set_connected(0);
        for event in
            [NetworkEvent::PeerConnected(peer), NetworkEvent::PeerDisconnected(peer), updated(&[])]
        {
            sender.send(event).unwrap();
        }
        assert!(eventually(|| seen.lock().len() == 1).await, "the lag's replay is held");
        network.set_connected(1);
        network.set_connected(0);
        gate.add_permits(1000);
        assert!(eventually(|| stops.load(Ordering::Relaxed) == 1).await, "{:?}", seen.lock());
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(*seen.lock(), [updated(&[])], "loss 1's retained events are dropped");

        network.set_connected(1);
        sender.send(NetworkEvent::PeerConnected(peer)).unwrap();
        sender.send(updated(&[peer])).unwrap();
        assert!(eventually(|| starts.load(Ordering::Relaxed) == 1).await, "{:?}", seen.lock());
        for event in [
            NetworkEvent::PeerDisconnected(peer),
            updated(&[]),
            NetworkEvent::PeerConnected(peer),
            updated(&[peer]),
        ] {
            sender.send(event).unwrap();
        }
        assert!(eventually(|| starts.load(Ordering::Relaxed) == 2).await, "{:?}", seen.lock());
        tokio::time::sleep(Duration::from_millis(250)).await;
        let back = [NetworkEvent::PeerConnected(peer), updated(&[peer])];
        assert_eq!(*seen.lock(), [&[updated(&[])][..], &back, &[updated(&[])], &back].concat(),);
        assert_eq!((stops.load(Ordering::Relaxed), starts.load(Ordering::Relaxed)), (2, 2));
        assert_eq!(network.disconnects(), 2);
        shutdown.cancel();
        assert_eq!(handle.await.unwrap().unwrap(), ManagerIdentifier::BlockHeader);
    }

    /// DEC-2010 n3-r2: a manager waiting for connections after a loss, lagged past the
    /// reconnect's `PeersUpdated`, restarts once on the snapshot the no-loss lag replays.
    #[tokio::test]
    async fn a_lag_past_the_reconnect_restarts_a_waiting_manager_once() {
        let manager = mock_manager();
        let (stops, starts, seen, ticks) = (
            manager.stops.clone(),
            manager.starts.clone(),
            manager.network_events.clone(),
            manager.tick_count.clone(),
        );
        let network = Arc::new(MockNetworkManager::new());
        let (context, shutdown, sender) = context_on(&manager, 1, network.clone());
        let handle = tokio::spawn(manager.run(context));
        assert!(eventually(|| ticks.load(Ordering::Relaxed) > 0).await, "the task runs");

        let peer = crate::test_utils::test_socket_address(1);
        network.set_connected(0);
        sender.send(updated(&[])).unwrap();
        assert!(eventually(|| stops.load(Ordering::Relaxed) == 1).await, "the loss stops it");
        network.set_connected(1);
        sender.send(NetworkEvent::PeerConnected(peer)).unwrap();
        sender.send(updated(&[peer])).unwrap();
        assert!(eventually(|| starts.load(Ordering::Relaxed) == 1).await, "{:?}", seen.lock());
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert_eq!(*seen.lock(), [updated(&[]), updated(&[peer])]);
        assert_eq!((stops.load(Ordering::Relaxed), starts.load(Ordering::Relaxed)), (1, 1));
        shutdown.cancel();
        assert_eq!(handle.await.unwrap().unwrap(), ManagerIdentifier::BlockHeader);
    }

    /// A manager with per-peer state gets the peers that came and went during a lag
    /// replayed, against the connected peers it re-reads, then `PeersUpdated` with them; no
    /// loss is replayed when no loss of every peer was missed.
    #[tokio::test]
    async fn a_lag_replays_the_peers_a_manager_tracks() {
        let addr = crate::test_utils::test_socket_address;
        let manager = MockManager {
            tracked: Some(vec![addr(1), addr(9)]),
            ..mock_manager()
        };
        let (stops, seen) = (manager.stops.clone(), manager.network_events.clone());
        let network = Arc::new(MockNetworkManager::new());
        network.set_connected(2);
        let (context, shutdown, sender) = context_on(&manager, 1, network);
        for _ in 0..2 {
            sender.send(NetworkEvent::PeerConnected(addr(1))).unwrap();
        }
        let handle = tokio::spawn(manager.run(context));
        assert!(eventually(|| seen.lock().len() >= 3).await);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            *seen.lock(),
            [
                NetworkEvent::PeerDisconnected(addr(9)),
                NetworkEvent::PeerConnected(addr(2)),
                updated(&[addr(1), addr(2)]),
            ]
        );
        assert_eq!(stops.load(Ordering::Relaxed), 0);
        shutdown.cancel();
        assert_eq!(handle.await.unwrap().unwrap(), ManagerIdentifier::BlockHeader);
    }

    /// During shutdown a lag is not an error. Shutdown and the lag are both ready and
    /// `select!` picks at random, so repeat it.
    #[tokio::test]
    async fn test_lag_during_shutdown_is_not_an_error() {
        for _ in 0..50 {
            let manager = mock_manager();
            let (context, shutdown, _events) = lagged_context(&manager);
            shutdown.cancel();
            let result = manager.run(context).await;
            assert!(matches!(result, Ok(ManagerIdentifier::BlockHeader)), "{result:?}");
        }
    }

    /// A lag with no loss of every peer, for a manager with no per-peer state, is logged and
    /// replays only `PeersUpdated` with the connected peers: the task keeps running, and the
    /// running manager doesn't restart.
    #[tokio::test]
    async fn test_a_lag_without_a_loss_is_logged() {
        let manager = mock_manager();
        let (tick_count, seen, starts) =
            (manager.tick_count.clone(), manager.network_events.clone(), manager.starts.clone());
        let (context, shutdown, _events) = lagged_context(&manager);
        let handle = tokio::spawn(async move { manager.run(context).await });

        tokio::time::sleep(Duration::from_millis(250)).await;
        assert!(!handle.is_finished(), "the task must survive the lag");
        assert!(tick_count.load(Ordering::Relaxed) >= 1);
        assert_eq!(*seen.lock(), [updated(&[crate::test_utils::test_socket_address(1)])]);
        assert_eq!(starts.load(Ordering::Relaxed), 0);
        shutdown.cancel();
        assert_eq!(handle.await.unwrap().unwrap(), ManagerIdentifier::BlockHeader);
    }
}
