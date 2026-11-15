use std::sync::Arc;

use dash_clock::MockClock;
use dashd_p2p::processing::chainlock_seen::{
    CLEANUP_SEEN_TIMEOUT, SEEN_CHAINLOCKS_CUTOFF_SIZE, SEEN_CHAINLOCKS_MAX_SIZE, SeenChainLocksMap,
};

fn hash(i: usize) -> [u8; 32] {
    let mut h = [0; 32];
    h[..8].copy_from_slice(&(i as u64).to_le_bytes());
    h
}

#[test]
fn prune_evicts_smallest_time_when_mock_time_goes_backwards() {
    let clock = Arc::new(MockClock::new());
    let seen = SeenChainLocksMap::new(clock.clone());
    assert!(seen.is_empty());
    for i in 0..SEEN_CHAINLOCKS_MAX_SIZE {
        clock.set_mocktime(10_000 - i as i64);
        assert!(seen.insert(hash(i)));
    }
    assert_eq!(seen.len(), SEEN_CHAINLOCKS_MAX_SIZE);
    assert!(!seen.insert(hash(0)));
    assert_eq!(seen.len(), SEEN_CHAINLOCKS_MAX_SIZE);
    clock.set_mocktime(1);
    seen.insert(hash(SEEN_CHAINLOCKS_MAX_SIZE));
    assert_eq!(seen.len(), SEEN_CHAINLOCKS_CUTOFF_SIZE);
    for i in 0..=SEEN_CHAINLOCKS_MAX_SIZE {
        assert_eq!(seen.contains(&hash(i)), i < SEEN_CHAINLOCKS_CUTOFF_SIZE, "{i}");
    }
}

#[test]
fn cleanup_is_synced_throttled_and_expires_at_timeout() {
    let clock = Arc::new(MockClock::new());
    clock.set_mocktime(100);
    let seen = SeenChainLocksMap::new(clock.clone());
    let cleanup = seen.cleanup();
    seen.insert(hash(0));
    clock.set_mocktime(100 + CLEANUP_SEEN_TIMEOUT - 1);
    cleanup.run(false);
    assert_eq!(cleanup.next_cleanup_micros(), 0);
    cleanup.run(true);
    assert!(seen.contains(&hash(0)));
    clock.set_mocktime(100 + CLEANUP_SEEN_TIMEOUT);
    cleanup.run(true);
    assert!(seen.contains(&hash(0)));
    clock.set_mocktime(100 + CLEANUP_SEEN_TIMEOUT + 29);
    cleanup.run(true);
    assert!(!seen.contains(&hash(0)));
    assert!(seen.is_empty());
}

#[test]
fn duplicate_keeps_first_seen_and_backwards_cleanup_keeps_future_entry() {
    let clock = Arc::new(MockClock::new());
    clock.set_mocktime(100);
    let seen = SeenChainLocksMap::new(clock.clone());
    seen.insert(hash(0));
    clock.set_mocktime(1);
    seen.cleanup().run(true);
    assert!(seen.contains(&hash(0)));
    clock.set_mocktime(100 + CLEANUP_SEEN_TIMEOUT);
    assert!(!seen.insert(hash(0)));
    seen.cleanup().run(true);
    assert!(!seen.contains(&hash(0)));
}

#[test]
fn cleanup_expires_at_exact_timeout() {
    let clock = Arc::new(MockClock::new());
    clock.set_mocktime(100);
    let seen = SeenChainLocksMap::new(clock.clone());
    seen.insert(hash(0));
    clock.set_mocktime(101);
    seen.insert(hash(1));
    clock.set_mocktime(100 + CLEANUP_SEEN_TIMEOUT);
    seen.cleanup().run(true);
    assert!(!seen.contains(&hash(0)));
    assert!(seen.contains(&hash(1)));
}
