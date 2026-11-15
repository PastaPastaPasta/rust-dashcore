//! `merkleRootMNList` in full against the incremental cache (EV4-serve D12; PLAN EV4-serve "BG:
//! crit incremental SML root ≥10× full") over 4,000 [`testkit::numbered`] masternodes, about
//! mainnet's size:
//!
//! - `sml_root/full`: [`merkle_root_mn_list_full`], every entry projected and hashed;
//! - `sml_root/cache_hot1`: the cache moved to the next block's list, where one masternode's hot
//!   state changed (a payment: no entry changes, the common block);
//! - `sml_root/cache_cold1` and `cache_cold10`: the same with one and ten entries changed (a
//!   `confirmedHash` set), so their leaves and paths rehash.
//!
//! Each cache row starts from a clone of a cache primed with the base list, made outside the timed
//! region.
//!
//! The `mnlistdiff` group serves a `getmnlistd` reply between the base list and the next block's
//! list with ten entries changed (build and encode at 70242, no quorum changes; EV4-serve D16):
//!
//! - `mnlistdiff/cold`: both lists freshly read, so each computes its Core order (a sort of every
//!   key) inside the timed region, as a first request after a list is rebuilt does;
//! - `mnlistdiff/warm`: both lists' Core orders already computed.

use std::collections::BTreeSet;
use std::hint::black_box;
use std::sync::Arc;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use dash_dml::mnlistdiff::{
    BlsSignatureBytes, MnListDiffRequest, MnListDiffSource, QuorumBase, QuorumKey, TargetBlock,
    build_mnlistdiff,
};
use dash_dml::testkit::{fresh, numbered};
use dash_dml::{MnList, SmlRootCache, merkle_root_mn_list_full};
use dash_num::Hash256;
use dash_primitives::BlockHash;

/// About mainnet's size (≈ 3,900 masternodes at 2.3M).
const MASTERNODES: u32 = 4_000;

fn list() -> MnList {
    let mut list = MnList::new(BlockHash::from([7; 32]), 2_300_000, MASTERNODES);
    for i in 0..MASTERNODES {
        list.try_add(numbered(i), false).expect("distinct masternodes");
    }
    list
}

/// `base` with `n` spread-out masternodes' `confirmedHash` changed.
fn with_cold_changes(base: &MnList, n: u32) -> MnList {
    let mut next = base.clone();
    for k in 0..n {
        let pro_tx = numbered(k * (MASTERNODES / n.max(1)) + 1).pro_tx_hash;
        let dmn = next.get(&pro_tx).expect("listed").clone();
        let mut cold = (*dmn.cold).clone();
        cold.confirmed_hash = Hash256::from([0xc0 ^ k as u8; 32]);
        next.update(&pro_tx, dmn.hot, Arc::new(fresh(cold))).expect("cold update");
    }
    next
}

fn sml_root(c: &mut Criterion) {
    let base = list();
    let mut primed = SmlRootCache::new();
    primed.merkle_root(&base).expect("not mutated");

    let mut hot1 = base.clone();
    let pro_tx = numbered(MASTERNODES / 2).pro_tx_hash;
    let mut hot = hot1.get(&pro_tx).expect("listed").hot;
    hot.last_paid_height = 2_300_001;
    hot1.update_hot(&pro_tx, hot).expect("hot update");

    let mut group = c.benchmark_group("sml_root");
    group.bench_function("full", |b| b.iter(|| black_box(merkle_root_mn_list_full(&base))));
    for (id, next) in [
        ("cache_hot1", hot1),
        ("cache_cold1", with_cold_changes(&base, 1)),
        ("cache_cold10", with_cold_changes(&base, 10)),
    ] {
        group.bench_function(id, |b| {
            b.iter_batched(
                || primed.clone(),
                |mut cache| {
                    let root = cache.merkle_root(&next);
                    (root, cache)
                },
                BatchSize::LargeInput,
            )
        });
    }
    group.finish();
}

/// A chain with no quorum changes between the two blocks; the target block has 200
/// transactions.
struct NoQuorums(TargetBlock);

impl MnListDiffSource for NoQuorums {
    type Error = ();
    fn active_quorums(&self, _: &BlockHash) -> Result<BTreeSet<QuorumKey>, ()> {
        Ok(BTreeSet::new())
    }
    fn mined_commitment(&self, _: &QuorumKey) -> Result<Option<Vec<u8>>, ()> {
        Ok(None)
    }
    fn quorum(&self, _: &QuorumKey) -> Result<Option<QuorumBase>, ()> {
        Ok(None)
    }
    fn coinbase_chainlock(&self, _: &BlockHash, _: i32) -> Result<Option<BlsSignatureBytes>, ()> {
        Ok(None)
    }
    fn target_block(&self, _: &BlockHash) -> Option<TargetBlock> {
        Some(self.0.clone())
    }
}

fn mnlistdiff(c: &mut Criterion) {
    let base = list();
    let next = with_cold_changes(&base, 10);
    let source = NoQuorums(TargetBlock {
        coinbase: vec![0; 250],
        txids: (0..200u32).map(|i| [i as u8; 32]).collect(),
    });
    let serve = |base: &MnList, next: &MnList| {
        let request = MnListDiffRequest {
            requested_base: *base.block_hash(),
            base_block: *base.block_hash(),
            base_list: base,
            block: *next.block_hash(),
            list: next,
            v20_active_after: true,
        };
        build_mnlistdiff(&request, &source).expect("no quorums").to_bytes(70242)
    };

    let mut group = c.benchmark_group("mnlistdiff");
    // `base` and `next` never compute their orders: each clone starts without one.
    group.bench_function("cold", |b| {
        b.iter_batched(
            || (base.clone(), next.clone()),
            |(base, next)| {
                let reply = serve(&base, &next);
                (reply, base, next)
            },
            BatchSize::LargeInput,
        )
    });
    let (warm_base, warm_next) = (base.clone(), next.clone());
    warm_base.core_order();
    warm_next.core_order();
    group.bench_function("warm", |b| b.iter(|| black_box(serve(&warm_base, &warm_next))));
    group.finish();
}

criterion_group!(benches, sml_root, mnlistdiff);
criterion_main!(benches);
