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

impl SyncManagerProgress {
    pub fn state(&self) -> SyncState {
        match self {
            SyncManagerProgress::BlockHeaders(progress) => progress.state(),
            SyncManagerProgress::FilterHeaders(progress) => progress.state(),
            SyncManagerProgress::Filters(progress) => progress.state(),
            SyncManagerProgress::Blocks(progress) => progress.state(),
            SyncManagerProgress::Masternodes(progress) => progress.state(),
            SyncManagerProgress::ChainLock(progress) => progress.state(),
            SyncManagerProgress::InstantSend(progress) => progress.state(),
            SyncManagerProgress::Mempool(progress) => progress.state(),
        }
    }
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
                connected_count,
                ..
            } => write!(f, "PeersUpdated({connected_count} peers)"),
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
        connected_count,
        ..
    } = event
    {
        if *connected_count == 0 {
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

    /// Whether this manager counts disconnects for `BlockHeaderSyncComplete`'s
    /// `connection_epoch`. A lag on the network-event channel skips some of them, so
    /// for such a manager the task ends with an error instead of logging it.
    fn counts_disconnects(&self) -> bool {
        false
    }

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
                            tracing::debug!("{} received network event: {}", identifier, event);
                            let progress_before = self.progress();
                            match self.handle_network_event(&event, &context.network).await {
                                Ok(events) => {
                                    if !events.is_empty() {
                                        for e in &events {
                                            tracing::debug!("{} emitting: {}", identifier, e);
                                        }
                                        context.emit_sync_events(events);
                                    }
                                    self.try_emit_progress(progress_before, &context.progress_sender);
                                }
                                Err(e) => {
                                    tracing::error!("{} error handling network event: {}", identifier, e);
                                }
                            }
                        }
                        // A lag while shutting down loses nothing that matters.
                        Err(broadcast::error::RecvError::Lagged(_))
                            if context.shutdown.is_cancelled() =>
                        {
                            break;
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) if self.counts_disconnects() => {
                            return Err(lagged_network_events(identifier, n));
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("{} lagged network events, skipped {}", identifier, n);
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

/// The error a disconnect-counting manager's task ends with when it falls behind the
/// network-event channel: the skipped events are gone, and its disconnect count (behind
/// `BlockHeaderSyncComplete::connection_epoch`) would silently diverge from the other
/// managers', so the coordinator reports it instead.
fn lagged_network_events(identifier: ManagerIdentifier, skipped: u64) -> SyncError {
    tracing::error!("{} fell {} events behind the network event channel", identifier, skipped);
    SyncError::InvalidState(format!(
        "{} fell {} events behind the network event channel",
        identifier, skipped
    ))
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
        counts_disconnects: bool,
        /// Emit one event per message, as the blocks manager emits `BlockProcessed`.
        echo: bool,
        events_seen: Arc<AtomicU32>,
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

        fn on_disconnect(&mut self) {}

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

        fn counts_disconnects(&self) -> bool {
            self.counts_disconnects
        }
    }

    #[tokio::test]
    async fn test_manager_task_shutdown() {
        let tick_count = Arc::new(AtomicU32::new(0));

        let manager = MockManager {
            tick_count: tick_count.clone(),
            ..mock_manager(false)
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

    fn mock_manager(counts_disconnects: bool) -> MockManager {
        MockManager {
            identifier: ManagerIdentifier::BlockHeader,
            state: SyncState::WaitForEvents,
            tick_count: Arc::new(AtomicU32::new(0)),
            counts_disconnects,
            echo: false,
            events_seen: Arc::new(AtomicU32::new(0)),
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
            ..mock_manager(false)
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

    /// A context whose network-event receiver has already lagged behind a capacity-1
    /// channel, plus the shutdown token and the channel's sender (dropping it closes the
    /// channel, which ends the task).
    fn lagged_context(
        manager: &MockManager,
    ) -> (SyncManagerTaskContext, CancellationToken, broadcast::Sender<NetworkEvent>) {
        let (_, message_receiver) = mpsc::unbounded_channel();
        let network_event_sender = broadcast::Sender::<NetworkEvent>::new(1);
        let network_event_receiver = network_event_sender.subscribe();
        for _ in 0..2 {
            network_event_sender
                .send(NetworkEvent::PeerConnected(crate::test_utils::test_socket_address(1)))
                .unwrap();
        }
        let shutdown = CancellationToken::new();
        let (progress_sender, _progress_rx) = watch::channel(manager.progress());
        let context = SyncManagerTaskContext {
            message_receiver,
            sync_event_sender: broadcast::Sender::<SyncEvent>::new(100),
            network_event_receiver,
            network: Arc::new(MockNetworkManager::new()),
            shutdown: shutdown.clone(),
            progress_sender,
        };
        (context, shutdown, network_event_sender)
    }

    /// A manager that counts disconnects must see every network event: a lag ends its task
    /// with an error the coordinator reports.
    #[tokio::test]
    async fn test_lagged_network_events_end_the_task_with_an_error() {
        let manager = mock_manager(true);
        let (context, _shutdown, _events) = lagged_context(&manager);
        let result = manager.run(context).await;
        assert!(
            matches!(&result, Err(SyncError::InvalidState(message)) if message.contains("behind")),
            "{result:?}"
        );
    }

    /// During shutdown a lag is not an error. Shutdown and the lag are both ready and
    /// `select!` picks at random, so repeat it.
    #[tokio::test]
    async fn test_lag_during_shutdown_is_not_an_error() {
        for _ in 0..50 {
            let manager = mock_manager(true);
            let (context, shutdown, _events) = lagged_context(&manager);
            shutdown.cancel();
            let result = manager.run(context).await;
            assert!(matches!(result, Ok(ManagerIdentifier::BlockHeader)), "{result:?}");
        }
    }

    /// Any other manager logs the lag and keeps running.
    #[tokio::test]
    async fn test_lag_is_logged_for_a_manager_that_does_not_count_disconnects() {
        let manager = mock_manager(false);
        let tick_count = manager.tick_count.clone();
        let (context, shutdown, _events) = lagged_context(&manager);
        let handle = tokio::spawn(async move { manager.run(context).await });

        tokio::time::sleep(Duration::from_millis(250)).await;
        assert!(!handle.is_finished(), "the task must survive the lag");
        assert!(tick_count.load(Ordering::Relaxed) >= 1);
        shutdown.cancel();
        assert_eq!(handle.await.unwrap().unwrap(), ManagerIdentifier::BlockHeader);
    }
}
