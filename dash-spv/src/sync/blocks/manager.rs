//! Blocks manager for parallel sync.
//!
//! Downloads blocks that matched wallet filters and processes them in height order.
//! Subscribes to BlockNeeded events and emits BlockProcessed events.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tokio::sync::RwLock;

use super::pipeline::BlocksPipeline;
use crate::error::SyncResult;
use crate::network::NetworkManager;
use crate::storage::{BlockHeaderStorage, BlockStorage};
use crate::sync::{BlocksProgress, SyncEvent, SyncManager, SyncState};
use crate::types::HashedBlock;
use key_wallet_manager::{BlockProcessingResult, WalletId, WalletInterface};

/// Blocks manager for downloading and processing matching blocks.
///
/// This manager:
/// - Subscribes to BlockNeeded events from FiltersManager
/// - Downloads blocks using pipelined requests
/// - Processes blocks through wallet in height order
/// - Emits BlockProcessed events
///
/// Generic over:
/// - `H: BlockHeaderStorage` for height lookups
/// - `B: BlockStorage` for storing and loading blocks
/// - `W: WalletInterface` for wallet operations
pub struct BlocksManager<H: BlockHeaderStorage, B: BlockStorage, W: WalletInterface> {
    /// Current progress of the manager.
    pub(super) progress: BlocksProgress,
    /// Block header storage (for height lookups).
    pub(super) header_storage: Arc<RwLock<H>>,
    /// Block storage (for storing and loading blocks).
    pub(super) block_storage: Arc<RwLock<B>>,
    /// Wallet for processing blocks.
    pub(super) wallet: Arc<RwLock<W>>,
    /// Pipeline for downloading blocks (handles buffering and height ordering).
    pub(super) pipeline: BlocksPipeline,
    /// Whether FiltersSyncComplete has been received.
    pub(super) filters_sync_complete: bool,
}

impl<H: BlockHeaderStorage, B: BlockStorage, W: WalletInterface> BlocksManager<H, B, W> {
    /// Create a new blocks manager with the given storage references.
    pub async fn new(
        wallet: Arc<RwLock<W>>,
        header_storage: Arc<RwLock<H>>,
        block_storage: Arc<RwLock<B>>,
    ) -> Self {
        let last_processed_height = wallet.read().await.last_processed_height();

        let mut initial_progress = BlocksProgress::default();
        initial_progress.update_last_processed(last_processed_height);

        Self {
            progress: initial_progress,
            header_storage,
            block_storage,
            wallet,
            pipeline: BlocksPipeline::new(),
            filters_sync_complete: false,
        }
    }

    pub(super) async fn send_pending(
        &mut self,
        network: &Arc<dyn NetworkManager>,
    ) -> SyncResult<()> {
        self.pipeline.send_pending(network).await?;
        Ok(())
    }

    /// Process buffered blocks in height order.
    ///
    /// Uses the pipeline's height-ordering logic to ensure blocks are processed
    /// in the correct sequence.
    pub(super) async fn process_buffered_blocks(&mut self) -> SyncResult<Vec<SyncEvent>> {
        let mut events = Vec::new();

        // Process blocks in height order using pipeline's ordering logic
        while let Some((block, height, interested)) = self.pipeline.take_next_ordered_block() {
            let hash = *block.hash();

            // Process the block only for the wallets whose filter matched it.
            // Already-synced wallets that did not match are not touched.
            let result = self.apply_block(&block, height, &interested).await;

            self.progress.add_processed(1);
            if result.relevant_tx_count() > 0 {
                self.progress.add_relevant(1);
            }
            // Collect confirmed txids before moving new_scripts out of result
            let confirmed_txids: Vec<_> = result.relevant_txids().cloned().collect();
            self.progress.update_last_processed(height);

            events.push(SyncEvent::BlockProcessed {
                block_hash: hash,
                height,
                wallets: interested,
                new_scripts: result.new_scripts,
                confirmed_txids,
            });

            self.reapply_blocks(result.reapply_heights).await;
        }

        // Check if pipeline is empty
        if self.pipeline.is_complete() && self.state() == SyncState::Syncing {
            if self.filters_sync_complete {
                // Filters are done and pipeline is empty - we're fully synced
                self.progress.set_state(SyncState::Synced);
                tracing::info!(
                    "Block sync complete, processed {} blocks",
                    self.progress.processed()
                );
            } else {
                // Pipeline empty but filters still syncing - wait for more blocks
                self.progress.set_state(SyncState::WaitForEvents);
            }
        }

        Ok(events)
    }

    async fn apply_block(
        &mut self,
        block: &HashedBlock,
        height: u32,
        wallets: &BTreeSet<WalletId>,
    ) -> BlockProcessingResult {
        let hash = *block.hash();
        let mut wallet = self.wallet.write().await;
        let result = wallet.process_block_for_wallets(block.block(), hash, height, wallets).await;
        drop(wallet);

        let total_relevant = result.relevant_tx_count();
        let new_scripts_total: usize = result.new_scripts.values().map(|v| v.len()).sum();
        if total_relevant > 0 {
            tracing::info!(
                "Found {} relevant transactions ({} new, {} existing) {} at height {}, new scripts: {}",
                total_relevant,
                result.new_txids.len(),
                result.existing_txids.len(),
                hash,
                height,
                new_scripts_total
            );
        }
        if new_scripts_total > 0 {
            tracing::debug!(
                "Block {} generated {} new scripts for gap limit maintenance across {} wallets",
                height,
                new_scripts_total,
                result.new_scripts.len()
            );
        }

        // Only count new transactions to avoid double-counting during rescans
        self.progress.add_transactions(result.new_txids.len() as u32);
        result
    }

    async fn reapply_blocks(&mut self, reapply: BTreeMap<WalletId, BTreeSet<u32>>) {
        let mut queue: BTreeSet<(u32, WalletId)> = reapply
            .into_iter()
            .flat_map(|(wallet_id, heights)| heights.into_iter().map(move |h| (h, wallet_id)))
            .collect();
        while let Some((height, wallet_id)) = queue.pop_first() {
            let block = match self.block_storage.read().await.load_block(height).await {
                Ok(Some(block)) => block,
                Ok(None) => {
                    tracing::warn!("Cannot re-apply block at height {}: not in storage", height);
                    continue;
                }
                Err(e) => {
                    tracing::warn!("Cannot re-apply block at height {}: {}", height, e);
                    continue;
                }
            };
            let result = self.apply_block(&block, height, &BTreeSet::from([wallet_id])).await;
            queue.extend(
                result.reapply_heights.into_iter().flat_map(|(wallet_id, heights)| {
                    heights.into_iter().map(move |h| (h, wallet_id))
                }),
            );
        }
    }
}

impl<H: BlockHeaderStorage, B: BlockStorage, W: WalletInterface> std::fmt::Debug
    for BlocksManager<H, B, W>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlocksManager")
            .field("progress", &self.progress)
            .field("pipeline", &self.pipeline)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::ClientConfig;
    use crate::network::{MessageType, PeerNetworkManager};
    use crate::storage::{
        DiskStorageManager, PersistentBlockHeaderStorage, PersistentBlockStorage, StorageManager,
    };
    use crate::sync::blocks::pipeline::MAX_DECLARED_AHEAD;
    use crate::sync::{ManagerIdentifier, SyncEvent, SyncManagerProgress};
    use crate::test_utils::{test_socket_address, MockNetworkManager};
    use crate::types::HashedBlock;
    use crate::types::HashedBlockHeader;
    use dashcore::consensus::encode::{deserialize_partial, serialize};
    use dashcore::network::address::Address as PeerAddress;
    use dashcore::network::constants::{ServiceFlags, PROTOCOL_VERSION};
    use dashcore::network::message::{NetworkMessage, RawNetworkMessage};
    use dashcore::network::message_blockdata::Inventory;
    use dashcore::network::message_network::VersionMessage;
    use dashcore::Block;
    use key_wallet_manager::FilterMatchKey;
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::tcp::OwnedWriteHalf;
    use tokio::net::TcpListener;

    use key_wallet_manager::test_utils::{MockWallet, MOCK_WALLET_ID};
    use std::collections::BTreeSet;

    type TestBlocksManager =
        BlocksManager<PersistentBlockHeaderStorage, PersistentBlockStorage, MockWallet>;
    type TestSyncManager = dyn SyncManager;

    async fn create_test_manager() -> TestBlocksManager {
        let storage = DiskStorageManager::with_temp_dir().await.unwrap();
        let wallet = Arc::new(RwLock::new(MockWallet::new()));
        BlocksManager::new(wallet, storage.block_headers(), storage.blocks()).await
    }

    #[tokio::test]
    async fn test_blocks_manager_new() {
        let manager = create_test_manager().await;
        assert_eq!(manager.identifier(), ManagerIdentifier::Block);
        assert_eq!(manager.state(), SyncState::WaitForEvents);
        assert_eq!(manager.wanted_message_types(), [MessageType::Block]);
    }

    #[tokio::test]
    async fn test_blocks_manager_progress() {
        let mut manager = create_test_manager().await;
        manager.progress.update_last_processed(500);
        manager.progress.add_processed(10);

        let manager_ref: &TestSyncManager = &manager;
        let progress = manager_ref.progress();
        if let SyncManagerProgress::Blocks(blocks_progress) = progress {
            assert_eq!(blocks_progress.last_processed(), 500);
            assert_eq!(blocks_progress.processed(), 10);
        } else {
            panic!("Expected SyncManagerProgress::Blocks");
        }
    }

    #[tokio::test]
    async fn test_blocks_manager_handle_blocks_needed_event() {
        use crate::network::NetworkManager;
        use crate::test_utils::MockNetworkManager;
        use key_wallet_manager::FilterMatchKey;
        use std::collections::BTreeMap;

        let mut manager = create_test_manager().await;
        manager.progress.set_state(SyncState::Synced);

        let mock = Arc::new(MockNetworkManager::new());
        let network: Arc<dyn NetworkManager> = mock.clone();

        let block_hash = dashcore::block::Header::dummy(0).block_hash();
        let mut blocks = BTreeMap::new();
        blocks.insert(FilterMatchKey::new(100, block_hash), BTreeSet::from([MOCK_WALLET_ID]));
        let event = SyncEvent::BlocksNeeded {
            blocks,
        };

        let events = manager.handle_sync_event(&event, &network).await.unwrap();

        // Should queue the block
        assert_eq!(manager.state(), SyncState::Syncing);
        assert!(events.is_empty());

        assert!(
            !mock.sent_messages().await.is_empty(),
            "expected a getdata to be declared for the needed block"
        );
    }

    /// `process_buffered_blocks` must call `process_block_for_wallets` with
    /// the exact wallet set carried in the pipeline so already-synced
    /// wallets are not touched by routing logic.
    #[tokio::test]
    async fn test_process_buffered_blocks_routes_wallet_set() {
        use dashcore::block::Header;
        use dashcore::{Block, TxMerkleNode};
        use dashcore_hashes::Hash;

        let mut manager = create_test_manager().await;
        manager.progress.set_state(SyncState::Syncing);

        let header = Header {
            version: dashcore::blockdata::block::Version::from_consensus(1),
            prev_blockhash: dashcore::BlockHash::all_zeros(),
            merkle_root: TxMerkleNode::all_zeros(),
            time: 0,
            bits: dashcore::CompactTarget::from_consensus(0),
            nonce: 0,
        };
        let block = Block {
            header,
            txdata: vec![],
        };
        manager.pipeline.add_from_storage(
            HashedBlock::from(&block),
            100,
            BTreeSet::from([MOCK_WALLET_ID]),
        );

        let events = manager.process_buffered_blocks().await.unwrap();
        assert!(matches!(events.first(), Some(SyncEvent::BlockProcessed { .. })));

        // MOCK_WALLET_ID was in the routed set, so MockWallet recorded the
        // block. (MockWallet::process_block_for_wallets returns early when
        // its id is absent.)
        let processed = manager.wallet.read().await.processed_blocks();
        let processed = processed.lock().await;
        assert_eq!(processed.len(), 1);
        assert_eq!(processed[0].1, 100);
    }

    #[tokio::test]
    async fn test_process_buffered_blocks_reapplies_requested_stored_block() {
        let storage = DiskStorageManager::with_temp_dir().await.unwrap();
        let mut wallet = MockWallet::new();
        wallet.set_reapply_heights(100, BTreeSet::from([200]));
        let wallet = Arc::new(RwLock::new(wallet));
        let mut manager: TestBlocksManager =
            BlocksManager::new(wallet.clone(), storage.block_headers(), storage.blocks()).await;
        manager.progress.set_state(SyncState::Syncing);

        manager
            .block_storage
            .write()
            .await
            .store_block(200, HashedBlock::dummy(200, vec![]))
            .await
            .unwrap();
        manager.pipeline.add_from_storage(
            HashedBlock::dummy(100, vec![]),
            100,
            BTreeSet::from([MOCK_WALLET_ID]),
        );

        let events = manager.process_buffered_blocks().await.unwrap();
        assert_eq!(
            events.iter().filter(|e| matches!(e, SyncEvent::BlockProcessed { .. })).count(),
            1
        );

        let processed = wallet.read().await.processed_blocks();
        let heights: Vec<u32> = processed.lock().await.iter().map(|(_, h)| *h).collect();
        assert_eq!(heights, vec![100, 200]);
        assert_eq!(manager.progress.processed(), 1);
    }

    /// A wallet that is NOT in the pipeline's interested set must not be
    /// routed the block. Two wallets are registered, but only `wallet_in`
    /// appears in the routed set; the other wallet's processed log must
    /// stay empty for that block.
    #[tokio::test]
    async fn test_process_buffered_blocks_excludes_uninterested_wallet() {
        use dashcore::block::Header;
        use dashcore::{Block, TxMerkleNode};
        use dashcore_hashes::Hash;
        use key_wallet_manager::test_utils::{MockWalletState, MultiMockWallet};
        use key_wallet_manager::WalletId;

        let storage = DiskStorageManager::with_temp_dir().await.unwrap();
        let multi = MultiMockWallet::new();
        let wallet_in: WalletId = [0xAA; 32];
        let wallet_out: WalletId = [0xBB; 32];
        let multi = Arc::new(RwLock::new(multi));
        {
            let mut w = multi.write().await;
            w.insert_wallet(wallet_in, MockWalletState::default());
            w.insert_wallet(wallet_out, MockWalletState::default());
        }
        let mut manager: BlocksManager<
            PersistentBlockHeaderStorage,
            PersistentBlockStorage,
            MultiMockWallet,
        > = BlocksManager::new(multi.clone(), storage.block_headers(), storage.blocks()).await;
        manager.progress.set_state(SyncState::Syncing);

        let header = Header {
            version: dashcore::blockdata::block::Version::from_consensus(1),
            prev_blockhash: dashcore::BlockHash::all_zeros(),
            merkle_root: TxMerkleNode::all_zeros(),
            time: 0,
            bits: dashcore::CompactTarget::from_consensus(0),
            nonce: 0,
        };
        let block = Block {
            header,
            txdata: vec![],
        };
        // Only wallet_in is in the routed set.
        manager.pipeline.add_from_storage(
            HashedBlock::from(&block),
            100,
            BTreeSet::from([wallet_in]),
        );

        let _ = manager.process_buffered_blocks().await.unwrap();

        let processed = multi.read().await.processed();
        let processed = processed.lock().await;
        // Exactly one entry, for wallet_in only.
        assert_eq!(processed.len(), 1);
        assert_eq!(processed[0].0, wallet_in);
        assert_eq!(processed[0].2, 100);
        assert!(
            !processed.iter().any(|(id, _, _)| *id == wallet_out),
            "wallet_out was not in the routed set, must not be processed"
        );
    }

    /// `on_disconnect` for `BlocksManager` keeps the downloaded buffer, the
    /// per-block wallet routing, and the `filters_sync_complete` flag; the
    /// broker keeps the in-flight requests and sends them to the peers that
    /// are left. Without this preservation, blocks waiting
    /// in `downloaded` for height ordering would be dropped, leaving
    /// `FiltersManager.tracker` entries that never get decremented.
    #[tokio::test]
    async fn test_on_disconnect_preserves_pipeline_work() {
        use dashcore::block::Header;
        use dashcore::{Block, TxMerkleNode};
        use dashcore_hashes::Hash;

        let mut manager = create_test_manager().await;
        manager.filters_sync_complete = true;

        let header = Header {
            version: dashcore::blockdata::block::Version::from_consensus(1),
            prev_blockhash: dashcore::BlockHash::all_zeros(),
            merkle_root: TxMerkleNode::all_zeros(),
            time: 0,
            bits: dashcore::CompactTarget::from_consensus(0),
            nonce: 0,
        };
        let block = Block {
            header,
            txdata: vec![],
        };

        // Already-downloaded block sitting in the pipeline.
        manager.pipeline.add_from_storage(
            HashedBlock::from(&block),
            200,
            BTreeSet::from([MOCK_WALLET_ID]),
        );

        manager.on_disconnect();

        assert!(!manager.pipeline.is_complete(), "downloaded buffer must survive on_disconnect");
        assert!(manager.filters_sync_complete);
    }

    fn blocks_needed(blocks: &[(u32, dashcore::BlockHash)]) -> SyncEvent {
        SyncEvent::BlocksNeeded {
            blocks: blocks
                .iter()
                .map(|(height, hash)| {
                    (FilterMatchKey::new(*height, *hash), BTreeSet::from([MOCK_WALLET_ID]))
                })
                .collect(),
        }
    }

    /// The blocks asked for since the mock's recorders were last cleared, sorted.
    async fn requested_blocks(mock: &MockNetworkManager) -> Vec<dashcore::BlockHash> {
        let mut hashes: Vec<_> = mock
            .sent_messages()
            .await
            .into_iter()
            .flat_map(|msg| match msg {
                NetworkMessage::GetData(inv) => inv,
                _ => vec![],
            })
            .filter_map(|inv| match inv {
                Inventory::Block(hash) => Some(hash),
                _ => None,
            })
            .collect();
        hashes.sort();
        hashes
    }

    /// The broker settles a block when it arrives, while the block still waits in this
    /// manager's inbound channel. The next `BlocksNeeded`, which the filters manager
    /// sends again for blocks still in flight, must not ask for it again: on a dense
    /// scan those repeats kept the download from making progress.
    #[tokio::test]
    async fn test_an_arrived_block_is_not_requested_again_by_the_next_blocks_needed() {
        let mut manager = create_test_manager().await;
        let mock = Arc::new(MockNetworkManager::new());
        let network: Arc<dyn NetworkManager> = mock.clone();
        let [a, b, c] = [1, 2, 3].map(|id| dashcore::block::Header::dummy(id).block_hash());

        manager.handle_sync_event(&blocks_needed(&[(100, a), (101, b)]), &network).await.unwrap();
        let mut first = vec![a, b];
        first.sort();
        assert_eq!(requested_blocks(&mock).await, first);

        // Both blocks arrive and the broker settles them; this manager hasn't handled them.
        mock.release_requests_in_play().await;
        mock.clear_sent().await;

        manager.handle_sync_event(&blocks_needed(&[(100, a), (102, c)]), &network).await.unwrap();
        assert_eq!(requested_blocks(&mock).await, vec![c], "only the new block is requested");
    }

    /// The broker settles a block when a copy arrives, so a copy that fails validation
    /// must be asked for again; an invalid block nobody wants is not.
    #[tokio::test]
    async fn test_a_block_that_fails_validation_is_requested_again() {
        let mut manager = create_test_manager().await;
        let mock = Arc::new(MockNetworkManager::new());
        let network: Arc<dyn NetworkManager> = mock.clone();
        // No transactions, so neither merkle root matches.
        let [wanted, unwanted] = [1, 2].map(|id| Block {
            header: dashcore::block::Header::dummy(id),
            txdata: vec![],
        });
        manager
            .handle_sync_event(&blocks_needed(&[(100, wanted.block_hash())]), &network)
            .await
            .unwrap();

        // The invalid copy arrives, which settles the request.
        mock.release_requests_in_play().await;
        mock.clear_sent().await;
        for block in [&wanted, &unwanted] {
            let msg = NetworkMessage::Block(block.clone()).into();
            assert!(manager.handle_message(test_socket_address(1), msg, &network).await.is_err());
        }
        assert_eq!(requested_blocks(&mock).await, vec![wanted.block_hash()]);
    }

    /// Blocks are declared at most [`MAX_DECLARED_AHEAD`] ahead of what this manager has
    /// taken in, lowest height first, as before #902. Each block taken in frees a place,
    /// so the download still finishes.
    #[tokio::test]
    async fn test_blocks_are_declared_at_most_a_window_ahead() {
        const COUNT: usize = 3 * MAX_DECLARED_AHEAD + 5;
        let blocks = valid_blocks(COUNT);
        let mut manager = create_test_manager().await;
        store_headers(&manager, &blocks).await;
        let mock = Arc::new(MockNetworkManager::new());
        let network: Arc<dyn NetworkManager> = mock.clone();

        let needed: Vec<(u32, dashcore::BlockHash)> =
            blocks.iter().enumerate().map(|(height, b)| (height as u32, b.block_hash())).collect();
        manager.handle_sync_event(&blocks_needed(&needed), &network).await.unwrap();
        assert_eq!(mock.sent_messages().await.len(), MAX_DECLARED_AHEAD, "the window fills");

        for (taken_in, block) in blocks.iter().enumerate() {
            let requested = requested_blocks(&mock).await;
            assert!(
                requested.len() - taken_in <= MAX_DECLARED_AHEAD,
                "{} declared ahead of {taken_in} taken in",
                requested.len() - taken_in
            );
            assert!(requested.contains(&block.block_hash()), "the next height is declared");
            let msg = NetworkMessage::Block(block.clone()).into();
            manager.handle_message(test_socket_address(1), msg, &network).await.unwrap();
        }

        assert_eq!(requested_blocks(&mock).await.len(), COUNT, "each block declared once");
        assert!(manager.pipeline.is_complete());
        assert_eq!(manager.progress.downloaded(), COUNT as u32);
    }

    /// `count` blocks at heights `0..count` that pass validation.
    fn valid_blocks(count: usize) -> Vec<Block> {
        let address = dashcore::Address::dummy(dashcore::Network::Regtest, 0);
        dashcore::block::Header::dummy_batch(0..count as u32)
            .into_iter()
            .enumerate()
            .map(|(i, header)| {
                let coinbase = dashcore::Transaction::dummy_coinbase(&address, 50 + i as u64);
                let mut block = Block {
                    header,
                    txdata: vec![coinbase],
                };
                block.header.merkle_root = block.compute_merkle_root().unwrap();
                block
            })
            .collect()
    }

    async fn store_headers(manager: &TestBlocksManager, blocks: &[Block]) {
        let headers: Vec<_> = blocks.iter().map(|b| HashedBlockHeader::from(&b.header)).collect();
        manager.header_storage.write().await.store_headers(&headers).await.unwrap();
    }

    fn needed(blocks: &[Block], heights: std::ops::Range<usize>) -> SyncEvent {
        let keys: Vec<_> = heights.map(|h| (h as u32, blocks[h].block_hash())).collect();
        blocks_needed(&keys)
    }

    /// A block taken in whose handling then fails frees a place with no declaration
    /// after it; the tick declares the next block, or the download would stop there.
    #[tokio::test]
    async fn test_tick_refills_a_place_freed_by_a_failed_block() {
        let blocks = valid_blocks(MAX_DECLARED_AHEAD + 1);
        // No headers stored: handling fails at the height lookup, after the block is taken in.
        let mut manager = create_test_manager().await;
        let mock = Arc::new(MockNetworkManager::new());
        let network: Arc<dyn NetworkManager> = mock.clone();
        manager.handle_sync_event(&needed(&blocks, 0..blocks.len()), &network).await.unwrap();
        assert_eq!(requested_blocks(&mock).await.len(), MAX_DECLARED_AHEAD);

        let msg = NetworkMessage::Block(blocks[0].clone()).into();
        assert!(manager.handle_message(test_socket_address(1), msg, &network).await.is_err());
        assert_eq!(requested_blocks(&mock).await.len(), MAX_DECLARED_AHEAD);

        manager.tick(&network).await.unwrap();
        let last = blocks[MAX_DECLARED_AHEAD].block_hash();
        assert!(requested_blocks(&mock).await.contains(&last), "the tick declares the next");
    }

    /// Lower heights queued after the window filled (a rescan) go before higher ones
    /// queued earlier: blocks are processed in height order, so the lowest is the one
    /// processing waits for.
    #[tokio::test]
    async fn test_lower_heights_queued_later_are_declared_first() {
        let blocks = valid_blocks(2 * MAX_DECLARED_AHEAD + 1);
        let mut manager = create_test_manager().await;
        store_headers(&manager, &blocks).await;
        let mock = Arc::new(MockNetworkManager::new());
        let network: Arc<dyn NetworkManager> = mock.clone();

        // Heights 20..=40: the window takes 20..40, height 40 waits.
        let high = MAX_DECLARED_AHEAD..blocks.len();
        manager.handle_sync_event(&needed(&blocks, high), &network).await.unwrap();
        manager.handle_sync_event(&needed(&blocks, 0..5), &network).await.unwrap();
        mock.clear_sent().await;

        let msg = NetworkMessage::Block(blocks[MAX_DECLARED_AHEAD].clone()).into();
        manager.handle_message(test_socket_address(1), msg, &network).await.unwrap();
        assert_eq!(requested_blocks(&mock).await, vec![blocks[0].block_hash()]);
    }

    fn peer_version(client: SocketAddr) -> VersionMessage {
        let services = ServiceFlags::NETWORK | ServiceFlags::BLOOM;
        VersionMessage {
            version: PROTOCOL_VERSION,
            services,
            timestamp: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64,
            receiver: PeerAddress::new(&client, ServiceFlags::NETWORK),
            sender: PeerAddress::new(&"127.0.0.1:0".parse().unwrap(), services),
            nonce: 1,
            user_agent: "/blocks-test:0.1/".to_owned(),
            start_height: 2,
            relay: false,
            mn_auth_challenge: [0; 32],
            masternode_connection: false,
        }
    }

    async fn send_message(writer: &mut OwnedWriteHalf, payload: NetworkMessage) {
        let raw = RawNetworkMessage {
            magic: dashcore::Network::Regtest.magic(),
            payload,
        };
        let _ = writer.write_all(&serialize(&raw)).await;
    }

    /// Serves `block` from a fake peer at `listener`. The first `getdata` for it, across
    /// every peer sharing `asked`, closes its connection unanswered, as if the peer were
    /// lost with the request in flight; later ones are answered.
    async fn serve_block(listener: TcpListener, block: Block, asked: Arc<AtomicUsize>) {
        while let Ok((stream, client)) = listener.accept().await {
            let (block, asked) = (block.clone(), asked.clone());
            tokio::spawn(async move {
                let (mut reader, mut writer) = stream.into_split();
                send_message(&mut writer, NetworkMessage::Version(peer_version(client))).await;
                send_message(&mut writer, NetworkMessage::Verack).await;
                let (mut pending, mut chunk) = (Vec::new(), [0u8; 4096]);
                loop {
                    match reader.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => pending.extend_from_slice(&chunk[..n]),
                    }
                    while let Ok((raw, used)) = deserialize_partial::<RawNetworkMessage>(&pending) {
                        pending.drain(..used);
                        match raw.payload {
                            // Unanswered, the supervisor keeps the peer as a backup.
                            NetworkMessage::Ping(nonce) => {
                                send_message(&mut writer, NetworkMessage::Pong(nonce)).await
                            }
                            NetworkMessage::GetData(inv)
                                if inv.contains(&Inventory::Block(block.block_hash())) =>
                            {
                                if asked.fetch_add(1, Ordering::SeqCst) == 0 {
                                    return;
                                }
                                send_message(&mut writer, NetworkMessage::Block(block.clone()))
                                    .await;
                            }
                            _ => {}
                        }
                    }
                }
            });
        }
    }

    /// The other side of declaring each block once: a block whose peer is lost before it
    /// arrives is still fetched with no second declaration. The broker keeps the request
    /// and sends it to the other peer, on the disconnect or, if that is slow to show, as
    /// an endgame hedge.
    #[tokio::test]
    async fn test_a_block_whose_peer_disconnects_is_still_fetched() {
        let address = dashcore::Address::dummy(dashcore::Network::Regtest, 0);
        let mut headers = dashcore::block::Header::dummy_batch(0..3);
        let mut block = Block {
            header: headers[2],
            txdata: vec![dashcore::Transaction::dummy_coinbase(&address, 50)],
        };
        block.header.merkle_root = block.compute_merkle_root().unwrap();
        headers[2] = block.header;

        let mut manager = create_test_manager().await;
        manager
            .header_storage
            .write()
            .await
            .store_headers(&headers.iter().map(HashedBlockHeader::from).collect::<Vec<_>>())
            .await
            .unwrap();

        let asked = Arc::new(AtomicUsize::new(0));
        let mut peers = Vec::new();
        for _ in 0..2 {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            peers.push(listener.local_addr().unwrap());
            tokio::spawn(serve_block(listener, block.clone(), asked.clone()));
        }
        let storage = tempfile::TempDir::new().unwrap();
        let mut config = ClientConfig::regtest()
            .with_restrict_to_configured_peers(true)
            .with_storage_path(storage.path());
        config.peers = peers;
        config.max_peers = 2;
        config.enable_filters = false;
        config.enable_masternodes = false;
        let network: Arc<dyn NetworkManager> = Arc::new(PeerNetworkManager::new(&config).await);
        let mut inbound = network.subscribe(&[MessageType::Block]).await;
        network.start().await;
        tokio::time::timeout(Duration::from_secs(10), async {
            while network.connected_count().await < 2 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("both peers must connect");

        manager
            .handle_sync_event(&blocks_needed(&[(2, block.block_hash())]), &network)
            .await
            .unwrap();
        let (peer, msg) = tokio::time::timeout(Duration::from_secs(30), inbound.recv())
            .await
            .expect("the block must still arrive after its first peer closed")
            .unwrap();
        manager.handle_message(peer, (*msg).clone(), &network).await.unwrap();

        assert!(
            asked.load(Ordering::SeqCst) >= 2,
            "the first getdata went unanswered, so the block came from a later one"
        );
        assert!(manager.pipeline.is_complete());
        assert_eq!(manager.progress.downloaded(), 1);
        network.stop().await;
    }
}
