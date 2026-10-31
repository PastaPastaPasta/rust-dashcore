use dash_evo::llmq::reference::{LruCache, PerTypeCache};
use dash_params::{ChainParamsBuilder, NetworkKind, Release};
use proptest::prelude::*;

use super::*;

fn regtest() -> ChainParams {
    ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().expect("regtest")
}

/// The chain's configured types, then one it does not configure.
fn types(params: &ChainParams) -> (Vec<LlmqType>, LlmqType) {
    let configured: Vec<LlmqType> = params.consensus.llmqs.iter().map(|p| p.llmq_type).collect();
    let other = (0..=u8::MAX)
        .map(LlmqType::new)
        .find(|t| !configured.contains(t))
        .expect("a type the chain does not configure");
    (configured, other)
}

/// `unordered_lru_cache`: nothing is dropped up to twice the size; one more keeps the `size`
/// most recently stamped, where a hit restamps and a miss does not.
#[test]
fn the_lru_keeps_the_most_recently_stamped_past_twice_its_size() {
    let mut lru = Lru::new(3);
    for n in 0..6u32 {
        lru.insert(n, n);
    }
    assert_eq!(lru.len(), 6);
    // Restamp 0; a miss stamps nothing.
    assert_eq!(lru.get(&0), Some(&0));
    assert_eq!(lru.get(&99), None);
    lru.insert(6, 6);
    assert_eq!(lru.len(), 3);
    let kept: Vec<_> = (0..7).filter(|n| lru.peek(n).is_some()).collect();
    assert_eq!(kept, [0, 5, 6]);
}

/// `get`, `exists` and `insert` take the next stamp; a miss and `peek` take none.
#[test]
fn get_exists_and_insert_stamp_and_peek_does_not() {
    let mut lru = Lru::new(4);
    lru.insert('a', 1);
    lru.insert('b', 2);
    assert_eq!((lru.stamp(&'a'), lru.stamp(&'b')), (Some(0), Some(1)));
    assert_eq!(lru.peek(&'a'), Some(&1));
    assert_eq!(lru.stamp(&'a'), Some(0));
    assert_eq!(lru.get(&'a'), Some(&1));
    assert_eq!(lru.stamp(&'a'), Some(2));
    assert!(lru.exists(&'b'));
    assert_eq!(lru.stamp(&'b'), Some(3));
    assert!(!lru.exists(&'z'));
    assert_eq!(lru.get(&'z'), None);
    lru.insert('c', 3);
    assert_eq!(lru.stamp(&'c'), Some(4));
}

/// `_emplace` on a present key overwrites the value and restamps it.
#[test]
fn an_insert_overwrites_and_restamps() {
    let mut lru = Lru::new(1);
    lru.insert('a', 1);
    lru.insert('b', 2);
    lru.insert('a', 3);
    assert_eq!(lru.peek(&'a'), Some(&3));
    assert_eq!(lru.stamp(&'a'), Some(2));
    // Past twice the size, the newest stamp is the one kept.
    lru.insert('c', 4);
    assert_eq!(lru.len(), 1);
    assert_eq!(lru.peek(&'c'), Some(&4));
}

/// `clear` drops the entries; the access counter runs on.
#[test]
fn clear_keeps_the_access_counter() {
    let mut lru = Lru::new(2);
    lru.insert('a', 1);
    lru.insert('b', 2);
    lru.clear();
    assert!(lru.is_empty());
    lru.insert('a', 1);
    assert_eq!(lru.stamp(&'a'), Some(2));
}

/// `Init`: one cache per configured type, sized `keepOldConnections`, `keepOldKeys` or by the size
/// function.
#[test]
fn each_configured_type_gets_its_size() {
    let params = regtest();
    let (configured, other) = types(&params);
    assert!(!configured.is_empty());
    let connections = PerTypeLru::<u32, u32>::new(&params, true).expect("in domain");
    let keys = PerTypeLru::<u32, u32>::new(&params, false).expect("in domain");
    // quorumsman.cpp:211-213's scan-cache size.
    let scans = PerTypeLru::<u32, u32>::sized(&params, |p| {
        i64::from(p.max_cycles(p.keep_old_connections))
            * (i64::from(p.dkg_mining_window_end) - i64::from(p.dkg_mining_window_start))
    })
    .expect("in domain");
    for p in &params.consensus.llmqs {
        let size = |c: &PerTypeLru<u32, u32>| c.cache(p.llmq_type).map(Lru::max_size);
        let n = |v: i32| Some(usize::try_from(v).expect("positive"));
        assert_eq!(size(&connections), n(p.keep_old_connections));
        assert_eq!(size(&keys), n(p.keep_old_keys));
        let window = p.dkg_mining_window_end - p.dkg_mining_window_start;
        assert_eq!(size(&scans), n(p.max_cycles(p.keep_old_connections) * window));
    }
    assert!(connections.cache(other).is_none());
}

/// A type the chain does not configure has no cache: a read misses and a write is dropped. Clearing
/// one type leaves the others.
#[test]
fn an_unconfigured_type_is_dropped_and_clear_is_per_type() {
    let params = regtest();
    let (configured, other) = types(&params);
    let mut cache = PerTypeLru::new(&params, true).expect("in domain");
    cache.insert(other, 1u32, 'x');
    assert_eq!(cache.get(other, &1), None);
    let (a, b) = (configured[0], configured[1]);
    cache.insert(a, 1, 'a');
    cache.insert(b, 1, 'b');
    let stamp = cache.cache(b).and_then(|c| c.stamp(&1));
    cache.clear(a);
    assert_eq!(cache.peek(a, &1), None);
    assert_eq!(cache.peek(b, &1), Some(&'b'));
    assert_eq!(cache.cache(b).and_then(|c| c.stamp(&1)), stamp);
}

/// A size below one is outside the domain: the first such type is the error.
#[test]
fn a_size_below_one_is_out_of_domain() {
    let params = regtest();
    let first = params.consensus.llmqs[0].llmq_type;
    assert_eq!(PerTypeLru::<u32, u32>::sized(&params, |_| 0).err(), Some(ParamsOutOfDomain(first)));
    assert_eq!(
        PerTypeLru::<u32, u32>::sized(&params, |_| -1).err(),
        Some(ParamsOutOfDomain(first))
    );
}

#[derive(Clone, Debug)]
enum Op {
    Insert(u8, u32),
    Get(u8),
    Exists(u8),
    Peek(u8),
    Clear,
}

fn op() -> impl Strategy<Value = Op> {
    let key = 0u8..10;
    prop_oneof![
        3 => (key.clone(), any::<u32>()).prop_map(|(k, v)| Op::Insert(k, v)),
        2 => key.clone().prop_map(Op::Get),
        1 => key.clone().prop_map(Op::Exists),
        1 => key.prop_map(Op::Peek),
        1 => Just(Op::Clear),
    ]
}

proptest! {
    /// The node's LRU and the reference model agree on every answer and on what they keep.
    #[test]
    fn the_lru_matches_the_reference_model(size in 1usize..5, ops in prop::collection::vec(op(), 0..80)) {
        let mut node = Lru::new(size);
        let mut model = LruCache::new(size);
        for op in ops {
            match op {
                Op::Insert(k, v) => {
                    node.insert(k, v);
                    model.insert(k, v);
                }
                Op::Get(k) => prop_assert_eq!(node.get(&k).copied(), model.get(&k)),
                Op::Exists(k) => prop_assert_eq!(node.exists(&k), model.exists(&k)),
                Op::Peek(k) => prop_assert_eq!(node.peek(&k), model.peek(&k)),
                Op::Clear => {
                    node.clear();
                    model.clear();
                }
            }
            prop_assert_eq!(node.len(), model.len());
        }
        for k in 0..10 {
            prop_assert_eq!(node.peek(&k), model.peek(&k));
        }
    }

    /// The per-type caches agree with the reference model, a type the chain does not configure
    /// included.
    #[test]
    fn the_per_type_cache_matches_the_reference_model(
        ops in prop::collection::vec((0usize..4, op()), 0..120),
    ) {
        let params = regtest();
        let (configured, other) = types(&params);
        // Three configured types and the unconfigured one.
        let pick = |i: usize| if i == 3 { other } else { configured[i % configured.len()] };
        let size = |p: &LlmqParams| p.keep_old_connections.min(3);
        let mut node = PerTypeLru::sized(&params, |p| i64::from(size(p))).expect("in domain");
        let mut model = PerTypeCache::sized(&params, size);
        for (i, op) in ops {
            let t = pick(i);
            match op {
                Op::Insert(k, v) => {
                    node.insert(t, k, v);
                    model.insert(t, k, v);
                }
                Op::Get(k) | Op::Exists(k) => {
                    prop_assert_eq!(node.get(t, &k).copied(), model.get(t, &k));
                }
                Op::Peek(k) => prop_assert_eq!(node.peek(t, &k), model.peek(t, &k)),
                Op::Clear => {
                    node.clear(t);
                    model.clear(t);
                }
            }
        }
        for t in configured.iter().copied().chain([other]) {
            for k in 0..10 {
                prop_assert_eq!(node.peek(t, &k), model.peek(t, &k));
            }
        }
    }
}
