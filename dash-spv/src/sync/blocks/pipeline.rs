//! Blocks pipeline implementation.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use crate::error::SyncResult;
use crate::network::{BlockRequest, NetworkManager, SendRequest};
use crate::types::HashedBlock;
use dashcore::BlockHash;
use key_wallet_manager::{FilterMatchKey, WalletId};

/// The most blocks declared and not yet taken in, as many as the pipeline kept in flight
/// before #902 (`MAX_CONCURRENT_BLOCK_DOWNLOADS`, 832e99bf2 `sync/blocks/pipeline.rs:17`).
/// Without a bound the broker fetches every wanted block at once, and they wait,
/// unprocessed, in this manager's inbound channel.
pub(super) const MAX_DECLARED_AHEAD: usize = 20;

/// Pipeline for downloading blocks with height-ordered processing.
pub(super) struct BlocksPipeline {
    /// Heights not yet downloaded.
    pending_heights: BTreeSet<u32>,
    /// Downloaded blocks ready to process (height -> block, with its cached hash).
    downloaded: BTreeMap<u32, HashedBlock>,
    /// Wanted blocks (hash -> height); a block leaves this map once downloaded.
    hash_to_height: HashMap<BlockHash, u32>,
    /// Wanted blocks not declared to the network manager yet, lowest height first.
    undeclared: BTreeSet<(u32, BlockHash)>,
    /// Declared blocks not taken in yet; at most [`MAX_DECLARED_AHEAD`].
    declared: HashSet<BlockHash>,
    /// Per-block interested wallets, populated when the block is queued.
    /// Only those wallets get the block processed.
    hash_to_wallets: HashMap<BlockHash, BTreeSet<WalletId>>,
}

impl std::fmt::Debug for BlocksPipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlocksPipeline")
            .field("pending_heights", &self.pending_heights.len())
            .field("downloaded", &self.downloaded.len())
            .field("wanted", &self.hash_to_height.len())
            .field("undeclared", &self.undeclared.len())
            .field("declared", &self.declared.len())
            .finish()
    }
}

impl Default for BlocksPipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl BlocksPipeline {
    /// Create a new blocks pipeline.
    pub(super) fn new() -> Self {
        Self {
            pending_heights: BTreeSet::new(),
            downloaded: BTreeMap::new(),
            hash_to_height: HashMap::new(),
            undeclared: BTreeSet::new(),
            declared: HashSet::new(),
            hash_to_wallets: HashMap::new(),
        }
    }

    /// Queue blocks with their heights and per-block interested wallet sets.
    ///
    /// Returns how many were not already tracked, so progress counts each block once.
    pub(super) fn queue(
        &mut self,
        blocks: impl IntoIterator<Item = (FilterMatchKey, BTreeSet<WalletId>)>,
    ) -> usize {
        let mut newly_tracked = 0;
        for (key, wallets) in blocks {
            let hash = *key.hash();
            let already_tracked =
                self.hash_to_height.contains_key(&hash) || self.hash_to_wallets.contains_key(&hash);
            if !already_tracked {
                self.pending_heights.insert(key.height());
                self.hash_to_height.insert(hash, key.height());
                self.undeclared.insert((key.height(), hash));
                newly_tracked += 1;
            }
            self.hash_to_wallets.entry(hash).or_default().extend(wallets);
        }
        newly_tracked
    }

    /// Check if the pipeline has completed all work.
    pub(super) fn is_complete(&self) -> bool {
        self.hash_to_height.is_empty()
            && self.downloaded.is_empty()
            && self.pending_heights.is_empty()
    }

    pub(super) fn has_pending_requests(&self) -> bool {
        !self.hash_to_height.is_empty()
    }

    /// Declare the lowest wanted blocks not declared yet, until [`MAX_DECLARED_AHEAD`] are
    /// declared and not taken in. Each block is declared once: the broker keeps a declared
    /// block until it arrives, sending it to another peer if its peer is lost, and settles
    /// it on arrival, before this pipeline takes it in, so declaring it again in that gap
    /// would fetch it again. Returns the number of blocks declared.
    pub(super) async fn send_pending(
        &mut self,
        network: &Arc<dyn NetworkManager>,
    ) -> SyncResult<usize> {
        let mut sent = 0;
        while self.declared.len() < MAX_DECLARED_AHEAD {
            let Some((height, hash)) = self.undeclared.pop_first() else {
                break;
            };
            if !self.hash_to_height.contains_key(&hash) {
                continue;
            }
            network
                .send(BlockRequest {
                    height,
                    hash,
                })
                .await;
            self.declared.insert(hash);
            sent += 1;
        }
        if sent > 0 {
            tracing::debug!("Declared {} wanted block(s) to the broker", sent);
        }
        Ok(sent)
    }

    /// Declare a wanted block again on the next [`Self::send_pending`]: the broker
    /// settled it when it arrived, but the copy that arrived was rejected.
    pub(super) fn redeclare(&mut self, hash: BlockHash) {
        if let Some(height) = self.hash_to_height.get(&hash) {
            self.declared.remove(&hash);
            self.undeclared.insert((*height, hash));
        }
    }

    /// Handle a received block using internal height mapping.
    ///
    /// Returns `true` if this was a wanted block, `false` if unrequested.
    pub(super) fn receive_block(&mut self, block: &HashedBlock) -> bool {
        let hash = *block.hash();
        let Some(height) = self.hash_to_height.remove(&hash) else {
            tracing::debug!("Ignoring unrequested block: {}", hash);
            return false;
        };
        self.undeclared.remove(&(height, hash));
        self.declared.remove(&hash);
        self.pending_heights.remove(&height);
        self.downloaded.insert(height, block.clone());
        true
    }

    /// Take the next block that's safe to process in height order, along with
    /// the wallet set whose filters matched this block.
    ///
    /// Returns None if:
    /// - No downloaded blocks available, or
    /// - Waiting for a lower-height block still pending
    pub(super) fn take_next_ordered_block(
        &mut self,
    ) -> Option<(HashedBlock, u32, BTreeSet<WalletId>)> {
        let lowest_downloaded = *self.downloaded.keys().next()?;

        // Check if any pending blocks have lower heights
        if let Some(&min_pending) = self.pending_heights.first() {
            if min_pending < lowest_downloaded {
                return None; // Wait for lower block
            }
        }

        let block = self.downloaded.remove(&lowest_downloaded).unwrap();
        let wallets = self.hash_to_wallets.remove(block.hash()).unwrap_or_default();
        Some((block, lowest_downloaded, wallets))
    }

    /// Add a block that was loaded from storage (skip download).
    ///
    /// Used when blocks are already persisted from a previous sync.
    pub(super) fn add_from_storage(
        &mut self,
        block: HashedBlock,
        height: u32,
        wallets: BTreeSet<WalletId>,
    ) {
        let hash = *block.hash();
        self.hash_to_wallets.entry(hash).or_default().extend(wallets);
        self.downloaded.insert(height, block);
    }
}

#[cfg(test)]
mod tests {
    use dashcore::blockdata::block::Block;
    use dashcore_hashes::Hash;

    use super::*;

    fn make_test_block(n: u8) -> Block {
        use dashcore::blockdata::block::Header;
        let header = Header {
            version: dashcore::blockdata::block::Version::from_consensus(1),
            prev_blockhash: BlockHash::from_byte_array([n; 32]),
            merkle_root: dashcore::TxMerkleNode::all_zeros(),
            time: n as u32,
            bits: dashcore::CompactTarget::from_consensus(0),
            nonce: n as u32,
        };
        Block {
            header,
            txdata: vec![],
        }
    }

    #[test]
    fn test_blocks_pipeline_new() {
        let pipeline = BlocksPipeline::new();
        assert!(pipeline.hash_to_height.is_empty());
        assert!(pipeline.is_complete());
    }

    #[test]
    fn test_queue_block() {
        let mut pipeline = BlocksPipeline::new();
        let block = make_test_block(1);
        pipeline.queue([(FilterMatchKey::new(100, block.block_hash()), BTreeSet::new())]);

        assert_eq!(pipeline.hash_to_height.len(), 1);
        assert!(!pipeline.is_complete());
        assert!(pipeline.has_pending_requests());
    }

    #[test]
    fn test_queue_multiple() {
        let mut pipeline = BlocksPipeline::new();
        let block1 = make_test_block(1);
        let block2 = make_test_block(2);
        let block3 = make_test_block(3);
        pipeline.queue([
            (FilterMatchKey::new(100, block1.block_hash()), BTreeSet::new()),
            (FilterMatchKey::new(101, block2.block_hash()), BTreeSet::new()),
            (FilterMatchKey::new(102, block3.block_hash()), BTreeSet::new()),
        ]);

        assert_eq!(pipeline.hash_to_height.len(), 3);
        assert_eq!(pipeline.pending_heights.len(), 3);
        assert!(pipeline.pending_heights.contains(&100));
        assert!(pipeline.pending_heights.contains(&101));
        assert!(pipeline.pending_heights.contains(&102));
    }

    #[test]
    fn test_receive_block_with_height() {
        let mut pipeline = BlocksPipeline::new();
        let block = make_test_block(1);
        let hash = block.block_hash();

        pipeline.queue([(FilterMatchKey::new(100, block.block_hash()), BTreeSet::new())]);

        assert!(pipeline.receive_block(&HashedBlock::from(&block)));
        assert!(pipeline.hash_to_height.is_empty());
        assert_eq!(pipeline.downloaded.len(), 1);
        assert!(pipeline.pending_heights.is_empty());
        assert_eq!(*pipeline.downloaded.get(&100).unwrap().hash(), hash);
    }

    #[test]
    fn test_receive_block_unrequested() {
        let mut pipeline = BlocksPipeline::new();
        let block = make_test_block(1);

        assert!(!pipeline.receive_block(&HashedBlock::from(&block)));
        assert!(pipeline.downloaded.is_empty());
    }

    #[test]
    fn test_take_next_ordered_block_in_order() {
        let mut pipeline = BlocksPipeline::new();
        let block1 = make_test_block(1);
        let block2 = make_test_block(2);
        let hash1 = block1.block_hash();
        let hash2 = block2.block_hash();

        pipeline.add_from_storage(HashedBlock::from(&block2), 101, BTreeSet::new());
        pipeline.pending_heights.insert(100);

        // Cannot take block 2 yet - waiting for block at height 100
        assert!(pipeline.take_next_ordered_block().is_none());

        pipeline.pending_heights.remove(&100);
        pipeline.add_from_storage(HashedBlock::from(&block1), 100, BTreeSet::new());

        let (block, height, _) = pipeline.take_next_ordered_block().unwrap();
        assert_eq!(height, 100);
        assert_eq!(*block.hash(), hash1);

        let (block, height, _) = pipeline.take_next_ordered_block().unwrap();
        assert_eq!(height, 101);
        assert_eq!(*block.hash(), hash2);

        assert!(pipeline.take_next_ordered_block().is_none());
    }

    #[test]
    fn test_take_next_ordered_block_waits_for_pending() {
        let mut pipeline = BlocksPipeline::new();
        let block2 = make_test_block(2);

        pipeline.pending_heights.insert(100);
        pipeline.add_from_storage(HashedBlock::from(&block2), 101, BTreeSet::new());

        assert!(pipeline.take_next_ordered_block().is_none());

        pipeline.pending_heights.remove(&100);

        let (_, height, _) = pipeline.take_next_ordered_block().unwrap();
        assert_eq!(height, 101);
    }

    #[test]
    fn test_add_from_storage() {
        let mut pipeline = BlocksPipeline::new();
        let block = make_test_block(1);
        let hash = block.block_hash();

        pipeline.add_from_storage(HashedBlock::from(&block), 100, BTreeSet::new());
        assert_eq!(pipeline.downloaded.len(), 1);

        let (taken_block, height, _) = pipeline.take_next_ordered_block().unwrap();
        assert_eq!(height, 100);
        assert_eq!(*taken_block.hash(), hash);
    }

    #[test]
    fn test_is_complete() {
        let mut pipeline = BlocksPipeline::new();
        assert!(pipeline.is_complete());

        let block = make_test_block(1);
        pipeline.add_from_storage(HashedBlock::from(&block), 100, BTreeSet::new());
        assert!(!pipeline.is_complete());

        pipeline.take_next_ordered_block();
        assert!(pipeline.is_complete());
    }

    #[test]
    fn test_is_complete_with_pending_heights() {
        let mut pipeline = BlocksPipeline::new();
        assert!(pipeline.is_complete());

        pipeline.pending_heights.insert(100);
        assert!(!pipeline.is_complete());

        pipeline.pending_heights.remove(&100);
        assert!(pipeline.is_complete());
    }

    #[test]
    fn test_queue_propagates_wallet_set_through_take_next() {
        let mut pipeline = BlocksPipeline::new();
        let block = make_test_block(1);
        let hash = block.block_hash();
        let wallets: BTreeSet<WalletId> = BTreeSet::from([[1u8; 32], [2u8; 32]]);

        pipeline.queue([(FilterMatchKey::new(100, hash), wallets.clone())]);
        assert!(pipeline.receive_block(&HashedBlock::from(&block)));

        let (taken_block, height, taken_wallets) = pipeline.take_next_ordered_block().unwrap();
        assert_eq!(*taken_block.hash(), hash);
        assert_eq!(height, 100);
        assert_eq!(taken_wallets, wallets);
    }

    #[test]
    fn test_queue_merges_wallet_sets_for_repeat_hashes() {
        let mut pipeline = BlocksPipeline::new();
        let block = make_test_block(1);
        let hash = block.block_hash();
        let wallets_a: BTreeSet<WalletId> = BTreeSet::from([[1u8; 32]]);
        let wallets_b: BTreeSet<WalletId> = BTreeSet::from([[2u8; 32], [3u8; 32]]);

        pipeline.queue([(FilterMatchKey::new(100, hash), wallets_a.clone())]);
        assert_eq!(pipeline.hash_to_height.len(), 1);
        pipeline.queue([(FilterMatchKey::new(100, hash), wallets_b.clone())]);
        assert_eq!(pipeline.hash_to_height.len(), 1);

        assert!(pipeline.receive_block(&HashedBlock::from(&block)));

        let (_, _, taken_wallets) = pipeline.take_next_ordered_block().unwrap();
        let mut expected = wallets_a;
        expected.extend(wallets_b);
        assert_eq!(taken_wallets, expected);
    }

    #[test]
    fn test_queue_does_not_re_enqueue_downloaded_hash() {
        let mut pipeline = BlocksPipeline::new();
        let block = make_test_block(1);
        let hash = block.block_hash();
        let wallets_a: BTreeSet<WalletId> = BTreeSet::from([[1u8; 32]]);
        let wallets_b: BTreeSet<WalletId> = BTreeSet::from([[2u8; 32]]);

        pipeline.queue([(FilterMatchKey::new(100, hash), wallets_a.clone())]);
        assert!(pipeline.receive_block(&HashedBlock::from(&block)));
        assert_eq!(pipeline.downloaded.len(), 1);
        assert!(pipeline.hash_to_height.is_empty());

        // Late-arriving match for the same hash must not re-enqueue.
        pipeline.queue([(FilterMatchKey::new(100, hash), wallets_b.clone())]);
        assert!(pipeline.hash_to_height.is_empty());
        assert_eq!(pipeline.downloaded.len(), 1);

        let (_, _, taken_wallets) = pipeline.take_next_ordered_block().unwrap();
        let mut expected = wallets_a;
        expected.extend(wallets_b);
        assert_eq!(taken_wallets, expected);
    }

    #[test]
    fn test_add_from_storage_merges_wallet_sets() {
        let mut pipeline = BlocksPipeline::new();
        let block = make_test_block(1);
        let wallets_a: BTreeSet<WalletId> = BTreeSet::from([[1u8; 32]]);
        let wallets_b: BTreeSet<WalletId> = BTreeSet::from([[2u8; 32]]);

        pipeline.add_from_storage(HashedBlock::from(&block), 100, wallets_a.clone());
        pipeline.add_from_storage(HashedBlock::from(&block), 100, wallets_b.clone());

        let (_, _, taken_wallets) = pipeline.take_next_ordered_block().unwrap();
        let mut expected = wallets_a;
        expected.extend(wallets_b);
        assert_eq!(taken_wallets, expected);
    }

    #[test]
    fn test_receive_block_duplicate() {
        let mut pipeline = BlocksPipeline::new();
        let block = make_test_block(1);

        pipeline.queue([(FilterMatchKey::new(100, block.block_hash()), BTreeSet::new())]);

        assert!(pipeline.receive_block(&HashedBlock::from(&block)));
        assert_eq!(pipeline.downloaded.len(), 1);

        assert!(!pipeline.receive_block(&HashedBlock::from(&block)));
        assert_eq!(pipeline.downloaded.len(), 1);
    }
}
