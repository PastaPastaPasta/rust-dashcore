//! `merkleRootMNList`: a full recompute against [`SmlList`]'s cached tree (PLAN SB7 BG).
//!
//! `full` is a Rust baseline that follows Core's algorithm; it is not a measurement of C++ Core, so
//! the ratios are against that baseline.
//!
//! A synthetic list of 5,000 entries (mainnet held about 4,900 at its peak) in the mainnet mix of
//! versions and types. `full` hashes every entry and the tree, as Core's `CalcMerkleRoot` does each
//! block; `incremental/k` replaces `k` entries of a list whose root is cached and takes the root
//! again, as a block that confirms, bans or updates `k` masternodes does; `insert` adds one, which
//! rehashes the nodes over the leaves from its position on.

// PF0 measures production code, so a bench build with `test-utils` fails. Debug builds of the bench
// (clippy and tests with `--all-features`) still compile.
#[cfg(all(feature = "test-utils", not(debug_assertions)))]
compile_error!("dash-evo's benches must not build with `test-utils`: PF0 measures production code");

use std::sync::LazyLock;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use dash_consensus::primitives::BlockHash;
use dash_evo::sml::{SmlEntry, SmlList, merkle_root_mn_list};
use dash_evo::types::{ProTxHash, RawMnType};
use dash_primitives::{AddrV1, NetInfo, NetInfoV1, PlatformNodeId, ServiceV1};
use dash_types::Numeric;

const ENTRIES: usize = 5_000;

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0
    }

    fn bytes<const N: usize>(&mut self) -> [u8; N] {
        let mut out = [0; N];
        for chunk in out.chunks_mut(8) {
            let word = self.next().to_le_bytes();
            chunk.copy_from_slice(&word[..chunk.len()]);
        }
        out
    }
}

fn entry(rng: &mut Lcg) -> SmlEntry {
    let kind = rng.next() % 10;
    let (version, mn_type) = match kind {
        0..7 => (1, RawMnType::REGULAR),
        7..9 => (2, RawMnType::EVO),
        _ => (2, RawMnType::REGULAR),
    };
    let mut addr = [0; 16];
    addr[10..12].copy_from_slice(&[0xff, 0xff]);
    addr[12..].copy_from_slice(&rng.bytes::<4>());
    SmlEntry {
        version,
        pro_reg_tx_hash: ProTxHash::from(rng.bytes::<32>()),
        confirmed_hash: BlockHash::from_lendian(rng.bytes()),
        net_info: NetInfo::Legacy(NetInfoV1(ServiceV1 {
            addr: AddrV1(addr),
            port: 9999,
        })),
        operator_key: rng.bytes(),
        voting_key_id: rng.bytes(),
        is_valid: !rng.next().is_multiple_of(20),
        mn_type,
        platform_http_port: if mn_type.is_evo() {
            443
        } else {
            0
        },
        platform_node_id: PlatformNodeId::from_lendian(if mn_type.is_evo() {
            rng.bytes()
        } else {
            [0; 20]
        }),
    }
}

/// The entries, and their list with its root taken so the tree is cached. Built on first use, so
/// a filtered-out row builds nothing (PF0).
static FIXTURE: LazyLock<(Vec<SmlEntry>, SmlList)> = LazyLock::new(|| {
    let mut rng = Lcg(0x5b7);
    let entries: Vec<SmlEntry> = (0..ENTRIES).map(|_| entry(&mut rng)).collect();
    let mut list = SmlList::from_entries(entries.iter().cloned());
    list.merkle_root().unwrap();
    (entries, list)
});

fn sml_root(c: &mut Criterion) {
    let mut group = c.benchmark_group("sml_root");
    group.bench_function(format!("full/{ENTRIES}"), |b| {
        let (entries, _) = &*FIXTURE;
        b.iter(|| merkle_root_mn_list(entries.iter()).unwrap());
    });

    for k in [1usize, 10, 100] {
        group.bench_function(format!("incremental/{ENTRIES}/{k}"), |b| {
            let (entries, list) = &*FIXTURE;
            // Spread over the list; each flips `isValid`, so every leaf changes.
            let changed: Vec<SmlEntry> = (0..k)
                .map(|i| {
                    let mut entry = entries[i * (ENTRIES / k)].clone();
                    entry.is_valid = !entry.is_valid;
                    entry
                })
                .collect();
            b.iter_batched_ref(
                || list.clone(),
                |list| {
                    for entry in &changed {
                        list.upsert(entry.clone());
                    }
                    list.merkle_root().unwrap()
                },
                BatchSize::SmallInput,
            );
        });
    }

    group.bench_function(format!("insert/{ENTRIES}"), |b| {
        let (_, list) = &*FIXTURE;
        let new = entry(&mut Lcg(0xadd));
        b.iter_batched_ref(
            || list.clone(),
            |list| {
                list.upsert(new.clone());
                list.merkle_root().unwrap()
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

criterion_group!(benches, sml_root);
criterion_main!(benches);
