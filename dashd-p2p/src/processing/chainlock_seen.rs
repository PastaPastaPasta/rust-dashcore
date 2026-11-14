use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use dash_clock::{Clock, NodeSeconds};
use parking_lot::Mutex;

/// Core's number of entries retained after a prune.
pub const SEEN_CHAINLOCKS_CUTOFF_SIZE: usize = 1024;
/// Core's seen-map size that triggers a prune when exceeded.
pub const SEEN_CHAINLOCKS_MAX_SIZE: usize = 2 * SEEN_CHAINLOCKS_CUTOFF_SIZE;
/// How long a seen hash is kept, in seconds.
pub const CLEANUP_SEEN_TIMEOUT: i64 = 24 * 60 * 60;

#[derive(Default)]
struct SeenShared {
    seen: Mutex<Seen>,
    /// The cleanup throttle's next node-clock time in microseconds; initially Core's epoch.
    next_cleanup: AtomicI64,
}

#[derive(Default)]
struct Seen {
    first_seen: HashMap<[u8; 32], (NodeSeconds, u64)>,
    next_order: u64,
}

/// Seen hashes and their first-seen times, shared with the cleanup task.
#[derive(Clone)]
pub struct SeenChainLocksMap {
    shared: Arc<SeenShared>,
    clock: Arc<dyn Clock>,
}

impl SeenChainLocksMap {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            shared: Arc::default(),
            clock,
        }
    }

    pub fn len(&self) -> usize {
        self.shared.seen.lock().first_seen.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn contains(&self, hash: &[u8; 32]) -> bool {
        self.shared.seen.lock().first_seen.contains_key(hash)
    }

    /// Returns whether the hash was newly inserted.
    pub fn insert(&self, hash: [u8; 32]) -> bool {
        let mut seen = self.shared.seen.lock();
        let order = seen.next_order;
        let Entry::Vacant(slot) = seen.first_seen.entry(hash) else {
            return false;
        };
        slot.insert((self.clock.node_now(), order));
        seen.next_order = order.wrapping_add(1);
        if seen.first_seen.len() > SEEN_CHAINLOCKS_MAX_SIZE {
            // Core: limitedmap.h:90 unordered_limitedmap::prune
            // Core leaves equal timestamps unordered; insertion order makes ties deterministic.
            let mut entries: Vec<_> = seen.first_seen.iter().map(|(h, t)| (*h, *t)).collect();
            let evict = entries.len() - SEEN_CHAINLOCKS_CUTOFF_SIZE;
            entries.select_nth_unstable_by_key(evict, |(_, t)| *t);
            for (hash, _) in &entries[..evict] {
                seen.first_seen.remove(hash);
            }
        }
        true
    }

    pub fn cleanup(&self) -> SeenChainLocksCleanup {
        SeenChainLocksCleanup(self.clone())
    }
}

/// The seen-map half of Core's `ChainlockHandler::Cleanup`.
#[derive(Clone)]
pub struct SeenChainLocksCleanup(SeenChainLocksMap);

impl SeenChainLocksCleanup {
    /// The throttle's next cleanup time in microseconds, or zero before its first run.
    pub fn next_cleanup_micros(&self) -> i64 {
        self.0.shared.next_cleanup.load(Ordering::Relaxed)
    }

    /// Forgets hashes seen at least 24 hours ago, only when synced and at most every 30 seconds.
    // Core: chainlock/handler.cpp:334 Cleanup
    pub fn run(&self, blockchain_synced: bool) {
        const CLEANUP_INTERVAL_MICROS: i64 = 30_000_000;
        if !blockchain_synced {
            return;
        }
        let now = self.0.clock.node_now_micros();
        let next = self.next_cleanup_micros();
        if now < next
            || self
                .0
                .shared
                .next_cleanup
                .compare_exchange(
                    next,
                    now.saturating_add(CLEANUP_INTERVAL_MICROS),
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .is_err()
        {
            return;
        }
        let now = self.0.clock.node_now();
        self.0
            .shared
            .seen
            .lock()
            .first_seen
            .retain(|_, (t, _)| now.0.saturating_sub(t.0) < CLEANUP_SEEN_TIMEOUT);
    }
}
