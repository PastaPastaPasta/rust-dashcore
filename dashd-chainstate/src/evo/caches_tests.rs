use std::sync::Arc;

use dash_dml::creditpool::CreditPool;
use dash_dml::store::MemEvoStore;
use dash_dml::testkit::MemChain;

use super::*;
use crate::evo::testkit;

fn hash(n: u32) -> BlockHash {
    let mut bytes = [0; 32];
    bytes[..4].copy_from_slice(&n.to_le_bytes());
    BlockHash::from(bytes)
}

/// `unordered_lru_cache`: nothing is dropped up to twice the size; one more keeps the `size`
/// most recently stamped, where a hit restamps and a miss does not.
#[test]
fn the_lru_keeps_the_most_recently_stamped_past_twice_its_size() {
    let mut lru = UnorderedLru::new(3);
    for n in 0..6 {
        lru.insert(hash(n), n);
    }
    assert_eq!(lru.len(), 6);
    // Restamp 0; a miss stamps nothing.
    assert_eq!(lru.get(&hash(0)), Some(0));
    assert_eq!(lru.get(&hash(99)), None);
    lru.insert(hash(6), 6);
    assert_eq!(lru.len(), 3);
    let kept: Vec<_> = (0..7).filter(|n| lru.get(&hash(*n)).is_some()).collect();
    assert_eq!(kept, [0, 5, 6]);
}

/// §3.7: below V20 the empty pool, uncached; above it the store's pool, kept, so a hit gives the
/// store's answer.
#[test]
fn a_pool_is_read_once_and_kept_above_v20() {
    let mut chain = MemChain::new();
    let b1 = chain.extend(chain.genesis(), 1);
    let pool = CreditPool {
        locked: 7,
        ..CreditPool::default()
    };
    let mut store = MemEvoStore::new();
    let mut delta = testkit::delta(&chain, b1);
    delta.credit_pool = Some(Arc::new(Ok(pool.clone())));
    store.apply(&delta).expect("applied");
    let view = store.view(&chain);
    let cache = CreditPoolCache::default();
    let h1 = chain.hash(b1);

    assert_eq!(cache.pool_after(&view, &h1, false), Ok(Ok(CreditPool::default())));
    assert_eq!(cache.lru().len(), 0);
    assert_eq!(cache.pool_after(&view, &h1, true), Ok(Ok(pool.clone())));
    assert_eq!(cache.lru().len(), 1);
    assert_eq!(cache.pool_after(&view, &h1, true), Ok(Ok(pool)));
    assert_eq!(cache.lru().len(), 1);
}
