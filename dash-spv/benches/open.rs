use std::sync::Arc;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use dash_spv::test_utils::{
    BlockHeaderStorage, DiskStorageManager, MockNetworkManager, StorageManager,
};
use dash_spv::types::HashedBlockHeader;
use dash_spv::{ClientConfig, DashSpvClient};
use dashcore::{block::Version, BlockHash, CompactTarget, Header};
use key_wallet::wallet::managed_wallet_info::ManagedWalletInfo;
use key_wallet_manager::WalletManager;
use tempfile::TempDir;
use tokio::runtime::{Builder, Runtime};
use tokio::sync::RwLock;

/// About mainnet's header count.
const HEADERS: u32 = 2_550_000;
/// Headers per `store_headers` call while the store is built: one segment file's worth.
const CHUNK: u32 = 10_000;

type Client = DashSpvClient<WalletManager<ManagedWalletInfo>, MockNetworkManager>;

fn header(height: u32, prev_blockhash: BlockHash) -> Header {
    Header {
        version: Version::from_consensus(1),
        prev_blockhash,
        merkle_root: dashcore_hashes::sha256d::Hash::from_byte_array([0; 32]).into(),
        time: height,
        bits: CompactTarget::from_consensus(0x207fffff),
        nonce: height,
    }
}

/// A stopped storage holding `HEADERS` linked headers from height 0, built a chunk at a time so
/// the chain is never in memory whole, and its config. A client opened on it serves the tip.
fn synced_store(rt: &Runtime) -> (TempDir, ClientConfig) {
    let dir = TempDir::new().unwrap();
    let config = ClientConfig::testnet().with_storage_path(dir.path());
    rt.block_on(async {
        let mut storage = DiskStorageManager::new(&config).await.unwrap();
        storage.stop().await;
        let mut prev = BlockHash::all_zeros();
        for start in (0..HEADERS).step_by(CHUNK as usize) {
            let chunk = (start..(start + CHUNK).min(HEADERS))
                .map(|h| {
                    let hashed = HashedBlockHeader::from(header(h, prev));
                    prev = *hashed.hash();
                    hashed
                })
                .collect::<Vec<_>>();
            storage.store_headers(&chunk).await.unwrap();
        }
        storage.stop().await;
    });
    let client = open(rt, inputs(&config));
    let tip = rt.block_on(client.tip_header()).map(|tip| tip.height());
    assert_eq!(tip, Some(HEADERS - 1));
    (dir, config)
}

type Inputs = (ClientConfig, MockNetworkManager, Arc<RwLock<WalletManager<ManagedWalletInfo>>>);

fn inputs(config: &ClientConfig) -> Inputs {
    let wallet = Arc::new(RwLock::new(WalletManager::new(config.network)));
    (config.clone(), MockNetworkManager::new(), wallet)
}

fn open(rt: &Runtime, (config, network, wallet): Inputs) -> Client {
    rt.block_on(DashSpvClient::new(config, network, wallet, vec![])).unwrap()
}

/// Open to ready over a synced store: `DashSpvClient::new` on a stopped storage holding
/// `HEADERS` linked headers, set up (stored, stopped and closed) unmeasured. Ready is `new`
/// returning: the storage open (its read of every segment file and the hash index it builds),
/// the tip read and the sync managers built from it, with the progress the client serves
/// before `start`. A drop writes nothing, so every iteration opens the same files; the client
/// is dropped after the measured region. The store is built once, on the first run of the bench.
fn bench_open(c: &mut Criterion) {
    let rt = Builder::new_multi_thread().worker_threads(4).enable_all().build().unwrap();
    let mut store = None;
    c.bench_function("open/ready/2550000", |b| {
        let (_, config) = store.get_or_insert_with(|| synced_store(&rt));
        b.iter_batched(|| inputs(config), |inputs| open(&rt, inputs), BatchSize::PerIteration)
    });
}

criterion_group!(
    name = open_to_ready;
    config = Criterion::default()
        .sample_size(10)
        .warm_up_time(Duration::from_secs(1));
    targets = bench_open);
criterion_main!(open_to_ready);
