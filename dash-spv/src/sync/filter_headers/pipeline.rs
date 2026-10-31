//! CFHeaders pipeline implementation.
//!
//! Handles pipelined download of compact block filter headers (BIP 157/158).
//! Uses DownloadCoordinator for batch tracking with out-of-order buffering.

use dashcore::network::message::NetworkMessage;
use dashcore::network::message_filter::CFHeaders;
use dashcore::BlockHash;
use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::time::{Duration, Instant};

use crate::error::{SyncError, SyncResult};
use crate::network::RequestSender;
use crate::storage::BlockHeaderStorage;
use crate::sync::download_coordinator::{DownloadConfig, DownloadCoordinator};

/// Batch size for filter header requests.
const FILTER_HEADERS_BATCH_SIZE: u32 = 2000;

/// Maximum concurrent CFHeaders requests.
const MAX_CONCURRENT_CFHEADERS_REQUESTS: usize = 10;

/// Timeout for CFHeaders requests (shorter for faster retry on multi-peer).
/// Timeout for CFHeaders requests. Single response but allow time for network latency.
const FILTER_HEADERS_TIMEOUT: Duration = Duration::from_secs(20);

/// Least time between two warnings about replies that do not cover their batch.
const MISMATCH_WARN_INTERVAL: Duration = Duration::from_secs(60);

/// Rate limit for the warning about replies that do not cover their batch.
///
/// One such reply is the expected leftover of a rebuilt queue; a peer that
/// keeps sending them makes the batch time out and retry every
/// `FILTER_HEADERS_TIMEOUT` for as long as it stays connected.
#[derive(Debug, Default)]
struct MismatchLog {
    last_warned: Option<Instant>,
    suppressed: u32,
}

/// Number of heights in a batch.
fn batch_len(batch: &RangeInclusive<u32>) -> usize {
    (batch.end() - batch.start()) as usize + 1
}

/// Pipeline for downloading compact block filter headers.
///
/// Uses DownloadCoordinator<BlockHash> for batch-level tracking (keyed by stop_hash),
/// with a HashMap buffer for out-of-order responses that need sequential processing.
///
/// A response is only accepted when it covers exactly the batch its stop hash
/// names: see [`Self::match_response`].
#[derive(Debug)]
pub(super) struct FilterHeadersPipeline {
    /// Core coordinator tracks batches by stop_hash.
    coordinator: DownloadCoordinator<BlockHash>,
    /// Maps stop_hash -> the heights its batch covers.
    batches: HashMap<BlockHash, RangeInclusive<u32>>,
    /// Out-of-order response buffer (start_height -> data).
    buffered: HashMap<u32, CFHeaders>,
    /// Next height to process sequentially.
    next_expected: u32,
    /// Target height for sync.
    target_height: u32,
    /// Rate limit for the mismatched-reply warning.
    mismatch_log: MismatchLog,
}

impl Default for FilterHeadersPipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl FilterHeadersPipeline {
    /// Create a new CFHeaders pipeline.
    pub(super) fn new() -> Self {
        Self {
            coordinator: DownloadCoordinator::new(
                DownloadConfig::default()
                    .with_max_concurrent(MAX_CONCURRENT_CFHEADERS_REQUESTS)
                    .with_timeout(FILTER_HEADERS_TIMEOUT),
            ),
            batches: HashMap::new(),
            buffered: HashMap::new(),
            next_expected: 0,
            target_height: 0,
            mismatch_log: MismatchLog::default(),
        }
    }

    /// Extend the pipeline to a new target height.
    ///
    /// Queues additional batches from the current target to the new target.
    pub(super) async fn extend_target(
        &mut self,
        storage: &impl BlockHeaderStorage,
        new_target: u32,
    ) -> SyncResult<()> {
        let old_target = self.target_height;
        if new_target <= old_target {
            return Ok(());
        }

        self.target_height = new_target;

        // Queue batches from (old_target + 1) to new_target
        let mut current = old_target + 1;
        let mut added = 0;

        while current <= new_target {
            let batch_end = (current + FILTER_HEADERS_BATCH_SIZE - 1).min(new_target);

            // Get stop hash for this batch
            let stop_hash =
                storage.get_header(batch_end).await?.map(|h| *h.hash()).ok_or_else(|| {
                    SyncError::Storage(format!("Missing header at height {}", batch_end))
                })?;

            self.coordinator.enqueue([stop_hash]);
            self.batches.insert(stop_hash, current..=batch_end);
            added += 1;

            current = batch_end + 1;
        }

        if added > 0 {
            tracing::info!(
                "Extended CFHeaders queue: +{} batches for heights {} to {}",
                added,
                old_target + 1,
                new_target
            );
        }

        Ok(())
    }

    /// Get the next expected height for sequential processing.
    pub(super) fn next_expected(&self) -> u32 {
        self.next_expected
    }

    /// Check if the pipeline is complete.
    pub(super) fn is_complete(&self) -> bool {
        self.coordinator.is_empty()
            && self.buffered.is_empty()
            && (self.target_height == 0 || self.next_expected > self.target_height)
    }

    /// Initialize the pipeline for a sync range.
    pub(super) async fn init(
        &mut self,
        storage: &impl BlockHeaderStorage,
        start_height: u32,
        target_height: u32,
    ) -> SyncResult<()> {
        self.coordinator.clear();
        self.batches.clear();
        self.buffered.clear();
        self.next_expected = start_height;
        self.target_height = target_height;

        // Build request queue
        let mut current = start_height;
        while current <= target_height {
            let batch_end = (current + FILTER_HEADERS_BATCH_SIZE - 1).min(target_height);

            // Get stop hash for this batch
            let stop_hash =
                storage.get_header(batch_end).await?.map(|h| *h.hash()).ok_or_else(|| {
                    SyncError::Storage(format!("Missing header at height {}", batch_end))
                })?;

            self.coordinator.enqueue([stop_hash]);
            self.batches.insert(stop_hash, current..=batch_end);

            current = batch_end + 1;
        }

        tracing::info!(
            "Built CFHeaders request queue: {} batches for heights {} to {}",
            self.coordinator.pending_count(),
            start_height,
            target_height
        );

        Ok(())
    }

    /// Send pending requests using a RequestSender (synchronous).
    pub(super) fn send_pending(&mut self, requests: &RequestSender) -> SyncResult<usize> {
        let count = self.coordinator.available_to_send();
        if count == 0 {
            return Ok(0);
        }

        let stop_hashes = self.coordinator.take_pending(count);
        let mut sent = 0;

        for stop_hash in stop_hashes {
            let Some(start_height) = self.batches.get(&stop_hash).map(|b| *b.start()) else {
                return Err(SyncError::InvalidState(format!(
                    "No batch entry for pending stop_hash {}",
                    stop_hash
                )));
            };

            requests.request_filter_headers(start_height, stop_hash)?;

            self.coordinator.mark_sent(&[stop_hash]);

            tracing::debug!(
                "Sent GetCFHeaders: start={}, stop={} ({} active, {} pending)",
                start_height,
                stop_hash,
                self.coordinator.active_count(),
                self.coordinator.pending_count()
            );

            sent += 1;
        }

        Ok(sent)
    }

    /// Try to match an incoming message to a pipeline response.
    ///
    /// Returns `Some((start_height, data))` if matched, `None` otherwise.
    pub(super) fn match_response(&mut self, msg: &NetworkMessage) -> Option<(u32, CFHeaders)> {
        let NetworkMessage::CFHeaders(cfheaders) = msg else {
            return None;
        };

        if cfheaders.filter_hashes.is_empty() {
            return None;
        }

        // Match by stop_hash - the response includes it
        if !self.coordinator.is_in_flight(&cfheaders.stop_hash) {
            return None;
        }

        // The stop hash alone does not identify a batch. A response to a
        // request sent before the pipeline was rebuilt (a disconnect resets it
        // while requests are still in flight) can share its stop hash with a
        // new batch that starts elsewhere: the 2000-height batch that ended at
        // the old target and the single-height batch the rebuilt queue ends
        // there. Taking it for the new batch stores its headers at the wrong
        // heights. Only a response covering exactly this batch is its answer;
        // the batch stays in flight until that one arrives.
        let batch = self.batches.get(&cfheaders.stop_hash)?.clone();
        if cfheaders.filter_hashes.len() != batch_len(&batch) {
            self.log_mismatch(&batch, cfheaders);
            return None;
        }
        Some((*batch.start(), cfheaders.clone()))
    }

    /// Report a reply that does not cover its batch, at warn level at most once
    /// per `MISMATCH_WARN_INTERVAL` and at debug level otherwise.
    fn log_mismatch(&mut self, batch: &RangeInclusive<u32>, cfheaders: &CFHeaders) {
        let log = &mut self.mismatch_log;
        let now = Instant::now();
        if log.last_warned.is_some_and(|at| now.duration_since(at) < MISMATCH_WARN_INTERVAL) {
            log.suppressed += 1;
            tracing::debug!(
                "Ignoring CFHeaders for batch {}..={}: expected {} headers, received {} (stop {})",
                batch.start(),
                batch.end(),
                batch_len(batch),
                cfheaders.filter_hashes.len(),
                cfheaders.stop_hash
            );
            return;
        }
        tracing::warn!(
            "Ignoring CFHeaders for batch {}..={}: expected {} headers, received {} (stop {}); \
             a stale reply after a reconnect, or a non-conforming peer if this repeats \
             ({} more since the last warning)",
            batch.start(),
            batch.end(),
            batch_len(batch),
            cfheaders.filter_hashes.len(),
            cfheaders.stop_hash,
            log.suppressed
        );
        log.last_warned = Some(now);
        log.suppressed = 0;
    }

    /// Handle a received response.
    ///
    /// Returns `Some(data)` if this response is the next expected and should
    /// be processed immediately. Returns `None` if buffered for later.
    pub(super) fn receive(&mut self, start_height: u32, data: CFHeaders) -> Option<CFHeaders> {
        self.coordinator.receive(&data.stop_hash);
        let batch = self.batches.remove(&data.stop_hash);
        // `match_response` hands over only a reply that covers its batch, so
        // the count the caller later passes to `advance` is the batch length
        // and `next_expected` stays on the batch starts `buffered` is keyed by.
        debug_assert!(
            batch.is_none_or(
                |b| *b.start() == start_height && batch_len(&b) == data.filter_hashes.len()
            ),
            "a reply must cover its batch exactly"
        );

        if start_height == self.next_expected {
            Some(data)
        } else if start_height > self.next_expected {
            // Out-of-order - buffer for later
            self.buffered.insert(start_height, data);
            None
        } else {
            // Already processed (duplicate)
            None
        }
    }

    /// Advance to the next expected height after processing.
    ///
    /// Returns any buffered responses that are now ready.
    pub(super) fn advance(&mut self, processed_count: u32) -> Vec<(u32, CFHeaders)> {
        self.next_expected += processed_count;

        // Check if next_expected is now in the buffer
        let mut ready = Vec::new();
        if let Some(data) = self.buffered.remove(&self.next_expected) {
            ready.push((self.next_expected, data));
        }
        ready
    }

    /// Re-request a received batch that was rejected instead of stored.
    ///
    /// Buffered batches above it chain from it, so they are dropped and re-requested
    /// too. `next_expected` is unchanged.
    pub(super) fn requeue_rejected(&mut self, start_height: u32, rejected: &CFHeaders) {
        let mut dropped: Vec<(u32, BlockHash, usize)> = self
            .buffered
            .extract_if(|&height, _| height > start_height)
            .map(|(height, data)| (height, data.stop_hash, data.filter_hashes.len()))
            .collect();
        // `enqueue_retry` pushes to the front, so queue the highest batch first.
        dropped.sort_unstable_by_key(|&(height, ..)| std::cmp::Reverse(height));
        let rejected = (start_height, rejected.stop_hash, rejected.filter_hashes.len());
        // A received reply covers its batch exactly, so its length gives the batch back.
        for (height, hash, len) in dropped.into_iter().chain([rejected]) {
            self.coordinator.enqueue_retry(hash);
            self.batches.insert(hash, height..=height + len as u32 - 1);
        }
    }

    /// Move in-flight `getcfheaders` requests back to pending after a peer
    /// disconnect so the next `send_pending` reissues them.
    ///
    /// `batches` must survive, since `send_pending` errors out on a pending
    /// stop hash with no batch. `next_expected` and the out-of-order
    /// buffer survive too, so already-received batches are not re-downloaded.
    pub(super) fn requeue_in_flight(&mut self) {
        self.coordinator.requeue_in_flight();
    }

    /// Re-enqueue timed out requests for retry.
    pub(super) fn handle_timeouts(&mut self) {
        for stop_hash in self.coordinator.check_timeouts() {
            self.coordinator.enqueue_retry(stop_hash);
        }
    }
}

#[cfg(test)]
mod tests {
    use dashcore_hashes::Hash;

    use super::*;
    use crate::network::NetworkRequest;
    use dashcore::hash_types::{FilterHash, FilterHeader};
    use dashcore::network::message_filter::GetCFHeaders;
    use tokio::sync::mpsc::unbounded_channel;

    #[test]
    fn test_cfheaders_pipeline_new() {
        let pipeline = FilterHeadersPipeline::new();
        assert!(pipeline.is_complete());
    }

    #[test]
    fn test_match_response_empty() {
        let mut pipeline = FilterHeadersPipeline::new();

        let empty_cfheaders = CFHeaders {
            filter_type: 0,
            stop_hash: dashcore::BlockHash::all_zeros(),
            previous_filter_header: dashcore::hash_types::FilterHeader::all_zeros(),
            filter_hashes: vec![],
        };

        // Empty response should return None
        assert!(pipeline.match_response(&NetworkMessage::CFHeaders(empty_cfheaders)).is_none());
    }

    #[test]
    fn test_match_response_wrong_message() {
        let mut pipeline = FilterHeadersPipeline::new();

        // Wrong message type should return None
        assert!(pipeline.match_response(&NetworkMessage::Verack).is_none());
    }

    #[test]
    fn test_receive_in_order() {
        use dashcore::hash_types::FilterHash;

        let mut pipeline = FilterHeadersPipeline::new();
        pipeline.next_expected = 1;
        pipeline.target_height = 100;

        let stop_hash = BlockHash::all_zeros();

        // Mark batch as in-flight (by stop_hash)
        pipeline.coordinator.mark_sent(&[stop_hash]);
        pipeline.batches.insert(stop_hash, 1..=1);

        let cfheaders = CFHeaders {
            filter_type: 0,
            stop_hash,
            previous_filter_header: dashcore::hash_types::FilterHeader::all_zeros(),
            filter_hashes: vec![FilterHash::all_zeros()],
        };

        // Should return data immediately
        let result = pipeline.receive(1, cfheaders.clone());
        assert!(result.is_some());
    }

    #[test]
    fn test_receive_out_of_order() {
        use dashcore::hash_types::FilterHash;

        let mut pipeline = FilterHeadersPipeline::new();
        pipeline.next_expected = 1;
        pipeline.target_height = 4000;

        let stop_hash = BlockHash::all_zeros();

        // Mark batch as in-flight (by stop_hash)
        pipeline.coordinator.mark_sent(&[stop_hash]);
        pipeline.batches.insert(stop_hash, 2000..=2000);

        let cfheaders = CFHeaders {
            filter_type: 0,
            stop_hash,
            previous_filter_header: dashcore::hash_types::FilterHeader::all_zeros(),
            filter_hashes: vec![FilterHash::all_zeros()],
        };

        // Should buffer (out of order)
        let result = pipeline.receive(2000, cfheaders);
        assert!(result.is_none());
        assert_eq!(pipeline.buffered.len(), 1);
    }

    #[test]
    fn test_advance_returns_buffered() {
        use dashcore::hash_types::FilterHash;

        let mut pipeline = FilterHeadersPipeline::new();
        pipeline.next_expected = 1;
        pipeline.target_height = 4000;

        // Buffer a response at height 2000
        let cfheaders = CFHeaders {
            filter_type: 0,
            stop_hash: BlockHash::all_zeros(),
            previous_filter_header: dashcore::hash_types::FilterHeader::all_zeros(),
            filter_hashes: vec![FilterHash::all_zeros()],
        };
        pipeline.buffered.insert(2000, cfheaders);

        // Advance to 2000
        let ready = pipeline.advance(1999);
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].0, 2000);
        assert_eq!(pipeline.buffered.len(), 0);
    }

    #[test]
    fn test_handle_timeouts_basic_retry() {
        use std::time::Duration;

        let mut pipeline = FilterHeadersPipeline {
            coordinator: DownloadCoordinator::new(
                DownloadConfig::default().with_timeout(Duration::from_millis(1)),
            ),
            batches: HashMap::new(),
            buffered: HashMap::new(),
            next_expected: 1,
            mismatch_log: MismatchLog::default(),
            target_height: 2000,
        };

        let stop_hash = BlockHash::all_zeros();
        pipeline.coordinator.mark_sent(&[stop_hash]);
        pipeline.batches.insert(stop_hash, 1..=1);

        std::thread::sleep(Duration::from_millis(5));

        pipeline.handle_timeouts();
        assert_eq!(pipeline.coordinator.pending_count(), 1);
    }

    #[test]
    fn test_send_pending_errors_on_missing_batch() {
        let mut pipeline = FilterHeadersPipeline::new();
        pipeline.next_expected = 1;
        pipeline.target_height = 2000;

        let hash_without_entry = BlockHash::from_byte_array([0x02; 32]);

        // Enqueue a stop_hash without a corresponding batch entry
        pipeline.coordinator.enqueue([hash_without_entry]);

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let requests = RequestSender::new(tx);

        let err = pipeline.send_pending(&requests).unwrap_err();
        assert!(matches!(err, SyncError::InvalidState(_)));
    }

    /// A peer disconnect requeues in-flight batches without discarding what the
    /// pipeline has already made of the ones that came back, so the reissued
    /// requests carry their original start heights and nothing is re-downloaded.
    #[test]
    fn test_requeue_in_flight_reissues_batches_and_keeps_progress() {
        let mut pipeline = FilterHeadersPipeline::new();
        pipeline.next_expected = 1;
        pipeline.target_height = 6000;

        let hash1 = BlockHash::from_byte_array([0x01; 32]);
        let hash2 = BlockHash::from_byte_array([0x02; 32]);
        pipeline.coordinator.mark_sent(&[hash1, hash2]);
        pipeline.batches.insert(hash1, 1..=2000);
        pipeline.batches.insert(hash2, 2001..=4000);

        // A third batch already came back out of order and is waiting for the
        // two above to be processed first.
        pipeline.buffered.insert(
            4001,
            CFHeaders {
                filter_type: 0,
                stop_hash: BlockHash::from_byte_array([0x03; 32]),
                previous_filter_header: FilterHeader::all_zeros(),
                filter_hashes: vec![FilterHash::all_zeros()],
            },
        );

        pipeline.requeue_in_flight();
        assert_eq!(pipeline.coordinator.active_count(), 0);
        assert_eq!(pipeline.coordinator.pending_count(), 2);

        let (tx, mut rx) = unbounded_channel();
        let requests = RequestSender::new(tx);
        assert_eq!(pipeline.send_pending(&requests).unwrap(), 2);

        let mut reissued = Vec::new();
        while let Ok(request) = rx.try_recv() {
            match request {
                NetworkRequest::SendMessage(NetworkMessage::GetCFHeaders(GetCFHeaders {
                    start_height,
                    stop_hash,
                    ..
                })) => reissued.push((start_height, stop_hash)),
                other => panic!("Expected GetCFHeaders, got {:?}", other),
            }
        }
        reissued.sort();
        assert_eq!(reissued, vec![(1, hash1), (2001, hash2)]);

        assert_eq!(pipeline.next_expected(), 1);
        assert_eq!(pipeline.buffered.len(), 1);
        assert_eq!(pipeline.target_height, 6000);
    }

    /// A rejected batch is requested again with its start height, along with every
    /// buffered batch above it, lowest first; batches below it are not touched.
    #[test]
    fn test_requeue_rejected_reissues_batch_and_drops_buffered_above() {
        let mut pipeline = FilterHeadersPipeline::new();
        pipeline.next_expected = 2001;
        pipeline.target_height = 8000;

        let rejected = BlockHash::from_byte_array([0x02; 32]);
        for (height, byte) in [(6001, 0x04), (4001, 0x03)] {
            pipeline.buffered.insert(
                height,
                CFHeaders {
                    filter_type: 0,
                    stop_hash: BlockHash::from_byte_array([byte; 32]),
                    previous_filter_header: FilterHeader::all_zeros(),
                    filter_hashes: vec![FilterHash::all_zeros()],
                },
            );
        }

        pipeline.requeue_rejected(
            2001,
            &CFHeaders {
                filter_type: 0,
                stop_hash: rejected,
                previous_filter_header: FilterHeader::all_zeros(),
                filter_hashes: vec![FilterHash::all_zeros()],
            },
        );
        assert!(pipeline.buffered.is_empty());
        assert_eq!(pipeline.next_expected(), 2001);

        let (tx, mut rx) = unbounded_channel();
        assert_eq!(pipeline.send_pending(&RequestSender::new(tx)).unwrap(), 3);
        let mut reissued = Vec::new();
        while let Ok(NetworkRequest::SendMessage(NetworkMessage::GetCFHeaders(get))) = rx.try_recv()
        {
            reissued.push((get.start_height, get.stop_hash));
        }
        assert_eq!(
            reissued,
            vec![
                (2001, rejected),
                (4001, BlockHash::from_byte_array([0x03; 32])),
                (6001, BlockHash::from_byte_array([0x04; 32])),
            ]
        );
    }

    #[test]
    fn test_handle_timeouts_multiple_batches() {
        use std::time::Duration;

        let mut pipeline = FilterHeadersPipeline {
            coordinator: DownloadCoordinator::new(
                DownloadConfig::default().with_timeout(Duration::from_millis(1)),
            ),
            batches: HashMap::new(),
            buffered: HashMap::new(),
            next_expected: 1,
            mismatch_log: MismatchLog::default(),
            target_height: 4000,
        };

        let hash1 = BlockHash::from_byte_array([0x01; 32]);
        let hash2 = BlockHash::from_byte_array([0x02; 32]);

        pipeline.coordinator.mark_sent(&[hash1, hash2]);
        pipeline.batches.insert(hash1, 1..=2000);
        pipeline.batches.insert(hash2, 2001..=4000);

        std::thread::sleep(Duration::from_millis(5));

        pipeline.handle_timeouts();
        // Both batches re-queued
        assert_eq!(pipeline.coordinator.pending_count(), 2);
        assert!(pipeline.batches.contains_key(&hash1));
        assert!(pipeline.batches.contains_key(&hash2));
    }

    /// A rebuilt queue can end in a single-height batch whose stop hash is the
    /// stop hash of a 2000-height batch the old queue still has in flight. The
    /// stale 2000-header answer must not be taken for the new batch, and the
    /// new batch must still accept its own answer afterwards.
    #[test]
    fn test_match_response_requires_the_batch_it_names() {
        let stop = BlockHash::from_byte_array([0x16; 32]);
        let response = |count: usize| {
            NetworkMessage::CFHeaders(CFHeaders {
                filter_type: 0,
                stop_hash: stop,
                previous_filter_header: FilterHeader::all_zeros(),
                filter_hashes: vec![FilterHash::all_zeros(); count],
            })
        };

        let mut pipeline = FilterHeadersPipeline::new();
        pipeline.next_expected = 16000;
        pipeline.target_height = 16000;
        pipeline.coordinator.mark_sent(&[stop]);
        pipeline.batches.insert(stop, 16000..=16000);

        // The old queue's 14001..=16000 answer.
        assert!(pipeline.match_response(&response(2000)).is_none());
        assert!(pipeline.coordinator.is_in_flight(&stop));

        let (start, data) = pipeline.match_response(&response(1)).expect("own answer matches");
        assert_eq!((start, data.filter_hashes.len()), (16000, 1));

        // The other way round: the old queue's single-height answer is too
        // short for a 2000-height batch with the same stop hash.
        pipeline.batches.insert(stop, 14001..=16000);
        assert!(pipeline.match_response(&response(1)).is_none());
        let (start, data) = pipeline.match_response(&response(2000)).expect("own answer matches");
        assert_eq!((start, data.filter_hashes.len()), (14001, 2000));
    }
}
