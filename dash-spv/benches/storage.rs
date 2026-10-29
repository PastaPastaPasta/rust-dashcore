use std::time::Duration;

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use dash_spv::{
    storage::{BlockHeaderStorage, DiskStorageManager, PersistentStorage, StorageManager},
    ClientConfig, Hash,
};
use dashcore::{block::Version, BlockHash, CompactTarget, Header};
use rand::{rngs::StdRng, Rng, SeedableRng};
use tempfile::TempDir;
use tokio::runtime::Builder;

fn create_test_header(height: u32) -> Header {
    Header {
        version: Version::from_consensus(1),
        prev_blockhash: BlockHash::all_zeros(),
        merkle_root: dashcore_hashes::sha256d::Hash::all_zeros().into(),
        time: height,
        bits: CompactTarget::from_consensus(0x207fffff),
        nonce: height,
    }
}

fn bench_disk_storage(c: &mut Criterion) {
    const CHUNK_SIZE: u32 = 13_000;
    const NUM_ELEMENTS: u32 = CHUNK_SIZE * 20;
    const SEED: u64 = 42;
    // The routines that keep their stores until the end of a batch hold every store's open files
    // (an SB14 store keeps up to 32 segment fds): a batch of this many stays under a 1,024-fd limit
    // whatever iteration count perfgate calibrates.
    const OPEN_STORES: BatchSize = BatchSize::NumIterations(16);

    let rt = Builder::new_multi_thread().worker_threads(4).enable_all().build().unwrap();

    let headers = (0..NUM_ELEMENTS)
        .map(create_test_header)
        .map(dash_spv::types::HashedBlockHeader::from)
        .collect::<Vec<_>>();
    let mut rng = StdRng::seed_from_u64(SEED);

    // The storage is opened in the setup, outside the measured region (BL-367): opening it runs on
    // tokio's blocking pool, and how many pool threads that spawns depends on timing, which made
    // this row's allocation count multimodal.
    c.bench_function("storage/disk/store", |b| {
        b.iter_batched(
            || {
                rt.block_on(async {
                    let dir = TempDir::new().unwrap();
                    let config = ClientConfig::testnet().with_storage_path(dir.path());
                    let mut storage = DiskStorageManager::new(&config).await.unwrap();
                    // No 5 s background persistence: when its ticks land inside the measured work
                    // is a matter of wall time, which made this bench's I/O and allocation counts
                    // bimodal.
                    storage.stop().await;
                    (dir, storage)
                })
            },
            |(dir, mut storage)| {
                rt.block_on(async {
                    for chunk in headers.chunks(CHUNK_SIZE as usize) {
                        storage.store_headers(chunk).await.unwrap();
                    }
                });
                // Dropped after the measured region, with the directory.
                (dir, storage)
            },
            OPEN_STORES,
        )
    });

    // The whole cost of making `store`'s headers durable: the same stores, then `stop()`, which
    // syncs the segments and raises the committed tip (SB14 §3). `store` is its foreground share.
    c.bench_function("storage/disk/store_durable", |b| {
        b.iter_batched(
            || {
                rt.block_on(async {
                    let dir = TempDir::new().unwrap();
                    let config = ClientConfig::testnet().with_storage_path(dir.path());
                    let mut storage = DiskStorageManager::new(&config).await.unwrap();
                    storage.stop().await;
                    (dir, storage)
                })
            },
            |(dir, mut storage)| {
                rt.block_on(async {
                    for chunk in headers.chunks(CHUNK_SIZE as usize) {
                        storage.store_headers(chunk).await.unwrap();
                    }
                    storage.stop().await;
                });
                // Dropped after the measured region, with the directory.
                (dir, storage)
            },
            OPEN_STORES,
        )
    });

    // `store` on a storage that already holds a durable chain, with linked headers (each one's
    // `prev` the one before, as sync stores them): the first half is stored and stopped
    // unmeasured, and the routine stores the second half.
    let mut prev = BlockHash::all_zeros();
    let linked = (0..2 * NUM_ELEMENTS)
        .map(|h| {
            let header = dash_spv::types::HashedBlockHeader::from(Header {
                prev_blockhash: prev,
                ..create_test_header(h)
            });
            prev = *header.hash();
            header
        })
        .collect::<Vec<_>>();
    let (held, added) = linked.split_at(NUM_ELEMENTS as usize);
    c.bench_function("storage/disk/store_warm", |b| {
        b.iter_batched(
            || {
                rt.block_on(async {
                    let dir = TempDir::new().unwrap();
                    let config = ClientConfig::testnet().with_storage_path(dir.path());
                    let mut storage = DiskStorageManager::new(&config).await.unwrap();
                    storage.stop().await;
                    for chunk in held.chunks(CHUNK_SIZE as usize) {
                        storage.store_headers(chunk).await.unwrap();
                    }
                    storage.stop().await;
                    (dir, storage)
                })
            },
            |(dir, mut storage)| {
                rt.block_on(async {
                    for chunk in added.chunks(CHUNK_SIZE as usize) {
                        storage.store_headers(chunk).await.unwrap();
                    }
                });
                // Dropped after the measured region, with the directory.
                (dir, storage)
            },
            OPEN_STORES,
        )
    });

    // Opening a storage that holds the stored headers: set up (stored, stopped and closed)
    // unmeasured, and the routine is `DiskStorageManager::new` on its directory.
    c.bench_function("storage/disk/open", |b| {
        b.iter_batched(
            || {
                rt.block_on(async {
                    let dir = TempDir::new().unwrap();
                    let config = ClientConfig::testnet().with_storage_path(dir.path());
                    let mut storage = DiskStorageManager::new(&config).await.unwrap();
                    storage.stop().await;
                    for chunk in headers.chunks(CHUNK_SIZE as usize) {
                        storage.store_headers(chunk).await.unwrap();
                    }
                    storage.stop().await;
                    drop(storage);
                    (dir, config)
                })
            },
            |(dir, config)| {
                let storage = rt.block_on(DiskStorageManager::new(&config)).unwrap();
                // Dropped after the measured region, with the directory.
                (dir, storage)
            },
            OPEN_STORES,
        )
    });

    // Writing the headers out: the stored headers are set up unmeasured and the routine is one explicit
    // `persist()`, so it measures the segment writes, renames and syncs alone (no timer decides when they run).
    // Three chunks stay within the first segment, which is written out whole.
    c.bench_function("storage/disk/persist", |b| {
        b.iter_batched(
            || {
                rt.block_on(async {
                    let dir = TempDir::new().unwrap();
                    let config = ClientConfig::testnet().with_storage_path(dir.path());
                    let mut storage = DiskStorageManager::new(&config).await.unwrap();
                    storage.stop().await;
                    for chunk in headers[..3 * CHUNK_SIZE as usize].chunks(CHUNK_SIZE as usize) {
                        storage.store_headers(chunk).await.unwrap();
                    }
                    (dir, storage)
                })
            },
            |(dir, storage)| {
                rt.block_on(async {
                    storage.block_headers().write().await.persist(dir.path()).await.unwrap();
                });
                // Dropped after the measured region, with the directory.
                (dir, storage)
            },
            OPEN_STORES,
        )
    });

    let config = ClientConfig::testnet().with_storage_path(TempDir::new().unwrap().path());

    let mut storage = rt.block_on(async {
        let mut storage = DiskStorageManager::new(&config).await.unwrap();

        for chunk in headers.chunks(CHUNK_SIZE as usize) {
            storage.store_headers(chunk).await.unwrap();
        }
        // Persisted, and no background worker ticking during the lookups below.
        storage.stop().await;

        storage
    });

    c.bench_function("storage/disk/get", |b| {
        b.to_async(&rt).iter_batched(
            || rng.random::<u32>() % NUM_ELEMENTS,
            async |height| {
                let _ = storage.get_header(height).await.unwrap();
            },
            BatchSize::SmallInput,
        )
    });

    // Its own generator, seeded like `get`'s: the lookups do not depend on how many iterations `get` ran.
    let mut rng = StdRng::seed_from_u64(SEED);
    c.bench_function("storage/disk/reverse_index", |b| {
        b.to_async(&rt).iter_batched(
            || {
                let height = rng.random::<u32>() % NUM_ELEMENTS;
                *headers[height as usize].hash()
            },
            async |hash| {
                let _ = storage.get_header_height_by_hash(&hash).await.unwrap();
            },
            BatchSize::SmallInput,
        )
    });

    rt.block_on(async {
        storage.stop().await;
    });
}

criterion_group!(
    name = disk_storage;
    config =  Criterion::default()
        .sample_size(10)
        .warm_up_time(Duration::from_secs(1));
    targets = bench_disk_storage);
criterion_main!(disk_storage);
