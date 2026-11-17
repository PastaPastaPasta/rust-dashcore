use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Duration;

use dashcore::{block::Header, BlockHash};
use serde_json::{json, Value};
use tempfile::TempDir;

use super::*;

fn config(path: &Path) -> ClientConfig {
    ClientConfig::testnet().with_storage_path(path)
}

fn chain() -> Vec<HashedBlockHeader> {
    Header::dummy_chain(8, BlockHash::all_zeros())
        .into_iter()
        .map(HashedBlockHeader::from)
        .collect()
}

fn version(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path.join("version.json")).unwrap()).unwrap()
}

fn write_version(path: &Path, value: &Value) {
    std::fs::write(path.join("version.json"), serde_json::to_vec(value).unwrap()).unwrap();
}

fn damage(path: &Path, offset: usize) {
    let file = path.join("block_headers/segment_000000.dat");
    let mut bytes = std::fs::read(&file).unwrap();
    bytes[offset] ^= 1;
    std::fs::write(file, bytes).unwrap();
}

async fn populated(path: &Path) {
    let mut storage = DiskStorageManager::new(&config(path)).await.unwrap();
    storage.store_headers(&chain()).await.unwrap();
    storage.store().run(|s| s.sync_wal()).await.unwrap();
}

async fn damaged(path: &Path) -> DiskStorageManager {
    populated(path).await;
    damage(path, 5 * 112 + 40);
    DiskStorageManager::new(&config(path)).await.unwrap()
}

fn assert_clean(value: &Value, generation: u64) {
    assert_eq!(value.get("generation").and_then(Value::as_u64).unwrap_or(0), generation);
    assert!(value.get("mismatch").is_none(), "{value}");
    assert!(value.get("clearing").is_none(), "{value}");
}

#[tokio::test]
async fn a_read_mismatch_clears_at_the_next_open() {
    let tmp = TempDir::new().unwrap();
    let storage = damaged(tmp.path()).await;
    assert!(matches!(storage.get_header(5).await, Err(StorageError::Corruption(_))));
    assert_eq!(version(tmp.path())["mismatch"], 0);
    drop(storage);
    let reopened = DiskStorageManager::new(&config(tmp.path())).await.unwrap();
    assert_eq!(reopened.get_tip_height().await, None);
    assert_clean(&version(tmp.path()), 1);
}

#[tokio::test]
async fn a_corrupt_layout_clear_keeps_the_reopened_generation() {
    let tmp = TempDir::new().unwrap();
    populated(tmp.path()).await;
    damage(tmp.path(), 5 * 112 + 4);
    let mut storage = DiskStorageManager::new(&config(tmp.path())).await.unwrap();
    assert_eq!(storage.get_tip_height().await, None);
    assert_clean(&version(tmp.path()), 1);
    assert_eq!(storage.store().store().generation(), 1);
    storage.store_headers(&chain()).await.unwrap();
    storage.store().run(|s| s.sync_wal()).await.unwrap();
    drop(storage);
    damage(tmp.path(), 5 * 112 + 40);
    let storage = DiskStorageManager::new(&config(tmp.path())).await.unwrap();
    assert!(matches!(storage.get_header(5).await, Err(StorageError::Corruption(_))));
    assert_eq!(version(tmp.path())["mismatch"], 1);
    drop(storage);
    let reopened = DiskStorageManager::new(&config(tmp.path())).await.unwrap();
    assert_eq!(reopened.get_tip_height().await, None);
    assert_clean(&version(tmp.path()), 2);
}

#[tokio::test]
async fn a_stale_mismatch_does_not_mark_a_rebuilt_store() {
    for force_damaged in [false, true] {
        let tmp = TempDir::new().unwrap();
        let mut storage = damaged(tmp.path()).await;
        let old = storage.block_headers();
        let store = storage.store().clone();
        let (copied_tx, copied_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *store.store().integrity.after_copy.lock().unwrap() = Some(Box::new(move || {
            copied_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        }));
        let read = tokio::spawn(async move { old.read().await.get_header(5).await });
        copied_rx.await.unwrap();
        storage.clear().await.unwrap();
        storage.store_headers(&chain()).await.unwrap();
        storage.store().run(|s| s.sync_wal()).await.unwrap();
        store.store().integrity.force_damaged.store(force_damaged, Ordering::Relaxed);
        release_tx.send(()).unwrap();
        assert!(matches!(read.await.unwrap(), Err(StorageError::Corruption(_))));
        assert_clean(&version(tmp.path()), 1);
        drop(store);
        drop(storage);
        let reopened = DiskStorageManager::new(&config(tmp.path())).await.unwrap();
        assert_eq!(reopened.get_tip_height().await, Some(7));
        assert_eq!(reopened.get_header(5).await.unwrap().unwrap().hash(), chain()[5].hash());
    }
}

#[tokio::test]
async fn damage_in_memory_only_does_not_mark() {
    let tmp = TempDir::new().unwrap();
    populated(tmp.path()).await;
    let storage = DiskStorageManager::new(&config(tmp.path())).await.unwrap();
    storage.store().store().damage_resident(5).unwrap();
    assert!(matches!(storage.get_header(5).await, Err(StorageError::Corruption(_))));
    assert_clean(&version(tmp.path()), 0);
}

#[tokio::test]
async fn an_uncommitted_mismatch_does_not_mark() {
    let tmp = TempDir::new().unwrap();
    let mut storage = DiskStorageManager::new(&config(tmp.path())).await.unwrap();
    storage.store_headers(&chain()).await.unwrap();
    storage.store().store().damage_resident(5).unwrap();
    storage.store().store().integrity.force_damaged.store(true, Ordering::Relaxed);
    assert!(matches!(storage.get_header(5).await, Err(StorageError::Corruption(_))));
    assert_clean(&version(tmp.path()), 0);
}

#[tokio::test]
async fn a_failed_intent_write_does_not_suppress_the_next() {
    let tmp = TempDir::new().unwrap();
    let storage = damaged(tmp.path()).await;
    storage.store().store().integrity.fail_mark.store(true, Ordering::Relaxed);
    assert!(matches!(storage.get_header(5).await, Err(StorageError::Corruption(_))));
    assert_clean(&version(tmp.path()), 0);
    assert!(matches!(storage.get_header(5).await, Err(StorageError::Corruption(_))));
    assert_eq!(version(tmp.path())["mismatch"], 0);
    assert_eq!(storage.store().store().integrity.writes.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn a_refused_range_leaves_no_intent() {
    let tmp = TempDir::new().unwrap();
    populated(tmp.path()).await;
    let storage = DiskStorageManager::new(&config(tmp.path())).await.unwrap();
    assert!(storage.load_headers(0..9).await.is_err());
    assert_clean(&version(tmp.path()), 0);
}

#[tokio::test]
async fn concurrent_mismatches_write_one_intent() {
    let tmp = TempDir::new().unwrap();
    let storage = damaged(tmp.path()).await;
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let headers = storage.block_headers();
        tasks.push(tokio::spawn(async move { headers.read().await.get_header(5).await }));
    }
    for task in tasks {
        assert!(matches!(task.await.unwrap(), Err(StorageError::Corruption(_))));
    }
    assert_eq!(storage.store().store().integrity.writes.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn a_direct_open_refuses_a_mismatch_intent_unchanged() {
    let tmp = TempDir::new().unwrap();
    populated(tmp.path()).await;
    let mut value = version(tmp.path());
    value["mismatch"] = json!(0);
    write_version(tmp.path(), &value);
    let before = io::files_of(tmp.path());
    assert!(PersistentBlockHeaderStorage::open(tmp.path()).is_err());
    assert_eq!(io::files_of(tmp.path()), before);
}

#[tokio::test]
async fn a_stale_intent_is_dropped_at_open() {
    let tmp = TempDir::new().unwrap();
    populated(tmp.path()).await;
    let mut value = version(tmp.path());
    value["generation"] = json!(2);
    value["mismatch"] = json!(1);
    write_version(tmp.path(), &value);
    let storage = DiskStorageManager::new(&config(tmp.path())).await.unwrap();
    assert_eq!(storage.get_tip_height().await, Some(7));
    assert_clean(&version(tmp.path()), 2);
}

#[tokio::test]
async fn a_clearing_write_keeps_the_mark() {
    let tmp = TempDir::new().unwrap();
    populated(tmp.path()).await;
    let mut value = version(tmp.path());
    value["generation"] = json!(2);
    value["mismatch"] = json!(2);
    write_version(tmp.path(), &value);
    migrator::write_clearing_sync(tmp.path()).unwrap();
    let value = version(tmp.path());
    assert_eq!(value["generation"], 2);
    assert_eq!(value["mismatch"], 2);
    assert_eq!(value["clearing"], true);
    migrator::StorageMigrator::migrate(tmp.path()).await.unwrap();
    assert_clean(&version(tmp.path()), 3);
}

fn one_blocking_thread(with_reader: bool) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .enable_time()
        .build()
        .unwrap();
    let tmp = TempDir::new().unwrap();
    let result = rt.block_on(async {
        let mut storage = damaged(tmp.path()).await;
        storage.stop().await;
        let store = storage.store().clone();
        tokio::time::timeout(Duration::from_secs(30), async {
            let mut read = None;
            let mut release = None;
            if with_reader {
                let (copied_tx, copied_rx) = tokio::sync::oneshot::channel();
                let (release_tx, release_rx) = std::sync::mpsc::channel();
                *store.store().integrity.after_copy.lock().unwrap() = Some(Box::new(move || {
                    copied_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                }));
                let headers = storage.block_headers();
                read = Some(tokio::spawn(async move { headers.read().await.get_header(5).await }));
                copied_rx.await.unwrap();
                release = Some(release_tx);
            }
            let (submitted_tx, submitted_rx) = tokio::sync::oneshot::channel();
            *store.store().integrity.clear_submitting.lock().unwrap() = Some(Box::new(move || {
                submitted_tx.send(()).unwrap();
            }));
            let clear = tokio::spawn(async move { storage.clear().await });
            submitted_rx.await.unwrap();
            tokio::task::yield_now().await;
            if let Some(release) = release {
                release.send(()).unwrap();
            }
            let read = match read {
                Some(read) => Some(read.await.unwrap()),
                None => None,
            };
            if with_reader {
                assert_eq!(store.store().integrity.writes.load(Ordering::Relaxed), 1);
            }
            clear.await.unwrap().unwrap();
            read
        })
        .await
    });
    rt.shutdown_background();
    let read = result.expect("a read and a clear must finish on one blocking thread");
    if with_reader {
        assert!(matches!(read, Some(Err(StorageError::Corruption(_)))));
    }
    assert_clean(&version(tmp.path()), 1);
}

#[test]
fn a_mismatched_read_and_a_clear_finish_on_one_blocking_thread() {
    one_blocking_thread(true);
}

#[test]
fn a_clear_alone_finishes_on_one_blocking_thread() {
    one_blocking_thread(false);
}
