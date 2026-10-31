//! The PF0 rows of the masternode list (EV2 §6.5; PLAN EV2a "BG: crit list clone/diff"):
//! `mnlist/clone_diff`, `mnlist/unique_probe`, `mnlist/unique_probe_prekeyed`,
//! `mnlist/codec_decode` and `mnlist/codec_hash` over a mainnet-sized list of
//! [`testkit::numbered`] masternodes (D14: synthetic until O8's canonical mainnet lists exist as
//! vectors).
//!
//! `unique_probe` builds each [`UniqueKey`] inside the timed loop, so it includes the key's
//! allocation and serialization (what a caller that holds an outpoint pays);
//! `unique_probe_prekeyed` probes the same 1,000 outpoints with the keys built once outside the
//! loop, so it measures `has_unique` alone (the hash and the lookup). The two rows together
//! split the cost.

use std::hint::black_box;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use dash_dml::codec::{
    ContextFreeNetInfo, canonical_mn_list_hash, decode_canonical_list,
    encode_canonical_list_unvalidated,
};
use dash_dml::testkit::numbered;
use dash_dml::{MnList, UniqueKey};
use dash_primitives::{BlockHash, OutPoint, TxHash};

/// About mainnet's size (≈ 3,900 masternodes at 2.3M).
const MASTERNODES: u32 = 4_000;
/// The updates per `clone_diff` iteration (EV2 §6.5: a block's worth and more).
const UPDATES: usize = 100;
/// The probes per `unique_probe` iteration, half of them hits.
const PROBES: u32 = 1_000;

fn list() -> MnList {
    let mut list = MnList::new(BlockHash::from([7; 32]), 2_300_000, MASTERNODES);
    for i in 0..MASTERNODES {
        list.try_add(numbered(i), false).expect("distinct masternodes");
    }
    list
}

/// A fixed sequence of masternode indexes (xorshift), the same in every process.
fn picks(n: usize) -> Vec<u32> {
    let mut x = 0x2545_f491_4f6c_dd1du64;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % u64::from(MASTERNODES)) as u32
        })
        .collect()
}

fn mnlist(c: &mut Criterion) {
    let base = list();
    let mut group = c.benchmark_group("mnlist");

    let targets: Vec<_> = picks(UPDATES).into_iter().map(|i| numbered(i).pro_tx_hash).collect();
    group.bench_function("clone_diff", |b| {
        b.iter(|| {
            let mut next = base.clone();
            for (n, pro_tx) in targets.iter().enumerate() {
                let mut hot = next.get(pro_tx).expect("listed").hot;
                hot.last_paid_height = 2_300_001 + n as i32;
                next.update_hot(pro_tx, hot).expect("hot update");
            }
            black_box(base.diff_to(&next))
        })
    });

    let probes: Vec<OutPoint> = picks(PROBES as usize)
        .into_iter()
        .enumerate()
        .map(|(n, i)| match n % 2 {
            0 => numbered(i).collateral,
            _ => OutPoint {
                hash: TxHash::from([n as u8; 32]),
                index: i,
            },
        })
        .collect();
    group.bench_function("unique_probe", |b| {
        b.iter(|| {
            let hits =
                probes.iter().filter(|op| base.has_unique(&UniqueKey::collateral(op))).count();
            black_box(hits)
        })
    });

    // The same probes with the keys built up front: only `has_unique` is measured.
    let keys: Vec<UniqueKey> = probes.iter().map(UniqueKey::collateral).collect();
    group.bench_function("unique_probe_prekeyed", |b| {
        b.iter(|| {
            let hits = keys.iter().filter(|key| base.has_unique(key)).count();
            black_box(hits)
        })
    });

    let mut bytes = Vec::new();
    encode_canonical_list_unvalidated(&base, &mut bytes);
    group.bench_function("codec_decode", |b| {
        b.iter_batched(
            || bytes.as_slice(),
            |mut input| {
                black_box(decode_canonical_list(&mut input, &ContextFreeNetInfo).expect("decodes"))
            },
            BatchSize::SmallInput,
        )
    });
    group.bench_function("codec_hash", |b| b.iter(|| black_box(canonical_mn_list_hash(&base))));
    group.finish();
}

criterion_group!(benches, mnlist);
criterion_main!(benches);
