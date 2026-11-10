use std::sync::Arc;

use dash_dml::creditpool::CreditPool;
use dash_dml::store::MemEvoStore;
use dash_dml::testkit::MemChain;

use super::*;
use crate::evo::testkit;

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
