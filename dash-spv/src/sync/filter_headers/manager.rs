//! Filter headers manager for parallel sync.
//!
//! Downloads compact block filter headers (BIP 157/158). Reacts to BlockHeadersStored
//! events to know when new headers are available. Emits FilterHeadersStored events.

use std::sync::Arc;

use dashcore::network::message_filter::CFHeaders;
use tokio::sync::RwLock;

use super::pipeline::FilterHeadersPipeline;
use crate::error::SyncResult;
use crate::network::RequestSender;
use crate::storage::{BlockHeaderStorage, FilterHeaderStorage};
use crate::sync::filter_headers::util::compute_filter_headers;
use crate::sync::progress::ProgressPercentage;
use crate::sync::{FilterHeadersProgress, HeaderCompletionGate, SyncEvent, SyncManager, SyncState};

/// Filter headers manager for downloading compact block filter headers.
///
/// This manager:
/// - Subscribes to BlockHeadersStored events to know when to start/resume
/// - Downloads filter headers using pipelined requests
/// - Emits FilterHeadersStored events for FiltersManager
///
/// Generic over:
/// - `H: BlockHeaderStorage` for reading block headers
/// - `FH: FilterHeaderStorage` for storing filter headers
pub struct FilterHeadersManager<H: BlockHeaderStorage, FH: FilterHeaderStorage> {
    /// Current progress of the manager.
    pub(super) progress: FilterHeadersProgress,
    /// Block header storage (for reading headers).
    header_storage: Arc<RwLock<H>>,
    /// Filter header storage (for storing filter headers).
    pub(super) filter_header_storage: Arc<RwLock<FH>>,
    /// Pipeline for downloading filter headers.
    pub(super) pipeline: FilterHeadersPipeline,
    /// Checkpoint start height - set when syncing from checkpoint to store prev header once.
    pub(super) checkpoint_start_height: Option<u32>,
    /// Whether block headers have caught up since every peer was last lost.
    /// Gates FilterHeadersSyncComplete so it never fires before they have.
    pub(super) header_gate: HeaderCompletionGate,
}

impl<H: BlockHeaderStorage, FH: FilterHeaderStorage> FilterHeadersManager<H, FH> {
    /// Whether block headers have caught up since every peer was last lost.
    pub(super) fn block_headers_synced(&self) -> bool {
        self.header_gate.synced()
    }

    /// The tip height block-header storage holds right now.
    ///
    /// Distinct from `progress.block_header_tip_height()`, which is only ever
    /// as fresh as the last `BlockHeadersStored` / `BlockHeaderSyncComplete`
    /// this manager was handed. Storage can move without either arriving —
    /// an out-of-order segment completing promotes a run of buffered headers —
    /// so the tick reads storage directly to notice.
    pub(super) async fn stored_block_header_tip(&self) -> Option<u32> {
        self.header_storage.read().await.get_tip_height().await
    }

    /// Transition to `Synced` and return `FilterHeadersSyncComplete` if block headers
    /// are done and filter headers have reached the target. Returns `None` if already
    /// `Synced` or conditions are not met.
    pub(super) fn try_complete_sync(&mut self) -> Option<SyncEvent> {
        if self.block_headers_synced()
            && self.progress.current_height() >= self.progress.target_height()
        {
            if self.state() == SyncState::Synced {
                return None;
            }
            self.set_state(SyncState::Synced);
            tracing::info!(
                "Filter header sync complete at height {}",
                self.progress.current_height()
            );
            return Some(SyncEvent::FilterHeadersSyncComplete {
                tip_height: self.progress.current_height(),
            });
        }
        None
    }

    /// Create a new filter headers manager with the given storage references.
    pub async fn new(
        header_storage: Arc<RwLock<H>>,
        filter_header_storage: Arc<RwLock<FH>>,
    ) -> SyncResult<Self> {
        // Load current filter tip
        let filter_tip =
            filter_header_storage.read().await.get_filter_tip_height().await?.unwrap_or(0);

        // Load block header tip for progress display
        let header_tip =
            header_storage.read().await.get_tip().await.map(|t| t.height()).unwrap_or(0);

        let mut initial_progress = FilterHeadersProgress::default();
        initial_progress.update_current_height(filter_tip);
        initial_progress.update_target_height(header_tip);
        initial_progress.update_block_header_tip_height(header_tip);

        Ok(Self {
            progress: initial_progress,
            header_storage,
            filter_header_storage,
            pipeline: FilterHeadersPipeline::default(),
            checkpoint_start_height: None,
            header_gate: HeaderCompletionGate::default(),
        })
    }

    /// Process a CFHeaders response - store headers and update state.
    pub(super) async fn process_cfheaders(
        &mut self,
        cfheaders: &CFHeaders,
        start_height: u32,
    ) -> SyncResult<u32> {
        let filter_headers = compute_filter_headers(cfheaders);
        let count = filter_headers.len() as u32;

        let mut storage = self.filter_header_storage.write().await;

        // For checkpoint sync, store previous_filter_header at start_height - 1
        // so filter verification can chain correctly. Only on first batch.
        if let Some(checkpoint_height) = self.checkpoint_start_height {
            if start_height == checkpoint_height && start_height > 0 {
                storage
                    .store_filter_headers_at_height(
                        &[cfheaders.previous_filter_header],
                        start_height - 1,
                    )
                    .await?;
                tracing::debug!(
                    "Stored checkpoint previous filter header at height {}",
                    start_height - 1
                );
                // Clear so we don't check again
                self.checkpoint_start_height = None;
            }
        }

        storage.store_filter_headers_at_height(&filter_headers, start_height).await?;

        drop(storage);

        self.progress.add_processed(count);

        Ok(count)
    }

    /// Start or resume filter header download.
    async fn start_download(&mut self, requests: &RequestSender) -> SyncResult<Vec<SyncEvent>> {
        // Get current filter tip
        let filter_headers_tip =
            self.filter_header_storage.read().await.get_filter_tip_height().await?.unwrap_or(0);

        // Get header start height (for checkpoint sync)
        let header_start_height =
            self.header_storage.read().await.get_start_height().await.unwrap_or(0);

        // Calculate start height
        let start_height = match filter_headers_tip {
            0 => header_start_height,
            n => (n + 1).max(header_start_height),
        };

        self.progress.update_current_height(filter_headers_tip);

        // Check if already at target (nothing to download)
        if start_height > self.progress.block_header_tip_height() {
            if let Some(event) = self.try_complete_sync() {
                return Ok(vec![event]);
            }
            return Ok(vec![]);
        }

        tracing::info!(
            "Starting filter header sync from {} to {}",
            start_height,
            self.progress.block_header_tip_height()
        );

        // Track checkpoint start height for storing prev header on first batch.
        // Only needed on fresh checkpoint sync (no existing filter headers).
        // On resume, start_height-1 is already stored so re-inserting would panic in debug builds.
        if start_height > 0 && filter_headers_tip == 0 {
            self.checkpoint_start_height = Some(start_height);
        }

        // Initialize pipeline with storage references
        let header_storage = self.header_storage.read().await;
        self.pipeline
            .init(&*header_storage, start_height, self.progress.block_header_tip_height())
            .await?;
        drop(header_storage);

        // Send initial requests
        self.pipeline.send_pending(requests)?;

        self.set_state(SyncState::Syncing);

        Ok(vec![])
    }

    /// Handle notification that new headers are available.
    ///
    /// Unified handler for both BlockHeaderSyncComplete and BlockHeadersStored events.
    /// Uses pipeline state to determine whether to init or extend.
    pub(super) async fn handle_new_headers(
        &mut self,
        tip_height: u32,
        requests: &RequestSender,
    ) -> SyncResult<Vec<SyncEvent>> {
        self.progress.update_block_header_tip_height(tip_height);
        self.update_target_height(tip_height);

        // Nothing to do if caught up to available headers
        if self.progress.current_height() >= self.progress.block_header_tip_height() {
            let mut events = Vec::new();
            if let Some(event) = self.try_complete_sync() {
                events.push(event);
            }
            return Ok(events);
        }

        match self.state() {
            SyncState::Synced | SyncState::Syncing => {
                // Configure pipeline based on its current state
                let header_storage = self.header_storage.read().await;
                if self.pipeline.is_complete() {
                    // Pipeline done/empty, need fresh init
                    self.pipeline
                        .init(
                            &*header_storage,
                            self.progress.current_height() + 1,
                            self.progress.block_header_tip_height(),
                        )
                        .await?;
                } else {
                    // Pipeline active, extend it
                    self.pipeline
                        .extend_target(&*header_storage, self.progress.block_header_tip_height())
                        .await?;
                }
                drop(header_storage);
                self.pipeline.send_pending(requests)?;
                Ok(vec![])
            }
            SyncState::WaitingForConnections | SyncState::WaitForEvents => {
                // Need full startup (calculates start from storage, handles checkpoints)
                self.start_download(requests).await
            }
            _ => Ok(vec![]),
        }
    }
}

impl<H: BlockHeaderStorage, FH: FilterHeaderStorage> std::fmt::Debug
    for FilterHeadersManager<H, FH>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilterHeadersManager").field("progress", &self.progress).finish()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::{MessageType, NetworkEvent, NetworkRequest, RequestSender};
    use crate::storage::{
        DiskStorageManager, PersistentBlockHeaderStorage, PersistentFilterHeaderStorage,
        StorageManager,
    };
    use crate::sync::{ManagerIdentifier, SyncManagerProgress};
    use crate::types::HashedBlockHeader;
    use dashcore::network::message::NetworkMessage;
    use dashcore::{block::Version, BlockHash, CompactTarget, Header as BlockHeader};
    use dashcore_hashes::Hash;

    type TestFilterHeadersManager =
        FilterHeadersManager<PersistentBlockHeaderStorage, PersistentFilterHeaderStorage>;
    type TestSyncManager = dyn SyncManager;

    async fn create_test_manager() -> TestFilterHeadersManager {
        let storage = DiskStorageManager::with_temp_dir().await.unwrap();
        FilterHeadersManager::new(storage.block_headers(), storage.filter_headers())
            .await
            .expect("Failed to create FilterHeadersManager")
    }

    fn create_test_request_sender(
    ) -> (RequestSender, tokio::sync::mpsc::UnboundedReceiver<crate::network::NetworkRequest>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (RequestSender::new(tx), rx)
    }

    #[tokio::test]
    async fn test_filter_headers_manager_new() {
        let manager = create_test_manager().await;
        assert_eq!(manager.identifier(), ManagerIdentifier::FilterHeader);
        assert_eq!(manager.state(), SyncState::WaitForEvents);
        assert_eq!(manager.wanted_message_types(), vec![MessageType::CFHeaders]);
        assert!(!manager.block_headers_synced());
    }

    #[tokio::test]
    async fn test_filter_headers_manager_progress() {
        let mut manager = create_test_manager().await;
        manager.progress.update_current_height(500);
        manager.progress.update_target_height(2000);
        manager.progress.update_block_header_tip_height(1000);
        manager.progress.add_processed(500);

        let manager_ref: &TestSyncManager = &manager;
        let progress = manager_ref.progress();
        if let SyncManagerProgress::FilterHeaders(progress) = progress {
            assert_eq!(progress.state(), SyncState::WaitForEvents);
            assert_eq!(progress.current_height(), 500);
            assert_eq!(progress.target_height(), 2000);
            assert_eq!(progress.block_header_tip_height(), 1000);
            assert_eq!(progress.processed(), 500);
            assert!(progress.last_activity().elapsed().as_secs() < 1);
        } else {
            panic!("Expected SyncManagerProgress::FilterHeaders");
        }
    }

    #[tokio::test]
    async fn test_try_complete_sync() {
        let mut manager = create_test_manager().await;
        manager.progress.update_current_height(1000);
        manager.progress.update_target_height(1000);
        manager.progress.update_block_header_tip_height(1000);
        manager.set_state(SyncState::Syncing);

        // Gated: returns None when block headers have not caught up
        assert!(manager.try_complete_sync().is_none());
        assert_eq!(manager.state(), SyncState::Syncing);

        // Emits once they have
        manager.header_gate.record(0);
        assert!(matches!(
            manager.try_complete_sync(),
            Some(SyncEvent::FilterHeadersSyncComplete { .. })
        ));
        assert_eq!(manager.state(), SyncState::Synced);

        // Idempotent: returns None when already Synced
        assert!(manager.try_complete_sync().is_none());
        assert_eq!(manager.state(), SyncState::Synced);
    }

    #[tokio::test]
    async fn test_block_headers_synced_event_gating() {
        let mut manager = create_test_manager().await;
        let (sender, _rx) = create_test_request_sender();

        // Filter headers caught up to block header tip and target
        manager.progress.update_current_height(1000);
        manager.progress.update_target_height(1000);
        manager.progress.update_block_header_tip_height(1000);
        manager.set_state(SyncState::WaitForEvents);

        // BlockHeadersStored does NOT mark block headers synced, no completion emitted
        let event = SyncEvent::BlockHeadersStored {
            tip_height: 1000,
        };
        let events = manager.handle_sync_event(&event, &sender).await.unwrap();
        assert!(!manager.block_headers_synced());
        assert!(!events.iter().any(|e| matches!(e, SyncEvent::FilterHeadersSyncComplete { .. })));

        // BlockHeaderSyncComplete sets the flag and emits completion
        let event = SyncEvent::BlockHeaderSyncComplete {
            tip_height: 1000,
            connection_epoch: 0,
        };
        let events = manager.handle_sync_event(&event, &sender).await.unwrap();
        assert!(manager.block_headers_synced());
        assert!(events.iter().any(|e| matches!(e, SyncEvent::FilterHeadersSyncComplete { .. })));
        assert_eq!(manager.state(), SyncState::Synced);
    }

    #[tokio::test]
    async fn test_block_header_sync_complete_during_active_download() {
        let mut manager = create_test_manager().await;
        let (sender, _rx) = create_test_request_sender();

        // Filter headers caught up to block tip, but target is higher (more headers coming)
        manager.progress.update_current_height(1000);
        manager.progress.update_target_height(2000);
        manager.progress.update_block_header_tip_height(1000);
        manager.set_state(SyncState::WaitForEvents);

        // BlockHeaderSyncComplete arrives but target not reached yet
        let event = SyncEvent::BlockHeaderSyncComplete {
            tip_height: 1000,
            connection_epoch: 0,
        };
        let events = manager.handle_sync_event(&event, &sender).await.unwrap();

        assert!(manager.block_headers_synced());
        assert!(!events.iter().any(|e| matches!(e, SyncEvent::FilterHeadersSyncComplete { .. })));
    }

    /// The tick must notice block-header storage advancing on its own.
    ///
    /// `handle_new_headers` — the only path that extends the CFHeaders queue —
    /// runs off `BlockHeadersStored` / `BlockHeaderSyncComplete`. Storage can
    /// move without either arriving, because a segment completing out of order
    /// promotes a run of buffered headers. When that happened on a mainnet
    /// restore the queue stayed at its old target and filter headers stopped
    /// permanently, while block headers and ChainLocks carried on.
    #[tokio::test]
    async fn test_tick_extends_when_storage_tip_advanced_without_an_event() {
        let storage = DiskStorageManager::with_temp_dir().await.unwrap();
        let header_storage = storage.block_headers();
        let mut manager =
            FilterHeadersManager::new(header_storage.clone(), storage.filter_headers())
                .await
                .expect("Failed to create FilterHeadersManager");
        let (sender, mut rx) = create_test_request_sender();

        // Mid-sync: this manager was last told the tip was 1000.
        manager.progress.update_current_height(1000);
        manager.progress.update_target_height(1000);
        manager.progress.update_block_header_tip_height(1000);
        manager.set_state(SyncState::Syncing);

        // Storage moves past that on its own — no event is delivered.
        let mut headers = Vec::new();
        let mut prev = BlockHash::from_byte_array([0u8; 32]);
        for nonce in 0..1200u32 {
            let header = HashedBlockHeader::from(BlockHeader {
                version: Version::from_consensus(1),
                prev_blockhash: prev,
                merkle_root: dashcore::TxMerkleNode::all_zeros(),
                time: 0,
                bits: CompactTarget::from_consensus(0x2100ffff),
                nonce,
            });
            prev = *header.hash();
            headers.push(header);
        }
        header_storage.write().await.store_headers_at_height(&headers, 0).await.unwrap();
        let stored_tip = manager.stored_block_header_tip().await.expect("storage has a tip");
        assert!(stored_tip > 1000, "test setup: storage tip must exceed the known tip");

        let manager_ref: &mut TestSyncManager = &mut manager;
        manager_ref.tick(&sender).await.unwrap();

        assert_eq!(
            manager.progress.block_header_tip_height(),
            stored_tip,
            "tick must pick up a tip that advanced without an event"
        );

        // The tip alone is not the fix. `handle_new_headers` updates that
        // field before it touches the pipeline, so a version that noticed the
        // advance and then failed to queue anything would still satisfy the
        // assertion above — and would leave sync exactly as stuck as before.
        // What has to be true is that a request for the new range went out.
        let mut requested_stops = Vec::new();
        while let Ok(request) = rx.try_recv() {
            match request {
                NetworkRequest::SendMessage(NetworkMessage::GetCFHeaders(get)) => {
                    requested_stops.push(get.stop_hash);
                }
                NetworkRequest::SendMessageToPeer(NetworkMessage::GetCFHeaders(get), _) => {
                    requested_stops.push(get.stop_hash);
                }
                _ => {}
            }
        }
        assert!(
            !requested_stops.is_empty(),
            "tick must queue and send CFHeaders requests for the newly available range"
        );
        // Every stop hash must be a header the new range actually contains, so
        // a request built from the stale target cannot pass this.
        let storage = header_storage.read().await;
        for stop in &requested_stops {
            let mut found = false;
            for height in 1001..=stored_tip {
                if let Ok(Some(header)) = storage.get_header(height).await {
                    if header.hash() == stop {
                        found = true;
                        break;
                    }
                }
            }
            assert!(found, "CFHeaders stop hash {stop} is not in the newly discovered range");
        }
    }

    #[tokio::test]
    async fn test_on_disconnect() {
        let mut manager = create_test_manager().await;

        // Set all fields that on_disconnect resets
        manager.header_gate.record(0);
        manager.checkpoint_start_height = Some(500);

        manager.on_disconnect();

        assert!(!manager.block_headers_synced());
        assert!(manager.checkpoint_start_height.is_none());
        assert!(manager.pipeline.is_complete());
    }

    fn peers_updated(connected_count: usize) -> NetworkEvent {
        let addresses: Vec<std::net::SocketAddr> =
            vec!["127.0.0.1:19999".parse().unwrap(); connected_count];
        NetworkEvent::PeersUpdated {
            connected_count,
            addresses,
            best_height: (connected_count > 0).then_some(40000),
        }
    }

    fn headers_complete(connection_epoch: u64) -> SyncEvent {
        SyncEvent::BlockHeaderSyncComplete {
            tip_height: 40000,
            connection_epoch,
        }
    }

    /// A manager whose filter headers have caught up with 40000 block headers,
    /// connected, waiting only for block headers to report completion.
    async fn create_caught_up_manager(sender: &RequestSender) -> TestFilterHeadersManager {
        let mut manager = create_test_manager().await;
        manager.progress.update_current_height(40000);
        manager.progress.update_target_height(40000);
        manager.progress.update_block_header_tip_height(40000);
        let manager_ref: &mut TestSyncManager = &mut manager;
        manager_ref.handle_network_event(&peers_updated(1), sender).await.unwrap();
        assert_eq!(manager.state(), SyncState::WaitForEvents);
        manager
    }

    fn completes(events: &[SyncEvent]) -> bool {
        events.iter().any(|e| matches!(e, SyncEvent::FilterHeadersSyncComplete { .. }))
    }

    /// Every peer drops; the block-header task handles the disconnect,
    /// reconnects, catches up and announces it, and this task handles that
    /// announcement before the disconnect it followed (sync and network events
    /// reach a task on separate channels). The stale disconnect must not cost
    /// the completion: there is no second announcement, so filter headers
    /// would wait in `WaitForEvents` for good and sync would never complete.
    #[tokio::test]
    async fn test_header_completion_handled_before_its_disconnect_is_kept() {
        let (sender, _rx) = create_test_request_sender();
        let mut manager = create_caught_up_manager(&sender).await;
        let manager_ref: &mut TestSyncManager = &mut manager;

        let events = manager_ref.handle_sync_event(&headers_complete(1), &sender).await.unwrap();
        assert!(completes(&events));

        manager_ref.handle_network_event(&peers_updated(0), &sender).await.unwrap();
        let events = manager_ref.handle_network_event(&peers_updated(1), &sender).await.unwrap();

        assert_eq!(manager.state(), SyncState::Synced);
        assert!(completes(&events), "the reconnect must complete again: {events:?}");
    }

    /// The other order, and the gate from #631 it must keep: after a
    /// disconnect, a completion from before it, including a late copy, does
    /// not count; the next one does.
    #[tokio::test]
    async fn test_header_completion_from_before_a_disconnect_does_not_count() {
        let (sender, _rx) = create_test_request_sender();
        let mut manager = create_caught_up_manager(&sender).await;
        let manager_ref: &mut TestSyncManager = &mut manager;

        let events = manager_ref.handle_sync_event(&headers_complete(0), &sender).await.unwrap();
        assert!(completes(&events));

        manager_ref.handle_network_event(&peers_updated(0), &sender).await.unwrap();
        let events = manager_ref.handle_network_event(&peers_updated(1), &sender).await.unwrap();
        assert!(!completes(&events));
        assert_eq!(manager_ref.state(), SyncState::WaitForEvents);

        let events = manager_ref.handle_sync_event(&headers_complete(0), &sender).await.unwrap();
        assert!(!completes(&events), "a completion from before the disconnect: {events:?}");
        assert_eq!(manager_ref.state(), SyncState::WaitForEvents);

        let events = manager_ref.handle_sync_event(&headers_complete(1), &sender).await.unwrap();
        assert!(completes(&events));
        assert_eq!(manager.state(), SyncState::Synced);
    }

    fn requested_cfheaders(rx: &mut tokio::sync::mpsc::UnboundedReceiver<NetworkRequest>) -> usize {
        let mut count = 0;
        while let Ok(request) = rx.try_recv() {
            if matches!(
                request,
                NetworkRequest::SendMessage(NetworkMessage::GetCFHeaders(_))
                    | NetworkRequest::SendMessageToPeer(NetworkMessage::GetCFHeaders(_), _)
            ) {
                count += 1;
            }
        }
        count
    }

    /// The reverse order again, with filter headers behind block headers. Then
    /// the reconnect has to resume the download, not just check for completion:
    /// nothing else would send a GetCFHeaders, since the block-header tip it
    /// knows is already the stored one.
    #[tokio::test]
    async fn test_header_completion_handled_before_its_disconnect_resumes_a_download() {
        let tip = 1200;
        let storage = DiskStorageManager::with_temp_dir().await.unwrap();
        storage
            .block_headers()
            .write()
            .await
            .store_headers(
                &BlockHeader::dummy_batch(0..tip + 1)
                    .into_iter()
                    .map(HashedBlockHeader::from)
                    .collect::<Vec<_>>(),
            )
            .await
            .unwrap();
        let mut manager =
            FilterHeadersManager::new(storage.block_headers(), storage.filter_headers())
                .await
                .expect("Failed to create FilterHeadersManager");
        let (sender, mut rx) = create_test_request_sender();
        let manager_ref: &mut TestSyncManager = &mut manager;
        manager_ref.handle_network_event(&peers_updated(1), &sender).await.unwrap();
        assert_eq!(manager_ref.state(), SyncState::WaitForEvents);

        let complete = SyncEvent::BlockHeaderSyncComplete {
            tip_height: tip,
            connection_epoch: 1,
        };
        manager_ref.handle_sync_event(&complete, &sender).await.unwrap();
        assert!(requested_cfheaders(&mut rx) > 0, "test setup: the download starts");

        manager_ref.handle_network_event(&peers_updated(0), &sender).await.unwrap();
        manager_ref.handle_network_event(&peers_updated(1), &sender).await.unwrap();

        assert_eq!(manager_ref.state(), SyncState::Syncing);
        assert!(requested_cfheaders(&mut rx) > 0, "the reconnect must resume the download");
    }

    /// A chain of 4001 block headers with filter headers for every height, and
    /// a filter-header manager over it with nothing stored yet.
    struct Replay {
        manager: TestFilterHeadersManager,
        headers: Vec<HashedBlockHeader>,
        chain: Vec<dashcore::hash_types::FilterHeader>,
        sender: RequestSender,
        _rx: tokio::sync::mpsc::UnboundedReceiver<NetworkRequest>,
    }

    impl Replay {
        const TIP: u32 = 4000;

        async fn new() -> Self {
            use dashcore::hash_types::FilterHeader;
            use dashcore::network::message_filter::CFHeaders;

            let storage = DiskStorageManager::with_temp_dir().await.unwrap();
            let header_storage = storage.block_headers();
            let mut headers = Vec::new();
            let mut prev = BlockHash::from_byte_array([0u8; 32]);
            for nonce in 0..=Self::TIP {
                let header = HashedBlockHeader::from(BlockHeader {
                    version: Version::from_consensus(1),
                    prev_blockhash: prev,
                    merkle_root: dashcore::TxMerkleNode::all_zeros(),
                    time: 0,
                    bits: CompactTarget::from_consensus(0x2100ffff),
                    nonce,
                });
                prev = *header.hash();
                headers.push(header);
            }
            header_storage.write().await.store_headers_at_height(&headers, 0).await.unwrap();
            let manager =
                FilterHeadersManager::new(header_storage.clone(), storage.filter_headers())
                    .await
                    .expect("Failed to create FilterHeadersManager");
            let chain = compute_filter_headers(&CFHeaders {
                filter_type: 0,
                stop_hash: *headers[Self::TIP as usize].hash(),
                previous_filter_header: FilterHeader::all_zeros(),
                filter_hashes: (0..=Self::TIP).map(Self::filter_hash).collect(),
            });
            let (sender, _rx) = create_test_request_sender();
            Self {
                manager,
                headers,
                chain,
                sender,
                _rx,
            }
        }

        fn filter_hash(height: u32) -> dashcore::hash_types::FilterHash {
            dashcore::hash_types::FilterHash::hash(&height.to_le_bytes())
        }

        /// The peer's reply to a request for `range`.
        fn answer(&self, range: std::ops::RangeInclusive<u32>) -> crate::network::Message {
            use dashcore::hash_types::FilterHeader;
            use dashcore::network::message_filter::CFHeaders;

            let previous_filter_header = match *range.start() {
                0 => FilterHeader::all_zeros(),
                h => self.chain[h as usize - 1],
            };
            crate::network::Message::new(
                "127.0.0.1:19999".parse().unwrap(),
                NetworkMessage::CFHeaders(CFHeaders {
                    filter_type: 0,
                    stop_hash: *self.headers[*range.end() as usize].hash(),
                    previous_filter_header,
                    filter_hashes: range.map(Self::filter_hash).collect(),
                }),
            )
        }

        /// Block headers reach each tip in turn while connected.
        async fn headers_reach(&mut self, tips: &[u32]) {
            for tip in tips {
                self.manager.handle_new_headers(*tip, &self.sender).await.unwrap();
            }
        }

        /// Every peer drops; the next connection rebuilds the queue from the
        /// filter tip up to `TIP`.
        async fn reconnect(&mut self) {
            self.manager.on_disconnect();
            self.manager.set_state(SyncState::WaitForEvents);
            self.manager.handle_new_headers(Self::TIP, &self.sender).await.unwrap();
        }

        /// Feed replies for these ranges, in order, returning the events.
        async fn receive(
            &mut self,
            ranges: impl IntoIterator<Item = std::ops::RangeInclusive<u32>>,
        ) -> Vec<SyncEvent> {
            let mut events = Vec::new();
            for range in ranges {
                let message = self.answer(range);
                let manager: &mut TestSyncManager = &mut self.manager;
                events.extend(manager.handle_message(message, &self.sender).await.unwrap());
            }
            events
        }

        /// Filter headers end at `TIP` and match the chain at `heights`, and no
        /// event reported a range the chain does not have.
        async fn assert_stored(&self, events: &[SyncEvent], heights: &[u32]) {
            for event in events {
                if let SyncEvent::FilterHeadersStored {
                    end_height,
                    ..
                } = event
                {
                    assert!(*end_height <= Self::TIP, "stored past the chain: {event:?}");
                }
            }
            let storage = self.manager.filter_header_storage.read().await;
            assert_eq!(storage.get_filter_tip_height().await.unwrap(), Some(Self::TIP));
            for &height in heights {
                assert_eq!(
                    storage.get_filter_header(height).await.unwrap(),
                    Some(self.chain[height as usize]),
                    "filter header at {height}"
                );
            }
        }
    }

    /// The disconnect sequence that broke the dashd peer-disconnection test,
    /// replayed: the queue is extended to 4000 in 2000-height steps from 2001,
    /// so the batch 2001..=4000 has stop hash hash(4000) and is in flight when
    /// the peer drops. The rebuilt queue runs 2000..=3999 then the single
    /// height 4000, whose stop hash is the same. When the old 2000-header
    /// answer arrives it must not land at 4000..=5999; filter headers end at
    /// 4000 and match the chain.
    #[tokio::test]
    async fn test_stale_cfheaders_after_a_reset_do_not_land_at_the_wrong_heights() {
        let mut replay = Replay::new().await;

        // First connection: tip 2000 queues 0..=1999 and 2000..=2000, the
        // tip moving to 4000 extends with 2001..=4000.
        replay.manager.set_state(SyncState::WaitForEvents);
        replay.headers_reach(&[2000, 4000]).await;
        replay.receive(std::iter::once(0..=1999)).await;

        // The peer drops with 2001..=4000 in flight.
        replay.reconnect().await;

        let events = replay.receive([2001..=4000, 2000..=3999, 4000..=4000]).await;
        replay.assert_stored(&events, &[2000, 3999, 4000]).await;
    }

    /// The mirror case: the stale answer is shorter than the rebuilt batch
    /// that shares its stop hash. The old queue has 3001..=4000 in flight;
    /// the rebuilt one asks for 2001..=4000 in one batch. The old 1000-header
    /// answer must not land at 2001..=3000, which would also leave the next
    /// expected height off the batch starts so the real answer is dropped.
    #[tokio::test]
    async fn test_short_stale_cfheaders_after_a_reset_do_not_land_at_the_wrong_heights() {
        let mut replay = Replay::new().await;

        // First connection: tip 2000 queues 0..=1999 and 2000..=2000, then
        // 2001..=3000 and 3001..=4000 as the tip moves on.
        replay.manager.set_state(SyncState::WaitForEvents);
        replay.headers_reach(&[2000, 3000, 4000]).await;
        replay.receive([0..=1999, 2000..=2000]).await;

        // The peer drops with 2001..=3000 and 3001..=4000 in flight.
        replay.reconnect().await;

        let events = replay.receive([3001..=4000, 2001..=4000]).await;
        replay.assert_stored(&events, &[2001, 3000, 3001, 4000]).await;
    }
}
