//! Event handler trait for receiving SPV client events.
//!
//! Provides `EventHandler`, a trait with default no-op implementations that
//! consumers override to receive push-based event notifications. The monitoring
//! infrastructure subscribes to internal channels and dispatches to the handler.

use std::fmt::Display;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::{broadcast, mpsc, watch, RwLock};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::network::NetworkEvent;
use crate::storage::{MetadataStorage, PersistentMetadataStorage};
use crate::sync::{SyncEvent, SyncProgress, SyncState, BEST_CHAINLOCK_METADATA_STORAGE_KEY};
use dashcore::ephemerealdata::chain_lock::ChainLock;
use key_wallet_manager::{WalletEvent, WalletInterface};

/// Minimum time between two logged progress values while no manager changes state.
const PROGRESS_LOG_INTERVAL: Duration = Duration::from_secs(5);

/// Trait for receiving SPV client events.
///
/// All methods have default no-op implementations, so consumers only
/// need to override the events they care about.
pub trait EventHandler: Send + Sync + 'static {
    /// Called for sync lifecycle events (headers stored, sync complete, etc.).
    fn on_sync_event(&self, _event: &SyncEvent) {}
    /// Called when peer connections change (connect, disconnect, peer list update).
    fn on_network_event(&self, _event: &NetworkEvent) {}
    /// Called when overall sync progress changes.
    fn on_progress(&self, _progress: &SyncProgress) {}
    /// Called for wallet events (transaction received, balance updated).
    fn on_wallet_event(&self, _event: &WalletEvent) {}
    /// Called on fatal errors (start failure, monitor channel failure, sync loop error).
    fn on_error(&self, _error: &str) {}
}

/// No-op implementation for consumers that don't need event notifications.
impl EventHandler for () {}

/// Spawns a task that monitors a broadcast channel and dispatches events to the handler.
///
/// Every received event is mirrored into `tracing::info!` as `"<name>: <event>"`
/// before consumer handlers run, so installing a tracing subscriber is enough to
/// get event log output. To silence event logs without affecting other
/// subscribers, set a per-target filter such as
/// `dash_spv::client::event_handler=warn`.
///
/// A lag skips the missed events: the monitor logs a warning, delivers the latest
/// `progress` to `on_progress` once and keeps receiving. An unexpected channel close
/// is sent via `on_failure` so the coordinator can report it as the single source of
/// error handling. On `shutdown` it delivers the events already queued before it exits.
pub(crate) fn spawn_broadcast_monitor<E, F>(
    name: &'static str,
    mut receiver: broadcast::Receiver<E>,
    handlers: Arc<Vec<Arc<dyn EventHandler>>>,
    progress: watch::Receiver<SyncProgress>,
    shutdown: CancellationToken,
    on_failure: mpsc::Sender<String>,
    dispatch_fn: F,
) -> JoinHandle<()>
where
    E: Clone + Display + Send + 'static,
    F: Fn(&dyn EventHandler, &E) + Send + Sync + 'static,
{
    tokio::spawn(async move {
        tracing::debug!("{} monitoring task started", name);
        let dispatch = |event: E| {
            tracing::info!("{}: {}", name, event);
            for handler in handlers.iter() {
                dispatch_fn(handler.as_ref(), &event);
            }
        };
        loop {
            tokio::select! {
                result = receiver.recv() => {
                    match result {
                        Ok(event) => dispatch(event),
                        Err(broadcast::error::RecvError::Closed) if shutdown.is_cancelled() => break,
                        Err(broadcast::error::RecvError::Closed) => {
                            let msg = format!("{} monitor channel closed unexpectedly", name);
                            tracing::error!("{}", msg);
                            let _ = on_failure.try_send(msg);
                            break;
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) if shutdown.is_cancelled() => break,
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("{} monitor lagged, skipped {} events", name, n);
                            let snapshot = progress.borrow().clone();
                            for handler in handlers.iter() {
                                handler.on_progress(&snapshot);
                            }
                        }
                    }
                }
                _ = shutdown.cancelled() => {
                    // What was sent before the stop is still owed to the handlers, such as
                    // the departures the network announces while it stops.
                    loop {
                        match receiver.try_recv() {
                            Ok(event) => dispatch(event),
                            Err(broadcast::error::TryRecvError::Lagged(n)) => {
                                tracing::warn!(
                                    "{} monitor lagged while stopping, missed {} events",
                                    name,
                                    n
                                );
                            }
                            Err(_) => break,
                        }
                    }
                    break;
                }
            }
        }
        tracing::debug!("{} monitoring task exiting", name);
    })
}

/// Spawns a task that monitors a watch channel for progress updates.
///
/// Sends the initial progress value, then monitors for changes. Every value reaches
/// the consumer handlers. A value is mirrored into `tracing::info!` when a manager
/// changes state or [`PROGRESS_LOG_INTERVAL`] has passed since the last logged one,
/// because sync updates progress many times a second.
pub(crate) fn spawn_progress_monitor(
    mut receiver: watch::Receiver<SyncProgress>,
    handlers: Arc<Vec<Arc<dyn EventHandler>>>,
    shutdown: CancellationToken,
    on_failure: mpsc::Sender<String>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        tracing::debug!("Progress monitoring task started");

        let mut logged: Option<(Instant, Vec<SyncState>)> = None;
        let mut dispatch = |progress: &SyncProgress| {
            let states = progress.manager_states();
            let due = logged.as_ref().is_none_or(|(at, logged_states)| {
                *logged_states != states || at.elapsed() >= PROGRESS_LOG_INTERVAL
            });
            if due {
                tracing::info!("SyncProgress: {}", progress);
                logged = Some((Instant::now(), states));
            }
            for handler in handlers.iter() {
                handler.on_progress(progress);
            }
        };

        dispatch(&receiver.borrow_and_update());

        loop {
            tokio::select! {
                result = receiver.changed() => {
                    match result {
                        Ok(()) => dispatch(&receiver.borrow_and_update()),
                        Err(_) if shutdown.is_cancelled() => break,
                        Err(_) => {
                            let msg = "Progress monitor channel closed unexpectedly".to_string();
                            tracing::error!("{}", msg);
                            let _ = on_failure.try_send(msg);
                            break;
                        }
                    }
                }
                _ = shutdown.cancelled() => break,
            }
        }
        tracing::debug!("Progress monitoring task exiting");
    })
}

/// Spawns a task that drives chainlock-driven wallet promotion.
///
/// Subscribes to [`SyncEvent::ChainLockReceived`] and
/// [`SyncEvent::SyncComplete`], serializing all `apply_chain_lock`
/// calls on a single task. During the initial sync chainlock arrivals
/// are buffered as the latest-seen height rather than applied; only
/// that height is passed on (`note_chain_lock_height`). The first
/// `SyncComplete` (cycle 0, or a later cycle while the initial sync
/// has not completed) applies the buffered chainlock once, then every
/// subsequent validated chainlock is applied directly. Only validated
/// chainlocks advance the wallet.
///
/// A lag resyncs instead of stopping: a synced `progress` completes the
/// initial sync, then the best chainlock in `stored_chain_locks` (which
/// the ChainLock manager saves before it emits one) takes the path of a
/// validated arrival. `stored_chain_locks` is `None` when this client's
/// ChainLocks are off, so nothing stored is read.
///
/// This is the single-writer point that the architecture relies on:
/// promotions never interleave with each other.
pub(crate) fn spawn_chainlock_wallet_dispatch<W>(
    mut sync_event_rx: broadcast::Receiver<SyncEvent>,
    progress: watch::Receiver<SyncProgress>,
    stored_chain_locks: Option<Arc<RwLock<PersistentMetadataStorage>>>,
    wallet: Arc<RwLock<W>>,
    shutdown: CancellationToken,
    on_failure: mpsc::Sender<String>,
) -> JoinHandle<()>
where
    W: WalletInterface + 'static,
{
    const NAME: &str = "ChainLock→wallet dispatch";
    tokio::spawn(async move {
        tracing::debug!("{} task started", NAME);
        let mut dispatch = ChainLockDispatch {
            wallet,
            initial_sync_complete: false,
            deferred_chain_lock: None,
        };
        loop {
            tokio::select! {
                result = sync_event_rx.recv() => {
                    match result {
                        Ok(SyncEvent::ChainLockReceived { chain_lock, validated: true }) => {
                            dispatch.chain_lock(chain_lock).await;
                        }
                        Ok(SyncEvent::SyncComplete { cycle, .. })
                            if cycle == 0 || !dispatch.initial_sync_complete =>
                        {
                            dispatch.complete_initial_sync().await;
                        }
                        Ok(_) => {}
                        Err(broadcast::error::RecvError::Closed) if shutdown.is_cancelled() => break,
                        Err(broadcast::error::RecvError::Closed) => {
                            let msg = format!("{} channel closed unexpectedly", NAME);
                            tracing::error!("{}", msg);
                            let _ = on_failure.try_send(msg);
                            break;
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) if shutdown.is_cancelled() => break,
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("{} monitor lagged, skipped {} events", NAME, n);
                            let synced = progress.borrow().is_synced();
                            if synced && !dispatch.initial_sync_complete {
                                dispatch.complete_initial_sync().await;
                            }
                            if let Some(chain_lock) =
                                stored_best_chain_lock(NAME, stored_chain_locks.as_ref()).await
                            {
                                dispatch.chain_lock(chain_lock).await;
                            }
                        }
                    }
                }
                _ = shutdown.cancelled() => break,
            }
        }
        tracing::debug!("{} task exiting", NAME);
    })
}

/// The wallet state [`spawn_chainlock_wallet_dispatch`] keeps between events.
struct ChainLockDispatch<W> {
    wallet: Arc<RwLock<W>>,
    initial_sync_complete: bool,
    deferred_chain_lock: Option<ChainLock>,
}

impl<W: WalletInterface> ChainLockDispatch<W> {
    /// A validated chainlock: applied once the initial sync completed, buffered before.
    async fn chain_lock(&mut self, chain_lock: ChainLock) {
        if self.initial_sync_complete {
            self.wallet.write().await.apply_chain_lock(chain_lock);
        } else if self
            .deferred_chain_lock
            .as_ref()
            .is_none_or(|buffered| chain_lock.block_height > buffered.block_height)
        {
            self.wallet.write().await.note_chain_lock_height(chain_lock.block_height);
            self.deferred_chain_lock = Some(chain_lock);
        }
    }

    /// The end of the initial sync: applies the buffered chainlock.
    async fn complete_initial_sync(&mut self) {
        self.initial_sync_complete = true;
        if let Some(chain_lock) = self.deferred_chain_lock.take() {
            self.wallet.write().await.apply_chain_lock(chain_lock);
        }
    }
}

/// The best chainlock the ChainLock manager stored, which a lagged dispatch resyncs from.
async fn stored_best_chain_lock(
    name: &str,
    stored_chain_locks: Option<&Arc<RwLock<PersistentMetadataStorage>>>,
) -> Option<ChainLock> {
    let storage = stored_chain_locks?;
    match storage.read().await.load_metadata::<ChainLock>(BEST_CHAINLOCK_METADATA_STORAGE_KEY).await
    {
        Ok(Some(chain_lock)) => {
            tracing::info!(
                "{} resyncs from the stored best chainlock at height {}",
                name,
                chain_lock.block_height
            );
            Some(chain_lock)
        }
        Ok(None) => {
            tracing::info!("{} has no stored best chainlock to resync from", name);
            None
        }
        Err(e) => {
            tracing::warn!("{} could not read the stored best chainlock: {}", name, e);
            None
        }
    }
}

/// How often the reservation-sweep task wakes to reclaim expired reservations.
const RESERVATION_SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// Periodically reclaim receive-address reservations that were handed out but
/// never funded and never explicitly released, which would otherwise pin
/// gap-limit headroom forever.
///
/// The wallet keeps no clock, so this task supplies `now` from the system clock
/// and the caller-supplied `ttl` (seconds). A system-clock failure yields a
/// `now` of `0`, which the wallet treats as "no clock" and reclaims nothing.
/// Runs until `shutdown` is cancelled.
pub(crate) fn spawn_reservation_sweep<W>(
    wallet: Arc<RwLock<W>>,
    ttl_secs: u64,
    shutdown: CancellationToken,
) -> JoinHandle<()>
where
    W: WalletInterface + 'static,
{
    const NAME: &str = "Reservation sweep";
    tokio::spawn(async move {
        tracing::debug!("{} task started", NAME);
        let mut interval = tokio::time::interval(RESERVATION_SWEEP_INTERVAL);
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    let reclaimed = wallet.write().await.sweep_expired_reservations(now, ttl_secs);
                    if reclaimed > 0 {
                        tracing::debug!("{}: reclaimed {} expired reservation(s)", NAME, reclaimed);
                    }
                }
                _ = shutdown.cancelled() => break,
            }
        }
        tracing::debug!("{} task exiting", NAME);
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
    use std::sync::{mpsc as std_mpsc, Arc};
    use std::time::Duration;

    use dashcore::bls_sig_utils::BLSSignature;
    use dashcore::ephemerealdata::chain_lock::ChainLock;
    use dashcore::ephemerealdata::instant_lock::InstantLock;
    use dashcore::prelude::CoreBlockHeight;
    use dashcore::{Address, Block, BlockHash, OutPoint, ScriptBuf, Transaction};
    use tokio::sync::{broadcast, mpsc, watch, RwLock};
    use tokio_util::sync::CancellationToken;

    use super::{
        spawn_broadcast_monitor, spawn_chainlock_wallet_dispatch, spawn_progress_monitor,
        EventHandler,
    };
    use crate::fault_hooks::with_sync_event_capacity;
    use crate::network::NetworkEvent;
    use crate::storage::{DiskStorageManager, MetadataStorage, StorageManager};
    use crate::sync::{
        sync_event_bus, BlockHeadersProgress, ManagerIdentifier, SyncEvent, SyncProgress,
        SyncState, BEST_CHAINLOCK_METADATA_STORAGE_KEY,
    };
    use crate::test_utils::test_socket_address;
    use key_wallet_manager::{
        BlockProcessingResult, MempoolTransactionResult, WalletError, WalletEvent, WalletId,
        WalletInterface,
    };

    struct RecordingHandler {
        sync_count: AtomicUsize,
        network_count: AtomicUsize,
        progress_count: AtomicUsize,
        wallet_count: AtomicUsize,
        error_count: AtomicUsize,
    }

    impl RecordingHandler {
        fn new() -> Self {
            Self {
                sync_count: AtomicUsize::new(0),
                network_count: AtomicUsize::new(0),
                progress_count: AtomicUsize::new(0),
                wallet_count: AtomicUsize::new(0),
                error_count: AtomicUsize::new(0),
            }
        }
    }

    impl EventHandler for RecordingHandler {
        fn on_sync_event(&self, _event: &SyncEvent) {
            self.sync_count.fetch_add(1, Ordering::SeqCst);
        }
        fn on_network_event(&self, _event: &NetworkEvent) {
            self.network_count.fetch_add(1, Ordering::SeqCst);
        }
        fn on_progress(&self, _progress: &SyncProgress) {
            self.progress_count.fetch_add(1, Ordering::SeqCst);
        }
        fn on_wallet_event(&self, _event: &WalletEvent) {
            self.wallet_count.fetch_add(1, Ordering::SeqCst);
        }
        fn on_error(&self, _error: &str) {
            self.error_count.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn handlers(
        entries: impl IntoIterator<Item = Arc<dyn EventHandler>>,
    ) -> Arc<Vec<Arc<dyn EventHandler>>> {
        Arc::new(entries.into_iter().collect())
    }

    fn idle_progress() -> watch::Receiver<SyncProgress> {
        watch::channel(SyncProgress::default()).1
    }

    fn synced_progress() -> SyncProgress {
        let mut headers = BlockHeadersProgress::default();
        headers.set_state(SyncState::Synced);
        let mut progress = SyncProgress::default();
        progress.update_headers(headers);
        assert!(progress.is_synced());
        progress
    }

    fn headers_stored(tip_height: u32) -> SyncEvent {
        SyncEvent::BlockHeadersStored {
            tip_height,
        }
    }

    fn chain_lock(block_height: u32) -> ChainLock {
        ChainLock {
            block_height,
            block_hash: BlockHash::dummy(block_height),
            signature: BLSSignature::from([0u8; 96]),
        }
    }

    /// The warnings logged on this thread while the returned guard lives.
    struct Warnings {
        lines: std_mpsc::Receiver<Vec<u8>>,
        text: String,
        _guard: tracing::subscriber::DefaultGuard,
    }

    struct WarningWriter(std_mpsc::Sender<Vec<u8>>);

    impl std::io::Write for WarningWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let _ = self.0.send(buf.to_vec());
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Warnings {
        fn capture() -> Self {
            let (sender, lines) = std_mpsc::channel();
            let subscriber = tracing_subscriber::fmt()
                .with_max_level(tracing::Level::WARN)
                .with_ansi(false)
                .with_writer(move || WarningWriter(sender.clone()))
                .finish();
            Self {
                lines,
                text: String::new(),
                _guard: tracing::subscriber::set_default(subscriber),
            }
        }

        fn count(&mut self, needle: &str) -> usize {
            while let Ok(chunk) = self.lines.try_recv() {
                self.text.push_str(&String::from_utf8_lossy(&chunk));
            }
            self.text.matches(needle).count()
        }
    }

    async fn wait_until(what: &str, mut done: impl AsyncFnMut() -> bool) {
        for _ in 0..1000 {
            if done().await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting until {what}");
    }

    /// Sleeps in every sync event, so its monitor falls behind a fast sender.
    struct SlowHandler {
        delay: Duration,
        entered: AtomicBool,
        last_tip: AtomicU32,
        progress_count: AtomicUsize,
        error_count: AtomicUsize,
    }

    impl EventHandler for SlowHandler {
        fn on_sync_event(&self, event: &SyncEvent) {
            self.entered.store(true, Ordering::SeqCst);
            std::thread::sleep(self.delay);
            if let SyncEvent::BlockHeadersStored {
                tip_height,
            } = event
            {
                self.last_tip.store(*tip_height, Ordering::SeqCst);
            }
        }
        fn on_progress(&self, _progress: &SyncProgress) {
            self.progress_count.fetch_add(1, Ordering::SeqCst);
        }
        fn on_error(&self, _error: &str) {
            self.error_count.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Records the chainlocks the dispatch hands the wallet.
    #[derive(Default)]
    struct ChainLockWallet {
        applied: Vec<u32>,
        noted: Vec<u32>,
    }

    #[async_trait::async_trait]
    impl WalletInterface for ChainLockWallet {
        async fn process_block_for_wallets(
            &mut self,
            _block: &Block,
            _block_hash: BlockHash,
            _height: CoreBlockHeight,
            _wallets: &BTreeSet<WalletId>,
        ) -> BlockProcessingResult {
            BlockProcessingResult::default()
        }
        async fn process_mempool_transaction(
            &mut self,
            _tx: &Transaction,
            _instant_lock: Option<InstantLock>,
        ) -> MempoolTransactionResult {
            MempoolTransactionResult::default()
        }
        fn monitored_addresses(&self) -> Vec<Address> {
            Vec::new()
        }
        fn monitored_script_pubkeys_for(&self, _wallet_id: &WalletId) -> Vec<ScriptBuf> {
            Vec::new()
        }
        fn watched_outpoints(&self) -> Vec<OutPoint> {
            Vec::new()
        }
        fn last_processed_height(&self) -> CoreBlockHeight {
            0
        }
        fn synced_height(&self) -> CoreBlockHeight {
            0
        }
        fn wallets_behind(&self, _height: CoreBlockHeight) -> BTreeSet<WalletId> {
            BTreeSet::new()
        }
        fn wallet_synced_height(&self, _wallet_id: &WalletId) -> CoreBlockHeight {
            0
        }
        fn update_wallet_synced_height(&mut self, _wallet_id: &WalletId, _height: CoreBlockHeight) {
        }
        fn truncate_above(&mut self, _height: CoreBlockHeight) -> Result<(), WalletError> {
            Ok(())
        }
        fn update_wallet_last_processed_height(
            &mut self,
            _wallet_id: &WalletId,
            _height: CoreBlockHeight,
        ) {
        }
        fn subscribe_events(&self) -> broadcast::Receiver<WalletEvent> {
            broadcast::channel(1).1
        }
        fn apply_chain_lock(&mut self, chain_lock: ChainLock) {
            self.applied.push(chain_lock.block_height);
        }
        fn note_chain_lock_height(&mut self, height: CoreBlockHeight) {
            self.noted.push(height);
        }
    }

    /// A dispatch on a 4-event bus over a store holding the best chainlock at height 100,
    /// lagged while the test holds the wallet lock: it buffers the chainlock at height 50,
    /// then misses `SyncComplete { cycle: 0 }` and the events after it.
    struct LaggedDispatch {
        bus: broadcast::Sender<SyncEvent>,
        wallet: Arc<RwLock<ChainLockWallet>>,
        failures: mpsc::Receiver<String>,
        shutdown: CancellationToken,
        task: tokio::task::JoinHandle<()>,
        _storage: DiskStorageManager,
    }

    impl LaggedDispatch {
        async fn run(progress: SyncProgress, chain_locks_on: bool) -> Self {
            let storage = DiskStorageManager::with_temp_dir().await.unwrap();
            let bytes = serde_json::to_vec(&chain_lock(100)).unwrap();
            storage
                .metadata()
                .write()
                .await
                .store_metadata(BEST_CHAINLOCK_METADATA_STORAGE_KEY, &bytes)
                .await
                .unwrap();
            let bus = with_sync_event_capacity(4, async { sync_event_bus() }).await;
            let wallet = Arc::new(RwLock::new(ChainLockWallet::default()));
            let shutdown = CancellationToken::new();
            let (failure_tx, failures) = mpsc::channel(1);
            let task = spawn_chainlock_wallet_dispatch(
                bus.subscribe(),
                watch::channel(progress).1,
                chain_locks_on.then(|| storage.metadata()),
                wallet.clone(),
                shutdown.clone(),
                failure_tx,
            );

            let held = wallet.write().await;
            bus.send(SyncEvent::ChainLockReceived {
                chain_lock: chain_lock(50),
                validated: true,
            })
            .unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
            bus.send(SyncEvent::SyncComplete {
                header_tip: 100,
                cycle: 0,
            })
            .unwrap();
            for tip_height in 0..8 {
                bus.send(headers_stored(tip_height)).unwrap();
            }
            drop(held);
            Self {
                bus,
                wallet,
                failures,
                shutdown,
                task,
                _storage: storage,
            }
        }

        async fn wait_for_noted(&self, height: u32) {
            wait_until(&format!("the wallet noted height {height}"), async || {
                self.wallet.read().await.noted.contains(&height)
            })
            .await;
        }

        async fn wait_for_applied(&self, heights: &[u32]) {
            wait_until(&format!("the wallet applied exactly {heights:?}"), async || {
                self.wallet.read().await.applied == heights
            })
            .await;
        }

        async fn stop(mut self) {
            assert!(matches!(self.failures.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
            self.shutdown.cancel();
            self.task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn noop_handler_does_not_panic() {
        let handler: () = ();
        let event = SyncEvent::BlockHeadersStored {
            tip_height: 100,
        };
        handler.on_sync_event(&event);
        handler.on_network_event(&NetworkEvent::PeersUpdated {
            addresses: vec![],
            best_height: 0,
        });
        handler.on_progress(&SyncProgress::default());
        handler.on_error("test error");
    }

    #[tokio::test]
    async fn broadcast_monitor_dispatches_events() {
        let (tx, rx) = broadcast::channel(16);
        let handler = Arc::new(RecordingHandler::new());
        let shutdown = CancellationToken::new();
        let (failure_tx, _failure_rx) = mpsc::channel(1);

        let task = spawn_broadcast_monitor(
            "test",
            rx,
            handlers([handler.clone() as Arc<dyn EventHandler>]),
            idle_progress(),
            shutdown.clone(),
            failure_tx,
            |h, event: &SyncEvent| h.on_sync_event(event),
        );

        tx.send(SyncEvent::BlockHeadersStored {
            tip_height: 1,
        })
        .unwrap();
        tx.send(SyncEvent::BlockHeadersStored {
            tip_height: 2,
        })
        .unwrap();
        tx.send(SyncEvent::SyncStart {
            identifier: ManagerIdentifier::BlockHeader,
        })
        .unwrap();

        // Give the task time to process
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        shutdown.cancel();
        task.await.unwrap();

        assert_eq!(handler.sync_count.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn broadcast_monitor_exits_on_shutdown() {
        let (_tx, rx) = broadcast::channel::<SyncEvent>(16);
        let handler = Arc::new(RecordingHandler::new());
        let shutdown = CancellationToken::new();
        let (failure_tx, _failure_rx) = mpsc::channel(1);

        let task = spawn_broadcast_monitor(
            "test",
            rx,
            handlers([handler.clone() as Arc<dyn EventHandler>]),
            idle_progress(),
            shutdown.clone(),
            failure_tx,
            |h, event: &SyncEvent| h.on_sync_event(event),
        );

        shutdown.cancel();
        task.await.unwrap();

        assert_eq!(handler.sync_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn broadcast_monitor_fails_on_unexpected_channel_close() {
        let (tx, rx) = broadcast::channel::<SyncEvent>(16);
        let handler = Arc::new(RecordingHandler::new());
        let shutdown = CancellationToken::new();
        let (failure_tx, mut failure_rx) = mpsc::channel(1);

        let task = spawn_broadcast_monitor(
            "test",
            rx,
            handlers([handler.clone() as Arc<dyn EventHandler>]),
            idle_progress(),
            shutdown.clone(),
            failure_tx,
            |h, event: &SyncEvent| h.on_sync_event(event),
        );

        // Drop sender without cancelling shutdown — this is unexpected
        drop(tx);
        task.await.unwrap();

        let msg = failure_rx.try_recv().expect("should have received failure message");
        assert!(msg.contains("closed unexpectedly"));
        assert_eq!(handler.error_count.load(Ordering::SeqCst), 0);
    }

    /// A handler too slow for the bus makes its monitor lag: the monitor skips what it
    /// missed, resyncs the progress and keeps delivering, and nothing stops the client.
    #[tokio::test]
    async fn slow_handler_lag_resyncs_and_keeps_running() {
        let mut warnings = Warnings::capture();
        let bus = with_sync_event_capacity(4, async { sync_event_bus() }).await;
        let handler = Arc::new(SlowHandler {
            delay: Duration::from_millis(50),
            entered: AtomicBool::new(false),
            last_tip: AtomicU32::new(u32::MAX),
            progress_count: AtomicUsize::new(0),
            error_count: AtomicUsize::new(0),
        });
        let shutdown = CancellationToken::new();
        let (failure_tx, mut failure_rx) = mpsc::channel(1);
        let task = spawn_broadcast_monitor(
            "SyncEvent",
            bus.subscribe(),
            handlers([handler.clone() as Arc<dyn EventHandler>]),
            idle_progress(),
            shutdown.clone(),
            failure_tx,
            |h, event: &SyncEvent| h.on_sync_event(event),
        );

        let sender = {
            let bus = bus.clone();
            let handler = handler.clone();
            std::thread::spawn(move || {
                bus.send(headers_stored(0)).unwrap();
                while !handler.entered.load(Ordering::SeqCst) {
                    std::thread::yield_now();
                }
                for tip_height in 1..64 {
                    bus.send(headers_stored(tip_height)).unwrap();
                }
            })
        };
        wait_until("the monitor delivered the last of the 64 events", async || {
            handler.last_tip.load(Ordering::SeqCst) == 63
        })
        .await;
        sender.join().unwrap();

        bus.send(headers_stored(1000)).unwrap();
        wait_until("the monitor delivered the next event", async || {
            handler.last_tip.load(Ordering::SeqCst) == 1000
        })
        .await;

        assert!(matches!(failure_rx.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
        assert_eq!(warnings.count("SyncEvent monitor lagged, skipped"), 1, "{}", warnings.text);
        assert_eq!(handler.progress_count.load(Ordering::SeqCst), 1);
        assert_eq!(handler.error_count.load(Ordering::SeqCst), 0);

        shutdown.cancel();
        task.await.unwrap();
    }

    /// A dispatch that lags across the end of the initial sync resyncs from the progress
    /// and the stored best chainlock, so the wallet still gets the chainlocks it missed.
    #[tokio::test]
    async fn chainlock_dispatch_lag_applies_the_deferred_lock() {
        let mut warnings = Warnings::capture();
        let dispatch = LaggedDispatch::run(synced_progress(), true).await;
        dispatch.wait_for_applied(&[50, 100]).await;
        assert_eq!(dispatch.wallet.read().await.noted, vec![50]);
        dispatch.stop().await;
        assert_eq!(
            warnings.count("ChainLock→wallet dispatch monitor lagged, skipped"),
            1,
            "{}",
            warnings.text
        );

        let dispatch = LaggedDispatch::run(SyncProgress::default(), true).await;
        dispatch.wait_for_noted(100).await;
        assert!(dispatch.wallet.read().await.applied.is_empty());
        dispatch
            .bus
            .send(SyncEvent::SyncComplete {
                header_tip: 100,
                cycle: 1,
            })
            .unwrap();
        dispatch.wait_for_applied(&[100]).await;
        assert_eq!(dispatch.wallet.read().await.noted, vec![50, 100]);
        dispatch.stop().await;
        assert_eq!(warnings.count("ChainLock→wallet dispatch monitor lagged, skipped"), 2);
    }

    /// With ChainLocks off the resync reads no stored chainlock, so the wallet gets none
    /// of it, while the dispatch keeps running.
    #[tokio::test]
    async fn chainlock_dispatch_lag_with_chain_locks_off_applies_no_stored_lock() {
        let mut warnings = Warnings::capture();
        let dispatch = LaggedDispatch::run(synced_progress(), false).await;
        dispatch.wait_for_applied(&[50]).await;
        dispatch
            .bus
            .send(SyncEvent::ChainLockReceived {
                chain_lock: chain_lock(150),
                validated: true,
            })
            .unwrap();
        dispatch.wait_for_applied(&[50, 150]).await;
        assert_eq!(dispatch.wallet.read().await.noted, vec![50]);
        dispatch.stop().await;
        assert_eq!(warnings.count("ChainLock→wallet dispatch monitor lagged, skipped"), 1);
    }

    #[tokio::test]
    async fn progress_monitor_sends_initial_and_updates() {
        let (tx, rx) = watch::channel(SyncProgress::default());
        let handler = Arc::new(RecordingHandler::new());
        let shutdown = CancellationToken::new();
        let (failure_tx, _failure_rx) = mpsc::channel(1);

        let task = spawn_progress_monitor(
            rx,
            handlers([handler.clone() as Arc<dyn EventHandler>]),
            shutdown.clone(),
            failure_tx,
        );

        // Give the task time to send initial progress
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Send two updates
        tx.send_modify(|_| {});
        tx.send_modify(|_| {});
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        shutdown.cancel();
        task.await.unwrap();

        // 1 initial + at least 1 update (watch coalesces rapid updates)
        assert!(handler.progress_count.load(Ordering::SeqCst) >= 2);
    }

    #[tokio::test]
    async fn progress_monitor_fails_on_unexpected_sender_drop() {
        let (tx, rx) = watch::channel(SyncProgress::default());
        let handler = Arc::new(RecordingHandler::new());
        let shutdown = CancellationToken::new();
        let (failure_tx, mut failure_rx) = mpsc::channel(1);

        let task = spawn_progress_monitor(
            rx,
            handlers([handler.clone() as Arc<dyn EventHandler>]),
            shutdown.clone(),
            failure_tx,
        );

        // Give it time to send initial
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Drop sender without cancelling shutdown — this is unexpected
        drop(tx);
        task.await.unwrap();

        // At least the initial progress was sent, failure sent via channel
        assert!(handler.progress_count.load(Ordering::SeqCst) >= 1);
        assert_eq!(handler.error_count.load(Ordering::SeqCst), 0);
        let msg = failure_rx.try_recv().expect("should have received failure message");
        assert!(msg.contains("closed unexpectedly"));
    }

    /// Events sent before the shutdown are still delivered, such as the departures
    /// the network announces while it stops.
    #[tokio::test]
    async fn network_event_dispatch() {
        let (tx, rx) = broadcast::channel(64);
        let handler = Arc::new(RecordingHandler::new());
        let shutdown = CancellationToken::new();
        let (failure_tx, _failure_rx) = mpsc::channel(1);

        let task = spawn_broadcast_monitor(
            "network",
            rx,
            handlers([handler.clone() as Arc<dyn EventHandler>]),
            idle_progress(),
            shutdown.clone(),
            failure_tx,
            |h, event: &NetworkEvent| h.on_network_event(event),
        );

        for id in 0..16 {
            let addr = test_socket_address(id);
            tx.send(NetworkEvent::PeerConnected(addr)).unwrap();
            tx.send(NetworkEvent::PeerDisconnected(addr)).unwrap();
        }
        // Before the monitor first runs, so all the events are still queued.
        shutdown.cancel();
        task.await.unwrap();

        assert_eq!(handler.network_count.load(Ordering::SeqCst), 32);
    }

    #[tokio::test]
    async fn broadcast_monitor_dispatches_to_all_handlers() {
        let (tx, rx) = broadcast::channel(16);
        let first = Arc::new(RecordingHandler::new());
        let second = Arc::new(RecordingHandler::new());
        let shutdown = CancellationToken::new();
        let (failure_tx, _failure_rx) = mpsc::channel(1);

        let task = spawn_broadcast_monitor(
            "test",
            rx,
            handlers([
                first.clone() as Arc<dyn EventHandler>,
                second.clone() as Arc<dyn EventHandler>,
            ]),
            idle_progress(),
            shutdown.clone(),
            failure_tx,
            |h, event: &SyncEvent| h.on_sync_event(event),
        );

        tx.send(SyncEvent::BlockHeadersStored {
            tip_height: 1,
        })
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        shutdown.cancel();
        task.await.unwrap();

        assert_eq!(first.sync_count.load(Ordering::SeqCst), 1);
        assert_eq!(second.sync_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn progress_monitor_dispatches_to_all_handlers() {
        let (tx, rx) = watch::channel(SyncProgress::default());
        let first = Arc::new(RecordingHandler::new());
        let second = Arc::new(RecordingHandler::new());
        let shutdown = CancellationToken::new();
        let (failure_tx, _failure_rx) = mpsc::channel(1);

        let task = spawn_progress_monitor(
            rx,
            handlers([
                first.clone() as Arc<dyn EventHandler>,
                second.clone() as Arc<dyn EventHandler>,
            ]),
            shutdown.clone(),
            failure_tx,
        );

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        tx.send_modify(|_| {});
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        shutdown.cancel();
        task.await.unwrap();

        assert!(first.progress_count.load(Ordering::SeqCst) >= 2);
        assert!(second.progress_count.load(Ordering::SeqCst) >= 2);
    }
}
